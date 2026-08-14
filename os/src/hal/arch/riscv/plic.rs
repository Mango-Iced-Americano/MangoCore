//! Minimal supervisor-mode PLIC dispatch for the single-hart kernel.
//!
//! Device callbacks only acknowledge hardware and publish lightweight work for
//! task context.  They must not acquire scheduler or network-stack locks.

use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use crate::hal::platform::{self, DeviceKind};
use crate::mm::PhysAddr;

/// Maximum PLIC source ID accepted by the static callback table.
///
/// JH7110 advertises 136 sources and QEMU virt has considerably fewer.
const MAX_IRQS: usize = 256;
const PRIORITY_BASE: usize = 0x0000;
const ENABLE_BASE: usize = 0x2000;
const ENABLE_CONTEXT_STRIDE: usize = 0x80;
const CONTEXT_BASE: usize = 0x20_0000;
const CONTEXT_STRIDE: usize = 0x1000;
const CONTEXT_THRESHOLD: usize = 0;
const CONTEXT_CLAIM_COMPLETE: usize = 4;

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
    fn enable_address(self, irq: usize) -> usize {
        self.base + ENABLE_BASE + self.context * ENABLE_CONTEXT_STRIDE + (irq / 32) * 4
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

/// Discover and enable the supervisor PLIC context from the retained FDT.
pub fn init() {
    let Some(range) = platform::platform_info()
        .devices
        .iter()
        .find(|device| {
            device.kind == DeviceKind::InterruptController
                && device.is_enabled()
                && device.mmio_range(0).is_some()
        })
        .and_then(|device| device.mmio_range(0))
    else {
        return;
    };
    let Some(context) = supervisor_context() else {
        return;
    };
    let Some(required_size) = CONTEXT_BASE
        .checked_add(context.saturating_mul(CONTEXT_STRIDE))
        .and_then(|offset| offset.checked_add(CONTEXT_CLAIM_COMPLETE + 4))
    else {
        return;
    };
    if range.size < required_size {
        return;
    }

    // SAFETY: FDT validation supplied an aligned, identity-mapped PLIC MMIO
    // range, and the bounds check above covers this supervisor context.
    write_register(range.base + CONTEXT_BASE + context * CONTEXT_STRIDE + CONTEXT_THRESHOLD, 0);
    PLIC_CONTEXT.store(context, Ordering::Relaxed);
    PLIC_BASE.store(range.base, Ordering::Release);
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

/// Claim, dispatch, and complete one supervisor external interrupt.
///
/// This is deliberately limited to bounded MMIO and a pre-registered callback.
/// In particular, it never polls smoltcp or logs through the serial console.
pub fn handle_external_interrupt() {
    let Some(plic) = configured_plic() else {
        return;
    };
    let irq = plic.claim();
    if irq == 0 {
        return;
    }

    let handler = if irq < MAX_IRQS {
        IRQ_HANDLERS.lock()[irq]
    } else {
        None
    };
    match handler {
        Some(handler) => handler(),
        None => record_unhandled_irq(irq),
    }
    plic.complete(irq);
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
fn read_register(address: usize) -> u32 {
    // SAFETY: PLIC setup validates that all generated physical register
    // addresses are aligned and covered by the supervisor MMIO map.
    unsafe { core::ptr::read_volatile(PhysAddr(address).direct_map_ptr().cast::<u32>()) }
}

#[inline(always)]
fn write_register(address: usize, value: u32) {
    // SAFETY: same validated supervisor MMIO mapping as read_register.
    unsafe { core::ptr::write_volatile(PhysAddr(address).direct_map_ptr().cast::<u32>(), value) }
}
