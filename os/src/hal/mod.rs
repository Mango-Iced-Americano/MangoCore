//! 硬件抽象层入口。
//!
//! 统一导出架构相关的启动、陷阱、页表、TLB、时钟、控制台和内核栈接口。
//! 上层内核代码应通过本模块访问架构能力，避免直接依赖 `arch/*` 的实现细节。
//!
//! # TLB
//!
//! 修改 PTE 后必须通过本模块导出的 `tlb_invalidate` 或页表接口自带的
//! flush 路径刷新 TLB。RISC-V 使用 `sfence.vma`，LoongArch64 使用
//! `invtlb`。

pub mod arch;
pub use arch::__switch;
pub use arch::config;
pub use arch::kstack_alloc;
pub use arch::program_timer_delta;
pub use arch::shutdown;
pub use arch::tlb_invalidate;
#[cfg(feature = "loongarch64")]
pub use arch::LsxRegs;
pub use arch::{
    boot_cpu_park, bootstrap_init, cpu_local_ptr, enter_secondary_idle, install_cpu_local,
    machine_init, secondary_cpu_idle, send_ipi, start_secondary_cpu, user_hwcap,
};
pub use arch::{console_flush, console_getchar, console_putchar, console_write_bytes};
pub use arch::{get_bad_addr, get_bad_instruction, get_exception_cause};
pub use arch::{get_clock_freq, get_time};
pub use arch::{local_irq_restore, local_irq_save};
pub use arch::{trap_cx_bottom_from_tid, ustack_bottom_from_tid};
pub use arch::{trap_handler, trap_return};
pub use arch::{
    KernelPageTableImpl, KernelStack, MachineContext, PageTableImpl, TrapContext, TrapImpl,
    UserContext, UserSignalMask,
};
pub use arch::{BLOCK_SZ, BUFFER_CACHE_NUM, KERNEL_HEAP_SIZE, MEMORY_END};
pub use arch::{MMIO, TICKS_PER_SEC};

/// Per-chunk bounce buffer size for I/O operations.
/// Computed as KERNEL_HEAP_SIZE / 128, bounded to [64KiB, 256KiB].
/// For 32MiB heap → 256KiB chunk.
pub const IO_CHUNK_SIZE: usize = {
    let heap = KERNEL_HEAP_SIZE;
    let raw = heap / 128;
    if raw < 64 * 1024 {
        64 * 1024
    } else if raw > 256 * 1024 {
        256 * 1024
    } else {
        raw
    }
};

/// Maximum user-visible read/write count (Linux-compatible).
/// Equals i32::MAX rounded down to page alignment.
pub const MAX_RW_COUNT: usize = (i32::MAX as usize) & !(crate::config::PAGE_SIZE as usize - 1);
