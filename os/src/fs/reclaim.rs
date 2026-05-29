//! Periodically reclaim filesystem caches from the scheduler loop.
//!
//! Stale metadata cleanup (inode_objects, page_caches, children) runs
//! unconditionally every THROTTLE ticks. Clean page cache shrink uses a
//! two-tier threshold plus heap-pressure fallback. Under severe heap
//! pressure (>90%), inode caches are also pruned.

use core::sync::atomic::{AtomicUsize, Ordering};

const THROTTLE: usize = 64;
const LOW_WATER_PAGES: isize = 1024;   // 4MB — gentle eviction
const HIGH_WATER_PAGES: isize = 4096;  // 16MB — aggressive eviction
const BATCH_PAGES: usize = 64;
const LOW_BATCH_PAGES: usize = 8;
const CRITICAL_BATCH_PAGES: usize = 32;
const HEAP_PRESSURE_PCT: usize = 75;   // trigger eviction when >75% heap used
const HEAP_CRITICAL_PCT: usize = 90;   // aggressive multi-cache eviction

fn heap_used_pct() -> usize {
    let (free, total, _, _, _) = crate::mm::heap_stats();
    if total == 0 {
        return 0;
    }
    (total - free) * 100 / total
}

fn heap_under_pressure() -> bool {
    heap_used_pct() > HEAP_PRESSURE_PCT
}

fn heap_critical() -> bool {
    heap_used_pct() > HEAP_CRITICAL_PCT
}

pub fn maybe_reclaim_fs_caches() {
    static TICK: AtomicUsize = AtomicUsize::new(0);

    let tick = TICK.fetch_add(1, Ordering::Relaxed);
    if tick % THROTTLE != 0 {
        return;
    }

    // 全局清理：FIFO registry 中两端都已关闭的陈旧条目
    crate::fs::dev::pipe::compact_fifo_registry();

    let fs = {
        let guard = crate::fs::ext4::ext4fs::GLOBAL_EXT4FS.lock();
        guard.as_ref().and_then(|w| w.upgrade())
    };

    if let Some(fs) = fs {
        let io_removed = fs.prune_inode_objects();
        let pc_removed = fs.prune_page_caches();
        let kids_removed = fs.prune_children_stale_entries();

        let cached = fs.get_cache_metric(6); // page_cache_cached_pages

        if heap_critical() {
            // Severe heap pressure: evict aggressively from all caches
            let freed = fs.shrink_all_page_caches_clean(CRITICAL_BATCH_PAGES);
            if freed > 0 {
                log::warn!(
                    "[reclaim] CRITICAL heap={}% clean_freed={} stale: io={} pc={} kids={} cached={}",
                    heap_used_pct(), freed, io_removed, pc_removed, kids_removed, cached
                );
            }
        } else if cached > HIGH_WATER_PAGES {
            let freed = fs.shrink_all_page_caches_clean(BATCH_PAGES);
            if freed > 0 {
                log::debug!(
                    "[reclaim] high-water clean_freed={} stale: io={} pc={} kids={}",
                    freed, io_removed, pc_removed, kids_removed
                );
            }
        } else if cached > LOW_WATER_PAGES || heap_under_pressure() {
            let freed = fs.shrink_all_page_caches_clean(LOW_BATCH_PAGES);
            if freed > 0 {
                log::debug!(
                    "[reclaim] low-water clean_freed={} stale: io={} pc={} kids={} cached={}",
                    freed, io_removed, pc_removed, kids_removed, cached
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
