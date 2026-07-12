//! 编译期选择的架构后端导出层。
//!
//! 该模块按 feature 选择 RISC-V 或 LoongArch64 后端，并向 `hal/mod.rs`
//! 提供统一的启动、页表、陷阱、时间、控制台和上下文切换接口。

#[cfg(feature = "loongarch64")]
pub mod loongarch64;
#[cfg(feature = "loongarch64")]
pub use loongarch64::{
    board,
    board::MMIO,
    bootstrap_init, config,
    config::BUFFER_CACHE_NUM,
    config::KERNEL_HEAP_SIZE,
    config::MEMORY_END,
    console_flush, console_getchar, console_putchar,
    local_irq_restore, local_irq_save,
    machine_init, shutdown, user_hwcap,
    time::{get_clock_freq, get_time, program_timer_delta, TICKS_PER_SEC},
    KernelPageTableImpl, PageTableImpl, __switch, kstack_alloc, tlb_invalidate,
    trap::{
        get_bad_addr, get_bad_instruction, get_exception_cause, trap_handler, trap_return,
        MachineContext, TrapContext, TrapImpl, UserContext, UserSignalMask,
    },
    trap_cx_bottom_from_tid, ustack_bottom_from_tid, KernelStack, BLOCK_SZ,
};
#[cfg(feature = "riscv")]
pub mod riscv;
#[cfg(feature = "riscv")]
pub use riscv::{
    bootstrap_init, config,
    config::{BLOCK_SZ, BUFFER_CACHE_NUM, KERNEL_HEAP_SIZE, MEMORY_END},
    kern_stack::kstack_alloc,
    kern_stack::trap_cx_bottom_from_tid,
    kern_stack::ustack_bottom_from_tid,
    kern_stack::KernelStack,
    machine_init, user_hwcap,
    rv_board::MMIO,
    sbi::{console_flush, console_getchar, console_putchar, local_irq_restore, local_irq_save, set_timer, shutdown},
    sv39::tlb_invalidate,
    switch::__switch,
    time::{get_clock_freq, get_time, program_timer_delta, TICKS_PER_SEC},
    trap::{
        context::TrapContext, get_bad_addr, get_bad_instruction, get_exception_cause, trap_handler,
        trap_return, UserContext, UserSignalMask,
    },
    KernelPageTableImpl, MachineContext, PageTableImpl, TrapImpl,
};
