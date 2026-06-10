use alloc::sync::Arc;
use alloc::vec::Vec;

use super::BlockDevice;
use crate::hal::BLOCK_SZ;

const SECTOR_SIZE: u64 = 512;
const LBA_PER_BLOCK: u64 = BLOCK_SZ as u64 / SECTOR_SIZE; // 8
const MBR_SIGNATURE_OFF: usize = 510;
const MBR_PART_TABLE_OFF: usize = 446;
const MBR_PART_ENTRY_SIZE: usize = 16;
const MBR_MAX_PRIMARY: usize = 4;

/// 从 MBR 分区表解析出的分区信息
#[derive(Debug, Clone)]
pub struct MbrPartition {
    pub partno: u8,        // 1-based
    pub type_code: u8,
    pub start_lba: u64,    // 512-byte sectors
    pub sectors: u64,      // 512-byte sectors
}

/// MBR 探测结果
pub enum MbrProbe {
    NoMbr,
    Unsupported,
    Partitions(Vec<MbrPartition>),
}

/// 解析 MBR 分区表。
/// 读取父设备的 block 0，检查 0x55AA 签名，解析 4 个主分区条目。
/// 只接受 4096 字节对齐的分区（start_lba % 8 == 0 && sectors % 8 == 0）。
pub fn probe_mbr(dev: &Arc<dyn BlockDevice>) -> MbrProbe {
    let mut buf = alloc::vec![0u8; BLOCK_SZ];
    dev.read_block(0, &mut buf);

    if buf[MBR_SIGNATURE_OFF] != 0x55 || buf[MBR_SIGNATURE_OFF + 1] != 0xAA {
        return MbrProbe::NoMbr;
    }

    let disk_sectors = dev.size_bytes().map(|b| b / SECTOR_SIZE);
    let mut saw_nonempty = false;
    let mut parts = Vec::new();

    for i in 0..MBR_MAX_PRIMARY {
        let off = MBR_PART_TABLE_OFF + i * MBR_PART_ENTRY_SIZE;
        let type_code = buf[off + 4];
        let start_lba = u32::from_le_bytes([
            buf[off + 8],
            buf[off + 9],
            buf[off + 10],
            buf[off + 11],
        ]) as u64;
        let sectors = u32::from_le_bytes([
            buf[off + 12],
            buf[off + 13],
            buf[off + 14],
            buf[off + 15],
        ]) as u64;

        // 跳过空条目
        if type_code == 0 || start_lba == 0 || sectors == 0 {
            continue;
        }
        saw_nonempty = true;

        // 跳过扩展分区
        if type_code == 0x05 || type_code == 0x0F || type_code == 0x85 {
            println!(
                "[mbr] skip partition {}: extended type {:#x} (unsupported)",
                i + 1,
                type_code
            );
            continue;
        }

        // 4096 字节对齐检查
        if start_lba % LBA_PER_BLOCK != 0 || sectors % LBA_PER_BLOCK != 0 {
            println!(
                "[mbr] skip partition {}: not 4096-aligned (start_lba={}, sectors={})",
                i + 1,
                start_lba,
                sectors
            );
            continue;
        }

        // 不溢出父设备
        if let Some(total) = disk_sectors {
            if start_lba.checked_add(sectors).map_or(true, |end| end > total) {
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
        MbrProbe::Partitions(parts)
    } else if saw_nonempty {
        MbrProbe::Unsupported
    } else {
        MbrProbe::NoMbr
    }
}

/// 基于父块设备的偏移视图的分区块设备。
/// 所有 read_block/write_block 请求都被转换为父设备上的偏移访问。
pub struct PartitionBlockDevice {
    parent: Arc<dyn BlockDevice>,
    start_block: usize,  // 父设备中的 4096 字节块偏移
    block_count: usize,  // 分区包含的 4096 字节块数
    size_bytes: u64,     // 分区精确字节大小，按 MBR 扇区数 * 512 计算
}

impl PartitionBlockDevice {
    pub fn new(
        parent: Arc<dyn BlockDevice>,
        start_lba: u64,
        sectors: u64,
    ) -> Self {
        let start_block = (start_lba / LBA_PER_BLOCK) as usize;
        let block_count = (sectors / LBA_PER_BLOCK) as usize;
        let size_bytes = sectors.saturating_mul(SECTOR_SIZE);
        Self {
            parent,
            start_block,
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
}

impl BlockDevice for PartitionBlockDevice {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) {
        let blocks = buf.len() / BLOCK_SZ;
        assert!(
            block_id.checked_add(blocks).map_or(true, |end| end <= self.block_count),
            "PartitionBlockDevice read OOB: block_id={}, blocks={}, block_count={}",
            block_id, blocks, self.block_count
        );
        self.parent.read_block(self.start_block + block_id, buf);
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) {
        let blocks = buf.len() / BLOCK_SZ;
        assert!(
            block_id.checked_add(blocks).map_or(true, |end| end <= self.block_count),
            "PartitionBlockDevice write OOB: block_id={}, blocks={}, block_count={}",
            block_id, blocks, self.block_count
        );
        self.parent.write_block(self.start_block + block_id, buf);
    }

    fn size_bytes(&self) -> Option<u64> {
        Some(self.size_bytes)
    }
}
