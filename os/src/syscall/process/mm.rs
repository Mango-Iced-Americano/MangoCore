use crate::config::PAGE_SIZE;
use crate::fs::vfs;
use crate::mm::{translated_byte_buffer, MapFlags, MapPermission, UserAccess};
use crate::syscall::errno::*;
use crate::task::{current_task, current_user_token};
use crate::utils::error::SyscallErr;
use log::{error, info, warn};

pub fn sys_sbrk(increment: isize) -> isize {
    let task = current_task().unwrap();
    let vm = task.process.vm();
    let new_addr = vm.lock().sbrk(increment);
    new_addr as isize
}

pub fn sys_brk(brk_addr: usize) -> isize {
    let task = current_task().unwrap();
    let vm = task.process.vm();
    let mut memory_set = vm.lock();
    let new_addr = if brk_addr == 0 {
        memory_set.sbrk(0)
    } else {
        let former_addr = memory_set.sbrk(0);
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
        memory_set.sbrk(grow_size)
    };

    info!(
        "[sys_brk] brk_addr: {:X}; new_addr: {:X}",
        brk_addr, new_addr
    );
    new_addr as isize
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

    let map_file = if flags.contains(MapFlags::MAP_ANONYMOUS) {
        None
    } else {
        if offset & (PAGE_SIZE - 1) != 0 || offset > isize::MAX as usize {
            return EINVAL;
        }
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        let file = match fd_table.get_file(fd) {
            Ok(file) => file,
            Err(e) => return -(e as isize),
        };
        if file.readable().is_err() {
            return EACCES;
        }
        if flags.contains(MapFlags::MAP_SHARED)
            && prot.contains(MapPermission::W)
            && file.writable().is_err()
        {
            return EACCES;
        }
        if !matches!(file.file_type(), vfs::FileType::File) {
            return EACCES;
        }
        Some(file.inode.clone())
    };

    let vm_ref = task.process.vm();
    let mut memory_set = vm_ref.lock();
    memory_set.mmap(start, len, prot, flags, offset, map_file)
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
    let result = task.process.vm().lock().munmap(start, len);
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
    let result = task.process.vm().lock().mprotect(addr, len, prot);
    match result {
        Ok(_) => SUCCESS,
        Err(errno) => errno,
    }
}

pub fn sys_mlock(addr: usize, len: usize) -> isize {
    if len == 0 {
        return SUCCESS;
    }
    if addr.checked_add(len).is_none() {
        return EINVAL;
    }
    match translated_byte_buffer(current_user_token(), addr as *const u8, len, UserAccess::Read) {
        Ok(_) => SUCCESS,
        Err(errno) => errno,
    }
}

pub fn sys_munlock(addr: usize, len: usize) -> isize {
    sys_mlock(addr, len)
}

pub fn sys_mlockall(flags: usize) -> isize {
    const MCL_CURRENT: usize = 1;
    const MCL_FUTURE: usize = 2;
    const MCL_ONFAULT: usize = 4;
    if flags & !(MCL_CURRENT | MCL_FUTURE | MCL_ONFAULT) != 0 {
        return EINVAL;
    }
    SUCCESS
}

pub fn sys_munlockall() -> isize {
    SUCCESS
}

pub fn sys_madvise(_addr: usize, _length: usize, _advice: usize) -> isize {
    // 暂时返回 EINVAL
    -(SyscallErr::EINVAL as isize)
}
