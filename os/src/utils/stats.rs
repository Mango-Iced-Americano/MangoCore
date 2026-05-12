//! 资源统计诊断模块
//!
//! 用 `println!` 直接输出（不依赖 LOG 宏），方便在 QEMU 串口看到。
//! 顶部 `STATS_ENABLED` 改为 `false` 即可一行关闭所有诊断输出。

use crate::fs::directory_tree::directory_node_count;
use crate::mm::{heap_stats, unallocated_frames};
use crate::task::{current_task, procs_count, task_manager_counts, zombie_count};

/// 是否启用资源统计输出
/// 改为 false 即可一行关闭
const STATS_ENABLED: bool = false;

/// 打印当前内核资源统计信息
///
/// 输出格式：
///   [kernel] [stats] free_frames=N ready=N int=N zombie=N dir_nodes=N cur_fds=N
///
/// 六个计数器分别对应：
///   free_frames — 空闲物理帧数
///   ready       — 就绪队列任务数
///   int         — 可中断队列任务数
///   zombie      — 僵尸任务数
///   dir_nodes   — 目录树节点数（VFS 缓存中 live 节点）
///   cur_fds     — 当前任务打开的文件描述符数
pub fn print_resource_stats() {
    if !STATS_ENABLED {
        return;
    }

    let free = unallocated_frames();
    let procs = procs_count();
    let zombies = zombie_count();
    let dir_nodes = directory_node_count();
    let (heap_free, heap_total) = heap_stats();

    // 当前进程 FD 数
    let cur_fds = match current_task() {
        Some(task) => task.files.lock().iter().filter(|fd| fd.is_some()).count(),
        None => 0,
    };

    let (ready, int_count) = task_manager_counts().unwrap_or((0, 0));

    println!(
        "[kernel] [stats] free_frames={} ready={} int={} zombie={} dir_nodes={} cur_fds={} heap_free={}K heap_total={}K",
        free, ready, int_count, zombies, dir_nodes, cur_fds, heap_free >> 10, heap_total >> 10
    );
}
