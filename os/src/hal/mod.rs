pub mod arch;
pub use arch::__switch;
pub use arch::config;
pub use arch::kstack_alloc;
pub use arch::shutdown;
pub use arch::tlb_invalidate;
pub use arch::{bootstrap_init, machine_init};
pub use arch::{console_flush, console_getchar, console_putchar};
pub use arch::{local_irq_restore, local_irq_save};
pub use arch::{get_bad_addr, get_bad_instruction, get_exception_cause};
pub use arch::{get_clock_freq, get_time};
pub use arch::program_timer_delta;
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
