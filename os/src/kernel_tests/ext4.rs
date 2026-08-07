//! L3 tests for the lwext4 block-adapter boundary.
//!
//! Zero-drive ktest deliberately keeps block-bridge coverage independent of
//! PID1 and external image topology; mounted-filesystem cases live separately.
//! Sequential PageCache data-integrity cases also live here: they verify
//! single-threaded file semantics without needing an SMP fixture.

#[path = "ext4/block_device.rs"]
mod block_device;
#[cfg(feature = "ext4_lwext4_backend")]
#[path = "ext4/byte_bridge.rs"]
mod byte_bridge;
#[cfg(feature = "ext4_lwext4_backend")]
#[path = "ext4/mounted_filesystem.rs"]
mod mounted_filesystem;

use alloc::{sync::Arc, vec, vec::Vec};

use crate::{
    config::PAGE_SIZE,
    fs::{tmpfs::TmpFS, vfs::IndexNode},
    kernel_tests::{fs_smp_fixture::FsSmpCacheInode, runner::KernelTest},
};

/// Returns the topology-independent ext4-related kernel tests.
pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new(
            "ext4::memblk_read_write",
            block_device::test_memblk_read_write,
        ),
        KernelTest::new(
            "ext4::memblk_isolation",
            block_device::test_memblk_isolation,
        ),
        #[cfg(feature = "ext4_lwext4_backend")]
        KernelTest::new(
            "ext4::open_unformatted_returns_err",
            block_device::test_open_unformatted_returns_err,
        ),
        #[cfg(feature = "ext4_lwext4_backend")]
        KernelTest::new(
            "ext4::lw_path_isolation",
            mounted_filesystem::test_lw_path_isolation,
        ),
        #[cfg(feature = "ext4_lwext4_backend")]
        KernelTest::new(
            "ext4::lwext4_2k_byte_bridge",
            byte_bridge::test_lwext4_2k_byte_bridge,
        ),
        #[cfg(feature = "ext4_lwext4_backend")]
        KernelTest::new(
            "ext4::partition_unaligned_batching",
            byte_bridge::test_partition_unaligned_batching,
        ),
        #[cfg(feature = "ext4_lwext4_backend")]
        KernelTest::new(
            "ext4::lwext4_flush_forwarding",
            byte_bridge::test_lwext4_flush_forwarding,
        ),
        KernelTest::new(
            "ext4::truncate_tail_zero_after_extend",
            test_truncate_tail_zero_after_extend,
        ),
    ]
}

/// 顺序数据完整性：extend 后 truncate，被截断的尾部必须归零且不可读。
///
/// 原为 fs_smp 组的顺序占位；该测试不涉及并发，按用户要求移入文件系统组。
/// 通过 `FsSmpCacheInode`（零盘 PageCache 后端）验证 resize 双向语义：
/// 1. 写入已知数据；
/// 2. extend 到两页后，扩展区读取必须为全零；
/// 3. truncate 回一页后，原第二页区域读取必须返回 0（tail 已截断）；
/// 4. 已写数据保持不变。
fn test_truncate_tail_zero_after_extend() -> Result<(), &'static str> {
    let cache = crate::kernel_tests::fs_smp_fixture::new_cache();
    let inode: Arc<dyn IndexNode> = Arc::new(FsSmpCacheInode::new(cache.clone(), TmpFS::new()));

    // 1. 写入第一页已知数据。
    let written = cache
        .write_kernel(0, &[0x5a; PAGE_SIZE], 0)
        .map_err(|_| "failed to write first page")?;
    if written != PAGE_SIZE {
        return Err("first-page write returned a partial count");
    }
    inode
        .resize(PAGE_SIZE)
        .map_err(|_| "failed to set file size to one page")?;

    // 2. extend 到两页；扩展区必须读回全零。
    inode
        .resize(2 * PAGE_SIZE)
        .map_err(|_| "failed to extend file to two pages")?;
    let mut extended = [0xffu8; PAGE_SIZE];
    let read = crate::kernel_tests::fs_smp_fixture::read_inode(&inode, PAGE_SIZE, &mut extended)
        .map_err(|_| "failed to read extended region")?;
    if read != PAGE_SIZE || extended.iter().any(|byte| *byte != 0) {
        return Err("extended region was not zero-filled");
    }

    // 3. truncate 回一页；原第二页区域读取必须返回 0（tail 截断）。
    inode
        .resize(PAGE_SIZE)
        .map_err(|_| "failed to truncate file back to one page")?;
    let mut tail = [0xeeu8; PAGE_SIZE];
    let read = crate::kernel_tests::fs_smp_fixture::read_inode(&inode, PAGE_SIZE, &mut tail)
        .map_err(|_| "failed to read truncated tail")?;
    if read != 0 {
        return Err("truncated tail region was still readable");
    }

    // 4. 已写数据保持不变。
    let mut first = [0x00u8; PAGE_SIZE];
    let read = crate::kernel_tests::fs_smp_fixture::read_inode(&inode, 0, &mut first)
        .map_err(|_| "failed to read first page after truncate")?;
    if read != PAGE_SIZE || !first.iter().all(|byte| *byte == 0x5a) {
        return Err("first-page data was corrupted by extend/truncate");
    }
    Ok(())
}
