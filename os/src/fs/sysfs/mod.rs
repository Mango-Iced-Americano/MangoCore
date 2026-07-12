//! sysfs — /sys 伪文件系统
//!
//! 仿照 procfs 设计，适配 oskernel2026-mango 的 VFS 架构。
//!
//! - 文件内容通过 `SysContentFn` 函数指针动态生成，或直接使用 `owned_content` 静态字符串
//! - 目录结构通过 `BTreeMap<String, Arc<dyn IndexNode>>` 管理
//! - 动态目录通过 `FindHookFn` / `ListHookFn` 钩子实现

use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::any::Any;
use core::fmt;
use spin::{Mutex, MutexGuard};

use crate::utils::error::SyscallErr;

use super::vfs::IndexNode;
use super::vfs::{
    FileFlags, FilePrivateData, FileSystem, FileType, FsInfo, InodeId, InodeMode, Metadata,
    SuperBlock,
};

pub mod files;

const SYSFS_MAGIC: u64 = 0x62656572;
const SYSFS_MAX_NAMELEN: u64 = 255;

// ── SysContentFn ───────────────────────────────────────────────────────

/// 文件内容生成函数
///
/// `extra_data` 由 inode 携带。返回实际拷贝的字节数。
pub type SysContentFn =
    fn(extra_data: usize, offset: usize, len: usize, buf: &mut [u8]) -> Result<usize, SyscallErr>;

/// 动态查找钩子：当 children BTreeMap 中找不到时调用
pub type FindHookFn = fn(inode: &SysInode, name: &str) -> Option<Arc<dyn IndexNode>>;

/// 动态列表钩子：在 list() 时返回额外的子项名称
pub type ListHookFn = fn(inode: &SysInode) -> Vec<String>;

/// 写入函数，用于可写 sysfs 文件
/// `extra_data` 由 inode 携带。`offset` 为写入偏移量，`buf` 为待写入数据。
/// 返回实际写入字节数。
pub type SysWriteFn = fn(extra_data: usize, offset: usize, buf: &[u8]) -> Result<usize, SyscallErr>;

// ── SysInodeData ───────────────────────────────────────────────────────

pub struct SysInodeData {
    pub parent: Weak<SysInode>,
    pub self_ref: Weak<SysInode>,
    pub fs: Weak<SysFS>,
    pub metadata: Metadata,
    pub children: BTreeMap<String, Arc<dyn IndexNode>>,
    pub content_fn: Option<SysContentFn>,
    /// Owned file content — no leak, freed when inode is dropped.
    pub owned_content: Option<String>,
    pub write_fn: Option<SysWriteFn>,
    pub writable: bool,
    pub find_hook: Option<FindHookFn>,
    pub list_hook: Option<ListHookFn>,
}

// ── SysInode ───────────────────────────────────────────────────────────

pub struct SysInode {
    pub inner: Mutex<SysInodeData>,
}

impl fmt::Debug for SysInode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SysInode").finish()
    }
}

// ── SysInode 构造方法 ──────────────────────────────────────────────────

impl SysInode {
    /// 构造一个完整的 inode，所有字段初始化（包括 owned_content=None）
    fn new_inner(parent: Weak<SysInode>, fs: Weak<SysFS>, metadata: Metadata) -> Arc<Self> {
        Arc::new_cyclic(|weak| SysInode {
            inner: Mutex::new(SysInodeData {
                parent,
                self_ref: weak.clone(),
                fs,
                metadata,
                children: BTreeMap::new(),
                content_fn: None,
                owned_content: None,
                write_fn: None,
                writable: false,
                find_hook: None,
                list_hook: None,
            }),
        })
    }

    /// 创建目录 inode
    fn new_dir(parent: Weak<SysInode>, fs: Weak<SysFS>, mode: InodeMode) -> Arc<Self> {
        let mut metadata = Metadata::new(FileType::Dir, mode);
        metadata.nlinks = 2;
        Self::new_inner(parent, fs, metadata)
    }

    /// 创建文件 inode（含 content_fn）
    fn new_file(
        parent: Weak<SysInode>,
        fs: Weak<SysFS>,
        mode: InodeMode,
        content_fn: SysContentFn,
    ) -> Arc<Self> {
        let inode = Self::new_inner(parent, fs, Metadata::new(FileType::File, mode));
        inode.inner.lock().content_fn = Some(content_fn);
        inode
    }

    /// 创建目录 inode，绕开 new_inner 直接用显式 Weak refs（用于钩子动态创建）
    fn new_dir_wired(
        parent_weak: Weak<SysInode>,
        fs_weak: Weak<SysFS>,
        mode: InodeMode,
    ) -> Arc<Self> {
        let mut metadata = Metadata::new(FileType::Dir, mode);
        metadata.nlinks = 2;
        Arc::new_cyclic(|weak| SysInode {
            inner: Mutex::new(SysInodeData {
                parent: parent_weak,
                self_ref: weak.clone(),
                fs: fs_weak,
                metadata,
                children: BTreeMap::new(),
                content_fn: None,
                owned_content: None,
                write_fn: None,
                writable: false,
                find_hook: None,
                list_hook: None,
            }),
        })
    }

    /// 在当前 inode 下添加子目录，返回内部类型
    pub fn add_dir_inner(
        self: &Arc<Self>,
        name: &str,
        mode: InodeMode,
    ) -> Result<Arc<SysInode>, SyscallErr> {
        let mut this = self.inner.lock();
        if this.metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        if this.children.contains_key(name) {
            return Err(SyscallErr::EEXIST);
        }
        let parent_weak = this.self_ref.clone();
        let fs_weak = this.fs.clone();
        let child = SysInode::new_dir(parent_weak, fs_weak, mode);
        this.children.insert(String::from(name), child.clone());
        this.metadata.nlinks += 1;
        Ok(child)
    }

    /// 在当前 inode 下添加子目录，返回 trait object
    pub fn add_dir(
        self: &Arc<Self>,
        name: &str,
        mode: InodeMode,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        self.add_dir_inner(name, mode)
            .map(|child| child as Arc<dyn IndexNode>)
    }

    /// 在当前 inode 下添加子文件（含 content_fn）
    pub fn add_file(
        self: &Arc<Self>,
        name: &str,
        mode: InodeMode,
        content_fn: SysContentFn,
    ) -> Result<(), SyscallErr> {
        let mut this = self.inner.lock();
        if this.metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        if this.children.contains_key(name) {
            return Err(SyscallErr::EEXIST);
        }
        let parent_weak = this.self_ref.clone();
        let fs_weak = this.fs.clone();
        let child = SysInode::new_file(parent_weak, fs_weak, mode, content_fn);
        this.children.insert(String::from(name), child);
        Ok(())
    }

    pub fn add_file_owned(
        self: &Arc<Self>,
        name: &str,
        mode: InodeMode,
        content: String,
    ) -> Result<(), SyscallErr> {
        let mut this = self.inner.lock();
        if this.metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        if this.children.contains_key(name) {
            return Err(SyscallErr::EEXIST);
        }
        let parent_weak = this.self_ref.clone();
        let fs_weak = this.fs.clone();
        let child = SysInode::new_inner(parent_weak, fs_weak, Metadata::new(FileType::File, mode));
        child.inner.lock().owned_content = Some(content);
        this.children.insert(String::from(name), child);
        Ok(())
    }

    /// 添加可写文件（含独立的读/写函数）
    pub fn add_writable_file_with_write(
        self: &Arc<Self>,
        name: &str,
        mode: InodeMode,
        content_fn: SysContentFn,
        write_fn: SysWriteFn,
    ) -> Result<(), SyscallErr> {
        let mut this = self.inner.lock();
        if this.metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        if this.children.contains_key(name) {
            return Err(SyscallErr::EEXIST);
        }
        let mut metadata = Metadata::new(FileType::File, mode);
        metadata.nlinks = 1;
        let parent_weak = this.self_ref.clone();
        let fs_weak = this.fs.clone();
        let child = Arc::new_cyclic(|weak| SysInode {
            inner: Mutex::new(SysInodeData {
                parent: parent_weak,
                self_ref: weak.clone(),
                fs: fs_weak,
                metadata,
                children: BTreeMap::new(),
                content_fn: Some(content_fn),
                owned_content: None,
                write_fn: Some(write_fn),
                writable: true,
                find_hook: None,
                list_hook: None,
            }),
        });
        this.children.insert(String::from(name), child);
        Ok(())
    }

    /// 添加仅可写的文件（无读内容，read_at 返回 0）
    pub fn add_write_only_file(
        self: &Arc<Self>,
        name: &str,
        mode: InodeMode,
        write_fn: SysWriteFn,
    ) -> Result<(), SyscallErr> {
        self.add_writable_file_with_write(name, mode, |_, _, _, _| Ok(0), write_fn)
    }

    /// 设置动态查找/列表钩子
    pub fn set_hooks(&self, find_hook: FindHookFn, list_hook: ListHookFn) {
        let mut this = self.inner.lock();
        this.find_hook = Some(find_hook);
        this.list_hook = Some(list_hook);
    }
}

// ── IndexNode impl for SysInode ────────────────────────────────────────

impl IndexNode for SysInode {
    crate::impl_index_node_as_any!(SysInode);

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
        let (file_type, writable, write_fn) = {
            let data = self.inner.lock();
            (data.metadata.file_type, data.writable, data.write_fn)
        };
        if file_type == FileType::File && writable {
            let written = if let Some(f) = write_fn {
                f(0, offset, &buf[..len])?
            } else {
                len
            };
            let now = crate::timer::TimeSpec::new();
            let mut data = self.inner.lock();
            data.metadata.mtime = now;
            data.metadata.ctime = now;
            Ok(written)
        } else {
            Err(SyscallErr::EPERM)
        }
    }

    fn resize(&self, len: usize) -> Result<(), SyscallErr> {
        let file_type = self.inner.lock().metadata.file_type;
        if file_type == FileType::Dir {
            return Err(SyscallErr::EISDIR);
        }
        if len == 0 {
            Ok(())
        } else {
            Err(SyscallErr::EINVAL)
        }
    }

    fn read_at(
        &self,
        offset: usize,
        len: usize,
        buf: &mut [u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        drop(_data); // SysFS doesn't use file-private data — release early
        if buf.len() < len {
            return Err(SyscallErr::EINVAL);
        }

        // Extract fields under short-lived lock; release before rendering
        let (content_fn, owned_content, file_type) = {
            let data = self.inner.lock();
            (
                data.content_fn,
                data.owned_content.clone(),
                data.metadata.file_type,
            )
        };

        match file_type {
            FileType::Dir => Err(SyscallErr::EISDIR),
            _ => {
                // owned_content takes priority over content_fn
                if let Some(s) = owned_content {
                    let bytes = s.as_bytes();
                    if offset >= bytes.len() {
                        return Ok(0);
                    }
                    let n = len.min(bytes.len() - offset).min(buf.len());
                    buf[..n].copy_from_slice(&bytes[offset..offset + n]);
                    return Ok(n);
                }
                match content_fn {
                    Some(f) => {
                        let n = f(0, offset, len, buf)?;
                        if n > len || n > buf.len() {
                            return Err(SyscallErr::EIO);
                        }
                        Ok(n)
                    }
                    None => Err(SyscallErr::ENOSYS),
                }
            }
        }
    }

    fn find(&self, name: &str) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        let (self_ref_arc, parent_arc, child_arc, find_hook) = {
            let data = self.inner.lock();
            if data.metadata.file_type != FileType::Dir {
                return Err(SyscallErr::ENOTDIR);
            }
            let self_ref = data.self_ref.upgrade().map(|n| n as Arc<dyn IndexNode>);
            let parent = data
                .parent
                .upgrade()
                .map(|n| n as Arc<dyn IndexNode>)
                .or_else(|| data.self_ref.upgrade().map(|n| n as Arc<dyn IndexNode>));
            let child = data.children.get(name).cloned();
            let hook = data.find_hook;
            (self_ref, parent, child, hook)
        }; // lock released — hook runs outside

        match name {
            "" | "." => self_ref_arc.ok_or(SyscallErr::ENOENT),
            ".." => parent_arc.ok_or(SyscallErr::ENOENT),
            _ => child_arc
                .or_else(|| find_hook.and_then(|h| h(self, name)))
                .ok_or(SyscallErr::ENOENT),
        }
    }

    fn list(&self) -> Result<Vec<String>, SyscallErr> {
        let (children_keys, list_hook) = {
            let data = self.inner.lock();
            if data.metadata.file_type != FileType::Dir {
                return Err(SyscallErr::ENOTDIR);
            }
            let keys: Vec<String> = data.children.keys().cloned().collect();
            (keys, data.list_hook)
        };

        let mut keys = Vec::new();
        keys.push(String::from("."));
        keys.push(String::from(".."));

        // Deduplicate: skip hook keys that match children keys
        let children_set: BTreeSet<_> = children_keys.iter().cloned().collect();
        keys.extend(children_keys);

        if let Some(h) = list_hook {
            for hk in h(self) {
                if !children_set.contains(&hk) {
                    keys.push(hk);
                }
            }
        }

        Ok(keys)
    }

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        Ok(self.inner.lock().metadata.clone())
    }

    fn set_metadata(&self, metadata: &Metadata) -> Result<(), SyscallErr> {
        let mut data = self.inner.lock();
        // sysfs is read-only: only allow timestamp updates (like Linux)
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
        // under a short-lived lock, then inspect them without holding self lock
        let (file_type, own_inode_id, parent, children) = {
            let data = self.inner.lock();
            if data.metadata.file_type != FileType::Dir {
                return Err(SyscallErr::ENOTDIR);
            }
            let parent = data.parent.upgrade();
            let children: Vec<(String, Arc<dyn IndexNode>)> = data
                .children
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            (
                data.metadata.file_type,
                data.metadata.inode_id,
                parent,
                children,
            )
        };

        if own_inode_id == ino {
            return Ok(String::from("."));
        }
        if let Some(ref p) = parent {
            if p.inner.lock().metadata.inode_id == ino {
                return Ok(String::from(".."));
            }
        }

        // Scan children (lock released, each child locks only itself)
        let mut matches: Vec<String> =
            children
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
                "sysfs get_entry_name: multiple entries with inode_id={}",
                ino
            ),
        }
    }

    fn fs(&self) -> Arc<dyn FileSystem> {
        self.inner
            .lock()
            .fs
            .upgrade()
            .expect("SysFS inode: fs has been dropped")
    }
}

// ── SysFS ──────────────────────────────────────────────────────────────

pub struct SysFS {
    root_inode: Arc<SysInode>,
    self_ref: Mutex<Weak<SysFS>>,
}

impl fmt::Debug for SysFS {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SysFS").finish()
    }
}

// ── FileSystem impl for SysFS ──────────────────────────────────────────

impl FileSystem for SysFS {
    fn root_inode(&self) -> Arc<dyn IndexNode> {
        self.root_inode.clone()
    }

    fn info(&self) -> FsInfo {
        FsInfo {
            blk_dev_id: 0,
            max_name_len: SYSFS_MAX_NAMELEN as usize,
            features: alloc::vec!["sysfs"],
        }
    }

    fn name(&self) -> &str {
        "sysfs"
    }

    fn super_block(&self) -> SuperBlock {
        SuperBlock {
            f_type: SYSFS_MAGIC,
            f_bsize: 512,
            f_namelen: SYSFS_MAX_NAMELEN,
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

// ── SysFS 构造 ─────────────────────────────────────────────────────────

impl SysFS {
    /// 创建 SysFS 实例
    ///
    /// 采用安全构造模式：先创建占位 root inode，再回填 parent/self_ref/fs。
    pub fn new() -> Arc<Self> {
        let mut root_metadata = Metadata::new(FileType::Dir, InodeMode::from_bits_truncate(0o555));
        root_metadata.nlinks = 2;

        let result = Arc::new(SysFS {
            root_inode: Arc::new(SysInode {
                inner: Mutex::new(SysInodeData {
                    parent: Weak::new(),
                    self_ref: Weak::new(),
                    fs: Weak::new(),
                    metadata: root_metadata,
                    children: BTreeMap::new(),
                    content_fn: None,
                    owned_content: None,
                    write_fn: None,
                    writable: false,
                    find_hook: None,
                    list_hook: None,
                }),
            }),
            self_ref: Mutex::new(Weak::new()),
        });

        // 设置 SysFS 的 self_ref
        *result.self_ref.lock() = Arc::downgrade(&result);

        // 初始化 root inode: parent(Weak::new() for root), self_ref, fs
        {
            let mut root_guard = result.root_inode.inner.lock();
            root_guard.parent = Weak::new();
            root_guard.self_ref = Arc::downgrade(&result.root_inode);
            root_guard.fs = Arc::downgrade(&result);
        }

        result
    }

    /// 获取根 inode 引用（用于注册子项）
    pub fn root(&self) -> &Arc<SysInode> {
        &self.root_inode
    }
}
