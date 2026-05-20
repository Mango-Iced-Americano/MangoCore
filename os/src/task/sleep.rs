use spin::Mutex;

use crate::timer::TimeSpec;

use super::{WaitQueue, WaitResult};

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

fn sleep_until_deadline(
    deadline: TimeSpec,
    report_remaining: bool,
) -> Result<(), SleepInterrupted> {
    if TimeSpec::now() >= deadline {
        return Ok(());
    }

    let wait_queue = Mutex::new(WaitQueue::new());
    match WaitQueue::wait_event_interruptible_timeout(&wait_queue, || None::<isize>, deadline) {
        WaitResult::Interrupted => {
            let remaining = if report_remaining {
                deadline - TimeSpec::now()
            } else {
                TimeSpec::new()
            };
            Err(SleepInterrupted { remaining })
        }
        WaitResult::Ready(_) | WaitResult::TimedOut => Ok(()),
    }
}
