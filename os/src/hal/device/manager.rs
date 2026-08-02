//! Device manager implementation.
//!
//! Wraps `PlatformInfo.devices` and provides typed query methods
//! for driver initialization.

use crate::hal::platform::info::{DeviceInfo, DeviceKind};
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

    /// Return all devices of a specific DeviceKind.
    pub fn find_by_kind(&self, kind: DeviceKind) -> Vec<&DeviceInfo> {
        self.devices
            .iter()
            .filter(|device| device.kind == kind)
            .collect()
    }

    /// Return the MMIO (base, size) of the first device matching `compatible`.
    /// Returns None if no match or if the device has no MMIO region.
    pub fn find_mmio(&self, compatible: &str) -> Option<(usize, usize)> {
        self.devices
            .iter()
            .find(|device| {
                device
                    .compatible
                    .iter()
                    .any(|candidate| candidate == compatible)
            })
            .and_then(|device| device.mmio)
    }

    /// Return block devices (VirtioBlock kind).
    pub fn find_block_devices(&self) -> Vec<&DeviceInfo> {
        self.find_by_kind(DeviceKind::VirtioBlock)
    }

    /// Return the first serial console device.
    pub fn find_console(&self) -> Option<&DeviceInfo> {
        self.devices
            .iter()
            .find(|device| device.kind == DeviceKind::Serial)
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
