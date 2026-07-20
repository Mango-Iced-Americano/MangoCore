use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use crate::drivers::block::{
    validate_block_buffer_length, BlockDevice, BlockDeviceError, BlockDeviceResult,
};
use crate::fs::vfs::FileSystem;
use crate::hal::BLOCK_SZ;

pub(super) struct ZeroBlockDevice;

impl BlockDevice for ZeroBlockDevice {
    fn read_block(&self, _block_id: usize, buf: &mut [u8]) -> BlockDeviceResult {
        validate_block_buffer_length(buf.len())?;
        buf.fill(0);
        Ok(())
    }

    fn write_block(&self, _block_id: usize, _buf: &[u8]) -> BlockDeviceResult {
        Err(BlockDeviceError::DeviceError)
    }
}

pub(super) struct FlushFailsAfterMountDevice {
    inner: Arc<dyn BlockDevice>,
    flush_count: AtomicUsize,
}

impl FlushFailsAfterMountDevice {
    pub(super) fn new(inner: Arc<dyn BlockDevice>) -> Self {
        Self {
            inner,
            flush_count: AtomicUsize::new(0),
        }
    }
}

impl BlockDevice for FlushFailsAfterMountDevice {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) -> BlockDeviceResult {
        self.inner.read_block(block_id, buf)
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) -> BlockDeviceResult {
        self.inner.write_block(block_id, buf)
    }

    fn flush(&self) -> BlockDeviceResult {
        if self.flush_count.fetch_add(1, Ordering::AcqRel) == 0 {
            self.inner.flush()
        } else {
            Err(BlockDeviceError::DeviceError)
        }
    }

    fn supports_reliable_flush(&self) -> bool {
        true
    }

    fn size_bytes(&self) -> Option<u64> {
        self.inner.size_bytes()
    }
}

/// Buffers writes until a device flush makes them visible to an unwrapped view.
pub(super) struct BarrierBlockDevice {
    inner: Arc<dyn BlockDevice>,
    pending: Mutex<BTreeMap<usize, [u8; BLOCK_SZ]>>,
}

impl BarrierBlockDevice {
    pub(super) fn new(inner: Arc<dyn BlockDevice>) -> Self {
        Self {
            inner,
            pending: Mutex::new(BTreeMap::new()),
        }
    }
}

impl BlockDevice for BarrierBlockDevice {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) -> BlockDeviceResult {
        validate_block_buffer_length(buf.len())?;
        for (offset, chunk) in buf.chunks_exact_mut(BLOCK_SZ).enumerate() {
            let current_block = block_id
                .checked_add(offset)
                .ok_or(BlockDeviceError::OutOfBounds)?;
            match self.pending.lock().get(&current_block).cloned() {
                Some(staged) => chunk.copy_from_slice(&staged),
                None => self.inner.read_block(current_block, chunk)?,
            }
        }
        Ok(())
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) -> BlockDeviceResult {
        validate_block_buffer_length(buf.len())?;
        let mut pending = self.pending.lock();
        for (offset, chunk) in buf.chunks_exact(BLOCK_SZ).enumerate() {
            let current_block = block_id
                .checked_add(offset)
                .ok_or(BlockDeviceError::OutOfBounds)?;
            let mut staged = [0; BLOCK_SZ];
            staged.copy_from_slice(chunk);
            pending.insert(current_block, staged);
        }
        Ok(())
    }

    fn flush(&self) -> BlockDeviceResult {
        let pending = core::mem::take(&mut *self.pending.lock());
        for (block_id, block) in pending {
            self.inner.write_block(block_id, &block)?;
        }
        self.inner.flush()
    }

    fn supports_reliable_flush(&self) -> bool {
        self.inner.supports_reliable_flush()
    }

    fn size_bytes(&self) -> Option<u64> {
        self.inner.size_bytes()
    }
}

pub(super) fn open_clean_media() -> Result<Arc<dyn FileSystem>, &'static str> {
    let device = crate::drivers::block::block_devices()[0]
        .clone()
        .ok_or("ktest requires a clean ext4 block device in slot 0")?;
    crate::fs::ext4_backend::open(device).map_err(|_| "clean ext4 image did not mount")
}
