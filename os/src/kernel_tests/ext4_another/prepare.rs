use alloc::sync::Arc;

use another_ext4::{ErrCode, Ext4, InodeMode, BLOCK_SIZE, EXT4_ROOT_INO};

use super::fixtures::clean_media_device;
use super::recording_device::RecordingBlockDevice;
#[cfg(feature = "perf_diag")]
use crate::fs::ext4_another::{prepare_stats_snapshots, Ext4FileSystem};

const SECTORS_PER_BLOCK: u64 = (BLOCK_SIZE / 512) as u64;

pub(super) fn test_prepare_and_data_write_reject_unrepresentable_ranges() -> Result<(), &'static str>
{
    let device = Arc::new(RecordingBlockDevice::new(clean_media_device()?));
    let ext4 = Ext4::load_writable(device).map_err(|_| "prepare test mount failed")?;
    let mode = InodeMode::FILE | InodeMode::ALL_RWX;
    let file = ext4
        .generic_create(EXT4_ROOT_INO, "another-prepare-range-overflow", mode)
        .map_err(|_| "prepare range test create failed")?;
    let unrepresentable_offset = (u32::MAX as usize + 1) * BLOCK_SIZE;
    match ext4.prepare_buffered_write(file, unrepresentable_offset, 1, 0, None) {
        Err(error) if error.code() == ErrCode::EFBIG => {}
        _ => return Err("prepare accepted a nonempty unrepresentable range"),
    }
    match ext4.write_data_only(file, unrepresentable_offset, &[0xA5]) {
        Err(error) if error.code() == ErrCode::EFBIG => {}
        _ => return Err("write_data_only accepted a nonempty unrepresentable range"),
    }
    ext4.generic_remove(EXT4_ROOT_INO, "another-prepare-range-overflow")
        .map_err(|_| "prepare range test cleanup failed")?;
    ext4.shutdown_writable()
        .map_err(|_| "prepare range test shutdown failed")?;
    Ok(())
}

pub(super) fn test_prepare_recovers_i_blocks_after_partial_legacy_failure(
) -> Result<(), &'static str> {
    const BASELINE: &str = "another-prepare-recovery-baseline";
    const TARGET: &str = "another-prepare-recovery-target";

    let device = Arc::new(RecordingBlockDevice::new(clean_media_device()?));
    let ext4 = Ext4::load_writable(device.clone()).map_err(|_| "prepare recovery mount failed")?;
    let mode = InodeMode::FILE | InodeMode::ALL_RWX;
    let baseline = ext4
        .generic_create(EXT4_ROOT_INO, BASELINE, mode)
        .map_err(|_| "prepare recovery baseline create failed")?;
    device.start_recording();
    ext4.prepare_buffered_write(baseline, 0, BLOCK_SIZE, BLOCK_SIZE as u64, None)
        .map_err(|_| "prepare recovery baseline allocation failed")?;
    let baseline_writes = device.finish_recording().legacy_writes;
    if baseline_writes == 0 {
        return Err("prepare recovery baseline did not issue legacy writes");
    }

    let target = ext4
        .generic_create(EXT4_ROOT_INO, TARGET, mode)
        .map_err(|_| "prepare recovery target create failed")?;
    device.fail_next_legacy_write_after(baseline_writes - 1);
    match ext4.prepare_buffered_write(target, 0, BLOCK_SIZE, BLOCK_SIZE as u64, None) {
        Err(error) if error.code() == ErrCode::EIO => {}
        _ => return Err("prepare recovery did not retain the allocation error"),
    }
    let attr = ext4
        .getattr(target)
        .map_err(|_| "prepare recovery getattr failed")?;
    if attr.blocks != SECTORS_PER_BLOCK {
        return Err("prepare recovery left i_blocks stale after partial allocation");
    }
    ext4.shutdown_writable()
        .map_err(|_| "prepare recovery shutdown failed")?;
    drop(ext4);

    let remounted = Ext4::load_writable(device).map_err(|_| "prepare recovery remount failed")?;
    let remounted_target = remounted
        .generic_lookup(EXT4_ROOT_INO, TARGET)
        .map_err(|_| "prepare recovery target disappeared after remount")?;
    let remounted_attr = remounted
        .getattr(remounted_target)
        .map_err(|_| "prepare recovery remount getattr failed")?;
    if remounted_attr.blocks != SECTORS_PER_BLOCK {
        return Err("prepare recovery did not persist reconciled i_blocks across remount");
    }
    remounted
        .generic_remove(EXT4_ROOT_INO, TARGET)
        .map_err(|_| "prepare recovery target cleanup failed")?;
    remounted
        .generic_remove(EXT4_ROOT_INO, BASELINE)
        .map_err(|_| "prepare recovery baseline cleanup failed")?;
    remounted
        .shutdown_writable()
        .map_err(|_| "prepare recovery remount shutdown failed")?;
    Ok(())
}

#[cfg(feature = "perf_diag")]
pub(super) fn test_prepare_stats_change_through_registered_bridge() -> Result<(), &'static str> {
    let fs = Ext4FileSystem::open(clean_media_device()?)
        .map_err(|_| "prepare stats bridge mount failed")?;
    if !fs.inner().prepare_stats_enabled() {
        return Err("prepare stats were not enabled by the diagnostic build");
    }

    let fs_id = fs.fs_id();
    let before = prepare_stats_snapshots()
        .into_iter()
        .find(|(id, _)| *id == fs_id)
        .map(|(_, snapshot)| snapshot)
        .ok_or("prepare stats bridge did not expose the mounted filesystem")?;
    let file = fs
        .inner()
        .generic_create(
            EXT4_ROOT_INO,
            "another-prepare-stats-bridge",
            InodeMode::FILE | InodeMode::ALL_RWX,
        )
        .map_err(|_| "prepare stats bridge create failed")?;
    fs.inner()
        .prepare_buffered_write(file, 0, BLOCK_SIZE, BLOCK_SIZE as u64, None)
        .map_err(|_| "prepare stats bridge buffered prepare failed")?;
    let after = prepare_stats_snapshots()
        .into_iter()
        .find(|(id, _)| *id == fs_id)
        .map(|(_, snapshot)| snapshot)
        .ok_or("prepare stats bridge lost the mounted filesystem")?;
    if after.calls <= before.calls {
        return Err("buffered prepare did not advance the bridged call counter");
    }
    fs.inner()
        .generic_remove(EXT4_ROOT_INO, "another-prepare-stats-bridge")
        .map_err(|_| "prepare stats bridge cleanup failed")?;
    fs.inner()
        .shutdown_writable()
        .map_err(|_| "prepare stats bridge shutdown failed")?;
    Ok(())
}
