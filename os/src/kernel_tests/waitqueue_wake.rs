//! FIFO, broadcast, and repetition tests for WaitQueue wake operations.

use crate::task::{self, WaitQueue};
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

lazy_static! {
    static ref FIFO_WQ: Mutex<WaitQueue> = Mutex::new(WaitQueue::new());
    static ref ALL_WQ: Mutex<WaitQueue> = Mutex::new(WaitQueue::new());
    static ref STRESS_WQ: Mutex<WaitQueue> = Mutex::new(WaitQueue::new());
}

static FIFO_RELEASE: AtomicBool = AtomicBool::new(false);
static FIFO_REGISTERED: AtomicUsize = AtomicUsize::new(0);
static FIFO_FINISHED: AtomicUsize = AtomicUsize::new(0);
static FIFO_ORDER: AtomicUsize = AtomicUsize::new(0);

fn fifo_worker() {
    let slot = FIFO_REGISTERED.fetch_add(1, Ordering::AcqRel);
    let _ = WaitQueue::wait_until(&FIFO_WQ, || FIFO_RELEASE.load(Ordering::Acquire).then_some(0));
    let order = FIFO_FINISHED.fetch_add(1, Ordering::AcqRel);
    FIFO_ORDER.fetch_or((slot + 1) << (order * 4), Ordering::AcqRel);
}

/// wake_at_most(1) drives the internal wake_one path in registration order.
pub(super) fn test_wake_one_fifo() -> Result<(), &'static str> {
    FIFO_RELEASE.store(false, Ordering::Release);
    FIFO_REGISTERED.store(0, Ordering::Release);
    FIFO_FINISHED.store(0, Ordering::Release);
    FIFO_ORDER.store(0, Ordering::Release);
    for _ in 0..3 {
        task::spawn_ktest_task(fifo_worker);
    }
    if !yield_until(|| FIFO_REGISTERED.load(Ordering::Acquire) == 3) {
        return Err("all FIFO workers did not register");
    }

    FIFO_RELEASE.store(true, Ordering::Release);
    for expected in 1..=3 {
        if FIFO_WQ.lock().wake_at_most(1) != 1 {
            return Err("wake_one path did not wake exactly one waiter");
        }
        if !yield_until(|| FIFO_FINISHED.load(Ordering::Acquire) == expected) {
            return Err("FIFO worker did not resume after its wake");
        }
    }
    if FIFO_ORDER.load(Ordering::Acquire) != 0x321 {
        return Err("wake_one path did not preserve waiter FIFO order");
    }
    Ok(())
}

static ALL_RELEASE: AtomicBool = AtomicBool::new(false);
static ALL_REGISTERED: AtomicUsize = AtomicUsize::new(0);
static ALL_FINISHED: AtomicUsize = AtomicUsize::new(0);

fn all_worker() {
    ALL_REGISTERED.fetch_add(1, Ordering::AcqRel);
    let _ = WaitQueue::wait_until(&ALL_WQ, || ALL_RELEASE.load(Ordering::Acquire).then_some(0));
    ALL_FINISHED.fetch_add(1, Ordering::AcqRel);
}

/// A single wake_all notifies every registered waiter exactly once.
pub(super) fn test_wake_all_wakes_all() -> Result<(), &'static str> {
    ALL_RELEASE.store(false, Ordering::Release);
    ALL_REGISTERED.store(0, Ordering::Release);
    ALL_FINISHED.store(0, Ordering::Release);
    for _ in 0..3 {
        task::spawn_ktest_task(all_worker);
    }
    if !yield_until(|| ALL_REGISTERED.load(Ordering::Acquire) == 3) {
        return Err("all wake_all workers did not register");
    }
    ALL_RELEASE.store(true, Ordering::Release);
    if ALL_WQ.lock().wake_all() != 3 {
        return Err("wake_all did not notify all three waiters");
    }
    if !yield_until(|| ALL_FINISHED.load(Ordering::Acquire) == 3) {
        return Err("not all wake_all workers returned");
    }
    if !ALL_WQ.lock().is_empty() {
        return Err("wake_all left duplicate queue entries");
    }
    Ok(())
}

static STRESS_RELEASE: AtomicBool = AtomicBool::new(false);
static STRESS_REGISTERED: AtomicUsize = AtomicUsize::new(0);
static STRESS_FINISHED: AtomicUsize = AtomicUsize::new(0);

fn stress_worker() {
    STRESS_REGISTERED.fetch_add(1, Ordering::AcqRel);
    let _ = WaitQueue::wait_until(&STRESS_WQ, || {
        STRESS_RELEASE.load(Ordering::Acquire).then_some(0)
    });
    STRESS_FINISHED.fetch_add(1, Ordering::AcqRel);
}

/// Repeated registration and wakeups cannot lose a task or enqueue it twice.
pub(super) fn test_thousand_cycle_stress() -> Result<(), &'static str> {
    STRESS_RELEASE.store(false, Ordering::Release);
    STRESS_REGISTERED.store(0, Ordering::Release);
    STRESS_FINISHED.store(0, Ordering::Release);

    for iteration in 1..=1000 {
        task::spawn_ktest_task(stress_worker);
        if !yield_until(|| STRESS_REGISTERED.load(Ordering::Acquire) == iteration) {
            return Err("stress worker did not register");
        }
        STRESS_RELEASE.store(true, Ordering::Release);
        if STRESS_WQ.lock().wake_all() != 1 || STRESS_WQ.lock().wake_all() != 0 {
            return Err("stress wake was lost or enqueued a duplicate task");
        }
        if !yield_until(|| STRESS_FINISHED.load(Ordering::Acquire) == iteration) {
            return Err("stress worker did not finish after wake");
        }
        STRESS_RELEASE.store(false, Ordering::Release);
    }
    Ok(())
}
