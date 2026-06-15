use spin::Mutex;

use crate::timer::{get_clock_freq, get_time, TimeSpec, NSEC_PER_SEC};

use super::{WaitQueue, WaitResult};

const PRECISE_SLEEP_SPIN_NS: usize = 750_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SleepInterrupted {
    pub remaining: TimeSpec,
}

#[inline(always)]
fn timespec_to_ticks(time: TimeSpec) -> usize {
    let freq = get_clock_freq();
    time.tv_sec
        .saturating_mul(freq)
        .saturating_add(time.tv_nsec.saturating_mul(freq) / NSEC_PER_SEC)
}

pub fn sleep_relative_interruptible(req: TimeSpec) -> Result<(), SleepInterrupted> {
    let deadline = TimeSpec::now() + req;
    sleep_until_deadline(deadline, true)
}

pub fn sleep_until_interruptible(deadline: TimeSpec) -> Result<(), SleepInterrupted> {
    sleep_until_deadline(deadline, false)
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

    let deadline_ticks = timespec_to_ticks(deadline);
    while get_time() < deadline_ticks {
        core::hint::spin_loop();
    }
    Ok(())
}
