//! Blocking, timeout, multi-queue, and stale-entry WaitQueue tests.

use crate::task::{self, WaitQueue, WaitResult};
use crate::timer::{self, TimeSpec};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
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

lazy_static! {
    static ref BLOCK_WQ: Mutex<WaitQueue> = Mutex::new(WaitQueue::new());
    static ref QUIET_WQ: Mutex<WaitQueue> = Mutex::new(WaitQueue::new());
    static ref MULTI_LEFT: Mutex<WaitQueue> = Mutex::new(WaitQueue::new());
    static ref MULTI_RIGHT: Mutex<WaitQueue> = Mutex::new(WaitQueue::new());
    static ref TIMEOUT_WQ: Mutex<WaitQueue> = Mutex::new(WaitQueue::new());
    static ref STALE_WQ: Mutex<WaitQueue> = Mutex::new(WaitQueue::new());
}

static BLOCK_CONDITION: AtomicBool = AtomicBool::new(false);
static BLOCK_ENTERED: AtomicBool = AtomicBool::new(false);
static BLOCK_DONE: AtomicBool = AtomicBool::new(false);

fn block_worker() {
    BLOCK_ENTERED.store(true, Ordering::Release);
    if WaitQueue::wait_until(&BLOCK_WQ, || {
        BLOCK_CONDITION.load(Ordering::Acquire).then_some(7)
    }) == 7
    {
        BLOCK_DONE.store(true, Ordering::Release);
    }
}

/// A blocked task must return only after a producer changes the condition and wakes it.
pub(super) fn test_basic_block_wake() -> Result<(), &'static str> {
    BLOCK_CONDITION.store(false, Ordering::Release);
    BLOCK_ENTERED.store(false, Ordering::Release);
    BLOCK_DONE.store(false, Ordering::Release);
    task::spawn_ktest_task(block_worker);

    if !yield_until(|| BLOCK_ENTERED.load(Ordering::Acquire)) {
        return Err("block worker did not enter wait_until");
    }
    BLOCK_CONDITION.store(true, Ordering::Release);
    if BLOCK_WQ.lock().wake_all() != 1 {
        return Err("wake_all did not notify the blocked worker");
    }
    if !yield_until(|| BLOCK_DONE.load(Ordering::Acquire)) {
        return Err("worker did not return Ready after wake_all");
    }
    Ok(())
}

static QUIET_CONDITION: AtomicBool = AtomicBool::new(false);
static QUIET_ENTERED: AtomicBool = AtomicBool::new(false);
static QUIET_RETURNED: AtomicBool = AtomicBool::new(false);

fn quiet_worker() {
    QUIET_ENTERED.store(true, Ordering::Release);
    let _ = WaitQueue::wait_until(&QUIET_WQ, || {
        QUIET_CONDITION.load(Ordering::Acquire).then_some(1)
    });
    QUIET_RETURNED.store(true, Ordering::Release);
}

/// An indefinite wait remains blocked across many ticks until explicitly notified.
pub(super) fn test_no_spurious_wake_without_fallback() -> Result<(), &'static str> {
    QUIET_CONDITION.store(false, Ordering::Release);
    QUIET_ENTERED.store(false, Ordering::Release);
    QUIET_RETURNED.store(false, Ordering::Release);
    task::spawn_ktest_task(quiet_worker);

    if !yield_until(|| QUIET_ENTERED.load(Ordering::Acquire)) {
        return Err("quiet worker did not register its wait");
    }
    let deadline = TimeSpec::now() + TimeSpec::from_ms(200);
    while TimeSpec::now() < deadline {
        task::suspend_current_and_run_next();
    }
    if QUIET_RETURNED.load(Ordering::Acquire) {
        return Err("indefinite wait returned without a condition change or wake");
    }

    QUIET_CONDITION.store(true, Ordering::Release);
    if QUIET_WQ.lock().wake_all() != 1 {
        return Err("quiet worker was not present after 200ms");
    }
    if !yield_until(|| QUIET_RETURNED.load(Ordering::Acquire)) {
        return Err("quiet worker did not finish after explicit wake");
    }
    Ok(())
}

/// A producer notification immediately after registration is retained as a one-shot wake.
pub(super) fn test_lost_wakeup_handshake() -> Result<(), &'static str> {
    let queue = Mutex::new(WaitQueue::new());
    let task = task::current_task().ok_or("no current task for lost-wakeup test")?;
    let _waiter = queue.lock().prepare_to_wait(Arc::downgrade(&task));

    if queue.lock().wake_all() != 1 {
        return Err("producer did not notify registered waiter");
    }
    if queue.lock().finish_wait(&task) {
        return Err("already-notified waiter was still registered");
    }
    if !queue.lock().is_empty() {
        return Err("one-shot notification left a stale queue entry");
    }
    Ok(())
}

static MULTI_CONDITION: AtomicBool = AtomicBool::new(false);
static MULTI_ENTERED: AtomicBool = AtomicBool::new(false);
static MULTI_RESULT: AtomicIsize = AtomicIsize::new(-1);

fn multi_queue_worker() {
    MULTI_ENTERED.store(true, Ordering::Release);
    let queues = [&*MULTI_LEFT, &*MULTI_RIGHT];
    let result = WaitQueue::wait_on_queues_interruptible_timeout(
        &queues,
        || MULTI_CONDITION.load(Ordering::Acquire).then_some(99),
        None,
    );
    let code = match result {
        WaitResult::Ready(99) => 99,
        WaitResult::Ready(_) => -2,
        WaitResult::Interrupted => -3,
        WaitResult::TimedOut => -4,
    };
    MULTI_RESULT.store(code, Ordering::Release);
}

fn run_multi_queue_wake(wake_left: bool) -> Result<(), &'static str> {
    MULTI_CONDITION.store(false, Ordering::Release);
    MULTI_ENTERED.store(false, Ordering::Release);
    MULTI_RESULT.store(-1, Ordering::Release);
    task::spawn_ktest_task(multi_queue_worker);
    if !yield_until(|| MULTI_ENTERED.load(Ordering::Acquire)) {
        return Err("multi-queue worker did not register");
    }
    MULTI_CONDITION.store(true, Ordering::Release);
    let woken = if wake_left {
        MULTI_LEFT.lock().wake_all()
    } else {
        MULTI_RIGHT.lock().wake_all()
    };
    if woken != 1 {
        return Err("selected multi-queue source did not wake the waiter");
    }
    if !yield_until(|| MULTI_RESULT.load(Ordering::Acquire) != -1) {
        return Err("multi-queue waiter did not return");
    }
    if MULTI_RESULT.load(Ordering::Acquire) != 99 {
        return Err("multi-queue waiter did not return Ready(99)");
    }
    let other_empty = if wake_left {
        MULTI_RIGHT.lock().is_empty()
    } else {
        MULTI_LEFT.lock().is_empty()
    };
    if !other_empty {
        return Err("other multi-queue registration was not cleaned up");
    }
    Ok(())
}

/// Either registered queue can satisfy the wait and removes the other registration.
pub(super) fn test_multi_queue_cleanup() -> Result<(), &'static str> {
    run_multi_queue_wake(true)?;
    run_multi_queue_wake(false)
}

static TIMEOUT_ENTERED: AtomicBool = AtomicBool::new(false);
static TIMEOUT_DONE: AtomicBool = AtomicBool::new(false);
static TIMEOUT_ELAPSED_MS: AtomicIsize = AtomicIsize::new(-1);

fn timeout_worker() {
    TIMEOUT_ENTERED.store(true, Ordering::Release);
    let started = timer::get_time_ms();
    let result = WaitQueue::wait_event_interruptible_timeout(
        &TIMEOUT_WQ,
        || None,
        TimeSpec::now() + TimeSpec::from_ms(50),
    );
    if result == WaitResult::TimedOut {
        TIMEOUT_ELAPSED_MS.store((timer::get_time_ms() - started) as isize, Ordering::Release);
    }
    TIMEOUT_DONE.store(true, Ordering::Release);
}

/// A timed wait without a producer expires at its deadline rather than using fallback wakeups.
pub(super) fn test_deadline_timeout() -> Result<(), &'static str> {
    TIMEOUT_ENTERED.store(false, Ordering::Release);
    TIMEOUT_DONE.store(false, Ordering::Release);
    TIMEOUT_ELAPSED_MS.store(-1, Ordering::Release);
    task::spawn_ktest_task(timeout_worker);
    if !yield_until(|| TIMEOUT_ENTERED.load(Ordering::Acquire)) {
        return Err("timeout worker did not enter its wait");
    }

    let harness_deadline = TimeSpec::now() + TimeSpec::from_ms(500);
    while !TIMEOUT_DONE.load(Ordering::Acquire) && TimeSpec::now() < harness_deadline {
        task::suspend_current_and_run_next();
    }
    let elapsed = TIMEOUT_ELAPSED_MS.load(Ordering::Acquire);
    if !TIMEOUT_DONE.load(Ordering::Acquire) || elapsed < 45 {
        return Err("50ms wait did not time out at its deadline");
    }
    Ok(())
}

static STALE_REGISTERED: AtomicBool = AtomicBool::new(false);

fn stale_waiter_worker() {
    if let Some(task) = task::current_task() {
        STALE_WQ.lock().add_task(Arc::downgrade(&task));
        STALE_REGISTERED.store(true, Ordering::Release);
    }
}

/// Entries from exited tasks are compacted and later wakes are harmless.
pub(super) fn test_stale_waiter_cleanup() -> Result<(), &'static str> {
    STALE_REGISTERED.store(false, Ordering::Release);
    task::spawn_ktest_task(stale_waiter_worker);
    if !yield_until(|| STALE_REGISTERED.load(Ordering::Acquire)) {
        return Err("stale waiter worker did not register");
    }

    for _ in 0..128 {
        if STALE_WQ.lock().compact_stale() == 1 {
            if STALE_WQ.lock().wake_all() != 0 {
                return Err("wake_all reported a compacted stale waiter");
            }
            return Ok(());
        }
        task::suspend_current_and_run_next();
    }
    Err("exited task did not become a stale weak waiter")
}
