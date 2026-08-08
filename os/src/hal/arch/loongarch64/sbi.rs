//! LoongArch64 平台服务封装。
//!
//! 提供 UART 控制台、关机、时钟频率读取和本地中断开关等 HAL 所需接口。

use embedded_hal::serial::nb::{Read, Write};
use spin::Mutex;

use crate::drivers::Ns16550a;
use core::arch::asm;

use super::board::UART_BASE;
use super::register::CrMd;

static UART: Mutex<Ns16550a> = Mutex::new(Ns16550a { base: UART_BASE });

fn write_byte(uart: &mut Ns16550a, byte: u8) {
    // NS16550 `write()` 在 THR 未就绪时返回 WouldBlock；丢弃这个结果会静默丢字符。
    while uart.write(byte).is_err() {
        core::hint::spin_loop();
    }
}

pub fn console_putchar(c: usize) {
    write_byte(&mut UART.lock(), c as u8);
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

/// 持有一次 UART 锁写完整个字节切片，每个字节都等待 THR ready。
pub fn console_write_bytes(data: &[u8]) {
    let mut uart = UART.lock();
    for &b in data {
        write_byte(&mut uart, b);
    }
}

/// panic 时绕过全局 UART 锁。局部句柄只保存 MMIO base，不拥有可别名的内存状态；
/// 即使另一个 CPU 停在普通输出区，也能尽力打印首个 panic 现场。
pub fn panic_console_write(data: &[u8]) {
    let mut uart = Ns16550a { base: UART_BASE };
    for &b in data {
        write_byte(&mut uart, b);
    }
}

pub fn machine_shutdown() -> ! {
    // SAFETY: [Category 11 — Provenance] QEMU's LoongArch platform reserves this
    // MMIO address for power management. This sole shutdown path performs one
    // volatile byte write and does not create a Rust reference to the register.
    unsafe {
        (0x100E_001C as *mut u8).write_volatile(0x34);
    }
    // QEMU 若异常拒绝关机，保持 IE 关闭并低功耗停驻，不能回到 panic 递归。
    loop {
        unsafe { core::arch::asm!("idle 0") };
    }
}

/// 别名：统一 HAL 停机入口按 `machine_shutdown` 命名，`shutdown` 保留给
/// develop 侧调用方使用同一实现。
pub fn shutdown() -> ! {
    machine_shutdown()
}

pub fn reboot() -> ! {
    shutdown()
}
