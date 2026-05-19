use alloc::sync::Arc;

use crate::task::{
    current_task, wake_interruptible, ProcessControlBlock, TaskControlBlock, TaskStatus,
};

use super::{PendingSignal, SigInfo, Signals};

fn current_sender_pid() -> usize {
    current_task().map(|task| task.pid()).unwrap_or(0)
}

fn process_signal_target(
    process: &ProcessControlBlock,
    signal: Signals,
) -> Option<Arc<TaskControlBlock>> {
    let mut interruptible = None;
    for task in process.threads() {
        let inner = task.acquire_inner_lock();
        if inner.task_status == TaskStatus::Zombie {
            continue;
        }
        if inner.task_status == TaskStatus::Interruptible && interruptible.is_none() {
            interruptible = Some(task.clone());
        }
        if !signal.difference(inner.sigmask).is_empty() {
            return Some(task.clone());
        }
    }
    interruptible
}

pub fn send_process_signal(process: &ProcessControlBlock, signal: Signals) -> bool {
    if signal.is_empty() {
        return true;
    }
    if let Ok(pending) = PendingSignal::from_signal_with_sender(
        signal,
        SigInfo::SI_USER as usize,
        current_sender_pid(),
    ) {
        process.enqueue_process_signal(pending);
    }
    if let Some(task) = process_signal_target(process, signal) {
        let mut inner = task.acquire_inner_lock();
        if inner.task_status == TaskStatus::Interruptible {
            inner.task_status = TaskStatus::Ready;
            drop(inner);
            wake_interruptible(task);
        }
    }
    true
}

pub fn send_thread_signal(task: &Arc<TaskControlBlock>, signal: Signals) -> bool {
    if signal.is_empty() {
        return true;
    }
    let mut inner = task.acquire_inner_lock();
    let _ = inner
        .sigpending
        .enqueue_signal_with_sender(signal, SigInfo::SI_TKILL as usize, current_sender_pid());
    if inner.task_status == TaskStatus::Interruptible {
        inner.task_status = TaskStatus::Ready;
        drop(inner);
        wake_interruptible(task.clone());
    }
    true
}
