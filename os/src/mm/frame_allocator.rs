//! 物理页帧分配器。
//!
//! 当前实现是单调增长区间加回收栈的 4 KiB 帧分配器。`FrameTracker` 通过 RAII
//! 在最后一个引用释放时把物理页归还给全局 `FRAME_ALLOCATOR`。
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
use crate::hal::{local_irq_restore, local_irq_save};
use crate::config::PAGE_SIZE;
use crate::hal::MEMORY_END;
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

/// 栈式帧分配器。
pub struct StackFrameAllocator {
    // 可分配物理页号起点，用于把 PPN 映射到 recycled_flags 下标。
    start: usize,
    // 当前分配器的位置，指向可分配区域的开始
    current: usize,
    // 分配器的结束地址，表示可分配内存区域的末尾
    end: usize,
    // 已回收的页面（内存框架）的列表
    recycled: Vec<usize>,
    // recycled 中 PPN 的 O(1) membership 标记，避免释放大量用户页时线性查重。
    recycled_flags: Vec<bool>,
}

impl StackFrameAllocator {
    /// 初始化可分配物理页范围 `[l, r)`。
    pub fn init(&mut self, l: PhysPageNum, r: PhysPageNum) {
        self.recycled.clear();
        self.recycled_flags.clear();
        self.start = l.0;
        self.current = l.0;
        self.end = r.0;
        let last_frames = self.end - self.current;
        self.recycled.reserve(last_frames);
        self.recycled_flags.resize(last_frames, false);
        println!("last {} Physical Frames.", last_frames);
    }
    /// 返回当前仍可分配的帧数量。
    pub fn unallocated_frames(&self) -> usize {
        self.end - self.current + self.recycled.len()
    }

    /// 返回帧分配器碎片化诊断 `(total, fresh, recycled, recycled_ratio)`。
    pub fn frag_diagnostic(&self) -> (usize, usize, usize, f64) {
        let fresh = self.end.saturating_sub(self.current);
        let recycled = self.recycled.len();
        let total = fresh + recycled;
        let ratio = if total > 0 {
            recycled as f64 / total as f64
        } else {
            0.0
        };
        (total, fresh, recycled, ratio)
    }

    /// 返回未分配 fresh 页的数量。
    pub fn fresh_available(&self) -> usize {
        self.end.saturating_sub(self.current)
    }

    /// 从 fresh pool 分配 `num` 个物理连续页，绕过回收栈。
    ///
    /// 调用方必须持有 `FRAME_ALLOCATOR` 锁。空分配（`num == 0`）返回空 `Vec`。
    /// fresh 页不足或 `Vec` 预留失败时返回 `None`。
    pub fn alloc_fresh(&mut self, num: usize) -> Option<Vec<Arc<FrameTracker>>> {
        if self.end.saturating_sub(self.current) < num {
            return None;
        }
        if num == 0 {
            return Some(Vec::new());
        }
        let mut frames = Vec::new();
        if frames.try_reserve(num).is_err() {
            return None;
        }
        for _ in 0..num {
            self.current += 1;
            let ppn = PhysPageNum(self.current - 1);
            frames.push(Arc::new(FrameTracker::new(ppn)));
        }
        Some(frames)
    }
}

impl FrameAllocator for StackFrameAllocator {
    fn new() -> Self {
        Self {
            start: 0,
            current: 0,
            end: 0,
            recycled: Vec::new(),
            recycled_flags: Vec::new(),
        }
    }

    /// 分配一个已清零物理页。
    fn alloc(&mut self) -> Option<FrameTracker> {
        let _start = crate::task::perf::perf_time_now();
        crate::task::perf::record_frame_alloc();
        // 优先使用回收的帧
        let result = if let Some(ppn) = self.recycled.pop() {
            self.mark_recycled(ppn, false);
            Some(FrameTracker::new(ppn.into()))
        } else if self.current == self.end {
            None
        } else {
            self.current += 1;
            #[cfg(not(feature = "zero_init"))]
            let ft = FrameTracker::new((self.current - 1).into());
            #[cfg(feature = "zero_init")]
            // Safety: `current - 1` 是本分配器刚取出的 fresh 帧，`zero_init`
            // 配置下调用方承诺后续路径负责初始化。
            let ft = unsafe { FrameTracker::new_uninit((self.current - 1).into()) };
            Some(ft)
        };
        crate::task::perf::record_frame_alloc_time_us(crate::task::perf::perf_time_now().saturating_sub(_start));
        result
    }

    /// 分配一个未清零物理页。
    ///
    /// # Safety
    ///
    /// 调用者必须保证返回页在读取或暴露给用户前会被完整覆盖。
    unsafe fn alloc_uninit(&mut self) -> Option<FrameTracker> {
        if let Some(ppn) = self.recycled.pop() {
            self.mark_recycled(ppn, false);
            // Safety: `ppn` 从回收栈弹出后重新归当前分配所有；调用者负责完整初始化。
            let frame_tracker = FrameTracker::new_uninit(ppn.into());
            //log::trace!("[frame_alloc_uninit] {:?}", frame_tracker);
            Some(frame_tracker)
        } else if self.current == self.end {
            None
        } else {
            self.current += 1;
            // Safety: `current - 1` 是 fresh 帧，尚未交给其他所有者；调用者负责初始化。
            let frame_tracker = FrameTracker::new_uninit((self.current - 1).into());
            Some(frame_tracker)
        }
    }

    /// 释放一个物理页。
    fn dealloc(&mut self, ppn: PhysPageNum) {
        let ppn = ppn.0;
        let alloc_start = self.start;
        if ppn < alloc_start || ppn >= self.end || ppn >= self.current {
            log::warn!(
                "[frame_dealloc] ignore invalid ppn={:#x}, valid=[{:#x}, {:#x}), current={:#x}",
                ppn,
                alloc_start,
                self.end,
                self.current
            );
            return;
        }
        // O(1) duplicate check.  The old linear scan made large mmap/free
        // workloads degenerate as the free-list grew.
        if self.is_recycled(ppn) {
            if option_env!("MODE") == Some("debug") {
                panic!("Frame ppn={:#x} has not been allocated!", ppn);
            }
            log::warn!("[frame_dealloc] ignore duplicate ppn={:#x}", ppn);
            return;
        }
        // recycle
        self.mark_recycled(ppn, true);
        self.recycled.push(ppn);
    }
}

impl StackFrameAllocator {
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
}

type FrameAllocatorImpl = StackFrameAllocator;

lazy_static! {
    /// 全局帧分配器。
    pub static ref FRAME_ALLOCATOR: RwLock<FrameAllocatorImpl> =
        RwLock::new(FrameAllocatorImpl::new());
}

/// 初始化全局帧分配器。
pub fn init_frame_allocator() {
    extern "C" {
        // 链接脚本提供的内核镜像结束地址。
        fn ekernel();
    }
    FRAME_ALLOCATOR.write().init(
        // 从内核结束地址ekernel
        PhysAddr::from(ekernel as usize).ceil(),
        // 到内存结束地址
        PhysAddr::from(MEMORY_END).floor(),
        // 作为可用物理内存
    );
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
pub fn frame_reserve(_num: usize) {
}

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

/// 连续分配 `num` 个物理页。
///
/// 通过 `local_irq_save`/`local_irq_restore` 禁止中断抢占，保证 LIFO
/// 栈分配器产出的页物理连续（中断处理中也可能分配帧）。中断关闭窗口仅覆盖
/// 分配循环自身，不包含 `Vec` 预留。
///
/// # Errors
///
/// `Vec` 预留空间失败、任意单页分配失败、或分配页物理不连续时返回 `None`；
/// 已分配帧会随局部变量释放回收。
pub fn frames_alloc(num: usize) -> Option<Vec<Arc<FrameTracker>>> {
    let mut frames = Vec::new();
    if frames.try_reserve(num).is_err() {
        return None;
    }
    let was_enabled = local_irq_save();
    for _ in 0..num {
        if let Some(frame_tracker) = frame_alloc() {
            frames.push(frame_tracker);
        } else {
            local_irq_restore(was_enabled);
            return None;
        }
    }
    local_irq_restore(was_enabled);
    // Verify physical contiguity — LIFO stack may not yield consecutive
    // page numbers after fragmented free patterns.
    if num > 1 {
        let base = frames[0].ppn.0;
        for i in 1..num {
            if frames[i].ppn.0 != base + i {
                return None;
            }
        }
    }
    Some(frames)
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
    let mut allocator = FRAME_ALLOCATOR.write();
    allocator.alloc_fresh(num)
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
