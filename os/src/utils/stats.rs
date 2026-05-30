//! 资源统计诊断模块

use crate::mm::{heap_stats, unallocated_frames};
use crate::task::{procs_count, quota, task_manager_counts, TaskControlBlock};
#[cfg(feature = "heap_trace")]
use alloc::string::String;
#[cfg(feature = "heap_trace")]
use core::fmt::Write;

const STATS_ENABLED: bool = cfg!(feature = "heap_trace");

fn ext4_cache_stats() -> (usize, usize, usize, usize, usize) {
    let mut pc = 0;
    let mut pd = 0;
    let mut ic = 0;
    let mut mb = 0;
    let mut md = 0;
    let g = crate::fs::ext4::ext4fs::GLOBAL_EXT4FS.lock();
    if let Some(fs) = g.as_ref().and_then(|w| w.upgrade()) {
        let c = fs.get_cache_metric(6);
        let d = fs.get_cache_metric(7);
        let i = fs.get_cache_metric(8);
        if c >= 0 {
            pc = c as usize;
        }
        if d >= 0 {
            pd = d as usize;
        }
        if i >= 0 {
            ic = i as usize;
        }
        let (l, dty, _) = fs.meta_block_cache.stats();
        mb = l * fs.block_size;
        md = dty;
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
    let mut palive = 0;
    let mut pzombie = 0;
    let mut fd_open = 0;
    let mut fd_slots = 0;
    let mut fd_cap = 0;
    let mut zfd_open = 0;
    let mut zfd_slots = 0;
    let mut zfd_cap = 0;

    for pcb in crate::task::ProcessManager::all_processes() {
        let is_zombie = pcb.is_zombie();
        if is_zombie {
            pzombie += 1;
        } else {
            palive += 1;
        }
        if let Some(files) = pcb.files().try_lock() {
            let o = files.fd_count();
            let s = files.len();
            let c = files.capacity();
            if is_zombie {
                zfd_open += o;
                zfd_slots += s;
                zfd_cap += c;
            } else {
                fd_open += o;
                fd_slots += s;
                fd_cap += c;
            }
        }
    }
    (
        palive,
        pzombie,
        fd_open,
        fd_slots,
        fd_cap,
        zfd_open + zfd_slots,
        zfd_slots + zfd_cap,
    )
}

fn proc_object_stats() -> (
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
) {
    let mut pcbs = 0;
    let mut zpcbs = 0;
    let mut tcb_slots = 0;
    let mut tcb_live = 0;
    let mut pcb_refs = 0;
    let mut zpcb_refs = 0;
    let mut as_refs = 0;
    let mut zas_refs = 0;
    let mut zvm_x = 0;
    for pcb in crate::task::ProcessManager::all_processes() {
        pcbs += 1;
        let z = pcb.is_zombie();
        if z {
            zpcbs += 1;
        }
        let pr = alloc::sync::Arc::strong_count(&pcb).saturating_sub(1);
        pcb_refs += pr;
        if z {
            zpcb_refs += pr;
        }
        let threads = pcb.threads.lock();
        tcb_slots += threads.len();
        tcb_live += threads.iter().filter(|t| t.upgrade().is_some()).count();
        drop(threads);
        let vm = pcb.vm();
        let vr = alloc::sync::Arc::strong_count(&vm).saturating_sub(1);
        as_refs += vr;
        if z {
            zas_refs += vr;
            zvm_x += vr.saturating_sub(1);
        }
    }
    (
        pcbs, zpcbs, tcb_live, tcb_slots, pcb_refs, zpcb_refs, as_refs, zas_refs, zvm_x,
    )
}

#[cfg(feature = "heap_trace")]
fn zombie_parent_stats() -> String {
    let mut groups: alloc::vec::Vec<(usize, usize)> = alloc::vec::Vec::new();
    for pcb in crate::task::ProcessManager::all_processes() {
        if !pcb.is_zombie() {
            continue;
        }
        let parent_pid = pcb.parent_pid();
        if let Some((_, count)) = groups.iter_mut().find(|(pid, _)| *pid == parent_pid) {
            *count += 1;
        } else {
            groups.push((parent_pid, 1));
        }
    }
    groups.sort_by(|a, b| b.1.cmp(&a.1));

    let mut out = String::with_capacity(192);
    for (parent_pid, zombie_count) in groups.into_iter().take(5) {
        if let Some(parent) = crate::task::ProcessManager::find_process(parent_pid) {
            let (children, zombie_children, live_children) = parent.debug_child_counts();
            let parent_refs = alloc::sync::Arc::strong_count(&parent).saturating_sub(1);
            let _ = write!(
                out,
                "{}:{} state={:?} kids={}/{}/{} refs={} ",
                parent_pid,
                zombie_count,
                parent.debug_state(),
                children,
                zombie_children,
                live_children,
                parent_refs
            );
        } else {
            let _ = write!(out, "{}:{} state=gone ", parent_pid, zombie_count);
        }
    }
    out
}

fn vma_stats() -> (usize, usize, usize) {
    let mut vmas = 0;
    let mut zvmas = 0;
    let mut frames = 0;
    for pcb in crate::task::ProcessManager::all_processes() {
        let vm = pcb.vm();
        let vc = vm.lock().vma_count();
        frames += alloc::sync::Arc::strong_count(&vm).saturating_sub(1);
        if pcb.is_zombie() {
            zvmas += vc;
        } else {
            vmas += vc;
        }
    }
    (vmas, zvmas, frames)
}

/// Count zombie processes that still hold a cwd inode reference.
fn zombie_cwd_count() -> usize {
    let mut n = 0;
    for pcb in crate::task::ProcessManager::all_processes() {
        if !pcb.is_zombie() {
            continue;
        }
        if let Some(files) = pcb.files().try_lock() {
            // The cwd is held via FsStatus.working_inode which is a vfs::File
            // containing an Arc<dyn IndexNode>. Check if it's non-null.
            if files.fd_count() > 0 {
                n += 1;
            }
        }
    }
    n
}

pub fn print_resource_stats(task: Option<&TaskControlBlock>) {
    if !STATS_ENABLED {
        return;
    }

    // Throttle: only print every N invocations (each process exit triggers one)
    static CALL_COUNT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    const THROTTLE: usize = 100;
    let n = CALL_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    if n % THROTTLE != 0 {
        return;
    }

    let free = unallocated_frames();
    let (heap_free, heap_total, _alloc_user, alloc_actual, waste) = heap_stats();
    let (ready, int_count) = task_manager_counts().unwrap_or((0, 0));
    let cur_fds = task
        .and_then(|t| t.process.files().try_lock().map(|f| f.fd_count()))
        .unwrap_or(0);

    let (pc, pd, ic, mbc, mbd) = ext4_cache_stats();
    let mntfs = crate::fs::vfs::mount::counters::mountfs_alive();
    let mntinode = crate::fs::vfs::mount::counters::mountfsinode_alive();
    let (pr_len, pr_cap, pr_alive, pr_stale) = pc_metadata_stats();
    let (kt, ka, ks, kb, nt, nb) = ext4_dentry_stats();
    let (pa, pz, fo, fs, fc, zfo, zfc) = proc_fd_stats();
    let (pcbs, zpcbs, tcbs, tcb_slots, pcb_refs, zpcb_refs, as_refs, zas_refs, zvm_x) =
        proc_object_stats();
    let (vmas, zvmas, pt_frames) = vma_stats();

    // Line 1: system resources
    let procs = procs_count();
    let real_procs = crate::task::ProcessManager::all_processes().len();
    let quota = quota::allocated_task_count();
    let proc_limit = crate::config::SYSTEM_TASK_LIMIT;
    println!(
        "[kernel] [stats] free_frames={} ready={} int={} procs={}/{} quota={}/{} heap={}K/{}/{}K waste={}K",
        free,
        ready,
        int_count,
        procs,
        real_procs,
        quota,
        proc_limit,
        heap_free >> 10,
        alloc_actual >> 10,
        heap_total >> 10,
        waste >> 10
    );
    // Line 2: cache memory
    println!(
        "[kernel] [stats] pc={}K dirty={}K ic={} mbc={}K mbd={} mounts={} mnode={}",
        pc * 4,
        pd * 4,
        ic,
        mbc >> 10,
        mbd,
        mntfs,
        mntinode
    );
    // Line 3: PageCache metadata + ext4 dentry
    let (el, ec, ev, eh) = crate::fs::entries_global_stats();
    println!(
        "[kernel] [stats] pc_reg={}/{}/{}/{} pc_ent={}/{}/{}/{} kids={}/{}/{}/{}K neg={}/{}K",
        pr_len,
        pr_cap,
        pr_alive,
        pr_stale,
        el,
        ec,
        ev,
        eh,
        kt,
        ka,
        ks,
        kb >> 10,
        nt,
        nb >> 10
    );
    // Line 4: process fd tables
    println!(
        "[kernel] [stats] proc z={} cur_fds={} fds={}/{}/{} zfds={}/{}",
        pz,
        cur_fds,
        fo,
        fs,
        fc,
        zfo >> 1,
        zfc >> 1
    );
    // Line 5: network socket stats
    let (tn, un, rn, sp) = crate::net::config::NET_INTERFACE.socket_stats();
    println!(
        "[kernel] [stats] net tcp={} udp={} raw={} pend={}",
        tn, un, rn, sp
    );
    // Line 7: I/O buffer stats (pipe + AF_UNIX ring)
    let pn = crate::fs::dev::pipe::pipe_buf_alive();
    let pb = crate::fs::dev::pipe::pipe_buf_bytes();
    let urn = crate::net::socket::unix::ring_buffer::rb_alive();
    let urb = crate::net::socket::unix::ring_buffer::rb_bytes();
    println!(
        "[kernel] [stats] io_buf pipe={}/{}K unix={}/{}K",
        pn,
        pb >> 10,
        urn,
        urb >> 10
    );
    // Line 7: buddy free histogram (orders with >0 blocks, size=2^order)
    let h = crate::mm::heap_free_histogram();
    let mut orders = alloc::string::String::with_capacity(128);
    for (i, &n) in h.iter().enumerate() {
        if n > 0 {
            use core::fmt::Write;
            let _ = write!(orders, "{}:{} ", i, n);
        }
    }
    println!("[kernel] [stats] buddy_free={}", orders);
    // Line 6: process/thread object lifecycle
    println!(
        "[kernel] [stats] objs pcb={} zpcb={} tcb={}/{} stale={} pcb_ref={}/{} as_ref={}/{}/{} vma={}/{} ptf={}",
        pcbs, zpcbs, tcbs, tcb_slots, tcb_slots.saturating_sub(tcbs),
        pcb_refs, zpcb_refs, as_refs, zas_refs, zvm_x, vmas, zvmas, pt_frames
    );
    // Line 9: dentry cache diagnostics + creation sources
    let (ev_tot, ev_sole, ev_ext, adv_rem) = crate::fs::vfs::dentry_cache::dcache_stats::snapshot();
    let (ms_find, ms_ovl, ms_par, ms_root, ms_crt, ms_br) =
        crate::fs::vfs::mount::counters::creation_snapshot();
    let zcwd = zombie_cwd_count();
    println!(
        "[kernel] [stats] diag dc_evict={}/{}/{} dc_adv_rm={} zcwd={} mnode_src=f{}o{}p{}r{}c{}b{}",
        ev_tot, ev_sole, ev_ext, adv_rem, zcwd, ms_find, ms_ovl, ms_par, ms_root, ms_crt, ms_br
    );
    #[cfg(feature = "heap_trace")]
    println!("[kernel] [stats] zombie_owner {}", zombie_parent_stats());
    #[cfg(feature = "heap_trace")]
    {
        crate::mm::heap_trace::print_summary();
    }
}
