use core::fmt::Debug;

use super::frame_store::{Frame, FrameState, VmPageStore};
use super::page_table::PageTable;
use super::VPNRange;
use super::KERNEL_SPACE;
use super::{frame_alloc, FrameTracker};
use super::user_mapper::UserMapper;
use super::{FaultAccess, MemoryError};
use super::{PhysPageNum, VirtAddr, VirtPageNum};
use crate::fs::vfs::IndexNode;
use crate::mm::frame_allocator::frame_alloc_uninit;

use alloc::sync::Arc;
use log::{error, trace, warn};
impl Debug for Vma {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Vma")
            .field("interval", &self.inner)
            .field("map_perm", &self.map_perm)
            .field(
                "map_file",
                &if self.map_file.is_some() { "yes" } else { "no" },
            )
            .field("locked", &self.locked)
            .field("wipe_on_fork", &self.wipe_on_fork)
            .finish()
    }
}
#[derive(Clone)]
/// A user virtual memory area, covering ELF segments, heap, stack and mmap regions.
pub struct Vma {
    /// Range of the mapped virtual page numbers.
    /// Page aligned.
    /// Map physical page frame tracker to virtual pages for RAII & lookup.
    pub inner: VmPageStore,
    /// Permissions which are the or of RWXU, where U stands for user.
    pub map_perm: MapPermission,
    pub map_file: Option<Arc<dyn IndexNode>>,
    /// Offset into the file where this VMA starts (in bytes).
    /// For anonymous mappings, this is always 0.
    pub map_file_offset: usize,
    pub may_write: bool,

    pub flags: MapFlags,
    pub locked: bool,
    pub wipe_on_fork: bool,
}

impl Vma {
    pub fn try_clone(&self) -> Result<Self, isize> {
        let inner = self.inner.try_clone()?;
        Ok(Self {
            inner,
            map_perm: self.map_perm,
            map_file: self.map_file.clone(),
            map_file_offset: self.map_file_offset,
            may_write: self.may_write,
            flags: self.flags,
            locked: self.locked,
            wipe_on_fork: self.wipe_on_fork,
        })
    }
    /// Construct a new segment without without allocating memory
    pub fn new(
        start_va: VirtAddr,
        end_va: VirtAddr,
        map_perm: MapPermission,
        map_file: Option<Arc<dyn IndexNode>>,
        map_file_offset: usize,
    ) -> Self {
        Self::try_new(start_va, end_va, map_perm, map_file, map_file_offset).unwrap()
    }
    pub fn try_new(
        start_va: VirtAddr,
        end_va: VirtAddr,
        map_perm: MapPermission,
        map_file: Option<Arc<dyn IndexNode>>,
        map_file_offset: usize,
    ) -> Result<Self, isize> {
        let start_vpn: VirtPageNum = start_va.floor();
        let end_vpn: VirtPageNum = end_va.ceil();
        trace!(
            "[Vma new] start_vpn:{:X}; end_vpn:{:X}; map_perm:{:?}",
            start_vpn.0,
            end_vpn.0,
            map_perm
        );
        let inner = VmPageStore::try_new(VPNRange::new(start_vpn, end_vpn))?;
        Ok(Self {
            inner,
            map_perm,
            map_file,
            map_file_offset,
            may_write: true,
            flags: MapFlags::empty(),
            locked: false,
            wipe_on_fork: false,
        })
    }
    /// Copier, but the physical pages are not allocated,
    /// thus leaving `data_frames` empty.
    pub fn from_another(another: &Vma) -> Self {
        Self {
            inner: VmPageStore::new(VPNRange::new(
                another.inner.vpn_range.get_start(),
                another.inner.vpn_range.get_end(),
            )),
            map_perm: another.map_perm,
            map_file: another.map_file.clone(),
            map_file_offset: another.map_file_offset,
            may_write: another.may_write,
            flags: another.flags,
            locked: another.locked,
            wipe_on_fork: another.wipe_on_fork,
        }
    }
    pub fn frame_is_unallocated(&self, vpn: VirtPageNum) -> bool {
        self.inner.is_unallocated(&vpn)
    }
    fn map_page_with_perm<T: PageTable>(
        &self,
        page_table: &mut T,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        perm: MapPermission,
    ) -> Result<(), MemoryError> {
        let mut mapper = UserMapper::new(page_table);
        if perm.contains(MapPermission::U) {
            mapper.map_user_page(vpn, ppn, perm)
        } else {
            mapper.map_privileged_user_page(vpn, ppn, perm)
        }
    }
    pub fn clear_stale_pte<T: PageTable>(&self, page_table: &mut T, vpn: VirtPageNum) -> bool {
        // lazy页不应该保留有效pte
        matches!(
            UserMapper::new(page_table).unmap_user_page_if_mapped(vpn),
            Ok(true)
        )
    }
    pub fn map_one<T: PageTable>(
        &mut self,
        page_table: &mut T,
        vpn: VirtPageNum,
    ) -> Result<PhysPageNum, (MemoryError, VirtPageNum)> {
        let is_mapped = UserMapper::new(page_table).is_mapped(vpn);
        if !is_mapped {
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
        let frame = frame_alloc().ok_or(MemoryError::OutOfMemory)?;
        let ppn = frame.ppn;
        self.inner.alloc_in_memory(vpn, frame)?;
        if let Err(err) = self.map_page_with_perm(page_table, vpn, ppn, self.map_perm) {
            self.inner.remove_in_memory(&vpn);
            return Err(err);
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
        if let Err(err) = self.map_page_with_perm(page_table, vpn, ppn, self.map_perm) {
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
        if !UserMapper::new(page_table).is_mapped(vpn) {
            return Err(MemoryError::NotMapped);
        }
        self.inner.remove_in_memory(&vpn);
        UserMapper::new(page_table).unmap_user_page(vpn)?;
        Ok(())
    }

    pub fn discard_range<T: PageTable>(
        &mut self,
        page_table: &mut T,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
    ) -> Result<(), MemoryError> {
        for vpn in VPNRange::new(start_vpn, end_vpn) {
            if !self.vm_contains(vpn) {
                return Err(MemoryError::BadAddress);
            }
            if let Err(err) = self.unmap_one(page_table, vpn) {
                if !matches!(err, MemoryError::NotMapped) {
                    return Err(err);
                }
            }
        }
        Ok(())
    }

    pub fn map_from_existing_page_table<T: PageTable>(
        &mut self,
        dst_page_table: &mut T,
        src_page_table: &mut T,
    ) -> Result<(), MemoryError> {
        let is_shared = self.flags.contains(MapFlags::MAP_SHARED);
        let is_file_backed = self.map_file.is_some();
        let map_perm = if is_shared && is_file_backed && self.map_perm.contains(MapPermission::W) {
            self.map_perm.difference(MapPermission::W)
        } else if is_shared {
            self.map_perm
        } else {
            self.map_perm.difference(MapPermission::W)
        };
        for vpn in self.inner.vpn_range {
            if let Some(ppn) = src_page_table.block_and_ret_mut(vpn) {
                if !UserMapper::new(dst_page_table).is_mapped(vpn) {
                    self.map_page_with_perm(dst_page_table, vpn, ppn, map_perm)?;
                } else {
                    return Err(MemoryError::AlreadyMapped);
                }
                if is_shared && self.map_perm.contains(MapPermission::W) {
                    let _ = UserMapper::new(src_page_table).set_user_flags(vpn, self.map_perm);
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

    pub fn map_from_kernel_area<T: PageTable>(
        &mut self,
        page_table: &mut T,
        start_vpn_in_kernel_area: VirtPageNum,
    ) -> Result<(), ()> {
        let kernel_space = KERNEL_SPACE.lock();
        let mut src_vpn = start_vpn_in_kernel_area;
        let mut mapped_vpns = alloc::vec::Vec::new();
        let vpn_range = self.get_inner().vpn_range;
        mapped_vpns
            .try_reserve(vpn_range.get_end().0 - vpn_range.get_start().0)
            .map_err(|_| ())?;
        let rollback = |page_table: &mut T, this: &mut Vma, mapped_vpns: &[VirtPageNum]| {
            let mut mapper = UserMapper::new(page_table);
            for vpn in mapped_vpns.iter().rev() {
                let _ = mapper.unmap_user_page_if_mapped(*vpn);
                this.inner.remove_in_memory(vpn);
            }
        };
        for vpn in vpn_range {
            if let Some(frame) = kernel_space.mapped_frame(src_vpn) {
                let ppn = frame.ppn;
                if !UserMapper::new(page_table).is_mapped(vpn) {
                    if self.inner.alloc_in_memory(vpn, frame.clone()).is_err() {
                        rollback(page_table, self, &mapped_vpns);
                        return Err(());
                    }
                    if let Err(_) =
                        UserMapper::new(page_table).map_user_page(vpn, ppn, self.map_perm)
                    {
                        self.inner.remove_in_memory(&vpn);
                        rollback(page_table, self, &mapped_vpns);
                        return Err(());
                    }
                    mapped_vpns.push(vpn);
                } else {
                    error!("[map_from_kernel_area] user vpn already mapped!");
                    rollback(page_table, self, &mapped_vpns);
                    return Err(());
                }
            } else {
                error!("[map_from_kernel_area] kernel vpn invalid!");
                rollback(page_table, self, &mapped_vpns);
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
                    let set_ppn_result = UserMapper::new(page_table).set_ppn(vpn, ppn);
                    self.inner.record_active(vpn)?;
                    self.inner.dec_compressed();
                    set_ppn_result?;
                }
                RestoredPage::Swapped(ppn) => {
                    let set_ppn_result = UserMapper::new(page_table).set_ppn(vpn, ppn);
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
            UserMapper::new(page_table).set_user_flags(vpn, self.map_perm)?;

            trace!("[copy_on_write] no copy occurred");
            Ok(old_ppn)
        } else {
            // do copy in this case
            let old_ppn = old_frame.ppn;
            if !UserMapper::new(page_table).is_mapped(vpn) {
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
            if UserMapper::new(page_table).set_ppn(vpn, new_ppn).is_err() {
                if let Some(new_frame) = self.inner.remove_in_memory(&vpn) {
                    drop(new_frame);
                }
                let _ = self.inner.alloc_in_memory(vpn, old_frame);
                return Err(MemoryError::NotMapped);
            }
            if UserMapper::new(page_table)
                .set_user_flags(vpn, self.map_perm)
                .is_err()
            {
                let _ = UserMapper::new(page_table).set_ppn(vpn, old_ppn);
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
    /// If `new_start` is equal to the current start of area, do nothing and return `Ok(())`.
    pub fn expand_down_to(&mut self, new_start: VirtAddr) -> Result<(), isize> {
        let new_start_vpn: VirtPageNum = new_start.floor();
        let old_start_vpn = self.inner.vpn_range.get_start();
        if new_start_vpn > old_start_vpn {
            warn!(
                "[expand_down_to] new_start_vpn: {:?} is higher than old_start_vpn: {:?}",
                new_start_vpn, old_start_vpn
            );
            return Err(crate::syscall::errno::EINVAL);
        }
        if self.map_file.is_some() {
            warn!("[expand_down_to] file-backed MAP_GROWSDOWN is unsupported");
            return Err(crate::syscall::errno::EINVAL);
        }
        self.inner
            .set_start(new_start_vpn)
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
    pub fn into_two(&mut self, cut: VirtPageNum) -> Result<Self, ()> {
        let second_file = self.map_file.clone();
        let second_offset = if self.map_file.is_some() {
            self.map_file_offset.checked_add(
                VirtAddr::from(cut).0 - VirtAddr::from(self.inner.vpn_range.get_start()).0,
            ).ok_or(())?
        } else {
            0
        };
        let second_frames = self.inner.into_two(cut)?;
        Ok(Vma {
            inner: second_frames,
            map_perm: self.map_perm,
            map_file: second_file,
            map_file_offset: second_offset,
            may_write: self.may_write,
            flags: self.flags,
            locked: self.locked,
            wipe_on_fork: self.wipe_on_fork,
        })
    }
    pub fn into_three(
        &mut self,
        first_cut: VirtPageNum,
        second_cut: VirtPageNum,
    ) -> Result<(Self, Self), ()> {
        // 第一次切分：把 [Start, End] 切成 [Start, first_cut] 和 [first_cut, End]
        // into_two handles file clone and offset calculation automatically
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
                    if UserMapper::new(page_table).unmap_user_page(vpn).is_err() {
                        log::warn!("[do_oom] compressed frame has no mapped pte: vpn={:?}", vpn);
                    }
                    self.inner.inc_compressed();
                    trace!("[do_oom] compress frame: vpn={:?}, zram_id: {}", vpn, zram_id);
                    continue;
                }
                Err(MemoryError::SharedPage) => continue,
                Err(MemoryError::ZramIsFull) => {}
                Err(MemoryError::NotInMemory) => continue,
                Err(e) => {
                    log::warn!("[do_oom] unexpected zip error {:?}, vpn={:?}", e, vpn);
                    continue;
                }
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
                    if UserMapper::new(page_table).unmap_user_page(vpn).is_err() {
                        log::warn!("[do_oom] swapped frame has no mapped pte: vpn={:?}", vpn);
                    }
                    self.inner.inc_swapped();
                    trace!("[do_oom] swap out frame: vpn={:?}, swap_id: {}", vpn, swap_id);
                    continue;
                }
                Err(MemoryError::SharedPage) => continue,
                Err(MemoryError::NotInMemory) => continue,
                Err(MemoryError::OutOfMemory)
                | Err(MemoryError::SwapIsFull)
                | Err(MemoryError::BackingStoreFailure) => {
                    log::warn!("[do_oom] swap unavailable/full, stop reclaim: vpn={:?}", vpn);
                    break;
                }
                Err(e) => {
                    log::warn!("[do_oom] unexpected swap error {:?}, vpn={:?}", e, vpn);
                    break;
                }
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
                    if UserMapper::new(page_table).unmap_user_page(vpn).is_err() {
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
                Err(MemoryError::OutOfMemory)
                | Err(MemoryError::SwapIsFull)
                | Err(MemoryError::BackingStoreFailure) => {
                    log::warn!("[force_swap] swap unavailable/full, stop reclaim: vpn={:?}", vpn);
                    break;
                }
                Err(MemoryError::SharedPage)
                | Err(MemoryError::NotInMemory)
                | Err(MemoryError::NotSwappedOut) => continue,
                Err(e) => {
                    log::warn!("[force_swap] unexpected swap error {:?}, vpn={:?}", e, vpn);
                    break;
                }
            }
        }
        self.inner.swapped_count() - swapped_before
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum VmAreaKind {
    Anonymous,
    FileBacked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum VmAreaMapping {
    Private,
    Shared,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum VmPageState {
    InMemory,
    Unallocated,
    #[cfg(feature = "oom_handler")]
    Compressed,
    #[cfg(feature = "oom_handler")]
    SwappedOut,
}

impl Vma {
    pub(super) fn vm_start(&self) -> VirtPageNum {
        self.inner.vpn_range.get_start()
    }

    pub(super) fn vm_end(&self) -> VirtPageNum {
        self.inner.vpn_range.get_end()
    }

    pub(super) fn vm_contains(&self, vpn: VirtPageNum) -> bool {
        self.vm_start() <= vpn && vpn < self.vm_end()
    }

    pub(super) fn vm_overlaps(&self, start_vpn: VirtPageNum, end_vpn: VirtPageNum) -> bool {
        start_vpn < self.vm_end() && end_vpn > self.vm_start()
    }

    pub(super) fn vm_is_user(&self) -> bool {
        self.map_perm.contains(MapPermission::U)
    }

    pub(super) fn vm_kind(&self) -> VmAreaKind {
        if self.map_file.is_some() {
            VmAreaKind::FileBacked
        } else {
            VmAreaKind::Anonymous
        }
    }

    pub(super) fn vm_mapping_type(&self) -> VmAreaMapping {
        if self.flags.contains(MapFlags::MAP_SHARED) {
            VmAreaMapping::Shared
        } else {
            VmAreaMapping::Private
        }
    }

    pub(super) fn vm_mapping(&self) -> VmAreaMapping {
        self.vm_mapping_type()
    }

    pub(super) fn vm_can_merge_lazy_private(
        &self,
        prot: MapPermission,
        flags: MapFlags,
    ) -> bool {
        self.flags
            .contains(MapFlags::MAP_PRIVATE | MapFlags::MAP_ANONYMOUS)
            && flags.contains(MapFlags::MAP_PRIVATE | MapFlags::MAP_ANONYMOUS)
            && prot == self.map_perm
            && self.map_file.is_none()
            && !self.wipe_on_fork
    }

    pub(super) fn vm_perm(&self) -> MapPermission {
        self.map_perm
    }

    pub(super) fn vm_locked(&self) -> bool {
        self.locked
    }

    pub(super) fn set_vm_locked(&mut self, locked: bool) {
        self.locked = locked;
    }

    pub(super) fn vm_access_allows(&self, access: FaultAccess) -> bool {
        let required = match access {
            FaultAccess::Load => MapPermission::R,
            FaultAccess::Store => MapPermission::W,
            FaultAccess::Execute => MapPermission::X,
        };
        self.vm_perm().contains(required)
    }

    pub(super) fn vm_allows(&self, access: FaultAccess) -> bool {
        self.vm_access_allows(access)
    }

    pub(super) fn vm_page_state(&self, vpn: VirtPageNum) -> Result<VmPageState, MemoryError> {
        Ok(match self.inner.frame_state(vpn)? {
            FrameState::InMemory => VmPageState::InMemory,
            FrameState::Unallocated => VmPageState::Unallocated,
            #[cfg(feature = "oom_handler")]
            FrameState::Compressed => VmPageState::Compressed,
            #[cfg(feature = "oom_handler")]
            FrameState::SwappedOut => VmPageState::SwappedOut,
        })
    }

    pub(super) fn vm_is_stale_lazy(&self, vpn: VirtPageNum) -> bool {
        matches!(self.vm_page_state(vpn), Ok(VmPageState::Unallocated))
    }

    pub(super) fn vm_file(&self) -> Option<Arc<dyn IndexNode>> {
        self.map_file.clone()
    }

    pub(super) fn vm_file_offset(&self, vpn: VirtPageNum) -> Result<usize, MemoryError> {
        if !self.vm_contains(vpn) {
            return Err(MemoryError::BadAddress);
        }
        self.map_file_offset
            .checked_add(VirtAddr::from(vpn).0 - VirtAddr::from(self.vm_start()).0)
            .ok_or(MemoryError::BeyondEOF)
    }

    #[cfg(feature = "oom_handler")]
    pub(super) fn vm_decompress_page(
        &mut self,
        vpn: VirtPageNum,
    ) -> Result<PhysPageNum, MemoryError> {
        self.vm_frame_mut(vpn)?.unzip()
    }

    #[cfg(feature = "oom_handler")]
    pub(super) fn vm_swap_in_page(&mut self, vpn: VirtPageNum) -> Result<PhysPageNum, MemoryError> {
        self.vm_frame_mut(vpn)?.swap_in()
    }

    #[cfg(feature = "oom_handler")]
    pub(super) fn vm_record_resident_page<T: PageTable>(
        &mut self,
        vpn: VirtPageNum,
    ) -> Result<(), MemoryError> {
        self.inner.record_active(vpn)
    }

    #[cfg(feature = "oom_handler")]
    pub(super) fn vm_dec_compressed(&mut self) {
        self.inner.dec_compressed();
    }

    #[cfg(feature = "oom_handler")]
    pub(super) fn vm_dec_swapped(&mut self) {
        self.inner.dec_swapped();
    }

    #[cfg(feature = "oom_handler")]
    fn vm_frame_mut(&mut self, vpn: VirtPageNum) -> Result<&mut Frame, MemoryError> {
        self.inner.frame_mut_if_present(vpn)
    }
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
