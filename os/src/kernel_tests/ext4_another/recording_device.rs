use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::convert::TryFrom;
use spin::Mutex;

use another_ext4::{Block, BlockDevice as AnotherBlockDevice, ErrCode, Ext4Error};

use crate::drivers::block::{
    validate_block_buffer_length, BlockDevice, BlockDeviceError, BlockDeviceResult,
};
use crate::hal::BLOCK_SZ;

const _: () = assert!(another_ext4::BLOCK_SIZE == BLOCK_SZ);

#[derive(Clone, Copy)]
pub(super) struct PhysicalRun {
    pub(super) start: u64,
    pub(super) blocks: usize,
}

struct RecordingState {
    enabled: bool,
    write_started: bool,
    read_after_write: bool,
    legacy_writes: usize,
    fail_legacy_write_after: Option<usize>,
    fail_mango_write_after: Option<usize>,
    runs: Vec<PhysicalRun>,
    mango_runs: Vec<PhysicalRun>,
}

impl RecordingState {
    fn new() -> Self {
        Self {
            enabled: false,
            write_started: false,
            read_after_write: false,
            legacy_writes: 0,
            fail_legacy_write_after: None,
            fail_mango_write_after: None,
            runs: Vec::new(),
            mango_runs: Vec::new(),
        }
    }
}

pub(super) struct RecordingSnapshot {
    pub(super) read_after_write: bool,
    pub(super) legacy_writes: usize,
    pub(super) runs: Vec<PhysicalRun>,
    pub(super) mango_runs: Vec<PhysicalRun>,
}

pub(super) struct RecordingBlockDevice {
    inner: Arc<dyn BlockDevice>,
    state: Mutex<RecordingState>,
}

impl RecordingBlockDevice {
    pub(super) fn new(inner: Arc<dyn BlockDevice>) -> Self {
        Self {
            inner,
            state: Mutex::new(RecordingState::new()),
        }
    }

    pub(super) fn start_recording(&self) {
        let mut state = self.state.lock();
        state.enabled = true;
        state.write_started = false;
        state.read_after_write = false;
        state.legacy_writes = 0;
        state.runs.clear();
        state.mango_runs.clear();
    }

    pub(super) fn finish_recording(&self) -> RecordingSnapshot {
        let mut state = self.state.lock();
        state.enabled = false;
        RecordingSnapshot {
            read_after_write: state.read_after_write,
            legacy_writes: state.legacy_writes,
            runs: core::mem::take(&mut state.runs),
            mango_runs: core::mem::take(&mut state.mango_runs),
        }
    }

    /// Fail exactly one subsequent legacy write after the requested successes.
    pub(super) fn fail_next_legacy_write_after(&self, successful_writes: usize) {
        self.state.lock().fail_legacy_write_after = Some(successful_writes);
    }

    /// Fail one `write_blocks()` call used by the Mango PageCache bridge.
    pub(super) fn fail_next_mango_write_after(&self, successful_writes: usize) {
        self.state.lock().fail_mango_write_after = Some(successful_writes);
    }

    fn block_index(block_id: u64) -> Result<usize, Ext4Error> {
        usize::try_from(block_id).map_err(|_| Ext4Error::new(ErrCode::EFBIG))
    }

    fn device_error(_: BlockDeviceError) -> Ext4Error {
        Ext4Error::new(ErrCode::EIO)
    }

    fn record_read(&self) {
        let mut state = self.state.lock();
        if state.enabled && state.write_started {
            state.read_after_write = true;
        }
    }

    fn record_legacy_write_or_fail(&self) -> bool {
        let mut state = self.state.lock();
        if let Some(remaining) = state.fail_legacy_write_after {
            if remaining == 0 {
                state.fail_legacy_write_after = None;
                return true;
            }
            state.fail_legacy_write_after = Some(remaining - 1);
        }
        if state.enabled {
            state.write_started = true;
            state.legacy_writes += 1;
        }
        false
    }

    fn record_run(&self, start: u64, blocks: usize) {
        let mut state = self.state.lock();
        if state.enabled {
            state.write_started = true;
            state.runs.push(PhysicalRun { start, blocks });
        }
    }

    fn record_mango_run(&self, start: u64, blocks: usize) {
        let mut state = self.state.lock();
        if state.enabled {
            state.write_started = true;
            state.mango_runs.push(PhysicalRun { start, blocks });
        }
    }

    fn record_mango_run_or_fail(&self, start: u64, blocks: usize) -> bool {
        let mut state = self.state.lock();
        if let Some(remaining) = state.fail_mango_write_after {
            if remaining == 0 {
                state.fail_mango_write_after = None;
                return true;
            }
            state.fail_mango_write_after = Some(remaining - 1);
        }
        if state.enabled {
            state.write_started = true;
            state.mango_runs.push(PhysicalRun { start, blocks });
        }
        false
    }
}

impl BlockDevice for RecordingBlockDevice {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) -> BlockDeviceResult {
        self.record_read();
        self.inner.read_block(block_id, buf)
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) -> BlockDeviceResult {
        validate_block_buffer_length(buf.len())?;
        let start = u64::try_from(block_id).map_err(|_| BlockDeviceError::OutOfBounds)?;
        if self.record_mango_run_or_fail(start, buf.len() / BLOCK_SZ) {
            return Err(BlockDeviceError::DeviceError);
        }
        self.inner.write_block(block_id, buf)
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

impl AnotherBlockDevice for RecordingBlockDevice {
    fn read_block(&self, block_id: u64) -> Result<Block, Ext4Error> {
        let block_index = Self::block_index(block_id)?;
        let mut image = Box::new([0; another_ext4::BLOCK_SIZE]);
        self.record_read();
        self.inner
            .read_block(block_index, &mut image[..])
            .map_err(Self::device_error)?;
        Ok(Block::new(block_id, image))
    }

    fn write_block(&self, block: &Block) -> Result<(), Ext4Error> {
        let block_index = Self::block_index(block.id)?;
        if self.record_legacy_write_or_fail() {
            return Err(Ext4Error::new(ErrCode::EIO));
        }
        self.inner
            .write_block(block_index, &block.data[..])
            .map_err(Self::device_error)
    }

    fn write_blocks(&self, start: u64, data: &[u8]) -> Result<(), Ext4Error> {
        if data.is_empty() || data.len() % another_ext4::BLOCK_SIZE != 0 {
            return Err(Ext4Error::new(ErrCode::EINVAL));
        }
        let block_index = Self::block_index(start)?;
        if self.record_mango_run_or_fail(start, data.len() / another_ext4::BLOCK_SIZE) {
            return Err(Ext4Error::new(ErrCode::EIO));
        }
        self.inner
            .write_block(block_index, data)
            .map_err(Self::device_error)
    }

    fn flush(&self) -> Result<(), Ext4Error> {
        self.inner.flush().map_err(Self::device_error)
    }

    fn supports_reliable_flush(&self) -> bool {
        self.inner.supports_reliable_flush()
    }
}
