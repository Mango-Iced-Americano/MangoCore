//! Loop block device — 将普通文件 inode 包装为块设备。
//!
//! 对标 Linux 的 loop 设备（`/dev/loop*`）。ktest 模式下用于把 initramfs
//! 内嵌的磁盘镜像文件包装为 `BlockDevice`，供文件系统挂载测试使用，
//! 避免在内存中分配大块 fixture（TestMemBlock 曾因 64MiB 堆分配触发 OOM）。

use alloc::sync::Arc;
use spin::Mutex;

use crate::fs::vfs::{FilePrivateData, IndexNode};
use crate::hal::BLOCK_SZ;

use super::{
    validate_block_buffer_length, BlockDevice, BlockDeviceError, BlockDeviceNameStyle,
    BlockDeviceResult,
};

/// 基于普通文件 inode 的 loop 块设备。
///
/// 底层 inode 可以是任意支持 `read_at`/`write_at`/`sync` 的实现
/// （initramfs 场景下为 ramfs inode），设备大小取自 inode 的 metadata。
pub struct LoopBlockDevice {
    inode: Arc<dyn IndexNode>,
    size: u64,
}

impl LoopBlockDevice {
    /// 用备份文件 inode 构造 loop 设备。
    ///
    /// 设备大小从 `inode.metadata()` 获取；metadata 失败时回退为 0。
    pub fn new(inode: Arc<dyn IndexNode>) -> Self {
        let size = inode
            .metadata()
            .map(|md| md.size.max(0) as u64)
            .unwrap_or(0);
        Self { inode, size }
    }
}

impl BlockDevice for LoopBlockDevice {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) -> BlockDeviceResult {
        validate_block_buffer_length(buf.len())?;
        let offset = block_id
            .checked_mul(BLOCK_SZ)
            .ok_or(BlockDeviceError::OutOfBounds)?;
        if offset as u64 >= self.size {
            return Err(BlockDeviceError::OutOfBounds);
        }
        let private = Mutex::new(FilePrivateData::Unused);
        let n = self
            .inode
            .read_at(offset, buf.len(), buf, private.lock())
            .map_err(|_| BlockDeviceError::DeviceError)?;
        if n < buf.len() {
            // 备份文件恰好为镜像大小，正常不应出现短读；此处零填充补齐。
            buf[n..].fill(0);
        }
        Ok(())
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) -> BlockDeviceResult {
        validate_block_buffer_length(buf.len())?;
        let offset = block_id
            .checked_mul(BLOCK_SZ)
            .ok_or(BlockDeviceError::OutOfBounds)?;
        if offset as u64 >= self.size {
            return Err(BlockDeviceError::OutOfBounds);
        }
        let private = Mutex::new(FilePrivateData::Unused);
        let n = self
            .inode
            .write_at(offset, buf.len(), buf, private.lock())
            .map_err(|_| BlockDeviceError::DeviceError)?;
        if n != buf.len() {
            return Err(BlockDeviceError::DeviceError);
        }
        Ok(())
    }

    fn flush(&self) -> BlockDeviceResult {
        self.inode.sync().map_err(|_| BlockDeviceError::DeviceError)
    }

    fn supports_reliable_flush(&self) -> bool {
        // ramfs 备份在内存中，flush 是无副作用的屏障。
        true
    }

    fn size_bytes(&self) -> Option<u64> {
        Some(self.size)
    }

    fn name_style(&self) -> BlockDeviceNameStyle {
        BlockDeviceNameStyle::Decimal("loop")
    }
}
