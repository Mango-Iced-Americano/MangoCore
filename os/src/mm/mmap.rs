use super::address_space::{AddressSpace, MemoryError};
use super::page_table::PageTable;
use super::vma::{MapFlags, MapPermission, Vma};
use super::{FrameTracker, VirtAddr};
use crate::config::*;
use crate::fs::vfs::IndexNode;
use crate::syscall::errno::*;
use alloc::sync::Arc;
use log::warn;

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

fn charges_overcommit(prot: MapPermission, flags: MapFlags) -> bool {
    flags.contains(MapFlags::MAP_ANONYMOUS) && prot.contains(MapPermission::W)
}

fn brk_overlap_blocks(area: &Vma) -> bool {
    let map_type = area.flags.bits() & MapFlags::MAP_TYPE.bits();
    let private_mapping = map_type == MapFlags::MAP_PRIVATE.bits();
    let writable_user = area
        .map_perm
        .contains(MapPermission::R | MapPermission::W | MapPermission::U);
    !area.vm_is_user() || area.map_file.is_some() || !private_mapping || !writable_user
}

pub(super) fn do_sbrk<T: PageTable>(
    address_space: &mut AddressSpace<T>,
    increment: isize,
) -> usize {
    let old_pt = address_space.heap_pt;
    let heap_bottom = address_space.heap_bottom;
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
            let start_vpn = VirtAddr::from(old_page_end).floor();
            let end_vpn = VirtAddr::from(new_page_end).ceil();
            if address_space
                .vmas
                .iter()
                .any(|area| area.vm_overlaps(start_vpn, end_vpn) && brk_overlap_blocks(area))
            {
                return old_pt;
            }
            if !crate::mm::overcommit_allows(address_space.committed_bytes(), len) {
                return old_pt;
            }
            let ret = do_mmap(
                address_space,
                old_page_end,
                len,
                MapPermission::R | MapPermission::W | MapPermission::U,
                MapFlags::MAP_ANONYMOUS | MapFlags::MAP_FIXED | MapFlags::MAP_PRIVATE,
                0,
                None,
                true,
                false,
            );
            if ret < 0 {
                warn!(
                    "[sbrk] heap grow mmap failed: start={:X}, len={:X}, err={}",
                    old_page_end, len, ret
                );
                return old_pt;
            }
        }
    } else if old_page_end > new_page_end {
        let len = old_page_end - new_page_end;
        if let Err(err) = do_munmap(address_space, new_page_end, len) {
            warn!(
                "[sbrk] heap shrink munmap failed: start={:X}, len={:X}, err={}",
                new_page_end, len, err
            );
            return old_pt;
        }
    }

    address_space.heap_pt = new_pt;
    new_pt
}

pub(super) fn do_mmap<T: PageTable>(
    address_space: &mut AddressSpace<T>,
    start: usize,
    len: usize,
    prot: MapPermission,
    flags: MapFlags,
    offset: usize,
    map_file: Option<Arc<dyn IndexNode>>,
    may_write: bool,
    write_sealed: bool,
) -> isize {
    // not aligned on a page boundary
    if start & 0xfff != 0 {
        return EINVAL;
    }
    let (start_hint, requested_end) = match checked_user_range(start, len) {
        Ok(range) => range,
        Err(errno) => return errno,
    };
    if charges_overcommit(prot, flags)
        && !crate::mm::overcommit_allows(address_space.committed_bytes(), len)
    {
        return ENOMEM;
    }
    // 文件映射 MAP_SHARED 改为懒加载，不再需要提前拒绝大映射
    let fixed =
        flags.contains(MapFlags::MAP_FIXED) || flags.contains(MapFlags::MAP_FIXED_NOREPLACE);
    let start_va: VirtAddr = if fixed {
        let start_vpn = start_hint.floor();
        let end_vpn = requested_end.ceil();
        if flags.contains(MapFlags::MAP_FIXED_NOREPLACE)
            && address_space.vmas.has_overlap(start_vpn, end_vpn)
        {
            return EEXIST;
        }
        // MAP_FIXED 允许覆盖空洞，空洞不是错误
        if let Err(errno) =
            address_space
                .vmas
                .unmap_range(&mut address_space.page_table, start_vpn, end_vpn, true)
        {
            return errno;
        }
        address_space.set_locked_pages(start_vpn, end_vpn, false);
        start_hint
    } else {
        let hinted_start = if start != 0 {
            let start_vpn = start_hint.floor();
            let end_vpn = requested_end.ceil();
            if address_space.vmas.is_mmap_range_free(start_vpn, end_vpn) {
                Some(start_hint)
            } else {
                None
            }
        } else {
            None
        };
        let start_va = match hinted_start {
            Some(start_va) => start_va,
            None => match address_space.vmas.find_free_mmap_range(len, PAGE_SIZE) {
                Ok(start_va) => start_va,
                Err(errno) => return errno,
            },
        };
        if !flags.contains(MapFlags::MAP_LOCKED) {
            match address_space
                .vmas
                .try_merge_lazy_private_mmap::<T>(start_va, len, prot, flags)
            {
                Ok(Some(end_va)) => return end_va.0 as isize,
                Ok(None) => {}
                Err(errno) => return errno,
            }
        }
        start_va
    };
    let end = match start_va.0.checked_add(len) {
        Some(end) => end,
        None => return EINVAL,
    };
    let end_va = VirtAddr::from(end);
    let start_vpn = start_va.floor();
    let end_vpn = end_va.ceil();
    if address_space.vmas.has_overlap(start_vpn, end_vpn) {
        return EINVAL;
    }
    if let Err(errno) = address_space.vmas.try_reserve(1) {
        return errno;
    }
    let mut new_area = match Vma::try_new(start_va, end_va, prot, None, 0) {
        Ok(area) => area,
        Err(e) => return e,
    };
    new_area.flags = flags;
    new_area.may_write = may_write;
    new_area.write_sealed = write_sealed;
    if !flags.contains(MapFlags::MAP_ANONYMOUS) {
        if offset & (PAGE_SIZE - 1) != 0 || offset > isize::MAX as usize {
            return EINVAL;
        }
        let Some(inode) = map_file else {
            return EBADF;
        };
        new_area.map_file = Some(inode);
        new_area.map_file_offset = offset;
    }

    if flags.contains(MapFlags::MAP_SHARED)
        && new_area.map_file.is_none()
        && new_area.map_perm.contains(MapPermission::W)
    {
        // Writable anonymous MAP_SHARED preallocates shared frames so fork
        // inherits the same backing pages, but installs user PTEs lazily. This
        // keeps Linux mincore/mlock2 residency semantics: untouched pages are
        // not present.
        if len > MAX_EAGER_MMAP_SIZE {
            return ENOMEM;
        }
        let vpn_range = new_area.inner.vpn_range;
        for vpn in vpn_range {
            if let Err(err) = new_area.alloc_one_zeroed_unmapped(vpn) {
                return match err {
                    MemoryError::OutOfMemory => ENOMEM,
                    _ => EINVAL,
                };
            }
        }
    }

    if let Err(errno) = address_space.vmas.insert_vma(new_area) {
        return errno;
    }
    if flags.contains(MapFlags::MAP_LOCKED) {
        address_space.set_locked_pages(start_vpn, end_vpn, true);
    }

    start_va.0 as isize
}

pub(super) fn do_shm_mmap<T: PageTable>(
    address_space: &mut AddressSpace<T>,
    start: usize,
    len: usize,
    prot: MapPermission,
    flags: MapFlags,
    frames: &[Arc<FrameTracker>],
    may_write: bool,
) -> isize {
    if start & (PAGE_SIZE - 1) != 0 {
        return EINVAL;
    }
    let (start_hint, requested_end) = match checked_user_range(start, len) {
        Ok(range) => range,
        Err(errno) => return errno,
    };
    let fixed =
        flags.contains(MapFlags::MAP_FIXED) || flags.contains(MapFlags::MAP_FIXED_NOREPLACE);
    let start_va = if fixed {
        let start_vpn = start_hint.floor();
        let end_vpn = requested_end.ceil();
        if flags.contains(MapFlags::MAP_FIXED_NOREPLACE)
            && address_space.vmas.has_overlap(start_vpn, end_vpn)
        {
            return EEXIST;
        }
        if let Err(errno) =
            address_space
                .vmas
                .unmap_range(&mut address_space.page_table, start_vpn, end_vpn, true)
        {
            return errno;
        }
        address_space.set_locked_pages(start_vpn, end_vpn, false);
        start_hint
    } else {
        match address_space.vmas.find_free_mmap_range(len, PAGE_SIZE) {
            Ok(start_va) => start_va,
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
    if frames.len() != end_vpn.0.saturating_sub(start_vpn.0) {
        return EINVAL;
    }
    if address_space.vmas.has_overlap(start_vpn, end_vpn) {
        return EINVAL;
    }
    if let Err(errno) = address_space.vmas.try_reserve(1) {
        return errno;
    }
    let mut new_area = match Vma::try_new(start_va, end_va, prot, None, 0) {
        Ok(area) => area,
        Err(e) => return e,
    };
    new_area.flags = flags;
    new_area.may_write = may_write;
    let mut frame_index = 0;
    for vpn in new_area.inner.vpn_range {
        if new_area
            .inner
            .alloc_in_memory(vpn, frames[frame_index].clone())
            .is_err()
        {
            return EINVAL;
        }
        frame_index += 1;
    }
    if address_space.push_no_alloc(new_area).is_err() {
        return ENOMEM;
    }
    start_va.0 as isize
}

pub(super) fn do_munmap<T: PageTable>(
    address_space: &mut AddressSpace<T>,
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
    address_space
        .vmas
        .unmap_range(&mut address_space.page_table, start_vpn, end_vpn, true)
        .map(|_| ())
}

pub(super) fn do_mprotect<T: PageTable>(
    address_space: &mut AddressSpace<T>,
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
    address_space
        .vmas
        .protect_range(&mut address_space.page_table, start_vpn, end_vpn, prot)
}
