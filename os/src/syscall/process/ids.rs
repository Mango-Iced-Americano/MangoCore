use crate::config::PAGE_SIZE;
use crate::fs::iov::IOVec;
use crate::mm::{
    check_user_range, copy_from_user, copy_from_user_array, copy_to_user, translated_byte_buffer,
    AddressSpace, FaultAccess, MapPermission, PageTableImpl, StepByOne, UserAccess, UserBuffer,
    UserPtr, UserPtrMut, VirtAddr,
};
use crate::syscall::errno::*;
use crate::task::{
    current_egid, current_euid, current_gid, current_parent_pid, current_pid, current_task,
    current_task_ref, current_tid, current_uid, current_user_token, update_ready_nice,
    ProcessControlBlock, ProcessManager, SeccompFilterInsn, Signals, TaskControlBlock,
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
const CAP_SYS_PTRACE: usize = 19;
const CAP_SYS_ADMIN: usize = 21;
const CAP_SYS_NICE: usize = 23;
const CAP_SYS_RESOURCE: usize = 24;
const CAP_SYS_TTY_CONFIG: usize = 26;
const CAP_FULL_SET: u64 = (1u64 << (CAP_LAST_CAP + 1)) - 1;
const NGROUPS_MAX: usize = 65536;
const LEGACY_NGROUPS_MAX: usize = 32;
const RLIMIT_NOFILE_MAX: usize = 1024 * 1024;
const PR_SET_PDEATHSIG: usize = 1;
const PR_GET_PDEATHSIG: usize = 2;
const PR_GET_DUMPABLE: usize = 3;
const PR_SET_DUMPABLE: usize = 4;
const PR_GET_KEEPCAPS: usize = 7;
const PR_SET_KEEPCAPS: usize = 8;
const PR_SET_NAME: usize = 15;
const PR_GET_NAME: usize = 16;
const PR_GET_SECCOMP: usize = 21;
const PR_SET_SECCOMP: usize = 22;
const PR_CAPBSET_READ: usize = 23;
const PR_CAPBSET_DROP: usize = 24;
const PR_GET_SECUREBITS: usize = 27;
const PR_SET_SECUREBITS: usize = 28;
const PR_SET_TIMERSLACK: usize = 29;
const PR_GET_TIMERSLACK: usize = 30;
const PR_SET_CHILD_SUBREAPER: usize = 36;
const PR_GET_CHILD_SUBREAPER: usize = 37;
const PR_SET_NO_NEW_PRIVS: usize = 38;
const PR_GET_NO_NEW_PRIVS: usize = 39;
const PR_SET_THP_DISABLE: usize = 41;
const PR_GET_THP_DISABLE: usize = 42;
const PR_CAP_AMBIENT: usize = 47;
const PR_CAP_AMBIENT_IS_SET: usize = 1;
const PR_CAP_AMBIENT_RAISE: usize = 2;
const PR_CAP_AMBIENT_LOWER: usize = 3;
const PR_CAP_AMBIENT_CLEAR_ALL: usize = 4;
const PR_GET_SPECULATION_CTRL: usize = 52;
const PR_TASK_COMM_LEN: usize = 16;
const PR_MAX_SIGNAL: usize = 64;
const PTRACE_TRACEME: usize = 0;
const PTRACE_CONT: usize = 7;
const PTRACE_KILL: usize = 8;
const PTRACE_ATTACH: usize = 16;
const PTRACE_DETACH: usize = 17;
const SECCOMP_MODE_DISABLED: usize = 0;
const SECCOMP_MODE_STRICT: usize = 1;
const SECCOMP_MODE_FILTER: usize = 2;
const SECCOMP_FILTER_MAX_LEN: usize = 4096;
const SECCOMP_RET_ACTION_FULL: u32 = 0xffff_0000;
const SECCOMP_RET_KILL_THREAD: u32 = 0x0000_0000;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const BPF_CLASS_MASK: u16 = 0x07;
const BPF_LD: u16 = 0x00;
const BPF_JMP: u16 = 0x05;
const BPF_RET: u16 = 0x06;
const BPF_SIZE_MASK: u16 = 0x18;
const BPF_W: u16 = 0x00;
const BPF_MODE_MASK: u16 = 0xe0;
const BPF_ABS: u16 = 0x20;
const BPF_OP_MASK: u16 = 0xf0;
const BPF_JEQ: u16 = 0x10;
const BPF_SRC_MASK: u16 = 0x08;
const BPF_K: u16 = 0x00;
const SECCOMP_DATA_NR_OFFSET: u32 = 0;
const SECBIT_NO_CAP_AMBIENT_RAISE: usize = 1 << 6;
const PR_SPEC_STORE_BYPASS: usize = 0;
const PROCESS_VM_MAX_IOVEC: usize = 1024;
const PROCESS_VM_MAX_COPY: usize = 8 * 1024 * 1024;
const PERSONALITY_GET: usize = 0xffff_ffff;
const IOPRIO_WHO_PROCESS: usize = 1;
const IOPRIO_CLASS_SHIFT: usize = 13;
const IOPRIO_PRIO_MASK: usize = (1 << IOPRIO_CLASS_SHIFT) - 1;
const IOPRIO_PRIO_NUM: usize = 8;
const IOPRIO_CLASS_NONE: usize = 0;
const IOPRIO_CLASS_RT: usize = 1;
const IOPRIO_CLASS_BE: usize = 2;
const IOPRIO_CLASS_IDLE: usize = 3;

fn ptrace_traceme_target(pid: usize) -> Result<Arc<ProcessControlBlock>, isize> {
    let current = current_task().unwrap();
    let target = ProcessManager::find_process(pid).ok_or(ESRCH)?;
    if target.parent_pid() != current.pid() {
        return Err(ESRCH);
    }
    let traced = target
        .any_live_thread()
        .map(|task| task.acquire_inner_lock().ptrace_traceme)
        .unwrap_or(false);
    if traced {
        Ok(target)
    } else {
        Err(ESRCH)
    }
}

pub fn sys_personality(persona: usize) -> isize {
    let task = current_task_ref().unwrap();
    let mut inner = task.acquire_inner_lock();
    let old = inner.personality;
    if persona != PERSONALITY_GET && persona != usize::MAX {
        inner.personality = persona & PERSONALITY_GET;
    }
    old as isize
}

pub fn sys_ptrace(request: usize, pid: usize, _addr: usize, _data: usize) -> isize {
    match request {
        PTRACE_TRACEME => {
            let task = current_task().unwrap();
            let mut inner = task.acquire_inner_lock();
            if inner.ptrace_traceme {
                return EPERM;
            }
            inner.ptrace_traceme = true;
            SUCCESS
        }
        PTRACE_CONT => match ptrace_traceme_target(pid) {
            Ok(process) => {
                crate::task::signal::send_process_signal(&process, Signals::SIGCONT);
                SUCCESS
            }
            Err(errno) => errno,
        },
        PTRACE_KILL => match ptrace_traceme_target(pid) {
            Ok(process) => {
                crate::task::signal::send_process_signal(&process, Signals::SIGKILL);
                SUCCESS
            }
            Err(errno) => errno,
        },
        PTRACE_ATTACH => {
            let task = current_task().unwrap();
            if pid == task.pid() {
                return EPERM;
            }
            let target = match ProcessManager::find_process(pid) {
                Some(process) => process,
                None => return ESRCH,
            };
            if task.euid() != 0 {
                return EPERM;
            }
            match target.ptrace_attach(task.pid(), 19) {
                Ok(()) => SUCCESS,
                Err(errno) => -(errno as isize),
            }
        }
        PTRACE_DETACH => {
            let task = current_task().unwrap();
            let target = match ProcessManager::find_process(pid) {
                Some(process) => process,
                None => return ESRCH,
            };
            match target.ptrace_detach(task.pid()) {
                Ok(()) => SUCCESS,
                Err(errno) => -(errno as isize),
            }
        }
        _ => EIO,
    }
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
    let task = current_task_ref().unwrap();
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
    let task = current_task_ref().unwrap();
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

pub fn sys_vhangup() -> isize {
    let task = current_task_ref().unwrap();
    let inner = task.acquire_inner_lock();
    if inner.euid == 0 || (inner.cap_effective & (1u64 << CAP_SYS_TTY_CONFIG)) != 0 {
        SUCCESS
    } else {
        EPERM
    }
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
    buffer.write_at(FIELD_OFFSET * 0, b"Linux\0");
    #[cfg(feature = "riscv")]
    buffer.write_at(FIELD_OFFSET * 2, b"5.10.0-1-rv64\0");
    #[cfg(feature = "loongarch64")]
    buffer.write_at(FIELD_OFFSET * 2, b"5.10.0-1-la64\0");
    buffer.write_at(FIELD_OFFSET * 3, b"#1 SMP blossom 5.10.0-1 (2025-01-10)\0");
    #[cfg(feature = "riscv")]
    buffer.write_at(FIELD_OFFSET * 4, b"rv64\0");
    #[cfg(feature = "loongarch64")]
    buffer.write_at(FIELD_OFFSET * 4, b"la64\0");
    let task = current_task_ref().unwrap();
    let uts_ref = task.process.uts();
    let uts = uts_ref.lock();
    buffer.write_at(FIELD_OFFSET * 1, &uts.nodename[..]);
    buffer.write_at(FIELD_OFFSET * 5, &uts.domainname[..]);
    SUCCESS
}

fn copy_uts_field(name: *const u8, len: usize) -> Result<[u8; 65], isize> {
    const UTS_FIELD_LEN: usize = 65;
    const UTS_NAME_MAX: usize = UTS_FIELD_LEN - 1;

    if len > UTS_NAME_MAX {
        return Err(EINVAL);
    }
    if len > 0 && name.is_null() {
        return Err(EFAULT);
    }
    let mut field = [0u8; UTS_FIELD_LEN];
    if len > 0 {
        copy_from_user_array(current_user_token(), name, field.as_mut_ptr(), len)?;
    }
    Ok(field)
}

pub fn sys_sethostname(name: *const u8, len: usize) -> isize {
    let task = current_task_ref().unwrap();
    if task.euid() != 0 {
        return EPERM;
    }
    let hostname = match copy_uts_field(name, len) {
        Ok(hostname) => hostname,
        Err(errno) => return errno,
    };
    let uts_ref = task.process.uts();
    uts_ref.lock().nodename = hostname;
    SUCCESS
}

pub fn sys_setdomainname(name: *const u8, len: usize) -> isize {
    let task = current_task_ref().unwrap();
    if task.euid() != 0 {
        return EPERM;
    }
    let domainname = match copy_uts_field(name, len) {
        Ok(domainname) => domainname,
        Err(errno) => return errno,
    };
    let uts_ref = task.process.uts();
    uts_ref.lock().domainname = domainname;
    SUCCESS
}

pub fn sys_getpid() -> isize {
    current_pid() as isize
}

pub fn sys_getppid() -> isize {
    current_parent_pid() as isize
}

pub fn sys_getuid() -> isize {
    current_uid() as isize
}

pub fn sys_geteuid() -> isize {
    current_euid() as isize
}

pub fn sys_getgid() -> isize {
    current_gid() as isize
}

pub fn sys_getegid() -> isize {
    current_egid() as isize
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
    let task = current_task_ref().unwrap();
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
    task.store_identity_hint(
        inner.uid, inner.euid, inner.suid, inner.gid, inner.egid, inner.sgid,
    );
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
    let task = current_task_ref().unwrap();
    let mut inner = task.acquire_inner_lock();
    let privileged = inner.euid == 0;
    let old_uid = inner.uid;
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
    if ruid.is_some() || euid.map_or(false, |id| id != old_uid) {
        inner.suid = inner.euid;
    }
    let cap_permitted = inner.cap_permitted;
    refresh_effective_caps(inner.euid, cap_permitted, &mut inner.cap_effective);
    task.store_identity_hint(
        inner.uid, inner.euid, inner.suid, inner.gid, inner.egid, inner.sgid,
    );
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
    let task = current_task_ref().unwrap();
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
    task.store_identity_hint(
        inner.uid, inner.euid, inner.suid, inner.gid, inner.egid, inner.sgid,
    );
    SUCCESS
}

pub fn sys_getresuid(ruid: *mut u32, euid: *mut u32, suid: *mut u32) -> isize {
    let token = current_user_token();
    let task = current_task_ref().unwrap();
    let values = [(ruid, task.uid()), (euid, task.euid()), (suid, task.suid())];
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
    let task = current_task_ref().unwrap();
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
    task.store_identity_hint(
        inner.uid, inner.euid, inner.suid, inner.gid, inner.egid, inner.sgid,
    );
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
    let task = current_task_ref().unwrap();
    let mut inner = task.acquire_inner_lock();
    let privileged = inner.euid == 0;
    let old_gid = inner.gid;
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
    if rgid.is_some() || egid.map_or(false, |id| id != old_gid) {
        inner.sgid = inner.egid;
    }
    task.store_identity_hint(
        inner.uid, inner.euid, inner.suid, inner.gid, inner.egid, inner.sgid,
    );
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
    let task = current_task_ref().unwrap();
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
    task.store_identity_hint(
        inner.uid, inner.euid, inner.suid, inner.gid, inner.egid, inner.sgid,
    );
    SUCCESS
}

pub fn sys_getresgid(rgid: *mut u32, egid: *mut u32, sgid: *mut u32) -> isize {
    let token = current_user_token();
    let task = current_task_ref().unwrap();
    let values = [(rgid, task.gid()), (egid, task.egid()), (sgid, task.sgid())];
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
    let fsuid = match parse_optional_id(fsuid) {
        Ok(Some(fsuid)) => fsuid,
        Ok(None) | Err(_) => return current_task_ref().unwrap().acquire_inner_lock().fsuid as isize,
    };
    let task = current_task_ref().unwrap();
    let mut inner = task.acquire_inner_lock();
    let old = inner.fsuid;
    if inner.euid == 0 || fsuid == inner.uid || fsuid == inner.euid || fsuid == inner.suid {
        inner.fsuid = fsuid;
    }
    old as isize
}

pub fn sys_setfsgid(fsgid: usize) -> isize {
    let fsgid = match parse_optional_id(fsgid) {
        Ok(Some(fsgid)) => fsgid,
        Ok(None) | Err(_) => return current_task_ref().unwrap().acquire_inner_lock().fsgid as isize,
    };
    let task = current_task_ref().unwrap();
    let mut inner = task.acquire_inner_lock();
    let old = inner.fsgid;
    if inner.euid == 0 || fsgid == inner.gid || fsgid == inner.egid || fsgid == inner.sgid {
        inner.fsgid = fsgid;
    }
    old as isize
}

pub fn sys_getgroups(size: usize, list: *mut u32) -> isize {
    let task = current_task_ref().unwrap();
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
    let task = current_task_ref().unwrap();
    if current_euid() != 0 {
        return EPERM;
    }
    if size > NGROUPS_MAX {
        return EINVAL;
    }
    if size > 0 {
        if list.is_null() {
            return EFAULT;
        }
        let byte_len = match size.checked_mul(size_of::<u32>()) {
            Some(len) => len,
            None => return EFAULT,
        };
        if !task
            .process
            .vm()
            .lock()
            .contains_valid_buffer(list as usize, byte_len, MapPermission::R)
        {
            return EFAULT;
        }
        if size > LEGACY_NGROUPS_MAX {
            return EINVAL;
        }
    }
    let mut groups = Vec::new();
    if groups.try_reserve(size).is_err() {
        return ENOMEM;
    }
    if size > 0 {
        let token = current_user_token();
        for idx in 0..size {
            let ptr = (list as usize + idx * size_of::<u32>()) as *const u32;
            match UserPtr::new(ptr).read(token) {
                Ok(gid) => groups.push(gid),
                Err(errno) => return errno,
            }
        }
    }
    task.acquire_inner_lock().groups = Arc::new(groups);
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
    let (effective, permitted, inheritable) = if header_value.pid == 0 {
        let task = current_task_ref().unwrap();
        let inner = task.acquire_inner_lock();
        (
            inner.cap_effective,
            inner.cap_permitted,
            inner.cap_inheritable,
        )
    } else {
        let task = match find_task_for_cap_pid(header_value.pid) {
            Ok(task) => task,
            Err(errno) => return errno,
        };
        let inner = task.acquire_inner_lock();
        (
            inner.cap_effective,
            inner.cap_permitted,
            inner.cap_inheritable,
        )
    };
    match write_cap_data(
        token,
        data,
        words,
        effective,
        permitted,
        inheritable,
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
    let current = current_task_ref().unwrap();
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
    inner.cap_ambient &= inner.cap_permitted & inner.cap_inheritable;
    SUCCESS
}

fn read_prctl_comm_from_user(ptr: usize) -> Result<[u8; PR_TASK_COMM_LEN], isize> {
    let token = current_user_token();
    let buffer = UserBuffer::new(translated_byte_buffer(
        token,
        ptr as *const u8,
        PR_TASK_COMM_LEN,
        UserAccess::Read,
    )?);
    let mut comm = [0u8; PR_TASK_COMM_LEN];
    buffer.read(&mut comm);
    if let Some(nul_pos) = comm.iter().position(|&ch| ch == 0) {
        for byte in &mut comm[nul_pos..] {
            *byte = 0;
        }
    } else {
        comm[PR_TASK_COMM_LEN - 1] = 0;
    }
    Ok(comm)
}

fn write_prctl_comm_to_user(ptr: usize, comm: &[u8; PR_TASK_COMM_LEN]) -> isize {
    let token = current_user_token();
    let mut buffer = match translated_byte_buffer(
        token,
        ptr as *const u8,
        PR_TASK_COMM_LEN,
        UserAccess::Write,
    ) {
        Ok(buffer) => UserBuffer::new(buffer),
        Err(errno) => return errno,
    };
    buffer.write(comm);
    SUCCESS
}

fn read_process_vm_iovecs(iov: *const IOVec, iovcnt: usize) -> Result<Vec<IOVec>, isize> {
    if iovcnt > PROCESS_VM_MAX_IOVEC {
        return Err(EINVAL);
    }
    let mut iovecs = Vec::<IOVec>::new();
    iovecs.try_reserve(iovcnt).map_err(|_| ENOMEM)?;
    if iovcnt != 0 {
        copy_from_user_array(current_user_token(), iov, iovecs.as_mut_ptr(), iovcnt)?;
    }
    unsafe {
        iovecs.set_len(iovcnt);
    }
    Ok(iovecs)
}

fn process_vm_iov_total(iovecs: &[IOVec]) -> Result<usize, isize> {
    let mut total = 0usize;
    for iov in iovecs {
        total = total
            .checked_add(iov.iov_len)
            .filter(|len| *len <= isize::MAX as usize)
            .ok_or(EINVAL)?;
    }
    Ok(total)
}

fn for_process_vm_iov_chunks<F>(
    process: &Arc<ProcessControlBlock>,
    iovecs: &[IOVec],
    cap: usize,
    access: FaultAccess,
    mut f: F,
) -> Result<(), isize>
where
    F: FnMut(&mut [u8]) -> Result<(), isize>,
{
    let vm_ref = process.vm();
    let mut vm = vm_ref.lock();
    let mut total = 0usize;
    for iov in iovecs {
        if total >= cap {
            break;
        }
        let len = iov.iov_len.min(cap - total);
        append_process_vm_iov_chunks(&mut vm, iov.iov_base, len, access, &mut f)?;
        total += len;
    }
    Ok(())
}

fn append_process_vm_iov_chunks<F>(
    vm: &mut AddressSpace<PageTableImpl>,
    ptr: *const u8,
    len: usize,
    access: FaultAccess,
    f: &mut F,
) -> Result<(), isize>
where
    F: FnMut(&mut [u8]) -> Result<(), isize>,
{
    if len == 0 {
        return Ok(());
    }
    let mut start = ptr as usize;
    let end = check_user_range(start, len)?;
    while start < end {
        let start_va = VirtAddr::from(start);
        let pa = vm.fault_in_user_va(start_va, access)?;
        let ppn = pa.floor();
        let mut next_vpn = start_va.floor();
        next_vpn.step();
        let mut end_va: VirtAddr = next_vpn.into();
        end_va = end_va.min(VirtAddr::from(end));
        let chunk_end = if end_va.page_offset() == 0 {
            PAGE_SIZE
        } else {
            end_va.page_offset()
        };
        f(&mut ppn.get_bytes_array()[start_va.page_offset()..chunk_end])?;
        start = end_va.into();
    }
    Ok(())
}

fn copy_process_vm_iovecs_to_slice(
    process: &Arc<ProcessControlBlock>,
    iovecs: &[IOVec],
    cap: usize,
    dst: &mut [u8],
) -> Result<(), isize> {
    let mut copied = 0usize;
    for_process_vm_iov_chunks(process, iovecs, cap, FaultAccess::Load, |chunk| {
        let end = copied + chunk.len();
        dst[copied..end].copy_from_slice(chunk);
        copied = end;
        Ok(())
    })
}

fn copy_slice_to_process_vm_iovecs(
    src: &[u8],
    process: &Arc<ProcessControlBlock>,
    iovecs: &[IOVec],
    cap: usize,
) -> Result<(), isize> {
    let mut copied = 0usize;
    for_process_vm_iov_chunks(process, iovecs, cap, FaultAccess::Store, |chunk| {
        let end = copied + chunk.len();
        chunk.copy_from_slice(&src[copied..end]);
        copied = end;
        Ok(())
    })
}

fn check_process_vm_access(
    current_process: &Arc<ProcessControlBlock>,
    target_process: &Arc<ProcessControlBlock>,
) -> Result<(), isize> {
    if current_process.pid == target_process.pid {
        return Ok(());
    }
    let current = current_task_ref().unwrap();
    let (uid, euid, suid, gid, egid, sgid, cap_effective) = {
        let inner = current.acquire_inner_lock();
        (
            inner.uid,
            inner.euid,
            inner.suid,
            inner.gid,
            inner.egid,
            inner.sgid,
            inner.cap_effective,
        )
    };
    let Some(target) = target_process.any_live_thread() else {
        return Err(ESRCH);
    };
    let (target_uid, target_euid, target_suid, target_gid, target_egid, target_sgid, dumpable) = {
        let inner = target.acquire_inner_lock();
        (
            inner.uid,
            inner.euid,
            inner.suid,
            inner.gid,
            inner.egid,
            inner.sgid,
            inner.dumpable,
        )
    };
    let privileged = euid == 0 || (cap_effective & (1u64 << CAP_SYS_PTRACE)) != 0;
    let same_creds = uid == target_uid
        && euid == target_euid
        && suid == target_suid
        && gid == target_gid
        && egid == target_egid
        && sgid == target_sgid;
    if privileged || (same_creds && dumpable != 0) {
        Ok(())
    } else {
        Err(EPERM)
    }
}

fn sys_process_vm_transfer(
    pid: usize,
    local_iov: *const IOVec,
    liovcnt: usize,
    remote_iov: *const IOVec,
    riovcnt: usize,
    flags: usize,
    write_remote: bool,
) -> isize {
    if flags != 0 {
        return EINVAL;
    }
    let local_iovecs = match read_process_vm_iovecs(local_iov, liovcnt) {
        Ok(iovecs) => iovecs,
        Err(errno) => return errno,
    };
    let remote_iovecs = match read_process_vm_iovecs(remote_iov, riovcnt) {
        Ok(iovecs) => iovecs,
        Err(errno) => return errno,
    };
    let local_total = match process_vm_iov_total(&local_iovecs) {
        Ok(total) => total,
        Err(errno) => return errno,
    };
    let remote_total = match process_vm_iov_total(&remote_iovecs) {
        Ok(total) => total,
        Err(errno) => return errno,
    };
    let copy_len = local_total.min(remote_total);
    if copy_len == 0 {
        return 0;
    }
    if copy_len > PROCESS_VM_MAX_COPY {
        return EFAULT;
    }
    let remote_process = match ProcessManager::find_process(pid) {
        Some(process) => process,
        None => return ESRCH,
    };
    let current_process = current_task_ref().unwrap().process.clone();
    if let Err(errno) = check_process_vm_access(&current_process, &remote_process) {
        return errno;
    }
    let mut scratch = Vec::new();
    if scratch.try_reserve(copy_len).is_err() {
        return ENOMEM;
    }
    unsafe {
        scratch.set_len(copy_len);
    }

    if write_remote {
        match copy_process_vm_iovecs_to_slice(&current_process, &local_iovecs, copy_len, &mut scratch)
        {
            Ok(()) => {}
            Err(errno) => return errno,
        }
        match copy_slice_to_process_vm_iovecs(&scratch, &remote_process, &remote_iovecs, copy_len) {
            Ok(()) => {}
            Err(errno) => return errno,
        }
    } else {
        match copy_process_vm_iovecs_to_slice(&remote_process, &remote_iovecs, copy_len, &mut scratch)
        {
            Ok(()) => {}
            Err(errno) => return errno,
        }
        match copy_slice_to_process_vm_iovecs(&scratch, &current_process, &local_iovecs, copy_len) {
            Ok(()) => {}
            Err(errno) => return errno,
        }
    }
    copy_len as isize
}

pub fn sys_process_vm_readv(
    pid: usize,
    local_iov: *const IOVec,
    liovcnt: usize,
    remote_iov: *const IOVec,
    riovcnt: usize,
    flags: usize,
) -> isize {
    sys_process_vm_transfer(pid, local_iov, liovcnt, remote_iov, riovcnt, flags, false)
}

pub fn sys_process_vm_writev(
    pid: usize,
    local_iov: *const IOVec,
    liovcnt: usize,
    remote_iov: *const IOVec,
    riovcnt: usize,
    flags: usize,
) -> isize {
    sys_process_vm_transfer(pid, local_iov, liovcnt, remote_iov, riovcnt, flags, true)
}

#[derive(Clone, Copy)]
#[repr(C)]
struct SockFilter {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct SockFprog {
    len: u16,
    filter: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SeccompSyscallAction {
    Allow,
    KillThread(Signals),
    KillProcess(Signals),
}

fn seccomp_filter_allows_return(ret: u32) -> bool {
    (ret & SECCOMP_RET_ACTION_FULL) == SECCOMP_RET_ALLOW
}

fn seccomp_filter_kills_return(ret: u32) -> bool {
    ret == SECCOMP_RET_KILL_THREAD || ret == SECCOMP_RET_KILL_PROCESS
}

fn seccomp_filter_kill_action(ret: u32) -> SeccompSyscallAction {
    if ret == SECCOMP_RET_KILL_PROCESS {
        SeccompSyscallAction::KillProcess(Signals::SIGSYS)
    } else {
        SeccompSyscallAction::KillThread(Signals::SIGSYS)
    }
}

fn verify_seccomp_filter(insns: &[SeccompFilterInsn]) -> Result<(), isize> {
    if insns.is_empty() || insns.len() > SECCOMP_FILTER_MAX_LEN {
        return Err(EINVAL);
    }
    let mut has_terminal_ret = false;
    for (pc, insn) in insns.iter().enumerate() {
        match insn.code & BPF_CLASS_MASK {
            BPF_LD => {
                if (insn.code & BPF_SIZE_MASK) != BPF_W
                    || (insn.code & BPF_MODE_MASK) != BPF_ABS
                    || insn.k != SECCOMP_DATA_NR_OFFSET
                {
                    return Err(EINVAL);
                }
            }
            BPF_JMP => {
                if (insn.code & BPF_OP_MASK) != BPF_JEQ
                    || (insn.code & BPF_SRC_MASK) != BPF_K
                    || pc + 1 + insn.jt as usize >= insns.len()
                    || pc + 1 + insn.jf as usize >= insns.len()
                {
                    return Err(EINVAL);
                }
            }
            BPF_RET => {
                if (insn.code & BPF_SRC_MASK) != BPF_K {
                    return Err(EINVAL);
                }
                if !seccomp_filter_allows_return(insn.k) && !seccomp_filter_kills_return(insn.k) {
                    return Err(EINVAL);
                }
                has_terminal_ret = true;
            }
            _ => return Err(EINVAL),
        }
    }
    if has_terminal_ret {
        Ok(())
    } else {
        Err(EINVAL)
    }
}

fn read_seccomp_filter(filter: usize) -> Result<Vec<SeccompFilterInsn>, isize> {
    if filter == 0 {
        return Err(EFAULT);
    }
    let token = current_user_token();
    let mut prog = SockFprog { len: 0, filter: 0 };
    copy_from_user(token, filter as *const SockFprog, &mut prog as *mut SockFprog)?;
    let len = prog.len as usize;
    if len == 0 || len > SECCOMP_FILTER_MAX_LEN {
        return Err(EINVAL);
    }
    if prog.filter == 0 {
        return Err(EFAULT);
    }
    let mut raw = Vec::new();
    if raw.try_reserve(len).is_err() {
        return Err(ENOMEM);
    }
    unsafe {
        raw.set_len(len);
    }
    copy_from_user_array(token, prog.filter as *const SockFilter, raw.as_mut_ptr(), len)?;
    let mut insns = Vec::new();
    if insns.try_reserve(len).is_err() {
        return Err(ENOMEM);
    }
    for raw_insn in raw {
        insns.push(SeccompFilterInsn {
            code: raw_insn.code,
            jt: raw_insn.jt,
            jf: raw_insn.jf,
            k: raw_insn.k,
        });
    }
    verify_seccomp_filter(&insns)?;
    Ok(insns)
}

fn eval_seccomp_filter(insns: &[SeccompFilterInsn], syscall_id: usize) -> SeccompSyscallAction {
    let mut pc = 0usize;
    let mut accumulator = 0u32;
    while pc < insns.len() {
        let insn = insns[pc];
        match insn.code & BPF_CLASS_MASK {
            BPF_LD => {
                accumulator = syscall_id as u32;
                pc += 1;
            }
            BPF_JMP => {
                if accumulator == insn.k {
                    pc += 1 + insn.jt as usize;
                } else {
                    pc += 1 + insn.jf as usize;
                }
            }
            BPF_RET => {
                return if seccomp_filter_allows_return(insn.k) {
                    SeccompSyscallAction::Allow
                } else {
                    seccomp_filter_kill_action(insn.k)
                };
            }
            _ => return SeccompSyscallAction::KillThread(Signals::SIGSYS),
        }
    }
    SeccompSyscallAction::KillThread(Signals::SIGSYS)
}

pub fn seccomp_action_for_syscall(syscall_id: usize) -> SeccompSyscallAction {
    use crate::syscall::syscall_id::{
        SYSCALL_EXIT, SYSCALL_READ, SYSCALL_SIGRETURN, SYSCALL_WRITE,
    };

    if !crate::task::any_seccomp_enabled() {
        return SeccompSyscallAction::Allow;
    }

    let task = match current_task_ref() {
        Some(task) => task,
        None => return SeccompSyscallAction::Allow,
    };
    let inner = task.acquire_inner_lock();
    match inner.seccomp_mode {
        SECCOMP_MODE_DISABLED => SeccompSyscallAction::Allow,
        SECCOMP_MODE_STRICT => match syscall_id {
            SYSCALL_READ | SYSCALL_WRITE | SYSCALL_EXIT | SYSCALL_SIGRETURN => {
                SeccompSyscallAction::Allow
            }
            _ => SeccompSyscallAction::KillProcess(Signals::SIGKILL),
        },
        SECCOMP_MODE_FILTER => eval_seccomp_filter(&inner.seccomp_filter, syscall_id),
        _ => SeccompSyscallAction::KillProcess(Signals::SIGKILL),
    }
}

fn sys_prctl_set_seccomp(mode: usize, filter: usize) -> isize {
    if mode == SECCOMP_MODE_STRICT {
        let task = current_task_ref().unwrap();
        let mut inner = task.acquire_inner_lock();
        if inner.seccomp_mode != SECCOMP_MODE_DISABLED {
            return EINVAL;
        }
        inner.seccomp_mode = SECCOMP_MODE_STRICT;
        inner.seccomp_filter.clear();
        drop(inner);
        task.account_seccomp_enabled();
        return SUCCESS;
    }
    if mode != SECCOMP_MODE_FILTER {
        return EINVAL;
    }
    let filter_insns = match read_seccomp_filter(filter) {
        Ok(insns) => insns,
        Err(errno) => return errno,
    };
    let task = current_task_ref().unwrap();
    let mut inner = task.acquire_inner_lock();
    if inner.seccomp_mode != SECCOMP_MODE_DISABLED {
        return EINVAL;
    }
    if !inner.no_new_privs && (inner.cap_effective & (1u64 << CAP_SYS_ADMIN)) == 0 {
        EACCES
    } else {
        inner.seccomp_mode = SECCOMP_MODE_FILTER;
        inner.seccomp_filter = filter_insns;
        drop(inner);
        task.account_seccomp_enabled();
        SUCCESS
    }
}

fn sys_prctl_cap_ambient(op: usize, cap: usize, arg4: usize, arg5: usize) -> isize {
    if arg4 != 0 || arg5 != 0 {
        return EINVAL;
    }
    let task = current_task_ref().unwrap();
    let mut inner = task.acquire_inner_lock();
    match op {
        PR_CAP_AMBIENT_IS_SET => {
            if cap > CAP_LAST_CAP {
                return EINVAL;
            }
            ((inner.cap_ambient & (1u64 << cap)) != 0) as isize
        }
        PR_CAP_AMBIENT_RAISE => {
            if cap > CAP_LAST_CAP {
                return EINVAL;
            }
            let mask = 1u64 << cap;
            if (inner.securebits & SECBIT_NO_CAP_AMBIENT_RAISE) != 0
                || (inner.cap_permitted & mask) == 0
                || (inner.cap_inheritable & mask) == 0
            {
                return EPERM;
            }
            inner.cap_ambient |= mask;
            SUCCESS
        }
        PR_CAP_AMBIENT_LOWER => {
            if cap > CAP_LAST_CAP {
                return EINVAL;
            }
            inner.cap_ambient &= !(1u64 << cap);
            SUCCESS
        }
        PR_CAP_AMBIENT_CLEAR_ALL => {
            if cap != 0 {
                return EINVAL;
            }
            inner.cap_ambient = 0;
            SUCCESS
        }
        _ => EINVAL,
    }
}

pub fn sys_prctl(option: usize, arg2: usize, arg3: usize, arg4: usize, arg5: usize) -> isize {
    let task = current_task_ref().unwrap();
    match option {
        PR_SET_PDEATHSIG => {
            if arg2 > PR_MAX_SIGNAL {
                return EINVAL;
            }
            task.acquire_inner_lock().pdeath_signal = arg2;
            SUCCESS
        }
        PR_GET_PDEATHSIG => {
            let signal = task.acquire_inner_lock().pdeath_signal as i32;
            if copy_to_user(current_user_token(), &signal, arg2 as *mut i32).is_err() {
                EFAULT
            } else {
                SUCCESS
            }
        }
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
            if (inner.cap_effective & (1u64 << CAP_SETPCAP)) == 0 {
                return EPERM;
            }
            inner.cap_bounding &= !(1u64 << arg2);
            inner.cap_ambient &= inner.cap_bounding;
            SUCCESS
        }
        PR_GET_KEEPCAPS => 0,
        PR_SET_KEEPCAPS => SUCCESS,
        PR_GET_DUMPABLE => task.acquire_inner_lock().dumpable as isize,
        PR_SET_DUMPABLE => {
            if arg2 > 1 {
                return EINVAL;
            }
            task.acquire_inner_lock().dumpable = arg2;
            SUCCESS
        }
        PR_SET_NAME => {
            let comm = match read_prctl_comm_from_user(arg2) {
                Ok(comm) => comm,
                Err(errno) => return errno,
            };
            task.acquire_inner_lock().task_comm = comm;
            SUCCESS
        }
        PR_GET_NAME => {
            let comm = task.acquire_inner_lock().task_comm;
            write_prctl_comm_to_user(arg2, &comm)
        }
        PR_SET_CHILD_SUBREAPER => {
            task.process.set_child_subreaper(arg2 != 0);
            SUCCESS
        }
        PR_GET_CHILD_SUBREAPER => {
            let enabled = task.process.is_child_subreaper() as i32;
            if copy_to_user(current_user_token(), &enabled, arg2 as *mut i32).is_err() {
                EFAULT
            } else {
                SUCCESS
            }
        }
        PR_GET_SECCOMP => task.acquire_inner_lock().seccomp_mode as isize,
        PR_SET_SECCOMP => sys_prctl_set_seccomp(arg2, arg3),
        PR_SET_NO_NEW_PRIVS => {
            if arg2 != 1 || arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return EINVAL;
            }
            task.acquire_inner_lock().no_new_privs = true;
            SUCCESS
        }
        PR_GET_NO_NEW_PRIVS => {
            if arg2 != 0 || arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return EINVAL;
            }
            task.acquire_inner_lock().no_new_privs as isize
        }
        PR_SET_THP_DISABLE => {
            if arg2 > 1 || arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return EINVAL;
            }
            task.acquire_inner_lock().thp_disabled = arg2 != 0;
            SUCCESS
        }
        PR_GET_THP_DISABLE => {
            if arg2 != 0 || arg3 != 0 || arg4 != 0 || arg5 != 0 {
                return EINVAL;
            }
            task.acquire_inner_lock().thp_disabled as isize
        }
        PR_CAP_AMBIENT => sys_prctl_cap_ambient(arg2, arg3, arg4, arg5),
        PR_GET_SPECULATION_CTRL => {
            if arg3 != 0 || arg4 != 0 || arg5 != 0 || arg2 != PR_SPEC_STORE_BYPASS {
                EINVAL
            } else {
                SUCCESS
            }
        }
        PR_GET_SECUREBITS => task.acquire_inner_lock().securebits as isize,
        PR_SET_SECUREBITS => {
            let mut inner = task.acquire_inner_lock();
            if (inner.cap_effective & (1u64 << CAP_SETPCAP)) == 0 {
                EPERM
            } else {
                inner.securebits = arg2;
                SUCCESS
            }
        }
        PR_SET_TIMERSLACK => {
            let mut inner = task.acquire_inner_lock();
            inner.timer_slack_ns = if arg2 == 0 {
                inner.timer_slack_default_ns
            } else {
                arg2
            };
            SUCCESS
        }
        PR_GET_TIMERSLACK => task.acquire_inner_lock().timer_slack_ns as isize,
        _ => EINVAL,
    }
}

fn is_child_of(process: &Arc<ProcessControlBlock>, parent: &Arc<ProcessControlBlock>) -> bool {
    process
        .parent()
        .map_or(false, |process_parent| process_parent.pid == parent.pid)
}

fn pgid_exists_in_session(pgid: usize, sid: usize) -> bool {
    ProcessManager::find_processes_by_pgid(pgid)
        .into_iter()
        .any(|process| process.getsid() == sid)
}

pub fn sys_setpgid(pid: usize, pgid: usize) -> isize {
    if (pid as isize) < 0 || (pgid as isize) < 0 {
        return EINVAL;
    }
    let current = current_task_ref().unwrap().process.clone();
    let process = if pid == 0 {
        current.clone()
    } else {
        match ProcessManager::find_process(pid) {
            Some(process) => process,
            None => return ESRCH,
        }
    };

    let target_is_current = process.pid == current.pid;
    if !target_is_current {
        if !is_child_of(&process, &current) {
            return ESRCH;
        }
        if process.has_execed() {
            return EACCES;
        }
    }

    let target_sid = process.getsid();
    if target_sid != current.getsid() {
        return EPERM;
    }
    if process.pid == target_sid {
        return EPERM;
    }

    let real_pgid = if pgid == 0 { process.pid } else { pgid };
    if real_pgid != process.pid && !pgid_exists_in_session(real_pgid, target_sid) {
        return EPERM;
    }

    process.setpgid(real_pgid)
}

pub fn sys_getpgid(pid: usize) -> isize {
    if (pid as isize) < 0 {
        return ESRCH;
    }
    if pid == 0 {
        return current_task_ref().unwrap().process.getpgid() as isize;
    }
    match ProcessManager::find_process(pid) {
        Some(process) => process.getpgid() as isize,
        None => ESRCH,
    }
}

pub fn sys_getsid(pid: usize) -> isize {
    if (pid as isize) < 0 {
        return ESRCH;
    }
    if pid == 0 {
        return current_task_ref().unwrap().process.getsid() as isize;
    }
    match ProcessManager::find_process(pid) {
        Some(process) => process.getsid() as isize,
        None => ESRCH,
    }
}

/// creates a new session if the calling process is not a process group leader.
/// The calling process is the leader of the new session, and its pgid is set to its pid.
/// 当前进程脱离父进程，从父进程的子进程列表中移除当前进程，当前进程的父进程设置为空。
pub fn sys_setsid() -> isize {
    let process = &current_task_ref().unwrap().process;
    if !ProcessManager::find_processes_by_pgid(process.pid).is_empty() {
        return EPERM;
    }
    process.setsid(process.pid);
    process.pid as isize
}

pub fn sys_gettid() -> isize {
    current_tid() as isize
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

fn current_rlimit_for(task: &TaskControlBlock, resource: Resource) -> Option<RLimit> {
    let unlimited = RLimit {
        rlim_cur: usize::MAX,
        rlim_max: usize::MAX,
    };

    let limit = match resource {
        Resource::CPU => {
            let inner = task.acquire_inner_lock();
            RLimit {
                rlim_cur: inner.cpu_limit_cur,
                rlim_max: inner.cpu_limit_max,
            }
        }
        Resource::DATA
        | Resource::RSS
        | Resource::AS
        | Resource::LOCKS
        | Resource::MSGQUEUE
        | Resource::RTTIME => unlimited,
        Resource::FSIZE => {
            let inner = task.acquire_inner_lock();
            RLimit {
                rlim_cur: inner.fsize_limit_cur,
                rlim_max: inner.fsize_limit_max,
            }
        }
        Resource::SIGPENDING => {
            let inner = task.acquire_inner_lock();
            RLimit {
                rlim_cur: inner.sigpending_limit_cur,
                rlim_max: inner.sigpending_limit_max,
            }
        }
        Resource::NICE => {
            let inner = task.acquire_inner_lock();
            RLimit {
                rlim_cur: inner.nice_limit_cur,
                rlim_max: inner.nice_limit_max,
            }
        }
        Resource::RTPRIO => {
            let inner = task.acquire_inner_lock();
            RLimit {
                rlim_cur: inner.rtprio_limit_cur,
                rlim_max: inner.rtprio_limit_max,
            }
        }
        Resource::CORE => {
            let inner = task.acquire_inner_lock();
            RLimit {
                rlim_cur: inner.core_limit_cur,
                rlim_max: inner.core_limit_max,
            }
        }
        Resource::STACK => {
            let inner = task.acquire_inner_lock();
            RLimit {
                rlim_cur: inner.stack_limit_cur,
                rlim_max: inner.stack_limit_max,
            }
        }
        Resource::MEMLOCK => {
            let inner = task.acquire_inner_lock();
            RLimit {
                rlim_cur: inner.memlock_limit_cur,
                rlim_max: inner.memlock_limit_max,
            }
        }
        Resource::NPROC => {
            let inner = task.acquire_inner_lock();
            RLimit {
                rlim_cur: inner.nproc_limit_cur,
                rlim_max: inner.nproc_limit_max,
            }
        },
        Resource::NOFILE => {
            let files_ref = task.process.files();
            let lock = files_ref.lock();
            RLimit {
                rlim_cur: lock.get_soft_limit(),
                rlim_max: lock.get_hard_limit(),
            }
        }
        Resource::NLIMITS | Resource::ILLEAGAL => return None,
    };
    Some(limit)
}

fn task_has_capability(task: &TaskControlBlock, cap: usize) -> bool {
    let inner = task.acquire_inner_lock();
    inner.euid == 0 || (inner.cap_effective & (1u64 << cap)) != 0
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
    let task = current_task_ref().unwrap();
    if pid != 0 && pid != task.pid() {
        return ESRCH;
    }

    let token = current_user_token();
    let resource = Resource::from_primitive(resource);
    info!(
        "[sys_prlimit] pid: {}, resource: {:?}, new_limit: {:?}, old_limit: {:?}",
        pid, resource, new_limit, old_limit
    );

    if resource == Resource::ILLEAGAL || resource == Resource::NLIMITS {
        return EINVAL;
    }

    if !old_limit.is_null() {
        let Some(limit) = current_rlimit_for(task, resource) else {
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
        let Some(current_limit) = current_rlimit_for(task, resource) else {
            return EINVAL;
        };
        if rlimit.rlim_max > current_limit.rlim_max
            && !task_has_capability(task, CAP_SYS_RESOURCE)
        {
            return EPERM;
        }
        if resource == Resource::NOFILE && rlimit.rlim_max > RLIMIT_NOFILE_MAX {
            return EPERM;
        }
        match resource {
            Resource::NOFILE => {
                task.process.files().lock().set_soft_limit(rlimit.rlim_cur);
                task.process.files().lock().set_hard_limit(rlimit.rlim_max);
            }
            Resource::FSIZE => {
                let mut inner = task.acquire_inner_lock();
                inner.fsize_limit_cur = rlimit.rlim_cur;
                inner.fsize_limit_max = rlimit.rlim_max;
            }
            Resource::NPROC => {
                let mut inner = task.acquire_inner_lock();
                inner.nproc_limit_cur = rlimit.rlim_cur;
                inner.nproc_limit_max = rlimit.rlim_max;
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
            Resource::SIGPENDING => {
                let mut inner = task.acquire_inner_lock();
                inner.sigpending_limit_cur = rlimit.rlim_cur;
                inner.sigpending_limit_max = rlimit.rlim_max;
            }
            Resource::CPU => {
                let mut inner = task.acquire_inner_lock();
                inner.cpu_limit_cur = rlimit.rlim_cur;
                inner.cpu_limit_max = rlimit.rlim_max;
                inner.cpu_limit_sigxcpu_sent = false;
            }
            Resource::CORE => {
                let mut inner = task.acquire_inner_lock();
                inner.core_limit_cur = rlimit.rlim_cur;
                inner.core_limit_max = rlimit.rlim_max;
            }
            Resource::DATA
            | Resource::RSS
            | Resource::AS
            | Resource::LOCKS
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
    let mut targets = Vec::new();
    match which {
        PRIO_PROCESS => {
            if who == 0 {
                targets.push(current_task().unwrap());
            } else if who < 0 {
                return Err(ESRCH);
            } else if let Some(task) = process_main_task(who as usize) {
                targets.push(task);
            }
        }
        PRIO_PGRP => {
            let pgid = if who == 0 {
                current_task_ref().unwrap().process.getpgid()
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
                current_euid()
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
    let (old_nice, state) = {
        let mut inner = task.acquire_inner_lock();
        let old_nice = inner.sched_nice;
        inner.sched_nice = nice;
        task.sched_nice_hint
            .store(inner.sched_nice, core::sync::atomic::Ordering::Relaxed);
        let state = SchedState {
            policy: inner.sched_policy,
            priority: inner.sched_priority,
            reset_on_fork: inner.sched_reset_on_fork,
            nice: inner.sched_nice,
            runtime: inner.sched_runtime,
            deadline: inner.sched_deadline,
            period: inner.sched_period,
        };
        (old_nice, state)
    };
    update_ready_nice(task, old_nice, state.nice);
    sync_process_sched_state(task, state);
}

pub fn sys_getpriority(which: usize, who: usize) -> isize {
    let targets = match priority_targets(syscall_arg_i32(which), syscall_arg_i32(who)) {
        Ok(targets) => targets,
        Err(errno) => return errno,
    };
    let mut best_nice = i32::MAX;
    for task in targets {
        best_nice = best_nice.min(task.sched_nice());
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
        let old_nice = task.sched_nice();
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
    if pid != 0 && ProcessManager::find_task(pid).is_none() {
        return ESRCH;
    }
    let token = current_user_token();
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

fn task_sched_state(task: &TaskControlBlock) -> SchedState {
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
    if let Some(task) = current_task_ref() {
        if pid == 0 || pid == task.pid() {
            return Ok(task_sched_state(task));
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
    let task = current_task_ref().unwrap();
    let inner = task.acquire_inner_lock();
    SchedAccess {
        euid: inner.euid,
        has_sys_nice: inner.euid == 0 || (inner.cap_effective & (1u64 << CAP_SYS_NICE)) != 0,
        rtprio_limit_cur: inner.rtprio_limit_cur,
    }
}

fn sched_same_owner(access: SchedAccess, task: &Arc<TaskControlBlock>) -> bool {
    access.euid == task.euid() || access.euid == task.uid()
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
    let (old_nice, state) = {
        let mut inner = task.acquire_inner_lock();
        let old_nice = inner.sched_nice;
        inner.sched_policy = base_policy;
        inner.sched_priority = priority;
        inner.sched_reset_on_fork = new_reset_on_fork;
        inner.sched_nice = attr.sched_nice;
        task.sched_nice_hint
            .store(inner.sched_nice, core::sync::atomic::Ordering::Relaxed);
        inner.sched_runtime = attr.sched_runtime;
        inner.sched_deadline = attr.sched_deadline;
        inner.sched_period = attr.sched_period;
        let state = SchedState {
            policy: inner.sched_policy,
            priority: inner.sched_priority,
            reset_on_fork: inner.sched_reset_on_fork,
            nice: inner.sched_nice,
            runtime: inner.sched_runtime,
            deadline: inner.sched_deadline,
            period: inner.sched_period,
        };
        (old_nice, state)
    };
    update_ready_nice(&task, old_nice, state.nice);
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
