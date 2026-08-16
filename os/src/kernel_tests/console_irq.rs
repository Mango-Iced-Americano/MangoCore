use alloc::{vec, vec::Vec};

use crate::kernel_tests::runner::KernelTest;

const LOOPBACK_TIMEOUT_MS: usize = 2000;

pub fn tests() -> Vec<KernelTest> {
    vec![KernelTest::with_timeout(
        "console_irq::uart_loopback_rx_interrupt",
        uart_loopback_rx_interrupt,
        8_000,
    )]
}

fn uart_loopback_rx_interrupt() -> Result<(), &'static str> {
    if crate::hal::arch::riscv::plic::boot_cpu_context().is_none() {
        return Err("SKIP: requires an initialized boot-CPU PLIC");
    }
    if !crate::hal::arch::riscv::sbi::console_uart_set_loopback(true) {
        return Err("SKIP: runtime UART loopback not supported");
    }

    let before = crate::hal::arch::riscv::sbi::console_rx_irq_count();
    // THR 写入后 16550 loopback 把同一字节送进 RX FIFO；RX 中断路径完全由
    // 硬件驱动：PLIC(irq 10) -> SEIE -> SupervisorExternal -> console_rx_interrupt。
    if !crate::hal::arch::riscv::sbi::console_uart_putchar(b'Q') {
        crate::hal::arch::riscv::sbi::console_uart_set_loopback(false);
        return Err("UART THR write failed");
    }
    let deadline = crate::hal::get_time()
        + crate::timer::ns_to_ticks_ceil(LOOPBACK_TIMEOUT_MS as u64 * 1_000_000) as usize;
    let fired = crate::hal::with_local_interrupts_enabled(|| {
        while crate::hal::arch::riscv::sbi::console_rx_irq_count() <= before {
            if crate::hal::get_time() >= deadline {
                return false;
            }
            crate::task::run_task_safe_point();
            core::hint::spin_loop();
        }
        true
    });
    crate::hal::arch::riscv::sbi::console_uart_set_loopback(false);
    if !fired {
        return Err("console RX interrupt did not fire via UART loopback");
    }
    crate::println!(
        "[console_irq] RX IRQ fired via UART loopback: before={}, after={}",
        before,
        crate::hal::arch::riscv::sbi::console_rx_irq_count()
    );
    Ok(())
}