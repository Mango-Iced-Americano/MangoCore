//! JH7110 L2 cache maintenance for non-coherent DMA buffers.
//!
//! The `FLUSH64` register cleans and invalidates one aligned L2 cache line.
//! DMA submitters must flush every CPU-written descriptor or buffer before the
//! device doorbell; DMA readers must flush the device-written range after
//! completion and before dereferencing it from the CPU. The final I/O fence
//! orders the register triggers with the subsequent device or CPU access.

#[cfg(target_arch = "riscv64")]
const JH7110_L2CC_BASE: usize = 0x0201_0000;
#[cfg(target_arch = "riscv64")]
const JH7110_L2CC_FLUSH64: usize = 0x0200;
#[cfg(target_arch = "riscv64")]
const JH7110_L2_CACHE_LINE_SIZE: usize = 64;

/// Flush every JH7110 L2 line intersecting a physical DMA range.
///
/// On the supported VisionFive 2 mapping this is the exact `FLUSH64` protocol
/// used by the GMAC driver. Other architectures retain only a full memory
/// fence because they do not expose the JH7110 L2 controller.
#[inline(always)]
pub fn jh7110_l2cc_flush_range(physical_address: usize, length: usize) {
    #[cfg(target_arch = "riscv64")]
    {
        if length == 0 {
            return;
        }
        let mut line = physical_address & !(JH7110_L2_CACHE_LINE_SIZE - 1);
        let end_line = physical_address
            .saturating_add(length - 1)
            & !(JH7110_L2_CACHE_LINE_SIZE - 1);
        while line <= end_line {
            // SAFETY: [Categories 6 and 11 — alignment and provenance] the
            // JH7110 L2CC base plus the documented, 32-bit aligned FLUSH64
            // offset is identity-mapped MMIO on the supported RISC-V board.
            unsafe {
                core::ptr::write_volatile(
                    crate::mm::PhysAddr(JH7110_L2CC_BASE + JH7110_L2CC_FLUSH64)
                        .direct_map_ptr()
                        .cast::<u32>(),
                    line as u32,
                );
            }
            line = line.saturating_add(JH7110_L2_CACHE_LINE_SIZE);
        }
    }
    jh7110_dma_barrier();
}

/// Order JH7110 cache-maintenance MMIO against DMA ownership changes.
#[inline(always)]
pub fn jh7110_dma_barrier() {
    #[cfg(target_arch = "riscv64")]
    {
        // SAFETY: `fence iorw, iorw` is a RISC-V ordering instruction with no
        // memory operands and this branch is compiled only for RISC-V.
        unsafe { core::arch::asm!("fence iorw, iorw", options(nostack, preserves_flags)) }
    }
    #[cfg(not(target_arch = "riscv64"))]
    {
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    }
}
