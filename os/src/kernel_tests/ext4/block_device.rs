//! In-memory `BlockDevice` fixtures used by topology-independent ext4 tests.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use crate::drivers::block::{BlockDevice, BlockDeviceResult};
use crate::hal::BLOCK_SZ;

/// A bounded, zero-extending in-memory block device.
struct TestMemBlock {
    data: Mutex<Vec<u8>>,
    size: u64,
}

impl TestMemBlock {
    fn new(size_bytes: usize) -> Self {
        Self {
            data: Mutex::new(alloc::vec![0u8; size_bytes]),
            size: size_bytes as u64,
        }
    }
}

impl BlockDevice for TestMemBlock {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) -> BlockDeviceResult {
        let offset = block_id * BLOCK_SZ;
        let data = self.data.lock();
        if offset >= data.len() {
            buf.fill(0);
            return Ok(());
        }
        let end = core::cmp::min(offset + buf.len(), data.len());
        let copy_len = end - offset;
        buf[..copy_len].copy_from_slice(&data[offset..end]);
        buf[copy_len..].fill(0);
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
}

/// Records I/O requests so byte-bridge tests can assert batching semantics.
pub(super) struct RecordingMemBlock<const BLOCK_SIZE: usize> {
    data: Mutex<Vec<u8>>,
    calls: Mutex<Vec<(bool, usize, usize)>>,
    flushes: AtomicUsize,
}

impl<const BLOCK_SIZE: usize> RecordingMemBlock<BLOCK_SIZE> {
    pub(super) fn new(size_bytes: usize, fill: u8) -> Self {
        Self {
            data: Mutex::new(alloc::vec![fill; size_bytes]),
            calls: Mutex::new(Vec::new()),
            flushes: AtomicUsize::new(0),
        }
    }

    pub(super) fn snapshot(&self) -> Vec<u8> {
        self.data.lock().clone()
    }

    pub(super) fn take_calls(&self) -> Vec<(bool, usize, usize)> {
        core::mem::take(&mut *self.calls.lock())
    }

    pub(super) fn flush_count(&self) -> usize {
        self.flushes.load(Ordering::Relaxed)
    }
}

impl<const BLOCK_SIZE: usize> BlockDevice for RecordingMemBlock<BLOCK_SIZE> {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) -> BlockDeviceResult {
        assert_eq!(buf.len() % BLOCK_SIZE, 0);
        self.calls.lock().push((false, block_id, buf.len()));
        let start = block_id * BLOCK_SIZE;
        buf.copy_from_slice(&self.data.lock()[start..start + buf.len()]);
        Ok(())
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) -> BlockDeviceResult {
        assert_eq!(buf.len() % BLOCK_SIZE, 0);
        self.calls.lock().push((true, block_id, buf.len()));
        let start = block_id * BLOCK_SIZE;
        self.data.lock()[start..start + buf.len()].copy_from_slice(buf);
        Ok(())
    }

    fn size_bytes(&self) -> Option<u64> {
        Some(self.data.lock().len() as u64)
    }

    fn flush(&self) -> BlockDeviceResult {
        self.flushes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

pub(super) fn test_memblk_read_write() -> Result<(), &'static str> {
    let dev = Arc::new(TestMemBlock::new(64 * 1024 * 1024));
    let mut pattern = [0u8; BLOCK_SZ];
    for (index, byte) in pattern.iter_mut().enumerate() {
        *byte = (index % 256) as u8;
    }
    dev.write_block(0, &pattern)
        .map_err(|_| "block 0 write failed")?;
    let mut actual = [0u8; BLOCK_SZ];
    dev.read_block(0, &mut actual)
        .map_err(|_| "block 0 read failed")?;
    if actual != pattern {
        return Err("block 0: read data does not match written data");
    }

    let second = [0xabu8; BLOCK_SZ];
    dev.write_block(1, &second)
        .map_err(|_| "block 1 write failed")?;
    dev.read_block(1, &mut actual)
        .map_err(|_| "block 1 read failed")?;
    if actual != second {
        return Err("block 1: read data does not match written data");
    }
    dev.read_block(0, &mut actual)
        .map_err(|_| "block 0 reread failed")?;
    if actual != pattern {
        return Err("block 0: data corrupted after writing block 1");
    }
    Ok(())
}

pub(super) fn test_memblk_isolation() -> Result<(), &'static str> {
    let first = Arc::new(TestMemBlock::new(64 * 1024 * 1024));
    let second = Arc::new(TestMemBlock::new(64 * 1024 * 1024));
    let first_data = [0x11u8; BLOCK_SZ];
    let second_data = [0x22u8; BLOCK_SZ];
    first
        .write_block(0, &first_data)
        .map_err(|_| "first write failed")?;
    second
        .write_block(0, &second_data)
        .map_err(|_| "second write failed")?;

    let mut actual = [0u8; BLOCK_SZ];
    first
        .read_block(0, &mut actual)
        .map_err(|_| "first read failed")?;
    if actual != first_data {
        return Err("first device leaked data or lost its write");
    }
    second
        .read_block(0, &mut actual)
        .map_err(|_| "second read failed")?;
    if actual != second_data {
        return Err("second device leaked data or lost its write");
    }
    Ok(())
}

#[cfg(feature = "ext4_lwext4_backend")]
pub(super) fn test_open_unformatted_returns_err() -> Result<(), &'static str> {
    use crate::fs::ext4_lwext4::ext4fs::Ext4FileSystem;

    match Ext4FileSystem::open_ext4rs(Arc::new(TestMemBlock::new(64 * 1024 * 1024))) {
        Ok(_) => Err("open_ext4rs should fail on an all-zero device"),
        Err(_) => Ok(()),
    }
}
