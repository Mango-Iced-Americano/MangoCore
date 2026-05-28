//! Periodically reclaim filesystem caches from the scheduler loop.
//!
//! Stale metadata cleanup (inode_objects, page_caches, children) runs
//! unconditionally every THROTTLE ticks. Clean page cache shrink is gated
//! behind a 64MB high-water mark to avoid premature eviction.

use core::sync::atomic::{AtomicUsize, Ordering};

const THROTTLE: usize = 64;
const HIGH_WATER_PAGES: isize = 16384; // 64MB
const BATCH_PAGES: usize = 64;

pub fn maybe_reclaim_fs_caches() {
    static TICK: AtomicUsize = AtomicUsize::new(0);

    let tick = TICK.fetch_add(1, Ordering::Relaxed);
    if tick % THROTTLE != 0 {
        return;
    }

    let fs = {
        let guard = crate::fs::ext4::ext4fs::GLOBAL_EXT4FS.lock();
        guard.as_ref().and_then(|w| w.upgrade())
    };

    if let Some(fs) = fs {
        let io_removed = fs.prune_inode_objects();
        let pc_removed = fs.prune_page_caches();
        let kids_removed = fs.prune_children_stale_entries();

        let cached = fs.get_cache_metric(6); // page_cache_cached_pages
        if cached > HIGH_WATER_PAGES {
            let freed = fs.shrink_all_page_caches_clean(BATCH_PAGES);
            if freed > 0 {
                log::debug!(
                    "[reclaim] clean_freed={} stale: io={} pc={} kids={}",
                    freed, io_removed, pc_removed, kids_removed
                );
            }
        } else if io_removed + pc_removed + kids_removed > 0 {
            log::debug!(
                "[reclaim] stale: io={} pc={} kids={}",
                io_removed, pc_removed, kids_removed
            );
        }
    }
}
