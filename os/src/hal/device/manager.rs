//! Device manager implementation.
//!
//! Wraps `PlatformInfo.devices` and provides typed query methods
//! for driver initialization.

use crate::hal::platform::info::{
    DeviceInfo, DeviceKind, MmioRange, RawPropertyValidity, ResourceValidity,
};
use alloc::vec::Vec;

/// Owned, query-only view of platform devices.
///
/// Created from `PlatformInfo.devices` after platform initialization.
/// All query methods are infallible — they return empty results
/// when no matching device is found.
#[derive(Debug, Clone)]
pub struct DeviceManager {
    devices: Vec<DeviceInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceQueryError {
    NotFound,
    Disabled,
    Ambiguous,
    MalformedResources,
    MissingMmioRange,
}

impl DeviceManager {
    /// Create a DeviceManager from a list of DeviceInfo entries.
    pub fn new(devices: Vec<DeviceInfo>) -> Self {
        Self { devices }
    }

    /// Return all devices matching the given compatible string.
    /// Matching is exact — `"virtio,mmio"` does not match `"virtio,mmio-device"`.
    pub fn find_by_compatible(&self, compatible: &str) -> Vec<&DeviceInfo> {
        self.devices
            .iter()
            .filter(|device| {
                device
                    .compatible
                    .iter()
                    .any(|candidate| candidate == compatible)
            })
            .collect()
    }

    pub fn find_enabled_by_exact_compatible(&self, compatible: &str) -> Vec<&DeviceInfo> {
        self.find_by_compatible(compatible)
            .into_iter()
            .filter(|device| {
                device.is_enabled() && device.raw_property_validity == RawPropertyValidity::Valid
            })
            .collect()
    }

    pub fn find_enabled_by_compatible(&self, compatible: &str) -> Vec<&DeviceInfo> {
        self.find_enabled_by_exact_compatible(compatible)
            .into_iter()
            .filter(|device| device.resource_validity == ResourceValidity::Valid)
            .collect()
    }

    pub fn unique_enabled_compatible(
        &self,
        compatible: &str,
    ) -> Result<&DeviceInfo, DeviceQueryError> {
        let matches = self.find_by_compatible(compatible);
        let mut enabled = matches.iter().filter(|device| device.is_enabled());
        let first = match enabled.next() {
            Some(device) => *device,
            None if matches.is_empty() => return Err(DeviceQueryError::NotFound),
            None => return Err(DeviceQueryError::Disabled),
        };
        if enabled.next().is_some() {
            return Err(DeviceQueryError::Ambiguous);
        }
        if first.resource_validity == ResourceValidity::Malformed {
            return Err(DeviceQueryError::MalformedResources);
        }
        Ok(first)
    }

    pub fn indexed_mmio(
        &self,
        compatible: &str,
        index: usize,
    ) -> Result<MmioRange, DeviceQueryError> {
        let device = self.unique_enabled_compatible(compatible)?;
        if device.resource_validity == ResourceValidity::Malformed {
            return Err(DeviceQueryError::MalformedResources);
        }
        device.mmio_range(index).ok_or(DeviceQueryError::MissingMmioRange)
    }

    /// Return all devices of a specific DeviceKind.
    pub fn find_by_kind(&self, kind: DeviceKind) -> Vec<&DeviceInfo> {
        self.devices
            .iter()
            .filter(|device| device.kind == kind)
            .collect()
    }

    /// Return the MMIO (base, size) of the first device matching `compatible`.
    /// Returns None if no match or if the device has no MMIO region.
    pub fn find_mmio(&self, compatible: &str) -> Option<MmioRange> {
        self.find_enabled_by_compatible(compatible)
            .into_iter()
            .next()
            .and_then(|device| device.mmio_range(0))
    }

    /// Return block devices (VirtioBlock kind).
    pub fn find_block_devices(&self) -> Vec<&DeviceInfo> {
        self.find_by_kind(DeviceKind::VirtioBlock)
            .into_iter()
            .filter(|device| {
                device.is_enabled() && device.resource_validity == ResourceValidity::Valid
            })
            .collect()
    }

    /// Return the first serial console device.
    pub fn find_console(&self) -> Option<&DeviceInfo> {
        self.devices
            .iter()
            .find(|device| {
                device.kind == DeviceKind::Serial
                    && device.is_enabled()
                    && device.resource_validity == ResourceValidity::Valid
            })
    }

    /// Return all devices (for iteration / debugging).
    pub fn all_devices(&self) -> &[DeviceInfo] {
        &self.devices
    }

    /// Return the number of registered devices.
    pub fn device_count(&self) -> usize {
        self.devices.len()
    }
}
