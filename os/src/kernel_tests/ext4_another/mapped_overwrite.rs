use alloc::sync::Arc;
use alloc::vec;

use crate::config::PAGE_SIZE;
use crate::fs::ext4_another::Ext4FileSystem;
use crate::fs::vfs::{FilePrivateData, FileSystem, FileType, IndexNode, InodeMode};
use crate::utils::error::SyscallErr;

use super::fixtures::open_clean_media;
use super::recording_device::RecordingBlockDevice;
use super::writeback_observer::{PageCacheBackendSwapGuard, WritebackCall};

fn write_all(file: &Arc<dyn IndexNode>, offset: usize, data: &[u8]) -> Result<(), &'static str> {
    let private = spin::Mutex::new(FilePrivateData::Unused);
    let written = file
        .write_at(offset, data.len(), data, private.lock())
        .map_err(|_| "mapped-overwrite test write failed")?;
    if written != data.len() {
        return Err("mapped-overwrite test write was short");
    }
    Ok(())
}

fn assert_contents(file: &Arc<dyn IndexNode>, expected: &[u8]) -> Result<(), &'static str> {
    let mut readback = vec![0; expected.len()];
    let private = spin::Mutex::new(FilePrivateData::Unused);
    let read = file
        .read_at(0, readback.len(), &mut readback, private.lock())
        .map_err(|_| "mapped-overwrite test readback failed")?;
    if read != expected.len() || readback != expected {
        return Err("mapped-overwrite test data integrity check failed");
    }
    Ok(())
}

fn cleanup(root: &Arc<dyn IndexNode>, name: &str) -> Result<(), &'static str> {
    match root.unlink(name) {
        Ok(()) => root
            .sync()
            .map_err(|_| "mapped-overwrite test cleanup sync failed"),
        Err(SyscallErr::ENOENT) => Ok(()),
        Err(_) => Err("mapped-overwrite test cleanup unlink failed"),
    }
}

pub(super) fn test_fully_mapped_overwrite_uses_fast_path() -> Result<(), &'static str> {
    const NAME: &str = "another-mapped-overwrite-fast-path";

    let fs = open_clean_media()?;
    let root = fs.root_inode();
    let result = (|| {
        let file = root
            .create(NAME, FileType::File, InodeMode::S_IRWXUGO)
            .map_err(|_| "mapped-overwrite test create failed")?;
        let initial = vec![0x5A; PAGE_SIZE * 2];
        write_all(&file, 0, &initial)?;
        file.sync()
            .map_err(|_| "mapped-overwrite initial sync failed")?;

        let cache = file
            .page_cache()
            .ok_or("mapped-overwrite test page cache missing")?;
        let observer = PageCacheBackendSwapGuard::install(&cache)?;
        let overwrite = vec![0xA5; PAGE_SIZE * 2];
        write_all(&file, 0, &overwrite)?;
        let sync_result = file
            .sync()
            .map_err(|_| "mapped-overwrite overwrite sync failed");
        let calls = observer.snapshot_calls();
        drop(observer);
        sync_result?;

        if calls
            .iter()
            .any(|call| matches!(call, WritebackCall::Page { .. }))
        {
            return Err("fully mapped overwrite fell back to per-page writeback");
        }
        if calls.as_slice()
            != [WritebackCall::Pages {
                start_index: 0,
                page_count: 2,
            }]
        {
            return Err("fully mapped overwrite did not use one write_pages(0, 2) call");
        }
        assert_contents(&file, &overwrite)
    })();
    let cleanup_result = cleanup(&root, NAME);
    result.and(cleanup_result)
}

pub(super) fn test_sparse_write_retains_fallback() -> Result<(), &'static str> {
    const NAME: &str = "another-mapped-overwrite-sparse";

    let fs = open_clean_media()?;
    let root = fs.root_inode();
    let result = (|| {
        let file = root
            .create(NAME, FileType::File, InodeMode::S_IRWXUGO)
            .map_err(|_| "sparse mapped-overwrite test create failed")?;
        let first = vec![0x11; PAGE_SIZE];
        let third = vec![0x33; PAGE_SIZE];
        write_all(&file, 0, &first)?;
        write_all(&file, PAGE_SIZE * 2, &third)?;
        file.sync()
            .map_err(|_| "sparse mapped-overwrite initial sync failed")?;

        let middle = vec![0x22; PAGE_SIZE];
        write_all(&file, PAGE_SIZE, &middle)?;
        file.sync()
            .map_err(|_| "sparse mapped-overwrite hole sync failed")?;

        let mut expected = vec![0; PAGE_SIZE * 3];
        expected[..PAGE_SIZE].copy_from_slice(&first);
        expected[PAGE_SIZE..PAGE_SIZE * 2].copy_from_slice(&middle);
        expected[PAGE_SIZE * 2..].copy_from_slice(&third);
        assert_contents(&file, &expected)
    })();
    let cleanup_result = cleanup(&root, NAME);
    result.and(cleanup_result)
}

pub(super) fn test_extending_write_retains_allocation() -> Result<(), &'static str> {
    const NAME: &str = "another-mapped-overwrite-extending";

    let fs = open_clean_media()?;
    let root = fs.root_inode();
    let result = (|| {
        let file = root
            .create(NAME, FileType::File, InodeMode::S_IRWXUGO)
            .map_err(|_| "extending mapped-overwrite test create failed")?;
        let first = vec![0x44; PAGE_SIZE];
        write_all(&file, 0, &first)?;
        file.sync()
            .map_err(|_| "extending mapped-overwrite initial sync failed")?;

        let extension = vec![0x55; PAGE_SIZE * 2];
        write_all(&file, PAGE_SIZE, &extension)?;
        file.sync()
            .map_err(|_| "extending mapped-overwrite extension sync failed")?;

        let mut expected = first;
        expected.extend_from_slice(&extension);
        assert_contents(&file, &expected)?;

        let remounted_file = open_clean_media()?
            .root_inode()
            .find(NAME)
            .map_err(|_| "extending mapped-overwrite file disappeared after remount")?;
        assert_contents(&remounted_file, &expected)
    })();
    let cleanup_result = cleanup(&root, NAME);
    result.and(cleanup_result)
}

pub(super) fn test_pure_overwrite_performs_no_allocation() -> Result<(), &'static str> {
    const NAME: &str = "another-mapped-overwrite-no-allocation";

    let inner = crate::drivers::block::block_device_by_role(
        crate::drivers::block::BlockDeviceRole::Root,
    )
    .ok_or("ktest requires a clean ext4 root block device")?;
    let device = Arc::new(RecordingBlockDevice::new(inner));
    let fs = Ext4FileSystem::open(device.clone())
        .map_err(|_| "mapped-overwrite recording mount failed")?;
    let root = fs.root_inode();
    let result = (|| {
        let file = root
            .create(NAME, FileType::File, InodeMode::S_IRWXUGO)
            .map_err(|_| "pure mapped-overwrite test create failed")?;
        let initial = vec![0x66; PAGE_SIZE];
        write_all(&file, 0, &initial)?;
        file.sync()
            .map_err(|_| "pure mapped-overwrite initial sync failed")?;

        device.start_recording();
        let overwrite = vec![0x99; PAGE_SIZE];
        let overwrite_result = write_all(&file, 0, &overwrite).and_then(|_| {
            file.sync()
                .map_err(|_| "pure mapped-overwrite overwrite sync failed")
        });
        let recording = device.finish_recording();
        overwrite_result?;

        if recording.legacy_writes != 0 {
            return Err("pure mapped overwrite allocated metadata through the fallback path");
        }
        if recording.mango_runs.is_empty() {
            return Err("pure mapped overwrite issued no mapped data write");
        }
        assert_contents(&file, &overwrite)
    })();
    let cleanup_result = cleanup(&root, NAME);
    result.and(cleanup_result)
}
