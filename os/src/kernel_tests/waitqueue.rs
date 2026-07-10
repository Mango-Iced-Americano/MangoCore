//! L3 tests for the WaitQueue subsystem.
//!
//! # Note on wake_once / wake_all tests
//!
//! Full `wake_once` and `wake_all` tests require multiple kernel tasks
//! (one waiter, one waker). This needs a minimal kernel-thread spawn API
//! which is planned as a follow-up. For now, we test:
//! - The no-lost-wakeup invariant (condition already true → no sleep)
//! - Basic queue operations (add, is_empty, compact_stale)
//! - wake_all on empty queue returns 0
//!
//! # Note on false-condition test
//!
//! We cannot test `wait_until` with a false condition because there is
//! no waker thread to unblock it. While `wait_until` has an internal
//! fallback timeout, relying on it would make the test flaky.

use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;
use crate::kernel_tests::runner::KernelTest;
use crate::task::WaitQueue;

/// Returns all waitqueue-related kernel tests.
pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new(
            "waitqueue::wake_before_wait_should_not_sleep",
            test_wake_before_wait_should_not_sleep,
        ),
        KernelTest::new("waitqueue::basic_queue_ops", test_basic_queue_ops),
        KernelTest::new("waitqueue::wake_all_on_empty", test_wake_all_on_empty),
        // TODO: wake_once, wake_all — requires kernel thread spawn API
    ]
}

/// Condition already satisfied: wait_until must return immediately.
/// This is the classic lost-wakeup prevention test.
///
/// With flag=true, the closure returns `Some(42)` on the first call,
/// so `wait_until` should return 42 without blocking.
fn test_wake_before_wait_should_not_sleep() -> Result<(), &'static str> {
    let wq = Mutex::new(WaitQueue::new());
    let flag = AtomicBool::new(true);

    let result = WaitQueue::wait_until(&wq, || {
        if flag.load(Ordering::Acquire) {
            Some(42)
        } else {
            None
        }
    });

    if result == 42 {
        Ok(())
    } else {
        Err("wait_until should have returned 42 immediately when condition is true")
    }
}

/// Basic queue operations: new, is_empty, compact_stale on empty queue.
fn test_basic_queue_ops() -> Result<(), &'static str> {
    let mut wq = WaitQueue::new();

    // New queue should start empty.
    if !wq.is_empty() {
        return Err("new WaitQueue should be empty");
    }

    // compact_stale on empty queue should return 0 and leave it empty.
    let removed = wq.compact_stale();
    if removed != 0 {
        return Err("compact_stale on empty queue should return 0");
    }
    if !wq.is_empty() {
        return Err("queue should still be empty after compact_stale");
    }

    Ok(())
}

/// Calling wake_all() on an empty queue should return 0.
fn test_wake_all_on_empty() -> Result<(), &'static str> {
    let mut wq = WaitQueue::new();
    let woken = wq.wake_all();
    if woken != 0 {
        return Err("wake_all on empty queue should return 0");
    }
    Ok(())
}
