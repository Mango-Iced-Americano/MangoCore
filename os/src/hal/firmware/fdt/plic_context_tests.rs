use alloc::{string::String, vec, vec::Vec};

use crate::hal::platform::{
    DeviceInfo, DeviceKind, DeviceStatus, MmioRange, RawProperty, RawPropertyValidity,
    ResourceValidity,
};

use super::RiscvPlicContextTopology;

fn device(node_path: &str, parent_path: Option<&str>) -> DeviceInfo {
    DeviceInfo {
        node_path: String::from(node_path),
        parent_path: parent_path.map(String::from),
        status: DeviceStatus::Enabled(None),
        compatible: Vec::new(),
        raw_properties: Vec::new(),
        raw_property_validity: RawPropertyValidity::Valid,
        mmio_ranges: Vec::<MmioRange>::new(),
        resource_validity: ResourceValidity::Valid,
        kind: DeviceKind::Other,
    }
}

fn property(device: &mut DeviceInfo, name: &str, value: Vec<u8>) {
    device.raw_properties.push(RawProperty::new(name, value));
}

fn cells(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|value| value.to_be_bytes()).collect()
}

#[test]
fn plic_contexts_follow_phandles_with_sparse_harts() {
    let mut cpus = device("/cpus", Some("/"));
    property(&mut cpus, "#address-cells", cells(&[1]));

    let mut cpu_one = device("/cpus/cpu@1", Some("/cpus"));
    property(&mut cpu_one, "reg", cells(&[1]));
    let mut cpu_two = device("/cpus/cpu@2", Some("/cpus"));
    property(&mut cpu_two, "reg", cells(&[2]));
    let mut cpu_three = device("/cpus/cpu@3", Some("/cpus"));
    property(&mut cpu_three, "reg", cells(&[3]));
    let mut cpu_four = device("/cpus/cpu@4", Some("/cpus"));
    property(&mut cpu_four, "reg", cells(&[4]));

    let mut intc_one = device("/cpus/cpu@1/interrupt-controller", Some("/cpus/cpu@1"));
    intc_one.compatible.push(String::from("riscv,cpu-intc"));
    property(&mut intc_one, "phandle", cells(&[1]));
    property(&mut intc_one, "#interrupt-cells", cells(&[1]));
    let mut intc_two = device("/cpus/cpu@2/interrupt-controller", Some("/cpus/cpu@2"));
    intc_two.compatible.push(String::from("riscv,cpu-intc"));
    property(&mut intc_two, "phandle", cells(&[2]));
    property(&mut intc_two, "#interrupt-cells", cells(&[1]));
    let mut intc_three = device("/cpus/cpu@3/interrupt-controller", Some("/cpus/cpu@3"));
    intc_three.compatible.push(String::from("riscv,cpu-intc"));
    property(&mut intc_three, "phandle", cells(&[3]));
    property(&mut intc_three, "#interrupt-cells", cells(&[1]));
    let mut intc_four = device("/cpus/cpu@4/interrupt-controller", Some("/cpus/cpu@4"));
    intc_four.compatible.push(String::from("riscv,cpu-intc"));
    property(&mut intc_four, "phandle", cells(&[4]));
    property(&mut intc_four, "#interrupt-cells", cells(&[1]));

    let mut plic = device("/soc/interrupt-controller@c000000", Some("/soc"));
    property(
        &mut plic,
        "interrupts-extended",
        cells(&[1, 11, 1, 9, 2, 11, 2, 9, 3, 11, 3, 9, 4, 11, 4, 9]),
    );
    let devices = vec![
        cpus,
        cpu_one,
        cpu_two,
        cpu_three,
        cpu_four,
        intc_one,
        intc_two,
        intc_three,
        intc_four,
    ];

    let contexts = RiscvPlicContextTopology::new(&devices, &[1, 2, 3, 4], 2)
        .supervisor_contexts(&plic);

    assert_eq!(contexts, Some([Some(3), Some(1), Some(5), Some(7), None, None, None, None,
        None, None, None, None, None, None, None, None]));
}
