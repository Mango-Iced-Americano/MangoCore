//! In-memory `BlockDevice` fixtures used by topology-independent ext4 tests.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use crate::drivers::block::BlockDevice;
use crate::hal::BLOCK_SZ;

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
    fn read_block(&self, block_id: usize, buf: &mut [u8]) {
        assert_eq!(buf.len() % BLOCK_SIZE, 0);
        self.calls.lock().push((false, block_id, buf.len()));
        let start = block_id * BLOCK_SIZE;
        buf.copy_from_slice(&self.data.lock()[start..start + buf.len()]);
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) {
        assert_eq!(buf.len() % BLOCK_SIZE, 0);
        self.calls.lock().push((true, block_id, buf.len()));
        let start = block_id * BLOCK_SIZE;
        self.data.lock()[start..start + buf.len()].copy_from_slice(buf);
    }

    fn size_bytes(&self) -> Option<u64> {
        Some(self.data.lock().len() as u64)
    }

    fn flush(&self) -> Result<(), crate::utils::error::SyscallErr> {
        self.flushes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

pub(super) fn test_memblk_read_write() -> Result<(), &'static str> {
    let dev = Arc::new(crate::kernel_tests::mem_block::MemBlockDevice::new(64 * 1024 * 1024));
    let mut pattern = [0u8; BLOCK_SZ];
    for (index, byte) in pattern.iter_mut().enumerate() {
        *byte = (index % 256) as u8;
    }
    dev.write_block(0, &pattern);
    let mut actual = [0u8; BLOCK_SZ];
    dev.read_block(0, &mut actual);
    if actual != pattern {
        return Err("block 0: read data does not match written data");
    }

    let second = [0xabu8; BLOCK_SZ];
    dev.write_block(1, &second);
    dev.read_block(1, &mut actual);
    if actual != second {
        return Err("block 1: read data does not match written data");
    }
    dev.read_block(0, &mut actual);
    if actual != pattern {
        return Err("block 0: data corrupted after writing block 1");
    }
    Ok(())
}

pub(super) fn test_memblk_isolation() -> Result<(), &'static str> {
    let first = Arc::new(crate::kernel_tests::mem_block::MemBlockDevice::new(64 * 1024 * 1024));
    let second = Arc::new(crate::kernel_tests::mem_block::MemBlockDevice::new(64 * 1024 * 1024));
    let first_data = [0x11u8; BLOCK_SZ];
    let second_data = [0x22u8; BLOCK_SZ];
    first.write_block(0, &first_data);
    second.write_block(0, &second_data);

    let mut actual = [0u8; BLOCK_SZ];
    first.read_block(0, &mut actual);
    if actual != first_data {
        return Err("first device leaked data or lost its write");
    }
    second.read_block(0, &mut actual);
    if actual != second_data {
        return Err("second device leaked data or lost its write");
    }
    Ok(())
}

pub(super) fn test_open_unformatted_returns_err() -> Result<(), &'static str> {
    use crate::fs::ext4_lwext4::ext4fs::Ext4FileSystem;

    match Ext4FileSystem::open_ext4rs(Arc::new(crate::kernel_tests::mem_block::MemBlockDevice::new(64 * 1024 * 1024))) {
        Ok(_) => Err("open_ext4rs should fail on an all-zero device"),
        Err(_) => Ok(()),
    }
}
