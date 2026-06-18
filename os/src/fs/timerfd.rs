use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    task::{add_kernel_timer, current_task, current_user_token, TimerAction, WaitQueue},
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
static TIMERFD_REGISTRY_MAYBE_NONEMPTY: AtomicBool = AtomicBool::new(false);
static TIMERFD_SWEEP_TICKS: AtomicUsize = AtomicUsize::new(0);
static TIMERFD_SWEEP_GENERATION: AtomicUsize = AtomicUsize::new(0);

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TimerFdSpec {
    pub it_interval: TimeSpec,
    pub it_value: TimeSpec,
}

#[derive(Debug)]
struct TimerFdState {
    interval: TimeSpec,
    /// Monotonic deadline used by the kernel timer queue.
    deadline: Option<TimeSpec>,
    /// Original realtime absolute deadline.  Only set for CLOCK_REALTIME
    /// timers armed with TFD_TIMER_ABSTIME; relative timers must not move when
    /// wall-clock time is adjusted.
    realtime_abs_deadline: Option<TimeSpec>,
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
                realtime_abs_deadline: None,
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
            inner.realtime_abs_deadline = None;
            return;
        }

        let interval_ns = inner.interval.to_ns_saturating().max(1) as usize;
        let deadline_ns = deadline.to_ns_saturating() as usize;
        let elapsed_ns = (now.to_ns_saturating() as usize).saturating_sub(deadline_ns);
        let count = 1usize.saturating_add(elapsed_ns / interval_ns);
        inner.expirations = inner.expirations.saturating_add(count as u64);
        let next_ns = deadline_ns.saturating_add(count.saturating_mul(interval_ns));
        inner.deadline = Some(TimeSpec::from_ns(next_ns));
        if let Some(abs_deadline) = inner.realtime_abs_deadline {
            let abs_ns = abs_deadline
                .to_ns_saturating()
                .saturating_add(count.saturating_mul(interval_ns) as u64);
            inner.realtime_abs_deadline = Some(TimeSpec::from_ns(abs_ns as usize));
        }
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

    fn notify_readable(&self) -> usize {
        self.read_wait
            .notify_events_all(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM)
    }

    fn wake_if_expired(&self, now_hint: TimeSpec) -> usize {
        let became_readable = {
            let mut inner = self.inner.lock();
            let was_empty = inner.expirations == 0;
            Self::update_locked(&mut inner, now_hint);
            was_empty && inner.expirations > 0
        };
        if became_readable {
            self.notify_readable()
        } else {
            0
        }
    }

    fn next_sweep_deadline(&self) -> Option<TimeSpec> {
        let inner = self.inner.lock();
        inner.deadline
    }

    fn get_time(&self) -> TimerFdSpec {
        let now = TimeSpec::now();
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
        let now_monotonic = if need_old_value || armed {
            Some(TimeSpec::now())
        } else {
            None
        };
        let now_clock = if armed && matches!(self.clock_id, CLOCK_REALTIME | CLOCK_REALTIME_ALARM) {
            Some(timerfd_clock_now(self.clock_id))
        } else {
            now_monotonic
        };
        let mut notify_readable = false;
        let old_value = {
            let mut inner = self.inner.lock();
            if let Some(now) = now_monotonic {
                Self::update_locked(&mut inner, now);
            }
            let old_value = if need_old_value {
                Self::current_spec_locked(&inner, now_monotonic.unwrap())
            } else {
                TimerFdSpec {
                    it_interval: TimeSpec::new(),
                    it_value: TimeSpec::new(),
                }
            };
            inner.interval = new_value.it_interval;
            inner.expirations = 0;
            inner.realtime_abs_deadline = None;
            inner.deadline = if new_value.it_value.is_zero() {
                None
            } else if (flags & TFD_TIMER_ABSTIME) != 0 {
                if matches!(self.clock_id, CLOCK_REALTIME | CLOCK_REALTIME_ALARM) {
                    inner.realtime_abs_deadline = Some(new_value.it_value);
                    Some(timerfd_realtime_deadline_to_monotonic(
                        new_value.it_value,
                        now_clock.unwrap(),
                        now_monotonic.unwrap(),
                    ))
                } else {
                    Some(new_value.it_value)
                }
            } else {
                Some(now_monotonic.unwrap() + new_value.it_value)
            };
            if let (Some(deadline), Some(now)) = (inner.deadline, now_monotonic) {
                if now >= deadline {
                    Self::update_locked(&mut inner, now);
                    notify_readable = inner.expirations > 0;
                }
            }
            old_value
        };
        if notify_readable {
            self.notify_readable();
        }
        rearm_timerfd_sweep();
        Ok(old_value)
    }

    fn sync_realtime_deadline_after_clock_set(
        &self,
        now_realtime: TimeSpec,
        now_monotonic: TimeSpec,
    ) -> usize {
        if !matches!(self.clock_id, CLOCK_REALTIME | CLOCK_REALTIME_ALARM) {
            return 0;
        }
        let became_readable = {
            let mut inner = self.inner.lock();
            let Some(abs_deadline) = inner.realtime_abs_deadline else {
                return 0;
            };
            let was_empty = inner.expirations == 0;
            inner.deadline = Some(timerfd_realtime_deadline_to_monotonic(
                abs_deadline,
                now_realtime,
                now_monotonic,
            ));
            Self::update_locked(&mut inner, now_monotonic);
            was_empty && inner.expirations > 0
        };
        if became_readable {
            self.notify_readable()
        } else {
            0
        }
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
            Self::update_locked(&mut inner, TimeSpec::now());
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
        Self::update_locked(&mut inner, TimeSpec::now());
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

fn timerfd_realtime_deadline_to_monotonic(
    deadline: TimeSpec,
    now_realtime: TimeSpec,
    now_monotonic: TimeSpec,
) -> TimeSpec {
    if deadline >= now_realtime {
        now_monotonic + (deadline - now_realtime)
    } else {
        now_monotonic - (now_realtime - deadline)
    }
}

fn register_timerfd(timerfd: &Arc<TimerFd>) {
    TIMERFD_REGISTRY.lock().push(Arc::downgrade(timerfd));
    TIMERFD_REGISTRY_MAYBE_NONEMPTY.store(true, Ordering::Release);
}

pub fn timerfd_registry_maybe_nonempty() -> bool {
    TIMERFD_REGISTRY_MAYBE_NONEMPTY.load(Ordering::Acquire)
}

pub fn timerfd_registry_is_empty() -> bool {
    if !timerfd_registry_maybe_nonempty() {
        return true;
    }

    let registry = TIMERFD_REGISTRY.lock();
    if registry.is_empty() {
        TIMERFD_REGISTRY_MAYBE_NONEMPTY.store(false, Ordering::Release);
        return true;
    }
    false
}

pub fn timerfd_sweep_is_current(generation: usize) -> bool {
    TIMERFD_SWEEP_GENERATION.load(Ordering::Acquire) == generation
}

pub fn rearm_timerfd_sweep() {
    let earliest = {
        if !timerfd_registry_maybe_nonempty() {
            None
        } else {
            let mut registry = TIMERFD_REGISTRY.lock();
            registry.retain(|weak| weak.strong_count() > 0);
            if registry.is_empty() {
                TIMERFD_REGISTRY_MAYBE_NONEMPTY.store(false, Ordering::Release);
                None
            } else {
                let mut earliest: Option<TimeSpec> = None;
                for weak in registry.iter() {
                    let Some(timerfd) = weak.upgrade() else {
                        continue;
                    };
                    let Some(deadline) = timerfd.next_sweep_deadline() else {
                        continue;
                    };
                    if earliest.map(|old| deadline < old).unwrap_or(true) {
                        earliest = Some(deadline);
                    }
                }
                earliest
            }
        }
    };

    let generation = TIMERFD_SWEEP_GENERATION
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1);
    if let Some(deadline) = earliest {
        add_kernel_timer(TimerAction::TimerFdSweep { generation }, deadline);
    }
}

pub fn wake_expired_timerfds(now: TimeSpec) -> usize {
    if !timerfd_registry_maybe_nonempty() {
        return 0;
    }

    let mut registry = TIMERFD_REGISTRY.lock();
    let mut woke = 0usize;
    for weak in registry.iter() {
        if let Some(timerfd) = weak.upgrade() {
            woke = woke.saturating_add(timerfd.wake_if_expired(now));
        }
    }

    if TIMERFD_SWEEP_TICKS.fetch_add(1, Ordering::Relaxed) % 64 == 0 {
        registry.retain(|weak| weak.strong_count() > 0);
        if registry.is_empty() {
            TIMERFD_REGISTRY_MAYBE_NONEMPTY.store(false, Ordering::Release);
        }
    }
    woke
}

pub fn handle_realtime_clock_was_set() -> usize {
    if !timerfd_registry_maybe_nonempty() {
        return 0;
    }

    let now_realtime = current_timespec();
    let now_monotonic = TimeSpec::now();
    let woke = {
        let mut registry = TIMERFD_REGISTRY.lock();
        let mut woke = 0usize;
        registry.retain(|weak| weak.strong_count() > 0);
        if registry.is_empty() {
            TIMERFD_REGISTRY_MAYBE_NONEMPTY.store(false, Ordering::Release);
        } else {
            for weak in registry.iter() {
                if let Some(timerfd) = weak.upgrade() {
                    woke = woke.saturating_add(
                        timerfd.sync_realtime_deadline_after_clock_set(
                            now_realtime,
                            now_monotonic,
                        ),
                    );
                }
            }
        }
        woke
    };
    rearm_timerfd_sweep();
    woke
}

fn with_timerfd<R>(
    fd: usize,
    f: impl FnOnce(&TimerFd) -> Result<R, SyscallErr>,
) -> Result<R, SyscallErr> {
    let task = current_task().ok_or(SyscallErr::ESRCH)?;
    let files = task.process.files();
    let file = {
        let fd_table = files.lock();
        fd_table.get_file(fd)?
    };
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
