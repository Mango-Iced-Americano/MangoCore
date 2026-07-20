//! L3 tests for the fallible persistent BlockDevice contract.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use crate::drivers::block::partition::PartitionBlockDevice;
use crate::drivers::block::{BlockDevice, BlockDeviceError};
use crate::hal::BLOCK_SZ;
use crate::kernel_tests::runner::KernelTest;

#[derive(Clone, Copy)]
enum FailedOperation {
    Read,
    Write,
    Flush,
}

struct FailingBlockDevice {
    failed_operation: FailedOperation,
    reliable_flush: bool,
}

impl FailingBlockDevice {
    const fn new(failed_operation: FailedOperation, reliable_flush: bool) -> Self {
        Self {
            failed_operation,
            reliable_flush,
        }
    }
}

impl BlockDevice for FailingBlockDevice {
    fn read_block(&self, _block_id: usize, _buf: &mut [u8]) -> Result<(), BlockDeviceError> {
        match self.failed_operation {
            FailedOperation::Read => Err(BlockDeviceError::DeviceError),
            FailedOperation::Write | FailedOperation::Flush => Ok(()),
        }
    }

    fn write_block(&self, _block_id: usize, _buf: &[u8]) -> Result<(), BlockDeviceError> {
        match self.failed_operation {
            FailedOperation::Read | FailedOperation::Flush => Ok(()),
            FailedOperation::Write => Err(BlockDeviceError::DeviceError),
        }
    }

    fn flush(&self) -> Result<(), BlockDeviceError> {
        match self.failed_operation {
            FailedOperation::Flush => Err(BlockDeviceError::DeviceError),
            FailedOperation::Read | FailedOperation::Write => Ok(()),
        }
    }

    fn supports_reliable_flush(&self) -> bool {
        self.reliable_flush
    }
}

/// Returns all fallible BlockDevice contract tests.
pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new(
            "block_device::partition_read_error_propagates",
            test_partition_read_error_propagates,
        ),
        KernelTest::new(
            "block_device::partition_write_error_propagates",
            test_partition_write_error_propagates,
        ),
        KernelTest::new(
            "block_device::partition_flush_error_propagates",
            test_partition_flush_error_propagates,
        ),
        KernelTest::new(
            "block_device::partition_rejects_unsupported_flush",
            test_partition_rejects_unsupported_flush,
        ),
    ]
}

fn test_partition_read_error_propagates() -> Result<(), &'static str> {
    let parent = Arc::new(FailingBlockDevice::new(FailedOperation::Read, true));
    let partition = PartitionBlockDevice::new(parent, 0, 8);
    let mut buf = [0u8; BLOCK_SZ];

    match partition.read_block(0, &mut buf) {
        Err(BlockDeviceError::DeviceError) => Ok(()),
        _ => Err("partition did not propagate the parent read error"),
    }
}

fn test_partition_write_error_propagates() -> Result<(), &'static str> {
    let parent = Arc::new(FailingBlockDevice::new(FailedOperation::Write, true));
    let partition = PartitionBlockDevice::new(parent, 0, 8);
    let buf = [0u8; BLOCK_SZ];

    match partition.write_block(0, &buf) {
        Err(BlockDeviceError::DeviceError) => Ok(()),
        _ => Err("partition did not propagate the parent write error"),
    }
}

fn test_partition_flush_error_propagates() -> Result<(), &'static str> {
    let parent = Arc::new(FailingBlockDevice::new(FailedOperation::Flush, true));
    let partition = PartitionBlockDevice::new(parent, 0, 8);

    if !partition.supports_reliable_flush() {
        return Err("partition lost the parent reliable-flush capability");
    }

    match partition.flush() {
        Err(BlockDeviceError::DeviceError) => Ok(()),
        _ => Err("partition did not propagate the parent flush error"),
    }
}

fn test_partition_rejects_unsupported_flush() -> Result<(), &'static str> {
    let parent = Arc::new(FailingBlockDevice::new(FailedOperation::Read, false));
    let partition = PartitionBlockDevice::new(parent, 0, 8);

    if partition.supports_reliable_flush() {
        return Err("partition reported reliable flush for an unsupported parent");
    }

    match partition.flush() {
        Err(BlockDeviceError::FlushUnsupported) => Ok(()),
        _ => Err("partition reported flush success without reliable-flush support"),
    }
}
