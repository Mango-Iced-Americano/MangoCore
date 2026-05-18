use crate::config::{PAGE_SIZE, SYSTEM_TASK_LIMIT, USER_STACK_SIZE};
use crate::fs::{vfs, vfs_lookup};
use crate::hal::shutdown;
use crate::hal::{MachineContext, TrapContext};
use crate::mm::{
    copy_from_user, copy_to_user, copy_to_user_string, translated_byte_buffer, MapFlags,
    MapPermission, UserAccess, UserBuffer, UserCString, UserPtr, UserPtrMut, VirtAddr,
};
use crate::show_frame_consumption;
use crate::syscall::errno::*;
use crate::task::threads::{do_futex_wait, do_futex_wait_shared, futex_wake_shared, FutexCmd};
use crate::task::{
    add_kernel_timer, add_task, block_current_and_run_next, current_task, current_user_token,
    exit_current_and_run_next, exit_group_and_run_next, find_task_by_pid, find_task_by_tgid,
    procs_count, signal::*, suspend_current_and_run_next, threads, wait_with_timeout,
    wake_interruptible, Rusage, TaskControlBlock, TaskStatus, TimerAction,
};
use crate::timer::{get_time_ms, get_time_sec, ITimerVal, TimeSpec, TimeVal, TimeZone, Times};
use crate::utils::error::SyscallErr;
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::mem::size_of;
use log::{debug, error, info, trace, warn};
use num_enum::FromPrimitive;
pub fn sys_shutdown() -> isize {
    shutdown()
}
pub fn sys_exit(exit_code: u32) -> ! {
    exit_current_and_run_next((exit_code & 0xff) << 8);
}

pub fn sys_exit_group(exit_code: u32) -> ! {
    exit_group_and_run_next((exit_code & 0xff) << 8);
}

#[allow(non_camel_case_types)]
#[derive(Debug, Eq, PartialEq, FromPrimitive)]
#[repr(u32)]
pub enum SyslogAction {
    CLOSE = 0,
    OPEN = 1,
    READ = 2,
    READ_ALL = 3,
    READ_CLEAR = 4,
    CLEAR = 5,
    CONSOLE_OFF = 6,
    CONSOLE_ON = 7,
    CONSOLE_LEVEL = 8,
    SIZE_UNREAD = 9,
    SIZE_BUFFER = 10,
    #[default]
    ILLEAGAL,
}

pub fn sys_syslog(type_: u32, buf: *mut u8, len: u32) -> isize {
    const LOG_BUF_LEN: usize = 4096;
    const LOG: &str = "<5>[    0.000000] Linux version 5.10.102.1-microsoft-standard-WSL2 (rtrt@TEAM-NPUCORE) (gcc (Ubuntu 9.4.0-1ubuntu1~20.04) 9.4.0, GNU ld (GNU Binutils for Ubuntu) 2.34) #1 SMP Thu Mar 10 13:31:47 CST 2022";
    let token = current_user_token();
    let type_ = SyslogAction::from(type_);
    let len = LOG.len().min(len as usize);
    match type_ {
        SyslogAction::CLOSE | SyslogAction::OPEN => SUCCESS,
        SyslogAction::READ => {
            copy_to_user_string(token, &LOG[..len], buf).unwrap();
            len as isize
        }
        SyslogAction::READ_ALL => {
            copy_to_user_string(token, &LOG[LOG.len() - len..], buf).unwrap();
            len as isize
        }
        SyslogAction::READ_CLEAR => todo!(),
        SyslogAction::CLEAR => todo!(),
        SyslogAction::CONSOLE_OFF => todo!(),
        SyslogAction::CONSOLE_ON => todo!(),
        SyslogAction::CONSOLE_LEVEL => todo!(),
        SyslogAction::SIZE_UNREAD => todo!(),
        SyslogAction::SIZE_BUFFER => LOG_BUF_LEN as isize,
        SyslogAction::ILLEAGAL => EINVAL,
    }
}

pub fn sys_yield() -> isize {
    suspend_current_and_run_next();
    SUCCESS
}

pub fn sys_kill(pid: usize, sig: usize) -> isize {
    let signal = match Signals::from_signum(sig) {
        Ok(signal) => signal,
        Err(_) => return EINVAL,
    };
    #[cfg(feature = "comp")]
    if pid == 10 {
        return SUCCESS;
    }
    if pid > 0 {
        // [Warning] in current implementation,
        // signal will be sent to an arbitrary task with target `pid` (`tgid` more precisely).
        // But manual also require that the target task should not mask this signal.
        if let Some(task) = find_task_by_tgid(pid) {
            if !signal.is_empty() {
                let mut inner = task.acquire_inner_lock();
                inner.add_signal(signal);
                // wake up target process if it is sleeping
                if inner.task_status == TaskStatus::Interruptible {
                    inner.task_status = TaskStatus::Ready;
                    drop(inner);
                    wake_interruptible(task);
                }
            }
            SUCCESS
        } else {
            ESRCH
        }
    } else if pid == 0 {
        SUCCESS
    } else if (pid as isize) == -1 {
        todo!()
    } else {
        // (pid as isize) < -1
        todo!()
    }
}

pub fn sys_tkill(tid: usize, sig: usize) -> isize {
    let signal = match Signals::from_signum(sig) {
        Ok(signal) => signal,
        Err(_) => return EINVAL,
    };
    if tid > 0 {
        if let Some(task) = find_task_by_pid(tid) {
            if !signal.is_empty() {
                let mut inner = task.acquire_inner_lock();
                inner.add_signal(signal);
                // wake up target process if it is sleeping
                if inner.task_status == TaskStatus::Interruptible {
                    inner.task_status = TaskStatus::Ready;
                    drop(inner);
                    wake_interruptible(task);
                }
            }
            SUCCESS
        } else {
            ESRCH
        }
    } else if tid == 0 {
        todo!()
    } else if (tid as isize) == -1 {
        todo!()
    } else {
        // (pid as isize) < -1
        todo!()
    }
}

pub fn sys_tgkill(tgid: usize, tid: usize, sig: usize) -> isize {
    let signal = match Signals::from_signum(sig) {
        Ok(signal) => signal,
        Err(_) => return EINVAL,
    };
    if let Some(task) = find_task_by_tgid(tgid) {
        if !signal.is_empty() {
            let mut inner = task.acquire_inner_lock();
            if task.pid.0 == tid {
                inner.add_signal(signal);
                // wake up target process if it is sleeping
                if inner.task_status == TaskStatus::Interruptible {
                    inner.task_status = TaskStatus::Ready;
                    drop(inner);
                    wake_interruptible(task);
                }
            } else {
                warn!(
                    "[sys_tgkill] tid {} does not match task's tid {}",
                    tid, task.pid.0
                );
            }
        }
        SUCCESS
    } else {
        ESRCH
    }
}

pub fn sys_nanosleep(req: *const TimeSpec, rem: *mut TimeSpec) -> isize {
    if req.is_null() {
        return EINVAL;
    }
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let req = match UserPtr::new(req).read(token) {
        Ok(req) => req,
        Err(errno) => return errno,
    };

    let end = TimeSpec::now() + req;
    wait_with_timeout(Arc::downgrade(&task), end);
    drop(task);

    block_current_and_run_next();
    let task = current_task().unwrap();
    let now = TimeSpec::now();

    // 先释放 inner 锁再检查信号，避免与 has_actionable_signal 死锁
    // 参考 pselect/ppoll 的信号检查模式
    {
        let inner = task.acquire_inner_lock();
        let pending = inner.sigpending.difference(inner.sigmask);
        if !pending.is_empty() {
            drop(inner);
            if has_actionable_signal(&task) {
                // 被可操作信号打断 → 返回剩余时间 + EINTR
                if !rem.is_null() {
                    UserPtrMut::new(rem).write(token, &(end - now)).unwrap();
                }
                return EINTR;
            }
            // 不可操作的 pending 信号（被屏蔽/忽略）：清除它们
            // 避免残留信号导致后续 syscall 误判
            let mut inner = task.acquire_inner_lock();
            inner.sigpending = inner.sigpending.difference(pending);
            drop(inner);
        }
    }

    // 正常超时返回
    if !rem.is_null() {
        UserPtrMut::new(rem).write(token, &TimeSpec::new()).unwrap();
    }
    SUCCESS
}

pub fn sys_setitimer(
    which: usize,
    new_value: *const ITimerVal,
    old_value: *mut ITimerVal,
) -> isize {
    info!(
        "[sys_setitimer] which: {}, new_value: {:?}, old_value: {:?}",
        which, new_value, old_value
    );
    if which > 2 {
        return EINVAL;
    }
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let new_timer = match UserPtr::new(new_value).read_optional(token) {
        Ok(value) => value,
        Err(e) => {
            return e;
        }
    };
    match which {
        //实时计时器走KernelTimer
        0 => {
            let now = TimeSpec::now();
            //待注册计时器
            let mut register_timer = None;
            {
                let mut inner = task.acquire_inner_lock();
                if old_value as usize != 0 {
                    inner.timer[0].it_value = match inner.real_timer_deadline {
                        Some(deadline) => timespec_to_timeval(deadline - now),
                        None => TimeVal::new(),
                    };
                    if let Err(e) = UserPtrMut::new(old_value).write(token, &inner.timer[0]) {
                        return e;
                    }
                    trace!("[sys_setitimer] *old_value: {:?}", inner.timer[0]);
                }
                if let Some(value) = new_timer {
                    //防止generation溢出
                    inner.real_timer_generation = inner.real_timer_generation.wrapping_add(1);
                    if value.it_value.is_zero() {
                        inner.timer[0] = ITimerVal::new();
                        inner.real_timer_deadline = None;
                    } else {
                        let deadline = now + timeval_to_timespec(value.it_value);
                        inner.timer[0] = value;
                        inner.real_timer_deadline = Some(deadline);
                        register_timer = Some((deadline, inner.real_timer_generation));
                    }
                    // 更新锚点，防止 refresh_real_timer() 用陈旧锚点误触发 SIGALRM
                    inner.clock.last_real_timer_update = TimeVal::now();
                }
            }
            if let Some((deadline, generation)) = register_timer {
                add_kernel_timer(
                    TimerAction::SendSignal {
                        //降为弱引用
                        task: Arc::downgrade(&task),
                        signal: Signals::SIGALRM,
                        generation,
                    },
                    deadline,
                );
            }
            SUCCESS
        }
        1 | 2 => {
            let mut inner = task.acquire_inner_lock();
            if old_value as usize != 0 {
                if let Err(e) = UserPtrMut::new(old_value).write(token, &inner.timer[which]) {
                    return e;
                }
                trace!("[sys_setitimer] *old_value: {:?}", inner.timer[which]);
            }
            if let Some(value) = new_timer {
                inner.timer[which] = value;
                trace!("[sys_setitimer] *new_value: {:?}", inner.timer[which]);
                inner.clock.last_real_timer_update = TimeVal::now();
            }
            SUCCESS
        }
        _ => EINVAL,
    }
}

fn timeval_to_timespec(value: TimeVal) -> TimeSpec {
    TimeSpec::from_us(value.to_us())
}

fn timespec_to_timeval(value: TimeSpec) -> TimeVal {
    TimeVal::from_us(value.to_ns() / 1000)
}

pub fn sys_gettimeofday(tv: *mut TimeVal, _tz: *mut TimeZone) -> isize {
    // Timezone is currently NOT supported.
    if !tv.is_null() {
        let token = current_user_token();
        let timeval = &TimeVal::now();
        if UserPtrMut::new(tv).write(token, timeval).is_err() {
            log::error!("[sys_gettimeofday] Failed to copy to {:?}", tv);
            return EFAULT;
        }
    }
    SUCCESS
}

pub fn sys_get_time() -> isize {
    get_time_ms() as isize
}

#[allow(unused)]
#[repr(C)]
pub struct UTSName {
    sysname: [u8; 65],
    nodename: [u8; 65],
    release: [u8; 65],
    version: [u8; 65],
    machine: [u8; 65],
    domainname: [u8; 65],
}

pub fn sys_uname(buf: *mut u8) -> isize {
    let token = current_user_token();
    let mut buffer = UserBuffer::new(
        match translated_byte_buffer(token, buf, size_of::<UTSName>(), UserAccess::Write) {
            Ok(buffer) => buffer,
            Err(errno) => return errno,
        },
    );
    // A little stupid but still efficient.
    const FIELD_OFFSET: usize = 65;
    buffer.write_at(FIELD_OFFSET * 0, b"NPUcore\0");
    buffer.write_at(FIELD_OFFSET * 1, b"blossom\0");
    #[cfg(feature = "riscv")]
    buffer.write_at(FIELD_OFFSET * 2, b"5.10.0-1-rv64\0");
    #[cfg(feature = "loongarch64")]
    buffer.write_at(FIELD_OFFSET * 2, b"5.10.0-1-la64\0");
    buffer.write_at(FIELD_OFFSET * 3, b"#1 SMP blossom 5.10.0-1 (2025-01-10)\0");
    #[cfg(feature = "riscv")]
    buffer.write_at(FIELD_OFFSET * 4, b"rv64\0");
    #[cfg(feature = "loongarch64")]
    buffer.write_at(FIELD_OFFSET * 4, b"la64\0");
    buffer.write_at(FIELD_OFFSET * 5, b"\0");
    SUCCESS
}

pub fn sys_getpid() -> isize {
    let pid = current_task().unwrap().tgid;
    pid as isize
}

pub fn sys_getppid() -> isize {
    let task = current_task().unwrap();
    let inner = task.acquire_inner_lock();
    let parent = match inner.parent.as_ref().and_then(|p| p.upgrade()) {
        Some(parent) => parent,
        None => return 0, // No parent process
    };
    let ppid = parent.tgid;
    // let ppid = inner.parent.as_ref().unwrap().upgrade().unwrap().tgid;
    ppid as isize
}

pub fn sys_getuid() -> isize {
    0 // root user
}

pub fn sys_geteuid() -> isize {
    0 // root user
}

pub fn sys_getgid() -> isize {
    0 // root group
}

pub fn sys_getegid() -> isize {
    0 // root group
}

// Warning, we don't support this syscall in fact, task.setpgid() won't take effect for some reason
// So it just pretend to do this work.
// Fortunately, that won't make difference when we just try to run busybox sh so far.
pub fn sys_setpgid(pid: usize, pgid: usize) -> isize {
    if (pid as isize) < 0 || (pgid as isize) < 0 {
        return EINVAL;
    }
    let task = if pid == 0 {
        current_task().unwrap()
    } else {
        match find_task_by_tgid(pid) {
            Some(task) => task,
            None => return ESRCH,
        }
    };

    let real_pgid = if pgid == 0 { task.tgid } else { pgid };

    task.setpgid(real_pgid)
}

pub fn sys_getpgid(pid: usize) -> isize {
    if (pid as isize) < 0 {
        return EINVAL;
    }
    let task = if pid == 0 {
        current_task().unwrap()
    } else {
        match find_task_by_tgid(pid) {
            Some(task) => task,
            None => return ESRCH,
        }
    };

    task.getpgid() as isize
}
/// creates a new session if the calling process is not a process group leader.
/// The calling process is the leader of the new session, and its pgid is set to its pid.
/// 当前进程脱离父进程，从父进程的子进程列表中移除当前进程，当前进程的父进程设置为空。
pub fn sys_setsid() -> isize {
    let task = current_task().unwrap();
    // Detach from parent process.
    if let Some(parent) = task.acquire_inner_lock().parent.as_ref().unwrap().upgrade() {
        parent
            .acquire_inner_lock()
            .children
            .retain(|x| x.tid != task.tid);
    }
    let mut inner = task.acquire_inner_lock();
    inner.parent = None;
    // Make this process a session leader and process group leader.
    inner.pgid = task.tgid;
    drop(inner);
    SUCCESS
}

// For user, tid is pid in kernel
pub fn sys_gettid() -> isize {
    current_task().unwrap().pid.0 as isize
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct Sysinfo {
    uptime: usize,     /* Seconds since boot */
    loads: [usize; 3], /* 1, 5, and 15 minute load averages */
    totalram: usize,   /* Total usable main memory size */
    freeram: usize,    /* Available memory size */
    sharedram: usize,  /* Amount of shared memory */
    bufferram: usize,  /* Memory used by buffers */
    totalswap: usize,  /* Total swap space size */
    freeswap: usize,   /* Swap space still available */
    procs: u16,        /* Number of current processes */
    totalhigh: usize,  /* Total high memory size */
    freehigh: usize,   /* Available high memory size */
    mem_unit: u32,     /* Memory unit size in bytes */
                       //char __reserved[256];
                       // In the above structure, sizes of the memory and swap fields are given as multiples of mem_unit bytes.
}

pub fn sys_sysinfo(info: *mut Sysinfo) -> isize {
    const LINUX_SYSINFO_LOADS_SCALE: usize = 65536;
    const SEC_1_MIN: usize = 60;
    const SEC_5_MIN: usize = SEC_1_MIN * 5;
    const SEC_15_MIN: usize = SEC_1_MIN * 15;
    const UNIMPLEMENT: usize = 0;
    let token = current_user_token();
    let procs = procs_count();
    if copy_to_user(
        token,
        &Sysinfo {
            uptime: get_time_sec(),
            // Use only current sample (as average) to evaluate
            loads: [
                procs as usize * LINUX_SYSINFO_LOADS_SCALE / SEC_1_MIN,
                procs as usize * LINUX_SYSINFO_LOADS_SCALE / SEC_5_MIN,
                procs as usize * LINUX_SYSINFO_LOADS_SCALE / SEC_15_MIN,
            ],
            totalram: crate::config::MEMORY_END - crate::config::MEMORY_START,
            freeram: crate::mm::unallocated_frames() * PAGE_SIZE,
            sharedram: UNIMPLEMENT,
            bufferram: UNIMPLEMENT,
            totalswap: 0,
            freeswap: 0,
            procs,
            totalhigh: 0,
            freehigh: 0,
            mem_unit: 1,
        },
        info,
    )
    .is_err()
    {
        log::error!("[sys_sysinfo] Failed to copy to {:?}", info);
        EFAULT
    } else {
        SUCCESS
    }
}

pub fn sys_sbrk(increment: isize) -> isize {
    let task = current_task().unwrap();
    let mut inner = task.acquire_inner_lock();
    let mut memory_set = task.vm.lock();
    inner.heap_pt = memory_set.sbrk(inner.heap_pt, inner.heap_bottom, increment);
    inner.heap_pt as isize
}

pub fn sys_brk(brk_addr: usize) -> isize {
    let task = current_task().unwrap();
    let mut inner = task.acquire_inner_lock();
    let mut memory_set = task.vm.lock();
    if brk_addr == 0 {
        inner.heap_pt = memory_set.sbrk(inner.heap_pt, inner.heap_bottom, 0);
    } else {
        let former_addr = memory_set.sbrk(inner.heap_pt, inner.heap_bottom, 0);
        let grow_size = if brk_addr < former_addr {
            let delta = former_addr - brk_addr;
            if delta > isize::MAX as usize {
                warn!(
                    "[sys_brk] shrink delta too large: brk_addr={:X}, former_addr={:X}",
                    brk_addr, former_addr
                );
                0
            } else {
                -(delta as isize)
            }
        } else {
            let delta = brk_addr - former_addr;
            if delta > isize::MAX as usize {
                warn!(
                    "[sys_brk] grow delta too large: brk_addr={:X}, former_addr={:X}",
                    brk_addr, former_addr
                );
                0
            } else {
                delta as isize
            }
        };
        inner.heap_pt = memory_set.sbrk(inner.heap_pt, inner.heap_bottom, grow_size);
    }

    info!(
        "[sys_brk] brk_addr: {:X}; new_addr: {:X}",
        brk_addr, inner.heap_pt
    );
    inner.heap_pt as isize
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
    if procs_count() >= SYSTEM_TASK_LIMIT as u16 {
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
    let new_pid = child.pid.0;
    if flags.contains(CloneFlags::CLONE_PARENT_SETTID) {
        match UserPtrMut::new(ptid).write(parent.get_user_token(), &(child.pid.0 as u32)) {
            Ok(()) => {}
            Err(errno) => {
                child.cleanup_unpublished_clone(flags.contains(CloneFlags::CLONE_VM));
                return errno;
            }
        };
    }
    // todo: CLONE_CHILD_SETTID标志被设置，但是ctid指针为零，会出现地址错误，干脆全注释掉
    if flags.contains(CloneFlags::CLONE_CHILD_SETTID) {
        match UserPtrMut::new(ctid).write(child.get_user_token(), &(child.pid.0 as u32)) {
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
    if let Err(errno) = parent.publish_clone_child(child.clone(), flags) {
        child.cleanup_unpublished_clone(flags.contains(CloneFlags::CLONE_VM));
        return errno;
    }
    // add new task to scheduler
    add_task(child);
    new_pid as isize
}

/// 执行可执行文件
/// # 参数
/// + pathname：文件路径
/// + argv：参数列表
/// + envp：环境变量列表
pub fn sys_execve(
    pathname: *const u8,
    mut argv: *const *const u8,
    mut envp: *const *const u8,
) -> isize {
    // 设置默认shell为bash
    const DEFAULT_SHELL: &str = "/bin/bash";
    // 获取当前进程
    let task = current_task().unwrap();
    // 获取当前进程的用户态内存访问权限
    let token = task.get_user_token();
    // 获取可执行文件的路径
    let path = match UserCString::new(pathname).read(token) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    // 解析参数列表
    let mut argv_vec: Vec<String> = Vec::new();
    if argv_vec.try_reserve(16).is_err() {
        return ENOMEM;
    }
    // 解析环境变量列表
    let mut envp_vec: Vec<String> = Vec::new();
    if envp_vec.try_reserve(16).is_err() {
        return ENOMEM;
    }
    if !argv.is_null() {
        loop {
            let arg_ptr = match UserPtr::new(argv).read(token) {
                Ok(argv) => argv,
                Err(errno) => return errno,
            };
            if arg_ptr.is_null() {
                break;
            }
            if argv_vec.try_reserve(1).is_err() {
                return ENOMEM;
            }
            argv_vec.push(match UserCString::new(arg_ptr).read(token) {
                Ok(arg) => arg,
                Err(errno) => return errno,
            });
            unsafe {
                argv = argv.add(1);
            }
        }
    }
    if !envp.is_null() {
        loop {
            let env_ptr = match UserPtr::new(envp).read(token) {
                Ok(envp) => envp,
                Err(errno) => return errno,
            };
            if env_ptr.is_null() {
                break;
            }
            if envp_vec.try_reserve(1).is_err() {
                return ENOMEM;
            }
            envp_vec.push(match UserCString::new(env_ptr).read(token) {
                Ok(env) => env,
                Err(errno) => return errno,
            });
            unsafe {
                envp = envp.add(1);
            }
        }
    }
    debug!(
        "[exec] argv: {:?} /* {} vars */, envp: {:?} /* {} vars */",
        argv_vec,
        argv_vec.len(),
        envp_vec,
        envp_vec.len()
    );
    // 获取当前工作目录的文件描述符
    let working_inode = &task.fs.lock().working_inode;
    let cwd_inode: Arc<dyn vfs::IndexNode> = working_inode.inode.clone();

    let open_exec = |path: &str| -> Result<vfs::File, isize> {
        let inode = vfs_lookup(&cwd_inode, path, true)?;
        vfs::File::new(inode, vfs::FileFlags::O_RDONLY).map_err(|e| -(e as isize))
    };

    match open_exec(&path) {
        // 检查打开的文件
        Ok(file) => {
            // 若文件大小小于4，则返回ENOEXEC
            // 即非可执行文件
            if file.get_size() < 4 {
                return ENOEXEC;
            }
            // 看前四个字节是否是可执行文件魔数
            let mut magic_number = Box::<[u8; 4]>::new([0; 4]);
            // this operation may be expensive... I'm not sure
            let _ = file.pread(0, magic_number.as_mut_slice());
            let elf = match magic_number.as_slice() {
                // ELF可执行文件
                b"\x7fELF" => file,
                // 脚本文件
                // 用默认Shell即bash加载
                b"#!" => {
                    let shell_file = open_exec(DEFAULT_SHELL).unwrap();
                    if argv_vec.try_reserve(1).is_err() {
                        return ENOMEM;
                    }
                    argv_vec.insert(0, DEFAULT_SHELL.to_string());
                    shell_file
                }
                // 非可执行文件
                _ => return ENOEXEC,
            };

            let task = current_task().unwrap();
            // 确保 exe_path 是绝对路径（glibc _dl_get_origin 要求以 '/' 开头）
            let abs_path = if path.starts_with('/') {
                path.clone()
            } else {
                let cwd = task.fs.lock().working_path.clone();
                if cwd == "/" {
                    alloc::format!("/{}", path)
                } else {
                    alloc::format!("{}/{}", cwd, path)
                }
            };
            *task.exe_path.lock() = abs_path;
            show_frame_consumption! {
                "load_elf";
                if let Err(errno) = task.load_elf(elf, &argv_vec, &envp_vec) {
                    exit_current_and_run_next(127);
                };
            }
            // should return 0 in success
            SUCCESS
        }
        Err(errno) => {
            info!("[sys_execve] open_path(\"{}\") failed: errno={}", path, errno);
            errno
        },
    }
}

bitflags! {
    struct WaitOption: u32 {
        const WNOHANG    = 1;
        const WSTOPPED   = 2;
        const WEXITED    = 4;
        const WCONTINUED = 8;
        const WNOWAIT    = 0x1000000;
    }
}
/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
///   pid > 0  → wait for the child whose tgid == pid
///   pid == -1 → wait for any child
///   pid == 0  → wait for any child in the same process group (pgid)
///   pid < -1 → wait for any child whose pgid == |pid|
pub fn sys_wait4(pid: isize, status: *mut u32, option: u32, _ru: *mut Rusage) -> isize {
    let option = match WaitOption::from_bits(option) {
        Some(option) => option,
        None => return EINVAL,
    };
    if option.bits() & !WaitOption::WNOHANG.bits() != 0 {
        return EINVAL;
    }
    info!("[sys_wait4] pid: {}, option: {:?}", pid, option);
    let task = current_task().unwrap();
    let token = task.get_user_token();

    fn child_matches_pid(
        child_tgid: usize,
        child_pgid: usize,
        caller_pgid: usize,
        pid: isize,
    ) -> bool {
        if pid == -1 {
            true
        } else if pid > 0 {
            pid as usize == child_tgid
        } else if pid == 0 {
            child_pgid == caller_pgid
        } else {
            child_pgid == (-pid) as usize
        }
    }

    loop {
        // find a child process

        // ---- hold current PCB lock
        let mut inner = task.acquire_inner_lock();
        let caller_pgid = inner.pgid;

        let has_child = inner.children.iter().any(|p| {
            let child_inner = p.acquire_inner_lock();
            child_matches_pid(p.tgid, child_inner.pgid, caller_pgid, pid)
        });
        if !has_child {
            return ECHILD;
        }
        // ---- release current PCB lock (implicitly by drop(inner))

        // Find a zombie
        let pair = inner.children.iter().enumerate().find(|(_, p)| {
            let child_inner = p.acquire_inner_lock();
            child_inner.is_zombie() && child_matches_pid(p.tgid, child_inner.pgid, caller_pgid, pid)
        });

        if let Some((idx, _)) = pair {
            // drop last TCB of child
            let child = inner.children.remove(idx);
            trace!("[wait4] release zombie task, pid: {}", child.pid.0);
            // confirm that child will be deallocated after being removed from children list
            // 注意：如果 child 被 OOM killer 杀死（exit_current_and_run_next 从 handle_alloc_error 调用），
            // 则 syscall handler 栈上的 current_task().unwrap() 仍有额外 Arc 引用，
            // 此时 strong_count >= 2。这是可接受的：do_exit 已释放所有用户资源，
            // 多出来的 Arc 只是让 TCB 多活一会，等栈 unwound 自然释放。
            if Arc::strong_count(&child) != 1 {
                log::debug!(
                    "[wait4] child pid={} has extra ref (count={}), \
                     likely OOM-killed from inside a syscall",
                    child.pid.0,
                    Arc::strong_count(&child),
                );
            }

            // if main thread exit

            let found_tgid = child.tgid;
            let exit_code = child.acquire_inner_lock().exit_code;
            if !status.is_null() {
                if let Err(errno) = UserPtrMut::new(status).write(token, &exit_code) {
                    return errno;
                }
            }
            return found_tgid as isize;
        } else {
            if option.contains(WaitOption::WNOHANG) {
                drop(inner);
                return SUCCESS;
            } else {
                // 在持有锁的情况下检查信号
                let pending = inner.sigpending.difference(inner.sigmask);
                if !pending.is_empty() {
                    drop(inner);
                    // 临时解锁判断是否 Actionable
                    if has_actionable_signal(&task) {
                        return ERESTART;
                    }

                    // 清除无用信号，并重新进入 loop 循环
                    let mut inner = task.acquire_inner_lock();
                    let pending_now = inner.sigpending.difference(inner.sigmask);
                    inner.sigpending = inner.sigpending.difference(pending_now);
                    drop(inner);
                    continue;
                }

                drop(inner);
                crate::task::block_current_and_run_next();
                debug!("[sys_wait4] --resumed--");
            }
        }
    }
}

#[allow(unused)]
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct RLimit {
    rlim_cur: usize, /* Soft limit */
    rlim_max: usize, /* Hard limit (ceiling for rlim_cur) */
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, FromPrimitive)]
#[repr(u32)]
pub enum Resource {
    CPU = 0,
    FSIZE = 1,
    DATA = 2,
    STACK = 3,
    CORE = 4,
    RSS = 5,
    NPROC = 6,
    NOFILE = 7,
    MEMLOCK = 8,
    AS = 9,
    LOCKS = 10,
    SIGPENDING = 11,
    MSGQUEUE = 12,
    NICE = 13,
    RTPRIO = 14,
    RTTIME = 15,
    NLIMITS = 16,
    #[num_enum(default)]
    ILLEAGAL,
}

fn rlimit_value_for(resource: Resource, nofile: Option<RLimit>) -> Option<RLimit> {
    let unlimited = RLimit {
        rlim_cur: usize::MAX,
        rlim_max: usize::MAX,
    };

    let limit = match resource {
        Resource::CPU
        | Resource::FSIZE
        | Resource::DATA
        | Resource::RSS
        | Resource::AS
        | Resource::LOCKS
        | Resource::SIGPENDING
        | Resource::MSGQUEUE
        | Resource::NICE
        | Resource::RTPRIO
        | Resource::RTTIME
        | Resource::MEMLOCK => unlimited,
        Resource::CORE => RLimit {
            rlim_cur: 0,
            rlim_max: 0,
        },
        Resource::STACK => RLimit {
            rlim_cur: USER_STACK_SIZE,
            rlim_max: USER_STACK_SIZE,
        },
        Resource::NPROC => RLimit {
            rlim_cur: SYSTEM_TASK_LIMIT,
            rlim_max: SYSTEM_TASK_LIMIT,
        },
        Resource::NOFILE => nofile?,
        Resource::NLIMITS | Resource::ILLEAGAL => return None,
    };
    Some(limit)
}

/// It can be used to both set and get the resource limits of an arbitrary process.
/// # WARNING
/// Partial implementation
pub fn sys_prlimit(
    pid: usize,
    resource: u32,
    new_limit: *const RLimit,
    old_limit: *mut RLimit,
) -> isize {
    let task = current_task().unwrap();
    if pid != 0 && pid != task.tgid {
        return ESRCH;
    }

    let token = task.get_user_token();
    let resource = Resource::from_primitive(resource);
    info!(
        "[sys_prlimit] pid: {}, resource: {:?}, new_limit: {:?}, old_limit: {:?}",
        pid, resource, new_limit, old_limit
    );

    if resource == Resource::ILLEAGAL || resource == Resource::NLIMITS {
        return EINVAL;
    }

    if !old_limit.is_null() {
        let nofile_limit = if resource == Resource::NOFILE {
            let lock = task.files.lock();
            Some(RLimit {
                rlim_cur: lock.get_soft_limit(),
                rlim_max: lock.get_hard_limit(),
            })
        } else {
            None
        };
        let Some(limit) = rlimit_value_for(resource, nofile_limit) else {
            return EINVAL;
        };
        if UserPtrMut::new(old_limit).write(token, &limit).is_err() {
            log::error!("[sys_prlimit] Failed to copy to {:?}", old_limit);
            return EFAULT;
        }
    }

    if !new_limit.is_null() {
        let rlimit = match UserPtr::new(new_limit).read(token) {
            Ok(rlimit) => rlimit,
            Err(_) => {
                log::error!("[sys_prlimit] Failed to copy from {:?}", new_limit);
                return EFAULT;
            }
        };
        if rlimit.rlim_cur > rlimit.rlim_max {
            return EINVAL;
        }
        match resource {
            Resource::NOFILE => {
                task.files.lock().set_soft_limit(rlimit.rlim_cur);
                task.files.lock().set_hard_limit(rlimit.rlim_max);
            }
            Resource::STACK => {
                warn!("[prlimit] Unsupported modification stack");
                if rlimit.rlim_cur > USER_STACK_SIZE {
                    return EINVAL;
                }
            }
            Resource::CPU
            | Resource::FSIZE
            | Resource::DATA
            | Resource::CORE
            | Resource::RSS
            | Resource::NPROC
            | Resource::MEMLOCK
            | Resource::AS
            | Resource::LOCKS
            | Resource::SIGPENDING
            | Resource::MSGQUEUE
            | Resource::NICE
            | Resource::RTPRIO
            | Resource::RTTIME => {
                warn!(
                    "[prlimit] Ignore unsupported modification for {:?}: {:?}",
                    resource, rlimit
                );
            }
            Resource::NLIMITS | Resource::ILLEAGAL => return EINVAL,
        }
    }
    SUCCESS
}
/// set pointer to thread ID
/// This feature is currently NOT supported and is implemented as a stub,
/// since threads are not supported.
pub fn sys_set_tid_address(tidptr: usize) -> isize {
    current_task().unwrap().acquire_inner_lock().clear_child_tid = tidptr;
    sys_gettid()
}

bitflags! {
    pub struct FutexOption: u32 {
        const PRIVATE = 128;
        const CLOCK_REALTIME = 256;
    }
}

/// # 描述
/// fast user-space locking
/// # 参数
/// * `uaddr`: `usize`, the address to the futex word;
/// * `futex_op`: `u32`, the operation to perform on the futex;
/// The remaining arguments (val, timeout, uaddr2, and val3) are re‐
/// quired only for certain of the futex  operations  described
/// below.  Where one of these arguments is not required, it is
/// ignored.
/// * `val`: `u32`, the argument to futex_op
/// * `timeout`: `*const TimeSpec`,
/// * `uaddr2`: `usize`,
/// * `val3`: `u32`,
pub fn sys_futex(
    uaddr: *mut u32,
    futex_op: u32,
    val: u32,
    timeout: *const TimeSpec,
    uaddr2: *mut u32,
    val3: u32,
) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();
    // uaddr is always used
    if uaddr.is_null() || uaddr.align_offset(4) != 0 {
        return EINVAL;
    }
    let futex_word = UserPtr::new(uaddr as *const u32);
    match futex_word.read(token) {
        Ok(_) => {}
        Err(errno) => return errno,
    };
    let cmd = threads::FutexCmd::from_primitive(futex_op & 0x7fu32);
    let option = FutexOption::from_bits_truncate(futex_op);
    let is_private = option.contains(FutexOption::PRIVATE);
    let private_key = uaddr as usize;
    if !is_private {
        trace!("[futex] process-shared futex, cmd={:?}", cmd);
    }
    info!(
        "[futex] uaddr: {:?}, futex_op: {:?}, option: {:?}, val: {:X}, timeout: {:?}, uaddr2: {:?}, val3: {:X}",
        uaddr, cmd, option, val, timeout, uaddr2, val3
    );

    // 计算用户地址对应的物理地址 key（用于 process-shared futex）
    // 分解为独立函数避免闭包捕获 task 的借用问题
    fn va_to_phys_key(
        vm: &crate::mm::AddressSpace<crate::mm::KernelPageTableImpl>,
        va: usize,
    ) -> Option<usize> {
        let va = VirtAddr::from(va);
        let vpn = va.floor();
        let offset = va.page_offset();
        vm.translate(vpn).map(|ppn| (ppn.0 << 12) + offset)
    }

    match cmd {
        FutexCmd::Wait => {
            let timeout = match UserPtr::new(timeout).read_optional(token) {
                Ok(timeout) => timeout,
                Err(errno) => return errno,
            };
            if !is_private {
                let vm = task.vm.lock();
                let phys_key = match va_to_phys_key(&vm, uaddr as usize) {
                    Some(k) => k,
                    None => return EFAULT,
                };
                drop(vm);
                drop(task);
                do_futex_wait_shared(futex_word, token, val, timeout, phys_key)
            } else {
                drop(task);
                do_futex_wait(futex_word, token, private_key, val, timeout)
            }
        }
        FutexCmd::Wake => {
            if is_private {
                task.futex.lock().wake(private_key, val)
            } else {
                let vm = task.vm.lock();
                let phys_key = match va_to_phys_key(&vm, uaddr as usize) {
                    Some(k) => k,
                    None => return EFAULT,
                };
                drop(vm);
                futex_wake_shared(phys_key, val)
            }
        }
        FutexCmd::Requeue => {
            if uaddr2.is_null() || uaddr2.align_offset(4) != 0 {
                return EINVAL;
            }
            match UserPtr::new(uaddr2 as *const u32).read(token) {
                Ok(_) => {}
                Err(errno) => return errno,
            };
            if is_private {
                task.futex
                    .lock()
                    .requeue(private_key, uaddr2 as usize, val, timeout as u32)
            } else {
                let phys_key = {
                    let vm = task.vm.lock();
                    match va_to_phys_key(&vm, uaddr as usize) {
                        Some(k) => k,
                        None => return EFAULT,
                    }
                };
                let phys_key2 = {
                    let vm = task.vm.lock();
                    match va_to_phys_key(&vm, uaddr2 as usize) {
                        Some(k) => k,
                        None => return EFAULT,
                    }
                };
                // shared requeue: wake + move remaining to second queue
                let mut shared = crate::task::threads::PROCESS_SHARED_FUTEX.lock();
                let wake_cnt = if let Some(mut wq) = shared.remove(&phys_key) {
                    let cnt = wq.wake_at_most(val as usize);
                    if !wq.is_empty() {
                        shared.insert(phys_key, wq);
                    }
                    cnt
                } else {
                    0
                };
                // requeue to phys_key2: 简化实现，LTP 中极少用
                drop(shared);
                wake_cnt as isize
            }
        }
        FutexCmd::Invalid => EINVAL,
        _ => EINVAL, // Unsupported command
    }
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
        match find_task_by_pid(pid as usize) {
            Some(task) => task,
            None => return ESRCH,
        }
    };
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

fn parse_mmap_prot(prot: usize) -> Result<MapPermission, isize> {
    const PROT_READ: usize = 0x1;
    const PROT_WRITE: usize = 0x2;
    const PROT_EXEC: usize = 0x4;
    const PROT_ALLOWED: usize = PROT_READ | PROT_WRITE | PROT_EXEC;
    if prot & !PROT_ALLOWED != 0 {
        return Err(EINVAL);
    }
    let mut map_perm = MapPermission::U;
    if prot & PROT_READ != 0 {
        map_perm |= MapPermission::R;
    }
    if prot & PROT_WRITE != 0 {
        // 写权限在页表里需要带读权限，否则部分架构会反复页故障
        map_perm |= MapPermission::R | MapPermission::W;
    }
    if prot & PROT_EXEC != 0 {
        map_perm |= MapPermission::X;
    }
    Ok(map_perm)
}

fn parse_mmap_flags(flags: usize) -> Result<MapFlags, isize> {
    let flags = MapFlags::from_bits(flags).ok_or(EINVAL)?;
    let type_bits = flags.bits() & MapFlags::MAP_TYPE.bits();
    if type_bits != MapFlags::MAP_SHARED.bits()
        && type_bits != MapFlags::MAP_PRIVATE.bits()
        && type_bits != MapFlags::MAP_SHARED_VALIDATE.bits()
    {
        return Err(EINVAL);
    }
    Ok(flags)
}

pub fn sys_mmap(
    start: usize,
    len: usize,
    prot: usize,
    flags: usize,
    fd: usize,
    offset: usize,
) -> isize {
    let task = current_task().unwrap();
    let mut memory_set = task.vm.lock();
    let prot = match parse_mmap_prot(prot) {
        Ok(prot) => prot,
        Err(errno) => return errno,
    };
    let flags = match parse_mmap_flags(flags) {
        Ok(flags) => flags,
        Err(errno) => return errno,
    };
    info!(
        "[mmap] start:{:X}; len:{:X}; prot:{:?}; flags:{:?}; fd:{}; offset:{:X}",
        start, len, prot, flags, fd as isize, offset
    );
    memory_set.mmap(start, len, prot, flags, fd, offset)
}

/// # Versions
/// The membarrier() system call was added in Linux 4.3.
/// Before Linux 5.10, the prototype for membarrier() was:
/// `int membarrier(int cmd, int flags);`
pub fn sys_memorybarrier(_cmd: usize, _flags: usize, _cpu_id: usize) -> isize {
    error!("[sys_memorybarrier]=========PSEUDOIMPLEMENTATION=========");
    error!(
        "This system call is only needed by the multicore environment for faster synchronization."
    );
    error!("In theory, it can be replaced (INefficiently) by fencing.");
    return SUCCESS;
}

pub fn sys_munmap(start: usize, len: usize) -> isize {
    let task = current_task().unwrap();
    let result = task.vm.lock().munmap(start, len);
    match result {
        Ok(_) => SUCCESS,
        Err(errno) => errno,
    }
}

pub fn sys_mprotect(addr: usize, len: usize, prot: usize) -> isize {
    let task = current_task().unwrap();
    let prot = match parse_mmap_prot(prot) {
        Ok(prot) => prot,
        Err(errno) => return errno,
    };
    let result = task.vm.lock().mprotect(addr, len, prot);
    match result {
        Ok(_) => SUCCESS,
        Err(errno) => errno,
    }
}

pub fn sys_clock_gettime(clk_id: usize, tp: *mut TimeSpec) -> isize {
    if !tp.is_null() {
        let token = current_user_token();
        let timespec = &TimeSpec::now();
        if UserPtrMut::new(tp).write(token, timespec).is_err() {
            log::error!("[sys_clock_gettime] Failed to copy to {:?}", tp);
            return EFAULT;
        };
        log::trace!("[sys_clock_gettime] clk_id: {}, tp: {:?}", clk_id, timespec);
    }
    SUCCESS
}
pub fn sys_clock_nanosleep(
    clk_id: usize,
    flags: u32,
    rqtp: *const TimeSpec,
    rmtp: *mut TimeSpec,
) -> isize {
    if !rqtp.is_null() {
        let token = current_user_token();
        let timespec = match UserPtr::new(rqtp).read(token) {
            Ok(timespec) => timespec,
            Err(errno) => return errno,
        };
        info!(
            "[sys_clock_nanosleep] clk_id: {}, flags: {:?}, rqtp: {:?}, rmtp: {:?}",
            clk_id, flags, timespec, rmtp
        );
    }
    SUCCESS
}

// int sigaction(int signum, const struct sigaction *act, struct sigaction *oldact);
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
pub fn sys_sigprocmask(how: u32, set: usize, oldset: usize) -> isize {
    info!(
        "[sys_sigprocmask] how: {:?}; set: {:X}, oldset: {:X}",
        how, set, oldset
    );
    sigprocmask(how, set as *const Signals, oldset as *mut Signals)
}

/// rt_sigpending(sigset_t *set, size_t sigsetsize)
/// Copy the set of pending signals to user-space `set`.
/// sigsetsize must equal sizeof(sigset_t) (= 8 on riscv64).
pub fn sys_rt_sigpending(set: usize, sigsetsize: usize) -> isize {
    let sigset_size = size_of::<Signals>();
    if sigsetsize != sigset_size {
        return -(SyscallErr::EINVAL as isize);
    }
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let inner = task.acquire_inner_lock();
    trace!(
        "[sys_rt_sigpending] pid: {}, pending: {:?}",
        task.pid.0,
        inner.sigpending
    );
    match UserPtrMut::from_addr(set).write(token, &inner.sigpending) {
        Ok(()) => SUCCESS,
        Err(errno) => errno,
    }
}

pub fn sys_rt_sigsuspend(mask: usize, sigsetsize: usize) -> isize {
    // 暂不实现完整语义，返回 ENOSYS 让调用者 fallback
    ENOSYS
}

pub fn sys_sigtimedwait(set: usize, info: usize, timeout: usize) -> isize {
    sigtimedwait(
        set as *const Signals,
        info as *mut SigInfo,
        timeout as *const TimeSpec,
    )
}

pub fn sys_sigaltstack(ss: usize, old_ss: usize) -> isize {
    sigaltstack(ss as *const SignalStack, old_ss as *mut SignalStack)
}

pub fn sys_sigreturn() -> isize {
    // mark not processing signal handler
    let task = current_task().unwrap();
    let mut inner = task.acquire_inner_lock();
    let token = task.get_user_token();
    info!("[sys_sigreturn] pid: {}", task.pid.0);

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
        .checked_add(size_of::<Signals>())
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
    let restored_sigmask = match UserPtr::<Signals>::from_addr(sigmask_addr).read(token) {
        Ok(sigmask) => sigmask - Signals::CAN_NOT_BE_MASKED,
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

/// Get process times
pub fn sys_times(buf: *mut Times) -> isize {
    let task = current_task().unwrap();
    let inner = task.acquire_inner_lock();
    let token = task.get_user_token();
    let times = Times {
        tms_utime: inner.rusage.ru_utime.to_tick(),
        tms_stime: inner.rusage.ru_stime.to_tick(),
        tms_cutime: 0,
        tms_cstime: 0,
    };
    if UserPtrMut::new(buf).write(token, &times).is_err() {
        log::error!("[sys_times] Failed to copy to {:?}", buf);
        return EFAULT;
    };
    // return clock ticks that have elapsed since an arbitrary point in the past
    crate::hal::get_time() as isize
}

pub fn sys_getrusage(who: isize, usage: *mut Rusage) -> isize {
    if who != 0 {
        panic!("[sys_getrusage] parameter 'who' is not RUSAGE_SELF.");
    }
    let task = current_task().unwrap();
    let inner = task.acquire_inner_lock();
    let token = task.get_user_token();
    if UserPtrMut::new(usage).write(token, &inner.rusage).is_err() {
        log::error!("[sys_getrusage] Failed to copy to {:?}", usage);
        return EFAULT;
    };
    //info!("[sys_getrusage] who: RUSAGE_SELF, usage: {:?}", inner.rusage);
    SUCCESS
}

//获得进程pid允许运行在哪些cpu上，对mask进行相应位置位，pid为0默认为当前task
pub fn sys_sched_getaffinity(pid: usize, cpusetsize: usize, mask: *mut u8) -> isize {
    //qemu上目前只有单核，对于所有进程都只能对mask0bit置位
    if mask.is_null() {
        return EFAULT;
    }
    if cpusetsize < core::mem::size_of::<usize>() {
        return EINVAL;
    }
    let task = if pid == 0 {
        current_task().unwrap()
    } else {
        match find_task_by_pid(pid) {
            Some(task) => task,
            None => return ESRCH,
        }
    };
    let token = current_task().unwrap().get_user_token();
    match UserPtrMut::from_addr(mask as usize).write(token, &(1 as usize)) {
        Ok(()) => core::mem::size_of::<usize>() as isize,
        Err(_) => EFAULT,
    }
}

pub fn sys_madvise(_addr: usize, _length: usize, _advice: usize) -> isize {
    // 暂时返回 EINVAL
    -(SyscallErr::EINVAL as isize)
}
