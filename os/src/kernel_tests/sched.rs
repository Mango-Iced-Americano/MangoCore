//! L3 tests for the scheduler.
//!
//! Multi-task tests (spawn_and_yield) require the scheduler to be active
//! (mango.mode=ktest with the new multi-task harness).

use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use crate::kernel_tests::runner::KernelTest;
use crate::task;

/// Returns all scheduler-related kernel tests.
pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new("sched::current_task_exists", test_current_task_exists),
        KernelTest::new("sched::ready_queue_has_init", test_ready_queue_has_init),
        KernelTest::new("sched::task_manager_counts", test_task_manager_counts),
        KernelTest::new("sched::spawn_and_yield", test_spawn_and_yield),
    ]
}

/// Verify tasks exist after add_initproc().
fn test_current_task_exists() -> Result<(), &'static str> {
    // In scheduler-active ktest, this runs inside a scheduled task
    // so we expect a valid current_task().
    if task::current_task().is_some() || task::task_manager_counts().map(|(r, _)| r > 0).unwrap_or(false) {
        Ok(())
    } else {
        Err("no current task and no ready tasks")
    }
}

/// Verify the scheduler has ready tasks.
fn test_ready_queue_has_init() -> Result<(), &'static str> {
    if task::has_ready_task() {
        Ok(())
    } else {
        Err("no ready tasks after init")
    }
}

/// Verify task_manager_counts() returns sensible values.
fn test_task_manager_counts() -> Result<(), &'static str> {
    let (ready, interruptible) = task::task_manager_counts()
        .ok_or("task_manager_counts returned None")?;
    if ready == 0 {
        return Err("ready_count should be > 0");
    }
    if ready > 4096 || interruptible > 4096 {
        return Err("task counts implausibly high");
    }
    Ok(())
}

/// Spawn a kernel task, yield, and verify it ran.
///
/// Pattern:
/// 1. Spawn a helper task that sets a flag and exits.
/// 2. Yield to let the spawned task run.
/// 3. Verify the flag was set.
fn test_spawn_and_yield() -> Result<(), &'static str> {
    static SPAWNED_RAN: AtomicBool = AtomicBool::new(false);

    task::spawn_ktest_task(|| {
        SPAWNED_RAN.store(true, Ordering::SeqCst);
    });

    // Yield to let the spawned task run and exit.
    task::suspend_current_and_run_next();

    if !SPAWNED_RAN.load(Ordering::SeqCst) {
        return Err("spawned task did not run");
    }

    Ok(())
}
