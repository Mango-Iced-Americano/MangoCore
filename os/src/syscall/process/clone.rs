use alloc::sync::Arc;

use crate::config::SYSTEM_TASK_LIMIT;
use crate::mm::UserPtrMut;
use crate::show_frame_consumption;
use crate::syscall::errno::*;
use crate::task::{current_task, signal::Signals, ProcessManager, TaskControlBlock};
use crate::utils::error::SyscallErr;
use log::{info, warn};

bitflags! {
    pub struct CloneFlags: u32 {
        //const CLONE_NEWTIME         =   0x00000080;
        /// 决定是否共享虚拟内存空间
        const CLONE_VM              =   0x00000100;
        /// 决定是否共享文件系统信息（如当前工作目录和根目录）
        const CLONE_FS              =   0x00000200;
        /// 使新进程共享打开的文件描述符表，但不共享文件描述符的状态
        const CLONE_FILES           =   0x00000400;
        /// 使新进程共享信号处理
        const CLONE_SIGHAND         =   0x00000800;
        const CLONE_PIDFD           =   0x00001000;
        const CLONE_PTRACE          =   0x00002000;
        const CLONE_VFORK           =   0x00004000;
        const CLONE_PARENT          =   0x00008000;
        const CLONE_THREAD          =   0x00010000;
        const CLONE_NEWNS           =   0x00020000;
        const CLONE_SYSVSEM         =   0x00040000;
        const CLONE_SETTLS          =   0x00080000;
        const CLONE_PARENT_SETTID   =   0x00100000;
        const CLONE_CHILD_CLEARTID  =   0x00200000;
        const CLONE_DETACHED        =   0x00400000;
        const CLONE_UNTRACED        =   0x00800000;
        const CLONE_CHILD_SETTID    =   0x01000000;
        const CLONE_NEWCGROUP       =   0x02000000;
        /// 使新进程拥有一个新的、独立的UTS命名空间，可以隔离主机名和域名
        const CLONE_NEWUTS          =   0x04000000;
        /// 使新进程拥有一个新的、独立的IPC命名空间，可以隔离System V IPC和POSIX消息队列
        const CLONE_NEWIPC          =   0x08000000;
        /// 使新进程拥有一个新的、独立的用户命名空间，可以隔离用户和用户组ID
        const CLONE_NEWUSER         =   0x10000000;
        /// 使新进程拥有一个新的、独立的PID命名空间，可以隔离进程ID
        const CLONE_NEWPID          =   0x20000000;
        /// 使新进程拥有一个新的、独立的网络命名空间，可以隔离网络设备、协议栈和端口
        const CLONE_NEWNET          =   0x40000000;
        const CLONE_IO              =   0x80000000;
    }
}

/// # Explanation of Parameters
/// Mainly about `ptid`, `tls` and `ctid`: \
/// `CLONE_SETTLS`: The TLS (Thread Local Storage) descriptor is set to `tls`. \
/// `CLONE_PARENT_SETTID`: Store the child thread ID at the location pointed to by `ptid` in the parent's memory. \
/// `CLONE_CHILD_SETTID`: Store the child thread ID at the location pointed to by `ctid` in the child's memory. \
/// `ptid` is also used in `CLONE_PIDFD` (since Linux 5.2) \
/// Since user programs rarely use these, we could do lazy implementation.
pub fn sys_clone(
    flags: u32,
    stack: *const u8,
    ptid: *mut u32,
    tls: usize,
    ctid: *mut u32,
) -> isize {
    // ---- 防御性检查 1：进程总量限制 ----
    if ProcessManager::process_count() >= SYSTEM_TASK_LIMIT as u16 {
        warn!(
            "[sys_clone] Total process limit reached ({})",
            SYSTEM_TASK_LIMIT
        );
        return -(SyscallErr::EAGAIN as isize); // Linux 语义：资源暂时不可用
    }
    // ---- 防御性检查 2：堆内存剩余预警 ----
    if crate::mm::unallocated_frames() < 32 {
        // 预留一点物理页给基本运作
        warn!("[sys_clone] Low physical memory, rejecting clone");
        return -(SyscallErr::ENOMEM as isize);
    }

    let parent = current_task().unwrap();
    // This signal will be sent to its parent when it exits
    // we need to add a field in TCB to support this feature, but not now.
    let exit_signal = match Signals::from_signum((flags & 0xff) as usize) {
        Ok(signal) => signal,
        Err(_) => {
            warn!(
                "[sys_clone] signum of exit_signal is unspecified or invalid: {}",
                (flags & 0xff) as usize
            );
            // This is permitted by standard, but we only support 64 signals
            Signals::empty()
        }
    };
    // Sure to succeed, because all bits are valid (See `CloneFlags`)
    let flags = CloneFlags::from_bits(flags & !0xff).unwrap();
    if flags.contains(CloneFlags::CLONE_VFORK) && flags.contains(CloneFlags::CLONE_THREAD) {
        return EINVAL;
    }
    info!(
        "[sys_clone] flags: {:?}, stack: {:?}, exit_signal: {:?}, ptid: {:?}, tls: {:?}, ctid: {:?}",
        flags, stack, exit_signal, ptid, tls, ctid
    );
    let mut child: Option<Arc<TaskControlBlock>> = None;
    show_frame_consumption! {
        "clone";
        child = match parent.sys_clone(flags, stack, tls, exit_signal) {
            Ok(task) => Some(task),
            Err(errno) => return errno,
        };
    }
    let child = match child {
        Some(task) => task,
        None => return ENOMEM,
    };
    let new_tid = child.tid.0;
    if flags.contains(CloneFlags::CLONE_PARENT_SETTID) {
        match UserPtrMut::new(ptid).write(parent.get_user_token(), &(new_tid as u32)) {
            Ok(()) => {}
            Err(errno) => {
                child.cleanup_unpublished_clone(flags.contains(CloneFlags::CLONE_VM));
                return errno;
            }
        };
    }
    // todo: CLONE_CHILD_SETTID标志被设置，但是ctid指针为零，会出现地址错误，干脆全注释掉
    if flags.contains(CloneFlags::CLONE_CHILD_SETTID) {
        match UserPtrMut::new(ctid).write(child.get_user_token(), &(new_tid as u32)) {
            Ok(()) => {}
            Err(errno) => log::warn!(
                "[sys_clone] Failed to set child_tid at {:?} with errno {}, but still create the thread",
                ctid, errno
            ),
        };
    }
    if flags.contains(CloneFlags::CLONE_CHILD_CLEARTID) {
        child.acquire_inner_lock().clear_child_tid = ctid as usize;
    }
    if let Err(errno) = ProcessManager::publish_clone_child(&parent, child.clone(), flags) {
        child.cleanup_unpublished_clone(flags.contains(CloneFlags::CLONE_VM));
        return errno;
    }
    if let Err(errno) = ProcessManager::schedule_clone_child(&parent, child.clone(), flags) {
        return errno;
    }
    new_tid as isize
}
