use alloc::sync::Arc;

use crate::mm::{UserPtr, UserPtrMut};
use crate::syscall::errno::*;
use crate::task::{
    add_kernel_timer, all_processes, current_task, current_user_token, find_process_by_pid,
    find_task_by_tid, signal::Signals, sleep_relative_interruptible,
    sleep_until_realtime_interruptible, wake_realtime_abstime_sleepers_after_clock_set,
    IntervalTimerKind, PosixTimer, Rusage, TimerAction,
};
use crate::timer::{
    current_timespec, current_timeval, get_time_ms, set_current_timespec, ITimerVal, TimeSpec,
    TimeVal, TimeZone, Times, NSEC_PER_SEC, USEC_PER_SEC,
};
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
    let Some(kind) = IntervalTimerKind::from_which(which) else {
        return EINVAL;
    };
    let task = current_task().unwrap();
    let token = current_user_token();
    // Linux 保留了 `new_value == NULL` 即停表的历史行为；不能把 optional
    // copyin 的 None 误解释为“不修改当前 timer”。
    let new_timer = if new_value.is_null() {
        ITimerVal::new()
    } else {
        match UserPtr::new(new_value).read(token) {
            Ok(value) => value,
            Err(errno) => return errno,
        }
    };
    if !valid_itimerval(new_timer) {
        return EINVAL;
    }

    // 当前线程在 trap 入口结算的用户时间可能仍停留在 TCB；先冲刷再读取
    // VIRTUAL/PROF 时钟，避免新 deadline 少算本次已消耗的 CPU 时间。
    task.flush_cpu_time();
    let (old_timer, register_real) = task.process.set_interval_timer(kind, new_timer);
    if let Some((deadline, generation)) = register_real {
        add_kernel_timer(
            TimerAction::IntervalTimerSignal {
                process: Arc::downgrade(&task.process),
                generation,
            },
            deadline,
        );
    }
    // Linux 先提交新 timer，再向用户写旧值；copyout EFAULT 不回滚已发布状态。
    if !old_value.is_null() {
        if let Err(errno) = UserPtrMut::new(old_value).write(token, &old_timer) {
            return errno;
        }
    }
    SUCCESS
}

pub fn sys_getitimer(which: usize, curr_value: *mut ITimerVal) -> isize {
    let Some(kind) = IntervalTimerKind::from_which(which) else {
        return EINVAL;
    };
    if curr_value.is_null() {
        return EFAULT;
    }

    let task = current_task().unwrap();
    let token = current_user_token();
    task.flush_cpu_time();
    let value = task.process.interval_timer(kind);

    match UserPtrMut::new(curr_value).write(token, &value) {
        Ok(()) => SUCCESS,
        Err(errno) => errno,
    }
}

fn valid_itimerval(value: ITimerVal) -> bool {
    fn valid_timeval(value: TimeVal) -> bool {
        value.tv_usec < USEC_PER_SEC && value.tv_sec <= (usize::MAX - value.tv_usec) / USEC_PER_SEC
    }

    valid_timeval(value.it_interval) && valid_timeval(value.it_value)
}

pub fn sys_timer_create(clock_id: usize, sevp: *const SigeventHeader, timerid: *mut i32) -> isize {
    if timerid.is_null() {
        return EFAULT;
    }
    if !valid_posix_timer_clock(clock_id) {
        return EINVAL;
    }

    let token = current_user_token();
    let (signal, signal_value) = if sevp.is_null() {
        (Signals::SIGALRM, None)
    } else {
        let event = match UserPtr::new(sevp).read(token) {
            Ok(event) => event,
            Err(errno) => return errno,
        };
        let signal = match event.sigev_notify {
            SIGEV_SIGNAL => match Signals::from_signum(event.sigev_signo as usize) {
                Ok(signal) => signal,
                Err(_) => return EINVAL,
            },
            SIGEV_NONE => Signals::empty(),
            _ => return EINVAL,
        };
        (signal, Some(event.sigev_value))
    };

    let task = current_task().unwrap();
    let process = task.process.clone();
    let id = match process.posix_timers().reserve_id() {
        Ok(id) => id,
        Err(error) => return -(error as isize),
    };

    if let Err(errno) = UserPtrMut::new(timerid).write(token, &(id as i32)) {
        process.posix_timers().cancel_reservation(id);
        return errno;
    }

    let signal_value = signal_value.unwrap_or(id);
    let timer = match clock_id {
        CLOCK_PROCESS_CPUTIME_ID => PosixTimer::new_process_cpu(clock_id, signal, signal_value),
        CLOCK_THREAD_CPUTIME_ID => {
            PosixTimer::new_thread_cpu(clock_id, signal, signal_value, &task)
        }
        _ => PosixTimer::new_wall(clock_id, signal, signal_value),
    };
    if !process.posix_timers().publish_reserved(id, timer) {
        // exec/group-exit 会先停止本线程，正常路径不会撤销仍在执行 syscall 的预留。
        process.posix_timers().cancel_reservation(id);
        return EAGAIN;
    }
    SUCCESS
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
    // 当前线程在本次 syscall 之前消耗的 CPU 时间可能还留在 TCB 批次中。
    // 先冲刷再装载 process CPU timer，避免相对 deadline 从过旧基准起算。
    task.flush_cpu_time();
    let process = task.process.clone();
    let mut register_timer = None;
    let mut old_spec = None;
    let mut generated_event = None;
    {
        let mut timers = process.posix_timers();
        let Some(mut timer) = timers.get(timer_id).cloned() else {
            return EINVAL;
        };
        // CPU timer 的 owner 线程退出后，对象仍留在进程表中供 delete/gettime
        // 使用，但不能重新装载到已经消失的 CPU clock 上。
        let cpu_now_us = if timer.is_cpu_clock() {
            match timer.cpu_time_us(&process) {
                Some(now) => Some(now),
                None => return ESRCH,
            }
        } else {
            None
        };
        if !old_value.is_null() {
            old_spec = Some(current_posix_itimerspec(&timer, &process));
        }

        timer.interval = new_spec.it_interval;
        timer.value = new_spec.it_value;
        let arm_seq = timers.alloc_arm_seq();
        timer.begin_arm(arm_seq);
        timer.wall_deadline = None;
        timer.realtime_abs_deadline = None;
        timer.cpu_deadline_us = None;
        if timer.value.is_zero() {
            // deadline 已同时在 wall/CPU 两个时钟域中清除。
        } else if let Some(now_us) = cpu_now_us {
            let value_us = PosixTimer::cpu_duration_us(timer.value);
            let deadline_us = if flags & TIMER_ABSTIME != 0 {
                value_us
            } else {
                now_us.saturating_add(value_us)
            };
            if flags & TIMER_ABSTIME != 0 && deadline_us <= now_us {
                if timer.interval.is_zero() {
                    timer.value = TimeSpec::new();
                    generated_event = timer.record_expiry(timer_id, 1);
                } else {
                    let interval_us = PosixTimer::cpu_duration_us(timer.interval);
                    let missed = now_us.saturating_sub(deadline_us) / interval_us;
                    let expirations = missed.saturating_add(1);
                    timer.value = timer.interval;
                    timer.cpu_deadline_us =
                        Some(deadline_us.saturating_add(expirations.saturating_mul(interval_us)));
                    generated_event =
                        timer.record_expiry(timer_id, expirations.min(usize::MAX as u64) as usize);
                }
            } else {
                timer.cpu_deadline_us = Some(deadline_us);
            }
        } else {
            let clock_now = posix_wall_clock_now(timer.clock_id);
            let now_monotonic = TimeSpec::now();
            if flags & TIMER_ABSTIME != 0 && timer.value <= clock_now {
                if timer.interval.is_zero() {
                    timer.value = TimeSpec::new();
                    generated_event = timer.record_expiry(timer_id, 1);
                } else {
                    let (deadline, overrun, next_abs_deadline) =
                        posix_timer_deadline_after_absolute_overrun(
                            timer.value,
                            timer.interval,
                            clock_now,
                            now_monotonic,
                        );
                    if is_realtime_posix_timer_clock(timer.clock_id) {
                        timer.realtime_abs_deadline = Some(next_abs_deadline);
                    }
                    timer.wall_deadline = Some(deadline);
                    register_timer = Some((deadline, timer.arm_seq));
                    generated_event = timer.record_expiry(timer_id, overrun.saturating_add(1));
                }
            } else {
                let deadline = posix_timer_deadline(
                    timer.clock_id,
                    flags,
                    timer.value,
                    clock_now,
                    now_monotonic,
                );
                if flags & TIMER_ABSTIME != 0 && is_realtime_posix_timer_clock(timer.clock_id) {
                    timer.realtime_abs_deadline = Some(timer.value);
                }
                timer.wall_deadline = Some(deadline);
                register_timer = Some((deadline, timer.arm_seq));
            }
        }
        *timers.get_mut(timer_id).unwrap() = timer;
        process.sync_posix_cpu_timer_hint(&timers);
    }

    // SignalQueue 可能扩容，唤醒还会进入调度器；二者都必须位于 timer 表锁外。
    if let Some(event) = generated_event {
        let _ = process.publish_posix_timer_signal(event);
    }
    if let Some((deadline, arm_seq)) = register_timer {
        add_kernel_timer(
            TimerAction::PosixTimerSignal {
                process: Arc::downgrade(&process),
                timer_id,
                arm_seq,
            },
            deadline,
        );
    }
    // 内核状态和 timer queue 已更新后才写回旧值；copyout 失败不撤销新配置。
    if let Some(old_spec) = old_spec {
        if let Err(errno) = UserPtrMut::new(old_value).write(token, &old_spec) {
            return errno;
        }
    }
    SUCCESS
}

pub fn sys_timer_gettime(timer_id: usize, curr_value: *mut ITimerSpec) -> isize {
    if curr_value.is_null() {
        return EFAULT;
    }
    let task = current_task().unwrap();
    task.flush_cpu_time();
    let process = task.process.clone();
    let value = {
        let timers = process.posix_timers();
        let Some(timer) = timers.get(timer_id) else {
            return EINVAL;
        };
        current_posix_itimerspec(timer, &process)
    };
    match UserPtrMut::new(curr_value).write(current_user_token(), &value) {
        Ok(()) => SUCCESS,
        Err(errno) => errno,
    }
}

pub fn sys_timer_getoverrun(timer_id: usize) -> isize {
    let process = current_task().unwrap().process.clone();
    let timers = process.posix_timers();
    match timers.get(timer_id) {
        Some(timer) => timer.last_overrun() as isize,
        _ => EINVAL,
    }
}

pub fn sys_timer_delete(timer_id: usize) -> isize {
    let process = current_task().unwrap().process.clone();
    let removed = {
        let mut timers = process.posix_timers();
        let pending_event = timers.get(timer_id).and_then(PosixTimer::pending_event);
        let removed = timers.remove(timer_id);
        process.sync_posix_cpu_timer_hint(&timers);
        removed.then_some(pending_event)
    };
    let Some(pending_event) = removed else {
        return EINVAL;
    };
    if let Some(event_id) = pending_event {
        process.remove_queued_posix_timer_signal(event_id);
    }
    SUCCESS
}

fn timeval_to_timespec(value: TimeVal) -> TimeSpec {
    TimeSpec::from_us(value.to_us())
}

fn timespec_to_timeval(value: TimeSpec) -> TimeVal {
    TimeVal::from_us((value.to_ns_saturating() / 1000) as usize)
}

fn valid_posix_timer_clock(clock_id: usize) -> bool {
    matches!(
        clock_id,
        CLOCK_REALTIME
            | CLOCK_REALTIME_ALARM
            | CLOCK_TAI
            | CLOCK_MONOTONIC
            | CLOCK_PROCESS_CPUTIME_ID
            | CLOCK_THREAD_CPUTIME_ID
            | CLOCK_BOOTTIME
            | CLOCK_BOOTTIME_ALARM
    )
}

fn posix_wall_clock_now(clock_id: usize) -> TimeSpec {
    match clock_id {
        CLOCK_REALTIME | CLOCK_REALTIME_ALARM | CLOCK_TAI => current_timespec(),
        CLOCK_MONOTONIC | CLOCK_BOOTTIME | CLOCK_BOOTTIME_ALARM => TimeSpec::now(),
        _ => TimeSpec::new(),
    }
}

fn is_realtime_posix_timer_clock(clock_id: usize) -> bool {
    matches!(clock_id, CLOCK_REALTIME | CLOCK_REALTIME_ALARM | CLOCK_TAI)
}

fn realtime_deadline_to_monotonic(
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

fn posix_timer_deadline(
    clock_id: usize,
    flags: u32,
    value: TimeSpec,
    clock_now: TimeSpec,
    now_monotonic: TimeSpec,
) -> TimeSpec {
    if flags & TIMER_ABSTIME != 0 && is_realtime_posix_timer_clock(clock_id) {
        return realtime_deadline_to_monotonic(value, clock_now, now_monotonic);
    }
    let duration = if flags & TIMER_ABSTIME != 0 {
        timespec_saturating_sub(value, clock_now)
    } else {
        value
    };
    now_monotonic + duration
}

fn posix_timer_deadline_after_absolute_overrun(
    value: TimeSpec,
    interval: TimeSpec,
    clock_now: TimeSpec,
    now_monotonic: TimeSpec,
) -> (TimeSpec, usize, TimeSpec) {
    let interval_ns = interval.to_ns_saturating().max(1) as usize;
    let elapsed_ns =
        (clock_now.to_ns_saturating() as usize).saturating_sub(value.to_ns_saturating() as usize);
    let expirations = 1usize.saturating_add(elapsed_ns / interval_ns);
    let next_clock_ns =
        (value.to_ns_saturating() as usize).saturating_add(expirations.saturating_mul(interval_ns));
    let next_abs_deadline = TimeSpec::from_ns(next_clock_ns);
    let duration = timespec_saturating_sub(next_abs_deadline, clock_now);
    let deadline = now_monotonic + duration;
    (deadline, expirations.saturating_sub(1), next_abs_deadline)
}

fn current_posix_itimerspec(
    timer: &PosixTimer,
    process: &crate::task::ProcessControlBlock,
) -> ITimerSpec {
    let value = if let Some(deadline_us) = timer.cpu_deadline_us {
        match timer.cpu_time_us(process) {
            Some(now_us) if now_us < deadline_us => {
                TimeSpec::from_us((deadline_us - now_us).min(usize::MAX as u64) as usize)
            }
            // Linux 对已经到期、但尚未被安全点领取的 CPU timer 返回 1ns，
            // 避免把“仍处于 armed 状态”误报成 disarmed。
            Some(_) => TimeSpec {
                tv_sec: 0,
                tv_nsec: 1,
            },
            // thread owner 已退出后，该 CPU clock 不再推进。
            None => TimeSpec::new(),
        }
    } else {
        match timer.wall_deadline {
            Some(deadline) => timespec_saturating_sub(deadline, TimeSpec::now()),
            None => TimeSpec::new(),
        }
    };
    ITimerSpec {
        it_interval: timer.interval,
        it_value: value,
    }
}

fn rearm_posix_realtime_timers_after_clock_set() -> usize {
    let now_realtime = current_timespec();
    let now_monotonic = TimeSpec::now();
    let mut registration_count = 0usize;

    for process in all_processes() {
        // clock_settime 可能同时使多个绝对 timer 到期。固定批次保证 timer
        // owner 锁内只更新状态，不进入 allocator、signal lock 或 timer heap。
        let mut events = [None; PosixTimer::MAX_COUNT];
        let mut event_count = 0usize;
        let mut registrations = [None; PosixTimer::MAX_COUNT];
        let mut process_registration_count = 0usize;
        {
            let mut timers = process.posix_timers();
            for timer_id in 0..timers.slot_count() {
                let Some(mut timer) = timers.get(timer_id).cloned() else {
                    continue;
                };
                if !is_realtime_posix_timer_clock(timer.clock_id) {
                    continue;
                }
                let Some(abs_deadline) = timer.realtime_abs_deadline else {
                    continue;
                };
                if timer.wall_deadline.is_none() {
                    timer.realtime_abs_deadline = None;
                    *timers.get_mut(timer_id).unwrap() = timer;
                    continue;
                }

                timer.arm_seq = timers.alloc_arm_seq();
                let mut generated = false;
                if abs_deadline <= now_realtime {
                    generated = true;
                    if timer.interval.is_zero() {
                        timer.value = TimeSpec::new();
                        timer.wall_deadline = None;
                        timer.realtime_abs_deadline = None;
                    } else {
                        let (deadline, overrun, next_abs_deadline) =
                            posix_timer_deadline_after_absolute_overrun(
                                abs_deadline,
                                timer.interval,
                                now_realtime,
                                now_monotonic,
                            );
                        timer.value = timer.interval;
                        timer.wall_deadline = Some(deadline);
                        timer.realtime_abs_deadline = Some(next_abs_deadline);
                        registrations[process_registration_count] =
                            Some((timer_id, timer.arm_seq, deadline));
                        process_registration_count += 1;
                        if let Some(event) =
                            timer.record_expiry(timer_id, overrun.saturating_add(1))
                        {
                            events[event_count] = Some(event);
                            event_count += 1;
                        }
                    }
                } else {
                    let deadline =
                        realtime_deadline_to_monotonic(abs_deadline, now_realtime, now_monotonic);
                    timer.wall_deadline = Some(deadline);
                    registrations[process_registration_count] =
                        Some((timer_id, timer.arm_seq, deadline));
                    process_registration_count += 1;
                }

                if generated && timer.interval.is_zero() {
                    if let Some(event) = timer.record_expiry(timer_id, 1) {
                        events[event_count] = Some(event);
                        event_count += 1;
                    }
                }
                *timers.get_mut(timer_id).unwrap() = timer;
            }
        }
        for event in events.iter().take(event_count).flatten().copied() {
            let _ = process.publish_posix_timer_signal(event);
        }
        for (timer_id, arm_seq, deadline) in registrations
            .iter()
            .take(process_registration_count)
            .flatten()
            .copied()
        {
            add_kernel_timer(
                TimerAction::PosixTimerSignal {
                    process: Arc::downgrade(&process),
                    timer_id,
                    arm_seq,
                },
                deadline,
            );
            registration_count += 1;
        }
    }
    registration_count
}

pub fn sys_gettimeofday(tv: *mut TimeVal, tz: *mut TimeZone) -> isize {
    if tv.is_null() && tz.is_null() {
        return SUCCESS;
    }
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
    if tv.is_null() && tz.is_null() {
        return SUCCESS;
    }
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
    handle_realtime_clock_was_set();
    SUCCESS
}

fn valid_timex_modes(modes: u32) -> bool {
    matches!(modes, ADJ_OFFSET_SINGLESHOT | ADJ_OFFSET_SS_READ) || (modes & !ADJ_VALID_MASK == 0)
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

fn update_timex_state(state: &mut TimexState, timex: &Timex) -> bool {
    let mut realtime_changed = false;
    if timex.modes == ADJ_OFFSET_SINGLESHOT || timex.modes == ADJ_OFFSET_SS_READ {
        return realtime_changed;
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
        realtime_changed = true;
    }
    realtime_changed
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
    let (ret, realtime_changed) = {
        let mut state = TIMEX_STATE.lock();
        let realtime_changed = update_timex_state(&mut state, &timex);
        let snapshot = *state;
        fill_timex_snapshot(&mut timex, snapshot);
        (timex_return_state(snapshot.status), realtime_changed)
    };
    if realtime_changed {
        handle_realtime_clock_was_set();
    }
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
    handle_realtime_clock_was_set();
    SUCCESS
}

fn handle_realtime_clock_was_set() {
    let _ = crate::fs::timerfd::handle_realtime_clock_was_set();
    let _ = rearm_posix_realtime_timers_after_clock_set();
    let _ = wake_realtime_abstime_sleepers_after_clock_set();
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

fn cpu_clock_from_rusage(rusage: Rusage, which: i32) -> TimeSpec {
    let utime = rusage.ru_utime.to_us();
    let stime = rusage.ru_stime.to_us();
    let us = match which {
        CPUCLOCK_PROF => utime.saturating_add(stime),
        // MangoCore 尚未维护独立 sum_exec_runtime；当前可用的等价近似是未按
        // nice 加权的真实 user + system 时间，不能把 vruntime 暴露为 CPU clock。
        CPUCLOCK_SCHED => utime.saturating_add(stime),
        CPUCLOCK_VIRT => utime,
        _ => 0,
    };
    TimeSpec::from_us(us)
}

fn validate_cpu_clock_id(clk_id: usize) -> Result<CpuClockId, isize> {
    let clock = decode_cpu_clock_id(clk_id)?;
    if clock.pid == 0 {
        return Ok(clock);
    }
    let exists = if clock.per_thread {
        find_task_by_tid(clock.pid).is_some()
    } else {
        let current = current_task().unwrap();
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
    if clock.per_thread {
        if clock.pid == 0 {
            let task = current_task().unwrap();
            let inner = task.acquire_inner_lock();
            return Ok(cpu_clock_from_rusage(inner.rusage, clock.which));
        } else {
            let task = find_task_by_tid(clock.pid).ok_or(EINVAL)?;
            let inner = task.acquire_inner_lock();
            return Ok(cpu_clock_from_rusage(inner.rusage, clock.which));
        }
    }

    let process = if clock.pid == 0 {
        let current = current_task().unwrap();
        current.flush_cpu_time();
        current.process.clone()
    } else {
        let current = current_task().unwrap();
        if clock.pid == current.gettid() {
            current.flush_cpu_time();
            current.process.clone()
        } else {
            find_process_by_pid(clock.pid).ok_or(EINVAL)?
        }
    };
    if process.live_thread_count() == 0 {
        return Err(EINVAL);
    }
    // PCB 账户同时保留已退出 sibling 的时间，避免扫描 live TCB 时在退出窗口
    // 丢失累计；查询也不再依次获取多个 task.inner。
    Ok(cpu_clock_from_rusage(process.cpu_rusage(), clock.which))
}

pub fn sys_clock_gettime(clk_id: usize, tp: *mut TimeSpec) -> isize {
    let timespec = match clk_id {
        CLOCK_REALTIME | CLOCK_REALTIME_COARSE | CLOCK_REALTIME_ALARM | CLOCK_TAI => {
            current_timespec()
        }
        CLOCK_MONOTONIC
        | CLOCK_MONOTONIC_RAW
        | CLOCK_MONOTONIC_COARSE
        | CLOCK_BOOTTIME
        | CLOCK_BOOTTIME_ALARM => TimeSpec::now(),
        CLOCK_PROCESS_CPUTIME_ID => {
            let task = current_task().unwrap();
            task.flush_cpu_time();
            cpu_clock_from_rusage(task.process.cpu_rusage(), CPUCLOCK_PROF)
        }
        CLOCK_THREAD_CPUTIME_ID => {
            let task = current_task().unwrap();
            let inner = task.acquire_inner_lock();
            cpu_clock_from_rusage(inner.rusage, CPUCLOCK_PROF)
        }
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
    }
    SUCCESS
}

pub fn sys_clock_getres(clk_id: usize, tp: *mut TimeSpec) -> isize {
    let ns = match clk_id {
        CLOCK_REALTIME | CLOCK_MONOTONIC | CLOCK_MONOTONIC_RAW | CLOCK_BOOTTIME
        | CLOCK_REALTIME_ALARM | CLOCK_BOOTTIME_ALARM | CLOCK_TAI => {
            // hardware counter resolution: ceil(1e9 / freq)
            let freq = crate::timer::clock_freq();
            ((1_000_000_000u128 + freq as u128 - 1) / freq as u128) as usize
        }
        CLOCK_REALTIME_COARSE | CLOCK_MONOTONIC_COARSE => {
            // sched tick granularity
            10_000_000 // 10 ms
        }
        // 进程账户按 1ms 批量冲刷；返回真实可保证的精度，不能继续宣称 1us。
        CLOCK_PROCESS_CPUTIME_ID => crate::task::PROCESS_CPU_ACCOUNT_BATCH_US * 1_000,
        CLOCK_THREAD_CPUTIME_ID => 1_000,
        _ => {
            let clock = match validate_cpu_clock_id(clk_id) {
                Ok(clock) => clock,
                Err(errno) => return errno,
            };
            if clock.per_thread {
                1_000
            } else {
                crate::task::PROCESS_CPU_ACCOUNT_BATCH_US * 1_000
            }
        }
    };
    if !tp.is_null() {
        let resolution = TimeSpec {
            tv_sec: 0,
            tv_nsec: ns,
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

    if flags & TIMER_ABSTIME != 0 {
        if clk_id == CLOCK_REALTIME {
            return match sleep_until_realtime_interruptible(req) {
                Ok(()) => SUCCESS,
                Err(_) => EINTR,
            };
        }
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
                    if let Err(errno) = UserPtrMut::new(rmtp).write(token, &interrupted.remaining) {
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
    task.flush_cpu_time();
    let process_rusage = task.process.cpu_rusage();
    let child_rusage = task.process.child_rusage();
    let token = current_user_token();
    let times = Times {
        tms_utime: timeval_to_user_ticks(process_rusage.ru_utime),
        tms_stime: timeval_to_user_ticks(process_rusage.ru_stime),
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
    let token = current_user_token();
    let rusage = match who {
        RUSAGE_SELF => {
            let resident_kb = task.process.vm().read(|vm| vm.resident_user_bytes()) / 1024;
            task.flush_cpu_time();
            let mut usage = task.process.cpu_rusage();
            usage.update_maxrss_kb(resident_kb);
            usage
        }
        RUSAGE_THREAD => {
            let resident_kb = task.process.vm().read(|vm| vm.resident_user_bytes()) / 1024;
            let mut inner = task.acquire_inner_lock();
            inner.rusage.update_maxrss_kb(resident_kb);
            inner.rusage
        }
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
    let us = value.to_us();
    if us == 0 {
        0
    } else {
        us.saturating_mul(USER_HZ).saturating_add(USEC_PER_SEC - 1) / USEC_PER_SEC
    }
}
