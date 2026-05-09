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
        // 多次尝试：每次失败后触发 OOM recovery，最多 3 轮
        for attempt in 0..3 {
            if let Ok(ptr) = self.inner.lock().alloc(layout) {
                return ptr.as_ptr();
            }
            if attempt < 2 {
                log::warn!(
                    "[OomAwareAllocator] alloc attempt {} failed (size={}, align={}), retrying...",
                    attempt + 1,
                    layout.size(),
                    layout.align(),
                );
                if self.try_oom_recovery(layout) {
                    continue;
                }
            }
        }
        // 所有尝试均失败 — 设置当前任务的 OOM kill pending 标志
        // 该标志将在 trap_return 中被检查，然后 SIGKILL 发送给本进程
        if let Some(task) = crate::task::current_task() {
            task.acquire_inner_lock().pending_oom_kill = true;
            // drop task Arc 引用，避免 refcount 泄漏
            drop(task);
        }
        core::ptr::null_mut()
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
    // 收集诊断信息（注意：print 走 UART 直出，不会再次分配）
    let timer_queue_size = crate::task::kernel_timer_queue_len();
    let task_info = crate::task::task_manager_counts();
    let syscall_name = crate::task::current_syscall_name();

    println!("=== HEAP ALLOCATION FAILED (FATAL) ===");
    println!("layout: size={}, align={}", layout.size(), layout.align());
    println!("current syscall: {}", syscall_name);
    match timer_queue_size {
        Some(sz) => println!("KERNEL_TIMER_QUEUE entries: {}", sz),
        None => println!("KERNEL_TIMER_QUEUE: locked"),
    }
    match task_info {
        Some((r, i)) => println!("tasks: ready={}, interruptible={}", r, i),
        None => println!("TASK_MANAGER: locked"),
    }
    println!("KERNEL_HEAP_SIZE: {} bytes", KERNEL_HEAP_SIZE);
    println!("=============================");
    println!("Shutting down due to unrecoverable heap exhaustion.");
    println!("=============================");

    crate::hal::shutdown()
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
