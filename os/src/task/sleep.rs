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
pub struct SleepInterrupted {
    pub remaining: TimeSpec,
}

pub fn sleep_relative_interruptible(req: TimeSpec) -> Result<(), SleepInterrupted> {
    let deadline = TimeSpec::now() + req;
    sleep_until_deadline(deadline, true)
}

pub fn sleep_until_interruptible(deadline: TimeSpec) -> Result<(), SleepInterrupted> {
    sleep_until_deadline(deadline, false)
}

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
