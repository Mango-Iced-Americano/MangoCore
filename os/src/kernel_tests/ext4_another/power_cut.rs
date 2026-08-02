use alloc::sync::Arc;

use crate::fs::vfs::{FileFlags, FilePrivateData, FileSystem, FileType, InodeMode};
use crate::hal::BLOCK_SZ;

use super::fixtures::BarrierBlockDevice;

pub(super) fn test_unsynced_close_power_cut_replays_consistently() -> Result<(), &'static str> {
    const DATA_LEN: usize = 64;
    const NAME: &str = "another-unsynced-close-power-cut-rerun-safe";
    const OLD: [u8; DATA_LEN] = [b'O'; DATA_LEN];
    const NEW: [u8; DATA_LEN] = [b'N'; DATA_LEN];

    let committed_device = crate::drivers::block::get_block_device(0)
        .ok_or("ktest requires a clean ext4 block device in slot 0")?;
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
            .map_err(|_| "create before power-cut simulation failed")?;
        let private = spin::Mutex::new(FilePrivateData::Unused);
        file.open(private.lock(), &FileFlags::O_WRONLY)
            .map_err(|_| "open before power-cut simulation failed")?;
        let private = spin::Mutex::new(FilePrivateData::Unused);
        let written = file
            .write_at(0, OLD.len(), &OLD, private.lock())
            .map_err(|_| "initial write before power-cut simulation failed")?;
        if written != OLD.len() {
            return Err("initial write before power-cut simulation was short");
        }
        file.sync()
            .map_err(|_| "initial fsync before power-cut simulation failed")?;

        let private = spin::Mutex::new(FilePrivateData::Unused);
        let written = file
            .write_at(0, NEW.len(), &NEW, private.lock())
            .map_err(|_| "unsynced overwrite before power-cut simulation failed")?;
        if written != NEW.len() {
            return Err("unsynced overwrite before power-cut simulation was short");
        }
        let private = spin::Mutex::new(FilePrivateData::Unused);
        file.close(private.lock())
            .map_err(|_| "close before simulated power cut failed")?;
    }

    let remounted_fs = crate::fs::ext4_backend::open(committed_device)
        .map_err(|_| "raw remount after simulated power cut failed")?;
    let remounted_root = remounted_fs.root_inode();
    let remounted_file = remounted_root
        .find(NAME)
        .map_err(|_| "file disappeared after simulated power cut")?;
    if remounted_file
        .metadata()
        .map_err(|_| "metadata after simulated power cut failed")?
        .size
        != DATA_LEN as i64
    {
        return Err("file length changed after simulated power cut");
    }
    let mut readback = [0u8; DATA_LEN];
    let private = spin::Mutex::new(FilePrivateData::Unused);
    let read = remounted_file
        .read_at(0, readback.len(), &mut readback, private.lock())
        .map_err(|_| "read after simulated power cut failed")?;
    if read != DATA_LEN || readback != OLD && readback != NEW {
        return Err("power-cut recovery produced a torn or unexpected overwrite");
    }
    remounted_file
        .sync()
        .map_err(|_| "fsync after power-cut recovery failed")?;
    drop(remounted_file);
    remounted_root
        .unlink(NAME)
        .and_then(|_| remounted_root.sync())
        .map_err(|_| "cleanup after power-cut recovery failed")?;
    drop(remounted_root);
    remounted_fs
        .on_umount()
        .map_err(|_| "clean unmount after power-cut recovery failed")
}

pub(super) fn test_close_then_clean_unmount_persists_and_clears_recover() -> Result<(), &'static str>
{
    const DATA: &[u8] = b"close-clean-unmount";
    const NAME: &str = "another-close-clean-unmount-rerun-safe";
    const EXT4_SUPERBLOCK_BLOCK: usize = 1024 / BLOCK_SZ;
    const FEATURE_INCOMPAT_OFFSET: usize = 0x60;
    const FEATURE_INCOMPAT_RECOVER: u32 = 0x0004;

    let committed_device = crate::drivers::block::get_block_device(0)
        .ok_or("ktest requires a clean ext4 block device in slot 0")?;
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
            .map_err(|_| "create before clean unmount check failed")?;
        let private = spin::Mutex::new(FilePrivateData::Unused);
        file.open(private.lock(), &FileFlags::O_WRONLY)
            .map_err(|_| "open before clean unmount check failed")?;
        let private = spin::Mutex::new(FilePrivateData::Unused);
        let written = file
            .write_at(0, DATA.len(), DATA, private.lock())
            .map_err(|_| "write before clean unmount check failed")?;
        if written != DATA.len() {
            return Err("write before clean unmount check was short");
        }
        let private = spin::Mutex::new(FilePrivateData::Unused);
        file.close(private.lock())
            .map_err(|_| "close before clean unmount check failed")?;
        drop(file);
        drop(root);
        fs.on_umount()
            .map_err(|_| "clean unmount after close failed")?;
    }

    let mut superblock = [0u8; BLOCK_SZ];
    committed_device
        .read_block(EXT4_SUPERBLOCK_BLOCK, &mut superblock)
        .map_err(|_| "raw superblock read after clean unmount failed")?;
    let incompatible_features = u32::from_le_bytes([
        superblock[FEATURE_INCOMPAT_OFFSET],
        superblock[FEATURE_INCOMPAT_OFFSET + 1],
        superblock[FEATURE_INCOMPAT_OFFSET + 2],
        superblock[FEATURE_INCOMPAT_OFFSET + 3],
    ]);
    if incompatible_features & FEATURE_INCOMPAT_RECOVER != 0 {
        return Err("clean unmount left FEATURE_INCOMPAT_RECOVER set on raw media");
    }

    let remounted_fs = crate::fs::ext4_backend::open(committed_device)
        .map_err(|_| "raw remount after clean unmount failed")?;
    let remounted_root = remounted_fs.root_inode();
    let remounted_file = remounted_root
        .find(NAME)
        .map_err(|_| "clean-unmount data disappeared after raw remount")?;
    let mut readback = [0u8; DATA.len()];
    let private = spin::Mutex::new(FilePrivateData::Unused);
    let read = remounted_file
        .read_at(0, readback.len(), &mut readback, private.lock())
        .map_err(|_| "raw remount could not read clean-unmount data")?;
    if read != DATA.len() || readback != *DATA {
        return Err("clean unmount did not persist target data");
    }
    drop(remounted_file);
    remounted_root
        .unlink(NAME)
        .and_then(|_| remounted_root.sync())
        .map_err(|_| "cleanup after clean-unmount check failed")?;
    drop(remounted_root);
    remounted_fs
        .on_umount()
        .map_err(|_| "final clean unmount after clean-unmount check failed")
}
