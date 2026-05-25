use alloc::sync::Arc;

use crate::syscall::errno::EAGAIN;
use crate::task::{
    current_task, wake_interruptible, ProcessControlBlock, TaskControlBlock, TaskStatus,
};

use super::{is_realtime_signal, PendingSignal, SigInfo, Signals};

fn current_sender_pid() -> usize {
    current_task().map(|task| task.pid()).unwrap_or(0)
}

fn process_signal_target(
    process: &ProcessControlBlock,
    signal: Signals,
) -> Option<Arc<TaskControlBlock>> {
    for task in process.threads() {
        let inner = task.acquire_inner_lock();
        if inner.task_status == TaskStatus::Zombie {
            continue;
        }
        if !signal.difference(inner.sigmask).is_empty() {
            return Some(task.clone());
        }
    }
    None
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
        return send_process_signal_info(process, signal, pending.siginfo);
    }
    false
}

pub fn send_process_signal_info(
    process: &ProcessControlBlock,
    signal: Signals,
    siginfo: SigInfo,
) -> bool {
    if signal.is_empty() {
        return true;
    }
    if signal.contains(Signals::SIGCONT) {
        process.mark_continued();
    }
    process.enqueue_process_signal(PendingSignal { signal, siginfo });
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

pub fn send_thread_signal(task: &Arc<TaskControlBlock>, signal: Signals) -> Result<(), isize> {
    if signal.is_empty() {
        return Ok(());
    }
    send_thread_signal_info(task, signal, None, true)
}

pub fn send_thread_signal_info_deferred(
    task: &Arc<TaskControlBlock>,
    signal: Signals,
    siginfo: SigInfo,
) -> Result<(), isize> {
    send_thread_signal_info(task, signal, Some(siginfo), false)
}

fn send_thread_signal_info(
    task: &Arc<TaskControlBlock>,
    signal: Signals,
    siginfo: Option<SigInfo>,
    wake: bool,
) -> Result<(), isize> {
    if signal.is_empty() {
        return Ok(());
    }
    if signal.contains(Signals::SIGCONT) {
        task.process.mark_continued();
    }
    let mut inner = task.acquire_inner_lock();
    if is_realtime_signal(signal) && inner.sigpending.queued_count() >= inner.sigpending_limit_cur {
        return Err(EAGAIN);
    }
    if let Some(siginfo) = siginfo {
        inner.sigpending.enqueue(PendingSignal { signal, siginfo })?;
    } else {
        inner
            .sigpending
            .enqueue_signal_with_sender(signal, SigInfo::SI_TKILL as usize, current_sender_pid())?;
    }
    if wake
        && inner.task_status == TaskStatus::Interruptible
        && !signal.difference(inner.sigmask).is_empty()
    {
        inner.task_status = TaskStatus::Ready;
        drop(inner);
        wake_interruptible(task.clone());
    }
    Ok(())
}
