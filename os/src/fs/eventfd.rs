use alloc::sync::Arc;
use core::any::Any;
use core::convert::TryInto;
use spin::{Mutex, MutexGuard};

use crate::{
    fs::{
        dev::DEV_FS,
        vfs::{
            event::{EPollEvent, EventWaitQueue},
            File, FileFlags, FilePrivateData, FileSystem, FileType, IndexNode, InodeMode,
            Metadata,
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
