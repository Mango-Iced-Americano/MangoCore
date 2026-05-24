//! 资源统计诊断模块
//!
//! 用 `println!` 直接输出（不依赖 LOG 宏），方便在 QEMU 串口看到。
//! 顶部 `STATS_ENABLED` 改为 `false` 即可一行关闭所有诊断输出。

use crate::mm::{heap_stats, unallocated_frames};
use crate::task::{procs_count, task_manager_counts, zombie_count, TaskControlBlock};

/// 是否启用资源统计输出。改为 false 即可一行关闭。
const STATS_ENABLED: bool = true;

/// 收集缓存内存统计
fn cache_memory_stats() -> (usize, usize, usize, usize, usize) {
    let mut page_cached = 0usize;
    let mut page_dirty = 0usize;
    let mut ext4_inode_cache = 0usize;
    let mut ext4_mbc_bytes = 0usize;
    let mut mbc_dirty = 0usize;

    let guard = crate::fs::ext4::ext4fs::GLOBAL_EXT4FS.lock();
    if let Some(fs) = guard.as_ref().and_then(|w| w.upgrade()) {
        let cached = fs.get_cache_metric(6);
        let dirty = fs.get_cache_metric(7);
        let ic_total = fs.get_cache_metric(8);
        if cached >= 0 { page_cached = cached as usize; }
        if dirty >= 0 { page_dirty = dirty as usize; }
        if ic_total >= 0 { ext4_inode_cache = ic_total as usize; }
        // MetaBlockCache: len/dirty/clean, each entry ≈ block_size bytes
        let (mbc_len, mbc_d, _mbc_clean) = fs.meta_block_cache.stats();
        ext4_mbc_bytes = mbc_len * fs.block_size;
        mbc_dirty = mbc_d;
    }

    (page_cached, page_dirty, ext4_inode_cache, ext4_mbc_bytes, mbc_dirty)
}

/// 打印当前内核资源统计信息
pub fn print_resource_stats(task: Option<&TaskControlBlock>) {
    if !STATS_ENABLED {
        return;
    }

    let free = unallocated_frames();
    let procs = procs_count();
    let zombies = zombie_count();
    let (heap_free, heap_total) = heap_stats();

    // 当前任务 FD 数（由调用方传入，避免 current_task() 在退出路径已失效）
    let cur_fds = task.and_then(|t| {
        t.process.files().try_lock().map(|f| f.fd_count())
    }).unwrap_or(0);

    let (ready, int_count) = task_manager_counts().unwrap_or((0, 0));

    println!(
        "[kernel] [stats] free_frames={} ready={} int={} zombie={} procs={} fds={} heap_free={}K heap_total={}K",
        free, ready, int_count, zombies, procs, cur_fds, heap_free >> 10, heap_total >> 10
    );

    // Cache memory line
    let (page_cached, page_dirty, ext4_ic, ext4_mbc, mbc_dirty) = cache_memory_stats();
    let mntfs = crate::fs::vfs::mount::counters::mountfs_alive();
    let mntinode = crate::fs::vfs::mount::counters::mountfsinode_alive();
    println!(
        "[kernel] [stats] page_cache={}K dirty={}K ext4_ic={} ext4_mbc={}K dirty={} mounts={} mntinode={}",
        (page_cached * 4), page_dirty * 4, ext4_ic, ext4_mbc >> 10, mbc_dirty, mntfs, mntinode
    );
}
