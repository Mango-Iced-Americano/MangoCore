//! RISC-V 平台内存布局和内核常量。
//!
//! 这些常量定义用户地址空间、内核堆、栈、trampoline、MMIO 和定时器频率。

#![allow(unused)]

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
// The FAT32 SMP ktest constructs the minimum standards-compliant 67,000-cluster
// in-memory volume (34,588,672 bytes) before it can exercise concurrent cluster
// allocation. Keep room for the live kernel and fixture instead of making the
// test depend on an impossible sub-32MiB contiguous allocation.
/// Bootstrap-only heap used before FRAME_ALLOCATOR can reserve runtime backing.
pub const KERNEL_BOOTSTRAP_HEAP_SIZE: usize = 32 * 1024 * 1024;
pub const MEMORY_SIZE: usize = 0x4000_0000;
pub const MMAP_BASE: usize = 0x2000_0000;
pub const MMAP_END: usize = 0xb800_0000;
// Keep the temporary kernel ELF window away from low FDT MMIO identity maps.
// It retains the former [MMAP_BASE, MMAP_END) capacity in an unused SV39 high-half range.
pub const KERNEL_PROGRAM_BASE: usize = 0xffff_ffc0_4000_0000;
pub const KERNEL_PROGRAM_END: usize = KERNEL_PROGRAM_BASE + (MMAP_END - MMAP_BASE);
pub const SKIP_NUM: usize = 2;

pub const PAGE_SIZE: usize = 0x1000;
pub const PAGE_SIZE_BITS: usize = 0xc;

pub const TRAMPOLINE: usize = usize::MAX - PAGE_SIZE + 1;
pub const SIGNAL_TRAMPOLINE: usize = TRAMPOLINE - PAGE_SIZE;
pub const TRAP_CONTEXT_BASE: usize = SIGNAL_TRAMPOLINE - PAGE_SIZE;

// Compile-time conservative upper bound: assume up to 16 GiB of RAM so the
// derived limit is a ceiling regardless of the FDT-described memory size.
// The runtime value is clamped to the FDT-derived `firmware::usable_memory_size()`
// at the task-quota enforcement point (`task/quota.rs`).
pub const SYSTEM_TASK_LIMIT: usize = {
    let ram = 0x4_0000_0000;
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

// 手动让可用内存空间与编译期常量保持一致；运行期实际拓扑仍以固件发布的
// region 为准（`firmware::memory_regions()`）。
pub const MEMORY_START: usize = 0x0000_0000_8000_0000;
pub const MEMORY_END: usize = MEMORY_START + MEMORY_SIZE;

/// Physical DRAM banks as half-open byte ranges.
pub const MEMORY_REGIONS: &[(usize, usize)] = &[(MEMORY_START, MEMORY_END)];
/// OpenSBI occupies the low 2 MiB of DRAM and transfers control to the kernel
/// at this address.
pub const FIRMWARE_END: usize = 0x8020_0000;
/// This range must never enter the frame allocator or the optional bulk-zero
/// path: overwriting it makes an early SATP switch re-enter the
/// firmware/kernel bootstrap loop.
pub const FIRMWARE_RESERVED_REGIONS: &[(usize, usize)] = &[(MEMORY_START, FIRMWARE_END)];
/// RAM currently exposed by the selected RISC-V board configuration.
pub const USABLE_MEMORY_SIZE: usize = MEMORY_END - FIRMWARE_END;

pub const BLOCK_SZ: usize = 4096;

pub const BUFFER_CACHE_NUM: usize = 16;
// dummy
pub const MEMORY_HIGH_BASE: usize = 0x0000_0000_0000_000;

/// Sv39 high-half base which maps QEMU virt RAM at physical `0x8000_0000`.
pub const KERNEL_VIRT_BASE: usize = 0xffff_ffc0_0000_0000;
/// Fixed virtual address of the Linux Image header (2 MiB above RAM base).
pub const KERNEL_LINK_VADDR: usize = KERNEL_VIRT_BASE + 0x0020_0000;

/// QEMU virt MMIO 区间快照，供平台/驱动探测使用；FDT 是权威来源，这里只作
/// 编译期兜底（与 `hal::arch::MMIO` 导出面保持一致）。
pub const MMIO: &[(usize, usize)] = &[
    (0x1000_0000, 0x1000),      // UART
    (0x1000_1000, 0x1000),      // virtio-mmio bus.0 (block x0)
    (0x1000_2000, 0x1000),      // virtio-mmio bus.1 (block x1 / tools disk)
    (0x1000_3000, 0x1000),      // virtio-mmio bus.2 (entropy source)
    (0x1000_8000, 0x1000),      // virtio-mmio bus.7 (net)
    (0x3000_0000, 0x1000_0000), // PCIe ECAM
    (0x4000_0000, 0x4000_0000), // PCIe 32-bit MMIO BAR window
    (0xC00_0000, 0x40_0000),    // PLIC
];


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
