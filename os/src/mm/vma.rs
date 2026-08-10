//! 单个虚拟内存区域（VMA）的页级操作。
//!
//! `Vma` 保存地址区间、权限、mmap flag、文件后端以及页帧状态表，并提供单页映射、
//! 解除映射、fork 继承、CoW、范围伸缩和 OOM 回收等操作。
//!
//! # Semantics
//!
//! 匿名私有页支持懒分配和 CoW；`MAP_SHARED` 页不参与 CoW；文件映射的首次 fault
//! 由 `filemap` 路径填充。用户 PTE 写入必须通过 `UserMapper`，由它同步更新
//! `MmuGather`；不得从 VMA 层直接绕过失效记录和 frame 退休协议。

use core::fmt::Debug;

use super::filemap::ElfLazyBacking;
use super::frame_store::{Frame, FrameState, VmPageStore};
use super::page_table::PageTable;
use super::user_mapper::UserMapper;
use super::VPNRange;
use super::KERNEL_SPACE;
use super::{frame_alloc, FrameTracker};
use super::{AddressSpace, FaultAccess, MemoryError, PageTableImpl};
use super::{PhysPageNum, VirtAddr, VirtPageNum};
use crate::fs::vfs::IndexNode;
use crate::mm::frame_allocator::frame_alloc_uninit;

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use log::{error, warn};
impl Debug for Vma {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Vma")
            .field("interval", &self.inner)
            .field("map_perm", &self.map_perm)
            .field(
                "map_file",
                &if self.map_file.is_some() { "yes" } else { "no" },
            )
            .field("elf_lazy", &self.elf_lazy.is_some())
            .field("wipe_on_fork", &self.wipe_on_fork)
            .field("dont_fork", &self.dont_fork)
            .field("fork_inherited", &self.fork_inherited)
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
    /// Immutable PT_LOAD recipe for demand-paged executable VMAs. This is
    /// separate from ordinary mmap file backing because resident ELF pages are
    /// private frames assembled from potentially overlapping segments.
    pub(super) elf_lazy: Option<Arc<ElfLazyBacking>>,
    /// Offset into the file where this VMA starts (in bytes).
    /// For anonymous mappings, this is always 0.
    pub map_file_offset: usize,
    pub may_write: bool,
    pub write_sealed: bool,

    pub flags: MapFlags,
    pub wipe_on_fork: bool,
    pub dont_fork: bool,
    /// Anonymous VMAs copied by fork must not be merged with a later child-only
    /// mmap, matching Linux anon_vma merge constraints.
    pub fork_inherited: bool,
    /// 所属地址空间的非拥有回指。它只在 VMA 进入 `VmaSet` 时由外层
    /// `AddressSpace` 安装，PageCache rmap 通过它找到需要修改的页表；Weak
    /// 不会让已经 exec/munmap 的地址空间因缓存注册表而泄漏。
    pub(crate) user_address_space: Option<Weak<AddressSpace<PageTableImpl>>>,
}

/// PageCache rmap walker 使用的不可变 VMA 快照。
///
/// walker 不持有 `Arc<Vma>`，只保留独立 rmap 的强引用。这样既不会破坏
/// `VmaSet` 对 VMA 的唯一所有权，又能防止释放后地址复用造成 ABA。
#[derive(Clone)]
pub(crate) struct FileVmaSnapshot {
    pub(crate) rmap: Arc<FileVmaRmap>,
    pub(crate) start: VirtPageNum,
    pub(crate) end: VirtPageNum,
    pub(crate) file_offset: usize,
    pub(crate) owner: Weak<AddressSpace<PageTableImpl>>,
}

/// PageCache i_mmap 中保存的不可变反向映射记录。
///
/// 它与 VMA 本体分离：PageCache 只能弱持有此记录，不能为 `Arc<Vma>` 增加
/// Weak 计数。这样 VmaSet 在持有 VM 锁时仍是 VMA 的唯一 Arc owner，可以安全地
/// 取得 `&mut Vma`；rmap walker 只消费这里的标量和所属地址空间，不会并发借用 VMA。
pub(crate) struct FileVmaRmap {
    inode: Arc<dyn IndexNode>,
    start: VirtPageNum,
    end: VirtPageNum,
    file_offset: usize,
    owner: Weak<AddressSpace<PageTableImpl>>,
}

impl FileVmaRmap {
    pub(crate) fn from_vma(area: &Vma) -> Option<Self> {
        if !area.flags.contains(MapFlags::MAP_SHARED) {
            return None;
        }
        Some(Self {
            inode: area.vm_file()?,
            start: area.vm_start(),
            end: area.vm_end(),
            file_offset: area.map_file_offset,
            owner: area.user_address_space.clone()?,
        })
    }

    pub(crate) fn register(this: &Arc<Self>) {
        if let Some(cache) = this.inode.ensure_page_cache() {
            cache.register_file_vma(this);
        }
    }

    pub(crate) fn unregister(this: &Arc<Self>) {
        if let Some(cache) = this.inode.page_cache() {
            cache.unregister_file_vma(Arc::as_ptr(this) as usize);
        }
    }

    pub(crate) fn snapshot(this: &Arc<Self>) -> FileVmaSnapshot {
        FileVmaSnapshot {
            rmap: this.clone(),
            start: this.start,
            end: this.end,
            file_offset: this.file_offset,
            owner: this.owner.clone(),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum VmaUnmapReason {
    RemoveArea,
    Range,
}

impl Vma {
    pub fn try_clone(&self) -> Result<Self, isize> {
        let inner = self.inner.try_clone()?;
        Ok(Self {
            inner,
            map_perm: self.map_perm,
            map_file: self.map_file.clone(),
            elf_lazy: self.elf_lazy.clone(),
            map_file_offset: self.map_file_offset,
            may_write: self.may_write,
            write_sealed: self.write_sealed,
            flags: self.flags,
            wipe_on_fork: self.wipe_on_fork,
            dont_fork: self.dont_fork,
            fork_inherited: self.fork_inherited,
            user_address_space: self.user_address_space.clone(),
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
        let inner = VmPageStore::try_new(VPNRange::new(start_vpn, end_vpn))?;
        Ok(Self {
            inner,
            map_perm,
            map_file,
            elf_lazy: None,
            map_file_offset,
            may_write: true,
            write_sealed: false,
            flags: MapFlags::empty(),
            wipe_on_fork: false,
            dont_fork: false,
            fork_inherited: false,
            user_address_space: None,
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
            elf_lazy: another.elf_lazy.clone(),
            map_file_offset: another.map_file_offset,
            may_write: another.may_write,
            write_sealed: another.write_sealed,
            flags: another.flags,
            wipe_on_fork: another.wipe_on_fork,
            dont_fork: another.dont_fork,
            fork_inherited: another.fork_inherited,
            user_address_space: another.user_address_space.clone(),
        }
    }
    pub fn mark_fork_inherited(&mut self) {
        self.fork_inherited = true;
    }
    pub fn frame_is_unallocated(&self, vpn: VirtPageNum) -> bool {
        self.inner.is_unallocated(&vpn)
    }
    fn map_page_with_perm<T: PageTable>(
        &self,
        mapper: &mut UserMapper<'_, T>,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        perm: MapPermission,
    ) -> Result<(), MemoryError> {
        if perm.contains(MapPermission::U) {
            mapper.map_user_page(vpn, ppn, perm)
        } else {
            mapper.map_privileged_user_page(vpn, ppn, perm)
        }
    }
    pub fn clear_stale_pte<T: PageTable>(
        &self,
        mapper: &mut UserMapper<'_, T>,
        vpn: VirtPageNum,
    ) -> bool {
        // lazy页不应该保留有效pte
        matches!(mapper.unmap_user_page_if_mapped(vpn), Ok(true))
    }
    pub fn map_one<T: PageTable>(
        &mut self,
        mapper: &mut UserMapper<'_, T>,
        vpn: VirtPageNum,
    ) -> Result<PhysPageNum, (MemoryError, VirtPageNum)> {
        let is_mapped = mapper.is_mapped(vpn);
        if !is_mapped {
            //if not mapped
            self.map_one_unchecked(mapper, vpn)
                .map_err(|err| (err, vpn))
        } else {
            //mapped
            Err((MemoryError::AlreadyMapped, vpn))
        }
    }

    pub fn map_one_unchecked<T: PageTable>(
        &mut self,
        mapper: &mut UserMapper<'_, T>,
        vpn: VirtPageNum,
    ) -> Result<PhysPageNum, MemoryError> {
        let frame = frame_alloc().ok_or(MemoryError::OutOfMemory)?;
        let ppn = frame.ppn;
        self.inner.alloc_in_memory(vpn, frame)?;
        if let Err(err) = self.map_page_with_perm(mapper, vpn, ppn, self.map_perm) {
            self.inner.remove_in_memory(&vpn);
            return Err(err);
        }
        Ok(ppn)
    }

    pub fn map_one_zeroed_unchecked<T: PageTable>(
        &mut self,
        mapper: &mut UserMapper<'_, T>,
        vpn: VirtPageNum,
    ) -> Result<PhysPageNum, MemoryError> {
        let frame = frame_alloc().ok_or(MemoryError::OutOfMemory)?;
        let ppn = frame.ppn;
        self.inner.alloc_in_memory(vpn, frame)?;
        if let Err(err) = self.map_page_with_perm(mapper, vpn, ppn, self.map_perm) {
            self.inner.remove_in_memory(&vpn);
            return Err(err);
        }
        Ok(ppn)
    }

    pub fn alloc_one_zeroed_unmapped(
        &mut self,
        vpn: VirtPageNum,
    ) -> Result<PhysPageNum, MemoryError> {
        let frame = frame_alloc().ok_or(MemoryError::OutOfMemory)?;
        let ppn = frame.ppn;
        self.inner.alloc_in_memory(vpn, frame)?;
        Ok(ppn)
    }

    /// 回滚尚未安装 PTE 的页帧。
    ///
    /// 仅供“先初始化、后发布”路径在初始化或映射失败时使用；若 PTE 已经
    /// 可见，必须改走带 TLB retire 的正式 unmap 流程。
    pub(super) fn remove_unmapped_frame(&mut self, vpn: VirtPageNum) {
        let frame = self
            .inner
            .remove_in_memory(&vpn)
            .expect("unmapped VMA frame disappeared during rollback");
        drop(frame);
    }

    pub(super) fn map_existing_in_memory<T: PageTable>(
        &mut self,
        mapper: &mut UserMapper<'_, T>,
        vpn: VirtPageNum,
    ) -> Result<PhysPageNum, MemoryError> {
        if mapper.is_mapped(vpn) {
            return Err(MemoryError::AlreadyMapped);
        }
        let ppn = self
            .inner
            .get_in_memory(&vpn)
            .map(|frame| frame.ppn)
            .ok_or(MemoryError::NotMapped)?;
        self.map_page_with_perm(mapper, vpn, ppn, self.map_perm)?;
        Ok(ppn)
    }
    /// Unmap a page in current area.
    /// If it is framed, then the physical pages will be removed from the `data_frames` Btree.
    /// This is unnecessary if the area is directly mapped.
    /// # Note
    /// Vpn should be in this map area, but the check is not enforced in this function!
    pub fn unmap_one<T: PageTable>(
        &mut self,
        mapper: &mut UserMapper<'_, T>,
        vpn: VirtPageNum,
    ) -> Result<(), MemoryError> {
        if !mapper.is_mapped(vpn) {
            return Err(MemoryError::NotMapped);
        }
        mapper.unmap_user_page(vpn)?;
        self.note_file_pte_unmapped(vpn);
        if let Some(frame) = self.inner.remove_in_memory(&vpn) {
            mapper.retire_frame(frame);
        }
        Ok(())
    }

    pub fn discard_range<T: PageTable>(
        &mut self,
        mapper: &mut UserMapper<'_, T>,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
    ) -> Result<(), MemoryError> {
        if start_vpn < self.vm_start() || end_vpn > self.vm_end() {
            return Err(MemoryError::BadAddress);
        }
        let mut cursor = start_vpn;
        while let Some(vpn) = self.inner.first_in_memory_vpn_in_range(cursor, end_vpn) {
            let unmapped = mapper.unmap_user_page_if_mapped(vpn)?;
            if unmapped {
                self.note_file_pte_unmapped(vpn);
            }
            if let Some(frame) = self.inner.remove_in_memory(&vpn) {
                if unmapped {
                    mapper.retire_frame(frame);
                }
            }
            cursor = VirtPageNum(vpn.0.saturating_add(1));
        }
        Ok(())
    }

    pub fn map_from_existing_page_table<T: PageTable>(
        &mut self,
        dst_mapper: &mut UserMapper<'_, T>,
        src_mapper: &mut UserMapper<'_, T>,
    ) -> Result<(), MemoryError> {
        let is_shared = self.flags.contains(MapFlags::MAP_SHARED);
        let is_file_backed = self.map_file.is_some();
        let is_writable = self.map_perm.contains(MapPermission::W);
        let protect_parent_for_cow = !is_shared && is_writable;
        let map_perm = if is_shared && is_file_backed && is_writable {
            self.map_perm.difference(MapPermission::W)
        } else if protect_parent_for_cow {
            self.map_perm.difference(MapPermission::W)
        } else {
            self.map_perm
        };
        let mut first_error = None;
        let mut mapped_end = self.inner.vpn_range.get_start();
        for vpn in self.inner.vpn_range {
            let ppn = if protect_parent_for_cow {
                src_mapper.block_write(vpn)
            } else {
                src_mapper.translate(vpn)
            };
            if let Some(ppn) = ppn {
                if !dst_mapper.is_mapped(vpn) {
                    let map_result = if map_perm.contains(MapPermission::U) {
                        dst_mapper.map_user_page(vpn, ppn, map_perm)
                    } else {
                        dst_mapper.map_privileged_user_page(vpn, ppn, map_perm)
                    };
                    if let Err(err) = map_result {
                        first_error = Some(err);
                        break;
                    }
                    self.note_file_pte_mapped(vpn);
                    mapped_end = VirtPageNum(vpn.0 + 1);
                } else {
                    first_error = Some(MemoryError::AlreadyMapped);
                    break;
                }
            }
        }
        if first_error.is_some() {
            for vpn in VPNRange::new(self.inner.vpn_range.get_start(), mapped_end) {
                let _ = dst_mapper.unmap_user_page_if_mapped(vpn);
            }
        }
        match first_error {
            Some(err) => Err(err),
            None => Ok(()),
        }
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
        mapper: &mut UserMapper<'_, T>,
        start_vpn_in_kernel_area: VirtPageNum,
    ) -> Result<(), ()> {
        let kernel_space = KERNEL_SPACE.lock();
        let mut src_vpn = start_vpn_in_kernel_area;
        let mut mapped_vpns = alloc::vec::Vec::new();
        let vpn_range = self.get_inner().vpn_range;
        mapped_vpns
            .try_reserve(vpn_range.get_end().0 - vpn_range.get_start().0)
            .map_err(|_| ())?;
        let rollback =
            |mapper: &mut UserMapper<'_, T>, this: &mut Vma, mapped_vpns: &[VirtPageNum]| {
                for vpn in mapped_vpns.iter().rev() {
                    let unmapped = mapper.unmap_user_page_if_mapped(*vpn).unwrap_or(false);
                    if let Some(frame) = this.inner.remove_in_memory(vpn) {
                        if unmapped {
                            mapper.retire_frame(frame);
                        }
                    }
                }
            };
        for vpn in vpn_range {
            if let Some(frame) = kernel_space.mapped_frame(src_vpn) {
                let ppn = frame.ppn;
                if !mapper.is_mapped(vpn) {
                    if self.inner.alloc_in_memory(vpn, frame.clone()).is_err() {
                        rollback(mapper, self, &mapped_vpns);
                        return Err(());
                    }
                    if let Err(_) = mapper.map_user_page(vpn, ppn, self.map_perm) {
                        self.inner.remove_in_memory(&vpn);
                        rollback(mapper, self, &mapped_vpns);
                        return Err(());
                    }
                    mapped_vpns.push(vpn);
                } else {
                    error!("[map_from_kernel_area] user vpn already mapped!");
                    rollback(mapper, self, &mapped_vpns);
                    return Err(());
                }
            } else {
                error!("[map_from_kernel_area] kernel vpn invalid!");
                rollback(mapper, self, &mapped_vpns);
                return Err(());
            }
            src_vpn = (src_vpn.0 + 1).into();
        }
        Ok(())
    }
    /// Unmap resident pages in `self` from `page_table`.
    pub(super) fn unmap<T: PageTable>(
        &mut self,
        mapper: &mut UserMapper<'_, T>,
        reason: VmaUnmapReason,
    ) -> Result<(), MemoryError> {
        let record_anon_private =
            crate::task::perf::stats_enabled_for(crate::task::perf::STATS_PROFILE_MEMORY_IO)
                && self.map_file.is_none()
                && self
                    .flags
                    .contains(MapFlags::MAP_PRIVATE | MapFlags::MAP_ANONYMOUS)
                && !self.flags.contains(MapFlags::MAP_SHARED);
        let requested_pages = self.vm_end().0.saturating_sub(self.vm_start().0);
        let start_ticks = if record_anon_private {
            crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO)
        } else {
            0
        };
        #[cfg(feature = "oom_handler")]
        let active_before = if record_anon_private {
            self.inner.active_len()
        } else {
            0
        };
        #[cfg(not(feature = "oom_handler"))]
        let active_before = 0;
        let resident_pages = self.inner.in_memory_len();
        let mut retain_scan_steps = 0usize;
        let end_vpn = self.vm_end();
        let mut cursor = self.vm_start();
        while let Some(vpn) = self.inner.first_in_memory_vpn_in_range(cursor, end_vpn) {
            let unmapped = match mapper.unmap_user_page_if_mapped(vpn) {
                Ok(unmapped) => unmapped,
                Err(error) => {
                    if record_anon_private {
                        crate::task::perf::record_anon_unmap(
                            matches!(reason, VmaUnmapReason::Range),
                            requested_pages,
                            resident_pages,
                            active_before,
                            retain_scan_steps,
                            start_ticks,
                            true,
                        );
                    }
                    return Err(error);
                }
            };
            if unmapped {
                self.note_file_pte_unmapped(vpn);
            }
            if record_anon_private {
                #[cfg(feature = "oom_handler")]
                {
                    retain_scan_steps = retain_scan_steps.saturating_add(self.inner.active_len());
                }
            }
            if let Some(frame) = self.inner.remove_in_memory(&vpn) {
                if unmapped {
                    mapper.retire_frame(frame);
                }
            }
            cursor = VirtPageNum(vpn.0.saturating_add(1));
        }
        if record_anon_private {
            crate::task::perf::record_anon_unmap(
                matches!(reason, VmaUnmapReason::Range),
                requested_pages,
                resident_pages,
                active_before,
                retain_scan_steps,
                start_ticks,
                false,
            );
        }
        Ok(())
    }
    fn cow_source_frame<T: PageTable>(
        &mut self,
        mapper: &mut UserMapper<'_, T>,
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
                    let set_ppn_result = mapper.set_ppn(vpn, ppn);
                    self.inner.record_active(vpn)?;
                    self.inner.dec_compressed();
                    set_ppn_result?;
                }
                RestoredPage::Swapped(ppn) => {
                    let set_ppn_result = mapper.set_ppn(vpn, ppn);
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
        mapper: &mut UserMapper<'_, T>,
        vpn: VirtPageNum,
    ) -> Result<PhysPageNum, MemoryError> {
        let old_frame = match self.cow_source_frame(mapper, vpn) {
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
        // cow_source_frame() returns a cloned Arc, so a page owned only by this
        // VMA has two strong refs here: the VMA entry and this local handle.
        if Arc::strong_count(&old_frame) <= 2 {
            let old_ppn = old_frame.ppn;
            mapper.set_user_flags(vpn, self.map_perm)?;
            Ok(old_ppn)
        } else {
            // do copy in this case
            let old_ppn = old_frame.ppn;
            if !mapper.is_mapped(vpn) {
                return Err(MemoryError::NotMapped);
            }
            // Safety: 新页会在下面立即用旧页完整覆盖，然后才替换 PTE 暴露给用户。
            let new_frame = unsafe { frame_alloc_uninit().ok_or(MemoryError::OutOfMemory)? };
            let new_ppn = new_frame.ppn;
            // Safety: `old_frame` 的 Arc 固定源页生命周期，外层地址空间写锁
            // 串行化 CoW 元数据；`new_frame` 尚未写入 VMA/PTE，因而由本路径
            // 独占。源页可能仍被其它 CPU 只读映射，所以只创建只读视图。
            unsafe {
                old_ppn.with_bytes(|src| {
                    new_ppn.with_bytes_mut(|dst| dst.copy_from_slice(src));
                });
            }
            let old_frame = self
                .inner
                .remove_in_memory(&vpn)
                .ok_or(MemoryError::BadAddress)?;
            if let Err(err) = self.inner.alloc_in_memory(vpn, new_frame) {
                self.inner
                    .alloc_in_memory(vpn, old_frame)
                    .expect("COW rollback could not restore the source frame");
                return Err(err);
            }
            if mapper.set_ppn(vpn, new_ppn).is_err() {
                if let Some(new_frame) = self.inner.remove_in_memory(&vpn) {
                    // `set_ppn` 失败说明新 PPN 从未写进 PTE，任何 CPU 都不可能
                    // 通过旧翻译访问这个新页，因此这里可以直接归还而无需退休。
                    drop(new_frame);
                }
                self.inner
                    .alloc_in_memory(vpn, old_frame)
                    .expect("COW rollback could not restore the source frame");
                return Err(MemoryError::NotMapped);
            }
            if mapper.set_user_flags(vpn, self.map_perm).is_err() {
                mapper
                    .set_ppn(vpn, old_ppn)
                    .expect("COW rollback lost the PTE after replacing its PPN");
                if let Some(new_frame) = self.inner.remove_in_memory(&vpn) {
                    // 新页曾经出现在 PTE 中；即使尚未主动刷新，也必须把它留到
                    // 本批提交后再释放，不能假设硬件一定没有并行填充该表项。
                    mapper.retire_frame(new_frame);
                }
                self.inner
                    .alloc_in_memory(vpn, old_frame)
                    .expect("COW rollback could not restore the source frame");
                return Err(MemoryError::NotMapped);
            }
            mapper.retire_frame(old_frame);
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
        mapper: &mut UserMapper<'_, T>,
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
            if let Err(_) = self.unmap_one(mapper, vpn) {
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
        mapper: &mut UserMapper<'_, T>,
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
            if let Err(_) = self.unmap_one(mapper, vpn) {
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
            self.map_file_offset
                .checked_add(
                    VirtAddr::from(cut).0 - VirtAddr::from(self.inner.vpn_range.get_start()).0,
                )
                .ok_or(())?
        } else {
            0
        };
        let second_frames = self.inner.into_two(cut)?;
        Ok(Vma {
            inner: second_frames,
            map_perm: self.map_perm,
            map_file: second_file,
            elf_lazy: self.elf_lazy.clone(),
            map_file_offset: second_offset,
            may_write: self.may_write,
            write_sealed: self.write_sealed,
            flags: self.flags,
            wipe_on_fork: self.wipe_on_fork,
            dont_fork: self.dont_fork,
            fork_inherited: self.fork_inherited,
            user_address_space: self.user_address_space.clone(),
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
    pub fn do_oom<T: PageTable>(&mut self, mapper: &mut UserMapper<'_, T>) -> usize {
        let compressed_before = self.inner.compressed_count();
        let swapped_before = self.inner.swapped_count();
        let candidate_count = self.inner.active_len();
        warn!("[do_oom] active pages: {}", candidate_count);
        // 只扫描进入函数时已有的候选。共享页会放回队尾；若继续使用
        // `while let`，一个被 futex pin 的页会在同一轮中被反复取出而死循环。
        for _ in 0..candidate_count {
            let Some(vpn) = self.inner.pop_active() else {
                break;
            };
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
                Ok(frame) => {
                    if mapper.unmap_user_page(vpn).is_ok() {
                        mapper.retire_frame(frame);
                    } else {
                        log::warn!("[do_oom] compressed frame has no mapped pte: vpn={:?}", vpn);
                    }
                    self.inner.inc_compressed();
                    continue;
                }
                Err(MemoryError::SharedPage) => {
                    self.inner.requeue_active(vpn);
                    continue;
                }
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
                Ok(frame) => {
                    if mapper.unmap_user_page(vpn).is_ok() {
                        mapper.retire_frame(frame);
                    } else {
                        log::warn!("[do_oom] swapped frame has no mapped pte: vpn={:?}", vpn);
                    }
                    self.inner.inc_swapped();
                    continue;
                }
                Err(MemoryError::SharedPage) => {
                    self.inner.requeue_active(vpn);
                    continue;
                }
                Err(MemoryError::NotInMemory) => continue,
                Err(MemoryError::OutOfMemory)
                | Err(MemoryError::SwapIsFull)
                | Err(MemoryError::BackingStoreFailure) => {
                    self.inner.requeue_active(vpn);
                    log::warn!(
                        "[do_oom] swap unavailable/full, stop reclaim: vpn={:?}",
                        vpn
                    );
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum VmAreaKind {
    Anonymous,
    FileBacked,
    ElfLazy,
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
        if self.elf_lazy.is_some() {
            VmAreaKind::ElfLazy
        } else if self.map_file.is_some() {
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

    pub(super) fn vm_can_merge_lazy_private(&self, prot: MapPermission, flags: MapFlags) -> bool {
        self.flags
            .contains(MapFlags::MAP_PRIVATE | MapFlags::MAP_ANONYMOUS)
            && flags.contains(MapFlags::MAP_PRIVATE | MapFlags::MAP_ANONYMOUS)
            && prot == self.map_perm
            && self.map_file.is_none()
            && !self.wipe_on_fork
            && !self.dont_fork
            && !self.fork_inherited
    }

    pub(super) fn vm_perm(&self) -> MapPermission {
        self.map_perm
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

    pub(super) fn vm_elf_backing(&self) -> Option<Arc<ElfLazyBacking>> {
        self.elf_lazy.clone()
    }

    pub(super) fn vm_file_offset(&self, vpn: VirtPageNum) -> Result<usize, MemoryError> {
        if !self.vm_contains(vpn) {
            return Err(MemoryError::BadAddress);
        }
        self.map_file_offset
            .checked_add(VirtAddr::from(vpn).0 - VirtAddr::from(self.vm_start()).0)
            .ok_or(MemoryError::BeyondEOF)
    }

    /// PTE 已撤销后才减少 PageCache 的映射计数。这里不持 PageCache 条目锁；
    /// VMA/页表状态由调用方的 VM 锁保护，而 PageCache 的计数只是 rmap 快速路径。
    pub(super) fn note_file_pte_unmapped(&self, vpn: VirtPageNum) {
        let Some(inode) = self.vm_file() else {
            return;
        };
        let Ok(file_offset) = self.vm_file_offset(vpn) else {
            return;
        };
        if let Some(cache) = inode.page_cache() {
            cache.unmap_page(file_offset >> crate::config::PAGE_SIZE_BITS);
        }
    }

    /// fork 已把 file-backed resident PTE 装入子页表后递增 PageCache map_count。
    /// 这与 fault 安装路径相同，计数只在 PTE 真正可见后更新。
    fn note_file_pte_mapped(&self, vpn: VirtPageNum) {
        let Some(inode) = self.vm_file() else {
            return;
        };
        let Ok(file_offset) = self.vm_file_offset(vpn) else {
            return;
        };
        if let Some(cache) = inode.page_cache() {
            cache.map_page(file_offset >> crate::config::PAGE_SIZE_BITS);
        }
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
        /// Global mapping: survives non-global TLB invalidations.
        const G = 1 << 5;
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
