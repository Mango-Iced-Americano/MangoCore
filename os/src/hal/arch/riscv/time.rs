//! RISC-V 时间源和调度 tick 编程。
//!
//! 优先使用 Sstc 直写 `stimecmp`，不支持时回退 SBI timer；`get_time()` 读取
//! 硬件 time 寄存器。

use crate::hal::platform::info::{DeviceInfo, RawPropertyError};
use core::arch::asm;
use core::sync::atomic::{AtomicBool, Ordering};
use riscv::register::time;

pub const TICKS_PER_SEC: usize = 25;

/// BSP 在 AP 上线前一次性发布；后续 boot-phase Release/Acquire 把该只读选择
/// 传递给 AP，因此 timer 热路径只需 Relaxed load。
static SSTC_ENABLED: AtomicBool = AtomicBool::new(false);

fn string_list_contains(value: &[u8], expected: &[u8]) -> bool {
    let Some(value) = value.strip_suffix(b"\0") else {
        return false;
    };
    let mut found = false;
    for entry in value.split(|byte| *byte == 0) {
        if entry.is_empty() {
            return false;
        }
        found |= entry == expected;
    }
    found
}

fn legacy_isa_contains(value: &[u8], expected: &[u8]) -> bool {
    let Some(value) = value.strip_suffix(b"\0") else {
        return false;
    };
    if value.iter().any(|byte| *byte == 0) {
        return false;
    }
    value
        .split(|byte| *byte == b'_')
        .skip(1)
        .any(|extension| {
            extension == expected
                || extension
                    .strip_prefix(expected)
                    .is_some_and(isa_version_suffix_is_valid)
        })
}

fn isa_version_suffix_is_valid(suffix: &[u8]) -> bool {
    if suffix.first().is_none_or(|byte| !byte.is_ascii_digit()) {
        return false;
    }
    let mut seen_minor = false;
    let mut minor_digits = 0usize;
    for byte in suffix {
        if byte.is_ascii_digit() {
            if seen_minor {
                minor_digits += 1;
            }
        } else if *byte == b'p' && !seen_minor {
            seen_minor = true;
        } else {
            return false;
        }
    }
    !seen_minor || minor_digits != 0
}

/// 优先解析规范化的 ISA extension string-list；只有属性缺失才退回 legacy ISA。
fn device_has_isa_ext(device: &DeviceInfo, expected: &[u8]) -> Option<bool> {
    match device.raw_property("riscv,isa-extensions") {
        Ok(value) => Some(string_list_contains(value, expected)),
        Err(RawPropertyError::Malformed) => Some(false),
        Err(RawPropertyError::Absent) => match device.raw_property("riscv,isa") {
            Ok(value) => Some(legacy_isa_contains(value, expected)),
            Err(RawPropertyError::Malformed) => Some(false),
            Err(RawPropertyError::Absent) => None,
        },
    }
}

fn device_has_sstc(device: &DeviceInfo) -> Option<bool> {
    device_has_isa_ext(device, b"sstc")
}

fn is_cpu_node(device: &DeviceInfo) -> bool {
    if device.parent_path.as_deref() != Some("/cpus") {
        return false;
    }
    let name_is_cpu = device
        .node_path
        .rsplit('/')
        .next()
        .is_some_and(|name| name == "cpu" || name.starts_with("cpu@"));
    let device_type_is_cpu = device
        .raw_property("device_type")
        .ok()
        .is_some_and(|value| value.strip_suffix(b"\0").unwrap_or(value) == b"cpu");
    name_is_cpu || device_type_is_cpu
}

/// 所有 enabled CPU 的 FDT ISA 都明确包含 `expected` 扩展时才返回 true。
pub(super) fn platform_supports_isa_ext(expected: &[u8]) -> bool {
    let platform = crate::hal::platform::platform_info();
    let mut cpu_count = 0usize;
    for cpu in platform
        .devices
        .iter()
        .filter(|device| device.is_enabled() && is_cpu_node(device))
    {
        cpu_count += 1;
        if !device_has_isa_ext(cpu, expected).unwrap_or(false) {
            return false;
        }
    }
    cpu_count == crate::smp::runtime_cpu_count()
}

fn platform_supports_sstc() -> bool {
    platform_supports_isa_ext(b"sstc")
}

/// 在 BSP 首次编程 timer、发布 AP 之前选择整机 timer backend。
pub(super) fn init_timer_backend() {
    let use_sstc = platform_supports_sstc();
    SSTC_ENABLED.store(use_sstc, Ordering::Relaxed);
    if use_sstc {
        crate::println!("[timer] backend=sstc (direct stimecmp)");
    } else {
        crate::println!("[timer] backend=sbi");
    }
}

#[inline(always)]
fn set_timer(deadline: usize) {
    if SSTC_ENABLED.load(Ordering::Relaxed) {
        // Safety: every enabled hart advertised Sstc in FDT. The firmware that
        // enters S-mode must also enable menvcfg.STCE and mcounteren.TM; OpenSBI
        // provides that contract. RV64 writes the complete 64-bit compare CSR.
        unsafe {
            asm!(
                "csrw 0x14d, {deadline}",
                deadline = in(reg) deadline,
                options(nostack)
            )
        };
    } else {
        super::sbi::set_timer(deadline);
    }
}

/// Return current time measured by ticks, which is NOT divided by frequency.
pub fn get_time() -> usize {
    time::read()
}

/// Set next trigger.
pub fn set_next_trigger() {
    set_timer(get_time() + get_clock_freq() / TICKS_PER_SEC);
}

/// Program a one-shot timer to fire after `delta_ticks` raw timer ticks.
#[inline]
pub fn program_timer_delta(delta_ticks: u64) {
    let profile_start = crate::task::processor::sched_profile_cycle_start();
    let now = get_time() as u64;
    set_timer(now.saturating_add(delta_ticks.max(1)) as usize);
    crate::task::processor::record_sched_program_timer_cycles(profile_start);
}

/// 清除当前 hart 的 timer pending，并在安全点处理前不再安排新事件。
///
/// SBI TIME 规定把比较值写到未来必须清除 pending bit；`usize::MAX` 在
/// RV64 上代表最远的绝对时间。安全点完成软件 timer 工作后会重新写入真实
/// deadline，因此 hard IRQ 不需要读取任何受锁队列。
pub fn quiesce_local_timer_interrupt() {
    set_timer(usize::MAX);
}

pub fn get_clock_freq() -> usize {
    crate::hal::firmware::timebase_frequency()
}
