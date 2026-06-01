use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use core::any::Any;
use core::sync::atomic::{AtomicUsize, Ordering};
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
    mm::{UserPtr, UserPtrMut},
    task::{current_task, current_user_token, WaitQueue},
    timer::{current_timespec, TimeSpec, NSEC_PER_SEC},
    utils::error::SyscallErr,
};

const CLOCK_REALTIME: usize = 0;
const CLOCK_MONOTONIC: usize = 1;
const CLOCK_BOOTTIME: usize = 7;
const CLOCK_REALTIME_ALARM: usize = 8;
const CLOCK_BOOTTIME_ALARM: usize = 9;

const TFD_CLOEXEC: u32 = 0o2000000;
const TFD_NONBLOCK: u32 = 0o4000;
const TFD_CREATE_VALID_FLAGS: u32 = TFD_CLOEXEC | TFD_NONBLOCK;
const TFD_TIMER_ABSTIME: u32 = 1;
const TFD_TIMER_CANCEL_ON_SET: u32 = 2;
const TFD_SETTIME_VALID_FLAGS: u32 = TFD_TIMER_ABSTIME | TFD_TIMER_CANCEL_ON_SET;

static TIMERFD_REGISTRY: Mutex<Vec<Weak<TimerFd>>> = Mutex::new(Vec::new());
static TIMERFD_SWEEP_TICKS: AtomicUsize = AtomicUsize::new(0);

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TimerFdSpec {
    pub it_interval: TimeSpec,
    pub it_value: TimeSpec,
}

#[derive(Debug)]
struct TimerFdState {
    interval: TimeSpec,
    deadline: Option<TimeSpec>,
    expirations: u64,
}

pub struct TimerFd {
    clock_id: usize,
    inner: Mutex<TimerFdState>,
    read_wait: EventWaitQueue,
    metadata: Metadata,
}

impl core::fmt::Debug for TimerFd {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TimerFd")
            .field("clock_id", &self.clock_id)
            .finish()
    }
}

impl TimerFd {
    fn new(clock_id: usize) -> Self {
        Self {
            clock_id,
            inner: Mutex::new(TimerFdState {
                interval: TimeSpec::new(),
                deadline: None,
                expirations: 0,
            }),
            read_wait: EventWaitQueue::new(),
            metadata: Metadata::new(
                FileType::File,
                InodeMode::S_IFREG | InodeMode::from_bits_truncate(0o600),
            ),
        }
    }

    fn update_locked(inner: &mut TimerFdState, now: TimeSpec) {
        let Some(deadline) = inner.deadline else {
            return;
        };
        if now < deadline {
            return;
        }

        if inner.interval.is_zero() {
            inner.expirations = inner.expirations.saturating_add(1);
            inner.deadline = None;
            return;
        }

        let interval_ns = inner.interval.to_ns().max(1);
        let elapsed_ns = now.to_ns().saturating_sub(deadline.to_ns());
        let count = 1usize.saturating_add(elapsed_ns / interval_ns);
        inner.expirations = inner.expirations.saturating_add(count as u64);
        let next_ns = deadline
            .to_ns()
            .saturating_add(count.saturating_mul(interval_ns));
        inner.deadline = Some(TimeSpec::from_ns(next_ns));
    }

    fn current_spec_locked(inner: &TimerFdState, now: TimeSpec) -> TimerFdSpec {
        let value = inner
            .deadline
            .map(|deadline| deadline - now)
            .unwrap_or_else(TimeSpec::new);
        TimerFdSpec {
            it_interval: inner.interval,
            it_value: value,
        }
    }

    fn notify_readable(&self) {
        self.read_wait
            .notify_events_all(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM);
    }

    fn wake_if_expired(&self, now_hint: TimeSpec) {
        let now = if matches!(self.clock_id, CLOCK_REALTIME | CLOCK_REALTIME_ALARM) {
            timerfd_clock_now(self.clock_id)
        } else {
            now_hint
        };
        let became_readable = {
            let mut inner = self.inner.lock();
            let was_empty = inner.expirations == 0;
            Self::update_locked(&mut inner, now);
            was_empty && inner.expirations > 0
        };
        if became_readable {
            self.notify_readable();
        }
    }

    fn get_time(&self) -> TimerFdSpec {
        let now = timerfd_clock_now(self.clock_id);
        let mut inner = self.inner.lock();
        Self::update_locked(&mut inner, now);
        Self::current_spec_locked(&inner, now)
    }

    fn set_time(
        &self,
        flags: u32,
        new_value: TimerFdSpec,
        need_old_value: bool,
    ) -> Result<TimerFdSpec, SyscallErr> {
        if (flags & !TFD_SETTIME_VALID_FLAGS) != 0 {
            return Err(SyscallErr::EINVAL);
        }
        validate_timespec(new_value.it_interval)?;
        validate_timespec(new_value.it_value)?;

        let armed = !new_value.it_value.is_zero();
        let now = if need_old_value || armed {
            Some(timerfd_clock_now(self.clock_id))
        } else {
            None
        };
        let old_value = {
            let mut inner = self.inner.lock();
            if let Some(now) = now {
                Self::update_locked(&mut inner, now);
            }
            let old_value = if need_old_value {
                Self::current_spec_locked(&inner, now.unwrap())
            } else {
                TimerFdSpec {
                    it_interval: TimeSpec::new(),
                    it_value: TimeSpec::new(),
                }
            };
            inner.interval = new_value.it_interval;
            inner.expirations = 0;
            inner.deadline = if new_value.it_value.is_zero() {
                None
            } else if (flags & TFD_TIMER_ABSTIME) != 0 {
                Some(new_value.it_value)
            } else {
                Some(now.unwrap() + new_value.it_value)
            };
            old_value
        };
        if let Some(now) = now {
            self.wake_if_expired(now);
        }
        Ok(old_value)
    }
}

impl IndexNode for TimerFd {
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

        let expirations = {
            let mut inner = self.inner.lock();
            Self::update_locked(&mut inner, timerfd_clock_now(self.clock_id));
            if inner.expirations == 0 {
                return Err(SyscallErr::EAGAIN);
            }
            let expirations = inner.expirations;
            inner.expirations = 0;
            expirations
        };

        buf[..8].copy_from_slice(&expirations.to_ne_bytes());
        Ok(8)
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
        let mut inner = self.inner.lock();
        Self::update_locked(&mut inner, timerfd_clock_now(self.clock_id));
        let events = if inner.expirations > 0 {
            EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM
        } else {
            EPollEvent::empty()
        };
        Ok(events.bits())
    }

    fn read_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(self.read_wait.wait_queue())
    }

    fn read_event_queue(&self) -> Option<&EventWaitQueue> {
        Some(&self.read_wait)
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

fn validate_clock_id(clock_id: usize) -> Result<(), SyscallErr> {
    match clock_id {
        CLOCK_REALTIME | CLOCK_MONOTONIC | CLOCK_BOOTTIME | CLOCK_REALTIME_ALARM
        | CLOCK_BOOTTIME_ALARM => Ok(()),
        _ => Err(SyscallErr::EINVAL),
    }
}

fn validate_timespec(value: TimeSpec) -> Result<(), SyscallErr> {
    const MAX_TIMERFD_SEC: usize = usize::MAX / NSEC_PER_SEC - 1;

    if value.tv_sec > MAX_TIMERFD_SEC || value.tv_nsec >= NSEC_PER_SEC {
        Err(SyscallErr::EINVAL)
    } else {
        Ok(())
    }
}

fn timerfd_clock_now(clock_id: usize) -> TimeSpec {
    match clock_id {
        CLOCK_REALTIME | CLOCK_REALTIME_ALARM => current_timespec(),
        _ => TimeSpec::now(),
    }
}

fn register_timerfd(timerfd: &Arc<TimerFd>) {
    TIMERFD_REGISTRY.lock().push(Arc::downgrade(timerfd));
}

pub fn wake_expired_timerfds(now: TimeSpec) {
    let mut registry = TIMERFD_REGISTRY.lock();
    for weak in registry.iter() {
        if let Some(timerfd) = weak.upgrade() {
            timerfd.wake_if_expired(now);
        }
    }

    if TIMERFD_SWEEP_TICKS.fetch_add(1, Ordering::Relaxed) % 64 == 0 {
        registry.retain(|weak| weak.strong_count() > 0);
    }
}

fn with_timerfd<R>(
    fd: usize,
    f: impl FnOnce(&TimerFd) -> Result<R, SyscallErr>,
) -> Result<R, SyscallErr> {
    let task = current_task().ok_or(SyscallErr::ESRCH)?;
    let files = task.process.files();
    let fd_table = files.lock();
    let file = fd_table.get_file(fd)?;
    let Some(timerfd) = file.inode_as_any_ref().downcast_ref::<TimerFd>() else {
        return Err(SyscallErr::EINVAL);
    };
    f(timerfd)
}

pub fn sys_timerfd_create(clock_id: usize, flags: u32) -> isize {
    if validate_clock_id(clock_id).is_err() || (flags & !TFD_CREATE_VALID_FLAGS) != 0 {
        return -(SyscallErr::EINVAL as isize);
    }

    let mut file_flags = FileFlags::O_RDWR;
    if (flags & TFD_NONBLOCK) != 0 {
        file_flags |= FileFlags::O_NONBLOCK;
    }
    if (flags & TFD_CLOEXEC) != 0 {
        file_flags |= FileFlags::O_CLOEXEC;
    }

    let timerfd = Arc::new(TimerFd::new(clock_id));
    register_timerfd(&timerfd);
    let inode = timerfd as Arc<dyn IndexNode>;
    let file = match File::new(inode, file_flags) {
        Ok(file) => file,
        Err(err) => return -(err as isize),
    };

    let task = current_task().unwrap();
    let files = task.process.files();
    let ret = match files.lock().alloc_fd(file, (flags & TFD_CLOEXEC) != 0) {
        Ok(fd) => fd as isize,
        Err(err) => -(err as isize),
    };
    ret
}

pub fn sys_timerfd_gettime(fd: usize, curr_value_ptr: *mut TimerFdSpec) -> isize {
    let curr_value = match with_timerfd(fd, |timerfd| Ok(timerfd.get_time())) {
        Ok(value) => value,
        Err(err) => return -(err as isize),
    };
    if curr_value_ptr.is_null() {
        return -(SyscallErr::EFAULT as isize);
    }
    match UserPtrMut::new(curr_value_ptr).write(current_user_token(), &curr_value) {
        Ok(()) => 0,
        Err(errno) => errno,
    }
}

pub fn sys_timerfd_settime(
    fd: usize,
    flags: u32,
    new_value: *const TimerFdSpec,
    old_value: *mut TimerFdSpec,
) -> isize {
    if new_value.is_null() {
        return -(SyscallErr::EFAULT as isize);
    }
    let token = current_user_token();
    let new_value = match UserPtr::new(new_value).read(token) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    let old_spec = match with_timerfd(fd, |timerfd| {
        timerfd.set_time(flags, new_value, !old_value.is_null())
    }) {
        Ok(old_spec) => old_spec,
        Err(err) => return -(err as isize),
    };
    if !old_value.is_null() {
        match UserPtrMut::new(old_value).write(token, &old_spec) {
            Ok(()) => {}
            Err(errno) => return errno,
        }
    }
    0
}
