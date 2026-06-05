use super::config::{
    KERNEL_STACK_SIZE, PAGE_SIZE, TRAP_CONTEXT_BASE, USER_STACK_BASE, USER_STACK_SIZE,
};
use alloc::vec::Vec;
use lazy_static::*;
use spin::Mutex;

const KERNEL_STACK_CACHE_BYTES: usize = 4 * 1024 * 1024;
const KERNEL_STACK_CACHE_LIMIT: usize = KERNEL_STACK_CACHE_BYTES / KERNEL_STACK_SIZE;

lazy_static! {
    static ref KERNEL_STACK_CACHE: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());
}

pub struct KernelStack(Vec<u8>);
impl KernelStack {
    pub fn new() -> Self {
        if let Some(stack) = KERNEL_STACK_CACHE.lock().pop() {
            return Self(stack);
        }
        Self(alloc::vec![0_u8; KERNEL_STACK_SIZE])
    }
    pub fn get_top(&self) -> usize {
        let (_, kernel_stack_top) = Self::kernel_stack_position(&self.0);
        kernel_stack_top
    }
    /// Return (bottom, top) of a kernel stack in kernel space.
    fn kernel_stack_position(v: &Vec<u8>) -> (usize, usize) {
        /* let top: usize = TRAMPOLINE - kstack_id * (KERNEL_STACK_SIZE + PAGE_SIZE); */
        let bottom = &v[0] as *const u8 as usize;
        let top: usize = bottom + KERNEL_STACK_SIZE;
        (bottom, top)
    }
}

impl Drop for KernelStack {
    fn drop(&mut self) {
        let mut stack = Vec::new();
        core::mem::swap(&mut stack, &mut self.0);
        if stack.len() != KERNEL_STACK_SIZE {
            return;
        }
        let mut cache = KERNEL_STACK_CACHE.lock();
        if cache.len() < KERNEL_STACK_CACHE_LIMIT && cache.try_reserve(1).is_ok() {
            cache.push(stack);
        }
    }
}

/// 根据线程id计算trap context的地址
pub fn trap_cx_bottom_from_tid(tid: usize) -> usize {
    TRAP_CONTEXT_BASE - tid * PAGE_SIZE
}

/// 根据线程id计算用户栈的地址
pub fn ustack_bottom_from_tid(tid: usize) -> usize {
    USER_STACK_BASE - tid * (PAGE_SIZE + USER_STACK_SIZE)
}

#[inline(always)]
/// 分配一个内核栈
pub fn kstack_alloc() -> KernelStack {
    KernelStack::new()
}
