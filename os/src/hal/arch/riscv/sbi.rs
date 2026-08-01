//! RISC-V SBI 调用封装。
//!
//! 提供 timer、console、shutdown 和本地中断开关等机器环境接口。

#![allow(unused)]

use core::{
    arch::asm,
    sync::atomic::{AtomicBool, Ordering},
};
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
const SBI_EXT_IPI: usize = 0x735049;
const SBI_IPI_SEND: usize = 0;
const SBI_EXT_RFENCE: usize = 0x52464e43;
const SBI_RFENCE_REMOTE_SFENCE_VMA_ASID: usize = 2;
const SBI_ERR_NOT_SUPPORTED: isize = -2;
const SBI_ERR_ALREADY_AVAILABLE: isize = -6;

/// CPU0 在 AP 上线前探测并发布；运行期只读，避免每次 shootdown 多做一次 ecall。
static RFENCE_AVAILABLE: AtomicBool = AtomicBool::new(false);

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

/// 通过 SBI v0.2 IPI extension 向一个硬件 hart 触发 supervisor software IRQ。
pub fn send_ipi(hart_id: usize) -> Result<(), isize> {
    if !probe_extension(SBI_EXT_IPI)? {
        return Err(SBI_ERR_NOT_SUPPORTED);
    }

    // 令 hart_mask_base 等于目标 hart ID，mask bit0 就精确表示该 hart，
    // 不依赖 MangoCore logical ID 与 OpenSBI hart ID 是否相同。
    let result = sbi_call_v02(SBI_EXT_IPI, SBI_IPI_SEND, 1, hart_id, 0, 0, 0);
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
        const THR: usize = 0x0; // Transmit Holding Register
        const LSR: usize = 0x5; // Line Status Register
        const THRE: u8 = 1 << 5; // Transmitter Holding Register Empty

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
