use alloc::sync::Arc;

use crate::fs::vfs::{FileFlags, FilePrivateData, FileSystem, FileType, InodeMode};
use crate::utils::error::SyscallErr;

use super::fixtures::{ArmableFlushDevice, BarrierBlockDevice, FlushFailsAfterMountDevice};

pub(super) fn test_fsync_and_syncfs_surface_flush_failures() -> Result<(), &'static str> {
    let device = crate::drivers::block::get_block_device(0)
        .ok_or("ktest requires a clean ext4 root block device")?;
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

    let committed_fs = crate::fs::ext4_backend::open(committed_device)
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

    let committed_fs = crate::fs::ext4_backend::open(committed_device)
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
