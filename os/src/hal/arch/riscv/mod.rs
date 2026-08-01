//! RISC-V HAL 后端。
//!
//! 包含统一 FDT 启动配置、SV39 页表、trap、SBI、时钟和上下文切换实现。

pub mod config;
pub mod kern_stack;
pub mod reset;
pub mod sbi;
pub mod sv39;
pub mod switch;
pub mod syscall_id;
pub mod time;
pub mod trap;

pub fn machine_init() {
    trap::init();
    trap::enable_timer_interrupt();
    // First timer deadline is set by timer_subsystem_init() after boot.
}

use time::set_next_trigger;

pub use trap::context::MachineContext;

pub type KernelPageTableImpl = sv39::Sv39PageTable;
pub type PageTableImpl = sv39::Sv39PageTable;
pub type TrapImpl = riscv::register::scause::Trap;
pub type InterruptImpl = riscv::register::scause::Interrupt;
pub type ExceptionImpl = riscv::register::scause::Exception;

pub fn bootstrap_init() {}

/// Return the Linux-compatible RISC-V ISA-letter bitmap for `AT_HWCAP`.
pub fn user_hwcap() -> usize {
    // IMAFDC, with the bit position derived from the extension letter.
    0x112d
}
