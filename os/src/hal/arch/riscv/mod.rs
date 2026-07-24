//! RISC-V HAL 后端。
//!
//! 包含 QEMU/K210/FU740/VisionFive2 配置、SV39 页表、trap、SBI、时钟和上下文切换实现。

pub mod config;
pub mod kern_stack;
pub mod sbi;
pub mod sv39;
pub mod switch;
pub mod syscall_id;
pub mod time;
pub mod trap;

#[cfg(feature = "board_rvqemu")]
#[path = "../../platform/riscv/qemu.rs"]
pub mod rv_board;
#[cfg(feature = "board_vf2")]
#[path = "../../platform/riscv/vf2.rs"]
pub mod rv_board;

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

pub fn bootstrap_init(_cpu_id: usize) {}

/// Install the Phase 1 boot-only CPU-local anchor in `tp`.
pub fn install_boot_cpu_local(ptr: usize) {
    // Safety: the psABI makes x4/tp non-allocatable to compiler temporaries,
    // but user TLS still owns it.  This boot-only write happens before traps.
    unsafe {
        core::arch::asm!("mv tp, {ptr}", ptr = in(reg) ptr, options(nostack));
    }
}

/// Read back the current CPU's boot-only anchor for immediate verification.
pub fn boot_cpu_local_ptr() -> usize {
    let ptr;
    // Safety: this is a read-only move from the same CPU-local register.
    unsafe {
        core::arch::asm!("mv {ptr}, tp", ptr = out(reg) ptr, options(nostack));
    }
    ptr
}

/// Ask OpenSBI HSM to enter the common assembly entry on one stopped hart.
pub fn start_secondary_cpu(cpu_id: usize, start_addr: usize) -> Result<(), isize> {
    // HSM passes its opaque argument in a1.  Phase 1 does not consume an
    // architecture boot argument on APs, so publish an explicit zero.
    sbi::hart_start(cpu_id, start_addr, 0)
}

/// Keep an online Phase 1 AP outside the legacy scheduler.
pub fn boot_cpu_park() -> ! {
    loop {
        // Safety: WFI is only a local processor hint.  AP interrupts remain
        // disabled, and a later IPI phase will replace this permanent park.
        unsafe { riscv::asm::wfi() };
    }
}

/// Return the Linux-compatible RISC-V ISA-letter bitmap for `AT_HWCAP`.
pub fn user_hwcap() -> usize {
    // IMAFDC, with the bit position derived from the extension letter.
    0x112d
}
