//! L3 tests for the WaitQueue subsystem.
//!
//! Multi-task tests (wake_one) require the scheduler to be active
//! (mango.mode=ktest with the new multi-task harness).

#[path = "waitqueue_blocking.rs"]
mod blocking;
#[path = "waitqueue_interrupt.rs"]
mod interrupt;
#[path = "waitqueue_wake.rs"]
mod wake;

use crate::kernel_tests::runner::KernelTest;
use crate::{
    fs::vfs::event::{EPollEvent, EventWaitQueue},
    task::WaitQueue,
};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

/// Returns all waitqueue-related kernel tests.
pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new(
            "waitqueue::wake_before_wait_should_not_sleep",
            test_wake_before_wait_should_not_sleep,
        ),
        KernelTest::new(
            "waitqueue::early_wake_cancels_block",
            test_early_wake_cancels_block,
        ),
        KernelTest::new(
            "waitqueue::condition_can_notify_same_queue",
            test_condition_can_notify_same_queue,
        ),
        KernelTest::new("waitqueue::basic_queue_ops", test_basic_queue_ops),
        KernelTest::new("waitqueue::wake_all_on_empty", test_wake_all_on_empty),
        KernelTest::new("waitqueue::wake_one", test_wake_one),
        KernelTest::new(
            "waitqueue::basic_block_wake",
            blocking::test_basic_block_wake,
        ),
        KernelTest::with_timeout(
            "waitqueue::no_spurious_wake_without_fallback",
            blocking::test_no_spurious_wake_without_fallback,
            1000,
        ),
        KernelTest::new(
            "waitqueue::lost_wakeup_handshake",
            blocking::test_lost_wakeup_handshake,
        ),
        KernelTest::new(
            "waitqueue::multi_queue_cleanup",
            blocking::test_multi_queue_cleanup,
        ),
        KernelTest::with_timeout(
            "waitqueue::deadline_timeout",
            blocking::test_deadline_timeout,
            1000,
        ),
        KernelTest::new(
            "waitqueue::stale_waiter_cleanup",
            blocking::test_stale_waiter_cleanup,
        ),
        KernelTest::new("waitqueue::wake_one_fifo", wake::test_wake_one_fifo),
        KernelTest::new(
            "waitqueue::wake_all_wakes_all",
            wake::test_wake_all_wakes_all,
        ),
        KernelTest::with_timeout(
            "waitqueue::thousand_cycle_stress",
            wake::test_thousand_cycle_stress,
            5000,
        ),
        KernelTest::new(
            "waitqueue::signal_interrupt",
            interrupt::test_signal_interrupt,
        ),
        KernelTest::new(
            "waitqueue::signal_wake_race",
            interrupt::test_signal_wake_race,
        ),
    ]
}

/// 条件检查可能同步推进生产者，例如 socket 检查会调用网络 poll。
/// 生产者通知同一个 EventWaitQueue 时必须无死锁且不能丢失本轮 token。
fn test_condition_can_notify_same_queue() -> Result<(), &'static str> {
    let event_queue = EventWaitQueue::new();
    let mut checks = 0usize;

    let result = WaitQueue::wait_until(event_queue.wait_queue(), || {
        checks += 1;
        match checks {
            1 => None,
            2 => {
                if event_queue.notify_events_all(EPollEvent::EPOLLIN) != 1 {
                    return Some(-1);
                }
                None
            }
            _ => Some(7),
        }
    });

    if result == 7 {
        Ok(())
    } else {
        Err("same-queue notification was lost or returned an invalid result")
    }
}

/// 精确构造“条件队列已注册，但任务尚未进入 Blocking”的窗口。
///
/// 旧实现会因当前任务仍是 Running 而丢掉 wake；新 entry token 必须
/// 保留该通知，并让 checked block 撤销随后登记的 Blocking。
fn test_early_wake_cancels_block() -> Result<(), &'static str> {
    let queue = Mutex::new(WaitQueue::new());
    let task = crate::task::current_task().ok_or("waitqueue test has no current task")?;
    let cpu = crate::smp::cpu_id();

    let entry = queue
        .lock()
        .prepare_to_wait(alloc::sync::Arc::downgrade(&task));
    if queue.lock().wake_at_most(1) != 1 {
        return Err("early wake did not claim the registered wait entry");
    }
    if entry.is_waiting() {
        return Err("early wake was not persisted in the wait entry");
    }

    crate::task::block_current_and_run_next_checked(|_| entry.is_waiting());
    if task.task_status() != crate::task::TaskStatus::Running(cpu) {
        return Err("early wake did not cancel Blocking ownership");
    }
    if !queue.lock().is_empty() {
        return Err("claimed wait entry remained in the queue");
    }
    Ok(())
}

/// Condition already satisfied: wait_until must return immediately.
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

    if !wq.is_empty() {
        return Err("new WaitQueue should be empty");
    }

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

/// Full wake_one test with real scheduler participation.
///
/// Pattern:
/// 1. Spawn a waker task that calls wake_all on the shared WQ.
/// 2. Current task (waiter) calls wait_until — blocks because condition false.
/// 3. Scheduler picks waker → waker sets WAKER_RAN flag then calls wake_all.
/// 4. Waker exits → scheduler picks waiter → condition now true → returns.
fn test_wake_one() -> Result<(), &'static str> {
    lazy_static! {
        static ref WQ: Mutex<WaitQueue> = Mutex::new(WaitQueue::new());
    }
    static WAKER_RAN: AtomicBool = AtomicBool::new(false);

    // Spawn waker task — will run after this task blocks.
    crate::task::spawn_ktest_task(|| {
        WAKER_RAN.store(true, Ordering::SeqCst);
        WQ.lock().wake_all();
    });

    // Current task: wait for the waker to wake us.
    let result = WaitQueue::wait_until(&WQ, || {
        if WAKER_RAN.load(Ordering::SeqCst) {
            Some(1)
        } else {
            None
        }
    });

    if result != 1 {
        return Err("wait_until should have returned 1 after waker ran");
    }

    // Let the waker task finish its remaining schedule before returning.
    // (The waker may have already exited, but yield once to be safe.)
    crate::task::suspend_current_and_run_next();

    if !WAKER_RAN.load(Ordering::SeqCst) {
        return Err("waker task did not run");
    }

    Ok(())
}
