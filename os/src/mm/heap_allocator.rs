//! 内核全局堆分配器。
//!
//! 本模块在 `buddy_system_allocator::MetadataHeap` 外包一层 OOM-aware `GlobalAlloc`，
//! 配合 slab allocator 处理小对象，大对象直接走 buddy heap。分配失败时会先尝试内存回收，
//! 再交给 `alloc_error_handler` 做最终诊断和 shutdown。
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
use buddy_system_allocator::{MetadataHeap, PageOrder, PageRun, AllocError as PageAllocError};
use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

use crate::mm::slab::{SlabAllocator, SlabAllocResult, slab_class_for, direct_charge, PageAllocator};

/// Thin adapter that implements our PageAllocator trait on MetadataHeap<32, 12>.
struct HeapPageAlloc<'a>(&'a mut MetadataHeap<32, 12>);

impl PageAllocator for HeapPageAlloc<'_> {
    fn alloc_pages(&mut self, order: PageOrder) -> Result<PageRun, PageAllocError> {
        self.0.alloc_pages(order)
    }
    unsafe fn dealloc_pages(&mut self, run: PageRun) {
        self.0.dealloc_pages(run)
    }
}

/// Inner state protected by the per-allocator mutex.
struct KernelHeapInner {
    heap: MetadataHeap<32, 12>,
    slab: SlabAllocator,
}

/// Kernel global allocator — slab for small objects, buddy for everything else.
pub struct KernelAllocator {
    inner: Mutex<KernelHeapInner>,
}

static OOM_RECOVERY_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

pub static KERNEL_HEAP_CURRENT_BYTES: AtomicUsize = AtomicUsize::new(0);
pub static KERNEL_HEAP_MAX_BYTES: AtomicUsize = AtomicUsize::new(0);

impl KernelAllocator {
    /// Create an uninitialised allocator.
    pub const fn empty() -> Self {
        Self {
            inner: Mutex::new(KernelHeapInner {
                heap: MetadataHeap::empty(),
                slab: SlabAllocator::empty(),
            }),
        }
    }

    /// Initialise the underlying buddy heap.
    ///
    /// # Safety
    ///
    /// `start..start + size` must be a unique, writable memory region that is never
    /// used by any other allocator or static object for the kernel's lifetime.
    pub unsafe fn init(&self, start: usize, size: usize) {
        let mut inner = self.inner.lock();
        inner.heap.try_init(start, size).expect("kernel heap init failed");
        inner.slab.init();
    }

    fn recover_for(&self, layout: Layout) -> bool {
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

    fn record_charge(&self, charge: usize) {
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
    }
}

// Safety: `KernelAllocator` uses an internal `Mutex` to serialise heap access;
// returned pointers come from the `HEAP_SPACE` region given exclusively to it.
unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        for _ in 0..3 {
            let mut guard = self.inner.lock();
            let inner = &mut *guard;
            let _alloc_start = crate::task::perf::perf_time_now();

            // Try the slab first for small objects.
            if let Some(result) = {
                let mut heap_alloc = HeapPageAlloc(&mut inner.heap);
                inner.slab.alloc(&mut heap_alloc, layout)
            } {
                let elapsed = crate::task::perf::perf_time_now().wrapping_sub(_alloc_start);
                crate::task::perf::record_heap_alloc();
                crate::task::perf::record_heap_alloc_cost(elapsed);
                let charge = result.charge;
                let ptr = result.ptr;
                drop(guard);
                self.record_charge(charge);
                #[cfg(feature = "heap_trace")]
                crate::mm::heap_trace::record_alloc(ptr.as_ptr(), layout, charge);
                return ptr.as_ptr();
            }

            // Direct buddy allocation for objects too large for slab.
            match inner.heap.alloc(layout) {
                Ok(ptr) => {
                    let elapsed = crate::task::perf::perf_time_now().wrapping_sub(_alloc_start);
                    crate::task::perf::record_heap_alloc();
                    crate::task::perf::record_heap_alloc_cost(elapsed);
                    let charge = direct_charge(layout);
                    drop(guard);
                    self.record_charge(charge);
                    #[cfg(feature = "heap_trace")]
                    crate::mm::heap_trace::record_alloc(ptr.as_ptr(), layout, charge);
                    return ptr.as_ptr();
                }
                Err(_) => {
                    let elapsed = crate::task::perf::perf_time_now().wrapping_sub(_alloc_start);
                    crate::task::perf::record_heap_alloc();
                    crate::task::perf::record_heap_alloc_cost(elapsed);
                    drop(guard);
                    if !self.recover_for(layout) {
                        break;
                    }
                }
            }
        }
        core::ptr::null_mut()
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let ptr = match core::ptr::NonNull::new(ptr) {
            Some(p) => p,
            None => return,
        };

        #[cfg(feature = "heap_trace")]
        crate::mm::heap_trace::record_dealloc(ptr.as_ptr());

        crate::task::perf::record_heap_dealloc();
        let _dealloc_start = crate::task::perf::perf_time_now();

        let mut guard = self.inner.lock();
        let inner = &mut *guard;

        // Route to slab if layout fits a slab class.
        if slab_class_for(layout).is_some() {
            let mut heap_alloc = HeapPageAlloc(&mut inner.heap);
            unsafe { inner.slab.dealloc(&mut heap_alloc, ptr, layout) };
            let charge = slab_class_for(layout).unwrap().1;
            let elapsed = crate::task::perf::perf_time_now().wrapping_sub(_dealloc_start);
            crate::task::perf::record_heap_dealloc_cost(elapsed);
            drop(guard);
            KERNEL_HEAP_CURRENT_BYTES.fetch_sub(charge, Ordering::Relaxed);
        } else {
            unsafe { inner.heap.dealloc(ptr, layout) };
            let charge = direct_charge(layout);
            let elapsed = crate::task::perf::perf_time_now().wrapping_sub(_dealloc_start);
            crate::task::perf::record_heap_dealloc_cost(elapsed);
            drop(guard);
            KERNEL_HEAP_CURRENT_BYTES.fetch_sub(charge, Ordering::Relaxed);
        }
    }
}

#[global_allocator]
/// 全局堆分配器。
static HEAP_ALLOCATOR: KernelAllocator = KernelAllocator::empty();

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
/// `allocated_user` 包含 slab 用户字节 + MetadataHeap 直接分配的用户字节。
pub fn heap_stats() -> (usize, usize, usize, usize, usize) {
    let inner = HEAP_ALLOCATOR.inner.lock();
    let total = inner.heap.stats_total_bytes();
    let alloc_actual = inner.heap.stats_alloc_actual();
    let buddy_user = inner.heap.stats_alloc_user();
    let slab_user = inner.slab.slab_user_bytes();
    let alloc_user = buddy_user + slab_user;
    let free = total.saturating_sub(alloc_actual);
    let waste = alloc_actual.saturating_sub(alloc_user);
    (free, total, alloc_user, alloc_actual, waste)
}

/// 返回每个 order 的空闲块数（order 12+ = 4 KiB+ 页面）。
pub fn heap_free_histogram() -> [usize; 32] {
    HEAP_ALLOCATOR.inner.lock().heap.free_block_counts()
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
