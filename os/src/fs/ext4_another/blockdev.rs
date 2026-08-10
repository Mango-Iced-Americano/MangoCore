use alloc::boxed::Box;
use alloc::sync::Arc;
use core::convert::TryFrom;

use crate::drivers::block::BlockDevice;
use crate::task::perf;

use super::errno::from_block_device;

const _: () = assert!(another_ext4::BLOCK_SIZE == crate::hal::BLOCK_SZ);

/// Adapts Mango's fallible block device contract to another_ext4.
pub(crate) struct MangoBlockDevice {
    device: Arc<dyn BlockDevice>,
}

impl MangoBlockDevice {
    pub(crate) fn new(device: Arc<dyn BlockDevice>) -> Self {
        Self { device }
    }

    fn block_index(block_id: u64) -> Result<usize, another_ext4::Ext4Error> {
        usize::try_from(block_id)
            .map_err(|_| another_ext4::Ext4Error::new(another_ext4::ErrCode::EFBIG))
    }
}

impl another_ext4::BlockDevice for MangoBlockDevice {
    fn read_block(&self, block_id: u64) -> Result<another_ext4::Block, another_ext4::Ext4Error> {
        let block_index = Self::block_index(block_id)?;
        let mut data = Box::new([0; another_ext4::BLOCK_SIZE]);
        self.device
            .read_block(block_index, &mut data[..])
            .map_err(|error| {
                log::error!(
                    "[ext4_another] READ FAILED: block_id={} mango_error={:?}",
                    block_id,
                    error,
                );
                another_ext4::Ext4Error::new(from_block_device(error))
            })?;
        Ok(another_ext4::Block::new(block_id, data))
    }

    fn write_block(&self, block: &another_ext4::Block) -> Result<(), another_ext4::Ext4Error> {
        let block_index = Self::block_index(block.id)?;
        self.device
            .write_block(block_index, &block.data[..])
            .map_err(|error| {
                log::error!(
                    "[ext4_another] WRITE FAILED: block_id={} mango_error={:?}",
                    block.id,
                    error,
                );
                another_ext4::Ext4Error::new(from_block_device(error))
            })
    }

    fn read_blocks(&self, start: u64, buf: &mut [u8]) -> Result<(), another_ext4::Ext4Error> {
        if buf.is_empty() {
            return Ok(());
        }
        if buf.len() % another_ext4::BLOCK_SIZE != 0 {
            return Err(another_ext4::Ext4Error::new(another_ext4::ErrCode::EINVAL));
        }
        let block_index = Self::block_index(start)?;
        self.device
            .read_block(block_index, buf)
            .map_err(|error| another_ext4::Ext4Error::new(from_block_device(error)))
    }

    fn flush(&self) -> Result<(), another_ext4::Ext4Error> {
        self.device
            .flush()
            .map_err(|error| another_ext4::Ext4Error::new(from_block_device(error)))
    }

    fn write_blocks(&self, start: u64, data: &[u8]) -> Result<(), another_ext4::Ext4Error> {
        if data.is_empty() {
            return Ok(());
        }
        if data.len() % another_ext4::BLOCK_SIZE != 0 {
            return Err(another_ext4::Ext4Error::new(another_ext4::ErrCode::EINVAL));
        }
        let block_index = Self::block_index(start)?;
        self.device
            .write_block(block_index, data)
            .map_err(|error| another_ext4::Ext4Error::new(from_block_device(error)))
    }

    fn supports_reliable_flush(&self) -> bool {
        self.device.supports_reliable_flush()
    }

    fn diagnostic_enabled(&self) -> bool {
        self.diagnostic_cycles() != 0
    }

    fn diagnostic_cycles(&self) -> usize {
        perf::perf_memory_io_time_now()
    }

    fn record_journal_commit(&self, bytes: usize) {
        perf::record_journal_commit(bytes);
    }

    fn record_writeback_data_write(&self, bytes: usize, cycles: usize) {
        perf::record_wb_tx_data_write(bytes, cycles);
    }

    fn record_writeback_alloc_extent(&self, pages: usize, cycles: usize) {
        perf::record_wb_tx_alloc_extent(pages, cycles);
    }

    fn record_writeback_journal_commit(
        &self,
        transaction_id: u32,
        staged_blocks: usize,
        cycles: usize,
        _reason: another_ext4::JournalCommitReason,
    ) {
        perf::record_wb_tx_journal_commit(transaction_id, staged_blocks, cycles);
    }

    fn record_writeback_journal_flush(
        &self,
        _transaction_id: u32,
        _phase: another_ext4::JournalFlushPhase,
        cycles: usize,
    ) {
        perf::record_wb_tx_journal_flush(cycles);
    }

    fn record_writeback_flush_boundary(
        &self,
        _reason: another_ext4::JournalCommitReason,
        cycles: usize,
    ) {
        perf::record_wb_tx_boundary_flush(cycles);
    }
}
