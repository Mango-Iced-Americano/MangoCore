pub mod null;
pub mod pipe;
pub mod rtc;
pub mod tty;
pub mod zero;
pub mod urandom;

use alloc::sync::Arc;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::any::Any;
use lazy_static::*;
use spin::{Mutex, MutexGuard};

use crate::fs::vfs::file_system::{FileSystem, FsInfo, SuperBlock};
use crate::fs::vfs::{
    FilePrivateData, FileType, IndexNode, InodeFlags, InodeId, InodeMode, Metadata, generate_inode_id,
};
use crate::fs::vfs::file::FileFlags;
use crate::timer::TimeSpec;
use crate::utils::error::SyscallErr;

/// 设备文件系统 — 所有设备文件（tty, null, zero 等）的虚拟文件系统
/// 参照 DragonOS 的 DevFS 实现。
#[derive(Debug)]
pub struct DevFS {
    root_inode: Arc<LockedDevFSInode>,
}

/// DevFS 带锁 inode
#[derive(Debug)]
pub struct LockedDevFSInode(pub Mutex<DevFSInode>);

/// DevFS inode 内部数据
#[derive(Debug)]
pub struct DevFSInode {
    parent: alloc::sync::Weak<LockedDevFSInode>,
    self_ref: alloc::sync::Weak<LockedDevFSInode>,
    children: BTreeMap<String, Arc<dyn IndexNode>>,
    metadata: Metadata,
    fs: alloc::sync::Weak<DevFS>,
}

impl DevFSInode {
    pub fn new(file_type: FileType, mode: InodeMode) -> Self {
        Self {
            parent: alloc::sync::Weak::default(),
            self_ref: alloc::sync::Weak::default(),
            children: BTreeMap::new(),
            metadata: Metadata {
                dev_id: 0,
                inode_id: generate_inode_id(),
                size: 0,
                blk_size: 0,
                blocks: 0,
                atime: TimeSpec::new(),
                mtime: TimeSpec::new(),
                ctime: TimeSpec::new(),
                file_type,
                mode,
                nlinks: if file_type == FileType::Dir { 2 } else { 1 },
                uid: 0,
                gid: 0,
                raw_dev: 0,
                flags: InodeFlags::empty(),
            },
            fs: alloc::sync::Weak::default(),
        }
    }
}

impl FileSystem for DevFS {
    fn root_inode(&self) -> Arc<dyn IndexNode> {
        self.root_inode.clone()
    }
    fn info(&self) -> FsInfo {
        FsInfo {
            blk_dev_id: 0,
            max_name_len: 255,
            features: alloc::vec!["devfs"],
        }
    }
    fn name(&self) -> &str {
        "devfs"
    }
    fn super_block(&self) -> SuperBlock {
        SuperBlock::default()
    }
    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}

impl DevFS {
    pub fn new() -> Arc<Self> {
        let root: Arc<LockedDevFSInode> = Arc::new(LockedDevFSInode(Mutex::new(
            DevFSInode::new(FileType::Dir, InodeMode::from_bits_truncate(0o755)),
        )));

        let devfs = Arc::new(DevFS {
            root_inode: root.clone(),
        });

        // 初始化 root inode 自引用
        let mut root_guard = devfs.root_inode.0.lock();
        root_guard.parent = Arc::downgrade(&root);
        root_guard.self_ref = Arc::downgrade(&root);
        root_guard.fs = Arc::downgrade(&devfs);
        drop(root_guard);

        devfs
    }

    /// 注册设备 inode（直接插入 children map）
    pub fn add_dev(&self, name: &str, dev: Arc<dyn IndexNode>) -> Result<(), SyscallErr> {
        self.root_inode.add_dev(name, dev)
    }

    /// 注册设备目录，例如 Linux 常见的 /dev/misc。
    pub fn add_dir(
        &self,
        name: &str,
        mode: InodeMode,
    ) -> Result<Arc<LockedDevFSInode>, SyscallErr> {
        self.root_inode.add_dir(name, mode)
    }
}

impl LockedDevFSInode {
    pub fn add_dev(&self, name: &str, dev: Arc<dyn IndexNode>) -> Result<(), SyscallErr> {
        let mut this = self.0.lock();
        if this.metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        if this.children.contains_key(name) {
            return Err(SyscallErr::EEXIST);
        }
        this.children.insert(String::from(name), dev);
        Ok(())
    }

    pub fn add_dir(
        &self,
        name: &str,
        mode: InodeMode,
    ) -> Result<Arc<LockedDevFSInode>, SyscallErr> {
        let mut this = self.0.lock();
        if this.metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        if this.children.contains_key(name) {
            return Err(SyscallErr::EEXIST);
        }
        let child = Arc::new_cyclic(|weak| {
            let mut data = DevFSInode::new(FileType::Dir, mode);
            data.parent = this.self_ref.clone();
            data.self_ref = weak.clone();
            data.fs = this.fs.clone();
            LockedDevFSInode(Mutex::new(data))
        });
        this.children.insert(String::from(name), child.clone());
        Ok(child)
    }
}

impl IndexNode for LockedDevFSInode {
    fn open(&self, _data: MutexGuard<FilePrivateData>, _flags: &FileFlags) -> Result<(), SyscallErr> {
        Ok(())
    }
    fn close(&self, _data: MutexGuard<FilePrivateData>) -> Result<(), SyscallErr> {
        Ok(())
    }
    fn read_at(&self, _offset: usize, _len: usize, _buf: &mut [u8], _data: MutexGuard<FilePrivateData>) -> Result<usize, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }
    fn write_at(&self, _offset: usize, _len: usize, _buf: &[u8], _data: MutexGuard<FilePrivateData>) -> Result<usize, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    fn find(&self, name: &str) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        let inode = self.0.lock();
        if inode.metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        match name {
            "" | "." => inode.self_ref.upgrade().map(|n| n as Arc<dyn IndexNode>).ok_or(SyscallErr::ENOENT),
            ".." => inode.parent.upgrade().map(|n| n as Arc<dyn IndexNode>).ok_or(SyscallErr::ENOENT),
            name => inode.children.get(name).cloned().ok_or(SyscallErr::ENOENT),
        }
    }

    fn list(&self) -> Result<Vec<String>, SyscallErr> {
        let inode = self.0.lock();
        if inode.metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        let mut keys = Vec::new();
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

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        Ok(self.0.lock().metadata.clone())
    }

    fn fs(&self) -> Arc<dyn FileSystem> {
        self.0.lock().fs.upgrade().expect("DevFS inode: fs dropped")
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}

lazy_static! {
    /// 共享的 DevFS 实例，设备文件使用（旧接口兼容）
    pub static ref DEV_FS: Arc<DevFS> = DevFS::new();
}

#[macro_export]
macro_rules! makedev {
    ($x:literal, $y:literal) => {
        (($x & 0xfffff000) << 32)
            | (($x & 0x00000fff) << 8)
            | (($y & 0xffffff00) << 12)
            | ($y & 0x000000ff)
    };
}
