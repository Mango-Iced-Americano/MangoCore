use crate::mm::{copy_to_user, UserPtrMut};
use crate::syscall::errno::*;
use crate::task::{
    current_task, current_user_token, exit_current_and_run_next, exit_group_and_run_next,
    signal::SigInfo, ProcessManager, Rusage,
};
use log::info;

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
pub fn sys_wait4(pid: isize, status: *mut u32, option: u32, _ru: *mut Rusage) -> isize {
    let option = match WaitOption::from_bits(option) {
        Some(option) => option,
        None => return EINVAL,
    };
    info!("[sys_wait4] pid: {}, option: {:?}", pid, option);
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let process = task.process.clone();
    match ProcessManager::wait_child(&process, pid, option.contains(WaitOption::WNOHANG)) {
        Ok(Some(child)) => {
            if !status.is_null() {
                if let Err(errno) = UserPtrMut::new(status).write(token, &child.exit_code) {
                    return errno;
                }
            }
            child.pid as isize
        }
        Ok(None) => SUCCESS,
        Err(errno) => errno,
    }
}

pub fn sys_waitid(
    idtype: usize,
    id: usize,
    infop: usize,
    options: u32,
    _ru: *mut Rusage,
) -> isize {
    const P_PIDFD: usize = 3;
    const SIGCHLD_SIGNUM: usize = 17;
    const CLD_EXITED: usize = 1;

    let option = match WaitOption::from_bits(options) {
        Some(option) => option,
        None => return EINVAL,
    };
    if idtype != P_PIDFD || !option.contains(WaitOption::WEXITED) {
        return EINVAL;
    }

    let task = current_task().unwrap();
    let token = task.get_user_token();
    let (target_pid, nonblock) = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        let file = match fd_table.get_file(id) {
            Ok(file) => file,
            Err(err) => return -(err as isize),
        };
        let target_pid = match super::signal::pidfd_file_target_pid(file) {
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
        &task.process,
        target_pid as isize,
        nonblock || option.contains(WaitOption::WNOHANG),
    ) {
        Ok(Some(child)) => {
            if infop != 0 {
                let siginfo = SigInfo::new_with_sender(SIGCHLD_SIGNUM, 0, CLD_EXITED, child.pid);
                if let Err(errno) = UserPtrMut::<SigInfo>::from_addr(infop).write(token, &siginfo)
                {
                    return errno;
                }
            }
            SUCCESS
        }
        Ok(None) => {
            if nonblock {
                EAGAIN
            } else {
                SUCCESS
            }
        }
        Err(errno) => errno,
    }
}

pub fn sys_set_tid_address(tidptr: usize) -> isize {
    let task = current_task().unwrap();
    task.acquire_inner_lock().clear_child_tid = tidptr;
    task.tid.0 as isize
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
    let task = if pid == 0 {
        current_task().unwrap()
    } else {
        match ProcessManager::find_task(pid as usize) {
            Some(task) => task,
            None => return ESRCH,
        }
    };
    let current = current_task().unwrap();
    if current.gettid() != task.gettid() {
        let (uid, euid, gid, egid, cap_effective) = {
            let inner = current.acquire_inner_lock();
            (
                inner.uid,
                inner.euid,
                inner.gid,
                inner.egid,
                inner.cap_effective,
            )
        };
        let (target_uid, target_euid, target_gid, target_egid) = {
            let inner = task.acquire_inner_lock();
            (inner.uid, inner.euid, inner.gid, inner.egid)
        };
        let privileged =
            euid == 0 || (cap_effective & (1u64 << CAP_SYS_PTRACE)) != 0;
        let same_creds = uid == target_uid
            && euid == target_euid
            && gid == target_gid
            && egid == target_egid;
        if !privileged && !same_creds {
            return EPERM;
        }
    }
    let inner = task.acquire_inner_lock();
    let token = current_user_token();
    if copy_to_user(token, &inner.robust_list.head, head_ptr).is_err() {
        log::error!("[sys_get_robust_list] Failed to copy to {:?}", head_ptr);
        return EFAULT;
    };
    if copy_to_user(token, &inner.robust_list.len, len_ptr).is_err() {
        log::error!("[sys_get_robust_list] Failed to copy to {:?}", len_ptr);
        return EFAULT;
    };
    SUCCESS
}
