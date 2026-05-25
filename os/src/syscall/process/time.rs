use alloc::sync::Arc;

use crate::mm::{UserPtr, UserPtrMut};
use crate::syscall::errno::*;
use crate::task::{
    add_kernel_timer, current_task, current_user_token, find_process_by_pid, find_task_by_tid,
    signal::Signals, PosixTimer, sleep_relative_interruptible, Rusage, TimerAction,
};
use crate::timer::{
    current_timespec, current_timeval, get_time_ms, set_current_timespec, ITimerVal, TimeSpec,
    TimeVal, TimeZone, Times, NSEC_PER_SEC, USEC_PER_SEC,
};
use log::{info, trace};
use spin::Mutex;

const CLOCK_REALTIME: usize = 0;
const CLOCK_MONOTONIC: usize = 1;
const CLOCK_PROCESS_CPUTIME_ID: usize = 2;
const CLOCK_THREAD_CPUTIME_ID: usize = 3;
const CLOCK_MONOTONIC_RAW: usize = 4;
const CLOCK_REALTIME_COARSE: usize = 5;
const CLOCK_MONOTONIC_COARSE: usize = 6;
const CLOCK_BOOTTIME: usize = 7;
const CLOCK_REALTIME_ALARM: usize = 8;
const CLOCK_BOOTTIME_ALARM: usize = 9;
const CLOCK_TAI: usize = 11;
const CPUCLOCK_CLOCK_MASK: i32 = 3;
const CPUCLOCK_PERTHREAD_MASK: i32 = 4;
const CPUCLOCK_PID_MASK: i32 = 7;
const CPUCLOCK_PROF: i32 = 0;
const CPUCLOCK_VIRT: i32 = 1;
const CPUCLOCK_SCHED: i32 = 2;
const CPUCLOCK_MAX: i32 = 3;
const TIMER_ABSTIME: u32 = 1;
const SIGEV_SIGNAL: i32 = 0;
const SIGEV_NONE: i32 = 1;
const MAX_POSIX_TIMERS: usize = 32;
const USER_HZ: usize = 100;
const CAP_SYS_TIME: usize = 25;
const ADJ_OFFSET: u32 = 0x0001;
const ADJ_FREQUENCY: u32 = 0x0002;
const ADJ_MAXERROR: u32 = 0x0004;
const ADJ_ESTERROR: u32 = 0x0008;
const ADJ_STATUS: u32 = 0x0010;
const ADJ_TIMECONST: u32 = 0x0020;
const ADJ_TAI: u32 = 0x0080;
const ADJ_SETOFFSET: u32 = 0x0100;
const ADJ_MICRO: u32 = 0x1000;
const ADJ_NANO: u32 = 0x2000;
const ADJ_TICK: u32 = 0x4000;
const ADJ_OFFSET_SINGLESHOT: u32 = 0x8001;
const ADJ_OFFSET_SS_READ: u32 = 0xa001;
const MIN_TICK: i64 = 9_000;
const MAX_TICK: i64 = 11_000;
const ADJ_VALID_MASK: u32 = ADJ_OFFSET
    | ADJ_FREQUENCY
    | ADJ_MAXERROR
    | ADJ_ESTERROR
    | ADJ_STATUS
    | ADJ_TIMECONST
    | ADJ_TAI
    | ADJ_SETOFFSET
    | ADJ_MICRO
    | ADJ_NANO
    | ADJ_TICK;
const TIME_OK: isize = 0;
const TIME_INS: isize = 1;
const TIME_DEL: isize = 2;
const TIME_ERROR: isize = 5;
const STA_INS: i32 = 0x0010;
const STA_DEL: i32 = 0x0020;
const STA_UNSYNC: i32 = 0x0040;
const STA_CLOCKERR: i32 = 0x1000;
const STA_NANO: i32 = 0x2000;

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct Timex {
    modes: u32,
    offset: i64,
    freq: i64,
    maxerror: i64,
    esterror: i64,
    status: i32,
    constant: i64,
    precision: i64,
    tolerance: i64,
    time: TimeVal,
    tick: i64,
    ppsfreq: i64,
    jitter: i64,
    shift: i32,
    stabil: i64,
    jitcnt: i64,
    calcnt: i64,
    errcnt: i64,
    stbcnt: i64,
    tai: i32,
    __padding: [i32; 11],
}

#[derive(Clone, Copy, Debug)]
struct TimexState {
    offset: i64,
    freq: i64,
    maxerror: i64,
    esterror: i64,
    status: i32,
    constant: i64,
    tick: i64,
    tai: i32,
}

impl TimexState {
    const fn new() -> Self {
        Self {
            offset: 0,
            freq: 0,
            maxerror: 0,
            esterror: 0,
            status: 0,
            constant: 0,
            tick: 10_000,
            tai: 0,
        }
    }
}

static TIMEX_STATE: Mutex<TimexState> = Mutex::new(TimexState::new());

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ITimerSpec {
    pub it_interval: TimeSpec,
    pub it_value: TimeSpec,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct SigeventHeader {
    sigev_value: usize,
    sigev_signo: i32,
    sigev_notify: i32,
}

pub fn sys_nanosleep(req: *const TimeSpec, rem: *mut TimeSpec) -> isize {
    let token = current_user_token();
    let req = match UserPtr::new(req).read(token) {
        Ok(req) => req,
        Err(errno) => return errno,
    };
    if !is_valid_timespec(req) {
        return EINVAL;
    }

    match sleep_relative_interruptible(req) {
        Ok(()) => SUCCESS,
        Err(interrupted) => {
            if !rem.is_null() {
                if let Err(errno) = UserPtrMut::new(rem).write(token, &interrupted.remaining) {
                    return errno;
                }
            }
            EINTR
        }
    }
}

pub fn sys_setitimer(
    which: usize,
    new_value: *const ITimerVal,
    old_value: *mut ITimerVal,
) -> isize {
    info!(
        "[sys_setitimer] which: {}, new_value: {:?}, old_value: {:?}",
        which, new_value, old_value
    );
    if which > 2 {
        return EINVAL;
    }
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let new_timer = match UserPtr::new(new_value).read_optional(token) {
        Ok(value) => value,
        Err(e) => {
            return e;
        }
    };
    match which {
        //实时计时器走KernelTimer
        0 => {
            let now = TimeSpec::now();
            //待注册计时器
            let mut register_timer = None;
            {
                let mut inner = task.acquire_inner_lock();
                if old_value as usize != 0 {
                    inner.timer[0].it_value = match inner.real_timer_deadline {
                        Some(deadline) => timespec_to_timeval(deadline - now),
                        None => TimeVal::new(),
                    };
                    if let Err(e) = UserPtrMut::new(old_value).write(token, &inner.timer[0]) {
                        return e;
                    }
                    trace!("[sys_setitimer] *old_value: {:?}", inner.timer[0]);
                }
                if let Some(value) = new_timer {
                    //防止generation溢出
                    inner.real_timer_generation = inner.real_timer_generation.wrapping_add(1);
                    if value.it_value.is_zero() {
                        inner.timer[0] = ITimerVal::new();
                        inner.real_timer_deadline = None;
                    } else {
                        let deadline = now + timeval_to_timespec(value.it_value);
                        inner.timer[0] = value;
                        inner.real_timer_deadline = Some(deadline);
                        register_timer = Some((deadline, inner.real_timer_generation));
                    }
                    // 更新锚点，防止 refresh_real_timer() 用陈旧锚点误触发 SIGALRM
                    inner.clock.last_real_timer_update = TimeVal::now();
                }
            }
            if let Some((deadline, generation)) = register_timer {
                add_kernel_timer(
                    TimerAction::SendSignal {
                        //降为弱引用
                        task: Arc::downgrade(&task),
                        signal: Signals::SIGALRM,
                        generation,
                    },
                    deadline,
                );
            }
            SUCCESS
        }
        1 | 2 => {
            let mut inner = task.acquire_inner_lock();
            if old_value as usize != 0 {
                if let Err(e) = UserPtrMut::new(old_value).write(token, &inner.timer[which]) {
                    return e;
                }
                trace!("[sys_setitimer] *old_value: {:?}", inner.timer[which]);
            }
            if let Some(value) = new_timer {
                inner.timer[which] = value;
                trace!("[sys_setitimer] *new_value: {:?}", inner.timer[which]);
                inner.clock.last_real_timer_update = TimeVal::now();
            }
            SUCCESS
        }
        _ => EINVAL,
    }
}

pub fn sys_getitimer(which: usize, curr_value: *mut ITimerVal) -> isize {
    if which > 2 {
        return EINVAL;
    }
    if curr_value.is_null() {
        return EFAULT;
    }

    let task = current_task().unwrap();
    let token = task.get_user_token();
    let now = TimeSpec::now();
    let value = {
        let inner = task.acquire_inner_lock();
        let mut value = inner.timer[which];
        if which == 0 {
            value.it_value = match inner.real_timer_deadline {
                Some(deadline) => timespec_to_timeval(deadline - now),
                None => TimeVal::new(),
            };
        }
        value
    };

    match UserPtrMut::new(curr_value).write(token, &value) {
        Ok(()) => SUCCESS,
        Err(errno) => errno,
    }
}

pub fn sys_timer_create(
    clock_id: usize,
    sevp: *const SigeventHeader,
    timerid: *mut i32,
) -> isize {
    if timerid.is_null() {
        return EFAULT;
    }
    if !valid_posix_timer_clock(clock_id) {
        return EINVAL;
    }

    let token = current_user_token();
    let signal = if sevp.is_null() {
        Signals::SIGALRM
    } else {
        let event = match UserPtr::new(sevp).read(token) {
            Ok(event) => event,
            Err(errno) => return errno,
        };
        match event.sigev_notify {
            SIGEV_SIGNAL => match Signals::from_signum(event.sigev_signo as usize) {
                Ok(signal) => signal,
                Err(_) => return EINVAL,
            },
            SIGEV_NONE => Signals::empty(),
            _ => return EINVAL,
        }
    };

    let task = current_task().unwrap();
    let id = {
        let mut inner = task.acquire_inner_lock();
        if let Some((id, slot)) = inner
            .posix_timers
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| slot.is_none())
        {
            *slot = Some(PosixTimer::new(clock_id, signal));
            id
        } else {
            if inner.posix_timers.len() >= MAX_POSIX_TIMERS {
                return EAGAIN;
            }
            if inner.posix_timers.try_reserve(1).is_err() {
                return ENOMEM;
            }
            inner
                .posix_timers
                .push(Some(PosixTimer::new(clock_id, signal)));
            inner.posix_timers.len() - 1
        }
    };

    match UserPtrMut::new(timerid).write(token, &(id as i32)) {
        Ok(()) => SUCCESS,
        Err(errno) => {
            let mut inner = task.acquire_inner_lock();
            if let Some(slot) = inner.posix_timers.get_mut(id) {
                *slot = None;
            }
            errno
        }
    }
}

pub fn sys_timer_settime(
    timer_id: usize,
    flags: u32,
    new_value: *const ITimerSpec,
    old_value: *mut ITimerSpec,
) -> isize {
    if flags & !TIMER_ABSTIME != 0 {
        return EINVAL;
    }
    if new_value.is_null() {
        return EINVAL;
    }

    let token = current_user_token();
    let new_spec = match UserPtr::new(new_value).read(token) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    if !is_valid_timespec(new_spec.it_interval) || !is_valid_timespec(new_spec.it_value) {
        return EINVAL;
    }

    let task = current_task().unwrap();
    let mut register_timer = None;
    {
        let mut inner = task.acquire_inner_lock();
        let Some(Some(timer)) = inner.posix_timers.get_mut(timer_id) else {
            return EINVAL;
        };
        if !old_value.is_null() {
            let old_spec = current_posix_itimerspec(timer);
            if let Err(errno) = UserPtrMut::new(old_value).write(token, &old_spec) {
                return errno;
            }
        }

        timer.interval = new_spec.it_interval;
        timer.value = new_spec.it_value;
        timer.generation = timer.generation.wrapping_add(1);
        if timer.value.is_zero() {
            timer.deadline = None;
        } else {
            let deadline = posix_timer_deadline(timer.clock_id, flags, timer.value);
            timer.deadline = Some(deadline);
            register_timer = Some((deadline, timer.signal, timer.generation));
        }
    }

    if let Some((deadline, signal, generation)) = register_timer {
        add_kernel_timer(
            TimerAction::PosixTimerSignal {
                task: Arc::downgrade(&task),
                timer_id,
                signal,
                generation,
            },
            deadline,
        );
    }
    SUCCESS
}

pub fn sys_timer_gettime(timer_id: usize, curr_value: *mut ITimerSpec) -> isize {
    if curr_value.is_null() {
        return EFAULT;
    }
    let task = current_task().unwrap();
    let value = {
        let inner = task.acquire_inner_lock();
        let Some(Some(timer)) = inner.posix_timers.get(timer_id) else {
            return EINVAL;
        };
        current_posix_itimerspec(timer)
    };
    match UserPtrMut::new(curr_value).write(task.get_user_token(), &value) {
        Ok(()) => SUCCESS,
        Err(errno) => errno,
    }
}

pub fn sys_timer_getoverrun(timer_id: usize) -> isize {
    let task = current_task().unwrap();
    let inner = task.acquire_inner_lock();
    match inner.posix_timers.get(timer_id) {
        Some(Some(_)) => 0,
        _ => EINVAL,
    }
}

pub fn sys_timer_delete(timer_id: usize) -> isize {
    let task = current_task().unwrap();
    let mut inner = task.acquire_inner_lock();
    match inner.posix_timers.get_mut(timer_id) {
        Some(slot @ Some(_)) => {
            *slot = None;
            SUCCESS
        }
        _ => EINVAL,
    }
}

fn timeval_to_timespec(value: TimeVal) -> TimeSpec {
    TimeSpec::from_us(value.to_us())
}

fn timespec_to_timeval(value: TimeSpec) -> TimeVal {
    TimeVal::from_us(value.to_ns() / 1000)
}

fn valid_posix_timer_clock(clock_id: usize) -> bool {
    matches!(
        clock_id,
        CLOCK_REALTIME
            | CLOCK_MONOTONIC
            | CLOCK_PROCESS_CPUTIME_ID
            | CLOCK_THREAD_CPUTIME_ID
            | CLOCK_BOOTTIME
    )
}

fn posix_timer_clock_now(clock_id: usize) -> TimeSpec {
    match clock_id {
        CLOCK_REALTIME => current_timespec(),
        CLOCK_MONOTONIC | CLOCK_PROCESS_CPUTIME_ID | CLOCK_THREAD_CPUTIME_ID | CLOCK_BOOTTIME => {
            TimeSpec::now()
        }
        _ => TimeSpec::new(),
    }
}

fn posix_timer_deadline(clock_id: usize, flags: u32, value: TimeSpec) -> TimeSpec {
    let duration = if flags & TIMER_ABSTIME != 0 {
        timespec_saturating_sub(value, posix_timer_clock_now(clock_id))
    } else {
        value
    };
    TimeSpec::now() + duration
}

fn current_posix_itimerspec(timer: &PosixTimer) -> ITimerSpec {
    let value = match timer.deadline {
        Some(deadline) => timespec_saturating_sub(deadline, TimeSpec::now()),
        None => TimeSpec::new(),
    };
    ITimerSpec {
        it_interval: timer.interval,
        it_value: value,
    }
}

pub fn sys_gettimeofday(tv: *mut TimeVal, tz: *mut TimeZone) -> isize {
    let token = current_user_token();
    if !tv.is_null() {
        let timeval = current_timeval();
        if UserPtrMut::new(tv).write(token, &timeval).is_err() {
            log::error!("[sys_gettimeofday] Failed to copy to {:?}", tv);
            return EFAULT;
        }
    }
    if !tz.is_null() {
        let timezone = TimeZone {
            tz_minuteswest: 0,
            tz_dsttime: 0,
        };
        if UserPtrMut::new(tz).write(token, &timezone).is_err() {
            log::error!("[sys_gettimeofday] Failed to copy to {:?}", tz);
            return EFAULT;
        }
    }
    SUCCESS
}

fn is_valid_timeval(timeval: TimeVal) -> bool {
    timeval.tv_sec <= isize::MAX as usize && timeval.tv_usec < USEC_PER_SEC
}

pub fn sys_settimeofday(tv: *const TimeVal, tz: *const TimeZone) -> isize {
    let token = current_user_token();
    if !tz.is_null() {
        if let Err(errno) = UserPtr::new(tz).read(token) {
            return errno;
        }
    }
    if tv.is_null() {
        return SUCCESS;
    }
    let timeval = match UserPtr::new(tv).read(token) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    if !is_valid_timeval(timeval) {
        return EINVAL;
    }
    if !has_time_adjust_permission() {
        return EPERM;
    }
    set_current_timespec(timeval_to_timespec(timeval));
    SUCCESS
}

fn valid_timex_modes(modes: u32) -> bool {
    matches!(modes, ADJ_OFFSET_SINGLESHOT | ADJ_OFFSET_SS_READ)
        || (modes & !ADJ_VALID_MASK == 0)
}

fn valid_timex_value(timex: &Timex) -> bool {
    valid_timex_modes(timex.modes)
        && (timex.modes & ADJ_TICK == 0 || (MIN_TICK..=MAX_TICK).contains(&timex.tick))
}

fn has_time_adjust_permission() -> bool {
    let task = current_task().unwrap();
    let inner = task.acquire_inner_lock();
    (inner.cap_effective & (1u64 << CAP_SYS_TIME)) != 0
}

fn update_timex_state(state: &mut TimexState, timex: &Timex) {
    if timex.modes == ADJ_OFFSET_SINGLESHOT || timex.modes == ADJ_OFFSET_SS_READ {
        return;
    }
    if timex.modes & ADJ_OFFSET != 0 {
        state.offset = timex.offset;
    }
    if timex.modes & ADJ_FREQUENCY != 0 {
        state.freq = timex.freq;
    }
    if timex.modes & ADJ_MAXERROR != 0 {
        state.maxerror = timex.maxerror;
    }
    if timex.modes & ADJ_ESTERROR != 0 {
        state.esterror = timex.esterror;
    }
    if timex.modes & ADJ_STATUS != 0 {
        state.status = timex.status;
    }
    if timex.modes & ADJ_TIMECONST != 0 {
        state.constant = timex.constant;
    }
    if timex.modes & ADJ_TICK != 0 {
        state.tick = timex.tick;
    }
    if timex.modes & ADJ_TAI != 0 {
        state.tai = timex.tai;
    }
    if timex.modes & ADJ_NANO != 0 {
        state.status |= STA_NANO;
    }
    if timex.modes & ADJ_MICRO != 0 {
        state.status &= !STA_NANO;
    }
    if timex.modes & ADJ_SETOFFSET != 0 {
        let target = current_timespec() + TimeSpec::from_us(timex.time.to_us());
        set_current_timespec(target);
    }
}

fn timex_return_state(status: i32) -> isize {
    if status & (STA_UNSYNC | STA_CLOCKERR) != 0 {
        TIME_ERROR
    } else if status & STA_INS != 0 || status == TIME_INS as i32 {
        TIME_INS
    } else if status & STA_DEL != 0 || status == TIME_DEL as i32 {
        TIME_DEL
    } else {
        TIME_OK
    }
}

fn fill_timex_snapshot(timex: &mut Timex, state: TimexState) {
    timex.offset = state.offset;
    timex.freq = state.freq;
    timex.maxerror = state.maxerror;
    timex.esterror = state.esterror;
    timex.status = state.status;
    timex.constant = state.constant;
    timex.precision = 1;
    timex.tolerance = 32_768_000;
    timex.time = current_timeval();
    timex.tick = state.tick;
    timex.ppsfreq = 0;
    timex.jitter = 0;
    timex.shift = 0;
    timex.stabil = 0;
    timex.jitcnt = 0;
    timex.calcnt = 0;
    timex.errcnt = 0;
    timex.stbcnt = 0;
    timex.tai = state.tai;
}

fn do_adjtimex(timex_ptr: *mut Timex) -> isize {
    let token = current_user_token();
    let mut timex = match UserPtr::new(timex_ptr as *const Timex).read(token) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    if !valid_timex_value(&timex) {
        return EINVAL;
    }
    if timex.modes != 0 && !has_time_adjust_permission() {
        return EPERM;
    }
    let ret = {
        let mut state = TIMEX_STATE.lock();
        update_timex_state(&mut state, &timex);
        let snapshot = *state;
        fill_timex_snapshot(&mut timex, snapshot);
        timex_return_state(snapshot.status)
    };
    match UserPtrMut::new(timex_ptr).write(token, &timex) {
        Ok(()) => ret,
        Err(errno) => errno,
    }
}

pub fn sys_adjtimex(timex: *mut Timex) -> isize {
    do_adjtimex(timex)
}

pub fn sys_clock_adjtime(clk_id: usize, timex: *mut Timex) -> isize {
    match clk_id {
        CLOCK_REALTIME => do_adjtimex(timex),
        _ => EINVAL,
    }
}

pub fn sys_clock_settime(clk_id: usize, tp: *const TimeSpec) -> isize {
    if clk_id != CLOCK_REALTIME {
        return EINVAL;
    }
    let token = current_user_token();
    let timespec = match UserPtr::new(tp).read(token) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    if !is_valid_timespec(timespec) {
        return EINVAL;
    }
    if !has_time_adjust_permission() {
        return EPERM;
    }
    set_current_timespec(timespec);
    SUCCESS
}

pub fn sys_get_time() -> isize {
    get_time_ms() as isize
}

#[derive(Clone, Copy)]
struct CpuClockId {
    pid: usize,
    per_thread: bool,
    which: i32,
}

fn decode_cpu_clock_id(clk_id: usize) -> Result<CpuClockId, isize> {
    let clock = clk_id as i32;
    if clock >= 0 {
        return Err(EINVAL);
    }
    let which = clock & CPUCLOCK_CLOCK_MASK;
    if which >= CPUCLOCK_MAX || (clock & CPUCLOCK_PID_MASK) == CPUCLOCK_PID_MASK {
        return Err(EINVAL);
    }
    let pid = !(clock >> 3);
    if pid < 0 {
        return Err(EINVAL);
    }
    Ok(CpuClockId {
        pid: pid as usize,
        per_thread: clock & CPUCLOCK_PERTHREAD_MASK != 0,
        which,
    })
}

fn scale_sched_cpu_clock_us(us: usize, nice: i32) -> usize {
    let nice = nice.clamp(-20, 19);
    if nice < 0 {
        us.saturating_mul((20 - nice) as usize) / 20
    } else if nice > 0 {
        us.saturating_mul(20) / (20 + nice as usize)
    } else {
        us
    }
}

fn cpu_clock_from_rusage(rusage: Rusage, which: i32, nice: i32) -> TimeSpec {
    let utime = rusage.ru_utime.to_us();
    let stime = rusage.ru_stime.to_us();
    let us = match which {
        CPUCLOCK_PROF => utime.saturating_add(stime),
        CPUCLOCK_SCHED => scale_sched_cpu_clock_us(utime.saturating_add(stime), nice),
        CPUCLOCK_VIRT => utime,
        _ => 0,
    };
    TimeSpec::from_us(us)
}

fn validate_cpu_clock_id(clk_id: usize) -> Result<CpuClockId, isize> {
    let clock = decode_cpu_clock_id(clk_id)?;
    let current = current_task().unwrap();
    if clock.pid == 0 {
        return Ok(clock);
    }
    let exists = if clock.per_thread {
        find_task_by_tid(clock.pid).is_some()
    } else {
        find_process_by_pid(clock.pid).is_some()
            || (clock.pid == current.gettid() && find_process_by_pid(current.pid()).is_some())
    };
    if exists {
        Ok(clock)
    } else {
        Err(EINVAL)
    }
}

fn cpu_clock_timespec(clk_id: usize) -> Result<TimeSpec, isize> {
    let clock = validate_cpu_clock_id(clk_id)?;
    let current = current_task().unwrap();
    if clock.per_thread {
        let task = if clock.pid == 0 {
            current
        } else {
            find_task_by_tid(clock.pid).ok_or(EINVAL)?
        };
        let inner = task.acquire_inner_lock();
        return Ok(cpu_clock_from_rusage(
            inner.rusage,
            clock.which,
            inner.sched_nice,
        ));
    }

    let process = if clock.pid == 0 || clock.pid == current.gettid() {
        current.process.clone()
    } else {
        find_process_by_pid(clock.pid).ok_or(EINVAL)?
    };
    let mut cpu_us = 0usize;
    let mut saw_thread = false;
    for weak in process.threads.lock().iter() {
        if let Some(task) = weak.upgrade() {
            let inner = task.acquire_inner_lock();
            let rusage = inner.rusage;
            let task_utime = rusage.ru_utime.to_us();
            let task_stime = rusage.ru_stime.to_us();
            let task_us = match clock.which {
                CPUCLOCK_PROF => task_utime.saturating_add(task_stime),
                CPUCLOCK_SCHED => scale_sched_cpu_clock_us(
                    task_utime.saturating_add(task_stime),
                    inner.sched_nice,
                ),
                CPUCLOCK_VIRT => task_utime,
                _ => return Err(EINVAL),
            };
            cpu_us = cpu_us.saturating_add(task_us);
            saw_thread = true;
        }
    }
    if !saw_thread {
        return Err(EINVAL);
    }
    Ok(TimeSpec::from_us(cpu_us))
}

pub fn sys_clock_gettime(clk_id: usize, tp: *mut TimeSpec) -> isize {
    let timespec = match clk_id {
        CLOCK_REALTIME | CLOCK_REALTIME_COARSE | CLOCK_REALTIME_ALARM | CLOCK_TAI => {
            current_timespec()
        }
        CLOCK_MONOTONIC
        | CLOCK_PROCESS_CPUTIME_ID
        | CLOCK_THREAD_CPUTIME_ID
        | CLOCK_MONOTONIC_RAW
        | CLOCK_MONOTONIC_COARSE
        | CLOCK_BOOTTIME
        | CLOCK_BOOTTIME_ALARM => TimeSpec::now(),
        _ => match cpu_clock_timespec(clk_id) {
            Ok(timespec) => timespec,
            Err(errno) => return errno,
        },
    };
    if !tp.is_null() {
        let token = current_user_token();
        if UserPtrMut::new(tp).write(token, &timespec).is_err() {
            log::error!("[sys_clock_gettime] Failed to copy to {:?}", tp);
            return EFAULT;
        };
        log::trace!("[sys_clock_gettime] clk_id: {}, tp: {:?}", clk_id, timespec);
    }
    SUCCESS
}

pub fn sys_clock_getres(clk_id: usize, tp: *mut TimeSpec) -> isize {
    match clk_id {
        CLOCK_REALTIME
        | CLOCK_MONOTONIC
        | CLOCK_PROCESS_CPUTIME_ID
        | CLOCK_THREAD_CPUTIME_ID
        | CLOCK_MONOTONIC_RAW
        | CLOCK_REALTIME_COARSE
        | CLOCK_MONOTONIC_COARSE
        | CLOCK_BOOTTIME
        | CLOCK_TAI => {}
        _ => {
            if let Err(errno) = validate_cpu_clock_id(clk_id) {
                return errno;
            }
        }
    }
    if !tp.is_null() {
        let resolution = TimeSpec {
            tv_sec: 0,
            tv_nsec: 1,
        };
        if UserPtrMut::new(tp)
            .write(current_user_token(), &resolution)
            .is_err()
        {
            return EFAULT;
        }
    }
    SUCCESS
}

pub fn sys_clock_nanosleep(
    clk_id: usize,
    flags: u32,
    rqtp: *const TimeSpec,
    rmtp: *mut TimeSpec,
) -> isize {
    if flags & !TIMER_ABSTIME != 0 {
        return EINVAL;
    }
    if let Err(errno) = check_sleep_clock(clk_id) {
        return errno;
    }

    let token = current_user_token();
    let req = match UserPtr::new(rqtp).read(token) {
        Ok(req) => req,
        Err(errno) => return errno,
    };
    if !is_valid_timespec(req) {
        return EINVAL;
    }

    info!(
        "[sys_clock_nanosleep] clk_id: {}, flags: {:?}, rqtp: {:?}, rmtp: {:?}",
        clk_id, flags, req, rmtp
    );

    if flags & TIMER_ABSTIME != 0 {
        // 绝对睡眠的时间点属于传入的 clock，等待队列内部只使用单调时间。
        let duration = timespec_saturating_sub(req, sleep_clock_now(clk_id));
        match sleep_relative_interruptible(duration) {
            Ok(()) => SUCCESS,
            Err(_) => EINTR,
        }
    } else {
        match sleep_relative_interruptible(req) {
            Ok(()) => SUCCESS,
            Err(interrupted) => {
                if !rmtp.is_null() {
                    if let Err(errno) =
                        UserPtrMut::new(rmtp).write(token, &interrupted.remaining)
                    {
                        return errno;
                    }
                }
                EINTR
            }
        }
    }
}

fn is_valid_timespec(timespec: TimeSpec) -> bool {
    timespec.tv_sec <= isize::MAX as usize && timespec.tv_nsec < NSEC_PER_SEC
}

fn sleep_clock_now(clk_id: usize) -> TimeSpec {
    match clk_id {
        CLOCK_REALTIME => current_timespec(),
        CLOCK_MONOTONIC | CLOCK_BOOTTIME => TimeSpec::now(),
        _ => TimeSpec::new(),
    }
}

fn timespec_saturating_sub(lhs: TimeSpec, rhs: TimeSpec) -> TimeSpec {
    if lhs <= rhs {
        return TimeSpec::new();
    }
    let mut tv_sec = lhs.tv_sec - rhs.tv_sec;
    let tv_nsec = if lhs.tv_nsec >= rhs.tv_nsec {
        lhs.tv_nsec - rhs.tv_nsec
    } else {
        tv_sec -= 1;
        lhs.tv_nsec + NSEC_PER_SEC - rhs.tv_nsec
    };
    TimeSpec { tv_sec, tv_nsec }
}

fn check_sleep_clock(clk_id: usize) -> Result<(), isize> {
    match clk_id {
        CLOCK_REALTIME | CLOCK_MONOTONIC | CLOCK_BOOTTIME => Ok(()),
        CLOCK_PROCESS_CPUTIME_ID
        | CLOCK_THREAD_CPUTIME_ID
        | CLOCK_MONOTONIC_RAW
        | CLOCK_REALTIME_COARSE
        | CLOCK_MONOTONIC_COARSE
        | CLOCK_REALTIME_ALARM
        | CLOCK_BOOTTIME_ALARM
        | CLOCK_TAI => Err(EOPNOTSUPP),
        _ => Err(EINVAL),
    }
}

pub fn sys_times(buf: *mut Times) -> isize {
    let task = current_task().unwrap();
    let (utime, stime) = {
        let inner = task.acquire_inner_lock();
        (
            timeval_to_user_ticks(inner.rusage.ru_utime),
            timeval_to_user_ticks(inner.rusage.ru_stime),
        )
    };
    let child_rusage = task.process.child_rusage();
    let token = task.get_user_token();
    let times = Times {
        tms_utime: utime,
        tms_stime: stime,
        tms_cutime: timeval_to_user_ticks(child_rusage.ru_utime),
        tms_cstime: timeval_to_user_ticks(child_rusage.ru_stime),
    };
    if UserPtrMut::new(buf).write(token, &times).is_err() {
        log::error!("[sys_times] Failed to copy to {:?}", buf);
        return EFAULT;
    };
    // return clock ticks that have elapsed since an arbitrary point in the past
    timeval_to_user_ticks(TimeVal::now()) as isize
}

pub fn sys_getrusage(who: isize, usage: *mut Rusage) -> isize {
    const RUSAGE_CHILDREN: isize = -1;
    const RUSAGE_SELF: isize = 0;
    const RUSAGE_THREAD: isize = 1;

    let task = current_task().unwrap();
    let token = task.get_user_token();
    let rusage = match who {
        RUSAGE_SELF | RUSAGE_THREAD => task.acquire_inner_lock().rusage,
        RUSAGE_CHILDREN => task.process.child_rusage(),
        _ => return EINVAL,
    };
    if UserPtrMut::new(usage).write(token, &rusage).is_err() {
        log::error!("[sys_getrusage] Failed to copy to {:?}", usage);
        return EFAULT;
    };
    //info!("[sys_getrusage] who: {:?}, usage: {:?}", who, rusage);
    SUCCESS
}

fn timeval_to_user_ticks(value: TimeVal) -> usize {
    value.to_us().saturating_mul(USER_HZ) / USEC_PER_SEC
}
