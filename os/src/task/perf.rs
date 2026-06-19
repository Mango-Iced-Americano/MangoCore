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
    // Fine-grained TLB counters
    static TLB_FLUSHES: AtomicUsize = AtomicUsize::new(0);       // total
    static TLB_FULL: AtomicUsize = AtomicUsize::new(0);           // full inval (invtlb 0x3 / sfence.vma no-arg)
    static TLB_PAGE: AtomicUsize = AtomicUsize::new(0);           // single-page (invtlb 0x5 / sfence.vma addr)
    static TLB_ACTIVATE: AtomicUsize = AtomicUsize::new(0);       // address-space switch
    static TLB_GLOBAL: AtomicUsize = AtomicUsize::new(0);         // global inval (invtlb 0x0)
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

    // ── P0: Syscall / Trap ──
    pub static SYSCALL_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static SYSCALL_GETPPID_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static SYSCALL_COST_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static TRAP_ENTER_COST_MAX_TICKS: AtomicUsize = AtomicUsize::new(0);

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
    pub fn record_syscall_enter(syscall_id: usize) {
        if !stats_enabled() { return; }
        SYSCALL_TOTAL.fetch_add(1, Ordering::Relaxed);
        if syscall_id == 173 { SYSCALL_GETPPID_TOTAL.fetch_add(1, Ordering::Relaxed); }
    }

    #[inline(always)]
    pub fn record_syscall_cost_ticks(ticks: usize) { if !stats_enabled() { return; } update_max(&SYSCALL_COST_MAX_TICKS, ticks); }

    #[inline(always)]
    pub fn record_trap_cost_ticks(ticks: usize) { if !stats_enabled() { return; } update_max(&TRAP_ENTER_COST_MAX_TICKS, ticks); }

    /// Reset all P0 performance counters (writable via /sys/kernel/stats/reset).
    pub fn reset_p0_counters() {
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
        // Syscall / Trap
        SYSCALL_TOTAL.store(0, Ordering::Relaxed);
        SYSCALL_GETPPID_TOTAL.store(0, Ordering::Relaxed);
        SYSCALL_COST_MAX_TICKS.store(0, Ordering::Relaxed);
        TRAP_ENTER_COST_MAX_TICKS.store(0, Ordering::Relaxed);
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
            unsafe { core::arch::asm!("rdcycle {}", out(reg) cycles) };
            cycles
        }
        #[cfg(target_arch = "loongarch64")]
        {
            let mut lo: usize; let mut hi: usize;
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
pub fn record_syscall_enter(_syscall_id: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_syscall_cost_ticks(_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_trap_cost_ticks(_ticks: usize) {}

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
#[inline(always)]
pub fn reset_p0_counters() {}
