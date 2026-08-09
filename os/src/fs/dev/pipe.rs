use alloc::boxed::Box;
use alloc::sync::{Arc, Weak};
use core::any::Any;
use core::ptr::copy_nonoverlapping;
use spin::Mutex;

use crate::config::PAGE_SIZE;
use crate::fs::dev::DEV_FS;
use crate::fs::vfs::event::{EPollEvent, EventWaitQueue};
use crate::fs::vfs::file_system::FileSystem as NewFileSystem;
use crate::fs::vfs::{FilePrivateData, FileType, IndexNode, InodeFlags, InodeMode, Metadata};
use crate::mm::UserBuffer;
use crate::task::{current_task, Signals, WaitQueue};
use crate::timer::TimeSpec;
use crate::utils::error::SyscallErr;

// ── pipe debug profile counters ─────────────────────────────────────────
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static PIPE_PROFILE_ENABLED: AtomicBool = AtomicBool::new(false);
static PIPE_READ_CALLS: AtomicU64 = AtomicU64::new(0);
static PIPE_READ_BYTES: AtomicU64 = AtomicU64::new(0);
static PIPE_READ_EAGAIN: AtomicU64 = AtomicU64::new(0);
static PIPE_READ_EOF: AtomicU64 = AtomicU64::new(0);
static PIPE_READ_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static PIPE_READ_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static PIPE_WRITE_CALLS: AtomicU64 = AtomicU64::new(0);
static PIPE_WRITE_BYTES: AtomicU64 = AtomicU64::new(0);
static PIPE_WRITE_EAGAIN: AtomicU64 = AtomicU64::new(0);
static PIPE_WRITE_EPIPE: AtomicU64 = AtomicU64::new(0);
static PIPE_WRITE_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static PIPE_WRITE_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static PIPE_POLL_CALLS: AtomicU64 = AtomicU64::new(0);
static PIPE_POLL_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static PIPE_POLL_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static PIPE_NOTIFY_READ: AtomicU64 = AtomicU64::new(0);
static PIPE_NOTIFY_WRITE: AtomicU64 = AtomicU64::new(0);
static PIPE_BUF_ALLOC: AtomicU64 = AtomicU64::new(0);
static PIPE_BUF_DROP: AtomicU64 = AtomicU64::new(0);
static PIPE_BUF_ALIVE_MAX: AtomicU64 = AtomicU64::new(0);
static PIPE_BUF_BYTES_MAX: AtomicU64 = AtomicU64::new(0);
static PIPE_RING_USED_MAX: AtomicU64 = AtomicU64::new(0);
static PIPE_RING_FREE_MIN: AtomicU64 = AtomicU64::new(u64::MAX);
static PIPE_FIFO_OPEN: AtomicU64 = AtomicU64::new(0);
static PIPE_FIFO_COMPACT_CALLS: AtomicU64 = AtomicU64::new(0);
static PIPE_FIFO_COMPACT_REMOVED: AtomicU64 = AtomicU64::new(0);
static PIPE_FIFO_REGISTRY_LEN_MAX: AtomicU64 = AtomicU64::new(0);
static PIPE_FIFO_HIT_READ: AtomicU64 = AtomicU64::new(0);
static PIPE_FIFO_HIT_WRITE: AtomicU64 = AtomicU64::new(0);

pub fn reset_pipe_profile() {
    PIPE_PROFILE_ENABLED.store(false, Ordering::Relaxed);
    let all: [&AtomicU64; 28] = [
        &PIPE_READ_CALLS,
        &PIPE_READ_BYTES,
        &PIPE_READ_EAGAIN,
        &PIPE_READ_EOF,
        &PIPE_READ_CYCLES_TOTAL,
        &PIPE_READ_CYCLES_MAX,
        &PIPE_WRITE_CALLS,
        &PIPE_WRITE_BYTES,
        &PIPE_WRITE_EAGAIN,
        &PIPE_WRITE_EPIPE,
        &PIPE_WRITE_CYCLES_TOTAL,
        &PIPE_WRITE_CYCLES_MAX,
        &PIPE_POLL_CALLS,
        &PIPE_POLL_CYCLES_TOTAL,
        &PIPE_POLL_CYCLES_MAX,
        &PIPE_NOTIFY_READ,
        &PIPE_NOTIFY_WRITE,
        &PIPE_BUF_ALLOC,
        &PIPE_BUF_DROP,
        &PIPE_BUF_ALIVE_MAX,
        &PIPE_BUF_BYTES_MAX,
        &PIPE_RING_USED_MAX,
        &PIPE_FIFO_OPEN,
        &PIPE_FIFO_COMPACT_CALLS,
        &PIPE_FIFO_COMPACT_REMOVED,
        &PIPE_FIFO_REGISTRY_LEN_MAX,
        &PIPE_FIFO_HIT_READ,
        &PIPE_FIFO_HIT_WRITE,
    ];
    for c in &all {
        c.store(0, Ordering::Relaxed);
    }
    PIPE_RING_FREE_MIN.store(u64::MAX, Ordering::Relaxed);
    PIPE_PROFILE_ENABLED.store(true, Ordering::Relaxed);
}

pub fn disable_pipe_profile() {
    PIPE_PROFILE_ENABLED.store(false, Ordering::Relaxed);
}

pub fn dump_pipe_profile(label: &str) {
    println!("[pipe_profile] {}", label);
    println!(
        "pipe_profile enabled={}",
        PIPE_PROFILE_ENABLED.load(Ordering::Relaxed) as usize
    );
    println!("pipe read_calls={} read_bytes={} read_eagain={} read_eof={} read_cycles_total={} read_cycles_max={}",
        PIPE_READ_CALLS.load(Ordering::Relaxed), PIPE_READ_BYTES.load(Ordering::Relaxed),
        PIPE_READ_EAGAIN.load(Ordering::Relaxed), PIPE_READ_EOF.load(Ordering::Relaxed),
        PIPE_READ_CYCLES_TOTAL.load(Ordering::Relaxed), PIPE_READ_CYCLES_MAX.load(Ordering::Relaxed));
    println!("pipe write_calls={} write_bytes={} write_eagain={} write_epipe={} write_cycles_total={} write_cycles_max={}",
        PIPE_WRITE_CALLS.load(Ordering::Relaxed), PIPE_WRITE_BYTES.load(Ordering::Relaxed),
        PIPE_WRITE_EAGAIN.load(Ordering::Relaxed), PIPE_WRITE_EPIPE.load(Ordering::Relaxed),
        PIPE_WRITE_CYCLES_TOTAL.load(Ordering::Relaxed), PIPE_WRITE_CYCLES_MAX.load(Ordering::Relaxed));
    println!(
        "pipe poll_calls={} poll_cycles_total={} poll_cycles_max={} notify_read={} notify_write={}",
        PIPE_POLL_CALLS.load(Ordering::Relaxed),
        PIPE_POLL_CYCLES_TOTAL.load(Ordering::Relaxed),
        PIPE_POLL_CYCLES_MAX.load(Ordering::Relaxed),
        PIPE_NOTIFY_READ.load(Ordering::Relaxed),
        PIPE_NOTIFY_WRITE.load(Ordering::Relaxed)
    );
    let ring_free_min = PIPE_RING_FREE_MIN.load(Ordering::Relaxed);
    println!("pipe buf_alloc={} buf_drop={} buf_alive_max={} buf_bytes_max={} ring_used_max={} ring_free_min={} fifo_open={} fifo_hit_read={} fifo_hit_write={}",
        PIPE_BUF_ALLOC.load(Ordering::Relaxed), PIPE_BUF_DROP.load(Ordering::Relaxed),
        PIPE_BUF_ALIVE_MAX.load(Ordering::Relaxed), PIPE_BUF_BYTES_MAX.load(Ordering::Relaxed),
        PIPE_RING_USED_MAX.load(Ordering::Relaxed),
        if ring_free_min == u64::MAX { 0 } else { ring_free_min },
        PIPE_FIFO_OPEN.load(Ordering::Relaxed), PIPE_FIFO_HIT_READ.load(Ordering::Relaxed),
        PIPE_FIFO_HIT_WRITE.load(Ordering::Relaxed));
    println!(
        "pipe fifo_compact_calls={} fifo_compact_removed={} fifo_registry_len_max={}",
        PIPE_FIFO_COMPACT_CALLS.load(Ordering::Relaxed),
        PIPE_FIFO_COMPACT_REMOVED.load(Ordering::Relaxed),
        PIPE_FIFO_REGISTRY_LEN_MAX.load(Ordering::Relaxed)
    );
}

#[inline(always)]
fn pipe_profile_enabled() -> bool {
    PIPE_PROFILE_ENABLED.load(Ordering::Relaxed)
}

#[inline(always)]
fn pipe_rdcycle() -> u64 {
    // Safety: `rdcycle` / `rdtime.d` read the cycle counter register with no memory
    // side effects.  The output is a register-only value; the instruction cannot
    // cause undefined behaviour in any execution context.
    #[cfg(target_arch = "riscv64")]
    {
        let cycles: usize;
        unsafe { core::arch::asm!("rdcycle {}", out(reg) cycles) };
        cycles as u64
    }
    #[cfg(target_arch = "loongarch64")]
    {
        let lo: usize;
        let hi: usize;
        unsafe { core::arch::asm!("rdtime.d {}, {}", out(reg) lo, out(reg) hi) };
        lo as u64
    }
    #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
    {
        0
    }
}

#[inline(always)]
fn pipe_profile_start(enabled: bool) -> u64 {
    if enabled {
        pipe_rdcycle()
    } else {
        0
    }
}

#[inline(always)]
fn pipe_inc(enabled: bool, slot: &AtomicU64) {
    if enabled {
        slot.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline(always)]
fn pipe_add(enabled: bool, slot: &AtomicU64, value: u64) {
    if enabled {
        slot.fetch_add(value, Ordering::Relaxed);
    }
}

fn pipe_atomic_max(slot: &AtomicU64, v: u64) {
    let mut cur = slot.load(Ordering::Relaxed);
    while v > cur {
        match slot.compare_exchange_weak(cur, v, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(n) => cur = n,
        }
    }
}

fn pipe_atomic_min(slot: &AtomicU64, v: u64) {
    let mut cur = slot.load(Ordering::Relaxed);
    while v < cur {
        match slot.compare_exchange_weak(cur, v, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(n) => cur = n,
        }
    }
}

#[inline(always)]
fn pipe_record_cycles(total: &AtomicU64, max: &AtomicU64, start: u64) {
    if !pipe_profile_enabled() {
        return;
    }
    let dt = pipe_rdcycle().saturating_sub(start);
    total.fetch_add(dt, Ordering::Relaxed);
    pipe_atomic_max(max, dt);
}

#[inline(always)]
fn pipe_record_ring_sizes(used: usize, free: usize) {
    if !pipe_profile_enabled() {
        return;
    }
    pipe_atomic_max(&PIPE_RING_USED_MAX, used as u64);
    pipe_atomic_min(&PIPE_RING_FREE_MIN, free as u64);
}

fn pipe_finish_read(start: u64, result: &Result<usize, SyscallErr>) {
    if !pipe_profile_enabled() {
        return;
    }
    match result {
        Ok(n) => {
            PIPE_READ_BYTES.fetch_add(*n as u64, Ordering::Relaxed);
            if *n == 0 {
                PIPE_READ_EOF.fetch_add(1, Ordering::Relaxed);
            }
        }
        Err(SyscallErr::EAGAIN) => {
            PIPE_READ_EAGAIN.fetch_add(1, Ordering::Relaxed);
        }
        _ => {}
    }
    pipe_record_cycles(&PIPE_READ_CYCLES_TOTAL, &PIPE_READ_CYCLES_MAX, start);
}

fn pipe_finish_write(start: u64, result: &Result<usize, SyscallErr>) {
    if !pipe_profile_enabled() {
        return;
    }
    match result {
        Ok(n) => {
            PIPE_WRITE_BYTES.fetch_add(*n as u64, Ordering::Relaxed);
        }
        Err(SyscallErr::EAGAIN) => {
            PIPE_WRITE_EAGAIN.fetch_add(1, Ordering::Relaxed);
        }
        Err(SyscallErr::EPIPE) => {
            PIPE_WRITE_EPIPE.fetch_add(1, Ordering::Relaxed);
        }
        _ => {}
    }
    pipe_record_cycles(&PIPE_WRITE_CYCLES_TOTAL, &PIPE_WRITE_CYCLES_MAX, start);
}

const FIONREAD: u32 = 0x541B;
const CAP_SYS_RESOURCE: usize = 24;
const PIPE_SET_SIZE_MAX: usize = 1usize << 31;

pub(crate) fn send_sigpipe_to_current() {
    if let Some(task) = current_task() {
        {
    task.acquire_inner_lock().add_signal(Signals::SIGPIPE);
    task.set_signal_pending();
        }
        task.process.notify_signal_waiters();
    }
}

pub struct Pipe {
    readable: bool,
    writable: bool,
    buffer: Arc<Mutex<PipeRingBuffer>>,
    // Locking: writes notify `read_wait` via `notify_events_at_most(EPOLLIN)`,
    // waking blocked readers.  Reads notify `write_wait` via
    // `notify_events_at_most(EPOLLOUT)`, waking blocked writers.
    pub(crate) read_wait: EventWaitQueue,
    pub(crate) write_wait: EventWaitQueue,
    fasync: crate::fs::vfs::fasync::FAsyncItems,
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
        let profiling = pipe_profile_enabled();
        pipe_inc(profiling, &PIPE_READ_CALLS);
        let profile_start = pipe_profile_start(profiling);
        if !self.readable {
            let result = Err(SyscallErr::EBADF);
            pipe_finish_read(profile_start, &result);
            return result;
        }
        if buf.is_empty() {
            let result = Ok(0);
            pipe_finish_read(profile_start, &result);
            return result;
        }
        let (result, write_end, wake_all_writers) = {
            let mut ring = self.buffer.lock();
            let write_end = ring.write_end.as_ref().and_then(Weak::upgrade);
            if ring.status == RingBufferStatus::EMPTY {
                if write_end.is_none() {
                    pipe_record_ring_sizes(0, ring.get_free_size());
                    let result = Ok(0);
                    pipe_finish_read(profile_start, &result);
                    return result;
                }
                pipe_record_ring_sizes(0, ring.get_free_size());
                let result = Err(SyscallErr::EAGAIN);
                pipe_finish_read(profile_start, &result);
                return result;
            }
            let read_bytes = ring.buffer_read(buf);
            ring.status = if ring.head == ring.tail {
                RingBufferStatus::EMPTY
            } else {
                RingBufferStatus::NORMAL
            };
            let free_size = ring.get_free_size();
            pipe_record_ring_sizes(ring.get_used_size(), free_size);
            // A PIPE_BUF writer needs its whole request to fit.  Waking only
            // one writer when the newly available space is smaller than
            // PIPE_BUF can select an ineligible writer and strand another
            // eligible waiter indefinitely.
            (
                Ok(read_bytes),
                write_end,
                read_bytes > 0 && free_size < PAGE_SIZE,
            )
        };
        if let Ok(_n) = &result {
            if let Some(write_end) = write_end {
                if wake_all_writers {
                    write_end
                        .write_wait
                        .notify_events_all(EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM);
                } else {
                    write_end
                        .write_wait
                        .notify_events_at_most(EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM, 1);
                }
                pipe_inc(profiling, &PIPE_NOTIFY_WRITE);
                if !write_end.fasync.is_empty() {
                    write_end.fasync.send_sigio(None);
                }
            }
        }
        pipe_finish_read(profile_start, &result);
        result
    }

    fn read_at_user(
        &self,
        _offset: usize,
        _len: usize,
        dst: &mut UserBuffer,
    ) -> Result<usize, SyscallErr> {
        let profiling = pipe_profile_enabled();
        pipe_inc(profiling, &PIPE_READ_CALLS);
        let profile_start = pipe_profile_start(profiling);
        if !self.readable {
            let result = Err(SyscallErr::EBADF);
            pipe_finish_read(profile_start, &result);
            return result;
        }
        if dst.len() == 0 {
            let result = Ok(0);
            pipe_finish_read(profile_start, &result);
            return result;
        }
        let (result, write_end, wake_all_writers) = {
            let mut ring = self.buffer.lock();
            let write_end = ring.write_end.as_ref().and_then(Weak::upgrade);
            if ring.status == RingBufferStatus::EMPTY {
                if write_end.is_none() {
                    pipe_record_ring_sizes(0, ring.get_free_size());
                    let result = Ok(0);
                    pipe_finish_read(profile_start, &result);
                    return result;
                }
                pipe_record_ring_sizes(0, ring.get_free_size());
                let result = Err(SyscallErr::EAGAIN);
                pipe_finish_read(profile_start, &result);
                return result;
            }
            // ring 锁必须覆盖“复制多少、消费多少”的状态变更，否则两个 reader
            // 可能消费同一段数据。UserBuffer 已在锁外 fault-in；锁内只走 nofault
            // 复制，映射一旦并发变化就返回，不让 spin lock 跨越 fault 等待点。
            let mut total = 0usize;
            let mut copy_failed = false;
            let dst_len = dst.len();
            while total < dst_len && ring.status != RingBufferStatus::EMPTY {
                let seg_start = ring.head;
                let seg_end = if ring.tail <= ring.head {
                    ring.capacity
                } else {
                    ring.tail
                };
                let seg_len = (dst_len - total).min(seg_end - seg_start);
                if seg_len == 0 {
                    break;
                }
                let seg_bytes = &ring.arr[seg_start..seg_start + seg_len];
                let n = match dst.write_from_at_nofault(total, seg_bytes) {
                    Ok(n) => n,
                    Err(_) => {
                        copy_failed = true;
                        break;
                    }
                };
                ring.head = if seg_start + n == ring.capacity {
                    0
                } else {
                    seg_start + n
                };
                total += n;
                ring.status = if ring.head == ring.tail {
                    RingBufferStatus::EMPTY
                } else {
                    RingBufferStatus::NORMAL
                };
                if n < seg_len {
                    break;
                }
            }
            let free_size = ring.get_free_size();
            pipe_record_ring_sizes(ring.get_used_size(), free_size);
            let result = if copy_failed && total == 0 {
                Err(SyscallErr::EFAULT)
            } else {
                Ok(total)
            };
            (result, write_end, total > 0 && free_size < PAGE_SIZE)
        };
        if let Ok(_n) = &result {
            if let Some(write_end) = write_end {
                if wake_all_writers {
                    write_end
                        .write_wait
                        .notify_events_all(EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM);
                } else {
                    write_end
                        .write_wait
                        .notify_events_at_most(EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM, 1);
                }
                pipe_inc(profiling, &PIPE_NOTIFY_WRITE);
                if !write_end.fasync.is_empty() {
                    write_end.fasync.send_sigio(None);
                }
            }
        }
        pipe_finish_read(profile_start, &result);
        result
    }

    fn write_at(
        &self,
        _offset: usize,
        _len: usize,
        buf: &[u8],
        _data: spin::MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        let profiling = pipe_profile_enabled();
        pipe_inc(profiling, &PIPE_WRITE_CALLS);
        let profile_start = pipe_profile_start(profiling);
        if !self.writable {
            let result = Err(SyscallErr::EBADF);
            pipe_finish_write(profile_start, &result);
            return result;
        }
        if buf.is_empty() {
            let result = Ok(0);
            pipe_finish_write(profile_start, &result);
            return result;
        }
        let (result, read_end) = {
            let mut ring = self.buffer.lock();
            let read_end = ring.read_end.as_ref().and_then(Weak::upgrade);
            if read_end.is_none() {
                pipe_record_ring_sizes(ring.get_used_size(), ring.get_free_size());
                (Err(SyscallErr::EPIPE), None)
            } else if ring.status == RingBufferStatus::FULL {
                pipe_record_ring_sizes(ring.get_used_size(), 0);
                (Err(SyscallErr::EAGAIN), read_end)
            } else if buf.len() <= PAGE_SIZE && ring.get_free_size() < buf.len() {
                pipe_record_ring_sizes(ring.get_used_size(), ring.get_free_size());
                (Err(SyscallErr::EAGAIN), read_end)
            } else {
                let write_bytes = ring.buffer_write(buf);
                ring.status = if ring.head == ring.tail {
                    RingBufferStatus::FULL
                } else {
                    RingBufferStatus::NORMAL
                };
                pipe_record_ring_sizes(ring.get_used_size(), ring.get_free_size());
                (Ok(write_bytes), read_end)
            }
        };
        if matches!(result, Err(SyscallErr::EPIPE)) {
            send_sigpipe_to_current();
        }
        if let Ok(_n) = &result {
            if let Some(read_end) = read_end {
                read_end
                    .read_wait
                    .notify_events_at_most(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM, 1);
                pipe_inc(profiling, &PIPE_NOTIFY_READ);
                if !read_end.fasync.is_empty() {
                    read_end.fasync.send_sigio(None);
                }
            }
        }
        pipe_finish_write(profile_start, &result);
        result
    }

    fn write_at_user(
        &self,
        _offset: usize,
        _len: usize,
        src: &UserBuffer,
    ) -> Result<usize, SyscallErr> {
        let profiling = pipe_profile_enabled();
        pipe_inc(profiling, &PIPE_WRITE_CALLS);
        let profile_start = pipe_profile_start(profiling);
        if !self.writable {
            let result = Err(SyscallErr::EBADF);
            pipe_finish_write(profile_start, &result);
            return result;
        }
        if src.len() == 0 {
            let result = Ok(0);
            pipe_finish_write(profile_start, &result);
            return result;
        }
        let (result, read_end) = {
            let mut ring = self.buffer.lock();
            let read_end = ring.read_end.as_ref().and_then(Weak::upgrade);
            if read_end.is_none() {
                pipe_record_ring_sizes(ring.get_used_size(), ring.get_free_size());
                (Err(SyscallErr::EPIPE), None)
            } else if ring.status == RingBufferStatus::FULL {
                pipe_record_ring_sizes(ring.get_used_size(), 0);
                (Err(SyscallErr::EAGAIN), read_end)
            } else if src.len() <= PAGE_SIZE && ring.get_free_size() < src.len() {
                pipe_record_ring_sizes(ring.get_used_size(), ring.get_free_size());
                (Err(SyscallErr::EAGAIN), read_end)
            } else {
                // ring 锁同时保护 PIPE_BUF 原子写与 tail 推进。这里同样只走
                // nofault 复制，避免 spin lock 内调页、CoW 或等待后端 I/O。
                let mut total = 0usize;
                let mut copy_failed = false;
                let src_len = src.len();
                while total < src_len && ring.status != RingBufferStatus::FULL {
                    let seg_start = ring.tail;
                    let seg_end = if ring.tail < ring.head {
                        ring.head
                    } else {
                        ring.capacity
                    };
                    let free = ring.get_free_size();
                    let seg_len = (src_len - total)
                        .min(free)
                        .min(seg_end - seg_start)
                        .min(PAGE_SIZE);
                    if seg_len == 0 {
                        break;
                    }
                    // 每次只推进实际复制的字节；后续页失败时保留已经完成的前缀。
                    let n = match src
                        .read_into_at_nofault(total, &mut ring.arr[seg_start..seg_start + seg_len])
                    {
                        Ok(n) => n,
                        Err(_) => {
                            copy_failed = true;
                            break;
                        }
                    };
                    if n == 0 {
                        break;
                    }
                    ring.tail = if seg_start + n == ring.capacity {
                        0
                    } else {
                        seg_start + n
                    };
                    total += n;
                    ring.status = if ring.head == ring.tail {
                        RingBufferStatus::FULL
                    } else {
                        RingBufferStatus::NORMAL
                    };
                    if n < seg_len {
                        break;
                    }
                }
                pipe_record_ring_sizes(ring.get_used_size(), ring.get_free_size());
                let result = if copy_failed && total == 0 {
                    Err(SyscallErr::EFAULT)
                } else {
                    Ok(total)
                };
                (result, read_end)
            }
        };
        if matches!(result, Err(SyscallErr::EPIPE)) {
            send_sigpipe_to_current();
        }
        if let Ok(_n) = &result {
            if let Some(read_end) = read_end {
                read_end
                    .read_wait
                    .notify_events_at_most(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM, 1);
                pipe_inc(profiling, &PIPE_NOTIFY_READ);
                if !read_end.fasync.is_empty() {
                    read_end.fasync.send_sigio(None);
                }
            }
        }
        pipe_finish_write(profile_start, &result);
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

    fn supports_user_buffer_io(&self) -> bool {
        true
    }

    fn poll(&self, _private_data: &FilePrivateData) -> Result<usize, SyscallErr> {
        let profiling = pipe_profile_enabled();
        pipe_inc(profiling, &PIPE_POLL_CALLS);
        let profile_start = pipe_profile_start(profiling);
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
        pipe_record_ring_sizes(ring.get_used_size(), ring.get_free_size());
        pipe_record_cycles(
            &PIPE_POLL_CYCLES_TOTAL,
            &PIPE_POLL_CYCLES_MAX,
            profile_start,
        );
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

    fn fasync_items(&self) -> Option<&crate::fs::vfs::fasync::FAsyncItems> {
        Some(&self.fasync)
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
        let (result, write_end) = {
            let mut ring = self.buffer.lock();
            let old_capacity = ring.capacity;
            let was_full = ring.status == RingBufferStatus::FULL;
            let result = ring.set_capacity_compat(requested);
            let write_end = if result.is_ok()
                && was_full
                && ring.capacity > old_capacity
                && ring.get_free_size() > 0
            {
                ring.write_end.as_ref().and_then(Weak::upgrade)
            } else {
                None
            };
            (result, write_end)
        };

        if let Some(write_end) = write_end {
            write_end.write_wait.notify_events_all(EPollEvent::EPOLLOUT);
        }
        result
    }

    /// Pass a wake-one reader baton after an interrupted or failed wait.
    /// The ring lock is deliberately released before waking the queue.
    pub(crate) fn pass_reader_baton_if_data(&self) {
        let has_data = {
            let ring = self.buffer.lock();
            ring.status != RingBufferStatus::EMPTY
        };
        if has_data {
            self.read_wait
                .notify_events_all(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM);
        }
    }

    /// Pass a wake-one writer baton after an interrupted or failed wait.
    /// The ring lock is deliberately released before waking the queue.
    pub(crate) fn pass_writer_baton_if_space(&self) {
        let has_space = {
            let ring = self.buffer.lock();
            ring.get_free_size() > 0
        };
        if has_space {
            self.write_wait
                .notify_events_all(EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM);
        }
    }

    /// Return a reference to the shared ring buffer `Arc`.
    /// Used by splice/tee to detect same-pipe identity via `Arc::ptr_eq`.
    pub fn buffer_arc(&self) -> &Arc<Mutex<PipeRingBuffer>> {
        &self.buffer
    }

    /// Peek data from the pipe without consuming it (head not advanced).
    /// Returns the number of bytes copied into `buf`.
    pub fn peek_at(&self, buf: &mut [u8]) -> usize {
        self.buffer.lock().peek_data(buf)
    }

    pub(crate) fn peer_read_end(&self) -> Option<Arc<Pipe>> {
        self.buffer.lock().read_end.as_ref().and_then(Weak::upgrade)
    }

    pub(crate) fn peer_write_end(&self) -> Option<Arc<Pipe>> {
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
            fasync: crate::fs::vfs::fasync::FAsyncItems::new(),
        }
    }
    pub fn write_end_with_buffer(buffer: Arc<Mutex<PipeRingBuffer>>) -> Self {
        Self {
            readable: false,
            writable: true,
            buffer,
            read_wait: EventWaitQueue::new(),
            write_wait: EventWaitQueue::new(),
            fasync: crate::fs::vfs::fasync::FAsyncItems::new(),
        }
    }
    pub fn read_write_end_with_buffer(buffer: Arc<Mutex<PipeRingBuffer>>) -> Self {
        Self {
            readable: true,
            writable: true,
            buffer,
            read_wait: EventWaitQueue::new(),
            write_wait: EventWaitQueue::new(),
            fasync: crate::fs::vfs::fasync::FAsyncItems::new(),
        }
    }
}

const RING_DEFAULT_BUFFER_SIZE: usize = 4096 * 16;

use core::sync::atomic::AtomicUsize;
static PIPE_BUF_COUNT: AtomicUsize = AtomicUsize::new(0);
static PIPE_BUF_BYTES: AtomicUsize = AtomicUsize::new(0);
static PIPE_MAX_SIZE: AtomicUsize = AtomicUsize::new(RING_DEFAULT_BUFFER_SIZE);
pub fn pipe_buf_alive() -> usize {
    PIPE_BUF_COUNT.load(Ordering::Relaxed)
}
pub fn pipe_buf_bytes() -> usize {
    PIPE_BUF_BYTES.load(Ordering::Relaxed)
}
pub fn pipe_max_size() -> usize {
    PIPE_MAX_SIZE.load(Ordering::Relaxed)
}
pub fn set_pipe_max_size(size: usize) -> bool {
    if size < PAGE_SIZE || size > RING_DEFAULT_BUFFER_SIZE {
        return false;
    }
    PIPE_MAX_SIZE.store(size, Ordering::Relaxed);
    true
}
pub fn pipe_user_pages_soft() -> usize {
    16384
}
pub fn pipe_user_pages_hard() -> usize {
    0
}

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
        let alive = PIPE_BUF_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        let bytes = PIPE_BUF_BYTES.fetch_add(RING_DEFAULT_BUFFER_SIZE, Ordering::Relaxed)
            + RING_DEFAULT_BUFFER_SIZE;
        let profiling = pipe_profile_enabled();
        pipe_inc(profiling, &PIPE_BUF_ALLOC);
        if profiling {
            pipe_atomic_max(&PIPE_BUF_ALIVE_MAX, alive as u64);
            pipe_atomic_max(&PIPE_BUF_BYTES_MAX, bytes as u64);
        }
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
    pub(crate) fn get_used_size(&self) -> usize {
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
        // After capacity increase, a formerly FULL ring may now
        // have free space and must transition to NORMAL so that
        // subsequent writes can proceed and blocked writers are
        // correctly woken by set_pipe_capacity_compat().
        if self.status == RingBufferStatus::FULL && self.get_free_size() > 0 {
            self.status = RingBufferStatus::NORMAL;
        }
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
            // Safety: `begin + read_bytes <= self.capacity` (guaranteed by
            // `read_bytes ≤ end - begin ≤ self.capacity` above), and
            // `total + read_bytes ≤ buf.len()` (guaranteed by the `while` loop
            // bound).  Source (`self.arr`) and destination (`buf`) are disjoint
            // (ring buffer on heap vs. caller-provided buffer).
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
            // Safety: `begin + write_bytes ≤ self.capacity` (guaranteed by
            // `write_bytes ≤ end - begin ≤ self.capacity` above), and
            // `total + write_bytes ≤ buf.len()` (guaranteed by the `while` loop
            // bound).  Source (`buf`) and destination (`self.arr`) are disjoint.
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
    /// Peek data from the ring buffer without advancing head.
    /// Used by tee() to duplicate pipe data without consuming it.
    /// Returns the number of bytes actually copied (≤ `buf.len()` and available data).
    pub fn peek_data(&self, buf: &mut [u8]) -> usize {
        if self.status == RingBufferStatus::EMPTY {
            return 0;
        }
        let available = self.get_used_size();
        let n = buf.len().min(available);
        let mut total = 0;
        let mut pos = self.head;
        while total < n {
            let end = if self.tail <= pos {
                self.capacity
            } else {
                self.tail
            };
            let chunk = (n - total).min(end - pos);
            if chunk == 0 {
                break;
            }
            // Safety: pos..pos+chunk is within arr bounds (guaranteed by ring invariants),
            // and total..total+chunk is within buf bounds.
            unsafe {
                copy_nonoverlapping(
                    self.arr.as_ptr().add(pos),
                    buf.as_mut_ptr().add(total),
                    chunk,
                );
            }
            total += chunk;
            pos = if pos + chunk == self.capacity {
                0
            } else {
                pos + chunk
            };
        }
        total
    }

    /// Atomically move up to `max_bytes` bytes from `src` into `dst`.
    /// Both ring buffers MUST already be locked by the caller.
    /// Returns the number of bytes actually moved (0 if no progress could
    /// be made — src empty, dst full, or both).
    pub fn splice_move(src: &mut Self, dst: &mut Self, max_bytes: usize) -> usize {
        let avail = src.get_used_size();
        let space = dst.get_free_size();
        let want = max_bytes.min(avail).min(space);
        if want == 0 {
            return 0;
        }

        let mut remaining = want;
        while remaining > 0 {
            // --- readable segment in `src` (ring-buffer invariant) -------
            // Valid data spans [src.head, src.tail) wrapping at capacity.
            let src_seg_end = if src.tail <= src.head {
                src.capacity
            } else {
                src.tail
            };
            let src_seg_len = (src_seg_end - src.head).min(remaining);
            if src_seg_len == 0 {
                break;
            }

            // --- writable segment in `dst` (ring-buffer invariant) ------
            // Free region spans [dst.tail, dst.head) wrapping at capacity.
            let dst_seg_end = if dst.tail < dst.head {
                dst.head
            } else {
                dst.capacity
            };
            let dst_seg_len = (dst_seg_end - dst.tail).min(src_seg_len);
            if dst_seg_len == 0 {
                break;
            }

            let chunk = src_seg_len.min(dst_seg_len);
            // Safety: `src.arr` and `dst.arr` are two distinct heap
            // allocations (each in its own `Box`).  The [head, head+chunk)
            // and [tail, tail+chunk) ranges are within their respective
            // capacities because the segment calculations above are bounded
            // by `remaining <= want <= space` and the ring invariants.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    src.arr.as_ptr().add(src.head),
                    dst.arr.as_mut_ptr().add(dst.tail),
                    chunk,
                );
            }

            src.head = if src.head + chunk == src.capacity {
                0
            } else {
                src.head + chunk
            };
            dst.tail = if dst.tail + chunk == dst.capacity {
                0
            } else {
                dst.tail + chunk
            };
            remaining -= chunk;
        }

        let moved = want - remaining;
        if moved > 0 {
            src.status = if src.head == src.tail {
                RingBufferStatus::EMPTY
            } else {
                RingBufferStatus::NORMAL
            };
            dst.status = if dst.head == dst.tail {
                RingBufferStatus::FULL
            } else {
                RingBufferStatus::NORMAL
            };
        }
        moved
    }

    fn set_write_end(&mut self, write_end: &Arc<Pipe>) {
        self.write_end = Some(Arc::downgrade(write_end));
    }
    fn set_read_end(&mut self, read_end: &Arc<Pipe>) {
        self.read_end = Some(Arc::downgrade(read_end));
    }
    pub(crate) fn all_write_ends_closed(&self) -> bool {
        self.write_end.as_ref().unwrap().upgrade().is_none()
    }
    pub(crate) fn all_read_ends_closed(&self) -> bool {
        self.read_end.as_ref().unwrap().upgrade().is_none()
    }
}

impl Drop for PipeRingBuffer {
    fn drop(&mut self) {
        pipe_inc(pipe_profile_enabled(), &PIPE_BUF_DROP);
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

static FIFO_REGISTRY: spin::Mutex<BTreeMap<(usize, usize), FifoEntry>> =
    spin::Mutex::new(BTreeMap::new());

/// Open a named FIFO inode, returning a Pipe end matching the access mode.
/// `dev_inode` identifies the FIFO (dev_id, inode_id).
/// `for_read` selects the read end; `for_write` selects the write end.
pub fn fifo_open(
    dev_inode: (usize, usize),
    for_read: bool,
    for_write: bool,
    nonblock: bool,
) -> Result<Arc<Pipe>, SyscallErr> {
    let profiling = pipe_profile_enabled();
    pipe_inc(profiling, &PIPE_FIFO_OPEN);
    let mut reg = FIFO_REGISTRY.lock();
    // 清理两端都已关闭的陈旧条目，防止 64KB PipeRingBuffer 永久泄漏。
    if let Some(entry) = reg.get(&dev_inode) {
        if entry.read_end.strong_count() == 0 && entry.write_end.strong_count() == 0 {
            reg.remove(&dev_inode);
        }
    }
    let len_after_entry = reg.len() + usize::from(!reg.contains_key(&dev_inode));
    if profiling {
        pipe_atomic_max(&PIPE_FIFO_REGISTRY_LEN_MAX, len_after_entry as u64);
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

    if for_read && for_write {
        if let Some(end) = entry.read_end.upgrade() {
            if end.writable {
                pipe_inc(profiling, &PIPE_FIFO_HIT_READ);
                pipe_inc(profiling, &PIPE_FIFO_HIT_WRITE);
                return Ok(end);
            }
        }
        let end = Arc::new(Pipe::read_write_end_with_buffer(buffer.clone()));
        buffer.lock().set_read_end(&end);
        buffer.lock().set_write_end(&end);
        entry.read_end = Arc::downgrade(&end);
        entry.write_end = Arc::downgrade(&end);
        return Ok(end);
    }

    if for_read {
        if let Some(r) = entry.read_end.upgrade() {
            pipe_inc(profiling, &PIPE_FIFO_HIT_READ);
            return Ok(r);
        }
        let r = Arc::new(Pipe::read_end_with_buffer(buffer.clone()));
        buffer.lock().set_read_end(&r);
        entry.read_end = Arc::downgrade(&r);
        return Ok(r);
    }

    if for_write {
        // O_WRONLY | O_NONBLOCK 且无读者 → ENXIO（Linux 语义）
        if nonblock && entry.read_end.strong_count() == 0 {
            return Err(SyscallErr::ENXIO);
        }
        if let Some(w) = entry.write_end.upgrade() {
            pipe_inc(profiling, &PIPE_FIFO_HIT_WRITE);
            return Ok(w);
        }
        let w = Arc::new(Pipe::write_end_with_buffer(buffer.clone()));
        buffer.lock().set_write_end(&w);
        entry.write_end = Arc::downgrade(&w);
        return Ok(w);
    }

    // O_RDWR: return write end (rare case)
    if let Some(w) = entry.write_end.upgrade() {
        pipe_inc(profiling, &PIPE_FIFO_HIT_WRITE);
        return Ok(w);
    }
    let w = Arc::new(Pipe::write_end_with_buffer(buffer.clone()));
    buffer.lock().set_write_end(&w);
    entry.write_end = Arc::downgrade(&w);
    Ok(w)
}

/// 清理 FIFO_REGISTRY 中所有两端都已关闭的陈旧条目，
/// 释放持有的 64KB PipeRingBuffer。由 reclaim 周期性触发。
pub fn compact_fifo_registry() -> usize {
    let profiling = pipe_profile_enabled();
    pipe_inc(profiling, &PIPE_FIFO_COMPACT_CALLS);
    let mut reg = FIFO_REGISTRY.lock();
    let before = reg.len();
    reg.retain(|_, entry| entry.read_end.strong_count() > 0 || entry.write_end.strong_count() > 0);
    let removed = before - reg.len();
    pipe_add(profiling, &PIPE_FIFO_COMPACT_REMOVED, removed as u64);
    if profiling {
        pipe_atomic_max(&PIPE_FIFO_REGISTRY_LEN_MAX, reg.len() as u64);
    }
    removed
}

// ── Public accessors for /sys/kernel/stats/pipe ──────────────────────

pub fn pipe_read_calls() -> u64 {
    PIPE_READ_CALLS.load(Ordering::Relaxed)
}
pub fn pipe_write_calls() -> u64 {
    PIPE_WRITE_CALLS.load(Ordering::Relaxed)
}
pub fn pipe_read_bytes() -> u64 {
    PIPE_READ_BYTES.load(Ordering::Relaxed)
}
pub fn pipe_write_bytes() -> u64 {
    PIPE_WRITE_BYTES.load(Ordering::Relaxed)
}
pub fn pipe_read_cycles() -> u64 {
    PIPE_READ_CYCLES_TOTAL.load(Ordering::Relaxed)
}
pub fn pipe_write_cycles() -> u64 {
    PIPE_WRITE_CYCLES_TOTAL.load(Ordering::Relaxed)
}
pub fn pipe_read_cycles_max() -> u64 {
    PIPE_READ_CYCLES_MAX.load(Ordering::Relaxed)
}
pub fn pipe_write_cycles_max() -> u64 {
    PIPE_WRITE_CYCLES_MAX.load(Ordering::Relaxed)
}
pub fn pipe_read_eagain() -> u64 {
    PIPE_READ_EAGAIN.load(Ordering::Relaxed)
}
pub fn pipe_write_eagain() -> u64 {
    PIPE_WRITE_EAGAIN.load(Ordering::Relaxed)
}
