use alloc::vec;

use crate::fs::vfs::{FilePrivateData, FileType, IndexNode};
use crate::utils::error::SyscallErr;

use super::fixtures::open_clean_media;

const SHORT_NAME: &str = "another-symlink-short";
const SHORT_TARGET: &str = "bin/busybox";
const LONG_NAME: &str = "another-symlink-long";
const LONG_TARGET: &str =
    "relative-target/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

pub(super) fn test_short_symlink_persists_across_clean_remount() -> Result<(), &'static str> {
    test_symlink_persists(SHORT_NAME, SHORT_TARGET)
}

pub(super) fn test_long_symlink_persists_across_clean_remount() -> Result<(), &'static str> {
    test_symlink_persists(LONG_NAME, LONG_TARGET)
}

fn test_symlink_persists(name: &str, target: &str) -> Result<(), &'static str> {
    let fs = open_clean_media()?;
    let root = fs.root_inode();
    remove_if_present(&root, name)?;

    let creation_result = (|| -> Result<(), &'static str> {
        let link = match root.symlink(name, target) {
            Ok(link) => link,
            Err(SyscallErr::EINVAL) => return Err("root.symlink returned EINVAL"),
            Err(_) => return Err("root.symlink returned an unexpected error"),
        };
        assert_link_target(&link, target)?;
        let looked_up = root
            .find(name)
            .map_err(|_| "created symlink was not reachable through parent lookup")?;
        assert_link_target(&looked_up, target)?;
        looked_up.sync().map_err(|_| "sync symlink failed")?;
        root.sync().map_err(|_| "sync parent failed")?;
        drop(looked_up);
        drop(link);
        Ok(())
    })();

    drop(root);
    fs.on_umount();
    drop(fs);
    creation_result?;

    let remounted_fs = open_clean_media()?;
    let remounted_root = remounted_fs.root_inode();
    let result = (|| -> Result<(), &'static str> {
        let link = remounted_root
            .find(name)
            .map_err(|_| "symlink disappeared after clean remount")?;
        assert_link_target(&link, target)?;
        drop(link);
        remounted_root
            .unlink(name)
            .map_err(|_| "symlink cleanup failed")?;
        remounted_root
            .sync()
            .map_err(|_| "sync after symlink cleanup failed")
    })();
    drop(remounted_root);
    remounted_fs.on_umount();
    result
}

fn assert_link_target(
    link: &alloc::sync::Arc<dyn IndexNode>,
    target: &str,
) -> Result<(), &'static str> {
    let metadata = link
        .metadata()
        .map_err(|_| "symlink metadata lookup failed")?;
    if metadata.file_type != FileType::SymLink {
        return Err("created inode is not a symbolic link");
    }

    let mut readback = vec![0; target.len()];
    let private = spin::Mutex::new(FilePrivateData::Unused);
    let read = link
        .read_at(0, readback.len(), &mut readback, private.lock())
        .map_err(|_| "readlink through VFS failed")?;
    if read != target.len() || readback != target.as_bytes() {
        return Err("readlink target did not match the created target");
    }
    Ok(())
}

fn remove_if_present(
    root: &alloc::sync::Arc<dyn IndexNode>,
    name: &str,
) -> Result<(), &'static str> {
    match root.unlink(name) {
        Ok(()) => root
            .sync()
            .map_err(|_| "sync after stale symlink cleanup failed"),
        Err(SyscallErr::ENOENT) => Ok(()),
        Err(_) => Err("stale symlink cleanup failed"),
    }
}
