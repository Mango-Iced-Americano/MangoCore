use alloc::sync::Arc;
use alloc::vec;

use another_ext4::{ErrCode, Ext4, InodeMode};

use crate::config::PAGE_SIZE;
use crate::fs::ext4_another::Ext4FileSystem;
use crate::fs::vfs::{FilePrivateData, FileSystem, FileType, InodeMode as VfsInodeMode};
use crate::utils::error::SyscallErr;

use super::fixtures::clean_media_device;
use super::recording_device::RecordingBlockDevice;
use super::writeback_observer::{PageCacheBackendSwapGuard, WritebackCall};

pub(super) fn test_page_cache_writeback_batches_contiguous_pages_through_mango_adapter() -> Result<(), &'static str> {
    const NAME: &str = "another-page-cache-batch";

    let fs = Ext4FileSystem::open(clean_media_device()?).map_err(|_| "page-cache test mount failed")?;
    let root = fs.root_inode();
    match root.unlink(NAME) {
        Ok(()) => root
            .sync()
            .map_err(|_| "page-cache test stale-name cleanup sync failed")?,
        Err(SyscallErr::ENOENT) => {}
        Err(_) => return Err("page-cache test stale-name cleanup unlink failed"),
    }

    let test_result = (|| {
        let file = root
            .create(NAME, FileType::File, VfsInodeMode::S_IRWXUGO)
            .map_err(|_| "page-cache test create failed")?;
        let initial = vec![0x5A; PAGE_SIZE * 2];
        let private = spin::Mutex::new(FilePrivateData::Unused);
        let written = file.write_at(0, initial.len(), &initial, private.lock()).map_err(|_| "page-cache test initial write failed")?;
        if written != initial.len() {
            return Err("page-cache test initial write was short");
        }
        file.sync()
            .map_err(|_| "page-cache test allocation sync failed")?;
        let cache = file.page_cache().ok_or("page-cache test cache missing")?;
        let observer = PageCacheBackendSwapGuard::install(&cache)?;

        let overwrite = vec![0xA5; PAGE_SIZE * 2];
        let written = file.write_at(0, overwrite.len(), &overwrite, private.lock()).map_err(|_| "page-cache test overwrite failed")?;
        if written != overwrite.len() {
            return Err("page-cache test overwrite was short");
        }

        let sync_result = file.sync().map_err(|_| "page-cache test overwrite sync failed");
        let calls = observer.snapshot_calls();
        drop(observer);
        sync_result?;
        if calls.iter().any(|call| matches!(call, WritebackCall::Page { .. })) {
            return Err("contiguous PageCache pages fell back to write_page");
        }
        if calls.as_slice() != [WritebackCall::Pages { start_index: 0, page_count: 2 }] {
            return Err("contiguous PageCache pages did not issue one logical write_pages(0, 2)");
        }
        Ok(())
    })();

    let cleanup_result = match root.unlink(NAME) {
        Ok(()) => root
            .sync()
            .map_err(|_| "page-cache test cleanup sync failed"),
        Err(SyscallErr::ENOENT) => Ok(()),
        Err(_) => Err("page-cache test cleanup unlink failed"),
    };
    test_result.and(cleanup_result)
}

pub(super) fn test_write_data_only_batches_physical_runs_before_data_io() -> Result<(), &'static str>
{
    const TARGET: &str = "another-write-data-only-runs";
    const FILLER: &str = "another-write-data-only-filler";
    const BLOCKS: usize = 20;
    const PREFIX: usize = 123;
    const SUFFIX: usize = 177;
    const EXPECTED_RUN_LENGTHS: [usize; 7] = [1, 1, 14, 1, 1, 1, 1];

    let device = Arc::new(RecordingBlockDevice::new(clean_media_device()?));
    let ext4 = Ext4::load_writable(device.clone()).map_err(|_| "writeback test mount failed")?;
    let mode = InodeMode::FILE | InodeMode::ALL_RWX;
    match ext4.generic_remove(another_ext4::EXT4_ROOT_INO, TARGET) {
        Ok(()) => {}
        Err(error) if error.code() == ErrCode::ENOENT => {}
        Err(_) => return Err("writeback test stale target cleanup failed"),
    }
    match ext4.generic_remove(another_ext4::EXT4_ROOT_INO, FILLER) {
        Ok(()) => {}
        Err(error) if error.code() == ErrCode::ENOENT => {}
        Err(_) => return Err("writeback test stale filler cleanup failed"),
    }
    let file_size = BLOCKS * another_ext4::BLOCK_SIZE;

    let body_result = (|| {
        let target = ext4
            .generic_create(another_ext4::EXT4_ROOT_INO, TARGET, mode)
            .map_err(|_| "writeback test target create failed")?;
        let filler = ext4
            .generic_create(another_ext4::EXT4_ROOT_INO, FILLER, mode)
            .map_err(|_| "writeback test filler create failed")?;
        ext4.prepare_buffered_write(
            target,
            0,
            16 * another_ext4::BLOCK_SIZE,
            file_size as u64,
            None,
        )
        .map_err(|_| "writeback test initial target allocation failed")?;
        for logical_block in 16..BLOCKS {
            let filler_offset = (logical_block - 16) * another_ext4::BLOCK_SIZE;
            ext4.prepare_buffered_write(
                filler,
                filler_offset,
                another_ext4::BLOCK_SIZE,
                file_size as u64,
                None,
            )
            .map_err(|_| "writeback test filler allocation failed")?;
            ext4.prepare_buffered_write(
                target,
                logical_block * another_ext4::BLOCK_SIZE,
                another_ext4::BLOCK_SIZE,
                file_size as u64,
                None,
            )
            .map_err(|_| "writeback test fragmented target allocation failed")?;
        }

        let initial = vec![0xA5; file_size];
        ext4.write_data_only(target, 0, &initial)
            .map_err(|_| "writeback test initial data write failed")?;
        ext4.commit_inode_size(target, file_size as u64, None)
            .map_err(|_| "writeback test initial size commit failed")?;

        let write_len = file_size - PREFIX - SUFFIX;
        let replacement = vec![0x3C; write_len];
        device.start_recording();
        let write_result = ext4
            .write_data_only(target, PREFIX, &replacement)
            .map_err(|_| "writeback test partial data write failed");
        let recording = device.finish_recording();
        let written = write_result?;
        if written != replacement.len() {
            return Err("write_data_only returned a short write");
        }
        if recording.read_after_write {
            return Err("write_data_only performed block I/O reads after its first data write");
        }
        if recording.legacy_writes != 0 {
            return Err("write_data_only bypassed BlockDevice::write_blocks");
        }
        if recording.mango_runs.len() != EXPECTED_RUN_LENGTHS.len() {
            return Err("write_data_only did not split fragmented mappings into physical runs");
        }
        for (run, expected_blocks) in recording.mango_runs.iter().zip(EXPECTED_RUN_LENGTHS) {
            if run.blocks != expected_blocks {
                return Err("write_data_only used logical rather than physical run boundaries");
            }
        }
        for adjacent in recording.mango_runs.windows(2) {
            let previous_end = adjacent[0]
                .start
                .checked_add(adjacent[0].blocks as u64)
                .ok_or("recorded physical run overflowed")?;
            if adjacent[1].start == previous_end {
                return Err("write_data_only split a physically contiguous run");
            }
        }
        let attr = ext4
            .getattr(target)
            .map_err(|_| "writeback test metadata lookup failed")?;
        if attr.size != file_size as u64 {
            return Err("write_data_only changed the regular-file size");
        }
        Ok((target, initial, replacement))
    })();

    let (target, initial, replacement) = match body_result {
        Ok(values) => values,
        Err(error) => {
            let cleanup_result = (|| {
                match ext4.generic_remove(another_ext4::EXT4_ROOT_INO, TARGET) {
                    Ok(()) => {}
                    Err(error) if error.code() == ErrCode::ENOENT => {}
                    Err(_) => return Err("writeback test target cleanup failed"),
                }
                match ext4.generic_remove(another_ext4::EXT4_ROOT_INO, FILLER) {
                    Ok(()) => {}
                    Err(error) if error.code() == ErrCode::ENOENT => {}
                    Err(_) => return Err("writeback test filler cleanup failed"),
                }
                ext4.shutdown_writable()
                    .map_err(|_| "writeback test final shutdown failed")
            })();
            if cleanup_result.is_err() { crate::println!("# writeback cleanup failed after body failure"); }
            return Err(error);
        }
    };

    if let Err(error) = ext4
        .shutdown_writable()
        .map_err(|_| "writeback test shutdown before remount failed")
    {
        let cleanup_result = (|| {
            ext4.generic_remove(another_ext4::EXT4_ROOT_INO, TARGET)
                .map_err(|_| "writeback test target cleanup failed")?;
            ext4.generic_remove(another_ext4::EXT4_ROOT_INO, FILLER)
                .map_err(|_| "writeback test filler cleanup failed")?;
            ext4.shutdown_writable()
                .map_err(|_| "writeback test final shutdown failed")
        })();
        if cleanup_result.is_err() { crate::println!("# writeback cleanup failed after body failure"); }
        return Err(error);
    }
    drop(ext4);

    let remounted = Ext4::load_writable(device).map_err(|_| "writeback test remount failed")?;
    let verification_result = (|| {
        let mut expected = initial;
        expected[PREFIX..PREFIX + replacement.len()].copy_from_slice(&replacement);
        let mut readback = vec![0; file_size];
        let read = remounted
            .read(target, 0, &mut readback)
            .map_err(|_| "writeback test remount read failed")?;
        if read != readback.len() || readback != expected {
            return Err("write_data_only lost a partial-block prefix or suffix after remount");
        }
        Ok(())
    })();
    let cleanup_result = (|| {
        remounted
            .generic_remove(another_ext4::EXT4_ROOT_INO, TARGET)
            .map_err(|_| "writeback test target cleanup failed")?;
        remounted
            .generic_remove(another_ext4::EXT4_ROOT_INO, FILLER)
            .map_err(|_| "writeback test filler cleanup failed")?;
        remounted
            .shutdown_writable()
            .map_err(|_| "writeback test final shutdown failed")
    })();
    verification_result.and(cleanup_result)
}
