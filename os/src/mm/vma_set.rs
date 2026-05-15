#![allow(dead_code)]

use super::user_mapper::UserMapper;
use super::vma::{MapFlags, MapPermission, Vma};
use super::{MemoryError, PageTable, VirtAddr, VirtPageNum};
use crate::syscall::errno::{EINVAL, ENOMEM};
use alloc::vec::Vec;
use core::ops::{Index, IndexMut};
use log::{debug, warn};

const MAX_VMA_COUNT: usize = 65_536;

pub(super) struct VmaSet {
    vmas: Vec<Vma>,
}

impl VmaSet {
    pub(super) fn new() -> Self {
        Self { vmas: Vec::new() }
    }

    pub(super) fn with_capacity(capacity: usize) -> Self {
        Self {
            vmas: Vec::with_capacity(capacity),
        }
    }

    pub(super) fn len(&self) -> usize {
        self.vmas.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.vmas.is_empty()
    }

    pub(super) fn iter(&self) -> core::slice::Iter<'_, Vma> {
        self.vmas.iter()
    }

    pub(super) fn iter_mut(&mut self) -> core::slice::IterMut<'_, Vma> {
        self.vmas.iter_mut()
    }

    pub(super) fn get(&self, idx: usize) -> Option<&Vma> {
        self.vmas.get(idx)
    }

    pub(super) fn get_mut(&mut self, idx: usize) -> Option<&mut Vma> {
        self.vmas.get_mut(idx)
    }

    pub(super) fn last(&self) -> Option<&Vma> {
        self.vmas.last()
    }

    pub(super) fn clear(&mut self) {
        self.vmas.clear();
    }

    pub(super) fn try_reserve(&mut self, additional: usize) -> Result<(), isize> {
        if self
            .len()
            .checked_add(additional)
            .map_or(true, |len| len > MAX_VMA_COUNT)
        {
            return Err(ENOMEM);
        }
        self.vmas.try_reserve(additional).map_err(|_| ENOMEM)
    }

    pub(super) fn push(&mut self, area: Vma) -> Result<(), isize> {
        // Keep legacy insertion order for special areas such as trap context.
        // User mmap insertions go through insert_ordered().
        self.try_reserve(1)?;
        self.vmas.push(area);
        Ok(())
    }

    pub(super) fn remove(&mut self, idx: usize) -> Vma {
        self.vmas.remove(idx)
    }

    pub(super) fn find_index(&self, vpn: VirtPageNum) -> Option<usize> {
        self.vmas.iter().position(|area| area.vm_contains(vpn))
    }

    pub(super) fn find_user_index(&self, vpn: VirtPageNum) -> Option<usize> {
        self.vmas
            .iter()
            .position(|area| area.vm_is_user() && area.vm_contains(vpn))
    }

    pub(super) fn last_mmap_index(
        &self,
        mmap_base: VirtPageNum,
        mmap_end: VirtPageNum,
    ) -> Option<usize> {
        self.vmas
            .iter()
            .enumerate()
            .filter(|(_, area)| {
                let start_vpn = area.vm_start();
                start_vpn >= mmap_base && start_vpn < mmap_end
            })
            .max_by_key(|(_, area)| area.vm_end().0)
            .map(|(idx, _)| idx)
    }

    pub(super) fn has_overlap(
        &self,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
    ) -> bool {
        self.vmas
            .iter()
            .any(|area| area.vm_overlaps(start_vpn, end_vpn))
    }

    pub(super) fn insert_ordered(&mut self, new_area: Vma) -> Result<(), isize> {
        if new_area.vm_start() >= new_area.vm_end() {
            return Err(EINVAL);
        }
        if self.has_overlap(new_area.vm_start(), new_area.vm_end()) {
            return Err(EINVAL);
        }
        self.try_reserve(1)?;
        let start_vpn = new_area.vm_start();
        if let Some(idx) = self
            .vmas
            .iter()
            .position(|area| area.vm_start() >= start_vpn)
        {
            self.vmas.insert(idx, new_area);
        } else {
            self.vmas.push(new_area);
        }
        Ok(())
    }

    pub(super) fn remove_area_with_start<T: PageTable>(
        &mut self,
        page_table: &mut T,
        start_vpn: VirtPageNum,
    ) -> Result<(), MemoryError> {
        if let Some(idx) = self.vmas.iter().position(|area| area.vm_start() == start_vpn) {
            let result = self.vmas[idx].unmap(page_table);
            self.vmas.remove(idx);
            result
        } else {
            Err(MemoryError::AreaNotFound)
        }
    }

    pub(super) fn split_for_range(
        &mut self,
        idx: usize,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
    ) -> Result<usize, isize> {
        let area_start_vpn = self.vmas[idx].vm_start();
        let area_end_vpn = self.vmas[idx].vm_end();
        if start_vpn < area_start_vpn || end_vpn > area_end_vpn || start_vpn >= end_vpn {
            return Err(EINVAL);
        }
        if start_vpn == area_start_vpn && end_vpn == area_end_vpn {
            Ok(idx)
        } else if start_vpn == area_start_vpn {
            self.try_reserve(1)?;
            let second = self.vmas[idx].into_two(end_vpn).map_err(|_| EINVAL)?;
            self.vmas.insert(idx + 1, second);
            Ok(idx)
        } else if end_vpn == area_end_vpn {
            self.try_reserve(1)?;
            let second = self.vmas[idx].into_two(start_vpn).map_err(|_| EINVAL)?;
            self.vmas.insert(idx + 1, second);
            Ok(idx + 1)
        } else {
            self.try_reserve(2)?;
            let (second, third) = self.vmas[idx]
                .into_three(start_vpn, end_vpn)
                .map_err(|_| EINVAL)?;
            self.vmas.insert(idx + 1, second);
            self.vmas.insert(idx + 2, third);
            Ok(idx + 1)
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
        let mut idx = 0usize;
        while idx < self.vmas.len() {
            if !self.vmas[idx].vm_overlaps(start_vpn, end_vpn) {
                idx += 1;
                continue;
            }
            if !self.vmas[idx].vm_is_user() {
                return Err(EINVAL);
            }
            found_area = true;
            let area_start_vpn = self.vmas[idx].vm_start();
            let area_end_vpn = self.vmas[idx].vm_end();
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
            let target_idx = self.split_for_range(idx, overlap_start, overlap_end)?;
            if self.vmas[target_idx].unmap(page_table).is_err() {
                warn!("[munmap] Some pages are already unmapped, is it caused by lazy alloc?");
            }
            self.vmas.remove(target_idx);
            idx = target_idx;
        }
        if found_area || allow_empty {
            Ok(found_area)
        } else {
            Err(EINVAL)
        }
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
            let Some(idx) = self.find_user_index(cursor) else {
                warn!("[mprotect] addr: {:?} is not in any user Vma", cursor);
                return Err(ENOMEM);
            };
            let area_end = self.vmas[idx].vm_end();
            let protect_end = if area_end < end_vpn {
                area_end
            } else {
                end_vpn
            };
            let target_idx = self.split_for_range(idx, cursor, protect_end)?;
            self.protect_area(page_table, target_idx, prot);
            cursor = protect_end;
        }
        Ok(())
    }

    pub(super) fn try_merge_lazy_private_mmap<T: PageTable>(
        &mut self,
        idx: usize,
        len: usize,
        prot: MapPermission,
        flags: MapFlags,
        mmap_end: usize,
    ) -> Result<Option<VirtAddr>, isize> {
        let area = &mut self.vmas[idx];
        if !area.vm_can_merge_lazy_private(prot, flags) {
            return Ok(None);
        }
        let end_va: VirtAddr = area.vm_end().into();
        let Some(new_end) = end_va.0.checked_add(len) else {
            return Err(EINVAL);
        };
        if new_end <= mmap_end {
            debug!("[mmap] merge with previous area, call expand_to");
            area.expand_to::<T>(VirtAddr::from(new_end))?;
            Ok(Some(end_va))
        } else {
            Ok(None)
        }
    }

    fn protect_area<T: PageTable>(
        &mut self,
        page_table: &mut T,
        idx: usize,
        prot: MapPermission,
    ) {
        let area = &mut self.vmas[idx];
        let mut has_unmapped_page = false;
        let actual_prot = if area.flags.contains(MapFlags::MAP_SHARED) {
            prot
        } else {
            prot - MapPermission::W
        };
        for vpn in area.inner.vpn_range {
            if area.frame_is_unallocated(vpn) {
                if area.clear_stale_pte(page_table, vpn) {
                    warn!(
                        "[mprotect] clear stale lazy pte: vpn={:?}, area={:?}",
                        vpn, area
                    );
                }
                has_unmapped_page = true;
                continue;
            }
            if UserMapper::new(page_table)
                .set_user_flags(vpn, actual_prot)
                .is_err()
            {
                has_unmapped_page = true;
            }
        }
        if has_unmapped_page {
            warn!("[mprotect] Some pages are not mapped, is it caused by lazy alloc?");
        }
        area.map_perm = prot;
    }

    fn ensure_can_add(&self, additional: usize) -> Result<(), isize> {
        if self
            .vmas
            .len()
            .checked_add(additional)
            .map_or(true, |len| len > MAX_VMA_COUNT)
        {
            Err(ENOMEM)
        } else {
            Ok(())
        }
    }
}

impl Index<usize> for VmaSet {
    type Output = Vma;

    fn index(&self, index: usize) -> &Self::Output {
        &self.vmas[index]
    }
}

impl IndexMut<usize> for VmaSet {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.vmas[index]
    }
}
