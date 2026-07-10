//! LoongArch64 平台服务封装。
//!
//! 提供 UART 控制台、关机、时钟频率读取和本地中断开关等 HAL 所需接口。

#![allow(unused)]

use embedded_hal::serial::nb::{Read, Write};

use crate::drivers::Ns16550a;
use core::{arch::asm, mem::MaybeUninit};

use super::board::UART_BASE;
use super::register::CrMd;

pub static mut UART: Ns16550a = Ns16550a { base: UART_BASE };

pub fn console_putchar(c: usize) {
    let mut retry = 0;
    // Safety: early console access is serialized by the kernel console path.
    // The global UART points at the fixed platform MMIO base.
    unsafe {
        UART.write(c as u8);
    }
}

pub fn console_flush() {
    // Safety: same UART singleton contract as `console_putchar`.
    unsafe { while UART.flush().is_err() {} }
}

pub fn console_getchar() -> usize {
    // Safety: same UART singleton contract as `console_putchar`.
    unsafe {
        if let Ok(i) = UART.read() {
            return i as usize;
        } else {
            return 1usize.wrapping_neg();
        }
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

/// 恢复中断使能状态到调用 local_irq_save 之前的值。
pub fn local_irq_restore(was_enabled: bool) {
    if was_enabled {
        let mut crmd = CrMd::read();
        crmd.set_ie(true);
        crmd.write();
    }
}

/// 通过 QEMU 平台关机寄存器关闭 LoongArch 虚拟机。
#[cfg(feature = "board_laqemu")]
pub fn shutdown() -> ! {
    // Safety: this writes the QEMU power-management MMIO shutdown register.
    // The address is platform-defined and the access is volatile.
    unsafe {
        (0x100E_001C as *mut u8).write_volatile(0x34);
    }
    loop {}
}

/// 内核请求关机时停止 2K1000 开发板执行。
#[cfg(feature = "board_2k1000")]
pub fn shutdown() -> ! {
    // HACK(2k1000-shutdown)：避免在实板上写入 QEMU 专用电源 MMIO 寄存器。
    // 依据：2K1000LA 早期上板尚未验证 ACPI/PM S5 关机序列。
    // 移除条件：`board_2k1000` 具备经过实板验证的 ACPI/PM S5 关机实现。
    loop {
        core::hint::spin_loop();
    }
}
