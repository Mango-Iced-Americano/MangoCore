use alloc::sync::Arc;

use crate::mm::{UserPtr, UserPtrMut};
use crate::syscall::errno::*;
use crate::task::{
    add_kernel_timer, current_task, current_user_token, signal::Signals, Rusage, TimerAction,
    WaitQueue, WaitResult,
};
use crate::timer::{get_time_ms, ITimerVal, TimeSpec, TimeVal, TimeZone, Times};
use log::{info, trace};

pub fn sys_nanosleep(req: *const TimeSpec, rem: *mut TimeSpec) -> isize {
    if req.is_null() {
        return EINVAL;
    }
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let req = match UserPtr::new(req).read(token) {
        Ok(req) => req,
        Err(errno) => return errno,
    };

    let end = TimeSpec::now() + req;
    let wait_queue = spin::Mutex::new(WaitQueue::new());
    let wait_result =
        WaitQueue::wait_event_interruptible_timeout(&wait_queue, || None::<isize>, end);
    let now = TimeSpec::now();

    // 先释放 inner 锁再检查信号，避免与 has_actionable_signal 死锁
    // 参考 pselect/ppoll 的信号检查模式
    if wait_result == WaitResult::Interrupted {
        // 被可操作信号打断 → 返回剩余时间 + EINTR
        if !rem.is_null() {
            UserPtrMut::new(rem).write(token, &(end - now)).unwrap();
        }
        return EINTR;
    }

    // 正常超时返回
    if !rem.is_null() {
        UserPtrMut::new(rem).write(token, &TimeSpec::new()).unwrap();
    }
    SUCCESS
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
    if !rqtp.is_null() {
        let token = current_user_token();
        let timespec = match UserPtr::new(rqtp).read(token) {
            Ok(timespec) => timespec,
            Err(errno) => return errno,
        };
        info!(
            "[sys_clock_nanosleep] clk_id: {}, flags: {:?}, rqtp: {:?}, rmtp: {:?}",
            clk_id, flags, timespec, rmtp
        );
    }
    SUCCESS
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
