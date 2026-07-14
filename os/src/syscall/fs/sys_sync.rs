use super::common::*;

pub fn sys_sync() -> isize {
    crate::fs::flush_all_page_caches();
    // Collect live ext4 instances, then flush metadata cache without holding the registry lock
    let mut guard = crate::fs::ext4::ext4fs::EXT4_REGISTRY.lock();
    let live: alloc::vec::Vec<_> = guard.iter().filter_map(|w| w.upgrade()).collect();
    guard.retain(|w| w.strong_count() > 0);
    drop(guard);
    for fs in &live {
        fs.flush_metadata_cache();
    }
    SUCCESS
}
