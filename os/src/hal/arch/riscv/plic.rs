//! Minimal supervisor-mode PLIC dispatch for the boot CPU.
//!
//! Device callbacks only acknowledge hardware and publish lightweight work for
//! task context.  They must not acquire scheduler or network-stack locks.

use core::{
    arch::asm,
    cmp::max,
    mem::size_of,
    sync::atomic::{AtomicUsize, Ordering},
};
use spin::Mutex;

use crate::hal::platform::{self, DeviceInfo, DeviceKind, MmioRange};

/// Maximum PLIC source ID accepted by the static callback table.
///
/// JH7110 advertises 136 sources and QEMU virt has considerably fewer.
const MAX_IRQS: usize = 256;
const PRIORITY_BASE: usize = 0x0000;
const ENABLE_BASE: usize = 0x2000;
const ENABLE_CONTEXT_STRIDE: usize = 0x80;
const ENABLE_WORDS_PER_CONTEXT: usize = ENABLE_CONTEXT_STRIDE / size_of::<u32>();
const CONTEXT_BASE: usize = 0x20_0000;
const CONTEXT_STRIDE: usize = 0x1000;
const CONTEXT_THRESHOLD: usize = 0;
const CONTEXT_CLAIM_COMPLETE: usize = 4;
const CLAIM_BUDGET: usize = 32;

/// Registered interrupt callback. Callbacks run with supervisor interrupts
/// masked, so they must only perform bounded, lock-free acknowledgement work.
pub type IrqHandler = fn();

static PLIC_BASE: AtomicUsize = AtomicUsize::new(0);
static PLIC_CONTEXT: AtomicUsize = AtomicUsize::new(0);
static IRQ_HANDLERS: Mutex<[Option<IrqHandler>; MAX_IRQS]> = Mutex::new([None; MAX_IRQS]);
/// `0` means no unknown source has been observed; `usize::MAX` means it has
/// already been reported from task context.
static FIRST_UNHANDLED_IRQ: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy)]
struct Plic {
    base: usize,
    context: usize,
}

impl Plic {
    fn enable_word_address(self, word: usize) -> usize {
        self.base + ENABLE_BASE + self.context * ENABLE_CONTEXT_STRIDE + word * size_of::<u32>()
    }

    fn enable_address(self, irq: usize) -> usize {
        self.enable_word_address(irq / 32)
    }

    fn context_address(self, offset: usize) -> usize {
        self.base + CONTEXT_BASE + self.context * CONTEXT_STRIDE + offset
    }

    fn priority_address(self, irq: usize) -> usize {
        self.base + PRIORITY_BASE + irq * 4
    }

    fn claim(self) -> usize {
        read_register(self.context_address(CONTEXT_CLAIM_COMPLETE)) as usize
    }

    fn complete(self, irq: usize) {
        write_register(self.context_address(CONTEXT_CLAIM_COMPLETE), irq as u32);
    }

    fn mask_source_for_local_context(self, irq: usize) {
        if irq >= ENABLE_WORDS_PER_CONTEXT * 32 {
            return;
        }
        let enable_address = self.enable_address(irq);
        let enabled = read_register(enable_address);
        write_register(enable_address, enabled & !(1u32 << (irq % 32)));
    }
}

fn configured_plic() -> Option<Plic> {
    let base = PLIC_BASE.load(Ordering::Acquire);
    (base != 0).then(|| Plic {
        base,
        context: PLIC_CONTEXT.load(Ordering::Relaxed),
    })
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

fn select_plic_range() -> Option<MmioRange> {
    let devices = &platform::platform_info().devices;
    devices
        .iter()
        .find(|device| is_usable_interrupt_controller(device) && is_plic_compatible(device))
        .or_else(|| devices.iter().find(|device| is_usable_interrupt_controller(device)))
        .and_then(|device| device.mmio_range(0))
}

fn required_register_size(context: usize) -> Option<usize> {
    let priority_end = PRIORITY_BASE.checked_add(MAX_IRQS.checked_mul(size_of::<u32>())?)?;
    let enable_end = ENABLE_BASE
        .checked_add(context.checked_mul(ENABLE_CONTEXT_STRIDE)?)
        .and_then(|offset| {
            offset.checked_add(ENABLE_WORDS_PER_CONTEXT.checked_mul(size_of::<u32>())?)
        })?;
    let context_end = CONTEXT_BASE
        .checked_add(context.checked_mul(CONTEXT_STRIDE)?)
        .and_then(|offset| offset.checked_add(CONTEXT_CLAIM_COMPLETE + size_of::<u32>()))?;
    Some(max(priority_end, max(enable_end, context_end)))
}

/// Discover, quiesce, and publish the boot CPU's supervisor PLIC context.
///
/// Publication happens only after every register used by this L1 dispatcher has
/// been checked against the FDT range and the inherited enable state is gone.
pub fn init_boot_cpu() -> bool {
    let Some(range) = select_plic_range() else {
        return false;
    };
    let Some(context) = supervisor_context() else {
        return false;
    };
    let Some(required_size) = required_register_size(context) else {
        return false;
    };
    if range.base % size_of::<u32>() != 0
        || range.size < required_size
        || range.base.checked_add(required_size).is_none()
    {
        return false;
    }

    let plic = Plic {
        base: range.base,
        context,
    };
    // Keep all inherited sources blocked until the whole local enable bitmap
    // has been cleared. Boot firmware may have left an unrelated level source
    // asserted before its driver installs a callback.
    write_register(plic.context_address(CONTEXT_THRESHOLD), u32::MAX);
    for word in 0..ENABLE_WORDS_PER_CONTEXT {
        write_register(plic.enable_word_address(word), 0);
    }
    write_register(plic.context_address(CONTEXT_THRESHOLD), 0);
    fence_io();
    PLIC_CONTEXT.store(context, Ordering::Relaxed);
    PLIC_BASE.store(range.base, Ordering::Release);
    true
}

/// Return the already-published boot CPU PLIC location for boot diagnostics.
pub fn boot_cpu_context() -> Option<(usize, usize)> {
    configured_plic().map(|plic| (plic.base, plic.context))
}

/// Register and enable one PLIC source.
///
/// Interrupts are locally masked while mutating the handler table so an
/// external interrupt can never spin on a task-context table lock.
pub fn register_handler(irq: usize, handler: IrqHandler) -> bool {
    if irq == 0 || irq >= MAX_IRQS {
        return false;
    }
    let Some(plic) = configured_plic() else {
        return false;
    };

    let interrupts_enabled = super::sbi::local_irq_save();
    IRQ_HANDLERS.lock()[irq] = Some(handler);
    // A nonzero priority makes the source eligible; threshold remains zero.
    write_register(plic.priority_address(irq), 1);
    let enable_address = plic.enable_address(irq);
    let enabled = read_register(enable_address);
    write_register(enable_address, enabled | (1u32 << (irq % 32)));
    super::sbi::local_irq_restore(interrupts_enabled);
    true
}

/// Claim, dispatch, and complete a bounded batch of supervisor external interrupts.
///
/// This is deliberately limited to bounded MMIO and a pre-registered callback.
/// In particular, it never polls smoltcp or logs through the serial console.
pub fn handle_external_interrupt() {
    let Some(plic) = configured_plic() else {
        return;
    };
    for _ in 0..CLAIM_BUDGET {
        let irq = plic.claim();
        if irq == 0 {
            break;
        }

        let handler = if irq < MAX_IRQS {
            IRQ_HANDLERS.lock()[irq]
        } else {
            None
        };
        match handler {
            Some(handler) => handler(),
            None => {
                // Disable unknown level sources before completion so they cannot
                // continuously retrigger and starve the bounded hard-IRQ path.
                plic.mask_source_for_local_context(irq);
                record_unhandled_irq(irq);
            }
        }
        fence_io();
        plic.complete(irq);
    }
}

/// Emit one deferred warning for the first unregistered source.
///
/// The scheduler invokes this outside interrupt context so serial logging cannot
/// deadlock an interrupt handler.
pub fn report_unhandled_irq() {
    let pending = FIRST_UNHANDLED_IRQ.load(Ordering::Acquire);
    if pending == 0 || pending == usize::MAX {
        return;
    }
    if FIRST_UNHANDLED_IRQ
        .compare_exchange(pending, usize::MAX, Ordering::AcqRel, Ordering::Relaxed)
        .is_ok()
    {
        log::warn!("[plic] completed unregistered external irq {}", pending - 1);
    }
}

fn record_unhandled_irq(irq: usize) {
    let _ = FIRST_UNHANDLED_IRQ.compare_exchange(
        0,
        irq.saturating_add(1),
        Ordering::Release,
        Ordering::Relaxed,
    );
}

#[inline(always)]
fn fence_io() {
    // SAFETY: [Category 13 — instruction contract] this emits only the
    // architectural I/O ordering barrier; the checked PLIC MMIO sequence on
    // either side owns all memory accesses.
    unsafe { asm!("fence iorw, iorw", options(nostack)) }
}

#[inline(always)]
fn read_register(address: usize) -> u32 {
    // SAFETY: PLIC setup validates that all generated register addresses are
    // aligned and lie in the FDT-described identity-mapped controller range.
    unsafe { core::ptr::read_volatile(address as *const u32) }
}

#[inline(always)]
fn write_register(address: usize, value: u32) {
    // SAFETY: PLIC setup validates that all generated register addresses are
    // aligned and lie in the FDT-described identity-mapped controller range.
    unsafe { core::ptr::write_volatile(address as *mut u32, value) }
}
