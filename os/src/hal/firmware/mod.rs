#![allow(static_mut_refs)]

//! Firmware description providers.
//!
//! Abstracts how the kernel discovers hardware: Flattened Device Tree (FDT),
//! ACPI tables, or compile-time static configuration.
//!
//! # Two-phase initialization
//!
//! 1. **Pre-heap** (`populate_memory_regions`): Validate and retain the raw
//!    DTB, then parse only `/memory` nodes to populate `MEMORY_BUF`. Called
//!    before `mm::init()` and BSS clear. Zero-allocation.
//!
//! 2. **Post-heap** (`build_platform_info`): Full FDT parse producing
//!    `PlatformInfo` with device nodes, cmdline, etc. Called after `mm::init()`.
//!
//! RV64 直接使用 SBI 传入的 FDT；LA64 先从 EFI system table（`a2`）按
//! `EFI_FDT_GUID` 查找 FDT，仅在 2K1000 实板 EFI 缺失时退回静态板级配置。

#[cfg(target_arch = "loongarch64")]
mod efi;
mod fdt;
#[cfg(all(target_arch = "loongarch64", feature = "boot_la_uboot_dmw"))]
mod static_provider;

pub use fdt::build_platform_info;

use crate::hal::boot;
#[cfg(all(target_arch = "loongarch64", feature = "boot_la_uboot_dmw"))]
use static_provider::{FIRMWARE_RESERVED_REGIONS_FALLBACK, MEMORY_REGIONS_FALLBACK};

/// Maximum number of DRAM banks supported.
pub const MAX_MEMORY_REGIONS: usize = 8;
/// Maximum number of firmware-reserved regions.
pub const MAX_FIRMWARE_RESERVED: usize = 16;
/// Maximum FDT-defined MMIO intervals mapped before driver probing.
pub const MAX_EARLY_MMIO_RANGES: usize = 128;
/// Maximum validated FDT size retained across BSS clear.
pub const MAX_FDT_SNAPSHOT_SIZE: usize = 2 * 1024 * 1024;

/// QEMU LoongArch virt publishes its generated DTB at this fixed physical
/// address when direct ELF boot omits the EFI system-table register handoff.
#[cfg(all(target_arch = "loongarch64", feature = "boot_la_qemu"))]
const LA64_QEMU_FDT_PADDR: usize = 0x0010_0000;

/// Static buffer for memory regions populated during early boot.
///
/// `populate_memory_regions()` writes here before `mm::init()`; frame allocation
/// reads the finalized data for the remainder of the kernel lifetime.
#[link_section = ".data.boot"]
pub static mut MEMORY_BUF: MemoryRegionBuf = MemoryRegionBuf::new();

/// Fixed-capacity FDT bytes retained before BSS clear.
///
/// The pre-heap boot path copies validated firmware bytes, then publishes
/// `len` as its final write. The snapshot is immutable for the remainder of
/// the kernel lifetime.
struct FdtSnapshot {
    bytes: [u8; MAX_FDT_SNAPSHOT_SIZE],
    len: usize,
}

impl FdtSnapshot {
    const fn new() -> Self {
        Self {
            bytes: [0; MAX_FDT_SNAPSHOT_SIZE],
            len: 0,
        }
    }
}

#[link_section = ".bss.boot"]
static mut FDT_SNAPSHOT: FdtSnapshot = FdtSnapshot::new();

/// Fixed-capacity buffer holding the FDT resources needed before allocation.
///
/// Populated by `populate_memory_regions()` from firmware data.
/// Read by `memory_regions()` and `firmware_reserved_regions()`.
pub struct MemoryRegionBuf {
    pub regions: [(usize, usize); MAX_MEMORY_REGIONS],
    pub reserved: [(usize, usize); MAX_FIRMWARE_RESERVED],
    pub mmio: [(usize, usize); MAX_EARLY_MMIO_RANGES],
    pub region_count: usize,
    pub reserved_count: usize,
    pub mmio_count: usize,
    /// FDT `/cpus` 探测到的逻辑 CPU 数量；0 表示 FDT 缺失 `/cpus` 或尚未初始化。
    pub cpu_count: usize,
    #[cfg(target_arch = "riscv64")]
    pub timebase_frequency: usize,
}

impl MemoryRegionBuf {
    pub const fn new() -> Self {
        Self {
            regions: [(0, 0); MAX_MEMORY_REGIONS],
            reserved: [(0, 0); MAX_FIRMWARE_RESERVED],
            mmio: [(0, 0); MAX_EARLY_MMIO_RANGES],
            region_count: 0,
            reserved_count: 0,
            mmio_count: 0,
            cpu_count: 0,
            #[cfg(target_arch = "riscv64")]
            timebase_frequency: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.region_count == 0
    }
}

/// Only the standard RV64 protocol provides an FDT in a1.
#[cfg(target_arch = "riscv64")]
fn has_valid_dtb() -> bool {
    let bi = boot::boot_info();
    matches!(bi.protocol, crate::hal::boot::BootProtocol::RiscvFdt)
        && bi.dtb_paddr != 0
        && bi.dtb_paddr & 0x3 == 0
}

/// Populate MEMORY_BUF from firmware data (FDT) or static fallback.
///
/// Called before `mem_clear()` and `mm::init()`.
/// Must NOT allocate — operates on raw bytes.
pub fn populate_memory_regions() {
    #[cfg(target_arch = "riscv64")]
    {
        if !has_valid_dtb() {
            panic!("RV64 boot requires an aligned FDT in a1");
        }
        let dtb_paddr = boot::boot_info().dtb_paddr;
        if fdt::capture_fdt_snapshot(dtb_paddr) && fdt::parse_memory_regions(dtb_paddr) {
            return;
        }
        panic!("RV64 boot FDT validation or pre-heap discovery failed");
    }
    #[cfg(target_arch = "loongarch64")]
    {
        // LoongArch 优先从 EFI system table 查找 FDT（设备自动探测是最终目标）；
        // 只有 boot_la_uboot_dmw 在 EFI 失败时才允许退回静态板级描述。
        let info = boot::boot_info();
        let fdt_result = efi::find_fdt(info.dtb_paddr).and_then(|dtb_paddr| {
            if fdt::capture_fdt_snapshot(dtb_paddr) && fdt::parse_memory_regions(dtb_paddr) {
                Ok(())
            } else {
                Err(efi::EfiFdtError::InvalidFdtBlob)
            }
        });
        if fdt_result.is_ok() {
            return;
        }

        #[cfg(feature = "boot_la_qemu")]
        if matches!(fdt_result, Err(efi::EfiFdtError::MissingSystemTable))
            && fdt::capture_fdt_snapshot(LA64_QEMU_FDT_PADDR)
            && fdt::parse_memory_regions(LA64_QEMU_FDT_PADDR)
        {
            crate::println!(
                "[firmware] QEMU direct boot omitted EFI handoff; using validated FDT at {:#x}",
                LA64_QEMU_FDT_PADDR
            );
            return;
        }

        #[cfg(feature = "boot_la_uboot_dmw")]
        {
            // 部分 2K1000 U-Boot 环境没有安装 EFI_FDT_GUID；现有静态板级
            // 内存保留区经过实板验证，因此允许显式退回这份保守描述。
            populate_from_static();
            crate::println!("[firmware] Using static memory configuration");
            return;
        }
        #[cfg(feature = "boot_la_qemu")]
        panic!(
            "LA64 QEMU requires a valid EFI/FDT handoff: {:?}",
            fdt_result
        );
    }
}

/// BSP 在清 BSS 前冻结固件资源的兼容入口（SMP 分支旧 API）。
/// 该路径不得分配内存或等待 AP。
pub fn discover_early_resources() {
    assert_eq!(crate::smp::cpu_id(), crate::smp::BOOT_CPU_ID);
    populate_memory_regions();
}

/// Return the active memory regions as a slice.
/// Called by `for_each_usable_frame_region()` in the frame allocator.
pub fn memory_regions() -> &'static [(usize, usize)] {
    // SAFETY: MEMORY_BUF is populated before mm::init() and never modified after.
    let buffer = unsafe { &*core::ptr::addr_of!(MEMORY_BUF) };
    if buffer.is_empty() {
        #[cfg(all(target_arch = "loongarch64", feature = "boot_la_uboot_dmw"))]
        return MEMORY_REGIONS_FALLBACK;
        #[cfg(not(all(target_arch = "loongarch64", feature = "boot_la_uboot_dmw")))]
        panic!("firmware memory resources were not initialized");
    }
    &buffer.regions[..buffer.region_count]
}

/// Return the active firmware-reserved regions as a slice.
pub fn firmware_reserved_regions() -> &'static [(usize, usize)] {
    // SAFETY: MEMORY_BUF is populated before mm::init() and never modified after.
    let buffer = unsafe { &*core::ptr::addr_of!(MEMORY_BUF) };
    if buffer.is_empty() {
        #[cfg(all(target_arch = "loongarch64", feature = "boot_la_uboot_dmw"))]
        return FIRMWARE_RESERVED_REGIONS_FALLBACK;
        #[cfg(not(all(target_arch = "loongarch64", feature = "boot_la_uboot_dmw")))]
        panic!("firmware reserved resources were not initialized");
    }
    &buffer.reserved[..buffer.reserved_count]
}

/// 遍历固件声明的可用整页 RAM，并额外排除调用者仍持有的物理区间。
///
/// 固件保留区可能未按页对齐，也可能与内核镜像等调用者排除区重叠。本函数在
/// 逐个 DRAM bank 内按页向外扩展排除区，并通过反复寻找下一个边界计算区间并集，
/// 因此不要求调用者预先排序或合并，也不需要在早期启动阶段分配临时 `Vec`。
pub fn for_each_usable_ram_range(
    additional_exclusions: &[(usize, usize)],
    mut visit: impl FnMut(usize, usize),
) {
    let page_size = crate::config::PAGE_SIZE;
    let page_mask = page_size - 1;
    let reserved = firmware_reserved_regions();
    let mut previous_region_end = 0usize;

    for &(raw_start, raw_end) in memory_regions() {
        assert!(raw_start < raw_end, "empty firmware memory region");
        assert!(
            raw_start >= previous_region_end,
            "firmware memory regions overlap or are unsorted"
        );
        previous_region_end = raw_end;

        let start = raw_start
            .checked_add(page_mask)
            .expect("firmware memory start alignment overflow")
            & !page_mask;
        let end = raw_end & !page_mask;
        let mut cursor = start.max(page_size);

        while cursor < end {
            let mut covering_end = cursor;
            let mut next_start = end;
            let mut next_end = end;

            for &(excluded_start, excluded_end) in
                reserved.iter().chain(additional_exclusions.iter())
            {
                assert!(
                    excluded_start < excluded_end,
                    "empty physical memory exclusion"
                );
                let excluded_start = excluded_start & !page_mask;
                let excluded_end = excluded_end
                    .checked_add(page_mask)
                    .expect("physical memory exclusion alignment overflow")
                    & !page_mask;
                if excluded_end <= cursor || excluded_start >= end {
                    continue;
                }
                if excluded_start <= cursor {
                    covering_end = covering_end.max(excluded_end.min(end));
                } else if excluded_start < next_start {
                    next_start = excluded_start;
                    next_end = excluded_end;
                } else if excluded_start == next_start {
                    next_end = next_end.max(excluded_end);
                }
            }

            if covering_end > cursor {
                cursor = covering_end;
                continue;
            }
            if cursor < next_start {
                visit(cursor, next_start.min(end));
            }
            if next_start >= end {
                break;
            }
            cursor = next_end.min(end);
        }
    }
}

/// Return FDT MMIO ranges which must be identity-mapped before drivers probe.
pub fn early_mmio_ranges() -> &'static [(usize, usize)] {
    // SAFETY: The pre-heap parser completes before the page-table constructor.
    let buffer = unsafe { &*core::ptr::addr_of!(MEMORY_BUF) };
    &buffer.mmio[..buffer.mmio_count]
}

/// Return the runtime FDT timebase frequency captured before timer setup.
///
/// LA64 不使用该入口：timer 频率由 CPUCFG 探测，见 `hal/arch/loongarch64/time.rs`。
#[cfg(target_arch = "riscv64")]
pub fn timebase_frequency() -> usize {
    // SAFETY: The pre-heap parser publishes this scalar before timer users run.
    let frequency = unsafe { (*core::ptr::addr_of!(MEMORY_BUF)).timebase_frequency };
    if frequency == 0 {
        panic!("firmware timebase frequency was not initialized");
    }
    frequency
}

/// 返回 FDT `/cpus` 探测到的逻辑 CPU 数量（Linux nr_cpu_ids 语义的探测源）。
///
/// 0 表示探测失败或尚未执行 `populate_memory_regions()`；调用方（`smp`）负责
/// 回退到编译期配置并截断到 `MAX_CPUS`。
pub fn cpu_count() -> usize {
    // SAFETY: The pre-heap parser publishes this scalar once before `mm::init()`
    // and never mutates it afterwards; all later access is read-only.
    unsafe { (*core::ptr::addr_of!(MEMORY_BUF)).cpu_count }
}

/// Sum discovered usable RAM ranges for runtime accounting.
pub fn usable_memory_size() -> usize {
    let mut total = 0usize;
    for_each_usable_ram_range(&[], |start, end| {
        total = total.saturating_add(end - start);
    });
    total
}

/// Fill MEMORY_BUF from compile-time constants (2K1000 板级回退)。
#[cfg(all(target_arch = "loongarch64", feature = "boot_la_uboot_dmw"))]
fn populate_from_static() {
    // SAFETY: This runs during single-threaded early boot before mm::init().
    let buffer = unsafe { &mut *core::ptr::addr_of_mut!(MEMORY_BUF) };
    buffer.region_count = 0;
    buffer.reserved_count = 0;
    buffer.mmio_count = 0;
    // 静态板级描述没有 FDT `/cpus`；保持 cpu_count=0，由 smp 回退到编译期配置。
    buffer.cpu_count = 0;

    for (index, &(start, end)) in MEMORY_REGIONS_FALLBACK.iter().enumerate() {
        if index >= MAX_MEMORY_REGIONS {
            break;
        }
        buffer.regions[index] = (start, end);
        buffer.region_count = index + 1;
    }
    for (index, &(start, end)) in FIRMWARE_RESERVED_REGIONS_FALLBACK.iter().enumerate() {
        if index >= MAX_FIRMWARE_RESERVED {
            break;
        }
        buffer.reserved[index] = (start, end);
        buffer.reserved_count = index + 1;
    }
}
