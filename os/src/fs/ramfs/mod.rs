//! RamFS — 纯内存文件系统
//!
//! 参照 DragonOS `kernel/src/filesystem/ramfs/mod.rs` 实现。
//! 数据以页为单位存储，使用 `BTreeMap<usize, Arc<FrameTracker>>` 管理物理页。
//! 目录结构用 `BTreeMap`。
//! 用于 VFS 层调试，不依赖任何块设备。

use alloc::{
    collections::BTreeMap,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::any::Any;
use spin::{Mutex, MutexGuard};

use crate::config::PAGE_SIZE;
use crate::fs::page_cache::{PageCache as NewPageCache, PageCacheBackend};
use crate::mm::{frame_alloc, FrameTracker};
use crate::utils::error::SyscallErr;

use super::vfs::{
    generate_inode_id, FileFlags, FilePrivateData, FileSystem, FileType, FsInfo, IndexNode,
    InodeFlags, InodeId, InodeMode, Metadata, SuperBlock,
};

/// RamFS inode 名称最大长度
const RAMFS_MAX_NAMELEN: usize = 64;
const RAMFS_BLOCK_SIZE: u64 = 512;
/// DragonOS ramfs magic
const RAMFS_MAGIC: u64 = 0x8584_58f6;

// ── LockedRamFSInode ──────────────────────────────────────────────────

/// 带锁的 RamFS inode 包装器
#[derive(Debug)]
pub struct LockedRamFSInode(pub Mutex<RamFSInode>);

// ── RamFS ─────────────────────────────────────────────────────────────

/// RamFS 文件系统实例
#[derive(Debug)]
pub struct RamFS {
    root_inode: Arc<LockedRamFSInode>,
    self_ref: Mutex<Weak<RamFS>>,
    /// 整个文件系统的最大页数（0 = 不限制）
    max_pages: usize,
    /// 当前已分配的页数
    page_count: Mutex<usize>,
}

// ── RamFSInode ────────────────────────────────────────────────────────

/// RamFS inode 内部数据（不加锁）
pub struct RamFSInode {
    /// 父目录弱引用
    parent: Weak<LockedRamFSInode>,
    /// 自身弱引用
    self_ref: Weak<LockedRamFSInode>,
    /// 子项 B 树
    children: BTreeMap<String, Arc<LockedRamFSInode>>,
    /// 文件数据 — 页索引 → 物理帧
    pages: BTreeMap<usize, Arc<FrameTracker>>,
    /// PageCache（懒初始化，供 filemap shared fault 使用）
    new_page_cache: Mutex<Option<Arc<NewPageCache>>>,
    /// 逻辑文件大小（字节）
    file_size: usize,
    /// 元数据
    metadata: Metadata,
    /// 所属文件系统弱引用
    fs: Weak<RamFS>,
}

impl core::fmt::Debug for RamFSInode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RamFSInode")
            .field("file_size", &self.file_size)
            .field("pages", &self.pages.len())
            .field("children", &self.children.len())
            .field("metadata", &self.metadata)
            .finish()
    }
}

impl RamFSInode {
    pub fn new() -> Self {
        Self {
            parent: Weak::default(),
            self_ref: Weak::default(),
            children: BTreeMap::new(),
            pages: BTreeMap::new(),
            new_page_cache: Mutex::new(None),
            file_size: 0,
            metadata: Metadata {
                dev_id: 0,
                inode_id: generate_inode_id(),
                size: 0,
                blk_size: 0,
                blocks: 0,
                atime: crate::timer::TimeSpec::new(),
                mtime: crate::timer::TimeSpec::new(),
                ctime: crate::timer::TimeSpec::new(),
                file_type: FileType::Dir,
                mode: InodeMode::S_IRWXUGO,
                nlinks: 2, // . 和来自父目录的引用
                uid: 0,
                gid: 0,
                raw_dev: 0,
                flags: InodeFlags::empty(),
            },
            fs: Weak::default(),
        }
    }
}

// ── 页操作辅助函数 ───────────────────────────────────────────────────

/// 分配一个物理页，返回 FrameTracker
fn alloc_page() -> Option<Arc<FrameTracker>> {
    crate::mm::frame_alloc()
}

/// 从 FrameTracker 获取页内偏移处的只读物理地址
fn page_ptr(frame: &Arc<FrameTracker>, offset_within_page: usize) -> *const u8 {
    let ppn = frame.ppn;
    let phys_addr = ppn.0 * PAGE_SIZE + offset_within_page;
    phys_addr as *const u8
}

/// 从 FrameTracker 获取页内偏移处的可写物理地址
fn page_ptr_mut(frame: &Arc<FrameTracker>, offset_within_page: usize) -> *mut u8 {
    let ppn = frame.ppn;
    let phys_addr = ppn.0 * PAGE_SIZE + offset_within_page;
    phys_addr as *mut u8
}

// ── FileSystem impl for RamFS ─────────────────────────────────────────

impl FileSystem for RamFS {
    fn root_inode(&self) -> Arc<dyn IndexNode> {
        self.root_inode.clone()
    }

    fn info(&self) -> FsInfo {
        FsInfo {
            blk_dev_id: 0,
            max_name_len: RAMFS_MAX_NAMELEN,
            features: alloc::vec![],
        }
    }

    fn name(&self) -> &str {
        "ramfs"
    }

    fn super_block(&self) -> SuperBlock {
        SuperBlock {
            f_type: RAMFS_MAGIC,
            f_bsize: RAMFS_BLOCK_SIZE,
            f_namelen: RAMFS_MAX_NAMELEN as u64,
            ..SuperBlock::default()
        }
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}

// ── RamFS 构造 ────────────────────────────────────────────────────────

impl RamFS {
    pub fn new() -> Arc<Self> {
        Self::new_inner(0)
    }

    /// 创建带页数配额的 RamFS（max_pages = 0 表示不限制）
    pub fn new_with_quota(max_pages: usize) -> Arc<Self> {
        Self::new_inner(max_pages)
    }

    fn new_inner(max_pages: usize) -> Arc<Self> {
        let root: Arc<LockedRamFSInode> =
            Arc::new(LockedRamFSInode(Mutex::new(RamFSInode::new())));

        let result: Arc<RamFS> = Arc::new(RamFS {
            root_inode: root,
            self_ref: Mutex::new(Weak::new()),
            max_pages,
            page_count: Mutex::new(0),
        });

        // 设置自引用
        *result.self_ref.lock() = Arc::downgrade(&result);

        // 初始化 root inode 的 parent / self_ref / fs
        let mut root_guard: MutexGuard<RamFSInode> = result.root_inode.0.lock();
        root_guard.parent = Arc::downgrade(&result.root_inode);
        root_guard.self_ref = Arc::downgrade(&result.root_inode);
        root_guard.fs = Arc::downgrade(&result);
        drop(root_guard);

        result
    }

    /// 获取 RamFS 的 Weak 引用（用于创建子 inode 时传递）
    pub fn downgrade(self: &Arc<Self>) -> Weak<RamFS> {
        Arc::downgrade(self)
    }
}

// ── RamFsPageCacheBackend ───────────────────────────────────────────────

struct RamFsPageCacheBackend {
    inode: Weak<LockedRamFSInode>,
}

impl PageCacheBackend for RamFsPageCacheBackend {
    fn read_page(&self, index: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        let inode = self.inode.upgrade().ok_or(SyscallErr::EIO)?;
        let inner = inode.0.lock();
        if let Some(frame) = inner.pages.get(&index) {
            let src = unsafe { &*(frame.ppn.0 as *const [u8; PAGE_SIZE]) };
            buf[..PAGE_SIZE].copy_from_slice(&src[..PAGE_SIZE]);
        } else {
            buf[..PAGE_SIZE].fill(0);
        }
        Ok(PAGE_SIZE)
    }

    fn write_page(&self, index: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
        let inode = self.inode.upgrade().ok_or(SyscallErr::EIO)?;
        let ramfs = {
            let inner = inode.0.lock();
            inner.fs.upgrade().ok_or(SyscallErr::EIO)?
        };
        let mut inner = inode.0.lock();
        if let Some(frame) = inner.pages.get(&index) {
            let dst = unsafe { &mut *(frame.ppn.0 as *mut [u8; PAGE_SIZE]) };
            dst[..PAGE_SIZE].copy_from_slice(&buf[..PAGE_SIZE]);
        } else {
            let frame = frame_alloc().ok_or(SyscallErr::ENOMEM)?;
            let dst = unsafe { &mut *(frame.ppn.0 as *mut [u8; PAGE_SIZE]) };
            dst[..PAGE_SIZE].copy_from_slice(&buf[..PAGE_SIZE]);
            inner.pages.insert(index, frame);
            if ramfs.max_pages > 0 {
                *ramfs.page_count.lock() += 1;
            }
        }
        Ok(PAGE_SIZE)
    }

    fn npages(&self) -> usize {
        let inode = match self.inode.upgrade() {
            Some(i) => i,
            None => return 0,
        };
        let inner = inode.0.lock();
        (inner.file_size + PAGE_SIZE - 1) / PAGE_SIZE
    }
}

// ── IndexNode impl for LockedRamFSInode ───────────────────────────────

impl IndexNode for LockedRamFSInode {
    fn open(
        &self,
        _data: MutexGuard<FilePrivateData>,
        _flags: &FileFlags,
    ) -> Result<(), SyscallErr> {
        Ok(())
    }

    fn close(&self, _data: MutexGuard<FilePrivateData>) -> Result<(), SyscallErr> {
        Ok(())
    }

    fn read_at(
        &self,
        offset: usize,
        len: usize,
        buf: &mut [u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        if buf.len() < len {
            return Err(SyscallErr::EINVAL);
        }
        let inode: MutexGuard<RamFSInode> = self.0.lock();
        if inode.metadata.file_type == FileType::Dir {
            return Err(SyscallErr::EISDIR);
        }

        // 超出文件末尾 → 返回 0
        if offset >= inode.file_size {
            return Ok(0);
        }

        let effective_len = len.min(inode.file_size - offset);
        let first_page = offset / PAGE_SIZE;
        let last_page = (offset + effective_len - 1) / PAGE_SIZE;
        let mut buf_offset: usize = 0;

        for page_idx in first_page..=last_page {
            let start_in_page = if page_idx == first_page {
                offset % PAGE_SIZE
            } else {
                0
            };
            let page_end = if page_idx == last_page {
                (offset + effective_len - 1) % PAGE_SIZE + 1
            } else {
                PAGE_SIZE
            };
            let bytes_in_page = page_end - start_in_page;

            if let Some(frame) = inode.pages.get(&page_idx) {
                // 页存在 → 从物理内存拷贝到 buf
                let src = page_ptr(frame, start_in_page);
                let dst = buf.as_mut_ptr().wrapping_add(buf_offset);
                unsafe {
                    core::ptr::copy_nonoverlapping(src, dst, bytes_in_page);
                }
            } else {
                // 空洞 → 填零
                buf[buf_offset..buf_offset + bytes_in_page].fill(0);
            }

            buf_offset += bytes_in_page;
        }

        Ok(effective_len)
    }

    fn write_at(
        &self,
        offset: usize,
        len: usize,
        buf: &[u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        if buf.len() < len {
            return Err(SyscallErr::EINVAL);
        }
        if len == 0 {
            return Ok(0);
        }

        let mut inode: MutexGuard<RamFSInode> = self.0.lock();
        if inode.metadata.file_type == FileType::Dir {
            return Err(SyscallErr::EISDIR);
        }

        // 获取 RamFS 引用（用于配额检查）
        let ramfs: Arc<RamFS> = inode.fs.upgrade().ok_or(SyscallErr::EINVAL)?;

        let first_page = offset / PAGE_SIZE;
        let last_page = (offset + len - 1) / PAGE_SIZE;
        let mut buf_offset: usize = 0;

        for page_idx in first_page..=last_page {
            let start_in_page = if page_idx == first_page {
                offset % PAGE_SIZE
            } else {
                0
            };
            let page_end = if page_idx == last_page {
                (offset + len - 1) % PAGE_SIZE + 1
            } else {
                PAGE_SIZE
            };
            let bytes_in_page = page_end - start_in_page;

            // 按需分配页
            if !inode.pages.contains_key(&page_idx) {
                // 配额检查
                if ramfs.max_pages > 0 {
                    let mut page_count: MutexGuard<usize> = ramfs.page_count.lock();
                    if *page_count >= ramfs.max_pages {
                        return Err(SyscallErr::ENOSPC);
                    }
                    *page_count += 1;
                }

                let frame: Arc<FrameTracker> = match alloc_page() {
                    Some(f) => f,
                    None => {
                        // 分配失败，回滚配额计数
                        if ramfs.max_pages > 0 {
                            let mut page_count = ramfs.page_count.lock();
                            *page_count = page_count.saturating_sub(1);
                        }
                        return Err(SyscallErr::ENOMEM);
                    }
                };

                inode.pages.insert(page_idx, frame);
            }

            // 从 buf 拷贝到物理页
            let frame: &Arc<FrameTracker> = inode.pages.get(&page_idx).unwrap();
            let dst: *mut u8 = page_ptr_mut(frame, start_in_page);
            let src: *const u8 = buf.as_ptr().wrapping_add(buf_offset);
            unsafe {
                core::ptr::copy_nonoverlapping(src, dst, bytes_in_page);
            }

            buf_offset += bytes_in_page;
        }

        // 更新文件大小
        let new_size: usize = offset + len;
        if new_size > inode.file_size {
            inode.file_size = new_size;
        }

        Ok(len)
    }

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        let inode = self.0.lock();
        let mut meta = inode.metadata.clone();
        meta.size = inode.file_size as i64;
        Ok(meta)
    }

    fn set_metadata(&self, metadata: &Metadata) -> Result<(), SyscallErr> {
        let mut inode = self.0.lock();
        inode.metadata.atime = metadata.atime;
        inode.metadata.mtime = metadata.mtime;
        inode.metadata.ctime = metadata.ctime;
        inode.metadata.mode = metadata.mode;
        inode.metadata.uid = metadata.uid;
        inode.metadata.gid = metadata.gid;
        Ok(())
    }

    fn find(&self, name: &str) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        let inode = self.0.lock();
        if inode.metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        match name {
            "" | "." => inode
                .self_ref
                .upgrade()
                .map(|n| n as Arc<dyn IndexNode>)
                .ok_or(SyscallErr::ENOENT),
            ".." => inode
                .parent
                .upgrade()
                .map(|n| n as Arc<dyn IndexNode>)
                .ok_or(SyscallErr::ENOENT),
            name => inode
                .children
                .get(name)
                .cloned()
                .map(|n| n as Arc<dyn IndexNode>)
                .ok_or(SyscallErr::ENOENT),
        }
    }

    fn list(&self) -> Result<Vec<String>, SyscallErr> {
        let inode = self.0.lock();
        if inode.metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        let mut keys: Vec<String> = Vec::new();
        keys.push(String::from("."));
        keys.push(String::from(".."));
        for k in inode.children.keys() {
            keys.push(k.clone());
        }
        Ok(keys)
    }

    fn create(
        &self,
        name: &str,
        file_type: FileType,
        mode: InodeMode,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        return self.create_with_data(name, file_type, mode, 0);
    }

    fn create_with_data(
        &self,
        name: &str,
        file_type: FileType,
        mode: InodeMode,
        data: usize,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        let mut inode = self.0.lock();
        if inode.metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        if inode.children.contains_key(name) {
            return Err(SyscallErr::EEXIST);
        }

        let result: Arc<LockedRamFSInode> = Arc::new(LockedRamFSInode(Mutex::new(RamFSInode {
            parent: inode.self_ref.clone(),
            self_ref: Weak::default(),
            children: BTreeMap::new(),
            pages: BTreeMap::new(),
            new_page_cache: Mutex::new(None),
            file_size: 0,
            metadata: Metadata {
                dev_id: 0,
                inode_id: generate_inode_id(),
                size: 0,
                blk_size: 0,
                blocks: 0,
                atime: crate::timer::TimeSpec::new(),
                mtime: crate::timer::TimeSpec::new(),
                ctime: crate::timer::TimeSpec::new(),
                file_type,
                mode,
                nlinks: if file_type == FileType::Dir { 2 } else { 1 },
                uid: 0,
                gid: 0,
                raw_dev: data as u64,
                flags: InodeFlags::empty(),
            },
            fs: inode.fs.clone(),
        })));

        // 初始化自引用
        result.0.lock().self_ref = Arc::downgrade(&result);

        inode
            .children
            .insert(String::from(name), result.clone());
        if file_type == FileType::Dir {
            inode.metadata.nlinks += 1;
        }
        Ok(result)
    }

    fn link(&self, name: &str, other: &Arc<dyn IndexNode>) -> Result<(), SyscallErr> {
        let other_inode: &LockedRamFSInode = other
            .as_any_ref()
            .downcast_ref::<LockedRamFSInode>()
            .ok_or(SyscallErr::EINVAL)?;
        let mut inode = self.0.lock();
        let mut other_locked = other_inode.0.lock();

        if inode.metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        if other_locked.metadata.file_type == FileType::Dir {
            return Err(SyscallErr::EISDIR);
        }
        if inode.children.contains_key(name) {
            return Err(SyscallErr::EEXIST);
        }

        inode.children.insert(
            String::from(name),
            other_locked.self_ref.upgrade().ok_or(SyscallErr::ENOENT)?,
        );
        other_locked.metadata.nlinks += 1;
        Ok(())
    }

    fn unlink(&self, name: &str) -> Result<(), SyscallErr> {
        let mut inode = self.0.lock();
        if inode.metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        let child = inode
            .children
            .remove(name)
            .ok_or(SyscallErr::ENOENT)?;
        let child_inode: &LockedRamFSInode = child
            .as_any_ref()
            .downcast_ref::<LockedRamFSInode>()
            .ok_or(SyscallErr::EINVAL)?;
        let child_pages: usize = {
            let mut child_locked = child_inode.0.lock();
            child_locked.metadata.nlinks -= 1;
            child_locked.pages.len()
        };
        // 回退配额计数：释放文件占用的物理页
        if let Some(ref ramfs) = inode.fs.upgrade() {
            if ramfs.max_pages > 0 {
                let mut page_count = ramfs.page_count.lock();
                *page_count = page_count.saturating_sub(child_pages);
            }
        }
        Ok(())
    }

    fn rename(
        &self,
        old_name: &str,
        new_parent: &Arc<dyn IndexNode>,
        new_name: &str,
    ) -> Result<(), SyscallErr> {
        let new_parent_inode: &LockedRamFSInode = new_parent
            .as_any_ref()
            .downcast_ref::<LockedRamFSInode>()
            .ok_or(SyscallErr::EINVAL)?;

        // Phase 1: remove child from old parent (under old lock)
        let (child, is_dir) = {
            let mut old_locked = self.0.lock();
            let child = old_locked
                .children
                .remove(old_name)
                .ok_or(SyscallErr::ENOENT)?;
            let is_dir = child.0.lock().metadata.file_type == FileType::Dir;
            if is_dir {
                old_locked.metadata.nlinks -= 1;
            }
            (child, is_dir)
        };

        // Phase 2: insert into new parent (under new lock)
        {
            let mut new_locked = new_parent_inode.0.lock();
            if new_locked.children.contains_key(new_name) {
                // Roll back: re-insert into old parent
                let mut old_locked = self.0.lock();
                old_locked.children.insert(String::from(old_name), child);
                if is_dir {
                    old_locked.metadata.nlinks += 1;
                }
                return Err(SyscallErr::EEXIST);
            }
            if is_dir {
                new_locked.metadata.nlinks += 1;
            }
            new_locked.children.insert(String::from(new_name), child);
        }
        Ok(())
    }

    fn rmdir(&self, name: &str) -> Result<(), SyscallErr> {
        let mut inode = self.0.lock();
        if inode.metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        let to_delete = inode.children.get(name).ok_or(SyscallErr::ENOENT)?;
        let mut child_locked = to_delete.0.lock();
        if child_locked.metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        if !child_locked.children.is_empty() {
            return Err(SyscallErr::ENOTEMPTY);
        }
        child_locked.metadata.nlinks -= 1;
        drop(child_locked);
        inode.children.remove(name);
        inode.metadata.nlinks -= 1;
        Ok(())
    }

    fn resize(&self, len: usize) -> Result<(), SyscallErr> {
        let mut inode = self.0.lock();
        if inode.metadata.file_type != FileType::File {
            return Err(SyscallErr::EINVAL);
        }

        if len < inode.file_size {
            // 缩容：释放超出新大小的页，清零最后一页的尾部
            let ramfs_opt: Option<Arc<RamFS>> = inode.fs.upgrade();
            let last_keep_page = if len == 0 {
                // 删除所有页
                let to_remove: Vec<usize> = inode.pages.keys().cloned().collect();
                for idx in &to_remove {
                    inode.pages.remove(idx);
                    if let Some(ref ramfs) = ramfs_opt {
                        if ramfs.max_pages > 0 {
                            let mut page_count = ramfs.page_count.lock();
                            *page_count = page_count.saturating_sub(1);
                        }
                    }
                }
                // last_keep_page 在 len==0 时不使用，设为 0 占位
                0
            } else {
                (len - 1) / PAGE_SIZE
            };

            if len > 0 {
                // 移除完全超出范围的页
                let to_remove: Vec<usize> = inode
                    .pages
                    .keys()
                    .filter(|&&k| k > last_keep_page)
                    .cloned()
                    .collect();
                for idx in &to_remove {
                    inode.pages.remove(idx);
                    if let Some(ref ramfs) = ramfs_opt {
                        if ramfs.max_pages > 0 {
                            let mut page_count = ramfs.page_count.lock();
                            *page_count = page_count.saturating_sub(1);
                        }
                    }
                }

                // 清零最后一页中超出新大小的部分
                let start_zero = len % PAGE_SIZE;
                if start_zero > 0 {
                    let last_page_idx = (len - 1) / PAGE_SIZE;
                    if let Some(frame) = inode.pages.get(&last_page_idx) {
                        let ptr: *mut u8 = page_ptr_mut(frame, start_zero);
                        let count: usize = PAGE_SIZE - start_zero;
                        unsafe {
                            core::ptr::write_bytes(ptr, 0, count);
                        }
                    }
                }
            }
        }
        // 扩容：只更新 file_size，页在 write_at 中按需分配

        inode.file_size = len;
        Ok(())
    }

    fn truncate(&self, len: usize) -> Result<(), SyscallErr> {
        let mut inode = self.0.lock();
        if inode.metadata.file_type == FileType::Dir {
            return Err(SyscallErr::EINVAL);
        }
        if len >= inode.file_size {
            // 扩容：只更新大小，页在 write_at 中按需分配
            inode.file_size = len;
            return Ok(());
        }
        drop(inode);
        // 缩容委托给 resize
        self.resize(len)
    }

    fn get_entry_name(&self, ino: InodeId) -> Result<String, SyscallErr> {
        let inode = self.0.lock();
        if inode.metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        let mut keys: Vec<String> = inode
            .children
            .iter()
            .filter_map(|(k, v)| {
                if v.0.lock().metadata.inode_id == ino {
                    Some(k.clone())
                } else {
                    None
                }
            })
            .collect();
        match keys.len() {
            0 => Err(SyscallErr::ENOENT),
            1 => Ok(keys.remove(0)),
            _ => panic!("ramfs get_entry_name: multiple entries with same inode_id"),
        }
    }

    fn page_cache(&self) -> Option<Arc<NewPageCache>> {
        let mut inner = self.0.lock();
        if inner.metadata.file_type == FileType::Dir {
            return None;
        }
        if let Some(ref pc) = *inner.new_page_cache.lock() {
            return Some(pc.clone());
        }
        let backend = Arc::new(RamFsPageCacheBackend {
            inode: inner.self_ref.clone(),
        });
        let pc: Arc<NewPageCache> = NewPageCache::new();
        pc.set_backend(backend);
        *inner.new_page_cache.lock() = Some(pc.clone());
        Some(pc)
    }

    fn fs(&self) -> Arc<dyn FileSystem> {
        self.0
            .lock()
            .fs
            .upgrade()
            .expect("RamFS inode: fs has been dropped")
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}
