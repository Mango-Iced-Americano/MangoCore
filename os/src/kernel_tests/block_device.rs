//! L3 tests for the fallible persistent BlockDevice contract.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use crate::drivers::block::partition::{probe_mbr, MbrProbe, PartitionBlockDevice};
use crate::drivers::block::{
    block_device_name, BlockDevice, BlockDeviceError, BlockDeviceNameStyle,
};
use crate::fs::boot_block::partition_name;
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

struct MbrBlockDevice {
    block0: [u8; BLOCK_SZ],
    size_bytes: u64,
}

impl BlockDevice for MbrBlockDevice {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) -> Result<(), BlockDeviceError> {
        if block_id != 0 || buf.len() != BLOCK_SZ {
            return Err(BlockDeviceError::OutOfBounds);
        }
        buf.copy_from_slice(&self.block0);
        Ok(())
    }

    fn write_block(&self, _block_id: usize, _buf: &[u8]) -> Result<(), BlockDeviceError> {
        Ok(())
    }

    fn size_bytes(&self) -> Option<u64> {
        Some(self.size_bytes)
    }
}

struct GptBlockDevice {
    blocks: [[u8; BLOCK_SZ]; 5],
    size_bytes: u64,
}

impl BlockDevice for GptBlockDevice {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) -> Result<(), BlockDeviceError> {
        let block = self
            .blocks
            .get(block_id)
            .ok_or(BlockDeviceError::OutOfBounds)?;
        if buf.len() != BLOCK_SZ {
            return Err(BlockDeviceError::OutOfBounds);
        }
        buf.copy_from_slice(block);
        Ok(())
    }

    fn write_block(&self, _block_id: usize, _buf: &[u8]) -> Result<(), BlockDeviceError> {
        Ok(())
    }

    fn size_bytes(&self) -> Option<u64> {
        Some(self.size_bytes)
    }
}

fn protective_mbr_gpt_device(header_signature: [u8; 8]) -> Arc<dyn BlockDevice> {
    let mut blocks = [[0u8; BLOCK_SZ]; 5];
    let block0 = &mut blocks[0];
    block0[510] = 0x55;
    block0[511] = 0xAA;
    block0[446 + 4] = 0xEE;
    block0[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
    block0[446 + 12..446 + 16].copy_from_slice(&u32::MAX.to_le_bytes());

    let header = &mut blocks[0][512..1024];
    header[..8].copy_from_slice(&header_signature);
    header[72..80].copy_from_slice(&2u64.to_le_bytes());
    header[80..84].copy_from_slice(&128u32.to_le_bytes());
    header[84..88].copy_from_slice(&128u32.to_le_bytes());

    let entry = &mut blocks[0][1024..1024 + 128];
    entry[..16].copy_from_slice(&[
        0xaf, 0x3d, 0xc6, 0x0f, 0x83, 0x84, 0x72, 0x47, 0x8e, 0x79, 0x3d, 0x69, 0xd8, 0x47, 0x7d,
        0xe4,
    ]);
    entry[32..40].copy_from_slice(&2048u64.to_le_bytes());
    entry[40..48].copy_from_slice(&4095u64.to_le_bytes());

    Arc::new(GptBlockDevice {
        blocks,
        size_bytes: (u64::from(u32::MAX) + 1) * 512,
    })
}

struct SectorPatternBlockDevice;

impl BlockDevice for SectorPatternBlockDevice {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) -> Result<(), BlockDeviceError> {
        for (offset, byte) in buf.iter_mut().enumerate() {
            *byte = (block_id * 8 + offset / 512) as u8;
        }
        Ok(())
    }

    fn write_block(&self, _block_id: usize, _buf: &[u8]) -> Result<(), BlockDeviceError> {
        Ok(())
    }
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
        KernelTest::new(
            "block_device::probe_mbr_accepts_unaligned_partition",
            test_probe_mbr_accepts_unaligned_partition,
        ),
        KernelTest::new(
            "block_device::probe_mbr_rejects_out_of_range_partition",
            test_probe_mbr_rejects_out_of_range_partition,
        ),
        KernelTest::new(
            "block_device::probe_mbr_uses_gpt_partitions",
            test_probe_mbr_uses_gpt_partitions,
        ),
        KernelTest::new(
            "block_device::probe_mbr_skips_protective_entry_without_gpt",
            test_probe_mbr_skips_protective_entry_without_gpt,
        ),
        KernelTest::new(
            "block_device::partition_unaligned_start_reads_correct_offset",
            test_partition_unaligned_start_reads_correct_offset,
        ),
        KernelTest::new(
            "block_device::driver_name_styles_and_partition_separators",
            test_driver_name_styles_and_partition_separators,
        ),
        KernelTest::new(
            "block_device::virtio_dma_bridge_roundtrips_local_reservation",
            test_virtio_dma_bridge_roundtrips_local_reservation,
        ),
    ]
}

fn test_virtio_dma_bridge_roundtrips_local_reservation() -> Result<(), &'static str> {
    use crate::drivers::block::virtio_dma_pool;

    let _bridge = virtio_dma_pool::dma_bridge_lock();
    let reservation = virtio_dma_pool::dma_pool_reserve(1)
        .ok_or("VirtIO DMA pool unavailable for bridge test")?;
    virtio_dma_pool::dma_bridge_set_reservation(Some(reservation));
    let consumed = virtio_dma_pool::dma_bridge_take_data_reservation()
        .ok_or("per-hart bridge lost its reservation")?;
    if consumed.slot != reservation.slot || consumed.gen != reservation.gen {
        return Err("per-hart bridge returned a different reservation");
    }
    virtio_dma_pool::dma_pool_cancel_reservation(consumed.slot, consumed.gen);
    Ok(())
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

fn test_probe_mbr_accepts_unaligned_partition() -> Result<(), &'static str> {
    let mut block0 = [0u8; BLOCK_SZ];
    block0[510] = 0x55;
    block0[511] = 0xAA;
    block0[446 + 4] = 0x83;
    block0[446 + 8..446 + 12].copy_from_slice(&3u32.to_le_bytes());
    block0[446 + 12..446 + 16].copy_from_slice(&16u32.to_le_bytes());
    let device: Arc<dyn BlockDevice> = Arc::new(MbrBlockDevice {
        block0,
        size_bytes: 128 * 512,
    });

    match probe_mbr(&device) {
        Ok(MbrProbe::Partitions(parts)) if parts.len() == 1 => {
            let partition = &parts[0];
            if partition.start_lba == 3 && partition.sectors == 16 {
                Ok(())
            } else {
                Err("probe_mbr changed the unaligned partition bounds")
            }
        }
        _ => Err("probe_mbr rejected a valid 512-byte-aligned partition"),
    }
}

fn test_probe_mbr_rejects_out_of_range_partition() -> Result<(), &'static str> {
    let mut block0 = [0u8; BLOCK_SZ];
    block0[510] = 0x55;
    block0[511] = 0xAA;
    block0[446 + 4] = 0x83;
    block0[446 + 8..446 + 12].copy_from_slice(&127u32.to_le_bytes());
    block0[446 + 12..446 + 16].copy_from_slice(&2u32.to_le_bytes());
    let device: Arc<dyn BlockDevice> = Arc::new(MbrBlockDevice {
        block0,
        size_bytes: 128 * 512,
    });

    match probe_mbr(&device) {
        Ok(MbrProbe::Unsupported) => Ok(()),
        _ => Err("probe_mbr accepted a partition past the end of its disk"),
    }
}

fn test_probe_mbr_uses_gpt_partitions() -> Result<(), &'static str> {
    let device = protective_mbr_gpt_device(*b"EFI PART");

    match probe_mbr(&device) {
        Ok(MbrProbe::Partitions(parts)) if parts.len() == 1 => {
            let partition = &parts[0];
            if partition.partno == 1 && partition.start_lba == 2048 && partition.sectors == 2048 {
                Ok(())
            } else {
                Err("probe_mbr did not publish the GPT partition bounds")
            }
        }
        _ => Err("probe_mbr published the protective MBR instead of the GPT partition"),
    }
}

fn test_probe_mbr_skips_protective_entry_without_gpt() -> Result<(), &'static str> {
    let device = protective_mbr_gpt_device(*b"BAD PART");

    match probe_mbr(&device) {
        Ok(MbrProbe::Unsupported) => Ok(()),
        _ => Err("probe_mbr published a protective MBR entry without a valid GPT header"),
    }
}

fn test_partition_unaligned_start_reads_correct_offset() -> Result<(), &'static str> {
    let partition = PartitionBlockDevice::new(Arc::new(SectorPatternBlockDevice), 3, 16);
    let mut buf = [0u8; BLOCK_SZ];

    partition
        .read_block(0, &mut buf)
        .map_err(|_| "partition read failed")?;

    if buf[0] == 3 && buf[511] == 3 && buf[512] == 4 && buf[BLOCK_SZ - 1] == 10 {
        Ok(())
    } else {
        Err("partition read did not apply its 512-byte LBA offset")
    }
}

fn test_driver_name_styles_and_partition_separators() -> Result<(), &'static str> {
    // Given: driver-declared virtio and MMC name styles.
    let virtio = BlockDeviceNameStyle::Alphabetic("vd");
    let mmc = BlockDeviceNameStyle::Decimal("mmcblk");

    // When: per-style device indices and partition names are generated.
    let vda = block_device_name(virtio, 0);
    let vdb = block_device_name(virtio, 1);
    let mmcblk0 = block_device_name(mmc, 0);

    // Then: disk and partition names follow the Linux separator convention.
    if vda != "vda" || vdb != "vdb" || mmcblk0 != "mmcblk0" {
        return Err("driver name styles generated an unexpected disk name");
    }
    if partition_name(&mmcblk0, 1) != "mmcblk0p1" {
        return Err("partition after a digit did not use the p separator");
    }
    if partition_name(&vda, 1) != "vda1" {
        return Err("partition after an alphabetic disk name used the wrong separator");
    }
    Ok(())
}
