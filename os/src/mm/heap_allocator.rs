//! 内核全局堆分配器。
//!
//! 本模块在 `buddy_system_allocator::Heap` 外包一层 OOM-aware `GlobalAlloc`，
//! 普通分配失败时会先尝试内存回收，再交给 `alloc_error_handler` 做最终诊断和 shutdown。
//!
//! # Locking
//!
//! buddy heap 由 `Mutex` 保护。分配失败后的回收路径在释放 heap 锁之后执行，避免
//! OOM 回收过程中再次分配或释放堆内存时递归持锁。
//!
//! # OOM
//!
//! `alloc` 最多重试三次；仍失败时返回 null，由 Rust 分配路径触发
//! `handle_alloc_error`。

use crate::{config::PAGE_SIZE, hal::KERNEL_HEAP_SIZE};
use buddy_system_allocator::Heap;
use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

/// 全局堆分配器。
///
/// 这里不用 `LockedHeap`，而是包一层 `GlobalAlloc`，这样普通内核堆分配失败时
/// 还能先尝试释放 cache / user page tracker 等可回收对象，再决定是否交给
/// `alloc_error_handler` 处理。
pub struct OomAwareAllocator {
    inner: Mutex<Heap<32>>,
}

static OOM_RECOVERY_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

pub static KERNEL_HEAP_CURRENT_BYTES: AtomicUsize = AtomicUsize::new(0);
pub static KERNEL_HEAP_MAX_BYTES: AtomicUsize = AtomicUsize::new(0);

impl OomAwareAllocator {
    /// 创建一个尚未初始化的分配器。
    pub const fn empty() -> Self {
        Self {
            inner: Mutex::new(Heap::empty()),
        }
    }

    /// 初始化底层 buddy heap。
    ///
    /// # Safety
    ///
    /// `start..start + size` 必须是一段唯一归本分配器管理的可写内存，并且在内核运行期间
    /// 不会被其他分配器或静态对象再次占用。
    pub unsafe fn init(&self, start: usize, size: usize) {
        self.inner.lock().init(start, size);
    }

    fn recover_for(&self, layout: Layout) -> bool {
        // 确保回收原子化，防止多次或递归回收
        if OOM_RECOVERY_IN_PROGRESS
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return false;
        }

        let recovered = self.recover_for_inner(layout);
        OOM_RECOVERY_IN_PROGRESS.store(false, Ordering::Release);
        recovered
    }

    fn recover_for_inner(&self, layout: Layout) -> bool {
        #[cfg(feature = "oom_handler")]
        {
            let pages = layout.size().saturating_add(PAGE_SIZE - 1) / PAGE_SIZE;
            let pages = pages.max(1);
            log::warn!(
                "[heap_alloc] alloc failed: size={}, align={}, try oom recovery for {} pages",
                layout.size(),
                layout.align(),
                pages
            );
            if crate::mm::frame_allocator::oom_handler(pages).is_ok() {
                return true;
            }
            log::warn!("[heap_alloc] oom recovery did not release enough memory");
        }
        #[cfg(not(feature = "oom_handler"))]
        {
            log::warn!(
                "[heap_alloc] alloc failed: size={}, align={}, oom_handler disabled",
                layout.size(),
                layout.align()
            );
        }
        false
    }
}

// Safety: `OomAwareAllocator` 使用内部 `Mutex` 序列化 buddy heap 访问，返回的指针
// 来自初始化时唯一交给它的 `HEAP_SPACE` 区间。
unsafe impl GlobalAlloc for OomAwareAllocator {
    /// 分配一块满足 `layout` 的内核堆内存。
    ///
    /// # Safety
    ///
    /// 遵循 `GlobalAlloc::alloc` 契约：调用者必须用同一分配器和相同 layout 释放返回指针。
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        for _ in 0..3 {
            let mut inner = self.inner.lock();
            let _alloc_start =
                crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
            if let Ok(ptr) = inner.alloc(layout) {
                let elapsed = crate::task::perf::perf_time_now_for(
                    crate::task::perf::STATS_PROFILE_MEMORY_IO,
                )
                .wrapping_sub(_alloc_start);
                crate::task::perf::record_heap_alloc();
                crate::task::perf::record_heap_alloc_cost(elapsed);
                let block_size = layout
                    .size()
                    .max(layout.align())
                    .max(core::mem::size_of::<usize>())
                    .next_power_of_two();
                drop(inner);
                // Perf gauge 使用 buddy 实际 block 大小统计当前占用和峰值。
                let prev = KERNEL_HEAP_CURRENT_BYTES.fetch_add(block_size, Ordering::Relaxed);
                let new_total = prev + block_size;
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
                #[cfg(feature = "heap_trace")]
                crate::mm::heap_trace::record_alloc(ptr.as_ptr(), layout, block_size);
                return ptr.as_ptr();
            }
            let elapsed =
                crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO)
                    .wrapping_sub(_alloc_start);
            crate::task::perf::record_heap_alloc();
            crate::task::perf::record_heap_alloc_cost(elapsed);
            drop(inner);
            if !self.recover_for(layout) {
                break;
            }
        }
        core::ptr::null_mut()
    }

    /// 释放一块内核堆内存。
    ///
    /// # Safety
    ///
    /// `ptr` 和 `layout` 必须来自先前成功的 `alloc` 调用；重复释放或 layout 不匹配会破坏
    /// buddy heap 元数据。
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(ptr) = core::ptr::NonNull::new(ptr) {
            #[cfg(feature = "heap_trace")]
            crate::mm::heap_trace::record_dealloc(ptr.as_ptr());
            let block_size = layout
                .size()
                .max(layout.align())
                .max(core::mem::size_of::<usize>())
                .next_power_of_two();
            crate::task::perf::record_heap_dealloc();
            let _dealloc_start =
                crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
            self.inner.lock().dealloc(ptr, layout);
            let elapsed =
                crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO)
                    .wrapping_sub(_dealloc_start);
            crate::task::perf::record_heap_dealloc_cost(elapsed);
            KERNEL_HEAP_CURRENT_BYTES.fetch_sub(block_size, Ordering::Relaxed);
        }
    }
}

#[global_allocator]
/// 全局堆分配器。
static HEAP_ALLOCATOR: OomAwareAllocator = OomAwareAllocator::empty();

#[alloc_error_handler]
/// 分配错误处理（带诊断信息和安全 shutdown）。
///
/// # Semantics
///
/// 打印触发 syscall、失败 layout 和 heap trace 后直接关闭内核。
///
/// # Locking
///
/// 这里绝不能调用 `exit_current_and_run_next`。`handle_alloc_error` 是 `-> !`
/// 发散函数，不能 unwind 当前栈；如果从 syscall handler 内部直接调度走，栈上的
/// `Mutex`/`RwLock` guard 永远不会释放，后续任务可能死锁或破坏文件系统状态。
pub fn handle_alloc_error(layout: core::alloc::Layout) -> ! {
    println!("=== HEAP ALLOCATION FAILED (FATAL) ===");
    let syscall_name = crate::task::current_syscall_name();
    println!("triggered by syscall: {}", syscall_name);
    println!("layout: size={}, align={}", layout.size(), layout.align());
    println!("KERNEL_HEAP_SIZE: {} bytes", KERNEL_HEAP_SIZE);
    #[cfg(feature = "heap_trace")]
    crate::mm::heap_trace::dump_oom(layout);
    println!("======================================");
    crate::hal::shutdown()
}

/// 返回 `(free_bytes, total_bytes, allocated_user, allocated_actual, internal_waste)`。
///
/// `internal_waste = allocated_actual - allocated_user`，表示 buddy block 对齐造成的内部碎片。
pub fn heap_stats() -> (usize, usize, usize, usize, usize) {
    let heap = HEAP_ALLOCATOR.inner.lock();
    let total = heap.stats_total_bytes();
    let alloc_actual = heap.stats_alloc_actual();
    let alloc_user = heap.stats_alloc_user();
    let free = total.saturating_sub(alloc_actual);
    let waste = alloc_actual.saturating_sub(alloc_user);
    (free, total, alloc_user, alloc_actual, waste)
}

/// 返回每个 order 的空闲块数（order 0 = 1B，order 16 = 64 KiB）。
pub fn heap_free_histogram() -> [usize; 32] {
    HEAP_ALLOCATOR.inner.lock().free_block_counts()
}

/// 全局堆内存空间。
static mut HEAP_SPACE: [u8; KERNEL_HEAP_SIZE] = [0; KERNEL_HEAP_SIZE];

/// 初始化内核堆。
pub fn init_heap() {
    // Safety: `HEAP_SPACE` 是本模块唯一的静态堆缓冲区，初始化只在启动早期执行一次。
    unsafe {
        HEAP_ALLOCATOR.init(HEAP_SPACE.as_ptr() as usize, KERNEL_HEAP_SIZE);
    }
    KERNEL_HEAP_CURRENT_BYTES.store(0, Ordering::Relaxed);
    KERNEL_HEAP_MAX_BYTES.store(0, Ordering::Relaxed);
    // Safety: 该全局 hook 仅在启动期写入一次，指向常驻的 perf 统计函数。
    unsafe {
        buddy_system_allocator::DEALLOC_SCAN_HOOK =
            crate::task::perf::record_heap_dealloc_scan_steps;
    }
}

#[allow(unused)]
/// 启动期堆分配自检。
pub fn heap_test() {
    use alloc::boxed::Box;
    use alloc::vec::Vec;
    extern "C" {
        fn sbss();
        fn ebss();
    }
    let bss_range = sbss as usize..ebss as usize;
    let a = Box::new(5);
    assert_eq!(*a, 5);
    assert!(bss_range.contains(&(a.as_ref() as *const _ as usize)));
    drop(a);
    let mut v: Vec<usize> = Vec::new();
    for i in 0..500 {
        v.push(i);
    }
    for i in 0..500 {
        assert_eq!(v[i], i);
    }
    assert!(bss_range.contains(&(v.as_ptr() as usize)));
    drop(v);
    println!("heap_test passed!");
}

/// Sentinel function for heap_trace backtrace filtering.
/// `first_useful_pc` skips any PC within ±8 KB of this function,
/// so allocator-internal frames are never reported as allocation sites.
#[no_mangle]
pub fn heap_allocator_text_marker() {}
