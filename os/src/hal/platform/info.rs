//! Owned, post-heap platform information model.
//!
//! Built once after `mm::init()` from firmware data or static fallback.
//! All kernel subsystems read from the resulting `PlatformInfo` singleton.

use alloc::string::String;
use alloc::vec::Vec;

use crate::hal::boot::RawBootInfo;

/// How the kernel discovered platform information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareKind {
    /// Compile-time static fallback (no DTB/ACPI).
    Static,
    /// Flattened Device Tree from firmware.
    Fdt,
    /// ACPI tables (future).
    Acpi,
}

/// A discovered or statically-defined hardware device.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// Compatible strings from the device tree (for example, `virtio,mmio`).
    /// Static devices use one descriptive string.
    pub compatible: Vec<String>,
    /// Optional MMIO region: physical base address and size in bytes.
    pub mmio: Option<(usize, usize)>,
    /// Device class for fast filtering.
    pub kind: DeviceKind,
}

/// Coarse device classification for fast lookups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    /// Serial / UART console.
    Serial,
    /// VirtIO block device.
    VirtioBlock,
    /// VirtIO network device.
    VirtioNet,
    /// Platform-level interrupt controller (PLIC, etc.).
    InterruptController,
    /// PCI host bridge (ECAM + MMIO window).
    PciHost,
    /// Catch-all for unrecognized devices.
    Other,
}

/// Owned, immutable platform snapshot built once after `mm::init()`.
#[derive(Debug, Clone)]
pub struct PlatformInfo {
    /// How this information was obtained.
    pub firmware: FirmwareKind,
    /// Raw boot handoff from the entry assembly.
    pub boot: RawBootInfo,
    /// Device model string from firmware (for example, `riscv-virtio,qemu`).
    pub model: Option<String>,
    /// Kernel command line, resolved during construction.
    pub cmdline: String,
    /// Discovered or statically-defined devices.
    pub devices: Vec<DeviceInfo>,
}

impl PlatformInfo {
    /// Builds a platform snapshot from the compile-time static fallback.
    ///
    /// Copies the entry handoff, resolves the compile-time `MANGO_CMDLINE`, and
    /// loads the active board's static device catalogue.
    pub fn from_static() -> Self {
        let boot = *crate::hal::boot::boot_info();
        let cmdline = crate::bootargs::get_cmdline().into();
        let devices = crate::hal::platform::fallback::static_devices_for_board();
        Self {
            firmware: FirmwareKind::Static,
            boot,
            model: None,
            cmdline,
            devices,
        }
    }

    /// Returns whether the kernel has no FDT or ACPI firmware description.
    pub const fn is_static_fallback(&self) -> bool {
        matches!(self.firmware, FirmwareKind::Static)
    }
}
