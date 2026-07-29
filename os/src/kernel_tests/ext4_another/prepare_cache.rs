//! Buffered-prepare positive-mapping cache regressions.

use alloc::sync::Arc;
use alloc::vec;

use another_ext4::{ErrCode, Ext4, InodeMode, BLOCK_SIZE, EXT4_ROOT_INO};

use super::fixtures::clean_media_device;
use super::recording_device::RecordingBlockDevice;
use crate::fs::ext4_another::{prepare_stats_snapshots, Ext4FileSystem};
use crate::fs::vfs::{FileFlags, FilePrivateData, FileSystem, FileType, InodeMode as VfsInodeMode};

const NAME: &str = "another-prepare-cache";
const WRITE_SIZE: usize = 1024;
const FIRST: [u8; WRITE_SIZE] = [0xA5; WRITE_SIZE];
const SECOND: [u8; WRITE_SIZE] = [0x5A; WRITE_SIZE];
const HOLE_WRITE: [u8; WRITE_SIZE] = [0x3C; WRITE_SIZE];

struct RestoreStatsOn(bool);

impl Drop for RestoreStatsOn {
    fn drop(&mut self) {
        crate::task::perf::STATS_ON.store(self.0, core::sync::atomic::Ordering::Relaxed);
    }
}

fn stats(fs_id: usize) -> Result<another_ext4::PrepareStatsSnapshot, &'static str> {
    prepare_stats_snapshots()
        .into_iter()
        .find(|(id, _)| *id == fs_id)
        .map(|(_, snapshot)| snapshot)
        .ok_or("prepare cache stats bridge did not expose the mounted filesystem")
}

pub(super) fn test_mapped_prepare_cache_invalidates_on_resize_and_skips_holes(
) -> Result<(), &'static str> {
    let restore_stats_on = RestoreStatsOn(crate::task::perf::STATS_ON.swap(
        true,
        core::sync::atomic::Ordering::Relaxed,
    ));
    let fs = Ext4FileSystem::open(clean_media_device()?)
        .map_err(|_| "prepare cache mount failed")?;
    if !fs.inner().prepare_stats_enabled() {
        return Err("prepare cache test requires perf_diag");
    }
    let fs_id = fs.fs_id();
    let root = fs.root_inode();
    let file = root
        .create(NAME, FileType::File, VfsInodeMode::S_IRWXUGO)
        .map_err(|_| "prepare cache create failed")?;
    let private = spin::Mutex::new(FilePrivateData::Unused);
    file.open(private.lock(), &FileFlags::O_WRONLY)
        .map_err(|_| "prepare cache open failed")?;

    let private = spin::Mutex::new(FilePrivateData::Unused);
    file.write_at(0, FIRST.len(), &FIRST, private.lock())
        .map_err(|_| "prepare cache initial write failed")?;
    let after_initial = stats(fs_id)?;

    let private = spin::Mutex::new(FilePrivateData::Unused);
    file.write_at(WRITE_SIZE, SECOND.len(), &SECOND, private.lock())
        .map_err(|_| "prepare cache mapped write failed")?;
    let after_mapped = stats(fs_id)?;
    if after_mapped
        .extent_query_calls
        .wrapping_sub(after_initial.extent_query_calls)
        != 0
    {
        return Err("mapped buffered prepare re-queried the extent tree");
    }

    file.resize(0)
        .map_err(|_| "prepare cache resize invalidation failed")?;
    let after_resize = stats(fs_id)?;
    let private = spin::Mutex::new(FilePrivateData::Unused);
    file.write_at(0, FIRST.len(), &FIRST, private.lock())
        .map_err(|_| "prepare cache post-resize write failed")?;
    let after_reprepare = stats(fs_id)?;
    if after_reprepare
        .extent_query_calls
        .wrapping_sub(after_resize.extent_query_calls)
        == 0
    {
        return Err("resize invalidation allowed a stale mapped prepare hit");
    }

    let private = spin::Mutex::new(FilePrivateData::Unused);
    file.write_at(BLOCK_SIZE * 2, HOLE_WRITE.len(), &HOLE_WRITE, private.lock())
        .map_err(|_| "prepare cache sparse-hole write failed")?;
    let after_hole = stats(fs_id)?;
    if after_hole
        .allocation_calls
        .wrapping_sub(after_reprepare.allocation_calls)
        == 0
    {
        return Err("positive mapping cache treated a sparse hole as mapped");
    }

    file.sync()
        .map_err(|_| "prepare cache fsync failed")?;
    drop(file);
    drop(root);
    fs.on_umount();
    drop(fs);

    let remounted = Ext4FileSystem::open(clean_media_device()?)
        .map_err(|_| "prepare cache remount failed")?;
    let root = remounted.root_inode();
    let file = root
        .find(NAME)
        .map_err(|_| "prepare cache file disappeared after remount")?;
    let mut first = [0u8; WRITE_SIZE];
    let private = spin::Mutex::new(FilePrivateData::Unused);
    let read = file
        .read_at(0, first.len(), &mut first, private.lock())
        .map_err(|_| "prepare cache remount first read failed")?;
    if read != FIRST.len() || first != FIRST {
        return Err("prepare cache fsync/remount lost the first mapping");
    }
    let mut sparse = [0u8; WRITE_SIZE];
    let private = spin::Mutex::new(FilePrivateData::Unused);
    let read = file
        .read_at(BLOCK_SIZE * 2, sparse.len(), &mut sparse, private.lock())
        .map_err(|_| "prepare cache remount sparse read failed")?;
    if read != HOLE_WRITE.len() || sparse != HOLE_WRITE {
        return Err("prepare cache fsync/remount lost sparse mapped data");
    }
    root.unlink(NAME)
        .map_err(|_| "prepare cache cleanup unlink failed")?;
    root.sync()
        .map_err(|_| "prepare cache cleanup sync failed")?;
    drop(restore_stats_on);
    Ok(())
}

pub(super) fn test_failed_prepare_does_not_cache_retry_mapping() -> Result<(), &'static str> {
    const BASELINE: &str = "another-prepare-cache-baseline";
    const TARGET: &str = "another-prepare-cache-retry";

    let device = Arc::new(RecordingBlockDevice::new(clean_media_device()?));
    let ext4 = Ext4::load_writable(device.clone()).map_err(|_| "prepare cache retry mount failed")?;
    let mode = InodeMode::FILE | InodeMode::ALL_RWX;
    let baseline = ext4
        .generic_create(EXT4_ROOT_INO, BASELINE, mode)
        .map_err(|_| "prepare cache retry baseline create failed")?;
    let before_probe_miss = ext4.prepare_stats_snapshot();
    device.start_recording();
    ext4
        .prepare_buffered_write(baseline, 0, BLOCK_SIZE, BLOCK_SIZE as u64, None)
        .map_err(|_| "prepare cache retry baseline prepare failed")?;
    let after_probe_miss = ext4.prepare_stats_snapshot();
    if after_probe_miss
        .extent_query_attempts
        .wrapping_sub(before_probe_miss.extent_query_attempts)
        < 3
    {
        return Err("probe miss did not reject publication after its mapping transaction changed epoch");
    }
    let writes = device.finish_recording().legacy_writes;
    if writes == 0 {
        return Err("prepare cache retry baseline did not record writes");
    }

    let target = ext4
        .generic_create(EXT4_ROOT_INO, TARGET, mode)
        .map_err(|_| "prepare cache retry target create failed")?;
    device.fail_next_legacy_write_after(writes - 1);
    match ext4.prepare_buffered_write(target, 0, BLOCK_SIZE, BLOCK_SIZE as u64, None) {
        Err(error) if error.code() == ErrCode::EIO => {}
        _ => return Err("failed prepare did not report its allocation error"),
    }
    let before_retry = ext4.prepare_stats_snapshot();
    ext4
        .prepare_buffered_write(target, 0, BLOCK_SIZE, BLOCK_SIZE as u64, None)
        .map_err(|_| "failed prepare left a stale cached mapping for retry")?;
    let after_retry = ext4.prepare_stats_snapshot();
    if after_retry
        .extent_query_attempts
        .wrapping_sub(before_retry.extent_query_attempts)
        == 0
    {
        return Err("failed prepare retry did not execute a real extent query");
    }
    ext4
        .write_data_only(target, 0, &vec![0xC3; BLOCK_SIZE])
        .map_err(|_| "prepare retry writeback failed")?;
    ext4
        .commit_inode_size(target, BLOCK_SIZE as u64, None)
        .map_err(|_| "prepare retry size commit failed")?;
    ext4
        .generic_remove(EXT4_ROOT_INO, TARGET)
        .map_err(|_| "prepare cache retry target cleanup failed")?;
    ext4
        .generic_remove(EXT4_ROOT_INO, BASELINE)
        .map_err(|_| "prepare cache retry baseline cleanup failed")?;
    ext4
        .shutdown_writable()
        .map_err(|_| "prepare cache retry shutdown failed")
}

pub(super) fn test_direct_range_commit_invalidates_prepared_cache() -> Result<(), &'static str> {
    const TARGET: &str = "another-prepare-cache-direct-range";

    let restore_stats_on = RestoreStatsOn(crate::task::perf::STATS_ON.swap(
        true,
        core::sync::atomic::Ordering::Relaxed,
    ));
    let device = Arc::new(RecordingBlockDevice::new(clean_media_device()?));
    let ext4 = Ext4::load_writable(device)
        .map_err(|_| "prepare cache direct-range mount failed")?;
    let mode = InodeMode::FILE | InodeMode::ALL_RWX;
    let target = ext4
        .generic_create(EXT4_ROOT_INO, TARGET, mode)
        .map_err(|_| "prepare cache direct-range create failed")?;

    ext4
        .prepare_buffered_write(target, 0, BLOCK_SIZE, BLOCK_SIZE as u64, None)
        .map_err(|_| "prepare cache direct-range initial prepare failed")?;
    ext4
        .commit_inode_size(target, BLOCK_SIZE as u64, None)
        .map_err(|_| "prepare cache direct-range initial size commit failed")?;

    let before_direct = ext4.prepare_stats_snapshot();
    ext4
        .prepare_buffered_write(target, BLOCK_SIZE, BLOCK_SIZE * 16, (BLOCK_SIZE * 17) as u64, None)
        .map_err(|_| "prepare cache direct-range prepare failed")?;
    let after_direct = ext4.prepare_stats_snapshot();
    if after_direct.zero_io.wrapping_sub(before_direct.zero_io) == 0 {
        return Err("direct-range test did not exercise the direct-range transaction");
    }

    ext4
        .prepare_buffered_write(target, 0, BLOCK_SIZE, (BLOCK_SIZE * 17) as u64, None)
        .map_err(|_| "prepare cache direct-range post-commit prepare failed")?;
    let after_requery = ext4.prepare_stats_snapshot();
    if after_requery
        .extent_query_attempts
        .wrapping_sub(after_direct.extent_query_attempts)
        == 0
    {
        return Err("direct-range commit left a stale prepared cache entry");
    }

    ext4
        .generic_remove(EXT4_ROOT_INO, TARGET)
        .map_err(|_| "prepare cache direct-range cleanup failed")?;
    ext4
        .shutdown_writable()
        .map_err(|_| "prepare cache direct-range shutdown failed")?;
    drop(restore_stats_on);
    Ok(())
}
