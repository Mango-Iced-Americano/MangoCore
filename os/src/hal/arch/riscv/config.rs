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
#[cfg(feature = "board_vf2")]
pub const KERNEL_HEAP_SIZE: usize = PAGE_SIZE * 0x2000;
pub const MMAP_BASE: usize = 0x2000_0000;
pub const MMAP_END: usize = 0xb800_0000;
// 公共内核 ELF 映射代码需要架构专属上界。RISC-V 不存在独立的高地址 PGDH 栈别名，
// 因此沿用 MMAP_END 作为正确上界，该常量不会改变 RISC-V 的既有地址布局。
pub const KERNEL_PROGRAM_END: usize = MMAP_END;
pub const SKIP_NUM: usize = 2;

// manually make usable memory space equal
#[cfg(not(feature = "board_vf2"))]
pub const MEMORY_START: usize = 0x0000_0000_8000_0000;
#[cfg(feature = "board_vf2")]
pub const MEMORY_START: usize = 0x4000_0000;
#[cfg(feature = "board_rvqemu")]
pub const MEMORY_END: usize = MEMORY_START + MEMORY_SIZE;
#[cfg(feature = "board_vf2")]
pub const MEMORY_END: usize = 0xC000_0000;

/// Physical DRAM banks as half-open byte ranges.
pub const MEMORY_REGIONS_FALLBACK: &[(usize, usize)] = &[(MEMORY_START, MEMORY_END)];
/// OpenSBI occupies the low 2 MiB of DRAM and transfers control to the kernel
/// at this address.
#[cfg(not(feature = "board_vf2"))]
pub const FIRMWARE_END: usize = 0x8020_0000;
/// VisionFive 2 loads the kernel at 0x4020_0000, above its low OpenSBI area.
#[cfg(feature = "board_vf2")]
pub const FIRMWARE_END: usize = 0x4020_0000;
/// This range must never enter the frame allocator or the optional bulk-zero
/// path: overwriting it makes an early SATP switch re-enter the
/// firmware/kernel bootstrap loop.
pub const FIRMWARE_RESERVED_REGIONS_FALLBACK: &[(usize, usize)] = &[(MEMORY_START, FIRMWARE_END)];
/// RAM currently exposed by the selected RISC-V board configuration.
///
/// `MEMORY_SIZE` is a historical common capacity constant, while fu740 and
/// cv1811h select a smaller `MEMORY_END`; use the actual region length for ABI
/// statistics so those boards retain their previous `sysinfo(2)` semantics.
pub const USABLE_MEMORY_SIZE: usize = MEMORY_END - FIRMWARE_END;
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
