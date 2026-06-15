use super::mapper::translate_page;
use super::page_table::{FaultAccess, PageTable, UserAccess};
use super::user_mapper::UserMapper;
use super::vma::*;
use super::vma_set::VmaSet;
use super::{FrameTracker, PhysAddr, PhysPageNum, VPNRange, VirtAddr, VirtPageNum, KERNEL_SPACE};
use crate::config::*;
use crate::fs::vfs::IndexNode;
use crate::hal::TrapContext;
use crate::hal::TICKS_PER_SEC;
use crate::should_map_trampoline;
use crate::syscall::errno::*;
use crate::task::{
    current_task_ref, trap_cx_bottom_from_slot, ustack_bottom_from_slot, AuxvEntry, AuxvType,
    ELFInfo,
};
use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Write;
use log::{debug, error, warn};

extern "C" {
    fn strampoline();
    fn ssignaltrampoline();
}

const PROC_SMAPS_DENSE_VMA_THRESHOLD: usize = 1024;
const PROC_SMAPS_FULL_ENTRY_ESTIMATE: usize = 1024;
const PROC_SMAPS_COMPACT_ENTRY_ESTIMATE: usize = 256;

#[allow(unused)]
#[derive(Debug)]
pub enum MemoryError {
    BadAddress,
    AreaNotFound,
    AlreadyMapped,
    NotMapped,
    NoPermission,
    NotInMemory,
    NotCompressed,
    NotSwappedOut,
    AlreadyAllocated,
    SharedPage,
    ZramIsFull,
    SwapIsFull,
    BeyondEOF,
    OutOfMemory,
    BackingStoreFailure,
}

/// The user memory address space.
pub struct AddressSpace<T: PageTable> {
    /// 页表实现
    pub(super) page_table: T,
    /// 用户 VMA 集合，负责用户区间查找、插入、拆分和空洞管理。
    pub(super) vmas: VmaSet,
    /// 进程堆起始位置，由 ELF 加载阶段初始化。
    pub(super) heap_bottom: usize,
    /// 当前 program break。该状态属于地址空间，CLONE_VM 线程自然共享。
    pub(super) heap_pt: usize,
    /// ABI-visible mlock state used for /proc/<pid>/status VmLck accounting.
    locked_pages: BTreeSet<VirtPageNum>,
}

impl<T: PageTable> AddressSpace<T> {
    /// Create a new struct with no information at all.
    pub fn new_bare() -> Self {
        Self {
            page_table: T::new(),
            vmas: VmaSet::with_capacity(16),
            heap_bottom: 0,
            heap_pt: 0,
            locked_pages: BTreeSet::new(),
        }
    }
    /// Getter to the token of current memory space, or "this" page table.
    pub fn token(&self) -> usize {
        self.page_table.token()
    }
    /// VMA 数量（用于诊断）
    pub fn vma_count(&self) -> usize {
        self.vmas.len()
    }
    pub fn committed_bytes(&self) -> usize {
        self.vmas.user_mapped_bytes()
    }
    pub fn has_shared_writable_mapping(&self, inode: &Arc<dyn IndexNode>) -> bool {
        self.vmas.has_shared_writable_mapping(inode)
    }
    /// Insert an anonymous segment containing the space between `start_va.floor()` to `end_va.ceil()`.
    /// The space is allocated and added to the current address space.
    /// # Prerequisite
    /// Assuming no conflicts. In other words, the space is NOT checked for space validity or overlap.
    /// It is merely mapped, pushed into the current address space.
    /// Since CoW is implemented, the space is NOT allocated until a page fault is triggered.
    pub fn insert_framed_area(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: MapPermission,
    ) {
        let mut area = Vma::new(start_va, end_va, permission, None, 0);
        area.flags = MapFlags::MAP_PRIVATE | MapFlags::MAP_ANONYMOUS;
        self.push(area, None).unwrap();
    }
    fn insert_user_stack_area(
        &mut self,
        stack_bottom: VirtAddr,
    ) -> Result<(), (MemoryError, VirtPageNum)> {
        let stack_top = VirtAddr::from(stack_bottom.0.saturating_sub(USER_STACK_SIZE));
        let init_top = VirtAddr::from(stack_bottom.0.saturating_sub(USER_STACK_INIT_SIZE));
        let start_vpn = stack_top.floor();
        self.vmas
            .try_reserve(1)
            .map_err(|_| (MemoryError::OutOfMemory, start_vpn))?;
        let mut area = Vma::new(
            stack_top,
            stack_bottom,
            MapPermission::R | MapPermission::W | MapPermission::U,
            None,
            0,
        );
        area.flags = MapFlags::MAP_PRIVATE | MapFlags::MAP_ANONYMOUS | MapFlags::MAP_STACK;
        for vpn in VPNRange::new(init_top.floor(), stack_bottom.ceil()) {
            area.map_one(&mut self.page_table, vpn)?;
        }
        self.vmas
            .push(area)
            .map_err(|_| (MemoryError::OutOfMemory, start_vpn))
    }
    fn insert_framed_area_first_ppn(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: MapPermission,
    ) -> Result<PhysPageNum, (MemoryError, VirtPageNum)> {
        let mut area = Vma::new(start_va, end_va, permission, None, 0);
        area.flags = MapFlags::MAP_PRIVATE | MapFlags::MAP_ANONYMOUS;
        self.push_with_first_ppn(area, None)?
            .ok_or((MemoryError::BadAddress, start_va.floor()))
    }
    pub fn remove_area_with_start_vpn(
        &mut self,
        start_vpn: VirtPageNum,
    ) -> Result<(), MemoryError> {
        self.vmas
            .remove_area_with_start(&mut self.page_table, start_vpn)
    }
    /// Push a not-yet-mapped VMA into current address space and copy the data into it if any.
    fn push(&mut self, vma: Vma, data: Option<&[u8]>) -> Result<(), (MemoryError, VirtPageNum)> {
        self.push_with_first_ppn(vma, data).map(|_| ())
    }

    /// Push a VMA and return the first physical page that was allocated.
    /// This lets hot paths such as thread clone keep the PPN produced during
    /// mapping instead of immediately doing a second page-table walk.
    fn push_with_first_ppn(
        &mut self,
        mut vma: Vma,
        data: Option<&[u8]>,
    ) -> Result<Option<PhysPageNum>, (MemoryError, VirtPageNum)> {
        let start_vpn = vma.inner.vpn_range.get_start();
        self.vmas
            .try_reserve(1)
            .map_err(|_| (MemoryError::OutOfMemory, start_vpn))?;
        let mut first_ppn = None;
        match data {
            Some(data) => {
                let mut start = 0;
                let len = data.len();
                for vpn in vma.inner.vpn_range {
                    let ppn = vma.map_one(&mut self.page_table, vpn)?;
                    first_ppn.get_or_insert(ppn);
                    let end = start + PAGE_SIZE;
                    let src = &data[start..len.min(end)];
                    ppn.get_bytes_array()[..src.len()].copy_from_slice(src);
                    start = end;
                }
            }
            None => {
                for vpn in vma.inner.vpn_range {
                    let ppn = vma.map_one(&mut self.page_table, vpn)?;
                    first_ppn.get_or_insert(ppn);
                }
            }
        }
        self.vmas
            .push(vma)
            .map_err(|_| (MemoryError::OutOfMemory, start_vpn))?;
        Ok(first_ppn)
    }
    /// other parts will be zeroed
    fn push_with_offset(
        &mut self,
        mut vma: Vma,
        offset: usize,
        data: &[u8],
    ) -> Result<(), (MemoryError, VirtPageNum)> {
        let start_vpn = vma.inner.vpn_range.get_start();
        self.vmas
            .try_reserve(1)
            .map_err(|_| (MemoryError::OutOfMemory, start_vpn))?;
        let len = data.len();
        let mut vpn_iter = vma.inner.vpn_range.into_iter();
        if let Some(vpn) = vpn_iter.next() {
            // special treatment for first page
            let first_ppn = vma.map_one(&mut self.page_table, vpn)?;
            let first_dst = first_ppn.get_bytes_array();
            first_dst[..offset].fill(0);
            let first_src = &data[..len.min(PAGE_SIZE - offset)];
            first_dst[offset..offset + first_src.len()].copy_from_slice(first_src);

            let mut start = PAGE_SIZE - offset;
            for vpn in vpn_iter {
                let ppn = vma.map_one(&mut self.page_table, vpn)?;
                let dst = ppn.get_bytes_array();
                let end = start + PAGE_SIZE;
                if start < len {
                    if len >= end {
                        let src = &data[start..end];
                        dst[..src.len()].copy_from_slice(src);
                    } else {
                        let src = &data[start..len];
                        dst[..src.len()].copy_from_slice(src);
                        dst[src.len()..].fill(0);
                    }
                } else {
                    dst.fill(0);
                }
                start = end;
            }
        }
        self.vmas
            .push(vma)
            .map_err(|_| (MemoryError::OutOfMemory, start_vpn))?;
        Ok(())
    }

    /// Push the map area into the memory set without copying or allocation.
    pub fn push_no_alloc(&mut self, vma: Vma) -> Result<(), ()> {
        self.vmas.try_reserve(1).map_err(|_| ())?;
        let mut mapper = UserMapper::new(&mut self.page_table);
        for vpn in vma.inner.vpn_range {
            let frame = vma.inner.get_in_memory(&vpn).unwrap();
            if !mapper.is_mapped(vpn) {
                //if not mapped
                mapper
                    .map_user_page(vpn, frame.ppn.clone(), vma.map_perm)
                    .map_err(|_| ())?;
            } else {
                return Err(());
            }
        }
        self.vmas.push(vma).map_err(|_| ())?;
        Ok(())
    }
    pub fn contains_valid_buffer(&self, buf: usize, size: usize, perm: MapPermission) -> bool {
        let Some(end) = buf.checked_add(size) else {
            return false;
        };
        if self.heap_bottom != 0 {
            let heap_limit = self.heap_bottom.saturating_add(USER_HEAP_SIZE);
            if buf < heap_limit && end > self.heap_pt && end > self.heap_bottom {
                return false;
            }
        }
        let start_vpn = VirtAddr::from(buf).floor();
        let end_vpn = VirtAddr::from(end).ceil();
        self.vmas
            .iter()
            .find(|area| {
                // If there is such a page in user space, and the addr is in the vpn range
                area.map_perm.contains(perm | MapPermission::U)
                    && area.get_start::<T>() <= start_vpn
                    && end_vpn <= area.get_end::<T>()
            })
            .is_some()
    }

    pub fn validate_msync_range(
        &self,
        addr: usize,
        len: usize,
        invalidate: bool,
    ) -> Result<(), isize> {
        if len == 0 {
            return Ok(());
        }
        let rounded_len = len
            .checked_add(PAGE_SIZE - 1)
            .map(|len| len & !(PAGE_SIZE - 1))
            .ok_or(ENOMEM)?;
        let end = addr.checked_add(rounded_len).ok_or(ENOMEM)?;
        if end > USER_VA_END {
            return Err(ENOMEM);
        }
        if self.heap_bottom != 0 {
            let heap_limit = self.heap_bottom.saturating_add(USER_HEAP_SIZE);
            if addr < heap_limit && end > self.heap_pt && end > self.heap_bottom {
                return Err(ENOMEM);
            }
        }
        let start_vpn = VirtAddr::from(addr).floor();
        let end_vpn = VirtAddr::from(end).ceil();
        if !self.vmas.covers_user_range(start_vpn, end_vpn) {
            return Err(ENOMEM);
        }
        if invalidate
            && self.vmas.iter().any(|area| {
                area.vm_is_user()
                    && area.vm_overlaps(start_vpn, end_vpn)
                    && area.flags.contains(MapFlags::MAP_LOCKED)
            })
        {
            return Err(EBUSY);
        }
        Ok(())
    }

    pub fn proc_maps_content(&self) -> String {
        let mut s = String::with_capacity(self.vmas.len() * 80);
        for vma in self.vmas.iter().filter(|vma| vma.vm_is_user()) {
            let start = vma.vm_start().0 * PAGE_SIZE;
            let end = vma.vm_end().0 * PAGE_SIZE;
            let perm = vma.vm_perm();
            let mapping = if vma.vm_mapping() == VmAreaMapping::Shared {
                's'
            } else {
                'p'
            };
            let _ = writeln!(
                s,
                "{:016x}-{:016x} {}{}{}{} {:08x} 00:00 0",
                start,
                end,
                if perm.contains(MapPermission::R) {
                    'r'
                } else {
                    '-'
                },
                if perm.contains(MapPermission::W) {
                    'w'
                } else {
                    '-'
                },
                if perm.contains(MapPermission::X) {
                    'x'
                } else {
                    '-'
                },
                mapping,
                vma.map_file_offset,
            );
        }
        s
    }

    fn write_proc_smaps_segment(
        s: &mut String,
        vma: &Vma,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
        locked_kb: usize,
    ) {
        let (size_kb, rss_kb) = Self::write_proc_smaps_header(s, vma, start_vpn, end_vpn);
        let _ = writeln!(s, "Size:           {:8} kB", size_kb);
        let _ = writeln!(s, "KernelPageSize: {:7} kB", PAGE_SIZE / 1024);
        let _ = writeln!(s, "MMUPageSize:    {:7} kB", PAGE_SIZE / 1024);
        let _ = writeln!(s, "Rss:            {:7} kB", rss_kb);
        let _ = writeln!(s, "Pss:            {:7} kB", rss_kb);
        let _ = writeln!(s, "Shared_Clean:         0 kB");
        let _ = writeln!(s, "Shared_Dirty:         0 kB");
        let _ = writeln!(s, "Private_Clean:        0 kB");
        let _ = writeln!(s, "Private_Dirty:  {:7} kB", rss_kb);
        let _ = writeln!(s, "Referenced:     {:7} kB", rss_kb);
        let _ = writeln!(s, "Anonymous:      {:7} kB", rss_kb);
        let _ = writeln!(s, "LazyFree:             0 kB");
        let _ = writeln!(s, "AnonHugePages:        0 kB");
        let _ = writeln!(s, "ShmemPmdMapped:       0 kB");
        let _ = writeln!(s, "FilePmdMapped:        0 kB");
        let _ = writeln!(s, "Shared_Hugetlb:       0 kB");
        let _ = writeln!(s, "Private_Hugetlb:      0 kB");
        let _ = writeln!(s, "Swap:                 0 kB");
        let _ = writeln!(s, "SwapPss:              0 kB");
        let _ = writeln!(s, "Locked:         {:7} kB", locked_kb);
        let _ = writeln!(s, "THPeligible:    0");
        let _ = writeln!(s, "VmFlags: rd wr mr mw me ac sd");
    }

    fn write_proc_smaps_header(
        s: &mut String,
        vma: &Vma,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
    ) -> (usize, usize) {
        let start = start_vpn.0 * PAGE_SIZE;
        let end = end_vpn.0 * PAGE_SIZE;
        let size_kb = (end - start) / 1024;
        let rss_pages = vma.inner.in_memory_len_in_range(start_vpn, end_vpn);
        let rss_kb = rss_pages * PAGE_SIZE / 1024;
        let perm = vma.vm_perm();
        let mapping = if vma.vm_mapping() == VmAreaMapping::Shared {
            's'
        } else {
            'p'
        };
        let _ = writeln!(
            s,
            "{:016x}-{:016x} {}{}{}{} {:08x} 00:00 0",
            start,
            end,
            if perm.contains(MapPermission::R) {
                'r'
            } else {
                '-'
            },
            if perm.contains(MapPermission::W) {
                'w'
            } else {
                '-'
            },
            if perm.contains(MapPermission::X) {
                'x'
            } else {
                '-'
            },
            mapping,
            vma.map_file_offset,
        );
        (size_kb, rss_kb)
    }

    fn write_proc_smaps_segment_compact(
        s: &mut String,
        vma: &Vma,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
    ) {
        let (size_kb, rss_kb) = Self::write_proc_smaps_header(s, vma, start_vpn, end_vpn);
        let _ = writeln!(s, "Size:           {:8} kB", size_kb);
        let _ = writeln!(s, "Rss:            {:7} kB", rss_kb);
        let _ = writeln!(s, "Pss:            {:7} kB", rss_kb);
        let _ = writeln!(s, "Private_Dirty:  {:7} kB", rss_kb);
        let _ = writeln!(s, "Referenced:     {:7} kB", rss_kb);
        let _ = writeln!(s, "Anonymous:      {:7} kB", rss_kb);
        let _ = writeln!(s, "Locked:               0 kB");
        let _ = writeln!(s, "VmFlags: rd wr mr mw me ac sd");
    }

    fn copy_proc_smaps_window(
        buf: &mut [u8],
        copied: &mut usize,
        emitted: &mut usize,
        offset: usize,
        limit: usize,
        segment: &str,
    ) -> bool {
        let start = *emitted;
        let end = start.saturating_add(segment.len());
        *emitted = end;

        if end <= offset {
            return false;
        }
        if start >= limit {
            return true;
        }

        let bytes = segment.as_bytes();
        let src_start = offset.saturating_sub(start);
        let src_end = limit.min(end).saturating_sub(start).min(bytes.len());
        if src_start >= src_end {
            return false;
        }

        let room = buf.len().saturating_sub(*copied);
        let copy_len = (src_end - src_start).min(room);
        if copy_len > 0 {
            buf[*copied..*copied + copy_len]
                .copy_from_slice(&bytes[src_start..src_start + copy_len]);
            *copied += copy_len;
        }
        *copied >= buf.len()
    }

    pub fn proc_smaps_read(&self, offset: usize, len: usize, buf: &mut [u8]) -> usize {
        let want = len.min(buf.len());
        if want == 0 {
            return 0;
        }

        let limit = offset.saturating_add(want);
        let user_vma_count = self.vmas.iter().filter(|vma| vma.vm_is_user()).count();
        let compact = self.locked_pages.is_empty() && user_vma_count >= PROC_SMAPS_DENSE_VMA_THRESHOLD;
        let entry_estimate = if compact {
            PROC_SMAPS_COMPACT_ENTRY_ESTIMATE
        } else {
            PROC_SMAPS_FULL_ENTRY_ESTIMATE
        };
        let mut segment = String::with_capacity(entry_estimate);
        let mut emitted = 0;
        let mut copied = 0;

        if compact {
            for vma in self.vmas.iter().filter(|vma| vma.vm_is_user()) {
                segment.clear();
                Self::write_proc_smaps_segment_compact(
                    &mut segment,
                    vma,
                    vma.vm_start(),
                    vma.vm_end(),
                );
                if Self::copy_proc_smaps_window(
                    buf,
                    &mut copied,
                    &mut emitted,
                    offset,
                    limit,
                    &segment,
                ) {
                    return copied;
                }
            }
            return copied;
        }

        if self.locked_pages.is_empty() {
            for vma in self.vmas.iter().filter(|vma| vma.vm_is_user()) {
                segment.clear();
                Self::write_proc_smaps_segment(&mut segment, vma, vma.vm_start(), vma.vm_end(), 0);
                if Self::copy_proc_smaps_window(
                    buf,
                    &mut copied,
                    &mut emitted,
                    offset,
                    limit,
                    &segment,
                ) {
                    return copied;
                }
            }
            return copied;
        }

        for vma in self.vmas.iter().filter(|vma| vma.vm_is_user()) {
            let mut segment_start = vma.vm_start();
            let end_vpn = vma.vm_end();
            while segment_start < end_vpn {
                let segment_locked = self.locked_pages.contains(&segment_start);
                let mut segment_end = VirtPageNum(segment_start.0 + 1);
                while segment_end < end_vpn
                    && self.locked_pages.contains(&segment_end) == segment_locked
                {
                    segment_end.0 += 1;
                }
                let locked_pages = if segment_locked {
                    segment_end.0 - segment_start.0
                } else {
                    0
                };
                let locked_kb = locked_pages * PAGE_SIZE / 1024;
                segment.clear();
                Self::write_proc_smaps_segment(
                    &mut segment,
                    vma,
                    segment_start,
                    segment_end,
                    locked_kb,
                );
                if Self::copy_proc_smaps_window(
                    buf,
                    &mut copied,
                    &mut emitted,
                    offset,
                    limit,
                    &segment,
                ) {
                    return copied;
                }
                segment_start = segment_end;
            }
        }
        copied
    }

    pub fn proc_smaps_content(&self) -> String {
        let user_vma_count = self.vmas.iter().filter(|vma| vma.vm_is_user()).count();
        if self.locked_pages.is_empty() && user_vma_count >= PROC_SMAPS_DENSE_VMA_THRESHOLD {
            let mut s = String::with_capacity(user_vma_count * PROC_SMAPS_COMPACT_ENTRY_ESTIMATE);
            for vma in self.vmas.iter().filter(|vma| vma.vm_is_user()) {
                Self::write_proc_smaps_segment_compact(&mut s, vma, vma.vm_start(), vma.vm_end());
            }
            return s;
        }

        let mut s = String::with_capacity(user_vma_count * PROC_SMAPS_FULL_ENTRY_ESTIMATE);
        if self.locked_pages.is_empty() {
            for vma in self.vmas.iter().filter(|vma| vma.vm_is_user()) {
                Self::write_proc_smaps_segment(&mut s, vma, vma.vm_start(), vma.vm_end(), 0);
            }
            return s;
        }
        for vma in self.vmas.iter().filter(|vma| vma.vm_is_user()) {
            let mut segment_start = vma.vm_start();
            let end_vpn = vma.vm_end();
            while segment_start < end_vpn {
                let segment_locked = self.locked_pages.contains(&segment_start);
                let mut segment_end = VirtPageNum(segment_start.0 + 1);
                while segment_end < end_vpn
                    && self.locked_pages.contains(&segment_end) == segment_locked
                {
                    segment_end.0 += 1;
                }
                let locked_pages = if segment_locked {
                    segment_end.0 - segment_start.0
                } else {
                    0
                };
                let locked_kb = locked_pages * PAGE_SIZE / 1024;
                Self::write_proc_smaps_segment(&mut s, vma, segment_start, segment_end, locked_kb);
                segment_start = segment_end;
            }
        }
        s
    }
    /// The REAL handler to page fault.
    /// Handles all types of page fault:(In regex:) "(Store|Load|Instruction)(Page)?Fault"
    /// Checks the permission to decide whether to copy.
    pub fn do_page_fault(
        &mut self,
        addr: VirtAddr,
        access: FaultAccess,
    ) -> Result<PhysAddr, MemoryError> {
        let vpn = addr.floor();
        let area_start = match self.vmas.find_user_vma_key(vpn) {
            Some(start) => Some(start),
            None => self.vmas.expand_growsdown_for_fault(vpn)?,
        };
        if area_start.is_some() {
            let ctx = super::page_fault::FaultContext::new(addr, access);
            let page_table = &mut self.page_table;
            let area = self.vmas.find_user_vma_mut(vpn).unwrap();
            let pa = super::page_fault::handle_page_fault(area, page_table, ctx)?;
            self.validate_fault_phys_addr(addr, pa)
        } else {
            // In all segments, nothing matches the requirements. Throws.
            error!("[do_page_fault] addr: {:?}, result: bad addr", addr);
            Err(MemoryError::BadAddress)
        }
    }

    /// Fault in one user VA and return the verified physical address.
    ///
    /// This is the single uaccess-facing contract: after success, the VA must
    /// have a valid user PTE, required access permission, and a physical address
    /// inside real memory.
    pub fn fault_in_user_va(
        &mut self,
        addr: VirtAddr,
        access: FaultAccess,
    ) -> Result<PhysAddr, isize> {
        super::frame_reserve(3);
        self.do_page_fault(addr, access)
            .and_then(|_| self.validate_user_fault_result(addr, access))
            .map_err(memory_error_to_errno)
    }

    pub fn fault_in_trap_va(
        &mut self,
        addr: VirtAddr,
        access: FaultAccess,
    ) -> Result<PhysAddr, isize> {
        super::frame_reserve(3);
        self.do_page_fault(addr, access)
            .map_err(memory_error_to_errno)
    }

    fn validate_fault_phys_addr(
        &self,
        addr: VirtAddr,
        pa: PhysAddr,
    ) -> Result<PhysAddr, MemoryError> {
        if pa.0 < MEMORY_START || pa.0 >= MEMORY_END {
            warn!(
                "[fault_in] translated user va {:#x} to invalid pa {:#x}",
                addr.0, pa.0
            );
            return Err(MemoryError::BadAddress);
        }
        Ok(pa)
    }

    fn validate_user_fault_result(
        &self,
        addr: VirtAddr,
        access: FaultAccess,
    ) -> Result<PhysAddr, MemoryError> {
        let vpn = addr.floor();
        let pa = self
            .page_table
            .translate_va(addr)
            .ok_or(MemoryError::NotMapped)?;

        self.validate_fault_phys_addr(addr, pa)?;

        let ok = match access {
            FaultAccess::Load => self
                .page_table
                .user_access_ok(vpn, UserAccess::Read)
                .unwrap_or(false),
            FaultAccess::Store => self
                .page_table
                .user_access_ok(vpn, UserAccess::Write)
                .unwrap_or(false),
            FaultAccess::Execute => {
                self.page_table.is_valid(vpn).unwrap_or(false)
                    && self.page_table.executable(vpn).unwrap_or(false)
            }
        };

        if !ok {
            warn!(
                "[fault_in] user va {:#x} failed post-fault permission check: {:?}",
                addr.0, access
            );
            return Err(MemoryError::NoPermission);
        }

        Ok(pa)
    }
    #[cfg(feature = "loongarch64")]
    #[cfg(feature = "oom_handler")]
    pub fn do_shallow_clean(&mut self) -> usize {
        let page_table = &mut self.page_table;
        self.vmas
            .iter_mut()
            .filter(|area| {
                let start_vpn = area.get_start::<T>();
                start_vpn.0 >= (USR_MMAP_BASE >> PAGE_SIZE_BITS)
                    && start_vpn.0 < (USR_MMAP_END >> PAGE_SIZE_BITS)
                    && area.map_file.is_none()
            })
            .map(|area| area.do_oom(page_table))
            .sum()
    }
    #[cfg(feature = "riscv")]
    #[cfg(feature = "oom_handler")]
    pub fn do_shallow_clean(&mut self) -> usize {
        let page_table = &mut self.page_table;
        self.vmas
            .iter_mut()
            .filter(|area| {
                let start_vpn = area.get_start::<T>();
                start_vpn.0 >= (MMAP_BASE >> PAGE_SIZE_BITS)
                    && start_vpn.0 < (TASK_SIZE >> PAGE_SIZE_BITS)
                    && area.map_file.is_none()
            })
            .map(|area| area.do_oom(page_table))
            .sum()
    }
    #[cfg(feature = "loongarch64")]
    #[cfg(feature = "oom_handler")]
    pub fn do_deep_clean(&mut self) -> usize {
        let page_table = &mut self.page_table;
        self.vmas
            .iter_mut()
            .filter(|area| {
                area.get_start::<T>().0 < (USER_VA_END >> PAGE_SIZE_BITS) && area.map_file.is_none()
            })
            .map(|area| {
                if area.get_start::<T>().0 < USR_MMAP_BASE >> PAGE_SIZE_BITS {
                    area.force_swap(page_table)
                } else {
                    area.do_oom(page_table)
                }
            })
            .sum()
    }
    #[cfg(feature = "riscv")]
    #[cfg(feature = "oom_handler")]
    pub fn do_deep_clean(&mut self) -> usize {
        let page_table = &mut self.page_table;
        self.vmas
            .iter_mut()
            .filter(|area| {
                area.get_start::<T>().0 < (TASK_SIZE >> PAGE_SIZE_BITS) && area.map_file.is_none()
            })
            .map(|area| {
                if area.get_start::<T>().0 < MMAP_BASE >> PAGE_SIZE_BITS {
                    area.force_swap(page_table)
                } else {
                    area.do_oom(page_table)
                }
            })
            .sum()
    }
    /// Mention that trampoline is not collected by areas.
    fn map_trampoline(&mut self) {
        UserMapper::new(&mut self.page_table)
            .map_privileged_user_page(
                VirtAddr::from(TRAMPOLINE).into(),
                PhysAddr::from(strampoline as usize).into(),
                MapPermission::R | MapPermission::X,
            )
            .unwrap();
    }

    /// Can be accessed in user mode.
    fn map_signaltrampoline(&mut self) {
        UserMapper::new(&mut self.page_table)
            .map_user_page(
                VirtAddr::from(SIGNAL_TRAMPOLINE).into(),
                PhysAddr::from(ssignaltrampoline as usize).into(),
                MapPermission::R | MapPermission::X | MapPermission::U,
            )
            .unwrap();
    }

    pub fn map_elf(&mut self, elf: &xmas_elf::ElfFile) -> Result<(usize, ELFInfo), isize> {
        let bias = match elf.header.pt2.type_().as_type() {
            // static
            xmas_elf::header::Type::Executable => 0,
            xmas_elf::header::Type::SharedObject => {
                match elf
                    .program_iter()
                    .filter(|ph| ph.get_type().unwrap() == xmas_elf::program::Type::Interp)
                    .count()
                {
                    // It's a loader!
                    0 => ELF_DYN_BASE,
                    // It's a dynamically linked PIE main executable. Keep it
                    // away from the NULL page so mprotect(0, ...) still
                    // follows normal unmapped-range ENOMEM semantics.
                    1 => ELF_PIE_BASE,
                    // Emmm, It has multiple interpreters.
                    _ => return Err(EINVAL),
                }
            }
            _ => return Err(ENOEXEC),
        };

        let mut program_break: Option<usize> = None;
        let mut interp_entry: Option<usize> = None;
        let mut interp_base: Option<usize> = None;
        let mut load_addr: Option<usize> = None; // top va of ELF which points to ELF header

        for ph in elf.program_iter() {
            // Map only when the sections that is to be loaded.
            match ph.get_type().unwrap() {
                xmas_elf::program::Type::Load => {
                    // 防御性检查：拒绝超大段（> 1GB），避免 ELF 加载路径一次性映射过大。
                    if ph.mem_size() as usize > 1024 * 1024 * 1024 {
                        log::error!("[map_elf] Segment too large: {} bytes", ph.mem_size());
                        return Err(ENOMEM);
                    }
                    let start_va: VirtAddr = (ph.virtual_addr() as usize + bias).into();
                    let end_va: VirtAddr =
                        ((ph.virtual_addr() + ph.mem_size()) as usize + bias).into();
                    let start_va_page_offset = start_va.page_offset();

                    let map_perm = MapPermission::from_ph_flags(ph.flags());
                    if load_addr.is_none() {
                        load_addr = Some(start_va.into());
                    }
                    let mut vma = match Vma::try_new(start_va, end_va, map_perm, None, 0) {
                        Ok(area) => area,
                        Err(e) => return Err(e),
                    };
                    vma.flags = MapFlags::MAP_PRIVATE;
                    let file_offset = ph.offset() as usize;
                    let file_size = ph.file_size() as usize;
                    let file_end = file_offset.checked_add(file_size).ok_or(ENOEXEC)?;
                    let segment_data = elf.input.get(file_offset..file_end).ok_or(ENOEXEC)?;
                    let vma_page_count = vma.get_end::<T>().0 - vma.get_start::<T>().0;
                    let file_page_count = VirtAddr::from(file_size).ceil().0;
                    let can_fast_map_readonly_segment = start_va_page_offset == 0
                        && (file_offset & (PAGE_SIZE - 1)) == 0
                        && file_size != 0
                        && !map_perm.contains(MapPermission::W)
                        && file_page_count == vma_page_count;

                    if can_fast_map_readonly_segment {
                        let kernel_start_vpn =
                            (VirtAddr::from(elf.input.as_ptr() as usize + file_offset)).floor();
                        if vma
                            .map_from_kernel_area(&mut self.page_table, kernel_start_vpn)
                            .is_ok()
                        {
                            self.vmas.push(vma).map_err(|_| ENOMEM)?;
                        } else {
                            warn!(
                                "[map_elf] readonly segment fast map failed; falling back to copy load: va={:#x}, file_offset={:#x}, file_size={:#x}",
                                start_va.0,
                                file_offset,
                                file_size
                            );
                            if let Err((err, vpn)) =
                                self.push_with_offset(vma, start_va_page_offset, segment_data)
                            {
                                error!(
                                    "[map_elf] fallback copy load failed: err={:?}, vpn={:?}",
                                    err, vpn
                                );
                                return Err(match err {
                                    MemoryError::OutOfMemory => ENOMEM,
                                    _ => ENOEXEC,
                                });
                            }
                        }
                    } else {
                        if let Err((err, vpn)) =
                            self.push_with_offset(vma, start_va_page_offset, segment_data)
                        {
                            error!("[map_elf] copy load failed: err={:?}, vpn={:?}", err, vpn);
                            return Err(match err {
                                MemoryError::OutOfMemory => ENOMEM,
                                _ => ENOEXEC,
                            });
                        };
                    }
                    let segment_end = VirtAddr::from(end_va.ceil()).0;
                    program_break =
                        Some(program_break.map_or(segment_end, |brk| brk.max(segment_end)));
                }
                xmas_elf::program::Type::Interp => {
                    //assert!(elf.input[(ph.offset() + ph.file_size()) as usize] == b'\0');
                    let path = String::from_utf8_lossy(
                        &elf.input
                            [ph.offset() as usize..(ph.offset() + ph.file_size() - 1) as usize],
                    );
                    let interp_data = crate::task::load_elf_interp(&path)?;
                    let interp = xmas_elf::ElfFile::new(interp_data).map_err(|_| ENOEXEC)?;
                    let (_, interp_info) = self.map_elf(&interp)?;
                    interp_entry = Some(interp_info.entry);
                    interp_base = Some(interp_info.base);
                    KERNEL_SPACE
                        .lock()
                        .remove_area_with_start_vpn(
                            VirtAddr::from(interp_data.as_ptr() as usize).ceil(),
                        )
                        .unwrap();
                }
                _ => {}
            }
        }
        match (program_break, load_addr) {
            (Some(program_break), Some(load_addr)) => Ok((
                program_break,
                ELFInfo {
                    entry: elf.header.pt2.entry_point() as usize + bias,
                    interp_entry,
                    base: if let Some(interp_base) = interp_base {
                        interp_base
                    } else {
                        bias
                    },
                    phnum: elf.header.pt2.ph_count() as usize,
                    phent: elf.header.pt2.ph_entry_size() as usize,
                    phdr: load_addr + elf.header.pt2.ph_offset() as usize,
                },
            )),
            _ => Err(ENOEXEC),
        }
    }
    /// Include sections in elf and trampoline and TrapContext and user stack,
    /// also returns user_sp and entry point.
    pub fn from_elf(elf_data: &[u8]) -> Result<(Self, usize, ELFInfo), isize> {
        let mut address_space = Self::new_bare();
        // map trampoline
        if should_map_trampoline!() {
            address_space.map_trampoline();
        }
        // map signaltrampoline
        address_space.map_signaltrampoline();
        let elf = xmas_elf::ElfFile::new(elf_data).map_err(|_| {
            log::warn!(
                "[from_elf] invalid ELF: {} bytes, first 16: {:02x?}",
                elf_data.len(),
                &elf_data[..16.min(elf_data.len())]
            );
            ENOEXEC
        })?;
        let (program_break, elf_info) = address_space.map_elf(&elf)?;
        address_space.heap_bottom = program_break;
        address_space.heap_pt = program_break;

        Ok((address_space, program_break, elf_info))
    }
    pub fn from_existing_user(
        user_space: &mut AddressSpace<T>,
        trap_cx_slot: usize,
        trap_cx: &TrapContext,
    ) -> Result<AddressSpace<T>, isize> {
        let mut address_space = Self::new_bare();
        // map trampoline
        if should_map_trampoline!() {
            address_space.map_trampoline();
        }
        // map signaltrampoline
        address_space.map_signaltrampoline();
        address_space.heap_bottom = user_space.heap_bottom;
        address_space.heap_pt = user_space.heap_pt;
        // map data sections/user heap/mmap area/user stack
        if address_space
            .vmas
            .try_reserve(user_space.vmas.len())
            .is_err()
        {
            return Err(crate::syscall::errno::ENOMEM);
        }
        for area in user_space
            .vmas
            .iter()
            .filter(|area| area.vm_is_user() && !area.dont_fork)
        {
            let mut new_area = if area.wipe_on_fork {
                Vma::from_another(area)
            } else {
                let mut cloned = area.try_clone()?;
                cloned
                    .map_from_existing_page_table(
                        &mut address_space.page_table,
                        &mut user_space.page_table,
                    )
                    .map_err(|_| crate::syscall::errno::ENOMEM)?;
                cloned
            };
            new_area.mark_fork_inherited();
            address_space.vmas.push(new_area)?;
        }
        // Copy the current task's trap context.  A process can have stale or
        // higher-numbered non-user VMAs after clone/exit churn, so do not guess
        // with last_non_user().
        let trap_cx_vpn: VirtPageNum =
            VirtAddr::from(trap_cx_bottom_from_slot(trap_cx_slot)).into();
        let trap_cx_area = user_space
            .vmas
            .get_by_start(trap_cx_vpn)
            .filter(|area| !area.vm_is_user())
            .ok_or(crate::syscall::errno::EINVAL)?;
        let area = Vma::from_another(trap_cx_area);
        let trap_cx_data = unsafe {
            core::slice::from_raw_parts(
                (trap_cx as *const TrapContext).cast::<u8>(),
                core::mem::size_of::<TrapContext>(),
            )
        };
        address_space
            .push(area, Some(trap_cx_data))
            .map_err(|_| crate::syscall::errno::ENOMEM)?;

        Ok(address_space)
    }
    pub fn activate(&self) {
        self.page_table.activate()
    }
    /// Translate the `vpn` into its corresponding `Some(PageTableEntry)` in the current memory set if exists
    /// `None` is returned if nothing is found.
    pub fn translate(&self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        translate_page(&self.page_table, vpn)
    }

    /// Return whether a non-private futex at `addr` must use the global shared key table.
    ///
    /// Linux uses an mm/address key for private mappings even when userspace does not pass
    /// FUTEX_PRIVATE_FLAG; only mappings that are actually shared need an object/page based key.
    pub fn futex_uses_shared_key(&self, addr: VirtAddr) -> Result<bool, isize> {
        let vpn = addr.floor();
        let start = self.vmas.find_user_vma_key(vpn).ok_or(EFAULT)?;
        let area = self.vmas.get_by_start(start).ok_or(EFAULT)?;
        Ok(area.vm_mapping_type() == VmAreaMapping::Shared)
    }

    pub fn recycle_data_pages(&mut self) {
        //*self = Self::new_bare();
        self.vmas.clear();
        self.locked_pages.clear();
    }

    /// Release all resources for a zombie process: VMA metadata, page table
    /// frames, and backing Vec storage.  The zombie no longer needs address
    /// space after exit; only wait4 metadata (pid, exit_code) is required.
    pub fn release_for_zombie(&mut self) {
        self.vmas.clear_no_hole();
        self.locked_pages.clear();
        self.page_table.release_frames();
    }
    pub fn sbrk(&mut self, increment: isize) -> usize {
        super::mmap::do_sbrk(self, increment)
    }

    pub fn mmap(
        &mut self,
        start: usize,
        len: usize,
        prot: MapPermission,
        flags: MapFlags,
        offset: usize,
        map_file: Option<Arc<dyn IndexNode>>,
        may_write: bool,
        write_sealed: bool,
    ) -> isize {
        super::mmap::do_mmap(
            self,
            start,
            len,
            prot,
            flags,
            offset,
            map_file,
            may_write,
            write_sealed,
        )
    }

    pub fn shm_mmap(
        &mut self,
        start: usize,
        len: usize,
        prot: MapPermission,
        flags: MapFlags,
        frames: &[Arc<FrameTracker>],
        may_write: bool,
    ) -> isize {
        super::mmap::do_shm_mmap(self, start, len, prot, flags, frames, may_write)
    }

    pub fn munmap(&mut self, start: usize, len: usize) -> Result<(), isize> {
        let result = super::mmap::do_munmap(self, start, len);
        if result.is_ok() {
            if let Some(end) = start.checked_add(len) {
                let start_vpn = VirtAddr::from(start).floor();
                let end_vpn = VirtAddr::from(end).ceil();
                self.set_locked_pages(start_vpn, end_vpn, false);
            }
        }
        result
    }

    pub fn mprotect(&mut self, addr: usize, len: usize, prot: MapPermission) -> Result<(), isize> {
        super::mmap::do_mprotect(self, addr, len, prot)
    }

    pub fn madvise(&mut self, start: usize, len: usize, advice: usize) -> Result<(), isize> {
        const MADV_DONTNEED: usize = 4;

        let start_vpn = VirtAddr::from(start).floor();
        let end_vpn = VirtAddr::from(start + len).ceil();
        if advice == MADV_DONTNEED && self.locked_pages.range(start_vpn..end_vpn).next().is_some() {
            return Err(EINVAL);
        }
        self.vmas
            .advise_range(&mut self.page_table, start_vpn, end_vpn, advice)
    }

    pub fn mincore(&self, start: usize, len: usize, residency: &mut [u8]) -> Result<(), isize> {
        let end = start.checked_add(len).ok_or(ENOMEM)?;
        let start_vpn = VirtAddr::from(start).floor();
        let end_vpn = VirtAddr::from(end).ceil();
        self.vmas
            .mincore_range(&self.page_table, start_vpn, end_vpn, residency)
    }

    fn user_lock_range(
        &self,
        start: usize,
        len: usize,
    ) -> Result<(VirtPageNum, VirtPageNum, usize), isize> {
        if len == 0 {
            return Ok((VirtPageNum(0), VirtPageNum(0), 0));
        }
        let end = start.checked_add(len).ok_or(ENOMEM)?;
        if end > USER_VA_END {
            return Err(ENOMEM);
        }
        let start_vpn = VirtAddr::from(start).floor();
        let end_vpn = VirtAddr::from(end).ceil();
        if !self.vmas.covers_user_range(start_vpn, end_vpn) {
            return Err(ENOMEM);
        }
        let locked_len = (end_vpn.0 - start_vpn.0).saturating_mul(PAGE_SIZE);
        Ok((start_vpn, end_vpn, locked_len))
    }

    pub(super) fn set_locked_pages(
        &mut self,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
        locked: bool,
    ) -> usize {
        let mut vpn = start_vpn;
        let mut page_count = 0usize;
        while vpn < end_vpn {
            if locked {
                self.locked_pages.insert(vpn);
            } else {
                self.locked_pages.remove(&vpn);
            }
            page_count = page_count.saturating_add(1);
            vpn.0 += 1;
        }
        page_count.saturating_mul(PAGE_SIZE)
    }

    pub fn mlock(&mut self, start: usize, len: usize) -> Result<usize, isize> {
        let (start_vpn, end_vpn, locked_len) = self.user_lock_range(start, len)?;
        let mut vpn = start_vpn;
        while vpn < end_vpn {
            self.fault_in_user_va(VirtAddr::from(vpn), FaultAccess::Load)?;
            vpn.0 += 1;
        }
        self.set_locked_pages(start_vpn, end_vpn, true);
        Ok(locked_len)
    }

    pub fn mlock_onfault(&mut self, start: usize, len: usize) -> Result<usize, isize> {
        let (start_vpn, end_vpn, locked_len) = self.user_lock_range(start, len)?;
        self.set_locked_pages(start_vpn, end_vpn, true);
        Ok(locked_len)
    }

    pub fn munlock(&mut self, start: usize, len: usize) -> Result<(), isize> {
        let (start_vpn, end_vpn, _) = self.user_lock_range(start, len)?;
        self.set_locked_pages(start_vpn, end_vpn, false);
        Ok(())
    }

    pub fn user_mapped_bytes(&self) -> usize {
        self.vmas.user_mapped_bytes()
    }

    pub fn resident_user_bytes(&self) -> usize {
        let resident_pages = self
            .vmas
            .iter()
            .filter(|vma| vma.vm_is_user())
            .map(|vma| vma.inner.in_memory_len())
            .fold(0usize, |acc, pages| acc.saturating_add(pages));
        resident_pages.saturating_mul(PAGE_SIZE)
    }

    pub fn locked_user_bytes(&self) -> usize {
        self.locked_pages.len().saturating_mul(PAGE_SIZE)
    }

    pub fn mlockall_current(&mut self) -> usize {
        let mut locked_len = 0usize;
        let ranges: Vec<(VirtPageNum, VirtPageNum)> = self
            .vmas
            .iter()
            .filter(|area| area.vm_is_user())
            .map(|area| (area.vm_start(), area.vm_end()))
            .collect();
        for (start_vpn, end_vpn) in ranges {
            locked_len = locked_len.saturating_add(self.set_locked_pages(start_vpn, end_vpn, true));
        }
        locked_len
    }

    pub fn munlockall(&mut self) {
        self.locked_pages.clear();
    }

    pub fn create_elf_tables(
        &self,
        mut user_sp: usize,
        argv_vec: &Vec<String>,
        envp_vec: &Vec<String>,
        elf_info: &ELFInfo,
    ) -> Result<usize, isize> {
        fn push_stack(sp: &mut usize, stack_bottom: usize, size: usize) -> Result<usize, isize> {
            *sp = sp.checked_sub(size).ok_or(E2BIG)?;
            if *sp < stack_bottom {
                return Err(E2BIG);
            }
            Ok(*sp)
        }

        fn align_stack(sp: &mut usize, stack_bottom: usize) -> Result<(), isize> {
            *sp &= !(core::mem::size_of::<usize>() - 1);
            if *sp < stack_bottom {
                return Err(E2BIG);
            }
            Ok(())
        }

        fn write_user_bytes<T: PageTable>(
            page_table: &T,
            mut dst: usize,
            mut src: &[u8],
        ) -> Result<(), isize> {
            while !src.is_empty() {
                let pa = page_table.translate_va(VirtAddr::from(dst)).ok_or(EFAULT)?;
                let page_offset = pa.page_offset();
                let copy_len = (PAGE_SIZE - page_offset).min(src.len());
                let page = pa.floor().get_bytes_array();
                page[page_offset..page_offset + copy_len].copy_from_slice(&src[..copy_len]);
                dst = dst.checked_add(copy_len).ok_or(EFAULT)?;
                src = &src[copy_len..];
            }
            Ok(())
        }

        fn write_user_slice<T: PageTable, U: Copy>(
            page_table: &T,
            dst: usize,
            src: &[U],
        ) -> Result<(), isize> {
            let bytes = core::mem::size_of::<U>()
                .checked_mul(src.len())
                .ok_or(E2BIG)?;
            if bytes == 0 {
                return Ok(());
            }
            let src = unsafe { core::slice::from_raw_parts(src.as_ptr() as *const u8, bytes) };
            write_user_bytes(page_table, dst, src)
        }

        let stack_bottom = user_sp.checked_sub(USER_STACK_SIZE).ok_or(E2BIG)?;
        // Keep the top guard gap used by the old startup ABI.
        user_sp = user_sp
            .checked_sub(2 * core::mem::size_of::<usize>())
            .ok_or(E2BIG)?;
        if user_sp < stack_bottom {
            return Err(E2BIG);
        }

        // we don't care about the order of env...
        let mut envp_user = Vec::<*const u8>::new();
        if envp_user
            .try_reserve(envp_vec.len().saturating_add(1))
            .is_err()
        {
            return Err(crate::syscall::errno::ENOMEM);
        }
        for env in envp_vec.iter() {
            let len = env.len().checked_add(1).ok_or(E2BIG)?;
            let dst = push_stack(&mut user_sp, stack_bottom, len)?;
            envp_user.push(dst as *const u8);
            write_user_bytes(&self.page_table, dst, env.as_bytes())?;
            write_user_bytes(&self.page_table, dst + env.len(), b"\0")?;
        }
        envp_user.push(core::ptr::null());

        // we don't care about the order of arg, too...
        let mut argv_user = Vec::<*const u8>::new();
        if argv_user
            .try_reserve(argv_vec.len().saturating_add(1))
            .is_err()
        {
            return Err(crate::syscall::errno::ENOMEM);
        }
        for arg in argv_vec.iter() {
            let len = arg.len().checked_add(1).ok_or(E2BIG)?;
            let dst = push_stack(&mut user_sp, stack_bottom, len)?;
            argv_user.push(dst as *const u8);
            write_user_bytes(&self.page_table, dst, arg.as_bytes())?;
            write_user_bytes(&self.page_table, dst + arg.len(), b"\0")?;
        }
        argv_user.push(core::ptr::null());
        // align downward to usize (64bit)
        align_stack(&mut user_sp, stack_bottom)?;

        // 16 random bytes
        let random_bits = [0xdeadbeefcafebabeusize, 0xdeadbeefcafebabeusize];
        let random_bits_ptr = push_stack(
            &mut user_sp,
            stack_bottom,
            random_bits.len() * core::mem::size_of::<usize>(),
        )?;
        write_user_slice(&self.page_table, random_bits_ptr, &random_bits)?;
        // padding
        let zero = 0usize;
        let padding_ptr = push_stack(&mut user_sp, stack_bottom, core::mem::size_of::<usize>())?;
        write_user_slice(&self.page_table, padding_ptr, core::slice::from_ref(&zero))?;
        let auxv = [
            // AuxvEntry::new(AuxvType::SYSINFO_EHDR, vDSO_mapping);
            // AuxvEntry::new(AuxvType::L1I_CACHESIZE, 0);
            // AuxvEntry::new(AuxvType::L1I_CACHEGEOMETRY, 0);
            // AuxvEntry::new(AuxvType::L1D_CACHESIZE, 0);
            // AuxvEntry::new(AuxvType::L1D_CACHEGEOMETRY, 0);
            // AuxvEntry::new(AuxvType::L2_CACHESIZE, 0);
            // AuxvEntry::new(AuxvType::L2_CACHEGEOMETRY, 0);
            // `0x112d` means IMADZifenciC, aka gc
            AuxvEntry::new(AuxvType::HWCAP, 0x112d),
            AuxvEntry::new(AuxvType::PAGESZ, PAGE_SIZE),
            AuxvEntry::new(AuxvType::CLKTCK, TICKS_PER_SEC),
            AuxvEntry::new(AuxvType::PHDR, elf_info.phdr),
            AuxvEntry::new(AuxvType::PHENT, elf_info.phent),
            AuxvEntry::new(AuxvType::PHNUM, elf_info.phnum),
            AuxvEntry::new(AuxvType::BASE, elf_info.base),
            AuxvEntry::new(AuxvType::FLAGS, 0),
            AuxvEntry::new(AuxvType::ENTRY, elf_info.entry),
            AuxvEntry::new(AuxvType::UID, 0),
            AuxvEntry::new(AuxvType::EUID, 0),
            AuxvEntry::new(AuxvType::GID, 0),
            AuxvEntry::new(AuxvType::EGID, 0),
            AuxvEntry::new(AuxvType::SECURE, 0),
            AuxvEntry::new(AuxvType::RANDOM, random_bits_ptr as usize),
            AuxvEntry::new(
                AuxvType::EXECFN,
                argv_user.first().copied().unwrap() as usize,
            ),
            AuxvEntry::new(AuxvType::NULL, 0),
        ];
        let auxv_ptr = push_stack(
            &mut user_sp,
            stack_bottom,
            auxv.len() * core::mem::size_of::<AuxvEntry>(),
        )?;
        write_user_slice(&self.page_table, auxv_ptr, auxv.as_slice())?;
        let envp_ptr = push_stack(
            &mut user_sp,
            stack_bottom,
            envp_user.len() * core::mem::size_of::<usize>(),
        )?;
        write_user_slice(&self.page_table, envp_ptr, envp_user.as_slice())?;
        let argv_ptr = push_stack(
            &mut user_sp,
            stack_bottom,
            argv_user.len() * core::mem::size_of::<usize>(),
        )?;
        write_user_slice(&self.page_table, argv_ptr, argv_user.as_slice())?;
        let argc_ptr = push_stack(&mut user_sp, stack_bottom, core::mem::size_of::<usize>())?;
        let argc = argv_vec.len();
        write_user_slice(&self.page_table, argc_ptr, core::slice::from_ref(&argc))?;

        // print user stack
        // let mut phys_addr = phys_user_sp & !0xf;
        // while phys_start >= phys_addr {
        //     trace!(
        //         "0x{:0>16X}:    {:0>16X}  {:0>16X}",
        //         phys_addr + virt_phys_offset,
        //         unsafe { *(phys_addr as *mut usize) },
        //         unsafe { *((phys_addr + core::mem::size_of::<usize>()) as *mut usize) }
        //     );
        //     phys_addr += 2 * core::mem::size_of::<usize>();
        // }
        Ok(user_sp)
    }
    pub fn alloc_user_res(&mut self, slot: usize, alloc_stack: bool) {
        self.alloc_user_res_with_trap_ppn(slot, alloc_stack)
            .unwrap_or_else(|err| {
                panic!(
                    "[alloc_user_res] failed to allocate user resources for slot {}: {:?}",
                    slot, err
                )
            });
    }

    pub fn alloc_user_res_with_trap_ppn(
        &mut self,
        slot: usize,
        alloc_stack: bool,
    ) -> Result<PhysPageNum, MemoryError> {
        if alloc_stack {
            let ustack_bottom = ustack_bottom_from_slot(slot);
            self.insert_user_stack_area(ustack_bottom.into())
                .map_err(|(err, _)| err)?;
        }
        // alloc trap_cx
        let trap_cx_bottom = trap_cx_bottom_from_slot(slot);
        let trap_cx_top = trap_cx_bottom + PAGE_SIZE;
        if let Some(ppn) = self.translate(VirtAddr::from(trap_cx_bottom).into()) {
            return Ok(ppn);
        }
        let trap_cx_ppn = self
            .insert_framed_area_first_ppn(
                trap_cx_bottom.into(),
                trap_cx_top.into(),
                MapPermission::R | MapPermission::W,
            )
            .map_err(|(err, _)| err)?;
        Ok(trap_cx_ppn)
    }

    pub fn dealloc_user_res(&mut self, slot: usize) {
        self.dealloc_user_res_with_stack(slot, true);
    }

    pub fn dealloc_user_res_with_stack(&mut self, slot: usize, dealloc_stack: bool) {
        self.dealloc_user_res_with_stack_inner(slot, dealloc_stack, false);
    }

    pub fn dealloc_user_res_keep_trap(&mut self, slot: usize, dealloc_stack: bool) {
        self.dealloc_user_res_with_stack_inner(slot, dealloc_stack, true);
    }

    fn dealloc_user_res_with_stack_inner(
        &mut self,
        slot: usize,
        dealloc_stack: bool,
        keep_trap: bool,
    ) {
        // dealloc ustack manually
        if dealloc_stack {
            let ustack_top_va: VirtAddr = (ustack_bottom_from_slot(slot) - USER_STACK_SIZE).into();
            if let Err(err) = self.remove_area_with_start_vpn(ustack_top_va.into()) {
                match err {
                    MemoryError::AreaNotFound => {
                        warn!(
                            "[dealloc_user_res] slot {}, user stack is not allocated",
                            slot
                        )
                    }
                    MemoryError::NotMapped => {
                        warn!(
                            "[dealloc_user_res] slot {}, user stack is partially unmapped, is it caused by oom?",
                            slot
                        )
                    }
                    _ => {} //忽略非致命错误
                }
            }
        }
        if keep_trap {
            return;
        }
        // 处理 trap_cx 回收
        let trap_cx_bottom_va: VirtAddr = trap_cx_bottom_from_slot(slot).into();

        // 改为这种写法，不再使用 unwrap()
        if let Err(err) = self.remove_area_with_start_vpn(trap_cx_bottom_va.into()) {
            match err {
                MemoryError::AreaNotFound => {
                    // 如果没找到该区域，可能是在之前的清理中整个 Area 都删了
                }
                MemoryError::NotMapped => {
                    // 如果页面已经不在页表里（被 OOM 换出），这在回收逻辑中是正常的
                }
                _ => {
                    // 其他错误也可以记录一下，但没必要 Panic 导致整个系统崩溃
                    debug!("[dealloc_user_res] trap_cx dealloc info: {:?}", err);
                }
            }
        }
        // dealloc trap_cx manually
        // let trap_cx_bottom_va: VirtAddr = trap_cx_bottom_from_slot(slot).into();
        // self.remove_area_with_start_vpn(trap_cx_bottom_va.into())
        //     .unwrap();
    }
}

fn memory_error_to_errno(err: MemoryError) -> isize {
    match err {
        MemoryError::BeyondEOF
        | MemoryError::NoPermission
        | MemoryError::BadAddress
        | MemoryError::NotMapped
        | MemoryError::BackingStoreFailure => EFAULT,
        MemoryError::OutOfMemory => ENOMEM,
        other => {
            warn!("[fault_in] unexpected memory error: {:?}", other);
            EFAULT
        }
    }
}

pub(super) fn check_page_fault(addr: VirtAddr, access: FaultAccess) -> Result<PhysAddr, isize> {
    // This is where we handle the page fault.
    let vm = match current_task_ref() {
        Some(task) => task.process.vm(),
        None => {
            log::warn!("[check_page_fault] No current task found, page fault in kernel?");
            return Err(EFAULT);
        }
    };
    let result = vm.lock().fault_in_trap_va(addr, access);
    result
}
