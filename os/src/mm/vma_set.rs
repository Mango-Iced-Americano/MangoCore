#![allow(dead_code)]

use super::user_mapper::UserMapper;
use super::vma::{MapFlags, MapPermission, Vma};
use super::{MemoryError, PageTable, VirtAddr, VirtPageNum};
use crate::config::*;
use crate::syscall::errno::{EINVAL, ENOMEM};
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use log::{debug, warn};

const MAX_VMA_COUNT: usize = 65_536;

pub(super) struct VmaSet {
    vmas: BTreeMap<VirtPageNum, Vma>,
    mmap_holes: BTreeMap<VirtPageNum, VirtPageNum>,
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

impl VmaSet {
    pub(super) fn new() -> Self {
        Self::with_capacity(0)
    }

    pub(super) fn with_capacity(_capacity: usize) -> Self {
        let (mmap_start, mmap_end) = mmap_bounds();
        let mut mmap_holes = BTreeMap::new();
        mmap_holes.insert(mmap_start, mmap_end);
        Self {
            vmas: BTreeMap::new(),
            mmap_holes,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.vmas.len()
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
        let (mmap_start, mmap_end) = mmap_bounds();
        self.mmap_holes.insert(mmap_start, mmap_end);
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
        self.vmas.insert(start, new_area);
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
            let result = area.unmap(page_table);
            let _ = self.release_mmap_range(start, end);
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
            if target.unmap(page_table).is_err() {
                warn!("[munmap] Some pages are already unmapped, is it caused by lazy alloc?");
            }
            self.release_mmap_range(released_start, released_end)?;
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
        Ok(())
    }

    pub(super) fn release_mmap_range(
        &mut self,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
    ) -> Result<(), isize> {
        let Some((mut start_vpn, mut end_vpn)) = self.clip_mmap_range(start_vpn, end_vpn) else {
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
        self.reserve_mmap_range(start_vpn, end_vpn)?;
        let expand_result = {
            let area = self.vmas.get_mut(&key).ok_or(EINVAL)?;
            area.expand_to::<T>(VirtAddr::from(new_end))
        };
        if let Err(errno) = expand_result {
            let _ = self.release_mmap_range(start_vpn, end_vpn);
            return Err(errno);
        }
        debug!("[mmap] merge with previous area, call expand_to");
        Ok(Some(start_va))
    }

    fn protect_area<T: PageTable>(
        &mut self,
        page_table: &mut T,
        area_start: VirtPageNum,
        prot: MapPermission,
    ) -> Result<(), isize> {
        let area = self.vmas.get_mut(&area_start).ok_or(EINVAL)?;
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
        Ok(())
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

    fn insert_split_piece(&mut self, area: Vma) {
        self.vmas.insert(area.vm_start(), area);
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

    fn is_mmap_range_free(&self, start_vpn: VirtPageNum, end_vpn: VirtPageNum) -> bool {
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
}
