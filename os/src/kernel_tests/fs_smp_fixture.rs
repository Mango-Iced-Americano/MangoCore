//! FS SMP ktest 的零盘 PageCache inode 与双 CPU user-write fixture。

use alloc::{sync::Arc, vec, vec::Vec};
use spin::Mutex;

use crate::{
    config::PAGE_SIZE,
    fs::{
        tmpfs::TmpFS,
        vfs::{
            BackendLifecycle, File, FileFlags, FilePrivateData, FileSystem, FileType, IndexNode,
            InodeMode, Metadata, MountFS, MountFSInode, MountFlags,
        },
        PageCache, PageCacheBackend,
    },
    kernel_tests::probe::{
        attach_probe_to_runner, build_user_probe, deadline_after, probe_quiesced, reap_probe,
        stop_probe, ProbeResult,
    },
    utils::error::SyscallErr,
};

/// ktest 的 `/dev/shm` 是未覆盖的 devfs 目录；此 fixture 在其上临时挂载 TmpFS。
/// Drop 无条件 detach，确保 probe 失败和超时不会污染下一项测试。
pub(crate) struct MountedTmpfsFixture {
    mount: Arc<MountFS>,
}

impl MountedTmpfsFixture {
    pub(crate) fn mount() -> Result<Self, &'static str> {
        let target = crate::fs::vfs_lookup_absolute("/dev/shm")
            .map_err(|_| "ktest has no /dev/shm mountpoint")?;
        let target = target
            .as_any_ref()
            .downcast_ref::<MountFSInode>()
            .ok_or("/dev/shm is not a MountFS inode")?;
        let fs = TmpFS::new();
        let mount = target
            .mount_subtree(
                BackendLifecycle::new(fs.clone()),
                fs.root_inode(),
                MountFlags::empty(),
                Some("/dev/shm".into()),
            )
            .map_err(|_| "failed to mount ktest TmpFS")?;
        Ok(Self { mount })
    }
}

impl Drop for MountedTmpfsFixture {
    fn drop(&mut self) {
        // probe 已在调用方 quiesce/reap；此处仅从可见树 detach，生命周期由 Arc 回收。
        let _ = self.mount.detach_recursive();
    }
}

/// PageCache 专用零盘后端；其读写均在 runner 或用户 syscall 内发生。
pub(crate) struct FsSmpPageBackend {
    data: Mutex<Vec<u8>>,
}

impl FsSmpPageBackend {
    fn new() -> Self {
        Self {
            data: Mutex::new(vec![0; PAGE_SIZE * 4]),
        }
    }
}

impl PageCacheBackend for FsSmpPageBackend {
    fn read_page(&self, index: usize, dst: &mut [u8]) -> Result<usize, SyscallErr> {
        let start = index.checked_mul(PAGE_SIZE).ok_or(SyscallErr::EIO)?;
        let data = self.data.lock();
        if start >= data.len() {
            dst.fill(0);
            return Ok(0);
        }
        let copied = (data.len() - start).min(dst.len());
        dst[..copied].copy_from_slice(&data[start..start + copied]);
        dst[copied..].fill(0);
        Ok(copied)
    }

    fn write_page(&self, index: usize, src: &[u8]) -> Result<usize, SyscallErr> {
        let start = index.checked_mul(PAGE_SIZE).ok_or(SyscallErr::EIO)?;
        let mut data = self.data.lock();
        if start >= data.len() {
            return Ok(0);
        }
        let copied = (data.len() - start).min(src.len());
        data[start..start + copied].copy_from_slice(&src[..copied]);
        Ok(copied)
    }

    fn npages(&self) -> usize {
        self.data.lock().len() / PAGE_SIZE
    }
}

/// 将 ktest PageCache 暴露为普通可写 inode，令 AP 只能通过真正的 user write syscall
/// 进入 `write_at_user`；它不在 AP 上借用任何 kernel-only helper。
pub(crate) struct FsSmpCacheInode {
    cache: Arc<PageCache>,
    /// 模拟真实 inode 的完整数据事务门，串行化 PageCache 与 size 的共同提交。
    io_txn: Mutex<()>,
    metadata: Mutex<Metadata>,
    fs: Arc<TmpFS>,
}

impl core::fmt::Debug for FsSmpCacheInode {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("FsSmpCacheInode")
            .finish_non_exhaustive()
    }
}

impl FsSmpCacheInode {
    pub(crate) fn new(cache: Arc<PageCache>, fs: Arc<TmpFS>) -> Self {
        Self {
            cache,
            io_txn: Mutex::new(()),
            metadata: Mutex::new(Metadata::new(FileType::File, InodeMode::S_IRWXUGO)),
            fs,
        }
    }
}

impl IndexNode for FsSmpCacheInode {
    fn read_at(
        &self,
        offset: usize,
        len: usize,
        buf: &mut [u8],
        _data: spin::MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        if buf.len() < len {
            return Err(SyscallErr::EINVAL);
        }
        let file_size = self.metadata.lock().size.max(0) as usize;
        if offset >= file_size {
            return Ok(0);
        }
        self.cache
            .read_kernel(offset, &mut buf[..len.min(file_size - offset)])
    }

    fn write_at_user(
        &self,
        offset: usize,
        len: usize,
        src: &crate::mm::UserBuffer,
    ) -> Result<usize, SyscallErr> {
        let _txn = self.io_txn.lock();
        let old_size = self.metadata.lock().size.max(0) as usize;
        let written = self.cache.write_at_user(offset, len, src, old_size)?;
        let end = offset.checked_add(written).ok_or(SyscallErr::EIO)?;
        let mut metadata = self.metadata.lock();
        metadata.size = metadata.size.max(end as i64);
        Ok(written)
    }

    fn supports_user_buffer_io(&self) -> bool {
        true
    }

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        Ok(self.metadata.lock().clone())
    }

    fn set_metadata(&self, metadata: &Metadata) -> Result<(), SyscallErr> {
        *self.metadata.lock() = metadata.clone();
        Ok(())
    }

    fn resize(&self, len: usize) -> Result<(), SyscallErr> {
        let _txn = self.io_txn.lock();
        let old_size = self.metadata.lock().size.max(0) as usize;
        if len <= old_size {
            self.cache.truncate(len)?;
        }
        self.metadata.lock().size = len as i64;
        Ok(())
    }

    fn fs(&self) -> Arc<dyn FileSystem> {
        self.fs.clone()
    }

    fn page_cache(&self) -> Option<Arc<PageCache>> {
        Some(self.cache.clone())
    }

    fn as_any_ref(&self) -> &dyn core::any::Any {
        self
    }
}

pub(crate) fn new_cache() -> Arc<PageCache> {
    let cache = PageCache::new();
    cache.set_backend(Arc::new(FsSmpPageBackend::new()));
    cache
}

pub(crate) fn read_inode(
    inode: &Arc<dyn IndexNode>,
    offset: usize,
    data: &mut [u8],
) -> Result<usize, SyscallErr> {
    inode.read_at(
        offset,
        data.len(),
        data,
        Mutex::new(FilePrivateData::Unused).lock(),
    )
}

/// 在 CPU1/CPU2 同步发布两个完整用户 TCB；每个 TCB 均只能经 write syscall 接触 PageCache。
pub(crate) fn run_dual_user_writes(
    inode: Arc<dyn IndexNode>,
    left_offset: usize,
    left_data: &[u8],
    right_offset: usize,
    right_data: &[u8],
) -> Result<(), &'static str> {
    if crate::smp::configured_cpu_count() < 3 {
        return Err("SKIP: requires CPU1 and CPU2 user probes");
    }
    let left_file = File::new(inode.clone(), FileFlags::O_WRONLY)
        .map_err(|_| "failed to create CPU1 writer fd")?;
    left_file.set_offset(left_offset);
    let right_file =
        File::new(inode, FileFlags::O_WRONLY).map_err(|_| "failed to create CPU2 writer fd")?;
    right_file.set_offset(right_offset);
    let left = build_user_probe(ProbeResult::WritePage, left_file, 64, Some(left_data))?;
    let right = build_user_probe(ProbeResult::WritePage, right_file, 64, Some(right_data))?;
    left.set_initial_cpus_allowed(1usize << 1);
    right.set_initial_cpus_allowed(1usize << 2);
    let left_parent = attach_probe_to_runner(&left)?;
    let right_parent = attach_probe_to_runner(&right)?;

    // 两个 TCB 在 runner 看到任一完成前均已发布；不以 sleep 扩大窗口，真实交错由 MTTCG 调度。
    crate::task::publish_task_on(left.clone(), 1);
    crate::task::publish_task_on(right.clone(), 2);
    let left_done = probe_quiesced(&left, &left.process, 1, deadline_after(3));
    let right_done = probe_quiesced(&right, &right.process, 2, deadline_after(3));
    let left_clean = left_done || stop_probe(&left, &left.process, 1);
    let right_clean = right_done || stop_probe(&right, &right.process, 2);
    let left_reaped = reap_probe(&left_parent, &left);
    let right_reaped = reap_probe(&right_parent, &right);
    if !left_clean || !right_clean || !left_reaped || !right_reaped {
        return Err("dual user writers did not quiesce and reap");
    }
    if left.process.exit_code() != 0 || right.process.exit_code() != 0 {
        return Err("dual user write syscall failed");
    }
    Ok(())
}
