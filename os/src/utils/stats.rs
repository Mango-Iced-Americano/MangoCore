//! 资源统计诊断模块

use crate::mm::{heap_stats, unallocated_frames};
use crate::task::{procs_count, task_manager_counts, zombie_count, TaskControlBlock};

const STATS_ENABLED: bool = true;

fn ext4_cache_stats() -> (usize, usize, usize, usize, usize) {
    let mut pc = 0; let mut pd = 0; let mut ic = 0; let mut mb = 0; let mut md = 0;
    let g = crate::fs::ext4::ext4fs::GLOBAL_EXT4FS.lock();
    if let Some(fs) = g.as_ref().and_then(|w| w.upgrade()) {
        let c = fs.get_cache_metric(6);  let d = fs.get_cache_metric(7);
        let i = fs.get_cache_metric(8);
        if c >= 0 { pc = c as usize; } if d >= 0 { pd = d as usize; }
        if i >= 0 { ic = i as usize; }
        let (l, dty, _) = fs.meta_block_cache.stats();
        mb = l * fs.block_size; md = dty;
    }
    (pc, pd, ic, mb, md)
}

fn ext4_dentry_stats() -> (usize, usize, usize, usize, usize, usize) {
    let g = crate::fs::ext4::ext4fs::GLOBAL_EXT4FS.lock();
    if let Some(fs) = g.as_ref().and_then(|w| w.upgrade()) {
        return fs.dentry_stats();
    }
    (0, 0, 0, 0, 0, 0)
}

fn pc_metadata_stats() -> (usize, usize, usize, usize) {
    crate::fs::registry_stats()
}

fn proc_fd_stats() -> (usize, usize, usize, usize, usize, usize, usize) {
    let mut palive = 0; let mut pzombie = 0;
    let mut fd_open = 0; let mut fd_slots = 0; let mut fd_cap = 0;
    let mut zfd_open = 0; let mut zfd_slots = 0; let mut zfd_cap = 0;

    for pcb in crate::task::ProcessManager::all_processes() {
        let is_zombie = pcb.is_zombie();
        if is_zombie { pzombie += 1; } else { palive += 1; }
        if let Some(files) = pcb.files().try_lock() {
            let o = files.fd_count();
            let s = files.len();
            let c = files.capacity();
            if is_zombie { zfd_open += o; zfd_slots += s; zfd_cap += c; }
            else { fd_open += o; fd_slots += s; fd_cap += c; }
        }
    }
    (palive, pzombie, fd_open, fd_slots, fd_cap, zfd_open + zfd_slots, zfd_slots + zfd_cap)
}

pub fn print_resource_stats(task: Option<&TaskControlBlock>) {
    if !STATS_ENABLED { return; }

    let free = unallocated_frames();
    let (heap_free, heap_total, _alloc_user, alloc_actual, waste) = heap_stats();
    let (ready, int_count) = task_manager_counts().unwrap_or((0, 0));
    let cur_fds = task.and_then(|t| t.process.files().try_lock().map(|f| f.fd_count())).unwrap_or(0);

    let (pc, pd, ic, mbc, mbd) = ext4_cache_stats();
    let mntfs = crate::fs::vfs::mount::counters::mountfs_alive();
    let mntinode = crate::fs::vfs::mount::counters::mountfsinode_alive();
    let (pr_len, pr_cap, pr_alive, pr_stale) = pc_metadata_stats();
    let (kt, ka, ks, kb, nt, nb) = ext4_dentry_stats();
    let (pa, pz, fo, fs, fc, zfo, zfc) = proc_fd_stats();

    // Line 1: system resources
    println!(
        "[kernel] [stats] free_frames={} ready={} int={} procs={} heap={}K/{}/{}K waste={}K",
        free, ready, int_count, pa, heap_free >> 10, alloc_actual >> 10, heap_total >> 10, waste >> 10
    );
    // Line 2: cache memory
    println!(
        "[kernel] [stats] pc={}K dirty={}K ic={} mbc={}K mbd={} mounts={} mnode={}",
        pc * 4, pd * 4, ic, mbc >> 10, mbd, mntfs, mntinode
    );
    // Line 3: PageCache metadata + ext4 dentry
    let (el, ec, ev, eh) = crate::fs::entries_global_stats();
    println!(
        "[kernel] [stats] pc_reg={}/{}/{}/{} pc_ent={}/{}/{}/{} kids={}/{}/{}/{}K neg={}/{}K",
        pr_len, pr_cap, pr_alive, pr_stale,
        el, ec, ev, eh,
        kt, ka, ks, kb >> 10, nt, nb >> 10
    );
    // Line 4: process fd tables
    println!(
        "[kernel] [stats] proc z={} cur_fds={} fds={}/{}/{} zfds={}/{}",
        pz, cur_fds, fo, fs, fc, zfo >> 1, zfc >> 1
    );
}
