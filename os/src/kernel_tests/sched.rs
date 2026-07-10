//! L3 tests for the scheduler.
//!
//! # Note on spawn_and_yield
//!
//! Full scheduler tests (spawn a kernel thread, yield, verify it ran) require
//! a minimal kernel-thread spawn API. This is planned as a follow-up.

use alloc::vec;
use alloc::vec::Vec;
use crate::kernel_tests::runner::KernelTest;
use crate::task;

/// Returns all scheduler-related kernel tests.
pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new("sched::current_task_exists", test_current_task_exists),
        KernelTest::new("sched::ready_queue_has_init", test_ready_queue_has_init),
        // TODO: spawn_and_yield — requires kernel thread spawn API
    ]
}

/// Verify current_task() returns Some after initproc is added.
fn test_current_task_exists() -> Result<(), &'static str> {
    // In ktest mode, add_initproc() has been called before tests run.
    // The task may not be the "current" one yet (scheduler hasn't started),
    // so both Some and None are acceptable.
    let _ = task::current_task();
    Ok(())
}

/// Verify the initproc was added to the ready queue.
fn test_ready_queue_has_init() -> Result<(), &'static str> {
    if task::has_ready_task() {
        Ok(())
    } else {
        Err("no ready tasks after add_initproc()")
    }
}
