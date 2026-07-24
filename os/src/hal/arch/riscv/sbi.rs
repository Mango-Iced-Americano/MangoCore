//! RISC-V SBI 调用封装。
//!
//! 提供 timer、console、shutdown 和本地中断开关等机器环境接口。

#![allow(unused)]

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

const SBI_EXT_BASE: usize = 0x10;
const SBI_BASE_PROBE_EXTENSION: usize = 3;
const SBI_EXT_HSM: usize = 0x48534d;
const SBI_HSM_HART_START: usize = 0;
const SBI_ERR_NOT_SUPPORTED: isize = -2;
const SBI_ERR_ALREADY_AVAILABLE: isize = -6;

#[derive(Clone, Copy, Debug)]
struct SbiRet {
    error: isize,
    value: usize,
}

#[inline(always)]
/// `ecall` wrapper to switch trap into S level.
fn sbi_call(which: usize, arg0: usize, arg1: usize, arg2: usize) -> usize {
    let mut ret;
    // Safety: OpenSBI defines the ecall ABI. Arguments are passed in a0-a2/a7
    // and the return value is read from a0; no Rust references cross the call.
    unsafe {
        asm!(
            "ecall",
            inlateout("x10") arg0 => ret,
            in("x11") arg1,
            in("x12") arg2,
            in("x17") which,
        );
    }
    ret
}

/// Invoke an SBI v0.2+ extension using the `(error, value)` return convention.
#[inline(always)]
fn sbi_call_v02(
    extension: usize,
    function: usize,
    arg0: usize,
    arg1: usize,
    arg2: usize,
) -> SbiRet {
    let error: isize;
    let value: usize;
    // Safety: the SBI v0.2 ABI assigns a0-a2 to arguments, a6/a7 to
    // function/extension IDs, and returns error/value in a0/a1.  No Rust
    // reference crosses the privilege boundary.
    unsafe {
        asm!(
            "ecall",
            inlateout("x10") arg0 => error,
            inlateout("x11") arg1 => value,
            in("x12") arg2,
            in("x16") function,
            in("x17") extension,
        );
    }
    SbiRet { error, value }
}

/// Start one stopped hart at the physical `_start` address.
pub fn hart_start(hart_id: usize, start_addr: usize, opaque: usize) -> Result<(), isize> {
    let probe = sbi_call_v02(
        SBI_EXT_BASE,
        SBI_BASE_PROBE_EXTENSION,
        SBI_EXT_HSM,
        0,
        0,
    );
    if probe.error != 0 {
        return Err(probe.error);
    }
    if probe.value == 0 {
        return Err(SBI_ERR_NOT_SUPPORTED);
    }

    let result = sbi_call_v02(
        SBI_EXT_HSM,
        SBI_HSM_HART_START,
        hart_id,
        start_addr,
        opaque,
    );
    match result.error {
        0 | SBI_ERR_ALREADY_AVAILABLE => Ok(()),
        error => Err(error),
    }
}

pub fn set_timer(timer: usize) {
    let profile_start = crate::task::processor::sched_profile_cycle_start();
    sbi_call(SBI_SET_TIMER, timer, 0, 0);
    crate::task::processor::record_sched_sbi_set_timer_cycles(profile_start);
}

pub fn console_putchar(c: usize) {
    sbi_call(SBI_CONSOLE_PUTCHAR, c, 0, 0);
}

pub fn console_getchar() -> usize {
    sbi_call(SBI_CONSOLE_GETCHAR, 0, 0, 0)
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
