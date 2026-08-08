use crate::fs::vfs::{FileFlags, FilePrivateData, FileType, IndexNode, InodeMode};
use crate::utils::error::SyscallErr;
use alloc::sync::Arc;

use super::fixtures::open_clean_media;

pub(super) fn test_reopen_before_sync_reads_fresh_pagecache_data() -> Result<(), &'static str> {
    const NAME: &str = "another-pagecache-reopen";
    const HEADER: &[u8] = b"\x7fELF";

    let fs = open_clean_media()?;
    let root = fs.root_inode();
    let file = root
        .create(NAME, FileType::File, InodeMode::S_IRWXUGO)
        .map_err(|_| "create for reopen test failed")?;
    let private = spin::Mutex::new(FilePrivateData::Unused);
    let written = file
        .write_at(0, HEADER.len(), HEADER, private.lock())
        .map_err(|_| "PageCache-backed header write failed")?;
    if written != HEADER.len() {
        return Err("PageCache-backed header write was short");
    }

    drop(file);
    let reopened = root.find(NAME).map_err(|_| "reopen before sync failed")?;
    let mut header = [0u8; HEADER.len()];
    let private = spin::Mutex::new(FilePrivateData::Unused);
    let read = reopened
        .read_at(0, header.len(), &mut header, private.lock())
        .map_err(|_| "reopen before sync read failed")?;
    if read != HEADER.len() || header != *HEADER {
        return Err("reopen before sync did not preserve the executable header");
    }
    reopened
        .sync()
        .map_err(|_| "sync after reopen test failed")?;
    root.unlink(NAME)
        .map_err(|_| "cleanup unlink after reopen test failed")?;
    root.sync()
        .map_err(|_| "sync after reopen test cleanup failed")
}

pub(super) fn test_writes_and_truncates_persist_across_independent_mounts(
) -> Result<(), &'static str> {
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

    let remounted_fs = open_clean_media()?;
    let remounted_root = remounted_fs.root_inode();
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

fn populate_depth_one_leading_hole(file: &Arc<dyn IndexNode>) -> Result<(), &'static str> {
    for lblock in [8usize, 10, 12, 14, 16] {
        let private = spin::Mutex::new(FilePrivateData::Unused);
        let written = file
            .write_at(lblock * another_ext4::BLOCK_SIZE, 1, b"x", private.lock())
            .map_err(|_| "sparse setup write failed")?;
        if written != 1 {
            return Err("sparse setup write was short");
        }
    }
    Ok(())
}

pub(super) fn test_depth_one_leading_hole_writes() -> Result<(), &'static str> {
    const NAME: &str = "another-depth-one-leading-hole-write";

    let fs = open_clean_media()?;
    let root = fs.root_inode();
    let file = root
        .create(NAME, FileType::File, InodeMode::S_IRWXUGO)
        .map_err(|_| "create for leading-hole write test failed")?;
    populate_depth_one_leading_hole(&file)?;

    let private = spin::Mutex::new(FilePrivateData::Unused);
    let written = file
        .write_at(0, 1, b"L", private.lock())
        .map_err(|_| "leading-hole write returned an error")?;
    if written != 1 {
        return Err("leading-hole write was short");
    }
    file.sync()
        .map_err(|_| "sync after leading-hole write failed")?;
    root.unlink(NAME)
        .map_err(|_| "cleanup unlink after leading-hole write failed")?;
    root.sync()
        .map_err(|_| "cleanup sync after leading-hole write failed")
}

pub(super) fn test_depth_one_leading_hole_truncate_succeeds() -> Result<(), &'static str> {
    const NAME: &str = "another-depth-one-leading-hole-truncate";

    let fs = open_clean_media()?;
    let root = fs.root_inode();
    let file = root
        .create(NAME, FileType::File, InodeMode::S_IRWXUGO)
        .map_err(|_| "create for leading-hole truncate test failed")?;
    populate_depth_one_leading_hole(&file)?;

    file.resize(7 * another_ext4::BLOCK_SIZE + 1)
        .map_err(|_| "leading-hole truncate returned an error")?;
    file.sync()
        .map_err(|_| "sync after leading-hole truncate failed")?;
    root.unlink(NAME)
        .map_err(|_| "cleanup unlink after leading-hole truncate failed")?;
    root.sync()
        .map_err(|_| "cleanup sync after leading-hole truncate failed")
}

pub(super) fn test_namespace_mutations_persist_across_independent_mounts(
) -> Result<(), &'static str> {
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

pub(super) fn test_metadata_mode_persists_across_independent_mounts() -> Result<(), &'static str> {
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
