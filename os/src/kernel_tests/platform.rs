//! L3 tests for the platform information model.

use crate::hal::device::{DeviceManager, DeviceQueryError};
use crate::hal::platform::info::{DeviceInfo, DeviceKind, FirmwareKind, MmioRange, PlatformInfo};
use crate::kernel_tests::runner::KernelTest;
use alloc::vec;
use alloc::vec::Vec;

/// Returns all platform information tests.
pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new(
            "platform::device_kind_matching",
            test_device_info_kind_matching,
        ),
        KernelTest::new("platform::device_no_mmio", test_device_info_no_mmio),
        KernelTest::new(
            "platform::device_manager_compatible",
            test_device_manager_find_by_compatible,
        ),
        KernelTest::new(
            "platform::device_manager_kind",
            test_device_manager_find_by_kind,
        ),
        KernelTest::new(
            "platform::device_manager_mmio",
            test_device_manager_find_mmio,
        ),
        KernelTest::new("platform::runtime_root_policy", test_runtime_root_policy),
        KernelTest::new(
            "platform::classifies_visionfive_models",
            test_classifies_visionfive_models,
        ),
    ]
}

/// Device kinds discriminate isolated VirtIO and serial test fixtures.
fn test_device_info_kind_matching() -> Result<(), &'static str> {
    let block = DeviceInfo::static_device(
        &["virtio,mmio"],
        vec![MmioRange::new(0x1000_1000, 0x1000)],
        DeviceKind::VirtioBlock,
    );
    let serial = DeviceInfo::static_device(
        &["ns16550a"],
        vec![MmioRange::new(0x1000_0000, 0x1000)],
        DeviceKind::Serial,
    );

    if block.kind != DeviceKind::VirtioBlock {
        return Err("virtio block device has wrong kind");
    }
    if serial.kind != DeviceKind::Serial {
        return Err("serial device has wrong kind");
    }
    if !block.compatible.iter().any(|compatible| compatible == "virtio,mmio") {
        return Err("virtio block device is missing its compatible string");
    }

    let future_firmware = [FirmwareKind::Fdt, FirmwareKind::Acpi];
    if future_firmware.contains(&FirmwareKind::Static) {
        return Err("future firmware kinds must not be static fallback");
    }
    let additional_kinds = [
        DeviceKind::VirtioNet,
        DeviceKind::InterruptController,
        DeviceKind::PciHost,
    ];
    if additional_kinds.contains(&DeviceKind::Other) {
        return Err("additional device kinds must not be Other");
    }
    Ok(())
}

/// A non-MMIO device such as a CPU node has no register range.
fn test_device_info_no_mmio() -> Result<(), &'static str> {
    let cpu = DeviceInfo::static_device(&["riscv"], vec![], DeviceKind::Other);

    if cpu.mmio_range(0).is_some() {
        return Err("non-MMIO device has a register range");
    }
    Ok(())
}

/// Device manager compatible lookup is exact and returns no result when absent.
fn test_device_manager_find_by_compatible() -> Result<(), &'static str> {
    let devices = vec![
        DeviceInfo::static_device(
            &["virtio,mmio"],
            vec![MmioRange::new(0x1000_1000, 0x1000)],
            DeviceKind::VirtioBlock,
        ),
        DeviceInfo::static_device(
            &["ns16550a"],
            vec![MmioRange::new(0x1000_0000, 0x1000)],
            DeviceKind::Serial,
        ),
    ];
    let device_manager = DeviceManager::new(devices);

    let virtio = device_manager.find_by_compatible("virtio,mmio");
    if virtio.len() != 1 || virtio[0].mmio_range(0) != Some(MmioRange::new(0x1000_1000, 0x1000)) {
        return Err("compatible lookup did not return the VirtIO device");
    }
    if device_manager.find_by_compatible("ns16550a").len() != 1 {
        return Err("compatible lookup did not return the serial device");
    }
    if !device_manager
        .find_by_compatible("virtio,mmio-device")
        .is_empty()
    {
        return Err("compatible lookup must be exact");
    }
    if !device_manager.find_by_compatible("nonexistent").is_empty() {
        return Err("unknown compatible must return no devices");
    }
    Ok(())
}

/// Device manager kind lookup finds block devices and the serial console.
fn test_device_manager_find_by_kind() -> Result<(), &'static str> {
    let devices = vec![
        DeviceInfo::static_device(
            &["virtio,mmio"],
            vec![MmioRange::new(0x1000_1000, 0x1000)],
            DeviceKind::VirtioBlock,
        ),
        DeviceInfo::static_device(
            &["virtio,mmio"],
            vec![MmioRange::new(0x1000_2000, 0x1000)],
            DeviceKind::VirtioBlock,
        ),
        DeviceInfo::static_device(
            &["ns16550a"],
            vec![MmioRange::new(0x1000_0000, 0x1000)],
            DeviceKind::Serial,
        ),
    ];
    let device_manager = DeviceManager::new(devices);

    if device_manager.find_block_devices().len() != 2 {
        return Err("block device lookup did not return both devices");
    }
    if device_manager.find_by_kind(DeviceKind::Serial).len() != 1 {
        return Err("kind lookup did not return the serial device");
    }
    match device_manager.find_console() {
        Some(console) if console.mmio_range(0) == Some(MmioRange::new(0x1000_0000, 0x1000)) => Ok(()),
        _ => Err("console lookup did not return the serial device"),
    }
}

/// Device manager returns MMIO only when the matched device provides it.
fn test_device_manager_find_mmio() -> Result<(), &'static str> {
    let devices = vec![
        DeviceInfo::static_device(
            &["ns16550a"],
            vec![MmioRange::new(0x1000_0000, 0x1000)],
            DeviceKind::Serial,
        ),
        DeviceInfo::static_device(&["riscv"], vec![], DeviceKind::Other),
    ];
    let device_manager = DeviceManager::new(devices);

    if device_manager.find_mmio("ns16550a") != Some(MmioRange::new(0x1000_0000, 0x1000)) {
        return Err("MMIO lookup returned an incorrect serial range");
    }
    if device_manager.find_mmio("riscv").is_some() {
        return Err("non-MMIO device returned an MMIO range");
    }
    if device_manager.find_mmio("nonexistent").is_some() {
        return Err("unknown device returned an MMIO range");
    }
    Ok(())
}

fn test_runtime_root_policy() -> Result<(), &'static str> {
    if crate::hal::platform::default_init_path() != "/initproc" {
        return Err("runtime platform has wrong default init path");
    }
    let devices = vec![DeviceInfo::static_device(
        &["virtio,mmio"],
        vec![MmioRange::new(0x1000_1000, 0x1000)],
        DeviceKind::Other,
    )];
    let platform = PlatformInfo {
        firmware: FirmwareKind::Fdt,
        boot: *crate::hal::boot::boot_info(),
        model: None,
        cmdline: alloc::string::String::new(),
        devices,
        console: None,
        pci_host: None,
    };
    if platform.default_root() != "/dev/vda" {
        return Err("enabled virtio FDT transport did not select /dev/vda");
    }
    let initramfs_platform = PlatformInfo {
        firmware: FirmwareKind::Fdt,
        boot: *crate::hal::boot::boot_info(),
        model: None,
        cmdline: alloc::string::String::new(),
        devices: Vec::new(),
        console: None,
        pci_host: None,
    };
    if initramfs_platform.default_root() != "initramfs" {
        return Err("unrecognized FDT devices must retain the initramfs root");
    }
    Ok(())
}

fn test_classifies_visionfive_models() -> Result<(), &'static str> {
    // Given: the VF2 U-Boot model spelling and QEMU's virt machine model.
    // When: the platform classifier evaluates their model strings.
    // Then: only the physical VisionFive model requests a firmware reboot.
    if !crate::hal::platform::is_visionfive_model("StarFive VisionFive V2")
        || !crate::hal::platform::is_visionfive_model("starfive,visionfive-v2")
    {
        return Err("VisionFive model was not recognized as a real board");
    }
    if crate::hal::platform::is_visionfive_model("riscv-virtio,qemu") {
        return Err("QEMU virt model was incorrectly recognized as a real board");
    }
    Ok(())
}
