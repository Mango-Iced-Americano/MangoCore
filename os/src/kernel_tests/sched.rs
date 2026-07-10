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
        KernelTest::new("sched::task_manager_counts", test_task_manager_counts),
        // TODO: spawn_and_yield — requires kernel thread spawn API
    ]
}

/// Verify tasks exist after add_initproc().
///
/// In ktest mode, the scheduler hasn't started yet (`run_tasks()` is called
/// after tests finish), so `current_task()` is None.  Instead we verify that
/// the task manager's ready queue is non-empty — the initproc was added by
/// `add_initproc()` before ktest runs.
fn test_current_task_exists() -> Result<(), &'static str> {
    if let Some((ready, _interruptible)) = task::task_manager_counts() {
        if ready == 0 {
            return Err("no ready tasks: add_initproc() should have added at least one");
        }
        Ok(())
    } else {
        Err("task_manager_counts returned None")
    }
}

/// Verify the initproc was added to the ready queue.
fn test_ready_queue_has_init() -> Result<(), &'static str> {
    if task::has_ready_task() {
        Ok(())
    } else {
        Err("no ready tasks after add_initproc()")
    }
}

/// Verify task_manager_counts() returns sensible values.
fn test_task_manager_counts() -> Result<(), &'static str> {
    let (ready, interruptible) = task::task_manager_counts()
        .ok_or("task_manager_counts returned None")?;
    // After add_initproc(), ready should be > 0.
    // interruptible can be 0 or more — just verify both are valid u16 values.
    if ready == 0 {
        return Err("ready_count should be > 0 after add_initproc()");
    }
    // Sanity: counts should not be absurdly high
    if ready > 4096 || interruptible > 4096 {
        return Err("task counts implausibly high");
    }
    Ok(())
}
