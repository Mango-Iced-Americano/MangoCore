//! LoongArch64 用户栈、trap context 和内核栈地址分配。
//!
//! 使用固定 slot 分配器管理 guard-window 内的 per-thread 内核栈和用户栈布局。

use super::config::{
    KERNEL_STACK_BOTTOM, KERNEL_STACK_MAX_SLOTS, KERNEL_STACK_SIZE, KERNEL_STACK_SLOT_SIZE,
    KERNEL_STACK_TOP, PAGE_SIZE, TRAP_CONTEXT_BASE, USER_STACK_BASE, USER_STACK_SIZE,
};
use crate::mm::{MapPermission, VirtAddr, KERNEL_SPACE};
use crate::task::pid::RecycleAllocator;
use alloc::vec::Vec;
use lazy_static::*;
use spin::Mutex;

#[cfg(all(feature = "boot_la_uboot_dmw", feature = "bringup_trace"))]
static BOARD_FIRST_KSTACK_PROBE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

lazy_static! {
    static ref KSTACK_ALLOCATOR: Mutex<RecycleAllocator> = Mutex::new(RecycleAllocator::new());
    static ref KSTACK_CACHE: Mutex<Vec<usize>> = Mutex::new(Vec::new());
}

/// TCB 析构可能发生在进程锁保护区内；队列把跨核 shootdown 延后到
/// 明确的无锁调度安全点，slot 在此之前不会归还分配器。
static KSTACK_RETIRE_QUEUE: Mutex<
    crate::hal::KernelStackRetireQueue<{ super::config::SYSTEM_TASK_LIMIT }>,
> = Mutex::new(crate::hal::KernelStackRetireQueue::new());

/// Return (bottom, top) of a kernel stack in kernel virtual space.
pub fn kernel_stack_position(kstack_id: usize) -> (usize, usize) {
    if kstack_id >= KERNEL_STACK_MAX_SLOTS {
        panic!(
            "la64 kernel stack slot {} exceeds max {}",
            kstack_id, KERNEL_STACK_MAX_SLOTS
        );
    }
    let slot_offset = kstack_id
        .checked_mul(KERNEL_STACK_SLOT_SIZE)
        .expect("la64 kernel stack slot offset overflow");
    let top = KERNEL_STACK_TOP
        .checked_sub(slot_offset)
        .expect("la64 kernel stack top underflow");
    let bottom = top - KERNEL_STACK_SIZE;
    (bottom, top)
}

pub fn kernel_stack_guard_slot(addr: usize) -> Option<usize> {
    if addr < KERNEL_STACK_BOTTOM || addr >= KERNEL_STACK_TOP {
        return None;
    }
    let distance_from_top = KERNEL_STACK_TOP - 1 - addr;
    let slot = distance_from_top / KERNEL_STACK_SLOT_SIZE;
    let offset_in_slot = distance_from_top % KERNEL_STACK_SLOT_SIZE;
    if slot < KERNEL_STACK_MAX_SLOTS && offset_in_slot >= KERNEL_STACK_SIZE {
        Some(slot)
    } else {
        None
    }
}

pub struct KernelStack(pub usize);

pub fn kstack_alloc() -> KernelStack {
    if let Some(kstack_id) = KSTACK_CACHE.lock().pop() {
        crate::task::perf::record_kstack_alloc(true);
        return KernelStack(kstack_id);
    }
    crate::task::perf::record_kstack_alloc(false);
    let alloc_id = KSTACK_ALLOCATOR.lock().alloc();
    let kstack_id = alloc_id
        .checked_sub(1)
        .expect("la64 kernel stack allocator returned zero");
    let (kstack_bottom, kstack_top) = kernel_stack_position(kstack_id);
    KERNEL_SPACE.lock().insert_kernel_stack_area(
        kstack_bottom.into(),
        kstack_top.into(),
        MapPermission::R | MapPermission::W,
    );
    #[cfg(all(feature = "boot_la_uboot_dmw", feature = "bringup_trace"))]
    if !BOARD_FIRST_KSTACK_PROBE.swap(true, core::sync::atomic::Ordering::Relaxed) {
        // 在启动栈仍有效时访问第一个新映射栈，从而在 `__switch` 使用新栈指针前，
        // 同时验证开发板规范虚拟地址窗口和 PGDH/PTE 映射。
        let probe_addr = kstack_top - core::mem::size_of::<usize>();
        let mapped_ppn = KERNEL_SPACE
            .lock()
            .mapped_frame(VirtAddr::from(probe_addr).floor())
            .map(|frame| frame.ppn.0)
            .unwrap_or(usize::MAX);
        println!(
            "[bringup][kstack:01] mapped high stack: bottom={:#x} top={:#x} probe={:#x} ppn={:#x} pgdh={:#x}",
            kstack_bottom,
            kstack_top,
            probe_addr,
            mapped_ppn,
            super::register::PGDH::read().get_base()
        );
        let probe_value = 0x4b53_5441_434b_4f4busize;
        let probe_ptr = probe_addr as *mut usize;
        // 安全性：`insert_kernel_stack_area` 已将该地址映射为可读写；初始栈为空，
        // 且 `trap_return` 随后会覆盖栈顶字。
        let observed = unsafe {
            core::ptr::write_volatile(probe_ptr, probe_value);
            core::ptr::read_volatile(probe_ptr)
        };
        println!(
            "[bringup][kstack:02] high stack write/read passed: value={:#x}",
            observed
        );
    }
    KernelStack(kstack_id)
}

impl KernelStack {
    pub fn get_top(&self) -> usize {
        let (_, kernel_stack_top) = kernel_stack_position(self.0);
        kernel_stack_top
    }
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

/// 在调用者不持有普通锁时，完成 PTE 撤销、全 CPU shootdown 和 slot 回收。
pub fn reclaim_retired_kernel_stacks(limit: usize) -> usize {
    let mut reclaimed = 0;
    while reclaimed < limit {
        let next = KSTACK_RETIRE_QUEUE.lock().pop();
        let Some(kstack_id) = next else {
            break;
        };
        let (kernel_stack_bottom, _) = kernel_stack_position(kstack_id);
        // `kernel_stack_bottom` 是字节地址，先显式归一成 VPN，避免 usize 的
        // `From` 实现把它原样解释成页号。
        let start_vpn = VirtAddr::from(kernel_stack_bottom).floor();
        crate::mm::remove_kernel_mapping_synchronized(start_vpn).unwrap();
        KSTACK_ALLOCATOR.lock().dealloc(kstack_id + 1);
        reclaimed += 1;
    }
    reclaimed
}

/// 根据线程id计算trap context的地址
pub fn trap_cx_bottom_from_tid(tid: usize) -> usize {
    if !(1..=KERNEL_STACK_MAX_SLOTS).contains(&tid) {
        panic!(
            "la64 trap context slot {} is outside 1..={}",
            tid, KERNEL_STACK_MAX_SLOTS
        );
    }
    TRAP_CONTEXT_BASE + (tid - 1) * PAGE_SIZE
}

/// 根据线程id计算用户栈的地址
pub fn ustack_bottom_from_tid(tid: usize) -> usize {
    USER_STACK_BASE - tid * (PAGE_SIZE + USER_STACK_SIZE)
}
