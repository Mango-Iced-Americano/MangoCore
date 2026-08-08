//! In-memory `BlockDevice` fixtures used by topology-independent ext4 tests.

use alloc::vec::Vec;
#[cfg(feature = "ext4_lwext4_backend")]
use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use crate::drivers::block::{BlockDevice, BlockDeviceResult};
use crate::hal::BLOCK_SZ;

/// A bounded, zero-extending in-memory block device.
///
/// Only used by the lwext4 `open_unformatted_returns_err` test; the
/// unconditional memblk self-tests were removed (they OOM'd the 32MiB rv64
/// kernel heap). Gated so default builds don't report dead code.
#[cfg(feature = "ext4_lwext4_backend")]
struct TestMemBlock {
    data: Mutex<Vec<u8>>,
    size: u64,
}

#[cfg(feature = "ext4_lwext4_backend")]
impl TestMemBlock {
    fn new(size_bytes: usize) -> Self {
        Self {
            data: Mutex::new(alloc::vec![0u8; size_bytes]),
            size: size_bytes as u64,
        }
    }
}

#[cfg(feature = "ext4_lwext4_backend")]
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

#[cfg(feature = "ext4_lwext4_backend")]
pub(super) fn test_open_unformatted_returns_err() -> Result<(), &'static str> {
    use crate::fs::ext4_lwext4::ext4fs::Ext4FileSystem;

    // 4MiB fixture: 只需要一个全零设备让 open 失败，尺寸与结论无关；
    // 64MiB 会撑爆 rv64 的 32MiB 内核堆。
    match Ext4FileSystem::open_ext4rs(Arc::new(TestMemBlock::new(4 * 1024 * 1024))) {
        Ok(_) => Err("open_ext4rs should fail on an all-zero device"),
        Err(_) => Ok(()),
    }
}
