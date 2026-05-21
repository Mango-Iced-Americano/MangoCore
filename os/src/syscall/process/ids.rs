use crate::config::{PAGE_SIZE, SYSTEM_TASK_LIMIT, USER_STACK_SIZE};
use crate::mm::{
    copy_to_user, translated_byte_buffer, UserAccess, UserBuffer, UserPtr, UserPtrMut,
};
use crate::syscall::errno::*;
use crate::task::{current_task, current_user_token, ProcessManager, TaskControlBlock};
use crate::timer::{get_time_sec, TimeSpec};
use alloc::sync::Arc;
use core::mem::size_of;
use log::{info, warn};
use num_enum::FromPrimitive;

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
    let pid = current_task().unwrap().pid();
    pid as isize
}

pub fn sys_getppid() -> isize {
    let task = current_task().unwrap();
    task.process.parent_pid() as isize
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
    let process = if pid == 0 {
        current_task().unwrap().process.clone()
    } else {
        match ProcessManager::find_process(pid) {
            Some(process) => process,
            None => return ESRCH,
        }
    };

    let real_pgid = if pgid == 0 { process.pid } else { pgid };

    process.setpgid(real_pgid)
}

pub fn sys_getpgid(pid: usize) -> isize {
    if (pid as isize) < 0 {
        return EINVAL;
    }
    let process = if pid == 0 {
        current_task().unwrap().process.clone()
    } else {
        match ProcessManager::find_process(pid) {
            Some(process) => process,
            None => return ESRCH,
        }
    };

    process.getpgid() as isize
}
/// creates a new session if the calling process is not a process group leader.
/// The calling process is the leader of the new session, and its pgid is set to its pid.
/// 当前进程脱离父进程，从父进程的子进程列表中移除当前进程，当前进程的父进程设置为空。
pub fn sys_setsid() -> isize {
    let task = current_task().unwrap();
    let process = task.process.clone();
    if let Some(parent) = process.parent() {
        parent.detach_child(process.pid);
    }
    // Make this process a session leader and process group leader.
    process.set_parent(None);
    process.setpgid(process.pid);
    SUCCESS
}

pub fn sys_gettid() -> isize {
    current_task().unwrap().tid.0 as isize
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
    let procs = ProcessManager::process_count();
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
    if pid != 0 && pid != task.pid() {
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
            let files_ref = task.process.files();
            let lock = files_ref.lock();
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
                task.process.files().lock().set_soft_limit(rlimit.rlim_cur);
                task.process.files().lock().set_hard_limit(rlimit.rlim_max);
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
        match ProcessManager::find_task(pid) {
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

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SchedParam {
    sched_priority: i32,
}

fn find_task_for_pid_or_current(pid: usize) -> Result<Arc<TaskControlBlock>, isize> {
    if pid == 0 {
        current_task().ok_or(ESRCH)
    } else {
        ProcessManager::find_task(pid).ok_or(ESRCH)
    }
}

fn valid_sched_policy(policy: usize) -> bool {
    matches!(policy & !0x40000000, 0 | 1 | 2 | 3 | 5 | 6)
}

fn valid_sched_priority(policy: usize, priority: i32) -> bool {
    match policy & !0x40000000 {
        1 | 2 => (1..=99).contains(&priority),
        _ => priority == 0,
    }
}

pub fn sys_sched_setparam(pid: usize, param: *const SchedParam) -> isize {
    let task = match find_task_for_pid_or_current(pid) {
        Ok(task) => task,
        Err(errno) => return errno,
    };
    let param = match UserPtr::new(param).read(current_user_token()) {
        Ok(param) => param,
        Err(_) => return EFAULT,
    };
    let mut inner = task.acquire_inner_lock();
    if !valid_sched_priority(inner.sched_policy, param.sched_priority) {
        return EINVAL;
    }
    inner.sched_priority = param.sched_priority;
    SUCCESS
}

pub fn sys_sched_setscheduler(pid: usize, policy: usize, param: *const SchedParam) -> isize {
    if !valid_sched_policy(policy) {
        return EINVAL;
    }
    let task = match find_task_for_pid_or_current(pid) {
        Ok(task) => task,
        Err(errno) => return errno,
    };
    let param = match UserPtr::new(param).read(current_user_token()) {
        Ok(param) => param,
        Err(_) => return EFAULT,
    };
    if !valid_sched_priority(policy, param.sched_priority) {
        return EINVAL;
    }
    let mut inner = task.acquire_inner_lock();
    inner.sched_policy = policy & !0x40000000;
    inner.sched_priority = param.sched_priority;
    SUCCESS
}

pub fn sys_sched_getscheduler(pid: usize) -> isize {
    match find_task_for_pid_or_current(pid) {
        Ok(task) => task.acquire_inner_lock().sched_policy as isize,
        Err(errno) => errno,
    }
}

pub fn sys_sched_getparam(pid: usize, param: *mut SchedParam) -> isize {
    let task = match find_task_for_pid_or_current(pid) {
        Ok(task) => task,
        Err(errno) => return errno,
    };
    let sched_priority = task.acquire_inner_lock().sched_priority;
    match UserPtrMut::new(param).write(current_user_token(), &SchedParam { sched_priority }) {
        Ok(()) => SUCCESS,
        Err(_) => EFAULT,
    }
}

pub fn sys_sched_setaffinity(pid: usize, cpusetsize: usize, mask: *const u8) -> isize {
    if let Err(errno) = find_task_for_pid_or_current(pid) {
        return errno;
    }
    if cpusetsize == 0 || mask.is_null() {
        return EFAULT;
    }
    if let Err(errno) =
        translated_byte_buffer(current_user_token(), mask, cpusetsize, UserAccess::Read)
    {
        return errno;
    }
    SUCCESS
}

pub fn sys_sched_get_priority_max(policy: usize) -> isize {
    if !valid_sched_policy(policy) {
        return EINVAL;
    }
    match policy & !0x40000000 {
        1 | 2 => 99,
        _ => 0,
    }
}

pub fn sys_sched_get_priority_min(policy: usize) -> isize {
    if !valid_sched_policy(policy) {
        return EINVAL;
    }
    match policy & !0x40000000 {
        1 | 2 => 1,
        _ => 0,
    }
}

pub fn sys_sched_rr_get_interval(pid: usize, tp: *mut TimeSpec) -> isize {
    if let Err(errno) = find_task_for_pid_or_current(pid) {
        return errno;
    }
    let interval = TimeSpec {
        tv_sec: 0,
        tv_nsec: 10_000_000,
    };
    match UserPtrMut::new(tp).write(current_user_token(), &interval) {
        Ok(()) => SUCCESS,
        Err(_) => EFAULT,
    }
}

pub fn sys_get_mempolicy(
    mode: *mut i32,
    nodemask: *mut usize,
    maxnode: usize,
    _addr: usize,
    _flags: usize,
) -> isize {
    let token = current_user_token();
    if !mode.is_null()
        && UserPtrMut::new(mode)
            .write(token, &0i32)
            .is_err()
    {
        return EFAULT;
    }
    if !nodemask.is_null() && maxnode > 0 {
        if UserPtrMut::new(nodemask).write(token, &1usize).is_err() {
            return EFAULT;
        }
    }
    SUCCESS
}
