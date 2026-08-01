#![allow(static_mut_refs)]

//! Firmware description providers.
//!
//! Abstracts how the kernel discovers hardware: Flattened Device Tree (FDT),
//! ACPI tables, or compile-time static configuration.
//!
//! # Two-phase initialization
//!
//! 1. **Pre-heap** (`populate_memory_regions`): Validate and retain the raw
//!    DTB, then parse only `/memory` nodes to populate `MEMORY_BUF`. Called
//!    before `mm::init()` and BSS clear. Zero-allocation.
//!
//! 2. **Post-heap** (`build_platform_info`): Full FDT parse producing
//!    `PlatformInfo` with device nodes, cmdline, etc. Called after `mm::init()`.

mod fdt;
#[cfg(not(target_arch = "riscv64"))]
mod static_provider;

pub use fdt::build_platform_info;

use crate::hal::boot;
#[cfg(not(target_arch = "riscv64"))]
use static_provider::{FIRMWARE_RESERVED_REGIONS_FALLBACK, MEMORY_REGIONS_FALLBACK};

/// Maximum number of DRAM banks supported.
pub const MAX_MEMORY_REGIONS: usize = 8;
/// Maximum number of firmware-reserved regions.
pub const MAX_FIRMWARE_RESERVED: usize = 8;
/// Maximum FDT-defined MMIO intervals mapped before driver probing.
pub const MAX_EARLY_MMIO_RANGES: usize = 128;
/// Maximum validated FDT size retained across BSS clear.
pub const MAX_FDT_SNAPSHOT_SIZE: usize = 2 * 1024 * 1024;

/// Static buffer for memory regions populated during early boot.
///
/// `populate_memory_regions()` writes here before `mm::init()`; frame allocation
/// reads the finalized data for the remainder of the kernel lifetime.
#[link_section = ".data.boot"]
pub static mut MEMORY_BUF: MemoryRegionBuf = MemoryRegionBuf::new();

/// Fixed-capacity FDT bytes retained before BSS clear.
///
/// The pre-heap boot path copies validated firmware bytes, then publishes
/// `len` as its final write. The snapshot is immutable for the remainder of
/// the kernel lifetime.
struct FdtSnapshot {
    bytes: [u8; MAX_FDT_SNAPSHOT_SIZE],
    len: usize,
}

impl FdtSnapshot {
    const fn new() -> Self {
        Self {
            bytes: [0; MAX_FDT_SNAPSHOT_SIZE],
            len: 0,
        }
    }
}

#[link_section = ".data.boot"]
static mut FDT_SNAPSHOT: FdtSnapshot = FdtSnapshot::new();

/// Fixed-capacity buffer holding the FDT resources needed before allocation.
///
/// Populated by `populate_memory_regions()` from static configuration.
/// Read by `memory_regions()` and `firmware_reserved_regions()`.
pub struct MemoryRegionBuf {
    pub regions: [(usize, usize); MAX_MEMORY_REGIONS],
    pub reserved: [(usize, usize); MAX_FIRMWARE_RESERVED],
    pub mmio: [(usize, usize); MAX_EARLY_MMIO_RANGES],
    pub region_count: usize,
    pub reserved_count: usize,
    pub mmio_count: usize,
    pub timebase_frequency: usize,
}

impl MemoryRegionBuf {
    pub const fn new() -> Self {
        Self {
            regions: [(0, 0); MAX_MEMORY_REGIONS],
            reserved: [(0, 0); MAX_FIRMWARE_RESERVED],
            mmio: [(0, 0); MAX_EARLY_MMIO_RANGES],
            region_count: 0,
            reserved_count: 0,
            mmio_count: 0,
            timebase_frequency: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.region_count == 0
    }
}

/// Only the standard RV64 protocol provides an FDT in a1.
#[cfg(target_arch = "riscv64")]
fn has_valid_dtb() -> bool {
    let bi = boot::boot_info();
    matches!(bi.protocol, crate::hal::boot::BootProtocol::RiscvFdt)
        && bi.dtb_paddr != 0
        && bi.dtb_paddr & 0x3 == 0
}

/// Populate MEMORY_BUF from firmware data (FDT) or static fallback.
///
/// Called before `mem_clear()` and `mm::init()`.
/// Must NOT allocate — operates on raw bytes.
pub fn populate_memory_regions() {
    #[cfg(target_arch = "riscv64")]
    {
        if !has_valid_dtb() {
            panic!("RV64 boot requires an aligned FDT in a1");
        }
        let dtb_paddr = boot::boot_info().dtb_paddr;
        if fdt::capture_fdt_snapshot(dtb_paddr) && fdt::parse_memory_regions(dtb_paddr) {
            return;
        }
        panic!("RV64 boot FDT validation or pre-heap discovery failed");
    }
    #[cfg(not(target_arch = "riscv64"))]
    {
        populate_from_static();
        crate::println!("[firmware] Using static memory configuration");
    }
}

/// Return the active memory regions as a slice.
/// Called by `for_each_usable_frame_region()` in the frame allocator.
pub fn memory_regions() -> &'static [(usize, usize)] {
    // SAFETY: MEMORY_BUF is populated before mm::init() and never modified after.
    let buffer = unsafe { &*core::ptr::addr_of!(MEMORY_BUF) };
    if buffer.is_empty() {
        #[cfg(target_arch = "riscv64")]
        panic!("RV64 FDT memory resources were not initialized");
        #[cfg(not(target_arch = "riscv64"))]
        return MEMORY_REGIONS_FALLBACK;
    }
    &buffer.regions[..buffer.region_count]
}

/// Return the active firmware-reserved regions as a slice.
pub fn firmware_reserved_regions() -> &'static [(usize, usize)] {
    // SAFETY: MEMORY_BUF is populated before mm::init() and never modified after.
    let buffer = unsafe { &*core::ptr::addr_of!(MEMORY_BUF) };
    if buffer.is_empty() {
        #[cfg(target_arch = "riscv64")]
        panic!("RV64 FDT reserved resources were not initialized");
        #[cfg(not(target_arch = "riscv64"))]
        return FIRMWARE_RESERVED_REGIONS_FALLBACK;
    }
    &buffer.reserved[..buffer.reserved_count]
}

/// Return FDT MMIO ranges which must be identity-mapped before drivers probe.
pub fn early_mmio_ranges() -> &'static [(usize, usize)] {
    // SAFETY: The pre-heap parser completes before the page-table constructor.
    let buffer = unsafe { &*core::ptr::addr_of!(MEMORY_BUF) };
    &buffer.mmio[..buffer.mmio_count]
}

/// Return the runtime FDT timebase frequency captured before timer setup.
pub fn timebase_frequency() -> usize {
    // SAFETY: The pre-heap parser publishes this scalar before timer users run.
    let frequency = unsafe { (*core::ptr::addr_of!(MEMORY_BUF)).timebase_frequency };
    if frequency == 0 {
        panic!("firmware timebase frequency was not initialized");
    }
    frequency
}

/// Sum discovered RAM ranges for runtime accounting.
pub fn usable_memory_size() -> usize {
    memory_regions()
        .iter()
        .fold(0usize, |total, (start, end)| total.saturating_add(end.saturating_sub(*start)))
}

/// Fill MEMORY_BUF from compile-time constants.
#[cfg(not(target_arch = "riscv64"))]
fn populate_from_static() {
    // SAFETY: This runs during single-threaded early boot before mm::init().
    let buffer = unsafe { &mut *core::ptr::addr_of_mut!(MEMORY_BUF) };
    buffer.region_count = 0;
    buffer.reserved_count = 0;

    for (index, &(start, end)) in MEMORY_REGIONS_FALLBACK.iter().enumerate() {
        if index >= MAX_MEMORY_REGIONS {
            break;
        }
        buffer.regions[index] = (start, end);
        buffer.region_count = index + 1;
    }
    for (index, &(start, end)) in FIRMWARE_RESERVED_REGIONS_FALLBACK.iter().enumerate() {
        if index >= MAX_FIRMWARE_RESERVED {
            break;
        }
        buffer.reserved[index] = (start, end);
        buffer.reserved_count = index + 1;
    }
}
