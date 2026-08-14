//! Unified kernel diagnostics — /sys/kernel/stats/ and /sys/kernel/tracing/
//!
//! All instrumentation converges here. Stats are formatted key=value from
//! AtomicUsize counters in [`crate::task::perf`]. Tracing control is via
//! writable files backed by [`crate::trace`].

use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
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

fn diagnostic_token(value: &str, limit: usize) -> String {
    let mut token = String::new();
    for ch in value.chars().take(limit) {
        token.push(if ch.is_whitespace() || ch == '=' {
            '_'
        } else {
            ch
        });
    }
    if token.is_empty() {
        token.push('-');
    }
    token
}

// ═══════════════════════════════════════════════════════════════════════
//  STATS: one-shot boot milestones (elapsed raw ticks from Rust entry)
// ═══════════════════════════════════════════════════════════════════════

fn stats_boot_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(320);
    let _ = writeln!(s, "clock_freq_hz={}", crate::hal::get_clock_freq());
    macro_rules! counter {
        ($name:literal, $counter:ident) => {
            let _ = writeln!(
                s,
                concat!($name, "={}"),
                read_counter(&crate::task::perf::$counter)
            );
        };
    }
    counter!("boot_console_ticks", BOOT_CONSOLE_TICKS);
    counter!("boot_mm_ticks", BOOT_MM_TICKS);
    counter!("boot_drivers_ticks", BOOT_DRIVERS_TICKS);
    counter!("boot_net_ticks", BOOT_NET_TICKS);
    counter!("boot_fs_ticks", BOOT_FS_TICKS);
    counter!("boot_initproc_ticks", BOOT_INITPROC_TICKS);
    counter!("boot_scheduler_ticks", BOOT_SCHEDULER_TICKS);
    write_str(offset, len, buf, &s)
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
    // Leave room for per-CPU and live-task identity snapshots appended below.
    let mut s = String::with_capacity(32768);
    let _ = writeln!(
        s,
        "scheduler_counter_schema_version={}",
        crate::task::perf::SCHED_COUNTER_SCHEMA_VERSION
    );
    let _ = writeln!(
        s,
        "ready_len_max={}",
        read_counter(&crate::task::perf::READY_LEN_MAX)
    );
    let _ = writeln!(
        s,
        "interruptible_len_max={}",
        read_counter(&crate::task::perf::INTERRUPTIBLE_LEN_MAX)
    );
    let _ = writeln!(
        s,
        "ready_zombie_max={}",
        read_counter(&crate::task::perf::READY_ZOMBIE_MAX)
    );
    let _ = writeln!(
        s,
        "interruptible_zombie_max={}",
        read_counter(&crate::task::perf::INTERRUPTIBLE_ZOMBIE_MAX)
    );
    let _ = writeln!(
        s,
        "dup_enqueue_total={}",
        read_counter(&crate::task::perf::DUPLICATE_READY_ENQUEUE)
    );
    let _ = writeln!(
        s,
        "add_ready_total={}",
        read_counter(&crate::task::perf::ADD_READY_TOTAL)
    );
    let _ = writeln!(
        s,
        "add_interruptible_total={}",
        read_counter(&crate::task::perf::ADD_INTERRUPTIBLE_TOTAL)
    );
    let _ = writeln!(
        s,
        "wake_interruptible_total={}",
        read_counter(&crate::task::perf::WAKE_INTERRUPTIBLE_TOTAL)
    );
    let _ = writeln!(
        s,
        "fair_pick_calls={}",
        read_counter(&crate::task::perf::FAIR_PICK_CALLS)
    );
    let _ = writeln!(
        s,
        "fast_path_calls={}",
        read_counter(&crate::task::perf::FAST_PATH_CALLS)
    );
    let _ = writeln!(
        s,
        "fair_scan_max={}",
        read_counter(&crate::task::perf::FAIR_SCAN_MAX)
    );
    let _ = writeln!(
        s,
        "zombie_drain_scan_total={}",
        read_counter(&crate::task::perf::ZOMBIE_DRAIN_SCAN_TOTAL)
    );
    let _ = writeln!(
        s,
        "zombie_drain_calls={}",
        read_counter(&crate::task::perf::ZOMBIE_DRAIN_CALLS)
    );
    let _ = writeln!(
        s,
        "zombie_drain_removed={}",
        read_counter(&crate::task::perf::ZOMBIE_DRAIN_REMOVED)
    );
    let _ = writeln!(
        s,
        "ready_nonzero_nice_cur={}",
        read_counter(&crate::task::perf::READY_NONZERO_NICE_CUR)
    );
    let _ = writeln!(
        s,
        "wake_local={}",
        read_counter(&crate::task::perf::WAKE_LOCAL)
    );
    let _ = writeln!(
        s,
        "wake_remote={}",
        read_counter(&crate::task::perf::WAKE_REMOTE)
    );
    let _ = writeln!(
        s,
        "wake_keep_last_cpu={}",
        read_counter(&crate::task::perf::WAKE_KEEP_LAST_CPU)
    );
    let _ = writeln!(
        s,
        "wake_select_idle_cpu={}",
        read_counter(&crate::task::perf::WAKE_SELECT_IDLE_CPU)
    );
    let _ = writeln!(
        s,
        "wake_select_least_loaded={}",
        read_counter(&crate::task::perf::WAKE_SELECT_LEAST_LOADED)
    );
    let _ = writeln!(
        s,
        "wake_last_busy_idle_available={}",
        read_counter(&crate::task::perf::WAKE_LAST_BUSY_IDLE_AVAILABLE)
    );
    let _ = writeln!(
        s,
        "new_task_idle_available={}",
        read_counter(&crate::task::perf::NEW_TASK_IDLE_AVAILABLE)
    );
    let _ = writeln!(
        s,
        "new_task_selected_idle={}",
        read_counter(&crate::task::perf::NEW_TASK_SELECTED_IDLE)
    );
    let _ = writeln!(
        s,
        "new_task_kept_busy_parent={}",
        read_counter(&crate::task::perf::NEW_TASK_KEPT_BUSY_PARENT)
    );
    let _ = writeln!(
        s,
        "wake_to_run_ticks_total={}",
        read_counter(&crate::task::perf::WAKE_TO_RUN_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "wake_to_run_ticks_max={}",
        read_counter(&crate::task::perf::WAKE_TO_RUN_TICKS_MAX)
    );
    let _ = writeln!(
        s,
        "task_run_slice_ticks_total={}",
        read_counter(&crate::task::perf::TASK_RUN_SLICE_TICKS_TOTAL)
    );
    let _ = writeln!(s, "scheduler_preempt_counter_schema_version={}", crate::task::perf::SCHED_PREEMPT_COUNTER_SCHEMA_VERSION);
    let _ = writeln!(s, "timer_preemptions={}", read_counter(&crate::task::perf::TIMER_PREEMPTIONS));
    let _ = writeln!(s, "timer_preemptions_no_local_competitor={}", read_counter(&crate::task::perf::TIMER_PREEMPTIONS_NO_LOCAL_COMPETITOR));
    let _ = writeln!(s, "timer_preemptions_with_local_competitor={}", read_counter(&crate::task::perf::TIMER_PREEMPTIONS_WITH_LOCAL_COMPETITOR));
    let _ = writeln!(s, "timer_preemptions_with_ipi={}", read_counter(&crate::task::perf::TIMER_PREEMPTIONS_WITH_IPI));
    let _ = writeln!(s, "timer_preemptions_elided_no_competitor={}", read_counter(&crate::task::perf::TIMER_PREEMPTIONS_ELIDED_NO_COMPETITOR));
    let _ = writeln!(s, "timer_same_task_resumes={}", read_counter(&crate::task::perf::TIMER_SAME_TASK_RESUMES));
    let _ = writeln!(
        s,
        "steal_attempts={}",
        read_counter(&crate::task::perf::STEAL_ATTEMPTS)
    );
    let _ = writeln!(
        s,
        "steal_candidate_found={}",
        read_counter(&crate::task::perf::STEAL_CANDIDATE_FOUND)
    );
    let _ = writeln!(
        s,
        "steal_no_remote_ready={}",
        read_counter(&crate::task::perf::STEAL_NO_REMOTE_READY)
    );
    let _ = writeln!(
        s,
        "steal_no_eligible_candidate={}",
        read_counter(&crate::task::perf::STEAL_NO_ELIGIBLE_CANDIDATE)
    );
    let _ = writeln!(
        s,
        "steal_success={}",
        read_counter(&crate::task::perf::STEAL_SUCCESS)
    );
    let _ = writeln!(
        s,
        "steal_recheck_failed={}",
        read_counter(&crate::task::perf::STEAL_RECHECK_FAILED)
    );
    let _ = writeln!(
        s,
        "steal_ktlb_sync_calls={}",
        read_counter(&crate::task::perf::STEAL_KTLB_SYNC_CALLS)
    );
    let _ = writeln!(
        s,
        "steal_ktlb_sync_ticks_total={}",
        read_counter(&crate::task::perf::STEAL_KTLB_SYNC_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "steal_ktlb_sync_ticks_max={}",
        read_counter(&crate::task::perf::STEAL_KTLB_SYNC_TICKS_MAX)
    );
    let mut cpu_snapshots = Vec::new();
    let mut runnable_total = 0usize;
    let mut current_total_excluding_collector = 0usize;
    for cpu in 0..crate::smp::configured_cpu_count() {
        let snapshot = crate::smp::task_state(cpu).read_diagnostics();
        runnable_total = runnable_total.saturating_add(snapshot.nr_running);
        current_total_excluding_collector = current_total_excluding_collector.saturating_add(
            usize::from(snapshot.current_present && snapshot.current_pid != 3),
        );
        cpu_snapshots.push(snapshot);
    }
    let _ = writeln!(
        s,
        "scheduler_clock_freq_hz={}",
        crate::hal::get_clock_freq()
    );
    let _ = writeln!(s, "runnable_total={}", runnable_total);
    let _ = writeln!(
        s,
        "current_total_excluding_collector={}",
        current_total_excluding_collector
    );
    let _ = writeln!(
        s,
        "active_tasks_excluding_collector={}",
        runnable_total.saturating_add(current_total_excluding_collector)
    );
    for (cpu, snapshot) in cpu_snapshots.iter().enumerate() {
        let syscall_id = snapshot
            .current_syscall_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| String::from("-"));
        let _ = writeln!(
            s,
            "cpu{}_current_present={} cpu{}_nr_running={} cpu{}_current_pid={} cpu{}_current_tid={} cpu{}_current_syscall_id={} cpu{}_steals={}",
            cpu,
            usize::from(snapshot.current_present),
            cpu,
            snapshot.nr_running,
            cpu,
            snapshot.current_pid,
            cpu,
            snapshot.current_tid,
            cpu,
            syscall_id,
            cpu,
            snapshot.steals,
        );
    }
    for process in crate::task::all_processes() {
        let (exe_path, crate_name) = process.exec_diagnostics();
        let exe_name = exe_path.rsplit('/').next().unwrap_or(&exe_path);
        let exe_name = diagnostic_token(exe_name, 96);
        let crate_name = diagnostic_token(&crate_name, 64);
        for task in process.threads() {
            let tid = task.gettid();
            let (
                comm,
                user_us,
                system_us,
                blocked_us,
                runnable_wait_us,
                blocked_reason,
                blocked_syscall_id,
            ) = task.runtime_diagnostics();
            let comm_len = comm
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(comm.len());
            let comm = diagnostic_token(core::str::from_utf8(&comm[..comm_len]).unwrap_or("-"), 15);
            let current = cpu_snapshots
                .iter()
                .enumerate()
                .find(|(_, snapshot)| snapshot.current_present && snapshot.current_tid == tid);
            let current_cpu = current
                .map(|(cpu, _)| cpu.to_string())
                .unwrap_or_else(|| String::from("-"));
            let syscall_id = current
                .and_then(|(_, snapshot)| snapshot.current_syscall_id)
                .map(|id| id.to_string())
                .unwrap_or_else(|| String::from("-"));
            let blocked_syscall_id = blocked_syscall_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| String::from("-"));
            let _ = writeln!(
                s,
                "task_diag pid={} tid={} state={:?} current_cpu={} comm={} exe={} crate={} syscall_id={} user_us={} kernel_us={} blocked_us={} runnable_wait_us={} blocked_reason={} blocked_syscall_id={}",
                process.pid,
                tid,
                task.task_status(),
                current_cpu,
                comm,
                exe_name,
                crate_name,
                syscall_id,
                user_us,
                system_us,
                blocked_us,
                runnable_wait_us,
                blocked_reason.as_str(),
                blocked_syscall_id,
            );
        }
    }
    for cpu in 0..crate::smp::MAX_CPUS {
        let _ = writeln!(
            s,
            "steal_attempts_cpu{}={}",
            cpu,
            read_counter(&crate::task::perf::STEAL_ATTEMPTS_BY_CPU[cpu])
        );
        let _ = writeln!(
            s,
            "steal_success_cpu{}={}",
            cpu,
            read_counter(&crate::task::perf::STEAL_SUCCESS_BY_CPU[cpu])
        );
        let _ = writeln!(
            s,
            "idle_busy_loops_cpu{}={}",
            cpu,
            read_counter(&crate::task::perf::SCHED_IDLE_BUSY_LOOPS_BY_CPU[cpu])
        );
        let _ = writeln!(
            s,
            "idle_wait_loops_cpu{}={}",
            cpu,
            read_counter(&crate::task::perf::SCHED_IDLE_WAIT_LOOPS_BY_CPU[cpu])
        );
        let _ = writeln!(
            s,
            "timer_preempt_last_tid_cpu{}={}",
            cpu,
            read_counter(&crate::task::perf::TIMER_PREEMPT_LAST_TID[cpu])
        );
    }
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
    let mut s = String::with_capacity(4096);
    let _ = writeln!(
        s,
        "ktimer_len_max={}",
        read_counter(&crate::task::perf::KTIMER_LEN_MAX)
    );
    let _ = writeln!(
        s,
        "ktimer_add_total={}",
        read_counter(&crate::task::perf::KTIMER_ADD_TOTAL)
    );
    let _ = writeln!(
        s,
        "ktimer_pop_max={}",
        read_counter(&crate::task::perf::KTIMER_POP_MAX)
    );
    let _ = writeln!(
        s,
        "ktimer_pop_total={}",
        read_counter(&crate::task::perf::KTIMER_POP_TOTAL)
    );
    let _ = writeln!(
        s,
        "ktimer_stale_waketask={}",
        read_counter(&crate::task::perf::KTIMER_STALE_WAKETASK)
    );
    let _ = writeln!(
        s,
        "ktimer_real_wake={}",
        read_counter(&crate::task::perf::KTIMER_REAL_WAKE)
    );
    let _ = writeln!(
        s,
        "ktimer_compact_calls={}",
        read_counter(&crate::task::perf::KTIMER_COMPACT_CALLS)
    );
    let _ = writeln!(
        s,
        "ktimer_stale_removed={}",
        read_counter(&crate::task::perf::KTIMER_STALE_REMOVED)
    );
    let _ = writeln!(
        s,
        "wait_with_timeout_total={}",
        read_counter(&crate::task::perf::WAIT_WITH_TIMEOUT_TOTAL)
    );
    let _ = writeln!(
        s,
        "timer_irq_ticks_total={}",
        read_counter(&crate::task::perf::TIMER_IRQ_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "timer_irq_ticks_max={}",
        read_counter(&crate::task::perf::TIMER_IRQ_TICKS_MAX)
    );
    let _ = writeln!(
        s,
        "timer_pop_nodes_total={}",
        read_counter(&crate::task::perf::TIMER_POP_NODES_TOTAL)
    );
    let _ = writeln!(
        s,
        "timer_pop_ticks_total={}",
        read_counter(&crate::task::perf::TIMER_POP_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "timer_pop_ticks_max={}",
        read_counter(&crate::task::perf::TIMER_POP_TICKS_MAX)
    );
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
    let _ = writeln!(
        s,
        "seccomp_check_calls={}",
        read_counter(&crate::task::perf::SECCOMP_CHECK_CALLS)
    );
    let _ = writeln!(
        s,
        "seccomp_check_ticks_total={}",
        read_counter(&crate::task::perf::SECCOMP_CHECK_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "seccomp_check_ticks_max={}",
        read_counter(&crate::task::perf::SECCOMP_CHECK_TICKS_MAX)
    );
    let _ = writeln!(
        s,
        "seccomp_disabled_bypass={}",
        read_counter(&crate::task::perf::SECCOMP_DISABLED_BYPASS)
    );
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
    let mut s = String::with_capacity(512);
    let _ = writeln!(
        s,
        "syscall_total={}",
        read_counter(&crate::task::perf::SYSCALL_TOTAL)
    );
    let _ = writeln!(
        s,
        "syscall_getppid_total={}",
        read_counter(&crate::task::perf::SYSCALL_GETPPID_TOTAL)
    );
    let _ = writeln!(
        s,
        "syscall_cost_max_ticks={}",
        read_counter(&crate::task::perf::SYSCALL_COST_MAX_TICKS)
    );
    let _ = writeln!(
        s,
        "syscall_cost_ticks_total={}",
        read_counter(&crate::task::perf::SYSCALL_COST_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "getppid_cost_ticks_total={}",
        read_counter(&crate::task::perf::GETPPID_COST_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "getppid_cost_ticks_max={}",
        read_counter(&crate::task::perf::GETPPID_COST_TICKS_MAX)
    );
    let _ = writeln!(
        s,
        "ecall_trap_cost_ticks_total={}",
        read_counter(&crate::task::perf::ECALL_TRAP_COST_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "ecall_trap_cost_ticks_max={}",
        read_counter(&crate::task::perf::ECALL_TRAP_COST_TICKS_MAX)
    );
    let _ = writeln!(
        s,
        "user_trap_returns={}",
        read_counter(&crate::task::perf::USER_TRAP_RETURNS)
    );
    let _ = writeln!(
        s,
        "user_return_barriers={}",
        read_counter(&crate::task::perf::USER_RETURN_BARRIERS)
    );
    let _ = writeln!(
        s,
        "user_unaligned_traps={}",
        read_counter(&crate::task::perf::USER_UNALIGNED_TRAPS)
    );
    let _ = writeln!(
        s,
        "user_unaligned_ticks_total={}",
        read_counter(&crate::task::perf::USER_UNALIGNED_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "user_unaligned_ticks_max={}",
        read_counter(&crate::task::perf::USER_UNALIGNED_TICKS_MAX)
    );
    macro_rules! unaligned_counter {
        ($name:literal, $counter:ident) => {
            let _ = writeln!(
                s,
                concat!($name, "={}"),
                read_counter(&crate::task::perf::$counter)
            );
        };
    }
    unaligned_counter!("user_unaligned_load_2", USER_UNALIGNED_LOAD_2);
    unaligned_counter!("user_unaligned_load_4", USER_UNALIGNED_LOAD_4);
    unaligned_counter!("user_unaligned_load_8", USER_UNALIGNED_LOAD_8);
    unaligned_counter!("user_unaligned_store_2", USER_UNALIGNED_STORE_2);
    unaligned_counter!("user_unaligned_store_4", USER_UNALIGNED_STORE_4);
    unaligned_counter!("user_unaligned_store_8", USER_UNALIGNED_STORE_8);
    unaligned_counter!("user_unaligned_float_loads", USER_UNALIGNED_FLOAT_LOADS);
    unaligned_counter!("user_unaligned_float_stores", USER_UNALIGNED_FLOAT_STORES);
    write_str(offset, len, buf, &s)
}

fn stats_ctxsw_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(128);
    let _ = writeln!(
        s,
        "context_switch_total={}",
        read_counter(&crate::task::perf::CONTEXT_SWITCH_TOTAL)
    );
    write_str(offset, len, buf, &s)
}

fn stats_reclaim_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(256);
    let _ = writeln!(
        s,
        "reclaim_runs_total={}",
        read_counter(&crate::task::perf::RECLAIM_RUNS_TOTAL)
    );
    let _ = writeln!(
        s,
        "reclaim_pages_scanned_total={}",
        read_counter(&crate::task::perf::RECLAIM_PAGES_SCANNED_TOTAL)
    );
    let _ = writeln!(
        s,
        "reclaim_pages_freed_total={}",
        read_counter(&crate::task::perf::RECLAIM_PAGES_FREED_TOTAL)
    );
    let _ = writeln!(
        s,
        "clock_scanned={}",
        read_counter(&crate::task::perf::CLOCK_SCANNED)
    );
    let _ = writeln!(
        s,
        "clock_second_chance={}",
        read_counter(&crate::task::perf::CLOCK_SECOND_CHANCE)
    );
    let _ = writeln!(
        s,
        "clock_evicted={}",
        read_counter(&crate::task::perf::CLOCK_EVICTED)
    );
    write_str(offset, len, buf, &s)
}

fn stats_tlb_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(3072);
    let remote = crate::smp::tlb_diagnostics();
    let _ = writeln!(
        s,
        "tlb_flushes={}",
        read_counter(&crate::task::perf::TLB_FLUSHES)
    );
    let _ = writeln!(s, "tlb_full={}", read_counter(&crate::task::perf::TLB_FULL));
    let _ = writeln!(s, "tlb_page={}", read_counter(&crate::task::perf::TLB_PAGE));
    let _ = writeln!(
        s,
        "tlb_activate={}",
        read_counter(&crate::task::perf::TLB_ACTIVATE)
    );
    let _ = writeln!(
        s,
        "tlb_global={}",
        read_counter(&crate::task::perf::TLB_GLOBAL)
    );
    let _ = writeln!(s, "tlb_shootdown_kernel_full={}", remote.kernel_full);
    let _ = writeln!(s, "tlb_shootdown_user_full={}", remote.user_full);
    let _ = writeln!(
        s,
        "tlb_shootdown_user_range_firmware={}",
        remote.user_range_firmware
    );
    let _ = writeln!(s, "tlb_shootdown_user_range_ipi={}", remote.user_range_ipi);
    let _ = writeln!(
        s,
        "tlb_shootdown_user_range_fallback={}",
        remote.user_range_fallback
    );
    let _ = writeln!(s, "tlb_shootdown_range_pages={}", remote.user_range_pages);
    let _ = writeln!(s, "tlb_shootdown_remote_targets={}", remote.remote_targets);
    let _ = writeln!(
        s,
        "tlb_shootdown_sync_ticks_total={}",
        remote.sync_ticks_total
    );
    let _ = writeln!(s, "tlb_shootdown_sync_ticks_max={}", remote.sync_ticks_max);
    let _ = writeln!(s, "tlb_shootdown_failures={}", remote.sync_failures);
    for index in 0..crate::smp::TLB_SHOOTDOWN_KIND_COUNT {
        let name = crate::smp::TLB_SHOOTDOWN_KIND_NAMES[index];
        let _ = writeln!(
            s,
            "tlb_shootdown_{}_ticks_total={}",
            name, remote.sync_ticks_by_kind_total[index]
        );
        let _ = writeln!(
            s,
            "tlb_shootdown_{}_ticks_max={}",
            name, remote.sync_ticks_by_kind_max[index]
        );
    }
    for index in 0..crate::smp::TLB_RFENCE_BUCKET_COUNT {
        let name = crate::smp::TLB_RFENCE_BUCKET_NAMES[index];
        let _ = writeln!(
            s,
            "tlb_rfence_bucket_{}_calls={}",
            name, remote.rfence_bucket_calls[index]
        );
        let _ = writeln!(
            s,
            "tlb_rfence_bucket_{}_ticks={}",
            name, remote.rfence_bucket_ticks[index]
        );
        let _ = writeln!(
            s,
            "tlb_rfence_bucket_{}_pages={}",
            name, remote.rfence_bucket_pages[index]
        );
        let _ = writeln!(
            s,
            "tlb_rfence_bucket_{}_targets={}",
            name, remote.rfence_bucket_targets[index]
        );
    }
    let _ = writeln!(
        s,
        "tlb_shootdown_clock_freq_hz={}",
        crate::hal::get_clock_freq()
    );
    write_str(offset, len, buf, &s)
}

fn stats_heap_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(4096);
    let (free, total, _, _, _) = crate::mm::heap_stats();
    let _ = writeln!(
        s,
        "heap_counter_schema_version={}",
        crate::task::perf::HEAP_COUNTER_SCHEMA_VERSION
    );
    let _ = writeln!(
        s,
        "heap_current_bytes={}",
        crate::mm::KERNEL_HEAP_CURRENT_BYTES.load(Ordering::Relaxed)
    );
    let _ = writeln!(
        s,
        "heap_max_bytes={}",
        crate::mm::KERNEL_HEAP_MAX_BYTES.load(Ordering::Relaxed)
    );
    let _ = writeln!(s, "heap_free_kb={}", free >> 10);
    let _ = writeln!(s, "heap_total_kb={}", total >> 10);
    let _ = writeln!(
        s,
        "heap_alloc_calls={}",
        read_counter(&crate::task::perf::HEAP_ALLOC_CALLS)
    );
    let _ = writeln!(
        s,
        "heap_alloc_ticks_total={}",
        read_counter(&crate::task::perf::HEAP_ALLOC_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "heap_alloc_ticks_max={}",
        read_counter(&crate::task::perf::HEAP_ALLOC_TICKS_MAX)
    );
    let _ = writeln!(
        s,
        "heap_dealloc_calls={}",
        read_counter(&crate::task::perf::HEAP_DEALLOC_CALLS)
    );
    let _ = writeln!(
        s,
        "heap_dealloc_ticks_total={}",
        read_counter(&crate::task::perf::HEAP_DEALLOC_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "heap_dealloc_ticks_max={}",
        read_counter(&crate::task::perf::HEAP_DEALLOC_TICKS_MAX)
    );
    let _ = writeln!(
        s,
        "heap_dealloc_scan_steps_total={}",
        read_counter(&crate::task::perf::HEAP_DEALLOC_SCAN_STEPS_TOTAL)
    );
    let _ = writeln!(
        s,
        "heap_alloc_requested_bytes={}",
        read_counter(&crate::task::perf::HEAP_ALLOC_REQUESTED_BYTES)
    );
    let _ = writeln!(
        s,
        "heap_lock_wait_ticks_total={}",
        read_counter(&crate::task::perf::HEAP_LOCK_WAIT_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "heap_lock_wait_ticks_max={}",
        read_counter(&crate::task::perf::HEAP_LOCK_WAIT_TICKS_MAX)
    );
    let _ = writeln!(
        s,
        "heap_lock_hold_ticks_total={}",
        read_counter(&crate::task::perf::HEAP_LOCK_HOLD_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "heap_lock_hold_ticks_max={}",
        read_counter(&crate::task::perf::HEAP_LOCK_HOLD_TICKS_MAX)
    );
    let _ = writeln!(
        s,
        "heap_alloc_lock_wait_ticks_total={}",
        read_counter(&crate::task::perf::HEAP_ALLOC_LOCK_WAIT_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "heap_alloc_lock_hold_ticks_total={}",
        read_counter(&crate::task::perf::HEAP_ALLOC_LOCK_HOLD_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "heap_dealloc_lock_wait_ticks_total={}",
        read_counter(&crate::task::perf::HEAP_DEALLOC_LOCK_WAIT_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "heap_dealloc_lock_hold_ticks_total={}",
        read_counter(&crate::task::perf::HEAP_DEALLOC_LOCK_HOLD_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "heap_slab_eligible_calls={}",
        read_counter(&crate::task::perf::HEAP_SLAB_ALLOC_CALLS)
    );
    let _ = writeln!(
        s,
        "heap_slab_fast_calls={}",
        read_counter(&crate::task::perf::HEAP_SLAB_FAST_CALLS)
    );
    let _ = writeln!(
        s,
        "heap_slab_refill_calls={}",
        read_counter(&crate::task::perf::HEAP_SLAB_REFILL_CALLS)
    );
    let _ = writeln!(
        s,
        "heap_slab_fallback_calls={}",
        read_counter(&crate::task::perf::HEAP_SLAB_FALLBACK_CALLS)
    );
    let _ = writeln!(
        s,
        "heap_direct_buddy_calls={}",
        read_counter(&crate::task::perf::HEAP_DIRECT_BUDDY_CALLS)
    );
    let _ = writeln!(
        s,
        "heap_direct_buddy_failures={}",
        read_counter(&crate::task::perf::HEAP_DIRECT_BUDDY_FAILURES)
    );
    let _ = writeln!(
        s,
        "heap_alloc_retry_attempts={}",
        read_counter(&crate::task::perf::HEAP_ALLOC_RETRY_ATTEMPTS)
    );
    let _ = writeln!(
        s,
        "heap_recovery_attempts={}",
        read_counter(&crate::task::perf::HEAP_RECOVERY_ATTEMPTS)
    );
    let _ = writeln!(
        s,
        "heap_recovery_successes={}",
        read_counter(&crate::task::perf::HEAP_RECOVERY_SUCCESSES)
    );
    let _ = writeln!(
        s,
        "heap_alloc_final_failures={}",
        read_counter(&crate::task::perf::HEAP_ALLOC_FINAL_FAILURES)
    );
    let heap_classes = [
        (8, &crate::task::perf::HEAP_CLASS_8_CALLS),
        (16, &crate::task::perf::HEAP_CLASS_16_CALLS),
        (32, &crate::task::perf::HEAP_CLASS_32_CALLS),
        (64, &crate::task::perf::HEAP_CLASS_64_CALLS),
        (128, &crate::task::perf::HEAP_CLASS_128_CALLS),
        (256, &crate::task::perf::HEAP_CLASS_256_CALLS),
        (512, &crate::task::perf::HEAP_CLASS_512_CALLS),
        (1024, &crate::task::perf::HEAP_CLASS_1024_CALLS),
        (2048, &crate::task::perf::HEAP_CLASS_2048_CALLS),
    ];
    for (bytes, counter) in heap_classes {
        let _ = writeln!(s, "heap_class_{}_calls={}", bytes, read_counter(counter));
    }
    let hist_names = [
        "zero",
        "1_63",
        "64_1023",
        "1024_16383",
        "16384_262143",
        "ge262144",
    ];
    for (index, name) in hist_names.iter().enumerate() {
        let _ = writeln!(
            s,
            "heap_lock_wait_hist_{}={}",
            name,
            read_counter(&crate::task::perf::HEAP_LOCK_WAIT_HIST[index])
        );
        let _ = writeln!(
            s,
            "heap_lock_hold_hist_{}={}",
            name,
            read_counter(&crate::task::perf::HEAP_LOCK_HOLD_HIST[index])
        );
    }
    for cpu in 0..crate::smp::MAX_CPUS {
        let _ = writeln!(
            s,
            "heap_cpu{}_lock_calls={}",
            cpu,
            read_counter(&crate::task::perf::HEAP_LOCK_CALLS_BY_CPU[cpu])
        );
        let _ = writeln!(
            s,
            "heap_cpu{}_lock_wait_ticks_total={}",
            cpu,
            read_counter(&crate::task::perf::HEAP_LOCK_WAIT_TICKS_BY_CPU[cpu])
        );
        let _ = writeln!(
            s,
            "heap_cpu{}_lock_hold_ticks_total={}",
            cpu,
            read_counter(&crate::task::perf::HEAP_LOCK_HOLD_TICKS_BY_CPU[cpu])
        );
    }
    let _ = writeln!(
        s,
        "page_faults={}",
        read_counter(&crate::task::perf::PAGE_FAULTS)
    );
    let _ = writeln!(
        s,
        "pagefault_ticks_total={}",
        read_counter(&crate::task::perf::PAGEFAULT_TIME_TICKS)
    );
    let _ = writeln!(
        s,
        "pagefault_time_count={}",
        read_counter(&crate::task::perf::PAGEFAULT_TIME_COUNT)
    );
    let _ = writeln!(
        s,
        "frame_alloc_hits={}",
        read_counter(&crate::task::perf::FRAME_ALLOC_HITS)
    );
    let _ = writeln!(
        s,
        "frame_free_hits={}",
        read_counter(&crate::task::perf::FRAME_FREE_HITS)
    );
    let _ = writeln!(
        s,
        "frame_alloc_ticks_total={}",
        read_counter(&crate::task::perf::FRAME_ALLOC_TIME_TICKS)
    );
    let _ = writeln!(
        s,
        "frame_alloc_time_count={}",
        read_counter(&crate::task::perf::FRAME_ALLOC_TIME_COUNT)
    );
    write_str(offset, len, buf, &s)
}

fn stats_anon_unmap_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(768);
    macro_rules! counter {
        ($name:literal, $counter:ident) => {
            let _ = writeln!(
                s,
                concat!($name, "={}"),
                read_counter(&crate::task::perf::$counter)
            );
        };
    }
    counter!("anon_unmap_calls_total", ANON_UNMAP_CALLS_TOTAL);
    counter!("anon_unmap_range_calls", ANON_UNMAP_RANGE_CALLS);
    counter!("anon_unmap_area_calls", ANON_UNMAP_AREA_CALLS);
    counter!(
        "anon_unmap_requested_pages_total",
        ANON_UNMAP_REQUESTED_PAGES_TOTAL
    );
    counter!(
        "anon_unmap_resident_pages_total",
        ANON_UNMAP_RESIDENT_PAGES_TOTAL
    );
    counter!(
        "anon_unmap_active_before_total",
        ANON_UNMAP_ACTIVE_BEFORE_TOTAL
    );
    counter!("anon_unmap_active_before_max", ANON_UNMAP_ACTIVE_BEFORE_MAX);
    counter!(
        "anon_unmap_retain_scan_steps_total",
        ANON_UNMAP_RETAIN_SCAN_STEPS_TOTAL
    );
    counter!("anon_unmap_ticks_total", ANON_UNMAP_TICKS_TOTAL);
    counter!("anon_unmap_ticks_max", ANON_UNMAP_TICKS_MAX);
    counter!("anon_unmap_errors_total", ANON_UNMAP_ERRORS_TOTAL);
    counter!("anon_unmap_pages_le_16", ANON_UNMAP_PAGES_LE_16);
    counter!("anon_unmap_pages_le_256", ANON_UNMAP_PAGES_LE_256);
    counter!("anon_unmap_pages_le_4096", ANON_UNMAP_PAGES_LE_4096);
    counter!("anon_unmap_pages_gt_4096", ANON_UNMAP_PAGES_GT_4096);
    write_str(offset, len, buf, &s)
}

fn stats_resource_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
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
    let (pc_reg_len, _pc_reg_cap, pc_reg_alive, pc_reg_stale) =
        crate::fs::page_cache::registry_stats();
    let (pc_ent_len, _pc_ent_cap, pc_ent_live, pc_ent_holes) =
        crate::fs::page_cache::entries_global_stats();
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
    // lwext4 metadata probes (legacy backend only; embedded here so old
    // initproc can see them).
    #[cfg(feature = "ext4_lwext4_backend")]
    {
        let lw = crate::fs::ext4_lwext4::counters::snapshot();
        let _ = writeln!(s, "lwext4_find={}", lw.0);
        let _ = writeln!(s, "lwext4_find_cycles={}", lw.1);
        let _ = writeln!(s, "lwext4_meta_cold={}", lw.7);
        let _ = writeln!(s, "lwext4_meta_hot={}", lw.8);
        let _ = writeln!(s, "lwext4_file_open={}", lw.10);
        let _ = writeln!(s, "lwext4_file_close={}", lw.13);
        let _ = writeln!(s, "lwext4_dir_entries={}", lw.15);
        let _ = writeln!(s, "lwext4_create_pre={}", lw.17);
        let _ = writeln!(s, "lwext4_ensure_pc={}", lw.20);
        let _ = writeln!(s, "lwext4_find_cache_hit={}", lw.21);
        let _ = writeln!(s, "lwext4_find_cache_miss={}", lw.22);
        let _ = writeln!(s, "lwext4_ensure_pc_creates={}", lw.23);
    }
    // mount/bind probes
    let mnt = crate::fs::vfs::mount::counters::mount_perf_snapshot();
    let _ = writeln!(s, "mnt_propagate={}", mnt.0);
    let _ = writeln!(s, "mnt_remove_fs_scan={}", mnt.3);
    let _ = writeln!(s, "mnt_rbind_calls={}", mnt.4);
    let _ = writeln!(s, "mnt_rbind_cycles={}", mnt.5);
    let _ = writeln!(s, "mnt_rbind_entries={}", mnt.6);
    let _ = writeln!(s, "mnt_rbind_dirent={}", mnt.7);
    let _ = writeln!(s, "mnt_rbind_seen_scan={}", mnt.8);
    write_str(offset, len, buf, &s)
}

fn stats_buddyinfo_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
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

fn stats_zombies_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(256);
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
    let total_zombies: usize = groups.iter().map(|(_, c)| c).sum();
    let _ = writeln!(s, "total_zombies={}", total_zombies);
    for (parent_pid, zombie_count) in groups.into_iter().take(10) {
        let _ = writeln!(
            s,
            "parent_pid={} zombie_children={}",
            parent_pid, zombie_count
        );
    }
    write_str(offset, len, buf, &s)
}

fn tracing_trigger_write(_extra: usize, _offset: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
    if buf.is_empty() {
        return Err(SyscallErr::EINVAL);
    }
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
    let _ = writeln!(
        s,
        "stats_profile={}",
        crate::task::perf::STATS_PROFILE.load(Ordering::Relaxed)
    );
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
//  STATS: profile (rw) — select one bounded diagnostic counter group
// ═══════════════════════════════════════════════════════════════════════

fn stats_profile_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let profile = crate::task::perf::STATS_PROFILE.load(Ordering::Relaxed);
    let name = match profile {
        crate::task::perf::STATS_PROFILE_CORE => "core",
        crate::task::perf::STATS_PROFILE_MEMORY_IO => "memory_io",
        crate::task::perf::STATS_PROFILE_CORE_MEMORY_IO => "core_memory_io",
        crate::task::perf::STATS_PROFILE_NETWORK_RUNTIME => "network_runtime",
        crate::task::perf::STATS_PROFILE_ALL => "all",
        _ => "unknown",
    };
    let value = format!("{} {}\n", name, profile);
    write_str(offset, len, buf, &value)
}

fn stats_profile_write(_extra: usize, _offset: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
    let command = core::str::from_utf8(buf)
        .map_err(|_| SyscallErr::EINVAL)?
        .trim();
    let profile = match command {
        "core" | "1" => crate::task::perf::STATS_PROFILE_CORE,
        "memory_io" | "2" => crate::task::perf::STATS_PROFILE_MEMORY_IO,
        "core_memory_io" | "3" => crate::task::perf::STATS_PROFILE_CORE_MEMORY_IO,
        "network_runtime" | "4" => crate::task::perf::STATS_PROFILE_NETWORK_RUNTIME,
        "all" | "7" => crate::task::perf::STATS_PROFILE_ALL,
        _ => return Err(SyscallErr::EINVAL),
    };
    crate::task::perf::STATS_PROFILE.store(profile, Ordering::Relaxed);
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
    let val = format!("{}\n", crate::trace::TRACE_DROPPED.load(Ordering::Relaxed));
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
    let _ = writeln!(
        s,
        "pc_read_calls={}",
        read_counter(&crate::task::perf::PC_READ_CALLS)
    );
    let _ = writeln!(
        s,
        "pc_read_pages={}",
        read_counter(&crate::task::perf::PC_READ_PAGES)
    );
    let _ = writeln!(
        s,
        "pc_read_user_calls={}",
        read_counter(&crate::task::perf::PC_READ_USER_CALLS)
    );
    let _ = writeln!(
        s,
        "pc_read_user_pages={}",
        read_counter(&crate::task::perf::PC_READ_USER_PAGES)
    );
    let _ = writeln!(
        s,
        "pc_read_miss={}",
        read_counter(&crate::task::perf::PC_READ_MISS)
    );
    let _ = writeln!(
        s,
        "pc_read_cycles={}",
        read_counter(&crate::task::perf::PC_READ_CYCLES_TOTAL)
    );
    let _ = writeln!(
        s,
        "pc_read_hit_cycles={}",
        read_counter(&crate::task::perf::PC_READ_HIT_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_read_miss_cycles={}",
        read_counter(&crate::task::perf::PC_READ_MISS_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_read_lookup_cycles={}",
        read_counter(&crate::task::perf::PC_READ_LOOKUP_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_read_miss_fill_cycles={}",
        read_counter(&crate::task::perf::PC_READ_MISS_FILL_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_read_valid_fill_cycles={}",
        read_counter(&crate::task::perf::PC_READ_VALID_FILL_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_read_copy_cycles={}",
        read_counter(&crate::task::perf::PC_READ_COPY_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_copy_cycles={}",
        read_counter(&crate::task::perf::PC_COPY_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_lookup_cycles={}",
        read_counter(&crate::task::perf::PC_LOOKUP_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_write_calls={}",
        read_counter(&crate::task::perf::PC_WRITE_CALLS)
    );
    let _ = writeln!(
        s,
        "pc_write_pages={}",
        read_counter(&crate::task::perf::PC_WRITE_PAGES)
    );
    let _ = writeln!(
        s,
        "pc_write_overwrite={}",
        read_counter(&crate::task::perf::PC_WRITE_OVERWRITE)
    );
    let _ = writeln!(
        s,
        "pc_write_eventually_full={}",
        read_counter(&crate::task::perf::PC_WRITE_EVENTUALLY_FULL)
    );
    let _ = writeln!(
        s,
        "pc_write_cycles={}",
        read_counter(&crate::task::perf::PC_WRITE_CYCLES_TOTAL)
    );
    let _ = writeln!(
        s,
        "pc_write_lookup_cycles={}",
        read_counter(&crate::task::perf::PC_WRITE_LOOKUP_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_write_copy_cycles={}",
        read_counter(&crate::task::perf::PC_WRITE_COPY_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_write_commit_cycles={}",
        read_counter(&crate::task::perf::PC_WRITE_COMMIT_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_wb_calls={}",
        read_counter(&crate::task::perf::PC_WRITEBACK_CALLS)
    );
    let _ = writeln!(
        s,
        "pc_wb_pages={}",
        read_counter(&crate::task::perf::PC_WRITEBACK_PAGES)
    );
    let _ = writeln!(
        s,
        "pc_wb_cycles={}",
        read_counter(&crate::task::perf::PC_WRITEBACK_CYCLES_TOTAL)
    );
    let _ = writeln!(
        s,
        "pc_falloc_cycles={}",
        read_counter(&crate::task::perf::PC_FALLOC_CYCLES_TOTAL)
    );
    let _ = writeln!(
        s,
        "wb_bg_calls={}",
        read_counter(&crate::task::perf::WB_BG_CALLS)
    );
    let _ = writeln!(
        s,
        "wb_throttle_calls={}",
        read_counter(&crate::task::perf::WB_THROTTLE_CALLS)
    );
    let _ = writeln!(
        s,
        "wb_redirty_pages={}",
        read_counter(&crate::task::perf::WB_REDIRTY_PAGES)
    );
    let _ = writeln!(
        s,
        "pc_lock_hold_cycles={}",
        read_counter(&crate::task::perf::PC_LOCK_HOLD_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_lock_hold_max={}",
        read_counter(&crate::task::perf::PC_LOCK_HOLD_MAX)
    );
    let _ = writeln!(
        s,
        "pc_lock_io_miss_reads={}",
        read_counter(&crate::task::perf::PC_LOCK_IO_MISS_READS)
    );
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
    let mut s = String::with_capacity(1800);
    let _ = writeln!(
        s,
        "blk_vread_reqs={}",
        read_counter(&crate::task::perf::BLK_VREAD_REQS)
    );
    let _ = writeln!(
        s,
        "blk_vread_secs={}",
        read_counter(&crate::task::perf::BLK_VREAD_SECS)
    );
    let _ = writeln!(
        s,
        "blk_vwrite_reqs={}",
        read_counter(&crate::task::perf::BLK_VWRITE_REQS)
    );
    let _ = writeln!(
        s,
        "blk_vwrite_secs={}",
        read_counter(&crate::task::perf::BLK_VWRITE_SECS)
    );
    let _ = writeln!(
        s,
        "journal_commit_count={}",
        read_counter(&crate::task::perf::JOURNAL_COMMIT_COUNT)
    );
    let _ = writeln!(
        s,
        "journal_commit_bytes={}",
        read_counter(&crate::task::perf::JOURNAL_COMMIT_BYTES)
    );
    let _ = writeln!(
        s,
        "device_flush_count={}",
        read_counter(&crate::task::perf::DEVICE_FLUSH_COUNT)
    );
    let _ = writeln!(
        s,
        "virtio_write_requests={}",
        read_counter(&crate::task::perf::VIRTIO_WRITE_REQUESTS)
    );
    let _ = writeln!(
        s,
        "virtio_write_bytes={}",
        read_counter(&crate::task::perf::VIRTIO_WRITE_BYTES)
    );
    let _ = writeln!(
        s,
        "virtio_read_requests={}",
        read_counter(&crate::task::perf::VIRTIO_READ_REQUESTS)
    );
    let _ = writeln!(
        s,
        "writeback_batch_count={}",
        read_counter(&crate::task::perf::WRITEBACK_BATCH_COUNT)
    );
    let _ = writeln!(
        s,
        "writeback_page_count={}",
        read_counter(&crate::task::perf::WRITEBACK_PAGE_COUNT)
    );
    let _ = writeln!(
        s,
        "wb_tx_data_write_calls={}",
        read_counter(&crate::task::perf::WB_TX_DATA_WRITE_CALLS)
    );
    let _ = writeln!(
        s,
        "wb_tx_data_write_bytes={}",
        read_counter(&crate::task::perf::WB_TX_DATA_WRITE_BYTES)
    );
    let _ = writeln!(
        s,
        "wb_tx_data_write_ticks={}",
        read_counter(&crate::task::perf::WB_TX_DATA_WRITE_TICKS)
    );
    let _ = writeln!(
        s,
        "wb_tx_alloc_extent_calls={}",
        read_counter(&crate::task::perf::WB_TX_ALLOC_EXTENT_CALLS)
    );
    let _ = writeln!(
        s,
        "wb_tx_alloc_extent_pages={}",
        read_counter(&crate::task::perf::WB_TX_ALLOC_EXTENT_PAGES)
    );
    let _ = writeln!(
        s,
        "wb_tx_alloc_extent_ticks={}",
        read_counter(&crate::task::perf::WB_TX_ALLOC_EXTENT_TICKS)
    );
    let _ = writeln!(
        s,
        "wb_tx_journal_commit_ticks={}",
        read_counter(&crate::task::perf::WB_TX_JOURNAL_COMMIT_TICKS)
    );
    let _ = writeln!(
        s,
        "wb_tx_journal_staged_blocks={}",
        read_counter(&crate::task::perf::WB_TX_JOURNAL_STAGED_BLOCKS)
    );
    let _ = writeln!(
        s,
        "wb_tx_journal_tx_first={}",
        read_counter(&crate::task::perf::WB_TX_JOURNAL_TX_FIRST)
    );
    let _ = writeln!(
        s,
        "wb_tx_journal_tx_last={}",
        read_counter(&crate::task::perf::WB_TX_JOURNAL_TX_LAST)
    );
    let _ = writeln!(
        s,
        "wb_tx_journal_flush_count={}",
        read_counter(&crate::task::perf::WB_TX_JOURNAL_FLUSH_COUNT)
    );
    let _ = writeln!(
        s,
        "wb_tx_journal_flush_ticks={}",
        read_counter(&crate::task::perf::WB_TX_JOURNAL_FLUSH_TICKS)
    );
    let _ = writeln!(
        s,
        "wb_tx_boundary_flush_count={}",
        read_counter(&crate::task::perf::WB_TX_BOUNDARY_FLUSH_COUNT)
    );
    let _ = writeln!(
        s,
        "wb_tx_boundary_flush_ticks={}",
        read_counter(&crate::task::perf::WB_TX_BOUNDARY_FLUSH_TICKS)
    );
    let _ = writeln!(
        s,
        "pwrite_uaccess_cycles={}",
        read_counter(&crate::task::perf::PWRITE_UACCESS_CYCLES)
    );
    let _ = writeln!(
        s,
        "pwrite_file_cycles={}",
        read_counter(&crate::task::perf::PWRITE_FILE_CYCLES)
    );
    let _ = writeln!(
        s,
        "pwrite_ext4_setup_cycles={}",
        read_counter(&crate::task::perf::PWRITE_EXT4_SETUP_CYCLES)
    );
    let _ = writeln!(
        s,
        "pwrite_ext4_post_cycles={}",
        read_counter(&crate::task::perf::PWRITE_EXT4_POST_CYCLES)
    );
    let _ = writeln!(
        s,
        "pwrite_total_count={}",
        read_counter(&crate::task::perf::PWRITE_TOTAL_COUNT)
    );
    let _ = writeln!(
        s,
        "pwrite_vfs_mode_cycles={}",
        read_counter(&crate::task::perf::PWRITE_VFS_MODE_CYCLES)
    );
    let _ = writeln!(
        s,
        "pwrite_vfs_seals_cycles={}",
        read_counter(&crate::task::perf::PWRITE_VFS_SEALS_CYCLES)
    );
    let _ = writeln!(
        s,
        "pwrite_vfs_touch_cycles={}",
        read_counter(&crate::task::perf::PWRITE_VFS_TOUCH_CYCLES)
    );
    let _ = writeln!(
        s,
        "pwrite_mount_writable_cycles={}",
        read_counter(&crate::task::perf::PWRITE_MOUNT_WRITABLE_CYCLES)
    );
    let _ = writeln!(
        s,
        "write_fd_prep_cycles={}",
        read_counter(&crate::task::perf::WRITE_FD_PREP_CYCLES)
    );
    let _ = writeln!(
        s,
        "write_uaccess_cycles={}",
        read_counter(&crate::task::perf::WRITE_UACCESS_CYCLES)
    );
    let _ = writeln!(
        s,
        "write_file_cycles={}",
        read_counter(&crate::task::perf::WRITE_FILE_CYCLES)
    );
    let _ = writeln!(
        s,
        "write_vfs_mode_cycles={}",
        read_counter(&crate::task::perf::WRITE_VFS_MODE_CYCLES)
    );
    let _ = writeln!(
        s,
        "write_vfs_seals_cycles={}",
        read_counter(&crate::task::perf::WRITE_VFS_SEALS_CYCLES)
    );
    let _ = writeln!(
        s,
        "write_offset_cycles={}",
        read_counter(&crate::task::perf::WRITE_OFFSET_CYCLES)
    );
    let _ = writeln!(
        s,
        "write_total_count={}",
        read_counter(&crate::task::perf::WRITE_TOTAL_COUNT)
    );
    let _ = writeln!(
        s,
        "pread_uaccess_cycles={}",
        read_counter(&crate::task::perf::PREAD_UACCESS_CYCLES)
    );
    let _ = writeln!(
        s,
        "pread_file_cycles={}",
        read_counter(&crate::task::perf::PREAD_FILE_CYCLES)
    );
    let _ = writeln!(
        s,
        "pread_ext4_logical_size_cycles={}",
        read_counter(&crate::task::perf::PREAD_EXT4_LOGICAL_SIZE_CYCLES)
    );
    let _ = writeln!(
        s,
        "pread_ext4_page_cache_cycles={}",
        read_counter(&crate::task::perf::PREAD_EXT4_PAGE_CACHE_CYCLES)
    );
    let _ = writeln!(
        s,
        "pread_total_count={}",
        read_counter(&crate::task::perf::PREAD_TOTAL_COUNT)
    );
    let _ = writeln!(
        s,
        "pread_vfs_mode_cycles={}",
        read_counter(&crate::task::perf::PREAD_VFS_MODE_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_write_lookup_cycles={}",
        read_counter(&crate::task::perf::PC_WRITE_LOOKUP_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_write_lease_cycles={}",
        read_counter(&crate::task::perf::PC_WRITE_LEASE_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_write_copy_cycles={}",
        read_counter(&crate::task::perf::PC_WRITE_COPY_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_write_commit_cycles={}",
        read_counter(&crate::task::perf::PC_WRITE_COMMIT_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_read_calls={}",
        read_counter(&crate::task::perf::PC_READ_CALLS)
    );
    let _ = writeln!(
        s,
        "pc_read_pages={}",
        read_counter(&crate::task::perf::PC_READ_PAGES)
    );
    let _ = writeln!(
        s,
        "pc_read_user_calls={}",
        read_counter(&crate::task::perf::PC_READ_USER_CALLS)
    );
    let _ = writeln!(
        s,
        "pc_read_user_pages={}",
        read_counter(&crate::task::perf::PC_READ_USER_PAGES)
    );
    let _ = writeln!(
        s,
        "pc_read_miss={}",
        read_counter(&crate::task::perf::PC_READ_MISS)
    );
    let _ = writeln!(
        s,
        "pc_read_hit_cycles={}",
        read_counter(&crate::task::perf::PC_READ_HIT_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_read_miss_cycles={}",
        read_counter(&crate::task::perf::PC_READ_MISS_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_read_lookup_cycles={}",
        read_counter(&crate::task::perf::PC_READ_LOOKUP_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_read_miss_fill_cycles={}",
        read_counter(&crate::task::perf::PC_READ_MISS_FILL_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_read_valid_fill_cycles={}",
        read_counter(&crate::task::perf::PC_READ_VALID_FILL_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_read_copy_cycles={}",
        read_counter(&crate::task::perf::PC_READ_COPY_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_lookup_cycles={}",
        read_counter(&crate::task::perf::PC_LOOKUP_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_copy_cycles={}",
        read_counter(&crate::task::perf::PC_COPY_CYCLES)
    );
    let _ = writeln!(
        s,
        "pc_read_cycles_total={}",
        read_counter(&crate::task::perf::PC_READ_CYCLES_TOTAL)
    );
    let _ = writeln!(
        s,
        "virtio_dma_pool_enabled={}",
        crate::drivers::block::virtio_dma_pool::dma_pool_is_enabled()
    );
    let _ = writeln!(s, "virtio_dma_bridge_schema_version=2");
    let _ = writeln!(s, "virtio_dma_bridge_per_hart=1");
    let _ = writeln!(
        s,
        "virtio_dma_small_pool_enabled={}",
        crate::drivers::block::virtio_dma_pool::dma_small_pool_is_enabled()
    );
    let _ = writeln!(
        s,
        "virtio_blk_read_chunks={}",
        read_counter(&crate::task::perf::VIRTIO_BLK_READ_CHUNKS)
    );
    let _ = writeln!(
        s,
        "virtio_blk_read_bytes={}",
        read_counter(&crate::task::perf::VIRTIO_BLK_READ_BYTES)
    );
    let _ = writeln!(
        s,
        "virtio_blk_write_chunks={}",
        read_counter(&crate::task::perf::VIRTIO_BLK_WRITE_CHUNKS)
    );
    let _ = writeln!(
        s,
        "virtio_blk_write_bytes={}",
        read_counter(&crate::task::perf::VIRTIO_BLK_WRITE_BYTES)
    );
    let _ = writeln!(
        s,
        "virtio_dma_pool_reserve_success={}",
        read_counter(&crate::task::perf::VIRTIO_DMA_POOL_RESERVE_SUCCESS)
    );
    let _ = writeln!(
        s,
        "virtio_dma_pool_reserve_fail={}",
        read_counter(&crate::task::perf::VIRTIO_DMA_POOL_RESERVE_FAIL)
    );
    let _ = writeln!(
        s,
        "virtio_dma_pool_consume={}",
        read_counter(&crate::task::perf::VIRTIO_DMA_POOL_CONSUME)
    );
    let _ = writeln!(
        s,
        "virtio_dma_pool_cancel={}",
        read_counter(&crate::task::perf::VIRTIO_DMA_POOL_CANCEL)
    );
    let _ = writeln!(
        s,
        "virtio_dma_pool_finish={}",
        read_counter(&crate::task::perf::VIRTIO_DMA_POOL_FINISH)
    );
    let _ = writeln!(
        s,
        "virtio_dma_share_calls={}",
        read_counter(&crate::task::perf::VIRTIO_DMA_SHARE_CALLS)
    );
    let _ = writeln!(
        s,
        "virtio_dma_share_data_pool={}",
        read_counter(&crate::task::perf::VIRTIO_DMA_SHARE_DATA_POOL)
    );
    let _ = writeln!(
        s,
        "virtio_dma_share_data_fallback={}",
        read_counter(&crate::task::perf::VIRTIO_DMA_SHARE_DATA_FALLBACK)
    );
    let _ = writeln!(
        s,
        "virtio_dma_share_header_fallback={}",
        read_counter(&crate::task::perf::VIRTIO_DMA_SHARE_HEADER_FALLBACK)
    );
    let _ = writeln!(
        s,
        "virtio_dma_share_status_fallback={}",
        read_counter(&crate::task::perf::VIRTIO_DMA_SHARE_STATUS_FALLBACK)
    );
    let _ = writeln!(
        s,
        "virtio_dma_share_indirect_fallback={}",
        read_counter(&crate::task::perf::VIRTIO_DMA_SHARE_INDIRECT_FALLBACK)
    );
    let _ = writeln!(
        s,
        "virtio_dma_share_other_fallback={}",
        read_counter(&crate::task::perf::VIRTIO_DMA_SHARE_OTHER_FALLBACK)
    );
    let _ = writeln!(
        s,
        "virtio_dma_share_header_pool={}",
        read_counter(&crate::task::perf::VIRTIO_DMA_SHARE_HEADER_POOL)
    );
    let _ = writeln!(
        s,
        "virtio_dma_share_status_pool={}",
        read_counter(&crate::task::perf::VIRTIO_DMA_SHARE_STATUS_POOL)
    );
    let _ = writeln!(
        s,
        "virtio_dma_share_indirect_pool={}",
        read_counter(&crate::task::perf::VIRTIO_DMA_SHARE_INDIRECT_POOL)
    );
    let _ = writeln!(
        s,
        "virtio_dma_bridge_lock_wait_ticks_total={}",
        read_counter(&crate::task::perf::VIRTIO_DMA_BRIDGE_LOCK_WAIT_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "virtio_dma_bridge_lock_hold_ticks_total={}",
        read_counter(&crate::task::perf::VIRTIO_DMA_BRIDGE_LOCK_HOLD_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "virtio_dma_bridge_lock_hold_ticks_max={}",
        read_counter(&crate::task::perf::VIRTIO_DMA_BRIDGE_LOCK_HOLD_TICKS_MAX)
    );
    let _ = writeln!(s, "fs_io_counter_schema_version=1");
    for transport in 0..2 {
        let name = if transport == 0 { "mmio" } else { "pci" };
        let _ = writeln!(s, "virtio_{name}_device_lock_calls={}", read_counter(&crate::task::perf::VIRTIO_DEVICE_LOCK_CALLS[transport][0]));
        let _ = writeln!(s, "virtio_{name}_device_lock_wait_ticks={}", read_counter(&crate::task::perf::VIRTIO_DEVICE_LOCK_WAIT_TICKS[transport][0]));
        let _ = writeln!(s, "virtio_{name}_device_lock_hold_ticks={}", read_counter(&crate::task::perf::VIRTIO_DEVICE_LOCK_HOLD_TICKS[transport][0]));
        for bucket in 0..5 {
            let _ = writeln!(s, "virtio_{name}_read_bucket_{bucket}={}", read_counter(&crate::task::perf::VIRTIO_REQUEST_SIZE_READ[transport][bucket]));
            let _ = writeln!(s, "virtio_{name}_write_bucket_{bucket}={}", read_counter(&crate::task::perf::VIRTIO_REQUEST_SIZE_WRITE[transport][bucket]));
        }
    }
    for (name, counter) in [
        ("deferred_threshold", &crate::task::perf::JOURNAL_COMMIT_REASON_COUNTS[0]),
        ("direct_metadata_barrier", &crate::task::perf::JOURNAL_COMMIT_REASON_COUNTS[1]),
        ("durability_boundary", &crate::task::perf::JOURNAL_COMMIT_REASON_COUNTS[2]),
        ("shutdown", &crate::task::perf::JOURNAL_COMMIT_REASON_COUNTS[3]),
        ("explicit", &crate::task::perf::JOURNAL_COMMIT_REASON_COUNTS[4]),
    ] {
        let _ = writeln!(s, "journal_commit_reason_{name}={}", read_counter(counter));
    }
    for (name, counter) in [
        ("active_log", &crate::task::perf::JOURNAL_FLUSH_PHASE_COUNTS[0]),
        ("commit_record", &crate::task::perf::JOURNAL_FLUSH_PHASE_COUNTS[1]),
        ("checkpoint", &crate::task::perf::JOURNAL_FLUSH_PHASE_COUNTS[2]),
        ("tail_update", &crate::task::perf::JOURNAL_FLUSH_PHASE_COUNTS[3]),
    ] {
        let _ = writeln!(s, "journal_flush_phase_{name}={}", read_counter(counter));
    }
    for (name, counter) in [
        ("deferred_threshold", &crate::task::perf::DIRECT_FLUSH_REASON_COUNTS[0]),
        ("direct_metadata_barrier", &crate::task::perf::DIRECT_FLUSH_REASON_COUNTS[1]),
        ("durability_boundary", &crate::task::perf::DIRECT_FLUSH_REASON_COUNTS[2]),
        ("shutdown", &crate::task::perf::DIRECT_FLUSH_REASON_COUNTS[3]),
        ("explicit", &crate::task::perf::DIRECT_FLUSH_REASON_COUNTS[4]),
    ] {
        let _ = writeln!(s, "direct_flush_reason_{name}={}", read_counter(counter));
    }
    let _ = writeln!(s, "ext4_dir_snapshot_hits={}", read_counter(&crate::task::perf::EXT4_DIR_SNAPSHOT_HITS));
    let _ = writeln!(s, "ext4_dir_snapshot_misses={}", read_counter(&crate::task::perf::EXT4_DIR_SNAPSHOT_MISSES));
    let _ = writeln!(s, "ext4_dir_snapshot_invalidations={}", read_counter(&crate::task::perf::EXT4_DIR_SNAPSHOT_INVALIDATIONS));
    let _ = writeln!(s, "filemap_pte_around_speculative_reuses={}", read_counter(&crate::task::perf::EXT4_FILEMAP_PTE_AROUND_SPECULATIVE_REUSES));
    let _ = writeln!(
        s,
        "sata_read_reqs={}",
        read_counter(&crate::task::perf::SATA_READ_REQS)
    );
    let _ = writeln!(
        s,
        "sata_read_bytes={}",
        read_counter(&crate::task::perf::SATA_READ_BYTES)
    );
    let _ = writeln!(
        s,
        "sata_read_ticks_total={}",
        read_counter(&crate::task::perf::SATA_READ_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "sata_read_ticks_max={}",
        read_counter(&crate::task::perf::SATA_READ_TICKS_MAX)
    );
    let _ = writeln!(
        s,
        "sata_write_reqs={}",
        read_counter(&crate::task::perf::SATA_WRITE_REQS)
    );
    let _ = writeln!(
        s,
        "sata_write_bytes={}",
        read_counter(&crate::task::perf::SATA_WRITE_BYTES)
    );
    let _ = writeln!(
        s,
        "sata_write_ticks_total={}",
        read_counter(&crate::task::perf::SATA_WRITE_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "sata_write_ticks_max={}",
        read_counter(&crate::task::perf::SATA_WRITE_TICKS_MAX)
    );
    let _ = writeln!(
        s,
        "sata_flush_reqs={}",
        read_counter(&crate::task::perf::SATA_FLUSH_REQS)
    );
    let _ = writeln!(
        s,
        "sata_flush_ticks_total={}",
        read_counter(&crate::task::perf::SATA_FLUSH_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "pread_uaccess_cycles={}",
        read_counter(&crate::task::perf::PREAD_UACCESS_CYCLES)
    );
    let _ = writeln!(
        s,
        "pread_file_cycles={}",
        read_counter(&crate::task::perf::PREAD_FILE_CYCLES)
    );
    let _ = writeln!(
        s,
        "pread_ext4_logical_size_cycles={}",
        read_counter(&crate::task::perf::PREAD_EXT4_LOGICAL_SIZE_CYCLES)
    );
    let _ = writeln!(
        s,
        "pread_ext4_page_cache_cycles={}",
        read_counter(&crate::task::perf::PREAD_EXT4_PAGE_CACHE_CYCLES)
    );
    let _ = writeln!(
        s,
        "pread_total_count={}",
        read_counter(&crate::task::perf::PREAD_TOTAL_COUNT)
    );
    let _ = writeln!(
        s,
        "pread_vfs_mode_cycles={}",
        read_counter(&crate::task::perf::PREAD_VFS_MODE_CYCLES)
    );
    let _ = writeln!(
        s,
        "journal_commit_count={}",
        read_counter(&crate::task::perf::JOURNAL_COMMIT_COUNT)
    );
    let _ = writeln!(
        s,
        "journal_commit_bytes={}",
        read_counter(&crate::task::perf::JOURNAL_COMMIT_BYTES)
    );
    let _ = writeln!(
        s,
        "wb_data_write_bytes={}",
        read_counter(&crate::task::perf::WB_DATA_WRITE_BYTES)
    );
    let _ = writeln!(
        s,
        "wb_data_write_cycles={}",
        read_counter(&crate::task::perf::WB_DATA_WRITE_CYCLES)
    );
    let _ = writeln!(
        s,
        "wb_alloc_extent_pages={}",
        read_counter(&crate::task::perf::WB_ALLOC_EXTENT_PAGES)
    );
    let _ = writeln!(
        s,
        "wb_alloc_extent_cycles={}",
        read_counter(&crate::task::perf::WB_ALLOC_EXTENT_CYCLES)
    );
    let _ = writeln!(
        s,
        "wb_tx_journal_commit_ticks={}",
        read_counter(&crate::task::perf::WB_TX_JOURNAL_COMMIT_TICKS)
    );
    let _ = writeln!(
        s,
        "wb_tx_journal_staged_blocks={}",
        read_counter(&crate::task::perf::WB_TX_JOURNAL_STAGED_BLOCKS)
    );
    let _ = writeln!(
        s,
        "wb_tx_journal_tx_first={}",
        read_counter(&crate::task::perf::WB_TX_JOURNAL_TX_FIRST)
    );
    let _ = writeln!(
        s,
        "wb_tx_journal_tx_last={}",
        read_counter(&crate::task::perf::WB_TX_JOURNAL_TX_LAST)
    );
    let _ = writeln!(
        s,
        "wb_tx_journal_flush_count={}",
        read_counter(&crate::task::perf::WB_TX_JOURNAL_FLUSH_COUNT)
    );
    let _ = writeln!(
        s,
        "wb_tx_journal_flush_ticks={}",
        read_counter(&crate::task::perf::WB_TX_JOURNAL_FLUSH_TICKS)
    );
    let _ = writeln!(
        s,
        "wb_flush_boundary_count={}",
        read_counter(&crate::task::perf::WB_FLUSH_BOUNDARY_COUNT)
    );
    let _ = writeln!(
        s,
        "wb_flush_boundary_ticks={}",
        read_counter(&crate::task::perf::WB_FLUSH_BOUNDARY_TICKS)
    );
    write_str(offset, len, buf, &s)
}

// ═══════════════════════════════════════════════════════════════════════
//  STATS: Network and Python/runtime attribution
// ═══════════════════════════════════════════════════════════════════════

fn stats_net_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(768);
    macro_rules! counter {
        ($name:literal, $counter:ident) => {
            let _ = writeln!(
                s,
                concat!($name, "={}"),
                read_counter(&crate::task::perf::$counter)
            );
        };
    }
    counter!("net_poll_calls", NET_POLL_CALLS);
    counter!("net_poll_progress", NET_POLL_PROGRESS);
    counter!("net_poll_lock_busy", NET_POLL_LOCK_BUSY);
    counter!("net_rx_packets", NET_RX_PACKETS);
    counter!("net_rx_bytes", NET_RX_BYTES);
    counter!("net_rx_drops", NET_RX_DROPS);
    counter!("net_tx_submit_packets", NET_TX_SUBMIT_PACKETS);
    counter!("net_tx_submit_bytes", NET_TX_SUBMIT_BYTES);
    counter!("net_tx_drops", NET_TX_DROPS);
    counter!("runtime_exec_calls", RUNTIME_EXEC_CALLS);
    counter!("runtime_exec_ticks_total", RUNTIME_EXEC_TICKS_TOTAL);
    counter!("runtime_openat_calls", RUNTIME_OPENAT_CALLS);
    counter!("runtime_read_calls", RUNTIME_READ_CALLS);
    counter!("runtime_mmap_calls", RUNTIME_MMAP_CALLS);
    write_str(offset, len, buf, &s)
}

// ═══════════════════════════════════════════════════════════════════════
//  STATS: Ext4
// ═══════════════════════════════════════════════════════════════════════

fn stats_ext4_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(768);
    let _ = writeln!(
        s,
        "ext4_map_lblock_calls={}",
        read_counter(&crate::task::perf::EXT4_MAP_LBLOCK_CALLS)
    );
    let _ = writeln!(
        s,
        "ext4_map_lblock_cycles={}",
        read_counter(&crate::task::perf::EXT4_MAP_LBLOCK_CYCLES)
    );
    let _ = writeln!(
        s,
        "ext4_map_cache_hits={}",
        read_counter(&crate::task::perf::EXT4_MAP_CACHE_HITS)
    );
    let _ = writeln!(
        s,
        "ext4_map_holes={}",
        read_counter(&crate::task::perf::EXT4_MAP_HOLES)
    );
    let _ = writeln!(
        s,
        "ext4_find_extent_calls={}",
        read_counter(&crate::task::perf::EXT4_FIND_EXTENT_CALLS)
    );
    let _ = writeln!(
        s,
        "ext4_find_extent_cycles={}",
        read_counter(&crate::task::perf::EXT4_FIND_EXTENT_CYCLES)
    );
    let _ = writeln!(
        s,
        "ext4_find_extent_depth={}",
        read_counter(&crate::task::perf::EXT4_FIND_EXTENT_DEPTH_SUM)
    );
    let _ = writeln!(
        s,
        "ext4_find_extent_meta_reads={}",
        read_counter(&crate::task::perf::EXT4_FIND_EXTENT_META_READS)
    );
    let _ = writeln!(
        s,
        "ext4_pc_readpages_calls={}",
        read_counter(&crate::task::perf::EXT4_PC_READPAGES_CALLS)
    );
    let _ = writeln!(
        s,
        "ext4_pc_readpages_pages={}",
        read_counter(&crate::task::perf::EXT4_PC_READPAGES_PAGES)
    );
    let _ = writeln!(
        s,
        "ext4_pc_readpages_runs={}",
        read_counter(&crate::task::perf::EXT4_PC_READPAGES_RUNS)
    );
    let _ = writeln!(
        s,
        "ext4_pc_writepages_calls={}",
        read_counter(&crate::task::perf::EXT4_PC_WRITEPAGES_CALLS)
    );
    let _ = writeln!(
        s,
        "ext4_pc_writepages_pages={}",
        read_counter(&crate::task::perf::EXT4_PC_WRITEPAGES_PAGES)
    );
    let _ = writeln!(
        s,
        "ext4_pc_writepages_runs={}",
        read_counter(&crate::task::perf::EXT4_PC_WRITEPAGES_RUNS)
    );
    let _ = writeln!(
        s,
        "ext4_pc_512b_fallback={}",
        read_counter(&crate::task::perf::EXT4_PC_512B_FALLBACK_PAGES)
    );
    let _ = writeln!(
        s,
        "ext4_alloc_ensure_calls={}",
        read_counter(&crate::task::perf::EXT4_ALLOC_ENSURE_CALLS)
    );
    let _ = writeln!(
        s,
        "ext4_alloc_lblocks={}",
        read_counter(&crate::task::perf::EXT4_ALLOC_LBLOCKS)
    );
    let _ = writeln!(
        s,
        "ext4_alloc_new_blocks={}",
        read_counter(&crate::task::perf::EXT4_ALLOC_NEW_BLOCKS)
    );
    let _ = writeln!(
        s,
        "ext4_alloc_cycles={}",
        read_counter(&crate::task::perf::EXT4_ALLOC_CYCLES)
    );
    let _ = writeln!(
        s,
        "ext4_direct_write_at_calls={}",
        read_counter(&crate::task::perf::EXT4_DIRECT_WRITE_AT_CALLS)
    );
    let mut cache_fs_count = 0usize;
    let mut cache_hits = 0usize;
    let mut cache_misses = 0usize;
    let mut cache_full_clears = 0usize;
    let mut cache_evicted = 0usize;
    let mut cache_high_water = 0usize;
    let mut extent_hits = 0usize;
    let mut extent_misses = 0usize;
    let mut extent_overwrites = 0usize;
    let mut extent_invalidations = 0usize;
    for (_, snapshot) in crate::fs::ext4_another::prepare_stats_snapshots() {
        cache_fs_count += 1;
        cache_hits += snapshot.inode_cache_hits;
        cache_misses += snapshot.inode_cache_misses;
        cache_full_clears += snapshot.inode_cache_full_clears;
        cache_evicted += snapshot.inode_cache_evicted_entries;
        cache_high_water = cache_high_water.max(snapshot.inode_cache_high_water);
        extent_hits += snapshot.prepared_extent_hits;
        extent_misses += snapshot.prepared_extent_misses;
        extent_overwrites += snapshot.prepared_extent_overwrites;
        extent_invalidations += snapshot.prepared_extent_epoch_invalidations;
    }
    let _ = writeln!(s, "another_ext4_prepare_stats_schema_version=2");
    let _ = writeln!(s, "another_ext4_prepare_fs_count={cache_fs_count}");
    let _ = writeln!(s, "another_ext4_inode_cache_hits={cache_hits}");
    let _ = writeln!(s, "another_ext4_inode_cache_misses={cache_misses}");
    let _ = writeln!(s, "another_ext4_inode_cache_full_clears={cache_full_clears}");
    let _ = writeln!(s, "another_ext4_inode_cache_evicted_entries={cache_evicted}");
    let _ = writeln!(s, "another_ext4_inode_cache_high_water={cache_high_water}");
    let _ = writeln!(s, "another_ext4_prepared_extent_hits={extent_hits}");
    let _ = writeln!(s, "another_ext4_prepared_extent_misses={extent_misses}");
    let _ = writeln!(s, "another_ext4_prepared_extent_overwrites={extent_overwrites}");
    let _ = writeln!(s, "another_ext4_prepared_extent_epoch_invalidations={extent_invalidations}");
    write_str(offset, len, buf, &s)
}

// ═══════════════════════════════════════════════════════════════════════
//  STATS: lwext4 metadata probes
// ═══════════════════════════════════════════════════════════════════════

#[cfg(feature = "ext4_lwext4_backend")]
fn stats_lwext4_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let lw = crate::fs::ext4_lwext4::counters::snapshot();
    let mut s = String::with_capacity(512);
    let _ = writeln!(s, "lwext4_find_calls={}", lw.0);
    let _ = writeln!(s, "lwext4_find_cycles={}", lw.1);
    let _ = writeln!(s, "lwext4_probe_type_calls={}", lw.2);
    let _ = writeln!(s, "lwext4_probe_type_cycles={}", lw.3);
    let _ = writeln!(s, "lwext4_get_inode_id_calls={}", lw.4);
    let _ = writeln!(s, "lwext4_get_inode_id_enoint={}", lw.5);
    let _ = writeln!(s, "lwext4_get_inode_id_cycles={}", lw.6);
    let _ = writeln!(s, "lwext4_metadata_cold={}", lw.7);
    let _ = writeln!(s, "lwext4_metadata_hot={}", lw.8);
    let _ = writeln!(s, "lwext4_metadata_cold_cycles={}", lw.9);
    let _ = writeln!(s, "lwext4_file_open_calls={}", lw.10);
    let _ = writeln!(s, "lwext4_file_open_cycles={}", lw.11);
    let _ = writeln!(s, "lwext4_file_size_calls={}", lw.12);
    let _ = writeln!(s, "lwext4_file_close_calls={}", lw.13);
    let _ = writeln!(s, "lwext4_file_close_cycles={}", lw.14);
    let _ = writeln!(s, "lwext4_dir_entries_calls={}", lw.15);
    let _ = writeln!(s, "lwext4_dir_entries_cycles={}", lw.16);
    let _ = writeln!(s, "lwext4_create_pre_check={}", lw.17);
    let _ = writeln!(s, "lwext4_logical_size_calls={}", lw.18);
    let _ = writeln!(s, "lwext4_logical_size_cycles={}", lw.19);
    let _ = writeln!(s, "lwext4_ensure_pc_calls={}", lw.20);
    let _ = writeln!(s, "lwext4_find_cache_hit={}", lw.21);
    let _ = writeln!(s, "lwext4_find_cache_miss={}", lw.22);
    let _ = writeln!(s, "lwext4_ensure_pc_creates={}", lw.23);
    write_str(offset, len, buf, &s)
}

#[cfg(not(feature = "ext4_lwext4_backend"))]
fn stats_lwext4_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    write_str(offset, len, buf, "backend=unavailable\n")
}

// ═══════════════════════════════════════════════════════════════════════
//  STATS: Mount/bind probes
// ═══════════════════════════════════════════════════════════════════════

fn stats_mount_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mnt = crate::fs::vfs::mount::counters::mount_perf_snapshot();
    let lc = crate::fs::vfs::mount::counters::lifecycle_snapshot();
    let diag_on =
        crate::fs::vfs::mount::MOUNT_LIFECYCLE_DIAG.load(core::sync::atomic::Ordering::Relaxed);
    let mut s = String::with_capacity(512);
    let _ = writeln!(s, "mount_diag_on={}", if diag_on { 1 } else { 0 });
    let _ = writeln!(s, "mount_propagate_calls={}", mnt.0);
    let _ = writeln!(s, "mount_propagate_cycles={}", mnt.1);
    let _ = writeln!(s, "mount_remove_fs_calls={}", mnt.2);
    let _ = writeln!(s, "mount_remove_fs_scan={}", mnt.3);
    let _ = writeln!(s, "mount_rbind_calls={}", mnt.4);
    let _ = writeln!(s, "mount_rbind_cycles={}", mnt.5);
    let _ = writeln!(s, "mount_rbind_entries={}", mnt.6);
    let _ = writeln!(s, "mount_rbind_dirent_calls={}", mnt.7);
    let _ = writeln!(s, "mount_rbind_seen_scan={}", mnt.8);
    let _ = writeln!(s, "mount_lifecycle_create={}", lc.0);
    let _ = writeln!(s, "mount_lifecycle_umount={}", lc.1);
    let _ = writeln!(s, "mount_lifecycle_detach={}", lc.2);
    let _ = writeln!(s, "mount_lifecycle_drop={}", lc.3);
    // BackendLifecycle counters
    let _ = writeln!(
        s,
        "lc_new={}",
        crate::fs::vfs::mount::LC_NEW.load(core::sync::atomic::Ordering::Relaxed)
    );
    let _ = writeln!(
        s,
        "lc_acquire={}",
        crate::fs::vfs::mount::LC_ACQUIRE.load(core::sync::atomic::Ordering::Relaxed)
    );
    let _ = writeln!(
        s,
        "lc_release_dying={}",
        crate::fs::vfs::mount::LC_RELEASE_DYING.load(core::sync::atomic::Ordering::Relaxed)
    );
    let _ = writeln!(
        s,
        "lc_drain={}",
        crate::fs::vfs::mount::LC_DRAIN.load(core::sync::atomic::Ordering::Relaxed)
    );
    write_str(offset, len, buf, &s)
}

// ═══════════════════════════════════════════════════════════════════════
//  Mount lifecycle diag on/off (rw)
// ═══════════════════════════════════════════════════════════════════════

fn mount_diag_on_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let val = if crate::fs::vfs::mount::MOUNT_LIFECYCLE_DIAG.load(Ordering::Relaxed) {
        "1\n"
    } else {
        "0\n"
    };
    write_str(offset, len, buf, val)
}

fn mount_diag_on_write(_extra: usize, _offset: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
    let val = match buf.first() {
        Some(b'1') => true,
        Some(b'0') => false,
        _ => return Err(SyscallErr::EINVAL),
    };
    crate::fs::vfs::mount::MOUNT_LIFECYCLE_DIAG.store(val, Ordering::Relaxed);
    Ok(buf.len())
}

// ═══════════════════════════════════════════════════════════════════════
//  STATS: Pipe
// ═══════════════════════════════════════════════════════════════════════

fn stats_pipe_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(384);
    let _ = writeln!(s, "read_calls={}", crate::fs::dev::pipe::pipe_read_calls());
    let _ = writeln!(s, "read_bytes={}", crate::fs::dev::pipe::pipe_read_bytes());
    let _ = writeln!(
        s,
        "read_eagain={}",
        crate::fs::dev::pipe::pipe_read_eagain()
    );
    let _ = writeln!(
        s,
        "read_cycles_total={}",
        crate::fs::dev::pipe::pipe_read_cycles()
    );
    let _ = writeln!(
        s,
        "read_cycles_max={}",
        crate::fs::dev::pipe::pipe_read_cycles_max()
    );
    let _ = writeln!(
        s,
        "write_calls={}",
        crate::fs::dev::pipe::pipe_write_calls()
    );
    let _ = writeln!(
        s,
        "write_bytes={}",
        crate::fs::dev::pipe::pipe_write_bytes()
    );
    let _ = writeln!(
        s,
        "write_eagain={}",
        crate::fs::dev::pipe::pipe_write_eagain()
    );
    let _ = writeln!(
        s,
        "write_cycles_total={}",
        crate::fs::dev::pipe::pipe_write_cycles()
    );
    let _ = writeln!(
        s,
        "write_cycles_max={}",
        crate::fs::dev::pipe::pipe_write_cycles_max()
    );
    let _ = writeln!(s, "buf_alive={}", crate::fs::dev::pipe::pipe_buf_alive());
    let _ = writeln!(s, "buf_bytes={}", crate::fs::dev::pipe::pipe_buf_bytes());
    write_str(offset, len, buf, &s)
}

// ═══════════════════════════════════════════════════════════════════════
//  STATS: syscall_top (ro) — top-20 syscalls by total ticks
// ═══════════════════════════════════════════════════════════════════════

fn stats_syscall_top_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(2048);
    let mut entries: alloc::vec::Vec<(usize, usize, usize)> = alloc::vec::Vec::new();
    let syscount = crate::task::perf::PERF_SYSCOUNT;
    for id in 0..syscount {
        let count = crate::task::perf::syscall_count(id);
        let ticks = crate::task::perf::syscall_ticks(id);
        if count > 0 {
            entries.push((id, count, ticks));
        }
    }
    entries.sort_by(|a, b| b.2.cmp(&a.2));
    let freq = crate::hal::get_clock_freq();
    for &(id, count, ticks) in entries.iter().take(20) {
        let name = crate::syscall::syscall_name(id);
        let avg = if count > 0 { ticks / count } else { 0 };
        let _ = writeln!(
            s,
            "{}:{} count:{} ticks:{} avg:{}",
            id, name, count, ticks, avg
        );
    }
    write_str(offset, len, buf, &s)
}

// ═══════════════════════════════════════════════════════════════════════
//  STATS: pagefault (ro) — per-action page fault stats
// ═══════════════════════════════════════════════════════════════════════

fn stats_pagefault_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(1024);
    let _ = writeln!(
        s,
        "pagefault_counter_schema_version={}",
        crate::task::perf::PAGEFAULT_COUNTER_SCHEMA_VERSION
    );
    let _ = writeln!(
        s,
        "pagefault_attempts={}",
        crate::task::perf::pagefault_attempts()
    );
    let _ = writeln!(
        s,
        "pagefault_completed={}",
        crate::task::perf::pagefault_completed()
    );
    let _ = writeln!(
        s,
        "pagefault_retries={}",
        crate::task::perf::pagefault_retries()
    );
    let _ = writeln!(
        s,
        "pagefault_errors={}",
        crate::task::perf::pagefault_errors()
    );
    let _ = writeln!(
        s,
        "pagefault_access_load={}",
        crate::task::perf::pagefault_access_count(0)
    );
    let _ = writeln!(
        s,
        "pagefault_access_store={}",
        crate::task::perf::pagefault_access_count(1)
    );
    let _ = writeln!(
        s,
        "pagefault_access_execute={}",
        crate::task::perf::pagefault_access_count(2)
    );
    let names = &crate::task::perf::PF_ACTION_NAMES;
    for tag in 0..names.len() {
        let count = crate::task::perf::pf_action_count(tag);
        let ticks = crate::task::perf::pf_action_ticks(tag);
        let _ = writeln!(s, "action_{} count={} ticks={}", names[tag], count, ticks);
    }
    let stage_names = &crate::task::perf::PF_STAGE_NAMES;
    for stage in 0..stage_names.len() {
        let count = crate::task::perf::pf_stage_count(stage);
        let ticks = crate::task::perf::pf_stage_ticks(stage);
        let _ = writeln!(
            s,
            "stage_{} count={} ticks={}",
            stage_names[stage], count, ticks
        );
    }
    let _ = writeln!(
        s,
        "anon_fault_around_enabled={}",
        usize::from(crate::mm::anon_fault_around_enabled())
    );
    let _ = writeln!(
        s,
        "anon_fault_around_attempts={}",
        read_counter(&crate::task::perf::ANON_FAULT_AROUND_ATTEMPTS)
    );
    let _ = writeln!(
        s,
        "anon_fault_around_triggered={}",
        read_counter(&crate::task::perf::ANON_FAULT_AROUND_TRIGGERED)
    );
    let _ = writeln!(
        s,
        "anon_fault_around_pages={}",
        read_counter(&crate::task::perf::ANON_FAULT_AROUND_PAGES)
    );
    let _ = writeln!(
        s,
        "anon_fault_around_stop_boundary={}",
        read_counter(&crate::task::perf::ANON_FAULT_AROUND_STOP_BOUNDARY)
    );
    let _ = writeln!(
        s,
        "anon_fault_around_stop_state={}",
        read_counter(&crate::task::perf::ANON_FAULT_AROUND_STOP_STATE)
    );
    let _ = writeln!(
        s,
        "anon_fault_around_stop_no_prezero={}",
        read_counter(&crate::task::perf::ANON_FAULT_AROUND_STOP_NO_PREZERO)
    );
    let _ = writeln!(
        s,
        "anon_fault_around_stop_error={}",
        read_counter(&crate::task::perf::ANON_FAULT_AROUND_STOP_ERROR)
    );
    write_str(offset, len, buf, &s)
}

// ═══════════════════════════════════════════════════════════════════════
//  STATS: vm (ro) — filemap + TLB cycle stats
// ═══════════════════════════════════════════════════════════════════════

fn stats_vm_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(512);
    let _ = writeln!(
        s,
        "filemap_counter_schema_version={}",
        crate::task::perf::FILEMAP_COUNTER_SCHEMA_VERSION
    );
    let _ = writeln!(
        s,
        "filemap_fault_frames={}",
        read_counter(&crate::task::perf::FILEMAP_FAULT_FRAMES)
    );
    let _ = writeln!(
        s,
        "filemap_fault_ticks={}",
        read_counter(&crate::task::perf::FILEMAP_FAULT_TICKS)
    );
    let _ = writeln!(
        s,
        "filemap_private_copy_ticks={}",
        read_counter(&crate::task::perf::FILEMAP_PRIVATE_COPY_TICKS)
    );
    let _ = writeln!(
        s,
        "filemap_map_user_ticks={}",
        read_counter(&crate::task::perf::FILEMAP_MAP_USER_TICKS)
    );
    let _ = writeln!(
        s,
        "filemap_read_fault_calls={}",
        read_counter(&crate::task::perf::FILEMAP_READ_FAULT_CALLS)
    );
    let _ = writeln!(
        s,
        "filemap_private_fault_calls={}",
        read_counter(&crate::task::perf::FILEMAP_PRIVATE_FAULT_CALLS)
    );
    let _ = writeln!(
        s,
        "filemap_shared_write_fault_calls={}",
        read_counter(&crate::task::perf::FILEMAP_SHARED_WRITE_FAULT_CALLS)
    );
    let _ = writeln!(
        s,
        "filemap_ready_hit={}",
        read_counter(&crate::task::perf::FILEMAP_READY_HIT)
    );
    let _ = writeln!(
        s,
        "filemap_not_ready_retry={}",
        read_counter(&crate::task::perf::FILEMAP_NOT_READY_RETRY)
    );
    let _ = writeln!(
        s,
        "filemap_backend_read_calls={}",
        read_counter(&crate::task::perf::FILEMAP_BACKEND_READ_CALLS)
    );
    let _ = writeln!(
        s,
        "filemap_backend_read_ticks_total={}",
        read_counter(&crate::task::perf::FILEMAP_BACKEND_READ_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "filemap_backend_read_ticks_max={}",
        read_counter(&crate::task::perf::FILEMAP_BACKEND_READ_TICKS_MAX)
    );
    let _ = writeln!(
        s,
        "filemap_backend_read_under_vm_calls={}",
        read_counter(&crate::task::perf::FILEMAP_BACKEND_READ_UNDER_VM_CALLS)
    );
    let _ = writeln!(
        s,
        "filemap_backend_read_under_vm_ticks_total={}",
        read_counter(&crate::task::perf::FILEMAP_BACKEND_READ_UNDER_VM_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "filemap_backend_read_under_vm_ticks_max={}",
        read_counter(&crate::task::perf::FILEMAP_BACKEND_READ_UNDER_VM_TICKS_MAX)
    );
    let _ = writeln!(
        s,
        "filemap_retry_wait_calls={}",
        read_counter(&crate::task::perf::FILEMAP_RETRY_WAIT_CALLS)
    );
    let _ = writeln!(
        s,
        "filemap_retry_wait_ticks_total={}",
        read_counter(&crate::task::perf::FILEMAP_RETRY_WAIT_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "filemap_retry_wait_ticks_max={}",
        read_counter(&crate::task::perf::FILEMAP_RETRY_WAIT_TICKS_MAX)
    );
    let _ = writeln!(
        s,
        "filemap_revalidate_retry={}",
        read_counter(&crate::task::perf::FILEMAP_REVALIDATE_RETRY)
    );
    let _ = writeln!(
        s,
        "filemap_revalidate_vma_changed={}",
        read_counter(&crate::task::perf::FILEMAP_REVALIDATE_VMA_CHANGED)
    );
    let _ = writeln!(
        s,
        "filemap_revalidate_eof_changed={}",
        read_counter(&crate::task::perf::FILEMAP_REVALIDATE_EOF_CHANGED)
    );
    let _ = writeln!(
        s,
        "filemap_fault_around_calls={}",
        read_counter(&crate::task::perf::FILEMAP_FAULT_AROUND_CALLS)
    );
    let _ = writeln!(
        s,
        "filemap_fault_around_pages_requested={}",
        read_counter(&crate::task::perf::FILEMAP_FAULT_AROUND_PAGES_REQUESTED)
    );
    let _ = writeln!(
        s,
        "filemap_fault_around_pages_missing={}",
        read_counter(&crate::task::perf::FILEMAP_FAULT_AROUND_PAGES_MISSING)
    );
    let _ = writeln!(
        s,
        "filemap_fault_around_claim_conflicts={}",
        read_counter(&crate::task::perf::FILEMAP_FAULT_AROUND_CLAIM_CONFLICTS)
    );
    let _ = writeln!(
        s,
        "filemap_fault_around_pages_published={}",
        read_counter(&crate::task::perf::FILEMAP_FAULT_AROUND_PAGES_PUBLISHED)
    );
    let _ = writeln!(
        s,
        "filemap_fault_around_pages_prefetched={}",
        read_counter(&crate::task::perf::FILEMAP_FAULT_AROUND_PAGES_PREFETCHED)
    );
    let _ = writeln!(
        s,
        "filemap_fault_around_backend_runs={}",
        read_counter(&crate::task::perf::FILEMAP_FAULT_AROUND_BACKEND_RUNS)
    );
    let _ = writeln!(
        s,
        "filemap_fault_around_useful_hits={}",
        read_counter(&crate::task::perf::FILEMAP_FAULT_AROUND_USEFUL_HITS)
    );
    let _ = writeln!(
        s,
        "filemap_fault_around_unused_discards={}",
        read_counter(&crate::task::perf::FILEMAP_FAULT_AROUND_UNUSED_DISCARDS)
    );
    let _ = writeln!(
        s,
        "filemap_fault_around_aborts={}",
        read_counter(&crate::task::perf::FILEMAP_FAULT_AROUND_ABORTS)
    );
    let _ = writeln!(
        s,
        "filemap_pte_around_calls={}",
        read_counter(&crate::task::perf::FILEMAP_PTE_AROUND_CALLS)
    );
    let _ = writeln!(
        s,
        "filemap_pte_around_pages_examined={}",
        read_counter(&crate::task::perf::FILEMAP_PTE_AROUND_PAGES_EXAMINED)
    );
    let _ = writeln!(
        s,
        "filemap_pte_around_pages_mapped={}",
        read_counter(&crate::task::perf::FILEMAP_PTE_AROUND_PAGES_MAPPED)
    );
    let _ = writeln!(
        s,
        "filemap_pte_around_not_ready_stops={}",
        read_counter(&crate::task::perf::FILEMAP_PTE_AROUND_NOT_READY_STOPS)
    );
    let _ = writeln!(
        s,
        "filemap_pte_around_state_conflicts={}",
        read_counter(&crate::task::perf::FILEMAP_PTE_AROUND_STATE_CONFLICTS)
    );
    let _ = writeln!(
        s,
        "filemap_pte_around_cache_errors={}",
        read_counter(&crate::task::perf::FILEMAP_PTE_AROUND_CACHE_ERRORS)
    );
    let _ = writeln!(
        s,
        "tlb_page_flush_cycles={}",
        read_counter(&crate::task::perf::TLB_PAGE_FLUSH_CYCLES)
    );
    let _ = writeln!(
        s,
        "tlb_full_flush_cycles={}",
        read_counter(&crate::task::perf::TLB_FULL_FLUSH_CYCLES)
    );
    let _ = writeln!(
        s,
        "tlb_activate_cycles={}",
        read_counter(&crate::task::perf::TLB_ACTIVATE_CYCLES)
    );
    let _ = writeln!(
        s,
        "execve_map_elf_ticks={}",
        read_counter(&crate::task::perf::EXECVE_MAP_ELF_TICKS)
    );
    let _ = writeln!(
        s,
        "execve_kernel_map_ticks={}",
        read_counter(&crate::task::perf::EXECVE_KERNEL_MAP_TICKS)
    );
    let _ = writeln!(
        s,
        "execve_interp_ticks={}",
        read_counter(&crate::task::perf::EXECVE_INTERP_TICKS)
    );
    let _ = writeln!(
        s,
        "execve_stack_tables_ticks={}",
        read_counter(&crate::task::perf::EXECVE_STACK_TABLES_TICKS)
    );
    let _ = writeln!(
        s,
        "execve_teardown_ticks={}",
        read_counter(&crate::task::perf::EXECVE_TEARDOWN_TICKS)
    );
    let _ = writeln!(
        s,
        "exec_direct_count={}",
        read_counter(&crate::task::perf::EXEC_DIRECT_COUNT)
    );
    let _ = writeln!(
        s,
        "exec_fallback_count={}",
        read_counter(&crate::task::perf::EXEC_FALLBACK_COUNT)
    );
    let _ = writeln!(
        s,
        "exec_direct_enosys_count={}",
        read_counter(&crate::task::perf::EXEC_DIRECT_ENOSYS_COUNT)
    );
    let _ = writeln!(
        s,
        "vm_read_lock_calls={}",
        read_counter(&crate::task::perf::VM_READ_LOCK_CALLS)
    );
    let _ = writeln!(
        s,
        "vm_read_lock_wait_ticks_total={}",
        read_counter(&crate::task::perf::VM_READ_LOCK_WAIT_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "vm_read_lock_hold_ticks_total={}",
        read_counter(&crate::task::perf::VM_READ_LOCK_HOLD_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "vm_write_lock_calls={}",
        read_counter(&crate::task::perf::VM_WRITE_LOCK_CALLS)
    );
    let _ = writeln!(
        s,
        "vm_write_lock_wait_ticks_total={}",
        read_counter(&crate::task::perf::VM_WRITE_LOCK_WAIT_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "vm_write_lock_hold_ticks_total={}",
        read_counter(&crate::task::perf::VM_WRITE_LOCK_HOLD_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "vm_flush_outside_lock_ticks_total={}",
        read_counter(&crate::task::perf::VM_FLUSH_OUTSIDE_LOCK_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "vm_read_lock_wait_ticks_max={}",
        read_counter(&crate::task::perf::VM_READ_LOCK_WAIT_TICKS_MAX)
    );
    let _ = writeln!(
        s,
        "vm_read_lock_hold_ticks_max={}",
        read_counter(&crate::task::perf::VM_READ_LOCK_HOLD_TICKS_MAX)
    );
    let _ = writeln!(
        s,
        "vm_write_lock_wait_ticks_max={}",
        read_counter(&crate::task::perf::VM_WRITE_LOCK_WAIT_TICKS_MAX)
    );
    let _ = writeln!(
        s,
        "vm_write_lock_hold_ticks_max={}",
        read_counter(&crate::task::perf::VM_WRITE_LOCK_HOLD_TICKS_MAX)
    );
    let _ = writeln!(
        s,
        "vm_flush_outside_lock_ticks_max={}",
        read_counter(&crate::task::perf::VM_FLUSH_OUTSIDE_LOCK_TICKS_MAX)
    );
    let _ = writeln!(
        s,
        "task_switch_same_mm={}",
        read_counter(&crate::task::perf::TASK_SWITCH_SAME_MM)
    );
    let _ = writeln!(
        s,
        "task_switch_different_mm={}",
        read_counter(&crate::task::perf::TASK_SWITCH_DIFFERENT_MM)
    );
    let _ = writeln!(
        s,
        "task_switch_to_kernel_only={}",
        read_counter(&crate::task::perf::TASK_SWITCH_TO_KERNEL_ONLY)
    );
    let _ = writeln!(
        s,
        "task_switch_idle_no_next={}",
        read_counter(&crate::task::perf::TASK_SWITCH_IDLE_NO_NEXT)
    );
    let _ = writeln!(
        s,
        "mm_activate_calls={}",
        read_counter(&crate::task::perf::MM_ACTIVATE_CALLS)
    );
    let _ = writeln!(
        s,
        "mm_activate_ticks_total={}",
        read_counter(&crate::task::perf::MM_ACTIVATE_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "mm_deactivate_calls={}",
        read_counter(&crate::task::perf::MM_DEACTIVATE_CALLS)
    );
    let _ = writeln!(
        s,
        "mm_deactivate_ticks_total={}",
        read_counter(&crate::task::perf::MM_DEACTIVATE_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "mm_same_already_active={}",
        read_counter(&crate::task::perf::MM_SAME_ALREADY_ACTIVE)
    );
    let _ = writeln!(
        s,
        "mm_generation_catchup={}",
        read_counter(&crate::task::perf::MM_GENERATION_CATCHUP)
    );
    let _ = writeln!(
        s,
        "mm_asid_rollover={}",
        read_counter(&crate::task::perf::MM_ASID_ROLLOVER)
    );
    let _ = writeln!(
        s,
        "frame_alloc_lock_wait_ticks_total={}",
        read_counter(&crate::task::perf::FRAME_GLOBAL_ALLOC_LOCK_WAIT_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "frame_alloc_lock_hold_ticks_total={}",
        read_counter(&crate::task::perf::FRAME_GLOBAL_ALLOC_LOCK_HOLD_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "frame_free_lock_wait_ticks_total={}",
        read_counter(&crate::task::perf::FRAME_GLOBAL_FREE_LOCK_WAIT_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "frame_free_lock_hold_ticks_total={}",
        read_counter(&crate::task::perf::FRAME_GLOBAL_FREE_LOCK_HOLD_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "frame_reserve_check_calls={}",
        read_counter(&crate::task::perf::FRAME_RESERVE_CHECK_CALLS)
    );
    let _ = writeln!(
        s,
        "frame_reserve_check_ticks_total={}",
        read_counter(&crate::task::perf::FRAME_RESERVE_CHECK_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "frame_reserve_oom_calls={}",
        read_counter(&crate::task::perf::FRAME_RESERVE_OOM_CALLS)
    );
    let _ = writeln!(
        s,
        "frame_alloc_source_fresh={}",
        read_counter(&crate::task::perf::FRAME_ALLOC_SOURCE_FRESH)
    );
    let _ = writeln!(
        s,
        "frame_alloc_source_recycled={}",
        read_counter(&crate::task::perf::FRAME_ALLOC_SOURCE_RECYCLED)
    );
    let _ = writeln!(
        s,
        "frame_alloc_source_prezeroed={}",
        read_counter(&crate::task::perf::FRAME_ALLOC_SOURCE_PREZEROED)
    );
    let _ = writeln!(
        s,
        "frame_prezero_pool_hits={}",
        read_counter(&crate::task::perf::FRAME_PREZERO_POOL_HITS)
    );
    let _ = writeln!(
        s,
        "frame_prezero_pool_misses={}",
        read_counter(&crate::task::perf::FRAME_PREZERO_POOL_MISSES)
    );
    let _ = writeln!(
        s,
        "frame_prezero_refill_pages={}",
        read_counter(&crate::task::perf::FRAME_PREZERO_REFILL_PAGES)
    );
    let _ = writeln!(
        s,
        "frame_prezero_refill_ticks_total={}",
        read_counter(&crate::task::perf::FRAME_PREZERO_REFILL_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "frame_prezero_policy={}",
        crate::mm::prezero_policy_name()
    );
    let _ = writeln!(
        s,
        "frame_prezero_refill_skipped_policy={}",
        read_counter(&crate::task::perf::FRAME_PREZERO_REFILL_SKIPPED_POLICY)
    );
    let _ = writeln!(
        s,
        "frame_prezero_refill_skipped_active={}",
        read_counter(&crate::task::perf::FRAME_PREZERO_REFILL_SKIPPED_ACTIVE)
    );
    let _ = writeln!(
        s,
        "frame_sync_zero_pages={}",
        read_counter(&crate::task::perf::FRAME_SYNC_ZERO_PAGES)
    );
    let (prezero_current, prezero_high_water) = crate::mm::prezero_pool_stats();
    let _ = writeln!(s, "frame_prezero_pool_current={}", prezero_current);
    let _ = writeln!(s, "frame_prezero_pool_high_water={}", prezero_high_water);
    let _ = writeln!(
        s,
        "anon_fault_locality_total={}",
        read_counter(&crate::task::perf::ANON_FAULT_LOCALITY_TOTAL)
    );
    let _ = writeln!(
        s,
        "anon_fault_locality_forward_1={}",
        read_counter(&crate::task::perf::ANON_FAULT_LOCALITY_FORWARD_1)
    );
    let _ = writeln!(
        s,
        "anon_fault_locality_forward_2_4={}",
        read_counter(&crate::task::perf::ANON_FAULT_LOCALITY_FORWARD_2_4)
    );
    let _ = writeln!(
        s,
        "anon_fault_locality_backward_1={}",
        read_counter(&crate::task::perf::ANON_FAULT_LOCALITY_BACKWARD_1)
    );
    let _ = writeln!(
        s,
        "anon_fault_locality_other={}",
        read_counter(&crate::task::perf::ANON_FAULT_LOCALITY_OTHER)
    );
    let _ = writeln!(
        s,
        "anon_fault_locality_task_switch={}",
        read_counter(&crate::task::perf::ANON_FAULT_LOCALITY_TASK_SWITCH)
    );
    let _ = writeln!(
        s,
        "frame_contig_pages={}",
        read_counter(&crate::task::perf::FRAME_CONTIG_PAGES)
    );
    let _ = writeln!(
        s,
        "frame_contig_zero_ticks_total={}",
        read_counter(&crate::task::perf::FRAME_CONTIG_ZERO_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "heap_lock_wait_ticks_total={}",
        read_counter(&crate::task::perf::HEAP_LOCK_WAIT_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "heap_lock_hold_ticks_total={}",
        read_counter(&crate::task::perf::HEAP_LOCK_HOLD_TICKS_TOTAL)
    );
    let _ = writeln!(
        s,
        "heap_slab_alloc_calls={}",
        read_counter(&crate::task::perf::HEAP_SLAB_ALLOC_CALLS)
    );
    let _ = writeln!(
        s,
        "heap_direct_buddy_calls={}",
        read_counter(&crate::task::perf::HEAP_DIRECT_BUDDY_CALLS)
    );
    let _ = writeln!(
        s,
        "heap_large_calls={}",
        read_counter(&crate::task::perf::HEAP_LARGE_CALLS)
    );
    let _ = writeln!(
        s,
        "exec_ptload_segments={}",
        read_counter(&crate::task::perf::EXEC_PTLOAD_SEGMENTS)
    );
    let _ = writeln!(
        s,
        "exec_ptload_pages={}",
        read_counter(&crate::task::perf::EXEC_PTLOAD_PAGES)
    );
    let _ = writeln!(
        s,
        "exec_ptload_file_bytes={}",
        read_counter(&crate::task::perf::EXEC_PTLOAD_FILE_BYTES)
    );
    let _ = writeln!(
        s,
        "exec_prefetch_ticks={}",
        read_counter(&crate::task::perf::EXEC_PREFETCH_TICKS)
    );
    let _ = writeln!(
        s,
        "exec_target_alloc_ticks={}",
        read_counter(&crate::task::perf::EXEC_TARGET_ALLOC_TICKS)
    );
    let _ = writeln!(
        s,
        "exec_target_zero_ticks={}",
        read_counter(&crate::task::perf::EXEC_TARGET_ZERO_TICKS)
    );
    let _ = writeln!(
        s,
        "exec_pagecache_copy_ticks={}",
        read_counter(&crate::task::perf::EXEC_PAGECACHE_COPY_TICKS)
    );
    let _ = writeln!(
        s,
        "exec_fallback_kmap_wait_ticks={}",
        read_counter(&crate::task::perf::EXEC_FALLBACK_KMAP_WAIT_TICKS)
    );
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
        has_perf_stats,
        has_perf_diag
    );

    // ── /sys/kernel/stats/ ──
    let stats_dir = kernel_dir.add_dir_inner("stats", InodeMode::from_bits_truncate(0o555))?;
    stats_dir.add_file("features", ro_mode, stats_features_content)?;
    stats_dir.add_writable_file_with_write(
        "stats_on",
        rw_mode,
        stats_on_content,
        stats_on_write,
    )?;
    stats_dir.add_writable_file_with_write(
        "profile",
        rw_mode,
        stats_profile_content,
        stats_profile_write,
    )?;
    stats_dir.add_writable_file_with_write(
        "mount_diag_on",
        rw_mode,
        mount_diag_on_content,
        mount_diag_on_write,
    )?;
    stats_dir.add_write_only_file(
        "reset",
        InodeMode::from_bits_truncate(0o200),
        stats_reset_write,
    )?;
    stats_dir.add_file("boot", ro_mode, stats_boot_content)?;
    stats_dir.add_file("taskq", ro_mode, stats_taskq_content)?;
    stats_dir.add_file("timer", ro_mode, stats_timer_content)?;
    stats_dir.add_file("seccomp", ro_mode, stats_seccomp_content)?;
    stats_dir.add_file("syscall", ro_mode, stats_syscall_content)?;
    stats_dir.add_file("ctxsw", ro_mode, stats_ctxsw_content)?;
    stats_dir.add_file("reclaim", ro_mode, stats_reclaim_content)?;
    stats_dir.add_file("tlb", ro_mode, stats_tlb_content)?;
    stats_dir.add_file("heap", ro_mode, stats_heap_content)?;
    stats_dir.add_file("anon_unmap", ro_mode, stats_anon_unmap_content)?;
    stats_dir.add_file("pagecache", ro_mode, stats_pagecache_content)?;
    stats_dir.add_file("blockio", ro_mode, stats_blockio_content)?;
    stats_dir.add_file("net", ro_mode, stats_net_content)?;
    stats_dir.add_file("ext4", ro_mode, stats_ext4_content)?;
    stats_dir.add_file("resource", ro_mode, stats_resource_content)?;
    stats_dir.add_file("buddyinfo", ro_mode, stats_buddyinfo_content)?;
    stats_dir.add_file("zombies", ro_mode, stats_zombies_content)?;
    stats_dir.add_file("pipe", ro_mode, stats_pipe_content)?;
    stats_dir.add_file("lwext4", ro_mode, stats_lwext4_content)?;
    stats_dir.add_file("mount", ro_mode, stats_mount_content)?;
    stats_dir.add_file("syscall_top", ro_mode, stats_syscall_top_content)?;
    stats_dir.add_file("pagefault", ro_mode, stats_pagefault_content)?;
    stats_dir.add_file("vm", ro_mode, stats_vm_content)?;

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
