//! WP1 的零盘 FS SMP ktest。
// allow: SIZE_OK - eight ktest fixtures and the dual-architecture user probes
// must stay in the sole user-authorized file for this work package.
//!
//! `spawn_ktest_task_on()` 只验证调度闭环，绝不承载 FS、设备或用户 MM 工作。
//! 本文件中唯一跨 CPU 的 FS 竞态由完整用户 TCB 在 AP 上经 syscall 触发；其余
//! WP1 保护性测试由 CPU0 runner 执行，待 WP2/WP3 建立对应并发协议后再扩展。

use alloc::{sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use crate::{
    config::PAGE_SIZE,
    fs::{
        tmpfs::TmpFS,
        vfs::{File, FileFlags, FilePrivateData, FileSystem, FileType, IndexNode, InodeMode, Metadata},
        PageCache, PageCacheBackend, PageCacheTestHook, PageState,
    },
    kernel_tests::runner::KernelTest,
    mm::{FaultAccess, MapFlags, MapPermission, VirtAddr},
    task::{ProcessManager, Signals, TaskControlBlock, TaskStatus},
    utils::error::SyscallErr,
};

const TEST_TIMEOUT_MS: usize = 5_000;
const PHASE_IDLE: usize = 0;
const PHASE_WRITER_HOLDS_ENTRY: usize = 1;
const PHASE_RELEASE_WRITER: usize = 2;
const SYSCALL_WRITE: usize = 64;
const SYSCALL_FTRUNCATE: usize = 46;
const SYSCALL_EXIT: usize = 93;

#[cfg(target_arch = "riscv64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.fs_smp_write_probe, "a"
    .balign 4
    .global __fs_smp_write_probe_start
    .global __fs_smp_write_probe_end
__fs_smp_write_probe_start:
    ecall
    lui t0, 1
    bne a0, t0, 1f
    addi a0, zero, 0
    j 2f
1:
    addi a0, zero, 1
2:
    addi a7, zero, {exit_syscall}
    ecall
3:  j 3b
__fs_smp_write_probe_end:
    .popsection
"#,
    exit_syscall = const SYSCALL_EXIT,
);

#[cfg(target_arch = "loongarch64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.fs_smp_write_probe, "a"
    .balign 4
    .global __fs_smp_write_probe_start
    .global __fs_smp_write_probe_end
__fs_smp_write_probe_start:
    syscall 0
    lu12i.w $t0, 1
    bne $a0, $t0, 1f
    move $a0, $zero
    b 2f
1:
    addi.d $a0, $zero, 1
2:
    addi.d $a7, $zero, {exit_syscall}
    syscall 0
3:  b 3b
__fs_smp_write_probe_end:
    .popsection
"#,
    exit_syscall = const SYSCALL_EXIT,
);

#[cfg(target_arch = "riscv64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.fs_smp_truncate_probe, "a"
    .balign 4
    .global __fs_smp_truncate_probe_start
    .global __fs_smp_truncate_probe_end
__fs_smp_truncate_probe_start:
    ecall
    bnez a0, 1f
    j 2f
1:
    addi a0, zero, 1
2:
    addi a7, zero, {exit_syscall}
    ecall
3:  j 3b
__fs_smp_truncate_probe_end:
    .popsection
"#,
    exit_syscall = const SYSCALL_EXIT,
);

#[cfg(target_arch = "loongarch64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.fs_smp_truncate_probe, "a"
    .balign 4
    .global __fs_smp_truncate_probe_start
    .global __fs_smp_truncate_probe_end
__fs_smp_truncate_probe_start:
    syscall 0
    beqz $a0, 1f
    addi.d $a0, $zero, 1
1:
    addi.d $a7, $zero, {exit_syscall}
    syscall 0
2:  b 2b
__fs_smp_truncate_probe_end:
    .popsection
"#,
    exit_syscall = const SYSCALL_EXIT,
);

extern "C" {
    static __fs_smp_write_probe_start: u8;
    static __fs_smp_write_probe_end: u8;
    static __fs_smp_truncate_probe_start: u8;
    static __fs_smp_truncate_probe_end: u8;
}

static HOOK_PHASE: AtomicUsize = AtomicUsize::new(PHASE_IDLE);
static HOOK_TIMED_OUT: AtomicUsize = AtomicUsize::new(0);

/// PageCache 专用零盘后端；其读写均在 runner 或用户 syscall 内发生。
struct FsSmpPageBackend {
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

/// 将 ktest PageCache 暴露为普通可写 inode，令 AP 只能通过真正的 user write
/// syscall 进入 write_at_user；它不在 AP 上借用任何 kernel-only helper。
struct FsSmpCacheInode {
    cache: Arc<PageCache>,
    metadata: Mutex<Metadata>,
    fs: Arc<TmpFS>,
}

impl core::fmt::Debug for FsSmpCacheInode {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_struct("FsSmpCacheInode").finish_non_exhaustive()
    }
}

impl FsSmpCacheInode {
    fn new(cache: Arc<PageCache>, fs: Arc<TmpFS>) -> Self {
        Self {
            cache,
            metadata: Mutex::new(Metadata::new(FileType::File, InodeMode::S_IRWXUGO)),
            fs,
        }
    }
}

impl IndexNode for FsSmpCacheInode {
    fn write_at_user(
        &self,
        offset: usize,
        len: usize,
        src: &crate::mm::UserBuffer,
    ) -> Result<usize, SyscallErr> {
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
        let old_size = self.metadata.lock().size.max(0) as usize;
        // 测试 inode 的初始逻辑长度为零；即使 writer 尚未在 hook 后发布新 EOF，
        // `ftruncate(fd, 0)` 也必须驱动 PageCache 失效已取得的 page entry。
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

/// 返回本组八项 FS SMP 测试。
pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::with_timeout("fs_smp::pagecache_user_write_vs_truncate", pagecache_user_write_vs_truncate, TEST_TIMEOUT_MS),
        KernelTest::with_timeout("fs_smp::pagecache_same_page_no_torn_copy", pagecache_same_page_no_torn_copy, TEST_TIMEOUT_MS),
        KernelTest::with_timeout("fs_smp::pagecache_writeback_redirty", pagecache_writeback_redirty, TEST_TIMEOUT_MS),
        KernelTest::with_timeout("fs_smp::ext4_create_same_name_exactly_once", ext4_create_same_name_exactly_once, TEST_TIMEOUT_MS),
        KernelTest::with_timeout("fs_smp::ext4_cross_rename_opposite_order", ext4_cross_rename_opposite_order, TEST_TIMEOUT_MS),
        KernelTest::with_timeout("fs_smp::tmpfs_lookup_unlink_generation", tmpfs_lookup_unlink_generation, TEST_TIMEOUT_MS),
        KernelTest::with_timeout("fs_smp::truncate_tail_zero_after_extend", truncate_tail_zero_after_extend, TEST_TIMEOUT_MS),
        KernelTest::with_timeout("fs_smp::different_page_parallel_progress", different_page_parallel_progress, TEST_TIMEOUT_MS),
    ]
}

fn deadline_after(seconds: usize) -> usize {
    crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(seconds))
}

fn wait_for_at_least(value: &AtomicUsize, expected: usize, deadline: usize) -> bool {
    while value.load(Ordering::Acquire) < expected {
        if crate::hal::get_time() >= deadline {
            return false;
        }
        core::hint::spin_loop();
    }
    true
}

fn pagecache_hook(page: usize) {
    if page != 0 {
        return;
    }
    // hook 位于 PageEntry 获取之后、user copy 之前；只能发布 phase 并自旋，
    // 不能拿锁、分配或调度，否则它本身会破坏待测窗口。
    HOOK_PHASE.store(PHASE_WRITER_HOLDS_ENTRY, Ordering::Release);
    if !wait_for_at_least(&HOOK_PHASE, PHASE_RELEASE_WRITER, deadline_after(3)) {
        HOOK_TIMED_OUT.store(1, Ordering::Release);
    }
}

fn new_cache(hook: Option<PageCacheTestHook>) -> Arc<PageCache> {
    let cache = PageCache::new_with_test_hook(hook);
    cache.set_backend(Arc::new(FsSmpPageBackend::new()));
    cache
}

fn write_inode(inode: &Arc<dyn IndexNode>, offset: usize, data: &[u8]) -> Result<usize, SyscallErr> {
    inode.write_at(offset, data.len(), data, Mutex::new(FilePrivateData::Unused).lock())
}

fn read_inode(inode: &Arc<dyn IndexNode>, offset: usize, data: &mut [u8]) -> Result<usize, SyscallErr> {
    inode.read_at(offset, data.len(), data, Mutex::new(FilePrivateData::Unused).lock())
}

fn user_probe_program(write: bool) -> &'static [u8] {
    let (start, end) = if write {
        (core::ptr::addr_of!(__fs_smp_write_probe_start), core::ptr::addr_of!(__fs_smp_write_probe_end))
    } else {
        (core::ptr::addr_of!(__fs_smp_truncate_probe_start), core::ptr::addr_of!(__fs_smp_truncate_probe_end))
    };
    // SAFETY: [Category 10/11 — bounds/provenance] 同一 `global_asm!` section 定义的
    // start/end 符号包围连续只读指令流，链接器固定其相对顺序；没有把整数恢复为指针。
    unsafe { core::slice::from_raw_parts(start, end.offset_from(start) as usize) }
}

fn map_user_page(task: &Arc<TaskControlBlock>, data: &[u8], executable: bool) -> Result<usize, &'static str> {
    task.process.vm().write(|space| {
        let address = space.mmap(
            0,
            PAGE_SIZE,
            MapPermission::R | MapPermission::W | MapPermission::U,
            MapFlags::MAP_PRIVATE | MapFlags::MAP_ANONYMOUS,
            0,
            None,
            true,
            false,
        );
        if address < 0 || data.len() > PAGE_SIZE {
            return Err("failed to reserve user probe page");
        }
        let address = address as usize;
        let pa = space
            .fault_in_user_va(VirtAddr::from(address), FaultAccess::Store)
            .map_err(|_| "failed to populate user probe page")?;
        let offset = pa.page_offset();
        pa.floor().get_bytes_array()[offset..offset + data.len()].copy_from_slice(data);
        if executable {
            space
                .mprotect(address, PAGE_SIZE, MapPermission::R | MapPermission::X | MapPermission::U)
                .map_err(|_| "failed to protect user probe code page")?;
        }
        Ok(address)
    })
}

fn build_user_probe(
    program: &'static [u8],
    file: Arc<File>,
    syscall: usize,
    data: Option<&[u8]>,
) -> Result<Arc<TaskControlBlock>, &'static str> {
    let inode = crate::fs::vfs_lookup_absolute("/init")
        .or_else(|_| crate::fs::vfs_lookup_absolute("/initproc"))
        .map_err(|_| "ktest initramfs has no user ELF scaffold")?;
    let elf = File::new(inode, FileFlags::O_RDONLY).map_err(|_| "failed to open user ELF scaffold")?;
    let task = TaskControlBlock::new(elf);
    task.process.close_files_on_exit();
    let fd = task
        .process
        .files()
        .lock()
        .alloc_fd(file, false)
        .map_err(|_| "failed to install user probe fd")?;
    let entry = map_user_page(&task, program, true)?;
    let buffer = match data {
        Some(bytes) => map_user_page(&task, bytes, false)?,
        None => 0,
    };
    let mut inner = task.acquire_inner_lock();
    let gp = &mut inner.trap_context_mut().gp;
    gp.pc = entry;
    gp.a0 = fd;
    gp.a1 = buffer;
    gp.a2 = data.map_or(0, |bytes| bytes.len());
    gp.a7 = syscall;
    drop(inner);
    Ok(task)
}

fn attach_probe_to_runner(task: &Arc<TaskControlBlock>) -> Result<Arc<crate::task::ProcessControlBlock>, &'static str> {
    let runner = crate::task::current_task().ok_or("ktest runner has no current task")?;
    let parent = runner.process.clone();
    drop(runner);
    let process = task.process.clone();
    parent
        .add_child(process.clone())
        .map_err(|_| "failed to attach user probe to ktest runner")?;
    process.set_parent(Some(Arc::downgrade(&parent)));
    Ok(parent)
}

fn probe_quiesced(task: &Arc<TaskControlBlock>, process: &Arc<crate::task::ProcessControlBlock>, cpu: usize, deadline: usize) -> bool {
    crate::hal::with_local_interrupts_enabled(|| {
        while !process.is_zombie()
            || task.task_status() != TaskStatus::Zombie
            || crate::task::processor::cpu_has_current(cpu)
            || crate::task::run_queue_count(cpu) != 0
        {
            if crate::hal::get_time() >= deadline {
                return false;
            }
            crate::task::run_task_safe_point();
            core::hint::spin_loop();
        }
        true
    })
}

fn stop_probe(task: &Arc<TaskControlBlock>, process: &Arc<crate::task::ProcessControlBlock>, cpu: usize) -> bool {
    if !process.is_zombie() {
        task.acquire_inner_lock().add_signal(Signals::SIGKILL);
        let _ = crate::smp::request_reschedule(cpu);
    }
    probe_quiesced(task, process, cpu, deadline_after(2))
}

fn reap_probe(parent: &Arc<crate::task::ProcessControlBlock>, task: &Arc<TaskControlBlock>) -> bool {
    ProcessManager::wait_child(parent, task.pid() as isize, true, true, false, false, false)
        .ok()
        .flatten()
        .is_some()
}

fn pagecache_user_write_vs_truncate() -> Result<(), &'static str> {
    if crate::smp::configured_cpu_count() < 3 {
        return Ok(());
    }
    HOOK_PHASE.store(PHASE_IDLE, Ordering::Release);
    HOOK_TIMED_OUT.store(0, Ordering::Release);
    let cache = new_cache(Some(pagecache_hook));
    let inode: Arc<dyn IndexNode> = Arc::new(FsSmpCacheInode::new(cache.clone(), TmpFS::new()));
    let writer_file = File::new(inode.clone(), FileFlags::O_WRONLY).map_err(|_| "failed to create writer fd")?;
    let truncater_file = File::new(inode, FileFlags::O_WRONLY).map_err(|_| "failed to create truncater fd")?;
    let writer = build_user_probe(user_probe_program(true), writer_file, SYSCALL_WRITE, Some(&[0xa5; PAGE_SIZE]))?;
    writer.set_initial_cpus_allowed(1usize << 1);
    let writer_parent = attach_probe_to_runner(&writer)?;
    crate::task::publish_task_on(writer.clone(), 1);
    if !wait_for_at_least(&HOOK_PHASE, PHASE_WRITER_HOLDS_ENTRY, deadline_after(2)) {
        HOOK_PHASE.store(PHASE_RELEASE_WRITER, Ordering::Release);
        let stopped = stop_probe(&writer, &writer.process, 1);
        let reaped = reap_probe(&writer_parent, &writer);
        HOOK_PHASE.store(PHASE_IDLE, Ordering::Release);
        return if stopped && reaped { Err("writer did not acquire PageCache entry") } else { Err("writer cleanup did not quiesce CPU1") };
    }

    let truncater = match build_user_probe(user_probe_program(false), truncater_file, SYSCALL_FTRUNCATE, None) {
        Ok(task) => task,
        Err(error) => {
            HOOK_PHASE.store(PHASE_RELEASE_WRITER, Ordering::Release);
            let stopped = stop_probe(&writer, &writer.process, 1);
            let reaped = reap_probe(&writer_parent, &writer);
            HOOK_PHASE.store(PHASE_IDLE, Ordering::Release);
            return if stopped && reaped { Err(error) } else { Err("writer cleanup did not quiesce CPU1") };
        }
    };
    truncater.set_initial_cpus_allowed(1usize << 2);
    let truncater_parent = match attach_probe_to_runner(&truncater) {
        Ok(parent) => parent,
        Err(error) => {
            HOOK_PHASE.store(PHASE_RELEASE_WRITER, Ordering::Release);
            let stopped = stop_probe(&writer, &writer.process, 1);
            let reaped = reap_probe(&writer_parent, &writer);
            HOOK_PHASE.store(PHASE_IDLE, Ordering::Release);
            return if stopped && reaped { Err(error) } else { Err("writer cleanup did not quiesce CPU1") };
        }
    };
    crate::task::publish_task_on(truncater.clone(), 2);
    // `ftruncate` probe 不能直接发布 completion，因此以 entry 被摘除作为完成代理。
    // 旧路径会在此短窗口内摘除 entry；op_gate 串行化下 truncate 仍等待读锁，entry 保持存在。
    let truncate_observation_deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq() / 10);
    while cache.contains_page(0) && crate::hal::get_time() < truncate_observation_deadline {
        core::hint::spin_loop();
    }
    let detached_before_release = !cache.contains_page(0);
    let truncate_finished_before_release = detached_before_release;
    HOOK_PHASE.store(PHASE_RELEASE_WRITER, Ordering::Release);
    let writer_done = probe_quiesced(&writer, &writer.process, 1, deadline_after(3));
    let truncater_done = if truncate_finished_before_release {
        true
    } else {
        probe_quiesced(&truncater, &truncater.process, 2, deadline_after(3))
    };
    let writer_clean = if writer_done { true } else { stop_probe(&writer, &writer.process, 1) };
    let truncater_clean = if truncater_done { true } else { stop_probe(&truncater, &truncater.process, 2) };
    let writer_reaped = reap_probe(&writer_parent, &writer);
    let truncater_reaped = reap_probe(&truncater_parent, &truncater);
    HOOK_PHASE.store(PHASE_IDLE, Ordering::Release);
    if !writer_clean || !truncater_clean || !writer_reaped || !truncater_reaped {
        return Err("pagecache user probes did not quiesce and reap");
    }
    if HOOK_TIMED_OUT.load(Ordering::Acquire) != 0 {
        return Err("writer hook did not receive a bounded release");
    }
    if writer.process.exit_code() != 0 || truncater.process.exit_code() != 0 {
        return Err("pagecache user probe syscall failed");
    }
    if truncate_finished_before_release && detached_before_release {
        return Err("writer completed after truncate detached page entry");
    }
    if cache.contains_page(0) {
        return Err("serialized truncate retained the page entry");
    }
    Ok(())
}

fn pagecache_same_page_no_torn_copy() -> Result<(), &'static str> {
    // WP2 owns the concurrent copy protocol; WP1 keeps a runner-side byte-integrity guard.
    let cache = new_cache(None);
    cache.write_kernel(0, &[0x41; PAGE_SIZE], 0).map_err(|_| "failed to seed page")?;
    cache.write_kernel(0, &[0x42; PAGE_SIZE], PAGE_SIZE).map_err(|_| "failed to replace page")?;
    let mut snapshot = [0u8; PAGE_SIZE];
    cache.read_kernel(0, &mut snapshot).map_err(|_| "failed to read page")?;
    if snapshot.iter().any(|byte| *byte != 0x42) {
        return Err("same-page copy exposed a torn pattern");
    }
    Ok(())
}

fn pagecache_writeback_redirty() -> Result<(), &'static str> {
    // WP2 owns the AP writeback/redirty interleaving; runner validates the state transition API.
    let cache = new_cache(None);
    cache.write_kernel(0, &[0x57; PAGE_SIZE], 0).map_err(|_| "failed to seed writeback cache")?;
    cache.writeback_page(0).map_err(|_| "writeback failed")?;
    let frame = cache.frame_for_write(0).map_err(|_| "redirty failed")?;
    frame.ppn.get_bytes_array().fill(0x52);
    if cache.state_of(0) != Some(PageState::Dirty) || !cache.is_dirty(0) {
        return Err("writeback/redirty lost Dirty state");
    }
    Ok(())
}

fn ext4_create_same_name_exactly_once() -> Result<(), &'static str> {
    // WP3 拥有 ext4 真实 RED：`kernel_tests/ext4.rs` 的零盘 TestMemBlock 是
    // ext4_lwext4 的未格式化 block-adapter fixture，并没有可供 native ext4 挂载的
    // mkfs 镜像；WP1 不得用来源不明的 test_img 触发 native ext4 panic。
    let fs = TmpFS::new();
    let root = fs.root_inode();
    root.create("wp1_same", FileType::File, InodeMode::S_IRWXUGO)
        .map_err(|_| "tmpfs first create failed")?;
    if root
        .create("wp1_same", FileType::File, InodeMode::S_IRWXUGO)
        .is_ok()
    {
        return Err("same-name protective create was not exactly once");
    }
    if root.find("wp1_same").is_err() {
        return Err("successful protective create was not published in the directory");
    }
    Ok(())
}

fn ext4_cross_rename_opposite_order() -> Result<(), &'static str> {
    // WP3 拥有 ext4 真实 RED；此处只在 runner 上以 tmpfs 验证同构的 create/rename
    // 基线，保证 WP1 零盘 ktest 不因 native ext4 镜像缺失而 panic。
    let fs = TmpFS::new();
    let root = fs.root_inode();
    let left = root
        .create("wp1_left", FileType::Dir, InodeMode::S_IRWXUGO)
        .map_err(|_| "failed to create protective left directory")?;
    let right = root
        .create("wp1_right", FileType::Dir, InodeMode::S_IRWXUGO)
        .map_err(|_| "failed to create protective right directory")?;
    left.create("x", FileType::File, InodeMode::S_IRWXUGO)
        .map_err(|_| "failed to create protective rename source")?;
    left.rename("x", &right, "x", 0)
        .map_err(|_| "protective rename failed")?;
    if right.find("x").is_err() {
        return Err("protective rename did not publish destination");
    }
    Ok(())
}

fn tmpfs_lookup_unlink_generation() -> Result<(), &'static str> {
    // WP3 owns concurrent lookup/unlink publication; runner guards the generation identity baseline.
    let fs = TmpFS::new();
    let root = fs.root_inode();
    for _ in 0..32 {
        let child = root.create("wp1_generation", FileType::File, InodeMode::S_IRWXUGO).map_err(|_| "tmpfs create failed")?;
        let expected = child.metadata().map_err(|_| "tmpfs metadata failed")?.inode_id;
        let found = root.find("wp1_generation").map_err(|_| "tmpfs lookup failed")?;
        if found.metadata().map_err(|_| "tmpfs lookup metadata failed")?.inode_id != expected {
            return Err("tmpfs lookup observed a stale generation identity");
        }
        root.unlink("wp1_generation").map_err(|_| "tmpfs unlink failed")?;
    }
    Ok(())
}

fn truncate_tail_zero_after_extend() -> Result<(), &'static str> {
    let fs = TmpFS::new();
    let root = fs.root_inode();
    let file = root.create("wp1_tail", FileType::File, InodeMode::S_IRWXUGO).map_err(|_| "failed to create tmpfs tail fixture")?;
    write_inode(&file, 0, &[0x7d; PAGE_SIZE]).map_err(|_| "failed to seed tmpfs tail fixture")?;
    file.truncate(PAGE_SIZE / 2).map_err(|_| "tmpfs truncate failed")?;
    file.resize(PAGE_SIZE).map_err(|_| "tmpfs extend failed")?;
    let mut tail = [0u8; PAGE_SIZE / 2];
    if read_inode(&file, PAGE_SIZE / 2, &mut tail).map_err(|_| "tmpfs tail read failed")? != tail.len()
        || tail.iter().any(|byte| *byte != 0)
    {
        return Err("truncate then extend exposed old tail bytes");
    }
    Ok(())
}

fn different_page_parallel_progress() -> Result<(), &'static str> {
    // WP2 owns per-entry parallel-progress scheduling; runner preserves its independent-page baseline.
    let cache = new_cache(None);
    cache.write_kernel(0, &[0x11; PAGE_SIZE], 0).map_err(|_| "page0 write failed")?;
    cache.write_kernel(PAGE_SIZE, &[0x31; PAGE_SIZE], 0).map_err(|_| "page1 write failed")?;
    if !cache.contains_page(0) || !cache.contains_page(1) {
        return Err("different-page writes did not retain independent entries");
    }
    Ok(())
}
