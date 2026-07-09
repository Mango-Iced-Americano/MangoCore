//! 任务、调度、内存和 I/O 热路径性能计数器。
//!
//! `perf_stats` feature 开启时，本模块导出真实计数器和记录函数；关闭时导出
//! 同名 no-op/stub，保证调用点无需条件编译。运行时开关 `STATS_ON` 控制是否记录。
//!
//! # Semantics
//!
//! 计数器使用 relaxed atomic，只用于诊断和趋势观察，不提供同步或 happens-before
//! 语义。

/// Runtime gate: 0 = counters frozen (no-op), 1 = recording.
/// Toggled via /sys/kernel/stats/stats_on. Always present so sysfs can
/// read/write it even when `perf_stats` is disabled at compile time.
pub static STATS_ON: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
fn stats_enabled() -> bool {
    false
}

#[cfg(feature = "perf_stats")]
mod enabled {
    use super::super::WaitResult;
    use core::sync::atomic::{AtomicUsize, Ordering};

    const PRINT_EVERY_CLONES: usize = 512;
    const PRINT_EVERY_EXITS: usize = 512;
    const PRINT_EVERY_FUTEX: usize = 2048;
    const PRINT_EVERY_SCHEDULES: usize = 8192;

    static CLONE_TOTAL: AtomicUsize = AtomicUsize::new(0);
    static CLONE_THREAD: AtomicUsize = AtomicUsize::new(0);
    static CLONE_SHARE_VM: AtomicUsize = AtomicUsize::new(0);
    static CLONE_STACK_ALLOC: AtomicUsize = AtomicUsize::new(0);
    static EXIT_THREAD: AtomicUsize = AtomicUsize::new(0);
    static EXIT_CLEAR_CHILD_TID: AtomicUsize = AtomicUsize::new(0);
    static EXIT_KEEP_TRAP: AtomicUsize = AtomicUsize::new(0);
    static TRAP_CACHE_STORE: AtomicUsize = AtomicUsize::new(0);
    static TRAP_CACHE_SKIP: AtomicUsize = AtomicUsize::new(0);
    static TRAP_CACHE_HIT: AtomicUsize = AtomicUsize::new(0);
    static TRAP_CACHE_MISS: AtomicUsize = AtomicUsize::new(0);
    static KSTACK_CACHE_HIT: AtomicUsize = AtomicUsize::new(0);
    static KSTACK_CACHE_MISS: AtomicUsize = AtomicUsize::new(0);
    static KSTACK_CACHE_STORE: AtomicUsize = AtomicUsize::new(0);
    static KSTACK_CACHE_DROP: AtomicUsize = AtomicUsize::new(0);
    static ZOMBIE_ENQUEUE: AtomicUsize = AtomicUsize::new(0);
    static ZOMBIE_DRAIN: AtomicUsize = AtomicUsize::new(0);
    static SCHEDULE_LOOPS: AtomicUsize = AtomicUsize::new(0);
    static SCHEDULE_FETCH: AtomicUsize = AtomicUsize::new(0);
    static SCHEDULE_IDLE: AtomicUsize = AtomicUsize::new(0);
    static TIMER_INTERRUPTS: AtomicUsize = AtomicUsize::new(0);
    static FUTEX_WAIT: AtomicUsize = AtomicUsize::new(0);
    static FUTEX_WAIT_SHARED: AtomicUsize = AtomicUsize::new(0);
    static FUTEX_WAIT_DEADLINE: AtomicUsize = AtomicUsize::new(0);
    static FUTEX_WAIT_READY: AtomicUsize = AtomicUsize::new(0);
    static FUTEX_WAIT_TIMEOUT: AtomicUsize = AtomicUsize::new(0);
    static FUTEX_WAIT_INTR: AtomicUsize = AtomicUsize::new(0);
    static FUTEX_WAKE: AtomicUsize = AtomicUsize::new(0);
    static FUTEX_WAKE_SHARED: AtomicUsize = AtomicUsize::new(0);
    static FUTEX_WAKE_HIT: AtomicUsize = AtomicUsize::new(0);
    static SYSCALL_CLONE: AtomicUsize = AtomicUsize::new(0);
    static SYSCALL_FUTEX: AtomicUsize = AtomicUsize::new(0);
    static SYSCALL_MMAP: AtomicUsize = AtomicUsize::new(0);
    static SYSCALL_MUNMAP: AtomicUsize = AtomicUsize::new(0);
    static SYSCALL_MPROTECT: AtomicUsize = AtomicUsize::new(0);
    static SYSCALL_SET_TID_ADDRESS: AtomicUsize = AtomicUsize::new(0);
    static SYSCALL_SET_ROBUST_LIST: AtomicUsize = AtomicUsize::new(0);
    static SYSCALL_EXIT: AtomicUsize = AtomicUsize::new(0);
    static SYSCALL_YIELD: AtomicUsize = AtomicUsize::new(0);
    static LAST_SYSCALL_ID: AtomicUsize = AtomicUsize::new(0);
    static LAST_SYSCALL_RET: AtomicUsize = AtomicUsize::new(0);

    // ── Per-syscall profiling ──
    pub const PERF_SYSCOUNT: usize = 512;
    static SYSCALL_COUNT: [AtomicUsize; PERF_SYSCOUNT] = [const { AtomicUsize::new(0) }; PERF_SYSCOUNT];
    static SYSCALL_TICKS: [AtomicUsize; PERF_SYSCOUNT] = [const { AtomicUsize::new(0) }; PERF_SYSCOUNT];

    // ── Page fault per-action profile ──
    // action 0=LazyAlloc 1=FileBackedRead 2=FileBackedSharedWrite
    //        3=FileBackedWrite 4=SharedWrite 5=Cow 6=Other
    pub const PF_ACTION_NAMES: [&str; 7] = [
        "LazyAlloc", "FileBackedRead", "FileBackedSharedWrite",
        "FileBackedWrite", "SharedWrite", "Cow", "Other",
    ];
    static PF_ACTION_COUNT: [AtomicUsize; 7] = [const { AtomicUsize::new(0) }; 7];
    static PF_ACTION_TICKS: [AtomicUsize; 7] = [const { AtomicUsize::new(0) }; 7];

    // ── Filemap fault phase counters ──
    pub static FILEMAP_FAULT_FRAMES: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_FAULT_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_PRIVATE_COPY_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_MAP_USER_TICKS: AtomicUsize = AtomicUsize::new(0);

    // ── TLB flush cycle counters ──
    pub static TLB_PAGE_FLUSH_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static TLB_FULL_FLUSH_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static TLB_ACTIVATE_CYCLES: AtomicUsize = AtomicUsize::new(0);

    // ── Execve phase cycle counters ──
    pub static EXECVE_MAP_ELF_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static EXECVE_KERNEL_MAP_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static EXECVE_INTERP_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static EXECVE_STACK_TABLES_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static EXECVE_TEARDOWN_TICKS: AtomicUsize = AtomicUsize::new(0);

    // Fine-grained TLB counters
    pub static TLB_FLUSHES: AtomicUsize = AtomicUsize::new(0);    // total
    pub static TLB_FULL: AtomicUsize = AtomicUsize::new(0);       // full inval (invtlb 0x3 / sfence.vma no-arg)
    pub static TLB_PAGE: AtomicUsize = AtomicUsize::new(0);       // single-page (invtlb 0x5 / sfence.vma addr)
    pub static TLB_ACTIVATE: AtomicUsize = AtomicUsize::new(0);   // address-space switch
    pub static TLB_GLOBAL: AtomicUsize = AtomicUsize::new(0);     // global inval (invtlb 0x0)
    static FRAME_ALLOC_HITS: AtomicUsize = AtomicUsize::new(0);
    static FRAME_FREE_HITS: AtomicUsize = AtomicUsize::new(0);
    static PAGE_FAULTS: AtomicUsize = AtomicUsize::new(0);
    static VFS_LOOKUPS: AtomicUsize = AtomicUsize::new(0);
    static VFS_LOOKUP_TIME_TICKS: AtomicUsize = AtomicUsize::new(0);
    static VFS_LOOKUP_TIME_COUNT: AtomicUsize = AtomicUsize::new(0);
    // Timing accumulators (raw ticks from get_time)
    static CLONE_TIME_TICKS: AtomicUsize = AtomicUsize::new(0);
    static PAGEFAULT_TIME_TICKS: AtomicUsize = AtomicUsize::new(0);
    static FRAME_ALLOC_TIME_TICKS: AtomicUsize = AtomicUsize::new(0);
    static CLONE_TIME_COUNT: AtomicUsize = AtomicUsize::new(0);
    static PAGEFAULT_TIME_COUNT: AtomicUsize = AtomicUsize::new(0);
    static FRAME_ALLOC_TIME_COUNT: AtomicUsize = AtomicUsize::new(0);

    #[inline(always)]
    fn stats_enabled() -> bool {
        super::STATS_ON.load(Ordering::Relaxed)
    }

    // ── P0: Scheduler / Task Queue ──
    pub static FAIR_PICK_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static FAST_PATH_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static FAIR_SCAN_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static DUPLICATE_READY_ENQUEUE: AtomicUsize = AtomicUsize::new(0);
    pub static ADD_READY_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static ADD_INTERRUPTIBLE_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static WAKE_INTERRUPTIBLE_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static READY_LEN_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static INTERRUPTIBLE_LEN_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static READY_ZOMBIE_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static INTERRUPTIBLE_ZOMBIE_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static ZOMBIE_DRAIN_SCAN_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static ZOMBIE_DRAIN_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static ZOMBIE_DRAIN_REMOVED: AtomicUsize = AtomicUsize::new(0);
    pub static READY_NONZERO_NICE_CUR: AtomicUsize = AtomicUsize::new(0);

    // ── P0: Kernel Timer ──
    pub static KTIMER_LEN_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static KTIMER_ADD_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static KTIMER_POP_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static KTIMER_POP_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static KTIMER_STALE_WAKETASK: AtomicUsize = AtomicUsize::new(0);
    pub static KTIMER_REAL_WAKE: AtomicUsize = AtomicUsize::new(0);
    pub static KTIMER_COMPACT_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static KTIMER_STALE_REMOVED: AtomicUsize = AtomicUsize::new(0);
    pub static WAIT_WITH_TIMEOUT_TOTAL: AtomicUsize = AtomicUsize::new(0);

    // ── P0: Timer IRQ / Pop Cost ──
    pub static TIMER_IRQ_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static TIMER_IRQ_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static TIMER_POP_NODES_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static TIMER_POP_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static TIMER_POP_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);

    // ── P0: Seccomp ──
    pub static SECCOMP_CHECK_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static SECCOMP_CHECK_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static SECCOMP_CHECK_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static SECCOMP_DISABLED_BYPASS: AtomicUsize = AtomicUsize::new(0);

    // ── P0: Syscall / Trap ──
    pub static SYSCALL_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static SYSCALL_GETPPID_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static SYSCALL_COST_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static TRAP_ENTER_COST_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);

    // ── P1: Syscall Cost (average + total) ──
    pub static GETPPID_COST_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static GETPPID_COST_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static SYSCALL_COST_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static ECALL_TRAP_COST_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static ECALL_TRAP_COST_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);

    // ── P1: Context Switch ──
    pub static CONTEXT_SWITCH_TOTAL: AtomicUsize = AtomicUsize::new(0);

    // ── P1: Page Reclaim ──
    pub static RECLAIM_RUNS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static RECLAIM_PAGES_SCANNED_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static RECLAIM_PAGES_FREED_TOTAL: AtomicUsize = AtomicUsize::new(0);

    // ── P1: Clock Eviction ──
    pub static CLOCK_SCANNED: AtomicUsize = AtomicUsize::new(0);
    pub static CLOCK_SECOND_CHANCE: AtomicUsize = AtomicUsize::new(0);
    pub static CLOCK_EVICTED: AtomicUsize = AtomicUsize::new(0);

    // ── P0: PageCache I/O ──
    pub static PC_READ_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static PC_READ_PAGES: AtomicUsize = AtomicUsize::new(0);
    pub static PC_READ_MISS: AtomicUsize = AtomicUsize::new(0);
    pub static PC_READ_CYCLES_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static PC_READ_HIT_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PC_READ_MISS_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PC_COPY_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PC_LOOKUP_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PC_WRITE_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static PC_WRITE_PAGES: AtomicUsize = AtomicUsize::new(0);
    pub static PC_WRITE_OVERWRITE: AtomicUsize = AtomicUsize::new(0);
    pub static PC_WRITE_EVENTUALLY_FULL: AtomicUsize = AtomicUsize::new(0);
    pub static PC_WRITE_CYCLES_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static PC_WRITEBACK_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static PC_WRITEBACK_PAGES: AtomicUsize = AtomicUsize::new(0);
    pub static PC_WRITEBACK_CYCLES_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static PC_FALLOC_CYCLES_TOTAL: AtomicUsize = AtomicUsize::new(0);

    // ── P0: Writeback Throttling ──
    pub static WB_BG_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static WB_THROTTLE_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static WB_REDIRTY_PAGES: AtomicUsize = AtomicUsize::new(0);

    // ── P0: Block Device I/O ──
    pub static BLK_VREAD_REQS: AtomicUsize = AtomicUsize::new(0);
    pub static BLK_VREAD_SECS: AtomicUsize = AtomicUsize::new(0);
    pub static BLK_VWRITE_REQS: AtomicUsize = AtomicUsize::new(0);
    pub static BLK_VWRITE_SECS: AtomicUsize = AtomicUsize::new(0);

    // ── P0: Ext4 Block Mapping ──
    pub static EXT4_MAP_LBLOCK_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static EXT4_MAP_LBLOCK_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static EXT4_MAP_CACHE_HITS: AtomicUsize = AtomicUsize::new(0);
    pub static EXT4_MAP_HOLES: AtomicUsize = AtomicUsize::new(0);

    // ── P0: Ext4 Extent Tree Search ──
    pub static EXT4_FIND_EXTENT_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static EXT4_FIND_EXTENT_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static EXT4_FIND_EXTENT_DEPTH_SUM: AtomicUsize = AtomicUsize::new(0);
    pub static EXT4_FIND_EXTENT_META_READS: AtomicUsize = AtomicUsize::new(0);

    // ── P0: Ext4 PageCache Backend Batch ──
    pub static EXT4_PC_READPAGES_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static EXT4_PC_READPAGES_PAGES: AtomicUsize = AtomicUsize::new(0);
    pub static EXT4_PC_READPAGES_RUNS: AtomicUsize = AtomicUsize::new(0);
    pub static EXT4_PC_WRITEPAGES_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static EXT4_PC_WRITEPAGES_PAGES: AtomicUsize = AtomicUsize::new(0);
    pub static EXT4_PC_WRITEPAGES_RUNS: AtomicUsize = AtomicUsize::new(0);
    pub static EXT4_PC_512B_FALLBACK_PAGES: AtomicUsize = AtomicUsize::new(0);

    // ── P0: Ext4 Allocation ──
    pub static EXT4_ALLOC_ENSURE_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static EXT4_ALLOC_LBLOCKS: AtomicUsize = AtomicUsize::new(0);
    pub static EXT4_ALLOC_NEW_BLOCKS: AtomicUsize = AtomicUsize::new(0);
    pub static EXT4_ALLOC_CYCLES: AtomicUsize = AtomicUsize::new(0);

    // ── P0: PageCache Lock Contention ──
    pub static PC_LOCK_HOLD_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PC_LOCK_HOLD_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static PC_LOCK_IO_MISS_READS: AtomicUsize = AtomicUsize::new(0);

    // ── P0: Ext4 direct write_at ──
    pub static EXT4_DIRECT_WRITE_AT_CALLS: AtomicUsize = AtomicUsize::new(0);

    // ── P0: Heap Allocator Cost ──
    pub static HEAP_ALLOC_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static HEAP_ALLOC_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static HEAP_ALLOC_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static HEAP_DEALLOC_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static HEAP_DEALLOC_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static HEAP_DEALLOC_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static HEAP_DEALLOC_SCAN_STEPS_TOTAL: AtomicUsize = AtomicUsize::new(0);

    #[inline(always)]
    fn update_max(counter: &AtomicUsize, val: usize) {
        let mut cur = counter.load(Ordering::Relaxed);
        while val > cur {
            match counter.compare_exchange_weak(cur, val, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break,
                Err(v) => cur = v,
            }
        }
    }

    #[inline(always)]
    pub fn record_taskq_add_ready() { if !stats_enabled() { return; } ADD_READY_TOTAL.fetch_add(1, Ordering::Relaxed); }

    #[inline(always)]
    pub fn record_taskq_add_interruptible() { if !stats_enabled() { return; } ADD_INTERRUPTIBLE_TOTAL.fetch_add(1, Ordering::Relaxed); }

    #[inline(always)]
    pub fn record_taskq_wake_interruptible() { if !stats_enabled() { return; } WAKE_INTERRUPTIBLE_TOTAL.fetch_add(1, Ordering::Relaxed); }

    #[inline(always)]
    pub fn record_taskq_dup_enqueue() { if !stats_enabled() { return; } DUPLICATE_READY_ENQUEUE.fetch_add(1, Ordering::Relaxed); }

    #[inline(always)]
    pub fn record_taskq_fetch(fair_pick: bool, scan_depth: usize) {
        if !stats_enabled() { return; }
        if fair_pick {
            FAIR_PICK_CALLS.fetch_add(1, Ordering::Relaxed);
            update_max(&FAIR_SCAN_MAX, scan_depth);
        } else {
            FAST_PATH_CALLS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_taskq_queue_lens(ready: usize, interruptible: usize, ready_zombie: usize, int_zombie: usize, nonzero_nice: usize) {
        if !stats_enabled() { return; }
        update_max(&READY_LEN_MAX, ready);
        update_max(&INTERRUPTIBLE_LEN_MAX, interruptible);
        update_max(&READY_ZOMBIE_MAX, ready_zombie);
        update_max(&INTERRUPTIBLE_ZOMBIE_MAX, int_zombie);
        READY_NONZERO_NICE_CUR.store(nonzero_nice, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_zombie_drain_full(scan_total: usize, calls: usize, removed: usize) {
        if !stats_enabled() { return; }
        if scan_total != 0 { ZOMBIE_DRAIN_SCAN_TOTAL.fetch_add(scan_total, Ordering::Relaxed); }
        if calls != 0 { ZOMBIE_DRAIN_CALLS.fetch_add(calls, Ordering::Relaxed); }
        if removed != 0 { ZOMBIE_DRAIN_REMOVED.fetch_add(removed, Ordering::Relaxed); }
    }

    #[inline(always)]
    pub fn record_ktimer_add() { if !stats_enabled() { return; } KTIMER_ADD_TOTAL.fetch_add(1, Ordering::Relaxed); }

    #[inline(always)]
    pub fn record_ktimer_len(len: usize) { if !stats_enabled() { return; } update_max(&KTIMER_LEN_MAX, len); }

    #[inline(always)]
    pub fn record_ktimer_pop(pop_count: usize) {
        if !stats_enabled() { return; }
        KTIMER_POP_TOTAL.fetch_add(1, Ordering::Relaxed);
        update_max(&KTIMER_POP_MAX, pop_count);
    }

    #[inline(always)]
    pub fn record_ktimer_stale_waketask() { if !stats_enabled() { return; } KTIMER_STALE_WAKETASK.fetch_add(1, Ordering::Relaxed); }

    #[inline(always)]
    pub fn record_ktimer_real_wake() { if !stats_enabled() { return; } KTIMER_REAL_WAKE.fetch_add(1, Ordering::Relaxed); }

    #[inline(always)]
    pub fn record_ktimer_compact(stale_removed: usize) {
        if !stats_enabled() { return; }
        KTIMER_COMPACT_CALLS.fetch_add(1, Ordering::Relaxed);
        if stale_removed != 0 { KTIMER_STALE_REMOVED.fetch_add(stale_removed, Ordering::Relaxed); }
    }

    #[inline(always)]
    pub fn record_wait_with_timeout() { if !stats_enabled() { return; } WAIT_WITH_TIMEOUT_TOTAL.fetch_add(1, Ordering::Relaxed); }

    #[inline(always)]
    pub fn record_timer_irq_cost(start: usize) {
        if !stats_enabled() { return; }
        let elapsed = perf_time_now().wrapping_sub(start);
        TIMER_IRQ_TICKS_TOTAL.fetch_add(elapsed, Ordering::Relaxed);
        update_max(&TIMER_IRQ_TICKS_MAX, elapsed);
    }

    #[inline(always)]
    pub fn record_timer_pop_cost(start: usize, nodes_popped: usize) {
        if !stats_enabled() { return; }
        let elapsed = perf_time_now().wrapping_sub(start);
        TIMER_POP_NODES_TOTAL.fetch_add(nodes_popped, Ordering::Relaxed);
        TIMER_POP_TICKS_TOTAL.fetch_add(elapsed, Ordering::Relaxed);
        update_max(&TIMER_POP_TICKS_MAX, elapsed);
    }

    #[inline(always)]
    pub fn record_seccomp_check_call() {
        if !stats_enabled() { return; }
        SECCOMP_CHECK_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_seccomp_check(start: usize, bypass: bool) {
        if !stats_enabled() { return; }
        let elapsed = perf_time_now().wrapping_sub(start);
        SECCOMP_CHECK_TICKS_TOTAL.fetch_add(elapsed, Ordering::Relaxed);
        update_max(&SECCOMP_CHECK_TICKS_MAX, elapsed);
        if bypass {
            SECCOMP_DISABLED_BYPASS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_seccomp_disabled_bypass() {
        if !stats_enabled() { return; }
        SECCOMP_DISABLED_BYPASS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_syscall_enter(syscall_id: usize) {
        if !stats_enabled() { return; }
        SYSCALL_TOTAL.fetch_add(1, Ordering::Relaxed);
        if syscall_id == 173 { SYSCALL_GETPPID_TOTAL.fetch_add(1, Ordering::Relaxed); }
    }

    #[inline(always)]
    pub fn record_syscall_cost_ticks(ticks: usize) {
        if !stats_enabled() { return; }
        SYSCALL_COST_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
        update_max(&SYSCALL_COST_MAX_TICKS, ticks);
    }

    #[inline(always)]
    pub fn record_trap_cost_ticks(ticks: usize) {
        if !stats_enabled() { return; }
        ECALL_TRAP_COST_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
        update_max(&ECALL_TRAP_COST_TICKS_MAX, ticks);
        update_max(&TRAP_ENTER_COST_MAX_TICKS, ticks);
    }

    #[inline(always)]
    pub fn record_getppid_cost(ticks: usize) {
        if !stats_enabled() { return; }
        GETPPID_COST_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
        update_max(&GETPPID_COST_TICKS_MAX, ticks);
    }

    #[inline(always)]
    pub fn record_context_switch() {
        if !stats_enabled() { return; }
        CONTEXT_SWITCH_TOTAL.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_reclaim_run() {
        if !stats_enabled() { return; }
        RECLAIM_RUNS_TOTAL.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_reclaim_pages_scanned(n: usize) {
        if n == 0 { return; }
        if !stats_enabled() { return; }
        RECLAIM_PAGES_SCANNED_TOTAL.fetch_add(n, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_reclaim_pages_freed(n: usize) {
        if n == 0 { return; }
        if !stats_enabled() { return; }
        RECLAIM_PAGES_FREED_TOTAL.fetch_add(n, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_clock_scanned(n: usize) {
        if n == 0 { return; }
        if !stats_enabled() { return; }
        CLOCK_SCANNED.fetch_add(n, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_clock_second_chance(n: usize) {
        if n == 0 { return; }
        if !stats_enabled() { return; }
        CLOCK_SECOND_CHANCE.fetch_add(n, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_clock_evicted(n: usize) {
        if n == 0 { return; }
        if !stats_enabled() { return; }
        CLOCK_EVICTED.fetch_add(n, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_heap_alloc() {
        if !stats_enabled() { return; }
        HEAP_ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_heap_alloc_cost(ticks: usize) {
        if !stats_enabled() { return; }
        HEAP_ALLOC_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
        update_max(&HEAP_ALLOC_TICKS_MAX, ticks);
    }

    #[inline(always)]
    pub fn record_heap_dealloc() {
        if !stats_enabled() { return; }
        HEAP_DEALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_heap_dealloc_cost(ticks: usize) {
        if !stats_enabled() { return; }
        HEAP_DEALLOC_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
        update_max(&HEAP_DEALLOC_TICKS_MAX, ticks);
    }

    #[inline(always)]
    pub fn record_heap_dealloc_scan_steps(steps: usize) {
        if !stats_enabled() { return; }
        HEAP_DEALLOC_SCAN_STEPS_TOTAL.fetch_add(steps, Ordering::Relaxed);
    }

    // ── PageCache recorders ──

    #[inline(always)]
    pub fn record_pc_read(pages: usize, cycles: usize, hit_cycles: usize, miss_cycles: usize) {
        if !stats_enabled() { return; }
        PC_READ_CALLS.fetch_add(1, Ordering::Relaxed);
        PC_READ_PAGES.fetch_add(pages, Ordering::Relaxed);
        PC_READ_CYCLES_TOTAL.fetch_add(cycles, Ordering::Relaxed);
        PC_READ_HIT_CYCLES.fetch_add(hit_cycles, Ordering::Relaxed);
        PC_READ_MISS_CYCLES.fetch_add(miss_cycles, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_pc_copy_cycles(cycles: usize) {
        if !stats_enabled() { return; }
        PC_COPY_CYCLES.fetch_add(cycles, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_pc_lookup_cycles(cycles: usize) {
        if !stats_enabled() { return; }
        PC_LOOKUP_CYCLES.fetch_add(cycles, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_pc_write(pages: usize, full_overwrite: bool, cycles: usize) {
        if !stats_enabled() { return; }
        PC_WRITE_CALLS.fetch_add(1, Ordering::Relaxed);
        PC_WRITE_PAGES.fetch_add(pages, Ordering::Relaxed);
        if full_overwrite { PC_WRITE_OVERWRITE.fetch_add(1, Ordering::Relaxed); }
        PC_WRITE_CYCLES_TOTAL.fetch_add(cycles, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_pc_write_eventually_full() {
        if !stats_enabled() { return; }
        PC_WRITE_EVENTUALLY_FULL.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_pc_writeback(pages: usize, cycles: usize) {
        if !stats_enabled() { return; }
        PC_WRITEBACK_CALLS.fetch_add(1, Ordering::Relaxed);
        PC_WRITEBACK_PAGES.fetch_add(pages, Ordering::Relaxed);
        PC_WRITEBACK_CYCLES_TOTAL.fetch_add(cycles, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_pc_miss() {
        if !stats_enabled() { return; }
        PC_READ_MISS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_pc_falloc_cycles(cycles: usize) {
        if !stats_enabled() { return; }
        PC_FALLOC_CYCLES_TOTAL.fetch_add(cycles, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_wb_bg_call() {
        if !stats_enabled() { return; }
        WB_BG_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_wb_throttle_call() {
        if !stats_enabled() { return; }
        WB_THROTTLE_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_wb_redirty() {
        if !stats_enabled() { return; }
        WB_REDIRTY_PAGES.fetch_add(1, Ordering::Relaxed);
    }

    // ── Block device recorders ──

    #[inline(always)]
    pub fn record_blk_vread(sectors: usize) {
        if !stats_enabled() { return; }
        BLK_VREAD_REQS.fetch_add(1, Ordering::Relaxed);
        BLK_VREAD_SECS.fetch_add(sectors, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_blk_vwrite(sectors: usize) {
        if !stats_enabled() { return; }
        BLK_VWRITE_REQS.fetch_add(1, Ordering::Relaxed);
        BLK_VWRITE_SECS.fetch_add(sectors, Ordering::Relaxed);
    }

    // ── Ext4 Block Mapping recorders ──

    #[inline(always)]
    pub fn record_ext4_map_lblock() {
        if !stats_enabled() { return; }
        EXT4_MAP_LBLOCK_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ext4_map_lblock_cost(cycles: usize) {
        if !stats_enabled() { return; }
        EXT4_MAP_LBLOCK_CYCLES.fetch_add(cycles, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ext4_map_cache_hit() {
        if !stats_enabled() { return; }
        EXT4_MAP_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ext4_map_hole() {
        if !stats_enabled() { return; }
        EXT4_MAP_HOLES.fetch_add(1, Ordering::Relaxed);
    }

    // ── Ext4 Extent Tree Search recorders ──

    #[inline(always)]
    pub fn record_ext4_find_extent_call() {
        if !stats_enabled() { return; }
        EXT4_FIND_EXTENT_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ext4_find_extent_cost(cycles: usize, depth: usize, meta_reads: usize) {
        if !stats_enabled() { return; }
        EXT4_FIND_EXTENT_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        EXT4_FIND_EXTENT_DEPTH_SUM.fetch_add(depth, Ordering::Relaxed);
        EXT4_FIND_EXTENT_META_READS.fetch_add(meta_reads, Ordering::Relaxed);
    }

    // ── Ext4 PageCache Backend Batch recorders ──

    #[inline(always)]
    pub fn record_ext4_pc_readpages_calls() {
        if !stats_enabled() { return; }
        EXT4_PC_READPAGES_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ext4_pc_readpages_pages(n: usize) {
        if !stats_enabled() { return; }
        EXT4_PC_READPAGES_PAGES.fetch_add(n, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ext4_pc_readpages_runs(n: usize) {
        if !stats_enabled() { return; }
        EXT4_PC_READPAGES_RUNS.fetch_add(n, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ext4_pc_writepages_calls() {
        if !stats_enabled() { return; }
        EXT4_PC_WRITEPAGES_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ext4_pc_writepages_pages(n: usize) {
        if !stats_enabled() { return; }
        EXT4_PC_WRITEPAGES_PAGES.fetch_add(n, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ext4_pc_writepages_runs(n: usize) {
        if !stats_enabled() { return; }
        EXT4_PC_WRITEPAGES_RUNS.fetch_add(n, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ext4_pc_512b_fallback(n: usize) {
        if !stats_enabled() { return; }
        EXT4_PC_512B_FALLBACK_PAGES.fetch_add(n, Ordering::Relaxed);
    }

    // ── Ext4 Allocation recorders ──

    #[inline(always)]
    pub fn record_ext4_alloc_ensure_calls() {
        if !stats_enabled() { return; }
        EXT4_ALLOC_ENSURE_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ext4_alloc_ensure(lblocks: usize, new_blocks: usize, cycles: usize) {
        if !stats_enabled() { return; }
        EXT4_ALLOC_LBLOCKS.fetch_add(lblocks, Ordering::Relaxed);
        EXT4_ALLOC_NEW_BLOCKS.fetch_add(new_blocks, Ordering::Relaxed);
        EXT4_ALLOC_CYCLES.fetch_add(cycles, Ordering::Relaxed);
    }

    // ── PageCache Lock Contention recorders ──

    #[inline(always)]
    pub fn record_pc_lock_hold(cycles: usize, io_miss: bool) {
        if !stats_enabled() { return; }
        PC_LOCK_HOLD_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        update_max(&PC_LOCK_HOLD_MAX, cycles);
        if io_miss {
            PC_LOCK_IO_MISS_READS.fetch_add(1, Ordering::Relaxed);
        }
    }

    // ── Ext4 direct write_at recorder ──

    #[inline(always)]
    pub fn record_ext4_direct_write_at() {
        if !stats_enabled() { return; }
        EXT4_DIRECT_WRITE_AT_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    /// Reset all P0+P1 performance counters (writable via /sys/kernel/stats/reset).
    pub fn reset_all_counters() {
        // Scheduler / Task Queue
        FAIR_PICK_CALLS.store(0, Ordering::Relaxed);
        FAST_PATH_CALLS.store(0, Ordering::Relaxed);
        FAIR_SCAN_MAX.store(0, Ordering::Relaxed);
        DUPLICATE_READY_ENQUEUE.store(0, Ordering::Relaxed);
        ADD_READY_TOTAL.store(0, Ordering::Relaxed);
        ADD_INTERRUPTIBLE_TOTAL.store(0, Ordering::Relaxed);
        WAKE_INTERRUPTIBLE_TOTAL.store(0, Ordering::Relaxed);
        READY_LEN_MAX.store(0, Ordering::Relaxed);
        INTERRUPTIBLE_LEN_MAX.store(0, Ordering::Relaxed);
        READY_ZOMBIE_MAX.store(0, Ordering::Relaxed);
        INTERRUPTIBLE_ZOMBIE_MAX.store(0, Ordering::Relaxed);
        ZOMBIE_DRAIN_SCAN_TOTAL.store(0, Ordering::Relaxed);
        ZOMBIE_DRAIN_CALLS.store(0, Ordering::Relaxed);
        ZOMBIE_DRAIN_REMOVED.store(0, Ordering::Relaxed);
        READY_NONZERO_NICE_CUR.store(0, Ordering::Relaxed);
        // Kernel Timer
        KTIMER_LEN_MAX.store(0, Ordering::Relaxed);
        KTIMER_ADD_TOTAL.store(0, Ordering::Relaxed);
        KTIMER_POP_MAX.store(0, Ordering::Relaxed);
        KTIMER_POP_TOTAL.store(0, Ordering::Relaxed);
        KTIMER_STALE_WAKETASK.store(0, Ordering::Relaxed);
        KTIMER_REAL_WAKE.store(0, Ordering::Relaxed);
        KTIMER_COMPACT_CALLS.store(0, Ordering::Relaxed);
        KTIMER_STALE_REMOVED.store(0, Ordering::Relaxed);
        WAIT_WITH_TIMEOUT_TOTAL.store(0, Ordering::Relaxed);
        // Timer IRQ / Pop Cost
        TIMER_IRQ_TICKS_TOTAL.store(0, Ordering::Relaxed);
        TIMER_IRQ_TICKS_MAX.store(0, Ordering::Relaxed);
        TIMER_POP_NODES_TOTAL.store(0, Ordering::Relaxed);
        TIMER_POP_TICKS_TOTAL.store(0, Ordering::Relaxed);
        TIMER_POP_TICKS_MAX.store(0, Ordering::Relaxed);
        // Seccomp
        SECCOMP_CHECK_CALLS.store(0, Ordering::Relaxed);
        SECCOMP_CHECK_TICKS_TOTAL.store(0, Ordering::Relaxed);
        SECCOMP_CHECK_TICKS_MAX.store(0, Ordering::Relaxed);
        SECCOMP_DISABLED_BYPASS.store(0, Ordering::Relaxed);
        // Syscall / Trap (P0)
        SYSCALL_TOTAL.store(0, Ordering::Relaxed);
        SYSCALL_GETPPID_TOTAL.store(0, Ordering::Relaxed);
        SYSCALL_COST_MAX_TICKS.store(0, Ordering::Relaxed);
        TRAP_ENTER_COST_MAX_TICKS.store(0, Ordering::Relaxed);
        // Syscall Cost (P1)
        GETPPID_COST_TICKS_TOTAL.store(0, Ordering::Relaxed);
        GETPPID_COST_TICKS_MAX.store(0, Ordering::Relaxed);
        SYSCALL_COST_TICKS_TOTAL.store(0, Ordering::Relaxed);
        ECALL_TRAP_COST_TICKS_TOTAL.store(0, Ordering::Relaxed);
        ECALL_TRAP_COST_TICKS_MAX.store(0, Ordering::Relaxed);
        // Context Switch (P1)
        CONTEXT_SWITCH_TOTAL.store(0, Ordering::Relaxed);
        // Page Reclaim (P1)
        RECLAIM_RUNS_TOTAL.store(0, Ordering::Relaxed);
        RECLAIM_PAGES_SCANNED_TOTAL.store(0, Ordering::Relaxed);
        RECLAIM_PAGES_FREED_TOTAL.store(0, Ordering::Relaxed);
        // Clock Eviction (P1)
        CLOCK_SCANNED.store(0, Ordering::Relaxed);
        CLOCK_SECOND_CHANCE.store(0, Ordering::Relaxed);
        CLOCK_EVICTED.store(0, Ordering::Relaxed);
        // Heap Allocator Cost (P0)
        HEAP_ALLOC_CALLS.store(0, Ordering::Relaxed);
        HEAP_ALLOC_TICKS_TOTAL.store(0, Ordering::Relaxed);
        HEAP_ALLOC_TICKS_MAX.store(0, Ordering::Relaxed);
        HEAP_DEALLOC_CALLS.store(0, Ordering::Relaxed);
        HEAP_DEALLOC_TICKS_TOTAL.store(0, Ordering::Relaxed);
        HEAP_DEALLOC_TICKS_MAX.store(0, Ordering::Relaxed);
        HEAP_DEALLOC_SCAN_STEPS_TOTAL.store(0, Ordering::Relaxed);
        // PageCache I/O (P0)
        PC_READ_CALLS.store(0, Ordering::Relaxed);
        PC_READ_PAGES.store(0, Ordering::Relaxed);
        PC_READ_MISS.store(0, Ordering::Relaxed);
        PC_READ_CYCLES_TOTAL.store(0, Ordering::Relaxed);
        PC_READ_HIT_CYCLES.store(0, Ordering::Relaxed);
        PC_READ_MISS_CYCLES.store(0, Ordering::Relaxed);
        PC_COPY_CYCLES.store(0, Ordering::Relaxed);
        PC_LOOKUP_CYCLES.store(0, Ordering::Relaxed);
        PC_WRITE_CALLS.store(0, Ordering::Relaxed);
        PC_WRITE_PAGES.store(0, Ordering::Relaxed);
        PC_WRITE_OVERWRITE.store(0, Ordering::Relaxed);
        PC_WRITE_EVENTUALLY_FULL.store(0, Ordering::Relaxed);
        PC_WRITE_CYCLES_TOTAL.store(0, Ordering::Relaxed);
        PC_WRITEBACK_CALLS.store(0, Ordering::Relaxed);
        PC_WRITEBACK_PAGES.store(0, Ordering::Relaxed);
        PC_WRITEBACK_CYCLES_TOTAL.store(0, Ordering::Relaxed);
        PC_FALLOC_CYCLES_TOTAL.store(0, Ordering::Relaxed);
        // Writeback Throttling (P0)
        WB_BG_CALLS.store(0, Ordering::Relaxed);
        WB_THROTTLE_CALLS.store(0, Ordering::Relaxed);
        WB_REDIRTY_PAGES.store(0, Ordering::Relaxed);
        // Block Device I/O (P0)
        BLK_VREAD_REQS.store(0, Ordering::Relaxed);
        BLK_VREAD_SECS.store(0, Ordering::Relaxed);
        BLK_VWRITE_REQS.store(0, Ordering::Relaxed);
        BLK_VWRITE_SECS.store(0, Ordering::Relaxed);
        // Ext4 Block Mapping (P0)
        EXT4_MAP_LBLOCK_CALLS.store(0, Ordering::Relaxed);
        EXT4_MAP_LBLOCK_CYCLES.store(0, Ordering::Relaxed);
        EXT4_MAP_CACHE_HITS.store(0, Ordering::Relaxed);
        EXT4_MAP_HOLES.store(0, Ordering::Relaxed);
        // Ext4 Extent Tree Search (P0)
        EXT4_FIND_EXTENT_CALLS.store(0, Ordering::Relaxed);
        EXT4_FIND_EXTENT_CYCLES.store(0, Ordering::Relaxed);
        EXT4_FIND_EXTENT_DEPTH_SUM.store(0, Ordering::Relaxed);
        EXT4_FIND_EXTENT_META_READS.store(0, Ordering::Relaxed);
        // Ext4 PageCache Backend Batch (P0)
        EXT4_PC_READPAGES_CALLS.store(0, Ordering::Relaxed);
        EXT4_PC_READPAGES_PAGES.store(0, Ordering::Relaxed);
        EXT4_PC_READPAGES_RUNS.store(0, Ordering::Relaxed);
        EXT4_PC_WRITEPAGES_CALLS.store(0, Ordering::Relaxed);
        EXT4_PC_WRITEPAGES_PAGES.store(0, Ordering::Relaxed);
        EXT4_PC_WRITEPAGES_RUNS.store(0, Ordering::Relaxed);
        EXT4_PC_512B_FALLBACK_PAGES.store(0, Ordering::Relaxed);
        // Ext4 Allocation (P0)
        EXT4_ALLOC_ENSURE_CALLS.store(0, Ordering::Relaxed);
        EXT4_ALLOC_LBLOCKS.store(0, Ordering::Relaxed);
        EXT4_ALLOC_NEW_BLOCKS.store(0, Ordering::Relaxed);
        EXT4_ALLOC_CYCLES.store(0, Ordering::Relaxed);
        // PageCache Lock Contention (P0)
        PC_LOCK_HOLD_CYCLES.store(0, Ordering::Relaxed);
        PC_LOCK_HOLD_MAX.store(0, Ordering::Relaxed);
        PC_LOCK_IO_MISS_READS.store(0, Ordering::Relaxed);
        // Ext4 direct write_at (P0)
        EXT4_DIRECT_WRITE_AT_CALLS.store(0, Ordering::Relaxed);

        // ── Per-syscall profiling ──
        for i in 0..PERF_SYSCOUNT {
            SYSCALL_COUNT[i].store(0, Ordering::Relaxed);
            SYSCALL_TICKS[i].store(0, Ordering::Relaxed);
        }

        // ── Page fault per-action ──
        for i in 0..7 {
            PF_ACTION_COUNT[i].store(0, Ordering::Relaxed);
            PF_ACTION_TICKS[i].store(0, Ordering::Relaxed);
        }

        // ── Filemap fault phase ──
        FILEMAP_FAULT_FRAMES.store(0, Ordering::Relaxed);
        FILEMAP_FAULT_TICKS.store(0, Ordering::Relaxed);
        FILEMAP_PRIVATE_COPY_TICKS.store(0, Ordering::Relaxed);
        FILEMAP_MAP_USER_TICKS.store(0, Ordering::Relaxed);

        // ── TLB flush cycles ──
        TLB_PAGE_FLUSH_CYCLES.store(0, Ordering::Relaxed);
        TLB_FULL_FLUSH_CYCLES.store(0, Ordering::Relaxed);
        TLB_ACTIVATE_CYCLES.store(0, Ordering::Relaxed);

        // ── Execve phase cycles ──
        EXECVE_MAP_ELF_TICKS.store(0, Ordering::Relaxed);
        EXECVE_KERNEL_MAP_TICKS.store(0, Ordering::Relaxed);
        EXECVE_INTERP_TICKS.store(0, Ordering::Relaxed);
        EXECVE_STACK_TABLES_TICKS.store(0, Ordering::Relaxed);
        EXECVE_TEARDOWN_TICKS.store(0, Ordering::Relaxed);
    }

    /// Print accumulated timing stats, then reset.
    pub fn perf_dump_timings(label: &str) {
        let freq = crate::hal::get_clock_freq();
        let to_ms = |ticks: usize| -> usize {
            if freq > 0 { ticks.saturating_mul(1000) / freq } else { 0 }
        };
        let c_ms = to_ms(CLONE_TIME_TICKS.swap(0, Ordering::Relaxed));
        let c_n = CLONE_TIME_COUNT.swap(0, Ordering::Relaxed);
        let p_ms = to_ms(PAGEFAULT_TIME_TICKS.swap(0, Ordering::Relaxed));
        let p_n = PAGEFAULT_TIME_COUNT.swap(0, Ordering::Relaxed);
        let f_ms = to_ms(FRAME_ALLOC_TIME_TICKS.swap(0, Ordering::Relaxed));
        let f_n = FRAME_ALLOC_TIME_COUNT.swap(0, Ordering::Relaxed);
        let v_ms = to_ms(VFS_LOOKUP_TIME_TICKS.swap(0, Ordering::Relaxed));
        let v_n = VFS_LOOKUP_TIME_COUNT.swap(0, Ordering::Relaxed);
        let v_total = VFS_LOOKUPS.swap(0, Ordering::Relaxed);
        crate::println!(
            "[timing] {} clone={}ms/{}calls avg={}us pf={}ms/{}calls avg={}us falloc={}ms/{}calls avg={}us vfs={}total/{}timed avg={}us",
            label,
            c_ms, c_n, if c_n > 0 { c_ms.saturating_mul(1000) / c_n } else { 0 },
            p_ms, p_n, if p_n > 0 { p_ms.saturating_mul(1000) / p_n } else { 0 },
            f_ms, f_n, if f_n > 0 { f_ms.saturating_mul(1000) / f_n } else { 0 },
            v_total, v_n, if v_n > 0 { v_ms.saturating_mul(1000) / v_n } else { 0 },
        );
    }

    #[inline(always)]
    pub fn record_clone_time_us(ticks: usize) {
        CLONE_TIME_TICKS.fetch_add(ticks, Ordering::Relaxed);
        CLONE_TIME_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_pagefault_time_us(ticks: usize) {
        PAGEFAULT_TIME_TICKS.fetch_add(ticks, Ordering::Relaxed);
        PAGEFAULT_TIME_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_frame_alloc_time_us(ticks: usize) {
        FRAME_ALLOC_TIME_TICKS.fetch_add(ticks, Ordering::Relaxed);
        FRAME_ALLOC_TIME_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    /// Read the RISC-V cycle CSR (or la64 rdtime) — single instruction, zero overhead.
    pub fn perf_time_now() -> usize {
        #[cfg(target_arch = "riscv64")]
        {
            let cycles: usize;
            // Safety: `rdcycle` 只读取当前 hart 的 cycle CSR，不访问内存，
            // 不修改控制寄存器，也不依赖栈或 ABI 外状态。
            unsafe { core::arch::asm!("rdcycle {}", out(reg) cycles) };
            cycles
        }
        #[cfg(target_arch = "loongarch64")]
        {
            let mut lo: usize; let mut hi: usize;
            // Safety: `rdtime.d` 只读取稳定计时器到通用寄存器，不访问内存；
            // 两个输出寄存器均由 asm! 约束分配。
            unsafe { core::arch::asm!("rdtime.d {},{}", out(reg) lo, out(reg) hi) };
            lo
        }
        #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
        { 0 }
    }

    fn load(counter: &AtomicUsize) -> usize {
        counter.load(Ordering::Relaxed)
    }

    fn print_snapshot(reason: &str) {
        let (fr_total, fr_fresh, fr_recycled, fr_ratio) = crate::mm::frame_frag_diag();
        crate::println!(
            "[perf] {} clone={} thread={} share_vm={} stack_alloc={} exit={} clear_tid={} keep_trap={} trap_store={} trap_skip={} trap_hit={} trap_miss={} kstack_hit={} kstack_miss={} kstack_store={} kstack_drop={} zombie_enq={} zombie_drain={} sched={} fetch={} idle={} timer={} fut_wait={} fut_wait_shared={} fut_wait_deadline={} fut_ready={} fut_timeout={} fut_intr={} fut_wake={} fut_wake_shared={} fut_wake_hit={} sc_clone={} sc_futex={} sc_mmap={} sc_munmap={} sc_mprotect={} sc_settid={} sc_robust={} sc_exit={} sc_yield={} tlb={} tlb_full={} tlb_page={} tlb_act={} tlb_glob={} frame_alloc={} frame_free={} pgfault={} vfs={} fr_total={} fr_fresh={} fr_recycled={} fr_ratio={:.4} last_sys={} last_ret={}",
            reason,
            load(&CLONE_TOTAL),
            load(&CLONE_THREAD),
            load(&CLONE_SHARE_VM),
            load(&CLONE_STACK_ALLOC),
            load(&EXIT_THREAD),
            load(&EXIT_CLEAR_CHILD_TID),
            load(&EXIT_KEEP_TRAP),
            load(&TRAP_CACHE_STORE),
            load(&TRAP_CACHE_SKIP),
            load(&TRAP_CACHE_HIT),
            load(&TRAP_CACHE_MISS),
            load(&KSTACK_CACHE_HIT),
            load(&KSTACK_CACHE_MISS),
            load(&KSTACK_CACHE_STORE),
            load(&KSTACK_CACHE_DROP),
            load(&ZOMBIE_ENQUEUE),
            load(&ZOMBIE_DRAIN),
            load(&SCHEDULE_LOOPS),
            load(&SCHEDULE_FETCH),
            load(&SCHEDULE_IDLE),
            load(&TIMER_INTERRUPTS),
            load(&FUTEX_WAIT),
            load(&FUTEX_WAIT_SHARED),
            load(&FUTEX_WAIT_DEADLINE),
            load(&FUTEX_WAIT_READY),
            load(&FUTEX_WAIT_TIMEOUT),
            load(&FUTEX_WAIT_INTR),
            load(&FUTEX_WAKE),
            load(&FUTEX_WAKE_SHARED),
            load(&FUTEX_WAKE_HIT),
            load(&SYSCALL_CLONE),
            load(&SYSCALL_FUTEX),
            load(&SYSCALL_MMAP),
            load(&SYSCALL_MUNMAP),
            load(&SYSCALL_MPROTECT),
            load(&SYSCALL_SET_TID_ADDRESS),
            load(&SYSCALL_SET_ROBUST_LIST),
            load(&SYSCALL_EXIT),
            load(&SYSCALL_YIELD),
            load(&TLB_FLUSHES),
            load(&TLB_FULL),
            load(&TLB_PAGE),
            load(&TLB_ACTIVATE),
            load(&TLB_GLOBAL),
            load(&FRAME_ALLOC_HITS),
            load(&FRAME_FREE_HITS),
            load(&PAGE_FAULTS),
            load(&VFS_LOOKUPS),
            fr_total,
            fr_fresh,
            fr_recycled,
            fr_ratio,
            load(&LAST_SYSCALL_ID),
            load(&LAST_SYSCALL_RET),
        );
        // VFS find diagnostics
        let (f_calls, f_ticks, f_overlay, f_hit, f_miss, f_lock, f_inner, f_insert, f_ov_ticks) = crate::fs::vfs::mount::counters::find_snapshot();
        let freq = crate::hal::get_clock_freq();
        let us = |t: usize| -> usize { if freq > 0 { t.saturating_mul(1000000) / freq } else { 0 } };
        let f_us = if f_calls > 0 { us(f_ticks) / f_calls } else { 0 };
        let lock_us = if f_calls > 0 { us(f_lock) / f_calls } else { 0 };
        let inner_us = if f_miss > 0 { us(f_inner) / f_miss } else { 0 };
        let insert_us = if f_miss > 0 { us(f_insert) / f_miss } else { 0 };
        crate::println!("[vfs-find] {} calls={} hit={} miss={} avg={}us lock={}us inner={}us insert={}us",
            reason, f_calls, f_hit, f_miss, f_us, lock_us, inner_us, insert_us);

        // lwext4 metadata diagnostics
        let lw = crate::fs::ext4_lwext4::counters::snapshot();
        crate::println!("[lwext4] find={} find_cycles={} probe_type={} pt_cycles={} get_inode_id={} gii_enoent={} gii_cycles={} meta_cold={} meta_hot={} meta_cold_cycles={} file_open={} fo_cycles={} file_size={} file_close={} fc_cycles={} dirent={} de_cycles={} create_pre={} logical_size={} ls_cycles={} ensure_pc={} cache_hit={} cache_miss={} pc_creates={}",
            lw.0, lw.1, lw.2, lw.3, lw.4, lw.5, lw.6, lw.7, lw.8, lw.9,
            lw.10, lw.11, lw.12, lw.13, lw.14, lw.15, lw.16, lw.17, lw.18, lw.19, lw.20,
            lw.21, lw.22, lw.23);

        // mount/bind diagnostics
        let mnt = crate::fs::vfs::mount::counters::mount_perf_snapshot();
        crate::println!("[mount_perf] propagate={} prop_cycles={} remove_fs={} rf_scan={} rbind={} rbind_cycles={} rbind_entries={} dirent_calls={} seen_scan={}",
            mnt.0, mnt.1, mnt.2, mnt.3, mnt.4, mnt.5, mnt.6, mnt.7, mnt.8);
    }

    /// Unconditionally print a perf snapshot (for group-boundary instrumentation).
    pub fn perf_snapshot(reason: &str) {
        print_snapshot(reason);
    }

    #[inline(always)]
    pub fn record_tlb_full() {
        TLB_FLUSHES.fetch_add(1, Ordering::Relaxed);
        TLB_FULL.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_tlb_page() {
        TLB_FLUSHES.fetch_add(1, Ordering::Relaxed);
        TLB_PAGE.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_tlb_activate() {
        // NOTE: only increments the category counter, not TLB_FLUSHES.
        // The underlying tlb_invalidate()/sfence.vma already accounts for the total.
        TLB_ACTIVATE.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_tlb_global() {
        TLB_FLUSHES.fetch_add(1, Ordering::Relaxed);
        TLB_GLOBAL.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_page_fault() {
        PAGE_FAULTS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_vfs_lookup() {
        VFS_LOOKUPS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_vfs_lookup_time_us(ticks: usize) {
        VFS_LOOKUP_TIME_TICKS.fetch_add(ticks, Ordering::Relaxed);
        VFS_LOOKUP_TIME_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_frame_alloc() {
        FRAME_ALLOC_HITS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_frame_free() {
        FRAME_FREE_HITS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_clone(is_thread: bool, share_vm: bool, stack_allocated: bool) {
        let n = CLONE_TOTAL.fetch_add(1, Ordering::Relaxed) + 1;
        if is_thread {
            CLONE_THREAD.fetch_add(1, Ordering::Relaxed);
        }
        if share_vm {
            CLONE_SHARE_VM.fetch_add(1, Ordering::Relaxed);
        }
        if stack_allocated {
            CLONE_STACK_ALLOC.fetch_add(1, Ordering::Relaxed);
        }
        if n % PRINT_EVERY_CLONES == 0 {
            print_snapshot("clone");
        }
    }

    #[inline(always)]
    pub fn record_exit_thread(clear_child_tid: bool, keep_trap_context: bool) {
        let n = EXIT_THREAD.fetch_add(1, Ordering::Relaxed) + 1;
        if clear_child_tid {
            EXIT_CLEAR_CHILD_TID.fetch_add(1, Ordering::Relaxed);
        }
        if keep_trap_context {
            EXIT_KEEP_TRAP.fetch_add(1, Ordering::Relaxed);
        }
        if n % PRINT_EVERY_EXITS == 0 {
            print_snapshot("exit");
        }
    }

    #[inline(always)]
    pub fn record_trap_cache_store(stored: bool) {
        if stored {
            TRAP_CACHE_STORE.fetch_add(1, Ordering::Relaxed);
        } else {
            TRAP_CACHE_SKIP.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_trap_cache_take(hit: bool) {
        if hit {
            TRAP_CACHE_HIT.fetch_add(1, Ordering::Relaxed);
        } else {
            TRAP_CACHE_MISS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_kstack_alloc(hit: bool) {
        if hit {
            KSTACK_CACHE_HIT.fetch_add(1, Ordering::Relaxed);
        } else {
            KSTACK_CACHE_MISS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_kstack_drop(cached: bool) {
        if cached {
            KSTACK_CACHE_STORE.fetch_add(1, Ordering::Relaxed);
        } else {
            KSTACK_CACHE_DROP.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_zombie_enqueue() {
        ZOMBIE_ENQUEUE.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_zombie_drain(count: usize) {
        if count != 0 {
            ZOMBIE_DRAIN.fetch_add(count, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_schedule_loop(fetched: bool) {
        let n = SCHEDULE_LOOPS.fetch_add(1, Ordering::Relaxed) + 1;
        if fetched {
            SCHEDULE_FETCH.fetch_add(1, Ordering::Relaxed);
        } else {
            SCHEDULE_IDLE.fetch_add(1, Ordering::Relaxed);
        }
        if load(&CLONE_TOTAL) >= 4500 && n % PRINT_EVERY_SCHEDULES == 0 {
            print_snapshot("sched");
        }
    }

    #[inline(always)]
    pub fn record_timer_interrupt() {
        let n = TIMER_INTERRUPTS.fetch_add(1, Ordering::Relaxed) + 1;
        if load(&CLONE_TOTAL) >= 4500 && n % 1024 == 0 {
            print_snapshot("timer");
        }
    }

    #[inline(always)]
    pub fn record_futex_wait(shared: bool, has_deadline: bool) {
        let n = FUTEX_WAIT.fetch_add(1, Ordering::Relaxed) + 1;
        if shared {
            FUTEX_WAIT_SHARED.fetch_add(1, Ordering::Relaxed);
        }
        if has_deadline {
            FUTEX_WAIT_DEADLINE.fetch_add(1, Ordering::Relaxed);
        }
        if load(&CLONE_TOTAL) >= 4500 && n % PRINT_EVERY_FUTEX == 0 {
            print_snapshot("futex-wait");
        }
    }

    #[inline(always)]
    pub fn record_futex_wait_result(result: WaitResult) {
        match result {
            WaitResult::Ready(_) => {
                FUTEX_WAIT_READY.fetch_add(1, Ordering::Relaxed);
            }
            WaitResult::TimedOut => {
                FUTEX_WAIT_TIMEOUT.fetch_add(1, Ordering::Relaxed);
            }
            WaitResult::Interrupted => {
                FUTEX_WAIT_INTR.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    #[inline(always)]
    pub fn record_futex_wake(shared: bool, woke: isize) {
        let n = FUTEX_WAKE.fetch_add(1, Ordering::Relaxed) + 1;
        if shared {
            FUTEX_WAKE_SHARED.fetch_add(1, Ordering::Relaxed);
        }
        if woke > 0 {
            FUTEX_WAKE_HIT.fetch_add(1, Ordering::Relaxed);
        }
        if load(&CLONE_TOTAL) >= 4500 && n % PRINT_EVERY_FUTEX == 0 {
            print_snapshot("futex-wake");
        }
    }

    // ── Per-syscall time recorder ──
    #[inline(always)]
    pub fn record_syscall_time(syscall_id: usize, elapsed: usize) {
        if !stats_enabled() { return; }
        if syscall_id < PERF_SYSCOUNT {
            SYSCALL_COUNT[syscall_id].fetch_add(1, Ordering::Relaxed);
            SYSCALL_TICKS[syscall_id].fetch_add(elapsed, Ordering::Relaxed);
        }
    }

    /// Read syscall count by ID (0 if out of range).
    pub fn syscall_count(id: usize) -> usize {
        if id < PERF_SYSCOUNT { SYSCALL_COUNT[id].load(Ordering::Relaxed) } else { 0 }
    }

    /// Read syscall total ticks by ID (0 if out of range).
    pub fn syscall_ticks(id: usize) -> usize {
        if id < PERF_SYSCOUNT { SYSCALL_TICKS[id].load(Ordering::Relaxed) } else { 0 }
    }

    // ── Page fault per-action recorders ──
    #[inline(always)]
    pub fn record_pagefault_action(action_tag: usize, elapsed: usize) {
        if !stats_enabled() { return; }
        if action_tag < 7 {
            PF_ACTION_COUNT[action_tag].fetch_add(1, Ordering::Relaxed);
            PF_ACTION_TICKS[action_tag].fetch_add(elapsed, Ordering::Relaxed);
        }
    }

    pub fn pf_action_count(action_tag: usize) -> usize {
        if action_tag < 7 { PF_ACTION_COUNT[action_tag].load(Ordering::Relaxed) } else { 0 }
    }

    pub fn pf_action_ticks(action_tag: usize) -> usize {
        if action_tag < 7 { PF_ACTION_TICKS[action_tag].load(Ordering::Relaxed) } else { 0 }
    }

    // ── TLB flush cycle recorders ──
    #[inline(always)]
    pub fn record_tlb_page_flush_cycles(cycles: usize) {
        if !stats_enabled() { return; }
        TLB_PAGE_FLUSH_CYCLES.fetch_add(cycles, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_tlb_full_flush_cycles(cycles: usize) {
        if !stats_enabled() { return; }
        TLB_FULL_FLUSH_CYCLES.fetch_add(cycles, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_tlb_activate_cycles(cycles: usize) {
        if !stats_enabled() { return; }
        TLB_ACTIVATE_CYCLES.fetch_add(cycles, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_syscall(syscall_id: usize, ret: isize) {
        LAST_SYSCALL_ID.store(syscall_id, Ordering::Relaxed);
        LAST_SYSCALL_RET.store(ret as usize, Ordering::Relaxed);
        match syscall_id {
            96 => {
                SYSCALL_SET_TID_ADDRESS.fetch_add(1, Ordering::Relaxed);
            }
            98 => {
                SYSCALL_FUTEX.fetch_add(1, Ordering::Relaxed);
            }
            99 => {
                SYSCALL_SET_ROBUST_LIST.fetch_add(1, Ordering::Relaxed);
            }
            93 | 94 => {
                SYSCALL_EXIT.fetch_add(1, Ordering::Relaxed);
            }
            124 => {
                SYSCALL_YIELD.fetch_add(1, Ordering::Relaxed);
            }
            215 => {
                SYSCALL_MUNMAP.fetch_add(1, Ordering::Relaxed);
            }
            220 => {
                SYSCALL_CLONE.fetch_add(1, Ordering::Relaxed);
            }
            222 => {
                SYSCALL_MMAP.fetch_add(1, Ordering::Relaxed);
            }
            226 => {
                SYSCALL_MPROTECT.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }
}

#[cfg(feature = "perf_stats")]
pub use enabled::*;

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_clone(_is_thread: bool, _share_vm: bool, _stack_allocated: bool) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_exit_thread(_clear_child_tid: bool, _keep_trap_context: bool) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_trap_cache_store(_stored: bool) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_trap_cache_take(_hit: bool) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_kstack_alloc(_hit: bool) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_kstack_drop(_cached: bool) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_zombie_enqueue() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_zombie_drain(_count: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_schedule_loop(_fetched: bool) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_timer_interrupt() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_futex_wait(_shared: bool, _has_deadline: bool) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_futex_wait_result(_result: crate::task::WaitResult) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_futex_wake(_shared: bool, _woke: isize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_syscall(_syscall_id: usize, _ret: isize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_syscall_time(_syscall_id: usize, _elapsed: usize) {}

#[cfg(not(feature = "perf_stats"))]
pub fn syscall_count(_id: usize) -> usize { 0 }

#[cfg(not(feature = "perf_stats"))]
pub fn syscall_ticks(_id: usize) -> usize { 0 }

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pagefault_action(_action_tag: usize, _elapsed: usize) {}

#[cfg(not(feature = "perf_stats"))]
pub fn pf_action_count(_action_tag: usize) -> usize { 0 }

#[cfg(not(feature = "perf_stats"))]
pub fn pf_action_ticks(_action_tag: usize) -> usize { 0 }

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_tlb_page_flush_cycles(_cycles: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_tlb_full_flush_cycles(_cycles: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_tlb_activate_cycles(_cycles: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn perf_snapshot(_reason: &str) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_tlb_full() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_tlb_page() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_tlb_activate() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_tlb_global() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_tlb_flush() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_frame_alloc() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_frame_free() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_page_fault() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_vfs_lookup() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_vfs_lookup_time_us(_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn perf_dump_timings(_label: &str) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_clone_time_us(_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pagefault_time_us(_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_frame_alloc_time_us(_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn perf_time_now() -> usize { 0 }

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_taskq_add_ready() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_taskq_add_interruptible() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_taskq_wake_interruptible() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_taskq_dup_enqueue() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_taskq_fetch(_fair_pick: bool, _scan_depth: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_taskq_queue_lens(_ready: usize, _interruptible: usize, _ready_zombie: usize, _int_zombie: usize, _nonzero_nice: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_zombie_drain_full(_scan_total: usize, _calls: usize, _removed: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_ktimer_add() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_ktimer_len(_len: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_ktimer_pop(_pop_count: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_ktimer_stale_waketask() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_ktimer_real_wake() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_ktimer_compact(_stale_removed: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_wait_with_timeout() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_timer_irq_cost(_start: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_timer_pop_cost(_start: usize, _nodes_popped: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_seccomp_check_call() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_seccomp_check(_start: usize, _bypass: bool) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_seccomp_disabled_bypass() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_syscall_enter(_syscall_id: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_syscall_cost_ticks(_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_trap_cost_ticks(_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_getppid_cost(_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_context_switch() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_reclaim_run() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_reclaim_pages_scanned(_n: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_reclaim_pages_freed(_n: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_clock_scanned(_n: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_clock_second_chance(_n: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_clock_evicted(_n: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_heap_alloc() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_heap_alloc_cost(_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_heap_dealloc() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_heap_dealloc_cost(_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_heap_dealloc_scan_steps(_steps: usize) {}

// ── PageCache recorders (no-op when perf_stats disabled) ──
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pc_read(_pages: usize, _cycles: usize, _hit_cycles: usize, _miss_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pc_copy_cycles(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pc_lookup_cycles(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pc_write(_pages: usize, _full_overwrite: bool, _cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pc_write_eventually_full() {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pc_writeback(_pages: usize, _cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pc_miss() {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pc_falloc_cycles(_cycles: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_wb_bg_call() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_wb_throttle_call() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_wb_redirty() {}

// ── Block device recorders (no-op when perf_stats disabled) ──
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_blk_vread(_sectors: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_blk_vwrite(_sectors: usize) {}

// ── Ext4/P0 recorders (no-op when perf_stats disabled) ──
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_ext4_map_lblock() {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_ext4_map_lblock_cost(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_ext4_map_cache_hit() {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_ext4_map_hole() {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_ext4_find_extent_call() {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_ext4_find_extent_cost(_cycles: usize, _depth: usize, _meta_reads: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_ext4_pc_readpages_calls() {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_ext4_pc_readpages_pages(_n: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_ext4_pc_readpages_runs(_n: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_ext4_pc_writepages_calls() {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_ext4_pc_writepages_pages(_n: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_ext4_pc_writepages_runs(_n: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_ext4_pc_512b_fallback(_n: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_ext4_alloc_ensure_calls() {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_ext4_alloc_ensure(_lblocks: usize, _new_blocks: usize, _cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pc_lock_hold(_cycles: usize, _io_miss: bool) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_ext4_direct_write_at() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn reset_all_counters() {}

// ── P0 counter stubs (zero-valued when perf_stats disabled) ──

#[cfg(not(feature = "perf_stats"))]
pub static FAIR_PICK_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static FAST_PATH_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static FAIR_SCAN_MAX: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static DUPLICATE_READY_ENQUEUE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ADD_READY_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ADD_INTERRUPTIBLE_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WAKE_INTERRUPTIBLE_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static READY_LEN_MAX: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static INTERRUPTIBLE_LEN_MAX: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static READY_ZOMBIE_MAX: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static INTERRUPTIBLE_ZOMBIE_MAX: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ZOMBIE_DRAIN_SCAN_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ZOMBIE_DRAIN_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ZOMBIE_DRAIN_REMOVED: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static READY_NONZERO_NICE_CUR: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

#[cfg(not(feature = "perf_stats"))]
pub static KTIMER_LEN_MAX: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static KTIMER_ADD_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static KTIMER_POP_MAX: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static KTIMER_POP_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static KTIMER_STALE_WAKETASK: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static KTIMER_REAL_WAKE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static KTIMER_COMPACT_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static KTIMER_STALE_REMOVED: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WAIT_WITH_TIMEOUT_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

#[cfg(not(feature = "perf_stats"))]
pub static SYSCALL_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static SYSCALL_GETPPID_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static SYSCALL_COST_MAX_TICKS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static TRAP_ENTER_COST_MAX_TICKS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

#[cfg(not(feature = "perf_stats"))]
pub static GETPPID_COST_TICKS_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static GETPPID_COST_TICKS_MAX: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static SYSCALL_COST_TICKS_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ECALL_TRAP_COST_TICKS_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ECALL_TRAP_COST_TICKS_MAX: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static CONTEXT_SWITCH_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static RECLAIM_RUNS_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static RECLAIM_PAGES_SCANNED_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static RECLAIM_PAGES_FREED_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ── Clock Eviction stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static CLOCK_SCANNED: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static CLOCK_SECOND_CHANCE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static CLOCK_EVICTED: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ── Timer IRQ / Pop Cost stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static TIMER_IRQ_TICKS_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static TIMER_IRQ_TICKS_MAX: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static TIMER_POP_NODES_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static TIMER_POP_TICKS_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static TIMER_POP_TICKS_MAX: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ── Seccomp stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static SECCOMP_CHECK_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static SECCOMP_CHECK_TICKS_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static SECCOMP_CHECK_TICKS_MAX: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static SECCOMP_DISABLED_BYPASS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ── Heap Allocator Cost stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static HEAP_ALLOC_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static HEAP_ALLOC_TICKS_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static HEAP_ALLOC_TICKS_MAX: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static HEAP_DEALLOC_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static HEAP_DEALLOC_TICKS_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static HEAP_DEALLOC_TICKS_MAX: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static HEAP_DEALLOC_SCAN_STEPS_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ── PageCache I/O stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static PC_READ_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_READ_PAGES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_READ_MISS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_READ_CYCLES_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_READ_HIT_CYCLES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_READ_MISS_CYCLES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_COPY_CYCLES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_LOOKUP_CYCLES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_WRITE_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_WRITE_PAGES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_WRITE_OVERWRITE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_WRITE_EVENTUALLY_FULL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_WRITE_CYCLES_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_WRITEBACK_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_WRITEBACK_PAGES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_WRITEBACK_CYCLES_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_FALLOC_CYCLES_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ── Writeback Throttling stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static WB_BG_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_THROTTLE_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_REDIRTY_PAGES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ── Block Device I/O stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static BLK_VREAD_REQS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static BLK_VREAD_SECS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static BLK_VWRITE_REQS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static BLK_VWRITE_SECS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ── Ext4 Block Mapping stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_MAP_LBLOCK_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_MAP_LBLOCK_CYCLES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_MAP_CACHE_HITS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_MAP_HOLES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ── Ext4 Extent Tree Search stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_FIND_EXTENT_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_FIND_EXTENT_CYCLES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_FIND_EXTENT_DEPTH_SUM: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_FIND_EXTENT_META_READS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ── Ext4 PageCache Backend Batch stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_PC_READPAGES_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_PC_READPAGES_PAGES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_PC_READPAGES_RUNS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_PC_WRITEPAGES_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_PC_WRITEPAGES_PAGES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_PC_WRITEPAGES_RUNS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_PC_512B_FALLBACK_PAGES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ── Ext4 Allocation stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_ALLOC_ENSURE_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_ALLOC_LBLOCKS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_ALLOC_NEW_BLOCKS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_ALLOC_CYCLES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ── PageCache Lock Contention stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static PC_LOCK_HOLD_CYCLES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_LOCK_HOLD_MAX: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_LOCK_IO_MISS_READS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ── Ext4 direct write_at stub ──
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_DIRECT_WRITE_AT_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ── TLB counter stubs (zero-valued when perf_stats disabled) ──
#[cfg(not(feature = "perf_stats"))]
pub static TLB_FLUSHES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static TLB_FULL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static TLB_PAGE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static TLB_ACTIVATE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static TLB_GLOBAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ── Per-syscall profiling stubs ──
#[cfg(not(feature = "perf_stats"))]
pub const PERF_SYSCOUNT: usize = 512;

// ── Filemap fault phase stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static FILEMAP_FAULT_FRAMES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static FILEMAP_FAULT_TICKS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static FILEMAP_PRIVATE_COPY_TICKS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static FILEMAP_MAP_USER_TICKS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ── TLB flush cycle stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static TLB_PAGE_FLUSH_CYCLES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static TLB_FULL_FLUSH_CYCLES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static TLB_ACTIVATE_CYCLES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ── Execve phase cycle stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static EXECVE_MAP_ELF_TICKS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXECVE_KERNEL_MAP_TICKS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXECVE_INTERP_TICKS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXECVE_STACK_TABLES_TICKS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXECVE_TEARDOWN_TICKS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ── PF action names stub ──
#[cfg(not(feature = "perf_stats"))]
pub const PF_ACTION_NAMES: [&str; 7] = [
    "LazyAlloc", "FileBackedRead", "FileBackedSharedWrite",
    "FileBackedWrite", "SharedWrite", "Cow", "Other",
];
