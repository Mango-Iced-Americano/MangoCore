//! RISC-V 平台内存布局和内核常量。
//!
//! 这些常量定义用户地址空间、内核堆、栈、trampoline、MMIO 和定时器频率。

#![allow(unused)]

pub const MEMORY_SIZE: usize = 0x4000_0000;
pub const USER_VA_BASE: usize = 0;
pub const TASK_SIZE: usize = 0xc000_0000;
pub const USER_VA_END: usize = TASK_SIZE;
pub const ELF_PIE_BASE: usize = USER_VA_BASE + 0x0040_0000;
pub const ELF_DYN_BASE: usize = TASK_SIZE / 3 * 2;
pub const USER_STACK_BASE: usize = TASK_SIZE - PAGE_SIZE;
pub const USER_STACK_SIZE: usize = PAGE_SIZE * 0x100;
pub const USER_STACK_INIT_SIZE: usize = PAGE_SIZE * 0x40;
pub const USER_HEAP_SIZE: usize = PAGE_SIZE * 0x100;

pub const KERNEL_STACK_SIZE: usize = PAGE_SIZE * 0x10;
#[cfg(feature = "board_rvqemu")]
pub const KERNEL_HEAP_SIZE: usize = PAGE_SIZE * 0x10000;
#[cfg(feature = "board_fu740")]
pub const KERNEL_HEAP_SIZE: usize = PAGE_SIZE * 0x2000;
#[cfg(feature = "board_cv1811h")]
pub const KERNEL_HEAP_SIZE: usize = PAGE_SIZE * 0x2000;
#[cfg(feature = "board_vf2")]
pub const KERNEL_HEAP_SIZE: usize = PAGE_SIZE * 0x2000;
pub const MMAP_BASE: usize = 0x2000_0000;
pub const MMAP_END: usize = 0xb800_0000;
pub const SKIP_NUM: usize = 2;

// manually make usable memory space equal
#[cfg(not(feature = "board_vf2"))]
pub const MEMORY_START: usize = 0x0000_0000_8000_0000;
#[cfg(feature = "board_vf2")]
pub const MEMORY_START: usize = 0x4000_0000;
#[cfg(feature = "board_rvqemu")]
pub const MEMORY_END: usize = MEMORY_START + MEMORY_SIZE;
#[cfg(feature = "board_fu740")]
pub const MEMORY_END: usize = 0x9000_0000;
#[cfg(feature = "board_cv1811h")]
pub const MEMORY_END: usize = 0x9000_0000; //256M
#[cfg(feature = "board_vf2")]
pub const MEMORY_END: usize = 0xC000_0000;
pub const PAGE_SIZE: usize = 0x1000;
pub const PAGE_SIZE_BITS: usize = 0xc;

pub const TRAMPOLINE: usize = usize::MAX - PAGE_SIZE + 1;
pub const SIGNAL_TRAMPOLINE: usize = TRAMPOLINE - PAGE_SIZE;
pub const TRAP_CONTEXT_BASE: usize = SIGNAL_TRAMPOLINE - PAGE_SIZE;

pub const MEMORY_PHYS: usize = 0x800_0000;
pub const DISK_IMAGE_BASE: usize = 0x8000_0000 + MEMORY_PHYS;
// pub const DISK_IMAGE_BASE: usize = MEMORY_END;

pub const SYSTEM_TASK_LIMIT: usize = {
    let ram = MEMORY_SIZE;
    let stack = KERNEL_STACK_SIZE;
    let limit = ram / (stack * 4);
    if limit < 512 {
        512
    } else if limit > 4096 {
        4096
    } else {
        limit
    }
};
pub const SYSTEM_TASK_SOFT_LIMIT: usize = SYSTEM_TASK_LIMIT * 9 / 10;
pub const SYSTEM_FD_LIMIT: usize = 4096;

pub const BLOCK_SZ: usize = 4096;

pub const BUFFER_CACHE_NUM: usize = 16;
// dummy
pub const MEMORY_HIGH_BASE: usize = 0x0000_0000_0000_000;

pub use crate::hal::arch::riscv::rv_board::{CLOCK_FREQ, MMIO};

#[macro_export]
macro_rules! signal_type {
    () => {
        usize
    };
}

#[macro_export]
macro_rules! newline {
    () => {
        "\r\n"
    };
}

#[macro_export]
macro_rules! should_map_trampoline {
    () => {
        true
    };
}
