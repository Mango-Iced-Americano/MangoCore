use crate::fs::vfs::{FileFlags, FilePrivateData, FileType, InodeMode};
use crate::utils::error::SyscallErr;

use super::fixtures::open_clean_media;

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
