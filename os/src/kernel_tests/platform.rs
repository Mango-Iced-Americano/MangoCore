//! L3 tests for the platform information model.

use crate::hal::device::DeviceManager;
use crate::hal::platform::info::{DeviceInfo, DeviceKind, FirmwareKind, PlatformInfo};
use crate::kernel_tests::runner::KernelTest;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// Returns all platform information tests.
pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new(
            "platform::info_static_fallback",
            test_platform_info_static_fallback,
        ),
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
        KernelTest::new("platform::policy_selection", test_platform_policy_selection),
    ]
}

/// Static construction produces the active board's fallback platform snapshot.
fn test_platform_info_static_fallback() -> Result<(), &'static str> {
    let platform = PlatformInfo::from_static();

    if !platform.is_static_fallback() {
        return Err("static fallback must report Static");
    }
    if platform.firmware != FirmwareKind::Static {
        return Err("static fallback has wrong firmware kind");
    }
    if platform.boot.protocol != crate::hal::boot::boot_info().protocol {
        return Err("static fallback did not copy the boot handoff");
    }
    if platform.cmdline != crate::bootargs::get_cmdline() {
        return Err("static fallback has wrong command line");
    }
    if platform.model.is_some() {
        return Err("static fallback has a model");
    }
    #[cfg(feature = "board_rvqemu")]
    if platform.devices.len() != 12 {
        return Err("RISC-V QEMU static fallback has wrong device count");
    }
    #[cfg(feature = "board_laqemu")]
    if platform.devices.len() != 1 {
        return Err("LoongArch QEMU static fallback has wrong device count");
    }
    #[cfg(feature = "board_vf2")]
    if platform.devices.len() != 1 {
        return Err("VisionFive 2 static fallback has wrong device count");
    }
    #[cfg(feature = "board_rvqemu")]
    if !platform.devices.iter().any(|device| {
        device.mmio == Some((0x1000_8000, 0x1000)) && device.kind == DeviceKind::VirtioNet
    }) {
        return Err("RISC-V QEMU static fallback is missing the virtio-net slot");
    }
    Ok(())
}

/// Device kinds discriminate statically described VirtIO and serial devices.
fn test_device_info_kind_matching() -> Result<(), &'static str> {
    let block = DeviceInfo {
        compatible: vec![String::from("virtio,mmio")],
        mmio: Some((0x1000_1000, 0x1000)),
        kind: DeviceKind::VirtioBlock,
    };
    let serial = DeviceInfo {
        compatible: vec![String::from("ns16550a")],
        mmio: Some((0x1000_0000, 0x1000)),
        kind: DeviceKind::Serial,
    };

    if block.kind != DeviceKind::VirtioBlock {
        return Err("virtio block device has wrong kind");
    }
    if serial.kind != DeviceKind::Serial {
        return Err("serial device has wrong kind");
    }
    if !block
        .compatible
        .iter()
        .any(|compatible| compatible == "virtio,mmio")
    {
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
    let cpu = DeviceInfo {
        compatible: vec![String::from("riscv")],
        mmio: None,
        kind: DeviceKind::Other,
    };

    if cpu.mmio.is_some() {
        return Err("non-MMIO device has a register range");
    }
    Ok(())
}

/// Device manager compatible lookup is exact and returns no result when absent.
fn test_device_manager_find_by_compatible() -> Result<(), &'static str> {
    let devices = vec![
        DeviceInfo {
            compatible: vec![String::from("virtio,mmio")],
            mmio: Some((0x1000_1000, 0x1000)),
            kind: DeviceKind::VirtioBlock,
        },
        DeviceInfo {
            compatible: vec![String::from("ns16550a")],
            mmio: Some((0x1000_0000, 0x1000)),
            kind: DeviceKind::Serial,
        },
    ];
    let device_manager = DeviceManager::new(devices);

    let virtio = device_manager.find_by_compatible("virtio,mmio");
    if virtio.len() != 1 || virtio[0].mmio != Some((0x1000_1000, 0x1000)) {
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
        DeviceInfo {
            compatible: vec![String::from("virtio,mmio")],
            mmio: Some((0x1000_1000, 0x1000)),
            kind: DeviceKind::VirtioBlock,
        },
        DeviceInfo {
            compatible: vec![String::from("virtio,mmio")],
            mmio: Some((0x1000_2000, 0x1000)),
            kind: DeviceKind::VirtioBlock,
        },
        DeviceInfo {
            compatible: vec![String::from("ns16550a")],
            mmio: Some((0x1000_0000, 0x1000)),
            kind: DeviceKind::Serial,
        },
    ];
    let device_manager = DeviceManager::new(devices);

    if device_manager.find_block_devices().len() != 2 {
        return Err("block device lookup did not return both devices");
    }
    if device_manager.find_by_kind(DeviceKind::Serial).len() != 1 {
        return Err("kind lookup did not return the serial device");
    }
    match device_manager.find_console() {
        Some(console) if console.mmio == Some((0x1000_0000, 0x1000)) => Ok(()),
        _ => Err("console lookup did not return the serial device"),
    }
}

/// Device manager returns MMIO only when the matched device provides it.
fn test_device_manager_find_mmio() -> Result<(), &'static str> {
    let devices = vec![
        DeviceInfo {
            compatible: vec![String::from("ns16550a")],
            mmio: Some((0x1000_0000, 0x1000)),
            kind: DeviceKind::Serial,
        },
        DeviceInfo {
            compatible: vec![String::from("riscv")],
            mmio: None,
            kind: DeviceKind::Other,
        },
    ];
    let device_manager = DeviceManager::new(devices);

    if device_manager.find_mmio("ns16550a") != Some((0x1000_0000, 0x1000)) {
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

/// The compile-time board feature selects the corresponding boot policy.
fn test_platform_policy_selection() -> Result<(), &'static str> {
    let policy = crate::hal::platform::select_policy();
    let name = policy.name();

    #[cfg(feature = "board_rvqemu")]
    assert_eq!(name, "qemu-riscv64");
    #[cfg(feature = "board_vf2")]
    assert_eq!(name, "visionfive2");
    #[cfg(feature = "board_laqemu")]
    assert_eq!(name, "qemu-loongarch64");

    Ok(())
}
