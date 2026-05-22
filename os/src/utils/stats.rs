//! 资源统计诊断模块
//!
//! 用 `println!` 直接输出（不依赖 LOG 宏），方便在 QEMU 串口看到。
//! 顶部 `STATS_ENABLED` 改为 `false` 即可一行关闭所有诊断输出。

use crate::mm::{heap_stats, unallocated_frames};
use crate::task::{procs_count, task_manager_counts, zombie_count, TaskControlBlock};

/// 是否启用资源统计输出。改为 false 即可一行关闭。
const STATS_ENABLED: bool = true;

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
}
