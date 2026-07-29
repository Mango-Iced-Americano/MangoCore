//! RISC-V SBI 调用封装。
//!
//! 提供 timer、console、shutdown 和本地中断开关等机器环境接口。

use core::arch::asm;
use riscv::register::sstatus;

const SBI_SET_TIMER: usize = 0;
const SBI_CONSOLE_PUTCHAR: usize = 1;
const SBI_CONSOLE_GETCHAR: usize = 2;
const SBI_CLEAR_IPI: usize = 3;
const SBI_SEND_IPI: usize = 4;
const SBI_REMOTE_FENCE_I: usize = 5;
const SBI_REMOTE_SFENCE_VMA: usize = 6;
const SBI_REMOTE_SFENCE_VMA_ASID: usize = 7;
const SBI_SHUTDOWN: usize = 8;
const SBI_SRST: usize = 0x5352_5354;

#[inline(always)]
/// `ecall` wrapper to switch trap into S level.
fn sbi_call(which: usize, arg0: usize, arg1: usize, arg2: usize) -> usize {
    let mut ret;
    // Safety: OpenSBI defines the ecall ABI. Arguments are passed in a0-a2,
    // legacy function ID is zeroed in a6, and `which` (EID) goes in a7.
    // The return value is read from a0; no Rust references cross the call.
    unsafe {
        asm!(
            "ecall",
            inlateout("x10") arg0 => ret,
            in("x11") arg1,
            in("x12") arg2,
            in("x16") 0usize,
            in("x17") which,
        );
    }
    ret
}

pub fn set_timer(timer: usize) {
    let profile_start = crate::task::processor::sched_profile_cycle_start();
    sbi_call(SBI_SET_TIMER, timer, 0, 0);
    crate::task::processor::record_sched_sbi_set_timer_cycles(profile_start);
}

pub fn console_putchar(c: usize) {
    sbi_call(SBI_CONSOLE_PUTCHAR, c, 0, 0);
}

/// VF2 JH7110 DW APB UART: 32-bit wide registers at shifted (×4) offsets.
///
/// MMIO addresses (physical, fixed in VF2 memory map):
/// - RBR (read):   0x1000_0000  — receive buffer register, low byte holds data
/// - LSR (read):   0x1000_0014 — line status register, bit 0 = Data Ready (DR)
///
/// Safety: these addresses are fixed per the JH7110 datasheet and are
/// mapped as strongly-ordered device memory by OpenSBI. Only the low u8
/// of RBR is consumed after DR is confirmed set. No read from RBR without
/// DR guard.
#[cfg(feature = "board_vf2")]
fn vf2_console_getchar() -> usize {
    // Safety: fixed VF2 UART MMIO at 0x1000_0000; volatile, device-memory semantics.
    unsafe {
        const LSR_ADDR: *const u32 = 0x1000_0014 as *const u32;
        const RBR_ADDR: *const u32 = 0x1000_0000 as *const u32;
        const DR: u32 = 0x01;

        let lsr = core::ptr::read_volatile(LSR_ADDR);
        if lsr & DR == 0 {
            return !0; // usize::MAX — no data available
        }
        let rbr = core::ptr::read_volatile(RBR_ADDR);
        (rbr & 0xFF) as usize
    }
}

pub fn console_getchar() -> usize {
    #[cfg(feature = "board_vf2")]
    {
        vf2_console_getchar()
    }
    #[cfg(not(feature = "board_vf2"))]
    {
        sbi_call(SBI_CONSOLE_GETCHAR, 0, 0, 0)
    }
}

pub fn console_flush() {}

/// 保存当前中断使能状态，并关中断（用于 console 临界区）。
pub fn local_irq_save() -> bool {
    let was_enabled = sstatus::read().sie();
    // Safety: clearing SIE only changes the local hart interrupt-enable bit.
    unsafe { sstatus::clear_sie() };
    was_enabled
}

/// 恢复中断使能状态到调用 local_irq_save 之前的值。
pub fn local_irq_restore(was_enabled: bool) {
    if was_enabled {
        // Safety: restoring SIE only changes the local hart interrupt-enable bit.
        unsafe { sstatus::set_sie() };
    }
}

/// Write a byte slice to the console, batching for efficiency.
///
/// On rvqemu (feature `board_rvqemu`): writes directly to NS16550A UART MMIO
/// at `0x1000_0000`, using THRE handshake and batching up to 16 bytes per
/// FIFO drain round. This bypasses SBI ecall overhead (~3μs per call).
///
/// On other riscv platforms: per-character fallback via [`console_putchar`].
pub fn console_write_bytes(data: &[u8]) {
    #[cfg(feature = "board_rvqemu")]
    {
        // NS16550A UART at fixed QEMU virt MMIO base
        const UART_BASE: usize = 0x1000_0000;
        const THR: usize = 0x0;   // Transmit Holding Register
        const LSR: usize = 0x5;   // Line Status Register
        const THRE: u8 = 1 << 5;  // Transmitter Holding Register Empty

        for chunk in data.chunks(16) {
            for &byte in chunk {
                // Wait until THR is empty (previous char transmitted / FIFO drained)
                loop {
                    // Safety: UART_BASE is a known-good MMIO region on QEMU virt.
                    let lsr = unsafe { core::ptr::read_volatile((UART_BASE + LSR) as *const u8) };
                    if lsr & THRE != 0 {
                        break;
                    }
                }
                // Safety: same UART MMIO region, write-only.
                unsafe { core::ptr::write_volatile((UART_BASE + THR) as *mut u8, byte) };
            }
        }
    }
    #[cfg(not(feature = "board_rvqemu"))]
    {
        for &b in data {
            console_putchar(b as usize);
        }
    }
}

pub fn shutdown() -> ! {
    sbi_call(SBI_SHUTDOWN, 0, 0, 0);
    panic!("It should shutdown!");
}

/// Cold reboot via SBI SRST extension (EID 0x53525354, FID 0).
/// Falls back to shutdown if SRST is not supported by the firmware.
///
/// Uses dedicated inline asm because SRST is a modern SBI extension that
/// requires the function ID (FID=0) in `a6`, which the legacy `sbi_call`
/// helper does not set.
pub fn reboot() -> ! {
    // Safety: SBI SRST ecall with EID=0x53525354 in a7, FID=0 in a6,
    // reset_type=1 (cold) in a0. No Rust references cross the call.
    unsafe {
        asm!(
            "ecall",
            in("x10") 1usize,              // a0 = reset_type (1 = cold reboot)
            in("x11") 0usize,              // a1 = reset_reason
            in("x16") 0usize,              // a6 = FID 0 (system_reset)
            in("x17") SBI_SRST,            // a7 = EID
        );
    }
    // If SRST not supported, fall back to shutdown.
    shutdown();
}
