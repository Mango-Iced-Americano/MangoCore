//! RamFS — 纯内存文件系统
//!
//! 参照 DragonOS `kernel/src/filesystem/ramfs/mod.rs` 实现。
//! 所有数据存储在 `Vec<u8>` 中，目录结构用 `BTreeMap`。
//! 用于 VFS 层调试，不依赖任何块设备。

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
}

// ── RamFSInode ────────────────────────────────────────────────────────

/// RamFS inode 内部数据（不加锁）
#[derive(Debug)]
pub struct RamFSInode {
    /// 父目录弱引用
    parent: Weak<LockedRamFSInode>,
    /// 自身弱引用
    self_ref: Weak<LockedRamFSInode>,
    /// 子项 B 树
    children: BTreeMap<String, Arc<LockedRamFSInode>>,
    /// 文件数据
    data: Vec<u8>,
    /// 元数据
    metadata: Metadata,
    /// 所属文件系统弱引用
    fs: Weak<RamFS>,
}

impl RamFSInode {
    pub fn new() -> Self {
        Self {
            parent: Weak::default(),
            self_ref: Weak::default(),
            children: BTreeMap::new(),
            data: Vec::new(),
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
        let root: Arc<LockedRamFSInode> =
            Arc::new(LockedRamFSInode(Mutex::new(RamFSInode::new())));

        let result: Arc<RamFS> = Arc::new(RamFS {
            root_inode: root,
            self_ref: Mutex::new(Weak::new()),
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
        let start = inode.data.len().min(offset);
        let end = inode.data.len().min(offset + len);
        if start >= end {
            return Ok(0);
        }
        let src = &inode.data[start..end];
        buf[..src.len()].copy_from_slice(src);
        Ok(src.len())
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
        let mut inode: MutexGuard<RamFSInode> = self.0.lock();
        if inode.metadata.file_type == FileType::Dir {
            return Err(SyscallErr::EISDIR);
        }
        let data: &mut Vec<u8> = &mut inode.data;
        if offset + len > data.len() {
            data.resize(offset + len, 0);
        }
        let target = &mut data[offset..offset + len];
        target.copy_from_slice(&buf[..len]);
        Ok(len)
    }

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        let inode = self.0.lock();
        let mut meta = inode.metadata.clone();
        meta.size = inode.data.len() as i64;
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
            data: Vec::new(),
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
        if name == "." || name == ".." {
            return Err(SyscallErr::ENOTEMPTY);
        }
        let to_delete = inode.children.get(name).ok_or(SyscallErr::ENOENT)?;
        if to_delete.0.lock().metadata.file_type == FileType::Dir {
            return Err(SyscallErr::EPERM);
        }
        to_delete.0.lock().metadata.nlinks -= 1;
        inode.children.remove(name);
        Ok(())
    }

    fn rmdir(&self, name: &str) -> Result<(), SyscallErr> {
        let mut inode = self.0.lock();
        if inode.metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        let to_delete = inode.children.get(name).ok_or(SyscallErr::ENOENT)?;
        if to_delete.0.lock().metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        to_delete.0.lock().metadata.nlinks -= 1;
        inode.children.remove(name);
        inode.metadata.nlinks -= 1;
        Ok(())
    }

    fn resize(&self, len: usize) -> Result<(), SyscallErr> {
        let mut inode = self.0.lock();
        if inode.metadata.file_type == FileType::File {
            inode.data.resize(len, 0);
            Ok(())
        } else {
            Err(SyscallErr::EINVAL)
        }
    }

    fn truncate(&self, len: usize) -> Result<(), SyscallErr> {
        let mut inode = self.0.lock();
        if inode.metadata.file_type == FileType::Dir {
            return Err(SyscallErr::EINVAL);
        }
        if inode.data.len() > len {
            inode.data.resize(len, 0);
        }
        Ok(())
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
