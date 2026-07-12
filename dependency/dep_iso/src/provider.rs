/// External functions that drivers must use
pub trait Provider {
    /// Page size (usually 4K)
    const PAGE_SIZE: usize;

    /// Port bitmap to restore after a host-controller reset.
    ///
    /// Most AHCI controllers preserve PI across reset. Platform integrations
    /// whose reset clears PI can provide the firmware-defined bitmap here.
    const AHCI_PORTS_IMPLEMENTED: Option<u32> = None;

    /// Writable AHCI capability bits that must survive a controller reset.
    ///
    /// Generic controllers expose CAP as read-only and leave both values at
    /// zero. Platform integrations with writable CAP bits can preserve a
    /// masked subset and force board-required capabilities after reset.
    const AHCI_CAPABILITY_SAVE_MASK: u32 = 0;
    const AHCI_CAPABILITY_FORCE_BITS: u32 = 0;

    /// Busy-wait for at least `micros` microseconds.
    ///
    /// Platform providers should override this with an architectural stable
    /// counter. The fallback is only for controllers whose links are already
    /// active and therefore never enter timed recovery.
    fn delay_us(micros: usize) {
        for _ in 0..micros.saturating_mul(100) {
            core::hint::spin_loop();
        }
    }

    /// Allocate consequent physical memory for DMA.
    /// Return (`virtual address`, `physical address`).
    /// The address is page aligned.
    fn alloc_dma(size: usize) -> (usize, usize);

    /// Deallocate DMA
    fn dealloc_dma(vaddr: usize, size: usize);
}
