//! TmpFS — 纯内存临时文件系统
//!
//! 参考 RamFS 实现，但使用 PageCache 作为唯一数据存储后端
//! （不使用 BTreeMap<FrameTracker>）。
//!
//! 关键特性：
//! - PageCache-only data storage（unevictable pages）
//! - 动态的 statfs（基于 size_limit / current_size）
//! - 支持挂载选项（max_bytes）
//! - 重命名时的 inode_id 锁排序防死锁
//! - 目录结构用 `BTreeMap`

use alloc::{
    collections::BTreeMap,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::{Mutex, MutexGuard};

use crate::config::PAGE_SIZE;
use crate::fs::page_cache::{PageCache, PageCacheBackend};
use crate::mm::frame_alloc;
use crate::utils::error::SyscallErr;

use super::vfs::{
    generate_inode_id, FileFlags, FilePrivateData, FileSystem, FileType, FsInfo, IndexNode,
    InodeFlags, InodeId, InodeMode, Metadata, SuperBlock,
};

// ── 常量 ─────────────────────────────────────────────────────────────────

/// TmpFS inode 名称最大长度
const TMPFS_MAX_NAMELEN: usize = 255;
/// Linux tmpfs magic
const TMPFS_MAGIC: u64 = 0x0102_1994;
/// statfs 块大小（字节）
const TMPFS_BLOCK_SIZE: u64 = 4096;

// ── TmpfsPageCacheBackend ───────────────────────────────────────────────

/// TmpFS 的 PageCache 后端 — 数据仅存于内存，无持久化存储
struct TmpfsPageCacheBackend;

impl PageCacheBackend for TmpfsPageCacheBackend {
    fn read_page(&self, _index: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        // TmpFS pages live entirely in PageCache; no persistent backend.
        // Explicitly zero-fill the buffer and report PAGE_SIZE.
        for b in buf.iter_mut() {
            *b = 0;
        }
        Ok(PAGE_SIZE)
    }

    fn write_page(&self, _index: usize, _buf: &[u8]) -> Result<usize, SyscallErr> {
        // 数据留在 PageCache 中，无需写回持久化存储
        Ok(PAGE_SIZE)
    }

    fn npages(&self) -> usize {
        // TmpFS 无后端页数限制，页面不可回收
        0
    }
}

// ── LockedTmpFSInode ─────────────────────────────────────────────────────

/// 带锁的 TmpFS inode 包装器
#[derive(Debug)]
pub struct LockedTmpFSInode(pub Mutex<TmpFSInode>);

// ── TmpFS ────────────────────────────────────────────────────────────────

/// TmpFS 文件系统实例
#[derive(Debug)]
pub struct TmpFS {
    root_inode: Arc<LockedTmpFSInode>,
    self_ref: Mutex<Weak<TmpFS>>,
    /// 文件系统大小上限（字节），None = 无限制
    size_limit: Mutex<Option<u64>>,
    /// 当前已使用的字节数（近似：所有 inode file_size 之和）
    current_size: AtomicU64,
}

// ── TmpFSInode ───────────────────────────────────────────────────────────

/// TmpFS inode 内部数据（不加锁）
pub struct TmpFSInode {
    /// 父目录弱引用
    parent: Weak<LockedTmpFSInode>,
    /// 自身弱引用
    self_ref: Weak<LockedTmpFSInode>,
    /// 子项 B 树
    children: BTreeMap<String, Arc<LockedTmpFSInode>>,
    /// PageCache（普通文件和符号链接在创建时初始化）
    page_cache: Option<Arc<PageCache>>,
    /// 扩展属性 (user.* only)
    xattrs: BTreeMap<String, Vec<u8>>,
    /// 逻辑文件大小（字节）
    file_size: usize,
    /// 元数据
    metadata: Metadata,
    /// 所属文件系统弱引用
    fs: Weak<TmpFS>,
}

impl core::fmt::Debug for TmpFSInode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TmpFSInode")
            .field("file_size", &self.file_size)
            .field("children", &self.children.len())
            .field("metadata", &self.metadata)
            .field("page_cache", &self.page_cache.is_some())
            .finish()
    }
}

impl TmpFSInode {
    /// 创建新的 TmpFS inode（默认类型为目录）
    pub fn new() -> Self {
        Self {
            parent: Weak::default(),
            self_ref: Weak::default(),
            children: BTreeMap::new(),
            page_cache: None,
            xattrs: BTreeMap::new(),
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

    /// 为该 inode 初始化 PageCache（用于普通文件和符号链接）
    fn init_page_cache(&mut self) {
        if self.page_cache.is_some() {
            return;
        }
        let ft = self.metadata.file_type;
        if ft != FileType::File && ft != FileType::SymLink {
            return;
        }
        let pc = PageCache::new();
        pc.set_unevictable(true);
        let backend: Arc<dyn PageCacheBackend> = Arc::new(TmpfsPageCacheBackend);
        pc.set_backend(backend);
        self.page_cache = Some(pc);
    }
}

/// Check if `target_id` is an ancestor of `inode` (i.e. `inode` is a descendant of `target_id`).
/// Walks the parent chain UPWARD from `inode`, locking ONE inode at a time.
/// This is safe to call when no directory locks are held — no deadlock risk.
fn is_ancestor_of(inode: &LockedTmpFSInode, target_id: InodeId) -> bool {
    let mut current_id = inode.0.lock().metadata.inode_id;
    if current_id == target_id {
        return true;
    }
    let mut current_arc = {
        let guard = inode.0.lock();
        guard.parent.upgrade()
    };
    while let Some(p) = current_arc {
        let p_guard = p.0.lock();
        let p_id = p_guard.metadata.inode_id;
        if p_id == target_id {
            return true;
        }
        // Reached root (parent points to itself) — stop
        if p_id == current_id {
            return false;
        }
        current_id = p_id;
        let p_parent = p_guard.parent.upgrade();
        drop(p_guard);
        current_arc = p_parent;
    }
    false
}

// ── FileSystem impl for TmpFS ────────────────────────────────────────────

impl FileSystem for TmpFS {
    fn root_inode(&self) -> Arc<dyn IndexNode> {
        self.root_inode.clone()
    }

    fn info(&self) -> FsInfo {
        FsInfo {
            blk_dev_id: 0,
            max_name_len: TMPFS_MAX_NAMELEN,
            features: alloc::vec![],
        }
    }

    fn name(&self) -> &str {
        "tmpfs"
    }

    fn super_block(&self) -> SuperBlock {
        // 动态 statfs：根据 size_limit 和 current_size 计算
        let limit = *self.size_limit.lock();
        let used = self.current_size.load(Ordering::Acquire);
        let bsize = TMPFS_BLOCK_SIZE;

        let (total, free) = match limit {
            Some(max_bytes) if max_bytes > 0 => {
                let total_blocks = ((max_bytes + bsize - 1) / bsize).max(1);
                let used_blocks = if used > 0 { (used + TMPFS_BLOCK_SIZE - 1) / TMPFS_BLOCK_SIZE } else { 0 };
                let free_blocks = total_blocks.saturating_sub(used_blocks);
                (total_blocks, free_blocks)
            }
            _ => {
                // 无限制：基于可用物理内存计算
                let available_frames = crate::mm::unallocated_frames() as u64;
                let available_bytes = available_frames * PAGE_SIZE as u64;
                let used_blocks = if used > 0 { (used + TMPFS_BLOCK_SIZE - 1) / TMPFS_BLOCK_SIZE } else { 0 };
                let avail_blocks = available_bytes / bsize;
                let total_blocks = used_blocks + avail_blocks;
                (total_blocks, avail_blocks)
            }
        };

        SuperBlock {
            f_type: TMPFS_MAGIC,
            f_bsize: bsize,
            f_blocks: total,
            f_bfree: free,
            f_bavail: free,
            f_namelen: TMPFS_MAX_NAMELEN as u64,
            f_frsize: bsize,
            ..SuperBlock::default()
        }
    }

    fn statfs(&self, _inode: &Arc<dyn IndexNode>) -> Result<SuperBlock, SyscallErr> {
        Ok(self.super_block())
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}

// ── TmpFS 构造 ───────────────────────────────────────────────────────────

impl TmpFS {
    /// 创建无大小限制的 TmpFS
    pub fn new() -> Arc<Self> {
        Self::new_with_options(0)
    }

    /// 创建带大小限制的 TmpFS
    /// - `max_bytes`: 0 = 不限制
    pub fn new_with_options(max_bytes: u64) -> Arc<Self> {
        let size_limit = if max_bytes == 0 { None } else { Some(max_bytes) };
        Self::new_inner(size_limit)
    }

    fn new_inner(size_limit: Option<u64>) -> Arc<Self> {
        let root: Arc<LockedTmpFSInode> =
            Arc::new(LockedTmpFSInode(Mutex::new(TmpFSInode::new())));

        let result: Arc<TmpFS> = Arc::new(TmpFS {
            root_inode: root,
            self_ref: Mutex::new(Weak::new()),
            size_limit: Mutex::new(size_limit),
            current_size: AtomicU64::new(0),
        });

        // 设置自引用
        *result.self_ref.lock() = Arc::downgrade(&result);

        // 初始化 root inode 的 parent / self_ref / fs
        let mut root_guard: MutexGuard<TmpFSInode> = result.root_inode.0.lock();
        root_guard.parent = Arc::downgrade(&result.root_inode);
        root_guard.self_ref = Arc::downgrade(&result.root_inode);
        root_guard.fs = Arc::downgrade(&result);
        drop(root_guard);

        result
    }

    /// 获取 TmpFS 的 Weak 引用（用于创建子 inode 时传递）
    pub fn downgrade(self: &Arc<Self>) -> Weak<TmpFS> {
        Arc::downgrade(self)
    }

    /// 添加或移除已用字节数
    fn add_size(&self, delta: i64) {
        if delta > 0 {
            self.current_size
                .fetch_add(delta as u64, Ordering::Relaxed);
        } else if delta < 0 {
            let sub = (-delta) as u64;
            self.current_size.fetch_sub(sub, Ordering::Relaxed);
        }
    }

    /// 检查是否有足够空间容纳额外字节
    fn check_space(&self, needed: u64) -> Result<(), SyscallErr> {
        let limit = *self.size_limit.lock();
        if let Some(max) = limit {
            if max > 0 {
                let current = self.current_size.load(Ordering::Acquire);
                if current.saturating_add(needed) > max {
                    return Err(SyscallErr::ENOSPC);
                }
            }
        }
        Ok(())
    }
}

// ── IndexNode impl for LockedTmpFSInode ──────────────────────────────────

impl IndexNode for LockedTmpFSInode {
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
        let inode: MutexGuard<TmpFSInode> = self.0.lock();
        if inode.metadata.file_type == FileType::Dir {
            return Err(SyscallErr::EISDIR);
        }

        // 超出文件末尾 → 返回 0
        if offset >= inode.file_size {
            return Ok(0);
        }

        let effective_len = len.min(inode.file_size - offset);

        // 使用 PageCache 读取
        let pc = inode.page_cache.as_ref().ok_or(SyscallErr::EIO)?;
        let read_buf = &mut buf[..effective_len];
        // Pre-fill with zeros so holes (sparse regions) return zero
        read_buf.fill(0);
        pc.read(offset, read_buf)
    }

    fn read_at_user(
        &self,
        offset: usize,
        len: usize,
        dst: &mut crate::mm::UserBuffer,
    ) -> Result<usize, SyscallErr> {
        let inode: MutexGuard<TmpFSInode> = self.0.lock();
        if inode.metadata.file_type == FileType::Dir {
            return Err(SyscallErr::EISDIR);
        }
        if offset >= inode.file_size {
            return Ok(0);
        }
        let read_len = len.min(inode.file_size - offset);
        let pc = inode.page_cache.as_ref().ok_or(SyscallErr::EIO)?;
        // Clone Arc to release inode lock before PageCache accesses
        let pc = pc.clone();
        drop(inode);
        // Pre-fill with zeros so holes return zero
        dst.fill_at(0, read_len, 0);
        pc.read_user(offset, read_len, dst)
    }

    fn write_at_user(
        &self,
        offset: usize,
        len: usize,
        src: &crate::mm::UserBuffer,
    ) -> Result<usize, SyscallErr> {
        if len == 0 {
            return Ok(0);
        }

        let mut inode: MutexGuard<TmpFSInode> = self.0.lock();
        if inode.metadata.file_type == FileType::Dir {
            return Err(SyscallErr::EISDIR);
        }

        let pc = inode.page_cache.as_ref().ok_or(SyscallErr::EIO)?;
        let pc = pc.clone();

        let new_size = offset.checked_add(len).ok_or(SyscallErr::EINVAL)?;

        // 检查空间配额
        if new_size > inode.file_size {
            let delta = (new_size - inode.file_size) as u64;
            let fs = inode.fs.upgrade().ok_or(SyscallErr::EIO)?;
            fs.check_space(delta)?;
        }

        // Hold inode lock through write + size update to avoid race with truncate/resize
        let n = pc.write_user(offset, len, src)?;

        // 更新文件大小
        if new_size > inode.file_size {
            let delta = (new_size - inode.file_size) as u64;
            let fs_lock = inode.fs.upgrade();
            inode.file_size = new_size;
            if let Some(ref fs) = fs_lock {
                fs.add_size(delta as i64);
            }
        }

        Ok(n)
    }

    fn supports_user_buffer_io(&self) -> bool {
        let inode = self.0.lock();
        let ft = inode.metadata.file_type;
        (ft == FileType::File || ft == FileType::SymLink) && inode.page_cache.is_some()
    }

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        let inode = self.0.lock();
        let mut meta = inode.metadata.clone();
        meta.size = inode.file_size as i64;
        // 按 512 字节块数计算 st_blocks
        if inode.file_size > 0 {
            meta.blocks = (inode.file_size + 511) / 512;
        } else {
            meta.blocks = 0;
        }
        meta.blk_size = 512;
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

    fn list_dirents(&self) -> Result<Vec<(String, InodeId, FileType)>, SyscallErr> {
        let mut result = Vec::new();
        for name in self.list()? {
            if let Ok(child) = self.find(&name) {
                if let Ok(meta) = child.metadata() {
                    result.push((name, meta.inode_id, meta.file_type));
                }
            }
        }
        Ok(result)
    }

    fn create(
        &self,
        name: &str,
        file_type: FileType,
        mode: InodeMode,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        self.create_with_data(name, file_type, mode, 0)
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

        let need_page_cache = file_type == FileType::File || file_type == FileType::SymLink;

        let mut child_inner = TmpFSInode {
            parent: inode.self_ref.clone(),
            self_ref: Weak::default(),
            children: BTreeMap::new(),
            page_cache: None,
            xattrs: BTreeMap::new(),
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
        };

        // 为文件和符号链接初始化 PageCache
        if need_page_cache {
            child_inner.init_page_cache();
        }

        let child: Arc<LockedTmpFSInode> =
            Arc::new(LockedTmpFSInode(Mutex::new(child_inner)));

        // 初始化自引用
        child.0.lock().self_ref = Arc::downgrade(&child);

        inode
            .children
            .insert(String::from(name), child.clone());
        if file_type == FileType::Dir {
            inode.metadata.nlinks += 1;
        }
        Ok(child)
    }

    fn create_with_attrs(
        &self,
        name: &str,
        file_type: FileType,
        attrs: crate::fs::vfs::CreateAttrs,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        // Delegate to create_with_data for the heavy lifting, then
        // fix up uid/gid on the child (NOT self/parent).
        let inode = self.create_with_data(name, file_type, attrs.mode, 0)?;
        if let Some(child) = inode.as_any_ref().downcast_ref::<LockedTmpFSInode>() {
            let mut child_inner = child.0.lock();
            child_inner.metadata.uid = attrs.uid;
            child_inner.metadata.gid = attrs.gid;
        }
        Ok(inode)
    }

    fn link(&self, name: &str, other: &Arc<dyn IndexNode>) -> Result<(), SyscallErr> {
        let other_inode: &LockedTmpFSInode = other
            .as_any_ref()
            .downcast_ref::<LockedTmpFSInode>()
            .ok_or(SyscallErr::EXDEV)?;

        let self_fs = self.0.lock().fs.upgrade().ok_or(SyscallErr::EIO)?;
        let other_fs = other_inode.0.lock().fs.upgrade().ok_or(SyscallErr::EIO)?;
        if !Arc::ptr_eq(&self_fs, &other_fs) {
            return Err(SyscallErr::EXDEV);
        }

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
        let child_inode: &LockedTmpFSInode = child
            .as_any_ref()
            .downcast_ref::<LockedTmpFSInode>()
            .ok_or(SyscallErr::EINVAL)?;

        let mut child_locked = child_inode.0.lock();

        // unlink 不可用于目录 — 必须返回 EISDIR
        if child_locked.metadata.file_type == FileType::Dir {
            // 恢复: 刚才 remove 了 child，但这是非法操作
            inode
                .children
                .insert(alloc::string::String::from(name), child.clone());
            return Err(SyscallErr::EISDIR);
        }

        child_locked.metadata.nlinks -= 1;
        let nlinks_after = child_locked.metadata.nlinks;
        let file_sz = child_locked.file_size as i64;
        drop(child_locked);

        // Only release quota when this is the last directory entry
        if nlinks_after == 0 {
            if let Some(ref fs) = inode.fs.upgrade() {
                if file_sz > 0 {
                    fs.add_size(-file_sz);
                }
            }
        }

        Ok(())
    }

    fn rename(
        &self,
        old_name: &str,
        new_parent: &Arc<dyn IndexNode>,
        new_name: &str,
        flags: u32,
    ) -> Result<(), SyscallErr> {
        use crate::fs::vfs::RENAME_NOREPLACE;

        let new_parent_inode: &LockedTmpFSInode = new_parent
            .as_any_ref()
            .downcast_ref::<LockedTmpFSInode>()
            .ok_or(SyscallErr::EXDEV)?;

        let self_fs = self.0.lock().fs.upgrade().ok_or(SyscallErr::EIO)?;
        let new_fs = new_parent_inode.0.lock().fs.upgrade().ok_or(SyscallErr::EIO)?;
        if !Arc::ptr_eq(&self_fs, &new_fs) {
            return Err(SyscallErr::EXDEV);
        }

        if new_parent_inode.0.lock().metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }

        let id_self = self.0.lock().metadata.inode_id;
        let id_new = new_parent_inode.0.lock().metadata.inode_id;

        let (child_is_dir, child_id) = {
            let old_locked = self.0.lock();
            let child = old_locked
                .children
                .get(old_name)
                .ok_or(SyscallErr::ENOENT)?
                .clone();
            let cl = child.0.lock();
            let is_dir = cl.metadata.file_type == FileType::Dir;
            let ino = cl.metadata.inode_id;
            (is_dir, ino)
        };

        if child_is_dir && id_self != id_new {
            if is_ancestor_of(new_parent_inode, child_id) {
                return Err(SyscallErr::EINVAL);
            }
        }

        if id_self == id_new {
            let mut locked = self.0.lock();
            let child = locked
                .children
                .remove(old_name)
                .ok_or(SyscallErr::ENOENT)?;

            if old_name == new_name {
                locked.children.insert(String::from(old_name), child);
                return Ok(());
            }

            let child_is_dir = child.0.lock().metadata.file_type == FileType::Dir;
            let mut release_sz: i64 = 0;
            if flags & RENAME_NOREPLACE != 0 && locked.children.contains_key(new_name) {
                // target exists but caller requested no overwrite — restore source
                locked.children.insert(String::from(old_name), child);
                return Err(SyscallErr::EEXIST);
            }

            if let Some(existing) = locked.children.remove(new_name) {
                if Arc::ptr_eq(&child, &existing) {
                    locked.children.insert(String::from(new_name), existing);
                    locked.children.insert(String::from(old_name), child);
                    return Ok(());
                }

                let existing_is_dir = {
                    let el = existing.0.lock();
                    el.metadata.file_type == FileType::Dir
                };

                if !child_is_dir && existing_is_dir {
                    locked.children.insert(String::from(new_name), existing);
                    locked.children.insert(String::from(old_name), child);
                    return Err(SyscallErr::EISDIR);
                }
                if child_is_dir && !existing_is_dir {
                    locked.children.insert(String::from(new_name), existing);
                    locked.children.insert(String::from(old_name), child);
                    return Err(SyscallErr::ENOTDIR);
                }

                if existing_is_dir {
                    let not_empty = {
                        let el = existing.0.lock();
                        !el.children.is_empty()
                    };
                    if not_empty {
                        locked.children.insert(String::from(new_name), existing);
                        locked.children.insert(String::from(old_name), child);
                        return Err(SyscallErr::ENOTEMPTY);
                    }
                    locked.metadata.nlinks -= 1;
                }

                {
                    let mut el = existing.0.lock();
                    el.metadata.nlinks -= 1;
                    if el.metadata.nlinks == 0 {
                        release_sz = el.file_size as i64;
                    }
                }
            }

            locked.children.insert(String::from(new_name), child);
            let fs = locked.fs.upgrade();
            drop(locked);
            if release_sz > 0 {
                if let Some(ref fs) = fs {
                    fs.add_size(-release_sz);
                }
            }
            return Ok(());
        }

        if id_self < id_new {
            let mut old_locked = self.0.lock();
            let mut new_locked = new_parent_inode.0.lock();

            let child = old_locked
                .children
                .remove(old_name)
                .ok_or(SyscallErr::ENOENT)?;
            let child_is_dir = child.0.lock().metadata.file_type == FileType::Dir;

            let mut release_sz: i64 = 0;
            if flags & RENAME_NOREPLACE != 0 && new_locked.children.contains_key(new_name) {
                old_locked.children.insert(String::from(old_name), child);
                return Err(SyscallErr::EEXIST);
            }
            if let Some(existing) = new_locked.children.remove(new_name) {
                if Arc::ptr_eq(&child, &existing) {
                    new_locked.children.insert(String::from(new_name), existing);
                    old_locked.children.insert(String::from(old_name), child);
                    return Ok(());
                }

                let existing_is_dir = {
                    let el = existing.0.lock();
                    el.metadata.file_type == FileType::Dir
                };

                if !child_is_dir && existing_is_dir {
                    new_locked.children.insert(String::from(new_name), existing);
                    old_locked.children.insert(String::from(old_name), child);
                    return Err(SyscallErr::EISDIR);
                }
                if child_is_dir && !existing_is_dir {
                    new_locked.children.insert(String::from(new_name), existing);
                    old_locked.children.insert(String::from(old_name), child);
                    return Err(SyscallErr::ENOTDIR);
                }

                if existing_is_dir {
                    let not_empty = {
                        let el = existing.0.lock();
                        !el.children.is_empty()
                    };
                    if not_empty {
                        new_locked.children.insert(String::from(new_name), existing);
                        old_locked.children.insert(String::from(old_name), child);
                        return Err(SyscallErr::ENOTEMPTY);
                    }
                    new_locked.metadata.nlinks -= 1;
                }

                {
                    let mut el = existing.0.lock();
                    el.metadata.nlinks -= 1;
                    if el.metadata.nlinks == 0 {
                        release_sz = el.file_size as i64;
                    }
                }
            }

            if child_is_dir {
                old_locked.metadata.nlinks -= 1;
                new_locked.metadata.nlinks += 1;
                let parent_weak = new_locked.self_ref.clone();
                drop(old_locked);
                child.0.lock().parent = parent_weak;
            } else {
                drop(old_locked);
            }

            new_locked.children.insert(String::from(new_name), child);
            drop(new_locked);

            if release_sz > 0 {
                self_fs.add_size(-release_sz);
            }
        } else {
            let mut new_locked = new_parent_inode.0.lock();
            let mut old_locked = self.0.lock();

            let child = old_locked
                .children
                .remove(old_name)
                .ok_or(SyscallErr::ENOENT)?;
            let child_is_dir = child.0.lock().metadata.file_type == FileType::Dir;

            let mut release_sz: i64 = 0;
            if flags & RENAME_NOREPLACE != 0 && new_locked.children.contains_key(new_name) {
                old_locked.children.insert(String::from(old_name), child);
                return Err(SyscallErr::EEXIST);
            }
            if let Some(existing) = new_locked.children.remove(new_name) {
                if Arc::ptr_eq(&child, &existing) {
                    new_locked.children.insert(String::from(new_name), existing);
                    old_locked.children.insert(String::from(old_name), child);
                    return Ok(());
                }

                let existing_is_dir = {
                    let el = existing.0.lock();
                    el.metadata.file_type == FileType::Dir
                };

                if !child_is_dir && existing_is_dir {
                    new_locked.children.insert(String::from(new_name), existing);
                    old_locked.children.insert(String::from(old_name), child);
                    return Err(SyscallErr::EISDIR);
                }
                if child_is_dir && !existing_is_dir {
                    new_locked.children.insert(String::from(new_name), existing);
                    old_locked.children.insert(String::from(old_name), child);
                    return Err(SyscallErr::ENOTDIR);
                }

                if existing_is_dir {
                    let not_empty = {
                        let el = existing.0.lock();
                        !el.children.is_empty()
                    };
                    if not_empty {
                        new_locked.children.insert(String::from(new_name), existing);
                        old_locked.children.insert(String::from(old_name), child);
                        return Err(SyscallErr::ENOTEMPTY);
                    }
                    new_locked.metadata.nlinks -= 1;
                }

                {
                    let mut el = existing.0.lock();
                    el.metadata.nlinks -= 1;
                    if el.metadata.nlinks == 0 {
                        release_sz = el.file_size as i64;
                    }
                }
            }

            if child_is_dir {
                old_locked.metadata.nlinks -= 1;
                new_locked.metadata.nlinks += 1;
                let parent_weak = new_locked.self_ref.clone();
                drop(old_locked);
                child.0.lock().parent = parent_weak;
            } else {
                drop(old_locked);
            }

            new_locked.children.insert(String::from(new_name), child);
            drop(new_locked);

            if release_sz > 0 {
                self_fs.add_size(-release_sz);
            }
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
        if inode.metadata.file_type != FileType::File
            && inode.metadata.file_type != FileType::SymLink
        {
            return Err(SyscallErr::EINVAL);
        }

        let old_size = inode.file_size;

        if len >= old_size {
            // 扩容：只更新 file_size，页在 write_at 中按需分配
            if len > old_size {
                let delta = (len - old_size) as u64;
                // 检查配额
                let fs = inode.fs.upgrade().ok_or(SyscallErr::EIO)?;
                fs.check_space(delta)?;
                inode.file_size = len;
                fs.add_size(delta as i64);
            }
            drop(inode);
            return Ok(());
        }

        // 缩容
        let delta = (old_size - len) as u64;
        let pc = inode.page_cache.as_ref();

        if let Some(pc) = pc {
            // 使用 PageCache 截断
            pc.truncate(len)?;

            // Zero-fill the tail of the last retained page to prevent data leaks
            let page_end = ((len + PAGE_SIZE - 1) / PAGE_SIZE) * PAGE_SIZE;
            let tail_start = len;
            if tail_start < page_end {
                let zero_buf = alloc::vec![0u8; page_end - tail_start];
                let _ = pc.write(tail_start, &zero_buf);
            }
        }

        inode.file_size = len;

        let fs = inode.fs.upgrade();
        if let Some(ref fs) = fs {
            fs.add_size(-(delta as i64));
        }
        drop(inode);

        Ok(())
    }

    fn truncate(&self, len: usize) -> Result<(), SyscallErr> {
        let inode = self.0.lock();
        if inode.metadata.file_type == FileType::Dir {
            return Err(SyscallErr::EINVAL);
        }
        if len >= inode.file_size {
            // 扩容：只更新大小，页在 write_at 中按需分配
            let old_size = inode.file_size;
            if len > old_size {
                let delta = (len - old_size) as u64;
                let fs = inode.fs.upgrade().ok_or(SyscallErr::EIO)?;
                drop(inode);
                fs.check_space(delta)?;
                let mut inode2 = self.0.lock();
                if len > inode2.file_size {
                    inode2.file_size = len;
                    fs.add_size(delta as i64);
                }
                return Ok(());
            }
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
            _ => panic!("tmpfs get_entry_name: multiple entries with same inode_id"),
        }
    }

    fn getxattr(&self, name: &str, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        let inode = self.0.lock();
        let value = inode.xattrs.get(name).ok_or(SyscallErr::ENODATA)?;
        let len = value.len();
        if buf.is_empty() {
            return Ok(len);
        }
        if buf.len() < len {
            return Err(SyscallErr::ERANGE);
        }
        buf[..len].copy_from_slice(value);
        Ok(len)
    }

    fn setxattr(
        &self,
        name: &str,
        value: &[u8],
        flags: u32,
    ) -> Result<usize, SyscallErr> {
        const XATTR_CREATE: u32 = 1;
        const XATTR_REPLACE: u32 = 2;
        let mut inode = self.0.lock();
        let exists = inode.xattrs.contains_key(name);
        if flags & XATTR_CREATE != 0 {
            if exists {
                return Err(SyscallErr::EEXIST);
            }
        }
        if flags & XATTR_REPLACE != 0 {
            if !exists {
                return Err(SyscallErr::ENODATA);
            }
        }
        inode.xattrs.insert(String::from(name), value.to_vec());
        Ok(0)
    }

    fn listxattr(&self, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        let inode = self.0.lock();
        let names: Vec<&String> = inode.xattrs.keys().collect();
        let mut total = 0usize;
        for name in &names {
            total += name.len() + 1;
        }
        if total == 0 {
            return Ok(0);
        }
        if buf.is_empty() {
            return Ok(total);
        }
        if buf.len() < total {
            return Err(SyscallErr::ERANGE);
        }
        let mut pos = 0;
        for name in &names {
            let bytes = name.as_bytes();
            buf[pos..pos + bytes.len()].copy_from_slice(bytes);
            pos += bytes.len();
            buf[pos] = 0;
            pos += 1;
        }
        Ok(total)
    }

    fn removexattr(&self, name: &str) -> Result<usize, SyscallErr> {
        let mut inode = self.0.lock();
        match inode.xattrs.remove(name) {
            Some(_) => Ok(0),
            None => Err(SyscallErr::ENODATA),
        }
    }

    fn page_cache(&self) -> Option<Arc<PageCache>> {
        let inner = self.0.lock();
        inner.page_cache.clone()
    }

    fn fs(&self) -> Arc<dyn FileSystem> {
        self.0
            .lock()
            .fs
            .upgrade()
            .expect("TmpFS inode: fs has been dropped")
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}
