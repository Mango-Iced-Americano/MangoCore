use crate::config::PAGE_SIZE;
use crate::fs::vfs;
use crate::mm::{
    copy_from_user_array, copy_to_user_array, fault_in_user_range, MapFlags, MapPermission,
    UserAccess,
};
use crate::syscall::errno::*;
use crate::task::{current_task, current_user_token};
use alloc::vec::Vec;
use log::warn;

const PROT_READ: usize = 0x1;
const PROT_WRITE: usize = 0x2;
const PROT_EXEC: usize = 0x4;
const PKEY_DISABLE_ACCESS: usize = 0x1;
const PKEY_DISABLE_WRITE: usize = 0x2;
const PKEY_DISABLE_ACCESS_WRITE: usize = PKEY_DISABLE_ACCESS | PKEY_DISABLE_WRITE;
const PKEY_DISABLE_EXECUTE: usize = 0x4;
const PKEY_NO_ACCESS_RIGHTS_KEY: isize = 1;
const PKEY_ACCESS_KEY: isize = 2;
const PKEY_WRITE_KEY: isize = 3;
const CAP_IPC_LOCK: usize = 14;
const SYS_RISCV_FLUSH_ICACHE_LOCAL: usize = 1;

#[cfg(feature = "riscv")]
pub fn sys_riscv_flush_icache(_start: usize, _end: usize, flags: usize) -> isize {
    if flags & !SYS_RISCV_FLUSH_ICACHE_LOCAL != 0 {
        return EINVAL;
    }

    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("fence.i", options(nostack, preserves_flags));
    }

    SUCCESS
}

pub fn sys_sbrk(increment: isize) -> isize {
    let task = current_task().unwrap();
    let vm = task.process.vm();
    let new_addr = vm.write(|memory_set| memory_set.sbrk(increment));
    new_addr as isize
}

pub fn sys_brk(brk_addr: usize) -> isize {
    let task = current_task().unwrap();
    let vm = task.process.vm();
    let new_addr = vm.write(|memory_set| {
        if brk_addr == 0 {
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
        }
    });

    new_addr as isize
}

fn parse_mmap_prot(prot: usize) -> Result<MapPermission, isize> {
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
    let type_bits = flags & MapFlags::MAP_TYPE.bits();
    if type_bits != MapFlags::MAP_SHARED.bits()
        && type_bits != MapFlags::MAP_PRIVATE.bits()
        && type_bits != MapFlags::MAP_SHARED_VALIDATE.bits()
    {
        return Err(EINVAL);
    }
    let unknown_bits = flags & !MapFlags::all().bits();
    if type_bits == MapFlags::MAP_SHARED_VALIDATE.bits() && unknown_bits != 0 {
        return Err(EOPNOTSUPP);
    }
    Ok(MapFlags::from_bits_truncate(flags))
}

pub fn sys_mmap(
    start: usize,
    len: usize,
    prot: usize,
    flags: usize,
    fd: usize,
    offset: usize,
) -> isize {
    let (files_ref, vm_ref) = {
        let task = current_task().unwrap();
        (task.process.files(), task.process.vm())
    };
    let fd_file = if flags & MapFlags::MAP_ANONYMOUS.bits() == 0 {
        let fd_table = files_ref.lock();
        match fd_table.get_file(fd) {
            Ok(file) => Some(file),
            Err(_) => return EBADF,
        }
    } else {
        None
    };
    if len == 0 {
        return EINVAL;
    }
    let prot = match parse_mmap_prot(prot) {
        Ok(prot) => prot,
        Err(errno) => return errno,
    };
    let mut flags = match parse_mmap_flags(flags) {
        Ok(flags) => flags,
        Err(errno) => return errno,
    };
    let mut may_write = true;
    let mut write_sealed = false;
    let map_file = if flags.contains(MapFlags::MAP_ANONYMOUS) {
        None
    } else {
        if offset & (PAGE_SIZE - 1) != 0 || offset > isize::MAX as usize {
            return EINVAL;
        }
        let file = fd_file.as_ref().unwrap();
        if file.readable().is_err() {
            return EACCES;
        }
        let file_writable = file.writable().is_ok();
        if flags.contains(MapFlags::MAP_SHARED) && prot.contains(MapPermission::W) && !file_writable
        {
            return EACCES;
        }
        let seals = file.memfd_seal_bits().unwrap_or(0);
        let sealed_against_write = (seals & (vfs::F_SEAL_WRITE | vfs::F_SEAL_FUTURE_WRITE)) != 0;
        if flags.contains(MapFlags::MAP_SHARED) && sealed_against_write {
            write_sealed = true;
            if prot.contains(MapPermission::W) {
                return EPERM;
            }
        }
        if flags.contains(MapFlags::MAP_SHARED) {
            may_write = file_writable;
        }
        let inode = vfs::MountFSInode::unwrap_inode(&file.inode);
        let is_zero = inode.as_any_ref().is::<crate::fs::dev::zero::Zero>();
        if !is_zero && !matches!(file.file_type(), vfs::FileType::File) {
            return EACCES;
        }
        if is_zero {
            flags |= MapFlags::MAP_ANONYMOUS;
            None
        } else {
            Some(inode)
        }
    };

    vm_ref.write(|memory_set| {
        memory_set.mmap(
            start,
            len,
            prot,
            flags,
            offset,
            map_file,
            may_write,
            write_sealed,
        )
    })
}

/// # Versions
/// The membarrier() system call was added in Linux 4.3.
/// Before Linux 5.10, the prototype for membarrier() was:
/// `int membarrier(int cmd, int flags);`
pub fn sys_memorybarrier(cmd: usize, flags: usize, _cpu_id: usize) -> isize {
    const MEMBARRIER_CMD_QUERY: usize = 0;
    const MEMBARRIER_CMD_GLOBAL: usize = 1 << 0;
    const MEMBARRIER_CMD_PRIVATE_EXPEDITED: usize = 1 << 3;
    const MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED: usize = 1 << 4;
    const MEMBARRIER_SUPPORTED_CMDS: usize = MEMBARRIER_CMD_GLOBAL
        | MEMBARRIER_CMD_PRIVATE_EXPEDITED
        | MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED;

    if flags != 0 {
        return EINVAL;
    }

    match cmd {
        MEMBARRIER_CMD_QUERY => MEMBARRIER_SUPPORTED_CMDS as isize,
        MEMBARRIER_CMD_GLOBAL => run_memory_barrier(crate::smp::online_cpu_mask()),
        MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED => {
            current_task()
                .unwrap()
                .process
                .vm()
                .register_private_expedited();
            SUCCESS
        }
        MEMBARRIER_CMD_PRIVATE_EXPEDITED => {
            let vm = current_task().unwrap().process.vm();
            match vm.private_expedited_targets() {
                Some(targets) => run_memory_barrier(targets),
                None => EPERM,
            }
        }
        _ => EINVAL,
    }
}

/// 执行已经完成 ABI 校验的 membarrier。
///
/// advertised 命令不能在运行期静默降级；若 IPI/ack 基础设施无法兑现完整
/// 内存序承诺，继续运行会让用户同步算法得到错误结果，因此明确 fail-stop。
fn run_memory_barrier(targets: usize) -> isize {
    if let Err(error) = crate::smp::synchronize_memory(targets) {
        panic!(
            "membarrier synchronization failed: targets={:#x} error={:?}",
            targets, error
        );
    }
    SUCCESS
}

pub fn sys_munmap(start: usize, len: usize) -> isize {
    let task = current_task().unwrap();
    let result = task.process.vm().write(|vm| vm.munmap(start, len));
    match result {
        Ok(_) => SUCCESS,
        Err(errno) => errno,
    }
}

fn page_round_up_len(len: usize) -> Option<usize> {
    len.checked_add(PAGE_SIZE - 1)
        .map(|len| len & !(PAGE_SIZE - 1))
}

fn checked_page_range(start: usize, len: usize) -> Result<usize, isize> {
    let end = start.checked_add(len).ok_or(EINVAL)?;
    if start >= crate::config::USER_VA_END || end > crate::config::USER_VA_END {
        return Err(EINVAL);
    }
    Ok(end)
}

fn ranges_overlap(a_start: usize, a_len: usize, b_start: usize, b_len: usize) -> bool {
    let Some(a_end) = a_start.checked_add(a_len) else {
        return true;
    };
    let Some(b_end) = b_start.checked_add(b_len) else {
        return true;
    };
    a_start < b_end && b_start < a_end
}

fn copy_current_user_range(src: usize, dst: usize, len: usize) -> Result<(), isize> {
    let token = current_user_token();
    let mut copied = 0usize;
    let mut scratch = [0u8; PAGE_SIZE];
    while copied < len {
        let chunk_len = (len - copied).min(PAGE_SIZE);
        let src_addr = src.checked_add(copied).ok_or(EFAULT)?;
        let dst_addr = dst.checked_add(copied).ok_or(EFAULT)?;
        // 两次 copy 分别在对应用户页的 VM 锁内完成；scratch 让源 PA 不跨解锁点泄漏。
        copy_from_user_array(
            token,
            src_addr as *const u8,
            scratch.as_mut_ptr(),
            chunk_len,
        )?;
        copy_to_user_array(token, scratch.as_ptr(), dst_addr as *mut u8, chunk_len)?;
        copied += chunk_len;
    }
    Ok(())
}

pub fn sys_mremap(
    old_addr: usize,
    old_size: usize,
    new_size: usize,
    flags: usize,
    new_addr: usize,
) -> isize {
    const MREMAP_MAYMOVE: usize = 0x1;
    const MREMAP_FIXED: usize = 0x2;
    const MREMAP_DONTUNMAP: usize = 0x4;
    const MREMAP_ALLOWED: usize = MREMAP_MAYMOVE | MREMAP_FIXED | MREMAP_DONTUNMAP;

    if old_addr & (PAGE_SIZE - 1) != 0 || flags & !MREMAP_ALLOWED != 0 {
        return EINVAL;
    }
    let old_len = match page_round_up_len(old_size) {
        Some(0) | None => return EINVAL,
        Some(len) => len,
    };
    let new_len = match page_round_up_len(new_size) {
        Some(0) | None => return EINVAL,
        Some(len) => len,
    };
    if checked_page_range(old_addr, old_len).is_err() {
        return EINVAL;
    }

    let may_move = flags & MREMAP_MAYMOVE != 0;
    let fixed = flags & MREMAP_FIXED != 0;
    let dont_unmap = flags & MREMAP_DONTUNMAP != 0;
    if fixed && !may_move {
        return EINVAL;
    }
    if dont_unmap && (!may_move || old_len != new_len) {
        return EINVAL;
    }

    if !fixed && !dont_unmap && new_len <= old_len {
        if new_len < old_len {
            let tail = match old_addr.checked_add(new_len) {
                Some(addr) => addr,
                None => return EINVAL,
            };
            let ret = sys_munmap(tail, old_len - new_len);
            if ret < 0 {
                return ret;
            }
        }
        return old_addr as isize;
    }

    if !may_move {
        let tail = match old_addr.checked_add(old_len) {
            Some(addr) => addr,
            None => return EINVAL,
        };
        let grow_len = new_len - old_len;
        let ret = sys_mmap(
            tail,
            grow_len,
            PROT_READ | PROT_WRITE,
            (MapFlags::MAP_PRIVATE | MapFlags::MAP_ANONYMOUS | MapFlags::MAP_FIXED_NOREPLACE)
                .bits(),
            usize::MAX,
            0,
        );
        return if ret < 0 { ENOMEM } else { old_addr as isize };
    }

    let target = if fixed {
        if new_addr & (PAGE_SIZE - 1) != 0
            || checked_page_range(new_addr, new_len).is_err()
            || ranges_overlap(old_addr, old_len, new_addr, new_len)
        {
            return EINVAL;
        }
        new_addr
    } else {
        0
    };

    let map_flags = if fixed {
        MapFlags::MAP_PRIVATE | MapFlags::MAP_ANONYMOUS | MapFlags::MAP_FIXED
    } else {
        MapFlags::MAP_PRIVATE | MapFlags::MAP_ANONYMOUS
    };
    let mapped = sys_mmap(
        target,
        new_len,
        PROT_READ | PROT_WRITE,
        map_flags.bits(),
        usize::MAX,
        0,
    );
    if mapped < 0 {
        return mapped;
    }
    let mapped = mapped as usize;
    let copy_len = old_size.min(new_size).min(old_len).min(new_len);
    if let Err(errno) = copy_current_user_range(old_addr, mapped, copy_len) {
        let _ = sys_munmap(mapped, new_len);
        return errno;
    }
    if !dont_unmap {
        let ret = sys_munmap(old_addr, old_len);
        if ret < 0 {
            let _ = sys_munmap(mapped, new_len);
            return ret;
        }
    }
    mapped as isize
}

pub fn sys_remap_file_pages(
    addr: usize,
    size: usize,
    prot: usize,
    _pgoff: usize,
    flags: usize,
) -> isize {
    const MAP_NONBLOCK: usize = 0x10000;

    if prot != 0 || flags & !MAP_NONBLOCK != 0 {
        return EINVAL;
    }
    let start = addr & !(PAGE_SIZE - 1);
    let len = match page_round_up_len(size) {
        Some(0) | None => return EINVAL,
        Some(len) => len,
    };
    if checked_page_range(start, len).is_err() {
        return EINVAL;
    }

    EINVAL
}

pub fn sys_mprotect(addr: usize, len: usize, prot: usize) -> isize {
    let task = current_task().unwrap();
    let prot = match parse_mmap_prot(prot) {
        Ok(prot) => prot,
        Err(errno) => return errno,
    };
    let result = task.process.vm().write(|vm| vm.mprotect(addr, len, prot));
    match result {
        Ok(_) => SUCCESS,
        Err(errno) => errno,
    }
}

pub fn sys_pkey_mprotect(addr: usize, len: usize, prot: usize, pkey: isize) -> isize {
    let effective_prot = match pkey {
        0 => prot,
        PKEY_NO_ACCESS_RIGHTS_KEY => prot,
        PKEY_ACCESS_KEY => 0,
        PKEY_WRITE_KEY => prot & !PROT_WRITE,
        _ => return EINVAL,
    };

    sys_mprotect(addr, len, effective_prot)
}

pub fn sys_pkey_alloc(flags: usize, access_rights: usize) -> isize {
    if flags != 0 {
        return EINVAL;
    }

    match access_rights {
        0 => PKEY_NO_ACCESS_RIGHTS_KEY,
        PKEY_DISABLE_ACCESS => PKEY_ACCESS_KEY,
        PKEY_DISABLE_WRITE => PKEY_WRITE_KEY,
        PKEY_DISABLE_ACCESS_WRITE => PKEY_ACCESS_KEY,
        rights if rights & PKEY_DISABLE_EXECUTE != 0 => EINVAL,
        _ => EINVAL,
    }
}

pub fn sys_pkey_free(pkey: isize) -> isize {
    match pkey {
        PKEY_NO_ACCESS_RIGHTS_KEY | PKEY_ACCESS_KEY | PKEY_WRITE_KEY => SUCCESS,
        _ => EINVAL,
    }
}

pub fn sys_mlock(addr: usize, len: usize) -> isize {
    let task = current_task().unwrap();
    let privileged = {
        let inner = task.acquire_inner_lock();
        inner.euid == 0 || (inner.cap_effective & (1u64 << CAP_IPC_LOCK)) != 0
    };
    // rlimit 是进程 owner；先快照再进入 VM 写侧，禁止把普通锁跨过 MM 操作。
    let memlock_limit = task.process.memlock_limit();
    let locked_len = match task.process.vm().write(|vm| vm.mlock(addr, len)) {
        Ok(locked_len) => locked_len,
        Err(errno) => return errno,
    };
    if !privileged {
        if memlock_limit == 0 {
            return EPERM;
        }
        if locked_len > memlock_limit {
            return ENOMEM;
        }
    }
    SUCCESS
}

pub fn sys_mlock2(addr: usize, len: usize, flags: usize) -> isize {
    const MLOCK_ONFAULT: usize = 1;
    if flags & !MLOCK_ONFAULT != 0 {
        return EINVAL;
    }
    if flags == 0 {
        return sys_mlock(addr, len);
    }

    let task = current_task().unwrap();
    let privileged = {
        let inner = task.acquire_inner_lock();
        inner.euid == 0 || (inner.cap_effective & (1u64 << CAP_IPC_LOCK)) != 0
    };
    let memlock_limit = task.process.memlock_limit();
    let locked_len = match task
        .process
        .vm()
        .write(|vm| vm.mlock_onfault(addr, len))
    {
        Ok(locked_len) => locked_len,
        Err(errno) => return errno,
    };
    if !privileged {
        if memlock_limit == 0 {
            return EPERM;
        }
        if locked_len > memlock_limit {
            return ENOMEM;
        }
    }
    SUCCESS
}

pub fn sys_munlock(addr: usize, len: usize) -> isize {
    let task = current_task().unwrap();
    match task.process.vm().write(|vm| vm.munlock(addr, len)) {
        Ok(_) => SUCCESS,
        Err(errno) => errno,
    }
}

pub fn sys_mlockall(flags: usize) -> isize {
    const MCL_CURRENT: usize = 1;
    const MCL_FUTURE: usize = 2;
    const MCL_ONFAULT: usize = 4;
    if flags == 0 || flags & !(MCL_CURRENT | MCL_FUTURE | MCL_ONFAULT) != 0 {
        return EINVAL;
    }
    let task = current_task().unwrap();
    let privileged = {
        let inner = task.acquire_inner_lock();
        inner.euid == 0 || (inner.cap_effective & (1u64 << CAP_IPC_LOCK)) != 0
    };
    let memlock_limit = task.process.memlock_limit();
    if !privileged && flags & MCL_CURRENT != 0 {
        if memlock_limit == 0 {
            return EPERM;
        }
        let mapped = task.process.vm().read(|vm| vm.user_mapped_bytes());
        if mapped > memlock_limit {
            return ENOMEM;
        }
    }
    if flags & MCL_CURRENT != 0 {
        task.process.vm().write(|vm| vm.mlockall_current());
    }
    SUCCESS
}

pub fn sys_munlockall() -> isize {
    let task = current_task().unwrap();
    task.process.vm().write(|vm| vm.munlockall());
    SUCCESS
}

pub fn sys_mincore(addr: usize, len: usize, vec: usize) -> isize {
    if addr & (PAGE_SIZE - 1) != 0 {
        return EINVAL;
    }

    let rounded_len = match page_round_up_len(len) {
        Some(len) => len,
        None => return ENOMEM,
    };
    if rounded_len == 0 {
        return SUCCESS;
    }
    if addr
        .checked_add(rounded_len)
        .map_or(true, |end| end > crate::config::USER_VA_END)
    {
        return ENOMEM;
    }

    let page_count = rounded_len / PAGE_SIZE;
    if fault_in_user_range(
        current_user_token(),
        vec as *const u8,
        page_count,
        UserAccess::Write,
    )
    .is_err()
    {
        return EFAULT;
    }

    let mut residency = Vec::new();
    if residency.try_reserve(page_count).is_err() {
        return ENOMEM;
    }
    residency.resize(page_count, 0);

    let task = current_task().unwrap();
    if let Err(errno) = task.process.vm().read(|vm| {
        vm.mincore(addr, rounded_len, residency.as_mut_slice())
    }) {
        return errno;
    }

    match copy_to_user_array(
        current_user_token(),
        residency.as_ptr(),
        vec as *mut u8,
        page_count,
    ) {
        Ok(_) => SUCCESS,
        Err(_) => EFAULT,
    }
}

pub fn sys_madvise(addr: usize, length: usize, advice: usize) -> isize {
    const MADV_NORMAL: usize = 0;
    const MADV_RANDOM: usize = 1;
    const MADV_SEQUENTIAL: usize = 2;
    const MADV_WILLNEED: usize = 3;
    const MADV_DONTNEED: usize = 4;
    const MADV_FREE: usize = 8;
    const MADV_DONTFORK: usize = 10;
    const MADV_DOFORK: usize = 11;
    const MADV_MERGEABLE: usize = 12;
    const MADV_UNMERGEABLE: usize = 13;
    const MADV_HUGEPAGE: usize = 14;
    const MADV_NOHUGEPAGE: usize = 15;
    const MADV_DONTDUMP: usize = 16;
    const MADV_DODUMP: usize = 17;
    const MADV_WIPEONFORK: usize = 18;
    const MADV_KEEPONFORK: usize = 19;
    const MADV_COLD: usize = 20;
    const MADV_PAGEOUT: usize = 21;

    if addr & (PAGE_SIZE - 1) != 0 {
        return EINVAL;
    }

    let len = match page_round_up_len(length) {
        Some(len) => len,
        None => return EINVAL,
    };
    if len == 0 {
        return SUCCESS;
    }
    if checked_page_range(addr, len).is_err() {
        return EINVAL;
    }

    match advice {
        MADV_NORMAL | MADV_RANDOM | MADV_SEQUENTIAL | MADV_WILLNEED | MADV_DONTNEED | MADV_FREE
        | MADV_DONTFORK | MADV_DOFORK | MADV_MERGEABLE | MADV_UNMERGEABLE | MADV_HUGEPAGE
        | MADV_NOHUGEPAGE | MADV_DONTDUMP | MADV_DODUMP | MADV_WIPEONFORK | MADV_KEEPONFORK
        | MADV_COLD | MADV_PAGEOUT => {
            match current_task()
                .unwrap()
                .process
                .vm()
                .write(|vm| vm.madvise(addr, len, advice))
            {
                Ok(_) => SUCCESS,
                Err(errno) => errno,
            }
        }
        _ => EINVAL,
    }
}
