//! 最原始的 buddy allocator 全局堆后端（对照实验，`legacy_buddy_heap` feature 编译时生效）。
//!
//! 目的：在当前内核**其它部分完全不变**的前提下，把全局堆分配器替换回
//! `docs/09_debug/buddy-allocator-scan-drift.md` 描述的最原始实现——
//! `buddy_system_allocator::Heap<32>`：intrusive free-list、dealloc 线性扫描找 buddy、
//! **没有 bitmap guard、没有 metadata、没有 slab**。bootstrap 与 runtime 内存通过
//! `add_to_heap` 并入同一块 buddy，不区分归还原点。
//!
//! 用 `EXTRA_FEATURES=legacy_buddy_heap`（配合 `make build/kernel`）开启；默认关闭时
//! 本文件不被编译，`os/src/mm/heap_allocator.rs`（MetadataHeap + slab）保持不变。

use crate::{config::PAGE_SIZE, hal::KERNEL_BOOTSTRAP_HEAP_SIZE};
use buddy_system_allocator::Heap;
use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

#[inline]
fn memory_perf_time_now() -> usize {
    crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO)
}

/// 单一 free-list buddy，无 slab、无 metadata、无 bitmap。
pub struct LegacyBuddy {
    heap: Mutex<Heap<32>>,
}

unsafe impl Send for LegacyBuddy {}

pub static KERNEL_HEAP_CURRENT_BYTES: AtomicUsize = AtomicUsize::new(0);
pub static KERNEL_HEAP_MAX_BYTES: AtomicUsize = AtomicUsize::new(0);

impl LegacyBuddy {
    pub const fn empty() -> Self {
        Self {
            heap: Mutex::new(Heap::empty()),
        }
    }

    /// 初始化 bootstrap 区。
    ///
    /// # Safety
    ///
    /// `start..start + size` 必须是独占、可写、且与其它分配器/静态对象不重叠的连续内存，
    /// 生命周期贯穿内核全程。
    pub unsafe fn init(&self, start: usize, size: usize) {
        unsafe {
            self.heap.lock().init(start, size);
        }
        KERNEL_HEAP_CURRENT_BYTES.store(0, Ordering::Relaxed);
        KERNEL_HEAP_MAX_BYTES.store(0, Ordering::Relaxed);
    }

    /// 追加 runtime 区（融入同一 buddy，不区分 bootstrap/runtime 归属）。
    ///
    /// # Safety
    ///
    /// 同 `init`：`start..start + size` 必须独占可用。
    pub unsafe fn add_runtime(&self, start: usize, size: usize) {
        unsafe {
            self.heap.lock().add_to_heap(start, start + size);
        }
    }

    fn stats_total_bytes(&self) -> usize {
        self.heap.lock().stats_total_bytes()
    }

    fn stats_alloc_actual(&self) -> usize {
        self.heap.lock().stats_alloc_actual()
    }

    fn stats_alloc_user(&self) -> usize {
        self.heap.lock().stats_alloc_user()
    }
}

#[global_allocator]
static HEAP_ALLOCATOR: LegacyBuddy = LegacyBuddy::empty();

// Safety: `LegacyBuddy` 用内部 Mutex 串行化堆访问，返回指针属于独占的 HEAP_SPACE/runtime 区。
unsafe impl GlobalAlloc for LegacyBuddy {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        crate::task::perf::record_heap_alloc_request(layout.size());
        let lock_start = memory_perf_time_now();
        let mut heap = self.heap.lock();
        let alloc_start = memory_perf_time_now();
        // 线性扫描 buddy：小对象也直接走 free-list，完整复现原报告 `dealloc` 的
        // `for block in free_list.iter_mut()` 扫描路径。
        if let Some(ptr) = heap.alloc(layout).ok() {
            let elapsed = memory_perf_time_now().wrapping_sub(alloc_start);
            crate::task::perf::record_heap_alloc();
            crate::task::perf::record_heap_alloc_cost(elapsed);
            crate::task::perf::record_heap_lock(memory_perf_time_now().wrapping_sub(lock_start), 0, true);
            let charge = layout.size();
            let prev = KERNEL_HEAP_CURRENT_BYTES.fetch_add(charge, Ordering::Relaxed);
            let new_total = prev + charge;
            let mut cur_max = KERNEL_HEAP_MAX_BYTES.load(Ordering::Relaxed);
            while new_total > cur_max {
                match KERNEL_HEAP_MAX_BYTES.compare_exchange_weak(
                    cur_max,
                    new_total,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(v) => cur_max = v,
                }
            }
            return ptr.as_ptr();
        }
        crate::task::perf::record_heap_final_failure();
        core::ptr::null_mut()
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let ptr = match core::ptr::NonNull::new(ptr) {
            Some(p) => p,
            None => return,
        };
        crate::task::perf::record_heap_dealloc();
        let dealloc_start = memory_perf_time_now();
        // 最原始 dealloc：push 回 free-list 后逐级线性扫描找 buddy 合并。
        unsafe {
            self.heap.lock().dealloc(ptr, layout);
        }
        let elapsed = memory_perf_time_now().wrapping_sub(dealloc_start);
        crate::task::perf::record_heap_dealloc_cost(elapsed);
        KERNEL_HEAP_CURRENT_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
    }
}

#[alloc_error_handler]
pub fn handle_alloc_error(layout: core::alloc::Layout) -> ! {
    println!("=== HEAP ALLOCATION FAILED (FATAL, legacy_buddy_heap) ===");
    let syscall_name = crate::task::current_syscall_name();
    println!("triggered by syscall: {}", syscall_name);
    println!("layout: size={}, align={}", layout.size(), layout.align());
    println!("======================================");
    crate::hal::shutdown()
}

/// 返回 `(free_bytes, total_bytes, allocated_user, allocated_actual, internal_waste)`。
pub fn heap_stats() -> (usize, usize, usize, usize, usize) {
    let total = HEAP_ALLOCATOR.stats_total_bytes();
    let alloc_actual = HEAP_ALLOCATOR.stats_alloc_actual();
    let alloc_user = HEAP_ALLOCATOR.stats_alloc_user();
    let free = total.saturating_sub(alloc_actual);
    let waste = alloc_actual.saturating_sub(alloc_user);
    (free, total, alloc_user, alloc_actual, waste)
}

/// panic 等不可等待上下文使用；legacy 后端布局简单，直接等价于 `heap_stats()`。
pub fn try_heap_stats() -> Option<(usize, usize, usize, usize, usize)> {
    Some(heap_stats())
}

/// 每个 order 的空闲块数。旧 `Heap` 无分阶计数统计，返回全零占位。
pub fn heap_free_histogram() -> [usize; 32] {
    [0; 32]
}

/// Early boot fallback storage。与 `heap_allocator.rs` 同名共享同一区间的用途。
static mut HEAP_SPACE: [u8; KERNEL_BOOTSTRAP_HEAP_SIZE] = [0; KERNEL_BOOTSTRAP_HEAP_SIZE];

/// 初始化内核堆（bootstrap）。
pub fn init_heap() {
    unsafe {
        HEAP_ALLOCATOR.init(
            core::ptr::addr_of_mut!(HEAP_SPACE).cast::<u8>() as usize,
            KERNEL_BOOTSTRAP_HEAP_SIZE,
        );
    }
    KERNEL_HEAP_CURRENT_BYTES.store(0, Ordering::Relaxed);
    KERNEL_HEAP_MAX_BYTES.store(0, Ordering::Relaxed);
}

/// 返回 runtime 目标，与 `heap_allocator.rs` 保持同一策略。
const fn runtime_heap_target(usable_memory: usize) -> usize {
    const MIN_HEAP: usize = 64 * 1024 * 1024;
    const MAX_HEAP: usize = 1024 * 1024 * 1024;
    let requested = usable_memory / 10;
    if requested < MIN_HEAP {
        MIN_HEAP
    } else if requested > MAX_HEAP {
        MAX_HEAP
    } else {
        requested
    }
}

/// 把 DRAM 追加进同一 buddy（add_to_heap）。
pub fn init_runtime_heap() {
    let target = runtime_heap_target(crate::hal::firmware::usable_memory_size());
    let extension = target.saturating_sub(KERNEL_BOOTSTRAP_HEAP_SIZE);
    if extension == 0 {
        return;
    }
    let pages = extension.div_ceil(PAGE_SIZE);
    let start = crate::mm::frame_allocator::reserve_fresh_contiguous(pages)
        .expect("runtime heap backing reservation failed");
    let size = pages * PAGE_SIZE;
    unsafe {
        HEAP_ALLOCATOR.add_runtime(start.start_addr().direct_map_ptr() as usize, size);
    }
    let (_, total, _, _, _) = heap_stats();
    println!(
        "[memory] kernel heap (legacy_buddy_heap): target={} usable={} bytes",
        total,
        crate::hal::firmware::usable_memory_size()
    );
    // 启动自检：验证 legacy free-list buddy 返回的页对齐分配正确（怀疑未对齐损坏块路径）。
    let mut bad = 0usize;
    for i in 0..256usize {
        let layout = unsafe { core::alloc::Layout::from_size_align_unchecked(4096, 4096) };
        let p = unsafe { HEAP_ALLOCATOR.alloc(layout) };
        if p.is_null() {
            println!("[legacy-heap] selfcheck alloc {:?} OOM at i={}", layout, i);
            break;
        }
        if (p as usize) % 4096 != 0 {
            bad += 1;
            if bad <= 5 {
                println!("[legacy-heap] selfcheck UNALIGNED 4K ptr={:#x} i={}", p as usize, i);
            }
        }
        unsafe {
            let slice = core::slice::from_raw_parts_mut(p, 4096);
            slice.fill(i as u8);
            let check = slice.iter().all(|&b| b == i as u8);
            if !check {
                println!("[legacy-heap] selfcheck CORRUPT i={}", i);
                break;
            }
            HEAP_ALLOCATOR.dealloc(p, layout);
        }
    }
    println!(
        "[legacy-heap] selfcheck done: 256 alloc/dealloc 4K, unaligned={}",
        bad
    );
}

/// 可用 buddy 容量。
pub fn kernel_heap_size() -> usize {
    heap_stats().1
}

/// heap_trace backtrace 过滤哨兵，语义与 `heap_allocator.rs` 一致。
#[no_mangle]
pub fn heap_allocator_text_marker() {}
