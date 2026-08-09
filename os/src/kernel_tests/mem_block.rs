//! 零盘 ktest 的内存块设备 fixture。
//!
//! 提供可在任意测试组间复用的内存 `BlockDevice`：
//! `MemBlockDevice` 是 ext4/FAT32 零盘测试共用的有界块设备；测试可以先在任务
//! 尚未发布时按字节写入格式元数据，再通过生产 `BlockDevice` 接口并发访问。

use alloc::vec::Vec;
use spin::Mutex;

use crate::drivers::block::{BlockDevice, BlockDeviceResult};
use crate::hal::BLOCK_SZ;

/// 有界、零扩展的内存块设备；越界读返回 0，越界写被忽略。
///
/// 存储按 `BLOCK_SZ` 分块：FAT32 fixture 需要约 34 MiB 连续存储，而 buddy
/// allocator 会把 `Vec::new(size)` 的请求取整到下一个 2 的幂 —— 34 MiB → 64 MiB
/// （order-24），在 64 MiB kernel heap 上等于要求整堆为空，任何残留分配都会让
/// `HEAP ALLOCATION FAILED`。分块后每个 4 KiB 块独立分配，不再依赖大块连续内存。
pub struct MemBlockDevice {
    data: Mutex<Vec<Vec<u8>>>,
    size: u64,
}

impl MemBlockDevice {
    pub fn new(size_bytes: usize) -> Self {
        let nblocks = size_bytes.div_ceil(BLOCK_SZ);
        let mut data = Vec::with_capacity(nblocks);
        for _ in 0..nblocks {
            data.push(alloc::vec![0u8; BLOCK_SZ]);
        }
        Self {
            data: Mutex::new(data),
            size: size_bytes as u64,
        }
    }

    /// 在测试发布并发任务前写入格式元数据。
    pub fn write_bytes(&self, offset: usize, bytes: &[u8]) -> Result<(), &'static str> {
        let mut data = self.data.lock();
        let end = offset
            .checked_add(bytes.len())
            .ok_or("mem block write range overflow")?;
        if end > data.len() * BLOCK_SZ {
            return Err("mem block write exceeds device capacity");
        }
        let mut block_off = offset / BLOCK_SZ;
        let mut in_block = offset % BLOCK_SZ;
        let mut remaining = bytes;
        while !remaining.is_empty() {
            let block = &mut data[block_off];
            let take = (BLOCK_SZ - in_block).min(remaining.len());
            block[in_block..in_block + take].copy_from_slice(&remaining[..take]);
            remaining = &remaining[take..];
            block_off += 1;
            in_block = 0;
        }
        Ok(())
    }
}

impl BlockDevice for MemBlockDevice {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) -> BlockDeviceResult {
        let data = self.data.lock();
        let nblocks = buf.len() / BLOCK_SZ;
        for i in 0..nblocks {
            let cur = block_id + i;
            if cur >= data.len() {
                buf[i * BLOCK_SZ..(i + 1) * BLOCK_SZ].fill(0);
            } else {
                buf[i * BLOCK_SZ..(i + 1) * BLOCK_SZ].copy_from_slice(&data[cur]);
            }
        }
        Ok(())
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) -> BlockDeviceResult {
        let mut data = self.data.lock();
        if block_id >= data.len() {
            return Ok(());
        }
        let nblocks = buf.len() / BLOCK_SZ;
        for i in 0..nblocks {
            let cur = block_id + i;
            if cur >= data.len() {
                break;
            }
            data[cur].copy_from_slice(&buf[i * BLOCK_SZ..(i + 1) * BLOCK_SZ]);
        }
        Ok(())
    }

    fn size_bytes(&self) -> Option<u64> {
        Some(self.size)
    }

    fn supports_reliable_flush(&self) -> bool {
        true
    }
}
