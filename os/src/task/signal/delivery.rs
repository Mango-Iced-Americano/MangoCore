//! 信号投递与唤醒。
//!
//! 本模块把进程级 pending signal、线程私有 pending signal 和调度器唤醒组合起来。
//! 投递操作只负责把信号加入队列并唤醒可能的等待者，真正的默认动作和用户 handler
//! 在 `do_signal()` 返回用户态前处理。

use alloc::sync::Arc;

use crate::syscall::errno::EAGAIN;
use crate::task::{
    current_task_ref, wake_interruptible, ProcessControlBlock, TaskControlBlock,
};

use super::{is_realtime_signal, PendingSignal, SigInfo, Signals};

fn current_sender_pid() -> usize {
    current_task_ref().map(|task| task.pid()).unwrap_or(0)
}

fn process_signal_target(
    process: &ProcessControlBlock,
    signal: Signals,
) -> Option<Arc<TaskControlBlock>> {
    for task in process.threads() {
        let inner = task.acquire_inner_lock();
        if task.is_zombie() {
            continue;
        }
        if signal.wakes_interruptible(inner.sigmask, inner.signal_wait_mask, true) {
            // 返回 cloned Arc 后由调用方释放 task.inner 再执行调度器唤醒。
            return Some(task.clone());
        }
    }
    None
}

fn wake_task_if_interruptible(task: Arc<TaskControlBlock>) {
    // 状态判断和 CAS 必须由同一个 TASK_MANAGER 临界区完成；在此预判会让
    // Blocking -> Blocked 的切栈窗口重新产生 TOCTOU 竞争。
    let _ = wake_interruptible(task);
}

fn wake_process_interruptible_threads(process: &ProcessControlBlock) {
    for task in process.threads() {
        wake_task_if_interruptible(task);
    }
}

/// 向进程共享 pending 队列投递普通用户信号。
///
/// # Semantics
///
/// 返回值表示信号编号是否可转换为 pending signal；成功投递后若存在可被该信号
/// 打断的 interruptible 线程，会唤醒其中一个。
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

/// 向进程共享 pending 队列投递带 `SigInfo` 的信号。
///
/// # Locking
///
/// 函数不会在持有进程 signal lock 时进入调度器。`SIGCONT` 会唤醒所有
/// interruptible 线程，其它信号只唤醒一个合适目标。
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
        process.enqueue_process_signal(PendingSignal { signal, siginfo });
        wake_process_interruptible_threads(process);
        return true;
    }
    process.enqueue_process_signal(PendingSignal { signal, siginfo });
    if let Some(task) = process_signal_target(process, signal) {
        wake_task_if_interruptible(task);
    }
    true
}

/// 向进程共享 pending 队列投递信号，但不主动唤醒其它线程。
///
/// 当前线程即将检查 pending signal 的路径使用该接口，避免不必要的调度队列操作。
pub fn send_process_signal_to_current_task(process: &ProcessControlBlock, signal: Signals) -> bool {
    if signal.is_empty() {
        return true;
    }
    let Ok(pending) = PendingSignal::from_signal_with_sender(
        signal,
        SigInfo::SI_USER as usize,
        current_sender_pid(),
    ) else {
        return false;
    };
    if signal.contains(Signals::SIGCONT) {
        process.mark_continued();
    }
    process.enqueue_process_signal(pending);
    true
}

/// 向指定线程投递 `tgkill` 风格的线程私有信号。
pub fn send_thread_signal(task: &Arc<TaskControlBlock>, signal: Signals) -> Result<(), isize> {
    if signal.is_empty() {
        return Ok(());
    }
    send_thread_signal_info(task, signal, None, true)
}

/// 向指定线程投递带 `SigInfo` 的信号，但延迟唤醒。
///
/// # Semantics
///
/// 用于调用方已经知道稍后会统一处理唤醒的路径，避免重复操作调度队列。
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
        inner
            .sigpending
            .enqueue(PendingSignal { signal, siginfo })?;
    } else {
        inner.sigpending.enqueue_signal_with_sender(
            signal,
            SigInfo::SI_TKILL as usize,
            current_sender_pid(),
        )?;
    }
    if signal.contains(Signals::SIGCONT) {
        drop(inner);
        wake_task_if_interruptible(task.clone());
        return Ok(());
    }
    if signal.wakes_interruptible(inner.sigmask, inner.signal_wait_mask, wake) {
        drop(inner);
        wake_task_if_interruptible(task.clone());
    }
    Ok(())
}
