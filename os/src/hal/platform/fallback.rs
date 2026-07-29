//! Static (compile-time) device descriptors.
//!
//! Each supported platform provides a `static_devices()` function that returns
//! the known MMIO device list. Used as fallback when no FDT or ACPI firmware
//! description is available.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use crate::hal::platform::info::{DeviceInfo, DeviceKind};

/// QEMU RISC-V virt machine static devices.
#[cfg(feature = "board_rvqemu")]
pub fn qemu_riscv_devices() -> Vec<DeviceInfo> {
    vec![
        DeviceInfo {
            compatible: vec![String::from("ns16550a")],
            mmio: Some((0x1000_0000, 0x1000)),
            kind: DeviceKind::Serial,
        },
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
            compatible: vec![String::from("virtio,mmio")],
            mmio: Some((0x1000_3000, 0x1000)),
            kind: DeviceKind::VirtioBlock,
        },
        DeviceInfo {
            compatible: vec![String::from("virtio,mmio")],
            mmio: Some((0x1000_4000, 0x1000)),
            kind: DeviceKind::VirtioBlock,
        },
        DeviceInfo {
            compatible: vec![String::from("virtio,mmio")],
            mmio: Some((0x1000_5000, 0x1000)),
            kind: DeviceKind::VirtioBlock,
        },
        DeviceInfo {
            compatible: vec![String::from("virtio,mmio")],
            mmio: Some((0x1000_6000, 0x1000)),
            kind: DeviceKind::VirtioBlock,
        },
        DeviceInfo {
            compatible: vec![String::from("virtio,mmio")],
            mmio: Some((0x1000_7000, 0x1000)),
            kind: DeviceKind::VirtioBlock,
        },
        DeviceInfo {
            compatible: vec![String::from("virtio,mmio")],
            mmio: Some((0x1000_8000, 0x1000)),
            kind: DeviceKind::VirtioNet,
        },
        DeviceInfo {
            compatible: vec![String::from("pci-host-ecam-generic")],
            mmio: Some((0x3000_0000, 0x1000_0000)),
            kind: DeviceKind::PciHost,
        },
        DeviceInfo {
            compatible: vec![String::from("pci-host-ecam-generic")],
            mmio: Some((0x4000_0000, 0x4000_0000)),
            kind: DeviceKind::PciHost,
        },
        DeviceInfo {
            compatible: vec![String::from("riscv,plic0")],
            mmio: Some((0x0C00_0000, 0x40_0000)),
            kind: DeviceKind::InterruptController,
        },
    ]
}

/// QEMU LoongArch64 virt machine static devices.
#[cfg(feature = "board_laqemu")]
pub fn qemu_la_devices() -> Vec<DeviceInfo> {
    vec![DeviceInfo {
        compatible: vec![String::from("ns16550a")],
        mmio: Some((0x1fe0_01e0, 0x100)),
        kind: DeviceKind::Serial,
    }]
}

/// Loongson 2K1000 static devices.
#[cfg(feature = "board_2k1000")]
pub fn k2100_devices() -> Vec<DeviceInfo> {
    vec![DeviceInfo {
        compatible: vec![String::from("ns16550a")],
        mmio: Some((0x1fe2_0000, 0x100)),
        kind: DeviceKind::Serial,
    }]
}

/// VisionFive 2 static devices (minimal — most discovery comes from FDT).
#[cfg(feature = "board_vf2")]
pub fn vf2_devices() -> Vec<DeviceInfo> {
    vec![DeviceInfo {
        compatible: vec![String::from("ns16550a")],
        mmio: Some((0x1000_0000, 0x1000)),
        kind: DeviceKind::Serial,
    }]
}

/// Return the static device list for the active board.
#[cfg(feature = "board_rvqemu")]
pub fn static_devices_for_board() -> Vec<DeviceInfo> {
    qemu_riscv_devices()
}

/// Return the static device list for the active board.
#[cfg(feature = "board_laqemu")]
pub fn static_devices_for_board() -> Vec<DeviceInfo> {
    qemu_la_devices()
}

/// Return the static device list for the active board.
#[cfg(feature = "board_2k1000")]
pub fn static_devices_for_board() -> Vec<DeviceInfo> {
    k2100_devices()
}

/// Return the static device list for the active board.
#[cfg(feature = "board_vf2")]
pub fn static_devices_for_board() -> Vec<DeviceInfo> {
    vf2_devices()
}

/// Return no static devices when the board has no catalogue.
#[cfg(not(any(
    feature = "board_rvqemu",
    feature = "board_laqemu",
    feature = "board_2k1000",
    feature = "board_vf2",
)))]
pub fn static_devices_for_board() -> Vec<DeviceInfo> {
    Vec::new()
}
