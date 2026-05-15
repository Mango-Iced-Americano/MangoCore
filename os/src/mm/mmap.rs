use super::memory_set::{MemoryError, MemorySet};
use super::page_table::PageTable;
use super::user_mapper::UserMapper;
use super::vma::{MapFlags, MapPermission, MapType, Vma};
use super::VirtAddr;
use crate::config::*;
use crate::fs::SeekWhence;
use crate::syscall::errno::*;
use crate::task::current_task;
use log::{trace, warn};

const MAX_EAGER_MMAP_SIZE: usize = 1024 * 1024 * 1024;

fn page_round_up_addr(addr: usize) -> Option<usize> {
    addr.checked_add(PAGE_SIZE - 1)
        .map(|addr| addr & !(PAGE_SIZE - 1))
}

fn checked_user_range(start: usize, len: usize) -> Result<(VirtAddr, VirtAddr), isize> {
    if len == 0 {
        return Err(EINVAL);
    }
    let end = start.checked_add(len).ok_or(EINVAL)?;
    if start >= USER_VA_END || end > USER_VA_END {
        return Err(EINVAL);
    }
    Ok((VirtAddr::from(start), VirtAddr::from(end)))
}

pub(super) fn do_sbrk<T: PageTable>(
    memory_set: &mut MemorySet<T>,
    heap_pt: usize,
    heap_bottom: usize,
    increment: isize,
) -> usize {
    let old_pt = heap_pt;
    let Some(limit) = heap_bottom.checked_add(USER_HEAP_SIZE) else {
        warn!(
            "[sbrk] heap limit overflow! heap_bottom: {:X}, heap_size: {:X}",
            heap_bottom, USER_HEAP_SIZE
        );
        return old_pt;
    };
    let new_pt = if increment > 0 {
        match old_pt.checked_add(increment as usize) {
            Some(new_pt) => new_pt,
            None => {
                warn!(
                    "[sbrk] grow overflow! old_pt: {:X}, increment: {:X}",
                    old_pt, increment
                );
                return old_pt;
            }
        }
    } else if increment < 0 {
        let Some(delta) = increment.checked_neg().map(|delta| delta as usize) else {
            warn!(
                "[sbrk] shrink overflow! old_pt: {:X}, increment: {:X}",
                old_pt, increment
            );
            return old_pt;
        };
        match old_pt.checked_sub(delta) {
            Some(new_pt) => new_pt,
            None => {
                warn!(
                    "[sbrk] shrink underflow! old_pt: {:X}, decrement: {:X}",
                    old_pt, delta
                );
                return old_pt;
            }
        }
    } else {
        return old_pt;
    };

    if new_pt < heap_bottom {
        warn!(
            "[sbrk] out of the lowerbound! lowerbound: {:X}, old_pt: {:X}, new_pt: {:X}",
            heap_bottom, old_pt, new_pt
        );
        return old_pt;
    }
    if new_pt > limit {
        warn!(
            "[sbrk] out of the upperbound! upperbound: {:X}, old_pt: {:X}, new_pt: {:X}",
            limit, old_pt, new_pt
        );
        return old_pt;
    }

    let Some(old_page_end) = page_round_up_addr(old_pt) else {
        warn!("[sbrk] old break round-up overflow! old_pt: {:X}", old_pt);
        return old_pt;
    };
    let Some(new_page_end) = page_round_up_addr(new_pt) else {
        warn!("[sbrk] new break round-up overflow! new_pt: {:X}", new_pt);
        return old_pt;
    };

    if new_pt > old_pt {
        if new_page_end > old_page_end {
            let len = new_page_end - old_page_end;
            let ret = do_mmap(
                memory_set,
                old_page_end,
                len,
                MapPermission::R | MapPermission::W | MapPermission::U,
                MapFlags::MAP_ANONYMOUS | MapFlags::MAP_FIXED | MapFlags::MAP_PRIVATE,
                1usize.wrapping_neg(),
                0,
            );
            if ret < 0 {
                warn!(
                    "[sbrk] heap grow mmap failed: start={:X}, len={:X}, err={}",
                    old_page_end, len, ret
                );
                return old_pt;
            }
        }
        trace!("[sbrk] heap area expanded to {:X}", new_pt);
    } else if old_page_end > new_page_end {
        let len = old_page_end - new_page_end;
        if let Err(err) = do_munmap(memory_set, new_page_end, len) {
            warn!(
                "[sbrk] heap shrink munmap failed: start={:X}, len={:X}, err={}",
                new_page_end, len, err
            );
            return old_pt;
        }
    }

    new_pt
}

pub(super) fn do_mmap<T: PageTable>(
    memory_set: &mut MemorySet<T>,
    start: usize,
    len: usize,
    prot: MapPermission,
    flags: MapFlags,
    fd: usize,
    offset: usize,
) -> isize {
    // not aligned on a page boundary
    if start & 0xfff != 0 {
        return EINVAL;
    }
    let (start_hint, requested_end) = match checked_user_range(start, len) {
        Ok(range) => range,
        Err(errno) => return errno,
    };
    // MAP_SHARED still maps pages eagerly in this compatibility layer.
    if flags.contains(MapFlags::MAP_SHARED) && len > MAX_EAGER_MMAP_SIZE {
        return ENOMEM;
    }
    let task = current_task().unwrap();
    let fixed =
        flags.contains(MapFlags::MAP_FIXED) || flags.contains(MapFlags::MAP_FIXED_NOREPLACE);
    let start_va: VirtAddr = if fixed {
        let start_vpn = start_hint.floor();
        let end_vpn = requested_end.ceil();
        if flags.contains(MapFlags::MAP_FIXED_NOREPLACE)
            && memory_set.vmas.has_overlap(start_vpn, end_vpn)
        {
            return EEXIST;
        }
        // MAP_FIXED 允许覆盖空洞，空洞不是错误
        if let Err(errno) =
            memory_set
                .vmas
                .unmap_range(&mut memory_set.page_table, start_vpn, end_vpn, true)
        {
            return errno;
        }
        start_hint
    } else {
        match memory_set.vmas.find_free_mmap_range(len, PAGE_SIZE) {
            Ok(start_va) => {
                match memory_set
                    .vmas
                    .try_merge_lazy_private_mmap::<T>(start_va, len, prot, flags)
                {
                    Ok(Some(end_va)) => return end_va.0 as isize,
                    Ok(None) => {}
                    Err(errno) => return errno,
                }
                start_va
            }
            Err(errno) => return errno,
        }
    };
    let end = match start_va.0.checked_add(len) {
        Some(end) => end,
        None => return EINVAL,
    };
    let end_va = VirtAddr::from(end);
    let start_vpn = start_va.floor();
    let end_vpn = end_va.ceil();
    if memory_set.vmas.has_overlap(start_vpn, end_vpn) {
        return EINVAL;
    }
    if let Err(errno) = memory_set.vmas.try_reserve(1) {
        return errno;
    }
    let mut new_area = match Vma::try_new(start_va, end_va, MapType::Framed, prot, None) {
        Ok(area) => area,
        Err(e) => return e,
    };
    new_area.flags = flags;
    if !flags.contains(MapFlags::MAP_ANONYMOUS) {
        if offset & (PAGE_SIZE - 1) != 0 || offset > isize::MAX as usize {
            return EINVAL;
        }
        warn!("[mmap] file-backed map!");
        let fd_table = task.files.lock();
        match fd_table.get_ref(fd) {
            Ok(file_descriptor) => {
                if !file_descriptor.readable() {
                    return EACCES;
                }
                if flags.contains(MapFlags::MAP_SHARED)
                    && prot.contains(MapPermission::W)
                    && !file_descriptor.writable()
                {
                    return EACCES;
                }
                if !file_descriptor.file.is_file() {
                    return EINVAL;
                }
                let file = file_descriptor.file.deep_clone();
                if file.lseek(offset as isize, SeekWhence::SEEK_SET).is_err() {
                    return EINVAL;
                }
                new_area.map_file = Some(file);
            }
            Err(errno) => return errno,
        }
    }

    if flags.contains(MapFlags::MAP_SHARED) {
        let map_file = new_area.map_file.clone();
        let area_start_va = VirtAddr::from(new_area.get_start::<T>()).0;
        let vpn_range = new_area.inner.vpn_range;
        if let Some(file) = &map_file {
            let old_offset = match file.lseek(0, SeekWhence::SEEK_CUR) {
                Ok(offset) => offset,
                Err(_) => return EINVAL,
            };
            let file_size = file.get_size();
            for vpn in vpn_range {
                let page_start_va = VirtAddr::from(vpn).0;
                let offset_in_area = page_start_va - area_start_va;
                let Some(file_offset) = old_offset.checked_add(offset_in_area) else {
                    return EINVAL;
                };
                let file_page_end = file_size.saturating_add(PAGE_SIZE - 1) & !0xfff;
                if file_offset <= file_page_end {
                    if let Ok(cache) = file.get_single_cache(file_offset) {
                        let cache_phys_page = cache.lock().get_tracker();
                        let cache_ppn = cache_phys_page.ppn;
                        if let Err(err) = new_area.inner.alloc_in_memory(vpn, cache_phys_page) {
                            return match err {
                                MemoryError::OutOfMemory => ENOMEM,
                                _ => EINVAL,
                            };
                        }
                        if let Err(err) =
                            UserMapper::new(&mut memory_set.page_table).map_user_page(
                                vpn,
                                cache_ppn,
                                new_area.map_perm,
                            )
                        {
                            new_area.inner.remove_in_memory(&vpn);
                            return match err {
                                MemoryError::OutOfMemory => ENOMEM,
                                _ => EINVAL,
                            };
                        }
                    } else {
                        if let Err(err) =
                            new_area.map_one_zeroed_unchecked(&mut memory_set.page_table, vpn)
                        {
                            return match err {
                                MemoryError::OutOfMemory => ENOMEM,
                                _ => EINVAL,
                            };
                        }
                    }
                } else {
                    if let Err(err) =
                        new_area.map_one_zeroed_unchecked(&mut memory_set.page_table, vpn)
                    {
                        return match err {
                            MemoryError::OutOfMemory => ENOMEM,
                            _ => EINVAL,
                        };
                    }
                }
            }
        } else {
            for vpn in vpn_range {
                if let Err(err) =
                    new_area.map_one_zeroed_unchecked(&mut memory_set.page_table, vpn)
                {
                    return match err {
                        MemoryError::OutOfMemory => ENOMEM,
                        _ => EINVAL,
                    };
                }
            }
        }
    }

    if let Err(errno) = memory_set.vmas.insert_vma(new_area) {
        return errno;
    }

    start_va.0 as isize
}

pub(super) fn do_munmap<T: PageTable>(
    memory_set: &mut MemorySet<T>,
    start: usize,
    len: usize,
) -> Result<(), isize> {
    let (start_va, end_va) = checked_user_range(start, len)?;
    if !start_va.aligned() {
        warn!("[munmap] Not aligned");
        return Err(EINVAL);
    }
    let start_vpn = start_va.floor();
    let end_vpn = end_va.ceil();
    memory_set
        .vmas
        .unmap_range(&mut memory_set.page_table, start_vpn, end_vpn, true)
        .map(|_| ())
}

pub(super) fn do_mprotect<T: PageTable>(
    memory_set: &mut MemorySet<T>,
    addr: usize,
    len: usize,
    prot: MapPermission,
) -> Result<(), isize> {
    if len == 0 {
        return Ok(());
    }
    let (start_va, end_va) = checked_user_range(addr, len)?;
    // addr is not a multiple of the system page size.
    if !start_va.aligned() {
        warn!("[mprotect] Not aligned");
        return Err(EINVAL);
    }
    warn!(
        "[mprotect] addr: {:X}, len: {:X}, prot: {:?}",
        addr, len, prot
    );
    let start_vpn = start_va.floor();
    let end_vpn = end_va.ceil();
    memory_set
        .vmas
        .protect_range(&mut memory_set.page_table, start_vpn, end_vpn, prot)
}
