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

/// pidfd inode — 指向进程的文件描述符。
///
/// 对标 Linux 5.3+ `pidfd_open(2)`。通过弱引用（`Weak<ProcessControlBlock>`）
/// 跟踪目标进程生命周期——当进程退出且 pid 被回收后，`target_pid()` 返回 `ESRCH`。
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

/// 创建指向目标进程的 pidfd File。
///
/// # Semantics
///
/// `target` 为指向 `ProcessControlBlock` 的 `Arc` 引用，由调用方保证有效性。
/// 内部创建 `PidFd` inode，通过弱引用跟踪目标进程。
/// `flags` 用于设置 `O_NONBLOCK` 等状态标志。
///
/// # Errors
///
/// 透传 `File::new()` 错误（如 `ENOMEM`）。
pub fn new_pidfd_file_with_flags(
    target: &Arc<ProcessControlBlock>,
    flags: FileFlags,
) -> Result<Arc<File>, SyscallErr> {
    let inode = Arc::new(PidFd::new(target)) as Arc<dyn IndexNode>;
    File::new(inode, flags)
}

/// 创建指向目标进程的 pidfd File（默认 `O_RDWR`）。
///
/// 便捷包装，等价于 `new_pidfd_file_with_flags(target, FileFlags::O_RDWR)`。
pub fn new_pidfd_file(target: &Arc<ProcessControlBlock>) -> Result<Arc<File>, SyscallErr> {
    new_pidfd_file_with_flags(target, FileFlags::O_RDWR)
}
