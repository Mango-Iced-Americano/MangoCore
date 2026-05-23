use crate::config::{PAGE_SIZE, SYSTEM_TASK_LIMIT};
use crate::mm::{
    copy_to_user, translated_byte_buffer, UserAccess, UserBuffer, UserPtr, UserPtrMut,
};
use crate::syscall::errno::*;
use crate::task::{
    current_task, current_user_token, ProcessControlBlock, ProcessManager, TaskControlBlock,
};
use crate::timer::{get_time_sec, TimeSpec};
use alloc::{sync::Arc, vec::Vec};
use core::{mem::size_of, ptr};
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
const CAP_SYS_NICE: usize = 23;
const CAP_FULL_SET: u64 = (1u64 << (CAP_LAST_CAP + 1)) - 1;
const NGROUPS_MAX: usize = 65536;
const PR_GET_DUMPABLE: usize = 3;
const PR_SET_DUMPABLE: usize = 4;
const PR_GET_KEEPCAPS: usize = 7;
const PR_SET_KEEPCAPS: usize = 8;
const PR_CAPBSET_READ: usize = 23;
const PR_CAPBSET_DROP: usize = 24;
const PERSONALITY_GET: usize = 0xffff_ffff;
const IOPRIO_WHO_PROCESS: usize = 1;
const IOPRIO_CLASS_SHIFT: usize = 13;
const IOPRIO_PRIO_MASK: usize = (1 << IOPRIO_CLASS_SHIFT) - 1;
const IOPRIO_PRIO_NUM: usize = 8;
const IOPRIO_CLASS_NONE: usize = 0;
const IOPRIO_CLASS_RT: usize = 1;
const IOPRIO_CLASS_BE: usize = 2;
const IOPRIO_CLASS_IDLE: usize = 3;

pub fn sys_personality(persona: usize) -> isize {
    let task = current_task().unwrap();
    let mut inner = task.acquire_inner_lock();
    let old = inner.personality;
    if persona != PERSONALITY_GET && persona != usize::MAX {
        inner.personality = persona & PERSONALITY_GET;
    }
    old as isize
}

fn valid_ioprio(class: usize, prio: usize) -> bool {
    match class {
        IOPRIO_CLASS_NONE => prio == 0,
        IOPRIO_CLASS_RT | IOPRIO_CLASS_BE | IOPRIO_CLASS_IDLE => prio < IOPRIO_PRIO_NUM,
        _ => false,
    }
}

pub fn sys_ioprio_get(which: usize, who: usize) -> isize {
    if which != IOPRIO_WHO_PROCESS {
        return EINVAL;
    }
    let task = current_task().unwrap();
    if who != 0 && who != task.pid() {
        return ESRCH;
    }
    let inner = task.acquire_inner_lock();
    ((inner.ioprio_class << IOPRIO_CLASS_SHIFT) | inner.ioprio_prio) as isize
}

pub fn sys_ioprio_set(which: usize, who: usize, ioprio: usize) -> isize {
    if which != IOPRIO_WHO_PROCESS {
        return EINVAL;
    }
    let task = current_task().unwrap();
    if who != 0 && who != task.pid() {
        return ESRCH;
    }
    let class = ioprio >> IOPRIO_CLASS_SHIFT;
    let prio = ioprio & IOPRIO_PRIO_MASK;
    if !valid_ioprio(class, prio) {
        return EINVAL;
    }
    let mut inner = task.acquire_inner_lock();
    inner.ioprio_class = class;
    inner.ioprio_prio = prio;
    SUCCESS
}

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
    if arg == usize::MAX || arg == u32::MAX as usize {
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
    let task = current_task().unwrap();
    let groups = task.acquire_inner_lock().groups.clone();
    if size == 0 {
        return groups.len() as isize;
    }
    if size > NGROUPS_MAX || size < groups.len() {
        return EINVAL;
    }
    if list.is_null() {
        return EFAULT;
    }
    let token = current_user_token();
    for (idx, gid) in groups.iter().enumerate() {
        let ptr = (list as usize + idx * size_of::<u32>()) as *mut u32;
        if let Err(errno) = UserPtrMut::new(ptr).write(token, gid) {
            return errno;
        }
    }
    groups.len() as isize
}

pub fn sys_setgroups(size: usize, list: *const u32) -> isize {
    let task = current_task().unwrap();
    if task.acquire_inner_lock().euid != 0 {
        return EPERM;
    }
    if size > NGROUPS_MAX {
        return EINVAL;
    }
    let mut groups = Vec::new();
    if groups.try_reserve(size).is_err() {
        return ENOMEM;
    }
    if size > 0 {
        if list.is_null() {
            return EFAULT;
        }
        let token = current_user_token();
        for idx in 0..size {
            let ptr = (list as usize + idx * size_of::<u32>()) as *const u32;
            match UserPtr::new(ptr).read(token) {
                Ok(gid) => groups.push(gid),
                Err(errno) => return errno,
            }
        }
    }
    task.acquire_inner_lock().groups = groups;
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
        return ESRCH;
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

pub fn sys_getsid(pid: usize) -> isize {
    if (pid as isize) < 0 {
        return ESRCH;
    }
    let process = if pid == 0 {
        current_task().unwrap().process.clone()
    } else {
        match ProcessManager::find_process(pid) {
            Some(process) => process,
            None => return ESRCH,
        }
    };

    process.getsid() as isize
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
    process.setsid(process.pid);
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

fn rlimit_value_for(
    resource: Resource,
    nofile: Option<RLimit>,
    nice: Option<RLimit>,
    rtprio: Option<RLimit>,
    stack: Option<RLimit>,
) -> Option<RLimit> {
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
        | Resource::RTTIME
        | Resource::MEMLOCK => unlimited,
        Resource::NICE => nice?,
        Resource::RTPRIO => rtprio?,
        Resource::CORE => RLimit {
            rlim_cur: 0,
            rlim_max: 0,
        },
        Resource::STACK => stack?,
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
        let rtprio_limit = if resource == Resource::RTPRIO {
            let inner = task.acquire_inner_lock();
            Some(RLimit {
                rlim_cur: inner.rtprio_limit_cur,
                rlim_max: inner.rtprio_limit_max,
            })
        } else {
            None
        };
        let nice_limit = if resource == Resource::NICE {
            let inner = task.acquire_inner_lock();
            Some(RLimit {
                rlim_cur: inner.nice_limit_cur,
                rlim_max: inner.nice_limit_max,
            })
        } else {
            None
        };
        let stack_limit = if resource == Resource::STACK {
            let inner = task.acquire_inner_lock();
            Some(RLimit {
                rlim_cur: inner.stack_limit_cur,
                rlim_max: inner.stack_limit_max,
            })
        } else {
            None
        };
        let memlock_limit = if resource == Resource::MEMLOCK {
            let inner = task.acquire_inner_lock();
            Some(RLimit {
                rlim_cur: inner.memlock_limit_cur,
                rlim_max: inner.memlock_limit_max,
            })
        } else {
            None
        };
        let Some(mut limit) =
            rlimit_value_for(resource, nofile_limit, nice_limit, rtprio_limit, stack_limit)
        else {
            return EINVAL;
        };
        if let Some(value) = memlock_limit {
            limit = value;
        }
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
            Resource::RTPRIO => {
                let mut inner = task.acquire_inner_lock();
                inner.rtprio_limit_cur = rlimit.rlim_cur;
                inner.rtprio_limit_max = rlimit.rlim_max;
            }
            Resource::NICE => {
                let mut inner = task.acquire_inner_lock();
                inner.nice_limit_cur = rlimit.rlim_cur;
                inner.nice_limit_max = rlimit.rlim_max;
            }
            Resource::STACK => {
                let mut inner = task.acquire_inner_lock();
                inner.stack_limit_cur = rlimit.rlim_cur;
                inner.stack_limit_max = rlimit.rlim_max;
                warn!(
                    "[prlimit] Accept stack limit update as ABI state only: {:?}",
                    rlimit
                );
            }
            Resource::MEMLOCK => {
                let mut inner = task.acquire_inner_lock();
                inner.memlock_limit_cur = rlimit.rlim_cur;
                inner.memlock_limit_max = rlimit.rlim_max;
            }
            Resource::CPU
            | Resource::FSIZE
            | Resource::DATA
            | Resource::CORE
            | Resource::RSS
            | Resource::NPROC
            | Resource::AS
            | Resource::LOCKS
            | Resource::SIGPENDING
            | Resource::MSGQUEUE
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

pub fn sys_getrlimit(resource: u32, old_limit: *mut RLimit) -> isize {
    sys_prlimit(0, resource, ptr::null(), old_limit)
}

pub fn sys_setrlimit(resource: u32, new_limit: *const RLimit) -> isize {
    sys_prlimit(0, resource, new_limit, ptr::null_mut())
}

const PRIO_PROCESS: i32 = 0;
const PRIO_PGRP: i32 = 1;
const PRIO_USER: i32 = 2;

fn syscall_arg_i32(arg: usize) -> i32 {
    arg as u32 as i32
}

fn process_main_task(pid: usize) -> Option<Arc<TaskControlBlock>> {
    ProcessManager::find_process(pid)
        .and_then(|process| process.threads().into_iter().next())
        .or_else(|| ProcessManager::find_task(pid))
}

fn priority_targets(which: i32, who: i32) -> Result<Vec<Arc<TaskControlBlock>>, isize> {
    let current = current_task().unwrap();
    let mut targets = Vec::new();
    match which {
        PRIO_PROCESS => {
            if who == 0 {
                targets.push(current);
            } else if who < 0 {
                return Err(ESRCH);
            } else if let Some(task) = process_main_task(who as usize) {
                targets.push(task);
            }
        }
        PRIO_PGRP => {
            let pgid = if who == 0 {
                current.process.getpgid()
            } else if who < 0 {
                return Err(ESRCH);
            } else {
                who as usize
            };
            for process in ProcessManager::find_processes_by_pgid(pgid) {
                targets.extend(process.threads());
            }
        }
        PRIO_USER => {
            let uid = if who == 0 {
                current.acquire_inner_lock().euid
            } else if who < 0 {
                return Err(ESRCH);
            } else {
                who as u32
            };
            for process in ProcessManager::all_processes() {
                for task in process.threads() {
                    let inner = task.acquire_inner_lock();
                    let matches_user = inner.uid == uid || inner.euid == uid;
                    drop(inner);
                    if matches_user {
                        targets.push(task);
                    }
                }
            }
        }
        _ => return Err(EINVAL),
    }
    if targets.is_empty() {
        Err(ESRCH)
    } else {
        Ok(targets)
    }
}

fn set_task_nice(task: &Arc<TaskControlBlock>, nice: i32) {
    let state = {
        let mut inner = task.acquire_inner_lock();
        inner.sched_nice = nice;
        SchedState {
            policy: inner.sched_policy,
            priority: inner.sched_priority,
            reset_on_fork: inner.sched_reset_on_fork,
            nice: inner.sched_nice,
            runtime: inner.sched_runtime,
            deadline: inner.sched_deadline,
            period: inner.sched_period,
        }
    };
    sync_process_sched_state(task, state);
}

pub fn sys_getpriority(which: usize, who: usize) -> isize {
    let targets = match priority_targets(syscall_arg_i32(which), syscall_arg_i32(who)) {
        Ok(targets) => targets,
        Err(errno) => return errno,
    };
    let mut best_nice = i32::MAX;
    for task in targets {
        best_nice = best_nice.min(task.acquire_inner_lock().sched_nice);
    }
    (20 - best_nice) as isize
}

pub fn sys_setpriority(which: usize, who: usize, prio: usize) -> isize {
    let targets = match priority_targets(syscall_arg_i32(which), syscall_arg_i32(who)) {
        Ok(targets) => targets,
        Err(errno) => return errno,
    };
    let nice = syscall_arg_i32(prio).clamp(-20, 19);
    let access = current_sched_access();
    for task in &targets {
        if !access.has_sys_nice && !sched_same_owner(access, task) {
            return EPERM;
        }
        let old_nice = task.acquire_inner_lock().sched_nice;
        if nice < old_nice && !access.has_sys_nice {
            return EACCES;
        }
    }
    for task in targets {
        set_task_nice(&task, nice);
    }
    SUCCESS
}

pub fn sys_getcpu(cpu: *mut u32, node: *mut u32, _tcache: usize) -> isize {
    let token = current_user_token();
    if !cpu.is_null() {
        if let Err(errno) = UserPtrMut::new(cpu).write(token, &0u32) {
            return errno;
        }
    }
    if !node.is_null() {
        if let Err(errno) = UserPtrMut::new(node).write(token, &0u32) {
            return errno;
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

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SchedAttr {
    size: u32,
    sched_policy: u32,
    sched_flags: u64,
    sched_nice: i32,
    sched_priority: u32,
    sched_runtime: u64,
    sched_deadline: u64,
    sched_period: u64,
}

const SCHED_NORMAL: usize = 0;
const SCHED_FIFO: usize = 1;
const SCHED_RR: usize = 2;
const SCHED_BATCH: usize = 3;
const SCHED_IDLE: usize = 5;
const SCHED_DEADLINE: usize = 6;
const SCHED_RESET_ON_FORK: usize = 0x4000_0000;
const SCHED_FLAG_RESET_ON_FORK: u64 = 0x01;

#[derive(Clone, Copy)]
struct SchedAccess {
    euid: u32,
    has_sys_nice: bool,
    rtprio_limit_cur: usize,
}

#[derive(Clone, Copy)]
struct SchedState {
    policy: usize,
    priority: i32,
    reset_on_fork: bool,
    nice: i32,
    runtime: u64,
    deadline: u64,
    period: u64,
}

fn task_sched_state(task: &Arc<TaskControlBlock>) -> SchedState {
    let inner = task.acquire_inner_lock();
    SchedState {
        policy: inner.sched_policy,
        priority: inner.sched_priority,
        reset_on_fork: inner.sched_reset_on_fork,
        nice: inner.sched_nice,
        runtime: inner.sched_runtime,
        deadline: inner.sched_deadline,
        period: inner.sched_period,
    }
}

fn process_sched_state(process: &Arc<ProcessControlBlock>) -> SchedState {
    let (policy, priority, reset_on_fork, nice, runtime, deadline, period) =
        process.sched_state();
    SchedState {
        policy,
        priority,
        reset_on_fork,
        nice,
        runtime,
        deadline,
        period,
    }
}

fn sync_process_sched_state(task: &Arc<TaskControlBlock>, state: SchedState) {
    task.process.set_sched_state(
        state.policy,
        state.priority,
        state.reset_on_fork,
        state.nice,
        state.runtime,
        state.deadline,
        state.period,
    );
}

fn signed_pid_invalid(pid: usize) -> bool {
    (pid as isize) < 0
}

fn find_task_for_pid_or_current(pid: usize) -> Result<Arc<TaskControlBlock>, isize> {
    if let Some(task) = current_task() {
        if pid == 0 || pid == task.pid() {
            return Ok(task);
        }
    }
    if let Some(task) = ProcessManager::find_task(pid) {
        return Ok(task);
    }
    ProcessManager::find_process(pid)
        .and_then(|process| {
            process
                .threads()
                .into_iter()
                .find(|task| task.gettid() == pid || task.pid() == pid)
        })
        .ok_or(ESRCH)
}

fn find_sched_state_for_pid_or_current(pid: usize) -> Result<SchedState, isize> {
    if let Some(task) = current_task() {
        if pid == 0 || pid == task.pid() {
            return Ok(task_sched_state(&task));
        }
    }
    if let Some(task) = ProcessManager::find_task(pid) {
        return Ok(task_sched_state(&task));
    }
    if let Some(process) = ProcessManager::find_process(pid) {
        return Ok(process_sched_state(&process));
    }
    Err(ESRCH)
}

fn valid_sched_policy(policy: usize) -> bool {
    matches!(
        policy & !SCHED_RESET_ON_FORK,
        SCHED_NORMAL | SCHED_FIFO | SCHED_RR | SCHED_BATCH | SCHED_IDLE | SCHED_DEADLINE
    )
}

fn valid_sched_priority(policy: usize, priority: i32) -> bool {
    match policy & !SCHED_RESET_ON_FORK {
        SCHED_FIFO | SCHED_RR => (1..=99).contains(&priority),
        _ => priority == 0,
    }
}

fn current_sched_access() -> SchedAccess {
    let task = current_task().unwrap();
    let inner = task.acquire_inner_lock();
    SchedAccess {
        euid: inner.euid,
        has_sys_nice: inner.euid == 0 || (inner.cap_effective & (1u64 << CAP_SYS_NICE)) != 0,
        rtprio_limit_cur: inner.rtprio_limit_cur,
    }
}

fn sched_same_owner(access: SchedAccess, task: &Arc<TaskControlBlock>) -> bool {
    let inner = task.acquire_inner_lock();
    access.euid == inner.euid || access.euid == inner.uid
}

fn can_apply_sched_change(
    task: &Arc<TaskControlBlock>,
    old_policy: usize,
    old_priority: i32,
    old_reset_on_fork: bool,
    new_policy: usize,
    new_priority: i32,
    new_reset_on_fork: bool,
) -> bool {
    let access = current_sched_access();
    if access.has_sys_nice {
        return true;
    }

    let new_base_policy = new_policy & !SCHED_RESET_ON_FORK;
    if new_base_policy == SCHED_DEADLINE {
        return false;
    }

    if matches!(new_base_policy, SCHED_FIFO | SCHED_RR) {
        if new_base_policy != old_policy && access.rtprio_limit_cur == 0 {
            return false;
        }
        if new_priority > old_priority
            && (new_priority < 0 || new_priority as usize > access.rtprio_limit_cur)
        {
            return false;
        }
    }

    if old_reset_on_fork && !new_reset_on_fork {
        return false;
    }

    sched_same_owner(access, task)
}

pub fn sys_sched_setparam(pid: usize, param: *const SchedParam) -> isize {
    if signed_pid_invalid(pid) || param.is_null() {
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
    let (old_policy, old_priority, old_reset_on_fork) = {
        let inner = task.acquire_inner_lock();
        if !valid_sched_priority(inner.sched_policy, param.sched_priority) {
            return EINVAL;
        }
        (inner.sched_policy, inner.sched_priority, inner.sched_reset_on_fork)
    };
    if !can_apply_sched_change(
        &task,
        old_policy,
        old_priority,
        old_reset_on_fork,
        old_policy,
        param.sched_priority,
        old_reset_on_fork,
    ) {
        return EPERM;
    }
    let state = {
        let mut inner = task.acquire_inner_lock();
        inner.sched_priority = param.sched_priority;
        SchedState {
            policy: inner.sched_policy,
            priority: inner.sched_priority,
            reset_on_fork: inner.sched_reset_on_fork,
            nice: inner.sched_nice,
            runtime: inner.sched_runtime,
            deadline: inner.sched_deadline,
            period: inner.sched_period,
        }
    };
    sync_process_sched_state(&task, state);
    SUCCESS
}

pub fn sys_sched_setscheduler(pid: usize, policy: usize, param: *const SchedParam) -> isize {
    if signed_pid_invalid(pid) || param.is_null() {
        return EINVAL;
    }
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
    let base_policy = policy & !SCHED_RESET_ON_FORK;
    let new_reset_on_fork = policy & SCHED_RESET_ON_FORK != 0;
    let (old_policy, old_priority, old_reset_on_fork) = {
        let inner = task.acquire_inner_lock();
        (inner.sched_policy, inner.sched_priority, inner.sched_reset_on_fork)
    };
    if !can_apply_sched_change(
        &task,
        old_policy,
        old_priority,
        old_reset_on_fork,
        base_policy,
        param.sched_priority,
        new_reset_on_fork,
    ) {
        return EPERM;
    }
    let state = {
        let mut inner = task.acquire_inner_lock();
        inner.sched_policy = base_policy;
        inner.sched_priority = param.sched_priority;
        inner.sched_reset_on_fork = new_reset_on_fork;
        inner.sched_runtime = 0;
        inner.sched_deadline = 0;
        inner.sched_period = 0;
        SchedState {
            policy: inner.sched_policy,
            priority: inner.sched_priority,
            reset_on_fork: inner.sched_reset_on_fork,
            nice: inner.sched_nice,
            runtime: inner.sched_runtime,
            deadline: inner.sched_deadline,
            period: inner.sched_period,
        }
    };
    sync_process_sched_state(&task, state);
    SUCCESS
}

pub fn sys_sched_getscheduler(pid: usize) -> isize {
    if signed_pid_invalid(pid) {
        return EINVAL;
    }
    match find_sched_state_for_pid_or_current(pid) {
        Ok(state) => {
            (state.policy
                | if state.reset_on_fork {
                    SCHED_RESET_ON_FORK
                } else {
                    0
                }) as isize
        }
        Err(errno) => errno,
    }
}

pub fn sys_sched_getparam(pid: usize, param: *mut SchedParam) -> isize {
    if signed_pid_invalid(pid) || param.is_null() {
        return EINVAL;
    }
    let state = match find_sched_state_for_pid_or_current(pid) {
        Ok(state) => state,
        Err(errno) => return errno,
    };
    match UserPtrMut::new(param).write(
        current_user_token(),
        &SchedParam {
            sched_priority: state.priority,
        },
    ) {
        Ok(()) => SUCCESS,
        Err(_) => EFAULT,
    }
}

pub fn sys_sched_setaffinity(pid: usize, cpusetsize: usize, mask: *const u8) -> isize {
    if signed_pid_invalid(pid) {
        return EINVAL;
    }
    let task = match find_task_for_pid_or_current(pid) {
        Ok(task) => task,
        Err(errno) => return errno,
    };
    if mask.is_null() {
        return EFAULT;
    }
    if cpusetsize == 0 {
        return EINVAL;
    }
    let buffers = match translated_byte_buffer(
        current_user_token(),
        mask,
        cpusetsize,
        UserAccess::Read,
    ) {
        Ok(buffers) => buffers,
        Err(errno) => return errno,
    };
    let user = UserBuffer::new(buffers);
    let mut first = [0u8; 1];
    user.read(&mut first);
    if first[0] & 1 == 0 {
        return EINVAL;
    }
    let access = current_sched_access();
    if !access.has_sys_nice && !sched_same_owner(access, &task) {
        return EPERM;
    }
    SUCCESS
}

pub fn sys_sched_get_priority_max(policy: usize) -> isize {
    if !valid_sched_policy(policy) {
        return EINVAL;
    }
    match policy & !SCHED_RESET_ON_FORK {
        SCHED_FIFO | SCHED_RR => 99,
        _ => 0,
    }
}

pub fn sys_sched_get_priority_min(policy: usize) -> isize {
    if !valid_sched_policy(policy) {
        return EINVAL;
    }
    match policy & !SCHED_RESET_ON_FORK {
        SCHED_FIFO | SCHED_RR => 1,
        _ => 0,
    }
}

pub fn sys_sched_rr_get_interval(pid: usize, tp: *mut TimeSpec) -> isize {
    if signed_pid_invalid(pid) {
        return EINVAL;
    }
    let state = match find_sched_state_for_pid_or_current(pid) {
        Ok(state) => state,
        Err(errno) => return errno,
    };
    let is_round_robin = state.policy == SCHED_RR;
    let interval = TimeSpec {
        tv_sec: 0,
        tv_nsec: if is_round_robin { 100_000_000 } else { 0 },
    };
    match UserPtrMut::new(tp).write(current_user_token(), &interval) {
        Ok(()) => SUCCESS,
        Err(_) => EFAULT,
    }
}

pub fn sys_sched_setattr(pid: usize, attr: *const SchedAttr, flags: usize) -> isize {
    if signed_pid_invalid(pid) || attr.is_null() || flags != 0 {
        return EINVAL;
    }
    let task = match find_task_for_pid_or_current(pid) {
        Ok(task) => task,
        Err(errno) => return errno,
    };
    let attr = match UserPtr::new(attr).read(current_user_token()) {
        Ok(attr) => attr,
        Err(_) => return EFAULT,
    };
    if (attr.size as usize) < size_of::<SchedAttr>()
        || !valid_sched_policy(attr.sched_policy as usize)
        || attr.sched_flags & !SCHED_FLAG_RESET_ON_FORK != 0
    {
        return EINVAL;
    }
    let policy = attr.sched_policy as usize;
    let priority = attr.sched_priority as i32;
    if !valid_sched_priority(policy, priority) {
        return EINVAL;
    }
    let base_policy = policy & !SCHED_RESET_ON_FORK;
    let new_reset_on_fork =
        policy & SCHED_RESET_ON_FORK != 0 || attr.sched_flags & SCHED_FLAG_RESET_ON_FORK != 0;
    let (old_policy, old_priority, old_reset_on_fork) = {
        let inner = task.acquire_inner_lock();
        (inner.sched_policy, inner.sched_priority, inner.sched_reset_on_fork)
    };
    if !can_apply_sched_change(
        &task,
        old_policy,
        old_priority,
        old_reset_on_fork,
        base_policy,
        priority,
        new_reset_on_fork,
    ) {
        return EPERM;
    }
    let state = {
        let mut inner = task.acquire_inner_lock();
        inner.sched_policy = base_policy;
        inner.sched_priority = priority;
        inner.sched_reset_on_fork = new_reset_on_fork;
        inner.sched_nice = attr.sched_nice;
        inner.sched_runtime = attr.sched_runtime;
        inner.sched_deadline = attr.sched_deadline;
        inner.sched_period = attr.sched_period;
        SchedState {
            policy: inner.sched_policy,
            priority: inner.sched_priority,
            reset_on_fork: inner.sched_reset_on_fork,
            nice: inner.sched_nice,
            runtime: inner.sched_runtime,
            deadline: inner.sched_deadline,
            period: inner.sched_period,
        }
    };
    sync_process_sched_state(&task, state);
    SUCCESS
}

pub fn sys_sched_getattr(pid: usize, attr: *mut SchedAttr, size: usize, flags: usize) -> isize {
    if signed_pid_invalid(pid) || attr.is_null() || size < size_of::<SchedAttr>() || flags != 0 {
        return EINVAL;
    }
    let state = match find_sched_state_for_pid_or_current(pid) {
        Ok(state) => state,
        Err(errno) => return errno,
    };
    let sched_policy = state.policy
        | if state.reset_on_fork {
            SCHED_RESET_ON_FORK
        } else {
            0
        };
    let attr_value = SchedAttr {
        size: size_of::<SchedAttr>() as u32,
        sched_policy: sched_policy as u32,
        sched_flags: if state.reset_on_fork {
            SCHED_FLAG_RESET_ON_FORK
        } else {
            0
        },
        sched_nice: state.nice,
        sched_priority: state.priority as u32,
        sched_runtime: state.runtime,
        sched_deadline: state.deadline,
        sched_period: state.period,
    };
    match UserPtrMut::new(attr).write(current_user_token(), &attr_value) {
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
