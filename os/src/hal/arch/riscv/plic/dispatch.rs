use core::sync::atomic::{AtomicUsize, Ordering};

use spin::Mutex;

use super::{
    local_plic,
    mmio::{fence_io, MAX_IRQS},
    plic_for_cpu,
};

const CLAIM_BUDGET: usize = 32;

/// Registered interrupt callback. Callbacks run with supervisor interrupts
/// masked, so they must only perform bounded, lock-free acknowledgement work.
pub type IrqHandler = fn();

static IRQ_HANDLERS: Mutex<[Option<IrqHandler>; MAX_IRQS]> = Mutex::new([None; MAX_IRQS]);
/// `0` means no unknown source has been observed; `usize::MAX` means it has
/// already been reported from task context.
static FIRST_UNHANDLED_IRQ: AtomicUsize = AtomicUsize::new(0);

/// Register a callback and enable its source in exactly one logical CPU context.
///
/// Interrupts are locally masked while mutating the handler table so an
/// external interrupt can never spin on a task-context table lock.
pub fn register_handler_on(irq: usize, handler: IrqHandler, target_cpu: usize) -> bool {
    if irq == 0 || irq >= MAX_IRQS || target_cpu >= crate::smp::MAX_CPUS {
        return false;
    }
    let Some(plic) = plic_for_cpu(target_cpu) else {
        return false;
    };

    let interrupts_enabled = super::super::sbi::local_irq_save();
    IRQ_HANDLERS.lock()[irq] = Some(handler);
    plic.enable_source(irq);
    super::super::sbi::local_irq_restore(interrupts_enabled);
    true
}

/// Claim, dispatch, and complete a bounded batch of supervisor external interrupts.
///
/// This is deliberately limited to bounded MMIO and a pre-registered callback.
/// In particular, it never polls smoltcp or logs through the serial console.
pub fn handle_external_interrupt() {
    let Some(plic) = local_plic() else {
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
                plic.mask_source(irq);
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
