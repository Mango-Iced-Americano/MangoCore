use super::mm::{sys_mmap, sys_munmap};
use crate::fs::{
    dev::DEV_FS,
    vfs::{
        event::{EPollEvent, EventWaitQueue},
        File, FileFlags, FilePrivateData, FileSystem, FileType, IndexNode, InodeMode, Metadata,
    },
};
use crate::mm::{
    copy_from_user, copy_from_user_array, copy_to_user, copy_to_user_array, translated_str,
    MapFlags,
};
use crate::net::socket::SocketFile;
use crate::syscall::errno::*;
use crate::task::{
    current_task, current_user_token,
    signal::{send_process_signal_info, SigInfo, Signals},
    ProcessManager, WaitQueue, WaitResult,
};
use crate::timer::{current_timespec, TimeSpec};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::fmt::Write;
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
const IPC_64: usize = 0x0100;

fn normalize_ipc_cmd(cmd: usize) -> usize {
    cmd & !IPC_64
}

const SHM_RDONLY: usize = 0o10000;
const SHM_RND: usize = 0o20000;
const SHM_REMAP: usize = 0o40000;
const SHM_EXEC: usize = 0o100000;
const SHM_LOCK: usize = 11;
const SHM_UNLOCK: usize = 12;
const SHM_STAT: usize = 13;
const SHM_INFO: usize = 14;
const SHM_STAT_ANY: usize = 15;
const SHM_DEST: usize = 0o1000;
const SHM_LOCKED: usize = 0o2000;
const SHMLBA: usize = crate::config::PAGE_SIZE;
#[cfg(not(target_arch = "riscv64"))]
const ARCH_SHMLBA: usize = 0x10000;
#[cfg(target_arch = "riscv64")]
const ARCH_SHMLBA: usize = crate::config::PAGE_SIZE;
const SHM_R: usize = 0o400;
const SHM_W: usize = 0o200;
const SHMMNI: usize = 4096;
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

const SIGEV_SIGNAL: i32 = 0;
const SIGEV_NONE: i32 = 1;
const SIGEV_THREAD: i32 = 2;
const MQ_NOTIFY_COOKIE_LEN: usize = 32;
const MQ_NOTIFY_WOKENUP: u8 = 1;
const MQ_NOTIFY_REMOVED: u8 = 2;

const MQ_O_ACCMODE: u32 = FileFlags::O_ACCMODE.bits();
const MQ_O_CREAT: u32 = FileFlags::O_CREAT.bits();
const MQ_O_EXCL: u32 = FileFlags::O_EXCL.bits();
const MQ_O_NONBLOCK: u32 = FileFlags::O_NONBLOCK.bits();
const MQ_O_CLOEXEC: u32 = FileFlags::O_CLOEXEC.bits();
const MQ_OPEN_VALID_FLAGS: u32 =
    MQ_O_ACCMODE | MQ_O_CREAT | MQ_O_EXCL | MQ_O_NONBLOCK | MQ_O_CLOEXEC;
const MQ_DEFAULT_QUEUES_MAX: usize = 256;
const MQ_HARD_QUEUES_MAX: usize = 4096;
const MQ_DEFAULT_MAXMSG: i64 = 10;
const MQ_DEFAULT_MSGSIZE: i64 = 8192;
const MQ_MAX_MAXMSG: i64 = 1024;
const MQ_MAX_MSGSIZE: i64 = 65536;

#[derive(Clone, Copy)]
struct MsgLimits {
    msgmax: usize,
    msgmnb: usize,
    msgmni: usize,
}

#[derive(Clone, Copy)]
struct MqLimits {
    queues_max: usize,
    msg_max: usize,
    msgsize_max: usize,
    msg_default: usize,
    msgsize_default: usize,
}

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

#[derive(Clone, Copy)]
struct SemLimits {
    semmsl: usize,
    semmns: usize,
    semopm: usize,
    semmni: usize,
}

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
            mode: (mode & 0o7777) as LinuxIpcMode,
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

#[repr(C)]
#[derive(Clone, Copy)]
struct LinuxShmidDs {
    shm_perm: LinuxIpcPerm,
    shm_segsz: usize,
    shm_atime: i64,
    shm_dtime: i64,
    shm_ctime: i64,
    shm_cpid: i32,
    shm_lpid: i32,
    shm_nattch: u64,
    unused4: u64,
    unused5: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct LinuxShmInfo {
    shmmax: usize,
    shmmin: usize,
    shmmni: usize,
    shmseg: usize,
    shmall: usize,
    unused1: usize,
    unused2: usize,
    unused3: usize,
    unused4: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct LinuxShmUsageInfo {
    used_ids: i32,
    shm_tot: usize,
    shm_rss: usize,
    shm_swp: usize,
    swap_attempts: usize,
    swap_successes: usize,
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
            qbytes: sysv_msgmnb(),
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
            next_id: -1,
            queues: BTreeMap::new(),
            wait_queue: WaitQueue::new(),
            removed_ids: Vec::new(),
        }
    }

    fn alloc_id(&mut self) -> i32 {
        if self.next_id >= 0 {
            let requested = self.next_id;
            self.next_id = -1;
            if !self.queues.contains_key(&requested) {
                return requested;
            }
        }
        let mut id = 1;
        while self.queues.contains_key(&id) {
            id = id.saturating_add(1).max(1);
        }
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

    fn set_next_id(&mut self, id: i32) -> bool {
        if id < -1 {
            return false;
        }
        self.next_id = id;
        true
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

#[derive(Clone, Copy)]
struct ShmAttachment {
    pid: usize,
    addr: usize,
}

struct ShmSegment {
    key: isize,
    size: usize,
    uid: u32,
    gid: u32,
    cuid: u32,
    cgid: u32,
    mode: usize,
    cpid: i32,
    lpid: i32,
    atime: usize,
    dtime: usize,
    ctime: usize,
    removed: bool,
    locked: bool,
    attachments: Vec<ShmAttachment>,
}

impl ShmSegment {
    fn new(key: isize, size: usize, mode: usize, uid: u32, gid: u32) -> Self {
        let now = now_sec();
        Self {
            key,
            size,
            uid,
            gid,
            cuid: uid,
            cgid: gid,
            mode: mode & 0o777,
            cpid: current_pid_i32(),
            lpid: 0,
            atime: 0,
            dtime: 0,
            ctime: now,
            removed: false,
            locked: false,
            attachments: Vec::new(),
        }
    }

    fn to_shmid_ds(&self) -> LinuxShmidDs {
        let status_bits = if self.removed { SHM_DEST } else { 0 }
            | if self.locked { SHM_LOCKED } else { 0 };
        LinuxShmidDs {
            shm_perm: LinuxIpcPerm::new(
                self.key,
                self.uid,
                self.gid,
                self.cuid,
                self.cgid,
                self.mode | status_bits,
            ),
            shm_segsz: self.size,
            shm_atime: self.atime as i64,
            shm_dtime: self.dtime as i64,
            shm_ctime: self.ctime as i64,
            shm_cpid: self.cpid,
            shm_lpid: self.lpid,
            shm_nattch: self.attachments.len() as u64,
            unused4: 0,
            unused5: 0,
        }
    }
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

    fn id_by_index(&self, index: i32) -> Option<i32> {
        if index < 0 {
            return None;
        }
        self.segments.keys().nth(index as usize).copied()
    }

    fn highest_index(&self) -> isize {
        if self.segments.is_empty() {
            0
        } else {
            self.segments.len() as isize - 1
        }
    }

    fn total_pages(&self) -> usize {
        self.segments
            .values()
            .map(|seg| (seg.size + crate::config::PAGE_SIZE - 1) / crate::config::PAGE_SIZE)
            .sum()
    }
}

lazy_static! {
    static ref SHM_REGISTRY: Mutex<ShmRegistry> = Mutex::new(ShmRegistry::new());
    static ref MSG_LIMITS: Mutex<MsgLimits> = Mutex::new(MsgLimits {
        msgmax: MSGMAX,
        msgmnb: MSGMNB,
        msgmni: MSGMNI,
    });
    static ref SEM_LIMITS: Mutex<SemLimits> = Mutex::new(SemLimits {
        semmsl: SEMMSL,
        semmns: SEMMNI * SEMMSL,
        semopm: SEMOPM,
        semmni: SEMMNI,
    });
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
            if !has_shm_permission(seg, shmflg & (SHM_R | SHM_W)) {
                return EACCES;
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
    if registry.segments.len() >= SHMMNI {
        return ENOSPC;
    }
    let (uid, gid) = current_ipc_ids();
    let id = registry.alloc_id();
    registry.segments.insert(
        id,
        ShmSegment::new(key, size, shmflg & 0o777, uid, gid),
    );
    id as isize
}

pub fn sys_shmat(shmid: i32, shmaddr: usize, shmflg: usize) -> isize {
    const VALID_SHMAT_FLAGS: usize = SHM_RDONLY | SHM_RND | SHM_REMAP | SHM_EXEC;
    if shmflg & !VALID_SHMAT_FLAGS != 0 {
        return EINVAL;
    }
    if shmaddr == 0 && shmflg & SHM_REMAP != 0 {
        return EINVAL;
    }
    let (size, removed) = {
        let registry = SHM_REGISTRY.lock();
        let Some(seg) = registry.segments.get(&shmid) else {
            return EINVAL;
        };
        let requested = if shmflg & SHM_RDONLY != 0 {
            SHM_R
        } else {
            SHM_R | SHM_W
        };
        if !has_shm_permission(seg, requested) {
            return EACCES;
        }
        (seg.size, seg.removed)
    };
    if removed {
        return EIDRM;
    }

    let fixed = shmaddr != 0;
    let attach_addr = if fixed {
        if shmflg & SHM_RND != 0 {
            round_shmat_addr(shmaddr)
        } else if shmaddr & (SHMLBA - 1) != 0 {
            return EINVAL;
        } else {
            shmaddr
        }
    } else {
        0
    };
    if fixed && attach_addr < 0x10000 {
        return EINVAL;
    }
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
    let pid = current_task().map(|task| task.pid()).unwrap_or(0);
    let current_pid = current_pid_i32();
    let mut registry = SHM_REGISTRY.lock();
    if let Some(seg) = registry.segments.get_mut(&shmid) {
        if seg.removed {
            let _ = sys_munmap(mapped, size);
            return EIDRM;
        }
        if seg.attachments.try_reserve(1).is_err() {
            let _ = sys_munmap(mapped, size);
            return ENOMEM;
        }
        seg.attachments.push(ShmAttachment { pid, addr: mapped });
        seg.lpid = current_pid;
        seg.atime = now_sec();
    } else {
        let _ = sys_munmap(mapped, size);
        return EIDRM;
    }
    mapped as isize
}

pub fn sys_shmdt(shmaddr: usize) -> isize {
    let mut detach = None;
    let pid = current_task().map(|task| task.pid()).unwrap_or(0);
    let current_pid = current_pid_i32();
    {
        let mut registry = SHM_REGISTRY.lock();
        for (id, seg) in registry.segments.iter_mut() {
            if let Some(pos) = seg
                .attachments
                .iter()
                .position(|attachment| attachment.pid == pid && attachment.addr == shmaddr)
            {
                seg.attachments.swap_remove(pos);
                seg.lpid = current_pid;
                seg.dtime = now_sec();
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

fn shminfo_snapshot() -> LinuxShmInfo {
    LinuxShmInfo {
        shmmax: MAX_SHM_SIZE,
        shmmin: 1,
        shmmni: SHMMNI,
        shmseg: SHMMNI,
        shmall: SHMMNI * (MAX_SHM_SIZE / crate::config::PAGE_SIZE),
        unused1: 0,
        unused2: 0,
        unused3: 0,
        unused4: 0,
    }
}

pub fn sysv_shmmax() -> usize {
    MAX_SHM_SIZE
}

pub fn sysv_shmmni() -> usize {
    SHMMNI
}

pub fn sysv_shmall() -> usize {
    SHMMNI * (MAX_SHM_SIZE / crate::config::PAGE_SIZE)
}

pub fn sysv_shm_proc_snapshot() -> String {
    let registry = SHM_REGISTRY.lock();
    let mut out = String::from(
        "       key      shmid perms                  size  cpid  lpid nattch   uid   gid  cuid  cgid      atime      dtime      ctime                   rss                  swap\n",
    );
    for (id, seg) in registry.segments.iter() {
        let ds = seg.to_shmid_ds();
        let _ = writeln!(
            out,
            "{:10} {:10} {:5o} {:21} {:5} {:5} {:6} {:5} {:5} {:5} {:5} {:10} {:10} {:10} {:21} {:21}",
            seg.key as i32,
            id,
            ds.shm_perm.mode as usize,
            ds.shm_segsz,
            ds.shm_cpid,
            ds.shm_lpid,
            ds.shm_nattch,
            ds.shm_perm.uid,
            ds.shm_perm.gid,
            ds.shm_perm.cuid,
            ds.shm_perm.cgid,
            ds.shm_atime,
            ds.shm_dtime,
            ds.shm_ctime,
            0,
            0
        );
    }
    out
}

pub fn sysv_msgmax() -> usize {
    MSG_LIMITS.lock().msgmax
}

pub fn sysv_msgmnb() -> usize {
    MSG_LIMITS.lock().msgmnb
}

pub fn sysv_msgmni() -> usize {
    MSG_LIMITS.lock().msgmni
}

pub fn set_sysv_msgmax(value: usize) -> bool {
    if value == 0 {
        return false;
    }
    MSG_LIMITS.lock().msgmax = value;
    true
}

pub fn set_sysv_msgmnb(value: usize) -> bool {
    if value == 0 {
        return false;
    }
    MSG_LIMITS.lock().msgmnb = value;
    true
}

pub fn set_sysv_msgmni(value: usize) -> bool {
    if value == 0 {
        return false;
    }
    MSG_LIMITS.lock().msgmni = value;
    true
}

pub fn sysv_msg_next_id() -> i32 {
    MSG_REGISTRY.lock().next_id
}

pub fn set_sysv_msg_next_id(value: i32) -> bool {
    MSG_REGISTRY.lock().set_next_id(value)
}

pub fn sysv_msg_proc_snapshot() -> String {
    let registry = MSG_REGISTRY.lock();
    let mut out = String::from(
        "       key      msqid perms      cbytes       qnum lspid lrpid   uid   gid  cuid  cgid      stime      rtime      ctime\n",
    );
    for (id, queue) in registry.queues.iter() {
        let _ = writeln!(
            out,
            "{:10} {:10} {:5o} {:11} {:10} {:5} {:5} {:5} {:5} {:5} {:5} {:10} {:10} {:10}",
            queue.key,
            id,
            queue.mode,
            queue.cbytes,
            queue.messages.len(),
            queue.lspid,
            queue.lrpid,
            queue.uid,
            queue.gid,
            queue.cuid,
            queue.cgid,
            queue.stime,
            queue.rtime,
            queue.ctime
        );
    }
    out
}

pub fn sysv_sem_limits() -> (usize, usize, usize, usize) {
    let limits = *SEM_LIMITS.lock();
    (limits.semmsl, limits.semmns, limits.semopm, limits.semmni)
}

pub fn set_sysv_sem_limits(semmsl: usize, semmns: usize, semopm: usize, semmni: usize) -> bool {
    if semmsl == 0 || semmns == 0 || semopm == 0 || semmni == 0 {
        return false;
    }
    if semmsl > SEMMSL || semmns > SEMMNI * SEMMSL || semopm > SEMOPM || semmni > SEMMNI {
        return false;
    }
    *SEM_LIMITS.lock() = SemLimits {
        semmsl,
        semmns,
        semopm,
        semmni,
    };
    true
}

pub fn sysv_sem_proc_snapshot() -> String {
    let registry = SEM_REGISTRY.lock();
    let mut out = String::from(
        "       key      semid perms      nsems   uid   gid  cuid  cgid      otime      ctime\n",
    );
    for (id, set) in registry.sets.iter() {
        let _ = writeln!(
            out,
            "{:10} {:10} {:5o} {:10} {:5} {:5} {:5} {:5} {:10} {:10}",
            set.key as i32,
            id,
            set.mode,
            set.semaphores.len(),
            set.uid,
            set.gid,
            set.cuid,
            set.cgid,
            set.otime,
            set.ctime
        );
    }
    out
}

fn shm_usage_snapshot(registry: &ShmRegistry) -> LinuxShmUsageInfo {
    LinuxShmUsageInfo {
        used_ids: registry.segments.len() as i32,
        shm_tot: registry.total_pages(),
        shm_rss: 0,
        shm_swp: 0,
        swap_attempts: 0,
        swap_successes: 0,
    }
}

fn shmctl_copy_stat(shmid: i32, cmd: usize, buf: usize) -> isize {
    let (real_id, ds) = {
        let registry = SHM_REGISTRY.lock();
        let id = if cmd == SHM_STAT || cmd == SHM_STAT_ANY {
            let id = registry
                .id_by_index(shmid)
                .or_else(|| registry.segments.contains_key(&shmid).then_some(shmid));
            match id {
                Some(id) => id,
                None => return EINVAL,
            }
        } else {
            shmid
        };
        let Some(seg) = registry.segments.get(&id) else {
            return EINVAL;
        };
        if cmd != SHM_STAT_ANY && !has_shm_permission(seg, SHM_R) {
            return EACCES;
        }
        (id, seg.to_shmid_ds())
    };
    match copy_to_user(
        current_user_token(),
        &ds as *const LinuxShmidDs,
        buf as *mut LinuxShmidDs,
    ) {
        Ok(()) if cmd == SHM_STAT || cmd == SHM_STAT_ANY => real_id as isize,
        Ok(()) => SUCCESS,
        Err(errno) => errno,
    }
}

pub fn sys_shmctl(shmid: i32, cmd: usize, buf: usize) -> isize {
    let cmd = normalize_ipc_cmd(cmd);
    match cmd {
        IPC_INFO => {
            let info = shminfo_snapshot();
            let highest = SHM_REGISTRY.lock().highest_index();
            return match copy_to_user(
                current_user_token(),
                &info as *const LinuxShmInfo,
                buf as *mut LinuxShmInfo,
            ) {
                Ok(()) => highest,
                Err(errno) => errno,
            };
        }
        SHM_INFO => {
            let (info, highest) = {
                let registry = SHM_REGISTRY.lock();
                (shm_usage_snapshot(&registry), registry.highest_index())
            };
            return match copy_to_user(
                current_user_token(),
                &info as *const LinuxShmUsageInfo,
                buf as *mut LinuxShmUsageInfo,
            ) {
                Ok(()) => highest,
                Err(errno) => errno,
            };
        }
        IPC_STAT | SHM_STAT | SHM_STAT_ANY => return shmctl_copy_stat(shmid, cmd, buf),
        IPC_SET => {
            let mut ds = LinuxShmidDs {
                shm_perm: LinuxIpcPerm::new(0, 0, 0, 0, 0, 0),
                shm_segsz: 0,
                shm_atime: 0,
                shm_dtime: 0,
                shm_ctime: 0,
                shm_cpid: 0,
                shm_lpid: 0,
                shm_nattch: 0,
                unused4: 0,
                unused5: 0,
            };
            if let Err(errno) = copy_from_user(
                current_user_token(),
                buf as *const LinuxShmidDs,
                &mut ds as *mut LinuxShmidDs,
            ) {
                return errno;
            }
            let mut registry = SHM_REGISTRY.lock();
            let Some(seg) = registry.segments.get_mut(&shmid) else {
                return EINVAL;
            };
            if !can_modify_shm_segment(seg) {
                return EPERM;
            }
            seg.uid = ds.shm_perm.uid;
            seg.gid = ds.shm_perm.gid;
            seg.mode = ds.shm_perm.mode as usize & 0o777;
            seg.ctime = now_sec();
            return SUCCESS;
        }
        IPC_RMID | SHM_LOCK | SHM_UNLOCK => {}
        _ => return EINVAL,
    }

    let mut registry = SHM_REGISTRY.lock();
    let Some(seg) = registry.segments.get_mut(&shmid) else {
        return EINVAL;
    };
    if !can_modify_shm_segment(seg) {
        return EPERM;
    }
    match cmd {
        IPC_RMID => {
            seg.removed = true;
            registry.remove_if_detached(shmid);
            SUCCESS
        }
        SHM_LOCK => {
            seg.locked = true;
            SUCCESS
        }
        SHM_UNLOCK => {
            seg.locked = false;
            SUCCESS
        }
        _ => EINVAL,
    }
}

pub fn shm_detach_process(pid: usize) {
    let mut registry = SHM_REGISTRY.lock();
    let mut maybe_remove = Vec::new();
    for (id, seg) in registry.segments.iter_mut() {
        let before = seg.attachments.len();
        seg.attachments.retain(|attachment| attachment.pid != pid);
        if seg.attachments.len() != before {
            seg.dtime = now_sec();
            seg.lpid = pid as i32;
        }
        if seg.removed && seg.attachments.is_empty() {
            if maybe_remove.try_reserve(1).is_ok() {
                maybe_remove.push(*id);
            }
        }
    }
    for id in maybe_remove {
        registry.segments.remove(&id);
    }
}

pub fn shm_clone_attachments(parent_pid: usize, child_pid: usize) -> Result<(), isize> {
    let mut registry = SHM_REGISTRY.lock();
    let mut inherited = Vec::new();
    for (id, seg) in registry.segments.iter() {
        for attachment in seg
            .attachments
            .iter()
            .filter(|attachment| attachment.pid == parent_pid)
        {
            inherited.try_reserve(1).map_err(|_| ENOMEM)?;
            inherited.push((*id, attachment.addr));
        }
    }
    if inherited.is_empty() {
        return Ok(());
    }
    for (id, seg) in registry.segments.iter_mut() {
        let count = inherited
            .iter()
            .filter(|(inherit_id, _)| inherit_id == id)
            .count();
        if count != 0 {
            seg.attachments.try_reserve(count).map_err(|_| ENOMEM)?;
        }
    }
    for (id, addr) in inherited {
        if let Some(seg) = registry.segments.get_mut(&id) {
            seg.attachments.push(ShmAttachment {
                pid: child_pid,
                addr,
            });
        }
    }
    Ok(())
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

fn round_shmat_addr(addr: usize) -> usize {
    // The bundled loongarch64 musl headers use generic 4K SHMLBA, while glibc
    // uses the arch 64K value; accept both user ABI expectations.
    if ARCH_SHMLBA > SHMLBA && (addr & (ARCH_SHMLBA - 1)) == ARCH_SHMLBA - 1 {
        addr & !(ARCH_SHMLBA - 1)
    } else {
        addr & !(SHMLBA - 1)
    }
}

fn has_shm_permission(seg: &ShmSegment, requested: usize) -> bool {
    if requested == 0 {
        return true;
    }
    let (euid, egid) = current_ipc_ids();
    if euid == 0 {
        return true;
    }
    let shift = if euid == seg.uid || euid == seg.cuid {
        6
    } else if egid == seg.gid || egid == seg.cgid {
        3
    } else {
        0
    };
    let available = (seg.mode >> shift) & 0o7;
    let mut need = 0;
    if requested & SHM_R != 0 {
        need |= 0o4;
    }
    if requested & SHM_W != 0 {
        need |= 0o2;
    }
    available & need == need
}

fn can_modify_shm_segment(seg: &ShmSegment) -> bool {
    let (euid, _) = current_ipc_ids();
    euid == 0 || euid == seg.uid || euid == seg.cuid
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
    let limits = *SEM_LIMITS.lock();
    LinuxSemInfo {
        semmap: limits.semmni as i32,
        semmni: limits.semmni as i32,
        semmns: limits.semmns as i32,
        semmnu: limits.semmni as i32,
        semmsl: limits.semmsl as i32,
        semopm: limits.semopm as i32,
        semume: limits.semopm as i32,
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
        if arg > i32::MAX as usize {
            let mut value = 0i32;
            if copy_from_user(
                current_user_token(),
                arg as *const i32,
                &mut value as *mut i32,
            )
            .is_ok()
            {
                if (0..=SEMVMX).contains(&value) {
                    return Ok(value);
                }
            }
        }
    }

    Err(ERANGE)
}

pub fn sys_semget(key: isize, nsems: usize, semflg: usize) -> isize {
    let mut registry = SEM_REGISTRY.lock();
    let limits = *SEM_LIMITS.lock();
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

    if nsems == 0 || nsems > limits.semmsl {
        return EINVAL;
    }
    if registry.sets.len() >= limits.semmni {
        return ENOSPC;
    }
    if registry.total_semaphores().saturating_add(nsems) > limits.semmns {
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
    let (ds, real_id) = {
        let registry = SEM_REGISTRY.lock();
        let id = if cmd == SEM_STAT || cmd == SEM_STAT_ANY {
            if let Some(id) = registry.id_by_index(id) {
                id
            } else if cmd == SEM_STAT_ANY && registry.sets.contains_key(&id) {
                id
            } else {
                return EINVAL;
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
        (set.to_semid_ds(), id)
    };
    match copy_to_user(
        current_user_token(),
        &ds as *const LinuxSemidDs,
        buf as *mut LinuxSemidDs,
    ) {
        Ok(()) if cmd == SEM_STAT || cmd == SEM_STAT_ANY => real_id as isize,
        Ok(()) => SUCCESS,
        Err(errno) => errno,
    }
}

pub fn sys_semctl(semid: i32, semnum: usize, cmd: usize, arg: usize) -> isize {
    let cmd = normalize_ipc_cmd(cmd);
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
    if nsops > SEM_LIMITS.lock().semopm {
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

fn msginfo_snapshot(registry: &MsgRegistry, usage: bool) -> LinuxMsgInfo {
    let limits = *MSG_LIMITS.lock();
    let queue_count = registry.queues.len();
    let message_count = registry
        .queues
        .values()
        .map(|queue| queue.messages.len())
        .sum::<usize>();
    let message_bytes = registry
        .queues
        .values()
        .map(|queue| queue.cbytes)
        .sum::<usize>();
    LinuxMsgInfo {
        msgpool: if usage { queue_count } else { limits.msgmni } as i32,
        msgmap: if usage { message_count } else { limits.msgmni } as i32,
        msgmax: limits.msgmax as i32,
        msgmnb: limits.msgmnb as i32,
        msgmni: limits.msgmni as i32,
        msgssz: 16,
        msgtql: if usage { message_bytes } else { MSGTQL } as i32,
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

    if registry.queues.len() >= sysv_msgmni() {
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
    if msgsz > sysv_msgmax() || msgflg & !(IPC_NOWAIT) != 0 {
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
    if msgsz > sysv_msgmax() || msgflg & !allowed_flags != 0 {
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
    let cmd = normalize_ipc_cmd(cmd);
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
                    let id = registry
                        .id_by_index(msqid)
                        .or_else(|| (cmd == MSG_STAT_ANY).then_some(msqid))
                        .filter(|id| registry.queues.contains_key(id));
                    match id {
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
            queue.qbytes = (ds.msg_qbytes as usize).min(sysv_msgmnb() * 64);
            queue.ctime = now_sec();
            registry.wait_queue.wake_all();
            SUCCESS
        }
        IPC_INFO | MSG_INFO => {
            let (info, highest) = {
                let registry = MSG_REGISTRY.lock();
                (msginfo_snapshot(&registry, cmd == MSG_INFO), registry.highest_index())
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

#[derive(Clone, Copy, Debug)]
#[repr(C)]
struct LinuxMqAttr {
    mq_flags: i64,
    mq_maxmsg: i64,
    mq_msgsize: i64,
    mq_curmsgs: i64,
}

impl LinuxMqAttr {
    fn new(maxmsg: i64, msgsize: i64) -> Self {
        Self {
            mq_flags: 0,
            mq_maxmsg: maxmsg,
            mq_msgsize: msgsize,
            mq_curmsgs: 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
struct MqSigeventHeader {
    sigev_value: usize,
    sigev_signo: i32,
    sigev_notify: i32,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
struct MqAbsTimeout {
    tv_sec: i64,
    tv_nsec: i64,
}

#[derive(Debug)]
struct MqMessage {
    prio: u32,
    data: Vec<u8>,
}

#[derive(Clone, Debug)]
enum MqNotification {
    None,
    Signal {
        owner_pid: usize,
        signo: usize,
        value: usize,
    },
    Thread {
        netlink_fd: usize,
        cookie: [u8; MQ_NOTIFY_COOKIE_LEN],
    },
}

#[derive(Debug)]
struct MqQueueInner {
    attr: LinuxMqAttr,
    messages: VecDeque<MqMessage>,
    notification: Option<MqNotification>,
    uid: u32,
    gid: u32,
    mode: u32,
}

struct MqQueue {
    inner: Mutex<MqQueueInner>,
    read_wait: EventWaitQueue,
    write_wait: EventWaitQueue,
    metadata: Metadata,
}

impl core::fmt::Debug for MqQueue {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let inner = self.inner.lock();
        f.debug_struct("MqQueue")
            .field("maxmsg", &inner.attr.mq_maxmsg)
            .field("msgsize", &inner.attr.mq_msgsize)
            .field("curmsgs", &inner.messages.len())
            .finish()
    }
}

impl MqQueue {
    fn new(attr: LinuxMqAttr, mode: u32, uid: u32, gid: u32) -> Self {
        let inode_mode = InodeMode::S_IFREG | InodeMode::from_bits_truncate(mode & 0o777);
        let mut metadata = Metadata::new(FileType::File, inode_mode);
        metadata.uid = uid;
        metadata.gid = gid;
        Self {
            inner: Mutex::new(MqQueueInner {
                attr,
                messages: VecDeque::new(),
                notification: None,
                uid,
                gid,
                mode: mode & 0o777,
            }),
            read_wait: EventWaitQueue::new(),
            write_wait: EventWaitQueue::new(),
            metadata,
        }
    }

    fn snapshot_attr(&self, flags: FileFlags) -> LinuxMqAttr {
        let inner = self.inner.lock();
        LinuxMqAttr {
            mq_flags: if flags.contains(FileFlags::O_NONBLOCK) {
                MQ_O_NONBLOCK as i64
            } else {
                0
            },
            mq_maxmsg: inner.attr.mq_maxmsg,
            mq_msgsize: inner.attr.mq_msgsize,
            mq_curmsgs: inner.messages.len() as i64,
        }
    }

    fn notify_readable(&self) {
        self.read_wait
            .notify_events_all(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM);
    }

    fn notify_writable(&self) {
        self.write_wait
            .notify_events_all(EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM);
    }
}

struct MqDescriptor {
    queue: Arc<MqQueue>,
}

impl core::fmt::Debug for MqDescriptor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MqDescriptor").finish()
    }
}

impl IndexNode for MqDescriptor {
    fn metadata(&self) -> Result<Metadata, crate::utils::error::SyscallErr> {
        Ok(self.queue.metadata.clone())
    }

    fn poll(&self, _private_data: &FilePrivateData) -> Result<usize, crate::utils::error::SyscallErr> {
        let inner = self.queue.inner.lock();
        let mut events = EPollEvent::empty();
        if !inner.messages.is_empty() {
            events |= EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM;
        }
        if inner.messages.len() < inner.attr.mq_maxmsg as usize {
            events |= EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM;
        }
        Ok(events.bits())
    }

    fn read_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(self.queue.read_wait.wait_queue())
    }

    fn write_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(self.queue.write_wait.wait_queue())
    }

    fn read_event_queue(&self) -> Option<&EventWaitQueue> {
        Some(&self.queue.read_wait)
    }

    fn write_event_queue(&self) -> Option<&EventWaitQueue> {
        Some(&self.queue.write_wait)
    }

    fn fs(&self) -> Arc<dyn FileSystem> {
        DEV_FS.clone()
    }

    fn is_stream(&self) -> bool {
        true
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}

#[derive(Debug)]
struct MqRegistry {
    queues: BTreeMap<String, Arc<MqQueue>>,
}

lazy_static! {
    static ref MQ_REGISTRY: Mutex<MqRegistry> = Mutex::new(MqRegistry {
        queues: BTreeMap::new(),
    });
    static ref MQ_LIMITS: Mutex<MqLimits> = Mutex::new(MqLimits {
        queues_max: MQ_DEFAULT_QUEUES_MAX,
        msg_max: MQ_MAX_MAXMSG as usize,
        msgsize_max: MQ_MAX_MSGSIZE as usize,
        msg_default: MQ_DEFAULT_MAXMSG as usize,
        msgsize_default: MQ_DEFAULT_MSGSIZE as usize,
    });
}

pub fn posix_mq_queues_max() -> usize {
    MQ_LIMITS.lock().queues_max
}

pub fn posix_mq_msg_max() -> usize {
    MQ_LIMITS.lock().msg_max
}

pub fn posix_mq_msgsize_max() -> usize {
    MQ_LIMITS.lock().msgsize_max
}

pub fn posix_mq_msg_default() -> usize {
    MQ_LIMITS.lock().msg_default
}

pub fn posix_mq_msgsize_default() -> usize {
    MQ_LIMITS.lock().msgsize_default
}

pub fn set_posix_mq_queues_max(value: usize) -> bool {
    if value == 0 || value > MQ_HARD_QUEUES_MAX {
        return false;
    }
    MQ_LIMITS.lock().queues_max = value;
    true
}

pub fn set_posix_mq_msg_max(value: usize) -> bool {
    if value == 0 || value > MQ_MAX_MAXMSG as usize {
        return false;
    }
    let mut limits = MQ_LIMITS.lock();
    if value < limits.msg_default {
        return false;
    }
    limits.msg_max = value;
    true
}

pub fn set_posix_mq_msgsize_max(value: usize) -> bool {
    if value == 0 || value > MQ_MAX_MSGSIZE as usize {
        return false;
    }
    let mut limits = MQ_LIMITS.lock();
    if value < limits.msgsize_default {
        return false;
    }
    limits.msgsize_max = value;
    true
}

pub fn set_posix_mq_msg_default(value: usize) -> bool {
    let mut limits = MQ_LIMITS.lock();
    if value == 0 || value > limits.msg_max {
        return false;
    }
    limits.msg_default = value;
    true
}

pub fn set_posix_mq_msgsize_default(value: usize) -> bool {
    let mut limits = MQ_LIMITS.lock();
    if value == 0 || value > limits.msgsize_max {
        return false;
    }
    limits.msgsize_default = value;
    true
}

fn mq_name_from_user(name: *const u8) -> Result<String, isize> {
    let name = translated_str(current_user_token(), name)?;
    if name.is_empty() {
        return Err(EINVAL);
    }
    if name.as_bytes().contains(&b'/') {
        return Err(EACCES);
    }
    if name.len() + 1 > 256 {
        return Err(ENAMETOOLONG);
    }
    Ok(name)
}

fn mq_attr_from_user(attr: usize) -> Result<LinuxMqAttr, isize> {
    if attr == 0 {
        let limits = *MQ_LIMITS.lock();
        return Ok(LinuxMqAttr::new(
            limits.msg_default as i64,
            limits.msgsize_default as i64,
        ));
    }
    let mut user_attr = LinuxMqAttr::new(0, 0);
    copy_from_user(
        current_user_token(),
        attr as *const LinuxMqAttr,
        &mut user_attr as *mut LinuxMqAttr,
    )?;
    let limits = *MQ_LIMITS.lock();
    if user_attr.mq_maxmsg <= 0
        || user_attr.mq_maxmsg > limits.msg_max as i64
        || user_attr.mq_msgsize <= 0
        || user_attr.mq_msgsize > limits.msgsize_max as i64
    {
        return Err(EINVAL);
    }
    user_attr.mq_flags = 0;
    user_attr.mq_curmsgs = 0;
    Ok(user_attr)
}

fn mq_file_flags(oflag: u32) -> Result<FileFlags, isize> {
    if (oflag & !MQ_OPEN_VALID_FLAGS) != 0 {
        return Err(EINVAL);
    }
    let access = oflag & MQ_O_ACCMODE;
    let mut flags = match access {
        0 => FileFlags::O_RDONLY,
        1 => FileFlags::O_WRONLY,
        2 => FileFlags::O_RDWR,
        _ => return Err(EINVAL),
    };
    if (oflag & MQ_O_NONBLOCK) != 0 {
        flags |= FileFlags::O_NONBLOCK;
    }
    if (oflag & MQ_O_CLOEXEC) != 0 {
        flags |= FileFlags::O_CLOEXEC;
    }
    Ok(flags)
}

fn mq_requested_access(oflag: u32) -> u32 {
    match oflag & MQ_O_ACCMODE {
        0 => 0o4,
        1 => 0o2,
        2 => 0o6,
        _ => 0,
    }
}

fn has_mq_permission(inner: &MqQueueInner, requested: u32) -> bool {
    if requested == 0 {
        return true;
    }
    let (euid, egid) = current_ipc_ids();
    if euid == 0 {
        return true;
    }
    let shift = if euid == inner.uid {
        6
    } else if egid == inner.gid {
        3
    } else {
        0
    };
    let available = (inner.mode >> shift) & 0o7;
    available & requested == requested
}

fn mq_netlink_socket_from_fd(fd: usize) -> Result<Arc<File>, isize> {
    let task = current_task().ok_or(EBADF)?;
    let files = task.process.files();
    let fd_table = files.lock();
    let file = fd_table.get_file(fd).map_err(|err| -(err as isize))?;
    let Some(socket_file) = file.inode_as_any_ref().downcast_ref::<SocketFile>() else {
        return Err(EBADF);
    };
    if !socket_file.inner.is_netlink_socket() {
        return Err(EBADF);
    }
    Ok(file)
}

fn mq_send_netlink_cookie(fd: usize, cookie: [u8; MQ_NOTIFY_COOKIE_LEN], code: u8) {
    let Ok(file) = mq_netlink_socket_from_fd(fd) else {
        return;
    };
    let Some(socket_file) = file.inode_as_any_ref().downcast_ref::<SocketFile>() else {
        return;
    };
    let mut data = Vec::new();
    if data.try_reserve(MQ_NOTIFY_COOKIE_LEN).is_err() {
        return;
    }
    data.extend_from_slice(&cookie);
    data[MQ_NOTIFY_COOKIE_LEN - 1] = code;
    let _ = socket_file.inner.push_netlink_message(data);
}

fn mq_deliver_notification(notification: MqNotification) {
    match notification {
        MqNotification::None => {}
        MqNotification::Signal {
            owner_pid,
            signo,
            value,
        } => {
            let Ok(signal) = Signals::from_signum(signo) else {
                return;
            };
            let Some(process) = ProcessManager::find_process(owner_pid) else {
                return;
            };
            let siginfo = SigInfo::new_with_sender_value(
                signo,
                0,
                SigInfo::SI_MESGQ as usize,
                owner_pid,
                value,
            );
            send_process_signal_info(&process, signal, siginfo);
        }
        MqNotification::Thread { netlink_fd, cookie } => {
            mq_send_netlink_cookie(netlink_fd, cookie, MQ_NOTIFY_WOKENUP);
        }
    }
}

fn mq_descriptor_from_fd(mqdes: usize) -> Result<(Arc<File>, Arc<MqQueue>), isize> {
    let task = current_task().ok_or(EBADF)?;
    let file = {
        let files = task.process.files();
        let fd_table = files.lock();
        let file = fd_table.get_file(mqdes).map_err(|err| -(err as isize))?;
        file
    };
    let queue = {
        let Some(desc) = file.inode_as_any_ref().downcast_ref::<MqDescriptor>() else {
            return Err(EBADF);
        };
        desc.queue.clone()
    };
    Ok((file, queue))
}

fn mq_timeout_deadline(timeout: usize) -> Result<Option<TimeSpec>, isize> {
    if timeout == 0 {
        return Ok(None);
    }
    let mut ts = MqAbsTimeout {
        tv_sec: 0,
        tv_nsec: 0,
    };
    copy_from_user(
        current_user_token(),
        timeout as *const MqAbsTimeout,
        &mut ts as *mut MqAbsTimeout,
    )?;
    if ts.tv_sec < 0 || ts.tv_nsec < 0 || ts.tv_nsec >= 1_000_000_000 {
        return Err(EINVAL);
    }
    let realtime_deadline = TimeSpec {
        tv_sec: ts.tv_sec as usize,
        tv_nsec: ts.tv_nsec as usize,
    };
    let now_realtime = current_timespec();
    let duration = if realtime_deadline > now_realtime {
        realtime_deadline - now_realtime
    } else {
        TimeSpec::new()
    };
    Ok(Some(TimeSpec::now() + duration))
}

fn mq_wait_send_ready(queue: &MqQueue, abs_timeout: usize) -> isize {
    let deadline = match mq_timeout_deadline(abs_timeout) {
        Ok(deadline) => deadline,
        Err(errno) => return errno,
    };
    let wait_queue = queue.write_wait.wait_queue();
    let result = match deadline {
        Some(deadline) => WaitQueue::wait_event_interruptible_timeout(wait_queue, || {
            let inner = queue.inner.lock();
            if inner.messages.len() < inner.attr.mq_maxmsg as usize {
                Some(SUCCESS)
            } else {
                None
            }
        }, deadline),
        None => WaitQueue::wait_event_interruptible(wait_queue, || {
            let inner = queue.inner.lock();
            if inner.messages.len() < inner.attr.mq_maxmsg as usize {
                Some(SUCCESS)
            } else {
                None
            }
        }),
    };
    match result {
        WaitResult::Ready(_) => SUCCESS,
        WaitResult::Interrupted => EINTR,
        WaitResult::TimedOut => ETIMEDOUT,
    }
}

fn mq_wait_receive_ready(queue: &MqQueue, abs_timeout: usize) -> isize {
    let deadline = match mq_timeout_deadline(abs_timeout) {
        Ok(deadline) => deadline,
        Err(errno) => return errno,
    };
    let wait_queue = queue.read_wait.wait_queue();
    let result = match deadline {
        Some(deadline) => WaitQueue::wait_event_interruptible_timeout(wait_queue, || {
            let inner = queue.inner.lock();
            if !inner.messages.is_empty() {
                Some(SUCCESS)
            } else {
                None
            }
        }, deadline),
        None => WaitQueue::wait_event_interruptible(wait_queue, || {
            let inner = queue.inner.lock();
            if !inner.messages.is_empty() {
                Some(SUCCESS)
            } else {
                None
            }
        }),
    };
    match result {
        WaitResult::Ready(_) => SUCCESS,
        WaitResult::Interrupted => EINTR,
        WaitResult::TimedOut => ETIMEDOUT,
    }
}

pub fn sys_mq_open(name: *const u8, oflag: u32, _mode: u32, attr: usize) -> isize {
    let name = match mq_name_from_user(name) {
        Ok(name) => name,
        Err(errno) => return errno,
    };
    let file_flags = match mq_file_flags(oflag) {
        Ok(flags) => flags,
        Err(errno) => return errno,
    };

    let mut created = false;
    let queues_max = posix_mq_queues_max();
    let queue = {
        let mut registry = MQ_REGISTRY.lock();
        if let Some(queue) = registry.queues.get(&name) {
            if (oflag & (MQ_O_CREAT | MQ_O_EXCL)) == (MQ_O_CREAT | MQ_O_EXCL) {
                return EEXIST;
            }
            if !has_mq_permission(&queue.inner.lock(), mq_requested_access(oflag)) {
                return EACCES;
            }
            queue.clone()
        } else {
            if (oflag & MQ_O_CREAT) == 0 {
                return ENOENT;
            }
            if registry.queues.len() >= queues_max {
                return ENOSPC;
            }
            let attr = match mq_attr_from_user(attr) {
                Ok(attr) => attr,
                Err(errno) => return errno,
            };
            let (uid, gid) = current_ipc_ids();
            let queue = Arc::new(MqQueue::new(attr, _mode, uid, gid));
            registry.queues.insert(name.clone(), queue.clone());
            created = true;
            queue
        }
    };

    let inode = Arc::new(MqDescriptor {
        queue: queue.clone(),
    }) as Arc<dyn IndexNode>;
    let file = match File::new(inode, file_flags) {
        Ok(file) => file,
        Err(err) => {
            if created {
                MQ_REGISTRY.lock().queues.remove(&name);
            }
            return -(err as isize);
        }
    };

    let task = current_task().unwrap();
    let ret = match task
        .process
        .files()
        .lock()
        .alloc_fd(file, (oflag & MQ_O_CLOEXEC) != 0)
    {
        Ok(fd) => fd as isize,
        Err(err) => {
            if created {
                MQ_REGISTRY.lock().queues.remove(&name);
            }
            -(err as isize)
        }
    };
    ret
}

pub fn sys_mq_unlink(name: *const u8) -> isize {
    let name = match mq_name_from_user(name) {
        Ok(name) => name,
        Err(errno) => return errno,
    };
    let mut registry = MQ_REGISTRY.lock();
    let Some(queue) = registry.queues.get(&name) else {
        return ENOENT;
    };
    if !has_mq_permission(&queue.inner.lock(), 0o2) {
        return EACCES;
    }
    registry.queues.remove(&name);
    SUCCESS
}

pub fn sys_mq_timedsend(
    mqdes: usize,
    msg_ptr: usize,
    msg_len: usize,
    msg_prio: u32,
    abs_timeout: usize,
) -> isize {
    let (file, queue) = match mq_descriptor_from_fd(mqdes) {
        Ok(v) => v,
        Err(errno) => return errno,
    };
    if file.writable().is_err() {
        return EBADF;
    }

    let msgsize = queue.inner.lock().attr.mq_msgsize as usize;
    if msg_len > msgsize {
        return EMSGSIZE;
    }

    let token = current_user_token();
    let mut data = Vec::new();
    if data.try_reserve(msg_len).is_err() {
        return ENOMEM;
    }
    data.resize(msg_len, 0);
    if let Err(errno) = copy_from_user_array(token, msg_ptr as *const u8, data.as_mut_ptr(), msg_len)
    {
        return errno;
    }

    let notification = loop {
        let mut inner = queue.inner.lock();
        if inner.messages.len() >= inner.attr.mq_maxmsg as usize {
            if file.is_nonblock() {
                return EAGAIN;
            }
            drop(inner);
            let errno = mq_wait_send_ready(&queue, abs_timeout);
            if errno != SUCCESS {
                return errno;
            }
            continue;
        }
        let pos = inner
            .messages
            .iter()
            .position(|message| message.prio < msg_prio)
            .unwrap_or(inner.messages.len());
        let was_empty = inner.messages.is_empty();
        inner.messages.insert(pos, MqMessage { prio: msg_prio, data });
        break if was_empty {
            inner.notification.take()
        } else {
            None
        };
    };

    queue.notify_readable();
    if let Some(notification) = notification {
        mq_deliver_notification(notification);
    }
    SUCCESS
}

pub fn sys_mq_timedreceive(
    mqdes: usize,
    msg_ptr: usize,
    msg_len: usize,
    msg_prio: *mut u32,
    abs_timeout: usize,
) -> isize {
    let (file, queue) = match mq_descriptor_from_fd(mqdes) {
        Ok(v) => v,
        Err(errno) => return errno,
    };
    if file.readable().is_err() {
        return EBADF;
    }

    let msgsize = queue.inner.lock().attr.mq_msgsize as usize;
    if msg_len < msgsize {
        return EMSGSIZE;
    }

    let message = loop {
        let mut inner = queue.inner.lock();
        match inner.messages.pop_front() {
            Some(message) => break message,
            None => {
                if file.is_nonblock() {
                    return EAGAIN;
                }
                drop(inner);
                let errno = mq_wait_receive_ready(&queue, abs_timeout);
                if errno != SUCCESS {
                    return errno;
                }
            }
        }
    };

    let token = current_user_token();
    if let Err(errno) =
        copy_to_user_array(token, message.data.as_ptr(), msg_ptr as *mut u8, message.data.len())
    {
        return errno;
    }
    if !msg_prio.is_null() {
        if let Err(errno) = copy_to_user(token, &message.prio as *const u32, msg_prio) {
            return errno;
        }
    }

    queue.notify_writable();
    message.data.len() as isize
}

pub fn sys_mq_getsetattr(mqdes: usize, newattr: usize, oldattr: usize) -> isize {
    let (file, queue) = match mq_descriptor_from_fd(mqdes) {
        Ok(v) => v,
        Err(errno) => return errno,
    };

    if oldattr != 0 {
        let snapshot = queue.snapshot_attr(file.flags());
        if let Err(errno) = copy_to_user(
            current_user_token(),
            &snapshot as *const LinuxMqAttr,
            oldattr as *mut LinuxMqAttr,
        ) {
            return errno;
        }
    }

    if newattr != 0 {
        let mut requested = LinuxMqAttr::new(0, 0);
        if let Err(errno) = copy_from_user(
            current_user_token(),
            newattr as *const LinuxMqAttr,
            &mut requested as *mut LinuxMqAttr,
        ) {
            return errno;
        }
        if (requested.mq_flags as u32) & !MQ_O_NONBLOCK != 0 {
            return EINVAL;
        }
        let mut flags = file.flags();
        if (requested.mq_flags as u32 & MQ_O_NONBLOCK) != 0 {
            flags |= FileFlags::O_NONBLOCK;
        } else {
            flags.remove(FileFlags::O_NONBLOCK);
        }
        if let Err(err) = file.set_flags(flags) {
            return -(err as isize);
        }
    }

    SUCCESS
}

pub fn sys_mq_notify(mqdes: usize, sevp: usize) -> isize {
    let notification = if sevp == 0 {
        None
    } else {
        let mut event = MqSigeventHeader {
            sigev_value: 0,
            sigev_signo: 0,
            sigev_notify: 0,
        };
        if let Err(errno) = copy_from_user(
            current_user_token(),
            sevp as *const MqSigeventHeader,
            &mut event as *mut MqSigeventHeader,
        ) {
            return errno;
        }

        let owner_pid = current_task().map(|task| task.pid()).unwrap_or(0);
        match event.sigev_notify {
            SIGEV_NONE => Some(MqNotification::None),
            SIGEV_SIGNAL => {
                if Signals::from_signum(event.sigev_signo as usize).is_err() {
                    return EINVAL;
                }
                Some(MqNotification::Signal {
                    owner_pid,
                    signo: event.sigev_signo as usize,
                    value: event.sigev_value,
                })
            }
            SIGEV_THREAD => {
                if event.sigev_signo < 0 || event.sigev_value == 0 {
                    return EBADF;
                }
                let mut cookie = [0u8; MQ_NOTIFY_COOKIE_LEN];
                if let Err(errno) = copy_from_user_array(
                    current_user_token(),
                    event.sigev_value as *const u8,
                    cookie.as_mut_ptr(),
                    MQ_NOTIFY_COOKIE_LEN,
                ) {
                    return errno;
                }
                if mq_netlink_socket_from_fd(event.sigev_signo as usize).is_err() {
                    return EBADF;
                }
                Some(MqNotification::Thread {
                    netlink_fd: event.sigev_signo as usize,
                    cookie,
                })
            }
            _ => return EINVAL,
        }
    };

    let (_, queue) = match mq_descriptor_from_fd(mqdes) {
        Ok(v) => v,
        Err(errno) => return errno,
    };

    if notification.is_none() {
        let removed = queue.inner.lock().notification.take();
        if let Some(MqNotification::Thread { netlink_fd, cookie }) = removed {
            mq_send_netlink_cookie(netlink_fd, cookie, MQ_NOTIFY_REMOVED);
        }
        return SUCCESS;
    }

    let mut inner = queue.inner.lock();
    if inner.notification.is_some() {
        return EBUSY;
    }
    inner.notification = notification;
    SUCCESS
}
