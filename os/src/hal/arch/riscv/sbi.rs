//! RISC-V SBI 调用封装。
//!
//! 提供 timer、console、shutdown 和本地中断开关等机器环境接口。

use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use riscv::register::sstatus;

use crate::drivers::serial::ns16550a::{
    Ns16550a, LSR_BREAK, LSR_FRAMING, LSR_OVERRUN, LSR_PARITY,
};
use mango_kernel_core::uart_rx_ring::ByteRing;

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

static CONSOLE_BASE: AtomicUsize = AtomicUsize::new(0);
static CONSOLE_SIZE: AtomicUsize = AtomicUsize::new(0);
static CONSOLE_REGISTER_SHIFT: AtomicUsize = AtomicUsize::new(0);
static CONSOLE_REGISTER_IO_WIDTH: AtomicUsize = AtomicUsize::new(1);
static CONSOLE_IRQ: AtomicUsize = AtomicUsize::new(0);
const CONSOLE_RX_DRAIN_LIMIT: usize = 64;
const CONSOLE_RX_RING_CAPACITY: usize = 512;
const CONSOLE_RX_REPORT_INTERVAL_MS: usize = 1_000;
static CONSOLE_RX_RING: ByteRing<CONSOLE_RX_RING_CAPACITY> = ByteRing::new();
static CONSOLE_RX_INTERRUPT_PENDING: AtomicBool = AtomicBool::new(false);
static CONSOLE_RX_THROTTLED: AtomicBool = AtomicBool::new(false);
static CONSOLE_RX_RING_OVERRUNS: AtomicUsize = AtomicUsize::new(0);
static CONSOLE_RX_TTY_OVERRUNS: AtomicUsize = AtomicUsize::new(0);
static CONSOLE_RX_LSR_OVERRUNS: AtomicUsize = AtomicUsize::new(0);
static CONSOLE_RX_PARITY_ERRORS: AtomicUsize = AtomicUsize::new(0);
static CONSOLE_RX_FRAMING_ERRORS: AtomicUsize = AtomicUsize::new(0);
static CONSOLE_RX_BREAKS: AtomicUsize = AtomicUsize::new(0);
static CONSOLE_RX_LAST_REPORT_MS: AtomicUsize = AtomicUsize::new(0);

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

/// Publish the serial console that the FDT selected after its MMIO range is mapped.
pub fn configure_runtime_console() {
    let Some(console) = crate::hal::platform::platform_info().console else {
        return;
    };
    CONSOLE_IRQ.store(console.irq.unwrap_or(0), Ordering::Release);
    CONSOLE_REGISTER_IO_WIDTH.store(console.register_io_width, Ordering::Release);
    CONSOLE_REGISTER_SHIFT.store(console.register_shift, Ordering::Release);
    CONSOLE_SIZE.store(console.range.size, Ordering::Release);
    CONSOLE_BASE.store(console.range.base, Ordering::Release);
}

fn runtime_console() -> Option<Ns16550a> {
    let base = CONSOLE_BASE.load(Ordering::Acquire);
    (base != 0).then(|| {
        Ns16550a::new(
            base,
            CONSOLE_SIZE.load(Ordering::Acquire),
            CONSOLE_REGISTER_SHIFT.load(Ordering::Acquire),
            CONSOLE_REGISTER_IO_WIDTH.load(Ordering::Acquire),
        )
    })
}

fn record_line_status_errors(status: u8) {
    if status & LSR_OVERRUN != 0 {
        CONSOLE_RX_LSR_OVERRUNS.fetch_add(1, Ordering::Relaxed);
    }
    if status & LSR_PARITY != 0 {
        CONSOLE_RX_PARITY_ERRORS.fetch_add(1, Ordering::Relaxed);
    }
    if status & LSR_FRAMING != 0 {
        CONSOLE_RX_FRAMING_ERRORS.fetch_add(1, Ordering::Relaxed);
    }
    if status & LSR_BREAK != 0 {
        CONSOLE_RX_BREAKS.fetch_add(1, Ordering::Relaxed);
    }
}

fn throttle_runtime_console_rx(uart: Ns16550a) {
    CONSOLE_RX_THROTTLED.store(true, Ordering::Release);
    let _ = uart.disable_receive_interrupts();
}

fn drain_runtime_uart_fifo() -> bool {
    let Some(uart) = runtime_console() else {
        return false;
    };
    let _ = uart.read_interrupt_identification();
    let mut received = false;
    uart.drain_rx(CONSOLE_RX_DRAIN_LIMIT, |byte, status| {
        received = true;
        record_line_status_errors(status);
        if CONSOLE_RX_RING.push(byte) {
            true
        } else {
            CONSOLE_RX_RING_OVERRUNS.fetch_add(1, Ordering::Relaxed);
            throttle_runtime_console_rx(uart);
            false
        }
    });
    if received {
        CONSOLE_RX_INTERRUPT_PENDING.store(true, Ordering::Release);
    }
    received
}

fn drain_legacy_console() -> bool {
    let mut received = false;
    for _ in 0..CONSOLE_RX_DRAIN_LIMIT {
        let byte = sbi_call(SBI_CONSOLE_GETCHAR, 0, 0, 0);
        if byte == usize::MAX {
            break;
        }
        received = true;
        if !CONSOLE_RX_RING.push(byte as u8) {
            CONSOLE_RX_RING_OVERRUNS.fetch_add(1, Ordering::Relaxed);
            break;
        }
    }
    if received {
        CONSOLE_RX_INTERRUPT_PENDING.store(true, Ordering::Release);
    }
    received
}

/// Register the console RX callback only after the PLIC context is initialized.
pub fn init_runtime_console_rx() {
    let irq = CONSOLE_IRQ.load(Ordering::Acquire);
    let Some(uart) = runtime_console() else {
        return;
    };
    if irq == 0 || !crate::hal::arch::riscv::plic::register_handler(irq, console_rx_interrupt) {
        return;
    }
    if uart.enable_receive_interrupts() {
        let _ = drain_runtime_uart_fifo();
    }
}

/// PLIC callback: consume bounded hardware FIFO work and publish it for the
/// scheduler. It must not take locks or call the line discipline.
fn console_rx_interrupt() {
    let _ = drain_runtime_uart_fifo();
}

/// Bounded polling fallback for missing/masked IRQs and sub-trigger FIFO data.
pub fn poll_runtime_console_rx() -> bool {
    let irq_state = local_irq_save();
    let received = if runtime_console().is_some() {
        drain_runtime_uart_fifo()
    } else {
        drain_legacy_console()
    };
    local_irq_restore(irq_state);
    received
}

/// Consume the IRQ wake flag in task context.
pub fn take_runtime_console_rx_interrupt() -> bool {
    CONSOLE_RX_INTERRUPT_PENDING.swap(false, Ordering::AcqRel)
}

/// Drain producer-buffer bytes in scheduler context. Returning false from the
/// consumer retains later bytes and applies UART backpressure.
pub fn drain_runtime_console_rx(mut consume: impl FnMut(u8) -> bool) -> usize {
    let mut drained = 0;
    while let Some(byte) = CONSOLE_RX_RING.pop() {
        drained += 1;
        if !consume(byte) {
            if let Some(uart) = runtime_console() {
                throttle_runtime_console_rx(uart);
            }
            CONSOLE_RX_INTERRUPT_PENDING.store(true, Ordering::Release);
            break;
        }
    }
    drained
}

/// Account a TTY hard-limit drop in task context and stop UART interrupts until
/// the scheduler observes consumer capacity again.
pub fn note_tty_input_overrun() {
    CONSOLE_RX_TTY_OVERRUNS.fetch_add(1, Ordering::Relaxed);
    if let Some(uart) = runtime_console() {
        throttle_runtime_console_rx(uart);
    }
}

/// Re-enable RX only after the TTY and producer ring both have room.
pub fn resume_runtime_console_rx(tty_has_space: bool) {
    if !tty_has_space || !CONSOLE_RX_RING.has_space() || !CONSOLE_RX_THROTTLED.load(Ordering::Acquire) {
        return;
    }
    let Some(uart) = runtime_console() else {
        return;
    };
    if uart.enable_receive_interrupts() {
        CONSOLE_RX_THROTTLED.store(false, Ordering::Release);
    }
}

/// Emit rate-limited RX error accounting only from scheduler context.
pub fn report_runtime_console_rx_overruns() {
    let now = crate::timer::get_time_ms();
    let last = CONSOLE_RX_LAST_REPORT_MS.load(Ordering::Relaxed);
    if last != 0 && now.saturating_sub(last) < CONSOLE_RX_REPORT_INTERVAL_MS {
        return;
    }
    let ring = CONSOLE_RX_RING_OVERRUNS.swap(0, Ordering::Relaxed);
    let tty = CONSOLE_RX_TTY_OVERRUNS.swap(0, Ordering::Relaxed);
    let lsr = CONSOLE_RX_LSR_OVERRUNS.swap(0, Ordering::Relaxed);
    let parity = CONSOLE_RX_PARITY_ERRORS.swap(0, Ordering::Relaxed);
    let framing = CONSOLE_RX_FRAMING_ERRORS.swap(0, Ordering::Relaxed);
    let breaks = CONSOLE_RX_BREAKS.swap(0, Ordering::Relaxed);
    if ring + tty + lsr + parity + framing + breaks == 0 {
        return;
    }
    CONSOLE_RX_LAST_REPORT_MS.store(now, Ordering::Relaxed);
    log::warn!(
        "[uart-rx] ring_overruns={} tty_overruns={} lsr_overruns={} parity={} framing={} breaks={}",
        ring,
        tty,
        lsr,
        parity,
        framing,
        breaks
    );
}

pub fn console_getchar() -> usize {
    let _ = poll_runtime_console_rx();
    CONSOLE_RX_RING.pop().map(usize::from).unwrap_or(usize::MAX)
}

pub fn console_flush() {}

/// 保存当前中断使能状态，并关中断（用于 console 临界区）。
pub fn local_irq_save() -> bool {
    let was_enabled = sstatus::read().sie();
    // Safety: clearing SIE only changes the local hart interrupt-enable bit.
    unsafe { sstatus::clear_sie() };
    was_enabled
}

/// 返回当前 hart 中断是否使能（sstatus.SIE）。
///
/// 网络发送路径用它区分"调度器/任务上下文（中断开启，可等待 VirtIO
/// completion 中断）"与"syscall/trap 上下文（中断关闭，等不到 completion，
/// 必须延迟发送）"。
pub fn irq_enabled() -> bool {
    sstatus::read().sie()
}

/// 恢复中断使能状态到调用 local_irq_save 之前的值。
pub fn local_irq_restore(was_enabled: bool) {
    if was_enabled {
        // Safety: restoring SIE only changes the local hart interrupt-enable bit.
        unsafe { sstatus::set_sie() };
    }
}

pub fn console_write_bytes(data: &[u8]) {
    let Some(uart) = runtime_console() else {
        for &b in data {
            console_putchar(b as usize);
        }
        return;
    };
    for &byte in data {
        while !uart.try_write(byte) {}
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
