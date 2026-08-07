use core::any::Any;

use crate::hal::BLOCK_SZ;

/// Errors reported by persistent block-device operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockDeviceError {
    InvalidBufferLength,
    OutOfBounds,
    DeviceError,
    DeviceUnavailable,
    FlushUnsupported,
}

pub type BlockDeviceResult<T = ()> = Result<T, BlockDeviceError>;

/// Driver-announced disk naming convention.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum BlockDeviceNameStyle {
    Alphabetic(&'static str),
    Decimal(&'static str),
}

pub(crate) fn validate_block_buffer_length(buf_len: usize) -> BlockDeviceResult {
    if buf_len == 0 || buf_len % BLOCK_SZ != 0 {
        return Err(BlockDeviceError::InvalidBufferLength);
    }
    Ok(())
}

pub trait BlockDevice: Send + Sync + Any {
    /// Returns the driver-specific convention for naming its raw disks.
    fn name_style(&self) -> BlockDeviceNameStyle {
        BlockDeviceNameStyle::Decimal("blk")
    }

    /// Read one or more complete blocks starting at `block_id`.
    ///
    /// Returns [`BlockDeviceError::InvalidBufferLength`] unless `buf` is a
    /// non-empty multiple of [`BLOCK_SZ`].
    fn read_block(&self, block_id: usize, buf: &mut [u8]) -> BlockDeviceResult;

    /// Write one or more complete blocks starting at `block_id`.
    ///
    /// Returns [`BlockDeviceError::InvalidBufferLength`] unless `buf` is a
    /// non-empty multiple of [`BLOCK_SZ`].
    fn write_block(&self, block_id: usize, buf: &[u8]) -> BlockDeviceResult;

    /// Internal write entry while a byte-level RMW transaction is already locked.
    #[doc(hidden)]
    fn write_block_rmw_guarded(&self, block_id: usize, buf: &[u8]) -> BlockDeviceResult {
        self.write_block(block_id, buf)
    }

    /// Flush previously completed writes to persistent storage.
    ///
    /// A successful return guarantees durability only when
    /// [`Self::supports_reliable_flush`] is true. Devices without that
    /// capability must return [`BlockDeviceError::FlushUnsupported`].
    fn flush(&self) -> BlockDeviceResult {
        Err(BlockDeviceError::FlushUnsupported)
    }

    /// Whether [`Self::flush`] provides a reliable persistence barrier.
    fn supports_reliable_flush(&self) -> bool {
        false
    }

    /// 返回块设备的可用字节大小。
    /// 默认返回 None（未知大小）。有能力报告大小的驱动应 override。
    fn size_bytes(&self) -> Option<u64> {
        None
    }

    /// Clear one block with `num`.
    fn clear_block(&self, block_id: usize, num: u8) -> BlockDeviceResult {
        self.write_block(block_id, &[num; BLOCK_SZ])
    }

    /// Clear `cnt` blocks with `num`.
    fn clear_mult_block(&self, block_id: usize, cnt: usize, num: u8) -> BlockDeviceResult {
        for offset in 0..cnt {
            let current_block = block_id
                .checked_add(offset)
                .ok_or(BlockDeviceError::OutOfBounds)?;
            self.clear_block(current_block, num)?;
        }
        Ok(())
    }
}
