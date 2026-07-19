//! L3 tests for ext4 multi-instance mount isolation.
//!
//! These tests verify that:
//! - In-memory block devices (`TestMemBlock`) work correctly and are independent
//! - The lwext4 `Ext4FileSystem` path translation (`lw_path`) produces
//!   distinct paths for different filesystem instances
//! - `open_ext4rs` properly rejects unformatted block devices
//!
//! # Limitation
//!
//! Full multi-ext4-instance mount isolation (formatting two in-memory devices
//! with ext4, mounting both, writing files to each, and reading back) is not
//! testable in ktest mode because lwext4's `ext4_mount` always uses `"/"` as
//! its mount point — the second mount is a no-op per the lwext4 global state
//! model.  This is a known limitation documented in `lwext4-upstream-fixes.md`.
//! The VFS-level `MountFS` isolation (tested via other filesystems) provides
//! the same guarantee for the public API layer.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use crate::drivers::block::BlockDevice;
use crate::hal::BLOCK_SZ;
use crate::kernel_tests::runner::KernelTest;

// ── TestMemBlock: reusable in-memory BlockDevice ─────────────────────────

/// A simple block device backed by `Vec<u8>`, sized in bytes on construction.
///
/// All reads beyond the device boundary return zeros; writes beyond the
/// boundary are silently truncated.  This matches the expected behaviour of
/// the `BlockDevice` trait for block-id-based I/O.
struct TestMemBlock {
    data: Mutex<Vec<u8>>,
    size: u64,
}

impl TestMemBlock {
    /// Create a new in-memory block device of `size_bytes` bytes.
    fn new(size_bytes: usize) -> Self {
        Self {
            data: Mutex::new(alloc::vec![0u8; size_bytes]),
            size: size_bytes as u64,
        }
    }
}

impl BlockDevice for TestMemBlock {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) {
        let offset = block_id * BLOCK_SZ;
        let data = self.data.lock();
        if offset >= data.len() {
            buf.fill(0);
            return;
        }
        let end = core::cmp::min(offset + buf.len(), data.len());
        let copy_len = end - offset;
        buf[..copy_len].copy_from_slice(&data[offset..end]);
        // Remaining bytes stay zero (buf was likely zeroed by the caller, but
        // we explicitly fill to be safe).
        if copy_len < buf.len() {
            buf[copy_len..].fill(0);
        }
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) {
        let offset = block_id * BLOCK_SZ;
        let mut data = self.data.lock();
        if offset >= data.len() {
            return; // silently ignore out-of-bounds writes
        }
        let end = core::cmp::min(offset + buf.len(), data.len());
        let copy_len = end - offset;
        data[offset..end].copy_from_slice(&buf[..copy_len]);
    }

    fn size_bytes(&self) -> Option<u64> {
        Some(self.size)
    }
}

// Safety: MangoCore is single-core; the Mutex guards concurrent access.
// Send + Sync are required by `Arc<dyn BlockDevice>`.
unsafe impl Send for TestMemBlock {}
unsafe impl Sync for TestMemBlock {}

/// Block-size-parametric mock used to verify the 2K board byte bridge from a
/// 4K QEMU ktest build.  It also records whether aligned middle runs remain a
/// single multi-block request.
struct RecordingMemBlock<const BLOCK_SIZE: usize> {
    data: Mutex<Vec<u8>>,
    calls: Mutex<Vec<(bool, usize, usize)>>,
    flushes: AtomicUsize,
}

impl<const BLOCK_SIZE: usize> RecordingMemBlock<BLOCK_SIZE> {
    fn new(size_bytes: usize, fill: u8) -> Self {
        Self {
            data: Mutex::new(alloc::vec![fill; size_bytes]),
            calls: Mutex::new(Vec::new()),
            flushes: AtomicUsize::new(0),
        }
    }

    fn snapshot(&self) -> Vec<u8> {
        self.data.lock().clone()
    }

    fn take_calls(&self) -> Vec<(bool, usize, usize)> {
        core::mem::take(&mut *self.calls.lock())
    }

    fn flush_count(&self) -> usize {
        self.flushes.load(Ordering::Relaxed)
    }
}

impl<const BLOCK_SIZE: usize> BlockDevice for RecordingMemBlock<BLOCK_SIZE> {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) {
        assert_eq!(buf.len() % BLOCK_SIZE, 0);
        self.calls.lock().push((false, block_id, buf.len()));
        let start = block_id * BLOCK_SIZE;
        let end = start + buf.len();
        buf.copy_from_slice(&self.data.lock()[start..end]);
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) {
        assert_eq!(buf.len() % BLOCK_SIZE, 0);
        self.calls.lock().push((true, block_id, buf.len()));
        let start = block_id * BLOCK_SIZE;
        let end = start + buf.len();
        self.data.lock()[start..end].copy_from_slice(buf);
    }

    fn size_bytes(&self) -> Option<u64> {
        Some(self.data.lock().len() as u64)
    }

    fn flush(&self) -> Result<(), crate::utils::error::SyscallErr> {
        self.flushes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

unsafe impl<const BLOCK_SIZE: usize> Send for RecordingMemBlock<BLOCK_SIZE> {}
unsafe impl<const BLOCK_SIZE: usize> Sync for RecordingMemBlock<BLOCK_SIZE> {}

// ── Test registration ───────────────────────────────────────────────────

/// Returns all ext4-related kernel tests.
pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new("ext4::memblk_read_write", test_memblk_read_write),
        KernelTest::new("ext4::memblk_isolation", test_memblk_isolation),
        KernelTest::new(
            "ext4::open_unformatted_returns_err",
            test_open_unformatted_returns_err,
        ),
        KernelTest::new("ext4::lw_path_isolation", test_lw_path_isolation),
        KernelTest::new("ext4::lwext4_2k_byte_bridge", test_lwext4_2k_byte_bridge),
        KernelTest::new(
            "ext4::partition_unaligned_batching",
            test_partition_unaligned_batching,
        ),
        KernelTest::new(
            "ext4::lwext4_partial_writeback_visibility",
            test_lwext4_partial_writeback_visibility,
        ),
        KernelTest::new(
            "ext4::lwext4_flush_forwarding",
            test_lwext4_flush_forwarding,
        ),
        KernelTest::new(
            "ext4::lwext4_nested_symlink_recreate",
            test_lwext4_nested_symlink_recreate,
        ),
    ]
}

/// Exercise the persistent-shell layout: a relative symlink is created in a
/// nested directory next to its executable target, removed, and recreated.
/// Both cycles must leave a discoverable symlink inode rather than ENOENT.
fn test_lwext4_nested_symlink_recreate() -> Result<(), &'static str> {
    use crate::fs::vfs::{FileType, InodeMode};

    const ROOT_NAME: &str = ".ktest_lwext4_symlink";
    const DIR_NAME: &str = "bin";
    const TARGET_NAME: &str = "busybox";
    const LINK_NAME: &str = "sh";

    let root = crate::fs::vfs_lookup_absolute("/sdcard")
        .map_err(|_| "ktest ext4 fixture is not mounted at /sdcard")?;
    let test_root = root
        .create(ROOT_NAME, FileType::Dir, InodeMode::S_IRWXU)
        .map_err(|_| "failed to create symlink test root")?;
    let bin = test_root
        .create(DIR_NAME, FileType::Dir, InodeMode::S_IRWXU)
        .map_err(|_| "failed to create symlink test bin directory")?;
    bin.create(
        TARGET_NAME,
        FileType::File,
        InodeMode::S_IRUSR | InodeMode::S_IWUSR | InodeMode::S_IXUSR,
    )
    .map_err(|_| "failed to create symlink target")?;

    for _ in 0..2 {
        let link = bin
            .symlink(LINK_NAME, TARGET_NAME)
            .map_err(|_| "failed to create nested relative symlink")?;
        match bin.symlink(LINK_NAME, "other-target") {
            Err(crate::utils::error::SyscallErr::EEXIST) => {}
            Ok(_) => return Err("duplicate symlink creation unexpectedly succeeded"),
            Err(_) => return Err("duplicate symlink creation returned wrong errno"),
        }
        if link
            .metadata()
            .map_err(|_| "failed to stat created symlink")?
            .file_type
            != FileType::SymLink
        {
            return Err("created inode is not a symlink");
        }
        if bin
            .find(LINK_NAME)
            .map_err(|_| "created symlink is not discoverable")?
            .metadata()
            .map_err(|_| "failed to stat discovered symlink")?
            .file_type
            != FileType::SymLink
        {
            return Err("discovered inode is not a symlink");
        }
        bin.unlink(LINK_NAME)
            .map_err(|_| "failed to remove nested relative symlink")?;
    }

    bin.unlink(TARGET_NAME)
        .map_err(|_| "failed to remove symlink target")?;
    test_root
        .rmdir(DIR_NAME)
        .map_err(|_| "failed to remove symlink test bin directory")?;
    root.rmdir(ROOT_NAME)
        .map_err(|_| "failed to remove symlink test root")?;
    root.sync()
        .map_err(|_| "failed to sync symlink test cleanup")?;
    Ok(())
}

/// The crash producer intentionally never returns: QEMU is killed while the
/// C journal test hook is parked between commit durability and home write.
pub fn orphan_crash_tests() -> Vec<KernelTest> {
    vec![KernelTest::new(
        "ext4_orphan_crash::journal_replay_window",
        test_lwext4_orphan_power_cut,
    )]
}

/// Second boot of the deterministic crash test.  Mount-time recovery must
/// replay the unlink transaction, free exactly one persistent orphan, and
/// leave the filesystem writable.
pub fn orphan_recovery_tests() -> Vec<KernelTest> {
    vec![KernelTest::new(
        "ext4_orphan_recover::mount_cleanup",
        test_lwext4_orphan_recovery,
    )]
}

fn test_lwext4_orphan_power_cut() -> Result<(), &'static str> {
    use crate::fs::ext4_lwext4::ext4fs::Ext4FileSystem;
    use crate::fs::vfs::{File, FileFlags, FileType, InodeMode};

    const NAME: &str = ".ktest_lwext4_orphan_power_cut";
    const PAYLOAD: &[u8] = b"mango-lwext4-orphan-replay-v1";

    let root = crate::fs::vfs_lookup_absolute("/sdcard")
        .map_err(|_| "ktest ext4 fixture is not mounted at /sdcard")?;
    let wrapper = root.fs();
    let mount_fs = wrapper
        .as_any_ref()
        .downcast_ref::<crate::fs::vfs::MountFS>()
        .ok_or("/sdcard filesystem is not a MountFS")?;
    let ext4 = mount_fs
        .lifecycle
        .fs()
        .as_any_ref()
        .downcast_ref::<Ext4FileSystem>()
        .ok_or("/sdcard backend is not lwext4")?;

    let _ = root.unlink(NAME);
    let inode = root
        .create(NAME, FileType::File, InodeMode::S_IRUSR | InodeMode::S_IWUSR)
        .map_err(|_| "failed to create orphan crash file")?;
    let file = File::new(inode.clone(), FileFlags::O_RDWR)
        .map_err(|_| "failed to open orphan crash file")?;
    if file
        .write(PAYLOAD)
        .map_err(|_| "failed to write orphan crash payload")?
        != PAYLOAD.len()
    {
        return Err("orphan crash payload write was short");
    }
    inode
        .sync()
        .map_err(|_| "failed to persist orphan crash payload")?;

    ext4
        .arm_journal_power_cut_for_test()
        .map_err(|_| "failed to arm journal power-cut hook")?;
    crate::println!("[KTEST ORPHAN CRASH] unlink transaction entering power-cut window");

    // This call reaches the armed hook and does not return.  `file` remains
    // live, so the zero-link inode must be recoverable after the forced stop.
    root.unlink(NAME)
        .map_err(|_| "orphan crash unlink returned before power cut")?;
    Err("journal power-cut hook unexpectedly returned")
}

fn test_lwext4_orphan_recovery() -> Result<(), &'static str> {
    use crate::fs::ext4_lwext4::ext4fs::Ext4FileSystem;
    use crate::fs::vfs::{File, FileFlags, FileType, InodeMode};

    const ORPHAN: &str = ".ktest_lwext4_orphan_power_cut";
    const PROBE: &str = ".ktest_lwext4_orphan_recovery_probe";
    const PAYLOAD: &[u8] = b"mango-lwext4-recovery-write-probe";

    let root = crate::fs::vfs_lookup_absolute("/sdcard")
        .map_err(|_| "ktest ext4 fixture is not mounted at /sdcard")?;
    let wrapper = root.fs();
    let mount_fs = wrapper
        .as_any_ref()
        .downcast_ref::<crate::fs::vfs::MountFS>()
        .ok_or("/sdcard filesystem is not a MountFS")?;
    let ext4 = mount_fs
        .lifecycle
        .fs()
        .as_any_ref()
        .downcast_ref::<Ext4FileSystem>()
        .ok_or("/sdcard backend is not lwext4")?;

    if ext4.recovered_orphans() != 1 {
        return Err("mount did not recover exactly one persistent orphan");
    }
    if root.find(ORPHAN).is_ok() {
        return Err("journal-replayed orphan pathname is still visible");
    }

    let _ = root.unlink(PROBE);
    let inode = root
        .create(PROBE, FileType::File, InodeMode::S_IRUSR | InodeMode::S_IWUSR)
        .map_err(|_| "failed to create post-recovery write probe")?;
    let file = File::new(inode, FileFlags::O_RDWR)
        .map_err(|_| "failed to open post-recovery write probe")?;
    if file
        .pwrite(0, PAYLOAD)
        .map_err(|_| "post-recovery probe write failed")?
        != PAYLOAD.len()
    {
        return Err("post-recovery probe write was short");
    }
    let mut actual = [0u8; PAYLOAD.len()];
    if file
        .pread(0, &mut actual)
        .map_err(|_| "post-recovery probe read failed")?
        != PAYLOAD.len()
        || actual != PAYLOAD
    {
        return Err("post-recovery probe readback mismatch");
    }
    drop(file);
    root.unlink(PROBE)
        .map_err(|_| "failed to remove post-recovery write probe")?;
    Ok(())
}

// ── Test 1: basic BlockDevice read/write ────────────────────────────────

/// Verify that `TestMemBlock` correctly persists data written to a block.
fn test_memblk_read_write() -> Result<(), &'static str> {
    let dev = Arc::new(TestMemBlock::new(64 * 1024 * 1024)); // 64 MiB

    // Write known pattern to block 0
    let mut pattern = [0u8; BLOCK_SZ];
    for i in 0..BLOCK_SZ {
        pattern[i] = (i % 256) as u8;
    }
    dev.write_block(0, &pattern);

    // Read back and compare
    let mut actual = [0u8; BLOCK_SZ];
    dev.read_block(0, &mut actual);

    if pattern != actual {
        return Err("block 0: read data does not match written data");
    }

    // Write different pattern to block 1
    let pattern2 = [0xABu8; BLOCK_SZ];
    dev.write_block(1, &pattern2);

    dev.read_block(1, &mut actual);
    if pattern2 != actual {
        return Err("block 1: read data does not match written data");
    }

    // Block 0 must still hold the original data
    let mut actual0 = [0u8; BLOCK_SZ];
    dev.read_block(0, &mut actual0);
    if pattern != actual0 {
        return Err("block 0: data corrupted after writing block 1");
    }

    Ok(())
}

// ── Test 2: two TestMemBlock instances are independent ──────────────────

/// Write different content to two separate `TestMemBlock` instances and
/// verify reads from one are not visible on the other.
fn test_memblk_isolation() -> Result<(), &'static str> {
    let dev1 = Arc::new(TestMemBlock::new(64 * 1024 * 1024));
    let dev2 = Arc::new(TestMemBlock::new(64 * 1024 * 1024));

    let buf1 = [0x11u8; BLOCK_SZ];
    let buf2 = [0x22u8; BLOCK_SZ];

    dev1.write_block(0, &buf1);
    dev2.write_block(0, &buf2);

    // dev1 must see buf1
    let mut r = [0u8; BLOCK_SZ];
    dev1.read_block(0, &mut r);
    if r != buf1 {
        return Err("dev1: data leaked from dev2 or not written correctly");
    }

    // dev2 must see buf2
    dev2.read_block(0, &mut r);
    if r != buf2 {
        return Err("dev2: data leaked from dev1 or not written correctly");
    }

    Ok(())
}

// ── Test 3: open_ext4rs rejects unformatted devices ─────────────────────

/// `Ext4FileSystem::open_ext4rs` must return an error (not panic) when the
/// underlying block device contains no valid ext4 superblock.
fn test_open_unformatted_returns_err() -> Result<(), &'static str> {
    use crate::fs::ext4_lwext4::ext4fs::Ext4FileSystem;

    let dev = Arc::new(TestMemBlock::new(64 * 1024 * 1024));

    let result = Ext4FileSystem::open_ext4rs(dev);
    match result {
        Ok(_) => Err("open_ext4rs should fail on an all-zero (unformatted) device"),
        Err(_e) => Ok(()),
    }
}

// ── Test 4: lw_path produces isolated paths per instance ────────────────

/// Verify the mounted instance's VFS-to-lwext4 path contract without mounting
/// the same block device a second time.  A duplicate raw mount would retain a
/// second superblock/cache view and could overwrite newer state at shutdown.
fn test_lw_path_isolation() -> Result<(), &'static str> {
    use crate::fs::ext4_lwext4::ext4fs::Ext4FileSystem;

    let root = crate::fs::vfs_lookup_absolute("/sdcard")
        .map_err(|_| "ktest ext4 fixture is not mounted at /sdcard")?;
    let wrapper = root.fs();
    let mount_fs = wrapper
        .as_any_ref()
        .downcast_ref::<crate::fs::vfs::MountFS>()
        .ok_or("/sdcard filesystem is not a MountFS")?;
    let fs1 = mount_fs
        .lifecycle
        .fs()
        .as_any_ref()
        .downcast_ref::<Ext4FileSystem>()
        .ok_or("/sdcard backend is not lwext4")?;

    // Basic contract: lw_path("/") returns the mount point.
    let root = fs1.lw_path("/");
    if root.is_empty() {
        return Err("lw_path(\"/\") returned empty string");
    }

    // lw_path("/foo") must start with the mount point.
    let foo = fs1.lw_path("/foo");
    if !foo.starts_with(&root) {
        return Err("lw_path(\"/foo\") should start with the mount point");
    }

    // lw_path("/bar") must differ from lw_path("/foo").
    let bar = fs1.lw_path("/bar");
    if foo == bar {
        return Err("lw_path(\"/foo\") and lw_path(\"/bar\") should be distinct paths");
    }

    // The fs_id must be non-zero.
    let id = fs1.dev_id();
    if id == 0 {
        return Err("Ext4FileSystem dev_id should be non-zero");
    }

    Ok(())
}

fn test_lwext4_2k_byte_bridge() -> Result<(), &'static str> {
    use crate::fs::ext4_lwext4::blockdev::{
        read_bytes_for_block_size, write_bytes_for_block_size,
    };

    const BOARD_BLOCK: usize = 2048;
    let concrete = Arc::new(RecordingMemBlock::<BOARD_BLOCK>::new(
        8 * BOARD_BLOCK,
        0xa5,
    ));
    let device: Arc<dyn BlockDevice> = concrete.clone();
    let start = 1024usize;
    let mut payload = alloc::vec![0u8; 1024 + 3 * BOARD_BLOCK + 333];
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
    let expected_write = vec![
        (false, 0, BOARD_BLOCK),
        (true, 0, BOARD_BLOCK),
        (true, 1, 3 * BOARD_BLOCK),
        (false, 4, BOARD_BLOCK),
        (true, 4, BOARD_BLOCK),
    ];
    if concrete.take_calls() != expected_write {
        return Err("2K bridge did not batch the aligned write middle");
    }

    let mut readback = alloc::vec![0u8; payload.len()];
    read_bytes_for_block_size::<BOARD_BLOCK>(&device, start, &mut readback);
    if readback != payload {
        return Err("2K bridge readback mismatch");
    }
    let expected_read = vec![
        (false, 0, BOARD_BLOCK),
        (false, 1, 3 * BOARD_BLOCK),
        (false, 4, BOARD_BLOCK),
    ];
    if concrete.take_calls() != expected_read {
        return Err("2K bridge did not batch the aligned read middle");
    }
    Ok(())
}

fn test_partition_unaligned_batching() -> Result<(), &'static str> {
    use crate::drivers::block::partition::{BlockSizeAdapter, PartitionBlockDevice};

    let concrete = Arc::new(RecordingMemBlock::<BLOCK_SZ>::new(
        12 * BLOCK_SZ,
        0x6d,
    ));
    let parent: Arc<dyn BlockDevice> = concrete.clone();
    let partition = PartitionBlockDevice::new(parent, 1, 64);
    let mut payload = alloc::vec![0u8; 2 * BLOCK_SZ];
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
    let expected_write = vec![
        (false, 0, BLOCK_SZ),
        (true, 0, BLOCK_SZ),
        (true, 1, BLOCK_SZ),
        (false, 2, BLOCK_SZ),
        (true, 2, BLOCK_SZ),
    ];
    if concrete.take_calls() != expected_write {
        return Err("unaligned partition did not batch its aligned middle");
    }

    let mut readback = alloc::vec![0u8; payload.len()];
    partition.read_block(0, &mut readback);
    if readback != payload {
        return Err("unaligned partition readback mismatch");
    }
    let expected_read = vec![
        (false, 0, BLOCK_SZ),
        (false, 1, BLOCK_SZ),
        (false, 2, BLOCK_SZ),
    ];
    if concrete.take_calls() != expected_read {
        return Err("unaligned partition read did not batch its middle");
    }

    // A partition whose byte length is not a multiple of BLOCK_SZ exposes a
    // zero-padded final logical block.  Writes to that block must stop at the
    // exact MBR boundary instead of clobbering the following partition/data.
    let tail_concrete = Arc::new(RecordingMemBlock::<BLOCK_SZ>::new(
        4 * BLOCK_SZ,
        0xc3,
    ));
    let tail_parent: Arc<dyn BlockDevice> = tail_concrete.clone();
    let tail_partition = PartitionBlockDevice::new(tail_parent, 1, 9);
    if tail_partition.block_count() != 2 {
        return Err("partial-tail partition reported wrong block count");
    }
    let tail_before = tail_concrete.snapshot();
    let tail_payload = [0x5au8; BLOCK_SZ];
    tail_partition.write_block(1, &tail_payload);
    let tail_after = tail_concrete.snapshot();
    let tail_absolute = 512usize + BLOCK_SZ;
    let tail_valid = 512usize;
    if tail_after[tail_absolute..tail_absolute + tail_valid]
        != tail_payload[..tail_valid]
    {
        return Err("partial-tail partition write payload mismatch");
    }
    if tail_after[..tail_absolute] != tail_before[..tail_absolute]
        || tail_after[tail_absolute + tail_valid..] != tail_before[tail_absolute + tail_valid..]
    {
        return Err("partial-tail partition write crossed its byte boundary");
    }
    let expected_tail_write = vec![(false, 1, BLOCK_SZ), (true, 1, BLOCK_SZ)];
    if tail_concrete.take_calls() != expected_tail_write {
        return Err("partial-tail partition write used unexpected parent I/O");
    }

    let mut tail_readback = [0xffu8; BLOCK_SZ];
    tail_partition.read_block(1, &mut tail_readback);
    if tail_readback[..tail_valid] != tail_payload[..tail_valid]
        || tail_readback[tail_valid..].iter().any(|byte| *byte != 0)
    {
        return Err("partial-tail partition read was not zero padded");
    }
    if tail_concrete.take_calls() != vec![(false, 1, BLOCK_SZ)] {
        return Err("partial-tail partition read used unexpected parent I/O");
    }

    // Two adjacent 512-byte partitions can occupy the same platform block.
    // Each byte-RMW must preserve the sibling's bytes.
    let sibling_concrete = Arc::new(RecordingMemBlock::<BLOCK_SZ>::new(
        2 * BLOCK_SZ,
        0x9c,
    ));
    let sibling_parent: Arc<dyn BlockDevice> = sibling_concrete.clone();
    let left = PartitionBlockDevice::new(sibling_parent.clone(), 1, 1);
    let right = PartitionBlockDevice::new(sibling_parent, 2, 1);
    left.write_block(0, &[0x11; BLOCK_SZ]);
    right.write_block(0, &[0x22; BLOCK_SZ]);
    let sibling_after = sibling_concrete.snapshot();
    if sibling_after[512..1024] != [0x11; 512]
        || sibling_after[1024..1536] != [0x22; 512]
    {
        return Err("sibling partition RMW lost adjacent partition bytes");
    }

    // FAT stacks BlockSizeAdapter over PartitionBlockDevice.  This exercises
    // the already-guarded delegation path and will time out if the shared RMW
    // lock is accidentally reacquired recursively.
    let nested_parent: Arc<dyn BlockDevice> = Arc::new(left);
    let nested = BlockSizeAdapter::new(nested_parent, 512);
    nested.write_block(0, &[0x33; 512]);
    let nested_after = sibling_concrete.snapshot();
    if nested_after[512..1024] != [0x33; 512]
        || nested_after[1024..1536] != [0x22; 512]
    {
        return Err("nested block-size adapter corrupted sibling partition bytes");
    }
    Ok(())
}

fn test_lwext4_flush_forwarding() -> Result<(), &'static str> {
    use crate::drivers::block::partition::{
        BlockSizeAdapter, PartitionBlockDevice, ReadOnlyBlockDevice,
    };
    use crate::fs::ext4_lwext4::blockdev::{MangoBlockDev, MangoKernelDevOp};
    use lwext4_rust::KernelDevOp;

    let concrete = Arc::new(RecordingMemBlock::<BLOCK_SZ>::new(
        4 * BLOCK_SZ,
        0,
    ));
    let parent: Arc<dyn BlockDevice> = concrete.clone();
    let partition: Arc<dyn BlockDevice> =
        Arc::new(PartitionBlockDevice::new(parent, 1, 8));
    let adapted: Arc<dyn BlockDevice> =
        Arc::new(BlockSizeAdapter::new(partition, 512));
    let read_only: Arc<dyn BlockDevice> =
        Arc::new(ReadOnlyBlockDevice::new(adapted));
    let mut bridge = MangoBlockDev {
        dev: read_only,
        pos: 0,
        size: (4 * BLOCK_SZ) as u64,
        read_only: true,
        blocked_writes: 0,
    };

    MangoKernelDevOp::flush(&mut bridge)
        .map_err(|_| "lwext4 bridge flush failed")?;
    if concrete.flush_count() != 1 {
        return Err("lwext4 flush did not reach the physical block device once");
    }
    Ok(())
}

/// A successful Mango PageCache writeback must remain visible after the upper
/// cache page is explicitly discarded.  This catches the two-level-cache bug
/// where a partial ext4 block remained only in lwext4's write-back cache while
/// ext4_fread() bypassed that cache and returned stale disk bytes.
fn test_lwext4_partial_writeback_visibility() -> Result<(), &'static str> {
    use crate::fs::vfs::{FilePrivateData, FileType, IndexNode as _, InodeMode};

    const NAME: &str = ".ktest_lwext4_partial_visibility";
    const OFFSET: usize = 37;
    const LEN: usize = 123;

    let root = crate::fs::vfs_lookup_absolute("/sdcard")
        .map_err(|_| "ktest ext4 fixture is not mounted at /sdcard")?;
    if !root
        .fs()
        .info()
        .features
        .iter()
        .any(|feature| *feature == "lwext4")
    {
        return Err("partial-writeback visibility test is not running on lwext4");
    }
    let _ = root.unlink(NAME);
    let inode = root
        .create(NAME, FileType::File, InodeMode::S_IRUSR | InodeMode::S_IWUSR)
        .map_err(|_| "failed to create partial-writeback test file")?;
    let mut expected = [0u8; LEN];
    for (index, byte) in expected.iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(29).wrapping_add(17);
    }
    let written = inode
        .write_at(
            OFFSET,
            expected.len(),
            &expected,
            spin::Mutex::new(FilePrivateData::Unused).lock(),
        )
        .map_err(|_| "partial PageCache write failed")?;
    if written != expected.len() {
        let _ = root.unlink(NAME);
        return Err("partial PageCache write was short");
    }

    let page_cache = inode
        .page_cache()
        .ok_or("partial-write test file has no PageCache")?;
    page_cache
        .writeback_all()
        .map_err(|_| "partial PageCache writeback failed")?;
    page_cache
        .invalidate_range(0, 1)
        .map_err(|_| "clean PageCache invalidation failed")?;

    let mut actual = [0u8; LEN];
    let read = inode
        .read_at(
            OFFSET,
            actual.len(),
            &mut actual,
            spin::Mutex::new(FilePrivateData::Unused).lock(),
        )
        .map_err(|_| "read after PageCache invalidation failed")?;
    let cleanup = root.unlink(NAME);
    if cleanup.is_err() {
        return Err("failed to clean partial-writeback test file");
    }
    if read != expected.len() || actual != expected {
        return Err("partial writeback was stale after upper-cache invalidation");
    }
    Ok(())
}
