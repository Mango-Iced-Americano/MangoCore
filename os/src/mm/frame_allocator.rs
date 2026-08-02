//! 物理页帧分配器。
//!
//! 当前实现为每个物理 DRAM region 维护单调增长游标，并共用一个回收栈。
//! `FrameTracker` 通过 RAII 在最后一个引用释放时把物理页归还给全局
//! `FRAME_ALLOCATOR`。region 之间的 MMIO/地址空洞不会进入分配器。
//!
//! # Locking
//!
//! 全局分配器由 `RwLock` 保护。分配路径可能触发 OOM 回收；调用者不应在持有
//! VMA、文件系统或调度器锁时依赖 OOM 回收一定成功。
//!
//! # Safety
//!
//! `*_uninit` 接口返回未清零的物理页，仅能用于立即完全覆盖整页内容的路径。

use super::{PhysAddr, PhysPageNum};
use crate::config::PAGE_SIZE;
use crate::hal::{firmware, local_irq_restore, local_irq_save};
#[cfg(feature = "oom_handler")]
use crate::task::current_task_ref;

use alloc::{sync::Arc, vec::Vec};
use core::fmt::{self, Debug, Formatter};
use lazy_static::*;
use spin::RwLock;

/// 一个已分配物理页帧的 RAII 跟踪器。
pub struct FrameTracker {
    /// 跟踪的物理页号。
    pub ppn: PhysPageNum,
}

impl FrameTracker {
    /// 分配跟踪器并把整页清零。
    pub fn new(ppn: PhysPageNum) -> Self {
        let zero_start = crate::task::perf::perf_memory_io_time_now();
        let ptr = ppn.get_dwords_array().as_mut_ptr();
        const WORDS_PER_PAGE: usize = PAGE_SIZE / core::mem::size_of::<u64>();
        const UNROLL: usize = 8;
        let mut i = 0;
        while i + UNROLL <= WORDS_PER_PAGE {
            // Safety: `ppn` 来自帧分配器的可用物理页，`get_dwords_array`
            // 暴露的页大小正好是 `WORDS_PER_PAGE` 个 u64；循环边界保证写入不越界。
            unsafe {
                ptr.add(i).write(0);
                ptr.add(i + 1).write(0);
                ptr.add(i + 2).write(0);
                ptr.add(i + 3).write(0);
                ptr.add(i + 4).write(0);
                ptr.add(i + 5).write(0);
                ptr.add(i + 6).write(0);
                ptr.add(i + 7).write(0);
            }
            i += UNROLL;
        }
        while i < WORDS_PER_PAGE {
            // Safety: 同上，尾部循环只覆盖剩余未清零的页内 u64。
            unsafe { ptr.add(i).write(0) };
            i += 1;
        }
        crate::task::perf::record_pagefault_stage(
            4,
            crate::task::perf::perf_memory_io_time_now().wrapping_sub(zero_start),
        );
        Self { ppn }
    }

    /// 创建不清零的帧跟踪器。
    ///
    /// # Safety
    ///
    /// 调用者必须保证 `ppn` 是刚从帧分配器取得、尚未交给其他所有者的页帧；
    /// 返回后必须在暴露给用户或内核读取前完全初始化该页。
    pub unsafe fn new_uninit(ppn: PhysPageNum) -> Self {
        Self { ppn }
    }
}

impl Debug for FrameTracker {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("FrameTracker:PPN={:#x}", self.ppn.0))
    }
}
impl Drop for FrameTracker {
    // `FrameTracker` 是物理页唯一回收入口；释放 Arc 最后一个引用时归还帧。
    fn drop(&mut self) {
        // println!("do drop at {}", self.ppn.0);
        frame_dealloc(self.ppn);
    }
}

/// 帧分配器接口。
trait FrameAllocator {
    fn new() -> Self;
    /// 分配并返回一页已清零物理页。
    fn alloc(&mut self) -> Option<FrameTracker>;
    /// 分配并返回一页未清零物理页。
    ///
    /// # Safety
    ///
    /// 调用者必须在任意读取或映射给用户前完全初始化返回页。
    unsafe fn alloc_uninit(&mut self) -> Option<FrameTracker>;
    /// 释放一页物理页。
    fn dealloc(&mut self, ppn: PhysPageNum);
}

/// 一个连续 DRAM region 的分配状态。
struct FrameRegion {
    start: usize,
    current: usize,
    end: usize,
    // recycled 中 PPN 的 O(1) membership 标记，避免释放大量用户页时线性查重。
    recycled_flags: Vec<bool>,
}

impl FrameRegion {
    fn new(start: usize, end: usize) -> Self {
        let mut recycled_flags = Vec::new();
        recycled_flags.resize(end - start, false);
        Self {
            start,
            current: start,
            end,
            recycled_flags,
        }
    }

    fn recycled_index(&self, ppn: usize) -> Option<usize> {
        ppn.checked_sub(self.start)
            .filter(|idx| *idx < self.recycled_flags.len())
    }

    fn is_recycled(&self, ppn: usize) -> bool {
        self.recycled_index(ppn)
            .map(|idx| self.recycled_flags[idx])
            .unwrap_or(false)
    }

    fn mark_recycled(&mut self, ppn: usize, value: bool) {
        if let Some(idx) = self.recycled_index(ppn) {
            self.recycled_flags[idx] = value;
        }
    }

    fn contains_extent(&self, start: usize, count: usize) -> bool {
        start >= self.start
            && start
                .checked_add(count)
                .map(|end| end <= self.end)
                .unwrap_or(false)
    }

    fn extent_is_recycled(&self, start: usize, count: usize) -> bool {
        self.contains_extent(start, count)
            && (start..start + count).all(|ppn| self.is_recycled(ppn))
    }

    fn mark_recycled_extent(&mut self, start: usize, count: usize, value: bool) {
        assert!(self.contains_extent(start, count));
        for ppn in start..start + count {
            self.mark_recycled(ppn, value);
        }
    }

    fn unallocated_frames(&self) -> usize {
        self.end.saturating_sub(self.current)
    }
}

/// A page-aligned linker-owned range released after its embedded payload is copied.
///
/// These pages are below `ekernel`, so they are deliberately excluded from the
/// fresh allocator regions. `free_flags` tracks their lifecycle after the
/// explicit release: `true` means the page is on `recycled`, while `false`
/// means it has subsequently been allocated.
struct ReclaimedRegion {
    start: usize,
    end: usize,
    free_flags: Vec<bool>,
}

impl ReclaimedRegion {
    fn try_new(start: usize, end: usize) -> Result<Self, &'static str> {
        let mut free_flags = Vec::new();
        free_flags
            .try_reserve_exact(end - start)
            .map_err(|_| "cannot allocate reclaimed-frame state")?;
        free_flags.resize(end - start, true);
        Ok(Self {
            start,
            end,
            free_flags,
        })
    }

    fn contains(&self, ppn: usize) -> bool {
        self.start <= ppn && ppn < self.end
    }

    fn index(&self, ppn: usize) -> Option<usize> {
        ppn.checked_sub(self.start)
            .filter(|idx| *idx < self.free_flags.len())
    }

    fn is_free(&self, ppn: usize) -> bool {
        self.index(ppn)
            .map(|idx| self.free_flags[idx])
            .unwrap_or(false)
    }

    fn mark_free(&mut self, ppn: usize, value: bool) {
        let idx = self
            .index(ppn)
            .expect("reclaimed frame outside its registered region");
        self.free_flags[idx] = value;
    }

    fn contains_extent(&self, start: usize, count: usize) -> bool {
        start >= self.start
            && start
                .checked_add(count)
                .map(|end| end <= self.end)
                .unwrap_or(false)
    }

    fn extent_is_free(&self, start: usize, count: usize) -> bool {
        self.contains_extent(start, count) && (start..start + count).all(|ppn| self.is_free(ppn))
    }

    fn mark_free_extent(&mut self, start: usize, count: usize, value: bool) {
        assert!(self.contains_extent(start, count));
        for ppn in start..start + count {
            self.mark_free(ppn, value);
        }
    }
}

/// 对每个可分配物理页区间调用 `f`。
///
/// 平台 DRAM region 先排除物理第 0 页、固件/设备保留区和当前内核镜像。
/// 对于 2K1000LA，低 256 MiB bank 会保留 U-Boot、DVO 和 CPU1 仍在使用的
/// carveout；第二个 bank 则从 `ekernel` 后开始。
pub(super) fn for_each_usable_frame_region(mut f: impl FnMut(PhysPageNum, PhysPageNum)) {
    extern "C" {
        fn skernel();
        fn ekernel();
    }

    let kernel_start = crate::hal::boot::kernel_linked_to_phys(skernel as *const () as usize);
    let kernel_end = crate::hal::boot::kernel_linked_to_phys(ekernel as *const () as usize);
    let mut previous_end = 0usize;

    let firmware_reserved_regions = firmware::firmware_reserved_regions();
    let mut exclusions = Vec::with_capacity(firmware_reserved_regions.len() + 1);
    exclusions.extend_from_slice(firmware_reserved_regions);
    exclusions.push((kernel_start, kernel_end));
    exclusions.sort_unstable_by_key(|range| range.0);

    // Firmware may place its DTB inside the kernel image's BSS range. Both
    // ranges describe unavailable pages, so coalesce them before subtraction.
    let mut merged_len = 0;
    for index in 0..exclusions.len() {
        let (start, end) = exclusions[index];
        if merged_len != 0 && start <= exclusions[merged_len - 1].1 {
            exclusions[merged_len - 1].1 = exclusions[merged_len - 1].1.max(end);
        } else {
            exclusions[merged_len] = (start, end);
            merged_len += 1;
        }
    }
    exclusions.truncate(merged_len);

    let mut previous_exclusion_end = 0usize;
    for &(start, end) in &exclusions {
        assert!(start < end, "empty physical memory exclusion");
        assert_eq!(start % PAGE_SIZE, 0, "unaligned exclusion start");
        assert_eq!(end % PAGE_SIZE, 0, "unaligned exclusion end");
        assert!(
            start >= previous_exclusion_end,
            "physical memory exclusions overlap or are unsorted"
        );
        previous_exclusion_end = end;
    }

    for &(region_start, region_end) in firmware::memory_regions() {
        assert!(region_start < region_end, "empty physical memory region");
        assert!(
            region_start >= previous_end,
            "physical memory regions overlap or are unsorted"
        );
        previous_end = region_end;

        assert_eq!(region_start % PAGE_SIZE, 0, "unaligned DRAM region start");
        assert_eq!(region_end % PAGE_SIZE, 0, "unaligned DRAM region end");

        let mut cursor = region_start.max(PAGE_SIZE);
        for &(excluded_start, excluded_end) in &exclusions {
            if excluded_end <= cursor {
                continue;
            }
            if excluded_start >= region_end {
                break;
            }
            let free_end = excluded_start.min(region_end);
            if cursor < free_end {
                f(
                    PhysAddr::from(cursor).floor(),
                    PhysAddr::from(free_end).floor(),
                );
            }
            cursor = cursor.max(excluded_end).min(region_end);
            if cursor == region_end {
                break;
            }
        }
        if cursor < region_end {
            f(
                PhysAddr::from(cursor).floor(),
                PhysAddr::from(region_end).floor(),
            );
        }
    }
}

/// Return whether a physical byte address belongs to a declared DRAM bank.
pub fn is_ram_phys_addr(addr: usize) -> bool {
    firmware::memory_regions()
        .iter()
        .any(|&(start, end)| start <= addr && addr < end)
}

/// Return whether a physical address is DRAM that may back an allocated page.
pub fn is_allocatable_ram_phys_addr(addr: usize) -> bool {
    addr >= PAGE_SIZE
        && is_ram_phys_addr(addr)
        && !firmware::firmware_reserved_regions()
            .iter()
            .any(|&(start, end)| start <= addr && addr < end)
}

/// 栈式多 region 帧分配器。
pub struct StackFrameAllocator {
    regions: Vec<FrameRegion>,
    // Linker-owned payload pages released only after their final read.
    reclaimed_regions: Vec<ReclaimedRegion>,
    // 首个尚未耗尽 fresh 页的 region。
    fresh_region: usize,
    // 已回收的页面（内存框架）的列表
    recycled: Vec<usize>,
}

impl StackFrameAllocator {
    /// 从平台 DRAM region 表初始化全部可分配物理页。
    pub fn init(&mut self) {
        self.regions.clear();
        self.reclaimed_regions.clear();
        self.recycled.clear();
        self.fresh_region = 0;
        self.regions.reserve(firmware::memory_regions().len());

        let mut total_frames = 0usize;
        for_each_usable_frame_region(|start, end| {
            total_frames = total_frames
                .checked_add(end.0 - start.0)
                .expect("physical frame count overflow");
            self.regions.push(FrameRegion::new(start.0, end.0));
        });
        self.recycled.reserve(total_frames);

        boot_trace!(
            "[memory] {} usable physical frames across {} region(s)",
            total_frames,
            self.regions.len()
        );
        for (index, region) in self.regions.iter().enumerate() {
            boot_trace!(
                "[memory] region{}: [{:#x}, {:#x}) frames={}",
                index,
                region.start * PAGE_SIZE,
                region.end * PAGE_SIZE,
                region.end - region.start
            );
        }
    }

    fn take_fresh_ppn(&mut self) -> Option<usize> {
        let previous_region = self.fresh_region;
        while self
            .regions
            .get(self.fresh_region)
            .map(|region| region.current == region.end)
            .unwrap_or(false)
        {
            self.fresh_region += 1;
        }
        if self.fresh_region != previous_region && self.fresh_region < self.regions.len() {
            boot_trace!(
                "[memory] fresh allocation advanced: region{} -> region{}",
                previous_region,
                self.fresh_region
            );
        }
        let region = self.regions.get_mut(self.fresh_region)?;
        let ppn = region.current;
        region.current += 1;
        Some(ppn)
    }

    /// Remove a physically contiguous recycled extent from one registered region.
    fn take_recycled_extent(&mut self, count: usize) -> Option<usize> {
        if count == 1 {
            return self.take_recycled_ppn();
        }

        // The free stack is unordered. Use its entries as possible extent
        // starts, then validate membership through the per-region bitmaps.
        // Multi-page DMA extents are small and infrequent; this avoids adding a
        // second ownership index while preserving released extent reuse.
        let start = self.recycled.iter().rev().copied().find(|&candidate| {
            self.regions
                .iter()
                .any(|region| region.extent_is_recycled(candidate, count))
                || self
                    .reclaimed_regions
                    .iter()
                    .any(|region| region.extent_is_free(candidate, count))
        })?;
        let end = start.checked_add(count)?;

        let old_len = self.recycled.len();
        self.recycled.retain(|&ppn| !(start <= ppn && ppn < end));
        assert_eq!(
            old_len - self.recycled.len(),
            count,
            "recycled extent bitmap/free-list mismatch"
        );

        if let Some(region) = self
            .regions
            .iter_mut()
            .find(|region| region.contains_extent(start, count))
        {
            region.mark_recycled_extent(start, count, false);
        } else if let Some(region) = self
            .reclaimed_regions
            .iter_mut()
            .find(|region| region.contains_extent(start, count))
        {
            region.mark_free_extent(start, count, false);
        } else {
            unreachable!("validated recycled extent lost its owner region");
        }
        Some(start)
    }

    /// Allocate one physically contiguous extent from a single DRAM region.
    ///
    /// Recycled extents are preferred. Fresh allocation may skip a region whose
    /// tail is too small, but it never joins tails across a DRAM/MMIO boundary.
    fn alloc_contiguous(&mut self, num: usize) -> Option<Vec<Arc<FrameTracker>>> {
        let mut frames = Vec::new();
        if frames.try_reserve(num).is_err() {
            return None;
        }
        if num == 0 {
            return Some(frames);
        }

        if let Some(start) = self.take_recycled_extent(num) {
            for ppn in start..start + num {
                let started = crate::task::perf::perf_time_now_for(
                    crate::task::perf::STATS_PROFILE_MEMORY_IO,
                );
                crate::task::perf::record_frame_alloc();
                frames.push(Arc::new(FrameTracker::new(ppn.into())));
                crate::task::perf::record_frame_alloc_time_us(
                    crate::task::perf::perf_time_now_for(
                        crate::task::perf::STATS_PROFILE_MEMORY_IO,
                    )
                    .saturating_sub(started),
                );
            }
            return Some(frames);
        }

        let region_index = self
            .regions
            .iter()
            .enumerate()
            .skip(self.fresh_region)
            .find(|(_, region)| region.end.saturating_sub(region.current) >= num)
            .map(|(index, _)| index)?;
        let start = self.regions[region_index].current;
        self.regions[region_index].current += num;

        for ppn in start..start + num {
            let started =
                crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
            crate::task::perf::record_frame_alloc();
            #[cfg(not(feature = "zero_init"))]
            let frame = FrameTracker::new(ppn.into());
            #[cfg(feature = "zero_init")]
            // Safety: the whole extent was just removed from this region's
            // fresh range; zero_init pre-cleared it during boot.
            let frame = unsafe { FrameTracker::new_uninit(ppn.into()) };
            frames.push(Arc::new(frame));
            crate::task::perf::record_frame_alloc_time_us(
                crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO)
                    .saturating_sub(started),
            );
        }
        Some(frames)
    }

    /// Allocate a fresh physically contiguous extent from one DRAM region.
    ///
    /// This deliberately bypasses recycled pages so DMA queue setup is not
    /// sensitive to free-list fragmentation. Small tails skipped here remain
    /// available to later single-page allocations.
    fn alloc_fresh_contiguous(&mut self, num: usize) -> Option<Vec<Arc<FrameTracker>>> {
        let mut frames = Vec::new();
        if frames.try_reserve(num).is_err() {
            return None;
        }
        if num == 0 {
            return Some(frames);
        }

        let region_index = self
            .regions
            .iter()
            .enumerate()
            .skip(self.fresh_region)
            .find(|(_, region)| region.end.saturating_sub(region.current) >= num)
            .map(|(index, _)| index)?;
        let start = self.regions[region_index].current;
        self.regions[region_index].current += num;

        for ppn in start..start + num {
            let started =
                crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
            crate::task::perf::record_frame_alloc();
            #[cfg(not(feature = "zero_init"))]
            let frame = FrameTracker::new(ppn.into());
            #[cfg(feature = "zero_init")]
            // Safety: this fresh extent has not previously been handed out and
            // zero_init cleared every allocator-owned region during boot.
            let frame = unsafe { FrameTracker::new_uninit(ppn.into()) };
            frames.push(Arc::new(frame));
            crate::task::perf::record_frame_alloc_time_us(
                crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO)
                    .saturating_sub(started),
            );
        }
        Some(frames)
    }

    fn take_recycled_ppn(&mut self) -> Option<usize> {
        let ppn = self.recycled.pop()?;
        if let Some(region) = self
            .regions
            .iter_mut()
            .find(|region| region.start <= ppn && ppn < region.end)
        {
            assert!(
                region.is_recycled(ppn),
                "free-list frame is not marked recycled"
            );
            region.mark_recycled(ppn, false);
            return Some(ppn);
        }
        if let Some(region) = self
            .reclaimed_regions
            .iter_mut()
            .find(|region| region.contains(ppn))
        {
            assert!(region.is_free(ppn), "reclaimed frame is not marked free");
            region.mark_free(ppn, false);
            return Some(ppn);
        }
        panic!("recycled frame outside registered allocator regions");
    }

    /// Register a linker-owned page range as free after its final use.
    fn reclaim_linker_frames(
        &mut self,
        start: PhysPageNum,
        end: PhysPageNum,
    ) -> Result<usize, &'static str> {
        if start > end {
            return Err("reclaimed frame range is reversed");
        }
        if start == end {
            return Ok(0);
        }

        let start_addr = start
            .0
            .checked_mul(PAGE_SIZE)
            .ok_or("reclaimed frame start overflows")?;
        let end_addr = end
            .0
            .checked_mul(PAGE_SIZE)
            .ok_or("reclaimed frame end overflows")?;
        if !firmware::memory_regions()
            .iter()
            .any(|&(region_start, region_end)| region_start <= start_addr && end_addr <= region_end)
        {
            return Err("reclaimed frame range is not inside one DRAM region");
        }
        if firmware::firmware_reserved_regions()
            .iter()
            .any(|&(reserved_start, reserved_end)| {
                start_addr < reserved_end && reserved_start < end_addr
            })
        {
            return Err("reclaimed frame range overlaps firmware-reserved DRAM");
        }
        let overlaps =
            |range_start: usize, range_end: usize| start.0 < range_end && range_start < end.0;
        if self
            .regions
            .iter()
            .any(|region| overlaps(region.start, region.end))
        {
            return Err("reclaimed frame range overlaps a fresh allocator region");
        }
        if self
            .reclaimed_regions
            .iter()
            .any(|region| overlaps(region.start, region.end))
        {
            return Err("reclaimed frame range overlaps an earlier release");
        }

        let frame_count = end.0 - start.0;
        let region = ReclaimedRegion::try_new(start.0, end.0)?;
        self.reclaimed_regions
            .try_reserve(1)
            .map_err(|_| "cannot register reclaimed-frame region")?;
        self.recycled
            .try_reserve(frame_count)
            .map_err(|_| "cannot extend physical-frame free list")?;

        self.reclaimed_regions.push(region);
        for ppn in start.0..end.0 {
            self.recycled.push(ppn);
        }
        boot_trace!(
            "[memory] reclaimed linker frames: [{:#x}, {:#x}) frames={}",
            start_addr,
            end_addr,
            frame_count
        );
        Ok(frame_count)
    }

    /// 返回当前仍可分配的帧数量。
    pub fn unallocated_frames(&self) -> usize {
        self.regions
            .iter()
            .map(FrameRegion::unallocated_frames)
            .sum::<usize>()
            + self.recycled.len()
    }

    /// 返回帧分配器碎片化诊断 `(total, fresh, recycled, recycled_ratio)`。
    pub fn frag_diagnostic(&self) -> (usize, usize, usize, f64) {
        let fresh = self
            .regions
            .iter()
            .map(FrameRegion::unallocated_frames)
            .sum();
        let recycled = self.recycled.len();
        let total = fresh + recycled;
        let ratio = if total > 0 {
            recycled as f64 / total as f64
        } else {
            0.0
        };
        (total, fresh, recycled, ratio)
    }
}

impl FrameAllocator for StackFrameAllocator {
    fn new() -> Self {
        Self {
            regions: Vec::new(),
            reclaimed_regions: Vec::new(),
            fresh_region: 0,
            recycled: Vec::new(),
        }
    }

    /// 分配一个已清零物理页。
    fn alloc(&mut self) -> Option<FrameTracker> {
        let _start =
            crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
        crate::task::perf::record_frame_alloc();
        // 优先使用回收的帧
        let result = if let Some(ppn) = self.take_recycled_ppn() {
            Some(FrameTracker::new(ppn.into()))
        } else if let Some(ppn) = self.take_fresh_ppn() {
            #[cfg(not(feature = "zero_init"))]
            let ft = FrameTracker::new(ppn.into());
            #[cfg(feature = "zero_init")]
            // Safety: `ppn` 是本分配器刚取出的 fresh 帧，`zero_init`
            // 配置下调用方承诺后续路径负责初始化。
            let ft = unsafe { FrameTracker::new_uninit(ppn.into()) };
            Some(ft)
        } else {
            None
        };
        crate::task::perf::record_frame_alloc_time_us(
            crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO)
                .saturating_sub(_start),
        );
        crate::task::perf::record_pagefault_stage(
            3,
            crate::task::perf::perf_memory_io_time_now().wrapping_sub(_start),
        );
        result
    }

    /// 分配一个未清零物理页。
    ///
    /// # Safety
    ///
    /// 调用者必须保证返回页在读取或暴露给用户前会被完整覆盖。
    unsafe fn alloc_uninit(&mut self) -> Option<FrameTracker> {
        if let Some(ppn) = self.take_recycled_ppn() {
            // Safety: `ppn` 从回收栈弹出后重新归当前分配所有；调用者负责完整初始化。
            let frame_tracker = FrameTracker::new_uninit(ppn.into());
            //log::trace!("[frame_alloc_uninit] {:?}", frame_tracker);
            Some(frame_tracker)
        } else if let Some(ppn) = self.take_fresh_ppn() {
            // Safety: `ppn` 是 fresh 帧，尚未交给其他所有者；调用者负责初始化。
            let frame_tracker = FrameTracker::new_uninit(ppn.into());
            Some(frame_tracker)
        } else {
            None
        }
    }

    /// 释放一个物理页。
    fn dealloc(&mut self, ppn: PhysPageNum) {
        let ppn = ppn.0;
        if let Some(region) = self
            .regions
            .iter_mut()
            .find(|region| region.start <= ppn && ppn < region.end)
        {
            if ppn >= region.current {
                log::warn!(
                    "[frame_dealloc] ignore invalid ppn={:#x}, valid=[{:#x}, {:#x}), current={:#x}",
                    ppn,
                    region.start,
                    region.end,
                    region.current
                );
                return;
            }
            // O(1) duplicate check. The old linear scan made large mmap/free
            // workloads degenerate as the free-list grew.
            if region.is_recycled(ppn) {
                if option_env!("MODE") == Some("debug") {
                    panic!("Frame ppn={:#x} has not been allocated!", ppn);
                }
                log::warn!("[frame_dealloc] ignore duplicate ppn={:#x}", ppn);
                return;
            }
            region.mark_recycled(ppn, true);
            self.recycled.push(ppn);
            return;
        }

        if let Some(region) = self
            .reclaimed_regions
            .iter_mut()
            .find(|region| region.contains(ppn))
        {
            if region.is_free(ppn) {
                if option_env!("MODE") == Some("debug") {
                    panic!("Reclaimed frame ppn={:#x} has not been allocated!", ppn);
                }
                log::warn!("[frame_dealloc] ignore duplicate ppn={:#x}", ppn);
                return;
            }
            region.mark_free(ppn, true);
            self.recycled.push(ppn);
            return;
        }

        log::warn!(
            "[frame_dealloc] ignore ppn={:#x} outside allocated frame regions",
            ppn
        );
    }
}

type FrameAllocatorImpl = StackFrameAllocator;

lazy_static! {
    /// 全局帧分配器。
    pub static ref FRAME_ALLOCATOR: RwLock<FrameAllocatorImpl> =
        RwLock::new(FrameAllocatorImpl::new());
}

#[cfg(all(
    feature = "loongarch64",
    feature = "boot_la_uboot_dmw",
    feature = "bringup_trace"
))]
fn probe_board_memory_word(pa: usize) {
    let ptr = pa as *mut u64;
    let pattern_a = 0x4d41_4e47_5241_4d31u64 ^ pa as u64;
    let pattern_b = !pattern_a;
    // Safety: `pa` is selected from an allocator-usable DRAM page before the
    // allocator is enabled. The raw address deliberately follows the same
    // DMW0 coherent-cached path used by PhysAddr/PhysPageNum; mixing it with a
    // DMW2/SUC alias would require explicit cache clean/invalidate operations.
    // The original word is restored before returning.
    unsafe {
        let original = core::ptr::read_volatile(ptr);
        core::ptr::write_volatile(ptr, pattern_a);
        core::arch::asm!("dbar 0", options(nostack, preserves_flags));
        assert_eq!(
            core::ptr::read_volatile(ptr),
            pattern_a,
            "2K1000LA DRAM probe pattern A mismatch at {:#x}",
            pa
        );
        core::ptr::write_volatile(ptr, pattern_b);
        core::arch::asm!("dbar 0", options(nostack, preserves_flags));
        assert_eq!(
            core::ptr::read_volatile(ptr),
            pattern_b,
            "2K1000LA DRAM probe pattern B mismatch at {:#x}",
            pa
        );
        core::ptr::write_volatile(ptr, original);
        core::arch::asm!("dbar 0", options(nostack, preserves_flags));
    }
}

#[cfg(all(
    feature = "loongarch64",
    feature = "boot_la_uboot_dmw",
    feature = "bringup_trace"
))]
fn probe_board_memory_regions() {
    for_each_usable_frame_region(|start, end| {
        let first = start.start_addr().0;
        let last = PhysPageNum(end.0 - 1).start_addr().0;
        probe_board_memory_word(first);
        if last != first {
            probe_board_memory_word(last);
        }
        boot_trace!("[memory] probe passed: first={:#x} last={:#x}", first, last);
    });
}

/// 初始化全局帧分配器。
pub fn init_frame_allocator() {
    #[cfg(all(
        feature = "loongarch64",
        feature = "boot_la_uboot_dmw",
        feature = "bringup_trace"
    ))]
    probe_board_memory_regions();
    FRAME_ALLOCATOR.write().init();
}

/// 尝试回收至少 `req` 个物理页。
///
/// # Locking
///
/// 该路径会尝试锁当前任务地址空间并通知所有任务执行 OOM 回收；调用者不应持有
/// 会被回收路径再次获取的锁。
#[cfg(feature = "oom_handler")]
pub fn oom_handler(req: usize) -> Result<(), ()> {
    let mut released = 0;
    if released >= req {
        return Ok(());
    }
    // step 2: 清理当前任务的内存
    if let Some(task) = current_task_ref() {
        let vm_ref = task.process.vm();
        let mut maybe_guard = vm_ref.try_lock();
        if let Some(address_space) = maybe_guard.as_mut() {
            released += address_space.do_shallow_clean();
            log::warn!("[oom_handler] current task released: {}", released);
        } else {
            log::warn!("[oom_handler] try lock current task vm failed!");
        }
    } else {
        log::warn!("[oom_handler] no current task, skip current task reclaim");
    }
    if released >= req {
        return Ok(());
    }
    // step 3: 清理所有任务的内存
    log::warn!("[oom_handler] notify all tasks!");
    crate::task::do_oom(req - released)
}

#[cfg(feature = "oom_handler")]
/// 尽力保证至少还有 `num` 个可分配帧。
pub fn frame_reserve(num: usize) {
    // 获取还可分配的帧数量
    let remain = FRAME_ALLOCATOR.read().unallocated_frames();
    if remain < num {
        if oom_handler(num - remain).is_err() {
            log::warn!(
                "[frame_reserve] unable to reserve {} frames, remain {}",
                num,
                remain
            );
        }
    }
}

#[cfg(not(feature = "oom_handler"))]
/// OOM handler 关闭时的空实现。
pub fn frame_reserve(_num: usize) {}

#[cfg(feature = "oom_handler")]
/// 分配一页物理页，失败时先尝试 OOM 回收。
pub fn frame_alloc() -> Option<Arc<FrameTracker>> {
    let result = FRAME_ALLOCATOR.write().alloc();
    match result {
        Some(frame_tracker) => Some(Arc::new(frame_tracker)),
        None => {
            let before = unallocated_frames();
            if oom_handler(1).is_err() {
                log::warn!("[frame_alloc] oom recovery failed");
                return None;
            }
            crate::show_frame_consumption!("GC", before);
            FRAME_ALLOCATOR
                .write()
                .alloc()
                .map(|frame_tracker| Arc::new(frame_tracker))
        }
    }
}

/// Allocate `num` physically contiguous pages from one ownership region.
///
/// The allocator lock covers the entire extent selection, and interrupts are
/// disabled while that lock is held so an interrupt-side frame allocation
/// cannot recurse into the same spin lock.
///
/// # Errors
///
/// Returns `None` when no registered fresh or recycled region has a large
/// enough extent, or the result vector cannot be reserved. The function never
/// spans a DRAM hole.
pub fn frames_alloc(num: usize) -> Option<Vec<Arc<FrameTracker>>> {
    let was_enabled = local_irq_save();
    let result = FRAME_ALLOCATOR.write().alloc_contiguous(num);
    local_irq_restore(was_enabled);
    result
}

/// Allocate `num` physical pages without requiring physical contiguity.
///
/// Unlike `frames_alloc`, this does NOT enforce `base + i` PPN ordering.
/// Suitable for virtual-memory mappings (e.g. SysV SHM) that map pages
/// individually via page tables.  DMA callers must use `frames_alloc` or
/// `frames_alloc_fresh_contiguous`.
///
/// # Errors
/// `Vec` reservation failure or any single-page allocation failure → `None`.
pub fn frames_alloc_any(num: usize) -> Option<Vec<Arc<FrameTracker>>> {
    let mut frames = Vec::new();
    if frames.try_reserve(num).is_err() {
        return None;
    }
    for _ in 0..num {
        if let Some(frame_tracker) = frame_alloc() {
            frames.push(frame_tracker);
        } else {
            return None;
        }
    }
    Some(frames)
}

/// 从 fresh pool 分配 `num` 个物理连续页，完全绕过回收栈。
///
/// 从 `FRAME_ALLOCATOR` 单调递增计数器直接分配，保证物理连续且不受
/// 碎片化回收模式影响。适用于 DMA/VirtIO 等要求物理连续的场景。
///
/// # Errors
///
/// fresh 页不足或 `Vec` 预留失败时返回 `None`；已分配帧会随局部变量释放回收。
pub fn frames_alloc_fresh_contiguous(num: usize) -> Option<Vec<Arc<FrameTracker>>> {
    let was_enabled = local_irq_save();
    let result = FRAME_ALLOCATOR.write().alloc_fresh_contiguous(num);
    local_irq_restore(was_enabled);
    result
}

#[cfg(not(feature = "oom_handler"))]
/// 分配一页物理页。
pub fn frame_alloc() -> Option<Arc<FrameTracker>> {
    FRAME_ALLOCATOR
        .write()
        .alloc()
        .map(|frame_tracker| Arc::new(frame_tracker))
}

#[cfg(feature = "oom_handler")]
/// 分配一页未清零物理页，失败时先尝试 OOM 回收。
///
/// # Safety
///
/// 调用者必须保证返回页在读取或映射给用户前被完整覆盖。
pub unsafe fn frame_alloc_uninit() -> Option<Arc<FrameTracker>> {
    let result = FRAME_ALLOCATOR.write().alloc_uninit();
    match result {
        Some(frame_tracker) => Some(Arc::new(frame_tracker)),
        None => {
            let before = unallocated_frames();
            if oom_handler(1).is_err() {
                log::warn!("[frame_alloc_uninit] oom recovery failed");
                return None;
            }
            crate::show_frame_consumption!("GC", before);
            FRAME_ALLOCATOR
                .write()
                // Safety: 本函数的调用方承担未初始化页契约。
                .alloc_uninit()
                .map(|frame_tracker| Arc::new(frame_tracker))
        }
    }
}

#[cfg(not(feature = "oom_handler"))]
/// 分配一页未清零物理页。
///
/// # Safety
///
/// 调用者必须保证返回页在读取或映射给用户前被完整覆盖。
pub unsafe fn frame_alloc_uninit() -> Option<Arc<FrameTracker>> {
    FRAME_ALLOCATOR
        .write()
        // Safety: 本函数的调用方承担未初始化页契约。
        .alloc_uninit()
        .map(|frame_tracker| Arc::new(frame_tracker))
}

/// 释放一页物理帧。
pub fn frame_dealloc(ppn: PhysPageNum) {
    crate::task::perf::record_frame_free();
    FRAME_ALLOCATOR.write().dealloc(ppn);
}

/// Release a linker-owned physical page range after its embedded bytes are no longer used.
///
/// The range is registered separately from fresh DRAM so ordinary
/// `frame_dealloc()` remains strict about allocator ownership.
///
/// # Safety
///
/// The caller must prove that every page in `[start, end)` is backed by DRAM,
/// does not contain any live kernel object, has no outstanding `FrameTracker`,
/// and will not be read again through its linker symbol after this call.
pub unsafe fn frame_reclaim_linker_range(
    start: PhysPageNum,
    end: PhysPageNum,
) -> Result<usize, &'static str> {
    FRAME_ALLOCATOR.write().reclaim_linker_frames(start, end)
}

/// 返回当前可用帧数量。
pub fn unallocated_frames() -> usize {
    FRAME_ALLOCATOR.read().unallocated_frames()
}

/// 诊断帧分配器碎片化 `(total_free, fresh, recycled, recycled_ratio)`。
pub fn frame_frag_diag() -> (usize, usize, usize, f64) {
    FRAME_ALLOCATOR.read().frag_diagnostic()
}

#[macro_export]
/// * `$place`: the name tag for the promotion.
/// * `statement`: the enclosed
/// * `before`:
/// 用于测量代码块的帧消耗情况
macro_rules! show_frame_consumption {
    ($place:literal; $($statement:stmt); *;) => {{
        if log::log_enabled!(log::Level::Debug) {
            let __frame_consumption_before = crate::mm::unallocated_frames();
            $($statement)*
            let __frame_consumption_after = crate::mm::unallocated_frames();
            log::debug!(
                "[{}] consumed frames: {}, last frames: {}",
                $place,
                __frame_consumption_before as isize - __frame_consumption_after as isize,
                __frame_consumption_after
            );
        } else {
            $($statement)*
        }
    }};
    ($place:literal, $before:ident) => {{
        if log::log_enabled!(log::Level::Debug) {
            let __frame_consumption_after = crate::mm::unallocated_frames();
            log::debug!(
                "[{}] consumed frames:{}, last frames:{}",
                $place,
                $before as isize - __frame_consumption_after as isize,
                __frame_consumption_after
            );
        }
    }};
}
