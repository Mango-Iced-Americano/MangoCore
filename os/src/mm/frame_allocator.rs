use super::{PhysAddr, PhysPageNum};
use crate::config::{MEMORY_START, PAGE_SIZE};
use crate::hal::MEMORY_END;
#[cfg(feature = "oom_handler")]
use crate::task::current_task_ref;

use alloc::{sync::Arc, vec::Vec};
use core::fmt::{self, Debug, Formatter};
use lazy_static::*;
use spin::RwLock;

/// 物理帧跟踪器
pub struct FrameTracker {
    /// 跟踪的物理页号
    pub ppn: PhysPageNum,
}

impl FrameTracker {
    pub fn new(ppn: PhysPageNum) -> Self {
        let ptr = ppn.get_dwords_array().as_mut_ptr();
        const WORDS_PER_PAGE: usize = PAGE_SIZE / core::mem::size_of::<u64>();
        const UNROLL: usize = 8;
        let mut i = 0;
        while i + UNROLL <= WORDS_PER_PAGE {
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
            unsafe { ptr.add(i).write(0) };
            i += 1;
        }
        Self { ppn }
    }
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
    // 自动回收物理帧
    fn drop(&mut self) {
        // println!("do drop at {}", self.ppn.0);
        frame_dealloc(self.ppn);
    }
}

/// 帧分配器接口
trait FrameAllocator {
    fn new() -> Self;
    /// 分配
    fn alloc(&mut self) -> Option<FrameTracker>;
    unsafe fn alloc_uninit(&mut self) -> Option<FrameTracker>;
    /// 释放
    fn dealloc(&mut self, ppn: PhysPageNum);
}

/// 栈式帧分配器
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
    /// 初始化方法
    pub fn init(&mut self, l: PhysPageNum, r: PhysPageNum) {
        self.start = l.0;
        self.current = l.0;
        self.end = r.0;
        let last_frames = self.end - self.current;
        self.recycled.reserve(last_frames);
        self.recycled_flags.resize(last_frames, false);
        println!("last {} Physical Frames.", last_frames);
    }
    /// 计算未分配的大小
    pub fn unallocated_frames(&self) -> usize {
        self.end - self.current + self.recycled.len()
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

    /// 分配一个物理页
    fn alloc(&mut self) -> Option<FrameTracker> {
        // 优先使用回收的帧
        if let Some(ppn) = self.recycled.pop() {
            self.mark_recycled(ppn, false);
            let frame_tracker = FrameTracker::new(ppn.into());
            Some(frame_tracker)
        } else if self.current == self.end {
            // 无可用帧
            None
        } else {
            // 否则分配当前页
            self.current += 1;
            #[cfg(not(feature = "zero_init"))]
            let frame_tracker = FrameTracker::new((self.current - 1).into());
            #[cfg(feature = "zero_init")]
            let frame_tracker = unsafe { FrameTracker::new_uninit((self.current - 1).into()) };
            Some(frame_tracker)
        }
    }
    unsafe fn alloc_uninit(&mut self) -> Option<FrameTracker> {
        if let Some(ppn) = self.recycled.pop() {
            self.mark_recycled(ppn, false);
            let frame_tracker = FrameTracker::new_uninit(ppn.into());
            //log::trace!("[frame_alloc_uninit] {:?}", frame_tracker);
            Some(frame_tracker)
        } else if self.current == self.end {
            None
        } else {
            self.current += 1;
            let frame_tracker = FrameTracker::new_uninit((self.current - 1).into());
            Some(frame_tracker)
        }
    }
    /// 释放一个物理页
    fn dealloc(&mut self, ppn: PhysPageNum) {
        let ppn = ppn.0;
        let alloc_start = PhysAddr::from(MEMORY_START).floor().0;
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
    /// 全局帧分配器
    pub static ref FRAME_ALLOCATOR: RwLock<FrameAllocatorImpl> =
        RwLock::new(FrameAllocatorImpl::new());
}
/// 初始化全局帧分配器
pub fn init_frame_allocator() {
    extern "C" {
        // 内核结束地址？
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

/// 尝试使用所有可能的方法来释放制定数量为`req`的页
/// 成功返回Ok(())，失败返回Err(())
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
/// 帧预留机制
/// # 参数
/// + num: 指定要保留的帧数量
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
pub fn frame_reserve(_num: usize) {
    // do nothing
}

#[cfg(feature = "oom_handler")]
/// 带OOM的分配操作
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

pub fn frames_alloc(num: usize) -> Option<Vec<Arc<FrameTracker>>> {
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

#[cfg(not(feature = "oom_handler"))]
/// 常规分配操作
pub fn frame_alloc() -> Option<Arc<FrameTracker>> {
    FRAME_ALLOCATOR
        .write()
        .alloc()
        .map(|frame_tracker| Arc::new(frame_tracker))
}

#[cfg(feature = "oom_handler")]
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
                .alloc_uninit()
                .map(|frame_tracker| Arc::new(frame_tracker))
        }
    }
}

#[cfg(not(feature = "oom_handler"))]
pub unsafe fn frame_alloc_uninit() -> Option<Arc<FrameTracker>> {
    FRAME_ALLOCATOR
        .write()
        .alloc_uninit()
        .map(|frame_tracker| Arc::new(frame_tracker))
}

/// 释放帧
pub fn frame_dealloc(ppn: PhysPageNum) {
    FRAME_ALLOCATOR.write().dealloc(ppn);
}

/// 计算可用帧数量
pub fn unallocated_frames() -> usize {
    FRAME_ALLOCATOR.read().unallocated_frames()
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
