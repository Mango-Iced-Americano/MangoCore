//! LoongArch EFI system table 的最小早期解析器。
//!
//! QEMU direct boot 与 2K1000 U-Boot 都把 EFI system table 放在 `a2`。
//! 这里只查找 `EFI_FDT_GUID`，不构造完整 EFI 对象，也不在建堆前分配内存。

use core::convert::TryFrom;

const EFI_SYSTEM_TABLE_SIGNATURE: u64 = 0x5453_5953_2049_4249;
const EFI_SYSTEM_TABLE_MIN_SIZE: usize = 120;
const CONFIG_TABLE_COUNT_OFFSET: usize = 104;
const CONFIG_TABLE_POINTER_OFFSET: usize = 112;
const CONFIG_TABLE_ENTRY_SIZE: usize = 24;
const MAX_CONFIG_TABLE_ENTRIES: usize = 32;

// EFI GUID 在内存中按前三个字段 little-endian、末 8 字节原序保存。
const EFI_FDT_GUID: [u8; 16] = [
    0xd5, 0x21, 0xb6, 0xb1, 0x9c, 0xf1, 0xa5, 0x41, 0x83, 0x0b, 0xd9, 0x15, 0x2c, 0x69, 0xaa, 0xe0,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EfiFdtError {
    MissingSystemTable,
    MisalignedSystemTable,
    SystemTableOutsideRam,
    BadSystemTableSignature,
    TooManyConfigTables,
    InvalidConfigTable,
    MissingFdt,
    InvalidFdtPointer,
    InvalidFdtBlob,
}

/// 去掉 LoongArch DMW 段号，只保留 CPU 实现支持的物理地址位。
fn normalize_paddr(address: usize) -> usize {
    address & ((1usize << crate::config::PALEN) - 1)
}

/// 固件指针只允许落在已知 DRAM 内，避免在早期阶段探测任意 MMIO。
pub(super) fn contains_firmware_range(address: usize, size: usize) -> bool {
    let Some(end) = address.checked_add(size) else {
        return false;
    };
    if size == 0 {
        return false;
    }

    // LA QEMU 把 EFI 表和 FDT 放在 256 MiB 低端启动 RAM；原有静态配置只
    // 描述 0x8000_0000 起的内核加载区，所以需单独承认这段固件 RAM。
    #[cfg(feature = "boot_la_qemu")]
    if end <= 0x1000_0000 {
        return true;
    }

    crate::config::MEMORY_REGIONS_FALLBACK
        .iter()
        .any(|&(start, region_end)| start <= address && end <= region_end)
}

fn read_bytes<const N: usize>(address: usize) -> Option<[u8; N]> {
    if !contains_firmware_range(address, N) {
        return None;
    }
    let mut bytes = [0; N];
    for (offset, byte) in bytes.iter_mut().enumerate() {
        // SAFETY: `contains_firmware_range` 已证明整个读取区间位于普通 DRAM，
        // 逐字节 volatile 读取不要求额外对齐，也不会触发 MMIO 副作用。
        *byte = unsafe { (address.checked_add(offset)? as *const u8).read_volatile() };
    }
    Some(bytes)
}

fn read_u64(address: usize) -> Option<u64> {
    Some(u64::from_le_bytes(read_bytes(address)?))
}

/// 从 EFI system table 的 configuration table 中定位 FDT 物理地址。
pub(super) fn find_fdt(system_table_arg: usize) -> Result<usize, EfiFdtError> {
    if system_table_arg == 0 {
        return Err(EfiFdtError::MissingSystemTable);
    }
    let system_table = normalize_paddr(system_table_arg);
    if system_table & 0x7 != 0 {
        return Err(EfiFdtError::MisalignedSystemTable);
    }
    if !contains_firmware_range(system_table, EFI_SYSTEM_TABLE_MIN_SIZE) {
        return Err(EfiFdtError::SystemTableOutsideRam);
    }
    if read_u64(system_table) != Some(EFI_SYSTEM_TABLE_SIGNATURE) {
        return Err(EfiFdtError::BadSystemTableSignature);
    }

    // QEMU 9.2 的 LoongArch direct-boot 表头字段并非完整 UEFI 实现；因此
    // 这里不依赖 revision/HeaderSize/CRC，只使用 ABI 中固定的 EFI64 字段，
    // 并以签名、可信 RAM、对齐和有界条目数建立读取边界。
    let count = read_u64(system_table + CONFIG_TABLE_COUNT_OFFSET)
        .and_then(|count| usize::try_from(count).ok())
        .ok_or(EfiFdtError::InvalidConfigTable)?;
    if count > MAX_CONFIG_TABLE_ENTRIES {
        return Err(EfiFdtError::TooManyConfigTables);
    }
    let table_bytes = count
        .checked_mul(CONFIG_TABLE_ENTRY_SIZE)
        .ok_or(EfiFdtError::InvalidConfigTable)?;
    let config_table = read_u64(system_table + CONFIG_TABLE_POINTER_OFFSET)
        .and_then(|address| usize::try_from(address).ok())
        .map(normalize_paddr)
        .ok_or(EfiFdtError::InvalidConfigTable)?;
    if config_table & 0x7 != 0 || !contains_firmware_range(config_table, table_bytes.max(1)) {
        return Err(EfiFdtError::InvalidConfigTable);
    }

    for index in 0..count {
        let entry = config_table
            .checked_add(index * CONFIG_TABLE_ENTRY_SIZE)
            .ok_or(EfiFdtError::InvalidConfigTable)?;
        if read_bytes::<16>(entry) != Some(EFI_FDT_GUID) {
            continue;
        }
        let fdt_paddr = read_u64(entry + 16)
            .and_then(|address| usize::try_from(address).ok())
            .map(normalize_paddr)
            .ok_or(EfiFdtError::InvalidFdtPointer)?;
        if fdt_paddr & 0x3 != 0 || !contains_firmware_range(fdt_paddr, 8) {
            return Err(EfiFdtError::InvalidFdtPointer);
        }
        return Ok(fdt_paddr);
    }
    Err(EfiFdtError::MissingFdt)
}
