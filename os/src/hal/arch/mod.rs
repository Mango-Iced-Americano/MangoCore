//! 编译期选择的架构后端导出层。
//!
//! 该模块按 feature 选择 RISC-V 或 LoongArch64 后端，并向 `hal/mod.rs`
//! 提供统一的启动、页表、陷阱、时间、控制台和上下文切换接口。

#[cfg(target_arch = "loongarch64")]
pub mod loongarch64;
#[cfg(target_arch = "loongarch64")]
pub use loongarch64::{
    __switch, board,
    board::MMIO,
    boot_cpu_park, bootstrap_init, config,
    config::BUFFER_CACHE_NUM,
    config::KERNEL_BOOTSTRAP_HEAP_SIZE,
    config::MEMORY_END,
    console_flush, console_getchar, console_putchar, console_write_bytes, cpu_local_ptr,
    enable_local_timer_interrupt, enter_secondary_idle, init_kernel_mapping_metadata,
    install_cpu_local, irq_enabled, kernel_tlb_invalidate, kstack_alloc, local_irq_restore,
    local_irq_save,
    machine_init, machine_shutdown, panic_console_write, prepare_secondary_cpu_stop, reboot,
    reclaim_retired_kernel_stacks, remote_user_tlb_invalidate_range, secondary_cpu_stop,
    cpu_wait_for_interrupt, send_ipi, start_secondary_cpu, syscall_id,
    time::{
        get_clock_freq, get_time, program_timer_delta, quiesce_local_timer_interrupt, TICKS_PER_SEC,
    },
    tlb_invalidate,
    trap::{
        get_bad_addr, get_bad_instruction, get_exception_cause, trap_handler, trap_return, LsxRegs,
        MachineContext, TrapContext, TrapImpl, UserContext, UserSignalMask,
    },
    trap_cx_bottom_from_tid, user_hwcap, user_tlb_invalidate, user_tlb_invalidate_page,
    user_tlb_invalidate_range, ustack_bottom_from_tid, KernelPageTableImpl, KernelStack,
    PageTableImpl, BLOCK_SZ, local_user_fault_tlb_invalidate_page,
};
#[cfg(target_arch = "riscv64")]
pub mod riscv;
#[cfg(target_arch = "riscv64")]
pub use riscv::{
    boot_cpu_park, bootstrap_init, config,
    config::{BLOCK_SZ, BUFFER_CACHE_NUM, KERNEL_BOOTSTRAP_HEAP_SIZE, MEMORY_END},
    cpu_local_ptr, enable_local_timer_interrupt, enter_secondary_idle,
    init_kernel_mapping_metadata, install_cpu_local,
    kern_stack::kstack_alloc,
    kern_stack::trap_cx_bottom_from_tid,
    kern_stack::ustack_bottom_from_tid,
    kern_stack::KernelStack,
    kernel_tlb_invalidate, machine_init, prepare_secondary_cpu_stop, reclaim_retired_kernel_stacks,
    remote_user_tlb_invalidate_range,
    reset::reboot,
    config::MMIO,
    sbi::{
        configure_runtime_console, console_flush, console_getchar, console_putchar,
        console_write_bytes, drain_runtime_console_rx, irq_enabled, local_irq_restore,
        local_irq_save, machine_shutdown, init_runtime_console_rx, panic_console_write,
        shutdown, take_runtime_console_rx_interrupt,
    },
    cpu_wait_for_interrupt, secondary_cpu_stop, send_ipi, start_secondary_cpu,
    sv39::tlb_invalidate,
    switch::__switch,
    syscall_id,
    time::{
        get_clock_freq, get_time, program_timer_delta, quiesce_local_timer_interrupt, TICKS_PER_SEC,
    },
    trap::{
        context::TrapContext, get_bad_addr, get_bad_instruction, get_exception_cause, trap_handler,
        trap_return, UserContext, UserSignalMask,
    },
    user_hwcap, user_tlb_invalidate, user_tlb_invalidate_page, user_tlb_invalidate_range,
    KernelPageTableImpl, MachineContext, PageTableImpl, TrapImpl, local_user_fault_tlb_invalidate_page,
};
