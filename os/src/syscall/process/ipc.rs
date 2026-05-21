use super::mm::{sys_mmap, sys_munmap};
use crate::mm::MapFlags;
use crate::syscall::errno::*;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

const IPC_PRIVATE: isize = 0;
const IPC_CREAT: usize = 0o1000;
const IPC_EXCL: usize = 0o2000;
const IPC_RMID: usize = 0;

const SHM_RDONLY: usize = 0o10000;
const SHM_RND: usize = 0o20000;
const SHM_REMAP: usize = 0o40000;
const SHMLBA: usize = crate::config::PAGE_SIZE;
const MAX_SHM_SIZE: usize = 16 * 1024 * 1024;

const PROT_READ: usize = 0x1;
const PROT_WRITE: usize = 0x2;

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
