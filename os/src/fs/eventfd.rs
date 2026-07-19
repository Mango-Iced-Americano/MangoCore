use alloc::sync::Arc;
use core::any::Any;
use core::convert::TryInto;
use spin::{Mutex, MutexGuard};

use crate::{
    fs::{
        dev::DEV_FS,
        vfs::{
            event::{EPollEvent, EventWaitQueue},
            File, FileFlags, FilePrivateData, FileSystem, FileType, IndexNode, InodeMode, Metadata,
        },
    },
    task::{current_task, WaitQueue},
    utils::error::SyscallErr,
};

const EFD_SEMAPHORE: u32 = 0x1;
const EFD_NONBLOCK: u32 = 0o4000;
const EFD_CLOEXEC: u32 = 0o2000000;
const EFD_VALID_FLAGS: u32 = EFD_SEMAPHORE | EFD_NONBLOCK | EFD_CLOEXEC;
const EVENTFD_COUNTER_MAX: u64 = u64::MAX - 1;

#[derive(Debug)]
struct EventFdInner {
    counter: u64,
}

/// EventFd — 基于 fd 的事件通知对象。
///
/// 维护一个 `u64` 计数器，范围 `0..EVENTFD_COUNTER_MAX`（`u64::MAX - 1`）。
/// - `read(2)` 返回当前计数值（归零）或 `EFD_SEMAPHORE` 模式下返回 `1`（递减）。
/// - `write(2)` 将用户值累加到计数器，溢出时阻塞或返回 `EAGAIN`。
///
/// # Locking
///
/// `inner` 保护 `counter`。`read_at` 递减后通过 `notify_writable` 唤醒 `write_wait`；
/// `write_at` 累加后通过 `notify_readable` 唤醒 `read_wait`。
/// 条件：`counter > 0` 时读就绪；`counter < EVENTFD_COUNTER_MAX` 时写就绪。
///
/// # Linux Compatibility
///
/// 对齐 Linux 6.6 `eventfd(2)`。支持 `EFD_CLOEXEC`、`EFD_NONBLOCK`、`EFD_SEMAPHORE`。
pub struct EventFd {
    inner: Mutex<EventFdInner>,
    semaphore: bool,
    read_wait: EventWaitQueue,
    write_wait: EventWaitQueue,
    metadata: Metadata,
}

impl core::fmt::Debug for EventFd {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EventFd")
            .field("semaphore", &self.semaphore)
            .finish()
    }
}

impl EventFd {
    fn new(initval: u32, flags: u32) -> Self {
        Self {
            inner: Mutex::new(EventFdInner {
                counter: initval as u64,
            }),
            semaphore: (flags & EFD_SEMAPHORE) != 0,
            read_wait: EventWaitQueue::new(),
            write_wait: EventWaitQueue::new(),
            metadata: Metadata::new(
                FileType::File,
                InodeMode::S_IFREG | InodeMode::from_bits_truncate(0o600),
            ),
        }
    }

    fn notify_readable(&self) {
        self.read_wait
            .notify_events_all(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM);
    }

    fn notify_writable(&self) {
        self.write_wait
            .notify_events_all(EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM);
    }
}

impl IndexNode for EventFd {
    fn read_at(
        &self,
        _offset: usize,
        len: usize,
        buf: &mut [u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        if len < core::mem::size_of::<u64>() || buf.len() < core::mem::size_of::<u64>() {
            return Err(SyscallErr::EINVAL);
        }

        let value = {
            let mut inner = self.inner.lock();
            if inner.counter == 0 {
                return Err(SyscallErr::EAGAIN);
            }

            if self.semaphore {
                inner.counter -= 1;
                1
            } else {
                let value = inner.counter;
                inner.counter = 0;
                value
            }
        };

        buf[..8].copy_from_slice(&value.to_ne_bytes());
        self.notify_writable();
        Ok(8)
    }

    fn write_at(
        &self,
        _offset: usize,
        len: usize,
        buf: &[u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        if len < core::mem::size_of::<u64>() || buf.len() < core::mem::size_of::<u64>() {
            return Err(SyscallErr::EINVAL);
        }

        let value = u64::from_ne_bytes(buf[..8].try_into().unwrap());
        if value == u64::MAX {
            return Err(SyscallErr::EINVAL);
        }

        {
            let mut inner = self.inner.lock();
            if EVENTFD_COUNTER_MAX.saturating_sub(inner.counter) < value {
                return Err(SyscallErr::EAGAIN);
            }
            inner.counter += value;
        }

        self.notify_readable();
        Ok(8)
    }

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        Ok(self.metadata.clone())
    }

    fn poll(&self, _private_data: &FilePrivateData) -> Result<usize, SyscallErr> {
        let inner = self.inner.lock();
        let mut events = EPollEvent::empty();
        if inner.counter > 0 {
            events |= EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM;
        }
        if inner.counter < EVENTFD_COUNTER_MAX {
            events |= EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM;
        }
        Ok(events.bits())
    }

    fn read_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(self.read_wait.wait_queue())
    }

    fn write_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(self.write_wait.wait_queue())
    }

    fn read_event_queue(&self) -> Option<&EventWaitQueue> {
        Some(&self.read_wait)
    }

    fn write_event_queue(&self) -> Option<&EventWaitQueue> {
        Some(&self.write_wait)
    }

    fn fs(&self) -> Arc<dyn FileSystem> {
        DEV_FS.clone()
    }

    fn is_stream(&self) -> bool {
        true
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}

/// 创建 eventfd 实例，返回可读写的 fd。
///
/// # Semantics
///
/// `initval` 为初始计数器值。`flags` 支持：
/// - `EFD_CLOEXEC`：fd 在 exec 时关闭
/// - `EFD_NONBLOCK`：非阻塞模式
/// - `EFD_SEMAPHORE`：信号量语义（read 返回 1 而非当前值，并递减 1）
///
/// 计数器范围为 0..`EVENTFD_COUNTER_MAX`（`u64::MAX - 1`）。
/// 阻塞模式下：
/// - `read(2)` 在计数器 == 0 时阻塞
/// - `write(2)` 在计数器达到最大值时阻塞（`EAGAIN`）
///
/// 计数器溢出时 write 阻塞或返回 `EAGAIN`，取决于 fd 是否为非阻塞模式。
///
/// # Linux Compatibility
///
/// 对齐 Linux 6.6 `eventfd2(2)`。MangoCore 使用 `sys_eventfd2` 作为统一入口
/// （gcc/libusb/musl 等 C 库会自动将 `eventfd()` 重写为 `eventfd2()`，无需单独 `sys_eventfd`）。
///
/// # Errors
///
/// - `EINVAL`：flags 包含非法位
/// - `ENOMEM`：内存分配失败
/// - `EMFILE`：进程 fd 表已满
pub fn sys_eventfd2(initval: u32, flags: u32) -> isize {
    if (flags & !EFD_VALID_FLAGS) != 0 {
        return -(SyscallErr::EINVAL as isize);
    }

    let mut file_flags = FileFlags::O_RDWR;
    if (flags & EFD_NONBLOCK) != 0 {
        file_flags |= FileFlags::O_NONBLOCK;
    }
    if (flags & EFD_CLOEXEC) != 0 {
        file_flags |= FileFlags::O_CLOEXEC;
    }

    let inode = Arc::new(EventFd::new(initval, flags)) as Arc<dyn IndexNode>;
    let file = match File::new(inode, file_flags) {
        Ok(file) => file,
        Err(err) => return -(err as isize),
    };

    let task = current_task().unwrap();
    let files = task.process.files();
    let ret = match files.lock().alloc_fd(file, (flags & EFD_CLOEXEC) != 0) {
        Ok(fd) => fd as isize,
        Err(err) => -(err as isize),
    };
    ret
}
