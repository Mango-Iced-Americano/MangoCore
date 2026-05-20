use alloc::sync::Arc;

use crate::mm::{UserPtr, UserPtrMut};
use crate::syscall::errno::*;
use crate::task::{
    add_kernel_timer, current_task, current_user_token, signal::Signals,
    sleep_relative_interruptible, sleep_until_interruptible, Rusage, TimerAction,
};
use crate::timer::{
    get_time_ms, ITimerVal, TimeSpec, TimeVal, TimeZone, Times, NSEC_PER_SEC,
};
use log::{info, trace};

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
const TIMER_ABSTIME: u32 = 1;

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

fn timeval_to_timespec(value: TimeVal) -> TimeSpec {
    TimeSpec::from_us(value.to_us())
}

fn timespec_to_timeval(value: TimeSpec) -> TimeVal {
    TimeVal::from_us(value.to_ns() / 1000)
}

pub fn sys_gettimeofday(tv: *mut TimeVal, _tz: *mut TimeZone) -> isize {
    // Timezone is currently NOT supported.
    if !tv.is_null() {
        let token = current_user_token();
        let timeval = &TimeVal::now();
        if UserPtrMut::new(tv).write(token, timeval).is_err() {
            log::error!("[sys_gettimeofday] Failed to copy to {:?}", tv);
            return EFAULT;
        }
    }
    SUCCESS
}

pub fn sys_get_time() -> isize {
    get_time_ms() as isize
}

pub fn sys_clock_gettime(clk_id: usize, tp: *mut TimeSpec) -> isize {
    if !tp.is_null() {
        let token = current_user_token();
        let timespec = &TimeSpec::now();
        if UserPtrMut::new(tp).write(token, timespec).is_err() {
            log::error!("[sys_clock_gettime] Failed to copy to {:?}", tp);
            return EFAULT;
        };
        log::trace!("[sys_clock_gettime] clk_id: {}, tp: {:?}", clk_id, timespec);
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
        match sleep_until_interruptible(req) {
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
    let inner = task.acquire_inner_lock();
    let token = task.get_user_token();
    let times = Times {
        tms_utime: inner.rusage.ru_utime.to_tick(),
        tms_stime: inner.rusage.ru_stime.to_tick(),
        tms_cutime: 0,
        tms_cstime: 0,
    };
    if UserPtrMut::new(buf).write(token, &times).is_err() {
        log::error!("[sys_times] Failed to copy to {:?}", buf);
        return EFAULT;
    };
    // return clock ticks that have elapsed since an arbitrary point in the past
    crate::hal::get_time() as isize
}

pub fn sys_getrusage(who: isize, usage: *mut Rusage) -> isize {
    if who != 0 {
        panic!("[sys_getrusage] parameter 'who' is not RUSAGE_SELF.");
    }
    let task = current_task().unwrap();
    let inner = task.acquire_inner_lock();
    let token = task.get_user_token();
    if UserPtrMut::new(usage).write(token, &inner.rusage).is_err() {
        log::error!("[sys_getrusage] Failed to copy to {:?}", usage);
        return EFAULT;
    };
    //info!("[sys_getrusage] who: RUSAGE_SELF, usage: {:?}", inner.rusage);
    SUCCESS
}
