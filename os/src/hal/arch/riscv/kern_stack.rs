//! RISC-V 用户栈、trap context 和内核栈地址分配。
//!
//! 通过 slot allocator 为线程分配互不重叠的栈和 trap context 虚拟地址。

use super::config::{
    KERNEL_STACK_SIZE, PAGE_SIZE, TRAMPOLINE, TRAP_CONTEXT_BASE, USER_STACK_BASE, USER_STACK_SIZE,
};
use crate::mm::{MapPermission, VirtAddr, KERNEL_SPACE};
use crate::task::pid::RecycleAllocator;
use alloc::vec::Vec;
use lazy_static::*;
use spin::Mutex;

lazy_static! {
    static ref KSTACK_ALLOCATOR: Mutex<RecycleAllocator> = Mutex::new(RecycleAllocator::new());
    static ref KSTACK_CACHE: Mutex<Vec<usize>> = Mutex::new(Vec::new());
}

/// 析构上下文可能仍持有进程锁，因此这里只登记待退休 slot；真正的
/// PTE 修改与跨核等待由无锁调度安全点执行。
static KSTACK_RETIRE_QUEUE: Mutex<
    crate::hal::KernelStackRetireQueue<{ super::config::SYSTEM_TASK_LIMIT }>,
> = Mutex::new(crate::hal::KernelStackRetireQueue::new());

/// Return (bottom, top) of a kernel stack in kernel space.
pub fn kernel_stack_position(kstack_id: usize) -> (usize, usize) {
    let top = TRAMPOLINE - kstack_id * (KERNEL_STACK_SIZE + PAGE_SIZE);
    let bottom = top - KERNEL_STACK_SIZE;
    (bottom, top)
}

pub struct KernelStack(pub usize);

pub fn kstack_alloc() -> KernelStack {
    if let Some(kstack_id) = KSTACK_CACHE.lock().pop() {
        crate::task::perf::record_kstack_alloc(true);
        return KernelStack(kstack_id);
    }
    crate::task::perf::record_kstack_alloc(false);
    let kstack_id = KSTACK_ALLOCATOR.lock().alloc();
    let (kstack_bottom, kstack_top) = kernel_stack_position(kstack_id);
    KERNEL_SPACE.lock().insert_kernel_stack_area(
        kstack_bottom.into(),
        kstack_top.into(),
        MapPermission::R | MapPermission::W,
    );
    KernelStack(kstack_id)
}

impl Drop for KernelStack {
    fn drop(&mut self) {
        let mut cache = KSTACK_CACHE.lock();
        if cache.len() < crate::hal::KERNEL_STACK_CACHE_LIMIT {
            cache.push(self.0);
            crate::task::perf::record_kstack_drop(true);
            return;
        }
        drop(cache);
        crate::task::perf::record_kstack_drop(false);
        KSTACK_RETIRE_QUEUE.lock().push(self.0);
    }
}

/// 在未持有普通锁的调度安全点退休已溢出缓存的内核栈。
pub fn reclaim_retired_kernel_stacks(limit: usize) -> usize {
    let mut reclaimed = 0;
    while reclaimed < limit {
        let next = KSTACK_RETIRE_QUEUE.lock().pop();
        let Some(kstack_id) = next else {
            break;
        };
        let (kernel_stack_bottom, _) = kernel_stack_position(kstack_id);
        // `kernel_stack_bottom` 的单位是 byte address；必须先转 VirtAddr 再取 VPN。
        // 直接 `usize.into()` 会把原始字节地址当作页号，导致查不到登记的映射。
        let start_vpn = VirtAddr::from(kernel_stack_bottom).floor();
        crate::mm::remove_kernel_mapping_synchronized(start_vpn).unwrap();
        KSTACK_ALLOCATOR.lock().dealloc(kstack_id);
        reclaimed += 1;
    }
    reclaimed
}

impl KernelStack {
    #[allow(unused)]
    pub fn push_on_top<T>(&self, value: T) -> *mut T
    where
        T: Sized,
    {
        let kernel_stack_top = self.get_top();
        let ptr_mut = (kernel_stack_top - core::mem::size_of::<T>()) as *mut T;
        unsafe {
            *ptr_mut = value;
        }
        ptr_mut
    }
    pub fn get_top(&self) -> usize {
        let (_, kernel_stack_top) = kernel_stack_position(self.0);
        kernel_stack_top
    }
}

pub fn trap_cx_bottom_from_tid(tid: usize) -> usize {
    TRAP_CONTEXT_BASE - tid * PAGE_SIZE
}

pub fn ustack_bottom_from_tid(tid: usize) -> usize {
    USER_STACK_BASE - tid * (PAGE_SIZE + USER_STACK_SIZE)
}
