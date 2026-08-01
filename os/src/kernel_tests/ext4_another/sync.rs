use alloc::sync::Arc;

use crate::fs::vfs::{FileFlags, FilePrivateData, FileSystem, FileType, InodeMode};
use crate::utils::error::SyscallErr;

use super::fixtures::{BarrierBlockDevice, FlushFailsAfterMountDevice};

pub(super) fn test_fsync_and_syncfs_surface_flush_failures() -> Result<(), &'static str> {
    let device = crate::drivers::block::block_device_by_role(
        crate::drivers::block::BlockDeviceRole::Root,
    )
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

    let committed_device = crate::drivers::block::block_device_by_role(
        crate::drivers::block::BlockDeviceRole::Root,
    )
    .ok_or("ktest requires a clean ext4 root block device")?;
    if !committed_device.supports_reliable_flush() {
        return Err("ktest fixture device lacks reliable flush support");
    }
    let barrier_device = Arc::new(BarrierBlockDevice::new(committed_device.clone()));
    let fs = crate::fs::ext4_another::Ext4FileSystem::open(barrier_device)
        .map_err(|_| "barrier-backed another_ext4 mount failed")?;
    let root = fs.root_inode();
    let body = (|| -> Result<(), &'static str> {
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

        if crate::syscall::fs::sys_sync() != 0 {
            return Err("global sys_sync did not return success");
        }

        {
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
                .map_err(|_| {
                    "unwrapped committed device view could not read global sys_sync data"
                })?;
            if read != DATA.len() || readback != *DATA {
                return Err("global sys_sync did not persist another_ext4 data");
            }
        }

        Ok(())
    })();
    let cleanup = root
        .unlink(NAME)
        .and_then(|_| fs.sync_all())
        .map_err(|_| "cleanup after global sys_sync check failed");
    match (body, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}
