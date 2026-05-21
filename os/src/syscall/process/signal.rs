use crate::hal::{MachineContext, TrapContext, UserSignalMask};
use crate::mm::{copy_from_user, UserPtr, UserPtrMut};
use crate::syscall::errno::*;
use crate::task::{current_task, exit_current_and_run_next, signal::*, ProcessManager};
use crate::timer::TimeSpec;
use crate::utils::error::SyscallErr;
use core::mem::size_of;
use log::{error, info, trace};

pub fn sys_kill(pid: usize, sig: usize) -> isize {
    let signal = match Signals::from_signum(sig) {
        Ok(signal) => signal,
        Err(_) => return EINVAL,
    };
    let pid_signed = pid as isize;
    if pid_signed > 0 {
        ProcessManager::send_signal_to_process(pid, signal)
    } else if pid_signed == 0 {
        ProcessManager::send_signal_to_current_group(signal)
    } else if pid_signed == -1 {
        ProcessManager::send_signal_to_all(signal)
    } else {
        let pgid = (-pid_signed) as usize;
        ProcessManager::send_signal_to_group(pgid, signal)
    }
}

pub fn sys_tkill(tid: usize, sig: usize) -> isize {
    if tid == 0 || (tid as isize) < 0 {
        return EINVAL;
    }
    let signal = match Signals::from_signum(sig) {
        Ok(signal) => signal,
        Err(_) => return EINVAL,
    };
    if let Some(task) = ProcessManager::find_task(tid) {
        send_thread_signal(&task, signal);
        SUCCESS
    } else {
        ESRCH
    }
}

pub fn sys_tgkill(pid: usize, tid: usize, sig: usize) -> isize {
    if pid == 0 || tid == 0 || (pid as isize) < 0 || (tid as isize) < 0 {
        return EINVAL;
    }
    let signal = match Signals::from_signum(sig) {
        Ok(signal) => signal,
        Err(_) => return EINVAL,
    };
    if let Some(task) = ProcessManager::find_task_in_process(pid, tid) {
        send_thread_signal(&task, signal);
        SUCCESS
    } else {
        ESRCH
    }
}

pub fn sys_sigaction(signum: usize, act: usize, oldact: usize) -> isize {
    trace!(
        "[sys_sigaction] signum: {:?}, act: {:X}, oldact: {:X}",
        signum,
        act,
        oldact
    );
    sigaction(signum, act as *const SigAction, oldact as *mut SigAction)
}

/// Note: code translation should be done in syscall rather than the call handler as the handler may be reused by kernel code which use kernel structs
pub fn sys_sigprocmask(how: u32, set: usize, oldset: usize, sigsetsize: usize) -> isize {
    if !valid_rt_sigset_size(sigsetsize) {
        return EINVAL;
    }
    info!(
        "[sys_sigprocmask] how: {:?}; set: {:X}, oldset: {:X}, sigsetsize: {}",
        how, set, oldset, sigsetsize
    );
    sigprocmask(how, set as *const Signals, oldset as *mut Signals)
}

fn valid_rt_sigset_size(sigsetsize: usize) -> bool {
    sigsetsize >= size_of::<u64>()
}

/// rt_sigpending(sigset_t *set, size_t sigsetsize)
/// Copy the set of pending signals to user-space `set`.
/// Only the low 64 signal bits are implemented; libc may pass a larger
/// sigset_t storage size on some architectures.
pub fn sys_rt_sigpending(set: usize, sigsetsize: usize) -> isize {
    if !valid_rt_sigset_size(sigsetsize) {
        return -(SyscallErr::EINVAL as isize);
    }
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let inner = task.acquire_inner_lock();
    let pending = inner.sigpending.pending() | task.process.shared_pending();
    let pending_bits = pending.bits() as u64;
    trace!(
        "[sys_rt_sigpending] tid: {}, pid: {}, pending: {:?}",
        task.tid.0,
        task.pid(),
        pending
    );
    match UserPtrMut::from_addr(set).write(token, &pending_bits) {
        Ok(()) => SUCCESS,
        Err(errno) => errno,
    }
}

pub fn sys_sigtimedwait(set: usize, info: usize, timeout: usize, sigsetsize: usize) -> isize {
    if !valid_rt_sigset_size(sigsetsize) {
        return EINVAL;
    }
    sigtimedwait(
        set as *const Signals,
        info as *mut SigInfo,
        timeout as *const TimeSpec,
    )
}

pub fn sys_rt_sigsuspend(set: usize, sigsetsize: usize) -> isize {
    if !valid_rt_sigset_size(sigsetsize) {
        return EINVAL;
    }
    sigsuspend(set as *const Signals)
}

pub fn sys_sigaltstack(ss: usize, old_ss: usize) -> isize {
    sigaltstack(ss as *const SignalStack, old_ss as *mut SignalStack)
}

pub fn sys_sigreturn() -> isize {
    // mark not processing signal handler
    let task = current_task().unwrap();
    let mut inner = task.acquire_inner_lock();
    let token = task.get_user_token();
    info!("[sys_sigreturn] tid: {}, pid: {}", task.tid.0, task.pid());

    let sp = inner.get_trap_cx().gp.sp;
    // restore sigmask & trap context
    let ucontext_addr = match sp
        .checked_add(size_of::<SigInfo>())
        .and_then(|addr| addr.checked_add(0x7))
    {
        Some(addr) => addr & !0x7,
        None => {
            error!("[sys_sigreturn] invalid signal frame address, send SIGSEGV");
            drop(inner);
            drop(task);
            exit_current_and_run_next(Signals::SIGSEGV.to_signum().unwrap() as u32);
        }
    };
    let sigmask_addr = match ucontext_addr
        .checked_add(2 * size_of::<usize>())
        .and_then(|addr| addr.checked_add(size_of::<SignalStack>()))
    {
        Some(addr) => addr,
        None => {
            error!("[sys_sigreturn] invalid sigmask address, send SIGSEGV");
            drop(inner);
            drop(task);
            exit_current_and_run_next(Signals::SIGSEGV.to_signum().unwrap() as u32);
        }
    };
    let mcontext_addr = match sigmask_addr
        .checked_add(size_of::<UserSignalMask>())
        .and_then(|addr| addr.checked_add(crate::hal::UserContext::PADDING_SIZE))
    {
        Some(addr) => addr,
        None => {
            error!("[sys_sigreturn] invalid machine context address, send SIGSEGV");
            drop(inner);
            drop(task);
            exit_current_and_run_next(Signals::SIGSEGV.to_signum().unwrap() as u32);
        }
    };
    let restored_sigmask = match UserPtr::<UserSignalMask>::from_addr(sigmask_addr).read(token) {
        Ok(sigmask) => sigmask.to_signals() - Signals::CAN_NOT_BE_MASKED,
        Err(_) => {
            error!("[sys_sigreturn] bad sigmask in signal frame, send SIGSEGV");
            drop(inner);
            drop(task);
            exit_current_and_run_next(Signals::SIGSEGV.to_signum().unwrap() as u32);
        }
    };
    let trap_cx_ptr = inner.get_trap_cx() as *mut TrapContext;
    if copy_from_user(
        token,
        mcontext_addr as *mut MachineContext,
        trap_cx_ptr.cast::<MachineContext>(),
    )
    .is_err()
    {
        error!("[sys_sigreturn] bad machine context in signal frame, send SIGSEGV");
        drop(inner);
        drop(task);
        exit_current_and_run_next(Signals::SIGSEGV.to_signum().unwrap() as u32);
    }
    inner.sigmask = restored_sigmask;
    inner.get_trap_cx().gp.a0 as isize // return a0: not modify any of trap_cx
}
