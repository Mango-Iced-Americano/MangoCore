use super::common::*;

pub fn sys_sync() -> isize {
    if let Err(error) = crate::fs::flush_all_page_caches() {
        return -(error as isize);
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
    if let Err(error) = crate::fs::ext4_another::sync_all_instances() {
        return -(error as isize);
    }
    SUCCESS
}
