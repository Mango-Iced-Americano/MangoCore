use core::fmt::Debug;

use super::frame_store::{Frame, VmPageStore};
use super::page_table::PageTable;
use super::VPNRange;
use super::KERNEL_SPACE;
use super::{frame_alloc, FrameTracker};
use super::{MemoryError, PageMapper};
use super::{PhysPageNum, VirtAddr, VirtPageNum};
use crate::fs::file_trait::File;
use crate::fs::SeekWhence;
use crate::mm::frame_allocator::frame_alloc_uninit;

use alloc::sync::Arc;
use alloc::vec::Vec;
use log::{error, trace, warn};
impl Debug for MapArea {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MapArea")
            .field("interval", &self.inner)
            .field("map_type", &self.map_type)
            .field("map_perm", &self.map_perm)
            .field(
                "map_file",
                &if self.map_file.is_some() { "yes" } else { "no" },
            )
            .finish()
    }
}
#[derive(Clone)]
/// Map area for different segments or a chunk of memory for memory mapped file access.
pub struct MapArea {
    /// Range of the mapped virtual page numbers.
    /// Page aligned.
    /// Map physical page frame tracker to virtual pages for RAII & lookup.
    pub inner: VmPageStore,
    /// Direct or framed(virtual) mapping?
    map_type: MapType,
    /// Permissions which are the or of RWXU, where U stands for user.
    pub map_perm: MapPermission,
    pub map_file: Option<Arc<dyn File>>,

    pub flags: MapFlags,
}

impl MapArea {
    pub fn try_clone(&self) -> Result<Self, isize> {
        let inner = self.inner.try_clone()?;
        Ok(Self {
            inner,
            map_type: self.map_type,
            map_perm: self.map_perm,
            map_file: self.map_file.clone(),
            flags: self.flags,
        })
    }
    /// Construct a new segment without without allocating memory
    pub fn new(
        start_va: VirtAddr,
        end_va: VirtAddr,
        map_type: MapType,
        map_perm: MapPermission,
        map_file: Option<Arc<dyn File>>,
    ) -> Self {
        Self::try_new(start_va, end_va, map_type, map_perm, map_file).unwrap()
    }
    pub fn try_new(
        start_va: VirtAddr,
        end_va: VirtAddr,
        map_type: MapType,
        map_perm: MapPermission,
        map_file: Option<Arc<dyn File>>,
    ) -> Result<Self, isize> {
        let start_vpn: VirtPageNum = start_va.floor();
        let end_vpn: VirtPageNum = end_va.ceil();
        trace!(
            "[MapArea new] start_vpn:{:X}; end_vpn:{:X}; map_perm:{:?}",
            start_vpn.0,
            end_vpn.0,
            map_perm
        );
        let inner = VmPageStore::try_new(VPNRange::new(start_vpn, end_vpn))?;
        Ok(Self {
            inner,
            map_type,
            map_perm,
            map_file,
            flags: MapFlags::empty(),
        })
    }
    /// Copier, but the physical pages are not allocated,
    /// thus leaving `data_frames` empty.
    pub fn from_another(another: &MapArea) -> Self {
        Self {
            inner: VmPageStore::new(VPNRange::new(
                another.inner.vpn_range.get_start(),
                another.inner.vpn_range.get_end(),
            )),
            map_type: another.map_type,
            map_perm: another.map_perm,
            map_file: another.map_file.clone(),
            flags: another.flags,
        }
    }
    pub fn frame_is_unallocated(&self, vpn: VirtPageNum) -> bool {
        self.inner.is_unallocated(&vpn)
    }
    pub fn clear_stale_pte<T: PageTable>(&self, page_table: &mut T, vpn: VirtPageNum) -> bool {
        // lazy页不应该保留有效pte
        matches!(PageMapper::new(page_table).unmap_if_mapped(vpn), Ok(true))
    }
    /// Create `MapArea` from `Vec<Arc<FrameTracker>>`. This function should only be used to
    /// generate a `MapArea` in `KERNEL_SPACE`. \
    /// # NOTE
    /// `start_vpn` will be set to `start_va.floor()`,
    /// `end_vpn` will be set to `start_vpn + frames.len()`,
    /// `map_file` will be set to `None`.
    #[cfg(feature = "oom_handler")]
    pub fn from_existing_frame(
        start_va: VirtAddr,
        map_type: MapType,
        map_perm: MapPermission,
        frames: Vec<Frame>,
    ) -> Self {
        let start_vpn = start_va.floor();
        let end_vpn = VirtPageNum::from(start_vpn.0 + frames.len());
        Self {
            inner: VmPageStore::from_existing_frames(VPNRange::new(start_vpn, end_vpn), frames),
            map_type,
            map_perm,
            map_file: None,
            flags: MapFlags::empty(),
        }
    }

    pub fn map_one<T: PageTable>(
        &mut self,
        page_table: &mut T,
        vpn: VirtPageNum,
    ) -> Result<PhysPageNum, (MemoryError, VirtPageNum)> {
        let is_mapped = PageMapper::new(page_table).is_mapped(vpn);
        if self.map_type == MapType::Identical || !is_mapped {
            //if not mapped
            self.map_one_unchecked(page_table, vpn)
                .map_err(|err| (err, vpn))
        } else {
            //mapped
            Err((MemoryError::AlreadyMapped, vpn))
        }
    }

    pub fn map_one_unchecked<T: PageTable>(
        &mut self,
        page_table: &mut T,
        vpn: VirtPageNum,
    ) -> Result<PhysPageNum, MemoryError> {
        let ppn: PhysPageNum;
        match self.map_type {
            MapType::Identical => {
                ppn = PhysPageNum(vpn.0);
                PageMapper::new(page_table).map_identical(vpn, ppn, self.map_perm);
            }
            MapType::Framed => {
                let frame = frame_alloc().ok_or(MemoryError::OutOfMemory)?;
                ppn = frame.ppn;
                self.inner.alloc_in_memory(vpn, frame)?;
                if let Err(err) = PageMapper::new(page_table).map(vpn, ppn, self.map_perm) {
                    self.inner.remove_in_memory(&vpn);
                    return Err(err);
                }
            }
        }
        Ok(ppn)
    }

    pub fn map_one_zeroed_unchecked<T: PageTable>(
        &mut self,
        page_table: &mut T,
        vpn: VirtPageNum,
    ) -> Result<PhysPageNum, MemoryError> {
        let frame = frame_alloc().ok_or(MemoryError::OutOfMemory)?;
        let ppn = frame.ppn;
        self.inner.alloc_in_memory(vpn, frame)?;
        if let Err(err) = PageMapper::new(page_table).map(vpn, ppn, self.map_perm) {
            self.inner.remove_in_memory(&vpn);
            return Err(err);
        }
        Ok(ppn)
    }
    /// Unmap a page in current area.
    /// If it is framed, then the physical pages will be removed from the `data_frames` Btree.
    /// This is unnecessary if the area is directly mapped.
    /// # Note
    /// Vpn should be in this map area, but the check is not enforced in this function!
    pub fn unmap_one<T: PageTable>(
        &mut self,
        page_table: &mut T,
        vpn: VirtPageNum,
    ) -> Result<(), MemoryError> {
        if !PageMapper::new(page_table).is_mapped(vpn) {
            return Err(MemoryError::NotMapped);
        }
        match self.map_type {
            MapType::Framed => {
                self.inner.remove_in_memory(&vpn);
                PageMapper::new(page_table).unmap(vpn)?;
            }
            _ => {}
        }
        Ok(())
    }

    // xein TODO:
    pub fn map_from_existing_page_table<T: PageTable>(
        &mut self,
        dst_page_table: &mut T,
        src_page_table: &mut T,
    ) -> Result<(), MemoryError> {
        let is_shared = self.flags.contains(MapFlags::MAP_SHARED);
        let map_perm = if is_shared {
            self.map_perm
        } else {
            self.map_perm.difference(MapPermission::W)
        };
        for vpn in self.inner.vpn_range {
            if let Some(ppn) = src_page_table.block_and_ret_mut(vpn) {
                if !PageMapper::new(dst_page_table).is_mapped(vpn) {
                    PageMapper::new(dst_page_table).map(vpn, ppn, map_perm)?;
                } else {
                    return Err(MemoryError::AlreadyMapped);
                }
                if is_shared && self.map_perm.contains(MapPermission::W) {
                    let _ = PageMapper::new(src_page_table).set_flags(vpn, self.map_perm);
                }
            }
        }
        Ok(())
    }
    pub fn get_inner(&self) -> &VmPageStore {
        &self.inner
    }
    pub fn get_start<T: PageTable>(&self) -> VirtPageNum {
        self.get_inner().vpn_range.get_start()
    }
    pub fn get_end<T: PageTable>(&self) -> VirtPageNum {
        self.get_inner().vpn_range.get_end()
    }

    pub fn get_lock(&self) -> &VmPageStore {
        &self.inner
    }
    pub fn map_from_kernel_area<T: PageTable>(
        &mut self,
        page_table: &mut T,
        start_vpn_in_kernel_area: VirtPageNum,
    ) -> Result<(), ()> {
        let mut kernel_space = KERNEL_SPACE.lock();
        let kernel_area = kernel_space
            .get_area_by_vpn_range(start_vpn_in_kernel_area)
            .unwrap();
        let mut src_vpn = start_vpn_in_kernel_area;
        for vpn in self.get_inner().vpn_range {
            if let Some(frame) = kernel_area.inner.get_in_memory(&src_vpn) {
                let ppn = frame.ppn;
                if !PageMapper::new(page_table).is_mapped(vpn) {
                    self.inner
                        .alloc_in_memory(vpn, frame.clone())
                        .map_err(|_| ())?;
                    if let Err(_) = PageMapper::new(page_table).map(vpn, ppn, self.map_perm) {
                        self.inner.remove_in_memory(&vpn);
                        return Err(());
                    }
                } else {
                    error!("[map_from_kernel_area] user vpn already mapped!");
                    return Err(());
                }
            } else {
                error!("[map_from_kernel_area] kernel vpn invalid!");
                return Err(());
            }
            src_vpn = (src_vpn.0 + 1).into();
        }
        Ok(())
    }
    /// Unmap all pages in `self` from `page_table` using unmap_one()
    pub fn unmap<T: PageTable>(&mut self, page_table: &mut T) -> Result<(), MemoryError> {
        let mut has_unmapped_page = false;
        for vpn in self.inner.vpn_range {
            // it's normal to get an `Error` because we are using lazy alloc strategy
            // we still need to unmap remaining pages of `self`, just throw this `Error` to caller
            if let Err(MemoryError::NotMapped) = self.unmap_one(page_table, vpn) {
                has_unmapped_page = true;
            }
        }
        if has_unmapped_page {
            Err(MemoryError::NotMapped)
        } else {
            Ok(())
        }
    }
    fn cow_source_frame<T: PageTable>(
        &mut self,
        page_table: &mut T,
        vpn: VirtPageNum,
    ) -> Result<Arc<FrameTracker>, MemoryError> {
        if !self.inner.contains_vpn(vpn) {
            return Err(MemoryError::BadAddress);
        }

        #[cfg(feature = "oom_handler")]
        {
            enum RestoredPage {
                None,
                Compressed(PhysPageNum),
                Swapped(PhysPageNum),
            }

            let restored = {
                let frame = self.inner.frame_mut_if_present(vpn)?;
                match frame {
                    Frame::InMemory(_) => RestoredPage::None,
                    Frame::Compressed(_) => {
                        let ppn = frame.unzip()?;
                        RestoredPage::Compressed(ppn)
                    }
                    Frame::SwappedOut(_) => {
                        let ppn = frame.swap_in()?;
                        RestoredPage::Swapped(ppn)
                    }
                    Frame::Unallocated => return Err(MemoryError::NotMapped),
                }
            };

            match restored {
                RestoredPage::None => {}
                RestoredPage::Compressed(ppn) => {
                    let set_ppn_result = PageMapper::new(page_table).set_ppn(vpn, ppn);
                    self.inner.record_active(vpn)?;
                    self.inner.dec_compressed();
                    set_ppn_result?;
                }
                RestoredPage::Swapped(ppn) => {
                    let set_ppn_result = PageMapper::new(page_table).set_ppn(vpn, ppn);
                    self.inner.record_active(vpn)?;
                    self.inner.dec_swapped();
                    set_ppn_result?;
                }
            }
        }

        self.inner
            .get_in_memory(&vpn)
            .cloned()
            .ok_or(MemoryError::BadAddress)
    }
    pub fn copy_on_write<T: PageTable>(
        &mut self,
        page_table: &mut T,
        vpn: VirtPageNum,
    ) -> Result<PhysPageNum, MemoryError> {
        let old_frame = match self.cow_source_frame(page_table, vpn) {
            Ok(frame) => frame,
            Err(err) => {
                warn!(
                    "[copy_on_write] mapped COW page has no resident frame: vpn={:?}, state={}, area={:?}",
                    vpn,
                    self.inner.frame_state_name(&vpn),
                    self
                );
                return Err(err);
            }
        };
        if Arc::strong_count(&old_frame) == 1 {
            let old_ppn = old_frame.ppn;
            PageMapper::new(page_table).set_flags(vpn, self.map_perm)?;

            trace!("[copy_on_write] no copy occurred");
            Ok(old_ppn)
        } else {
            // do copy in this case
            let old_ppn = old_frame.ppn;
            if !PageMapper::new(page_table).is_mapped(vpn) {
                return Err(MemoryError::NotMapped);
            }
            // alloc new frame
            let new_frame = unsafe { frame_alloc_uninit().ok_or(MemoryError::OutOfMemory)? };
            let new_ppn = new_frame.ppn;
            // copy data
            new_ppn
                .get_bytes_array()
                .copy_from_slice(old_ppn.get_bytes_array());
            let old_frame = self
                .inner
                .remove_in_memory(&vpn)
                .ok_or(MemoryError::BadAddress)?;
            if let Err(err) = self.inner.alloc_in_memory(vpn, new_frame) {
                let _ = self.inner.alloc_in_memory(vpn, old_frame);
                return Err(err);
            }
            if PageMapper::new(page_table).set_ppn(vpn, new_ppn).is_err() {
                if let Some(new_frame) = self.inner.remove_in_memory(&vpn) {
                    drop(new_frame);
                }
                let _ = self.inner.alloc_in_memory(vpn, old_frame);
                return Err(MemoryError::NotMapped);
            }
            if PageMapper::new(page_table)
                .set_flags(vpn, self.map_perm)
                .is_err()
            {
                let _ = PageMapper::new(page_table).set_ppn(vpn, old_ppn);
                if let Some(new_frame) = self.inner.remove_in_memory(&vpn) {
                    drop(new_frame);
                }
                let _ = self.inner.alloc_in_memory(vpn, old_frame);
                return Err(MemoryError::NotMapped);
            }
            trace!("[copy_on_write] copy occurred");
            Ok(new_ppn)
        }
    }
    /// If `new_end` is equal to the current end of area, do nothing and return `Ok(())`.
    pub fn expand_to<T: PageTable>(&mut self, new_end: VirtAddr) -> Result<(), isize> {
        let new_end_vpn: VirtPageNum = new_end.ceil();
        let old_end_vpn = self.inner.vpn_range.get_end();
        if new_end_vpn < old_end_vpn {
            warn!(
                "[expand_to] new_end_vpn: {:?} is lower than old_end_vpn: {:?}",
                new_end_vpn, old_end_vpn
            );
            return Err(crate::syscall::errno::EINVAL);
        }
        // `set_end` must be done before calling `map_one`
        // because `map_one` will insert frames into `data_frames`
        // if we don't `set_end` in advance, this insertion is out of bound
        self.inner
            .set_end(new_end_vpn)
            .map_err(|_| crate::syscall::errno::ENOMEM)?;
        Ok(())
    }
    /// If `new_end` is equal to the current end of area, do nothing and return `Ok(())`.
    pub fn shrink_to<T: PageTable>(
        &mut self,
        page_table: &mut T,
        new_end: VirtAddr,
    ) -> Result<(), ()> {
        let new_end_vpn: VirtPageNum = new_end.ceil();
        let old_end_vpn = self.inner.vpn_range.get_end();
        if new_end_vpn > old_end_vpn {
            warn!(
                "[expand_to] new_end_vpn: {:?} is higher than old_end_vpn: {:?}",
                new_end_vpn, old_end_vpn
            );
            return Err(());
        }
        let mut has_unmapped_page = false;
        for vpn in VPNRange::new(new_end_vpn, old_end_vpn) {
            if let Err(_) = self.unmap_one(page_table, vpn) {
                has_unmapped_page = true;
            }
        }
        // `set_end` must be done after calling `map_one`
        // for the similar reason with `expand_to`
        self.inner.set_end(new_end_vpn)?;
        if has_unmapped_page {
            warn!("[shrink_to] Some pages are already unmapped, is it caused by lazy alloc?");
            Err(())
        } else {
            Ok(())
        }
    }
    /// If `new_start` is equal to the current start of area, do nothing and return `Ok(())`.
    pub fn rshrink_to<T: PageTable>(
        &mut self,
        page_table: &mut T,
        new_start: VirtAddr,
    ) -> Result<(), ()> {
        let new_start_vpn: VirtPageNum = new_start.floor();
        let old_start_vpn = self.inner.vpn_range.get_start();
        if new_start_vpn < old_start_vpn {
            warn!(
                "[rshrink_to] new_start_vpn: {:?} is lower than old_start_vpn: {:?}",
                new_start_vpn, old_start_vpn
            );
            return Err(());
        }
        let mut has_unmapped_page = false;
        for vpn in VPNRange::new(old_start_vpn, new_start_vpn) {
            if let Err(_) = self.unmap_one(page_table, vpn) {
                has_unmapped_page = true;
            }
        }
        // `set_start` must be done after calling `map_one`
        // for the similar reason with `expand_to`
        self.inner.set_start(new_start_vpn)?;
        if has_unmapped_page {
            warn!("[rshrink_to] Some pages are already unmapped, is it caused by lazy alloc?");
            Err(())
        } else {
            Ok(())
        }
    }
    pub fn check_overlapping(
        &self,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
    ) -> Option<(VirtPageNum, VirtPageNum)> {
        let area_start_vpn = self.get_inner().vpn_range.get_start();
        let area_end_vpn = self.get_inner().vpn_range.get_end();
        if end_vpn <= area_start_vpn || start_vpn >= area_end_vpn {
            return None;
        } else {
            let start = if start_vpn > area_start_vpn {
                start_vpn
            } else {
                area_start_vpn
            };
            let end = if end_vpn < area_end_vpn {
                end_vpn
            } else {
                area_end_vpn
            };
            return Some((start, end));
        }
    }
    pub fn into_two(&mut self, cut: VirtPageNum) -> Result<Self, ()> {
        let second_file = if let Some(file) = &self.map_file {
            let new_file = file.deep_clone();
            let old_offset = file.lseek(0, SeekWhence::SEEK_CUR).map_err(|_| ())?;
            let new_offset = old_offset
                .checked_add(
                    VirtAddr::from(cut).0 - VirtAddr::from(self.inner.vpn_range.get_start()).0,
                )
                .ok_or(())?;
            if new_offset > isize::MAX as usize {
                return Err(());
            }
            new_file
                .lseek(new_offset as isize, SeekWhence::SEEK_SET)
                .map_err(|_| ())?;
            Some(new_file)
        } else {
            None
        };
        let second_frames = self.inner.into_two(cut)?;
        Ok(MapArea {
            inner: second_frames,
            map_type: self.map_type,
            map_perm: self.map_perm,
            map_file: second_file,
            flags: self.flags,
        })
    }
    pub fn into_three(
        &mut self,
        first_cut: VirtPageNum,
        second_cut: VirtPageNum,
    ) -> Result<(Self, Self), ()> {
        // if self.map_file.is_some() {
        //     warn!("[into_three] break apart file-back MapArea!");
        //     return Err(());
        // }
        // let (second_frames, third_frames) = self.inner.into_three(first_cut, second_cut)?;
        // Ok((
        //     MapArea {
        //         inner: second_frames,
        //         map_type: self.map_type,
        //         map_perm: self.map_perm,
        //         map_file: None,
        //         flags: self.flags,
        //     },
        //     MapArea {
        //         inner: third_frames,
        //         map_type: self.map_type,
        //         map_perm: self.map_perm,
        //         map_file: None,
        //         flags: self.flags,
        //     },
        // ))

        // 第一次切分：把 [Start, End] 切成 [Start, first_cut] 和 [first_cut, End]
        // into_two 会自动处理文件的 deep_clone 和偏移量计算
        let mut second_area = self.into_two(first_cut)?;

        // 第二次切分：把刚才得到的后半段 [first_cut, End] 再次切分
        // 切成 [first_cut, second_cut] 和 [second_cut, End]
        let third_area = second_area.into_two(second_cut)?;

        // 返回 (中间段, 最后段)
        // 此时 self 变成了第一段
        Ok((second_area, third_area))
    }
    #[cfg(feature = "oom_handler")]
    pub fn do_oom<T: PageTable>(&mut self, page_table: &mut T) -> usize {
        let compressed_before = self.inner.compressed_count();
        let swapped_before = self.inner.swapped_count();
        warn!("[do_oom] active pages: {}", self.inner.active_len());
        while let Some(vpn) = self.inner.pop_active() {
            if !self.inner.contains_vpn(vpn) {
                log::warn!("[do_oom] Defensive skip: vpn {:?} out of range", vpn);
                continue;
            }

            let zip_result = {
                let Ok(frame) = self.inner.frame_mut_if_present(vpn) else {
                    continue;
                };
                if !matches!(frame, Frame::InMemory(_)) {
                    continue;
                }
                frame.zip()
            };

            match zip_result {
                Ok(zram_id) => {
                    if PageMapper::new(page_table).unmap(vpn).is_err() {
                        log::warn!("[do_oom] compressed frame has no mapped pte: vpn={:?}", vpn);
                    }
                    self.inner.inc_compressed();
                    trace!("[do_oom] compress frame: vpn={:?}, zram_id: {}", vpn, zram_id);
                    continue;
                }
                Err(MemoryError::SharedPage) => continue,
                Err(MemoryError::ZramIsFull) => {}
                _ => unreachable!(),
            }

            let swap_result = {
                let Ok(frame) = self.inner.frame_mut_if_present(vpn) else {
                    continue;
                };
                if !matches!(frame, Frame::InMemory(_)) {
                    continue;
                }
                frame.swap_out()
            };

            match swap_result {
                Ok(swap_id) => {
                    if PageMapper::new(page_table).unmap(vpn).is_err() {
                        log::warn!("[do_oom] swapped frame has no mapped pte: vpn={:?}", vpn);
                    }
                    self.inner.inc_swapped();
                    trace!("[do_oom] swap out frame: vpn={:?}, swap_id: {}", vpn, swap_id);
                    continue;
                }
                Err(MemoryError::SharedPage) => continue,
                _ => unreachable!(),
            }
        }
        self.inner.compressed_count() + self.inner.swapped_count()
            - compressed_before
            - swapped_before
    }
    #[cfg(feature = "oom_handler")]
    pub fn force_swap<T: PageTable>(&mut self, page_table: &mut T) -> usize {
        let swapped_before = self.inner.swapped_count();
        warn!("[force_swap] active pages: {}", self.inner.active_len());
        while let Some(vpn) = self.inner.pop_active() {
            if !self.inner.contains_vpn(vpn) {
                log::warn!("[force_swap] Defensive skip: vpn {:?} out of range", vpn);
                continue;
            }

            let swap_result = {
                let Ok(frame) = self.inner.frame_mut_if_present(vpn) else {
                    continue;
                };
                if !matches!(frame, Frame::InMemory(_)) {
                    continue;
                }
                frame.force_swap_out()
            };

            match swap_result {
                Ok(swap_id) => {
                    if PageMapper::new(page_table).unmap(vpn).is_err() {
                        log::warn!(
                            "[force_swap] swapped frame has no mapped pte: vpn={:?}",
                            vpn
                        );
                    }
                    self.inner.inc_swapped();
                    trace!(
                        "[force_swap] swap out frame: vpn={:?}, swap_id: {}",
                        vpn, swap_id
                    );
                    continue;
                }
                _ => unreachable!(),
            }
        }
        self.inner.swapped_count() - swapped_before
    }
}

#[derive(Copy, Clone, PartialEq, Debug)]
pub enum MapType {
    Identical,
    Framed,
}

bitflags! {
    pub struct MapPermission: u8 {
        const R = 1 << 1;
        const W = 1 << 2;
        const X = 1 << 3;
        const U = 1 << 4;
    }
}
impl MapPermission {
    #[inline(always)]
    pub fn from_ph_flags(ph_flags: xmas_elf::program::Flags) -> Self {
        let mut map_perm = MapPermission::U;
        if ph_flags.is_read() {
            map_perm |= MapPermission::R;
        }
        if ph_flags.is_write() {
            map_perm |= MapPermission::W;
        }
        if ph_flags.is_execute() {
            map_perm |= MapPermission::X;
        }
        map_perm
    }
}

bitflags! {
    pub struct MapFlags: usize {
        const MAP_SHARED            =   0x01;
        const MAP_PRIVATE           =   0x02;
        const MAP_SHARED_VALIDATE   =   0x03;
        const MAP_TYPE              =   0x0f;
        const MAP_FIXED             =   0x10;
        const MAP_ANONYMOUS         =   0x20;
        const MAP_NORESERVE         =   0x4000;
        const MAP_GROWSDOWN         =   0x0100;
        const MAP_DENYWRITE         =   0x0800;
        const MAP_EXECUTABLE        =   0x1000;
        const MAP_LOCKED            =   0x2000;
        const MAP_POPULATE          =   0x8000;
        const MAP_NONBLOCK          =   0x10000;
        const MAP_STACK             =   0x20000;
        const MAP_HUGETLB           =   0x40000;
        const MAP_SYNC              =   0x80000;
        const MAP_FIXED_NOREPLACE   =   0x100000;
        const MAP_FILE              =   0;
    }
}

// #[derive(Debug)]
// pub struct VPNRange {
// 	start: VirtPageNum,
// 	end: VirtPageNum,
// }
// impl VPNRange {
// 	pub fn get_start(&self) -> VirtPageNum {
// 		self.start
// 	}
// 	pub fn get_end(&self) -> VirtPageNum {
// 		self.end
// 	}
// 	pub fn new(start: VirtPageNum, end: VirtPageNum) -> Self {
// 		Self { start, end }
// 	}
// 	pub fn len(&self) -> usize {
// 		self.end.0 - self.start.0
// 	}
// 	pub fn contains(&self, vpn: VirtPageNum) -> bool {
// 		vpn >= self.start && vpn < self.end
// 	}
// }
