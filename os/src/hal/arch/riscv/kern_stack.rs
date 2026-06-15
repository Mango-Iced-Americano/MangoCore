use super::config::{
    KERNEL_STACK_SIZE, PAGE_SIZE, TRAMPOLINE, TRAP_CONTEXT_BASE, USER_STACK_BASE, USER_STACK_SIZE,
};
use crate::mm::{MapPermission, VirtAddr, KERNEL_SPACE};
use crate::task::pid::RecycleAllocator;
use alloc::vec::Vec;
use lazy_static::*;
use spin::Mutex;

const KSTACK_CACHE_LIMIT: usize = 128;

lazy_static! {
    static ref KSTACK_ALLOCATOR: Mutex<RecycleAllocator> = Mutex::new(RecycleAllocator::new());
    static ref KSTACK_CACHE: Mutex<Vec<usize>> = Mutex::new(Vec::new());
}

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
        if cache.len() < KSTACK_CACHE_LIMIT {
            cache.push(self.0);
            crate::task::perf::record_kstack_drop(true);
            return;
        }
        drop(cache);
        crate::task::perf::record_kstack_drop(false);
        let (kernel_stack_bottom, _) = kernel_stack_position(self.0);
        let kernel_stack_bottom_va: VirtAddr = kernel_stack_bottom.into();
        KERNEL_SPACE
            .lock()
            .remove_area_with_start_vpn(kernel_stack_bottom_va.into())
            .unwrap();
        KSTACK_ALLOCATOR.lock().dealloc(self.0)
    }
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
