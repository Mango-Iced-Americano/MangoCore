use alloc::sync::{Arc, Weak};
use core::any::Any;
use spin::MutexGuard;

use crate::{
    fs::{
        dev::DEV_FS,
        vfs::{File, FileFlags, FilePrivateData, FileSystem, FileType, IndexNode, InodeMode, Metadata},
    },
    task::ProcessControlBlock,
    utils::error::SyscallErr,
};

#[derive(Debug)]
pub struct PidFd {
    target_pid: usize,
    target: Weak<ProcessControlBlock>,
    metadata: Metadata,
}

impl PidFd {
    pub fn new(target: &Arc<ProcessControlBlock>) -> Self {
        Self {
            target_pid: target.pid,
            target: Arc::downgrade(target),
            metadata: Metadata::new(
                FileType::File,
                InodeMode::S_IFREG | InodeMode::from_bits_truncate(0o600),
            ),
        }
    }

    pub fn target_pid(&self) -> Result<usize, SyscallErr> {
        match self.target.upgrade() {
            Some(process) if process.pid == self.target_pid && !process.pid_released() => {
                Ok(self.target_pid)
            }
            _ => Err(SyscallErr::ESRCH),
        }
    }
}

impl IndexNode for PidFd {
    fn read_at(
        &self,
        _offset: usize,
        _len: usize,
        _buf: &mut [u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        Err(SyscallErr::EINVAL)
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
        DEV_FS.clone()
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}

pub fn new_pidfd_file_with_flags(
    target: &Arc<ProcessControlBlock>,
    flags: FileFlags,
) -> Result<File, SyscallErr> {
    let inode = Arc::new(PidFd::new(target)) as Arc<dyn IndexNode>;
    File::new(inode, flags)
}

pub fn new_pidfd_file(target: &Arc<ProcessControlBlock>) -> Result<File, SyscallErr> {
    new_pidfd_file_with_flags(target, FileFlags::O_RDWR)
}
