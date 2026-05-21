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

const LINUX_CAPABILITY_VERSION_1: u32 = 0x19980330;
const LINUX_CAPABILITY_VERSION_2: u32 = 0x20071026;
const LINUX_CAPABILITY_VERSION_3: u32 = 0x20080522;
const CAP_LAST_CAP: usize = 40;
const CAP_SETPCAP: usize = 8;
const CAP_FULL_SET: u64 = (1u64 << (CAP_LAST_CAP + 1)) - 1;
const PR_GET_DUMPABLE: usize = 3;
const PR_SET_DUMPABLE: usize = 4;
const PR_GET_KEEPCAPS: usize = 7;
const PR_SET_KEEPCAPS: usize = 8;
const PR_CAPBSET_READ: usize = 23;
const PR_CAPBSET_DROP: usize = 24;

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct CapUserHeader {
    version: u32,
    pid: i32,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct CapUserData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
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
    current_task().unwrap().acquire_inner_lock().uid as isize
}

pub fn sys_geteuid() -> isize {
    current_task().unwrap().acquire_inner_lock().euid as isize
}

pub fn sys_getgid() -> isize {
    current_task().unwrap().acquire_inner_lock().gid as isize
}

pub fn sys_getegid() -> isize {
    current_task().unwrap().acquire_inner_lock().egid as isize
}

fn parse_id(arg: usize) -> Result<u32, isize> {
    if arg > u32::MAX as usize {
        Err(EINVAL)
    } else {
        Ok(arg as u32)
    }
}

fn parse_optional_id(arg: usize) -> Result<Option<u32>, isize> {
    if arg == usize::MAX {
        Ok(None)
    } else {
        parse_id(arg).map(Some)
    }
}

fn refresh_effective_caps(euid: u32, cap_permitted: u64, cap_effective: &mut u64) {
    if euid == 0 {
        *cap_effective = cap_permitted;
    } else {
        *cap_effective = 0;
    }
}

pub fn sys_setuid(uid: usize) -> isize {
    let uid = match parse_id(uid) {
        Ok(uid) => uid,
        Err(errno) => return errno,
    };
    let task = current_task().unwrap();
    let mut inner = task.acquire_inner_lock();
    if inner.euid != 0 && uid != inner.uid && uid != inner.euid && uid != inner.suid {
        return EPERM;
    }
    if inner.euid == 0 {
        inner.uid = uid;
        inner.euid = uid;
        inner.suid = uid;
        inner.fsuid = uid;
    } else {
        inner.euid = uid;
        inner.fsuid = uid;
    }
    let cap_permitted = inner.cap_permitted;
    refresh_effective_caps(inner.euid, cap_permitted, &mut inner.cap_effective);
    SUCCESS
}

pub fn sys_setreuid(ruid: usize, euid: usize) -> isize {
    let ruid = match parse_optional_id(ruid) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    let euid = match parse_optional_id(euid) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    let task = current_task().unwrap();
    let mut inner = task.acquire_inner_lock();
    let privileged = inner.euid == 0;
    if !privileged {
        if let Some(id) = ruid {
            if id != inner.uid && id != inner.euid {
                return EPERM;
            }
        }
        if let Some(id) = euid {
            if id != inner.uid && id != inner.euid && id != inner.suid {
                return EPERM;
            }
        }
    }
    if let Some(id) = ruid {
        inner.uid = id;
    }
    if let Some(id) = euid {
        inner.euid = id;
        inner.fsuid = id;
    }
    if privileged || ruid.is_some() || euid.map_or(false, |id| id != inner.uid) {
        inner.suid = inner.euid;
    }
    let cap_permitted = inner.cap_permitted;
    refresh_effective_caps(inner.euid, cap_permitted, &mut inner.cap_effective);
    SUCCESS
}

pub fn sys_setresuid(ruid: usize, euid: usize, suid: usize) -> isize {
    let ruid = match parse_optional_id(ruid) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    let euid = match parse_optional_id(euid) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    let suid = match parse_optional_id(suid) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    let task = current_task().unwrap();
    let mut inner = task.acquire_inner_lock();
    if inner.euid != 0 {
        for id in [ruid, euid, suid].iter().copied().flatten() {
            if id != inner.uid && id != inner.euid && id != inner.suid {
                return EPERM;
            }
        }
    }
    if let Some(id) = ruid {
        inner.uid = id;
    }
    if let Some(id) = euid {
        inner.euid = id;
        inner.fsuid = id;
    }
    if let Some(id) = suid {
        inner.suid = id;
    }
    let cap_permitted = inner.cap_permitted;
    refresh_effective_caps(inner.euid, cap_permitted, &mut inner.cap_effective);
    SUCCESS
}

pub fn sys_getresuid(ruid: *mut u32, euid: *mut u32, suid: *mut u32) -> isize {
    let token = current_user_token();
    let task = current_task().unwrap();
    let inner = task.acquire_inner_lock();
    let values = [(ruid, inner.uid), (euid, inner.euid), (suid, inner.suid)];
    for (ptr, value) in values {
        if !ptr.is_null() {
            if let Err(errno) = UserPtrMut::new(ptr).write(token, &value) {
                return errno;
            }
        }
    }
    SUCCESS
}

pub fn sys_setgid(gid: usize) -> isize {
    let gid = match parse_id(gid) {
        Ok(gid) => gid,
        Err(errno) => return errno,
    };
    let task = current_task().unwrap();
    let mut inner = task.acquire_inner_lock();
    if inner.euid != 0 && gid != inner.gid && gid != inner.egid && gid != inner.sgid {
        return EPERM;
    }
    if inner.euid == 0 {
        inner.gid = gid;
        inner.egid = gid;
        inner.sgid = gid;
        inner.fsgid = gid;
    } else {
        inner.egid = gid;
        inner.fsgid = gid;
    }
    SUCCESS
}

pub fn sys_setregid(rgid: usize, egid: usize) -> isize {
    let rgid = match parse_optional_id(rgid) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    let egid = match parse_optional_id(egid) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    let task = current_task().unwrap();
    let mut inner = task.acquire_inner_lock();
    let privileged = inner.euid == 0;
    if !privileged {
        if let Some(id) = rgid {
            if id != inner.gid && id != inner.egid {
                return EPERM;
            }
        }
        if let Some(id) = egid {
            if id != inner.gid && id != inner.egid && id != inner.sgid {
                return EPERM;
            }
        }
    }
    if let Some(id) = rgid {
        inner.gid = id;
    }
    if let Some(id) = egid {
        inner.egid = id;
        inner.fsgid = id;
    }
    if privileged || rgid.is_some() || egid.map_or(false, |id| id != inner.gid) {
        inner.sgid = inner.egid;
    }
    SUCCESS
}

pub fn sys_setresgid(rgid: usize, egid: usize, sgid: usize) -> isize {
    let rgid = match parse_optional_id(rgid) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    let egid = match parse_optional_id(egid) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    let sgid = match parse_optional_id(sgid) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    let task = current_task().unwrap();
    let mut inner = task.acquire_inner_lock();
    if inner.euid != 0 {
        for id in [rgid, egid, sgid].iter().copied().flatten() {
            if id != inner.gid && id != inner.egid && id != inner.sgid {
                return EPERM;
            }
        }
    }
    if let Some(id) = rgid {
        inner.gid = id;
    }
    if let Some(id) = egid {
        inner.egid = id;
        inner.fsgid = id;
    }
    if let Some(id) = sgid {
        inner.sgid = id;
    }
    SUCCESS
}

pub fn sys_getresgid(rgid: *mut u32, egid: *mut u32, sgid: *mut u32) -> isize {
    let token = current_user_token();
    let task = current_task().unwrap();
    let inner = task.acquire_inner_lock();
    let values = [(rgid, inner.gid), (egid, inner.egid), (sgid, inner.sgid)];
    for (ptr, value) in values {
        if !ptr.is_null() {
            if let Err(errno) = UserPtrMut::new(ptr).write(token, &value) {
                return errno;
            }
        }
    }
    SUCCESS
}

pub fn sys_setfsuid(fsuid: usize) -> isize {
    let fsuid = match parse_id(fsuid) {
        Ok(fsuid) => fsuid,
        Err(_) => return current_task().unwrap().acquire_inner_lock().fsuid as isize,
    };
    let task = current_task().unwrap();
    let mut inner = task.acquire_inner_lock();
    let old = inner.fsuid;
    if inner.euid == 0 || fsuid == inner.uid || fsuid == inner.euid || fsuid == inner.suid {
        inner.fsuid = fsuid;
    }
    old as isize
}

pub fn sys_setfsgid(fsgid: usize) -> isize {
    let fsgid = match parse_id(fsgid) {
        Ok(fsgid) => fsgid,
        Err(_) => return current_task().unwrap().acquire_inner_lock().fsgid as isize,
    };
    let task = current_task().unwrap();
    let mut inner = task.acquire_inner_lock();
    let old = inner.fsgid;
    if inner.euid == 0 || fsgid == inner.gid || fsgid == inner.egid || fsgid == inner.sgid {
        inner.fsgid = fsgid;
    }
    old as isize
}

pub fn sys_getgroups(size: usize, list: *mut u32) -> isize {
    if size == 0 {
        return 0;
    }
    if list.is_null() {
        return EFAULT;
    }
    0
}

pub fn sys_setgroups(size: usize, list: *const u32) -> isize {
    if current_task().unwrap().acquire_inner_lock().euid != 0 {
        return EPERM;
    }
    if size > 0 {
        let token = current_user_token();
        if translated_byte_buffer(token, list as *const u8, size * size_of::<u32>(), UserAccess::Read)
            .is_err()
        {
            return EFAULT;
        }
    }
    SUCCESS
}

fn cap_words(version: u32) -> Option<usize> {
    match version {
        LINUX_CAPABILITY_VERSION_1 => Some(1),
        LINUX_CAPABILITY_VERSION_2 | LINUX_CAPABILITY_VERSION_3 => Some(2),
        _ => None,
    }
}

fn find_task_for_cap_pid(pid: i32) -> Result<Arc<TaskControlBlock>, isize> {
    if pid < 0 {
        return Err(EINVAL);
    }
    if pid == 0 {
        return Ok(current_task().unwrap());
    }
    let pid = pid as usize;
    if let Some(task) = ProcessManager::find_task(pid) {
        return Ok(task);
    }
    if let Some(process) = ProcessManager::find_process(pid) {
        if let Some(task) = process.any_live_thread() {
            return Ok(task);
        }
    }
    Err(ESRCH)
}

fn write_cap_data(
    token: usize,
    data: *mut CapUserData,
    words: usize,
    effective: u64,
    permitted: u64,
    inheritable: u64,
) -> Result<(), isize> {
    if data.is_null() {
        return Err(EFAULT);
    }
    let first = CapUserData {
        effective: effective as u32,
        permitted: permitted as u32,
        inheritable: inheritable as u32,
    };
    UserPtrMut::new(data).write(token, &first)?;
    if words == 2 {
        let second_ptr = (data as usize + size_of::<CapUserData>()) as *mut CapUserData;
        let second = CapUserData {
            effective: (effective >> 32) as u32,
            permitted: (permitted >> 32) as u32,
            inheritable: (inheritable >> 32) as u32,
        };
        UserPtrMut::new(second_ptr).write(token, &second)?;
    }
    Ok(())
}

fn read_cap_data(
    token: usize,
    data: *const CapUserData,
    words: usize,
) -> Result<(u64, u64, u64), isize> {
    if data.is_null() {
        return Err(EFAULT);
    }
    let first = UserPtr::new(data).read(token)?;
    let mut effective = first.effective as u64;
    let mut permitted = first.permitted as u64;
    let mut inheritable = first.inheritable as u64;
    if words == 2 {
        let second_ptr = (data as usize + size_of::<CapUserData>()) as *const CapUserData;
        let second = UserPtr::new(second_ptr).read(token)?;
        effective |= (second.effective as u64) << 32;
        permitted |= (second.permitted as u64) << 32;
        inheritable |= (second.inheritable as u64) << 32;
    }
    Ok((
        effective & CAP_FULL_SET,
        permitted & CAP_FULL_SET,
        inheritable & CAP_FULL_SET,
    ))
}

pub fn sys_capget(header: *mut CapUserHeader, data: *mut CapUserData) -> isize {
    let token = current_user_token();
    let mut header_value = match UserPtr::new(header as *const CapUserHeader).read(token) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    let words = match cap_words(header_value.version) {
        Some(words) => words,
        None => {
            header_value.version = LINUX_CAPABILITY_VERSION_3;
            let _ = UserPtrMut::new(header).write(token, &header_value);
            return EINVAL;
        }
    };
    let task = match find_task_for_cap_pid(header_value.pid) {
        Ok(task) => task,
        Err(errno) => return errno,
    };
    let inner = task.acquire_inner_lock();
    match write_cap_data(
        token,
        data,
        words,
        inner.cap_effective,
        inner.cap_permitted,
        inner.cap_inheritable,
    ) {
        Ok(()) => SUCCESS,
        Err(errno) => errno,
    }
}

pub fn sys_capset(header: *mut CapUserHeader, data: *const CapUserData) -> isize {
    let token = current_user_token();
    let mut header_value = match UserPtr::new(header as *const CapUserHeader).read(token) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    let words = match cap_words(header_value.version) {
        Some(words) => words,
        None => {
            header_value.version = LINUX_CAPABILITY_VERSION_3;
            let _ = UserPtrMut::new(header).write(token, &header_value);
            return EINVAL;
        }
    };
    let current = current_task().unwrap();
    let current_pid = current.pid() as i32;
    if header_value.pid != 0 && header_value.pid != current.tid.0 as i32 && header_value.pid != current_pid {
        return match find_task_for_cap_pid(header_value.pid) {
            Ok(_) => EPERM,
            Err(errno) => errno,
        };
    }
    let (new_effective, new_permitted, new_inheritable) = match read_cap_data(token, data, words) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    if new_effective & !new_permitted != 0 {
        return EPERM;
    }
    let mut inner = current.acquire_inner_lock();
    if new_permitted & !inner.cap_permitted != 0 {
        return EPERM;
    }
    let inheritable_limit = if inner.cap_effective & (1u64 << CAP_SETPCAP) != 0 {
        inner.cap_inheritable | inner.cap_permitted | inner.cap_bounding
    } else {
        inner.cap_inheritable | inner.cap_permitted
    };
    if new_inheritable & !inheritable_limit != 0 {
        return EPERM;
    }
    inner.cap_effective = new_effective & CAP_FULL_SET;
    inner.cap_permitted = new_permitted & CAP_FULL_SET;
    inner.cap_inheritable = new_inheritable & CAP_FULL_SET;
    SUCCESS
}

pub fn sys_prctl(option: usize, arg2: usize, _arg3: usize, _arg4: usize, _arg5: usize) -> isize {
    let task = current_task().unwrap();
    match option {
        PR_CAPBSET_READ => {
            if arg2 > CAP_LAST_CAP {
                return EINVAL;
            }
            let inner = task.acquire_inner_lock();
            ((inner.cap_bounding & (1u64 << arg2)) != 0) as isize
        }
        PR_CAPBSET_DROP => {
            if arg2 > CAP_LAST_CAP {
                return EINVAL;
            }
            let mut inner = task.acquire_inner_lock();
            if inner.euid != 0 && (inner.cap_effective & (1u64 << CAP_SETPCAP)) == 0 {
                return EPERM;
            }
            inner.cap_bounding &= !(1u64 << arg2);
            SUCCESS
        }
        PR_GET_KEEPCAPS => 0,
        PR_SET_KEEPCAPS => SUCCESS,
        PR_GET_DUMPABLE => 1,
        PR_SET_DUMPABLE => SUCCESS,
        _ => EINVAL,
    }
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
