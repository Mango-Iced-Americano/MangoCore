//! LoongArch64 平台服务封装。
//!
//! 提供 UART 控制台、关机、时钟频率读取和本地中断开关等 HAL 所需接口。

use embedded_hal::serial::nb::{Read, Write};
use spin::Mutex;

use crate::drivers::Ns16550a;
use core::arch::asm;

use super::board::UART_BASE;
use super::register::CrMd;

static UART: Mutex<Ns16550a> = Mutex::new(Ns16550a::new(UART_BASE, 0x100, 0, 1));

pub fn console_putchar(c: usize) {
    UART.lock().write(c as u8);
}

pub fn console_flush() {
    let mut uart = UART.lock();
    while uart.flush().is_err() {}
}

pub fn console_getchar() -> usize {
    let mut uart = UART.lock();
    if let Ok(i) = uart.read() {
        i as usize
    } else {
        1usize.wrapping_neg()
    }
}

/// 保存当前中断使能状态，并关中断（用于 console 临界区）。
pub fn local_irq_save() -> bool {
    let mut crmd = CrMd::read();
    let was_enabled = crmd.is_interrupt_enabled();
    crmd.set_ie(false);
    crmd.write();
    was_enabled
}

/// 返回当前核中断是否使能（CRMD.IE）。
///
/// 网络发送路径用它区分"调度器/任务上下文（中断开启，可等待 VirtIO
/// completion 中断）"与"syscall/trap 上下文（中断关闭，等不到 completion，
/// 必须延迟发送）"。
pub fn irq_enabled() -> bool {
    CrMd::read().is_interrupt_enabled()
}

/// 恢复中断使能状态到调用 local_irq_save 之前的值。
pub fn local_irq_restore(was_enabled: bool) {
    if was_enabled {
        let mut crmd = CrMd::read();
        crmd.set_ie(true);
        crmd.write();
    }
}

/// Write a byte slice to the console.
///
/// LoongArch64 already writes directly to UART MMIO (no SBI ecall overhead),
/// so the benefit is marginal compared to rv64.  Same per-character loop as
/// [`console_putchar`], but inlined to avoid the static-method call overhead.
pub fn console_write_bytes(data: &[u8]) {
    for &b in data {
        console_putchar(b as usize);
    }
}

pub fn shutdown() -> ! {
    // SAFETY: [Category 11 — Provenance] QEMU's LoongArch platform reserves this
    // MMIO address for power management. This sole shutdown path performs one
    // volatile byte write and does not create a Rust reference to the register.
    unsafe {
        (0x100E_001C as *mut u8).write_volatile(0x34);
    }
    loop {}
}

pub fn reboot() -> ! {
    shutdown()
}
