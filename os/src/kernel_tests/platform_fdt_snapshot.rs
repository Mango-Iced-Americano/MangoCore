use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::platform_fdt_fixture::vf2_mmc_snapshot;
use crate::hal::device::DeviceManager;
use crate::hal::platform::info::{
    DeviceInfo, DeviceKind, DeviceStatus, MmioRange, RawProperty, RawPropertyError,
    RawPropertyValidity, ResourceValidity,
};
use crate::kernel_tests::runner::KernelTest;

#[cfg(target_arch = "riscv64")]
use crate::hal::platform::info::FirmwareKind;

pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new(
            "platform_fdt_snapshot::preserves_live_vf2_mmc_node_shapes",
            test_preserves_live_vf2_mmc_node_shapes,
        ),
        KernelTest::new(
            "platform_fdt_snapshot::rejects_absent_and_malformed_raw_resources",
            test_rejects_absent_and_malformed_raw_resources,
        ),
        KernelTest::new(
            "platform_fdt_snapshot::exact_compatible_rejects_malformed_raw_snapshot",
            test_exact_compatible_rejects_malformed_raw_snapshot,
        ),
        #[cfg(target_arch = "riscv64")]
        KernelTest::new(
            "platform_fdt_snapshot::captures_qemu_boot_fdt_raw_properties",
            test_captures_qemu_boot_fdt_raw_properties,
        ),
        #[cfg(target_arch = "riscv64")]
        KernelTest::new(
            "platform_fdt_snapshot::retains_qemu_soc_device_layer",
            test_retains_qemu_soc_device_layer,
        ),
        #[cfg(target_arch = "riscv64")]
        KernelTest::new(
            "platform_fdt_snapshot::discovers_qemu_pci_host",
            test_discovers_qemu_pci_host,
        ),
    ]
}

fn test_preserves_live_vf2_mmc_node_shapes() -> Result<(), &'static str> {
    // Given: the parent and the two enabled MMC nodes observed in the live VF2 FDT.
    let manager = DeviceManager::new(vf2_mmc_snapshot());

    // When: a driver enumerates its exact compatible and reads binding-owned bytes.
    let nodes = manager.find_enabled_by_exact_compatible("snps,dw-mshc");

    // Then: both raw node shapes, including their parent relationship, are retained.
    if manager.device_count() != 4 {
        return Err("FDT snapshot dropped nodes without compatible properties");
    }
    let soc = match manager
        .all_devices()
        .iter()
        .find(|node| node.node_path == "/soc")
    {
        Some(node) => node,
        None => return Err("FDT snapshot dropped the MMC parent node"),
    };
    if soc.parent_path.as_deref() != Some("/") {
        return Err("FDT snapshot lost the stable parent relationship");
    }
    if nodes.len() != 2 {
        return Err("exact compatible lookup did not enumerate both MMC nodes");
    }

    let sdio0 = match nodes.iter().find(|node| node.node_path == "/soc/sdio0@16010000") {
        Some(node) => *node,
        None => return Err("FDT snapshot dropped sdio0"),
    };
    let sdio1 = match nodes.iter().find(|node| node.node_path == "/soc/sdio1@16020000") {
        Some(node) => *node,
        None => return Err("FDT snapshot dropped sdio1"),
    };
    if sdio0.mmio_range(0) != Some(MmioRange::new(0x1601_0000, 0x1_0000))
        || sdio1.mmio_range(0) != Some(MmioRange::new(0x1602_0000, 0x1_0000))
    {
        return Err("FDT snapshot lost an MMC register range");
    }
    assert_raw_properties(
        sdio0,
        &[
            ("compatible", b"snps,dw-mshc\0"),
            ("reg", &[0, 0, 0, 0, 0x16, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0]),
            (
                "clocks",
                &[0, 0, 0, 15, 0, 0, 0, 91, 0, 0, 0, 15, 0, 0, 0, 93],
            ),
            ("clock-names", b"biu\0ciu\0"),
            ("resets", &[0, 0, 0, 16, 0, 0, 0, 64]),
            ("reset-names", b"reset\0"),
            ("assigned-clocks", &[0, 0, 0, 15, 0, 0, 0, 93]),
            ("assigned-clock-rates", &[0x02, 0xfa, 0xf0, 0x80]),
            ("fifo-depth", &[0, 0, 0, 32]),
            ("bus-width", &[0, 0, 0, 8]),
            ("pinctrl-names", b"default\0"),
            ("pinctrl-0", &[0, 0, 0, 22]),
            ("status", b"okay\0"),
            ("u-boot,dm-spl", b""),
            ("phandle", &[0, 0, 0, 95]),
        ],
    )?;
    assert_raw_properties(
        sdio1,
        &[
            ("compatible", b"snps,dw-mshc\0"),
            ("reg", &[0, 0, 0, 0, 0x16, 2, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0]),
            (
                "clocks",
                &[0, 0, 0, 15, 0, 0, 0, 92, 0, 0, 0, 15, 0, 0, 0, 94],
            ),
            ("clock-names", b"biu\0ciu\0"),
            ("resets", &[0, 0, 0, 16, 0, 0, 0, 65]),
            ("reset-names", b"reset\0"),
            ("assigned-clocks", &[0, 0, 0, 15, 0, 0, 0, 94]),
            ("assigned-clock-rates", &[0x02, 0xfa, 0xf0, 0x80]),
            ("fifo-depth", &[0, 0, 0, 32]),
            ("bus-width", &[0, 0, 0, 4]),
            ("pinctrl-names", b"default\0"),
            ("pinctrl-0", &[0, 0, 0, 23]),
            ("status", b"okay\0"),
            ("u-boot,dm-spl", b""),
            ("phandle", &[0, 0, 0, 96]),
        ],
    )?;
    let sdio0_clocks = raw_property_exact::<16>(sdio0, "clocks")?;
    let sdio1_clocks = raw_property_exact::<16>(sdio1, "clocks")?;
    if sdio0_clocks[..8] != [0, 0, 0, 15, 0, 0, 0, 91]
        || sdio0_clocks[8..] != [0, 0, 0, 15, 0, 0, 0, 93]
        || sdio1_clocks[..8] != [0, 0, 0, 15, 0, 0, 0, 92]
        || sdio1_clocks[8..] != [0, 0, 0, 15, 0, 0, 0, 94]
    {
        return Err("FDT snapshot lost a second MMC clock specifier");
    }
    for property in ["interrupts", "starfive,sysreg", "data-addr"] {
        if !matches!(sdio0.raw_property(property), Err(RawPropertyError::Absent))
            || !matches!(sdio1.raw_property(property), Err(RawPropertyError::Absent))
        {
            return Err("missing MMC property was inferred from unrelated FDT data");
        }
    }
    Ok(())
}

fn test_rejects_absent_and_malformed_raw_resources() -> Result<(), &'static str> {
    // Given: an enabled node with a truncated clock reference and no reset reference.
    let node = DeviceInfo {
        node_path: String::from("/soc/storage@0"),
        parent_path: Some(String::from("/soc")),
        status: DeviceStatus::Enabled(Some(String::from("okay"))),
        compatible: vec![String::from("example,storage")],
        raw_properties: vec![RawProperty::new("clocks", vec![0, 0, 0, 15, 0, 0, 0])],
        raw_property_validity: RawPropertyValidity::Valid,
        mmio_ranges: vec![MmioRange::new(0x1000_0000, 0x1000)],
        resource_validity: ResourceValidity::Valid,
        kind: DeviceKind::Other,
    };

    // When: a driver requires the exact raw byte shape of each resource.
    // Then: malformed and absent properties remain distinct and unusable.
    if !matches!(
        node.raw_property_exact::<8>("clocks"),
        Err(RawPropertyError::Malformed)
    ) {
        return Err("truncated raw property became a usable resource");
    }
    if !matches!(
        node.raw_property_exact::<8>("resets"),
        Err(RawPropertyError::Absent)
    ) {
        return Err("absent raw property became an inferred resource");
    }
    Ok(())
}

fn test_exact_compatible_rejects_malformed_raw_snapshot() -> Result<(), &'static str> {
    // Given: one malformed raw snapshot and one node with only malformed MMIO resources.
    let malformed_raw = DeviceInfo {
        node_path: String::from("/soc/malformed-raw@0"),
        parent_path: Some(String::from("/soc")),
        status: DeviceStatus::Enabled(Some(String::from("okay"))),
        compatible: vec![String::from("example,storage")],
        raw_properties: Vec::new(),
        raw_property_validity: RawPropertyValidity::Malformed,
        mmio_ranges: Vec::new(),
        resource_validity: ResourceValidity::Valid,
        kind: DeviceKind::Other,
    };
    let mut malformed_mmio = malformed_raw.clone();
    malformed_mmio.node_path = String::from("/soc/malformed-mmio@0");
    malformed_mmio.raw_property_validity = RawPropertyValidity::Valid;
    malformed_mmio.resource_validity = ResourceValidity::Malformed;
    let manager = DeviceManager::new(vec![malformed_raw, malformed_mmio]);

    // When: an exact-compatible query enumerates nodes for a binding owner.
    let exact = manager.find_enabled_by_exact_compatible("example,storage");

    // Then: malformed raw snapshots are fail-closed, while MMIO validation remains separate.
    if exact.len() != 1 || exact[0].node_path != "/soc/malformed-mmio@0" {
        return Err("exact compatible query accepted malformed raw FDT data");
    }
    Ok(())
}

#[cfg(target_arch = "riscv64")]
fn test_captures_qemu_boot_fdt_raw_properties() -> Result<(), &'static str> {
    if crate::hal::boot::boot_info().protocol != crate::hal::boot::BootProtocol::RiscvFdt {
        return Err("QEMU ktest did not receive an RISC-V FDT boot handoff");
    }
    let platform = crate::hal::platform::platform_info();
    match platform.firmware {
        FirmwareKind::Fdt => {
            let virtio = match platform
                .devices
                .iter()
                .find(|node| node.compatible.iter().any(|value| value == "virtio,mmio"))
            {
                Some(node) => node,
                None => return Err("QEMU boot FDT did not produce a virtio-mmio snapshot node"),
            };
            if !virtio.is_enabled()
                || virtio.parent_path.is_none()
                || raw_property_exact::<12>(virtio, "compatible")? != b"virtio,mmio\0"
            {
                return Err("FDT parser did not preserve an enabled QEMU virtio-mmio raw property");
            }
            Ok(())
        }
        FirmwareKind::Static => Err("RISC-V QEMU ktest selected a static platform fallback"),
        FirmwareKind::Acpi => Err("RISC-V QEMU ktest selected an unsupported ACPI platform"),
    }
}

#[cfg(target_arch = "riscv64")]
fn test_retains_qemu_soc_device_layer() -> Result<(), &'static str> {
    // Given: QEMU virt places the early-mapped peripherals below `/soc`.
    let platform = crate::hal::platform::platform_info();

    // When: the post-heap snapshot exposes the FDT nodes used by early MMIO.
    for compatible in ["riscv,plic0", "ns16550a", "virtio,mmio"] {
        let device = platform
            .devices
            .iter()
            .find(|device| device.compatible.iter().any(|value| value == compatible))
            .ok_or("QEMU boot FDT omitted a required /soc device")?;

        // Then: every required node remains a direct `/soc` child with MMIO.
        if device.parent_path.as_deref() != Some("/soc") || device.mmio_range(0).is_none() {
            return Err("QEMU /soc device lost its direct parent or MMIO range");
        }
    }
    Ok(())
}

#[cfg(target_arch = "riscv64")]
fn test_discovers_qemu_pci_host() -> Result<(), &'static str> {
    // Given: the PCI host described by QEMU's boot FDT.
    let platform = crate::hal::platform::platform_info();

    // When: the platform snapshot resolves PCI host resources.
    let host = platform
        .pci_host()
        .ok_or("QEMU boot FDT did not expose a usable PCI host")?;

    // Then: the ECAM register range and non-empty MMIO window are both available.
    if host.ecam_base == 0 || host.ecam_size == 0 {
        return Err("PCI host discovery lost the ECAM register range");
    }
    if host.mmio_base == 0 || host.mmio_size == 0 {
        return Err("PCI host discovery lost the MMIO window");
    }
    Ok(())
}

fn raw_property_exact<'a, const N: usize>(
    node: &'a DeviceInfo,
    name: &str,
) -> Result<&'a [u8; N], &'static str> {
    node.raw_property_exact(name)
        .map_err(|_| "FDT snapshot lost a required MMC property")
}

fn assert_raw_properties(
    node: &DeviceInfo,
    properties: &[(&str, &[u8])],
) -> Result<(), &'static str> {
    for &(name, expected) in properties {
        let actual = node
            .raw_property(name)
            .map_err(|_| "FDT snapshot lost an observed MMC property")?;
        if actual != expected {
            return Err("FDT snapshot changed an observed MMC property");
        }
    }
    Ok(())
}
