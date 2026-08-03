//! 固件资源发现层。
//!
//! 初始化分为两个阶段：BSP 在清 BSS、建堆前把 FDT 复制进 `.bss.boot`
//! 并解析固定容量的早期资源；堆可用后再构造拥有内存所有权的
//! [`crate::hal::platform::PlatformInfo`]。AP 只读取发布后的结果。

#[cfg(target_arch = "loongarch64")]
mod efi;
mod fdt;
#[cfg(all(target_arch = "loongarch64", feature = "board_2k1000"))]
mod static_provider;

use crate::hal::boot;
use core::mem::MaybeUninit;
#[cfg(all(target_arch = "loongarch64", feature = "board_2k1000"))]
use static_provider::{FIRMWARE_RESERVED_REGIONS_FALLBACK, MEMORY_REGIONS_FALLBACK};

pub const MAX_MEMORY_REGIONS: usize = 8;
pub const MAX_FIRMWARE_RESERVED: usize = 16;
pub const MAX_EARLY_MMIO_RANGES: usize = 128;
pub const MAX_FDT_SNAPSHOT_SIZE: usize = 2 * 1024 * 1024;

/// 建堆前固定容量的固件资源表；只允许 BSP 写一次。
#[link_section = ".data.boot"]
static mut MEMORY_BUF: MemoryRegionBuf = MemoryRegionBuf::new();

struct FdtSnapshotMeta {
    len: usize,
    source_paddr: usize,
}

impl FdtSnapshotMeta {
    const fn new() -> Self {
        Self {
            len: 0,
            source_paddr: 0,
        }
    }
}

/// 大缓冲放在 `sbss` 之前的 NOBITS 区，既躲过 `mem_clear()`，也不把 2 MiB
/// 零字节写进内核镜像。只有元数据需要真实的 `.data.boot` 初值。
#[link_section = ".bss.boot"]
static mut FDT_SNAPSHOT_BYTES: MaybeUninit<[u8; MAX_FDT_SNAPSHOT_SIZE]> = MaybeUninit::uninit();

/// 复制完成后才发布 `len`；BSP 在启动 AP 前不再修改该快照。
#[link_section = ".data.boot"]
static mut FDT_SNAPSHOT_META: FdtSnapshotMeta = FdtSnapshotMeta::new();

struct MemoryRegionBuf {
    regions: [(usize, usize); MAX_MEMORY_REGIONS],
    reserved: [(usize, usize); MAX_FIRMWARE_RESERVED],
    mmio: [(usize, usize); MAX_EARLY_MMIO_RANGES],
    region_count: usize,
    reserved_count: usize,
    mmio_count: usize,
    #[cfg(target_arch = "riscv64")]
    timebase_frequency: usize,
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
            #[cfg(target_arch = "riscv64")]
            timebase_frequency: 0,
        }
    }

    fn is_empty(&self) -> bool {
        self.region_count == 0
    }
}

/// BSP 在清 BSS 前冻结固件资源。该路径不得分配内存或等待 AP。
pub fn discover_early_resources() {
    assert_eq!(crate::smp::cpu_id(), crate::smp::BOOT_CPU_ID);
    #[cfg(target_arch = "riscv64")]
    {
        let info = boot::boot_info();
        let dtb_paddr = info.firmware_arg_paddr;
        if !matches!(info.protocol, crate::hal::boot::BootProtocol::RiscvFdt)
            || dtb_paddr == 0
            || dtb_paddr & 0x3 != 0
        {
            panic!("RV64 boot requires an aligned FDT in a1");
        }
        if fdt::capture_fdt_snapshot(dtb_paddr) && fdt::parse_early_resources() {
            return;
        }
        panic!("RV64 FDT validation or early resource discovery failed");
    }
    #[cfg(target_arch = "loongarch64")]
    {
        let info = boot::boot_info();
        let fdt_result = efi::find_fdt(info.firmware_arg_paddr).and_then(|dtb_paddr| {
            if fdt::capture_fdt_snapshot(dtb_paddr) && fdt::parse_early_resources() {
                Ok(())
            } else {
                Err(efi::EfiFdtError::InvalidFdtBlob)
            }
        });
        if fdt_result.is_ok() {
            return;
        }

        #[cfg(feature = "board_2k1000")]
        {
            // 部分 2K1000 U-Boot 环境没有安装 EFI_FDT_GUID；现有静态板级
            // 内存保留区经过实板验证，因此允许显式退回这份保守描述。
            populate_from_static();
            return;
        }
        #[cfg(feature = "board_laqemu")]
        panic!(
            "LA64 QEMU requires a valid EFI/FDT handoff: {:?}",
            fdt_result
        );
    }
}

/// 堆就绪后把早期固件快照转换为拥有所有权的平台描述。
pub(crate) fn build_platform_info() -> Option<crate::hal::platform::PlatformInfo> {
    fdt::build_platform_info()
}

pub fn memory_regions() -> &'static [(usize, usize)] {
    // Safety: BSP 在启动 AP、初始化 frame allocator 前完成唯一写入，之后只读。
    let buffer = unsafe { &*core::ptr::addr_of!(MEMORY_BUF) };
    if buffer.is_empty() {
        #[cfg(all(target_arch = "loongarch64", feature = "board_2k1000"))]
        return MEMORY_REGIONS_FALLBACK;
        #[cfg(not(all(target_arch = "loongarch64", feature = "board_2k1000")))]
        panic!("firmware memory resources were not initialized");
    }
    &buffer.regions[..buffer.region_count]
}

pub fn firmware_reserved_regions() -> &'static [(usize, usize)] {
    // Safety: 与 `memory_regions()` 相同，表在 AP 启动前已经冻结。
    let buffer = unsafe { &*core::ptr::addr_of!(MEMORY_BUF) };
    if buffer.is_empty() {
        #[cfg(all(target_arch = "loongarch64", feature = "board_2k1000"))]
        return FIRMWARE_RESERVED_REGIONS_FALLBACK;
        #[cfg(not(all(target_arch = "loongarch64", feature = "board_2k1000")))]
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

pub fn early_mmio_ranges() -> &'static [(usize, usize)] {
    // Safety: 页表构造者只读取 BSP 已冻结的早期表。
    let buffer = unsafe { &*core::ptr::addr_of!(MEMORY_BUF) };
    &buffer.mmio[..buffer.mmio_count]
}

#[cfg(target_arch = "riscv64")]
pub fn timebase_frequency() -> usize {
    // Safety: 标量与资源表在同一 BSP-only 阶段写入。
    let frequency = unsafe { (*core::ptr::addr_of!(MEMORY_BUF)).timebase_frequency };
    if frequency == 0 {
        panic!("firmware timebase frequency was not initialized");
    }
    frequency
}

pub fn usable_memory_size() -> usize {
    let mut total = 0usize;
    for_each_usable_ram_range(&[], |start, end| {
        total = total.saturating_add(end - start);
    });
    total
}

#[cfg(all(target_arch = "loongarch64", feature = "board_2k1000"))]
fn populate_from_static() {
    // Safety: 只有 BSP 在清 BSS 前调用，AP 尚未被启动。
    let buffer = unsafe { &mut *core::ptr::addr_of_mut!(MEMORY_BUF) };
    buffer.region_count = 0;
    buffer.reserved_count = 0;
    buffer.mmio_count = 0;

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
