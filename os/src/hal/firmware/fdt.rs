//! FDT (Flattened Device Tree) parser.
//!
//! Wraps the `fdt` crate. Pre-heap parsing operates on raw bytes;
//! post-heap full parsing uses the `fdt::Fdt` type.

use crate::hal::boot::BootProtocol;
use crate::hal::firmware::{MAX_FIRMWARE_RESERVED, MAX_MEMORY_REGIONS};
use crate::hal::platform::info::{DeviceInfo, DeviceKind, FirmwareKind, PlatformInfo};
use alloc::string::String;
use alloc::vec::Vec;
use core::slice;

/// Read the `totalsize` field from the FDT header at `paddr`.
/// FDT header layout: magic(u32 BE) totalsize(u32 BE) at offset 4.
///
/// # Safety
/// `paddr` must point to identity-mapped, four-byte-aligned memory containing
/// an FDT header.
unsafe fn read_totalsize(paddr: usize) -> usize {
    // Basic sanity: FDT headers are non-zero and page-aligned.
    if paddr == 0 || paddr & 0xFFF != 0 {
        return 0;
    }

    let header = paddr as *const u32;
    // SAFETY: [Categories 6 and 11 — alignment and provenance] `paddr` was
    // validated non-zero and page-aligned; the RISC-V FDT boot protocol
    // guarantees it identifies an identity-mapped FDT header.
    let magic = u32::from_be(unsafe { header.read_volatile() });
    if magic != 0xd00dfeed {
        return 0;
    }

    // SAFETY: [Categories 6, 10, and 11 — alignment, bounds, provenance] The
    // validated FDT magic establishes the fixed header layout, whose second
    // u32 field is in bounds and aligned at `header.add(1)`.
    let totalsize_be = unsafe { header.add(1).read_volatile() };
    let size = u32::from_be(totalsize_be) as usize;
    if !(40..=2 * 1024 * 1024).contains(&size) {
        return 0;
    }
    size
}

/// Parse `/memory` nodes from raw DTB bytes at `paddr`.
/// Fills the static `MEMORY_BUF`. Returns true on success.
///
/// Called pre-heap — operates on raw bytes, does NOT allocate.
///
/// # Safety
/// `paddr` must point to a valid DTB in identity-mapped memory.
pub fn parse_memory_regions(dtb_paddr: usize) -> bool {
    let total_size = unsafe { read_totalsize(dtb_paddr) };
    if total_size < 40 || total_size > 2 * 1024 * 1024 {
        return false;
    }

    // SAFETY: [Categories 10 and 11 — bounds and provenance] The boot
    // protocol supplies an identity-mapped DTB at `dtb_paddr`; `total_size`
    // was read from its header and bounded before this slice is formed.
    let blob = unsafe { slice::from_raw_parts(dtb_paddr as *const u8, total_size) };
    let fdt = match fdt::Fdt::new_unaligned_fallible(blob) {
        Ok(fdt) => fdt,
        Err(_) => return false,
    };
    let memory = match fdt
        .root()
        .and_then(|root| root.memory())
        .and_then(|memory| memory.reg())
    {
        Ok(memory) => memory,
        Err(_) => return false,
    };

    // SAFETY: [Categories 1 and 2 — aliasing and data races]
    // `populate_memory_regions()` runs once during single-threaded boot before
    // `mm::init()`; all later access to MEMORY_BUF is read-only.
    let buf = unsafe { &mut *core::ptr::addr_of_mut!(crate::hal::firmware::MEMORY_BUF) };
    buf.region_count = 0;
    buf.reserved_count = 0;

    for region in memory.iter::<u64, usize>() {
        if buf.region_count >= MAX_MEMORY_REGIONS {
            break;
        }
        let region = match region {
            Ok(region) => region,
            Err(_) => return false,
        };
        let start = region.address as usize;
        let size = region.len;
        if size == 0 {
            continue;
        }
        buf.regions[buf.region_count] = (start, start.wrapping_add(size));
        buf.region_count += 1;
    }

    if buf.region_count == 0 {
        return false;
    }

    // Preserve compile-time firmware reserved regions as a floor. FDT may add
    // additional reserved-memory regions in the future.
    let reserved = crate::hal::firmware::static_provider::FIRMWARE_RESERVED_REGIONS_FALLBACK;
    buf.reserved_count = 0;
    for (index, &(start, end)) in reserved.iter().enumerate() {
        if index >= MAX_FIRMWARE_RESERVED {
            break;
        }
        buf.reserved[index] = (start, end);
        buf.reserved_count = index + 1;
    }

    // Keep the DTB blob out of the frame allocator as well.
    let dtb_start = dtb_paddr & !0xFFF;
    let dtb_end = (dtb_paddr + total_size + 0xFFF) & !0xFFF;
    if buf.reserved_count < MAX_FIRMWARE_RESERVED {
        buf.reserved[buf.reserved_count] = (dtb_start, dtb_end);
        buf.reserved_count += 1;
    }

    true
}

type FallibleFdt<'a> = fdt::Fdt<
    'a,
    (
        fdt::parsing::unaligned::UnalignedParser<'a>,
        fdt::parsing::NoPanic,
    ),
>;

/// Walk the FDT and collect nodes with a `compatible` property as devices.
///
/// The first `reg` entry, when present and representable by the FDT crate,
/// becomes the device's MMIO region.
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

    for node in nodes {
        let Ok((_, node)) = node else {
            continue;
        };
        let Ok(Some(compatible)) = node.property::<fdt::properties::Compatible<'_>>() else {
            continue;
        };
        let compatible: Vec<String> = compatible.all().map(String::from).collect();

        let mmio = node
            .reg()
            .ok()
            .flatten()
            .and_then(|reg| reg.iter::<usize, usize>().next())
            .and_then(Result::ok)
            .map(|region| (region.address, region.len));

        devices.push(DeviceInfo {
            kind: classify_device(&compatible),
            compatible,
            mmio,
        });
    }
}

/// Classify a device using its compatible strings.
fn classify_device(compatible: &[String]) -> DeviceKind {
    for entry in compatible {
        match entry.as_str() {
            "ns16550a" | "ns16550" => return DeviceKind::Serial,
            // virtio,mmio is a transport, not a device type. The actual type
            // is determined when the block and network drivers probe it.
            "virtio,mmio" => return DeviceKind::Other,
            "riscv,plic0" | "riscv,plic" => return DeviceKind::InterruptController,
            "pci-host-ecam-generic" | "pci-host-cam-generic" => return DeviceKind::PciHost,
            _ => {}
        }
    }
    DeviceKind::Other
}

/// Build a full `PlatformInfo` from the DTB at `paddr`.
///
/// Must be called AFTER `mm::init()` — uses `alloc`.
/// Returns `None` if the DTB is absent, invalid, or lies in a page
/// that is not identity-mapped after the kernel page-table switch
/// (e.g. because it was reserved as a firmware carveout).
pub fn build_platform_info(dtb_paddr: usize) -> Option<PlatformInfo> {
    if dtb_paddr == 0 {
        return None;
    }
    let bi = crate::hal::boot::boot_info();
    if !matches!(bi.protocol, BootProtocol::RiscvFdt) {
        return None;
    }

    // After mm::init() the kernel page table only identity-maps
    // usable frame regions.  If the DTB was carved out (e.g. by the
    // pre-heap FDT memory parser) its pages are no longer accessible.
    if !crate::mm::is_ram_phys_addr(dtb_paddr) {
        return None;
    }

    let total_size = unsafe { read_totalsize(dtb_paddr) };
    if total_size < 40 || total_size > 2 * 1024 * 1024 {
        return None;
    }

    // SAFETY: [Categories 10 and 11 — bounds and provenance] The boot
    // protocol supplies an identity-mapped DTB at `dtb_paddr`; `total_size`
    // was read from its header and bounded before this slice is formed.
    let blob = unsafe { slice::from_raw_parts(dtb_paddr as *const u8, total_size) };
    let fdt = fdt::Fdt::new_unaligned_fallible(blob).ok()?;

    let boot = *crate::hal::boot::boot_info();
    let root = fdt.root().ok()?;
    let model = root.model().ok().map(String::from);

    // Kernel command line: prefer DTB /chosen/bootargs.
    let cmdline = root
        .chosen()
        .ok()
        .and_then(|chosen| chosen.bootargs().ok())
        .flatten()
        .map(String::from)
        .unwrap_or_else(|| crate::bootargs::get_cmdline().into());

    let mut devices = enumerate_devices(&fdt);
    // Sort by MMIO base address so virtio slots are probed in QEMU order:
    // x0 (0x10001000) before x1 (0x10002000), and so on.
    devices.sort_by_key(|device| device.mmio.map(|(base, _)| base).unwrap_or(usize::MAX));

    Some(PlatformInfo {
        firmware: FirmwareKind::Fdt,
        boot,
        model,
        cmdline,
        devices,
    })
}
