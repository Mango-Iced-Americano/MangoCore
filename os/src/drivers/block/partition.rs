use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use super::BlockDevice;
use crate::hal::BLOCK_SZ;

pub const LOGICAL_SECTOR_SIZE: u64 = 512;
const MBR_SIGNATURE_OFF: usize = 510;
const MBR_PART_TABLE_OFF: usize = 446;
const MBR_PART_ENTRY_SIZE: usize = 16;
const MBR_MAX_PRIMARY: usize = 4;

const _: () = assert!(BLOCK_SZ >= LOGICAL_SECTOR_SIZE as usize);
const _: () = assert!(BLOCK_SZ % LOGICAL_SECTOR_SIZE as usize == 0);

#[derive(Debug, Clone)]
pub struct MbrPartition {
    pub partno: u8,
    pub type_code: u8,
    pub start_lba: u64,
    pub sectors: u64,
}

impl MbrPartition {
    pub fn size_bytes(&self) -> u64 {
        self.sectors.saturating_mul(LOGICAL_SECTOR_SIZE)
    }
}

pub enum MbrProbe {
    NoMbr,
    Unsupported,
    Partitions(Vec<MbrPartition>),
}

fn read_u32_le(buf: &[u8], offset: usize) -> Option<u32> {
    let bytes = buf.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn partition_in_device(dev: &Arc<dyn BlockDevice>, start_lba: u64, sectors: u64) -> bool {
    if start_lba == 0 || sectors == 0 {
        return false;
    }
    let Some(end_lba) = start_lba.checked_add(sectors) else {
        return false;
    };
    dev.size_bytes()
        .map(|bytes| end_lba <= bytes / LOGICAL_SECTOR_SIZE)
        .unwrap_or(true)
}

fn checked_device_offset(
    block_id: usize,
    block_size: usize,
    len: usize,
    size_bytes: Option<u64>,
) -> u64 {
    assert!(block_size > 0, "logical block size must not be zero");
    assert_eq!(
        len % block_size,
        0,
        "I/O length must be a multiple of the logical block size"
    );
    let offset = (block_id as u64)
        .checked_mul(block_size as u64)
        .expect("block offset overflow");
    let end = offset
        .checked_add(len as u64)
        .expect("I/O length overflow");
    if let Some(size_bytes) = size_bytes {
        assert!(
            end <= size_bytes,
            "block I/O out of bounds: block_id={} block_size={} len={} size={}",
            block_id,
            block_size,
            len,
            size_bytes
        );
    }
    offset
}

fn read_parent_bytes(parent: &Arc<dyn BlockDevice>, absolute: u64, buf: &mut [u8]) {
    if buf.is_empty() {
        return;
    }
    if absolute % BLOCK_SZ as u64 == 0 && buf.len() % BLOCK_SZ == 0 {
        parent.read_block((absolute / BLOCK_SZ as u64) as usize, buf);
        return;
    }

    let mut bounce = vec![0u8; BLOCK_SZ];
    let mut done = 0usize;
    while done < buf.len() {
        let position = absolute + done as u64;
        let parent_block = (position / BLOCK_SZ as u64) as usize;
        let in_block = (position % BLOCK_SZ as u64) as usize;
        let copy_len = (BLOCK_SZ - in_block).min(buf.len() - done);
        parent.read_block(parent_block, &mut bounce);
        buf[done..done + copy_len].copy_from_slice(&bounce[in_block..in_block + copy_len]);
        done += copy_len;
    }
}

fn write_parent_bytes(parent: &Arc<dyn BlockDevice>, absolute: u64, buf: &[u8]) {
    if buf.is_empty() {
        return;
    }
    if absolute % BLOCK_SZ as u64 == 0 && buf.len() % BLOCK_SZ == 0 {
        parent.write_block((absolute / BLOCK_SZ as u64) as usize, buf);
        return;
    }

    let mut bounce = vec![0u8; BLOCK_SZ];
    let mut done = 0usize;
    while done < buf.len() {
        let position = absolute + done as u64;
        let parent_block = (position / BLOCK_SZ as u64) as usize;
        let in_block = (position % BLOCK_SZ as u64) as usize;
        let copy_len = (BLOCK_SZ - in_block).min(buf.len() - done);
        parent.read_block(parent_block, &mut bounce);
        bounce[in_block..in_block + copy_len].copy_from_slice(&buf[done..done + copy_len]);
        parent.write_block(parent_block, &bounce);
        done += copy_len;
    }
}

/// 解析传统 MBR 中的四个主分区项。
///
/// 扩展分区和 GPT 保护分区仅报告为不支持。本层保留分区表中的 512 字节 LBA，
/// 由 `PartitionBlockDevice` 转换到各平台的 `BLOCK_SZ`，不额外要求起点对齐。
pub fn probe_mbr(dev: &Arc<dyn BlockDevice>) -> MbrProbe {
    let mut sector = vec![0u8; BLOCK_SZ];
    dev.read_block(0, &mut sector);
    if sector[MBR_SIGNATURE_OFF] != 0x55 || sector[MBR_SIGNATURE_OFF + 1] != 0xaa {
        return MbrProbe::NoMbr;
    }

    let mut saw_nonempty = false;
    let mut saw_unsupported = false;
    let mut saw_protective_gpt = false;
    let mut partitions = Vec::new();
    for index in 0..MBR_MAX_PRIMARY {
        let offset = MBR_PART_TABLE_OFF + index * MBR_PART_ENTRY_SIZE;
        let entry = &sector[offset..offset + MBR_PART_ENTRY_SIZE];
        let boot_flag = entry[0];
        let type_code = entry[4];
        let start_lba = read_u32_le(entry, 8).unwrap_or(0) as u64;
        let sectors = read_u32_le(entry, 12).unwrap_or(0) as u64;
        if type_code == 0 || start_lba == 0 || sectors == 0 {
            continue;
        }
        saw_nonempty = true;

        if boot_flag != 0 && boot_flag != 0x80 {
            println!(
                "[mbr] skip partition {}: invalid boot flag {:#04x}",
                index + 1,
                boot_flag
            );
            saw_unsupported = true;
            continue;
        }
        if type_code == 0xee {
            println!("[mbr] protective GPT entry found; GPT is not supported yet");
            saw_unsupported = true;
            saw_protective_gpt = true;
            continue;
        }
        if matches!(type_code, 0x05 | 0x0f | 0x85) {
            println!(
                "[mbr] skip partition {}: extended type {:#04x} is unsupported",
                index + 1,
                type_code
            );
            saw_unsupported = true;
            continue;
        }
        if !partition_in_device(dev, start_lba, sectors) {
            println!(
                "[mbr] skip partition {}: out of disk range start={} sectors={}",
                index + 1,
                start_lba,
                sectors
            );
            saw_unsupported = true;
            continue;
        }

        partitions.push(MbrPartition {
            partno: (index + 1) as u8,
            type_code,
            start_lba,
            sectors,
        });
    }

    if saw_protective_gpt {
        // A hybrid protective MBR must not be partially interpreted as a normal MBR.
        MbrProbe::Unsupported
    } else if !partitions.is_empty() {
        println!(
            "[mbr] valid MBR with {} usable partition(s)",
            partitions.len()
        );
        MbrProbe::Partitions(partitions)
    } else if saw_nonempty || saw_unsupported {
        MbrProbe::Unsupported
    } else {
        MbrProbe::NoMbr
    }
}

/// MBR 分区的字节偏移视图。
///
/// MBR 使用 512 字节 LBA，而 MangoCore 各平台有独立的 `BLOCK_SZ`。未对齐分区
/// 通过 bounce buffer 访问；自然对齐的分区继续使用整块直接 I/O。
pub struct PartitionBlockDevice {
    parent: Arc<dyn BlockDevice>,
    start_lba: u64,
    sectors: u64,
    start_byte: u64,
    size_bytes: u64,
}

impl PartitionBlockDevice {
    pub fn new(parent: Arc<dyn BlockDevice>, start_lba: u64, sectors: u64) -> Self {
        let start_byte = start_lba
            .checked_mul(LOGICAL_SECTOR_SIZE)
            .expect("partition start byte overflow");
        let size_bytes = sectors
            .checked_mul(LOGICAL_SECTOR_SIZE)
            .expect("partition size overflow");
        assert!(size_bytes > 0, "partition must not be empty");
        if let Some(parent_size) = parent.size_bytes() {
            assert!(
                start_byte
                    .checked_add(size_bytes)
                    .is_some_and(|end| end <= parent_size),
                "partition exceeds parent device"
            );
        }
        Self {
            parent,
            start_lba,
            sectors,
            start_byte,
            size_bytes,
        }
    }

    pub fn start_lba(&self) -> u64 {
        self.start_lba
    }

    pub fn sectors(&self) -> u64 {
        self.sectors
    }

    pub fn block_count(&self) -> u64 {
        self.size_bytes / BLOCK_SZ as u64
    }

    fn absolute_offset(&self, block_id: usize, len: usize) -> u64 {
        let relative = checked_device_offset(block_id, BLOCK_SZ, len, Some(self.size_bytes));
        self.start_byte
            .checked_add(relative)
            .expect("partition absolute offset overflow")
    }
}

impl BlockDevice for PartitionBlockDevice {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) {
        let absolute = self.absolute_offset(block_id, buf.len());
        read_parent_bytes(&self.parent, absolute, buf);
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) {
        let absolute = self.absolute_offset(block_id, buf.len());
        write_parent_bytes(&self.parent, absolute, buf);
    }

    fn size_bytes(&self) -> Option<u64> {
        Some(self.size_bytes)
    }
}

/// Translate filesystem-native block numbers to the platform `BLOCK_SZ` unit.
///
/// ext4 block numbers and FAT sector numbers are expressed in their on-disk block size,
/// which can differ from the 2 KiB block unit used by the 2K1000 SATA wrapper.
pub struct BlockSizeAdapter {
    parent: Arc<dyn BlockDevice>,
    logical_block_size: usize,
    size_bytes: Option<u64>,
}

impl BlockSizeAdapter {
    pub fn new(parent: Arc<dyn BlockDevice>, logical_block_size: usize) -> Self {
        assert!(
            logical_block_size.is_power_of_two(),
            "filesystem block size must be a power of two"
        );
        let size_bytes = parent.size_bytes();
        Self {
            parent,
            logical_block_size,
            size_bytes,
        }
    }

    pub fn logical_block_size(&self) -> usize {
        self.logical_block_size
    }

    fn absolute_offset(&self, block_id: usize, len: usize) -> u64 {
        checked_device_offset(block_id, self.logical_block_size, len, self.size_bytes)
    }
}

impl BlockDevice for BlockSizeAdapter {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) {
        let absolute = self.absolute_offset(block_id, buf.len());
        read_parent_bytes(&self.parent, absolute, buf);
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) {
        let absolute = self.absolute_offset(block_id, buf.len());
        write_parent_bytes(&self.parent, absolute, buf);
    }

    fn size_bytes(&self) -> Option<u64> {
        self.size_bytes
    }
}

/// Last-resort physical write barrier for board read-only validation images.
///
/// `MountFlags::RDONLY` rejects VFS mutations, while this wrapper also catches internal
/// filesystem writeback paths that do not pass through `MountFSInode`.
pub struct ReadOnlyBlockDevice {
    parent: Arc<dyn BlockDevice>,
    blocked_writes: AtomicUsize,
}

impl ReadOnlyBlockDevice {
    pub fn new(parent: Arc<dyn BlockDevice>) -> Self {
        Self {
            parent,
            blocked_writes: AtomicUsize::new(0),
        }
    }
}

impl BlockDevice for ReadOnlyBlockDevice {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) {
        self.parent.read_block(block_id, buf);
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) {
        let count = self.blocked_writes.fetch_add(1, Ordering::Relaxed);
        if count < 4 {
            println!(
                "[block][ro] blocked internal write: block_id={} bytes={}",
                block_id,
                buf.len()
            );
        }
    }

    fn size_bytes(&self) -> Option<u64> {
        self.parent.size_bytes()
    }
}
