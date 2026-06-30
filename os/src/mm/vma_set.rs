//! 用户 VMA 集合和 mmap 空洞管理。
//!
//! `VmaSet` 用起始 VPN 索引所有 VMA，并维护 mmap arena 的空洞表、用户 VMA 计数和
//! 用户映射页数缓存。它负责拆分、合并、覆盖、保护、建议和 grow-down 栈扩展。
//!
//! # Invariants
//!
//! `vmas` 中的区间必须非空且互不重叠；`mmap_holes` 必须位于 mmap arena 内并保持合并；
//! `user_area_count`/`user_page_count` 必须只统计用户 VMA。

use super::user_mapper::UserMapper;
use super::vma::{MapFlags, MapPermission, Vma};
use super::{MemoryError, PageTable, VirtAddr, VirtPageNum};
use crate::config::*;
use crate::fs::vfs::IndexNode;
use crate::syscall::errno::{EACCES, EINVAL, ENOMEM, EPERM};
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use log::warn;

const STACK_GUARD_GAP_PAGES: usize = 256;
const GROWSDOWN_MAX_FAULT_GAP_PAGES: usize = USER_STACK_SIZE / PAGE_SIZE;

pub(super) struct VmaSet {
    vmas: BTreeMap<VirtPageNum, Vma>,
    mmap_holes: BTreeMap<VirtPageNum, VirtPageNum>,
    user_area_count: usize,
    user_page_count: usize,
}

#[cfg(feature = "loongarch64")]
fn mmap_bounds() -> (VirtPageNum, VirtPageNum) {
    (
        VirtAddr::from(USR_MMAP_BASE).floor(),
        VirtAddr::from(USR_MMAP_END).floor(),
    )
}

#[cfg(feature = "riscv")]
fn mmap_bounds() -> (VirtPageNum, VirtPageNum) {
    (
        VirtAddr::from(MMAP_BASE).floor(),
        VirtAddr::from(MMAP_END).floor(),
    )
}

fn align_up(value: usize, align: usize) -> Option<usize> {
    if align <= 1 {
        return Some(value);
    }
    value
        .checked_add(align - 1)
        .map(|value| value & !(align - 1))
}

fn file_backed_page_resident(area: &Vma, vpn: VirtPageNum) -> bool {
    let Some(inode) = area.vm_file() else {
        return false;
    };
    let Ok(file_offset) = area.vm_file_offset(vpn) else {
        return false;
    };
    inode
        .page_cache()
        .map(|pc| pc.contains_page(file_offset >> PAGE_SIZE_BITS))
        .unwrap_or(false)
}

fn is_anonymous_private(area: &Vma) -> bool {
    area.map_file.is_none()
        && area
            .flags
            .contains(MapFlags::MAP_PRIVATE | MapFlags::MAP_ANONYMOUS)
        && !area.flags.contains(MapFlags::MAP_SHARED)
}

fn area_page_count(area: &Vma) -> usize {
    area.vm_end().0.saturating_sub(area.vm_start().0)
}

fn errno_to_memory_error(errno: isize) -> MemoryError {
    match errno {
        ENOMEM => MemoryError::OutOfMemory,
        EACCES => MemoryError::NoPermission,
        _ => MemoryError::BadAddress,
    }
}

impl VmaSet {
    pub(super) fn new() -> Self {
        Self::with_capacity(0)
    }

    pub(super) fn with_capacity(_capacity: usize) -> Self {
        let (mmap_start, mmap_end) = mmap_bounds();
        let mut mmap_holes = BTreeMap::new();
        mmap_holes.insert(mmap_start, mmap_end);
        let set = Self {
            vmas: BTreeMap::new(),
            mmap_holes,
            user_area_count: 0,
            user_page_count: 0,
        };
        set.debug_assert_invariants();
        set
    }

    pub(super) fn len(&self) -> usize {
        self.vmas.len()
    }

    fn accounted_len(&self) -> usize {
        self.user_area_count
    }

    pub(super) fn has_shared_writable_mapping(&self, inode: &Arc<dyn IndexNode>) -> bool {
        self.vmas.values().any(|area| {
            if !area.flags.contains(MapFlags::MAP_SHARED)
                || !area.map_perm.contains(MapPermission::W)
            {
                return false;
            }
            match &area.map_file {
                Some(mapped_inode) => Arc::ptr_eq(mapped_inode, inode),
                None => false,
            }
        })
    }

    pub(super) fn is_empty(&self) -> bool {
        self.vmas.is_empty()
    }

    pub(super) fn iter(&self) -> alloc::collections::btree_map::Values<'_, VirtPageNum, Vma> {
        self.vmas.values()
    }

    pub(super) fn iter_mut(
        &mut self,
    ) -> alloc::collections::btree_map::ValuesMut<'_, VirtPageNum, Vma> {
        self.vmas.values_mut()
    }

    pub(super) fn get_by_start(&self, start_vpn: VirtPageNum) -> Option<&Vma> {
        self.vmas.get(&start_vpn)
    }

    pub(super) fn get_mut_by_start(&mut self, start_vpn: VirtPageNum) -> Option<&mut Vma> {
        self.vmas.get_mut(&start_vpn)
    }

    pub(super) fn last_non_user(&self) -> Option<&Vma> {
        self.vmas
            .values()
            .rev()
            .find(|area| !area.vm_is_user())
    }

    pub(super) fn clear(&mut self) {
        self.vmas.clear();
        self.mmap_holes.clear();
        self.user_area_count = 0;
        self.user_page_count = 0;
        let (mmap_start, mmap_end) = mmap_bounds();
        self.mmap_holes.insert(mmap_start, mmap_end);
        self.debug_assert_invariants();
    }

    pub(super) fn clear_no_hole(&mut self) {
        self.vmas.clear();
        self.mmap_holes.clear();
        self.user_area_count = 0;
        self.user_page_count = 0;
    }

    pub(super) fn try_reserve(&mut self, additional: usize) -> Result<(), isize> {
        self.ensure_can_add(additional)
    }

    pub(super) fn push(&mut self, area: Vma) -> Result<(), isize> {
        self.insert_vma(area)
    }

    pub(super) fn find_vma_key(&self, vpn: VirtPageNum) -> Option<VirtPageNum> {
        self.vmas
            .range(..=vpn)
            .next_back()
            .and_then(|(start, area)| {
                if area.vm_contains(vpn) {
                    Some(*start)
                } else {
                    None
                }
            })
    }

    pub(super) fn find_user_vma_key(&self, vpn: VirtPageNum) -> Option<VirtPageNum> {
        self.find_vma_key(vpn).and_then(|start| {
            self.vmas
                .get(&start)
                .filter(|area| area.vm_is_user())
                .map(|_| start)
        })
    }

    pub(super) fn find_user_vma_mut(&mut self, vpn: VirtPageNum) -> Option<&mut Vma> {
        let start = self.find_user_vma_key(vpn)?;
        self.vmas.get_mut(&start)
    }

    pub(super) fn expand_growsdown_for_fault(
        &mut self,
        fault_vpn: VirtPageNum,
    ) -> Result<Option<VirtPageNum>, MemoryError> {
        let Some((old_start, _)) = self.vmas.range(fault_vpn..).next() else {
            return Ok(None);
        };
        let old_start = *old_start;
        let Some(area) = self.vmas.get(&old_start) else {
            return Ok(None);
        };
        if fault_vpn >= old_start
            || !area.vm_is_user()
            || !area.flags.contains(MapFlags::MAP_GROWSDOWN)
        {
            return Ok(None);
        }

        let fault_gap_pages = old_start.0 - fault_vpn.0;
        if fault_gap_pages > GROWSDOWN_MAX_FAULT_GAP_PAGES {
            warn!(
                "[MAP_GROWSDOWN] reject distant fault: fault={:?}, start={:?}",
                fault_vpn, old_start
            );
            return Ok(None);
        }

        if let Some((_, prev)) = self.vmas.range(..old_start).next_back() {
            let prev_end = prev.vm_end();
            if prev_end > fault_vpn {
                return Ok(None);
            }
            let guard_gap_pages = fault_vpn.0.saturating_sub(prev_end.0);
            if guard_gap_pages < STACK_GUARD_GAP_PAGES {
                warn!(
                    "[MAP_GROWSDOWN] reject guard gap: fault={:?}, prev_end={:?}, gap_pages={}",
                    fault_vpn, prev_end, guard_gap_pages
                );
                return Ok(None);
            }
        }

        if self.has_overlap(fault_vpn, old_start) {
            return Ok(None);
        }
        self.reserve_mmap_range(fault_vpn, old_start)
            .map_err(errno_to_memory_error)?;
        let mut area = self.vmas.remove(&old_start).ok_or(MemoryError::BadAddress)?;
        let old_pages = area_page_count(&area);
        let is_user = area.vm_is_user();
        if let Err(errno) = area.expand_down_to(VirtAddr::from(fault_vpn)) {
            self.vmas.insert(old_start, area);
            let _ = self.release_mmap_range(fault_vpn, old_start);
            return Err(errno_to_memory_error(errno));
        }
        let new_start = area.vm_start();
        if is_user {
            self.user_page_count = self
                .user_page_count
                .saturating_add(area_page_count(&area).saturating_sub(old_pages));
        }
        self.vmas.insert(new_start, area);
        self.debug_assert_invariants();
        Ok(Some(new_start))
    }

    pub(super) fn has_overlap(
        &self,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
    ) -> bool {
        if start_vpn >= end_vpn {
            return false;
        }
        if let Some((_, area)) = self.vmas.range(..=start_vpn).next_back() {
            if area.vm_end() > start_vpn {
                return true;
            }
        }
        self.vmas
            .range(start_vpn..)
            .next()
            .map_or(false, |(_, area)| area.vm_start() < end_vpn)
    }

    pub(super) fn insert_vma(&mut self, new_area: Vma) -> Result<(), isize> {
        let start = new_area.vm_start();
        let end = new_area.vm_end();
        if start >= end {
            return Err(EINVAL);
        }
        if self.has_overlap(start, end) {
            return Err(EINVAL);
        }
        self.try_reserve(1)?;
        self.reserve_mmap_range(start, end)?;
        let is_user = new_area.vm_is_user();
        let pages = area_page_count(&new_area);
        self.vmas.insert(start, new_area);
        if is_user {
            self.user_area_count += 1;
            self.user_page_count += pages;
        }
        self.debug_assert_invariants();
        Ok(())
    }

    pub(super) fn remove_area_with_start<T: PageTable>(
        &mut self,
        page_table: &mut T,
        start_vpn: VirtPageNum,
    ) -> Result<(), MemoryError> {
        if let Some(mut area) = self.vmas.remove(&start_vpn) {
            let start = area.vm_start();
            let end = area.vm_end();
            self.untrack_area(&area);
            let result = area.unmap(page_table);
            let _ = self.release_mmap_range(start, end);
            self.debug_assert_invariants();
            result
        } else {
            Err(MemoryError::AreaNotFound)
        }
    }

    pub(super) fn split_for_range(
        &mut self,
        area_start: VirtPageNum,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
    ) -> Result<VirtPageNum, isize> {
        let area = self.vmas.get(&area_start).ok_or(EINVAL)?;
        let split_is_user = area.vm_is_user();
        let area_start_vpn = area.vm_start();
        let area_end_vpn = area.vm_end();
        if start_vpn < area_start_vpn || end_vpn > area_end_vpn || start_vpn >= end_vpn {
            return Err(EINVAL);
        }
        let additional = if start_vpn == area_start_vpn && end_vpn == area_end_vpn {
            0
        } else if start_vpn == area_start_vpn || end_vpn == area_end_vpn {
            1
        } else {
            2
        };
        self.try_reserve(additional)?;
        let mut area = self.vmas.remove(&area_start).ok_or(EINVAL)?;
        if start_vpn == area_start_vpn && end_vpn == area_end_vpn {
            self.vmas.insert(area_start_vpn, area);
            self.debug_assert_invariants();
            Ok(area_start_vpn)
        } else if start_vpn == area_start_vpn {
            let second = match area.into_two(end_vpn) {
                Ok(second) => second,
                Err(_) => {
                    self.vmas.insert(area_start_vpn, area);
                    return Err(EINVAL);
                }
            };
            let target_start = area.vm_start();
            self.insert_split_piece(area);
            self.insert_split_piece(second);
            if split_is_user {
                self.user_area_count += 1;
            }
            self.debug_assert_invariants();
            Ok(target_start)
        } else if end_vpn == area_end_vpn {
            let second = match area.into_two(start_vpn) {
                Ok(second) => second,
                Err(_) => {
                    self.vmas.insert(area_start_vpn, area);
                    return Err(EINVAL);
                }
            };
            let target_start = second.vm_start();
            self.insert_split_piece(area);
            self.insert_split_piece(second);
            if split_is_user {
                self.user_area_count += 1;
            }
            self.debug_assert_invariants();
            Ok(target_start)
        } else {
            let (second, third) = match area.into_three(start_vpn, end_vpn) {
                Ok(parts) => parts,
                Err(_) => {
                    self.vmas.insert(area_start_vpn, area);
                    return Err(EINVAL);
                }
            };
            let target_start = second.vm_start();
            self.insert_split_piece(area);
            self.insert_split_piece(second);
            self.insert_split_piece(third);
            if split_is_user {
                self.user_area_count += 2;
            }
            self.debug_assert_invariants();
            Ok(target_start)
        }
    }

    pub(super) fn unmap_range<T: PageTable>(
        &mut self,
        page_table: &mut T,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
        allow_empty: bool,
    ) -> Result<bool, isize> {
        let mut found_area = false;
        while let Some(area_start) = self.first_overlap_key(start_vpn, end_vpn) {
            let area = self.vmas.get(&area_start).ok_or(EINVAL)?;
            if !area.vm_is_user() {
                return Err(EINVAL);
            }
            found_area = true;
            let area_start_vpn = area.vm_start();
            let area_end_vpn = area.vm_end();
            let overlap_start = if start_vpn > area_start_vpn {
                start_vpn
            } else {
                area_start_vpn
            };
            let overlap_end = if end_vpn < area_end_vpn {
                end_vpn
            } else {
                area_end_vpn
            };
            let target_start = self.split_for_range(area_start, overlap_start, overlap_end)?;
            let mut target = self.vmas.remove(&target_start).ok_or(EINVAL)?;
            let released_start = target.vm_start();
            let released_end = target.vm_end();
            self.untrack_area(&target);
            if target.unmap(page_table).is_err() {
                warn!("[munmap] Some pages are already unmapped, is it caused by lazy alloc?");
            }
            self.release_mmap_range(released_start, released_end)?;
        }
        self.debug_assert_invariants();
        if found_area || allow_empty {
            Ok(found_area)
        } else {
            Err(EINVAL)
        }
    }

    pub(super) fn advise_range<T: PageTable>(
        &mut self,
        page_table: &mut T,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
        advice: usize,
    ) -> Result<(), isize> {
        const MADV_DONTNEED: usize = 4;
        const MADV_FREE: usize = 8;
        const MADV_DONTFORK: usize = 10;
        const MADV_DOFORK: usize = 11;
        const MADV_MERGEABLE: usize = 12;
        const MADV_UNMERGEABLE: usize = 13;
        const MADV_WIPEONFORK: usize = 18;
        const MADV_KEEPONFORK: usize = 19;

        let mut cursor = start_vpn;
        while cursor < end_vpn {
            let area_start = self.find_user_vma_key(cursor).ok_or(ENOMEM)?;
            let area_end = self.vmas.get(&area_start).ok_or(ENOMEM)?.vm_end();
            let advise_end = if area_end < end_vpn {
                area_end
            } else {
                end_vpn
            };

            if advice == MADV_DONTNEED {
                let area = self.vmas.get_mut(&area_start).ok_or(ENOMEM)?;
                if area.map_file.is_none() && area.flags.contains(MapFlags::MAP_PRIVATE) {
                    area.discard_range(page_table, cursor, advise_end)
                        .map_err(|_| EINVAL)?;
                }
            }
            if advice == MADV_FREE {
                let area = self.vmas.get(&area_start).ok_or(ENOMEM)?;
                if !is_anonymous_private(area) {
                    return Err(EINVAL);
                }
            }
            if advice == MADV_MERGEABLE || advice == MADV_UNMERGEABLE {
                let area = self.vmas.get(&area_start).ok_or(ENOMEM)?;
                if !area.map_perm.contains(MapPermission::W) {
                    return Err(EINVAL);
                }
            }
            if advice == MADV_DONTFORK || advice == MADV_DOFORK {
                let target_start = self.split_for_range(area_start, cursor, advise_end)?;
                let area = self.vmas.get_mut(&target_start).ok_or(ENOMEM)?;
                area.dont_fork = advice == MADV_DONTFORK;
            }
            if advice == MADV_WIPEONFORK || advice == MADV_KEEPONFORK {
                if advice == MADV_WIPEONFORK {
                    let area = self.vmas.get(&area_start).ok_or(ENOMEM)?;
                    if !is_anonymous_private(area) {
                        return Err(EINVAL);
                    }
                }
                let target_start = self.split_for_range(area_start, cursor, advise_end)?;
                let area = self.vmas.get_mut(&target_start).ok_or(ENOMEM)?;
                area.wipe_on_fork = advice == MADV_WIPEONFORK;
            }

            cursor = advise_end;
        }
        Ok(())
    }

    pub(super) fn mincore_range<T: PageTable>(
        &self,
        page_table: &T,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
        residency: &mut [u8],
    ) -> Result<(), isize> {
        let mut cursor = start_vpn;
        let mut index = 0usize;
        while cursor < end_vpn {
            let area_start = self.find_user_vma_key(cursor).ok_or(ENOMEM)?;
            let area = self.vmas.get(&area_start).ok_or(ENOMEM)?;
            let area_end = area.vm_end();
            let scan_end = if area_end < end_vpn {
                area_end
            } else {
                end_vpn
            };

            while cursor < scan_end {
                if let Some(slot) = residency.get_mut(index) {
                    *slot = if page_table.is_mapped(cursor)
                        || file_backed_page_resident(area, cursor)
                    {
                        1
                    } else {
                        0
                    };
                }
                index += 1;
                cursor.0 += 1;
            }
        }
        Ok(())
    }

    pub(super) fn covers_user_range(
        &self,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
    ) -> bool {
        let mut cursor = start_vpn;
        while cursor < end_vpn {
            let Some(area_start) = self.find_user_vma_key(cursor) else {
                return false;
            };
            let Some(area) = self.vmas.get(&area_start) else {
                return false;
            };
            if area.vm_end() <= cursor {
                return false;
            }
            cursor = if area.vm_end() < end_vpn {
                area.vm_end()
            } else {
                end_vpn
            };
        }
        true
    }

    pub(super) fn user_mapped_bytes(&self) -> usize {
        self.user_page_count.saturating_mul(PAGE_SIZE)
    }

    pub(super) fn protect_range<T: PageTable>(
        &mut self,
        page_table: &mut T,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
        prot: MapPermission,
    ) -> Result<(), isize> {
        let mut cursor = start_vpn;
        while cursor < end_vpn {
            let Some(area_start) = self.find_user_vma_key(cursor) else {
                warn!("[mprotect] addr: {:?} is not in any user Vma", cursor);
                return Err(ENOMEM);
            };
            let area = self.vmas.get(&area_start).ok_or(ENOMEM)?;
            if prot.contains(MapPermission::W)
                && area.flags.contains(MapFlags::MAP_SHARED)
            {
                if area.write_sealed {
                    return Err(EPERM);
                }
                if !area.may_write {
                    return Err(EACCES);
                }
            }
            cursor = if area.vm_end() < end_vpn {
                area.vm_end()
            } else {
                end_vpn
            };
        }

        let mut cursor = start_vpn;
        while cursor < end_vpn {
            let Some(area_start) = self.find_user_vma_key(cursor) else {
                warn!("[mprotect] addr: {:?} is not in any user Vma", cursor);
                return Err(ENOMEM);
            };
            let area_end = self.vmas.get(&area_start).ok_or(ENOMEM)?.vm_end();
            let protect_end = if area_end < end_vpn {
                area_end
            } else {
                end_vpn
            };
            let target_start = self.split_for_range(area_start, cursor, protect_end)?;
            self.protect_area(page_table, target_start, prot)?;
            cursor = protect_end;
        }
        self.debug_assert_invariants();
        Ok(())
    }

    pub(super) fn find_free_mmap_range(
        &self,
        len: usize,
        align: usize,
    ) -> Result<VirtAddr, isize> {
        let align = align.max(PAGE_SIZE);
        for (hole_start, hole_end) in self.mmap_holes.iter() {
            let hole_start_addr = VirtAddr::from(*hole_start).0;
            let hole_end_addr = VirtAddr::from(*hole_end).0;
            let Some(start_addr) = align_up(hole_start_addr, align) else {
                return Err(EINVAL);
            };
            let Some(end_addr) = start_addr.checked_add(len) else {
                return Err(EINVAL);
            };
            if end_addr <= hole_end_addr {
                return Ok(VirtAddr::from(start_addr));
            }
        }
        Err(ENOMEM)
    }

    pub(super) fn reserve_mmap_range(
        &mut self,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
    ) -> Result<(), isize> {
        let Some((start_vpn, end_vpn)) = self.clip_mmap_range(start_vpn, end_vpn) else {
            self.debug_assert_invariants();
            return Ok(());
        };
        let overlapping: Vec<(VirtPageNum, VirtPageNum)> = self
            .mmap_holes
            .range(..end_vpn)
            .filter(|(_, hole_end)| **hole_end > start_vpn)
            .map(|(hole_start, hole_end)| (*hole_start, *hole_end))
            .collect();
        for (hole_start, hole_end) in overlapping {
            self.mmap_holes.remove(&hole_start);
            if hole_start < start_vpn {
                self.mmap_holes.insert(hole_start, start_vpn);
            }
            if end_vpn < hole_end {
                self.mmap_holes.insert(end_vpn, hole_end);
            }
        }
        self.debug_assert_invariants();
        Ok(())
    }

    pub(super) fn release_mmap_range(
        &mut self,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
    ) -> Result<(), isize> {
        let Some((mut start_vpn, mut end_vpn)) = self.clip_mmap_range(start_vpn, end_vpn) else {
            self.debug_assert_invariants();
            return Ok(());
        };
        if let Some((prev_start, prev_end)) = self
            .mmap_holes
            .range(..=start_vpn)
            .next_back()
            .map(|(start, end)| (*start, *end))
        {
            if prev_end >= start_vpn {
                start_vpn = prev_start;
                if prev_end > end_vpn {
                    end_vpn = prev_end;
                }
                self.mmap_holes.remove(&prev_start);
            }
        }
        loop {
            let next = self
                .mmap_holes
                .range(start_vpn..)
                .next()
                .map(|(start, end)| (*start, *end));
            let Some((next_start, next_end)) = next else {
                break;
            };
            if next_start > end_vpn {
                break;
            }
            if next_end > end_vpn {
                end_vpn = next_end;
            }
            self.mmap_holes.remove(&next_start);
        }
        self.mmap_holes.insert(start_vpn, end_vpn);
        self.debug_assert_invariants();
        Ok(())
    }

    pub(super) fn try_merge_lazy_private_mmap<T: PageTable>(
        &mut self,
        start_va: VirtAddr,
        len: usize,
        prot: MapPermission,
        flags: MapFlags,
    ) -> Result<Option<VirtAddr>, isize> {
        let start_vpn = start_va.floor();
        let Some(new_end) = start_va.0.checked_add(len) else {
            return Err(EINVAL);
        };
        let end_vpn = VirtAddr::from(new_end).ceil();
        if !self.is_mmap_range_free(start_vpn, end_vpn) {
            return Ok(None);
        }
        let Some((key, area)) = self.vmas.range(..=start_vpn).next_back() else {
            return Ok(None);
        };
        if area.vm_end() != start_vpn || !area.vm_can_merge_lazy_private(prot, flags) {
            return Ok(None);
        }
        let key = *key;
        let old_pages = area_page_count(area);
        let is_user = area.vm_is_user();
        self.reserve_mmap_range(start_vpn, end_vpn)?;
        let expand_result = {
            let area = self.vmas.get_mut(&key).ok_or(EINVAL)?;
            area.expand_to::<T>(VirtAddr::from(new_end))
        };
        if let Err(errno) = expand_result {
            let _ = self.release_mmap_range(start_vpn, end_vpn);
            return Err(errno);
        }
        if is_user {
            let area = self.vmas.get(&key).ok_or(EINVAL)?;
            self.user_page_count = self
                .user_page_count
                .saturating_add(area_page_count(area).saturating_sub(old_pages));
        }
        self.debug_assert_invariants();
        Ok(Some(start_va))
    }

    fn protect_area<T: PageTable>(
        &mut self,
        page_table: &mut T,
        area_start: VirtPageNum,
        prot: MapPermission,
    ) -> Result<(), isize> {
        let area = self.vmas.get_mut(&area_start).ok_or(EINVAL)?;
        let actual_prot = if area.flags.contains(MapFlags::MAP_SHARED) {
            prot
        } else {
            prot - MapPermission::W
        };
        let mut failed_mapped_page = false;
        area.inner.for_each_in_memory_vpn(|vpn| {
            if UserMapper::new(page_table)
                .set_user_flags(vpn, actual_prot)
                .is_err()
            {
                failed_mapped_page = true;
            }
        });
        if failed_mapped_page {
            warn!("[mprotect] failed to update flags for some resident pages");
        }
        area.map_perm = prot;
        Ok(())
    }

    fn ensure_can_add(&self, additional: usize) -> Result<(), isize> {
        if self
            .accounted_len()
            .checked_add(additional)
            // Linux rejects at the failure point after the visible map count
            // can exceed max_map_count by one; internal non-user VMAs should
            // not reduce the user-visible limit.
            .map_or(true, |len| len > crate::mm::max_map_count().saturating_add(1))
        {
            Err(ENOMEM)
        } else {
            Ok(())
        }
    }

    fn insert_split_piece(&mut self, area: Vma) {
        self.vmas.insert(area.vm_start(), area);
    }

    fn untrack_area(&mut self, area: &Vma) {
        if area.vm_is_user() {
            self.user_area_count = self.user_area_count.saturating_sub(1);
            self.user_page_count = self.user_page_count.saturating_sub(area_page_count(area));
        }
    }

    fn first_overlap_key(
        &self,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
    ) -> Option<VirtPageNum> {
        if let Some((key, area)) = self.vmas.range(..=start_vpn).next_back() {
            if area.vm_overlaps(start_vpn, end_vpn) {
                return Some(*key);
            }
        }
        self.vmas
            .range(start_vpn..)
            .find(|(_, area)| area.vm_overlaps(start_vpn, end_vpn))
            .map(|(key, _)| *key)
    }

    fn clip_mmap_range(
        &self,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
    ) -> Option<(VirtPageNum, VirtPageNum)> {
        let (mmap_start, mmap_end) = mmap_bounds();
        let start_vpn = if start_vpn > mmap_start {
            start_vpn
        } else {
            mmap_start
        };
        let end_vpn = if end_vpn < mmap_end {
            end_vpn
        } else {
            mmap_end
        };
        if start_vpn < end_vpn {
            Some((start_vpn, end_vpn))
        } else {
            None
        }
    }

    pub(super) fn is_mmap_range_free(&self, start_vpn: VirtPageNum, end_vpn: VirtPageNum) -> bool {
        let Some((clipped_start, clipped_end)) = self.clip_mmap_range(start_vpn, end_vpn) else {
            return false;
        };
        if clipped_start != start_vpn || clipped_end != end_vpn {
            return false;
        }
        let mut cursor = start_vpn;
        while cursor < end_vpn {
            let Some((_, hole_end)) = self.mmap_holes.range(..=cursor).next_back() else {
                return false;
            };
            if *hole_end <= cursor {
                return false;
            }
            cursor = if *hole_end < end_vpn {
                *hole_end
            } else {
                end_vpn
            };
        }
        true
    }

    #[inline(always)]
    fn debug_assert_invariants(&self) {
        #[cfg(debug_assertions)]
        self.check_invariants();
    }

    fn check_invariants(&self) {
        let mut prev_vma_end = None;
        for (key, area) in self.vmas.iter() {
            let start = area.vm_start();
            let end = area.vm_end();
            debug_assert_eq!(*key, start, "VmaSet key must match VMA start");
            debug_assert!(start < end, "VMA must be non-empty: {:?}", area);
            if let Some(prev_end) = prev_vma_end {
                debug_assert!(
                    prev_end <= start,
                    "VMA ranges must not overlap: prev_end={:?}, start={:?}",
                    prev_end,
                    start
                );
            }
            prev_vma_end = Some(end);
        }

        let (mmap_start, mmap_end) = mmap_bounds();
        let mut prev_hole_end = None;
        for (hole_start, hole_end) in self.mmap_holes.iter() {
            debug_assert!(
                *hole_start < *hole_end,
                "mmap hole must be non-empty: {:?}..{:?}",
                hole_start,
                hole_end
            );
            debug_assert!(
                *hole_start >= mmap_start && *hole_end <= mmap_end,
                "mmap hole out of arena: {:?}..{:?}",
                hole_start,
                hole_end
            );
            if let Some(prev_end) = prev_hole_end {
                debug_assert!(
                    prev_end < *hole_start,
                    "mmap holes must be disjoint and merged: prev_end={:?}, start={:?}",
                    prev_end,
                    hole_start
                );
            }
            for area in self.vmas.values() {
                debug_assert!(
                    !area.vm_overlaps(*hole_start, *hole_end),
                    "mmap hole overlaps VMA: hole={:?}..{:?}, area={:?}",
                    hole_start,
                    hole_end,
                    area
                );
            }
            prev_hole_end = Some(*hole_end);
        }

        let user_area_count = self.vmas.values().filter(|area| area.vm_is_user()).count();
        let user_page_count = self
            .vmas
            .values()
            .filter(|area| area.vm_is_user())
            .map(area_page_count)
            .fold(0usize, |acc, pages| acc.saturating_add(pages));
        debug_assert_eq!(
            self.user_area_count, user_area_count,
            "cached user VMA count drifted"
        );
        debug_assert_eq!(
            self.user_page_count, user_page_count,
            "cached user mapped pages drifted"
        );
    }
}
