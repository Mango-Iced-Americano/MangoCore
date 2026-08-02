use alloc::sync::Arc;

use crate::mm::{copy_to_user, UserPtrMut};
use crate::syscall::errno::*;
use crate::task::{
    current_task, current_user_token, exit_current_and_run_next, exit_group_and_run_next,
    signal::SigInfo, ProcessControlBlock, ProcessManager, Rusage,
};
const CAP_SYS_PTRACE: usize = 19;

pub fn sys_exit(exit_code: u32) -> ! {
    exit_current_and_run_next((exit_code & 0xff) << 8);
}

pub fn sys_exit_group(exit_code: u32) -> ! {
    exit_group_and_run_next((exit_code & 0xff) << 8);
}

bitflags! {
    struct WaitOption: u32 {
        const WNOHANG    = 1;
        // wait4()/waitpid() 常见 Linux 选项。当前内核还不支持 stopped/continued
        // 子进程状态上报，但这些标志不能直接作为 EINVAL 处理，否则 shell 的
        // system()/脚本执行路径会失败。
        const WSTOPPED   = 2;
        const WEXITED    = 4;
        const WCONTINUED = 8;
        const WNOWAIT    = 0x1000000;
        const WNOTHREAD  = 0x20000000;
        const WALL       = 0x40000000;
        const WCLONE     = 0x80000000;
    }
}
/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
///   pid > 0  → wait for the child whose process id == pid
///   pid == -1 → wait for any child
///   pid == 0  → wait for any child in the same process group (pgid)
///   pid < -1 → wait for any child whose pgid == |pid|
pub fn sys_wait4(pid: isize, status: *mut u32, option: u32, ru: *mut Rusage) -> isize {
    if pid == i32::MIN as isize {
        return ESRCH;
    }
    let option = match WaitOption::from_bits(option) {
        Some(option) => option,
        None => return EINVAL,
    };
    let task = current_task().unwrap();
    let token = current_user_token();
    let process = task.process.clone();
    match ProcessManager::wait_child(
        &process,
        pid,
        option.contains(WaitOption::WNOHANG),
        true,
        option.contains(WaitOption::WSTOPPED),
        option.contains(WaitOption::WCONTINUED),
        option.contains(WaitOption::WNOWAIT),
    ) {
        Ok(Some(child)) => {
            if !status.is_null() {
                if let Err(errno) = UserPtrMut::new(status).write(token, &child.status) {
                    return errno;
                }
            }
            // Linux 在 child 已被领取后依次写 status、rusage；任一 EFAULT 都
            // 不回滚 reap，也不能重新发布同一个退出事件。
            if !ru.is_null() {
                if let Err(errno) = UserPtrMut::new(ru).write(token, &child.rusage) {
                    return errno;
                }
            }
            child.pid as isize
        }
        Ok(None) => SUCCESS,
        Err(errno) => errno,
    }
}

pub fn sys_waitid(idtype: usize, id: usize, infop: usize, options: u32, ru: *mut Rusage) -> isize {
    const P_PIDFD: usize = 3;

    let option = match WaitOption::from_bits(options) {
        Some(option) => option,
        None => return EINVAL,
    };
    if !(option.intersects(WaitOption::WEXITED | WaitOption::WSTOPPED | WaitOption::WCONTINUED)) {
        return EINVAL;
    }

    let task = current_task().unwrap();
    let token = current_user_token();
    let process = task.process.clone();
    if idtype != P_PIDFD {
        let wait_pid = match waitid_target_pid(idtype, id, &process) {
            Ok(pid) => pid,
            Err(errno) => return errno,
        };
        return waitid_wait_child(&process, wait_pid, infop, ru, option, token);
    }

    let (target_pid, nonblock) = {
        let files_ref = process.files();
        let fd_table = files_ref.lock();
        let file = match fd_table.get_file(id) {
            Ok(file) => file,
            Err(err) => return -(err as isize),
        };
        let target_pid = match super::signal::pidfd_file_target_pid(&*file) {
            Ok(pid) => pid,
            Err(errno) => return errno,
        };
        (target_pid, file.is_nonblock())
    };

    if nonblock {
        if let Some(process) = ProcessManager::find_process(target_pid) {
            if !process.is_zombie() {
                return EAGAIN;
            }
        }
    }

    match ProcessManager::wait_child(
        &process,
        target_pid as isize,
        nonblock || option.contains(WaitOption::WNOHANG),
        option.contains(WaitOption::WEXITED),
        option.contains(WaitOption::WSTOPPED),
        option.contains(WaitOption::WCONTINUED),
        option.contains(WaitOption::WNOWAIT),
    ) {
        Ok(Some(child)) => {
            // raw waitid 的第五个参数是 rusage；Linux 先写 rusage，再写
            // siginfo。child 已领取，copyout 失败不撤销状态变化。
            if !ru.is_null() {
                if let Err(errno) = UserPtrMut::new(ru).write(token, &child.rusage) {
                    return errno;
                }
            }
            if infop != 0 {
                let siginfo = waitid_siginfo(child.pid, child.status);
                if let Err(errno) = UserPtrMut::<SigInfo>::from_addr(infop).write(token, &siginfo) {
                    return errno;
                }
            }
            SUCCESS
        }
        Ok(None) => {
            if nonblock {
                EAGAIN
            } else if infop != 0 {
                match UserPtrMut::<SigInfo>::from_addr(infop).write(token, &SigInfo::new(0, 0, 0)) {
                    Ok(()) => SUCCESS,
                    Err(errno) => errno,
                }
            } else {
                SUCCESS
            }
        }
        Err(errno) => errno,
    }
}

fn waitid_target_pid(
    idtype: usize,
    id: usize,
    process: &Arc<ProcessControlBlock>,
) -> Result<isize, isize> {
    const P_ALL: usize = 0;
    const P_PID: usize = 1;
    const P_PGID: usize = 2;

    match idtype {
        P_ALL => Ok(-1),
        P_PID => {
            if id > isize::MAX as usize {
                Err(ESRCH)
            } else {
                Ok(id as isize)
            }
        }
        P_PGID => {
            let pgid = if id == 0 { process.getpgid() } else { id };
            if pgid > isize::MAX as usize {
                Err(ESRCH)
            } else if pgid == 0 {
                Ok(0)
            } else {
                Ok(-(pgid as isize))
            }
        }
        _ => Err(EINVAL),
    }
}

fn waitid_wait_child(
    process: &Arc<ProcessControlBlock>,
    pid: isize,
    infop: usize,
    ru: *mut Rusage,
    option: WaitOption,
    token: usize,
) -> isize {
    match ProcessManager::wait_child(
        process,
        pid,
        option.contains(WaitOption::WNOHANG),
        option.contains(WaitOption::WEXITED),
        option.contains(WaitOption::WSTOPPED),
        option.contains(WaitOption::WCONTINUED),
        option.contains(WaitOption::WNOWAIT),
    ) {
        Ok(Some(child)) => {
            if !ru.is_null() {
                if let Err(errno) = UserPtrMut::new(ru).write(token, &child.rusage) {
                    return errno;
                }
            }
            if infop != 0 {
                let siginfo = waitid_siginfo(child.pid, child.status);
                if let Err(errno) = UserPtrMut::<SigInfo>::from_addr(infop).write(token, &siginfo) {
                    return errno;
                }
            }
            SUCCESS
        }
        Ok(None) => {
            if infop != 0 {
                match UserPtrMut::<SigInfo>::from_addr(infop).write(token, &SigInfo::new(0, 0, 0)) {
                    Ok(()) => SUCCESS,
                    Err(errno) => errno,
                }
            } else {
                SUCCESS
            }
        }
        Err(errno) => errno,
    }
}

fn waitid_siginfo(pid: usize, wait_status: u32) -> SigInfo {
    const SIGCHLD_SIGNUM: usize = 17;
    const CLD_EXITED: usize = 1;
    const CLD_KILLED: usize = 2;
    const CLD_DUMPED: usize = 3;
    const CLD_STOPPED: usize = 5;
    const CLD_CONTINUED: usize = 6;
    const SIGCONT_SIGNUM: usize = 18;

    if wait_status == 0xffff {
        return SigInfo::new_with_sender_value(
            SIGCHLD_SIGNUM,
            0,
            CLD_CONTINUED,
            pid,
            SIGCONT_SIGNUM,
        );
    }

    if (wait_status & 0xff) == 0x7f {
        let stop_signal = ((wait_status >> 8) & 0xff) as usize;
        return SigInfo::new_with_sender_value(SIGCHLD_SIGNUM, 0, CLD_STOPPED, pid, stop_signal);
    }

    let term_signal = (wait_status & 0x7f) as usize;
    if term_signal != 0 {
        let code = if (wait_status & 0x80) != 0 {
            CLD_DUMPED
        } else {
            CLD_KILLED
        };
        return SigInfo::new_with_sender_value(SIGCHLD_SIGNUM, 0, code, pid, term_signal);
    }

    let exit_status = ((wait_status >> 8) & 0xff) as usize;
    SigInfo::new_with_sender_value(SIGCHLD_SIGNUM, 0, CLD_EXITED, pid, exit_status)
}

pub fn sys_set_tid_address(tidptr: usize) -> isize {
    let task = current_task().unwrap();
    task.acquire_inner_lock().clear_child_tid = tidptr;
    task.gettid() as isize
}

pub fn sys_set_robust_list(head: usize, len: usize) -> isize {
    if len != crate::task::RobustList::HEAD_SIZE {
        return EINVAL;
    }
    let task = current_task().unwrap();
    let mut inner = task.acquire_inner_lock();
    inner.robust_list.head = head;
    //inner.robust_list.len = len;
    SUCCESS
}

pub fn sys_get_robust_list(pid: u32, head_ptr: *mut usize, len_ptr: *mut usize) -> isize {
    let current = current_task().unwrap();
    let token = current_user_token();
    let target = if pid == 0 {
        current.clone()
    } else {
        let task = match ProcessManager::find_task(pid as usize) {
            Some(task) => task,
            None => return ESRCH,
        };
        if current.gettid() != task.gettid() {
            let uid = current.uid();
            let euid = current.euid();
            let gid = current.gid();
            let egid = current.egid();
            let cap_effective = current.acquire_inner_lock().cap_effective;
            let target_uid = task.uid();
            let target_euid = task.euid();
            let target_gid = task.gid();
            let target_egid = task.egid();
            let privileged = euid == 0 || (cap_effective & (1u64 << CAP_SYS_PTRACE)) != 0;
            let same_creds = uid == target_uid
                && euid == target_euid
                && gid == target_gid
                && egid == target_egid;
            if !privileged && !same_creds {
                return EPERM;
            }
        }
        task
    };

    // 目标 Arc 固定 TCB 生命周期；只在 task.inner 内快照两个标量，用户页缺失、
    // CoW 或 TLB shootdown 都必须发生在普通任务锁之外。
    let robust_list = target.acquire_inner_lock().robust_list;
    // Linux 先写长度再写 head；长度是 64 位 robust_list_head 的结构体大小 24，
    // 不是 usize 指针大小。
    if copy_to_user(token, &robust_list.len, len_ptr).is_err() {
        log::error!("[sys_get_robust_list] Failed to copy to {:?}", len_ptr);
        return EFAULT;
    };
    if copy_to_user(token, &robust_list.head, head_ptr).is_err() {
        log::error!("[sys_get_robust_list] Failed to copy to {:?}", head_ptr);
        return EFAULT;
    };
    SUCCESS
}
