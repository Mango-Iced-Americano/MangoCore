use crate::fs::ext4::BLOCK_SIZE;
use alloc::sync::Arc;
use alloc::vec;
use core::ops::AddAssign;
use lazy_static::*;
use log::info;
use spin::Mutex;

use crate::drivers::block::BlockDevice;

/// 文件系统类型枚举。
///
/// `Null` 表示未检测到已知文件系统。
#[allow(unused, non_camel_case_types)]
#[derive(Debug, PartialEq, Eq)]
pub enum FS_Type {
    Null,
    Fat32,
    Ext4,
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

/// 读取块设备的第一个扇区（`BLOCK_SIZE` 字节），通过魔数检测文件系统类型。
///
/// 检测顺序：FAT32（`0x55AA` 在偏移 510）→ ext4（`0xEF53` 在偏移 1080）。
/// 均不匹配时返回 `FS_Type::Null`。
pub fn detect_fs(block_device: &Arc<dyn BlockDevice>) -> FS_Type {
    let mut buf = vec![0u8; BLOCK_SIZE];
    block_device.read_block(0, &mut buf);
    if buf[510] == 0x55 && buf[511] == 0xAA {
        info!("[fs] found fat32 filesystem");
        FS_Type::Fat32
    } else {
        let superblock_offset = 1024;
        let magic_number_high_index = superblock_offset + 56;
        let magic_number_low_index = superblock_offset + 57;
        let magic_number =
            u16::from_le_bytes([buf[magic_number_high_index], buf[magic_number_low_index]]);
        info!("[fs] read magic number: {}", magic_number);
        if magic_number == 0xEF53 {
            info!("[fs] found ext4 filesystem");
            FS_Type::Ext4
        } else {
            info!("[fs] no filesystem found");
            FS_Type::Null
        }
    }
}
