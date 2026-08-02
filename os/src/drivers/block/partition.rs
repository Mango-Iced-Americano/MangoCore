use alloc::sync::Arc;
use alloc::vec::Vec;

use core::convert::{TryFrom, TryInto};

use super::{validate_block_buffer_length, BlockDevice, BlockDeviceError, BlockDeviceResult};
use crate::hal::BLOCK_SZ;

const SECTOR_SIZE: u64 = 512;
const LBA_PER_BLOCK: u64 = BLOCK_SZ as u64 / SECTOR_SIZE; // 8
const MBR_SIGNATURE_OFF: usize = 510;
const MBR_PART_TABLE_OFF: usize = 446;
const MBR_PART_ENTRY_SIZE: usize = 16;
const MBR_MAX_PRIMARY: usize = 4;
const GPT_HEADER_LBA: u64 = 1;
const GPT_HEADER_SIZE: usize = SECTOR_SIZE as usize;
const GPT_SIGNATURE: [u8; 8] = *b"EFI PART";
const GPT_PARTITION_ARRAY_LBA_OFF: usize = 72;
const GPT_ENTRY_COUNT_OFF: usize = 80;
const GPT_ENTRY_SIZE_OFF: usize = 84;
const GPT_ENTRY_TYPE_GUID_SIZE: usize = 16;
const GPT_ENTRY_FIRST_LBA_OFF: usize = 32;
const GPT_ENTRY_LAST_LBA_OFF: usize = 40;
const GPT_MIN_ENTRY_SIZE: usize = 128;
const GPT_MAX_ENTRY_SIZE: usize = 512;
const GPT_MAX_ENTRIES: u32 = u8::MAX as u32;

/// 从 MBR 分区表解析出的分区信息
#[derive(Debug, Clone)]
pub struct MbrPartition {
    pub partno: u8, // 1-based
    pub type_code: u8,
    pub start_lba: u64, // 512-byte sectors
    pub sectors: u64,   // 512-byte sectors
}

/// MBR 探测结果
pub enum MbrProbe {
    NoMbr,
    Unsupported,
    Partitions(Vec<MbrPartition>),
}

fn read_device_bytes(
    dev: &Arc<dyn BlockDevice>,
    offset: u64,
    len: usize,
) -> BlockDeviceResult<Vec<u8>> {
    let len_u64 = u64::try_from(len).map_err(|_| BlockDeviceError::OutOfBounds)?;
    let end = offset
        .checked_add(len_u64)
        .ok_or(BlockDeviceError::OutOfBounds)?;
    let mut bytes = alloc::vec![0u8; len];
    let mut block = alloc::vec![0u8; BLOCK_SZ];
    let mut copied = 0usize;
    let mut current_offset = offset;

    while current_offset < end {
        let block_id = usize::try_from(current_offset / BLOCK_SZ as u64)
            .map_err(|_| BlockDeviceError::OutOfBounds)?;
        let block_offset = usize::try_from(current_offset % BLOCK_SZ as u64)
            .map_err(|_| BlockDeviceError::OutOfBounds)?;
        dev.read_block(block_id, &mut block)?;

        let available = BLOCK_SZ - block_offset;
        let remaining = len - copied;
        let copy_len = available.min(remaining);
        bytes[copied..copied + copy_len]
            .copy_from_slice(&block[block_offset..block_offset + copy_len]);
        copied += copy_len;
        current_offset = current_offset
            .checked_add(u64::try_from(copy_len).map_err(|_| BlockDeviceError::OutOfBounds)?)
            .ok_or(BlockDeviceError::OutOfBounds)?;
    }

    Ok(bytes)
}

fn parse_gpt_partitions(
    dev: &Arc<dyn BlockDevice>,
    disk_sectors: Option<u64>,
) -> BlockDeviceResult<Option<Vec<MbrPartition>>> {
    let header_offset = GPT_HEADER_LBA * SECTOR_SIZE;
    let header = read_device_bytes(dev, header_offset, GPT_HEADER_SIZE)?;
    if header[..GPT_SIGNATURE.len()] != GPT_SIGNATURE {
        return Ok(None);
    }

    let array_lba = u64::from_le_bytes(
        header[GPT_PARTITION_ARRAY_LBA_OFF..GPT_PARTITION_ARRAY_LBA_OFF + 8]
            .try_into()
            .map_err(|_| BlockDeviceError::DeviceError)?,
    );
    let entry_count = u32::from_le_bytes(
        header[GPT_ENTRY_COUNT_OFF..GPT_ENTRY_COUNT_OFF + 4]
            .try_into()
            .map_err(|_| BlockDeviceError::DeviceError)?,
    );
    let entry_size = u32::from_le_bytes(
        header[GPT_ENTRY_SIZE_OFF..GPT_ENTRY_SIZE_OFF + 4]
            .try_into()
            .map_err(|_| BlockDeviceError::DeviceError)?,
    );
    let entry_size = usize::try_from(entry_size).map_err(|_| BlockDeviceError::OutOfBounds)?;
    if entry_count == 0
        || entry_count > GPT_MAX_ENTRIES
        || !(GPT_MIN_ENTRY_SIZE..=GPT_MAX_ENTRY_SIZE).contains(&entry_size)
    {
        return Ok(Some(Vec::new()));
    }

    let array_offset = array_lba
        .checked_mul(SECTOR_SIZE)
        .ok_or(BlockDeviceError::OutOfBounds)?;
    let array_len = usize::try_from(entry_count)
        .ok()
        .and_then(|count| count.checked_mul(entry_size))
        .ok_or(BlockDeviceError::OutOfBounds)?;
    let entries = read_device_bytes(dev, array_offset, array_len)?;
    let mut parts = Vec::new();

    for index in 0..usize::try_from(entry_count).map_err(|_| BlockDeviceError::OutOfBounds)? {
        let entry_offset = index * entry_size;
        let entry = &entries[entry_offset..entry_offset + entry_size];
        if entry[..GPT_ENTRY_TYPE_GUID_SIZE].iter().all(|byte| *byte == 0) {
            continue;
        }

        let first_lba = u64::from_le_bytes(
            entry[GPT_ENTRY_FIRST_LBA_OFF..GPT_ENTRY_FIRST_LBA_OFF + 8]
                .try_into()
                .map_err(|_| BlockDeviceError::DeviceError)?,
        );
        let last_lba = u64::from_le_bytes(
            entry[GPT_ENTRY_LAST_LBA_OFF..GPT_ENTRY_LAST_LBA_OFF + 8]
                .try_into()
                .map_err(|_| BlockDeviceError::DeviceError)?,
        );
        let Some(sectors) = last_lba
            .checked_sub(first_lba)
            .and_then(|length| length.checked_add(1))
        else {
            continue;
        };
        if disk_sectors.is_some_and(|total| last_lba >= total) {
            continue;
        }

        parts.push(MbrPartition {
            partno: u8::try_from(index + 1).map_err(|_| BlockDeviceError::OutOfBounds)?,
            type_code: 0xEE,
            start_lba: first_lba,
            sectors,
        });
    }

    Ok(Some(parts))
}

/// 解析 MBR 分区表。
/// 读取父设备的 block 0，检查 0x55AA 签名，解析 4 个主分区条目。
pub fn probe_mbr(dev: &Arc<dyn BlockDevice>) -> BlockDeviceResult<MbrProbe> {
    let mut buf = alloc::vec![0u8; BLOCK_SZ];
    dev.read_block(0, &mut buf)?;

    if buf[MBR_SIGNATURE_OFF] != 0x55 || buf[MBR_SIGNATURE_OFF + 1] != 0xAA {
        return Ok(MbrProbe::NoMbr);
    }

    let disk_sectors = dev.size_bytes().map(|b| b / SECTOR_SIZE);
    let has_protective_mbr = (0..MBR_MAX_PRIMARY).any(|i| {
        buf[MBR_PART_TABLE_OFF + i * MBR_PART_ENTRY_SIZE + 4] == 0xEE
    });
    if has_protective_mbr {
        if let Some(parts) = parse_gpt_partitions(dev, disk_sectors)? {
            return Ok(MbrProbe::Partitions(parts));
        }
    }

    let mut saw_nonempty = false;
    let mut parts = Vec::new();

    for i in 0..MBR_MAX_PRIMARY {
        let off = MBR_PART_TABLE_OFF + i * MBR_PART_ENTRY_SIZE;
        let type_code = buf[off + 4];
        let start_lba =
            u32::from_le_bytes([buf[off + 8], buf[off + 9], buf[off + 10], buf[off + 11]]) as u64;
        let sectors =
            u32::from_le_bytes([buf[off + 12], buf[off + 13], buf[off + 14], buf[off + 15]]) as u64;

        // 跳过空条目
        if type_code == 0 || start_lba == 0 || sectors == 0 {
            continue;
        }
        saw_nonempty = true;

        // 跳过扩展分区
        if type_code == 0x05 || type_code == 0x0F || type_code == 0x85 || type_code == 0xEE {
            println!(
                "[mbr] skip partition {}: extended type {:#x} (unsupported)",
                i + 1,
                type_code
            );
            continue;
        }

        // 不溢出父设备
        if let Some(total) = disk_sectors {
            if start_lba
                .checked_add(sectors)
                .map_or(true, |end| end > total)
            {
                println!(
                    "[mbr] skip partition {}: out of disk range (start={}, sectors={}, disk_sectors={})",
                    i + 1,
                    start_lba,
                    sectors,
                    total
                );
                continue;
            }
        }

        parts.push(MbrPartition {
            partno: (i + 1) as u8,
            type_code,
            start_lba,
            sectors,
        });
    }

    if !parts.is_empty() {
        Ok(MbrProbe::Partitions(parts))
    } else if saw_nonempty {
        Ok(MbrProbe::Unsupported)
    } else {
        Ok(MbrProbe::NoMbr)
    }
}

/// 基于父块设备的偏移视图的分区块设备。
/// 所有 read_block/write_block 请求都被转换为父设备上的偏移访问。
pub struct PartitionBlockDevice {
    parent: Arc<dyn BlockDevice>,
    start_block: usize, // 父设备中的 4096 字节块偏移
    start_offset: usize, // start_block 内的 512 字节扇区偏移
    block_count: usize, // 分区包含的 4096 字节块数
    size_bytes: u64,    // 分区精确字节大小，按 MBR 扇区数 * 512 计算
}

impl PartitionBlockDevice {
    pub fn new(parent: Arc<dyn BlockDevice>, start_lba: u64, sectors: u64) -> Self {
        let start_block = (start_lba / LBA_PER_BLOCK) as usize;
        let start_offset = ((start_lba % LBA_PER_BLOCK) * SECTOR_SIZE) as usize;
        let block_count = (sectors / LBA_PER_BLOCK) as usize;
        let size_bytes = sectors.saturating_mul(SECTOR_SIZE);
        Self {
            parent,
            start_block,
            start_offset,
            block_count,
            size_bytes,
        }
    }

    pub fn start_block(&self) -> usize {
        self.start_block
    }

    pub fn block_count(&self) -> usize {
        self.block_count
    }

    fn parent_block_id(&self, block_id: usize, buf_len: usize) -> BlockDeviceResult<usize> {
        validate_block_buffer_length(buf_len)?;
        let blocks = buf_len / BLOCK_SZ;
        let end_block = block_id
            .checked_add(blocks)
            .ok_or(BlockDeviceError::OutOfBounds)?;
        if end_block > self.block_count {
            return Err(BlockDeviceError::OutOfBounds);
        }
        self.start_block
            .checked_add(block_id)
            .ok_or(BlockDeviceError::OutOfBounds)
    }

    fn read_unaligned(&self, parent_block_id: usize, buf: &mut [u8]) -> BlockDeviceResult {
        let parent_blocks = buf.len() / BLOCK_SZ + 1;
        let mut bounce = alloc::vec![0u8; parent_blocks * BLOCK_SZ];
        self.parent.read_block(parent_block_id, &mut bounce)?;
        let end = self.start_offset + buf.len();
        buf.copy_from_slice(&bounce[self.start_offset..end]);
        Ok(())
    }

    fn write_unaligned(&self, parent_block_id: usize, buf: &[u8]) -> BlockDeviceResult {
        let parent_blocks = buf.len() / BLOCK_SZ + 1;
        let mut bounce = alloc::vec![0u8; parent_blocks * BLOCK_SZ];
        self.parent.read_block(parent_block_id, &mut bounce)?;
        let end = self.start_offset + buf.len();
        bounce[self.start_offset..end].copy_from_slice(buf);
        self.parent.write_block(parent_block_id, &bounce)
    }
}

// ── BlockSizeAdapter ────────────────────────────────────────────────────

/// Adapts a block device's native block size to a smaller logical block size.
///
/// The child block size must evenly divide the parent's native block size.
/// Read-modify-write is used for sub-BLOCK_SZ writes, so this adapter is
/// suitable for test setup and small I/O but not for performance paths.
pub struct BlockSizeAdapter {
    parent: Arc<dyn BlockDevice>,
    child_size: usize,
    child_per_parent: usize,
}

impl BlockSizeAdapter {
    pub fn new(parent: Arc<dyn BlockDevice>, child_size: usize) -> Self {
        assert!(
            child_size > 0 && BLOCK_SZ % child_size == 0,
            "BlockSizeAdapter: child_size {} must be a positive divisor of BLOCK_SZ {}",
            child_size,
            BLOCK_SZ,
        );
        Self {
            parent,
            child_size,
            child_per_parent: BLOCK_SZ / child_size,
        }
    }

    fn parent_for_child(&self, child_id: usize) -> (usize, usize) {
        let parent_block = child_id / self.child_per_parent;
        let offset_in_parent = (child_id % self.child_per_parent) * self.child_size;
        (parent_block, offset_in_parent)
    }
}

impl BlockDevice for BlockSizeAdapter {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) -> BlockDeviceResult {
        if buf.len() != self.child_size {
            return Err(BlockDeviceError::InvalidBufferLength);
        }
        let (parent_block, offset_in_parent) = self.parent_for_child(block_id);
        let mut tmp = alloc::vec![0u8; BLOCK_SZ];
        self.parent.read_block(parent_block, &mut tmp)?;
        buf.copy_from_slice(&tmp[offset_in_parent..offset_in_parent + self.child_size]);
        Ok(())
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) -> BlockDeviceResult {
        if buf.len() != self.child_size {
            return Err(BlockDeviceError::InvalidBufferLength);
        }
        let (parent_block, offset_in_parent) = self.parent_for_child(block_id);
        let mut tmp = alloc::vec![0u8; BLOCK_SZ];
        self.parent.read_block(parent_block, &mut tmp)?;
        tmp[offset_in_parent..offset_in_parent + self.child_size].copy_from_slice(buf);
        self.parent.write_block(parent_block, &tmp)
    }

    fn flush(&self) -> BlockDeviceResult {
        self.parent.flush()
    }

    fn supports_reliable_flush(&self) -> bool {
        self.parent.supports_reliable_flush()
    }

    fn size_bytes(&self) -> Option<u64> {
        self.parent.size_bytes()
    }
}

// ── ReadOnlyBlockDevice ─────────────────────────────────────────────────

/// Wraps a `BlockDevice` to reject all write operations.
///
/// Reads, flush, and metadata queries are forwarded to the inner device.
pub struct ReadOnlyBlockDevice {
    inner: Arc<dyn BlockDevice>,
}

impl ReadOnlyBlockDevice {
    pub fn new(inner: Arc<dyn BlockDevice>) -> Self {
        Self { inner }
    }
}

impl BlockDevice for ReadOnlyBlockDevice {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) -> BlockDeviceResult {
        self.inner.read_block(block_id, buf)
    }

    fn write_block(&self, _block_id: usize, _buf: &[u8]) -> BlockDeviceResult {
        Err(BlockDeviceError::DeviceError)
    }

    fn flush(&self) -> BlockDeviceResult {
        self.inner.flush()
    }

    fn supports_reliable_flush(&self) -> bool {
        self.inner.supports_reliable_flush()
    }

    fn size_bytes(&self) -> Option<u64> {
        self.inner.size_bytes()
    }
}

// ── PartitionBlockDevice BlockDevice impl ───────────────────────────────

impl BlockDevice for PartitionBlockDevice {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) -> BlockDeviceResult {
        let parent_block_id = self.parent_block_id(block_id, buf.len())?;
        if self.start_offset == 0 {
            self.parent.read_block(parent_block_id, buf)
        } else {
            self.read_unaligned(parent_block_id, buf)
        }
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) -> BlockDeviceResult {
        let parent_block_id = self.parent_block_id(block_id, buf.len())?;
        if self.start_offset == 0 {
            self.parent.write_block(parent_block_id, buf)
        } else {
            self.write_unaligned(parent_block_id, buf)
        }
    }

    fn flush(&self) -> BlockDeviceResult {
        if !self.parent.supports_reliable_flush() {
            return Err(BlockDeviceError::FlushUnsupported);
        }
        self.parent.flush()
    }

    fn supports_reliable_flush(&self) -> bool {
        self.parent.supports_reliable_flush()
    }

    fn size_bytes(&self) -> Option<u64> {
        Some(self.size_bytes)
    }
}
