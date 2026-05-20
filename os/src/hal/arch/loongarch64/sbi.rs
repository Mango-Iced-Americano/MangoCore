#![allow(unused)]

use embedded_hal::serial::nb::{Read, Write};

use crate::drivers::Ns16550a;
use core::{arch::asm, mem::MaybeUninit};

use super::acpi::Pm1Cnt;
use super::board::UART_BASE;
use super::register::CrMd;

pub static mut UART: Ns16550a = Ns16550a { base: UART_BASE };

pub fn console_putchar(c: usize) {
    let mut retry = 0;
    unsafe {
        UART.write(c as u8);
    }
}

pub fn console_flush() {
    unsafe { while UART.flush().is_err() {} }
}

pub fn console_getchar() -> usize {
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

pub fn shutdown() -> ! {
    let mut pm1_cnt: Pm1Cnt = Pm1Cnt::empty();
    // pm1_cnt.set_s5().write();
    unsafe {
        (0x100E_001C as *mut u8).write_volatile(0x34);
    }
    loop {}
}