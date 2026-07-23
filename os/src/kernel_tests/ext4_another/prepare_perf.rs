//! Bounded buffered-write allocation diagnostics for another_ext4.

use another_ext4::BLOCK_SIZE;

use super::fixtures::clean_media_device;
use crate::fs::ext4_another::{prepare_stats_snapshots, Ext4FileSystem};
use crate::fs::vfs::{FileFlags, FilePrivateData, FileSystem, FileType, InodeMode as VfsInodeMode};
use crate::utils::error::SyscallErr;

const NAME: &str = "another-prepare-1k-append";
const WRITE_SIZE: usize = 1024;
const WRITES: usize = 64;
const DATA: [u8; WRITE_SIZE] = [0xA5; WRITE_SIZE];
const TOTAL_SIZE: usize = WRITES * WRITE_SIZE;
const ALLOCATED_BLOCKS: usize = TOTAL_SIZE / BLOCK_SIZE;

struct RestoreStatsOn(bool);

impl Drop for RestoreStatsOn {
    fn drop(&mut self) {
        crate::task::perf::STATS_ON.store(self.0, core::sync::atomic::Ordering::Relaxed);
    }
}

fn prepare_stats_snapshot(
    fs_id: usize,
) -> Result<another_ext4::PrepareStatsSnapshot, &'static str> {
    prepare_stats_snapshots()
        .into_iter()
        .find(|(id, _)| *id == fs_id)
        .map(|(_, snapshot)| snapshot)
        .ok_or("prepare stats bridge did not expose the mounted filesystem")
}

/// Exercise bounded 1KiB buffered appends and emit finite preparation metrics.
pub(super) fn test_buffered_1k_append_prepare_metrics_persist_after_remount(
) -> Result<(), &'static str> {
    let restore_stats_on = RestoreStatsOn(crate::task::perf::STATS_ON.swap(
        true,
        core::sync::atomic::Ordering::Relaxed,
    ));
    let fs = Ext4FileSystem::open(clean_media_device()?)
        .map_err(|_| "1KiB append diagnostic mount failed")?;
    if !fs.inner().prepare_stats_enabled() {
        return Err("1KiB append diagnostic build did not enable prepare stats");
    }
    let fs_id = fs.fs_id();
    let root = fs.root_inode();
    match root.unlink(NAME) {
        Ok(()) => root
            .sync()
            .map_err(|_| "1KiB append diagnostic stale cleanup sync failed")?,
        Err(SyscallErr::ENOENT) => {}
        Err(_) => return Err("1KiB append diagnostic stale cleanup failed"),
    }
    let before = prepare_stats_snapshot(fs_id)?;
    let file = root
        .create(NAME, FileType::File, VfsInodeMode::S_IRWXUGO)
        .map_err(|_| "1KiB append diagnostic create failed")?;
    let private = spin::Mutex::new(FilePrivateData::Unused);
    file.open(private.lock(), &FileFlags::O_WRONLY)
        .map_err(|_| "1KiB append diagnostic open failed")?;
    for write_index in 0..WRITES {
        let offset = write_index * WRITE_SIZE;
        let private = spin::Mutex::new(FilePrivateData::Unused);
        let written = file
            .write_at(offset, DATA.len(), &DATA, private.lock())
            .map_err(|_| "1KiB append diagnostic write failed")?;
        if written != DATA.len() {
            return Err("1KiB append diagnostic write was short");
        }
    }
    file.sync()
        .map_err(|_| "1KiB append diagnostic writeback failed")?;
    let after = prepare_stats_snapshot(fs_id)?;

    let calls = after.calls.wrapping_sub(before.calls);
    let requested = after.requested_blocks.wrapping_sub(before.requested_blocks);
    let mapped = after.mapped_blocks.wrapping_sub(before.mapped_blocks);
    let missing = after.missing_blocks.wrapping_sub(before.missing_blocks);
    let failures = after.failures.wrapping_sub(before.failures);
    let bitmap_io = after.bitmap_io.wrapping_sub(before.bitmap_io);
    let gdt_io = after.gdt_io.wrapping_sub(before.gdt_io);
    let superblock_io = after.superblock_io.wrapping_sub(before.superblock_io);
    let inode_io = after.inode_io.wrapping_sub(before.inode_io);
    let inode_read_calls = after.inode_read_calls.wrapping_sub(before.inode_read_calls);
    let inode_read_cycles = after.inode_read_cycles.wrapping_sub(before.inode_read_cycles);
    let extent_query_calls = after
        .extent_query_calls
        .wrapping_sub(before.extent_query_calls);
    let extent_query_cycles = after
        .extent_query_cycles
        .wrapping_sub(before.extent_query_cycles);
    let allocation_calls = after.allocation_calls.wrapping_sub(before.allocation_calls);
    let allocation_cycles = after.allocation_cycles.wrapping_sub(before.allocation_cycles);
    let inode_persist_calls = after
        .inode_persist_calls
        .wrapping_sub(before.inode_persist_calls);
    let inode_persist_cycles = after
        .inode_persist_cycles
        .wrapping_sub(before.inode_persist_cycles);
    let lock_hold_calls = after.lock_hold_calls.wrapping_sub(before.lock_hold_calls);
    let lock_hold_cycles = after.lock_hold_cycles.wrapping_sub(before.lock_hold_cycles);
    let metadata_io = bitmap_io
        .wrapping_add(gdt_io)
        .wrapping_add(superblock_io)
        .wrapping_add(inode_io);
    crate::println!(
        "EXT4_ANOTHER_PREPARE_1K_METRICS writes={} bytes={} allocated_blocks={} calls={} requested={} mapped={} missing={} metadata_io={} bitmap_io={} gdt_io={} superblock_io={} inode_io={} extent_io={} zero_io={} block_count_full_traversals={} inode_read_calls={} inode_read_cycles={} extent_query_calls={} extent_query_cycles={} allocation_calls={} allocation_cycles={} inode_persist_calls={} inode_persist_cycles={} lock_hold_calls={} lock_hold_cycles={}",
        WRITES,
        TOTAL_SIZE,
        ALLOCATED_BLOCKS,
        calls,
        requested,
        mapped,
        missing,
        metadata_io,
        bitmap_io,
        gdt_io,
        superblock_io,
        inode_io,
        after.extent_io.wrapping_sub(before.extent_io),
        after.zero_io.wrapping_sub(before.zero_io),
        after
            .block_count_full_traversals
            .wrapping_sub(before.block_count_full_traversals),
        inode_read_calls,
        inode_read_cycles,
        extent_query_calls,
        extent_query_cycles,
        allocation_calls,
        allocation_cycles,
        inode_persist_calls,
        inode_persist_cycles,
        lock_hold_calls,
        lock_hold_cycles,
    );
    if after.generation != before.generation
        || calls == 0
        // A miss that allocates mappings invalidates its own token, so the
        // next loop iteration re-queries before publishing the positive cache
        // entry. The bounded retry is one extra prepare per allocation, never
        // a return to one prepare per 1KiB write.
        || calls > ALLOCATED_BLOCKS * 2
        || failures != 0
        || inode_read_calls == 0
        || extent_query_calls > WRITES + ALLOCATED_BLOCKS
        || allocation_calls == 0
        || inode_persist_calls == 0
        || lock_hold_calls == 0
    {
        return Err("1KiB append diagnostic PrepareStats did not record the bounded workload");
    }

    drop(file);
    drop(root);
    fs.on_umount();
    drop(fs);

    let remounted_fs = Ext4FileSystem::open(clean_media_device()?)
        .map_err(|_| "1KiB append diagnostic remount failed")?;
    let remounted_root = remounted_fs.root_inode();
    let remounted_file = remounted_root
        .find(NAME)
        .map_err(|_| "1KiB append diagnostic file disappeared after remount")?;
    let mut readback = [0u8; TOTAL_SIZE];
    let private = spin::Mutex::new(FilePrivateData::Unused);
    let read = remounted_file
        .read_at(0, readback.len(), &mut readback, private.lock())
        .map_err(|_| "1KiB append diagnostic remount read failed")?;
    if read != readback.len() || readback != [0xA5; TOTAL_SIZE] {
        return Err("1KiB append diagnostic data did not persist after remount");
    }
    remounted_root
        .unlink(NAME)
        .map_err(|_| "1KiB append diagnostic cleanup unlink failed")?;
    remounted_root
        .sync()
        .map_err(|_| "1KiB append diagnostic cleanup sync failed")?;
    drop(remounted_file);
    drop(remounted_root);
    remounted_fs.on_umount();

    drop(restore_stats_on);

    Ok(())
}
