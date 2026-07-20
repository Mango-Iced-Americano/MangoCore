//! In-kernel contract tests for the feature-gated another_ext4 writable bridge.

#[cfg(feature = "ext4_another_backend")]
use alloc::sync::Arc;
use alloc::vec;
#[cfg(feature = "ext4_another_backend")]
use core::convert::TryFrom;
#[cfg(feature = "ext4_another_backend")]
use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(feature = "ext4_another_backend")]
use crate::config::PAGE_SIZE;
#[cfg(feature = "ext4_another_backend")]
use crate::drivers::block::{
    validate_block_buffer_length, BlockDevice, BlockDeviceError, BlockDeviceResult,
};
#[cfg(feature = "ext4_another_backend")]
use crate::fs::vfs::{FileFlags, FilePrivateData, FileSystem, FileType, InodeMode};
use crate::kernel_tests::runner::KernelTest;
#[cfg(feature = "ext4_another_backend")]
use crate::utils::error::SyscallErr;

#[cfg(feature = "ext4_another_backend")]
struct ZeroBlockDevice;

#[cfg(feature = "ext4_another_backend")]
impl BlockDevice for ZeroBlockDevice {
    fn read_block(&self, _block_id: usize, buf: &mut [u8]) -> BlockDeviceResult {
        validate_block_buffer_length(buf.len())?;
        buf.fill(0);
        Ok(())
    }

    fn write_block(&self, _block_id: usize, _buf: &[u8]) -> BlockDeviceResult {
        Err(BlockDeviceError::DeviceError)
    }
}

#[cfg(feature = "ext4_another_backend")]
struct FlushFailsAfterMountDevice {
    inner: Arc<dyn BlockDevice>,
    flush_count: AtomicUsize,
}

#[cfg(feature = "ext4_another_backend")]
impl FlushFailsAfterMountDevice {
    fn new(inner: Arc<dyn BlockDevice>) -> Self {
        Self {
            inner,
            flush_count: AtomicUsize::new(0),
        }
    }
}

#[cfg(feature = "ext4_another_backend")]
impl BlockDevice for FlushFailsAfterMountDevice {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) -> BlockDeviceResult {
        self.inner.read_block(block_id, buf)
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) -> BlockDeviceResult {
        self.inner.write_block(block_id, buf)
    }

    fn flush(&self) -> BlockDeviceResult {
        if self.flush_count.fetch_add(1, Ordering::AcqRel) == 0 {
            self.inner.flush()
        } else {
            Err(BlockDeviceError::DeviceError)
        }
    }

    fn supports_reliable_flush(&self) -> bool {
        true
    }

    fn size_bytes(&self) -> Option<u64> {
        self.inner.size_bytes()
    }
}

/// Returns all another_ext4 bridge tests.
pub fn tests() -> alloc::vec::Vec<KernelTest> {
    #[cfg(feature = "ext4_another_backend")]
    {
        return vec![
            KernelTest::new(
                "ext4_another::rejects_unreliable_flush_before_media_parse",
                test_rejects_unreliable_flush_before_media_parse,
            ),
            KernelTest::new(
                "ext4_another::clean_media_supports_metadata_lookup_and_page_reads",
                test_clean_media_supports_metadata_lookup_and_page_reads,
            ),
            KernelTest::new(
                "ext4_another::writes_and_truncates_persist_across_independent_mounts",
                test_writes_and_truncates_persist_across_independent_mounts,
            ),
            KernelTest::new(
                "ext4_another::namespace_mutations_persist_across_independent_mounts",
                test_namespace_mutations_persist_across_independent_mounts,
            ),
            KernelTest::new(
                "ext4_another::metadata_mode_persists_across_independent_mounts",
                test_metadata_mode_persists_across_independent_mounts,
            ),
            KernelTest::new(
                "ext4_another::fsync_and_syncfs_surface_flush_failures",
                test_fsync_and_syncfs_surface_flush_failures,
            ),
        ];
    }

    #[cfg(not(feature = "ext4_another_backend"))]
    vec![]
}

#[cfg(feature = "ext4_another_backend")]
fn open_clean_media() -> Result<Arc<dyn FileSystem>, &'static str> {
    let device = crate::drivers::block::block_devices()[0]
        .clone()
        .ok_or("ktest requires a clean ext4 block device in slot 0")?;
    crate::fs::ext4_backend::open(device).map_err(|_| "clean ext4 image did not mount")
}

#[cfg(feature = "ext4_another_backend")]
#[cfg(feature = "ext4_another_backend")]
fn test_clean_media_supports_metadata_lookup_and_page_reads() -> Result<(), &'static str> {
    let fs = open_clean_media()?;
    let root = fs.root_inode();
    if root
        .metadata()
        .map_err(|_| "root metadata failed")?
        .file_type
        != FileType::Dir
    {
        return Err("ext4 root is not a directory");
    }
    let dot = root.find(".").map_err(|_| "root lookup for . failed")?;
    if dot.metadata().map_err(|_| "dot metadata failed")?.inode_id
        != root
            .metadata()
            .map_err(|_| "root metadata changed")?
            .inode_id
    {
        return Err("root lookup for . changed the inode id");
    }
    let entries = root
        .list_dirents()
        .map_err(|_| "root directory listing failed")?;
    for (name, _, file_type) in entries {
        if file_type != FileType::File {
            continue;
        }
        let file = root
            .find(&name)
            .map_err(|_| "directory entry lookup failed")?;
        let size = usize::try_from(file.metadata().map_err(|_| "file metadata failed")?.size)
            .map_err(|_| "file size does not fit usize")?;
        let cache = file
            .ensure_page_cache()
            .ok_or("regular file has no PageCache")?;
        let mut hole = [0xA5; PAGE_SIZE];
        cache
            .read(
                size.div_ceil(PAGE_SIZE).saturating_mul(PAGE_SIZE),
                &mut hole,
            )
            .map_err(|_| "PageCache backend failed to zero-fill an EOF hole")?;
        if hole.iter().any(|byte| *byte != 0) {
            return Err("PageCache EOF hole was not zero-filled");
        }
        if size > PAGE_SIZE {
            let mut cross_page = [0u8; 2];
            let private = spin::Mutex::new(FilePrivateData::Unused);
            let read = file
                .read_at(
                    PAGE_SIZE - 1,
                    cross_page.len(),
                    &mut cross_page,
                    private.lock(),
                )
                .map_err(|_| "cross-page regular-file read failed")?;
            if read != cross_page.len() {
                return Err("cross-page regular-file read was short");
            }
        }
        return Ok(());
    }
    Err("clean ext4 test image has no regular file at its root")
}

#[cfg(feature = "ext4_another_backend")]
#[cfg(feature = "ext4_another_backend")]
fn test_rejects_unreliable_flush_before_media_parse() -> Result<(), &'static str> {
    let result = crate::fs::ext4_backend::open(Arc::new(ZeroBlockDevice));
    match result {
        Err(SyscallErr::EROFS) => Ok(()),
        Err(_) => Err("another_ext4 parsed media before rejecting unreliable flush"),
        Ok(_) => Err("another_ext4 mounted a device without reliable flush"),
    }
}

#[cfg(feature = "ext4_another_backend")]
fn test_writes_and_truncates_persist_across_independent_mounts() -> Result<(), &'static str> {
    const NAME: &str = "another-wave4-data";
    const DATA: &[u8] = b"writeback";
    const TRUNCATED: &[u8] = b"write";

    let fs = open_clean_media()?;
    let root = fs.root_inode();
    let file = root
        .create(NAME, FileType::File, InodeMode::S_IRWXUGO)
        .map_err(|_| "create on writable another_ext4 mount failed")?;
    let private = spin::Mutex::new(FilePrivateData::Unused);
    file.open(private.lock(), &FileFlags::O_WRONLY)
        .map_err(|_| "writable another_ext4 file rejected O_WRONLY")?;
    let private = spin::Mutex::new(FilePrivateData::Unused);
    let written = file
        .write_at(0, DATA.len(), DATA, private.lock())
        .map_err(|_| "PageCache-backed write failed")?;
    if written != DATA.len() {
        return Err("PageCache-backed write was short");
    }
    file.resize(TRUNCATED.len())
        .map_err(|_| "truncate failed")?;
    file.sync().map_err(|_| "fsync failed")?;

    let remounted_root = open_clean_media()?.root_inode();
    let remounted_file = remounted_root
        .find(NAME)
        .map_err(|_| "file disappeared after independent remount")?;
    let mut readback = [0u8; TRUNCATED.len()];
    let private = spin::Mutex::new(FilePrivateData::Unused);
    let read = remounted_file
        .read_at(0, readback.len(), &mut readback, private.lock())
        .map_err(|_| "read after independent remount failed")?;
    if read != TRUNCATED.len() || readback != *TRUNCATED {
        return Err("writeback or truncate did not persist across independent remount");
    }

    root.unlink(NAME)
        .map_err(|_| "cleanup unlink failed after persistence check")?;
    root.sync().map_err(|_| "fsync after cleanup unlink failed")
}

#[cfg(feature = "ext4_another_backend")]
fn test_namespace_mutations_persist_across_independent_mounts() -> Result<(), &'static str> {
    const DIR: &str = "another-wave4-dir";
    const MOVED_DIR: &str = "another-wave4-dir-moved";
    const FILE: &str = "child";

    let fs = open_clean_media()?;
    let root = fs.root_inode();
    let dir = root
        .mkdir(DIR, InodeMode::S_IRWXUGO)
        .map_err(|_| "mkdir failed")?;
    dir.create(FILE, FileType::File, InodeMode::S_IRWXUGO)
        .map_err(|_| "create inside directory failed")?;
    root.rename(DIR, &root, MOVED_DIR, 0)
        .map_err(|_| "rename failed")?;
    let moved = root
        .find(MOVED_DIR)
        .map_err(|_| "renamed directory is not reachable")?;
    moved.unlink(FILE).map_err(|_| "unlink failed")?;
    root.rmdir(MOVED_DIR).map_err(|_| "rmdir failed")?;
    root.sync()
        .map_err(|_| "fsync after namespace mutations failed")?;

    match open_clean_media()?.root_inode().find(MOVED_DIR) {
        Err(SyscallErr::ENOENT) => Ok(()),
        _ => Err("rmdir did not persist across independent remount"),
    }
}

#[cfg(feature = "ext4_another_backend")]
fn test_metadata_mode_persists_across_independent_mounts() -> Result<(), &'static str> {
    const NAME: &str = "another-mode";
    const PERMISSIONS: InodeMode = InodeMode::S_IRWXUGO;

    let fs = open_clean_media()?;
    let root = fs.root_inode();
    let file = root
        .create(NAME, FileType::File, InodeMode::S_IRUSR)
        .map_err(|_| "create for metadata mode test failed")?;
    let mut metadata = file
        .metadata()
        .map_err(|_| "metadata before mode update failed")?;
    metadata.mode = (metadata.mode & InodeMode::S_IFMT) | PERMISSIONS;
    file.set_metadata(&metadata)
        .map_err(|_| "set_metadata rejected a mode update")?;
    file.sync().map_err(|_| "fsync after mode update failed")?;

    let remounted_file = open_clean_media()?
        .root_inode()
        .find(NAME)
        .map_err(|_| "file disappeared after mode update remount")?;
    let remounted_mode = remounted_file
        .metadata()
        .map_err(|_| "metadata after mode update remount failed")?
        .mode;
    if remounted_mode & InodeMode::S_IALLUGO != PERMISSIONS {
        return Err("mode update did not persist across independent remount");
    }

    root.unlink(NAME)
        .map_err(|_| "cleanup unlink after mode update failed")?;
    root.sync()
        .map_err(|_| "fsync after mode update cleanup failed")
}

#[cfg(feature = "ext4_another_backend")]
fn test_fsync_and_syncfs_surface_flush_failures() -> Result<(), &'static str> {
    let device = crate::drivers::block::block_devices()[0]
        .clone()
        .ok_or("ktest requires a clean ext4 block device in slot 0")?;
    if !device.supports_reliable_flush() {
        return Err("ktest fixture device lacks reliable flush support");
    }
    let fs = crate::fs::ext4_another::Ext4FileSystem::open(Arc::new(
        FlushFailsAfterMountDevice::new(device),
    ))
    .map_err(|_| "writable mount failed before the injected flush failure")?;
    let root = fs.root_inode();
    match root.sync() {
        Err(SyscallErr::EIO) => {}
        _ => return Err("fsync path hid the injected flush failure"),
    }
    match fs.sync_all() {
        Err(SyscallErr::EIO) => Ok(()),
        _ => Err("syncfs path hid the injected flush failure"),
    }
}
