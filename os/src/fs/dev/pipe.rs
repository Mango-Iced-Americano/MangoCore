use alloc::boxed::Box;
use alloc::sync::{Arc, Weak};
use core::any::Any;
use core::ptr::copy_nonoverlapping;
use spin::Mutex;

use crate::config::PAGE_SIZE;
use crate::fs::vfs::{
    FilePrivateData, FileType, IndexNode, InodeFlags, InodeMode, Metadata,
};
use crate::fs::vfs::event::{EPollEvent, EventWaitQueue};
use crate::fs::vfs::file_system::FileSystem as NewFileSystem;
use crate::fs::dev::DEV_FS;
use crate::task::{current_task, WaitQueue};
use crate::timer::TimeSpec;
use crate::utils::error::SyscallErr;

const FIONREAD: u32 = 0x541B;
const CAP_SYS_RESOURCE: usize = 24;
const PIPE_SET_SIZE_MAX: usize = 1usize << 31;

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
            if buf.len() <= PAGE_SIZE && ring.get_free_size() < buf.len() {
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
            if ring.get_free_size() >= PAGE_SIZE || ring.all_read_ends_closed() {
                revents |= EPollEvent::EPOLLOUT.bits();
            }
        }
        if ring.all_write_ends_closed() && ring.all_read_ends_closed() {
            revents |= EPollEvent::EPOLLHUP.bits();
        }
        Ok(revents)
    }

    fn ioctl(
        &self,
        cmd: u32,
        argp: usize,
        _private_data: spin::MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        match cmd {
            FIONREAD => {
                let n = self.buffer.lock().get_used_size().min(i32::MAX as usize) as i32;
                let token = current_task()
                    .map(|task| task.get_user_token())
                    .ok_or(SyscallErr::EFAULT)?;
                crate::mm::UserPtrMut::from_addr(argp)
                    .write(token, &n)
                    .map_err(|_| SyscallErr::EFAULT)?;
                Ok(0)
            }
            _ => Err(SyscallErr::ENOSYS),
        }
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
    pub fn pipe_capacity(&self) -> usize {
        self.buffer.lock().capacity
    }

    pub fn set_pipe_capacity_compat(&self, requested: usize) -> Result<usize, SyscallErr> {
        self.buffer.lock().set_capacity_compat(requested)
    }

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

use core::sync::atomic::{AtomicUsize, Ordering};
static PIPE_BUF_COUNT: AtomicUsize = AtomicUsize::new(0);
static PIPE_BUF_BYTES: AtomicUsize = AtomicUsize::new(0);
static PIPE_MAX_SIZE: AtomicUsize = AtomicUsize::new(RING_DEFAULT_BUFFER_SIZE);
pub fn pipe_buf_alive() -> usize { PIPE_BUF_COUNT.load(Ordering::Relaxed) }
pub fn pipe_buf_bytes() -> usize { PIPE_BUF_BYTES.load(Ordering::Relaxed) }
pub fn pipe_max_size() -> usize { PIPE_MAX_SIZE.load(Ordering::Relaxed) }
pub fn set_pipe_max_size(size: usize) -> bool {
    if size < PAGE_SIZE || size > RING_DEFAULT_BUFFER_SIZE {
        return false;
    }
    PIPE_MAX_SIZE.store(size, Ordering::Relaxed);
    true
}
pub fn pipe_user_pages_soft() -> usize { 16384 }
pub fn pipe_user_pages_hard() -> usize { 0 }

#[derive(Copy, Clone, PartialEq, Debug)]
enum RingBufferStatus {
    FULL,
    EMPTY,
    NORMAL,
}

pub struct PipeRingBuffer {
    arr: Box<[u8; RING_DEFAULT_BUFFER_SIZE]>,
    capacity: usize,
    head: usize,
    tail: usize,
    status: RingBufferStatus,
    write_end: Option<Weak<Pipe>>,
    read_end: Option<Weak<Pipe>>,
}

impl PipeRingBuffer {
    fn new() -> Self {
        PIPE_BUF_COUNT.fetch_add(1, Ordering::Relaxed);
        PIPE_BUF_BYTES.fetch_add(RING_DEFAULT_BUFFER_SIZE, Ordering::Relaxed);
        Self {
            arr: Box::new([0u8; RING_DEFAULT_BUFFER_SIZE]),
            capacity: initial_pipe_capacity(),
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
            self.capacity
        } else if self.status == RingBufferStatus::EMPTY {
            0
        } else {
            assert!(self.head != self.tail);
            if self.head < self.tail {
                self.tail - self.head
            } else {
                self.tail + self.capacity - self.head
            }
        }
    }
    fn get_free_size(&self) -> usize {
        self.capacity - self.get_used_size()
    }
    fn set_capacity_compat(&mut self, requested: usize) -> Result<usize, SyscallErr> {
        if requested > PIPE_SET_SIZE_MAX {
            return Err(SyscallErr::EINVAL);
        }
        let requested = requested.max(PAGE_SIZE);
        if !current_has_sys_resource() && requested > pipe_max_size() {
            return Err(SyscallErr::EPERM);
        }
        if requested > RING_DEFAULT_BUFFER_SIZE {
            return Err(SyscallErr::EINVAL);
        }
        let new_capacity = (requested + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        if new_capacity > RING_DEFAULT_BUFFER_SIZE {
            return Err(SyscallErr::EINVAL);
        }
        let used = self.get_used_size();
        if used > new_capacity {
            return Err(SyscallErr::EBUSY);
        }
        if used == 0 {
            self.head = 0;
            self.tail = 0;
            self.status = RingBufferStatus::EMPTY;
        } else if self.head >= new_capacity || self.tail > new_capacity {
            return Err(SyscallErr::EBUSY);
        }
        self.capacity = new_capacity;
        Ok(self.capacity)
    }
    #[inline]
    fn buffer_read(&mut self, buf: &mut [u8]) -> usize {
        let mut total = 0;
        while total < buf.len() && self.status != RingBufferStatus::EMPTY {
            let begin = self.head;
            let end = if self.tail <= self.head {
                self.capacity
            } else {
                self.tail
            };
            let read_bytes = (buf.len() - total).min(end - begin);
            if read_bytes == 0 {
                break;
            }
            unsafe {
                copy_nonoverlapping(
                    self.arr.as_ptr().add(begin),
                    buf.as_mut_ptr().add(total),
                    read_bytes,
                );
            };
            self.head = if begin + read_bytes == self.capacity {
                0
            } else {
                begin + read_bytes
            };
            total += read_bytes;
            self.status = if self.head == self.tail {
                RingBufferStatus::EMPTY
            } else {
                RingBufferStatus::NORMAL
            };
        }
        total
    }
    #[inline]
    fn buffer_write(&mut self, buf: &[u8]) -> usize {
        let mut total = 0;
        while total < buf.len() && self.status != RingBufferStatus::FULL {
            let free = self.get_free_size();
            if free == 0 {
                break;
            }
            let begin = self.tail;
            let end = if self.tail < self.head {
                self.head
            } else {
                self.capacity
            };
            let write_bytes = (buf.len() - total).min(free).min(end - begin);
            if write_bytes == 0 {
                break;
            }
            unsafe {
                copy_nonoverlapping(
                    buf.as_ptr().add(total),
                    self.arr.as_mut_ptr().add(begin),
                    write_bytes,
                );
            };
            self.tail = if begin + write_bytes == self.capacity {
                0
            } else {
                begin + write_bytes
            };
            total += write_bytes;
            self.status = if self.head == self.tail {
                RingBufferStatus::FULL
            } else {
                RingBufferStatus::NORMAL
            };
        }
        total
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
        PIPE_BUF_COUNT.fetch_sub(1, Ordering::Relaxed);
        PIPE_BUF_BYTES.fetch_sub(RING_DEFAULT_BUFFER_SIZE, Ordering::Relaxed);
    }
}

fn initial_pipe_capacity() -> usize {
    if current_is_root() {
        RING_DEFAULT_BUFFER_SIZE
    } else {
        pipe_max_size().min(RING_DEFAULT_BUFFER_SIZE).max(PAGE_SIZE)
    }
}

fn current_is_root() -> bool {
    current_task()
        .map(|task| task.acquire_inner_lock().euid == 0)
        .unwrap_or(true)
}

fn current_has_sys_resource() -> bool {
    current_task()
        .map(|task| {
            let inner = task.acquire_inner_lock();
            (inner.cap_effective & (1u64 << CAP_SYS_RESOURCE)) != 0
        })
        .unwrap_or(true)
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

// ── Named FIFO support ──────────────────────────────────────────────────

use alloc::collections::BTreeMap;

struct FifoEntry {
    read_end: Weak<Pipe>,
    write_end: Weak<Pipe>,
    buffer: Arc<Mutex<PipeRingBuffer>>,
}

static FIFO_REGISTRY: spin::Mutex<BTreeMap<(usize, usize), FifoEntry>> = spin::Mutex::new(BTreeMap::new());

/// Open a named FIFO inode, returning a Pipe end matching the access mode.
/// `dev_inode` identifies the FIFO (dev_id, inode_id).
/// `for_read` selects the read end; `for_write` selects the write end.
pub fn fifo_open(dev_inode: (usize, usize), for_read: bool, for_write: bool) -> Option<Arc<Pipe>> {
    let mut reg = FIFO_REGISTRY.lock();
    // 清理两端都已关闭的陈旧条目，防止 64KB PipeRingBuffer 永久泄漏。
    if let Some(entry) = reg.get(&dev_inode) {
        if entry.read_end.strong_count() == 0 && entry.write_end.strong_count() == 0 {
            reg.remove(&dev_inode);
        }
    }
    let entry = reg.entry(dev_inode).or_insert_with(|| {
        // Create ring buffer without linking ends yet
        let buf = Arc::new(Mutex::new(PipeRingBuffer::new()));
        FifoEntry {
            read_end: Weak::new(),
            write_end: Weak::new(),
            buffer: buf,
        }
    });

    let buffer = entry.buffer.clone();

    if for_read {
        if let Some(r) = entry.read_end.upgrade() {
            return Some(r);
        }
        let r = Arc::new(Pipe::read_end_with_buffer(buffer.clone()));
        buffer.lock().set_read_end(&r);
        entry.read_end = Arc::downgrade(&r);
        return Some(r);
    }

    if for_write {
        if let Some(w) = entry.write_end.upgrade() {
            return Some(w);
        }
        let w = Arc::new(Pipe::write_end_with_buffer(buffer.clone()));
        buffer.lock().set_write_end(&w);
        entry.write_end = Arc::downgrade(&w);
        return Some(w);
    }

    // O_RDWR: return write end (rare case)
    if let Some(w) = entry.write_end.upgrade() {
        return Some(w);
    }
    let w = Arc::new(Pipe::write_end_with_buffer(buffer.clone()));
    buffer.lock().set_write_end(&w);
    entry.write_end = Arc::downgrade(&w);
    Some(w)
}

/// 清理 FIFO_REGISTRY 中所有两端都已关闭的陈旧条目，
/// 释放持有的 64KB PipeRingBuffer。由 reclaim 周期性触发。
pub fn compact_fifo_registry() -> usize {
    let mut reg = FIFO_REGISTRY.lock();
    let before = reg.len();
    reg.retain(|_, entry| {
        entry.read_end.strong_count() > 0 || entry.write_end.strong_count() > 0
    });
    before - reg.len()
}
