//! RISC-V SBI 调用封装。
//!
//! 提供 timer、console、shutdown/reboot、本地中断开关，以及 SMP 所需的
//! SBI v0.2 HSM/IPI/RFENCE 扩展。console 优先使用 FDT 选择的 MMIO 串口
//! （`configure_runtime_console` 发布），未配置时退回 SBI ecall。

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

const SBI_EXT_BASE: usize = 0x10;
const SBI_BASE_PROBE_EXTENSION: usize = 3;
const SBI_EXT_HSM: usize = 0x48534d;
const SBI_HSM_HART_START: usize = 0;
const SBI_EXT_IPI: usize = 0x735049;
const SBI_IPI_SEND: usize = 0;
const SBI_EXT_RFENCE: usize = 0x52464e43;
const SBI_RFENCE_REMOTE_SFENCE_VMA_ASID: usize = 2;
const SBI_ERR_NOT_SUPPORTED: isize = -2;
const SBI_ERR_ALREADY_AVAILABLE: isize = -6;

/// CPU0 在 AP 上线前探测并发布；运行期只读，避免每次 shootdown 多做一次 ecall。
static RFENCE_AVAILABLE: AtomicBool = AtomicBool::new(false);

/// CPU0 在 AP 上线前探测并发布；运行期只读，避免每次 doorbell 多做一次
/// BASE probe ecall（LA64 对应路径直接 IOCSR 写，不进固件）。
static IPI_AVAILABLE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug)]
struct SbiRet {
    error: isize,
    value: usize,
}

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

/// Invoke an SBI v0.2+ extension using the `(error, value)` return convention.
#[inline(always)]
fn sbi_call_v02(
    extension: usize,
    function: usize,
    arg0: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
) -> SbiRet {
    let error: isize;
    let value: usize;
    // Safety: SBI v0.2 ABI 使用 a0-a4 传参数、a6/a7 传 function/extension ID，
    // 并在 a0/a1 返回 error/value；特权级切换期间没有 Rust 引用越过边界。
    unsafe {
        asm!(
            "ecall",
            inlateout("x10") arg0 => error,
            inlateout("x11") arg1 => value,
            in("x12") arg2,
            in("x13") arg3,
            in("x14") arg4,
            in("x16") function,
            in("x17") extension,
        );
    }
    SbiRet { error, value }
}

fn probe_extension(extension: usize) -> Result<bool, isize> {
    let result = sbi_call_v02(
        SBI_EXT_BASE,
        SBI_BASE_PROBE_EXTENSION,
        extension,
        0,
        0,
        0,
        0,
    );
    if result.error == 0 {
        Ok(result.value != 0)
    } else {
        Err(result.error)
    }
}

/// 在 CPU0 上一次性探测 RFENCE，返回值供启动日志说明实际后端。
pub fn init_rfence() -> Result<bool, isize> {
    let available = probe_extension(SBI_EXT_RFENCE)?;
    RFENCE_AVAILABLE.store(available, Ordering::Release);
    Ok(available)
}

/// Start one stopped hart at the physical `_start` address.
pub fn hart_start(hart_id: usize, start_addr: usize, opaque: usize) -> Result<(), isize> {
    if !probe_extension(SBI_EXT_HSM)? {
        return Err(SBI_ERR_NOT_SUPPORTED);
    }

    let result = sbi_call_v02(
        SBI_EXT_HSM,
        SBI_HSM_HART_START,
        hart_id,
        start_addr,
        opaque,
        0,
        0,
    );
    match result.error {
        0 | SBI_ERR_ALREADY_AVAILABLE => Ok(()),
        error => Err(error),
    }
}

/// 在 CPU0 上一次性探测 IPI，返回值供启动日志说明实际后端。
pub fn init_ipi() -> Result<bool, isize> {
    let available = probe_extension(SBI_EXT_IPI)?;
    crate::task::perf::record_ipi_probe();
    IPI_AVAILABLE.store(available, Ordering::Release);
    Ok(available)
}

/// 通过 SBI v0.2 IPI extension 向一个硬件 hart 触发 supervisor software IRQ。
pub fn send_ipi(hart_id: usize) -> Result<(), isize> {
    if !IPI_AVAILABLE.load(Ordering::Acquire) {
        return Err(SBI_ERR_NOT_SUPPORTED);
    }

    let send_start = crate::task::perf::perf_time_now();
    // 令 hart_mask_base 等于目标 hart ID，mask bit0 就精确表示该 hart，
    // 不依赖 MangoCore logical ID 与 OpenSBI hart ID 是否相同。
    let result = sbi_call_v02(SBI_EXT_IPI, SBI_IPI_SEND, 1, hart_id, 0, 0, 0);
    crate::task::perf::record_ipi_send(
        crate::task::perf::perf_time_now().wrapping_sub(send_start),
    );
    if result.error == 0 {
        Ok(())
    } else {
        Err(result.error)
    }
}

/// 让一组硬件 hart 同步失效指定 ASID 的 `[start, start + size)` 翻译。
///
/// SBI RFENCE FID 2 把 ASID 放在 a4；调用成功返回时，所有目标 hart 已完成
/// `SFENCE.VMA` 等价操作，所以下一层可以安全退休被解除映射的 frame。
pub fn remote_sfence_vma_asid(
    hart_mask: usize,
    start: usize,
    size: usize,
    asid: u16,
) -> Result<bool, isize> {
    if !RFENCE_AVAILABLE.load(Ordering::Acquire) {
        return Ok(false);
    }

    let result = sbi_call_v02(
        SBI_EXT_RFENCE,
        SBI_RFENCE_REMOTE_SFENCE_VMA_ASID,
        hart_mask,
        0,
        start,
        size,
        asid as usize,
    );
    match result.error {
        0 => Ok(true),
        SBI_ERR_NOT_SUPPORTED => {
            RFENCE_AVAILABLE.store(false, Ordering::Release);
            Ok(false)
        }
        error => Err(error),
    }
}

pub(super) fn set_timer(timer: usize) {
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

/// Write a byte slice to the console.
///
/// 优先使用 `configure_runtime_console` 发布的 FDT 串口 MMIO（THRE 握手
/// 批量写）；未配置时退回逐字符 SBI ecall。
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

/// panic 输出不经过内核 console 锁；底层 MMIO/SBI 调用本身不持有 Rust 锁。
pub fn panic_console_write(data: &[u8]) {
    console_write_bytes(data);
}

pub fn machine_shutdown() -> ! {
    sbi_call(SBI_SHUTDOWN, 0, 0, 0);
    // 固件若异常返回，也不能再次进入 panic → shutdown 递归；保持本地
    // 中断关闭并永久等待，保留第一次失败现场。
    unsafe { core::arch::asm!("csrw sie, zero", options(nostack)) };
    loop {
        unsafe { riscv::asm::wfi() };
    }
}

/// 统一 HAL 停机入口别名：`shutdown` 保留给 develop 侧调用方使用同一实现。
pub fn shutdown() -> ! {
    machine_shutdown()
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
