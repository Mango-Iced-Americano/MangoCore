use super::mm::{sys_mmap, sys_munmap};
use crate::mm::{copy_from_user, copy_from_user_array, copy_to_user, copy_to_user_array, MapFlags};
use crate::syscall::errno::*;
use crate::task::{
    current_task, current_user_token, discard_non_actionable_unblocked_signals,
    has_actionable_signal, suspend_current_and_run_next,
};
use crate::timer::{current_timespec, get_time_ms};
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
const MSG_BLOCK_TIMEOUT_MS: usize = 3000;

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
    removed_ids: Vec<i32>,
}

impl MsgRegistry {
    fn new() -> Self {
        Self {
            next_id: 1,
            queues: BTreeMap::new(),
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

fn sleep_for_msg_retry() -> Option<isize> {
    suspend_current_and_run_next();
    let task = current_task().unwrap();
    if has_actionable_signal(&task) {
        Some(EINTR)
    } else {
        discard_non_actionable_unblocked_signals(&task);
        task.acquire_inner_lock().refresh_real_timer();
        None
    }
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

pub fn sys_msgsnd(msqid: i32, msgp: usize, msgsz: usize, msgflg: usize) -> isize {
    if msgsz > MSGMAX || msgflg & !(IPC_NOWAIT) != 0 {
        return EINVAL;
    }
    let (mtype, data) = match read_msg_payload(msgp, msgsz) {
        Ok(msg) => msg,
        Err(errno) => return errno,
    };

    let wait_start = get_time_ms();
    loop {
        {
            let mut registry = MSG_REGISTRY.lock();
            let Some(queue) = registry.queues.get_mut(&msqid) else {
                return EINVAL;
            };
            if !has_msg_permission(queue, MSG_W) {
                return EACCES;
            }
            if queue.cbytes.saturating_add(msgsz) <= queue.qbytes
                && queue.messages.len() < MSGTQL
            {
                let serial = queue.next_serial;
                queue.next_serial = queue.next_serial.saturating_add(1).max(1);
                if queue.messages.try_reserve(1).is_err() {
                    return ENOMEM;
                }
                queue.messages.push_back(Message {
                    serial,
                    mtype,
                    data,
                });
                queue.cbytes = queue.cbytes.saturating_add(msgsz);
                queue.lspid = current_pid_i32();
                queue.stime = now_sec();
                return SUCCESS;
            }
        }

        if msgflg & IPC_NOWAIT != 0 {
            return EAGAIN;
        }
        if get_time_ms().saturating_sub(wait_start) >= MSG_BLOCK_TIMEOUT_MS {
            return EINTR;
        }
        if let Some(errno) = sleep_for_msg_retry() {
            return errno;
        }
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
    let Some(queue) = registry.queues.get_mut(&msqid) else {
        return;
    };
    if let Some(idx) = queue.messages.iter().position(|msg| msg.serial == serial) {
        if let Some(message) = queue.messages.remove(idx) {
            queue.cbytes = queue.cbytes.saturating_sub(message.data.len());
            queue.lrpid = current_pid_i32();
            queue.rtime = now_sec();
        }
    } else if copy_len != 0 {
        queue.rtime = now_sec();
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

    let wait_start = get_time_ms();
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
                if get_time_ms().saturating_sub(wait_start) >= MSG_BLOCK_TIMEOUT_MS {
                    return EINTR;
                }
                if let Some(errno) = sleep_for_msg_retry() {
                    return errno;
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
