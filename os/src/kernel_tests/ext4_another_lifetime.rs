//! Generation-lifetime regressions for the feature-gated another_ext4 bridge.

#[cfg(feature = "ext4_another_backend")]
use crate::fs::vfs::{FileFlags, FilePrivateData, FileSystem, FileType, IndexNode, InodeMode};
use crate::kernel_tests::runner::KernelTest;
#[cfg(feature = "ext4_another_backend")]
use alloc::sync::Arc;

#[cfg(feature = "ext4_another_backend")]
#[path = "ext4_another_lifetime_sync.rs"]
mod sync;

/// Returns generation-aware another_ext4 lifetime tests.
pub(crate) fn tests() -> alloc::vec::Vec<KernelTest> {
    #[cfg(feature = "ext4_another_backend")]
    {
        return alloc::vec![
            KernelTest::new(
                "ext4_another::open_dirty_unlink_recreate_does_not_alias_old_inode",
                test_open_dirty_unlink_recreate_does_not_alias_old_inode,
            ),
            KernelTest::new(
                "ext4_another::rename_replacement_preserves_replaced_open_inode",
                test_rename_replacement_preserves_replaced_open_inode,
            ),
            KernelTest::new(
                "ext4_another::persists_non_aligned_eof_extension_across_early_writeback_and_cold_lookup",
                test_persists_non_aligned_eof_extension_across_early_writeback_and_cold_lookup,
            ),
            KernelTest::new(
                "ext4_another::partial_reclaim_still_runs_final_barrier_and_keeps_scoped_error",
                sync::test_partial_reclaim_still_runs_final_barrier_and_keeps_scoped_error,
            ),
        ];
    }

    #[cfg(not(feature = "ext4_another_backend"))]
    alloc::vec![]
}

#[cfg(feature = "ext4_another_backend")]
fn open_clean_media() -> Result<Arc<dyn FileSystem>, &'static str> {
    let device = crate::drivers::block::block_devices()[0]
        .clone()
        .ok_or("ktest requires a clean ext4 block device in slot 0")?;
    crate::fs::ext4_backend::open(device).map_err(|_| "clean ext4 image did not mount")
}

#[cfg(feature = "ext4_another_backend")]
fn write_open_file(inode: &Arc<dyn IndexNode>, data: &[u8]) -> Result<(), &'static str> {
    let private = spin::Mutex::new(FilePrivateData::Unused);
    inode
        .open(private.lock(), &FileFlags::O_WRONLY)
        .map_err(|_| "open of regular file failed")?;
    let private = spin::Mutex::new(FilePrivateData::Unused);
    let written = inode
        .write_at(0, data.len(), data, private.lock())
        .map_err(|_| "write to open regular file failed")?;
    if written != data.len() {
        return Err("write to open regular file was short");
    }
    Ok(())
}

#[cfg(feature = "ext4_another_backend")]
fn read_file(inode: &Arc<dyn IndexNode>, expected: &[u8]) -> Result<(), &'static str> {
    let mut readback = [0u8; 8];
    let private = spin::Mutex::new(FilePrivateData::Unused);
    let read = inode
        .read_at(
            0,
            expected.len(),
            &mut readback[..expected.len()],
            private.lock(),
        )
        .map_err(|_| "read of regular file failed")?;
    if read != expected.len() || readback[..expected.len()] != *expected {
        return Err("regular file data did not match its lifetime");
    }
    Ok(())
}

#[cfg(feature = "ext4_another_backend")]
fn test_open_dirty_unlink_recreate_does_not_alias_old_inode() -> Result<(), &'static str> {
    const NAME: &str = "another-wave1-unlink";
    const OLD: &[u8] = b"old!";
    const NEW: &[u8] = b"new!";

    let fs = open_clean_media()?;
    let root = fs.root_inode();
    let result = (|| -> Result<(), &'static str> {
        let old = root
            .create(NAME, FileType::File, InodeMode::S_IRWXUGO)
            .map_err(|_| "create old file failed")?;
        write_open_file(&old, OLD)?;
        root.unlink(NAME)
            .map_err(|_| "unlink old open file failed")?;
        old.metadata()
            .map_err(|_| "unlink immediately reclaimed the old open inode")?;

        let replacement = root
            .create(NAME, FileType::File, InodeMode::S_IRWXUGO)
            .map_err(|_| "recreate after unlink failed")?;
        write_open_file(&replacement, NEW)?;
        replacement.sync().map_err(|_| "sync replacement failed")?;

        write_open_file(&old, OLD)?;
        old.sync().map_err(|_| "sync old unlinked file failed")?;
        read_file(&replacement, NEW)
    })();
    let cleanup = root
        .unlink(NAME)
        .and_then(|_| root.sync())
        .map_err(|_| "cleanup after unlink/recreate lifetime test failed");
    match (result, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

#[cfg(feature = "ext4_another_backend")]
fn test_rename_replacement_preserves_replaced_open_inode() -> Result<(), &'static str> {
    const SOURCE: &str = "another-wave1-source";
    const TARGET: &str = "another-wave1-target";
    const OLD_TARGET: &[u8] = b"old-tgt";
    const SOURCE_DATA: &[u8] = b"source!";

    let fs = open_clean_media()?;
    let root = fs.root_inode();
    let result = (|| -> Result<(), &'static str> {
        let replaced = root
            .create(TARGET, FileType::File, InodeMode::S_IRWXUGO)
            .map_err(|_| "create replacement target failed")?;
        write_open_file(&replaced, OLD_TARGET)?;
        let source = root
            .create(SOURCE, FileType::File, InodeMode::S_IRWXUGO)
            .map_err(|_| "create rename source failed")?;
        write_open_file(&source, SOURCE_DATA)?;

        root.rename(SOURCE, &root, TARGET, 0)
            .map_err(|_| "rename replacement failed")?;
        write_open_file(&replaced, OLD_TARGET)?;
        replaced
            .sync()
            .map_err(|_| "sync replaced open inode failed")?;
        let replacement = root
            .find(TARGET)
            .map_err(|_| "replacement target disappeared")?;
        read_file(&replacement, SOURCE_DATA)
    })();
    let cleanup = root
        .unlink(TARGET)
        .and_then(|_| root.sync())
        .map_err(|_| "cleanup after rename replacement lifetime test failed");
    match (result, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

#[cfg(feature = "ext4_another_backend")]
fn test_persists_non_aligned_eof_extension_across_early_writeback_and_cold_lookup(
) -> Result<(), &'static str> {
    const NAME: &str = "another-wave1-eof-extension";
    const OFFSET: usize = 2;
    const PAYLOAD: &[u8] = b"abc";
    const EXPECTED: &[u8] = b"\0\0abc";

    let fs = open_clean_media()?;
    let root = fs.root_inode();
    let result = (|| -> Result<(), &'static str> {
        let inode = root
            .create(NAME, FileType::File, InodeMode::S_IRWXUGO)
            .map_err(|_| "create EOF extension persistence file failed")?;
        let private = spin::Mutex::new(FilePrivateData::Unused);
        inode
            .open(private.lock(), &FileFlags::O_WRONLY)
            .map_err(|_| "open EOF extension persistence file failed")?;
        let private = spin::Mutex::new(FilePrivateData::Unused);
        let written = inode
            .write_at(OFFSET, PAYLOAD.len(), PAYLOAD, private.lock())
            .map_err(|_| "nonzero-offset EOF extension write failed")?;
        if written != PAYLOAD.len() {
            return Err("nonzero-offset EOF extension write was short");
        }
        inode
            .sync()
            .map_err(|_| "per-inode sync failed before cold lookup")?;
        drop(inode);

        let cold_inode = root
            .find(NAME)
            .map_err(|_| "cold lookup failed after per-inode sync")?;
        read_file(&cold_inode, EXPECTED)
            .map_err(|_| "cold lookup lost zero gap or payload after per-inode sync")
    })();
    let cleanup = root
        .unlink(NAME)
        .and_then(|_| root.sync())
        .map_err(|_| "cleanup after EOF extension persistence test failed");
    match (result, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}
