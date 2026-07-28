use alloc::sync::{Arc, Weak};
use core::{
    any::Any,
    sync::atomic::{AtomicBool, Ordering},
};
use spin::{Mutex, MutexGuard};

use crate::{
    fs::{
        dev::DEV_FS,
        vfs::{
            event::{EPollEvent, EventWaitQueue},
            File, FileFlags, FilePrivateData, FileSystem, FileType, IndexNode, InodeMode, Metadata,
        },
    },
    task::{ProcessControlBlock, WaitQueue},
    utils::error::SyscallErr,
};

/// Stable pidfd readiness state shared by every pidfd for one process.
///
/// A pidfd retains this state after its target PCB has been reaped, so exit
/// readiness remains observable for the descriptor's lifetime.
pub struct PidFdState {
    exited: AtomicBool,
    waiters: EventWaitQueue,
}

impl PidFdState {
    pub fn new(exited: bool) -> Self {
        Self {
            exited: AtomicBool::new(exited),
            waiters: EventWaitQueue::new(),
        }
    }

    pub fn exited(&self) -> bool {
        self.exited.load(Ordering::Acquire)
    }

    /// Publish process exit before waking pidfd poll and epoll waiters.
    pub fn notify_exit(&self) {
        self.exited.store(true, Ordering::Release);
        self.waiters
            .notify_events_all(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM);
    }
}

impl core::fmt::Debug for PidFdState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PidFdState")
            .field("exited", &self.exited())
            .finish()
    }
}

/// pidfd inode — 指向进程的文件描述符。
///
/// 对标 Linux 5.3+ `pidfd_open(2)`。通过弱引用（`Weak<ProcessControlBlock>`）
/// 跟踪目标进程生命周期——当进程退出且 pid 被回收后，`target_pid()` 返回 `ESRCH`。
#[derive(Debug)]
pub struct PidFd {
    target_pid: usize,
    target: Weak<ProcessControlBlock>,
    state: Arc<PidFdState>,
    metadata: Metadata,
}

impl PidFd {
    pub fn new(target: &Arc<ProcessControlBlock>) -> Self {
        Self {
            target_pid: target.pid,
            target: Arc::downgrade(target),
            state: target.pidfd_state(),
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

    fn poll(&self, _private_data: &FilePrivateData) -> Result<usize, SyscallErr> {
        if self.state.exited() {
            Ok((EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM).bits())
        } else {
            Ok(0)
        }
    }

    fn read_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(self.state.waiters.wait_queue())
    }

    fn read_event_queue(&self) -> Option<&EventWaitQueue> {
        Some(&self.state.waiters)
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
