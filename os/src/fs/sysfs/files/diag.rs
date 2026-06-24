//! Unified kernel diagnostics — /sys/kernel/stats/ and /sys/kernel/tracing/
//!
//! All instrumentation converges here. Stats are formatted key=value from
//! AtomicUsize counters in [`crate::task::perf`]. Tracing control is via
//! writable files backed by [`crate::trace`].

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use core::fmt::Write;
use core::sync::atomic::Ordering;

use crate::fs::sysfs::{SysContentFn, SysInode, SysWriteFn};
use crate::fs::vfs::InodeMode;
use crate::utils::error::SyscallErr;

// ── Helpers ────────────────────────────────────────────────────────────

fn write_str(offset: usize, len: usize, buf: &mut [u8], s: &str) -> Result<usize, SyscallErr> {
    let bytes = s.as_bytes();
    if offset >= bytes.len() {
        return Ok(0);
    }
    let n = len.min(bytes.len() - offset).min(buf.len());
    buf[..n].copy_from_slice(&bytes[offset..offset + n]);
    Ok(n)
}

fn read_counter(c: &core::sync::atomic::AtomicUsize) -> usize {
    c.load(Ordering::Relaxed)
}

// ═══════════════════════════════════════════════════════════════════════
//  STATS: Task Queue
// ═══════════════════════════════════════════════════════════════════════

fn stats_taskq_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(512);
    let _ = writeln!(s, "ready_len_max={}", read_counter(&crate::task::perf::READY_LEN_MAX));
    let _ = writeln!(s, "interruptible_len_max={}", read_counter(&crate::task::perf::INTERRUPTIBLE_LEN_MAX));
    let _ = writeln!(s, "ready_zombie_max={}", read_counter(&crate::task::perf::READY_ZOMBIE_MAX));
    let _ = writeln!(s, "interruptible_zombie_max={}", read_counter(&crate::task::perf::INTERRUPTIBLE_ZOMBIE_MAX));
    let _ = writeln!(s, "dup_enqueue_total={}", read_counter(&crate::task::perf::DUPLICATE_READY_ENQUEUE));
    let _ = writeln!(s, "add_ready_total={}", read_counter(&crate::task::perf::ADD_READY_TOTAL));
    let _ = writeln!(s, "add_interruptible_total={}", read_counter(&crate::task::perf::ADD_INTERRUPTIBLE_TOTAL));
    let _ = writeln!(s, "wake_interruptible_total={}", read_counter(&crate::task::perf::WAKE_INTERRUPTIBLE_TOTAL));
    let _ = writeln!(s, "fair_pick_calls={}", read_counter(&crate::task::perf::FAIR_PICK_CALLS));
    let _ = writeln!(s, "fast_path_calls={}", read_counter(&crate::task::perf::FAST_PATH_CALLS));
    let _ = writeln!(s, "fair_scan_max={}", read_counter(&crate::task::perf::FAIR_SCAN_MAX));
    let _ = writeln!(s, "zombie_drain_scan_total={}", read_counter(&crate::task::perf::ZOMBIE_DRAIN_SCAN_TOTAL));
    let _ = writeln!(s, "zombie_drain_calls={}", read_counter(&crate::task::perf::ZOMBIE_DRAIN_CALLS));
    let _ = writeln!(s, "zombie_drain_removed={}", read_counter(&crate::task::perf::ZOMBIE_DRAIN_REMOVED));
    let _ = writeln!(s, "ready_nonzero_nice_cur={}", read_counter(&crate::task::perf::READY_NONZERO_NICE_CUR));
    write_str(offset, len, buf, &s)
}

// ═══════════════════════════════════════════════════════════════════════
//  STATS: Kernel Timer
// ═══════════════════════════════════════════════════════════════════════

fn stats_timer_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(512);
    let _ = writeln!(s, "ktimer_len_max={}", read_counter(&crate::task::perf::KTIMER_LEN_MAX));
    let _ = writeln!(s, "ktimer_add_total={}", read_counter(&crate::task::perf::KTIMER_ADD_TOTAL));
    let _ = writeln!(s, "ktimer_pop_max={}", read_counter(&crate::task::perf::KTIMER_POP_MAX));
    let _ = writeln!(s, "ktimer_pop_total={}", read_counter(&crate::task::perf::KTIMER_POP_TOTAL));
    let _ = writeln!(s, "ktimer_stale_waketask={}", read_counter(&crate::task::perf::KTIMER_STALE_WAKETASK));
    let _ = writeln!(s, "ktimer_real_wake={}", read_counter(&crate::task::perf::KTIMER_REAL_WAKE));
    let _ = writeln!(s, "ktimer_compact_calls={}", read_counter(&crate::task::perf::KTIMER_COMPACT_CALLS));
    let _ = writeln!(s, "ktimer_stale_removed={}", read_counter(&crate::task::perf::KTIMER_STALE_REMOVED));
    let _ = writeln!(s, "wait_with_timeout_total={}", read_counter(&crate::task::perf::WAIT_WITH_TIMEOUT_TOTAL));
    let _ = writeln!(s, "timer_irq_ticks_total={}", read_counter(&crate::task::perf::TIMER_IRQ_TICKS_TOTAL));
    let _ = writeln!(s, "timer_irq_ticks_max={}", read_counter(&crate::task::perf::TIMER_IRQ_TICKS_MAX));
    let _ = writeln!(s, "timer_pop_nodes_total={}", read_counter(&crate::task::perf::TIMER_POP_NODES_TOTAL));
    let _ = writeln!(s, "timer_pop_ticks_total={}", read_counter(&crate::task::perf::TIMER_POP_TICKS_TOTAL));
    let _ = writeln!(s, "timer_pop_ticks_max={}", read_counter(&crate::task::perf::TIMER_POP_TICKS_MAX));
    write_str(offset, len, buf, &s)
}

// ═══════════════════════════════════════════════════════════════════════
//  STATS: Seccomp
// ═══════════════════════════════════════════════════════════════════════

fn stats_seccomp_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(256);
    let _ = writeln!(s, "seccomp_check_calls={}", read_counter(&crate::task::perf::SECCOMP_CHECK_CALLS));
    let _ = writeln!(s, "seccomp_check_ticks_total={}", read_counter(&crate::task::perf::SECCOMP_CHECK_TICKS_TOTAL));
    let _ = writeln!(s, "seccomp_check_ticks_max={}", read_counter(&crate::task::perf::SECCOMP_CHECK_TICKS_MAX));
    let _ = writeln!(s, "seccomp_disabled_bypass={}", read_counter(&crate::task::perf::SECCOMP_DISABLED_BYPASS));
    write_str(offset, len, buf, &s)
}

// ═══════════════════════════════════════════════════════════════════════
//  STATS: Syscall / Trap
// ═══════════════════════════════════════════════════════════════════════

fn stats_syscall_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(384);
    let _ = writeln!(s, "syscall_total={}", read_counter(&crate::task::perf::SYSCALL_TOTAL));
    let _ = writeln!(s, "syscall_getppid_total={}", read_counter(&crate::task::perf::SYSCALL_GETPPID_TOTAL));
    let _ = writeln!(s, "syscall_cost_max_ticks={}", read_counter(&crate::task::perf::SYSCALL_COST_MAX_TICKS));
    let _ = writeln!(s, "syscall_cost_ticks_total={}", read_counter(&crate::task::perf::SYSCALL_COST_TICKS_TOTAL));
    let _ = writeln!(s, "getppid_cost_ticks_total={}", read_counter(&crate::task::perf::GETPPID_COST_TICKS_TOTAL));
    let _ = writeln!(s, "getppid_cost_ticks_max={}", read_counter(&crate::task::perf::GETPPID_COST_TICKS_MAX));
    let _ = writeln!(s, "ecall_trap_cost_ticks_total={}", read_counter(&crate::task::perf::ECALL_TRAP_COST_TICKS_TOTAL));
    let _ = writeln!(s, "ecall_trap_cost_ticks_max={}", read_counter(&crate::task::perf::ECALL_TRAP_COST_TICKS_MAX));
    write_str(offset, len, buf, &s)
}

fn stats_ctxsw_content(_extra: usize, offset: usize, len: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(128);
    let _ = writeln!(s, "context_switch_total={}", read_counter(&crate::task::perf::CONTEXT_SWITCH_TOTAL));
    write_str(offset, len, buf, &s)
}

fn stats_reclaim_content(_extra: usize, offset: usize, len: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(192);
    let _ = writeln!(s, "reclaim_runs_total={}", read_counter(&crate::task::perf::RECLAIM_RUNS_TOTAL));
    let _ = writeln!(s, "reclaim_pages_scanned_total={}", read_counter(&crate::task::perf::RECLAIM_PAGES_SCANNED_TOTAL));
    let _ = writeln!(s, "reclaim_pages_freed_total={}", read_counter(&crate::task::perf::RECLAIM_PAGES_FREED_TOTAL));
    write_str(offset, len, buf, &s)
}

fn stats_tlb_content(_extra: usize, offset: usize, len: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(192);
    let _ = writeln!(s, "tlb_flushes={}", read_counter(&crate::task::perf::TLB_FLUSHES));
    let _ = writeln!(s, "tlb_full={}", read_counter(&crate::task::perf::TLB_FULL));
    let _ = writeln!(s, "tlb_page={}", read_counter(&crate::task::perf::TLB_PAGE));
    let _ = writeln!(s, "tlb_activate={}", read_counter(&crate::task::perf::TLB_ACTIVATE));
    let _ = writeln!(s, "tlb_global={}", read_counter(&crate::task::perf::TLB_GLOBAL));
    write_str(offset, len, buf, &s)
}

fn stats_heap_content(_extra: usize, offset: usize, len: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(512);
    let (free, total, _, _, _) = crate::mm::heap_stats();
    let _ = writeln!(s, "heap_current_bytes={}", crate::mm::KERNEL_HEAP_CURRENT_BYTES.load(Ordering::Relaxed));
    let _ = writeln!(s, "heap_max_bytes={}", crate::mm::KERNEL_HEAP_MAX_BYTES.load(Ordering::Relaxed));
    let _ = writeln!(s, "heap_free_kb={}", free >> 10);
    let _ = writeln!(s, "heap_total_kb={}", total >> 10);
    let _ = writeln!(s, "heap_alloc_calls={}", read_counter(&crate::task::perf::HEAP_ALLOC_CALLS));
    let _ = writeln!(s, "heap_alloc_ticks_total={}", read_counter(&crate::task::perf::HEAP_ALLOC_TICKS_TOTAL));
    let _ = writeln!(s, "heap_alloc_ticks_max={}", read_counter(&crate::task::perf::HEAP_ALLOC_TICKS_MAX));
    let _ = writeln!(s, "heap_dealloc_calls={}", read_counter(&crate::task::perf::HEAP_DEALLOC_CALLS));
    let _ = writeln!(s, "heap_dealloc_ticks_total={}", read_counter(&crate::task::perf::HEAP_DEALLOC_TICKS_TOTAL));
    let _ = writeln!(s, "heap_dealloc_ticks_max={}", read_counter(&crate::task::perf::HEAP_DEALLOC_TICKS_MAX));
    let _ = writeln!(s, "heap_dealloc_scan_steps_total={}", read_counter(&crate::task::perf::HEAP_DEALLOC_SCAN_STEPS_TOTAL));
    write_str(offset, len, buf, &s)
}

fn stats_resource_content(_extra: usize, offset: usize, len: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(512);
    let (ready, int_count) = crate::task::task_manager_counts().unwrap_or((0, 0));
    let (tcp, udp, raw, pending) = crate::net::config::NET_INTERFACE.socket_stats();
    let pipe_alive = crate::fs::dev::pipe::pipe_buf_alive();
    let pipe_bytes = crate::fs::dev::pipe::pipe_buf_bytes();
    let unix_ring = crate::net::socket::unix::ring_buffer::rb_alive();
    let unix_bytes = crate::net::socket::unix::ring_buffer::rb_bytes();
    let mounts = crate::fs::vfs::mount::counters::mountfs_alive();
    let mnt_inodes = crate::fs::vfs::mount::counters::mountfsinode_alive();
    let (dc_evict_total, dc_evict_sole, dc_evict_extern, dc_adv_removed) =
        crate::fs::vfs::dentry_cache::dcache_stats::snapshot();
    let (pc_reg_len, _pc_reg_cap, pc_reg_alive, pc_reg_stale) = crate::fs::page_cache::registry_stats();
    let (pc_ent_len, _pc_ent_cap, pc_ent_live, pc_ent_holes) = crate::fs::page_cache::entries_global_stats();
    let (heap_free, heap_total, _, alloc_actual, waste) = crate::mm::heap_stats();
    let free_frames = crate::mm::unallocated_frames();

    let _ = writeln!(s, "ready_tasks={}", ready);
    let _ = writeln!(s, "interruptible_tasks={}", int_count);
    let _ = writeln!(s, "free_frames={}", free_frames);
    let _ = writeln!(s, "heap_free_kb={}", heap_free >> 10);
    let _ = writeln!(s, "heap_total_kb={}", heap_total >> 10);
    let _ = writeln!(s, "heap_alloc_actual_kb={}", alloc_actual >> 10);
    let _ = writeln!(s, "heap_waste_kb={}", waste >> 10);
    let _ = writeln!(s, "tcp_sockets={}", tcp);
    let _ = writeln!(s, "udp_sockets={}", udp);
    let _ = writeln!(s, "raw_sockets={}", raw);
    let _ = writeln!(s, "pending_sockets={}", pending);
    let _ = writeln!(s, "pipe_buf_alive={}", pipe_alive);
    let _ = writeln!(s, "pipe_buf_bytes_kb={}", pipe_bytes >> 10);
    let _ = writeln!(s, "unix_ring_alive={}", unix_ring);
    let _ = writeln!(s, "unix_ring_bytes_kb={}", unix_bytes >> 10);
    let _ = writeln!(s, "mountfs_alive={}", mounts);
    let _ = writeln!(s, "mountfs_inode_alive={}", mnt_inodes);
    let _ = writeln!(s, "dc_evict_total={}", dc_evict_total);
    let _ = writeln!(s, "dc_evict_sole={}", dc_evict_sole);
    let _ = writeln!(s, "dc_evict_extern={}", dc_evict_extern);
    let _ = writeln!(s, "dc_advance_removed={}", dc_adv_removed);
    let _ = writeln!(s, "pc_registry_len={}", pc_reg_len);
    let _ = writeln!(s, "pc_registry_alive={}", pc_reg_alive);
    let _ = writeln!(s, "pc_registry_stale={}", pc_reg_stale);
    let _ = writeln!(s, "pc_entries_len={}", pc_ent_len);
    let _ = writeln!(s, "pc_entries_live={}", pc_ent_live);
    let _ = writeln!(s, "pc_entries_holes={}", pc_ent_holes);
    write_str(offset, len, buf, &s)
}

fn stats_buddyinfo_content(_extra: usize, offset: usize, len: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(256);
    let h = crate::mm::heap_free_histogram();
    let _ = writeln!(s, "order free_blocks");
    for (order, &count) in h.iter().enumerate() {
        if count > 0 {
            let _ = writeln!(s, "{} {}", order, count);
        }
    }
    write_str(offset, len, buf, &s)
}

fn stats_zombies_content(_extra: usize, offset: usize, len: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(256);
    let mut groups: alloc::vec::Vec<(usize, usize)> = alloc::vec::Vec::new();
    for pcb in crate::task::ProcessManager::all_processes() {
        if !pcb.is_zombie() { continue; }
        let parent_pid = pcb.parent_pid();
        if let Some((_, count)) = groups.iter_mut().find(|(pid, _)| *pid == parent_pid) {
            *count += 1;
        } else {
            groups.push((parent_pid, 1));
        }
    }
    groups.sort_by(|a, b| b.1.cmp(&a.1));
    let total_zombies: usize = groups.iter().map(|(_, c)| c).sum();
    let _ = writeln!(s, "total_zombies={}", total_zombies);
    for (parent_pid, zombie_count) in groups.into_iter().take(10) {
        let _ = writeln!(s, "parent_pid={} zombie_children={}", parent_pid, zombie_count);
    }
    write_str(offset, len, buf, &s)
}

fn tracing_trigger_write(_extra: usize, _offset: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
    if buf.is_empty() { return Err(SyscallErr::EINVAL); }
    let cmd = core::str::from_utf8(buf).map_err(|_| SyscallErr::EINVAL)?;
    match cmd.trim() {
        "buddy" | "zombie" | "heap" => { /* trigger accepted, scan deferred */ }
        _ => return Err(SyscallErr::EINVAL),
    }
    Ok(buf.len())
}

// ═══════════════════════════════════════════════════════════════════════
//  STATS: features (ro) — compile-time feature flags
// ═══════════════════════════════════════════════════════════════════════

fn stats_features_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(128);
    let _ = writeln!(s, "perf_stats={}", cfg!(feature = "perf_stats"));
    let _ = writeln!(s, "perf_diag={}", cfg!(feature = "perf_diag"));
    let _ = writeln!(s, "heap_trace={}", cfg!(feature = "heap_trace"));
    write_str(offset, len, buf, &s)
}

// ═══════════════════════════════════════════════════════════════════════
//  STATS: stats_on (rw) — enable/disable statistics collection
// ═══════════════════════════════════════════════════════════════════════

fn stats_on_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let val = if crate::task::perf::STATS_ON.load(Ordering::Relaxed) {
        "1\n"
    } else {
        "0\n"
    };
    write_str(offset, len, buf, val)
}

fn stats_on_write(_extra: usize, _offset: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
    let val = match buf.first() {
        Some(b'1') => true,
        Some(b'0') => false,
        _ => return Err(SyscallErr::EINVAL),
    };
    crate::task::perf::STATS_ON.store(val, Ordering::Relaxed);
    Ok(buf.len())
}

// ═══════════════════════════════════════════════════════════════════════
//  STATS: reset (w) — zero all P0 performance counters
// ═══════════════════════════════════════════════════════════════════════

fn stats_reset_write(_extra: usize, _offset: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
    crate::task::perf::reset_all_counters();
    Ok(buf.len())
}

// ═══════════════════════════════════════════════════════════════════════
//  TRACING: tracing_on (rw)
// ═══════════════════════════════════════════════════════════════════════

fn tracing_on_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let val = if crate::trace::TRACING_ON.load(Ordering::Relaxed) {
        "1\n"
    } else {
        "0\n"
    };
    write_str(offset, len, buf, val)
}

fn tracing_on_write(_extra: usize, _offset: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
    let val = match buf.first() {
        Some(b'1') => true,
        Some(b'0') => false,
        _ => return Err(SyscallErr::EINVAL),
    };
    crate::trace::TRACING_ON.store(val, Ordering::Relaxed);
    Ok(buf.len())
}

// ═══════════════════════════════════════════════════════════════════════
//  TRACING: trace (ro) — formatted ring buffer dump
// ═══════════════════════════════════════════════════════════════════════

fn trace_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let s = crate::trace::dump_to_string(512);
    write_str(offset, len, buf, &s)
}

// ═══════════════════════════════════════════════════════════════════════
//  TRACING: dropped (ro) — count of events dropped
// ═══════════════════════════════════════════════════════════════════════

fn trace_dropped_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let val = format!(
        "{}\n",
        crate::trace::TRACE_DROPPED.load(Ordering::Relaxed)
    );
    write_str(offset, len, buf, &val)
}

// ═══════════════════════════════════════════════════════════════════════
//  TRACING: buffer_size (ro)
// ═══════════════════════════════════════════════════════════════════════

fn buffer_size_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let val = format!("{}\n", crate::trace::TRACE_SIZE);
    write_str(offset, len, buf, &val)
}

// ═══════════════════════════════════════════════════════════════════════
//  TRACING: clear (w) — reset ring buffer
// ═══════════════════════════════════════════════════════════════════════

fn trace_clear_write(_extra: usize, _offset: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
    crate::trace::clear_ring();
    crate::trace::TRACE_DROPPED.store(0, Ordering::Relaxed);
    Ok(buf.len())
}

// ═══════════════════════════════════════════════════════════════════════
//  STATS: PageCache I/O
// ═══════════════════════════════════════════════════════════════════════

fn stats_pagecache_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(512);
    let _ = writeln!(s, "pc_read_calls={}", read_counter(&crate::task::perf::PC_READ_CALLS));
    let _ = writeln!(s, "pc_read_pages={}", read_counter(&crate::task::perf::PC_READ_PAGES));
    let _ = writeln!(s, "pc_read_miss={}", read_counter(&crate::task::perf::PC_READ_MISS));
    let _ = writeln!(s, "pc_read_cycles={}", read_counter(&crate::task::perf::PC_READ_CYCLES_TOTAL));
    let _ = writeln!(s, "pc_read_hit_cycles={}", read_counter(&crate::task::perf::PC_READ_HIT_CYCLES));
    let _ = writeln!(s, "pc_read_miss_cycles={}", read_counter(&crate::task::perf::PC_READ_MISS_CYCLES));
    let _ = writeln!(s, "pc_copy_cycles={}", read_counter(&crate::task::perf::PC_COPY_CYCLES));
    let _ = writeln!(s, "pc_lookup_cycles={}", read_counter(&crate::task::perf::PC_LOOKUP_CYCLES));
    let _ = writeln!(s, "pc_write_calls={}", read_counter(&crate::task::perf::PC_WRITE_CALLS));
    let _ = writeln!(s, "pc_write_pages={}", read_counter(&crate::task::perf::PC_WRITE_PAGES));
    let _ = writeln!(s, "pc_write_overwrite={}", read_counter(&crate::task::perf::PC_WRITE_OVERWRITE));
    let _ = writeln!(s, "pc_write_eventually_full={}", read_counter(&crate::task::perf::PC_WRITE_EVENTUALLY_FULL));
    let _ = writeln!(s, "pc_write_cycles={}", read_counter(&crate::task::perf::PC_WRITE_CYCLES_TOTAL));
    let _ = writeln!(s, "pc_wb_calls={}", read_counter(&crate::task::perf::PC_WRITEBACK_CALLS));
    let _ = writeln!(s, "pc_wb_pages={}", read_counter(&crate::task::perf::PC_WRITEBACK_PAGES));
    let _ = writeln!(s, "pc_wb_cycles={}", read_counter(&crate::task::perf::PC_WRITEBACK_CYCLES_TOTAL));
    let _ = writeln!(s, "pc_falloc_cycles={}", read_counter(&crate::task::perf::PC_FALLOC_CYCLES_TOTAL));
    write_str(offset, len, buf, &s)
}

// ═══════════════════════════════════════════════════════════════════════
//  STATS: Block I/O
// ═══════════════════════════════════════════════════════════════════════

fn stats_blockio_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(256);
    let _ = writeln!(s, "blk_vread_reqs={}", read_counter(&crate::task::perf::BLK_VREAD_REQS));
    let _ = writeln!(s, "blk_vread_secs={}", read_counter(&crate::task::perf::BLK_VREAD_SECS));
    let _ = writeln!(s, "blk_vwrite_reqs={}", read_counter(&crate::task::perf::BLK_VWRITE_REQS));
    let _ = writeln!(s, "blk_vwrite_secs={}", read_counter(&crate::task::perf::BLK_VWRITE_SECS));
    write_str(offset, len, buf, &s)
}

// ═══════════════════════════════════════════════════════════════════════
//  Registration
// ═══════════════════════════════════════════════════════════════════════

pub fn register_all(kernel_dir: &Arc<SysInode>) -> Result<(), SyscallErr> {
    let rw_mode = InodeMode::from_bits_truncate(0o644);
    let ro_mode = InodeMode::from_bits_truncate(0o444);

    let has_perf_stats = cfg!(feature = "perf_stats");
    let has_perf_diag = cfg!(feature = "perf_diag");
    crate::println!(
        "[kernel] perf_diag features: perf_stats={} perf_diag={}",
        has_perf_stats, has_perf_diag
    );

    // ── /sys/kernel/stats/ ──
    let stats_dir = kernel_dir.add_dir_inner("stats", InodeMode::from_bits_truncate(0o555))?;
    stats_dir.add_file("features", ro_mode, stats_features_content)?;
    stats_dir.add_writable_file_with_write("stats_on", rw_mode, stats_on_content, stats_on_write)?;
    stats_dir.add_write_only_file(
        "reset",
        InodeMode::from_bits_truncate(0o200),
        stats_reset_write,
    )?;
    stats_dir.add_file("taskq", ro_mode, stats_taskq_content)?;
    stats_dir.add_file("timer", ro_mode, stats_timer_content)?;
    stats_dir.add_file("seccomp", ro_mode, stats_seccomp_content)?;
    stats_dir.add_file("syscall", ro_mode, stats_syscall_content)?;
    stats_dir.add_file("ctxsw", ro_mode, stats_ctxsw_content)?;
    stats_dir.add_file("reclaim", ro_mode, stats_reclaim_content)?;
    stats_dir.add_file("tlb", ro_mode, stats_tlb_content)?;
    stats_dir.add_file("heap", ro_mode, stats_heap_content)?;
    stats_dir.add_file("pagecache", ro_mode, stats_pagecache_content)?;
    stats_dir.add_file("blockio", ro_mode, stats_blockio_content)?;
    stats_dir.add_file("resource", ro_mode, stats_resource_content)?;
    stats_dir.add_file("buddyinfo", ro_mode, stats_buddyinfo_content)?;
    stats_dir.add_file("zombies", ro_mode, stats_zombies_content)?;

    // ── /sys/kernel/tracing/ ──
    let trace_dir = kernel_dir.add_dir_inner("tracing", InodeMode::from_bits_truncate(0o555))?;
    trace_dir.add_writable_file_with_write(
        "tracing_on",
        rw_mode,
        tracing_on_content,
        tracing_on_write,
    )?;
    trace_dir.add_file("trace", ro_mode, trace_content)?;
    trace_dir.add_file("dropped", ro_mode, trace_dropped_content)?;
    trace_dir.add_file("buffer_size", ro_mode, buffer_size_content)?;
    trace_dir.add_write_only_file(
        "clear",
        InodeMode::from_bits_truncate(0o200),
        trace_clear_write,
    )?;
    trace_dir.add_write_only_file(
        "trigger",
        InodeMode::from_bits_truncate(0o200),
        tracing_trigger_write,
    )?;

    Ok(())
}
