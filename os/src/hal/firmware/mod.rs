#![allow(static_mut_refs)]

//! Firmware description providers.
//!
//! Abstracts how the kernel discovers hardware: Flattened Device Tree (FDT),
//! ACPI tables, or compile-time static configuration.
//!
//! # Two-phase initialization
//!
//! 1. **Pre-heap** (`populate_memory_regions`): Parse only `/memory` nodes
//!    from the DTB to populate `MEMORY_BUF`. Called before `mm::init()`.
//!    Zero-allocation — operates directly on the raw DTB byte slice.
//!
//! 2. **Post-heap** (`build_platform_info`): Full FDT parse producing
//!    `PlatformInfo` with device nodes, cmdline, etc. Called after `mm::init()`.

mod fdt;
mod static_provider;

pub use fdt::build_platform_info;

use crate::hal::boot;
use static_provider::{FIRMWARE_RESERVED_REGIONS_FALLBACK, MEMORY_REGIONS_FALLBACK};

/// Maximum number of DRAM banks supported.
pub const MAX_MEMORY_REGIONS: usize = 8;
/// Maximum number of firmware-reserved regions.
pub const MAX_FIRMWARE_RESERVED: usize = 8;

/// Static buffer for memory regions populated during early boot.
///
/// `populate_memory_regions()` writes here before `mm::init()`; frame allocation
/// reads the finalized data for the remainder of the kernel lifetime.
#[link_section = ".data.boot"]
pub static mut MEMORY_BUF: MemoryRegionBuf = MemoryRegionBuf::new();

/// Fixed-capacity buffer holding DRAM regions and reserved carveouts.
///
/// Populated by `populate_memory_regions()` from static configuration.
/// Read by `memory_regions()` and `firmware_reserved_regions()`.
pub struct MemoryRegionBuf {
    pub regions: [(usize, usize); MAX_MEMORY_REGIONS],
    pub reserved: [(usize, usize); MAX_FIRMWARE_RESERVED],
    pub region_count: usize,
    pub reserved_count: usize,
}

impl MemoryRegionBuf {
    pub const fn new() -> Self {
        Self {
            regions: [(0, 0); MAX_MEMORY_REGIONS],
            reserved: [(0, 0); MAX_FIRMWARE_RESERVED],
            region_count: 0,
            reserved_count: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.region_count == 0
    }
}

/// Only RiscvFdt protocol provides a valid DTB in a1.
/// All other protocols (UbootGo, LoongArchLegacy, Test) must use static fallback.
fn has_valid_dtb() -> bool {
    let bi = boot::boot_info();
    if bi.dtb_paddr == 0 {
        return false;
    }
    matches!(bi.protocol, crate::hal::boot::BootProtocol::RiscvFdt)
}

/// Populate MEMORY_BUF from firmware data (FDT) or static fallback.
///
/// Called after `mem_clear()` and `console::log_init()`, before `mm::init()`.
/// Must NOT allocate — operates on raw bytes.
pub fn populate_memory_regions() {
    if has_valid_dtb() && fdt::parse_memory_regions(boot::boot_info().dtb_paddr) {
        return;
    }
    populate_from_static();
    crate::println!("[firmware] Using static memory configuration (no DTB or FDT parse failed)");
}

/// Return the active memory regions as a slice.
/// Called by `for_each_usable_frame_region()` in the frame allocator.
pub fn memory_regions() -> &'static [(usize, usize)] {
    // SAFETY: MEMORY_BUF is populated before mm::init() and never modified after.
    let buffer = unsafe { &*core::ptr::addr_of!(MEMORY_BUF) };
    if buffer.is_empty() {
        return MEMORY_REGIONS_FALLBACK;
    }
    &buffer.regions[..buffer.region_count]
}

/// Return the active firmware-reserved regions as a slice.
pub fn firmware_reserved_regions() -> &'static [(usize, usize)] {
    // SAFETY: MEMORY_BUF is populated before mm::init() and never modified after.
    let buffer = unsafe { &*core::ptr::addr_of!(MEMORY_BUF) };
    if buffer.is_empty() {
        return FIRMWARE_RESERVED_REGIONS_FALLBACK;
    }
    &buffer.reserved[..buffer.reserved_count]
}

/// Fill MEMORY_BUF from compile-time constants.
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
