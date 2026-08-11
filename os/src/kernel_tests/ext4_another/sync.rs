use alloc::sync::Arc;

use crate::fs::vfs::{FileFlags, FilePrivateData, FileSystem, FileType, InodeMode};
use crate::utils::error::SyscallErr;

use super::fixtures::{ArmableFlushDevice, BarrierBlockDevice};

pub(super) fn test_consecutive_unlinks_batch_until_sync() -> Result<(), &'static str> {
    const FIRST: &str = "another-unlink-batch-first";
    const SECOND: &str = "another-unlink-batch-second";

    let device = crate::drivers::block::get_block_device(0)
        .ok_or("ktest requires a clean ext4 root block device")?;
    if !device.supports_reliable_flush() {
        return Err("ktest fixture device lacks reliable flush support");
    }
    let barrier = Arc::new(BarrierBlockDevice::new(device));
    let fs = crate::fs::ext4_another::Ext4FileSystem::open(barrier.clone())
        .map_err(|_| "batching fixture mount failed")?;
    let root = fs.root_inode();
    let _ = root.unlink(FIRST);
    let _ = root.unlink(SECOND);
    root.sync().map_err(|_| "stale-name cleanup sync failed")?;
    root.create(FIRST, FileType::File, InodeMode::S_IRWXUGO)
        .map_err(|_| "first batching fixture create failed")?;
    root.create(SECOND, FileType::File, InodeMode::S_IRWXUGO)
        .map_err(|_| "second batching fixture create failed")?;
    root.sync()
        .map_err(|_| "batching fixture durability setup failed")?;

    let flushes_before = barrier.flush_count();
    root.unlink(FIRST)
        .map_err(|_| "first batched unlink failed")?;
    if barrier.flush_count() != flushes_before || root.find(FIRST).is_ok() {
        return Err("first unlink flushed or remained visible");
    }
    root.unlink(SECOND)
        .map_err(|_| "second batched unlink failed")?;
    if barrier.flush_count() != flushes_before || root.find(SECOND).is_ok() {
        return Err("second unlink flushed or remained visible");
    }

    root.sync().map_err(|_| "batched unlink sync failed")?;
    if barrier.flush_count() - flushes_before != 4 {
        return Err("deferred namespace sync did not use exactly four journal barriers");
    }
    Ok(())
}

pub(super) fn test_regular_namespace_mutations_share_one_sync_batch() -> Result<(), &'static str> {
    const SOURCE: &str = "another-batch-source";
    const TARGET: &str = "another-batch-target";
    const ALIAS: &str = "another-batch-alias";

    let device = crate::drivers::block::get_block_device(0)
        .ok_or("ktest requires a clean ext4 root block device")?;
    if !device.supports_reliable_flush() {
        return Err("ktest fixture device lacks reliable flush support");
    }
    let barrier = Arc::new(BarrierBlockDevice::new(device));
    let fs = crate::fs::ext4_another::Ext4FileSystem::open(barrier.clone())
        .map_err(|_| "namespace batching fixture mount failed")?;
    let root = fs.root_inode();
    let _ = root.unlink(ALIAS);
    let _ = root.unlink(SOURCE);
    let _ = root.unlink(TARGET);
    root.sync().map_err(|_| "batching fixture cleanup failed")?;
    let source = root
        .create(SOURCE, FileType::File, InodeMode::S_IRWXUGO)
        .map_err(|_| "batching source create failed")?;
    root.create(TARGET, FileType::File, InodeMode::S_IRWXUGO)
        .map_err(|_| "batching target create failed")?;
    root.sync()
        .map_err(|_| "batching fixture setup sync failed")?;

    let flushes_before = barrier.flush_count();
    root.link(ALIAS, &source)
        .map_err(|_| "batched hard link failed")?;
    root.unlink(ALIAS)
        .map_err(|_| "batched non-final unlink failed")?;
    root.rename(SOURCE, &root, TARGET, 0)
        .map_err(|_| "batched replacement rename failed")?;
    if barrier.flush_count() != flushes_before {
        return Err("regular namespace mutation forced an early journal commit");
    }

    root.sync()
        .map_err(|_| "regular namespace batch sync failed")?;
    if barrier.flush_count() - flushes_before != 4 {
        return Err("regular namespace batch was not committed by one four-phase transaction");
    }
    if root.find(SOURCE).is_ok() || root.find(ALIAS).is_ok() || root.find(TARGET).is_err() {
        return Err("batched namespace result is not visible after sync");
    }
    root.unlink(TARGET)
        .and_then(|_| root.sync())
        .map_err(|_| "regular namespace batching cleanup failed")
}

pub(super) fn test_failed_fsync_retains_dirty_generation_for_retry() -> Result<(), &'static str> {
    const NAME: &str = "another-fsync-retry";
    const DATA: &[u8] = b"retry-after-flush-error";

    let committed_device = crate::drivers::block::get_block_device(0)
        .ok_or("ktest requires a clean ext4 root block device")?;
    let barrier = Arc::new(BarrierBlockDevice::new(committed_device.clone()));
    let flush_device = Arc::new(ArmableFlushDevice::new(barrier));
    let fs = crate::fs::ext4_another::Ext4FileSystem::open(flush_device.clone())
        .map_err(|_| "fsync retry fixture mount failed")?;
    let root = fs.root_inode();
    let _ = root.unlink(NAME);
    root.sync()
        .map_err(|_| "fsync retry fixture cleanup failed")?;
    let file = root
        .create(NAME, FileType::File, InodeMode::S_IRWXUGO)
        .map_err(|_| "fsync retry file create failed")?;
    let private = spin::Mutex::new(FilePrivateData::Unused);
    let written = file
        .write_at(0, DATA.len(), DATA, private.lock())
        .map_err(|_| "fsync retry write failed")?;
    if written != DATA.len() {
        return Err("fsync retry write was short");
    }

    // Drain data writeback and its allocation/mapped-write journal first so
    // the injected failure lands on the final retryable durability boundary.
    // A failure during an in-flight four-phase journal commit is intentionally
    // fail-stop because its media state may be uncertain; that separate
    // contract is covered by the another_ext4 journal transaction tests.
    file.page_cache()
        .ok_or("fsync retry file has no page cache")?
        .writeback_all_before_io_gate()
        .map_err(|_| "fsync retry writeback preparation failed")?;
    fs.flush_device()
        .map_err(|_| "fsync retry journal preparation failed")?;

    flush_device.arm_failure();
    if file.sync() != Err(SyscallErr::EIO) {
        return Err("armed fsync did not surface the flush failure");
    }
    flush_device.disarm_failure();
    file.sync()
        .map_err(|_| "dirty generation was not retryable after flush failure")?;
    drop(file);
    drop(root);
    drop(fs);

    let committed_fs = crate::fs::ext4_backend::open(committed_device, false)
        .map_err(|_| "committed view did not mount after fsync retry")?;
    let committed_root = committed_fs.root_inode();
    let committed_file = committed_root
        .find(NAME)
        .map_err(|_| "fsync retry did not persist the file")?;
    let mut readback = [0u8; DATA.len()];
    let private = spin::Mutex::new(FilePrivateData::Unused);
    let read = committed_file
        .read_at(0, readback.len(), &mut readback, private.lock())
        .map_err(|_| "fsync retry data read failed")?;
    if read != DATA.len() || readback != *DATA {
        return Err("fsync retry lost data after the successful boundary");
    }
    drop(committed_file);
    committed_root
        .unlink(NAME)
        .and_then(|_| committed_root.sync())
        .map_err(|_| "fsync retry cleanup failed")
}

pub(super) fn test_fsync_and_syncfs_surface_flush_failures() -> Result<(), &'static str> {
    let device = crate::drivers::block::get_block_device(0)
        .ok_or("ktest requires a clean ext4 root block device")?;
    if !device.supports_reliable_flush() {
        return Err("ktest fixture device lacks reliable flush support");
    }
    let flush_device = Arc::new(ArmableFlushDevice::new(device));
    let fs = crate::fs::ext4_another::Ext4FileSystem::open(flush_device.clone())
        .map_err(|_| "writable mount failed before the injected flush failure")?;
    let root = fs.root_inode();
    flush_device.arm_failure();
    match root.sync() {
        Err(SyscallErr::EIO) => {}
        _ => return Err("fsync path hid the injected flush failure"),
    }
    match fs.sync_all() {
        Err(SyscallErr::EIO) => Ok(()),
        _ => Err("syncfs path hid the injected flush failure"),
    }
}

pub(super) fn test_global_sys_sync_persists_across_unwrapped_device_view(
) -> Result<(), &'static str> {
    const DATA: &[u8] = b"global-sync";
    const NAME: &str = "another-global-sync-rerun-safe";

    let committed_device = crate::drivers::block::get_block_device(0)
        .ok_or("ktest requires a clean ext4 root block device")?;
    if !committed_device.supports_reliable_flush() {
        return Err("ktest fixture device lacks reliable flush support");
    }
    {
        let barrier_device = Arc::new(BarrierBlockDevice::new(committed_device.clone()));
        let fs = crate::fs::ext4_another::Ext4FileSystem::open(barrier_device)
            .map_err(|_| "barrier-backed another_ext4 mount failed")?;
        let root = fs.root_inode();
        let file = root
            .create(NAME, FileType::File, InodeMode::S_IRWXUGO)
            .map_err(|_| "create before global sys_sync failed")?;
        let private = spin::Mutex::new(FilePrivateData::Unused);
        file.open(private.lock(), &FileFlags::O_WRONLY)
            .map_err(|_| "open before global sys_sync failed")?;
        let private = spin::Mutex::new(FilePrivateData::Unused);
        let written = file
            .write_at(0, DATA.len(), DATA, private.lock())
            .map_err(|_| "write before global sys_sync failed")?;
        if written != DATA.len() {
            return Err("write before global sys_sync was short");
        }
        let private = spin::Mutex::new(FilePrivateData::Unused);
        file.close(private.lock())
            .map_err(|_| "close before global sys_sync failed")?;

        if crate::syscall::fs::sys_sync() != 0 {
            return Err("global sys_sync did not return success");
        }
    }

    let committed_fs = crate::fs::ext4_backend::open(committed_device, false)
        .map_err(|_| "unwrapped committed device view did not mount")?;
    let committed_root = committed_fs.root_inode();
    let committed_file = committed_root
        .find(NAME)
        .map_err(|_| "global sys_sync did not persist another_ext4 data")?;
    let mut readback = [0u8; DATA.len()];
    let private = spin::Mutex::new(FilePrivateData::Unused);
    let read = committed_file
        .read_at(0, readback.len(), &mut readback, private.lock())
        .map_err(|_| "unwrapped committed device view could not read global sys_sync data")?;
    if read != DATA.len() || readback != *DATA {
        return Err("global sys_sync did not persist another_ext4 data");
    }
    drop(committed_file);
    committed_root
        .unlink(NAME)
        .and_then(|_| committed_root.sync())
        .map_err(|_| "cleanup after global sys_sync check failed")
}

pub(super) fn test_close_does_not_trigger_durability_and_later_fsync_persists(
) -> Result<(), &'static str> {
    const DATA: &[u8] = b"close-then-fsync";
    const NAME: &str = "another-close-then-fsync-rerun-safe";

    let committed_device = crate::drivers::block::get_block_device(0)
        .ok_or("ktest requires a clean ext4 block device in slot 0")?;
    if !committed_device.supports_reliable_flush() {
        return Err("ktest fixture device lacks reliable flush support");
    }
    {
        let barrier_device = Arc::new(BarrierBlockDevice::new(committed_device.clone()));
        let flush_device = Arc::new(ArmableFlushDevice::new(barrier_device.clone()));
        let fs = crate::fs::ext4_another::Ext4FileSystem::open(flush_device.clone())
            .map_err(|_| "barrier-backed another_ext4 mount failed")?;
        let root = fs.root_inode();
        let file = root
            .create(NAME, FileType::File, InodeMode::S_IRWXUGO)
            .map_err(|_| "create before close durability check failed")?;
        let private = spin::Mutex::new(FilePrivateData::Unused);
        file.open(private.lock(), &FileFlags::O_WRONLY)
            .map_err(|_| "open before close durability check failed")?;
        let private = spin::Mutex::new(FilePrivateData::Unused);
        let written = file
            .write_at(0, DATA.len(), DATA, private.lock())
            .map_err(|_| "write before close durability check failed")?;
        if written != DATA.len() {
            return Err("write before close durability check was short");
        }

        let writes_before_close = barrier_device.write_count();
        let flushes_before_close = barrier_device.flush_count();
        let attempts_before_close = flush_device.flush_count();
        flush_device.arm_failure();
        let private = spin::Mutex::new(FilePrivateData::Unused);
        file.close(private.lock())
            .map_err(|_| "close unexpectedly attempted persistence")?;
        if barrier_device.write_count() != writes_before_close {
            return Err("close unexpectedly triggered writeback");
        }
        if barrier_device.flush_count() != flushes_before_close
            || flush_device.flush_count() != attempts_before_close
        {
            return Err("close unexpectedly triggered a persistence barrier");
        }

        flush_device.disarm_failure();
        file.sync()
            .map_err(|_| "fsync after close did not persist data")?;
    }

    let committed_fs = crate::fs::ext4_backend::open(committed_device, false)
        .map_err(|_| "unwrapped committed device view did not mount after fsync")?;
    let committed_root = committed_fs.root_inode();
    let committed_file = committed_root
        .find(NAME)
        .map_err(|_| "fsync after close did not persist the file")?;
    let mut readback = [0u8; DATA.len()];
    let private = spin::Mutex::new(FilePrivateData::Unused);
    let read = committed_file
        .read_at(0, readback.len(), &mut readback, private.lock())
        .map_err(|_| "unwrapped committed device view could not read fsync data")?;
    if read != DATA.len() || readback != *DATA {
        return Err("fsync after close did not persist the target data");
    }
    drop(committed_file);
    committed_root
        .unlink(NAME)
        .and_then(|_| committed_root.sync())
        .map_err(|_| "cleanup after close durability check failed")
}
