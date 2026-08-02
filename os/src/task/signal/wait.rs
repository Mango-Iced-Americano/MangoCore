//! 信号等待系统调用实现。
//!
//! `sigsuspend` 和 `sigtimedwait` 都临时改变或检查当前线程的信号状态，并通过
//! `WaitQueue` 进入可中断等待。等待条件检查必须在释放 `task.inner` 后执行信号
//! 动作判定，避免信号处理路径与任务锁出现锁顺序反转。

use crate::config::PAGE_SIZE;
use crate::mm::{UserPtr, UserPtrMut};
use crate::signal_type;
use crate::syscall::errno::*;
use crate::task::{current_task, current_user_token, TaskControlBlock, WaitQueue, WaitResult};
use crate::timer::{TimeSpec, NSEC_PER_SEC};

use super::{PendingSignal, SigHandler, SigInfo, Signals, SIG_DFL_IGNORE};

fn read_user_sigset(token: usize, set: *const Signals) -> Result<Signals, isize> {
    if (set as usize) < PAGE_SIZE {
        return Err(EFAULT);
    }
    let bits = UserPtr::new(set as *const u64).read(token)?;
    Ok(Signals::from_bits_truncate(bits as signal_type!()))
}

fn read_optional_user_timespec(
    token: usize,
    timeout: *const TimeSpec,
) -> Result<Option<TimeSpec>, isize> {
    if timeout.is_null() {
        Ok(None)
    } else if (timeout as usize) < PAGE_SIZE {
        Err(EFAULT)
    } else {
        UserPtr::new(timeout).read_optional(token)
    }
}

fn take_pending_signal_matching(task: &TaskControlBlock, set: Signals) -> Option<PendingSignal> {
    {
        let mut inner = task.acquire_inner_lock();
        let matching = inner.sigpending.pending() & set;
        if let Some(pending) = inner.sigpending.dequeue_matching(matching) {
            return Some(pending);
        }
    }
    // `task.inner` 已释放后再访问进程共享 pending 队列，避免嵌套获取任务锁。
    task.process.take_shared_matching(set)
}

fn remove_one_pending_signal(task: &TaskControlBlock, signal: Signals) {
    let mut inner = task.acquire_inner_lock();
    if inner.sigpending.contains(signal) {
        inner.sigpending.remove_signal(signal);
        return;
    }
    drop(inner);
    // 信号默认忽略时，需要同时清理共享 pending；不能持有 task.inner。
    task.process.take_shared_signal(signal);
}

fn take_sigtimedwait_interrupt(task: &TaskControlBlock, wait_set: Signals) -> bool {
    let (thread_pending, sigmask) = {
        let inner = task.acquire_inner_lock();
        (inner.sigpending.pending(), inner.sigmask)
    };
    let pending = (thread_pending | task.process.shared_pending())
        .difference(sigmask)
        .difference(wait_set);
    if pending.is_empty() {
        return false;
    }

    let sighand_ref = task.process.sighand();
    let sighand = sighand_ref.lock();
    for signum in 1..=64usize {
        let signal = match Signals::from_signum(signum) {
            Ok(signal) => signal,
            Err(_) => continue,
        };
        if !pending.contains(signal) {
            continue;
        }
        match sighand.get(signum) {
            Some(act) if act.handler == SigHandler::SIG_IGN => {
                drop(sighand);
                remove_one_pending_signal(task, signal);
                return false;
            }
            Some(_) => {
                return true;
            }
            None if SIG_DFL_IGNORE.contains(signal) => {
                drop(sighand);
                remove_one_pending_signal(task, signal);
                return false;
            }
            None => return true,
        }
    }
    false
}

/// 临时替换信号 mask 并等待任一可处理信号。
///
/// # Semantics
///
/// 成功的 `sigsuspend` 按 Linux 语义不会正常返回；被信号打断时返回
/// `-ERESTART`，由 syscall 返回路径转换为 `-EINTR`。旧 mask 保存在
/// `sigmask_to_restore`，由 `sigreturn` 恢复。
///
/// # Errors
///
/// `set` 指向的用户内存不可读时返回 `-EFAULT`。
pub fn sigsuspend(set: *const Signals) -> isize {
    let token = current_user_token();
    let new_mask = match read_user_sigset(token, set) {
        Ok(mask) => mask - Signals::CAN_NOT_BE_MASKED,
        Err(errno) => return errno,
    };
    {
        let task = current_task().unwrap();
        let mut inner = task.acquire_inner_lock();
        let old_mask = inner.sigmask;
        inner.sigmask = new_mask;
        inner.sigmask_to_restore = Some(old_mask);
    }

    let wait_queue = spin::Mutex::new(WaitQueue::new());
    match WaitQueue::wait_event_interruptible(&wait_queue, || None::<isize>) {
        WaitResult::Ready(value) => value,
        WaitResult::Interrupted => ERESTART,
        WaitResult::TimedOut => EAGAIN,
    }
}

/// 等待 `set` 中任一信号变为 pending。
///
/// # Semantics
///
/// 成功时返回信号编号，并在 `info` 非空时写回 `SigInfo`。若等待集合外出现
/// 可处理信号，返回 `-ERESTART` 交由 syscall 返回路径转为 `-EINTR` 或重启。
///
/// # Errors
///
/// - `-EFAULT`：`set`、`timeout` 或 `info` 指向的用户内存不可访问。
/// - `-EINVAL`：`timeout.tv_nsec >= 1s` 或秒字段溢出。
/// - `-EAGAIN`：超时到达且没有匹配信号。
///
/// # Locking
///
/// 条件闭包只短暂获取 `task.inner` 和进程共享 signal lock，并且可能由
/// `WaitQueue` 在自己的锁内调用，因此闭包不得访问用户内存。成功领取的信号先
/// 保存在 syscall 栈上，等待路径完全退出后才写用户态 `info`。
pub fn sigtimedwait(set: *const Signals, info: *mut SigInfo, timeout: *const TimeSpec) -> isize {
    let token = current_user_token();
    let set = match read_user_sigset(token, set) {
        Ok(set) => set - Signals::CAN_NOT_BE_MASKED,
        Err(errno) => return errno,
    };
    let timeout = match read_optional_user_timespec(token, timeout) {
        Ok(timeout) => timeout,
        Err(errno) => return errno,
    };
    if let Some(timeout) = timeout {
        if timeout.tv_sec > isize::MAX as usize || timeout.tv_nsec >= NSEC_PER_SEC {
            return EINVAL;
        }
    }
    let start = TimeSpec::now();
    let deadline = timeout.map(|timeout| start + timeout);

    let wait_queue = spin::Mutex::new(WaitQueue::new());
    let task = current_task().unwrap();
    {
        let mut inner = task.acquire_inner_lock();
        inner.signal_wait_mask = set;
    }
    let mut received = None;
    let mut wait_condition = || -> Option<isize> {
        if let Some(pending) = take_pending_signal_matching(&task, set) {
            let signum = pending.signum() as isize;
            // dequeue 已在 signal owner 锁内完成；这里只把唯一领取结果交给
            // syscall 栈，不能在 WaitQueue 条件锁内触发用户缺页。
            received = Some(pending);
            return Some(signum);
        }
        if take_sigtimedwait_interrupt(&task, set) {
            return Some(ERESTART);
        }
        None
    };

    let mut wait_result = if let Some(deadline) = deadline {
        WaitQueue::wait_event_interruptible_timeout(&wait_queue, &mut wait_condition, deadline)
    } else {
        WaitQueue::wait_event_interruptible(&wait_queue, &mut wait_condition)
    };
    // WaitQueue 的 Interrupted 只说明“不应继续睡眠”，不拥有 signal。Linux
    // do_sigtimedwait 同样会在每次睡眠结束后重新 dequeue，使已经 pending 的
    // waited signal 优先于 EINTR/EAGAIN。先销毁闭包，释放它对 received 的借用。
    drop(wait_condition);
    if !matches!(wait_result, WaitResult::Ready(_)) {
        if let Some(pending) = take_pending_signal_matching(&task, set) {
            let signum = pending.signum() as isize;
            received = Some(pending);
            wait_result = WaitResult::Ready(signum);
        }
    }
    {
        let mut inner = task.acquire_inner_lock();
        inner.signal_wait_mask = Signals::empty();
    }
    match wait_result {
        WaitResult::Ready(value) => {
            if let Some(pending) = received {
                if !info.is_null()
                    && UserPtrMut::new(info)
                        .write(token, &pending.siginfo)
                        .is_err()
                {
                    log::error!("[sys_sigtimedwait] Error copying to info {:?} ", info);
                    return EFAULT;
                }
            }
            value
        }
        WaitResult::Interrupted => ERESTART,
        WaitResult::TimedOut => EAGAIN,
    }
}
