//! Signal-interrupt and signal/wake race tests for WaitQueue.

use crate::task::{self, Signals, WaitQueue, WaitResult};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

fn yield_until(mut condition: impl FnMut() -> bool) -> bool {
    for _ in 0..128 {
        if condition() {
            return true;
        }
        task::suspend_current_and_run_next();
    }
    condition()
}

fn result_code(result: WaitResult) -> usize {
    match result {
        WaitResult::Ready(55) => 1,
        WaitResult::Ready(_) => 2,
        WaitResult::Interrupted => 3,
        WaitResult::TimedOut => 4,
    }
}

lazy_static! {
    static ref INTERRUPT_WQ: Mutex<WaitQueue> = Mutex::new(WaitQueue::new());
    static ref RACE_WQ: Mutex<WaitQueue> = Mutex::new(WaitQueue::new());
}

static INTERRUPT_ENTERED: AtomicBool = AtomicBool::new(false);
static INTERRUPT_RESULT: AtomicUsize = AtomicUsize::new(0);

fn interrupt_worker() {
    INTERRUPT_ENTERED.store(true, Ordering::Release);
    let result = WaitQueue::wait_until_interruptible(&INTERRUPT_WQ, || None);
    INTERRUPT_RESULT.store(result_code(result), Ordering::Release);
}

/// A signal sent to a blocked interruptible waiter returns Interrupted.
pub(super) fn test_signal_interrupt() -> Result<(), &'static str> {
    INTERRUPT_ENTERED.store(false, Ordering::Release);
    INTERRUPT_RESULT.store(0, Ordering::Release);
    task::spawn_ktest_task(interrupt_worker);
    if !yield_until(|| INTERRUPT_ENTERED.load(Ordering::Acquire)) {
        return Err("interruptible worker did not register");
    }
    if !task::send_signal_to_interruptible(Signals::SIGUSR1) {
        return Err("signal injection did not find the blocked worker");
    }
    if !yield_until(|| INTERRUPT_RESULT.load(Ordering::Acquire) != 0) {
        return Err("signal did not resume interruptible waiter");
    }
    if INTERRUPT_RESULT.load(Ordering::Acquire) != 3 {
        return Err("signal wake did not return Interrupted");
    }
    Ok(())
}

static RACE_CONDITION: AtomicBool = AtomicBool::new(false);
static RACE_ENTERED: AtomicBool = AtomicBool::new(false);
static RACE_RESULT: AtomicUsize = AtomicUsize::new(0);

fn race_worker() {
    RACE_ENTERED.store(true, Ordering::Release);
    let result = WaitQueue::wait_until_interruptible(&RACE_WQ, || {
        RACE_CONDITION.load(Ordering::Acquire).then_some(55)
    });
    RACE_RESULT.store(result_code(result), Ordering::Release);
}

fn run_signal_wake_race(condition_ready: bool, expected: usize) -> Result<(), &'static str> {
    RACE_CONDITION.store(false, Ordering::Release);
    RACE_ENTERED.store(false, Ordering::Release);
    RACE_RESULT.store(0, Ordering::Release);
    task::spawn_ktest_task(race_worker);
    if !yield_until(|| RACE_ENTERED.load(Ordering::Acquire)) {
        return Err("race worker did not register");
    }
    RACE_CONDITION.store(condition_ready, Ordering::Release);
    if !task::send_signal_to_interruptible(Signals::SIGUSR1) {
        return Err("race signal did not find the waiter");
    }
    if RACE_WQ.lock().wake_all() != 1 {
        return Err("race explicit wake did not notify the waiter");
    }
    if !yield_until(|| RACE_RESULT.load(Ordering::Acquire) != 0) {
        return Err("race waiter did not return");
    }
    if RACE_RESULT.load(Ordering::Acquire) != expected {
        return Err("signal and wake race returned the wrong result");
    }
    Ok(())
}

/// Ready wins over a simultaneous signal; otherwise the signal wins.
pub(super) fn test_signal_wake_race() -> Result<(), &'static str> {
    run_signal_wake_race(true, 1)?;
    run_signal_wake_race(false, 3)
}
