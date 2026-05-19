use alloc::sync::Arc;

use crate::signal_type;
use crate::config::PAGE_SIZE;
use crate::mm::{UserPtr, UserPtrMut};
use crate::syscall::errno::*;
use crate::task::{
    block_current_and_run_next_checked, current_task, wait_with_timeout, TaskControlBlock,
};
use crate::timer::TimeSpec;

use super::{
    has_actionable_signal, PendingSignal, SigHandler, SigInfo, Signals, SIG_DFL_IGNORE,
};

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
    task.process.take_shared_matching(set)
}

fn remove_one_pending_signal(task: &TaskControlBlock, signal: Signals) {
    let mut inner = task.acquire_inner_lock();
    if inner.sigpending.contains(signal) {
        inner.sigpending.remove_signal(signal);
        return;
    }
    drop(inner);
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

    let sighand = task.sighand.lock();
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

fn has_sigtimedwait_wakeup(task: &TaskControlBlock, wait_set: Signals) -> bool {
    let (thread_pending, sigmask) = {
        let inner = task.acquire_inner_lock();
        (inner.sigpending.pending(), inner.sigmask)
    };
    let pending = thread_pending | task.process.shared_pending();
    !(pending & wait_set).is_empty()
        || !pending.difference(sigmask).difference(wait_set).is_empty()
}

pub fn sigsuspend(set: *const Signals) -> isize {
    let mut task = current_task().unwrap();
    let token = task.get_user_token();
    let new_mask = match read_user_sigset(token, set) {
        Ok(mask) => {
            mask - Signals::CAN_NOT_BE_MASKED
        }
        Err(errno) => return errno,
    };
    {
        let mut inner = task.acquire_inner_lock();
        let old_mask = inner.sigmask;
        inner.sigmask = new_mask;
        inner.sigmask_to_restore = Some(old_mask);
    }

    loop {
        if has_actionable_signal(&task) {
            return ERESTART;
        }
        drop(task);
        block_current_and_run_next_checked(|task| !has_actionable_signal(task));
        task = current_task().unwrap();
    }
}

pub fn sigtimedwait(set: *const Signals, info: *mut SigInfo, timeout: *const TimeSpec) -> isize {
    let mut task = current_task().unwrap();
    let token = task.get_user_token();
    let set = match read_user_sigset(token, set) {
        Ok(set) => set,
        Err(errno) => return errno,
    };
    let timeout = match read_optional_user_timespec(token, timeout) {
        Ok(timeout) => timeout,
        Err(errno) => return errno,
    };
    let start = TimeSpec::now();
    let deadline = timeout.map(|timeout| start + timeout);

    loop {
        if let Some(pending) = take_pending_signal_matching(&task, set) {
            if !info.is_null() {
                if UserPtrMut::new(info)
                    .write(token, &pending.siginfo)
                    .is_err()
                {
                    log::error!("[sys_sigtimedwait] Error copying to info {:?} ", info);
                    return EFAULT;
                };
            }
            return pending.signum() as isize;
        }
        if take_sigtimedwait_interrupt(&task, set) {
            return EINTR;
        }

        if let Some(deadline) = deadline {
            if deadline <= TimeSpec::now() {
                return EAGAIN;
            }
            wait_with_timeout(Arc::downgrade(&task), deadline);
        }

        drop(task);
        block_current_and_run_next_checked(|task| {
            !has_sigtimedwait_wakeup(task, set)
                && deadline
                    .map(|deadline| TimeSpec::now() < deadline)
                    .unwrap_or(true)
        });
        task = current_task().unwrap();
    }
}
