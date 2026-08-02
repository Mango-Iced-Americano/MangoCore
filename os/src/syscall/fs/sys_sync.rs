use super::common::*;

pub fn sys_sync() -> isize {
    // Linux sync(2) returns 0 even on I/O errors, but failed writeback remains
    // observable through later fsync() calls and the error is recorded here.
    if let Err(e) = crate::fs::flush_all_page_caches() {
        log::error!("sys_sync: flush_all_page_caches failed: {:?}", e);
    }
    // Collect live ext4 instances, then flush metadata cache without holding the registry lock
    let mut guard = crate::fs::ext4::ext4fs::EXT4_REGISTRY.lock();
    let live: alloc::vec::Vec<_> = guard.iter().filter_map(|w| w.upgrade()).collect();
    guard.retain(|w| w.strong_count() > 0);
    drop(guard);
    for fs in &live {
        fs.flush_metadata_cache();
    }
    #[cfg(feature = "ext4_another_backend")]
    crate::fs::ext4_another::sync_all_instances();
    SUCCESS
}
