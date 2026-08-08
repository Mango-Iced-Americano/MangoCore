use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::hal::device::{DeviceManager, DeviceQueryError};
use crate::hal::platform::info::{
    DeviceInfo, DeviceKind, DeviceStatus, MmioRange, RawPropertyValidity, ResourceValidity,
};
use crate::kernel_tests::runner::KernelTest;

pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new(
            "platform_resources::preserves_identity_and_all_mmio_ranges",
            test_preserves_identity_and_all_mmio_ranges,
        ),
        KernelTest::new(
            "platform_resources::rejects_disabled_ambiguous_and_malformed_devices",
            test_rejects_disabled_ambiguous_and_malformed_devices,
        ),
    ]
}

fn test_preserves_identity_and_all_mmio_ranges() -> Result<(), &'static str> {
    // Given: one enabled device with two MMIO ranges.
    let manager = DeviceManager::new(vec![device(
        "/soc/storage@10000000",
        DeviceStatus::Enabled(None),
        ResourceValidity::Valid,
        vec![
            MmioRange::new(0x1000_0000, 0x1000),
            MmioRange::new(0x1000_2000, 0x2000),
        ],
    )]);

    // When: the compatible and indexed-MMIO queries run.
    let matched = manager
        .unique_enabled_compatible("example,storage")
        .map_err(|_| "strict compatible query failed")?;
    // Then: identity, compatible list, and the selected MMIO range are preserved.
    if matched.node_path != "/soc/storage@10000000" {
        return Err("strict compatible query lost the FDT node path");
    }
    if matched.compatible.len() != 2 {
        return Err("strict compatible query lost a compatible string");
    }
    if manager
        .indexed_mmio("example,storage", 1)
        .map_err(|_| "indexed MMIO query failed")?
        != MmioRange::new(0x1000_2000, 0x2000)
    {
        return Err("indexed MMIO query did not return the second range");
    }
    Ok(())
}

fn test_rejects_disabled_ambiguous_and_malformed_devices() -> Result<(), &'static str> {
    // Given: disabled, ambiguous, and malformed resource snapshots.
    let disabled = DeviceManager::new(vec![device(
        "/soc/disabled@0",
        DeviceStatus::Disabled(String::from("disabled")),
        ResourceValidity::Valid,
        vec![MmioRange::new(0x1000_0000, 0x1000)],
    )]);
    // When: each strict query resolves its matching resource.
    // Then: no unsafe resource selection succeeds.
    if !matches!(
        disabled.unique_enabled_compatible("example,storage"),
        Err(DeviceQueryError::Disabled)
    ) {
        return Err("disabled device was selected by a strict compatible query");
    }

    let ambiguous = DeviceManager::new(vec![
        device(
            "/soc/storage@0",
            DeviceStatus::Enabled(None),
            ResourceValidity::Valid,
            vec![MmioRange::new(0x1000_0000, 0x1000)],
        ),
        device(
            "/soc/storage@1",
            DeviceStatus::Enabled(None),
            ResourceValidity::Valid,
            vec![MmioRange::new(0x1000_2000, 0x1000)],
        ),
    ]);
    if !matches!(
        ambiguous.unique_enabled_compatible("example,storage"),
        Err(DeviceQueryError::Ambiguous)
    ) {
        return Err("ambiguous devices were selected by a strict compatible query");
    }

    let malformed = DeviceManager::new(vec![device(
        "/soc/storage@2",
        DeviceStatus::Enabled(None),
        ResourceValidity::Malformed,
        Vec::new(),
    )]);
    if !matches!(
        malformed.indexed_mmio("example,storage", 0),
        Err(DeviceQueryError::MalformedResources)
    ) {
        return Err("malformed MMIO resources were selected by an indexed query");
    }
    Ok(())
}

fn device(
    node_path: &str,
    status: DeviceStatus,
    resource_validity: ResourceValidity,
    mmio_ranges: Vec<MmioRange>,
) -> DeviceInfo {
    DeviceInfo {
        node_path: String::from(node_path),
        parent_path: None,
        status,
        compatible: vec![
            String::from("example,storage"),
            String::from("example,storage-v2"),
        ],
        raw_properties: Vec::new(),
        raw_property_validity: RawPropertyValidity::Valid,
        mmio_ranges,
        resource_validity,
        kind: DeviceKind::Other,
    }
}
