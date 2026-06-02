//! /proc/<pid>/ns/* — namespace reference files
//!
//! Opening these files creates fds whose inodes carry Arc<XxxNamespace> handles.
//! `setns(fd, CLONE_NEWXXX)` downcasts the inode to retrieve the namespace
//! and switches the calling process into it.

use alloc::sync::Arc;
use core::any::Any;
use spin::MutexGuard;

use crate::fs::{
    procfs::ProcFS,
    vfs::{FileFlags, FilePrivateData, FileSystem, FileType, IndexNode, InodeMode, Metadata},
};
use crate::task::{NetNamespace, MountNamespace, IpcNamespace};
use crate::utils::error::SyscallErr;

/// Inode for /proc/<pid>/ns/net.
///
/// Stores a reference-counted handle to the process's network namespace.
/// Implements `as_any_ref()` so `sys_setns` can downcast and switch namespaces.
#[derive(Debug)]
pub struct ProcNsNetInode {
    ns: Arc<NetNamespace>,
    metadata: Metadata,
    procfs: alloc::sync::Weak<ProcFS>,
}

impl ProcNsNetInode {
    pub fn new(ns: Arc<NetNamespace>, procfs: alloc::sync::Weak<ProcFS>) -> Self {
        Self {
            ns,
            metadata: Metadata::new(
                FileType::File,
                InodeMode::S_IFREG | InodeMode::from_bits_truncate(0o444),
            ),
            procfs,
        }
    }

    /// Return a reference to the stored network namespace.
    pub fn netns(&self) -> &Arc<NetNamespace> {
        &self.ns
    }
}

impl IndexNode for ProcNsNetInode {
    fn read_at(
        &self,
        _offset: usize,
        _len: usize,
        _buf: &mut [u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        Ok(0)
    }

    fn write_at(
        &self,
        _offset: usize,
        _len: usize,
        _buf: &[u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        Err(SyscallErr::EINVAL)
    }

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        Ok(self.metadata.clone())
    }

    fn fs(&self) -> Arc<dyn FileSystem> {
        self.procfs
            .upgrade()
            .expect("ProcNSNetInode: ProcFS has been dropped")
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}

// ── /proc/<pid>/ns/mnt ────────────────────────────────────────────────

#[derive(Debug)]
pub struct ProcNsMntInode {
    ns: Arc<MountNamespace>,
    metadata: Metadata,
    procfs: alloc::sync::Weak<ProcFS>,
}

impl ProcNsMntInode {
    pub fn new(ns: Arc<MountNamespace>, procfs: alloc::sync::Weak<ProcFS>) -> Self {
        Self {
            ns,
            metadata: Metadata::new(
                FileType::File,
                InodeMode::S_IFREG | InodeMode::from_bits_truncate(0o444),
            ),
            procfs,
        }
    }

    pub fn mntns(&self) -> &Arc<MountNamespace> {
        &self.ns
    }
}

impl IndexNode for ProcNsMntInode {
    fn read_at(&self, _o: usize, _l: usize, _buf: &mut [u8], _d: MutexGuard<FilePrivateData>) -> Result<usize, SyscallErr> { Ok(0) }
    fn write_at(&self, _o: usize, _l: usize, _b: &[u8], _d: MutexGuard<FilePrivateData>) -> Result<usize, SyscallErr> { Err(SyscallErr::EINVAL) }
    fn metadata(&self) -> Result<Metadata, SyscallErr> { Ok(self.metadata.clone()) }
    fn fs(&self) -> Arc<dyn FileSystem> {
        self.procfs.upgrade().expect("ProcNsMntInode: ProcFS dropped")
    }
    fn as_any_ref(&self) -> &dyn Any { self }
}

// ── /proc/<pid>/ns/ipc ────────────────────────────────────────────────

#[derive(Debug)]
pub struct ProcNsIpcInode {
    ns: Arc<IpcNamespace>,
    metadata: Metadata,
    procfs: alloc::sync::Weak<ProcFS>,
}

impl ProcNsIpcInode {
    pub fn new(ns: Arc<IpcNamespace>, procfs: alloc::sync::Weak<ProcFS>) -> Self {
        Self {
            ns,
            metadata: Metadata::new(
                FileType::File,
                InodeMode::S_IFREG | InodeMode::from_bits_truncate(0o444),
            ),
            procfs,
        }
    }

    pub fn ipcns(&self) -> &Arc<IpcNamespace> {
        &self.ns
    }
}

impl IndexNode for ProcNsIpcInode {
    fn read_at(&self, _o: usize, _l: usize, _buf: &mut [u8], _d: MutexGuard<FilePrivateData>) -> Result<usize, SyscallErr> { Ok(0) }
    fn write_at(&self, _o: usize, _l: usize, _b: &[u8], _d: MutexGuard<FilePrivateData>) -> Result<usize, SyscallErr> { Err(SyscallErr::EINVAL) }
    fn metadata(&self) -> Result<Metadata, SyscallErr> { Ok(self.metadata.clone()) }
    fn fs(&self) -> Arc<dyn FileSystem> {
        self.procfs.upgrade().expect("ProcNsIpcInode: ProcFS dropped")
    }
    fn as_any_ref(&self) -> &dyn Any { self }
}
