//! Byte-granular lwext4 and partition adapter regression tests.

use alloc::sync::Arc;
use alloc::vec;

use super::block_device::RecordingMemBlock;
use crate::drivers::block::BlockDevice;
use crate::hal::BLOCK_SZ;

pub(super) fn test_lwext4_2k_byte_bridge() -> Result<(), &'static str> {
    use crate::fs::ext4_lwext4::blockdev::{read_bytes_for_block_size, write_bytes_for_block_size};

    const BOARD_BLOCK: usize = 2048;
    let concrete = Arc::new(RecordingMemBlock::<BOARD_BLOCK>::new(8 * BOARD_BLOCK, 0xa5));
    let device: Arc<dyn BlockDevice> = concrete.clone();
    let start = 1024usize;
    let mut payload = vec![0u8; 1024 + 3 * BOARD_BLOCK + 333];
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(37).wrapping_add(11);
    }
    let before = concrete.snapshot();

    write_bytes_for_block_size::<BOARD_BLOCK>(&device, start, &payload);
    let after = concrete.snapshot();
    if after[start..start + payload.len()] != payload {
        return Err("2K bridge write payload mismatch");
    }
    if after[..start] != before[..start]
        || after[start + payload.len()..] != before[start + payload.len()..]
    {
        return Err("2K bridge write changed adjacent bytes");
    }
    if concrete.take_calls()
        != vec![
            (false, 0, BOARD_BLOCK),
            (true, 0, BOARD_BLOCK),
            (true, 1, 3 * BOARD_BLOCK),
            (false, 4, BOARD_BLOCK),
            (true, 4, BOARD_BLOCK),
        ]
    {
        return Err("2K bridge did not batch the aligned write middle");
    }

    let mut readback = vec![0u8; payload.len()];
    read_bytes_for_block_size::<BOARD_BLOCK>(&device, start, &mut readback);
    if readback != payload {
        return Err("2K bridge readback mismatch");
    }
    if concrete.take_calls()
        != vec![
            (false, 0, BOARD_BLOCK),
            (false, 1, 3 * BOARD_BLOCK),
            (false, 4, BOARD_BLOCK),
        ]
    {
        return Err("2K bridge did not batch the aligned read middle");
    }
    Ok(())
}

pub(super) fn test_partition_unaligned_batching() -> Result<(), &'static str> {
    use crate::drivers::block::partition::{BlockSizeAdapter, PartitionBlockDevice};

    let concrete = Arc::new(RecordingMemBlock::<BLOCK_SZ>::new(12 * BLOCK_SZ, 0x6d));
    let partition = PartitionBlockDevice::new(concrete.clone(), 1, 64);
    let mut payload = vec![0u8; 2 * BLOCK_SZ];
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(13).wrapping_add(7);
    }
    let before = concrete.snapshot();
    partition.write_block(0, &payload);
    let after = concrete.snapshot();
    let absolute = 512usize;
    if after[absolute..absolute + payload.len()] != payload {
        return Err("unaligned partition write payload mismatch");
    }
    if after[..absolute] != before[..absolute]
        || after[absolute + payload.len()..] != before[absolute + payload.len()..]
    {
        return Err("unaligned partition write changed adjacent bytes");
    }
    if concrete.take_calls()
        != vec![
            (false, 0, BLOCK_SZ),
            (true, 0, BLOCK_SZ),
            (true, 1, BLOCK_SZ),
            (false, 2, BLOCK_SZ),
            (true, 2, BLOCK_SZ),
        ]
    {
        return Err("unaligned partition did not batch its aligned middle");
    }

    let mut readback = vec![0u8; payload.len()];
    partition.read_block(0, &mut readback);
    if readback != payload {
        return Err("unaligned partition readback mismatch");
    }
    if concrete.take_calls()
        != vec![
            (false, 0, BLOCK_SZ),
            (false, 1, BLOCK_SZ),
            (false, 2, BLOCK_SZ),
        ]
    {
        return Err("unaligned partition read did not batch its middle");
    }

    let tail = Arc::new(RecordingMemBlock::<BLOCK_SZ>::new(4 * BLOCK_SZ, 0xc3));
    let tail_partition = PartitionBlockDevice::new(tail.clone(), 1, 9);
    if tail_partition.block_count() != 2 {
        return Err("partial-tail partition reported wrong block count");
    }
    let tail_before = tail.snapshot();
    let tail_payload = [0x5au8; BLOCK_SZ];
    tail_partition.write_block(1, &tail_payload);
    let tail_after = tail.snapshot();
    let tail_absolute = 512usize + BLOCK_SZ;
    let tail_valid = 512usize;
    if tail_after[tail_absolute..tail_absolute + tail_valid] != tail_payload[..tail_valid] {
        return Err("partial-tail partition write payload mismatch");
    }
    if tail_after[..tail_absolute] != tail_before[..tail_absolute]
        || tail_after[tail_absolute + tail_valid..] != tail_before[tail_absolute + tail_valid..]
    {
        return Err("partial-tail partition write crossed its byte boundary");
    }
    if tail.take_calls() != vec![(false, 1, BLOCK_SZ), (true, 1, BLOCK_SZ)] {
        return Err("partial-tail partition write used unexpected parent I/O");
    }

    let mut tail_readback = [0xffu8; BLOCK_SZ];
    tail_partition.read_block(1, &mut tail_readback);
    if tail_readback[..tail_valid] != tail_payload[..tail_valid]
        || tail_readback[tail_valid..].iter().any(|byte| *byte != 0)
    {
        return Err("partial-tail partition read was not zero padded");
    }
    if tail.take_calls() != vec![(false, 1, BLOCK_SZ)] {
        return Err("partial-tail partition read used unexpected parent I/O");
    }

    let siblings = Arc::new(RecordingMemBlock::<BLOCK_SZ>::new(2 * BLOCK_SZ, 0x9c));
    let left = PartitionBlockDevice::new(siblings.clone(), 1, 1);
    let right = PartitionBlockDevice::new(siblings.clone(), 2, 1);
    left.write_block(0, &[0x11; BLOCK_SZ]);
    right.write_block(0, &[0x22; BLOCK_SZ]);
    let sibling_after = siblings.snapshot();
    if sibling_after[512..1024] != [0x11; 512] || sibling_after[1024..1536] != [0x22; 512] {
        return Err("sibling partition RMW lost adjacent partition bytes");
    }

    let nested_parent: Arc<dyn BlockDevice> = Arc::new(left);
    let nested = BlockSizeAdapter::new(nested_parent, 512);
    nested.write_block(0, &[0x33; 512]);
    let nested_after = siblings.snapshot();
    if nested_after[512..1024] != [0x33; 512] || nested_after[1024..1536] != [0x22; 512] {
        return Err("nested block-size adapter corrupted sibling partition bytes");
    }
    Ok(())
}

pub(super) fn test_lwext4_flush_forwarding() -> Result<(), &'static str> {
    use crate::drivers::block::partition::{
        BlockSizeAdapter, PartitionBlockDevice, ReadOnlyBlockDevice,
    };
    use crate::fs::ext4_lwext4::blockdev::{MangoBlockDev, MangoKernelDevOp};
    use lwext4_rust::KernelDevOp;

    let concrete = Arc::new(RecordingMemBlock::<BLOCK_SZ>::new(4 * BLOCK_SZ, 0));
    let partition: Arc<dyn BlockDevice> =
        Arc::new(PartitionBlockDevice::new(concrete.clone(), 1, 8));
    let adapted: Arc<dyn BlockDevice> = Arc::new(BlockSizeAdapter::new(partition, 512));
    let read_only: Arc<dyn BlockDevice> = Arc::new(ReadOnlyBlockDevice::new(adapted));
    let mut bridge = MangoBlockDev {
        dev: read_only,
        pos: 0,
        size: (4 * BLOCK_SZ) as u64,
        read_only: true,
        blocked_writes: 0,
    };
    MangoKernelDevOp::flush(&mut bridge).map_err(|_| "lwext4 bridge flush failed")?;
    if concrete.flush_count() != 1 {
        return Err("lwext4 flush did not reach the physical block device once");
    }
    Ok(())
}
