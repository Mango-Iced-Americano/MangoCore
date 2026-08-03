//! FDT (Flattened Device Tree) parser.
//!
//! Wraps the `fdt` crate. Pre-heap parsing operates on raw bytes;
//! post-heap full parsing uses the external `fdt` crate.

use crate::hal::boot::BootProtocol;
use crate::hal::firmware::{MAX_FDT_SNAPSHOT_SIZE, MAX_MEMORY_REGIONS};
use crate::hal::platform::{
    ConsoleInfo, DeviceInfo, DeviceKind, DeviceStatus, FirmwareKind, MmioRange, PciHost,
    PlatformInfo, RawProperty, RawPropertyValidity, ResourceValidity,
};
use ::fdt as devicetree;
use alloc::string::String;
use alloc::vec::Vec;
use core::convert::{TryFrom, TryInto};
use core::slice;

use super::{FDT_SNAPSHOT_BYTES, FDT_SNAPSHOT_META};

/// Read the `totalsize` field from the FDT header at `paddr`.
/// FDT header layout: magic(u32 BE) totalsize(u32 BE) at offset 4.
///
/// # Safety
/// `paddr` must point to identity-mapped, four-byte-aligned memory containing
/// an FDT header.
unsafe fn read_totalsize(paddr: usize) -> usize {
    // Basic sanity: FDT headers are non-zero and aligned for volatile u32 reads.
    if paddr == 0 || paddr & 0x3 != 0 {
        return 0;
    }

    let header = paddr as *const u32;
    // SAFETY: [Categories 6 and 11 — alignment and provenance] `paddr` was
    // validated non-zero and aligned; the architecture boot protocol guarantees
    // it identifies an identity-mapped FDT header in ordinary RAM.
    let magic = u32::from_be(unsafe { header.read_volatile() });
    if magic != 0xd00dfeed {
        return 0;
    }

    // SAFETY: [Categories 6, 10, and 11 — alignment, bounds, provenance] The
    // validated FDT magic establishes the fixed header layout, whose second
    // u32 field is in bounds and aligned at `header.add(1)`.
    let totalsize_be = unsafe { header.add(1).read_volatile() };
    let size = u32::from_be(totalsize_be) as usize;
    if !(40..=MAX_FDT_SNAPSHOT_SIZE).contains(&size) {
        return 0;
    }
    size
}

/// Copy a validated firmware FDT into the durable boot-data snapshot.
///
/// Called before BSS clear, while the firmware address remains accessible.
pub(super) fn capture_fdt_snapshot(dtb_paddr: usize) -> bool {
    let boot = crate::hal::boot::boot_info();
    if !matches!(
        boot.protocol,
        BootProtocol::RiscvFdt | BootProtocol::LoongArchEfi
    ) || dtb_paddr == 0
        || dtb_paddr & 0x3 != 0
    {
        return false;
    }

    let total_size = unsafe { read_totalsize(dtb_paddr) };
    if total_size == 0 {
        return false;
    }
    #[cfg(target_arch = "loongarch64")]
    if !super::efi::contains_firmware_range(dtb_paddr, total_size) {
        return false;
    }

    let source = {
        // SAFETY: [Categories 10 and 11 — bounds and provenance] The firmware
        // handoff supplies an identity-mapped FDT at `dtb_paddr`; LA64 also
        // proves the complete range is DRAM. Magic and size were checked above.
        let blob = unsafe { slice::from_raw_parts(dtb_paddr as *const u8, total_size) };
        if devicetree::Fdt::new_unaligned_fallible(blob).is_err() {
            return false;
        }
        blob.as_ptr()
    };

    let metadata = core::ptr::addr_of_mut!(FDT_SNAPSHOT_META);
    // SAFETY: [Categories 1 and 2 — aliasing and data races] This function
    // runs once during single-threaded boot. A nonzero length makes the
    // snapshot immutable, and all later access is read-only.
    if unsafe { (*metadata).len } != 0 {
        return false;
    }
    // SAFETY: [Categories 1, 10, and 11 — aliasing, bounds, provenance] The
    // source was validated as `total_size` accessible bytes above, the
    // destination is the fixed MAX_FDT_SNAPSHOT_SIZE buffer, and `ptr::copy`
    // deliberately permits overlapping source and destination ranges. Length
    // publication follows only after the complete copy.
    unsafe {
        let destination = core::ptr::addr_of_mut!(FDT_SNAPSHOT_BYTES).cast::<u8>();
        core::ptr::copy(source, destination, total_size);
        (*metadata).source_paddr = dtb_paddr;
        (*metadata).len = total_size;
    }
    true
}

/// Return the immutable FDT snapshot published during early boot.
fn fdt_snapshot() -> Option<&'static [u8]> {
    // SAFETY: [Categories 1 and 2 — aliasing and data races] Early boot
    // publishes the length only after the complete copy, before `mm::init()`
    // and scheduler startup. The snapshot is never mutated afterwards.
    let metadata = unsafe { &*core::ptr::addr_of!(FDT_SNAPSHOT_META) };
    if metadata.len == 0 || metadata.len > MAX_FDT_SNAPSHOT_SIZE {
        return None;
    }
    // SAFETY: `capture_fdt_snapshot` 在发布非零 len 前已写完这些字节，且后续
    // 不再修改缓冲；专用 linker section 保证 `mem_clear()` 不会覆盖它。
    Some(unsafe {
        slice::from_raw_parts(
            core::ptr::addr_of!(FDT_SNAPSHOT_BYTES).cast::<u8>(),
            metadata.len,
        )
    })
}

/// Parse `/memory` nodes from the durable DTB snapshot.
/// Fills the static `MEMORY_BUF`. Returns true on success.
///
/// Called pre-heap — does NOT allocate.
pub(super) fn parse_early_resources() -> bool {
    let blob = match fdt_snapshot() {
        Some(blob) => blob,
        None => return false,
    };
    let total_size = blob.len();
    let fdt = match devicetree::Fdt::new_unaligned_fallible(blob) {
        Ok(fdt) => fdt,
        Err(_) => return false,
    };
    // SAFETY: [Categories 1 and 2 — aliasing and data races]
    // `discover_early_resources()` 仅由 BSP 在 `mm::init()` 前调用；
    // AP 尚未启动，后续对 MEMORY_BUF 的访问全部只读。
    let buf = unsafe { &mut *core::ptr::addr_of_mut!(crate::hal::firmware::MEMORY_BUF) };
    buf.region_count = 0;
    buf.reserved_count = 0;

    if !parse_memory_nodes(&fdt, buf) {
        return false;
    }

    buf.reserved_count = 0;
    buf.mmio_count = 0;
    #[cfg(target_arch = "riscv64")]
    {
        buf.timebase_frequency = 0;
    }

    // Keep the DTB blob out of the frame allocator as well.
    // SAFETY: metadata 与快照由同一个 BSP-only 提交点一次性发布。
    let dtb_paddr = unsafe { (*core::ptr::addr_of!(FDT_SNAPSHOT_META)).source_paddr };
    let dtb_start = dtb_paddr & !0xFFF;
    let dtb_end = match dtb_paddr
        .checked_add(total_size)
        .and_then(|end| end.checked_add(0xFFF))
    {
        Some(end) => end & !0xFFF,
        None => return false,
    };
    if !push_range(
        &mut buf.reserved,
        &mut buf.reserved_count,
        (dtb_start, dtb_end),
    ) {
        return false;
    }

    if !parse_memreserve(blob, buf) || !parse_node_resources(&fdt, buf) {
        return false;
    }
    #[cfg(target_arch = "riscv64")]
    {
        return buf.timebase_frequency != 0;
    }
    #[cfg(target_arch = "loongarch64")]
    true
}

/// 收集所有根级 `memory@...` 节点，而不是只取第一个 `/memory`。
///
/// Devicetree 允许一个 memory 节点含多个 `reg`，也允许多个 memory 节点。
/// 固定表满时必须失败，不能静默少报 RAM；排序后拒绝重叠，给后续 frame
/// allocator 提供稳定的半开区间合同。
fn parse_memory_nodes(
    fdt: &FallibleFdt<'_>,
    buffer: &mut crate::hal::firmware::MemoryRegionBuf,
) -> bool {
    let Ok(nodes) = fdt.all_nodes() else {
        return false;
    };
    for entry in nodes {
        let Ok((depth, node)) = entry else {
            return false;
        };
        let Ok(name) = node.name() else {
            return false;
        };
        if depth != 1 || &*name.name != "memory" {
            continue;
        }
        let Ok(Some(regions)) = node.reg() else {
            return false;
        };
        for region in regions.iter::<usize, usize>() {
            let Ok(region) = region else {
                return false;
            };
            if region.len == 0 {
                continue;
            }
            if buffer.region_count == MAX_MEMORY_REGIONS {
                return false;
            }
            let Some(end) = region.address.checked_add(region.len) else {
                return false;
            };
            buffer.regions[buffer.region_count] = (region.address, end);
            buffer.region_count += 1;
        }
    }
    if buffer.region_count == 0 {
        return false;
    }

    buffer.regions[..buffer.region_count].sort_unstable_by_key(|range| range.0);
    buffer.regions[..buffer.region_count]
        .windows(2)
        .all(|pair| pair[0].1 <= pair[1].0)
}

fn push_range<const N: usize>(
    ranges: &mut [(usize, usize); N],
    count: &mut usize,
    mut range: (usize, usize),
) -> bool {
    if range.0 >= range.1 {
        return false;
    }
    let mut index = 0;
    while index < *count {
        let existing = ranges[index];
        if range.0 <= existing.1 && existing.0 <= range.1 {
            range.0 = range.0.min(existing.0);
            range.1 = range.1.max(existing.1);
            ranges[index] = ranges[*count - 1];
            *count -= 1;
            continue;
        }
        index += 1;
    }
    if *count == N {
        return false;
    }
    ranges[*count] = range;
    *count += 1;
    true
}

fn be_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    bytes
        .get(offset..offset.checked_add(4)?)?
        .try_into()
        .ok()
        .map(u32::from_be_bytes)
}

fn be_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    bytes
        .get(offset..offset.checked_add(8)?)?
        .try_into()
        .ok()
        .map(u64::from_be_bytes)
}

fn parse_memreserve(blob: &[u8], buffer: &mut crate::hal::firmware::MemoryRegionBuf) -> bool {
    let Some(mut offset) = be_u32(blob, 16).map(|offset| offset as usize) else {
        return false;
    };
    if offset < 40 || offset % 8 != 0 || offset >= blob.len() {
        return false;
    }
    while let (Some(start), Some(size)) = (be_u64(blob, offset), be_u64(blob, offset + 8)) {
        offset = match offset.checked_add(16) {
            Some(next) => next,
            None => return false,
        };
        if start == 0 && size == 0 {
            return true;
        }
        let Ok(start) = usize::try_from(start) else {
            return false;
        };
        let Ok(size) = usize::try_from(size) else {
            return false;
        };
        let Some(end) = start.checked_add(size) else {
            return false;
        };
        if !push_range(
            &mut buffer.reserved,
            &mut buffer.reserved_count,
            (start, end),
        ) {
            return false;
        }
    }
    false
}

fn range_overlaps_memory(range: (usize, usize), memory: &[(usize, usize)]) -> bool {
    memory
        .iter()
        .any(|memory_range| range.0 < memory_range.1 && memory_range.0 < range.1)
}

fn parse_node_resources(
    fdt: &FallibleFdt<'_>,
    buffer: &mut crate::hal::firmware::MemoryRegionBuf,
) -> bool {
    let Ok(nodes) = fdt.all_nodes() else {
        return false;
    };
    let mut reserved_memory_depth = None;
    for entry in nodes {
        let Ok((depth, node)) = entry else {
            return false;
        };
        if reserved_memory_depth.is_some_and(|parent_depth| depth <= parent_depth) {
            reserved_memory_depth = None;
        }
        let Ok(node_name) = node.name() else {
            return false;
        };
        if depth == 1 && &*node_name.name == "reserved-memory" {
            reserved_memory_depth = Some(depth);
        }
        #[cfg(target_arch = "riscv64")]
        if depth == 1 && &*node_name.name == "cpus" {
            let Ok(properties) = node.properties() else {
                return false;
            };
            for property in properties {
                let Ok(property) = property else {
                    return false;
                };
                if property.name == "timebase-frequency" {
                    let Some(frequency) = be_u32(property.value, 0) else {
                        return false;
                    };
                    buffer.timebase_frequency = frequency as usize;
                }
            }
        }
        // `reg` 只在根总线或一层 SoC 总线下能直接解释为 CPU 物理地址。
        // PCI 子节点的 `reg` 是 BDF/PCI address cells，不能冒充 early MMIO；
        // `/reserved-memory` 子树则不受深度限制，所有 carveout 都必须保留。
        if reserved_memory_depth.is_some() || depth <= 2 {
            let Ok(Some(regions)) = node.reg() else {
                continue;
            };
            for region in regions.iter::<usize, usize>() {
                let Ok(region) = region else {
                    return false;
                };
                if region.len == 0 {
                    continue;
                }
                let Some(end) = region.address.checked_add(region.len) else {
                    return false;
                };
                let range = (region.address, end);
                let page_range = (
                    range.0 & !0xfff,
                    match range.1.checked_add(0xfff) {
                        Some(end) => end & !0xfff,
                        None => return false,
                    },
                );
                if reserved_memory_depth.is_some() {
                    if !push_range(&mut buffer.reserved, &mut buffer.reserved_count, range) {
                        return false;
                    }
                } else if !range_overlaps_memory(page_range, &buffer.regions[..buffer.region_count])
                    && !push_range(&mut buffer.mmio, &mut buffer.mmio_count, range)
                {
                    return false;
                }
            }
        }
        let is_pci_host = node
            .property::<devicetree::properties::Compatible<'_>>()
            .ok()
            .flatten()
            .is_some_and(|compatible| {
                compatible.all().any(|entry| {
                    matches!(&*entry, "pci-host-ecam-generic" | "pci-host-cam-generic")
                })
            });
        if is_pci_host {
            let Ok(properties) = node.properties() else {
                return false;
            };
            for property in properties {
                let Ok(property) = property else {
                    return false;
                };
                if property.name == "ranges" && !parse_pci_mmio_ranges(property.value, buffer) {
                    return false;
                }
            }
        }
    }
    true
}

fn parse_pci_mmio_ranges(
    ranges: &[u8],
    buffer: &mut crate::hal::firmware::MemoryRegionBuf,
) -> bool {
    if ranges.len() % PCI_RANGE_ENTRY_BYTES != 0 {
        return false;
    }
    for range in ranges.chunks_exact(PCI_RANGE_ENTRY_BYTES) {
        let Some(space) = be_u32(range, 0).map(|value| value & PCI_RANGE_SPACE_MASK) else {
            return false;
        };
        if !matches!(space, PCI_RANGE_MEMORY32 | PCI_RANGE_MEMORY64) {
            continue;
        }
        let Some(base) = be_u64(range, 12).and_then(|value| usize::try_from(value).ok()) else {
            return false;
        };
        let Some(size) = be_u64(range, 20).and_then(|value| usize::try_from(value).ok()) else {
            return false;
        };
        if size == 0 {
            continue;
        }
        let Some(end) = base.checked_add(size) else {
            return false;
        };
        if !push_range(&mut buffer.mmio, &mut buffer.mmio_count, (base, end)) {
            return false;
        }
    }
    true
}

type FallibleFdt<'a> = devicetree::Fdt<
    'a,
    (
        devicetree::parsing::unaligned::UnalignedParser<'a>,
        devicetree::parsing::NoPanic,
    ),
>;

fn enumerate_devices(fdt: &FallibleFdt<'_>) -> Vec<DeviceInfo> {
    let mut devices = Vec::new();
    walk_nodes(fdt, &mut devices);
    devices
}

/// Collect device information from the FDT's depth-first node walk.
fn walk_nodes(fdt: &FallibleFdt<'_>, devices: &mut Vec<DeviceInfo>) {
    let Ok(nodes) = fdt.all_nodes() else {
        return;
    };
    let mut path_components = Vec::new();

    for node in nodes {
        let Ok((depth, node)) = node else {
            continue;
        };
        let node_name = match node.name() {
            Ok(node_name) => node_name,
            Err(_) => continue,
        };
        let node_component = match node_name.unit_address {
            Some(unit_address) => alloc::format!("{}@{}", &*node_name.name, unit_address),
            None => String::from(&*node_name.name),
        };
        if depth == 0 {
            path_components.clear();
        } else {
            let parent_depth = depth.saturating_sub(1);
            if path_components.len() < parent_depth {
                continue;
            }
            path_components.truncate(parent_depth);
            path_components.push(node_component);
        }
        let node_path = match depth {
            0 => String::from("/"),
            _ => alloc::format!("/{}", path_components.join("/")),
        };
        let parent_path = match depth {
            0 => None,
            1 => Some(String::from("/")),
            _ => Some(alloc::format!(
                "/{}",
                path_components[..path_components.len() - 1].join("/")
            )),
        };
        let (raw_properties, raw_property_validity) = match node.properties() {
            Ok(properties) => {
                let mut raw_properties = Vec::new();
                let mut raw_property_validity = RawPropertyValidity::Valid;
                for property in properties {
                    match property {
                        Ok(property)
                            if raw_properties
                                .iter()
                                .all(|entry: &RawProperty| entry.name != property.name) =>
                        {
                            raw_properties
                                .push(RawProperty::new(property.name, property.value.to_vec()));
                        }
                        Ok(_) | Err(_) => {
                            raw_property_validity = RawPropertyValidity::Malformed;
                            break;
                        }
                    }
                }
                (raw_properties, raw_property_validity)
            }
            Err(_) => (Vec::new(), RawPropertyValidity::Malformed),
        };
        let compatible = match node.property::<devicetree::properties::Compatible<'_>>() {
            Ok(Some(compatible)) => compatible.all().map(String::from).collect(),
            Ok(None) | Err(_) => Vec::new(),
        };

        let (mmio_ranges, mmio_resource_validity) = match node.reg() {
            Ok(Some(regions)) => {
                let mut mmio_ranges = Vec::new();
                let mut resource_validity = ResourceValidity::Valid;
                for region in regions.iter::<usize, usize>() {
                    match region {
                        Ok(region) if region.len > 0 => {
                            mmio_ranges.push(MmioRange::new(region.address, region.len));
                        }
                        Ok(_) | Err(_) => {
                            resource_validity = ResourceValidity::Malformed;
                            break;
                        }
                    }
                }
                (mmio_ranges, resource_validity)
            }
            Ok(None) => (Vec::new(), ResourceValidity::Valid),
            Err(_) => (Vec::new(), ResourceValidity::Malformed),
        };
        let resource_validity = match (mmio_resource_validity, raw_property_validity) {
            (ResourceValidity::Valid, RawPropertyValidity::Valid) => ResourceValidity::Valid,
            (ResourceValidity::Malformed, _) | (_, RawPropertyValidity::Malformed) => {
                ResourceValidity::Malformed
            }
        };
        let status = match node.property::<devicetree::properties::Status<'_>>() {
            Ok(Some(status)) => DeviceStatus::from_fdt(Some(&status)),
            Ok(None) => DeviceStatus::from_fdt(None),
            Err(_) => DeviceStatus::Malformed,
        };

        devices.push(DeviceInfo {
            node_path,
            parent_path,
            status,
            kind: classify_device(&compatible),
            compatible,
            raw_properties,
            raw_property_validity,
            mmio_ranges,
            resource_validity,
        });
    }
}

/// Classify a device using its compatible strings.
fn classify_device(compatible: &[String]) -> DeviceKind {
    for entry in compatible {
        match entry.as_str() {
            "ns16550a" | "ns16550" | "snps,dw-apb-uart" => return DeviceKind::Serial,
            // virtio,mmio is a transport, not a device type. The actual type
            // is determined when the block and network drivers probe it.
            "virtio,mmio" => return DeviceKind::Other,
            "riscv,plic0"
            | "riscv,plic"
            | "loongson,cpu-interrupt-controller"
            | "loongson,liointc"
            | "loongson,eiointc"
            | "loongson,ls2k1000-icu"
            | "loongson,ls2k2000-eiointc" => return DeviceKind::InterruptController,
            "pci-host-ecam-generic" | "pci-host-cam-generic" => return DeviceKind::PciHost,
            _ => {}
        }
    }
    DeviceKind::Other
}

fn property_cstr<'a>(device: &'a DeviceInfo, property_name: &str) -> Option<&'a str> {
    let value = device.raw_property(property_name).ok()?;
    let terminator = value
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(value.len());
    core::str::from_utf8(&value[..terminator]).ok()
}

fn resolve_stdout_path(devices: &[DeviceInfo]) -> Option<String> {
    let chosen = devices
        .iter()
        .find(|device| device.node_path == "/chosen")?;
    let stdout = property_cstr(chosen, "stdout-path")?;
    let node_path = stdout.split(':').next()?;
    if node_path.starts_with('/') {
        return Some(String::from(node_path));
    }
    let aliases = devices
        .iter()
        .find(|device| device.node_path == "/aliases")?;
    property_cstr(aliases, node_path).map(String::from)
}

fn resolve_console(devices: &[DeviceInfo]) -> Option<ConsoleInfo> {
    let stdout_path = resolve_stdout_path(devices)?;
    let serial = devices.iter().find(|device| {
        device.node_path == stdout_path
            && device.kind == DeviceKind::Serial
            && device.is_enabled()
            && device.resource_validity == ResourceValidity::Valid
    })?;
    let range = serial.mmio_range(0)?;
    let register_shift = match serial.raw_property("reg-shift") {
        Ok(value) => usize::try_from(be_u32(value, 0)?).ok()?,
        Err(_) => 0,
    };
    if register_shift > 3 || range.size <= (5usize << register_shift) {
        return None;
    }
    Some(ConsoleInfo {
        range,
        register_shift,
    })
}

const PCI_RANGE_ENTRY_BYTES: usize = 28;
const PCI_RANGE_MEMORY32: u32 = 0x0200_0000;
const PCI_RANGE_MEMORY64: u32 = 0x0300_0000;
const PCI_RANGE_SPACE_MASK: u32 = 0x0300_0000;

fn resolve_pci_host(devices: &[DeviceInfo]) -> Option<PciHost> {
    let host = devices.iter().find(|device| {
        device.kind == DeviceKind::PciHost
            && device.is_enabled()
            && device.resource_validity == ResourceValidity::Valid
    })?;
    let ecam = host.mmio_range(0)?;
    let ranges = host.raw_property("ranges").ok()?;
    if ranges.len() % PCI_RANGE_ENTRY_BYTES != 0 {
        return None;
    }

    for range in ranges.chunks_exact(PCI_RANGE_ENTRY_BYTES) {
        let space = be_u32(range, 0)? & PCI_RANGE_SPACE_MASK;
        if !matches!(space, PCI_RANGE_MEMORY32 | PCI_RANGE_MEMORY64) {
            continue;
        }
        let mmio_base = usize::try_from(be_u64(range, 12)?).ok()?;
        let mmio_size = usize::try_from(be_u64(range, 20)?).ok()?;
        if mmio_size != 0 {
            return Some(PciHost {
                ecam_base: ecam.base,
                ecam_size: ecam.size,
                mmio_base,
                mmio_size,
            });
        }
    }
    None
}

/// Build a full `PlatformInfo` from the pre-heap FDT snapshot.
///
/// Must be called AFTER `mm::init()` — uses `alloc`.
/// Returns `None` if the snapshot is absent or invalid.
pub(super) fn build_platform_info() -> Option<PlatformInfo> {
    let boot = *crate::hal::boot::boot_info();
    if !matches!(
        boot.protocol,
        BootProtocol::RiscvFdt | BootProtocol::LoongArchEfi
    ) {
        return None;
    }

    let blob = fdt_snapshot()?;
    let fdt = devicetree::Fdt::new_unaligned_fallible(blob).ok()?;
    let root = fdt.root().ok()?;
    let model = root.model().ok().map(String::from);

    // Kernel command line: prefer DTB /chosen/bootargs.
    let cmdline = root
        .chosen()
        .ok()
        .and_then(|chosen| chosen.bootargs().ok())
        .flatten()
        .map(String::from)
        .unwrap_or_else(|| crate::bootargs::compiled_cmdline().into());

    let mut devices = enumerate_devices(&fdt);
    // Sort by MMIO base address so devices are probed in a deterministic
    // address-ascending order (e.g. virtio slot x0 before x1).
    devices.sort_by_key(|device| {
        device
            .mmio_range(0)
            .map(|range| range.base)
            .unwrap_or(usize::MAX)
    });

    let console = resolve_console(&devices);
    let pci_host = resolve_pci_host(&devices);
    Some(PlatformInfo {
        firmware: FirmwareKind::Fdt,
        boot,
        model,
        cmdline,
        devices,
        console,
        pci_host,
    })
}
