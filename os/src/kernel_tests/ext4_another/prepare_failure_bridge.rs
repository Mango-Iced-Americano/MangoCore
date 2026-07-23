//! Failure/retry coverage through the registered Mango another_ext4 bridge.

use alloc::sync::Arc;

use super::fixtures::clean_media_device;
use super::recording_device::RecordingBlockDevice;
use crate::fs::ext4_another::{prepare_stats_snapshots, Ext4FileSystem};
use crate::fs::vfs::{FileFlags, FilePrivateData, FileSystem, FileType, InodeMode};

const BASELINE: &str = "another-prepare-bridge-baseline";
const TARGET: &str = "another-prepare-bridge-retry";
const DATA: [u8; 1024] = [0xD4; 1024];

fn stats(fs_id: usize) -> Result<another_ext4::PrepareStatsSnapshot, &'static str> {
    prepare_stats_snapshots()
        .into_iter()
        .find(|(id, _)| *id == fs_id)
        .map(|(_, snapshot)| snapshot)
        .ok_or("bridge failure test did not find registered filesystem stats")
}

pub(super) fn test_write_at_prepare_failure_retries_without_stale_cache() -> Result<(), &'static str> {
    let device = Arc::new(RecordingBlockDevice::new(clean_media_device()?));
    let fs = Ext4FileSystem::open(device.clone()).map_err(|_| "bridge failure mount failed")?;
    let fs_id = fs.fs_id();
    let root = fs.root_inode();
    let baseline = root
        .create(BASELINE, FileType::File, InodeMode::S_IRWXUGO)
        .map_err(|_| "bridge baseline create failed")?;
    baseline
        .open(spin::Mutex::new(FilePrivateData::Unused).lock(), &FileFlags::O_WRONLY)
        .map_err(|_| "bridge baseline open failed")?;
    device.start_recording();
    baseline
        .write_at(0, DATA.len(), &DATA, spin::Mutex::new(FilePrivateData::Unused).lock())
        .map_err(|_| "bridge baseline write_at failed")?;
    let writes = device.finish_recording().mango_runs.len();
    if writes == 0 {
        return Err("bridge baseline write_at did not reach the Mango block bridge");
    }

    let target = root
        .create(TARGET, FileType::File, InodeMode::S_IRWXUGO)
        .map_err(|_| "bridge target create failed")?;
    target
        .open(spin::Mutex::new(FilePrivateData::Unused).lock(), &FileFlags::O_WRONLY)
        .map_err(|_| "bridge target open failed")?;
    device.fail_next_mango_write_after(writes - 1);
    if target
        .write_at(0, DATA.len(), &DATA, spin::Mutex::new(FilePrivateData::Unused).lock())
        .is_ok()
    {
        return Err("bridge injected prepare failure unexpectedly succeeded");
    }
    // The retry delta begins after the injected failure, so it cannot be
    // satisfied by extent queries performed by the failed attempt.
    let before_retry = stats(fs_id)?;
    target
        .write_at(0, DATA.len(), &DATA, spin::Mutex::new(FilePrivateData::Unused).lock())
        .map_err(|_| "bridge retry write_at failed")?;
    let after_retry = stats(fs_id)?;
    if after_retry
        .extent_query_attempts
        .wrapping_sub(before_retry.extent_query_attempts)
        == 0
    {
        return Err("bridge retry did not execute a real extent query");
    }
    target.sync().map_err(|_| "bridge retry sync failed")?;
    let mut readback = [0u8; DATA.len()];
    if target
        .read_at(0, readback.len(), &mut readback, spin::Mutex::new(FilePrivateData::Unused).lock())
        .map_err(|_| "bridge retry readback failed")?
        != DATA.len()
        || readback != DATA
    {
        return Err("bridge retry published stale mapping or lost data");
    }
    Ok(())
}
