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
use crate::hal::{local_irq_restore, local_irq_save};
#[cfg(feature = "oom_handler")]
use crate::task::current_task;

use alloc::{sync::Arc, vec::Vec};
use core::fmt::{self, Debug, Formatter};
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use lazy_static::*;
use spin::RwLock;

/// Idle CPUs may keep this many already-zeroed single pages ready for demand
/// faults.  The pool is deliberately small relative to the 8 GiB BuildStorm
/// guest and is disabled under memory pressure.
pub const PREZERO_POOL_HIGH_WATER: usize = 256;
const PREZERO_POOL_LOW_WATER: usize = PREZERO_POOL_HIGH_WATER / 2;
const PREZERO_REFILL_PER_IDLE_TICK: usize = 2;
const PREZERO_REFILL_PER_IDLE_WAKE: usize = 32;
const PREZERO_MIN_FREE_FRAMES: usize = 2048;
const PREZERO_POLICY_UNINITIALIZED: u8 = 0;
const PREZERO_POLICY_IDLE: u8 = 1;
const PREZERO_POLICY_QUIESCENT: u8 = 2;
const PREZERO_POLICY_OFF: u8 = 3;

static PREZERO_POLICY: AtomicU8 = AtomicU8::new(PREZERO_POLICY_UNINITIALIZED);
/// A low-water notification is coalesced until one idle AP claims it.
static PREZERO_REFILL_REQUESTED: AtomicBool = AtomicBool::new(false);

fn parse_prezero_policy() -> u8 {
    crate::bootargs::get_cmdline()
        .split_whitespace()
        .find_map(|token| token.strip_prefix("mango.mm.prezero="))
        .map(|value| match value {
            "off" | "0" => PREZERO_POLICY_OFF,
            "quiescent" => PREZERO_POLICY_QUIESCENT,
            _ => PREZERO_POLICY_IDLE,
        })
        .unwrap_or(PREZERO_POLICY_IDLE)
}

fn prezero_policy() -> u8 {
    let policy = PREZERO_POLICY.load(Ordering::Acquire);
    if policy != PREZERO_POLICY_UNINITIALIZED {
        return policy;
    }
    let parsed = parse_prezero_policy();
    match PREZERO_POLICY.compare_exchange(
        PREZERO_POLICY_UNINITIALIZED,
        parsed,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => parsed,
        Err(installed) => installed,
    }
}

/// Runtime prezero policy selected by `mango.mm.prezero=`.
pub fn prezero_policy_name() -> &'static str {
    match prezero_policy() {
        PREZERO_POLICY_OFF => "off",
        PREZERO_POLICY_QUIESCENT => "quiescent",
        _ => "idle",
    }
}

fn prezero_refill_allowed() -> bool {
    match prezero_policy() {
        PREZERO_POLICY_OFF => {
            crate::task::perf::record_frame_prezero_refill_skipped(false);
            false
        }
        PREZERO_POLICY_QUIESCENT => {
            let has_ready = crate::task::has_ready_task();
            let has_current = (0..crate::smp::configured_cpu_count())
                .any(|cpu| crate::task::processor::cpu_current_count(cpu) != 0);
            if has_ready || has_current {
                crate::task::perf::record_frame_prezero_refill_skipped(true);
                false
            } else {
                true
            }
        }
        _ => true,
    }
}

/// Wake one genuinely idle AP after demand allocation drains the prezero pool.
/// The allocator lock has already been released before this function sends an
/// IPI, so scheduler and allocator lock ordering cannot cycle.
fn request_idle_prezero_refill(remaining: usize) {
    if remaining >= PREZERO_POOL_LOW_WATER {
        return;
    }

    let online = crate::smp::online_cpu_mask();
    let schedulers = crate::smp::scheduler_cpu_mask();
    let stopped = crate::smp::stopped_cpu_mask();
    let available_aps = online & schedulers & !stopped & !1usize;
    // Early boot allocations occur before any AP scheduler exists.  Avoid
    // consulting bootargs or publishing a request until somebody can serve it.
    if available_aps == 0
        || prezero_policy() != PREZERO_POLICY_IDLE
        || PREZERO_REFILL_REQUESTED.swap(true, Ordering::AcqRel)
    {
        return;
    }
    for cpu in 1..crate::smp::configured_cpu_count() {
        let bit = 1usize << cpu;
        if available_aps & bit == 0
            || crate::task::processor::cpu_current_count(cpu) != 0
            || crate::task::run_queue_count(cpu) != 0
        {
            continue;
        }
        if crate::smp::request_reschedule(cpu).is_ok() {
            return;
        }
    }
    // No AP can service the request now.  A later allocation retries instead
    // of leaving a permanently claimed notification behind.
    PREZERO_REFILL_REQUESTED.store(false, Ordering::Release);
}

/// Clear one allocator-owned page before it is published to another owner.
fn zero_frame_bytes(ppn: PhysPageNum) {
    let ptr = ppn.start_addr().direct_map_ptr().cast::<u64>();
    const WORDS_PER_PAGE: usize = PAGE_SIZE / core::mem::size_of::<u64>();
    const UNROLL: usize = 8;
    let mut i = 0;
    while i + UNROLL <= WORDS_PER_PAGE {
        // Safety: the allocator has removed `ppn` from every free structure,
        // the page is not visible to an owner, and the bounds cover one page.
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
        // Safety: same ownership and bounds argument as the unrolled loop.
        unsafe { ptr.add(i).write(0) };
        i += 1;
    }
}

/// 一个已分配物理页帧的 RAII 跟踪器。
pub struct FrameTracker {
    /// 跟踪的物理页号。
    pub ppn: PhysPageNum,
}

impl FrameTracker {
    /// 分配跟踪器并把整页清零。
    pub fn new(ppn: PhysPageNum) -> Self {
        let zero_start = crate::task::perf::perf_memory_io_time_now();
        zero_frame_bytes(ppn);
        crate::task::perf::record_frame_sync_zero();
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

/// 已从全局分配器元数据中摘除、尚未发布给调用者的单页所有权。
///
/// reservation 离开 allocator 锁后再完成整页清零，避免把内存带宽操作
/// 放在全局写锁内。若未来在完成发布前增加失败路径，Drop 会把页归还。
struct FrameReservation {
    ppn: Option<PhysPageNum>,
    needs_zero: bool,
    started_ticks: usize,
}

impl FrameReservation {
    /// 在 allocator 锁外完成初始化，并把页的回收责任转交给 FrameTracker。
    fn into_tracker(mut self) -> FrameTracker {
        // 先从 reservation 中取走 PPN，再构造 tracker。这样即使清零
        // 或后续统计路径意外 panic，reservation 的 Drop 也不会与
        // 已构造的 tracker 重复归还同一 PPN。
        let ppn = self
            .ppn
            .take()
            .expect("frame reservation was already consumed");
        let tracker = if self.needs_zero {
            FrameTracker::new(ppn)
        } else {
            // Safety: 只有 zero_init 启用时首次领取的 fresh 页会跳过清零；
            // BSP 已在 frame allocator 发布前清零全部可分配 fresh region。
            unsafe { FrameTracker::new_uninit(ppn) }
        };
        crate::task::perf::record_frame_alloc_time_us(
            crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO)
                .saturating_sub(self.started_ticks),
        );
        tracker
    }
}

impl Drop for FrameReservation {
    fn drop(&mut self) {
        if let Some(ppn) = self.ppn.take() {
            frame_dealloc(ppn);
        }
    }
}

/// A page temporarily owned by an idle CPU while it is being zeroed.  If the
/// refill path cannot publish it, Drop returns it to the ordinary recycled
/// list so no page is leaked.
struct PrezeroReservation {
    ppn: Option<PhysPageNum>,
}

impl Drop for PrezeroReservation {
    fn drop(&mut self) {
        if let Some(ppn) = self.ppn.take() {
            frame_dealloc(ppn);
        }
    }
}

/// 帧分配器接口。
trait FrameAllocator {
    fn new() -> Self;
    /// 锁内唯一领取一页；清零由返回的 reservation 在锁外完成。
    fn reserve_one(&mut self) -> Option<FrameReservation>;
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

    // 内核以 KERNEL_LINK_VADDR 高地址链接（见 hal/arch/*/linker.ld）。排除内核
    // 镜像时必须先用 kernel_linked_to_phys 转成运行时物理地址，否则链接地址
    // 永不与 DRAM region 重叠，内核镜像物理页会被帧分配器当作可用帧分配
    // （页表页写进内核 .data 会破坏运行中的内核，表现为启动卡死）。
    let kernel_start = crate::hal::boot::kernel_linked_to_phys(skernel as *const () as usize);
    let kernel_end = crate::hal::boot::kernel_linked_to_phys(ekernel as *const () as usize);
    let kernel_image = [(kernel_start, kernel_end)];
    crate::hal::firmware::for_each_usable_ram_range(&kernel_image, |start, end| {
        f(PhysAddr::from(start).floor(), PhysAddr::from(end).floor());
    });
}

/// Return whether a physical byte address belongs to a declared DRAM bank.
pub fn is_ram_phys_addr(addr: usize) -> bool {
    crate::hal::firmware::memory_regions()
        .iter()
        .any(|&(start, end)| start <= addr && addr < end)
}

/// 判断地址所在整页是否属于固件可用 RAM。
///
/// 这里只验证物理拓扑，不查询当前分配状态。页的实时所有权由页表、VMA 与
/// `FrameTracker` 保证；若在每次 uaccess 后验检查中读取 allocator 锁，会让用户复制
/// 热路径和帧分配产生无谓竞争。也不能永久排除整个内核链接范围，因为其中的 payload
/// 完整页会在复制后以 `ReclaimedRegion` 正式归还。
pub fn is_allocatable_ram_phys_addr(addr: usize) -> bool {
    let page_start = addr & !(PAGE_SIZE - 1);
    let Some(page_end) = page_start.checked_add(PAGE_SIZE) else {
        return false;
    };
    let overlaps_page = |start: usize, end: usize| page_start < end && start < page_end;

    page_start >= PAGE_SIZE
        && crate::hal::firmware::memory_regions()
            .iter()
            .any(|&(start, end)| start <= page_start && page_end <= end)
        && !crate::hal::firmware::firmware_reserved_regions()
            .iter()
            .any(|&(start, end)| overlaps_page(start, end))
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
    // 已从 free structures 摘除并在锁外清零、可直接发布的单页。
    prezeroed: Vec<usize>,
}

impl StackFrameAllocator {
    /// 从平台 DRAM region 表初始化全部可分配物理页。
    pub fn init(&mut self) {
        self.regions.clear();
        self.reclaimed_regions.clear();
        self.recycled.clear();
        self.prezeroed.clear();
        self.prezeroed
            .try_reserve_exact(PREZERO_POOL_HIGH_WATER + crate::smp::MAX_CPUS * 2)
            .ok();
        self.fresh_region = 0;
        self.regions.reserve(
            crate::hal::firmware::memory_regions().len()
                + crate::hal::firmware::firmware_reserved_regions().len()
                + 1,
        );

        let mut total_frames = 0usize;
        for_each_usable_frame_region(|start, end| {
            total_frames = total_frames
                .checked_add(end.0 - start.0)
                .expect("physical frame count overflow");
            self.regions.push(FrameRegion::new(start.0, end.0));
        });
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
                let zero_start = crate::task::perf::perf_time_now_for(
                    crate::task::perf::STATS_PROFILE_MEMORY_IO,
                );
                frames.push(Arc::new(FrameTracker::new(ppn.into())));
                crate::task::perf::record_frame_alloc_source(true);
                crate::task::perf::record_frame_contig_page(
                    crate::task::perf::perf_time_now_for(
                        crate::task::perf::STATS_PROFILE_MEMORY_IO,
                    )
                    .wrapping_sub(zero_start),
                );
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
            crate::task::perf::record_frame_alloc_source(false);
            crate::task::perf::record_frame_contig_page(
                crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO)
                    .wrapping_sub(started),
            );
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
            crate::task::perf::record_frame_alloc_source(false);
            crate::task::perf::record_frame_contig_page(
                crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO)
                    .wrapping_sub(started),
            );
            crate::task::perf::record_frame_alloc_time_us(
                crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO)
                    .saturating_sub(started),
            );
        }
        Some(frames)
    }

    /// 从 fresh pool 永久摘取一个连续 extent，不创建 `FrameTracker`。
    ///
    /// 该接口仅供内核堆扩容使用：返回的页会成为 buddy heap 的长期 backing，
    /// 因而不能由 RAII 回收，也不能进入普通 frame free-list。
    fn reserve_fresh_contiguous(&mut self, num: usize) -> Option<PhysPageNum> {
        if num == 0 {
            return None;
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
        Some(start.into())
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

    /// Claim one free page for background zeroing.  The global allocator lock
    /// protects only ownership transfer; the 4 KiB clear runs after unlock.
    fn reserve_for_prezero(&mut self) -> Option<PrezeroReservation> {
        if self.prezeroed.capacity() < PREZERO_POOL_HIGH_WATER
            || self.prezeroed.len() >= PREZERO_POOL_HIGH_WATER
            || self.unallocated_frames() <= PREZERO_MIN_FREE_FRAMES
        {
            return None;
        }
        let ppn = self.take_recycled_ppn().or_else(|| {
            if cfg!(feature = "zero_init") {
                None
            } else {
                self.take_fresh_ppn()
            }
        })?;
        Some(PrezeroReservation {
            ppn: Some(ppn.into()),
        })
    }

    /// Claim one already-zeroed page without falling back to synchronous zeroing
    /// or OOM recovery. Speculative fault-around uses this path so a failed
    /// prediction can consume only bounded idle work, never demand-path work.
    fn reserve_prezeroed_only(&mut self) -> Option<FrameReservation> {
        let started_ticks =
            crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
        let Some(ppn) = self.prezeroed.pop() else {
            crate::task::perf::record_frame_prezero_miss();
            return None;
        };
        crate::task::perf::record_frame_alloc();
        crate::task::perf::record_frame_alloc_source_prezeroed();
        crate::task::perf::record_frame_prezero_hit();
        Some(FrameReservation {
            ppn: Some(ppn.into()),
            needs_zero: false,
            started_ticks,
        })
    }

    fn publish_prezeroed(&mut self, ppn: PhysPageNum) {
        // Capacity is reserved at init for the high-water mark plus all CPUs'
        // possible in-flight pages, so this push never allocates under lock.
        assert!(self.prezeroed.len() < self.prezeroed.capacity());
        self.prezeroed.push(ppn.0);
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
        if !crate::hal::firmware::memory_regions()
            .iter()
            .any(|&(region_start, region_end)| region_start <= start_addr && end_addr <= region_end)
        {
            return Err("reclaimed frame range is not inside one DRAM region");
        }
        if crate::hal::firmware::firmware_reserved_regions()
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
            + self.prezeroed.len()
    }

    /// 返回帧分配器碎片化诊断 `(total, fresh, recycled, recycled_ratio)`。
    pub fn frag_diagnostic(&self) -> (usize, usize, usize, f64) {
        let fresh = self
            .regions
            .iter()
            .map(FrameRegion::unallocated_frames)
            .sum();
        let recycled = self.recycled.len() + self.prezeroed.len();
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
            prezeroed: Vec::new(),
        }
    }

    /// 从共享元数据中领取一个单页 reservation。
    fn reserve_one(&mut self) -> Option<FrameReservation> {
        let started_ticks =
            crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
        crate::task::perf::record_frame_alloc();
        let result = if let Some(ppn) = self.prezeroed.pop() {
            crate::task::perf::record_frame_alloc_source_prezeroed();
            crate::task::perf::record_frame_prezero_hit();
            Some((PhysPageNum::from(ppn), false))
        } else if let Some(ppn) = self.take_recycled_ppn() {
            crate::task::perf::record_frame_prezero_miss();
            crate::task::perf::record_frame_alloc_source(true);
            // recycled 也包含显式释放的 linker payload 页，必须重新清零。
            Some((PhysPageNum::from(ppn), true))
        } else if let Some(ppn) = self.take_fresh_ppn() {
            crate::task::perf::record_frame_prezero_miss();
            crate::task::perf::record_frame_alloc_source(false);
            Some((PhysPageNum::from(ppn), !cfg!(feature = "zero_init")))
        } else {
            crate::task::perf::record_frame_prezero_miss();
            None
        };
        match result {
            Some((ppn, needs_zero)) => Some(FrameReservation {
                ppn: Some(ppn),
                needs_zero,
                started_ticks,
            }),
            None => {
                // 保持既有统计口径：失败的领取尝试也计数并记录耗时。
                crate::task::perf::record_frame_alloc_time_us(
                    crate::task::perf::perf_time_now_for(
                        crate::task::perf::STATS_PROFILE_MEMORY_IO,
                    )
                    .saturating_sub(started_ticks),
                );
                None
            }
        }
    }

    /// 分配一个未清零物理页。
    ///
    /// # Safety
    ///
    /// 调用者必须保证返回页在读取或暴露给用户前会被完整覆盖。
    unsafe fn alloc_uninit(&mut self) -> Option<FrameTracker> {
        if let Some(ppn) = self.take_recycled_ppn() {
            crate::task::perf::record_frame_alloc_source(true);
            // Safety: `ppn` 从回收栈弹出后重新归当前分配所有；调用者负责完整初始化。
            let frame_tracker = FrameTracker::new_uninit(ppn.into());
            //log::trace!("[frame_alloc_uninit] {:?}", frame_tracker);
            Some(frame_tracker)
        } else if let Some(ppn) = self.take_fresh_ppn() {
            crate::task::perf::record_frame_alloc_source(false);
            // Safety: `ppn` 是 fresh 帧，尚未交给其他所有者；调用者负责初始化。
            let frame_tracker = FrameTracker::new_uninit(ppn.into());
            Some(frame_tracker)
        } else if let Some(ppn) = self.prezeroed.pop() {
            // A zeroed page also satisfies the weaker uninitialized-page
            // contract. Keep this fallback so the bounded idle pool cannot
            // make free memory invisible to full-page-copy callers.
            crate::task::perf::record_frame_alloc_source_prezeroed();
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

#[inline]
fn with_frame_alloc_lock<R>(op: impl FnOnce(&mut FrameAllocatorImpl) -> R) -> R {
    let start = crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
    let mut guard = FRAME_ALLOCATOR.write();
    let acquired = crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
    let result = op(&mut *guard);
    let released = crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
    crate::task::perf::record_frame_global_alloc_lock(
        acquired.wrapping_sub(start),
        released.wrapping_sub(acquired),
    );
    result
}

#[inline]
fn with_frame_free_lock<R>(op: impl FnOnce(&mut FrameAllocatorImpl) -> R) -> R {
    let start = crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
    let mut guard = FRAME_ALLOCATOR.write();
    let acquired = crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
    let result = op(&mut *guard);
    let released = crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
    crate::task::perf::record_frame_global_free_lock(
        acquired.wrapping_sub(start),
        released.wrapping_sub(acquired),
    );
    result
}

#[inline]
fn with_frame_contig_lock<R>(op: impl FnOnce(&mut FrameAllocatorImpl) -> R) -> R {
    let start = crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
    let mut guard = FRAME_ALLOCATOR.write();
    let acquired = crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
    let result = op(&mut *guard);
    let released = crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
    crate::task::perf::record_frame_contig_lock(
        acquired.wrapping_sub(start),
        released.wrapping_sub(acquired),
    );
    result
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
    with_frame_alloc_lock(|allocator| allocator.init());
}

/// Spend a bounded amount of an idle scheduler tick preparing zeroed demand
/// pages.  CPU0 and APs share this routine; every iteration drops the allocator
/// lock before touching page contents.
pub fn idle_prezero_refill() -> usize {
    refill_prezero_pages(PREZERO_REFILL_PER_IDLE_TICK)
}

fn refill_prezero_pages(limit: usize) -> usize {
    if !prezero_refill_allowed() {
        return 0;
    }
    let mut completed = 0;
    for _ in 0..limit {
        let Some(mut reservation) =
            with_frame_alloc_lock(|allocator| allocator.reserve_for_prezero())
        else {
            break;
        };
        let ppn = reservation
            .ppn
            .expect("prezero reservation lost its physical page");
        let started = crate::task::perf::perf_memory_io_time_now();
        zero_frame_bytes(ppn);
        let elapsed = crate::task::perf::perf_memory_io_time_now().wrapping_sub(started);
        with_frame_alloc_lock(|allocator| allocator.publish_prezeroed(ppn));
        reservation.ppn = None;
        crate::task::perf::record_frame_prezero_refill(elapsed);
        completed += 1;
    }
    completed
}

/// Claim one coalesced low-water request from an AP idle scheduler.
pub(crate) fn take_idle_prezero_refill_request() -> bool {
    PREZERO_REFILL_REQUESTED.swap(false, Ordering::Acquire)
}

/// Refill a bounded batch after an event-driven AP wake.
pub(crate) fn idle_prezero_refill_batch() -> usize {
    refill_prezero_pages(PREZERO_REFILL_PER_IDLE_WAKE)
}

/// Try to obtain one page prepared by idle prezeroing.
///
/// This deliberately has no recycled/fresh fallback and never invokes OOM
/// recovery. It is suitable only for optional speculative work that can be
/// abandoned when the pool is empty.
pub(super) fn try_frame_alloc_prezeroed() -> Option<Arc<FrameTracker>> {
    let (reservation, remaining) = with_frame_alloc_lock(|allocator| {
        let reservation = allocator.reserve_prezeroed_only();
        (reservation, allocator.prezeroed.len())
    });
    request_idle_prezero_refill(remaining);
    reservation.map(|reservation| Arc::new(reservation.into_tracker()))
}

fn reserve_one_notifying() -> Option<FrameReservation> {
    let (reservation, remaining) = with_frame_alloc_lock(|allocator| {
        let reservation = allocator.reserve_one();
        (reservation, allocator.prezeroed.len())
    });
    request_idle_prezero_refill(remaining);
    reservation
}

/// Current prezero pool occupancy and its configured high-water mark.
pub fn prezero_pool_stats() -> (usize, usize) {
    (
        FRAME_ALLOCATOR.read().prezeroed.len(),
        PREZERO_POOL_HIGH_WATER,
    )
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
    if let Some(task) = current_task() {
        let vm_ref = task.process.vm();
        match vm_ref.try_write(|address_space| address_space.do_shallow_clean()) {
            Some(count) => {
                released += count;
                log::warn!("[oom_handler] current task released: {}", released);
            }
            None => log::warn!("[oom_handler] try lock current task vm failed!"),
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
    let started = crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
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
    crate::task::perf::record_frame_reserve_check(
        crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO)
            .wrapping_sub(started),
        remain < num,
    );
}

#[cfg(not(feature = "oom_handler"))]
/// OOM handler 关闭时的空实现。
pub fn frame_reserve(_num: usize) {}

#[cfg(feature = "oom_handler")]
/// 分配一页物理页，失败时先尝试 OOM 回收。
pub fn frame_alloc() -> Option<Arc<FrameTracker>> {
    let reservation = reserve_one_notifying();
    match reservation {
        Some(reservation) => Some(Arc::new(reservation.into_tracker())),
        None => {
            let before = unallocated_frames();
            if oom_handler(1).is_err() {
                log::warn!("[frame_alloc] oom recovery failed");
                return None;
            }
            crate::show_frame_consumption!("GC", before);
            let reservation = reserve_one_notifying();
            reservation.map(|reservation| Arc::new(reservation.into_tracker()))
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
    let result = with_frame_contig_lock(|allocator| allocator.alloc_contiguous(num));
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
    let result = with_frame_contig_lock(|allocator| allocator.alloc_fresh_contiguous(num));
    local_irq_restore(was_enabled);
    result
}

/// Reserve one fresh physically contiguous extent as permanent kernel-owned memory.
///
/// Unlike [`frames_alloc_fresh_contiguous`], this creates no `FrameTracker` or
/// `Vec`, so bootstrap heap expansion cannot consume the heap it is creating.
/// The caller must retain the range for the kernel lifetime and never pass it to
/// `frame_dealloc`.
pub(crate) fn reserve_fresh_contiguous(num: usize) -> Option<PhysPageNum> {
    let was_enabled = local_irq_save();
    let result = with_frame_contig_lock(|allocator| allocator.reserve_fresh_contiguous(num));
    local_irq_restore(was_enabled);
    result
}

#[cfg(not(feature = "oom_handler"))]
/// 分配一页物理页。
pub fn frame_alloc() -> Option<Arc<FrameTracker>> {
    let reservation = reserve_one_notifying();
    reservation.map(|reservation| Arc::new(reservation.into_tracker()))
}

#[cfg(feature = "oom_handler")]
/// 分配一页未清零物理页，失败时先尝试 OOM 回收。
///
/// # Safety
///
/// 调用者必须保证返回页在读取或映射给用户前被完整覆盖。
pub unsafe fn frame_alloc_uninit() -> Option<Arc<FrameTracker>> {
    let result = with_frame_alloc_lock(|allocator| allocator.alloc_uninit());
    match result {
        Some(frame_tracker) => Some(Arc::new(frame_tracker)),
        None => {
            let before = unallocated_frames();
            if oom_handler(1).is_err() {
                log::warn!("[frame_alloc_uninit] oom recovery failed");
                return None;
            }
            crate::show_frame_consumption!("GC", before);
            with_frame_alloc_lock(|allocator| {
                // Safety: 本函数的调用方承担未初始化页契约。
                allocator
                    .alloc_uninit()
                    .map(|frame_tracker| Arc::new(frame_tracker))
            })
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
    with_frame_alloc_lock(|allocator| {
        // Safety: 本函数的调用方承担未初始化页契约。
        allocator
            .alloc_uninit()
            .map(|frame_tracker| Arc::new(frame_tracker))
    })
}

/// 释放一页物理帧。
pub fn frame_dealloc(ppn: PhysPageNum) {
    crate::task::perf::record_frame_free();
    with_frame_free_lock(|allocator| allocator.dealloc(ppn));
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
    with_frame_free_lock(|allocator| allocator.reclaim_linker_frames(start, end))
}

/// 返回当前可用帧数量。
pub fn unallocated_frames() -> usize {
    FRAME_ALLOCATOR.read().unallocated_frames()
}

/// panic 等不可等待上下文使用的空闲帧统计；写锁忙时立即返回 `None`。
pub fn try_unallocated_frames() -> Option<usize> {
    Some(FRAME_ALLOCATOR.try_read()?.unallocated_frames())
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
