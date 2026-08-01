//! Owned, post-heap platform information model.
//!
//! Built once after `mm::init()` from firmware data or static fallback.
//! All kernel subsystems read from the resulting `PlatformInfo` singleton.

use alloc::string::String;
use alloc::vec::Vec;
use core::convert::TryInto;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmioRange {
    pub base: usize,
    pub size: usize,
}

/// A serial console selected by FDT `/chosen/stdout-path`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsoleInfo {
    pub range: MmioRange,
    pub register_shift: usize,
}

impl MmioRange {
    pub const fn new(base: usize, size: usize) -> Self {
        Self { base, size }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceStatus {
    Enabled(Option<String>),
    Disabled(String),
    Malformed,
}

impl DeviceStatus {
    pub fn from_fdt(status: Option<&str>) -> Self {
        match status {
            None => Self::Enabled(None),
            Some("ok") | Some("okay") => Self::Enabled(status.map(String::from)),
            Some(status) if status.is_empty() => Self::Malformed,
            Some(status) => Self::Disabled(String::from(status)),
        }
    }

    pub const fn is_enabled(&self) -> bool {
        matches!(self, Self::Enabled(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceValidity {
    Valid,
    Malformed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawProperty {
    pub name: String,
    pub value: Vec<u8>,
}

impl RawProperty {
    pub fn new(name: &str, value: Vec<u8>) -> Self {
        Self {
            name: String::from(name),
            value,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawPropertyValidity {
    Valid,
    Malformed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawPropertyError {
    Absent,
    Malformed,
}

/// A hardware device discovered from firmware.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub node_path: String,
    pub parent_path: Option<String>,
    pub status: DeviceStatus,
    /// Compatible strings from the device tree (for example, `virtio,mmio`).
    pub compatible: Vec<String>,
    pub raw_properties: Vec<RawProperty>,
    pub raw_property_validity: RawPropertyValidity,
    pub mmio_ranges: Vec<MmioRange>,
    pub resource_validity: ResourceValidity,
    /// Device class for fast filtering.
    pub kind: DeviceKind,
}

impl DeviceInfo {
    /// Construct generic device data for isolated tests.
    pub fn static_device(
        compatible: &[&str],
        mmio_ranges: Vec<MmioRange>,
        kind: DeviceKind,
    ) -> Self {
        Self {
            node_path: String::new(),
            parent_path: None,
            status: DeviceStatus::Enabled(None),
            compatible: compatible.iter().map(|entry| String::from(*entry)).collect(),
            raw_properties: Vec::new(),
            raw_property_validity: RawPropertyValidity::Valid,
            mmio_ranges,
            resource_validity: ResourceValidity::Valid,
            kind,
        }
    }

    pub const fn is_enabled(&self) -> bool {
        self.status.is_enabled()
    }

    pub fn mmio_range(&self, index: usize) -> Option<MmioRange> {
        self.mmio_ranges.get(index).copied()
    }

    pub fn raw_property(&self, name: &str) -> Result<&[u8], RawPropertyError> {
        match self.raw_property_validity {
            RawPropertyValidity::Valid => self
                .raw_properties
                .iter()
                .find(|property| property.name == name)
                .map(|property| property.value.as_slice())
                .ok_or(RawPropertyError::Absent),
            RawPropertyValidity::Malformed => Err(RawPropertyError::Malformed),
        }
    }

    pub fn raw_property_exact<const N: usize>(
        &self,
        name: &str,
    ) -> Result<&[u8; N], RawPropertyError> {
        self.raw_property(name)?
            .try_into()
            .map_err(|_| RawPropertyError::Malformed)
    }
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

/// PCI host resources resolved from an enabled firmware node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciHost {
    pub ecam_base: usize,
    pub ecam_size: usize,
    pub mmio_base: usize,
    pub mmio_size: usize,
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
    /// Devices discovered from firmware.
    pub devices: Vec<DeviceInfo>,
    /// Validated runtime console, if firmware selected a supported serial node.
    pub console: Option<ConsoleInfo>,
    /// Validated PCI host resources, when firmware exposes an ECAM host bridge.
    pub pci_host: Option<PciHost>,
}

impl PlatformInfo {
    /// Builds a platform snapshot from the compile-time boot contract.
    ///
    /// Copies the entry handoff and resolves the compile-time `MANGO_CMDLINE`.
    /// A static boot contract never invents hardware devices.
    pub fn from_static() -> Self {
        let boot = *crate::hal::boot::boot_info();
        let cmdline = crate::bootargs::get_cmdline().into();
        Self {
            firmware: FirmwareKind::Static,
            boot,
            model: None,
            cmdline,
            devices: Vec::new(),
            console: None,
            pci_host: None,
        }
    }

    /// Returns whether the kernel has no FDT or ACPI firmware description.
    pub const fn is_static_fallback(&self) -> bool {
        matches!(self.firmware, FirmwareKind::Static)
    }

    /// Return the first valid PCI host discovered from firmware.
    pub const fn pci_host(&self) -> Option<PciHost> {
        self.pci_host
    }

    /// Select an external root only when FDT exposes an enabled virtio-mmio transport.
    pub fn default_root(&self) -> &'static str {
        if self.firmware == FirmwareKind::Fdt
            && self.devices.iter().any(|device| {
                device.is_enabled()
                    && device.resource_validity == ResourceValidity::Valid
                    && device.compatible.iter().any(|compatible| compatible == "virtio,mmio")
            })
        {
            "/dev/vda"
        } else {
            "initramfs"
        }
    }
}
