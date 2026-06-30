//! 可中断睡眠辅助。
//!
//! 本模块为 `nanosleep`/`clock_nanosleep` 等路径提供基于 `WaitQueue` 的睡眠。
//! 单核 QEMU 上短尾部使用自旋补偿，以减少定时器唤醒后返回用户态的过早超时。

use core::sync::atomic::{AtomicUsize, Ordering};

use lazy_static::lazy_static;
use spin::Mutex;

use crate::timer::{current_timespec, get_time, timespec_to_ticks_ceil, TimeSpec};

use super::{WaitQueue, WaitResult};

const PRECISE_SLEEP_SPIN_NS: usize = 750_000;

lazy_static! {
    static ref REALTIME_ABSTIME_SLEEP_WAIT: Mutex<WaitQueue> = Mutex::new(WaitQueue::new());
}

static REALTIME_CLOCK_CHANGE_SEQ: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 睡眠被信号中断时返回的剩余时间。
pub struct SleepInterrupted {
    /// 剩余时间；绝对时间睡眠在中断时按对应时钟重新计算。
    pub remaining: TimeSpec,
}

/// 按相对时间睡眠，允许可处理信号中断。
///
/// # Errors
///
/// 被信号中断时返回 `SleepInterrupted`，其中 `remaining` 为剩余相对时间。
pub fn sleep_relative_interruptible(req: TimeSpec) -> Result<(), SleepInterrupted> {
    let deadline = TimeSpec::now() + req;
    sleep_until_deadline(deadline, true)
}

/// 按单调时钟绝对 deadline 睡眠，允许信号中断。
pub fn sleep_until_interruptible(deadline: TimeSpec) -> Result<(), SleepInterrupted> {
    sleep_until_deadline(deadline, false)
}

/// 按实时时钟绝对 deadline 睡眠，允许信号和 `clock_settime` 唤醒重算。
///
/// # Locking
///
/// 通过全局 `REALTIME_ABSTIME_SLEEP_WAIT` 等待队列挂起；`clock_settime`
/// 调用 `wake_realtime_abstime_sleepers_after_clock_set()` 唤醒所有等待者。
pub fn sleep_until_realtime_interruptible(deadline: TimeSpec) -> Result<(), SleepInterrupted> {
    loop {
        let now_realtime = current_timespec();
        if now_realtime >= deadline {
            return Ok(());
        }

        let observed_seq = REALTIME_CLOCK_CHANGE_SEQ.load(Ordering::Relaxed);
        let deadline_monotonic = TimeSpec::now() + (deadline - now_realtime);
        match WaitQueue::wait_event_interruptible_timeout(
            &REALTIME_ABSTIME_SLEEP_WAIT,
            || {
                if REALTIME_CLOCK_CHANGE_SEQ.load(Ordering::Relaxed) != observed_seq {
                    Some(0)
                } else if current_timespec() >= deadline {
                    Some(0)
                } else {
                    None
                }
            },
            deadline_monotonic,
        ) {
            WaitResult::Ready(_) | WaitResult::TimedOut => {}
            WaitResult::Interrupted => {
                return Err(SleepInterrupted {
                    remaining: realtime_remaining(deadline),
                });
            }
        }
    }
}

/// 在实时时钟被修改后唤醒所有绝对实时时钟睡眠者。
///
/// 返回被唤醒的任务数量。
pub fn wake_realtime_abstime_sleepers_after_clock_set() -> usize {
    REALTIME_CLOCK_CHANGE_SEQ.fetch_add(1, Ordering::Relaxed);
    REALTIME_ABSTIME_SLEEP_WAIT.lock().wake_all()
}

fn realtime_remaining(deadline: TimeSpec) -> TimeSpec {
    let now = current_timespec();
    if deadline <= now {
        TimeSpec::new()
    } else {
        deadline - now
    }
}

fn sleep_until_deadline(
    deadline: TimeSpec,
    report_remaining: bool,
) -> Result<(), SleepInterrupted> {
    let now = TimeSpec::now();
    if now >= deadline {
        return Ok(());
    }

    let spin_guard = TimeSpec::from_ns(PRECISE_SLEEP_SPIN_NS);
    if deadline - now > spin_guard {
        let wait_deadline = deadline - spin_guard;
        let wait_queue = Mutex::new(WaitQueue::new());
        match WaitQueue::wait_event_interruptible_timeout(
            &wait_queue,
            || None::<isize>,
            wait_deadline,
        ) {
            WaitResult::Interrupted => {
                let remaining = if report_remaining {
                    deadline - TimeSpec::now()
                } else {
                    TimeSpec::new()
                };
                return Err(SleepInterrupted { remaining });
            }
            WaitResult::Ready(_) | WaitResult::TimedOut => {}
        }
    }

    let deadline_ticks = timespec_to_ticks_ceil(deadline);
    while get_time() < deadline_ticks {
        core::hint::spin_loop();
    }
    Ok(())
}
