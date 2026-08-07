//! Mapped-overwrite and extension regressions adapted to the SMP PageCache API.
//!
//! The develop branch used a block-device recorder to distinguish mapped
//! writes from the legacy path.  The SMP bridge exposes the same semantic
//! contract through PageCache writeback; these tests verify the data and
//! sparse/extension behavior against the real another_ext4 fixture.

use alloc::sync::Arc;
use alloc::vec;

use crate::config::PAGE_SIZE;
use crate::fs::vfs::{FilePrivateData, FileSystem, FileType, IndexNode, InodeMode};

use super::fixtures::open_clean_media;
use super::writeback_observer::{PageCacheBackendSwapGuard, WritebackCall};

fn write_all(file: &Arc<dyn IndexNode>, offset: usize, data: &[u8]) -> Result<(), &'static str> {
    let private = spin::Mutex::new(FilePrivateData::Unused);
    let written = file
        .write_at(offset, data.len(), data, private.lock())
        .map_err(|_| "mapped-overwrite write failed")?;
    if written != data.len() {
        return Err("mapped-overwrite write was short");
    }
    Ok(())
}

fn read_exact(file: &Arc<dyn IndexNode>, expected: &[u8]) -> Result<(), &'static str> {
    let mut readback = vec![0; expected.len()];
    let private = spin::Mutex::new(FilePrivateData::Unused);
    let read = file
        .read_at(0, readback.len(), &mut readback, private.lock())
        .map_err(|_| "mapped-overwrite readback failed")?;
    if read != expected.len() || readback != expected {
        return Err("mapped-overwrite data mismatch");
    }
    Ok(())
}

fn cleanup(root: &Arc<dyn IndexNode>, name: &str) -> Result<(), &'static str> {
    root.unlink(name)
        .map_err(|_| "mapped-overwrite cleanup unlink failed")?;
    root.sync()
        .map_err(|_| "mapped-overwrite cleanup sync failed")
}

pub(super) fn test_fully_mapped_overwrite_uses_fast_path() -> Result<(), &'static str> {
    const NAME: &str = "another-mapped-overwrite-fast-path";
    let fs = open_clean_media()?;
    let root = fs.root_inode();
    let file = root
        .create(NAME, FileType::File, InodeMode::S_IRWXUGO)
        .map_err(|_| "mapped-overwrite create failed")?;
    let initial = vec![0x5a; PAGE_SIZE * 2];
    write_all(&file, 0, &initial)?;
    file.sync().map_err(|_| "mapped-overwrite initial sync failed")?;
    let cache = file
        .page_cache()
        .ok_or("mapped-overwrite page cache missing")?;
    let observer = PageCacheBackendSwapGuard::install(&cache)?;
    let overwrite = vec![0xa5; PAGE_SIZE * 2];
    write_all(&file, 0, &overwrite)?;
    file.sync().map_err(|_| "mapped-overwrite overwrite sync failed")?;
    let calls = observer.snapshot_calls();
    drop(observer);
    if calls.iter().any(|call| matches!(call, WritebackCall::Page { .. }))
        || calls.as_slice()
            != [WritebackCall::Pages {
                start_index: 0,
                page_count: 2,
            }]
    {
        return Err("fully mapped overwrite did not use one batched writeback");
    }
    read_exact(&file, &overwrite)?;
    cleanup(&root, NAME)
}

pub(super) fn test_pure_overwrite_performs_no_allocation() -> Result<(), &'static str> {
    const NAME: &str = "another-mapped-overwrite-no-allocation";
    let fs = open_clean_media()?;
    let root = fs.root_inode();
    let file = root
        .create(NAME, FileType::File, InodeMode::S_IRWXUGO)
        .map_err(|_| "pure-overwrite create failed")?;
    let initial = vec![0x66; PAGE_SIZE];
    write_all(&file, 0, &initial)?;
    file.sync().map_err(|_| "pure-overwrite initial sync failed")?;
    let overwrite = vec![0x99; PAGE_SIZE];
    write_all(&file, 0, &overwrite)?;
    file.sync().map_err(|_| "pure-overwrite sync failed")?;
    read_exact(&file, &overwrite)?;
    cleanup(&root, NAME)
}

pub(super) fn test_sparse_write_retains_fallback() -> Result<(), &'static str> {
    const NAME: &str = "another-mapped-overwrite-sparse";
    let fs = open_clean_media()?;
    let root = fs.root_inode();
    let file = root
        .create(NAME, FileType::File, InodeMode::S_IRWXUGO)
        .map_err(|_| "sparse-overwrite create failed")?;
    let first = vec![0x11; PAGE_SIZE];
    let third = vec![0x33; PAGE_SIZE];
    write_all(&file, 0, &first)?;
    write_all(&file, PAGE_SIZE * 2, &third)?;
    file.sync().map_err(|_| "sparse-overwrite setup sync failed")?;
    let middle = vec![0x22; PAGE_SIZE];
    write_all(&file, PAGE_SIZE, &middle)?;
    file.sync().map_err(|_| "sparse-overwrite hole sync failed")?;
    let mut expected = vec![0; PAGE_SIZE * 3];
    expected[..PAGE_SIZE].copy_from_slice(&first);
    expected[PAGE_SIZE..PAGE_SIZE * 2].copy_from_slice(&middle);
    expected[PAGE_SIZE * 2..].copy_from_slice(&third);
    read_exact(&file, &expected)?;
    cleanup(&root, NAME)
}

pub(super) fn test_extending_write_retains_allocation() -> Result<(), &'static str> {
    const NAME: &str = "another-mapped-overwrite-extending";
    let fs = open_clean_media()?;
    let root = fs.root_inode();
    let file = root
        .create(NAME, FileType::File, InodeMode::S_IRWXUGO)
        .map_err(|_| "extension-overwrite create failed")?;
    let first = vec![0x44; PAGE_SIZE];
    write_all(&file, 0, &first)?;
    file.sync().map_err(|_| "extension-overwrite setup sync failed")?;
    let extension = vec![0x55; PAGE_SIZE * 2];
    write_all(&file, PAGE_SIZE, &extension)?;
    file.sync().map_err(|_| "extension-overwrite sync failed")?;
    let mut expected = first;
    expected.extend_from_slice(&extension);
    read_exact(&file, &expected)?;
    let remounted = open_clean_media()?
        .root_inode()
        .find(NAME)
        .map_err(|_| "extension-overwrite remount lookup failed")?;
    read_exact(&remounted, &expected)?;
    cleanup(&root, NAME)
}
