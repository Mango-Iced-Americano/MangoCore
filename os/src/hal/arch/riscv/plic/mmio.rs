use core::{
    arch::asm,
    cmp::max,
    mem::size_of,
};

use crate::mm::PhysAddr;

pub(super) const MAX_IRQS: usize = 256;
const PRIORITY_BASE: usize = 0x0000;
const ENABLE_BASE: usize = 0x2000;
const ENABLE_CONTEXT_STRIDE: usize = 0x80;
const ENABLE_WORDS_PER_CONTEXT: usize = ENABLE_CONTEXT_STRIDE / size_of::<u32>();
const CONTEXT_BASE: usize = 0x20_0000;
const CONTEXT_STRIDE: usize = 0x1000;
const CONTEXT_THRESHOLD: usize = 0;
const CONTEXT_CLAIM_COMPLETE: usize = 4;

#[derive(Clone, Copy)]
pub(super) struct Plic {
    base: usize,
    context: usize,
}

impl Plic {
    pub(super) const fn new(base: usize, context: usize) -> Self {
        Self { base, context }
    }

    pub(super) const fn base(self) -> usize {
        self.base
    }

    pub(super) const fn context(self) -> usize {
        self.context
    }

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
        self.base + PRIORITY_BASE + irq * size_of::<u32>()
    }

    pub(super) fn enable_source(self, irq: usize) {
        write_register(self.priority_address(irq), 1);
        let enable_address = self.enable_address(irq);
        let enabled = read_register(enable_address);
        write_register(enable_address, enabled | (1u32 << (irq % 32)));
    }

    pub(super) fn claim(self) -> usize {
        read_register(self.context_address(CONTEXT_CLAIM_COMPLETE)) as usize
    }

    pub(super) fn complete(self, irq: usize) {
        write_register(self.context_address(CONTEXT_CLAIM_COMPLETE), irq as u32);
    }

    pub(super) fn mask_source(self, irq: usize) {
        if irq >= ENABLE_WORDS_PER_CONTEXT * 32 {
            return;
        }
        let enable_address = self.enable_address(irq);
        let enabled = read_register(enable_address);
        write_register(enable_address, enabled & !(1u32 << (irq % 32)));
    }

    pub(super) fn disable(self) {
        write_register(self.context_address(CONTEXT_THRESHOLD), u32::MAX);
        for word in 0..ENABLE_WORDS_PER_CONTEXT {
            write_register(self.enable_word_address(word), 0);
        }
    }

    pub(super) fn initialize_local(self) {
        write_register(self.context_address(CONTEXT_THRESHOLD), 0);
    }
}

pub(super) fn required_register_size(
    contexts: &[Option<usize>; crate::smp::MAX_CPUS],
) -> Option<usize> {
    let priority_end = PRIORITY_BASE.checked_add(MAX_IRQS.checked_mul(size_of::<u32>())?)?;
    let mut required = priority_end;
    for context in contexts.iter().flatten() {
        let enable_end = ENABLE_BASE
            .checked_add(context.checked_mul(ENABLE_CONTEXT_STRIDE)?)
            .and_then(|offset| {
                offset.checked_add(ENABLE_WORDS_PER_CONTEXT.checked_mul(size_of::<u32>())?)
            })?;
        let context_end = CONTEXT_BASE
            .checked_add(context.checked_mul(CONTEXT_STRIDE)?)
            .and_then(|offset| {
                offset.checked_add(CONTEXT_CLAIM_COMPLETE + size_of::<u32>())
            })?;
        required = max(required, max(enable_end, context_end));
    }
    Some(required)
}

#[inline(always)]
pub(super) fn fence_io() {
    // SAFETY: [Category 13 — instruction contract] this emits only the
    // architectural I/O ordering barrier; the checked PLIC MMIO sequence on
    // either side owns all memory accesses.
    unsafe { asm!("fence iorw, iorw", options(nostack)) }
}

#[inline(always)]
fn read_register(address: usize) -> u32 {
    // SAFETY: [Categories 6 and 10 — alignment and bounds] init_controller()
    // validates every published context register against this MMIO range.
    unsafe { core::ptr::read_volatile(PhysAddr(address).direct_map_ptr().cast::<u32>()) }
}

#[inline(always)]
fn write_register(address: usize, value: u32) {
    // SAFETY: [Categories 6 and 10 — alignment and bounds] the same checked
    // PLIC address generation as read_register() applies to every write.
    unsafe { core::ptr::write_volatile(PhysAddr(address).direct_map_ptr().cast::<u32>(), value) }
}
