//! procfs — /proc 伪文件系统
//!
//! 仿照 DragonOS `kernel/src/filesystem/procfs/` 设计，适配 oskernel2026-mango 的 VFS 架构。
//!
//! - 文件内容通过 `ProcContentFn` 函数指针动态生成
//! - 目录结构通过 `BTreeMap<String, Arc<dyn IndexNode>>` 管理
//! - 符号链接的 target 路径存储在 `symlink_target` 字段中

use alloc::{
    collections::BTreeMap,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::any::Any;
use spin::{Mutex, MutexGuard};

use crate::utils::error::SyscallErr;

use super::vfs::{
    generate_inode_id, FileFlags, FilePrivateData, FileSystem, FileType, FsInfo, IndexNode,
    InodeFlags, InodeId, InodeMode, Metadata, SuperBlock,
};

pub mod files;
pub mod pid;

const PROC_SUPER_MAGIC: u64 = 0x9fa0;
const PROCFS_MAX_NAMELEN: u64 = 255;
const PROCFS_SYMLINK_MAX: usize = 64;

// ── ProcContentFn ──────────────────────────────────────────────────────

/// 文件内容生成函数
///
/// `extra_data` 由 inode 携带（如 PID）。返回实际拷贝的字节数。
pub type ProcContentFn = fn(
    extra_data: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr>;

/// 动态查找钩子：当 children BTreeMap 中找不到时调用
pub type FindHookFn = fn(inode: &LockedProcInode, name: &str) -> Option<Arc<dyn IndexNode>>;

/// 动态列表钩子：在 list() 时返回额外的子项名称
pub type ListHookFn = fn(inode: &LockedProcInode) -> Vec<String>;

// ── ProcFS ─────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct ProcFS {
    root_inode: Arc<LockedProcInode>,
    self_ref: Mutex<Weak<ProcFS>>,
}

// ── LockedProcInode ────────────────────────────────────────────────────

#[derive(Debug)]
pub struct LockedProcInode(pub Mutex<ProcInodeData>);

// ── ProcInodeData ──────────────────────────────────────────────────────

#[derive(Debug)]
pub struct ProcInodeData {
    pub parent: Weak<LockedProcInode>,
    pub self_ref: Weak<LockedProcInode>,
    pub fs: Weak<ProcFS>,
    pub metadata: Metadata,
    pub children: BTreeMap<String, Arc<dyn IndexNode>>,
    pub content_fn: Option<ProcContentFn>,
    pub extra_data: usize,
    /// 符号链接目标路径（仅 SymLink inode）
    pub symlink_target: Option<String>,
    /// 动态查找钩子（如 /proc 根目录的 PID 查找）
    pub find_hook: Option<FindHookFn>,
    /// 动态列表钩子（如 /proc 根目录的 PID 枚举）
    pub list_hook: Option<ListHookFn>,
}

// ── ProcInodeData 构造 ─────────────────────────────────────────────────

impl ProcInodeData {
    fn new_dir(mode: InodeMode) -> Self {
        Self {
            parent: Weak::default(),
            self_ref: Weak::default(),
            fs: Weak::default(),
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
                mode,
                nlinks: 2,
                uid: 0,
                gid: 0,
                raw_dev: 0,
                flags: InodeFlags::empty(),
            },
            children: BTreeMap::new(),
            content_fn: None,
            extra_data: 0,
            symlink_target: None,
            find_hook: None,
            list_hook: None,
        }
    }

    fn new_file(mode: InodeMode, content_fn: ProcContentFn) -> Self {
        Self {
            parent: Weak::default(),
            self_ref: Weak::default(),
            fs: Weak::default(),
            metadata: Metadata {
                dev_id: 0,
                inode_id: generate_inode_id(),
                size: 0,
                blk_size: 0,
                blocks: 0,
                atime: crate::timer::TimeSpec::new(),
                mtime: crate::timer::TimeSpec::new(),
                ctime: crate::timer::TimeSpec::new(),
                file_type: FileType::File,
                mode,
                nlinks: 1,
                uid: 0,
                gid: 0,
                raw_dev: 0,
                flags: InodeFlags::empty(),
            },
            children: BTreeMap::new(),
            content_fn: Some(content_fn),
            extra_data: 0,
            symlink_target: None,
            find_hook: None,
            list_hook: None,
        }
    }

    fn new_symlink(target: &str) -> Self {
        let target_bytes = target.as_bytes();
        let mut data = Self {
            parent: Weak::default(),
            self_ref: Weak::default(),
            fs: Weak::default(),
            metadata: Metadata {
                dev_id: 0,
                inode_id: generate_inode_id(),
                size: target_bytes.len() as i64,
                blk_size: 0,
                blocks: 0,
                atime: crate::timer::TimeSpec::new(),
                mtime: crate::timer::TimeSpec::new(),
                ctime: crate::timer::TimeSpec::new(),
                file_type: FileType::SymLink,
                mode: InodeMode::S_IFLNK | InodeMode::S_IRWXUGO,
                nlinks: 1,
                uid: 0,
                gid: 0,
                raw_dev: 0,
                flags: InodeFlags::empty(),
            },
            children: BTreeMap::new(),
            content_fn: None,
            extra_data: 0,
            symlink_target: Some(String::from(target)),
            find_hook: None,
            list_hook: None,
        };
        // We set the actual size to match the target length so vfs_lookup
        // symlink resolution allocates the right buffer.
        assert!(
            target_bytes.len() <= PROCFS_SYMLINK_MAX,
            "symlink target too long"
        );
        data
    }
}

// ── LockedProcInode 构造方法 ───────────────────────────────────────────

impl LockedProcInode {
    /// 创建目录 inode（已完成 parent/self_ref/fs 绑定）
    fn new_dir_wired(
        parent: Weak<LockedProcInode>,
        fs: Weak<ProcFS>,
        mode: InodeMode,
    ) -> Arc<Self> {
        let mut data = ProcInodeData::new_dir(mode);
        data.parent = parent;
        data.fs = fs;
        Arc::new_cyclic(|weak| {
            data.self_ref = weak.clone();
            LockedProcInode(Mutex::new(data))
        })
    }

    /// 创建文件 inode（已完成 parent/self_ref/fs 绑定）
    fn new_file_wired(
        parent: Weak<LockedProcInode>,
        fs: Weak<ProcFS>,
        mode: InodeMode,
        content_fn: ProcContentFn,
        extra_data: usize,
    ) -> Arc<Self> {
        let mut data = ProcInodeData::new_file(mode, content_fn);
        data.parent = parent;
        data.fs = fs;
        data.extra_data = extra_data;
        Arc::new_cyclic(|weak| {
            data.self_ref = weak.clone();
            LockedProcInode(Mutex::new(data))
        })
    }

    /// 创建符号链接 inode
    fn new_symlink_wired(
        parent: Weak<LockedProcInode>,
        fs: Weak<ProcFS>,
        target: &str,
    ) -> Arc<Self> {
        let mut data = ProcInodeData::new_symlink(target);
        data.parent = parent;
        data.fs = fs;
        Arc::new_cyclic(|weak| {
            data.self_ref = weak.clone();
            LockedProcInode(Mutex::new(data))
        })
    }

    /// 在当前 inode 下添加子目录，自动设置 parent/fs/nlinks
    pub fn add_dir(
        self: &Arc<Self>,
        name: &str,
        mode: InodeMode,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        let mut this = self.0.lock();
        if this.metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        if this.children.contains_key(name) {
            return Err(SyscallErr::EEXIST);
        }
        if name.len() > PROCFS_MAX_NAMELEN as usize {
            return Err(SyscallErr::ENAMETOOLONG);
        }
        let child = LockedProcInode::new_dir_wired(
            this.self_ref.clone(),
            this.fs.clone(),
            mode,
        );
        this.children.insert(String::from(name), child.clone());
        this.metadata.nlinks += 1;
        Ok(child)
    }

    /// 在当前 inode 下添加子文件，自动设置 parent/fs
    pub fn add_file(
        self: &Arc<Self>,
        name: &str,
        mode: InodeMode,
        content_fn: ProcContentFn,
        extra_data: usize,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        let mut this = self.0.lock();
        if this.metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        if this.children.contains_key(name) {
            return Err(SyscallErr::EEXIST);
        }
        if name.len() > PROCFS_MAX_NAMELEN as usize {
            return Err(SyscallErr::ENAMETOOLONG);
        }
        let child = LockedProcInode::new_file_wired(
            this.self_ref.clone(),
            this.fs.clone(),
            mode,
            content_fn,
            extra_data,
        );
        this.children.insert(String::from(name), child.clone());
        Ok(child)
    }

    /// 在当前 inode 下添加符号链接
    pub fn add_symlink(
        self: &Arc<Self>,
        name: &str,
        target: &str,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        let mut this = self.0.lock();
        if this.metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        if this.children.contains_key(name) {
            return Err(SyscallErr::EEXIST);
        }
        if name.len() > PROCFS_MAX_NAMELEN as usize {
            return Err(SyscallErr::ENAMETOOLONG);
        }
        if target.len() > PROCFS_SYMLINK_MAX {
            return Err(SyscallErr::ENAMETOOLONG);
        }
        let child = LockedProcInode::new_symlink_wired(
            this.self_ref.clone(),
            this.fs.clone(),
            target,
        );
        this.children.insert(String::from(name), child.clone());
        Ok(child)
    }

    /// 在当前 inode 下添加动态符号链接（target 由 content_fn 动态生成）
    pub fn add_dynamic_symlink(
        self: &Arc<Self>,
        name: &str,
        content_fn: ProcContentFn,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        let mut this = self.0.lock();
        if this.metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        if this.children.contains_key(name) {
            return Err(SyscallErr::EEXIST);
        }
        if name.len() > PROCFS_MAX_NAMELEN as usize {
            return Err(SyscallErr::ENAMETOOLONG);
        }
        let mut data = ProcInodeData::new_file(InodeMode::from_bits_truncate(0o777), content_fn);
        data.metadata.file_type = FileType::SymLink;
        data.metadata.mode = InodeMode::S_IFLNK | InodeMode::S_IRWXUGO;
        data.metadata.size = PROCFS_SYMLINK_MAX as i64;
        data.parent = this.self_ref.clone();
        data.fs = this.fs.clone();
        let child = Arc::new_cyclic(|weak| {
            data.self_ref = weak.clone();
            LockedProcInode(Mutex::new(data))
        });
        this.children.insert(String::from(name), child.clone());
        Ok(child)
    }

    /// 在当前目录 inode 上设置动态查找/列表钩子（用于 PID 目录等）
    pub fn set_hooks(&self, find_hook: FindHookFn, list_hook: ListHookFn) {
        let mut this = self.0.lock();
        this.find_hook = Some(find_hook);
        this.list_hook = Some(list_hook);
    }
}

// ── FileSystem impl for ProcFS ─────────────────────────────────────────

impl FileSystem for ProcFS {
    fn root_inode(&self) -> Arc<dyn IndexNode> {
        self.root_inode.clone()
    }

    fn info(&self) -> FsInfo {
        FsInfo {
            blk_dev_id: 0,
            max_name_len: PROCFS_MAX_NAMELEN as usize,
            features: alloc::vec!["proc"],
        }
    }

    fn name(&self) -> &str {
        "proc"
    }

    fn super_block(&self) -> SuperBlock {
        SuperBlock {
            f_type: PROC_SUPER_MAGIC,
            f_bsize: 512,
            f_namelen: PROCFS_MAX_NAMELEN,
            ..SuperBlock::default()
        }
    }

    fn support_readahead(&self) -> bool {
        false
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}

// ── ProcFS 构造 ────────────────────────────────────────────────────────

impl ProcFS {
    /// 创建 ProcFS 实例
    ///
    /// 采用 RamFS 的安全构造模式，通过 lock + 回填设置自引用，避免 unsafe raw pointer。
    pub fn new() -> Arc<Self> {
        let result = Arc::new(ProcFS {
            // 先创建占位 root inode，在下面回填 parent/self_ref/fs
            root_inode: Arc::new(LockedProcInode(Mutex::new(ProcInodeData::new_dir(
                InodeMode::from_bits_truncate(0o555),
            )))),
            self_ref: Mutex::new(Weak::new()),
        });

        // 设置 ProcFS 的 self_ref
        *result.self_ref.lock() = Arc::downgrade(&result);

        // 初始化 root inode: parent(Weak::new() for root), self_ref, fs
        {
            let mut root_guard = result.root_inode.0.lock();
            root_guard.parent = Weak::new(); // root has no parent
            root_guard.self_ref = Arc::downgrade(&result.root_inode);
            root_guard.fs = Arc::downgrade(&result);
        }

        result
    }

    /// 获取根 inode 引用（用于注册子项）
    pub fn root(&self) -> &Arc<LockedProcInode> {
        &self.root_inode
    }
}

// ── IndexNode impl for LockedProcInode ─────────────────────────────────

impl IndexNode for LockedProcInode {
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

        // Extract data without holding lock (prevents deadlock if content_fn
        // needs to access other kernel state)
        let (content_fn, extra_data, file_type, symlink_target) = {
            let data = self.0.lock();
            (
                data.content_fn,
                data.extra_data,
                data.metadata.file_type,
                data.symlink_target.clone(),
            )
        };

        match file_type {
            FileType::Dir => Err(SyscallErr::EISDIR),
            FileType::SymLink => {
                if let Some(ref target) = symlink_target {
                    proc_read_str(offset, len, buf, target)
                } else if let Some(f) = content_fn {
                    let n = f(extra_data, offset, len, buf)?;
                    if n > len || n > buf.len() {
                        return Err(SyscallErr::EIO);
                    }
                    Ok(n)
                } else {
                    Err(SyscallErr::ENOSYS)
                }
            }
            _ => match content_fn {
                Some(f) => {
                    let n = f(extra_data, offset, len, buf)?;
                    if n > len || n > buf.len() {
                        return Err(SyscallErr::EIO);
                    }
                    Ok(n)
                }
                None => Err(SyscallErr::ENOSYS),
            },
        }
    }

    fn write_at(
        &self,
        _offset: usize,
        _len: usize,
        _buf: &[u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        Err(SyscallErr::EPERM)
    }

    fn find(&self, name: &str) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        let (self_ref_arc, parent_arc, child_arc, find_hook) = {
            let data = self.0.lock();
            if data.metadata.file_type != FileType::Dir {
                return Err(SyscallErr::ENOTDIR);
            }
            let self_ref = data
                .self_ref
                .upgrade()
                .map(|n| n as Arc<dyn IndexNode>);
            let parent = data
                .parent
                .upgrade()
                .map(|n| n as Arc<dyn IndexNode>)
                .or_else(|| {
                    data.self_ref
                        .upgrade()
                        .map(|n| n as Arc<dyn IndexNode>)
                });
            let child = data.children.get(name).cloned();
            let hook = data.find_hook;
            (self_ref, parent, child, hook)
        }; // lock released here — hook runs outside

        match name {
            "" | "." => self_ref_arc.ok_or(SyscallErr::ENOENT),
            ".." => parent_arc.ok_or(SyscallErr::ENOENT),
            name => child_arc
                .or_else(|| find_hook.and_then(|h| h(self, name)))
                .ok_or(SyscallErr::ENOENT),
        }
    }

    fn list(&self) -> Result<Vec<String>, SyscallErr> {
        let (file_type, children_keys, list_hook) = {
            let data = self.0.lock();
            if data.metadata.file_type != FileType::Dir {
                return Err(SyscallErr::ENOTDIR);
            }
            let keys: Vec<String> = data.children.keys().cloned().collect();
            (data.metadata.file_type, keys, data.list_hook)
        };
        let mut keys = Vec::new();
        keys.push(String::from("."));
        keys.push(String::from(".."));
        keys.extend(children_keys);
        if let Some(h) = list_hook {
            keys.extend(h(self));
        }
        Ok(keys)
    }

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        Ok(self.0.lock().metadata.clone())
    }

    fn set_metadata(&self, metadata: &Metadata) -> Result<(), SyscallErr> {
        let mut data = self.0.lock();
        // procfs is read-only: only allow timestamp updates (like Linux)
        data.metadata.atime = metadata.atime;
        data.metadata.mtime = metadata.mtime;
        data.metadata.ctime = metadata.ctime;
        // mode/uid/gid changes are rejected
        if metadata.mode != data.metadata.mode
            || metadata.uid != data.metadata.uid
            || metadata.gid != data.metadata.gid
        {
            return Err(SyscallErr::EPERM);
        }
        Ok(())
    }

    fn get_entry_name(&self, ino: InodeId) -> Result<String, SyscallErr> {
        // Avoid nested locks: extract parent Arc and children Arc list
        // under a short-lived lock, then inspect them without holding self lock.
        let (file_type, own_inode_id, parent, children) = {
            let data = self.0.lock();
            if data.metadata.file_type != FileType::Dir {
                return Err(SyscallErr::ENOTDIR);
            }
            let parent = data.parent.upgrade();
            let children: Vec<(String, Arc<dyn IndexNode>)> = data
                .children
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            (data.metadata.file_type, data.metadata.inode_id, parent, children)
        };

        if own_inode_id == ino {
            return Ok(String::from("."));
        }
        if let Some(ref p) = parent {
            if p.0.lock().metadata.inode_id == ino {
                return Ok(String::from(".."));
            }
        }

        // Scan children (lock released, each child locks only itself)
        let mut matches: Vec<String> = children
            .into_iter()
            .filter_map(|(name, child)| {
                child.metadata().ok().and_then(|m| {
                    if m.inode_id == ino {
                        Some(name)
                    } else {
                        None
                    }
                })
            })
            .collect();

        match matches.len() {
            0 => Err(SyscallErr::ENOENT),
            1 => Ok(matches.remove(0)),
            _ => panic!(
                "procfs get_entry_name: multiple entries with inode_id={}",
                ino
            ),
        }
    }

    fn resize(&self, _len: usize) -> Result<(), SyscallErr> {
        Err(SyscallErr::EPERM)
    }

    fn truncate(&self, len: usize) -> Result<(), SyscallErr> {
        self.resize(len)
    }

    fn fs(&self) -> Arc<dyn FileSystem> {
        self.0
            .lock()
            .fs
            .upgrade()
            .expect("ProcFS inode: fs has been dropped")
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}

// ── 工具函数 ───────────────────────────────────────────────────────────

/// 通用 proc 文件读取：将字符串按 offset/len 拷贝到 buf
pub fn proc_read_str(
    offset: usize,
    len: usize,
    buf: &mut [u8],
    data: &str,
) -> Result<usize, SyscallErr> {
    let bytes = data.as_bytes();
    if offset >= bytes.len() {
        return Ok(0);
    }
    let available = bytes.len() - offset;
    let copy_len = len.min(available).min(buf.len());
    buf[..copy_len].copy_from_slice(&bytes[offset..offset + copy_len]);
    Ok(copy_len)
}

/// 通用 proc 文件读取：将字节数组按 offset/len 拷贝到 buf
pub fn proc_read_bytes(
    offset: usize,
    len: usize,
    buf: &mut [u8],
    data: &[u8],
) -> Result<usize, SyscallErr> {
    if offset >= data.len() {
        return Ok(0);
    }
    let available = data.len() - offset;
    let copy_len = len.min(available).min(buf.len());
    buf[..copy_len].copy_from_slice(&data[offset..offset + copy_len]);
    Ok(copy_len)
}
