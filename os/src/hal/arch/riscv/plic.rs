//! Supervisor-mode PLIC dispatch with one context per logical CPU.
//!
//! Device callbacks only acknowledge hardware and publish lightweight work for
//! task context.  They must not acquire scheduler or network-stack locks.

mod dispatch;
mod mmio;

use core::{
    mem::size_of,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use crate::hal::platform::{self, DeviceInfo, DeviceKind};

use mmio::{fence_io, required_register_size, Plic};

pub use dispatch::{handle_external_interrupt, register_handler_on, report_unhandled_irq, IrqHandler};

static PLIC_BASE: AtomicUsize = AtomicUsize::new(0);

struct PlicCpuContext {
    /// FDT context index + 1; zero means this logical CPU has no S-mode PLIC context.
    index_plus_one: AtomicUsize,
    /// 本 CPU 已解除 threshold 后才允许 claim/complete，避免 AP 在本地 MMIO
    /// 初始化完成前进入 external trap 路径。
    initialized: AtomicBool,
}

impl PlicCpuContext {
    const fn new() -> Self {
        Self {
            index_plus_one: AtomicUsize::new(0),
            initialized: AtomicBool::new(false),
        }
    }
}

static PLIC_CONTEXTS: [PlicCpuContext; crate::smp::MAX_CPUS] =
    [const { PlicCpuContext::new() }; crate::smp::MAX_CPUS];

fn plic_for_cpu(cpu_id: usize) -> Option<Plic> {
    let base = PLIC_BASE.load(Ordering::Acquire);
    let context_plus_one = PLIC_CONTEXTS
        .get(cpu_id)?
        .index_plus_one
        .load(Ordering::Acquire);
    (base != 0 && context_plus_one != 0).then(|| Plic::new(base, context_plus_one - 1))
}

fn local_plic() -> Option<Plic> {
    let cpu_id = crate::smp::cpu_id();
    PLIC_CONTEXTS
        .get(cpu_id)?
        .initialized
        .load(Ordering::Acquire)
        .then(|| plic_for_cpu(cpu_id))?
}

fn supervisor_context() -> Option<usize> {
    let hart = crate::hal::boot::boot_info().hart_id;
    if platform::is_real_board() {
        // JH7110 lists Hart 0 machine-external first, then machine/supervisor
        // pairs for Harts 1..4. VisionFive 2 boots S-mode Linux-class kernels
        // on Hart 1, whose supervisor context is therefore 2.
        hart.checked_sub(1)?.checked_mul(2)?.checked_add(2)
    } else {
        // QEMU virt lists machine/supervisor contexts as pairs for each hart.
        hart.checked_mul(2)?.checked_add(1)
    }
}

fn is_plic_compatible(device: &DeviceInfo) -> bool {
    device.compatible.iter().any(|compatible| {
        matches!(
            compatible.as_str(),
            "riscv,plic0" | "riscv,plic" | "sifive,plic-1.0.0"
        )
    })
}

fn is_usable_interrupt_controller(device: &DeviceInfo) -> bool {
    device.kind == DeviceKind::InterruptController
        && device.is_enabled()
        && device.mmio_range(0).is_some()
}

fn select_plic_device() -> Option<&'static DeviceInfo> {
    let devices = &platform::platform_info().devices;
    devices
        .iter()
        .find(|device| is_usable_interrupt_controller(device) && is_plic_compatible(device))
        .or_else(|| devices.iter().find(|device| is_usable_interrupt_controller(device)))
}

fn fallback_contexts() -> Option<[Option<usize>; crate::smp::MAX_CPUS]> {
    let mut contexts = [None; crate::smp::MAX_CPUS];
    contexts[crate::smp::BOOT_CPU_ID] = Some(supervisor_context()?);
    Some(contexts)
}

fn publish_contexts(contexts: &[Option<usize>; crate::smp::MAX_CPUS]) {
    for (cpu_id, context) in contexts.iter().enumerate() {
        let state = &PLIC_CONTEXTS[cpu_id];
        state.initialized.store(false, Ordering::Relaxed);
        state.index_plus_one.store(
            context.and_then(|context| context.checked_add(1)).unwrap_or(0),
            Ordering::Relaxed,
        );
    }
}

fn disable_context(plic: Plic) {
    plic.disable();
}

/// Discover, quiesce, and publish every FDT-described supervisor PLIC context.
///
/// Publication happens only after every published context register has been
/// checked against the FDT range and inherited enables are gone.
pub fn init_controller() -> bool {
    let Some(device) = select_plic_device() else {
        return false;
    };
    let Some(range) = device.mmio_range(0) else {
        return false;
    };
    let (contexts, fallback) = match crate::hal::firmware::riscv_plic_supervisor_contexts(device)
    {
        Some(contexts) if contexts[crate::smp::BOOT_CPU_ID].is_some() => (contexts, false),
        Some(_) | None => match fallback_contexts() {
            Some(contexts) => (contexts, true),
            None => return false,
        },
    };
    let Some(required_size) = required_register_size(&contexts) else {
        return false;
    };
    if range.base % size_of::<u32>() != 0
        || range.size < required_size
        || range.base.checked_add(required_size).is_none()
    {
        return false;
    }

    for context in contexts.iter().flatten() {
        disable_context(Plic::new(range.base, *context));
    }
    fence_io();
    publish_contexts(&contexts);
    PLIC_BASE.store(range.base, Ordering::Release);
    if fallback {
        crate::println!(
            "[plic] interrupts-extended unavailable; using boot-CPU fallback context"
        );
    } else if (1..crate::smp::configured_cpu_count())
        .any(|cpu_id| contexts[cpu_id].is_none())
    {
        crate::println!("[plic] one or more APs lack an S-mode context; SEIE remains disabled there");
    }
    true
}

/// Enable the PLIC threshold for the current logical CPU only.
pub fn init_local_context() -> bool {
    let cpu_id = crate::smp::cpu_id();
    let Some(plic) = plic_for_cpu(cpu_id) else {
        return false;
    };
    plic.initialize_local();
    fence_io();
    PLIC_CONTEXTS[cpu_id]
        .initialized
        .store(true, Ordering::Release);
    true
}

/// Return the already-published boot CPU PLIC location for boot diagnostics.
pub fn boot_cpu_context() -> Option<(usize, usize)> {
    plic_for_cpu(crate::smp::BOOT_CPU_ID).map(|plic| (plic.base(), plic.context()))
}

/// Register sources used by CPU0-only deferred consumers (network and console).
pub fn register_handler(irq: usize, handler: IrqHandler) -> bool {
    register_handler_on(irq, handler, crate::smp::BOOT_CPU_ID)
}
