use crate::fs::ext4::BLOCK_SIZE;
use alloc::sync::Arc;
use alloc::vec;
use core::ops::AddAssign;
use lazy_static::*;
use log::info;
use spin::Mutex;

use crate::config::PAGE_SIZE;
use crate::drivers::block::BlockDevice;
use crate::drivers::BLOCK_DEVICE;

/// 文件系统类型枚举。
///
/// `Null` 表示未检测到已知文件系统（或 `FORCE_RAMFS` 强制跳过块设备检测）。
#[allow(unused, non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FS_Type {
    Null,
    Fat32,
    Ext4,
}

/// Result of probing an on-disk filesystem, including its native block unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetectedFs {
    pub fs_type: FS_Type,
    pub block_size: usize,
}

/// 检测到的文件系统描述符。
///
/// `fs_id` 由全局自增计数器 `FS_ID_COUNTER` 分配，在当前启动周期内唯一。
/// `fs_type` 在构造后不可变。
#[derive(Debug)]
pub struct FileSystem {
    pub fs_id: usize,
    pub fs_type: FS_Type,
}

lazy_static! {
    static ref FS_ID_COUNTER: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
}

impl FileSystem {
    /// 分配一个新的文件系统描述符，`fs_id` 自动递增。
    pub fn new(fs_type: FS_Type) -> Self {
        FS_ID_COUNTER.lock().add_assign(1);
        let fs_id = *FS_ID_COUNTER.lock();
        Self { fs_id, fs_type }
    }
}

fn read_u16_le(buf: &[u8], offset: usize) -> Option<u16> {
    let bytes = buf.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32_le(buf: &[u8], offset: usize) -> Option<u32> {
    let bytes = buf.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// 校验 FAT32 BIOS 参数块，避免把同样以 0x55AA 结尾的 MBR 分区表误判为 FAT。
fn fat32_sector_size(buf: &[u8]) -> Option<usize> {
    if buf.len() < 512 || buf[510] != 0x55 || buf[511] != 0xaa {
        return None;
    }

    let jump_is_valid = (buf[0] == 0xeb && buf[2] == 0x90) || buf[0] == 0xe9;
    let bytes_per_sector = read_u16_le(buf, 11).unwrap_or(0) as usize;
    let sectors_per_cluster = buf[13];
    let reserved_sectors = read_u16_le(buf, 14).unwrap_or(0);
    let fat_count = buf[16];
    let root_entry_count = read_u16_le(buf, 17).unwrap_or(u16::MAX);
    let fat_size_16 = read_u16_le(buf, 22).unwrap_or(u16::MAX);
    let total_sectors_32 = read_u32_le(buf, 32).unwrap_or(0);
    let fat_size_32 = read_u32_le(buf, 36).unwrap_or(0);
    let root_cluster = read_u32_le(buf, 44).unwrap_or(0);

    let valid = jump_is_valid
        && matches!(bytes_per_sector, 512 | 1024 | 2048 | 4096)
        && bytes_per_sector <= PAGE_SIZE
        && PAGE_SIZE % bytes_per_sector == 0
        && sectors_per_cluster.is_power_of_two()
        && reserved_sectors != 0
        && matches!(fat_count, 1 | 2)
        && root_entry_count == 0
        && fat_size_16 == 0
        && total_sectors_32 != 0
        && fat_size_32 != 0
        && root_cluster >= 2;
    valid.then_some(bytes_per_sector)
}

/// 读取首个平台块并识别裸 ext4/FAT32 及其原生块大小。
pub fn detect_fs_layout(block_device: &Arc<dyn BlockDevice>) -> Option<DetectedFs> {
    let mut buf = vec![0u8; BLOCK_SIZE];
    block_device.read_block(0, &mut buf);
    let ext4_magic = read_u16_le(&buf, 1024 + 56).unwrap_or(0);
    if ext4_magic == 0xef53 {
        let log_block_size = read_u32_le(&buf, 1024 + 24).unwrap_or(u32::MAX);
        let block_size = 1024usize.checked_shl(log_block_size).unwrap_or(0);
        if block_size.is_power_of_two()
            && (1024..=PAGE_SIZE).contains(&block_size)
            && PAGE_SIZE % block_size == 0
        {
            info!("[fs] found ext4 filesystem, block_size={}", block_size);
            return Some(DetectedFs {
                fs_type: FS_Type::Ext4,
                block_size,
            });
        }
        info!(
            "[fs] ext4 magic found but block size {} is unsupported",
            block_size
        );
        return None;
    }
    if let Some(block_size) = fat32_sector_size(&buf) {
        info!("[fs] found fat32 filesystem, sector_size={}", block_size);
        return Some(DetectedFs {
            fs_type: FS_Type::Fat32,
            block_size,
        });
    }
    info!("[fs] no filesystem found");
    None
}

/// 兼容旧调用方的类型探测入口。
pub fn detect_fs(block_device: &Arc<dyn BlockDevice>) -> FS_Type {
    detect_fs_layout(block_device)
        .map(|detected| detected.fs_type)
        .unwrap_or(FS_Type::Null)
}

/// 挂载前的文件系统检测入口。
///
/// 若 `FORCE_RAMFS` 标志为 `true`，跳过块设备检测，直接返回 `FS_Type::Null`
///（由 ramfs 接管）。否则调用 `detect_fs(&BLOCK_DEVICE)`。
pub fn pre_mount() -> FS_Type {
    if super::FORCE_RAMFS.load(core::sync::atomic::Ordering::Relaxed) {
        println!("[fs] ramfs forced, skipping block device detection");
        return FS_Type::Null;
    }
    detect_fs(&BLOCK_DEVICE)
}
