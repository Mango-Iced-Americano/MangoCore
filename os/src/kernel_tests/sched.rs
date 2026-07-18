//! L3 tests for the scheduler.
//!
//! In ktest mode, the runner is spawned via spawn_ktest_task() and
//! runs inside the scheduler (run_tasks() is active). Tests verify
//! that the scheduler is operational and tasks can be spawned+yielded.

use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use crate::kernel_tests::runner::KernelTest;
use crate::task;

/// Returns all scheduler-related kernel tests.
pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new("sched::current_task_exists", test_current_task_exists),
        KernelTest::new("sched::scheduler_is_running", test_scheduler_is_running),
        KernelTest::new("sched::counts_in_range", test_counts_in_range),
        KernelTest::new("sched::spawn_and_yield", test_spawn_and_yield),
    ]
}

/// Verify we have a current task (scheduler assigned one to run).
fn test_current_task_exists() -> Result<(), &'static str> {
    match task::current_task() {
        Some(_) => Ok(()),
        None => Err("no current task — scheduler may not have started"),
    }
}

/// Verify the scheduler is running (we're inside a scheduled task).
fn test_scheduler_is_running() -> Result<(), &'static str> {
    // After spawn_ktest_task + run_tasks(), the runner IS the current task.
    // If the scheduler weren't running, current_task() would be None.
    if task::current_task().is_some() {
        Ok(())
    } else {
        Err("scheduler is not running — no current task")
    }
}

/// Verify task_manager_counts returns valid values (no overflow/garbage).
fn test_counts_in_range() -> Result<(), &'static str> {
    let (ready, interruptible) = task::task_manager_counts()
        .ok_or("task_manager_counts returned None")?;
    // In ktest mode, ready can be 0 (only the runner is active).
    // Both counts must be in a reasonable range.
    if ready > 4096 || interruptible > 4096 {
        return Err("task counts implausibly high");
    }
    Ok(())
}

/// Spawn a kernel task, yield, and verify it ran.
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
