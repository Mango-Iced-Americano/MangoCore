use alloc::sync::Arc;

use crate::fs::{
    pidfd::{new_pidfd_file_with_flags, PidFd},
    procfs::LockedProcInode,
    vfs::{File, FileFlags, FileType, IndexNode, MountFSInode},
};
use crate::hal::{MachineContext, TrapContext, UserSignalMask};
use crate::mm::{copy_from_user, UserPtr, UserPtrMut};
use crate::syscall::errno::*;
use crate::task::{
    current_task, exit_current_and_run_next, signal::*, ProcessControlBlock, ProcessManager,
};
use crate::timer::TimeSpec;
use crate::utils::error::SyscallErr;
use core::mem::size_of;
use log::{error, info, trace};

fn can_signal_process(target: &ProcessControlBlock) -> bool {
    let Some(sender) = current_task() else {
        return false;
    };
    if sender.pid() == target.pid {
        return true;
    }
    let sender_inner = sender.acquire_inner_lock();
    let sender_uid = sender_inner.uid;
    let sender_euid = sender_inner.euid;
    drop(sender_inner);

    if sender_euid == 0 {
        return true;
    }

    let Some(target_task) = target.any_live_thread() else {
        return true;
    };
    let target_inner = target_task.acquire_inner_lock();
    sender_uid == target_inner.uid
        || sender_uid == target_inner.suid
        || sender_euid == target_inner.uid
        || sender_euid == target_inner.suid
}

pub(super) fn pidfd_file_target_pid(file: &File) -> Result<usize, isize> {
    let inode = MountFSInode::unwrap_inode(&file.inode);
    if let Some(pidfd) = inode.as_any_ref().downcast_ref::<PidFd>() {
        return Ok(pidfd.target_pid());
    }
    if let Some(proc_inode) = inode.as_any_ref().downcast_ref::<LockedProcInode>() {
        let data = proc_inode.0.lock();
        if data.metadata.file_type == FileType::Dir && data.extra_data != 0 {
            return Ok(data.extra_data);
        }
    }
    Err(EBADF)
}

fn pidfd_target_pid(pidfd: usize) -> Result<usize, isize> {
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = fd_table.get_file(pidfd).map_err(|err| -(err as isize))?;
    pidfd_file_target_pid(file)
}

pub fn sys_kill(pid: usize, sig: usize) -> isize {
    let signal = match Signals::from_signum(sig) {
        Ok(signal) => signal,
        Err(_) => return EINVAL,
    };
    let pid_signed = pid as isize;
    if pid_signed > 0 {
        let Some(process) = ProcessManager::find_process(pid) else {
            return ESRCH;
        };
        if !can_signal_process(&process) {
            return EPERM;
        }
        send_process_signal(&process, signal);
        SUCCESS
    } else if pid_signed == 0 {
        ProcessManager::send_signal_to_current_group(signal)
    } else if pid_signed == -1 {
        ProcessManager::send_signal_to_all(signal)
    } else {
        let pgid = (-pid_signed) as usize;
        ProcessManager::send_signal_to_group(pgid, signal)
    }
}

pub fn sys_pidfd_open(pid: usize, flags: usize) -> isize {
    const PIDFD_NONBLOCK: usize = FileFlags::O_NONBLOCK.bits() as usize;

    if pid == 0 || (pid as isize) < 0 {
        return EINVAL;
    }
    if flags & !PIDFD_NONBLOCK != 0 {
        return EINVAL;
    }
    if ProcessManager::find_process(pid).is_none() {
        return ESRCH;
    }

    let mut file_flags = FileFlags::O_RDWR;
    if flags & PIDFD_NONBLOCK != 0 {
        file_flags.insert(FileFlags::O_NONBLOCK);
    }
    let file = match new_pidfd_file_with_flags(pid, file_flags) {
        Ok(file) => file,
        Err(err) => return -(err as isize),
    };

    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let mut fd_table = files_ref.lock();
    match fd_table.alloc_fd(file, true) {
        Ok(fd) => fd as isize,
        Err(err) => -(err as isize),
    }
}

pub fn sys_pidfd_getfd(pidfd: usize, targetfd: usize, flags: usize) -> isize {
    if flags != 0 {
        return EINVAL;
    }

    let target_pid = match pidfd_target_pid(pidfd) {
        Ok(pid) => pid,
        Err(errno) => return errno,
    };
    let Some(process) = ProcessManager::find_process(target_pid) else {
        return ESRCH;
    };
    if process.is_zombie() {
        return ESRCH;
    }
    if !can_signal_process(&process) {
        return EPERM;
    }

    let remote_file = {
        let files_ref = process.files();
        let fd_table = files_ref.lock();
        let file = match fd_table.get_file(targetfd) {
            Ok(file) => file,
            Err(err) => return -(err as isize),
        };
        match file.try_clone() {
            Some(file) => file,
            None => return ENOMEM,
        }
    };

    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let mut fd_table = files_ref.lock();
    match fd_table.alloc_fd(remote_file, true) {
        Ok(fd) => fd as isize,
        Err(err) => -(err as isize),
    }
}

pub fn sys_kcmp(pid1: usize, pid2: usize, kcmp_type: usize, idx1: usize, idx2: usize) -> isize {
    const KCMP_FILE: usize = 0;

    if kcmp_type != KCMP_FILE {
        return EINVAL;
    }

    let Some(process1) = ProcessManager::find_process(pid1) else {
        return ESRCH;
    };
    let Some(process2) = ProcessManager::find_process(pid2) else {
        return ESRCH;
    };

    let inode1: Arc<dyn IndexNode> = {
        let files_ref = process1.files();
        let fd_table = files_ref.lock();
        match fd_table.get_file(idx1) {
            Ok(file) => file.inode.clone(),
            Err(err) => return -(err as isize),
        }
    };
    let inode2: Arc<dyn IndexNode> = {
        let files_ref = process2.files();
        let fd_table = files_ref.lock();
        match fd_table.get_file(idx2) {
            Ok(file) => file.inode.clone(),
            Err(err) => return -(err as isize),
        }
    };

    if Arc::ptr_eq(&inode1, &inode2) {
        0
    } else {
        1
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
        match send_thread_signal(&task, signal) {
            Ok(()) => SUCCESS,
            Err(err) => err,
        }
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
        match send_thread_signal(&task, signal) {
            Ok(()) => SUCCESS,
            Err(err) => err,
        }
    } else {
        ESRCH
    }
}

pub fn sys_pidfd_send_signal(pidfd: usize, sig: usize, info: usize, flags: usize) -> isize {
    if flags != 0 {
        return EINVAL;
    }
    let signal = match Signals::from_signum(sig) {
        Ok(signal) => signal,
        Err(_) => return EINVAL,
    };

    let task = current_task().unwrap();
    let token = task.get_user_token();
    let queued_siginfo = if info != 0 {
        match UserPtr::<SigInfo>::from_addr(info).read(token) {
            Ok(siginfo) => {
                if siginfo.signo() != sig {
                    return EINVAL;
                }
                Some(siginfo)
            }
            Err(_) => return EFAULT,
        }
    } else {
        None
    };

    let target_pid = match pidfd_target_pid(pidfd) {
        Ok(pid) => pid,
        Err(errno) => return errno,
    };

    let Some(process) = ProcessManager::find_process(target_pid) else {
        return ESRCH;
    };
    if !can_signal_process(&process) {
        return EPERM;
    }
    if signal.is_empty() {
        return SUCCESS;
    }
    match queued_siginfo {
        Some(siginfo) => {
            if target_pid != task.pid() && siginfo.is_kernel_generated() {
                return EPERM;
            }
            send_process_signal_info(&process, signal, siginfo.with_signal_sender(sig, task.pid()));
            SUCCESS
        }
        None => ProcessManager::send_signal_to_process(target_pid, signal),
    }
}

pub fn sys_sigaction(signum: usize, act: usize, oldact: usize) -> isize {
    trace!(
        "[sys_sigaction] signum: {:?}, act: {:X}, oldact: {:X}",
        signum,
        act,
        oldact
    );
    sigaction(
        signum,
        act as *const UserSigAction,
        oldact as *mut UserSigAction,
    )
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

pub fn sys_rt_sigqueueinfo(pid: usize, sig: usize, info: usize) -> isize {
    let signal = match Signals::from_signum(sig) {
        Ok(signal) => signal,
        Err(_) => return EINVAL,
    };

    let task = current_task().unwrap();
    let siginfo = match UserPtr::<SigInfo>::from_addr(info).read(task.get_user_token()) {
        Ok(siginfo) => siginfo,
        Err(_) => return EFAULT,
    };
    if siginfo.signo() != 0 && siginfo.signo() != sig {
        return EINVAL;
    }

    let target_task = ProcessManager::find_task(pid);
    let process = match ProcessManager::find_process(pid) {
        Some(process) => process,
        None => match &target_task {
            Some(target_task) => target_task.process.clone(),
            None => return ESRCH,
        },
    };
    if !can_signal_process(&process) {
        return EPERM;
    }
    if signal.is_empty() {
        return SUCCESS;
    }
    if pid != task.pid() && siginfo.is_kernel_generated() {
        return EPERM;
    }

    let siginfo = siginfo.with_signal_sender(sig, task.pid());
    if let Some(target_task) = target_task {
        if target_task.gettid() == pid && target_task.pid() != pid {
            return match send_thread_signal_info_deferred(&target_task, signal, siginfo) {
                Ok(()) => SUCCESS,
                Err(errno) => errno,
            };
        }
    }

    send_process_signal_info(&process, signal, siginfo);
    SUCCESS
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
