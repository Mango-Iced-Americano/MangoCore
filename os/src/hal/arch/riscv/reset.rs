//! RISC-V reboot routing and VisionFive 2 watchdog reset.

use core::arch::asm;
use core::hint::spin_loop;

const SYSCRG_BASE: usize = 0x1302_0000;
const WDT_BASE: usize = 0x1307_0000;

const WDT_APB_CLOCK: usize = 122 * 4;
const WDT_CORE_CLOCK: usize = 123 * 4;
const RESET_ASSERT3: usize = 0x0304;
const RESET_STATUS3: usize = 0x0314;
const CLOCK_ENABLE: u32 = 1 << 31;
const WDT_RESETS: u32 = (1 << 13) | (1 << 14);

const WDOG_LOAD: usize = 0x000;
const WDOG_CONTROL: usize = 0x008;
const WDOG_INT_CLR: usize = 0x00c;
const WDOG_LOCK: usize = 0xc00;
const WDOG_UNLOCK_VALUE: u32 = 0x1acc_e551;
const WDOG_COUNTER_ENABLE_AND_RESET: u32 = 0b11;
const WDOG_REBOOT_LOAD: u32 = 1_000_000;
const RESET_STATUS_POLL_LIMIT: usize = 1_000_000;

#[inline(always)]
fn read_reg(base: usize, offset: usize) -> u32 {
    // SAFETY: Categories 6 and 11. The base and offset are 4-byte aligned
    // JH7110 register constants, so the volatile load is aligned and valid.
    unsafe {
        core::ptr::read_volatile(
            crate::mm::PhysAddr(base + offset)
                .direct_map_ptr()
                .cast::<u32>(),
        )
    }
}

#[inline(always)]
fn write_reg(base: usize, offset: usize, value: u32) {
    // SAFETY: Categories 6 and 11. The base and offset are 4-byte aligned
    // JH7110 register constants, so the volatile store is aligned and valid.
    unsafe {
        core::ptr::write_volatile(
            crate::mm::PhysAddr(base + offset)
                .direct_map_ptr()
                .cast::<u32>(),
            value,
        )
    }
}

#[inline(always)]
fn fence_iorw() {
    // SAFETY: `fence iorw, iorw` orders device I/O and has no Rust memory or
    // register operands; this module is compiled only for RISC-V.
    unsafe { asm!("fence iorw, iorw", options(nostack, preserves_flags)) }
}

/// Reset a JH7110 SoC through its watchdog without relying on OpenSBI SRST.
///
/// The physical VF2 FDT must retain `reg` ranges for the SYSCRG and watchdog:
/// the pre-heap FDT parser identity-maps every non-RAM `reg` range used here.
pub fn jh7110_watchdog_reboot() -> ! {
    let _interrupts_were_enabled = super::sbi::local_irq_save();

    write_reg(
        SYSCRG_BASE,
        WDT_APB_CLOCK,
        read_reg(SYSCRG_BASE, WDT_APB_CLOCK) | CLOCK_ENABLE,
    );
    write_reg(
        SYSCRG_BASE,
        WDT_CORE_CLOCK,
        read_reg(SYSCRG_BASE, WDT_CORE_CLOCK) | CLOCK_ENABLE,
    );
    write_reg(
        SYSCRG_BASE,
        RESET_ASSERT3,
        read_reg(SYSCRG_BASE, RESET_ASSERT3) & !WDT_RESETS,
    );

    let mut polls_remaining = RESET_STATUS_POLL_LIMIT;
    while read_reg(SYSCRG_BASE, RESET_STATUS3) & WDT_RESETS != WDT_RESETS {
        if polls_remaining == 0 {
            break;
        }
        polls_remaining -= 1;
        spin_loop();
    }

    write_reg(WDT_BASE, WDOG_LOCK, WDOG_UNLOCK_VALUE);
    write_reg(WDT_BASE, WDOG_CONTROL, 0);
    write_reg(WDT_BASE, WDOG_INT_CLR, 1);
    write_reg(WDT_BASE, WDOG_LOAD, WDOG_REBOOT_LOAD);
    fence_iorw();
    write_reg(WDT_BASE, WDOG_CONTROL, WDOG_COUNTER_ENABLE_AND_RESET);
    write_reg(WDT_BASE, WDOG_LOCK, 0);
    fence_iorw();

    loop {
        spin_loop();
    }
}

/// Reboot a physical VisionFive board through its watchdog and use SBI elsewhere.
pub fn reboot() -> ! {
    if crate::hal::platform::is_real_board() {
        jh7110_watchdog_reboot()
    }
    super::sbi::reboot()
}
