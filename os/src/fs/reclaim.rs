//! Periodically reclaim filesystem caches from the scheduler loop.
//!
//! Page cache watermarks are checked every `THROTTLE` scheduler ticks.
//! Expensive stale weak cleanup is budgeted and cursor-based so polluted
//! workloads do not accumulate one large inode/children prune spike.

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

// ── reclaim 插桩计数器 ───────────────────────────────────────────────────

static RECLAIM_CALLS: AtomicU64 = AtomicU64::new(0);
static RECLAIM_RUNS: AtomicU64 = AtomicU64::new(0);
static RECLAIM_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static RECLAIM_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static RECLAIM_LIVE_FS_TOTAL: AtomicU64 = AtomicU64::new(0);
static RECLAIM_IO_REMOVED: AtomicU64 = AtomicU64::new(0);
static RECLAIM_IO_SCANNED: AtomicU64 = AtomicU64::new(0);
static RECLAIM_IO_BUDGET_HIT: AtomicU64 = AtomicU64::new(0);
static RECLAIM_IO_SKIPPED: AtomicU64 = AtomicU64::new(0);
static RECLAIM_PC_REMOVED: AtomicU64 = AtomicU64::new(0);
static RECLAIM_KIDS_REMOVED: AtomicU64 = AtomicU64::new(0);
static RECLAIM_KIDS_PARENTS_SCANNED: AtomicU64 = AtomicU64::new(0);
static RECLAIM_KIDS_ENTRIES_SCANNED: AtomicU64 = AtomicU64::new(0);
static RECLAIM_KIDS_BUDGET_HIT: AtomicU64 = AtomicU64::new(0);
static RECLAIM_KIDS_TIME_BUDGET_HIT: AtomicU64 = AtomicU64::new(0);
static RECLAIM_KIDS_SKIPPED: AtomicU64 = AtomicU64::new(0);
static RECLAIM_CLEAN_FREED: AtomicU64 = AtomicU64::new(0);
static RECLAIM_CACHED_PAGES_MAX: AtomicUsize = AtomicUsize::new(0);
static RECLAIM_HEAP_PRESSURE_RUNS: AtomicU64 = AtomicU64::new(0);
static RECLAIM_HEAP_CRITICAL_RUNS: AtomicU64 = AtomicU64::new(0);

// ── 分阶段计时 ─────────────────────────────────────────────────────────────

static STAGE_FIFO_CALLS: AtomicU64 = AtomicU64::new(0);
static STAGE_FIFO_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static STAGE_FIFO_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);

static STAGE_REGISTRY_CALLS: AtomicU64 = AtomicU64::new(0);
static STAGE_REGISTRY_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static STAGE_REGISTRY_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);

static STAGE_PRUNE_IO_CALLS: AtomicU64 = AtomicU64::new(0);
static STAGE_PRUNE_IO_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static STAGE_PRUNE_IO_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);

static STAGE_PRUNE_PC_CALLS: AtomicU64 = AtomicU64::new(0);
static STAGE_PRUNE_PC_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static STAGE_PRUNE_PC_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);

static STAGE_PRUNE_KIDS_CALLS: AtomicU64 = AtomicU64::new(0);
static STAGE_PRUNE_KIDS_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static STAGE_PRUNE_KIDS_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);

static STAGE_EVICT_DIR_CALLS: AtomicU64 = AtomicU64::new(0);
static STAGE_EVICT_DIR_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static STAGE_EVICT_DIR_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);

static STAGE_CACHE_METRIC_CALLS: AtomicU64 = AtomicU64::new(0);
static STAGE_CACHE_METRIC_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static STAGE_CACHE_METRIC_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);

static STAGE_SHRINK_CALLS: AtomicU64 = AtomicU64::new(0);
static STAGE_SHRINK_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static STAGE_SHRINK_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);

#[inline(always)]
fn stage_tick(calls: &AtomicU64, total: &AtomicU64, max: &AtomicU64, dt: u64) {
    calls.fetch_add(1, Ordering::Relaxed);
    total.fetch_add(dt, Ordering::Relaxed);
    atomic_max_u64(max, dt);
}

#[inline(always)]
fn reclaim_cycle_now() -> u64 {
    #[cfg(target_arch = "riscv64")]
    {
        let cycles: usize;
        unsafe { core::arch::asm!("rdcycle {}", out(reg) cycles) };
        cycles as u64
    }
    #[cfg(target_arch = "loongarch64")]
    {
        let lo: usize;
        let hi: usize;
        unsafe { core::arch::asm!("rdtime.d {}, {}", out(reg) lo, out(reg) hi) };
        lo as u64
    }
    #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
    {
        0
    }
}

fn atomic_max_usize(slot: &AtomicUsize, value: usize) {
    let mut cur = slot.load(Ordering::Relaxed);
    while value > cur {
        match slot.compare_exchange_weak(cur, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(next) => cur = next,
        }
    }
}

fn atomic_max_u64(slot: &AtomicU64, value: u64) {
    let mut cur = slot.load(Ordering::Relaxed);
    while value > cur {
        match slot.compare_exchange_weak(cur, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(next) => cur = next,
        }
    }
}

pub fn reset_reclaim_stats() {
    RECLAIM_CALLS.store(0, Ordering::Relaxed);
    RECLAIM_RUNS.store(0, Ordering::Relaxed);
    RECLAIM_CYCLES_TOTAL.store(0, Ordering::Relaxed);
    RECLAIM_CYCLES_MAX.store(0, Ordering::Relaxed);
    RECLAIM_LIVE_FS_TOTAL.store(0, Ordering::Relaxed);
    RECLAIM_IO_REMOVED.store(0, Ordering::Relaxed);
    RECLAIM_IO_SCANNED.store(0, Ordering::Relaxed);
    RECLAIM_IO_BUDGET_HIT.store(0, Ordering::Relaxed);
    RECLAIM_IO_SKIPPED.store(0, Ordering::Relaxed);
    RECLAIM_PC_REMOVED.store(0, Ordering::Relaxed);
    RECLAIM_KIDS_REMOVED.store(0, Ordering::Relaxed);
    RECLAIM_KIDS_PARENTS_SCANNED.store(0, Ordering::Relaxed);
    RECLAIM_KIDS_ENTRIES_SCANNED.store(0, Ordering::Relaxed);
    RECLAIM_KIDS_BUDGET_HIT.store(0, Ordering::Relaxed);
    RECLAIM_KIDS_TIME_BUDGET_HIT.store(0, Ordering::Relaxed);
    RECLAIM_KIDS_SKIPPED.store(0, Ordering::Relaxed);
    RECLAIM_CLEAN_FREED.store(0, Ordering::Relaxed);
    RECLAIM_CACHED_PAGES_MAX.store(0, Ordering::Relaxed);
    RECLAIM_HEAP_PRESSURE_RUNS.store(0, Ordering::Relaxed);
    RECLAIM_HEAP_CRITICAL_RUNS.store(0, Ordering::Relaxed);
    reset_stage_stats();
}

fn reset_stage_stats() {
    STAGE_FIFO_CALLS.store(0, Ordering::Relaxed);
    STAGE_FIFO_CYCLES_TOTAL.store(0, Ordering::Relaxed);
    STAGE_FIFO_CYCLES_MAX.store(0, Ordering::Relaxed);
    STAGE_REGISTRY_CALLS.store(0, Ordering::Relaxed);
    STAGE_REGISTRY_CYCLES_TOTAL.store(0, Ordering::Relaxed);
    STAGE_REGISTRY_CYCLES_MAX.store(0, Ordering::Relaxed);
    STAGE_PRUNE_IO_CALLS.store(0, Ordering::Relaxed);
    STAGE_PRUNE_IO_CYCLES_TOTAL.store(0, Ordering::Relaxed);
    STAGE_PRUNE_IO_CYCLES_MAX.store(0, Ordering::Relaxed);
    STAGE_PRUNE_PC_CALLS.store(0, Ordering::Relaxed);
    STAGE_PRUNE_PC_CYCLES_TOTAL.store(0, Ordering::Relaxed);
    STAGE_PRUNE_PC_CYCLES_MAX.store(0, Ordering::Relaxed);
    STAGE_PRUNE_KIDS_CALLS.store(0, Ordering::Relaxed);
    STAGE_PRUNE_KIDS_CYCLES_TOTAL.store(0, Ordering::Relaxed);
    STAGE_PRUNE_KIDS_CYCLES_MAX.store(0, Ordering::Relaxed);
    STAGE_EVICT_DIR_CALLS.store(0, Ordering::Relaxed);
    STAGE_EVICT_DIR_CYCLES_TOTAL.store(0, Ordering::Relaxed);
    STAGE_EVICT_DIR_CYCLES_MAX.store(0, Ordering::Relaxed);
    STAGE_CACHE_METRIC_CALLS.store(0, Ordering::Relaxed);
    STAGE_CACHE_METRIC_CYCLES_TOTAL.store(0, Ordering::Relaxed);
    STAGE_CACHE_METRIC_CYCLES_MAX.store(0, Ordering::Relaxed);
    STAGE_SHRINK_CALLS.store(0, Ordering::Relaxed);
    STAGE_SHRINK_CYCLES_TOTAL.store(0, Ordering::Relaxed);
    STAGE_SHRINK_CYCLES_MAX.store(0, Ordering::Relaxed);
}

pub fn dump_reclaim_stats(label: &str) {
    let calls = RECLAIM_CALLS.load(Ordering::Relaxed);
    let runs = RECLAIM_RUNS.load(Ordering::Relaxed);
    println!("=== reclaim Profile: {} ===", label);
    println!(
        "reclaim calls={} runs={} cycles_total={} cycles_max={} live_fs_total={}",
        calls,
        runs,
        RECLAIM_CYCLES_TOTAL.load(Ordering::Relaxed),
        RECLAIM_CYCLES_MAX.load(Ordering::Relaxed),
        RECLAIM_LIVE_FS_TOTAL.load(Ordering::Relaxed),
    );
    println!(
        "reclaim io_removed={} pc_removed={} kids_removed={} clean_freed={} cached_pages_max={} heap_pressure_runs={} heap_critical_runs={}",
        RECLAIM_IO_REMOVED.load(Ordering::Relaxed),
        RECLAIM_PC_REMOVED.load(Ordering::Relaxed),
        RECLAIM_KIDS_REMOVED.load(Ordering::Relaxed),
        RECLAIM_CLEAN_FREED.load(Ordering::Relaxed),
        RECLAIM_CACHED_PAGES_MAX.load(Ordering::Relaxed),
        RECLAIM_HEAP_PRESSURE_RUNS.load(Ordering::Relaxed),
        RECLAIM_HEAP_CRITICAL_RUNS.load(Ordering::Relaxed),
    );
    println!(
        "reclaim_budget io_scanned={} io_budget_hit={} io_skipped={} kids_parents_scanned={} kids_entries_scanned={} kids_budget_hit={} kids_time_hit={} kids_skipped={}",
        RECLAIM_IO_SCANNED.load(Ordering::Relaxed),
        RECLAIM_IO_BUDGET_HIT.load(Ordering::Relaxed),
        RECLAIM_IO_SKIPPED.load(Ordering::Relaxed),
        RECLAIM_KIDS_PARENTS_SCANNED.load(Ordering::Relaxed),
        RECLAIM_KIDS_ENTRIES_SCANNED.load(Ordering::Relaxed),
        RECLAIM_KIDS_BUDGET_HIT.load(Ordering::Relaxed),
        RECLAIM_KIDS_TIME_BUDGET_HIT.load(Ordering::Relaxed),
        RECLAIM_KIDS_SKIPPED.load(Ordering::Relaxed),
    );
    // Stage breakdown
    dump_stage(
        "fifo",
        &STAGE_FIFO_CALLS,
        &STAGE_FIFO_CYCLES_TOTAL,
        &STAGE_FIFO_CYCLES_MAX,
    );
    dump_stage(
        "registry",
        &STAGE_REGISTRY_CALLS,
        &STAGE_REGISTRY_CYCLES_TOTAL,
        &STAGE_REGISTRY_CYCLES_MAX,
    );
    dump_stage(
        "prune_io",
        &STAGE_PRUNE_IO_CALLS,
        &STAGE_PRUNE_IO_CYCLES_TOTAL,
        &STAGE_PRUNE_IO_CYCLES_MAX,
    );
    dump_stage(
        "prune_pc",
        &STAGE_PRUNE_PC_CALLS,
        &STAGE_PRUNE_PC_CYCLES_TOTAL,
        &STAGE_PRUNE_PC_CYCLES_MAX,
    );
    dump_stage(
        "prune_kids",
        &STAGE_PRUNE_KIDS_CALLS,
        &STAGE_PRUNE_KIDS_CYCLES_TOTAL,
        &STAGE_PRUNE_KIDS_CYCLES_MAX,
    );
    dump_stage(
        "evict_dir",
        &STAGE_EVICT_DIR_CALLS,
        &STAGE_EVICT_DIR_CYCLES_TOTAL,
        &STAGE_EVICT_DIR_CYCLES_MAX,
    );
    dump_stage(
        "cache_metric",
        &STAGE_CACHE_METRIC_CALLS,
        &STAGE_CACHE_METRIC_CYCLES_TOTAL,
        &STAGE_CACHE_METRIC_CYCLES_MAX,
    );
    dump_stage(
        "shrink",
        &STAGE_SHRINK_CALLS,
        &STAGE_SHRINK_CYCLES_TOTAL,
        &STAGE_SHRINK_CYCLES_MAX,
    );
}

fn dump_stage(name: &str, calls: &AtomicU64, total: &AtomicU64, max: &AtomicU64) {
    let c = calls.load(Ordering::Relaxed);
    let t = total.load(Ordering::Relaxed);
    let m = max.load(Ordering::Relaxed);
    println!(
        "reclaim_stage_{} calls={} cycles_total={} cycles_max={}",
        name, c, t, m
    );
}

const THROTTLE: usize = 64;
const LOW_WATER_PAGES: isize = 1024;   // 4MB — gentle eviction
const HIGH_WATER_PAGES: isize = 4096;  // 16MB — aggressive eviction
const BATCH_PAGES: usize = 64;
const LOW_BATCH_PAGES: usize = 8;
const CRITICAL_BATCH_PAGES: usize = 32;
const HEAP_PRESSURE_PCT: usize = 75;   // trigger eviction when >75% heap used
const HEAP_CRITICAL_PCT: usize = 90;   // aggressive multi-cache eviction
const INODE_PRUNE_BUDGET: usize = 64;
const CHILDREN_PRUNE_PARENT_BUDGET: usize = 8;
const CHILDREN_PRUNE_ENTRY_BUDGET: usize = 64;
const INODE_PRUNE_FORCE_PRESSURE_BUDGET: usize = 32;
const INODE_PRUNE_FORCE_CRITICAL_BUDGET: usize = 64;
const CHILDREN_PRUNE_PARENT_FORCE_PRESSURE_BUDGET: usize = 4;
const CHILDREN_PRUNE_PARENT_FORCE_CRITICAL_BUDGET: usize = 8;
const CHILDREN_PRUNE_ENTRY_FORCE_PRESSURE_BUDGET: usize = 32;
const CHILDREN_PRUNE_ENTRY_FORCE_CRITICAL_BUDGET: usize = 64;
const CHILDREN_PRUNE_CYCLE_BUDGET: u64 = 8_000_000;

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
    RECLAIM_CALLS.fetch_add(1, Ordering::Relaxed);
    crate::task::perf::record_reclaim_run();
    static TICK: AtomicUsize = AtomicUsize::new(0);

    let tick = TICK.fetch_add(1, Ordering::Relaxed);
    if tick % THROTTLE != 0 {
        return;
    }

    RECLAIM_RUNS.fetch_add(1, Ordering::Relaxed);
    let _t0 = reclaim_cycle_now();

    // Cooperative background writeback: flush dirty pages before
    // reclaim stages so sync/fsync tail latency is smoothed.
    crate::fs::page_cache::maybe_background_writeback();

    let under_pressure = heap_under_pressure();
    let critical = heap_critical();
    let force_weak_prune = under_pressure || critical;
    // Weak cache cleanup is not the memory-safety path. Keep forced sweeps
    // small even under heap pressure so scheduler-loop reclaim cannot build
    // a long pipe/context-switch latency spike.
    let inode_budget = if critical {
        INODE_PRUNE_FORCE_CRITICAL_BUDGET
    } else if under_pressure {
        INODE_PRUNE_FORCE_PRESSURE_BUDGET
    } else {
        INODE_PRUNE_BUDGET
    };
    let children_parent_budget = if critical {
        CHILDREN_PRUNE_PARENT_FORCE_CRITICAL_BUDGET
    } else if under_pressure {
        CHILDREN_PRUNE_PARENT_FORCE_PRESSURE_BUDGET
    } else {
        CHILDREN_PRUNE_PARENT_BUDGET
    };
    let children_entry_budget = if critical {
        CHILDREN_PRUNE_ENTRY_FORCE_CRITICAL_BUDGET
    } else if under_pressure {
        CHILDREN_PRUNE_ENTRY_FORCE_PRESSURE_BUDGET
    } else {
        CHILDREN_PRUNE_ENTRY_BUDGET
    };

    // Stage 1: compact_fifo_registry
    let t1 = reclaim_cycle_now();
    crate::fs::dev::pipe::compact_fifo_registry();
    let t2 = reclaim_cycle_now();
    stage_tick(
        &STAGE_FIFO_CALLS,
        &STAGE_FIFO_CYCLES_TOTAL,
        &STAGE_FIFO_CYCLES_MAX,
        t2.saturating_sub(t1),
    );

    // Stage 2: ext4_registry lock + live collect + retain
    let mut guard = crate::fs::ext4::ext4fs::EXT4_REGISTRY.lock();
    let live: alloc::vec::Vec<_> = guard.iter().filter_map(|w| w.upgrade()).collect();
    guard.retain(|w| w.strong_count() > 0);
    drop(guard);
    let t3 = reclaim_cycle_now();
    stage_tick(
        &STAGE_REGISTRY_CALLS,
        &STAGE_REGISTRY_CYCLES_TOTAL,
        &STAGE_REGISTRY_CYCLES_MAX,
        t3.saturating_sub(t2),
    );

    RECLAIM_LIVE_FS_TOTAL.fetch_add(live.len() as u64, Ordering::Relaxed);

    for fs in &live {
        // Stage 3: budgeted prune_inode_objects
        let ts = reclaim_cycle_now();
        let io_stats = fs.prune_inode_objects_budgeted(inode_budget, force_weak_prune);
        let te = reclaim_cycle_now();
        stage_tick(
            &STAGE_PRUNE_IO_CALLS,
            &STAGE_PRUNE_IO_CYCLES_TOTAL,
            &STAGE_PRUNE_IO_CYCLES_MAX,
            te.saturating_sub(ts),
        );
        let io_removed = io_stats.removed;

        // Stage 4: prune_page_caches
        let ts = reclaim_cycle_now();
        let pc_removed = fs.prune_page_caches();
        let te = reclaim_cycle_now();
        stage_tick(
            &STAGE_PRUNE_PC_CALLS,
            &STAGE_PRUNE_PC_CYCLES_TOTAL,
            &STAGE_PRUNE_PC_CYCLES_MAX,
            te.saturating_sub(ts),
        );

        // Stage 5: budgeted prune_children_stale_entries
        let ts = reclaim_cycle_now();
        let kids_stats = fs.prune_children_stale_entries_budgeted(
            children_parent_budget,
            children_entry_budget,
            CHILDREN_PRUNE_CYCLE_BUDGET,
            force_weak_prune,
        );
        let te = reclaim_cycle_now();
        stage_tick(
            &STAGE_PRUNE_KIDS_CALLS,
            &STAGE_PRUNE_KIDS_CYCLES_TOTAL,
            &STAGE_PRUNE_KIDS_CYCLES_MAX,
            te.saturating_sub(ts),
        );
        let kids_removed = kids_stats.removed;

        RECLAIM_IO_REMOVED.fetch_add(io_removed as u64, Ordering::Relaxed);
        RECLAIM_IO_SCANNED.fetch_add(io_stats.scanned as u64, Ordering::Relaxed);
        if io_stats.budget_hit {
            RECLAIM_IO_BUDGET_HIT.fetch_add(1, Ordering::Relaxed);
        }
        if io_stats.skipped {
            RECLAIM_IO_SKIPPED.fetch_add(1, Ordering::Relaxed);
        }
        RECLAIM_PC_REMOVED.fetch_add(pc_removed as u64, Ordering::Relaxed);
        RECLAIM_KIDS_REMOVED.fetch_add(kids_removed as u64, Ordering::Relaxed);
        RECLAIM_KIDS_PARENTS_SCANNED
            .fetch_add(kids_stats.parents_scanned as u64, Ordering::Relaxed);
        RECLAIM_KIDS_ENTRIES_SCANNED
            .fetch_add(kids_stats.entries_scanned as u64, Ordering::Relaxed);
        if kids_stats.budget_hit {
            RECLAIM_KIDS_BUDGET_HIT.fetch_add(1, Ordering::Relaxed);
        }
        if kids_stats.time_budget_hit {
            RECLAIM_KIDS_TIME_BUDGET_HIT.fetch_add(1, Ordering::Relaxed);
        }
        if kids_stats.skipped {
            RECLAIM_KIDS_SKIPPED.fetch_add(1, Ordering::Relaxed);
        }

        // Stage 6: evict_dir_cache
        let ts = reclaim_cycle_now();
        fs.evict_dir_cache();
        let te = reclaim_cycle_now();
        stage_tick(
            &STAGE_EVICT_DIR_CALLS,
            &STAGE_EVICT_DIR_CYCLES_TOTAL,
            &STAGE_EVICT_DIR_CYCLES_MAX,
            te.saturating_sub(ts),
        );

        // Stage 7: get_cache_metric
        let ts = reclaim_cycle_now();
        let cached = fs.get_cache_metric(6); // page_cache_cached_pages
        let te = reclaim_cycle_now();
        stage_tick(
            &STAGE_CACHE_METRIC_CALLS,
            &STAGE_CACHE_METRIC_CYCLES_TOTAL,
            &STAGE_CACHE_METRIC_CYCLES_MAX,
            te.saturating_sub(ts),
        );
        atomic_max_usize(&RECLAIM_CACHED_PAGES_MAX, cached.max(0) as usize);

        if critical {
            RECLAIM_HEAP_CRITICAL_RUNS.fetch_add(1, Ordering::Relaxed);
            let ts = reclaim_cycle_now();
            let freed = fs.shrink_all_page_caches_clean(CRITICAL_BATCH_PAGES);
            let te = reclaim_cycle_now();
            stage_tick(
                &STAGE_SHRINK_CALLS,
                &STAGE_SHRINK_CYCLES_TOTAL,
                &STAGE_SHRINK_CYCLES_MAX,
                te.saturating_sub(ts),
            );
            RECLAIM_CLEAN_FREED.fetch_add(freed as u64, Ordering::Relaxed);
            if freed > 0 {
                log::warn!(
                    "[reclaim] CRITICAL heap={}% clean_freed={} stale: io={} pc={} kids={} cached={}",
                    heap_used_pct(), freed, io_removed, pc_removed, kids_removed, cached
                );
            }
        } else if cached > HIGH_WATER_PAGES {
            let ts = reclaim_cycle_now();
            let freed = fs.shrink_all_page_caches_clean(BATCH_PAGES);
            let te = reclaim_cycle_now();
            stage_tick(
                &STAGE_SHRINK_CALLS,
                &STAGE_SHRINK_CYCLES_TOTAL,
                &STAGE_SHRINK_CYCLES_MAX,
                te.saturating_sub(ts),
            );
            RECLAIM_CLEAN_FREED.fetch_add(freed as u64, Ordering::Relaxed);
            if freed > 0 {
                log::debug!(
                    "[reclaim] high-water clean_freed={} stale: io={} pc={} kids={}",
                    freed, io_removed, pc_removed, kids_removed
                );
            }
        } else if cached > LOW_WATER_PAGES || under_pressure {
            RECLAIM_HEAP_PRESSURE_RUNS.fetch_add(1, Ordering::Relaxed);
            let ts = reclaim_cycle_now();
            let freed = fs.shrink_all_page_caches_clean(LOW_BATCH_PAGES);
            let te = reclaim_cycle_now();
            stage_tick(
                &STAGE_SHRINK_CALLS,
                &STAGE_SHRINK_CYCLES_TOTAL,
                &STAGE_SHRINK_CYCLES_MAX,
                te.saturating_sub(ts),
            );
            RECLAIM_CLEAN_FREED.fetch_add(freed as u64, Ordering::Relaxed);
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

    let dt = reclaim_cycle_now().saturating_sub(_t0);
    RECLAIM_CYCLES_TOTAL.fetch_add(dt, Ordering::Relaxed);
    atomic_max_u64(&RECLAIM_CYCLES_MAX, dt);
}
