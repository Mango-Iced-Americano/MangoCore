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
    pub const fn empty() -> Self {
        Self {
            inner: Mutex::new(Heap::empty()),
        }
    }

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

unsafe impl GlobalAlloc for OomAwareAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        for _ in 0..3 {
            let mut inner = self.inner.lock();
            if let Ok(ptr) = inner.alloc(layout) {
                let block_size = layout.size()
                    .max(layout.align())
                    .max(core::mem::size_of::<usize>())
                    .next_power_of_two();
                drop(inner);
                // Perf gauge: track current allocation and peak
                let prev = KERNEL_HEAP_CURRENT_BYTES.fetch_add(block_size, Ordering::Relaxed);
                let new_total = prev + block_size;
                let mut cur_max = KERNEL_HEAP_MAX_BYTES.load(Ordering::Relaxed);
                while new_total > cur_max {
                    match KERNEL_HEAP_MAX_BYTES.compare_exchange_weak(cur_max, new_total, Ordering::Relaxed, Ordering::Relaxed) {
                        Ok(_) => break,
                        Err(v) => cur_max = v,
                    }
                }
                #[cfg(feature = "heap_trace")]
                crate::mm::heap_trace::record_alloc(ptr.as_ptr(), layout, block_size);
                return ptr.as_ptr();
            }
            drop(inner);
            if !self.recover_for(layout) {
                break;
            }
        }
        core::ptr::null_mut()
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(ptr) = core::ptr::NonNull::new(ptr) {
            #[cfg(feature = "heap_trace")]
            crate::mm::heap_trace::record_dealloc(ptr.as_ptr());
            let block_size = layout.size()
                .max(layout.align())
                .max(core::mem::size_of::<usize>())
                .next_power_of_two();
            self.inner.lock().dealloc(ptr, layout);
            KERNEL_HEAP_CURRENT_BYTES.fetch_sub(block_size, Ordering::Relaxed);
        }
    }
}

#[global_allocator]
/// 全局堆分配器
static HEAP_ALLOCATOR: OomAwareAllocator = OomAwareAllocator::empty();

// 标记为全局分配错误处理器
#[alloc_error_handler]
/// 分配错误处理（带诊断信息 + 安全 shutdown）
///
/// 行为：
/// - 打印完整诊断信息（syscall 名、堆统计）
/// - 直接 shutdown 内核
///
/// 注意：绝不能在这里调用 exit_current_and_run_next！因为 handle_alloc_error 是
/// `-> !` 发散函数，无法 unwinding 调用栈。如果从内核代码中（syscall handler 内部）
/// 调用 exit_current_and_run_next 调度走，被杀死任务栈上的锁守卫（Mutex/RwLock 等）
/// 永远得不到释放，导致后续任务死锁或文件系统损坏。
/// 正确的做法是：在 alloc() 中做好多次重试+OOM recovery，只有在万不得已时才 shutdown。
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

/// 返回 (free_bytes, total_bytes, allocated_user, allocated_actual, internal_waste)
/// where internal_waste = allocated_actual - allocated_user (fragmentation overhead)
pub fn heap_stats() -> (usize, usize, usize, usize, usize) {
    let heap = HEAP_ALLOCATOR.inner.lock();
    let total = heap.stats_total_bytes();
    let alloc_actual = heap.stats_alloc_actual();
    let alloc_user = heap.stats_alloc_user();
    let free = total.saturating_sub(alloc_actual);
    let waste = alloc_actual.saturating_sub(alloc_user);
    (free, total, alloc_user, alloc_actual, waste)
}

/// 返回每 order 的空闲块数（order 0 = 1B, order 16 = 64KB, ...）
pub fn heap_free_histogram() -> [usize; 32] {
    HEAP_ALLOCATOR.inner.lock().free_block_counts()
}

/// 全局堆内存空间
static mut HEAP_SPACE: [u8; KERNEL_HEAP_SIZE] = [0; KERNEL_HEAP_SIZE];

/// 初始化用于内核加载开始时的堆
pub fn init_heap() {
    unsafe {
        // 起始地址和大小
        HEAP_ALLOCATOR.init(HEAP_SPACE.as_ptr() as usize, KERNEL_HEAP_SIZE);
    }
    KERNEL_HEAP_CURRENT_BYTES.store(0, Ordering::Relaxed);
    KERNEL_HEAP_MAX_BYTES.store(0, Ordering::Relaxed);
}

#[allow(unused)]
/// 堆测试函数
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
