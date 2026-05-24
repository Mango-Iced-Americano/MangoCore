use alloc::boxed::Box;
use alloc::sync::{Arc, Weak};
use core::any::Any;
use core::ptr::copy_nonoverlapping;
use spin::Mutex;

use crate::fs::vfs::{
    FilePrivateData, FileType, IndexNode, InodeFlags, InodeMode, Metadata,
};
use crate::fs::vfs::event::{EPollEvent, EventWaitQueue};
use crate::fs::vfs::file_system::FileSystem as NewFileSystem;
use crate::fs::dev::DEV_FS;
use crate::task::WaitQueue;
use crate::timer::TimeSpec;
use crate::utils::error::SyscallErr;

pub struct Pipe {
    readable: bool,
    writable: bool,
    buffer: Arc<Mutex<PipeRingBuffer>>,
    read_wait: EventWaitQueue,
    write_wait: EventWaitQueue,
}

impl core::fmt::Debug for Pipe {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Pipe")
            .field("readable", &self.readable)
            .field("writable", &self.writable)
            .finish()
    }
}

impl IndexNode for Pipe {
    fn read_at(
        &self,
        _offset: usize,
        _len: usize,
        buf: &mut [u8],
        _data: spin::MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        if !self.readable {
            return Err(SyscallErr::EBADF);
        }
        if buf.is_empty() {
            return Ok(0);
        }
        let result = {
            let mut ring = self.buffer.lock();
            if ring.status == RingBufferStatus::EMPTY {
                if ring.all_write_ends_closed() {
                    return Ok(0); // EOF
                }
                return Err(SyscallErr::EAGAIN);
            }
            let read_bytes = ring.buffer_read(buf);
            ring.status = if ring.head == ring.tail {
                RingBufferStatus::EMPTY
            } else {
                RingBufferStatus::NORMAL
            };
            Ok(read_bytes)
        };
        if let Ok(_n) = &result {
            if let Some(write_end) = self.peer_write_end() {
                write_end
                    .write_wait
                    .notify_events_at_most(EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM, 1);
            }
        }
        result
    }

    fn write_at(
        &self,
        _offset: usize,
        _len: usize,
        buf: &[u8],
        _data: spin::MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        if !self.writable {
            return Err(SyscallErr::EBADF);
        }
        if buf.is_empty() {
            return Ok(0);
        }
        let result = {
            let mut ring = self.buffer.lock();
            if ring.status == RingBufferStatus::FULL {
                if ring.all_read_ends_closed() {
                    return Err(SyscallErr::EPIPE); // Broken pipe
                }
                return Err(SyscallErr::EAGAIN);
            }
            let write_bytes = ring.buffer_write(buf);
            ring.status = if ring.head == ring.tail {
                RingBufferStatus::FULL
            } else {
                RingBufferStatus::NORMAL
            };
            Ok(write_bytes)
        };
        if let Ok(_n) = &result {
            if let Some(read_end) = self.peer_read_end() {
                read_end
                    .read_wait
                    .notify_events_at_most(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM, 1);
            }
        }
        result
    }

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        Ok(Metadata {
            dev_id: 0,
            inode_id: 0,
            size: 0,
            blk_size: 0,
            blocks: 0,
            atime: TimeSpec::new(),
            mtime: TimeSpec::new(),
            ctime: TimeSpec::new(),
            file_type: FileType::Pipe,
            mode: InodeMode::S_IFIFO | InodeMode::from_bits_truncate(0o666),
            nlinks: 1,
            uid: 0,
            gid: 0,
            flags: InodeFlags::empty(),
            raw_dev: 0,
        })
    }

    fn is_stream(&self) -> bool {
        true
    }

    fn poll(&self, _private_data: &FilePrivateData) -> Result<usize, SyscallErr> {
        let ring = self.buffer.lock();
        let mut revents: usize = 0;
        if self.readable {
            if ring.status != RingBufferStatus::EMPTY || ring.all_write_ends_closed() {
                revents |= EPollEvent::EPOLLIN.bits();
            }
        }
        if self.writable {
            if ring.status != RingBufferStatus::FULL || ring.all_read_ends_closed() {
                revents |= EPollEvent::EPOLLOUT.bits();
            }
        }
        if ring.all_write_ends_closed() && ring.all_read_ends_closed() {
            revents |= EPollEvent::EPOLLHUP.bits();
        }
        Ok(revents)
    }

    fn read_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(self.read_wait.wait_queue())
    }

    fn write_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(self.write_wait.wait_queue())
    }

    fn read_event_queue(&self) -> Option<&crate::fs::vfs::event::EventWaitQueue> {
        Some(&self.read_wait)
    }

    fn write_event_queue(&self) -> Option<&crate::fs::vfs::event::EventWaitQueue> {
        Some(&self.write_wait)
    }

    fn fs(&self) -> Arc<dyn NewFileSystem> {
        DEV_FS.clone()
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        if self.readable {
            if let Some(write_end) = self.peer_write_end() {
                write_end
                    .write_wait
                    .notify_events_all(EPollEvent::EPOLLOUT | EPollEvent::EPOLLHUP);
            }
        }
        if self.writable {
            if let Some(read_end) = self.peer_read_end() {
                read_end
                    .read_wait
                    .notify_events_all(EPollEvent::EPOLLIN | EPollEvent::EPOLLHUP);
            }
        }
    }
}

impl Pipe {
    fn peer_read_end(&self) -> Option<Arc<Pipe>> {
        self.buffer
            .lock()
            .read_end
            .as_ref()
            .and_then(Weak::upgrade)
    }

    fn peer_write_end(&self) -> Option<Arc<Pipe>> {
        self.buffer
            .lock()
            .write_end
            .as_ref()
            .and_then(Weak::upgrade)
    }

    pub fn read_end_with_buffer(buffer: Arc<Mutex<PipeRingBuffer>>) -> Self {
        Self {
            readable: true,
            writable: false,
            buffer,
            read_wait: EventWaitQueue::new(),
            write_wait: EventWaitQueue::new(),
        }
    }
    pub fn write_end_with_buffer(buffer: Arc<Mutex<PipeRingBuffer>>) -> Self {
        Self {
            readable: false,
            writable: true,
            buffer,
            read_wait: EventWaitQueue::new(),
            write_wait: EventWaitQueue::new(),
        }
    }
}

#[cfg(feature = "board_fu740")]
const RING_DEFAULT_BUFFER_SIZE: usize = 4096 * 16;
#[cfg(not(feature = "board_fu740"))]
const RING_DEFAULT_BUFFER_SIZE: usize = 4096 * 16;

use core::sync::atomic::AtomicUsize;
static PIPE_BUF_COUNT: AtomicUsize = AtomicUsize::new(0);
static PIPE_BUF_BYTES: AtomicUsize = AtomicUsize::new(0);
pub fn pipe_buf_alive() -> usize { PIPE_BUF_COUNT.load(core::sync::atomic::Ordering::Relaxed) }
pub fn pipe_buf_bytes() -> usize { PIPE_BUF_BYTES.load(core::sync::atomic::Ordering::Relaxed) }

#[derive(Copy, Clone, PartialEq, Debug)]
enum RingBufferStatus {
    FULL,
    EMPTY,
    NORMAL,
}

pub struct PipeRingBuffer {
    arr: Box<[u8; RING_DEFAULT_BUFFER_SIZE]>,
    head: usize,
    tail: usize,
    status: RingBufferStatus,
    write_end: Option<Weak<Pipe>>,
    read_end: Option<Weak<Pipe>>,
}

impl PipeRingBuffer {
    fn new() -> Self {
        PIPE_BUF_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        PIPE_BUF_BYTES.fetch_add(RING_DEFAULT_BUFFER_SIZE, core::sync::atomic::Ordering::Relaxed);
        Self {
            arr: Box::new([0u8; RING_DEFAULT_BUFFER_SIZE]),
            head: 0,
            tail: 0,
            status: RingBufferStatus::EMPTY,
            write_end: None,
            read_end: None,
        }
    }
    #[allow(unused)]
    fn get_used_size(&self) -> usize {
        if self.status == RingBufferStatus::FULL {
            self.arr.len()
        } else if self.status == RingBufferStatus::EMPTY {
            0
        } else {
            assert!(self.head != self.tail);
            if self.head < self.tail {
                self.tail - self.head
            } else {
                self.tail + self.arr.len() - self.head
            }
        }
    }
    #[inline]
    fn buffer_read(&mut self, buf: &mut [u8]) -> usize {
        // get range
        let begin = self.head;
        let end = if self.tail <= self.head {
            RING_DEFAULT_BUFFER_SIZE
        } else {
            self.tail
        };
        // copy
        let read_bytes = buf.len().min(end - begin);
        unsafe {
            copy_nonoverlapping(self.arr.as_ptr().add(begin), buf.as_mut_ptr(), read_bytes);
        };
        // update head
        self.head = if begin + read_bytes == RING_DEFAULT_BUFFER_SIZE {
            0
        } else {
            begin + read_bytes
        };
        read_bytes
    }
    #[inline]
    fn buffer_write(&mut self, buf: &[u8]) -> usize {
        // get range
        let begin = self.tail;
        let end = if self.tail < self.head {
            self.head
        } else {
            RING_DEFAULT_BUFFER_SIZE
        };
        // write
        let write_bytes = buf.len().min(end - begin);
        unsafe {
            copy_nonoverlapping(buf.as_ptr(), self.arr.as_mut_ptr().add(begin), write_bytes);
        };
        // update tail
        self.tail = if begin + write_bytes == RING_DEFAULT_BUFFER_SIZE {
            0
        } else {
            begin + write_bytes
        };
        write_bytes
    }
    fn set_write_end(&mut self, write_end: &Arc<Pipe>) {
        self.write_end = Some(Arc::downgrade(write_end));
    }
    fn set_read_end(&mut self, read_end: &Arc<Pipe>) {
        self.read_end = Some(Arc::downgrade(read_end));
    }
    fn all_write_ends_closed(&self) -> bool {
        self.write_end.as_ref().unwrap().upgrade().is_none()
    }
    fn all_read_ends_closed(&self) -> bool {
        self.read_end.as_ref().unwrap().upgrade().is_none()
    }
}

impl Drop for PipeRingBuffer {
    fn drop(&mut self) {
        PIPE_BUF_COUNT.fetch_sub(1, core::sync::atomic::Ordering::Relaxed);
        PIPE_BUF_BYTES.fetch_sub(RING_DEFAULT_BUFFER_SIZE, core::sync::atomic::Ordering::Relaxed);
    }
}

/// Return (read_end, write_end)
pub fn make_pipe() -> (Arc<Pipe>, Arc<Pipe>) {
    let buffer = Arc::new(Mutex::new(PipeRingBuffer::new()));
    // buffer仅剩两个强引用，这样读写端关闭后就会被释放
    let read_end = Arc::new(Pipe::read_end_with_buffer(buffer.clone()));
    let write_end = Arc::new(Pipe::write_end_with_buffer(buffer.clone()));
    buffer.lock().set_write_end(&write_end);
    buffer.lock().set_read_end(&read_end);
    (read_end, write_end)
}
