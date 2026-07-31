use alloc::sync::Arc;

use crate::fs::{
    dev::DEV_FS,
    pidfd::{new_pidfd_file_with_flags, PidFd},
    procfs::LockedProcInode,
    vfs::{
        event::EPollEvent, File, FileFlags, FilePrivateData, FileSystem, FileType, IndexNode,
        InodeMode, Metadata, MountFSInode,
    },
};
use crate::hal::{MachineContext, TrapContext, UserSignalMask};
use crate::mm::{copy_from_user, UserPtr, UserPtrMut};
use crate::signal_type;
use crate::syscall::errno::*;
use crate::task::{
    current_euid, current_syscall_name, current_task, current_uid, current_user_token,
    exit_current_and_run_next, signal::*, ProcessControlBlock, ProcessManager, TaskControlBlock,
};
use crate::timer::TimeSpec;
use crate::utils::error::SyscallErr;
use core::any::Any;
use core::mem::size_of;
use log::error;
use spin::{Mutex, MutexGuard};

fn can_signal_process(target: &ProcessControlBlock) -> bool {
    let Some(sender) = current_task() else {
        return false;
    };
    if sender.pid() == target.pid {
        return true;
    }
    let sender_uid = current_uid();
    let sender_euid = current_euid();

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
        return pidfd.target_pid().map_err(|err| -(err as isize));
    }
    if let Some(proc_inode) = inode.as_any_ref().downcast_ref::<LockedProcInode>() {
        let (file_type, pid, process_ref) = {
            let data = proc_inode.0.lock();
            (
                data.metadata.file_type,
                data.extra_data,
                data.process_ref.clone(),
            )
        };
        if file_type == FileType::Dir && pid != 0 {
            if let Some(process_ref) = process_ref {
                return match process_ref.upgrade() {
                    Some(process) if process.pid == pid && !process.pid_released() => Ok(pid),
                    _ => Err(ESRCH),
                };
            }
            return Ok(pid);
        }
    }
    Err(EBADF)
}

fn pidfd_target_pid(pidfd: usize) -> Result<usize, isize> {
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = fd_table.get_file(pidfd).map_err(|err| -(err as isize))?;
    pidfd_file_target_pid(&*file)
}

const SFD_NONBLOCK: usize = FileFlags::O_NONBLOCK.bits() as usize;
const SFD_CLOEXEC: usize = FileFlags::O_CLOEXEC.bits() as usize;
const SFD_VALID_FLAGS: usize = SFD_NONBLOCK | SFD_CLOEXEC;

#[derive(Clone, Copy)]
#[repr(C)]
struct SignalfdSiginfo {
    ssi_signo: u32,
    ssi_errno: i32,
    ssi_code: i32,
    ssi_pid: u32,
    ssi_uid: u32,
    ssi_fd: i32,
    ssi_tid: u32,
    ssi_band: u32,
    ssi_overrun: u32,
    ssi_trapno: u32,
    ssi_status: i32,
    ssi_int: i32,
    ssi_ptr: u64,
    ssi_utime: u64,
    ssi_stime: u64,
    ssi_addr: u64,
    ssi_addr_lsb: u16,
    __pad2: u16,
    ssi_syscall: i32,
    ssi_call_addr: u64,
    ssi_arch: u32,
    __pad: [u8; 28],
}

impl SignalfdSiginfo {
    fn from_siginfo(info: SigInfo) -> Self {
        Self {
            ssi_signo: info.signo() as u32,
            ssi_errno: info.errno(),
            ssi_code: info.code(),
            ssi_pid: info.sender_pid(),
            ssi_uid: info.sender_uid(),
            ssi_fd: 0,
            ssi_tid: 0,
            ssi_band: 0,
            ssi_overrun: 0,
            ssi_trapno: 0,
            ssi_status: 0,
            ssi_int: info.value() as i32,
            ssi_ptr: info.value() as u64,
            ssi_utime: 0,
            ssi_stime: 0,
            ssi_addr: 0,
            ssi_addr_lsb: 0,
            __pad2: 0,
            ssi_syscall: 0,
            ssi_call_addr: 0,
            ssi_arch: 0,
            __pad: [0; 28],
        }
    }

    fn bytes(&self) -> &[u8] {
        unsafe {
            core::slice::from_raw_parts(
                (self as *const SignalfdSiginfo) as *const u8,
                size_of::<SignalfdSiginfo>(),
            )
        }
    }
}

struct SignalFd {
    mask: Mutex<Signals>,
    metadata: Metadata,
}

impl core::fmt::Debug for SignalFd {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SignalFd")
            .field("mask", &self.mask.lock().bits())
            .finish()
    }
}

impl SignalFd {
    fn new(mask: Signals) -> Self {
        Self {
            mask: Mutex::new(mask),
            metadata: Metadata::new(
                FileType::File,
                InodeMode::S_IFREG | InodeMode::from_bits_truncate(0o600),
            ),
        }
    }

    fn set_mask(&self, mask: Signals) {
        *self.mask.lock() = mask;
    }

    fn pending_mask(&self) -> Signals {
        *self.mask.lock()
    }
}

impl IndexNode for SignalFd {
    fn read_at(
        &self,
        _offset: usize,
        len: usize,
        buf: &mut [u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        let info_size = size_of::<SignalfdSiginfo>();
        if len < info_size || buf.len() < info_size {
            return Err(SyscallErr::EINVAL);
        }

        let count = core::cmp::min(len, buf.len()) / info_size;
        let task = current_task().ok_or(SyscallErr::ESRCH)?;
        let mask = self.pending_mask();
        let mut written = 0usize;
        for slot in 0..count {
            let Some(pending) = take_pending_signal_matching(&task, mask) else {
                break;
            };
            let info = SignalfdSiginfo::from_siginfo(pending.siginfo);
            let start = slot * info_size;
            buf[start..start + info_size].copy_from_slice(info.bytes());
            written += info_size;
        }

        if written == 0 {
            Err(SyscallErr::EAGAIN)
        } else {
            Ok(written)
        }
    }

    fn write_at(
        &self,
        _offset: usize,
        _len: usize,
        _buf: &[u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        Err(SyscallErr::EINVAL)
    }

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        Ok(self.metadata.clone())
    }

    fn poll(&self, _private_data: &FilePrivateData) -> Result<usize, SyscallErr> {
        let task = current_task().ok_or(SyscallErr::ESRCH)?;
        if has_pending_signal_matching(&task, self.pending_mask()) {
            Ok((EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM).bits())
        } else {
            Ok(0)
        }
    }

    fn fs(&self) -> Arc<dyn FileSystem> {
        DEV_FS.clone()
    }

    fn is_stream(&self) -> bool {
        true
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}

fn read_signalfd_mask(token: usize, mask: usize, sigsetsize: usize) -> Result<Signals, isize> {
    if !valid_rt_sigset_size(sigsetsize) {
        return Err(EINVAL);
    }
    if mask == 0 {
        return Err(EFAULT);
    }
    let bits = UserPtr::<u64>::from_addr(mask).read(token)?;
    Ok(Signals::from_bits_truncate(bits as signal_type!()))
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

fn has_pending_signal_matching(task: &TaskControlBlock, set: Signals) -> bool {
    let thread_pending = task.acquire_inner_lock().sigpending.pending();
    (thread_pending | task.process.shared_pending()).intersects(set)
}

pub fn sys_signalfd4(fd: usize, mask: usize, sigsetsize: usize, flags: usize) -> isize {
    if flags & !SFD_VALID_FLAGS != 0 {
        return EINVAL;
    }

    let (token, files_ref) = {
        let task = current_task().unwrap();
        (current_user_token(), task.process.files())
    };
    let sigmask = match read_signalfd_mask(token, mask, sigsetsize) {
        Ok(mask) => mask,
        Err(errno) => return errno,
    };

    let fd_signed = fd as isize;
    if fd_signed == -1 {
        let mut file_flags = FileFlags::O_RDWR;
        if flags & SFD_NONBLOCK != 0 {
            file_flags.insert(FileFlags::O_NONBLOCK);
        }
        if flags & SFD_CLOEXEC != 0 {
            file_flags.insert(FileFlags::O_CLOEXEC);
        }

        let inode = Arc::new(SignalFd::new(sigmask)) as Arc<dyn IndexNode>;
        let file = match File::new(inode, file_flags) {
            Ok(file) => file,
            Err(err) => return -(err as isize),
        };
        let mut fd_table = files_ref.lock();
        return match fd_table.alloc_fd(file, flags & SFD_CLOEXEC != 0) {
            Ok(new_fd) => new_fd as isize,
            Err(err) => -(err as isize),
        };
    }

    if fd_signed < 0 {
        return EBADF;
    }

    let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(err) => return -(err as isize),
    };
    let inode = MountFSInode::unwrap_inode(&file.inode);
    if let Some(signalfd) = inode.as_any_ref().downcast_ref::<SignalFd>() {
        signalfd.set_mask(sigmask);
        fd as isize
    } else {
        EINVAL
    }
}

pub fn sys_kill(pid: usize, sig: usize) -> isize {
    let signal = match Signals::from_signum(sig) {
        Ok(signal) => signal,
        Err(_) => return EINVAL,
    };
    if signal.contains(Signals::SIGKILL) {
        let (sender_tid, sender_pid) = current_task()
            .map(|task| (task.gettid(), task.pid()))
            .unwrap_or((0, 0));
        log::warn!(
            "[sigkill_diag] sys_kill sender tid={} pid={} syscall={} target_raw={}",
            sender_tid,
            sender_pid,
            current_syscall_name(),
            pid as isize
        );
    }
    let pid_signed = pid as isize;
    if pid_signed > 0 {
        if let Some(task_ref) = current_task() {
            if task_ref.pid() == pid && task_ref.process.live_thread_count() == 1 {
                send_process_signal_to_current_task(&task_ref.process, signal);
                return SUCCESS;
            }
        }
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
    let Some(process) = ProcessManager::find_process(pid) else {
        return ESRCH;
    };

    let mut file_flags = FileFlags::O_RDWR;
    if flags & PIDFD_NONBLOCK != 0 {
        file_flags.insert(FileFlags::O_NONBLOCK);
    }
    let file = match new_pidfd_file_with_flags(&process, file_flags) {
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
        file
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
    const KCMP_VM: usize = 1;
    const KCMP_FILES: usize = 2;
    const KCMP_FS: usize = 3;
    const KCMP_SIGHAND: usize = 4;
    const KCMP_IO: usize = 5;
    const KCMP_SYSVSEM: usize = 6;

    let Some(process1) = ProcessManager::find_process(pid1) else {
        return ESRCH;
    };
    let Some(process2) = ProcessManager::find_process(pid2) else {
        return ESRCH;
    };

    match kcmp_type {
        KCMP_FILE => {
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
        KCMP_VM => {
            let vm1 = process1.vm();
            let vm2 = process2.vm();
            if Arc::ptr_eq(&vm1, &vm2) {
                0
            } else {
                1
            }
        }
        KCMP_FILES => {
            let files1 = process1.files();
            let files2 = process2.files();
            if Arc::ptr_eq(&files1, &files2) {
                0
            } else {
                1
            }
        }
        KCMP_FS => {
            let fs1 = process1.fs();
            let fs2 = process2.fs();
            if Arc::ptr_eq(&fs1, &fs2) {
                0
            } else {
                1
            }
        }
        KCMP_SIGHAND => {
            let sighand1 = process1.sighand();
            let sighand2 = process2.sighand();
            if Arc::ptr_eq(&sighand1, &sighand2) {
                0
            } else {
                1
            }
        }
        KCMP_IO | KCMP_SYSVSEM => 0,
        _ => EINVAL,
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
    if signal.contains(Signals::SIGKILL) {
        let (sender_tid, sender_pid) = current_task()
            .map(|task| (task.gettid(), task.pid()))
            .unwrap_or((0, 0));
        log::warn!(
            "[sigkill_diag] sys_tkill sender tid={} pid={} syscall={} target_tid={}",
            sender_tid,
            sender_pid,
            current_syscall_name(),
            tid
        );
    }
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
    if signal.contains(Signals::SIGKILL) {
        let (sender_tid, sender_pid) = current_task()
            .map(|task| (task.gettid(), task.pid()))
            .unwrap_or((0, 0));
        log::warn!(
            "[sigkill_diag] sys_tgkill sender tid={} pid={} syscall={} target_pid={} target_tid={}",
            sender_tid,
            sender_pid,
            current_syscall_name(),
            pid,
            tid
        );
    }
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
    let token = current_user_token();
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
    if signal.contains(Signals::SIGKILL) {
        log::warn!(
            "[sigkill_diag] pidfd_send_signal sender tid={} pid={} syscall={} target_pid={}",
            task.gettid(),
            task.pid(),
            current_syscall_name(),
            target_pid
        );
    }

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
            send_process_signal_info(
                &process,
                signal,
                siginfo.with_signal_sender(sig, task.pid()),
            );
            SUCCESS
        }
        None => ProcessManager::send_signal_to_process(target_pid, signal),
    }
}

pub fn sys_sigaction(signum: usize, act: usize, oldact: usize, sigsetsize: usize) -> isize {
    if !valid_rt_sigaction_size(sigsetsize) {
        return EINVAL;
    }
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
    sigprocmask(how, set as *const Signals, oldset as *mut Signals)
}

fn valid_rt_sigset_size(sigsetsize: usize) -> bool {
    sigsetsize >= size_of::<u64>()
}

fn valid_rt_sigaction_size(sigsetsize: usize) -> bool {
    sigsetsize == size_of::<u64>()
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
    let token = current_user_token();
    let pending = {
        let inner = task.acquire_inner_lock();
        inner.sigpending.pending() | task.process.shared_pending()
    };
    let pending_bits = pending.bits() as u64;
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
    let siginfo = match UserPtr::<SigInfo>::from_addr(info).read(current_user_token()) {
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
    let token = current_user_token();
    let mut inner = task.acquire_inner_lock();

    let sp = inner.trap_context_mut().gp.sp;
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
    let mcontext_addr = match ucontext_addr.checked_add(crate::hal::UserContext::MCONTEXT_OFFSET) {
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
    #[cfg(feature = "loongarch64")]
    let restored_lsx = match ucontext_addr
        .checked_add(crate::hal::UserContext::LSX_OFFSET)
        .and_then(|addr| UserPtr::<crate::hal::LsxRegs>::from_addr(addr).read(token).ok())
    {
        Some(lsx) => lsx,
        None => {
            error!("[sys_sigreturn] bad LSX context in signal frame, send SIGSEGV");
            drop(inner);
            drop(task);
            exit_current_and_run_next(Signals::SIGSEGV.to_signum().unwrap() as u32);
        }
    };
    let trap_cx_ptr = inner.trap_context_mut() as *mut TrapContext;
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
    #[cfg(feature = "loongarch64")]
    {
        let trap_cx = inner.trap_context_mut();
        trap_cx.lsx = restored_lsx;
        // LoongArch FPRs alias the low 64 bits of LSX registers. The signal
        // ABI exposes both snapshots, and the existing scalar mcontext has
        // precedence when a handler edits it. Merge that low lane before the
        // trap return path restores the complete LSX register file.
        for (vector, scalar) in trap_cx.lsx.v.iter_mut().zip(trap_cx.fp.f.iter()) {
            vector[0] = *scalar as u64;
        }
    }
    inner.sigmask = restored_sigmask;
    inner.trap_context_mut().gp.a0 as isize // return a0: not modify any of trap_cx
}
