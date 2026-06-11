use alloc::sync::Arc;

use crate::config::{PAGE_SIZE, SYSTEM_TASK_LIMIT};
use crate::fs::pidfd::new_pidfd_file;
use crate::fs::vfs::MountFSInode;
use crate::mm::{
    translated_byte_buffer, FaultAccess, UserAccess, UserBuffer, UserPtrMut, VirtAddr,
};
use crate::show_frame_consumption;
use crate::syscall::errno::*;
use crate::task::{current_task, signal::Signals, IpcNamespace, MountNamespace, ProcessManager, TaskControlBlock};
use crate::utils::error::SyscallErr;
use log::{info, warn};

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CloneArgsV0 {
    flags: u64,
    pidfd: u64,
    child_tid: u64,
    parent_tid: u64,
    exit_signal: u64,
    stack: u64,
    stack_size: u64,
    tls: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CloneArgs {
    flags: u64,
    pidfd: u64,
    child_tid: u64,
    parent_tid: u64,
    exit_signal: u64,
    stack: u64,
    stack_size: u64,
    tls: u64,
    set_tid: u64,
    set_tid_size: u64,
    cgroup: u64,
}

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

fn read_clone3_args(uargs: *const u8, size: usize, token: usize) -> Result<CloneArgs, isize> {
    const MIN_SIZE: usize = core::mem::size_of::<CloneArgsV0>();
    const SUPPORTED_SIZE: usize = core::mem::size_of::<CloneArgs>();

    if size < MIN_SIZE {
        return Err(EINVAL);
    }
    if size > PAGE_SIZE {
        return Err(E2BIG);
    }
    if uargs.is_null() {
        return Err(EFAULT);
    }

    let copy_len = size.min(SUPPORTED_SIZE);
    let user = UserBuffer::new(translated_byte_buffer(
        token,
        uargs,
        copy_len,
        UserAccess::Read,
    )?);
    let mut args = CloneArgs::default();
    let dst = unsafe {
        core::slice::from_raw_parts_mut((&mut args as *mut CloneArgs).cast::<u8>(), copy_len)
    };
    user.read(dst);

    if size > SUPPORTED_SIZE {
        let extra_len = size - SUPPORTED_SIZE;
        let extra = UserBuffer::new(translated_byte_buffer(
            token,
            unsafe { uargs.add(SUPPORTED_SIZE) },
            extra_len,
            UserAccess::Read,
        )?);
        for idx in 0..extra.len() {
            if extra[idx] != 0 {
                return Err(E2BIG);
            }
        }
    }

    Ok(args)
}

fn write_u32_to_task_user(
    task: &Arc<TaskControlBlock>,
    ptr: *mut u32,
    value: u32,
) -> Result<(), isize> {
    if ptr.is_null() {
        return Err(EFAULT);
    }

    let bytes = value.to_ne_bytes();
    let vm = task.process.vm();
    let mut vm = vm.lock();
    for (offset, byte) in bytes.iter().enumerate() {
        let pa = vm.fault_in_user_va(VirtAddr::from(ptr as usize + offset), FaultAccess::Store)?;
        pa.floor().get_bytes_array()[pa.page_offset()] = *byte;
    }
    Ok(())
}

fn drop_parent_fd(parent: &Arc<TaskControlBlock>, fd: usize) {
    let files = parent.process.files();
    let _ = files.lock().drop_fd(fd);
}

fn sys_clone_inner(
    flags: u32,
    stack: *const u8,
    ptid: *mut u32,
    tls: usize,
    ctid: *mut u32,
    pidfd_ptr: Option<*mut u32>,
) -> isize {
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
    if flags.contains(CloneFlags::CLONE_SIGHAND) && !flags.contains(CloneFlags::CLONE_VM) {
        return EINVAL;
    }
    if flags.contains(CloneFlags::CLONE_THREAD) && !flags.contains(CloneFlags::CLONE_SIGHAND) {
        return EINVAL;
    }
    if flags.contains(CloneFlags::CLONE_VFORK) && flags.contains(CloneFlags::CLONE_THREAD) {
        return EINVAL;
    }
    if flags.contains(CloneFlags::CLONE_NEWNS) && flags.contains(CloneFlags::CLONE_FS) {
        return EINVAL;
    }
    if (flags.contains(CloneFlags::CLONE_NEWUTS) || flags.contains(CloneFlags::CLONE_NEWNET)
        || flags.contains(CloneFlags::CLONE_NEWNS) || flags.contains(CloneFlags::CLONE_NEWIPC))
        && parent.acquire_inner_lock().euid != 0
    {
        return EPERM;
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
            Err(errno) => {
                println!(
                    "[sys_clone] clone failed: errno={} flags={:?} quota={}/{} registry={} free_frames={} heap={}K",
                    errno,
                    flags,
                    crate::task::quota::allocated_task_count(),
                    SYSTEM_TASK_LIMIT,
                    crate::task::ProcessManager::all_processes().len(),
                    crate::mm::unallocated_frames(),
                    crate::mm::heap_stats().1 >> 10,
                );
                return errno;
            }
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
        match write_u32_to_task_user(&child, ctid, new_tid as u32) {
            Ok(()) => {}
            Err(errno) => {
                child.cleanup_unpublished_clone(flags.contains(CloneFlags::CLONE_VM));
                return errno;
            }
        };
    }
    if flags.contains(CloneFlags::CLONE_CHILD_CLEARTID) {
        child.acquire_inner_lock().clear_child_tid = ctid as usize;
    }
    let mut allocated_pidfd = None;
    if flags.contains(CloneFlags::CLONE_PIDFD) {
        let Some(pidfd_ptr) = pidfd_ptr else {
            child.cleanup_unpublished_clone(flags.contains(CloneFlags::CLONE_VM));
            return EINVAL;
        };
        let file = match new_pidfd_file(&child.process) {
            Ok(file) => file,
            Err(err) => {
                child.cleanup_unpublished_clone(flags.contains(CloneFlags::CLONE_VM));
                return -(err as isize);
            }
        };
        let files = parent.process.files();
        let pidfd = match files.lock().alloc_fd(file, false) {
            Ok(fd) => fd,
            Err(err) => {
                child.cleanup_unpublished_clone(flags.contains(CloneFlags::CLONE_VM));
                return -(err as isize);
            }
        };
        match UserPtrMut::new(pidfd_ptr).write(parent.get_user_token(), &(pidfd as u32)) {
            Ok(()) => allocated_pidfd = Some(pidfd),
            Err(errno) => {
                drop_parent_fd(&parent, pidfd);
                child.cleanup_unpublished_clone(flags.contains(CloneFlags::CLONE_VM));
                return errno;
            }
        };
    }
    if let Err(errno) = ProcessManager::publish_clone_child(&parent, child.clone(), flags) {
        if let Some(pidfd) = allocated_pidfd {
            drop_parent_fd(&parent, pidfd);
        }
        child.cleanup_unpublished_clone(flags.contains(CloneFlags::CLONE_VM));
        return errno;
    }
    ProcessManager::schedule_clone_child(&parent, child.clone(), flags);
    new_tid as isize
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
    if flags & CloneFlags::CLONE_PIDFD.bits() != 0
        && flags & CloneFlags::CLONE_PARENT_SETTID.bits() != 0
    {
        return EINVAL;
    }
    let pidfd_ptr = if flags & CloneFlags::CLONE_PIDFD.bits() != 0 {
        Some(ptid)
    } else {
        None
    };
    sys_clone_inner(flags, stack, ptid, tls, ctid, pidfd_ptr)
}

pub fn sys_unshare(flags: u32) -> isize {
    let flags = match CloneFlags::from_bits(flags) {
        Some(flags) => flags,
        None => return EINVAL,
    };

    let supported = CloneFlags::CLONE_FILES
        | CloneFlags::CLONE_FS
        | CloneFlags::CLONE_NEWUTS
        | CloneFlags::CLONE_NEWNET
        | CloneFlags::CLONE_NEWNS
        | CloneFlags::CLONE_NEWIPC;
    if !flags.difference(supported).is_empty() {
        return EINVAL;
    }

    let task = current_task().unwrap();
    if (flags.contains(CloneFlags::CLONE_NEWUTS) || flags.contains(CloneFlags::CLONE_NEWNET)
        || flags.contains(CloneFlags::CLONE_NEWNS) || flags.contains(CloneFlags::CLONE_NEWIPC))
        && task.acquire_inner_lock().euid != 0
    {
        return EPERM;
    }
    if flags.contains(CloneFlags::CLONE_FILES) {
        if let Err(e) = task.process.unshare_files() {
            return -(e as isize);
        }
    }
    if flags.contains(CloneFlags::CLONE_FS) {
        task.process.unshare_fs();
    }
    if flags.contains(CloneFlags::CLONE_NEWUTS) {
        task.process.unshare_uts();
    }
    if flags.contains(CloneFlags::CLONE_NEWNET) {
        if task.process.live_thread_count() != 1 {
            return EINVAL;
        }
        task.process.unshare_net();
    }
    if flags.contains(CloneFlags::CLONE_NEWNS) {
        if task.process.live_thread_count() != 1 {
            return EINVAL;
        }
        task.process.set_mnt(MountNamespace::new());
    }
    if flags.contains(CloneFlags::CLONE_NEWIPC) {
        if task.process.live_thread_count() != 1 {
            return EINVAL;
        }
        task.process.set_ipc(IpcNamespace::new());
    }
    SUCCESS
}

pub fn sys_clone3(uargs: *const u8, size: usize) -> isize {
    let token = current_task().unwrap().get_user_token();
    let args = match read_clone3_args(uargs, size, token) {
        Ok(args) => args,
        Err(errno) => return errno,
    };

    if args.flags >> 32 != 0 {
        return EINVAL;
    }

    let mut flags = args.flags as u32;
    if args.exit_signal > 0xff {
        return EINVAL;
    }
    if flags & CloneFlags::CLONE_PIDFD.bits() != 0
        && translated_byte_buffer(
            token,
            args.pidfd as *const u8,
            core::mem::size_of::<u32>(),
            UserAccess::Write,
        )
        .is_err()
    {
        return EFAULT;
    }
    if (args.stack == 0) != (args.stack_size == 0) {
        return EINVAL;
    }
    flags |= args.exit_signal as u32;

    let stack = if args.stack == 0 {
        core::ptr::null()
    } else {
        match (args.stack as usize).checked_add(args.stack_size as usize) {
            Some(sp) => sp as *const u8,
            None => return EINVAL,
        }
    };

    let pidfd_ptr = if flags & CloneFlags::CLONE_PIDFD.bits() != 0 {
        Some(args.pidfd as *mut u32)
    } else {
        None
    };

    sys_clone_inner(
        flags,
        stack,
        args.parent_tid as *mut u32,
        args.tls as usize,
        args.child_tid as *mut u32,
        pidfd_ptr,
    )
}

/// Switch to a different namespace by fd.
/// Linux: int setns(int fd, int nstype);
pub fn sys_setns(fd: usize, nstype: usize) -> isize {
    const CLONE_NEWNET_VAL: usize = 0x40000000;
    const CLONE_NEWNS_VAL: usize = 0x00020000;
    const CLONE_NEWIPC_VAL: usize = 0x08000000;
    if nstype != 0 && nstype != CLONE_NEWNET_VAL
        && nstype != CLONE_NEWNS_VAL && nstype != CLONE_NEWIPC_VAL
    {
        return EINVAL;
    }

    let task = current_task().unwrap();
    let euid = task.acquire_inner_lock().euid;
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(f) => f,
        Err(_) => return EBADF,
    };

    let inode = MountFSInode::unwrap_inode(&file.inode);

    if let Some(ns_inode) = inode
        .as_any_ref()
        .downcast_ref::<crate::fs::procfs::pid::ns::ProcNsNetInode>()
    {
        if nstype != 0 && nstype != CLONE_NEWNET_VAL {
            return EINVAL;
        }
        if euid != 0 {
            return EPERM;
        }
        let new_ns = ns_inode.netns().clone();
        drop(fd_table);
        task.process.set_net(new_ns);
        return 0;
    }

    // ProcNsMntInode / ProcNsIpcInode to be added in a separate task;
    // setns branches for those will be wired once the inode types exist.

    use crate::fs::procfs::pid::ns::{ProcNsMntInode, ProcNsIpcInode};

    if let Some(ns_inode) = inode.as_any_ref().downcast_ref::<ProcNsMntInode>() {
        if nstype != 0 && nstype != CLONE_NEWNS_VAL { return EINVAL; }
        if euid != 0 {
            return EPERM;
        }
        let new_ns = ns_inode.mntns().clone();
        drop(fd_table);
        task.process.set_mnt(new_ns);
        return 0;
    }

    if let Some(ns_inode) = inode.as_any_ref().downcast_ref::<ProcNsIpcInode>() {
        if nstype != 0 && nstype != CLONE_NEWIPC_VAL { return EINVAL; }
        if euid != 0 {
            return EPERM;
        }
        let new_ns = ns_inode.ipcns().clone();
        drop(fd_table);
        task.process.set_ipc(new_ns);
        return 0;
    }

    EINVAL
}
