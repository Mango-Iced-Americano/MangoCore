use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::hal::platform::info::{
    DeviceInfo, DeviceKind, DeviceStatus, MmioRange, RawProperty, RawPropertyValidity,
    ResourceValidity,
};

pub(crate) fn vf2_mmc_snapshot() -> Vec<DeviceInfo> {
    vec![
        DeviceInfo {
            node_path: String::from("/"),
            parent_path: None,
            status: DeviceStatus::Enabled(None),
            compatible: Vec::new(),
            raw_properties: Vec::new(),
            raw_property_validity: RawPropertyValidity::Valid,
            mmio_ranges: Vec::new(),
            resource_validity: ResourceValidity::Valid,
            kind: DeviceKind::Other,
        },
        DeviceInfo {
            node_path: String::from("/soc"),
            parent_path: Some(String::from("/")),
            status: DeviceStatus::Enabled(None),
            compatible: Vec::new(),
            raw_properties: Vec::new(),
            raw_property_validity: RawPropertyValidity::Valid,
            mmio_ranges: Vec::new(),
            resource_validity: ResourceValidity::Valid,
            kind: DeviceKind::Other,
        },
        DeviceInfo {
            node_path: String::from("/soc/sdio0@16010000"),
            parent_path: Some(String::from("/soc")),
            status: DeviceStatus::Enabled(Some(String::from("okay"))),
            compatible: vec![String::from("snps,dw-mshc")],
            raw_properties: vec![
                RawProperty::new("compatible", b"snps,dw-mshc\0".to_vec()),
                RawProperty::new(
                    "reg",
                    vec![0, 0, 0, 0, 0x16, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0],
                ),
                RawProperty::new(
                    "clocks",
                    vec![0, 0, 0, 15, 0, 0, 0, 91, 0, 0, 0, 15, 0, 0, 0, 93],
                ),
                RawProperty::new("clock-names", b"biu\0ciu\0".to_vec()),
                RawProperty::new("resets", vec![0, 0, 0, 16, 0, 0, 0, 64]),
                RawProperty::new("reset-names", b"reset\0".to_vec()),
                RawProperty::new("assigned-clocks", vec![0, 0, 0, 15, 0, 0, 0, 93]),
                RawProperty::new("assigned-clock-rates", vec![0x02, 0xfa, 0xf0, 0x80]),
                RawProperty::new("fifo-depth", vec![0, 0, 0, 32]),
                RawProperty::new("bus-width", vec![0, 0, 0, 8]),
                RawProperty::new("pinctrl-names", b"default\0".to_vec()),
                RawProperty::new("pinctrl-0", vec![0, 0, 0, 22]),
                RawProperty::new("status", b"okay\0".to_vec()),
                RawProperty::new("u-boot,dm-spl", Vec::new()),
                RawProperty::new("phandle", vec![0, 0, 0, 95]),
            ],
            raw_property_validity: RawPropertyValidity::Valid,
            mmio_ranges: vec![MmioRange::new(0x1601_0000, 0x1_0000)],
            resource_validity: ResourceValidity::Valid,
            kind: DeviceKind::Other,
        },
        DeviceInfo {
            node_path: String::from("/soc/sdio1@16020000"),
            parent_path: Some(String::from("/soc")),
            status: DeviceStatus::Enabled(Some(String::from("okay"))),
            compatible: vec![String::from("snps,dw-mshc")],
            raw_properties: vec![
                RawProperty::new("compatible", b"snps,dw-mshc\0".to_vec()),
                RawProperty::new(
                    "reg",
                    vec![0, 0, 0, 0, 0x16, 2, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0],
                ),
                RawProperty::new(
                    "clocks",
                    vec![0, 0, 0, 15, 0, 0, 0, 92, 0, 0, 0, 15, 0, 0, 0, 94],
                ),
                RawProperty::new("clock-names", b"biu\0ciu\0".to_vec()),
                RawProperty::new("resets", vec![0, 0, 0, 16, 0, 0, 0, 65]),
                RawProperty::new("reset-names", b"reset\0".to_vec()),
                RawProperty::new("assigned-clocks", vec![0, 0, 0, 15, 0, 0, 0, 94]),
                RawProperty::new("assigned-clock-rates", vec![0x02, 0xfa, 0xf0, 0x80]),
                RawProperty::new("fifo-depth", vec![0, 0, 0, 32]),
                RawProperty::new("bus-width", vec![0, 0, 0, 4]),
                RawProperty::new("pinctrl-names", b"default\0".to_vec()),
                RawProperty::new("pinctrl-0", vec![0, 0, 0, 23]),
                RawProperty::new("status", b"okay\0".to_vec()),
                RawProperty::new("u-boot,dm-spl", Vec::new()),
                RawProperty::new("phandle", vec![0, 0, 0, 96]),
            ],
            raw_property_validity: RawPropertyValidity::Valid,
            mmio_ranges: vec![MmioRange::new(0x1602_0000, 0x1_0000)],
            resource_validity: ResourceValidity::Valid,
            kind: DeviceKind::Other,
        },
    ]
}
