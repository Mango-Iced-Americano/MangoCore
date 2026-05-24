//! 文件系统缓存周期回收
//!
//! 从调度循环中调用，带节流和水位检查。仅回收干净页，不写回脏页。

use core::sync::atomic::{AtomicUsize, Ordering};

const THROTTLE: usize = 64;
const HIGH_WATER_PAGES: isize = 16384; // 64MB
const BATCH_PAGES: usize = 64;

/// 周期回收 page cache 干净页（调度循环中调用，带节流）
/// 仅在 page_cache > 64MB 时触发，每次最多回收 64 页
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
        let cached = fs.get_cache_metric(6); // page_cache_cached_pages
        if cached > HIGH_WATER_PAGES {
            let stats = fs.reclaim_fs_caches(BATCH_PAGES);
            if stats.clean_pages_freed > 0 {
                log::debug!(
                    "[reclaim] freed={} before={} after={}",
                    stats.clean_pages_freed,
                    stats.cached_pages_before,
                    stats.cached_pages_after
                );
            }
        }
    }
}
