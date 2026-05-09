use crate::hal::KERNEL_HEAP_SIZE;
use buddy_system_allocator::Heap;
use core::alloc::{GlobalAlloc, Layout};
use spin::Mutex;

/// 具备 OOM recovery 能力的全局堆分配器
pub struct OomAwareAllocator {
    inner: Mutex<Heap<32>>,
}

impl OomAwareAllocator {
    /// 创建一个空的分配器
    pub const fn empty() -> Self {
        Self {
            inner: Mutex::new(Heap::empty()),
        }
    }

    /// 初始化堆内存
    pub fn init(&self, start: usize, size: usize) {
        unsafe {
            self.inner.lock().init(start, size);
        }
    }

    /// 尝试 OOM recovery
    fn try_oom_recovery(&self, layout: Layout) -> bool {
        #[cfg(feature = "oom_handler")]
        {
            let page_size = 0x1000usize;
            let pages = (layout.size() + page_size - 1) / page_size;
            log::warn!(
                "[OomAwareAllocator] alloc failed (size={}, align={}), triggering OOM recovery ({} pages)...",
                layout.size(),
                layout.align(),
                pages,
            );
            if crate::task::do_oom(pages).is_ok() {
                log::info!("[OomAwareAllocator] OOM recovery succeeded, retrying allocation");
                return true;
            }
            log::error!("[OomAwareAllocator] OOM recovery failed (no memory released)");
        }
        #[cfg(not(feature = "oom_handler"))]
        {
            log::warn!(
                "[OomAwareAllocator] heap exhausted (size={}, align={}), enable `oom_handler` feature for recovery",
                layout.size(),
                layout.align(),
            );
        }
        false
    }
}

unsafe impl GlobalAlloc for OomAwareAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // 第一次尝试
        if let Ok(ptr) = self.inner.lock().alloc(layout) {
            return ptr.as_ptr();
        }
        // 第一次失败了，尝试 OOM recovery
        if self.try_oom_recovery(layout) {
            match self.inner.lock().alloc(layout) {
                Ok(ptr) => ptr.as_ptr(),
                Err(_) => core::ptr::null_mut(),
            }
        } else {
            core::ptr::null_mut()
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(non_null) = core::ptr::NonNull::new(ptr) {
            self.inner.lock().dealloc(non_null, layout);
        }
    }
}

#[global_allocator]
/// 全局堆分配器
static HEAP_ALLOCATOR: OomAwareAllocator = OomAwareAllocator::empty();

// 标记为全局分配错误处理器
#[alloc_error_handler]
/// 分配错误处理（带诊断信息）
pub fn handle_alloc_error(layout: core::alloc::Layout) -> ! {
    // 收集诊断信息（注意：log 走 UART 直出，不会再次分配）
    let timer_queue_size = crate::task::kernel_timer_queue_len();
    let task_info = crate::task::task_manager_counts();

    log::error!("=== HEAP ALLOCATION FAILED ===");
    log::error!("layout: size={}, align={}", layout.size(), layout.align());
    match timer_queue_size {
        Some(sz) => log::error!("KERNEL_TIMER_QUEUE entries: {}", sz),
        None => log::error!("KERNEL_TIMER_QUEUE: locked"),
    }
    match task_info {
        Some((r, i)) => log::error!("tasks: ready={}, interruptible={}", r, i),
        None => log::error!("TASK_MANAGER: locked"),
    }
    log::error!("KERNEL_HEAP_SIZE: {} bytes", KERNEL_HEAP_SIZE);
    log::error!("=============================");

    panic!("Heap allocation failed, layout = {:?}", layout);
}

/// 全局堆内存空间
static mut HEAP_SPACE: [u8; KERNEL_HEAP_SIZE] = [0; KERNEL_HEAP_SIZE];

/// 初始化用于内核加载开始时的堆
pub fn init_heap() {
    unsafe {
        HEAP_ALLOCATOR.init(HEAP_SPACE.as_ptr() as usize, KERNEL_HEAP_SIZE);
    }
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
