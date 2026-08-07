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
pub struct MemBlockDevice {
    data: Mutex<Vec<u8>>,
    size: u64,
}

impl MemBlockDevice {
    pub fn new(size_bytes: usize) -> Self {
        Self {
            data: Mutex::new(alloc::vec![0u8; size_bytes]),
            size: size_bytes as u64,
        }
    }

    /// 在测试发布并发任务前写入格式元数据。
    pub fn write_bytes(&self, offset: usize, bytes: &[u8]) -> Result<(), &'static str> {
        let mut data = self.data.lock();
        let end = offset
            .checked_add(bytes.len())
            .ok_or("mem block write range overflow")?;
        if end > data.len() {
            return Err("mem block write exceeds device capacity");
        }
        data[offset..end].copy_from_slice(bytes);
        Ok(())
    }
}

impl BlockDevice for MemBlockDevice {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) -> BlockDeviceResult {
        let offset = block_id * BLOCK_SZ;
        let data = self.data.lock();
        if offset >= data.len() {
            buf.fill(0);
            return Ok(());
        }
        let end = core::cmp::min(offset + buf.len(), data.len());
        buf[..end - offset].copy_from_slice(&data[offset..end]);
        buf[end - offset..].fill(0);
        Ok(())
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) -> BlockDeviceResult {
        let offset = block_id * BLOCK_SZ;
        let mut data = self.data.lock();
        if offset >= data.len() {
            return Ok(());
        }
        let end = core::cmp::min(offset + buf.len(), data.len());
        data[offset..end].copy_from_slice(&buf[..end - offset]);
        Ok(())
    }

    fn size_bytes(&self) -> Option<u64> {
        Some(self.size)
    }

    fn supports_reliable_flush(&self) -> bool {
        true
    }
}
