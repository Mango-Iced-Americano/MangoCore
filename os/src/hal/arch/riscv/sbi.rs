//! RISC-V SBI 调用封装。
//!
//! 提供 timer、console、shutdown 和本地中断开关等机器环境接口。

use core::arch::asm;
use core::sync::atomic::{AtomicUsize, Ordering};
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

static CONSOLE_BASE: AtomicUsize = AtomicUsize::new(0);
static CONSOLE_SIZE: AtomicUsize = AtomicUsize::new(0);
static CONSOLE_REGISTER_SHIFT: AtomicUsize = AtomicUsize::new(0);

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
    CONSOLE_REGISTER_SHIFT.store(console.register_shift, Ordering::Release);
    CONSOLE_SIZE.store(console.range.size, Ordering::Release);
    CONSOLE_BASE.store(console.range.base, Ordering::Release);
}

pub fn console_getchar() -> usize {
    let base = CONSOLE_BASE.load(Ordering::Acquire);
    if base == 0 {
        return sbi_call(SBI_CONSOLE_GETCHAR, 0, 0, 0);
    }
    let shift = CONSOLE_REGISTER_SHIFT.load(Ordering::Acquire);
    let size = CONSOLE_SIZE.load(Ordering::Acquire);
    let lsr_offset = 5usize << shift;
    if size <= lsr_offset {
        return usize::MAX;
    }
    // SAFETY: FDT validation recorded an enabled serial `reg` range, KernelSpace
    // identity-mapped that range, and the checked offsets stay inside it.
    let status = unsafe { core::ptr::read_volatile((base + lsr_offset) as *const u8) };
    if status & 1 == 0 {
        return usize::MAX;
    }
    // SAFETY: the data register is at offset zero of the same validated range;
    // the line-status read above established that a byte is available.
    unsafe { core::ptr::read_volatile(base as *const u8) as usize }
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
    let base = CONSOLE_BASE.load(Ordering::Acquire);
    if base == 0 {
        for &b in data {
            console_putchar(b as usize);
        }
        return;
    }
    let shift = CONSOLE_REGISTER_SHIFT.load(Ordering::Acquire);
    let size = CONSOLE_SIZE.load(Ordering::Acquire);
    let lsr_offset = 5usize << shift;
    if size <= lsr_offset {
        return;
    }
    for &byte in data {
        loop {
            // SAFETY: FDT validation recorded an enabled serial `reg` range,
            // KernelSpace identity-mapped it, and the LSR offset is in bounds.
            let status = unsafe { core::ptr::read_volatile((base + lsr_offset) as *const u8) };
            if status & (1 << 5) != 0 {
                break;
            }
        }
        // SAFETY: offset zero is the transmit register in the validated serial
        // range and the THRE handshake above permits the volatile write.
        unsafe { core::ptr::write_volatile(base as *mut u8, byte) };
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
