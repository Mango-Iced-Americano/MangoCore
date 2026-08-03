//! 平台设备只读查询器。
//!
//! Batch 2 先稳定查询接口，Driver 批次再接入消费者；在此之前这些入口会被
//! dead-code 检查判定为未使用。豁免仅限本文件，消费者迁移后应整体删除。

#![allow(
    dead_code,
    reason = "DeviceManager 在后续 Driver 融合批次接入，本批只冻结查询合同"
)]

use crate::hal::platform::{
    DeviceInfo, DeviceKind, MmioRange, RawPropertyValidity, ResourceValidity,
};
use alloc::vec::Vec;

/// 拥有设备描述副本的只读查询视图。
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
        device
            .mmio_range(index)
            .ok_or(DeviceQueryError::MissingMmioRange)
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
        self.devices.iter().find(|device| {
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
