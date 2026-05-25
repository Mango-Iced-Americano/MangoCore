use super::mm::{sys_mmap, sys_munmap};
use crate::mm::{copy_from_user, copy_from_user_array, copy_to_user, copy_to_user_array, MapFlags};
use crate::syscall::errno::*;
use crate::task::{current_task, current_user_token, WaitQueue, WaitResult};
use crate::timer::{current_timespec, TimeSpec};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::mem::size_of;
use lazy_static::lazy_static;
use spin::Mutex;

const IPC_PRIVATE: isize = 0;
const IPC_CREAT: usize = 0o1000;
const IPC_EXCL: usize = 0o2000;
const IPC_NOWAIT: usize = 0o4000;
const IPC_RMID: usize = 0;
const IPC_SET: usize = 1;
const IPC_STAT: usize = 2;
const IPC_INFO: usize = 3;

const SHM_RDONLY: usize = 0o10000;
const SHM_RND: usize = 0o20000;
const SHM_REMAP: usize = 0o40000;
const SHMLBA: usize = crate::config::PAGE_SIZE;
const MAX_SHM_SIZE: usize = 16 * 1024 * 1024;

const MSG_STAT: usize = 11;
const MSG_INFO: usize = 12;
const MSG_STAT_ANY: usize = 13;
const MSG_NOERROR: usize = 0o10000;
const MSG_EXCEPT: usize = 0o20000;
const MSG_COPY: usize = 0o40000;
const MSG_R: usize = 0o400;
const MSG_W: usize = 0o200;
const MSGMNI: usize = 1024;
const MSGMAX: usize = 8192;
const MSGMNB: usize = 16384;
const MSGTQL: usize = 4096;

const GETPID: usize = 11;
const GETVAL: usize = 12;
const GETALL: usize = 13;
const GETNCNT: usize = 14;
const GETZCNT: usize = 15;
const SETVAL: usize = 16;
const SETALL: usize = 17;
const SEM_STAT: usize = 18;
const SEM_INFO: usize = 19;
const SEM_STAT_ANY: usize = 20;
const SEM_UNDO: i16 = 0x1000;
const SEM_R: usize = 0o400;
const SEM_A: usize = 0o200;
const SEMMNI: usize = 1024;
const SEMMSL: usize = 32000;
const SEMOPM: usize = 500;
const SEMVMX: i32 = 32767;
const SEMAEM: i32 = 32767;

const PROT_READ: usize = 0x1;
const PROT_WRITE: usize = 0x2;

#[cfg(target_arch = "riscv64")]
type LinuxIpcMode = u16;
#[cfg(not(target_arch = "riscv64"))]
type LinuxIpcMode = u32;

#[cfg(target_arch = "riscv64")]
type LinuxIpcSeq = u16;
#[cfg(not(target_arch = "riscv64"))]
type LinuxIpcSeq = u32;

#[repr(C)]
#[derive(Clone, Copy)]
struct LinuxIpcPerm {
    key: i32,
    uid: u32,
    gid: u32,
    cuid: u32,
    cgid: u32,
    mode: LinuxIpcMode,
    #[cfg(target_arch = "riscv64")]
    pad1: u16,
    seq: LinuxIpcSeq,
    #[cfg(target_arch = "riscv64")]
    pad2: u16,
    unused1: u64,
    unused2: u64,
}

impl LinuxIpcPerm {
    fn new(key: isize, uid: u32, gid: u32, cuid: u32, cgid: u32, mode: usize) -> Self {
        Self {
            key: key as i32,
            uid,
            gid,
            cuid,
            cgid,
            mode: (mode & 0o777) as LinuxIpcMode,
            #[cfg(target_arch = "riscv64")]
            pad1: 0,
            seq: 0 as LinuxIpcSeq,
            #[cfg(target_arch = "riscv64")]
            pad2: 0,
            unused1: 0,
            unused2: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct LinuxMsqidDs {
    msg_perm: LinuxIpcPerm,
    msg_stime: i64,
    msg_rtime: i64,
    msg_ctime: i64,
    msg_cbytes: u64,
    msg_qnum: u64,
    msg_qbytes: u64,
    msg_lspid: i32,
    msg_lrpid: i32,
    reserved4: u64,
    reserved5: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct LinuxMsgInfo {
    msgpool: i32,
    msgmap: i32,
    msgmax: i32,
    msgmnb: i32,
    msgmni: i32,
    msgssz: i32,
    msgtql: i32,
    msgseg: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct LinuxSemidDs {
    sem_perm: LinuxIpcPerm,
    sem_otime: i64,
    sem_ctime: i64,
    sem_nsems: u64,
    reserved3: u64,
    reserved4: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct LinuxSemInfo {
    semmap: i32,
    semmni: i32,
    semmns: i32,
    semmnu: i32,
    semmsl: i32,
    semopm: i32,
    semume: i32,
    semusz: i32,
    semvmx: i32,
    semaem: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct LinuxSembuf {
    sem_num: u16,
    sem_op: i16,
    sem_flg: i16,
}

#[derive(Clone)]
struct Message {
    serial: u64,
    mtype: isize,
    data: Vec<u8>,
}

struct MsgQueue {
    key: isize,
    uid: u32,
    gid: u32,
    cuid: u32,
    cgid: u32,
    mode: usize,
    qbytes: usize,
    messages: VecDeque<Message>,
    cbytes: usize,
    next_serial: u64,
    lspid: i32,
    lrpid: i32,
    stime: usize,
    rtime: usize,
    ctime: usize,
}

impl MsgQueue {
    fn new(key: isize, mode: usize, uid: u32, gid: u32) -> Self {
        let now = now_sec();
        Self {
            key,
            uid,
            gid,
            cuid: uid,
            cgid: gid,
            mode: mode & 0o777,
            qbytes: MSGMNB,
            messages: VecDeque::new(),
            cbytes: 0,
            next_serial: 1,
            lspid: 0,
            lrpid: 0,
            stime: 0,
            rtime: 0,
            ctime: now,
        }
    }

    fn to_msqid_ds(&self) -> LinuxMsqidDs {
        LinuxMsqidDs {
            msg_perm: LinuxIpcPerm::new(
                self.key, self.uid, self.gid, self.cuid, self.cgid, self.mode,
            ),
            msg_stime: self.stime as i64,
            msg_rtime: self.rtime as i64,
            msg_ctime: self.ctime as i64,
            msg_cbytes: self.cbytes as u64,
            msg_qnum: self.messages.len() as u64,
            msg_qbytes: self.qbytes as u64,
            msg_lspid: self.lspid,
            msg_lrpid: self.lrpid,
            reserved4: 0,
            reserved5: 0,
        }
    }
}

struct MsgRegistry {
    next_id: i32,
    queues: BTreeMap<i32, MsgQueue>,
    wait_queue: WaitQueue,
    removed_ids: Vec<i32>,
}

impl MsgRegistry {
    fn new() -> Self {
        Self {
            next_id: 1,
            queues: BTreeMap::new(),
            wait_queue: WaitQueue::new(),
            removed_ids: Vec::new(),
        }
    }

    fn alloc_id(&mut self) -> i32 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1).max(1);
        id
    }

    fn find_by_key(&self, key: isize) -> Option<i32> {
        self.queues
            .iter()
            .find(|(_, queue)| queue.key == key)
            .map(|(id, _)| *id)
    }

    fn id_by_index(&self, index: i32) -> Option<i32> {
        if index < 0 {
            return None;
        }
        self.queues.keys().nth(index as usize).copied()
    }

    fn highest_index(&self) -> isize {
        if self.queues.is_empty() {
            0
        } else {
            self.queues.len() as isize - 1
        }
    }

    fn mark_removed(&mut self, id: i32) {
        if !self.removed_ids.contains(&id) {
            if self.removed_ids.try_reserve(1).is_ok() {
                self.removed_ids.push(id);
            }
        }
    }

    fn was_removed(&self, id: i32) -> bool {
        self.removed_ids.contains(&id)
    }
}

#[derive(Clone, Copy)]
struct Semaphore {
    value: i32,
    last_pid: i32,
    ncnt: u16,
    zcnt: u16,
}

impl Semaphore {
    fn new() -> Self {
        Self {
            value: 0,
            last_pid: 0,
            ncnt: 0,
            zcnt: 0,
        }
    }
}

struct SemSet {
    key: isize,
    uid: u32,
    gid: u32,
    cuid: u32,
    cgid: u32,
    mode: usize,
    semaphores: Vec<Semaphore>,
    otime: usize,
    ctime: usize,
}

impl SemSet {
    fn new(key: isize, nsems: usize, mode: usize, uid: u32, gid: u32) -> Result<Self, isize> {
        let mut semaphores = Vec::new();
        semaphores.try_reserve_exact(nsems).map_err(|_| ENOMEM)?;
        semaphores.resize(nsems, Semaphore::new());
        Ok(Self {
            key,
            uid,
            gid,
            cuid: uid,
            cgid: gid,
            mode: mode & 0o777,
            semaphores,
            otime: 0,
            ctime: now_sec(),
        })
    }

    fn to_semid_ds(&self) -> LinuxSemidDs {
        LinuxSemidDs {
            sem_perm: LinuxIpcPerm::new(
                self.key, self.uid, self.gid, self.cuid, self.cgid, self.mode,
            ),
            sem_otime: self.otime as i64,
            sem_ctime: self.ctime as i64,
            sem_nsems: self.semaphores.len() as u64,
            reserved3: 0,
            reserved4: 0,
        }
    }
}

struct SemRegistry {
    next_id: i32,
    sets: BTreeMap<i32, SemSet>,
    wait_queue: WaitQueue,
    removed_ids: Vec<i32>,
}

impl SemRegistry {
    fn new() -> Self {
        Self {
            next_id: 1,
            sets: BTreeMap::new(),
            wait_queue: WaitQueue::new(),
            removed_ids: Vec::new(),
        }
    }

    fn alloc_id(&mut self) -> i32 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1).max(1);
        id
    }

    fn find_by_key(&self, key: isize) -> Option<(i32, &SemSet)> {
        self.sets
            .iter()
            .find(|(_, set)| set.key == key)
            .map(|(id, set)| (*id, set))
    }

    fn id_by_index(&self, index: i32) -> Option<i32> {
        if index < 0 {
            return None;
        }
        self.sets.keys().nth(index as usize).copied()
    }

    fn highest_index(&self) -> isize {
        if self.sets.is_empty() {
            0
        } else {
            self.sets.len() as isize - 1
        }
    }

    fn total_semaphores(&self) -> usize {
        self.sets.values().map(|set| set.semaphores.len()).sum()
    }

    fn mark_removed(&mut self, id: i32) {
        if !self.removed_ids.contains(&id) {
            if self.removed_ids.try_reserve(1).is_ok() {
                self.removed_ids.push(id);
            }
        }
    }

    fn was_removed(&self, id: i32) -> bool {
        self.removed_ids.contains(&id)
    }
}

struct ShmSegment {
    key: isize,
    size: usize,
    removed: bool,
    attachments: Vec<usize>,
}

struct ShmRegistry {
    next_id: i32,
    segments: BTreeMap<i32, ShmSegment>,
}

impl ShmRegistry {
    fn new() -> Self {
        Self {
            next_id: 1,
            segments: BTreeMap::new(),
        }
    }

    fn alloc_id(&mut self) -> i32 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1).max(1);
        id
    }

    fn find_by_key(&self, key: isize) -> Option<(i32, &ShmSegment)> {
        self.segments
            .iter()
            .find(|(_, seg)| !seg.removed && seg.key == key)
            .map(|(id, seg)| (*id, seg))
    }

    fn remove_if_detached(&mut self, shmid: i32) {
        let should_remove = self
            .segments
            .get(&shmid)
            .map(|seg| seg.removed && seg.attachments.is_empty())
            .unwrap_or(false);
        if should_remove {
            self.segments.remove(&shmid);
        }
    }
}

lazy_static! {
    static ref SHM_REGISTRY: Mutex<ShmRegistry> = Mutex::new(ShmRegistry::new());
    static ref MSG_REGISTRY: Mutex<MsgRegistry> = Mutex::new(MsgRegistry::new());
    static ref SEM_REGISTRY: Mutex<SemRegistry> = Mutex::new(SemRegistry::new());
}

pub fn sys_shmget(key: isize, size: usize, shmflg: usize) -> isize {
    let mut registry = SHM_REGISTRY.lock();
    if key != IPC_PRIVATE {
        if let Some((id, seg)) = registry.find_by_key(key) {
            if shmflg & IPC_CREAT != 0 && shmflg & IPC_EXCL != 0 {
                return EEXIST;
            }
            if size > seg.size {
                return EINVAL;
            }
            return id as isize;
        }
        if shmflg & IPC_CREAT == 0 {
            return ENOENT;
        }
    }

    if size == 0 || size > MAX_SHM_SIZE {
        return EINVAL;
    }
    let id = registry.alloc_id();
    registry.segments.insert(
        id,
        ShmSegment {
            key,
            size,
            removed: false,
            attachments: Vec::new(),
        },
    );
    id as isize
}

pub fn sys_shmat(shmid: i32, shmaddr: usize, shmflg: usize) -> isize {
    let (size, removed) = {
        let registry = SHM_REGISTRY.lock();
        let Some(seg) = registry.segments.get(&shmid) else {
            return EINVAL;
        };
        (seg.size, seg.removed)
    };
    if removed {
        return EIDRM;
    }

    let fixed = shmaddr != 0;
    let attach_addr = if fixed {
        if shmaddr & (SHMLBA - 1) != 0 {
            if shmflg & SHM_RND == 0 {
                return EINVAL;
            }
            shmaddr & !(SHMLBA - 1)
        } else {
            shmaddr
        }
    } else {
        0
    };
    let prot = if shmflg & SHM_RDONLY != 0 {
        PROT_READ
    } else {
        PROT_READ | PROT_WRITE
    };
    let mut flags = MapFlags::MAP_SHARED | MapFlags::MAP_ANONYMOUS;
    if fixed {
        flags |= if shmflg & SHM_REMAP != 0 {
            MapFlags::MAP_FIXED
        } else {
            MapFlags::MAP_FIXED_NOREPLACE
        };
    }

    let mapped = sys_mmap(attach_addr, size, prot, flags.bits(), usize::MAX, 0);
    if mapped < 0 {
        return mapped;
    }
    let mapped = mapped as usize;
    let mut registry = SHM_REGISTRY.lock();
    if let Some(seg) = registry.segments.get_mut(&shmid) {
        if seg.removed {
            let _ = sys_munmap(mapped, size);
            return EIDRM;
        }
        seg.attachments.push(mapped);
    } else {
        let _ = sys_munmap(mapped, size);
        return EIDRM;
    }
    mapped as isize
}

pub fn sys_shmdt(shmaddr: usize) -> isize {
    let mut detach = None;
    {
        let mut registry = SHM_REGISTRY.lock();
        for (id, seg) in registry.segments.iter_mut() {
            if let Some(pos) = seg.attachments.iter().position(|addr| *addr == shmaddr) {
                seg.attachments.swap_remove(pos);
                detach = Some((*id, seg.size));
                break;
            }
        }
        if let Some((id, _)) = detach {
            registry.remove_if_detached(id);
        }
    }
    let Some((_, size)) = detach else {
        return EINVAL;
    };
    sys_munmap(shmaddr, size)
}

pub fn sys_shmctl(shmid: i32, cmd: usize, _buf: usize) -> isize {
    if cmd != IPC_RMID {
        return EINVAL;
    }
    let mut registry = SHM_REGISTRY.lock();
    let Some(seg) = registry.segments.get_mut(&shmid) else {
        return EINVAL;
    };
    seg.removed = true;
    registry.remove_if_detached(shmid);
    SUCCESS
}

fn current_pid_i32() -> i32 {
    current_task().map(|task| task.pid() as i32).unwrap_or(0)
}

fn current_ipc_ids() -> (u32, u32) {
    let task = current_task().unwrap();
    let inner = task.acquire_inner_lock();
    (inner.euid, inner.egid)
}

fn now_sec() -> usize {
    current_timespec().tv_sec
}

fn has_sem_permission(set: &SemSet, requested: usize) -> bool {
    if requested == 0 {
        return true;
    }
    let (euid, egid) = current_ipc_ids();
    if euid == 0 {
        return true;
    }
    let shift = if euid == set.uid || euid == set.cuid {
        6
    } else if egid == set.gid || egid == set.cgid {
        3
    } else {
        0
    };
    let available = (set.mode >> shift) & 0o7;
    let mut need = 0;
    if requested & SEM_R != 0 {
        need |= 0o4;
    }
    if requested & SEM_A != 0 {
        need |= 0o2;
    }
    available & need == need
}

fn can_modify_sem_set(set: &SemSet) -> bool {
    let (euid, _) = current_ipc_ids();
    euid == 0 || euid == set.uid || euid == set.cuid
}

fn seminfo_snapshot(registry: &SemRegistry, runtime: bool) -> LinuxSemInfo {
    let total = registry.total_semaphores();
    LinuxSemInfo {
        semmap: SEMMNI as i32,
        semmni: SEMMNI as i32,
        semmns: (SEMMNI * SEMMSL) as i32,
        semmnu: SEMMNI as i32,
        semmsl: SEMMSL as i32,
        semopm: SEMOPM as i32,
        semume: SEMOPM as i32,
        semusz: if runtime {
            registry.sets.len() as i32
        } else {
            size_of::<SemSet>() as i32
        },
        semvmx: SEMVMX,
        semaem: if runtime { total as i32 } else { SEMAEM },
    }
}

fn semctl_setval_value(arg: usize) -> Result<i32, isize> {
    if arg <= SEMVMX as usize {
        return Ok(arg as i32);
    }

    #[cfg(target_arch = "loongarch64")]
    {
        let low = arg as i32;
        if (0..=SEMVMX).contains(&low) {
            return Ok(low);
        }
        if arg >= crate::config::USER_VA_BASE && arg < crate::config::USER_VA_END {
            let mut value = 0i32;
            copy_from_user(
                current_user_token(),
                arg as *const i32,
                &mut value as *mut i32,
            )?;
            if (0..=SEMVMX).contains(&value) {
                return Ok(value);
            }
        }
    }

    Err(ERANGE)
}

pub fn sys_semget(key: isize, nsems: usize, semflg: usize) -> isize {
    let mut registry = SEM_REGISTRY.lock();
    if key != IPC_PRIVATE {
        if let Some((id, set)) = registry.find_by_key(key) {
            if semflg & IPC_CREAT != 0 && semflg & IPC_EXCL != 0 {
                return EEXIST;
            }
            if nsems > set.semaphores.len() {
                return EINVAL;
            }
            if !has_sem_permission(set, semflg & (SEM_R | SEM_A)) {
                return EACCES;
            }
            return id as isize;
        }
        if semflg & IPC_CREAT == 0 {
            return ENOENT;
        }
    }

    if nsems == 0 || nsems > SEMMSL {
        return EINVAL;
    }
    if registry.sets.len() >= SEMMNI {
        return ENOSPC;
    }
    let (uid, gid) = current_ipc_ids();
    let id = registry.alloc_id();
    let set = match SemSet::new(key, nsems, semflg & 0o777, uid, gid) {
        Ok(set) => set,
        Err(errno) => return errno,
    };
    registry.sets.insert(id, set);
    id as isize
}

fn semctl_copy_stat(id: i32, cmd: usize, buf: usize) -> isize {
    let ds = {
        let registry = SEM_REGISTRY.lock();
        let id = if cmd == SEM_STAT || cmd == SEM_STAT_ANY {
            match registry.id_by_index(id) {
                Some(id) => id,
                None => return EINVAL,
            }
        } else {
            id
        };
        let Some(set) = registry.sets.get(&id) else {
            return EINVAL;
        };
        if cmd != SEM_STAT_ANY && !has_sem_permission(set, SEM_R) {
            return EACCES;
        }
        set.to_semid_ds()
    };
    match copy_to_user(
        current_user_token(),
        &ds as *const LinuxSemidDs,
        buf as *mut LinuxSemidDs,
    ) {
        Ok(()) if cmd == SEM_STAT || cmd == SEM_STAT_ANY => {
            let registry = SEM_REGISTRY.lock();
            registry.id_by_index(id).unwrap_or(id) as isize
        }
        Ok(()) => SUCCESS,
        Err(errno) => errno,
    }
}

pub fn sys_semctl(semid: i32, semnum: usize, cmd: usize, arg: usize) -> isize {
    match cmd {
        IPC_INFO | SEM_INFO => {
            let (info, highest) = {
                let registry = SEM_REGISTRY.lock();
                (
                    seminfo_snapshot(&registry, cmd == SEM_INFO),
                    registry.highest_index(),
                )
            };
            return match copy_to_user(
                current_user_token(),
                &info as *const LinuxSemInfo,
                arg as *mut LinuxSemInfo,
            ) {
                Ok(()) => highest,
                Err(errno) => errno,
            };
        }
        IPC_STAT | SEM_STAT | SEM_STAT_ANY => return semctl_copy_stat(semid, cmd, arg),
        IPC_RMID => {
            let mut registry = SEM_REGISTRY.lock();
            let Some(set) = registry.sets.get(&semid) else {
                return EINVAL;
            };
            if !can_modify_sem_set(set) {
                return EPERM;
            }
            registry.sets.remove(&semid);
            registry.mark_removed(semid);
            registry.wait_queue.wake_all();
            return SUCCESS;
        }
        IPC_SET => {
            let mut ds = LinuxSemidDs {
                sem_perm: LinuxIpcPerm::new(0, 0, 0, 0, 0, 0),
                sem_otime: 0,
                sem_ctime: 0,
                sem_nsems: 0,
                reserved3: 0,
                reserved4: 0,
            };
            if let Err(errno) = copy_from_user(
                current_user_token(),
                arg as *const LinuxSemidDs,
                &mut ds as *mut LinuxSemidDs,
            ) {
                return errno;
            }
            let mut registry = SEM_REGISTRY.lock();
            let Some(set) = registry.sets.get_mut(&semid) else {
                return EINVAL;
            };
            if !can_modify_sem_set(set) {
                return EPERM;
            }
            set.uid = ds.sem_perm.uid;
            set.gid = ds.sem_perm.gid;
            set.mode = ds.sem_perm.mode as usize & 0o777;
            set.ctime = now_sec();
            return SUCCESS;
        }
        _ => {}
    }

    let mut registry = SEM_REGISTRY.lock();
    let Some(set) = registry.sets.get_mut(&semid) else {
        return EINVAL;
    };
    if semnum >= set.semaphores.len() {
        return EINVAL;
    }

    let mut wake_waiters = false;
    let result = match cmd {
        GETPID => set.semaphores[semnum].last_pid as isize,
        GETVAL => {
            if !has_sem_permission(set, SEM_R) {
                return EACCES;
            }
            set.semaphores[semnum].value as isize
        }
        GETNCNT => set.semaphores[semnum].ncnt as isize,
        GETZCNT => set.semaphores[semnum].zcnt as isize,
        GETALL => {
            if !has_sem_permission(set, SEM_R) {
                return EACCES;
            }
            let mut values = Vec::new();
            if values.try_reserve_exact(set.semaphores.len()).is_err() {
                return ENOMEM;
            }
            values.extend(set.semaphores.iter().map(|sem| sem.value as u16));
            match copy_to_user_array(
                current_user_token(),
                values.as_ptr(),
                arg as *mut u16,
                values.len(),
            ) {
                Ok(()) => SUCCESS,
                Err(errno) => errno,
            }
        }
        SETVAL => {
            if !can_modify_sem_set(set) {
                return EPERM;
            }
            let value = match semctl_setval_value(arg) {
                Ok(value) => value,
                Err(errno) => return errno,
            };
            let sem = &mut set.semaphores[semnum];
            sem.value = value;
            sem.last_pid = current_pid_i32();
            set.ctime = now_sec();
            wake_waiters = true;
            SUCCESS
        }
        SETALL => {
            if !can_modify_sem_set(set) {
                return EPERM;
            }
            let mut values = Vec::new();
            if values.try_reserve_exact(set.semaphores.len()).is_err() {
                return ENOMEM;
            }
            values.resize(set.semaphores.len(), 0u16);
            if let Err(errno) = copy_from_user_array(
                current_user_token(),
                arg as *const u16,
                values.as_mut_ptr(),
                values.len(),
            ) {
                return errno;
            }
            if values.iter().any(|value| *value as i32 > SEMVMX) {
                return ERANGE;
            }
            let pid = current_pid_i32();
            for (sem, value) in set.semaphores.iter_mut().zip(values.iter()) {
                sem.value = *value as i32;
                sem.last_pid = pid;
            }
            set.ctime = now_sec();
            wake_waiters = true;
            SUCCESS
        }
        _ => EINVAL,
    };
    if wake_waiters {
        registry.wait_queue.wake_all();
    }
    result
}

enum SemApplyResult {
    Applied,
    Blocked { sem_num: usize, wait_zero: bool },
}

fn try_apply_sem_ops(set: &mut SemSet, ops: &[LinuxSembuf]) -> Result<SemApplyResult, isize> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(set.semaphores.len())
        .map_err(|_| ENOMEM)?;
    values.extend(set.semaphores.iter().map(|sem| sem.value));

    for op in ops {
        let sem_num = op.sem_num as usize;
        if sem_num >= set.semaphores.len() {
            return Err(EFBIG);
        }
        if op.sem_flg & !(IPC_NOWAIT as i16 | SEM_UNDO) != 0 {
            return Err(EINVAL);
        }
        let need = if op.sem_op == 0 { SEM_R } else { SEM_A };
        if !has_sem_permission(set, need) {
            return Err(EACCES);
        }
        if op.sem_op > 0 {
            let next = values[sem_num].saturating_add(op.sem_op as i32);
            if next > SEMVMX {
                return Err(ERANGE);
            }
            values[sem_num] = next;
        } else if op.sem_op < 0 {
            let decrement = -(op.sem_op as i32);
            if values[sem_num] < decrement {
                if op.sem_flg & IPC_NOWAIT as i16 != 0 {
                    return Err(EAGAIN);
                }
                return Ok(SemApplyResult::Blocked {
                    sem_num,
                    wait_zero: false,
                });
            }
            values[sem_num] -= decrement;
        } else if values[sem_num] != 0 {
            if op.sem_flg & IPC_NOWAIT as i16 != 0 {
                return Err(EAGAIN);
            }
            return Ok(SemApplyResult::Blocked {
                sem_num,
                wait_zero: true,
            });
        }
    }

    let pid = current_pid_i32();
    let mut changed = false;
    for op in ops {
        if op.sem_op != 0 {
            let sem = &mut set.semaphores[op.sem_num as usize];
            sem.value = values[op.sem_num as usize];
            sem.last_pid = pid;
            changed = true;
        }
    }
    if changed {
        set.otime = now_sec();
    }
    Ok(SemApplyResult::Applied)
}

fn read_sem_ops(sops: usize, nsops: usize) -> Result<Vec<LinuxSembuf>, isize> {
    if nsops == 0 {
        return Err(EINVAL);
    }
    if nsops > SEMOPM {
        return Err(E2BIG);
    }
    let mut ops = Vec::new();
    ops.try_reserve_exact(nsops).map_err(|_| ENOMEM)?;
    ops.resize(
        nsops,
        LinuxSembuf {
            sem_num: 0,
            sem_op: 0,
            sem_flg: 0,
        },
    );
    copy_from_user_array(
        current_user_token(),
        sops as *const LinuxSembuf,
        ops.as_mut_ptr(),
        nsops,
    )?;
    Ok(ops)
}

fn sem_block_deadline(timeout: usize) -> Result<Option<TimeSpec>, isize> {
    if timeout == 0 {
        return Ok(None);
    }
    let mut ts = TimeSpec::new();
    copy_from_user(
        current_user_token(),
        timeout as *const TimeSpec,
        &mut ts as *mut TimeSpec,
    )?;
    if ts.tv_nsec >= 1_000_000_000 {
        return Err(EINVAL);
    }
    Ok(Some(TimeSpec::now() + ts))
}

fn cleanup_sem_wait(set: &mut SemSet, registered: &mut Option<(usize, bool)>) {
    let Some((sem_num, wait_zero)) = registered.take() else {
        return;
    };
    if let Some(sem) = set.semaphores.get_mut(sem_num) {
        if wait_zero {
            sem.zcnt = sem.zcnt.saturating_sub(1);
        } else {
            sem.ncnt = sem.ncnt.saturating_sub(1);
        }
    }
}

fn update_sem_wait(set: &mut SemSet, registered: &mut Option<(usize, bool)>, next: (usize, bool)) {
    if registered.map(|current| current == next).unwrap_or(false) {
        return;
    }
    cleanup_sem_wait(set, registered);
    if let Some(sem) = set.semaphores.get_mut(next.0) {
        if next.1 {
            sem.zcnt = sem.zcnt.saturating_add(1);
        } else {
            sem.ncnt = sem.ncnt.saturating_add(1);
        }
        *registered = Some(next);
    }
}

fn sem_wait_condition(
    registry: &mut SemRegistry,
    semid: i32,
    ops: &[LinuxSembuf],
    registered: &mut Option<(usize, bool)>,
) -> Option<isize> {
    let mut wake_waiters = false;
    let result = {
        let Some(set) = registry.sets.get_mut(&semid) else {
            return Some(if registry.was_removed(semid) { EIDRM } else { EINVAL });
        };
        match try_apply_sem_ops(set, ops) {
            Ok(SemApplyResult::Applied) => {
                cleanup_sem_wait(set, registered);
                wake_waiters = true;
                Some(SUCCESS)
            }
            Ok(SemApplyResult::Blocked { sem_num, wait_zero }) => {
                update_sem_wait(set, registered, (sem_num, wait_zero));
                None
            }
            Err(errno) => {
                cleanup_sem_wait(set, registered);
                Some(errno)
            }
        }
    };
    if wake_waiters {
        registry.wait_queue.wake_all();
    }
    result
}

pub fn sys_semtimedop(semid: i32, sops: usize, nsops: usize, timeout: usize) -> isize {
    let ops = match read_sem_ops(sops, nsops) {
        Ok(ops) => ops,
        Err(errno) => return errno,
    };

    {
        let mut registry = SEM_REGISTRY.lock();
        let mut wake_waiters = false;
        let result = {
            let Some(set) = registry.sets.get_mut(&semid) else {
                return EINVAL;
            };
            match try_apply_sem_ops(set, &ops) {
                Ok(SemApplyResult::Applied) => {
                    wake_waiters = true;
                    Some(SUCCESS)
                }
                Ok(SemApplyResult::Blocked { .. }) => None,
                Err(errno) => Some(errno),
            }
        };
        if wake_waiters {
            registry.wait_queue.wake_all();
        }
        if let Some(errno) = result {
            return errno;
        }
    }

    let deadline = match sem_block_deadline(timeout) {
        Ok(deadline) => deadline,
        Err(errno) => return errno,
    };
    let mut registered = None;
    let wait_result = if let Some(deadline) = deadline {
        WaitQueue::wait_event_interruptible_timeout_locked(
            &SEM_REGISTRY,
            |registry| &mut registry.wait_queue,
            |registry| sem_wait_condition(registry, semid, &ops, &mut registered),
            deadline,
        )
    } else {
        WaitQueue::wait_event_interruptible_locked(
            &SEM_REGISTRY,
            |registry| &mut registry.wait_queue,
            |registry| sem_wait_condition(registry, semid, &ops, &mut registered),
        )
    };

    let mut registry = SEM_REGISTRY.lock();
    if let Some(set) = registry.sets.get_mut(&semid) {
        cleanup_sem_wait(set, &mut registered);
    }
    match wait_result {
        WaitResult::Ready(value) => value,
        WaitResult::Interrupted => EINTR,
        WaitResult::TimedOut => EAGAIN,
    }
}

pub fn sys_semop(semid: i32, sops: usize, nsops: usize) -> isize {
    sys_semtimedop(semid, sops, nsops, 0)
}

fn has_msg_permission(queue: &MsgQueue, requested: usize) -> bool {
    if requested == 0 {
        return true;
    }
    let (euid, egid) = current_ipc_ids();
    if euid == 0 {
        return true;
    }
    let shift = if euid == queue.uid || euid == queue.cuid {
        6
    } else if egid == queue.gid || egid == queue.cgid {
        3
    } else {
        0
    };
    let available = (queue.mode >> shift) & 0o7;
    let mut need = 0;
    if requested & MSG_R != 0 {
        need |= 0o4;
    }
    if requested & MSG_W != 0 {
        need |= 0o2;
    }
    available & need == need
}

fn can_modify_msg_queue(queue: &MsgQueue) -> bool {
    let (euid, _) = current_ipc_ids();
    euid == 0 || euid == queue.uid || euid == queue.cuid
}

fn checked_msg_payload_addr(msgp: usize, msgsz: usize) -> Result<usize, isize> {
    let payload = msgp.checked_add(size_of::<isize>()).ok_or(EFAULT)?;
    payload.checked_add(msgsz).ok_or(EFAULT)?;
    Ok(payload)
}

fn read_msg_payload(msgp: usize, msgsz: usize) -> Result<(isize, Vec<u8>), isize> {
    if msgp == 0 {
        return Err(EFAULT);
    }
    let token = current_user_token();
    let mut mtype = 0isize;
    copy_from_user(token, msgp as *const isize, &mut mtype as *mut isize)?;
    if mtype <= 0 {
        return Err(EINVAL);
    }

    let payload_addr = checked_msg_payload_addr(msgp, msgsz)?;
    let mut data = Vec::new();
    data.try_reserve_exact(msgsz).map_err(|_| ENOMEM)?;
    data.resize(msgsz, 0);
    if msgsz != 0 {
        copy_from_user_array(
            token,
            payload_addr as *const u8,
            data.as_mut_ptr(),
            msgsz,
        )?;
    }
    Ok((mtype, data))
}

fn write_msg_to_user(msgp: usize, mtype: isize, data: &[u8], copy_len: usize) -> Result<(), isize> {
    let token = current_user_token();
    copy_to_user(token, &mtype as *const isize, msgp as *mut isize)?;
    if copy_len != 0 {
        let payload_addr = checked_msg_payload_addr(msgp, copy_len)?;
        copy_to_user_array(token, data.as_ptr(), payload_addr as *mut u8, copy_len)?;
    }
    Ok(())
}

fn msginfo_snapshot(queue_count: usize) -> LinuxMsgInfo {
    LinuxMsgInfo {
        msgpool: MSGMNI as i32,
        msgmap: MSGMNI as i32,
        msgmax: MSGMAX as i32,
        msgmnb: MSGMNB as i32,
        msgmni: MSGMNI as i32,
        msgssz: 16,
        msgtql: MSGTQL.min(queue_count.saturating_mul(MSGTQL)) as i32,
        msgseg: 0xffff,
    }
}

fn select_msg_index(queue: &MsgQueue, msgtyp: isize, msgflg: usize) -> Option<usize> {
    if msgflg & MSG_COPY != 0 {
        return queue
            .messages
            .get(msgtyp as usize)
            .map(|_| msgtyp as usize);
    }

    if msgtyp == 0 {
        return if queue.messages.is_empty() {
            None
        } else {
            Some(0)
        };
    }

    if msgtyp > 0 {
        if msgflg & MSG_EXCEPT != 0 {
            return queue.messages.iter().position(|msg| msg.mtype != msgtyp);
        }
        return queue.messages.iter().position(|msg| msg.mtype == msgtyp);
    }

    let limit = msgtyp.saturating_abs();
    let mut best: Option<(usize, isize)> = None;
    for (idx, msg) in queue.messages.iter().enumerate() {
        if msg.mtype <= limit && best.map(|(_, mtype)| msg.mtype < mtype).unwrap_or(true) {
            best = Some((idx, msg.mtype));
        }
    }
    best.map(|(idx, _)| idx)
}

pub fn sys_msgget(key: isize, msgflg: usize) -> isize {
    let mut registry = MSG_REGISTRY.lock();
    if key != IPC_PRIVATE {
        if let Some(id) = registry.find_by_key(key) {
            if msgflg & IPC_CREAT != 0 && msgflg & IPC_EXCL != 0 {
                return EEXIST;
            }
            let Some(queue) = registry.queues.get(&id) else {
                return EINVAL;
            };
            if !has_msg_permission(queue, msgflg & (MSG_R | MSG_W)) {
                return EACCES;
            }
            return id as isize;
        }
        if msgflg & IPC_CREAT == 0 {
            return ENOENT;
        }
    }

    if registry.queues.len() >= MSGMNI {
        return ENOSPC;
    }
    let (uid, gid) = current_ipc_ids();
    let id = registry.alloc_id();
    registry
        .queues
        .insert(id, MsgQueue::new(key, msgflg & 0o777, uid, gid));
    id as isize
}

fn try_msgsnd_locked(
    registry: &mut MsgRegistry,
    msqid: i32,
    mtype: isize,
    data: &[u8],
) -> Option<isize> {
    let mut wake_waiters = false;
    let result = {
        let Some(queue) = registry.queues.get_mut(&msqid) else {
            return Some(if registry.was_removed(msqid) { EIDRM } else { EINVAL });
        };
        if !has_msg_permission(queue, MSG_W) {
            return Some(EACCES);
        }
        if queue.cbytes.saturating_add(data.len()) > queue.qbytes
            || queue.messages.len() >= MSGTQL
        {
            return None;
        }

        let serial = queue.next_serial;
        queue.next_serial = queue.next_serial.saturating_add(1).max(1);
        if queue.messages.try_reserve(1).is_err() {
            return Some(ENOMEM);
        }
        let mut payload = Vec::new();
        if payload.try_reserve_exact(data.len()).is_err() {
            return Some(ENOMEM);
        }
        payload.extend_from_slice(data);
        queue.messages.push_back(Message {
            serial,
            mtype,
            data: payload,
        });
        queue.cbytes = queue.cbytes.saturating_add(data.len());
        queue.lspid = current_pid_i32();
        queue.stime = now_sec();
        wake_waiters = true;
        Some(SUCCESS)
    };
    if wake_waiters {
        registry.wait_queue.wake_all();
    }
    result
}

pub fn sys_msgsnd(msqid: i32, msgp: usize, msgsz: usize, msgflg: usize) -> isize {
    if msgsz > MSGMAX || msgflg & !(IPC_NOWAIT) != 0 {
        return EINVAL;
    }
    let (mtype, data) = match read_msg_payload(msgp, msgsz) {
        Ok(msg) => msg,
        Err(errno) => return errno,
    };

    if msgflg & IPC_NOWAIT != 0 {
        let mut registry = MSG_REGISTRY.lock();
        return try_msgsnd_locked(&mut registry, msqid, mtype, &data).unwrap_or(EAGAIN);
    }

    match WaitQueue::wait_event_interruptible_locked(
        &MSG_REGISTRY,
        |registry| &mut registry.wait_queue,
        |registry| try_msgsnd_locked(registry, msqid, mtype, &data),
    ) {
        WaitResult::Ready(value) => value,
        WaitResult::Interrupted => EINTR,
        WaitResult::TimedOut => EINTR,
    }
}

fn msg_recv_wait_condition(
    registry: &mut MsgRegistry,
    msqid: i32,
    msgtyp: isize,
    msgflg: usize,
) -> Option<isize> {
    let Some(queue) = registry.queues.get(&msqid) else {
        return Some(if registry.was_removed(msqid) { EIDRM } else { EINVAL });
    };
    if !has_msg_permission(queue, MSG_R) {
        return Some(EACCES);
    }
    if select_msg_index(queue, msgtyp, msgflg).is_some() {
        Some(SUCCESS)
    } else {
        None
    }
}

fn prepare_msgrcv(
    msqid: i32,
    msgsz: usize,
    msgtyp: isize,
    msgflg: usize,
) -> Result<(u64, isize, Vec<u8>, usize, bool), isize> {
    let registry = MSG_REGISTRY.lock();
    let Some(queue) = registry.queues.get(&msqid) else {
        if registry.was_removed(msqid) {
            return Err(EIDRM);
        }
        return Err(EINVAL);
    };
    if !has_msg_permission(queue, MSG_R) {
        return Err(EACCES);
    }
    let Some(idx) = select_msg_index(queue, msgtyp, msgflg) else {
        return Err(EAGAIN);
    };
    let Some(message) = queue.messages.get(idx) else {
        return Err(EAGAIN);
    };
    if message.data.len() > msgsz && msgflg & MSG_NOERROR == 0 {
        return Err(E2BIG);
    }

    let copy_len = message.data.len().min(msgsz);
    let mut data = Vec::new();
    data.try_reserve_exact(copy_len).map_err(|_| ENOMEM)?;
    data.extend_from_slice(&message.data[..copy_len]);
    Ok((
        message.serial,
        message.mtype,
        data,
        copy_len,
        msgflg & MSG_COPY != 0,
    ))
}

fn remove_received_message(msqid: i32, serial: u64, copy_len: usize) {
    let mut registry = MSG_REGISTRY.lock();
    let mut wake_waiters = false;
    {
        let Some(queue) = registry.queues.get_mut(&msqid) else {
            return;
        };
        if let Some(idx) = queue.messages.iter().position(|msg| msg.serial == serial) {
            if let Some(message) = queue.messages.remove(idx) {
                queue.cbytes = queue.cbytes.saturating_sub(message.data.len());
                queue.lrpid = current_pid_i32();
                queue.rtime = now_sec();
                wake_waiters = true;
            }
        } else if copy_len != 0 {
            queue.rtime = now_sec();
        }
    }
    if wake_waiters {
        registry.wait_queue.wake_all();
    }
}

fn wait_for_msg_recv(msqid: i32, msgtyp: isize, msgflg: usize) -> isize {
    match WaitQueue::wait_event_interruptible_locked(
        &MSG_REGISTRY,
        |registry| &mut registry.wait_queue,
        |registry| msg_recv_wait_condition(registry, msqid, msgtyp, msgflg),
    ) {
        WaitResult::Ready(value) => value,
        WaitResult::Interrupted => EINTR,
        WaitResult::TimedOut => EINTR,
    }
}

pub fn sys_msgrcv(
    msqid: i32,
    msgp: usize,
    msgsz: usize,
    msgtyp: isize,
    msgflg: usize,
) -> isize {
    let allowed_flags = IPC_NOWAIT | MSG_NOERROR | MSG_EXCEPT | MSG_COPY;
    if msgsz > MSGMAX || msgflg & !allowed_flags != 0 {
        return EINVAL;
    }
    if msgflg & MSG_COPY != 0
        && (msgflg & IPC_NOWAIT == 0 || msgflg & MSG_EXCEPT != 0 || msgtyp < 0)
    {
        return EINVAL;
    }

    loop {
        match prepare_msgrcv(msqid, msgsz, msgtyp, msgflg) {
            Ok((serial, mtype, data, copy_len, copy_only)) => {
                if let Err(errno) = write_msg_to_user(msgp, mtype, &data, copy_len) {
                    return errno;
                }
                if !copy_only {
                    remove_received_message(msqid, serial, copy_len);
                }
                return copy_len as isize;
            }
            Err(errno) if errno == EAGAIN => {
                if msgflg & IPC_NOWAIT != 0 {
                    return ENOMSG;
                }
                let wait_result = wait_for_msg_recv(msqid, msgtyp, msgflg);
                if wait_result < 0 {
                    return wait_result;
                }
            }
            Err(errno) => return errno,
        }
    }
}

pub fn sys_msgctl(msqid: i32, cmd: usize, buf: usize) -> isize {
    match cmd {
        IPC_RMID => {
            let mut registry = MSG_REGISTRY.lock();
            let Some(queue) = registry.queues.get(&msqid) else {
                return EINVAL;
            };
            if !can_modify_msg_queue(queue) {
                return EPERM;
            }
            registry.queues.remove(&msqid);
            registry.mark_removed(msqid);
            registry.wait_queue.wake_all();
            SUCCESS
        }
        IPC_STAT | MSG_STAT | MSG_STAT_ANY => {
            let (id, ds) = {
                let registry = MSG_REGISTRY.lock();
                let id = if cmd == MSG_STAT || cmd == MSG_STAT_ANY {
                    match registry.id_by_index(msqid) {
                        Some(id) => id,
                        None => return EINVAL,
                    }
                } else {
                    msqid
                };
                let Some(queue) = registry.queues.get(&id) else {
                    return EINVAL;
                };
                if cmd != MSG_STAT_ANY && !has_msg_permission(queue, MSG_R) {
                    return EACCES;
                }
                (id, queue.to_msqid_ds())
            };
            if let Err(errno) = copy_to_user(
                current_user_token(),
                &ds as *const LinuxMsqidDs,
                buf as *mut LinuxMsqidDs,
            ) {
                return errno;
            }
            if cmd == MSG_STAT || cmd == MSG_STAT_ANY {
                id as isize
            } else {
                SUCCESS
            }
        }
        IPC_SET => {
            let token = current_user_token();
            let mut ds = LinuxMsqidDs {
                msg_perm: LinuxIpcPerm::new(0, 0, 0, 0, 0, 0),
                msg_stime: 0,
                msg_rtime: 0,
                msg_ctime: 0,
                msg_cbytes: 0,
                msg_qnum: 0,
                msg_qbytes: 0,
                msg_lspid: 0,
                msg_lrpid: 0,
                reserved4: 0,
                reserved5: 0,
            };
            if let Err(errno) = copy_from_user(
                token,
                buf as *const LinuxMsqidDs,
                &mut ds as *mut LinuxMsqidDs,
            ) {
                return errno;
            }
            let mut registry = MSG_REGISTRY.lock();
            let Some(queue) = registry.queues.get_mut(&msqid) else {
                return EINVAL;
            };
            if !can_modify_msg_queue(queue) {
                return EPERM;
            }
            queue.uid = ds.msg_perm.uid;
            queue.gid = ds.msg_perm.gid;
            queue.mode = ds.msg_perm.mode as usize & 0o777;
            queue.qbytes = (ds.msg_qbytes as usize).min(MSGMNB * 64);
            queue.ctime = now_sec();
            registry.wait_queue.wake_all();
            SUCCESS
        }
        IPC_INFO | MSG_INFO => {
            let (info, highest) = {
                let registry = MSG_REGISTRY.lock();
                (
                    msginfo_snapshot(registry.queues.len()),
                    registry.highest_index(),
                )
            };
            if let Err(errno) = copy_to_user(
                current_user_token(),
                &info as *const LinuxMsgInfo,
                buf as *mut LinuxMsgInfo,
            ) {
                return errno;
            }
            highest
        }
        _ => EINVAL,
    }
}
