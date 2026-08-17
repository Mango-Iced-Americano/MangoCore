use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::drivers::net::gmac_jh7110::discover_gmac0_resources;
use crate::hal::device::DeviceManager;
use crate::hal::platform::info::{
    DeviceInfo, DeviceKind, DeviceStatus, MmioRange, RawProperty, RawPropertyValidity,
    ResourceValidity,
};
use crate::kernel_tests::runner::KernelTest;

pub(super) fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new(
            "gmac_probe::qemu_live_fdt_has_no_jh7110_gmac",
            qemu_live_fdt_has_no_jh7110_gmac,
        ),
        KernelTest::new(
            "gmac_probe::discovers_supported_gmac0_compatibles",
            discovers_supported_gmac0_compatibles,
        ),
        KernelTest::new(
            "gmac_probe::rejects_incomplete_or_unsupported_nodes",
            rejects_incomplete_or_unsupported_nodes,
        ),
    ]
}

fn qemu_live_fdt_has_no_jh7110_gmac() -> Result<(), &'static str> {
    // Given: the live QEMU virt FDT catalogue.
    let platform = crate::hal::platform::platform_info();
    let manager = DeviceManager::new(platform.devices.clone());

    // When: the GMAC binding parser performs discovery without instantiating hardware.
    let resources = discover_gmac0_resources(&manager);

    // Then: QEMU exposes no JH7110 GMAC resources; a real-board invocation skips this
    // QEMU-specific assertion rather than touching board MMIO from the test.
    match resources {
        None => Ok(()),
        Some(_) => Err("SKIP: live FDT contains JH7110 GMAC resources"),
    }
}

fn discovers_supported_gmac0_compatibles() -> Result<(), &'static str> {
    // Given: every binding-compatible spelling accepted by the JH7110 GMAC L1 driver.
    let compatible_sets = [
        &["starfive,jh7110-eqos-5.20"][..],
        &["starfive,jh7110-dwmac"][..],
        &["starfive,dwmac", "snps,dwmac-5.10a"][..],
    ];

    for compatibles in compatible_sets {
        let manager = DeviceManager::new(vec![valid_gmac0(compatibles)]);

        // When: the pure resource discovery parses the FDT node.
        let resources = discover_gmac0_resources(&manager)
            .ok_or("supported GMAC0 compatible was not discovered")?;

        // Then: only the FDT-supplied GMAC0 base and first interrupt cell are returned.
        if resources.base != 0x1603_0000 || resources.irq != 7 {
            return Err("GMAC0 discovery returned incorrect FDT resources");
        }
    }
    Ok(())
}

fn rejects_incomplete_or_unsupported_nodes() -> Result<(), &'static str> {
    // Given: malformed, disabled, undersized, and non-GMAC0 node shapes.
    let mut disabled = valid_gmac0(&["starfive,jh7110-eqos-5.20"]);
    disabled.status = DeviceStatus::Disabled(String::from("disabled"));

    let mut missing_reg = valid_gmac0(&["starfive,jh7110-eqos-5.20"]);
    missing_reg.mmio_ranges.clear();

    let mut short_range = valid_gmac0(&["starfive,jh7110-eqos-5.20"]);
    short_range.mmio_ranges[0] = MmioRange::new(0x1603_0000, 0x1160);

    let mut missing_irq = valid_gmac0(&["starfive,jh7110-eqos-5.20"]);
    missing_irq.raw_properties.clear();

    let mut zero_irq = valid_gmac0(&["starfive,jh7110-eqos-5.20"]);
    zero_irq.raw_properties[0] = RawProperty::new("interrupts", vec![0, 0, 0, 0]);

    let mut gmac1 = valid_gmac0(&["starfive,jh7110-eqos-5.20"]);
    gmac1.mmio_ranges[0] = MmioRange::new(0x1604_0000, 0x2000);

    let bare_dwmac = valid_gmac0(&["snps,dwmac-5.20"]);

    let mut malformed_raw = valid_gmac0(&["starfive,jh7110-eqos-5.20"]);
    malformed_raw.raw_property_validity = RawPropertyValidity::Malformed;

    let mut malformed_resource = valid_gmac0(&["starfive,jh7110-eqos-5.20"]);
    malformed_resource.resource_validity = ResourceValidity::Malformed;

    let rejected = [
        disabled,
        missing_reg,
        short_range,
        missing_irq,
        zero_irq,
        gmac1,
        bare_dwmac,
        malformed_raw,
        malformed_resource,
    ];

    for node in rejected {
        let manager = DeviceManager::new(vec![node]);

        // When: FDT discovery evaluates a node that violates one binding contract.
        let resources = discover_gmac0_resources(&manager);

        // Then: it fails closed before any MMIO operation can be reached.
        if resources.is_some() {
            return Err("invalid GMAC FDT node produced usable resources");
        }
    }
    Ok(())
}

fn valid_gmac0(compatibles: &[&str]) -> DeviceInfo {
    DeviceInfo {
        node_path: String::from("/soc/ethernet@16030000"),
        parent_path: Some(String::from("/soc")),
        status: DeviceStatus::Enabled(Some(String::from("okay"))),
        compatible: compatibles.iter().map(|entry| String::from(*entry)).collect(),
        raw_properties: vec![RawProperty::new("interrupts", vec![0, 0, 0, 7])],
        raw_property_validity: RawPropertyValidity::Valid,
        mmio_ranges: vec![MmioRange::new(0x1603_0000, 0x2000)],
        resource_validity: ResourceValidity::Valid,
        kind: DeviceKind::Other,
    }
}
