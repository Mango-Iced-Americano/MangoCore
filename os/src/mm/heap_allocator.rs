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

use crate::{config::PAGE_SIZE, hal::KERNEL_BOOTSTRAP_HEAP_SIZE};
use buddy_system_allocator::{AllocError as PageAllocError, MetadataHeap, PageOrder, PageRun};
use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

use crate::mm::slab::{direct_charge, slab_class_for, PageAllocator, SlabAllocator};

#[inline]
fn memory_perf_time_now() -> usize {
    crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO)
}

/// Bootstrap storage is retained because frame allocator metadata outlives boot.
/// The runtime heap is a second, disjoint MetadataHeap because MetadataHeap owns
/// metadata inside one contiguous backing range and has no add-memory API.
struct KernelHeap {
    bootstrap: MetadataHeap<32, 12>,
    runtime: Option<MetadataHeap<32, 12>>,
    runtime_backing: Option<core::ops::Range<usize>>,
}

impl KernelHeap {
    const fn empty() -> Self {
        Self {
            bootstrap: MetadataHeap::empty(),
            runtime: None,
            runtime_backing: None,
        }
    }

    unsafe fn init_bootstrap(&mut self, start: usize, size: usize) {
        self.bootstrap
            .try_init(start, size)
            .expect("kernel bootstrap heap init failed");
    }

    unsafe fn add_runtime(&mut self, start: usize, size: usize) {
        assert!(self.runtime.is_none(), "runtime heap initialized twice");
        let mut runtime = MetadataHeap::empty();
        runtime
            .try_init(start, size)
            .expect("kernel runtime heap init failed");
        self.runtime_backing = Some(start..start + size);
        self.runtime = Some(runtime);
    }

    fn runtime_owns(&self, address: usize) -> bool {
        self.runtime_backing
            .as_ref()
            .is_some_and(|range| range.contains(&address))
    }

    fn alloc(&mut self, layout: Layout) -> Result<core::ptr::NonNull<u8>, ()> {
        if let Some(runtime) = self.runtime.as_mut() {
            if let Ok(ptr) = runtime.alloc(layout) {
                return Ok(ptr);
            }
        }
        self.bootstrap.alloc(layout)
    }

    unsafe fn dealloc(&mut self, ptr: core::ptr::NonNull<u8>, layout: Layout) {
        if self.runtime_owns(ptr.as_ptr() as usize) {
            unsafe {
                self.runtime
                    .as_mut()
                    .expect("runtime heap backing without heap metadata")
                    .dealloc(ptr, layout);
            }
        } else {
            unsafe { self.bootstrap.dealloc(ptr, layout) };
        }
    }

    fn alloc_pages(&mut self, order: PageOrder) -> Result<PageRun, PageAllocError> {
        if let Some(runtime) = self.runtime.as_mut() {
            if let Ok(run) = runtime.alloc_pages(order) {
                return Ok(run);
            }
        }
        self.bootstrap.alloc_pages(order)
    }

    unsafe fn dealloc_pages(&mut self, run: PageRun) {
        if self.runtime_owns(run.base.as_ptr() as usize) {
            unsafe {
                self.runtime
                    .as_mut()
                    .expect("runtime heap backing without heap metadata")
                    .dealloc_pages(run);
            }
        } else {
            unsafe { self.bootstrap.dealloc_pages(run) };
        }
    }

    fn stats_total_bytes(&self) -> usize {
        self.bootstrap.stats_total_bytes()
            + self
                .runtime
                .as_ref()
                .map_or(0, MetadataHeap::stats_total_bytes)
    }

    fn stats_alloc_actual(&self) -> usize {
        self.bootstrap.stats_alloc_actual()
            + self
                .runtime
                .as_ref()
                .map_or(0, MetadataHeap::stats_alloc_actual)
    }

    fn stats_alloc_user(&self) -> usize {
        self.bootstrap.stats_alloc_user()
            + self
                .runtime
                .as_ref()
                .map_or(0, MetadataHeap::stats_alloc_user)
    }

    fn free_block_counts(&self) -> [usize; 32] {
        let mut counts = self.bootstrap.free_block_counts();
        if let Some(runtime) = self.runtime.as_ref() {
            for (total, count) in counts.iter_mut().zip(runtime.free_block_counts()) {
                *total += count;
            }
        }
        counts
    }
}

/// Thin adapter that implements our PageAllocator trait on the composite heap.
struct HeapPageAlloc<'a>(&'a mut KernelHeap);

impl PageAllocator for HeapPageAlloc<'_> {
    fn alloc_pages(&mut self, order: PageOrder) -> Result<PageRun, PageAllocError> {
        self.0.alloc_pages(order)
    }
    unsafe fn dealloc_pages(&mut self, run: PageRun) {
        unsafe { self.0.dealloc_pages(run) }
    }
}

/// Inner state protected by the per-allocator mutex.
struct KernelHeapInner {
    heap: KernelHeap,
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
                heap: KernelHeap::empty(),
                slab: SlabAllocator::empty(),
            }),
        }
    }

    /// Initialise bootstrap buddy storage before frame allocator metadata exists.
    ///
    /// # Safety
    ///
    /// `start..start + size` must be a unique, writable memory region that is never
    /// used by any other allocator or static object for the kernel's lifetime.
    pub unsafe fn init(&self, start: usize, size: usize) {
        let mut inner = self.inner.lock();
        unsafe { inner.heap.init_bootstrap(start, size) };
        inner.slab.init();
    }

    /// Add runtime backing after the frame allocator can permanently reserve it.
    unsafe fn add_runtime(&self, start: usize, size: usize) {
        let mut inner = self.inner.lock();
        unsafe { inner.heap.add_runtime(start, size) };
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
            let lock_start = memory_perf_time_now();
            let mut guard = self.inner.lock();
            let lock_acquired = memory_perf_time_now();
            let inner = &mut *guard;
            let alloc_start = memory_perf_time_now();
            let slab_class = slab_class_for(layout).map(|(_, bytes)| bytes);

            // Try the slab first for small objects.
            if let Some(result) = {
                let mut heap_alloc = HeapPageAlloc(&mut inner.heap);
                inner.slab.alloc(&mut heap_alloc, layout)
            } {
                let elapsed = memory_perf_time_now().wrapping_sub(alloc_start);
                crate::task::perf::record_heap_alloc();
                crate::task::perf::record_heap_alloc_cost(elapsed);
                crate::task::perf::record_heap_alloc_path(slab_class);
                let charge = result.charge;
                let ptr = result.ptr;
                crate::task::perf::record_heap_lock(
                    lock_acquired.wrapping_sub(lock_start),
                    memory_perf_time_now().wrapping_sub(lock_acquired),
                );
                drop(guard);
                self.record_charge(charge);
                #[cfg(feature = "heap_trace")]
                crate::mm::heap_trace::record_alloc(ptr.as_ptr(), layout, charge);
                return ptr.as_ptr();
            }

            // Direct buddy allocation for objects too large for slab.
            match inner.heap.alloc(layout) {
                Ok(ptr) => {
                    let elapsed = memory_perf_time_now().wrapping_sub(alloc_start);
                    crate::task::perf::record_heap_alloc();
                    crate::task::perf::record_heap_alloc_cost(elapsed);
                    crate::task::perf::record_heap_alloc_path(None);
                    let charge = direct_charge(layout);
                    crate::task::perf::record_heap_lock(
                        lock_acquired.wrapping_sub(lock_start),
                        memory_perf_time_now().wrapping_sub(lock_acquired),
                    );
                    drop(guard);
                    self.record_charge(charge);
                    #[cfg(feature = "heap_trace")]
                    crate::mm::heap_trace::record_alloc(ptr.as_ptr(), layout, charge);
                    return ptr.as_ptr();
                }
                Err(_) => {
                    let elapsed = memory_perf_time_now().wrapping_sub(alloc_start);
                    crate::task::perf::record_heap_alloc();
                    crate::task::perf::record_heap_alloc_cost(elapsed);
                    crate::task::perf::record_heap_alloc_path(None);
                    crate::task::perf::record_heap_lock(
                        lock_acquired.wrapping_sub(lock_start),
                        memory_perf_time_now().wrapping_sub(lock_acquired),
                    );
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
        let dealloc_start = memory_perf_time_now();

        let lock_start = memory_perf_time_now();
        let mut guard = self.inner.lock();
        let lock_acquired = memory_perf_time_now();
        let inner = &mut *guard;

        // Route to slab if layout fits a slab class.
        if slab_class_for(layout).is_some() {
            let mut heap_alloc = HeapPageAlloc(&mut inner.heap);
            unsafe { inner.slab.dealloc(&mut heap_alloc, ptr, layout) };
            let charge = slab_class_for(layout).unwrap().1;
            let elapsed = memory_perf_time_now().wrapping_sub(dealloc_start);
            crate::task::perf::record_heap_dealloc_cost(elapsed);
            crate::task::perf::record_heap_lock(
                lock_acquired.wrapping_sub(lock_start),
                memory_perf_time_now().wrapping_sub(lock_acquired),
            );
            drop(guard);
            KERNEL_HEAP_CURRENT_BYTES.fetch_sub(charge, Ordering::Relaxed);
        } else {
            unsafe { inner.heap.dealloc(ptr, layout) };
            let charge = direct_charge(layout);
            let elapsed = memory_perf_time_now().wrapping_sub(dealloc_start);
            crate::task::perf::record_heap_dealloc_cost(elapsed);
            crate::task::perf::record_heap_lock(
                lock_acquired.wrapping_sub(lock_start),
                memory_perf_time_now().wrapping_sub(lock_acquired),
            );
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
    println!("KERNEL_HEAP_SIZE: {} bytes", kernel_heap_size());
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
    heap_stats_locked(&inner)
}

/// panic 等不可等待上下文使用的堆统计；锁忙时立即返回 `None`。
pub fn try_heap_stats() -> Option<(usize, usize, usize, usize, usize)> {
    let inner = HEAP_ALLOCATOR.inner.try_lock()?;
    Some(heap_stats_locked(&inner))
}

fn heap_stats_locked(inner: &KernelHeapInner) -> (usize, usize, usize, usize, usize) {
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

/// Early boot fallback storage, retained for frame allocator metadata allocated
/// before runtime DRAM backing can be reserved.
static mut HEAP_SPACE: [u8; KERNEL_BOOTSTRAP_HEAP_SIZE] = [0; KERNEL_BOOTSTRAP_HEAP_SIZE];

/// 初始化内核堆。
pub fn init_heap() {
    // SAFETY: [Category 13 — GlobalAlloc contract] `HEAP_SPACE` is exclusively
    // owned by this allocator, and the single-threaded boot path calls init once
    // before any allocation can occur. `addr_of_mut!` creates no reference to the
    // mutable static, so it preserves the static-mut aliasing invariant.
    unsafe {
        HEAP_ALLOCATOR.init(
            core::ptr::addr_of_mut!(HEAP_SPACE).cast::<u8>() as usize,
            KERNEL_BOOTSTRAP_HEAP_SIZE,
        );
    }
    KERNEL_HEAP_CURRENT_BYTES.store(0, Ordering::Relaxed);
    KERNEL_HEAP_MAX_BYTES.store(0, Ordering::Relaxed);
}

/// Return the runtime target after clamping usable RAM to the kernel heap policy.
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

/// Expand the bootstrap allocator with DRAM after frame allocator setup.
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
    // SAFETY: reserve_fresh_contiguous permanently removes this disjoint extent
    // from FRAME_ALLOCATOR; direct_map_ptr makes it accessible before activation.
    unsafe {
        HEAP_ALLOCATOR.add_runtime(start.start_addr().direct_map_ptr() as usize, size);
    }
    let (_, total, _, _, _) = heap_stats();
    println!("[memory] kernel heap: target={} usable={} bytes", total, crate::hal::firmware::usable_memory_size());
}

/// Return usable buddy capacity, including bootstrap and runtime backing.
pub fn kernel_heap_size() -> usize {
    heap_stats().1
}

#[expect(
    dead_code,
    reason = "manual boot-time allocator diagnostic is retained for bring-up investigations"
)]
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
