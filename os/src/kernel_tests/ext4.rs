//! L3 tests for the lwext4 block-adapter boundary and the loop-mounted
//! ktest test disks.
//!
//! Zero-drive ktest deliberately keeps block-bridge coverage independent of
//! PID1 and external image topology. The real-disk tests additionally exercise
//! the initramfs-embedded loop disks (`/test-ext` ext4, `/test-fat` FAT32)
//! through the VFS, mounting and unmounting them per test.

#[path = "ext4/block_device.rs"]
mod block_device;
#[path = "ext4/real_disk.rs"]
mod real_disk;
#[cfg(feature = "ext4_lwext4_backend")]
#[path = "ext4/byte_bridge.rs"]
mod byte_bridge;
#[cfg(feature = "ext4_lwext4_backend")]
#[path = "ext4/mounted_filesystem.rs"]
mod mounted_filesystem;

use alloc::vec;
use alloc::vec::Vec;

use crate::kernel_tests::runner::KernelTest;

/// Returns the topology-independent ext4-related kernel tests.
pub fn tests() -> Vec<KernelTest> {
    let mut tests = vec![
        KernelTest::new(
            "ext4::mountpoint_exists",
            real_disk::test_ext4_mountpoint_exists,
        ),
        KernelTest::new(
            "ext4::create_write_read_remove",
            real_disk::test_ext4_create_write_read_remove,
        ),
        KernelTest::new(
            "ext4::fat32_create_write_read_remove",
            real_disk::test_fat32_create_write_read_remove,
        ),
    ];
    #[cfg(feature = "ext4_lwext4_backend")]
    tests.extend([
        KernelTest::new(
            "ext4::open_unformatted_returns_err",
            block_device::test_open_unformatted_returns_err,
        ),
        KernelTest::new(
            "ext4::lw_path_isolation",
            mounted_filesystem::test_lw_path_isolation,
        ),
        KernelTest::new(
            "ext4::lwext4_2k_byte_bridge",
            byte_bridge::test_lwext4_2k_byte_bridge,
        ),
        KernelTest::new(
            "ext4::partition_unaligned_batching",
            byte_bridge::test_partition_unaligned_batching,
        ),
        KernelTest::new(
            "ext4::lwext4_flush_forwarding",
            byte_bridge::test_lwext4_flush_forwarding,
        ),
    ]);
    tests
}
