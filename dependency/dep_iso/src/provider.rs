/// External functions that drivers must use
pub trait Provider {
    /// Page size (usually 4K)
    const PAGE_SIZE: usize;

    /// Port bitmap to restore after a host-controller reset.
    ///
    /// Most AHCI controllers preserve PI across reset. Platform integrations
    /// whose reset clears PI can provide the firmware-defined bitmap here.
    const AHCI_PORTS_IMPLEMENTED: Option<u32> = None;

    /// Allocate consequent physical memory for DMA.
    /// Return (`virtual address`, `physical address`).
    /// The address is page aligned.
    fn alloc_dma(size: usize) -> (usize, usize);

    /// Deallocate DMA
    fn dealloc_dma(vaddr: usize, size: usize);
}
