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

/// Runtime counter profiles. Only one profile is selected during a diagnostic
/// window so unrelated hot paths do not pay counter or timer costs.
pub const STATS_PROFILE_CORE: usize = 1;
pub const STATS_PROFILE_MEMORY_IO: usize = 1 << 1;
pub const STATS_PROFILE_NETWORK_RUNTIME: usize = 1 << 2;
pub const STATS_PROFILE_ALL: usize =
    STATS_PROFILE_CORE | STATS_PROFILE_MEMORY_IO | STATS_PROFILE_NETWORK_RUNTIME;
pub static STATS_PROFILE: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(STATS_PROFILE_CORE);
/// `/sys/kernel/stats/taskq` 调度计数器字段语义版本。
pub const SCHED_COUNTER_SCHEMA_VERSION: usize = 6;
/// `/sys/kernel/stats/vm` filemap counter field semantics version.
pub const FILEMAP_COUNTER_SCHEMA_VERSION: usize = 3;

/// Read the architecture cycle counter without enabling the diagnostics framework.
#[inline(always)]
pub fn perf_time_now_unconditional() -> usize {
    #[cfg(target_arch = "riscv64")]
    {
        let cycles: usize;
        // SAFETY: [Category 13 — library/unsafe contract]. `rdcycle` only reads
        // the current hart's cycle CSR into a compiler-allocated general register;
        // it neither accesses memory nor changes processor state.
        unsafe { core::arch::asm!("rdcycle {}", out(reg) cycles) };
        cycles
    }
    #[cfg(target_arch = "loongarch64")]
    {
        let mut lo: usize;
        let mut hi: usize;
        // SAFETY: [Category 13 — library/unsafe contract]. `rdtime.d` only reads
        // the stable timer into compiler-allocated general registers and does not
        // access memory or modify control state.
        unsafe { core::arch::asm!("rdtime.d {},{}", out(reg) lo, out(reg) hi) };
        lo
    }
    #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
    {
        0
    }
}

pub const BOOT_STAGE_ENTRY: usize = 0;
pub const BOOT_STAGE_CONSOLE: usize = 1;
pub const BOOT_STAGE_MM: usize = 2;
pub const BOOT_STAGE_DRIVERS: usize = 3;
pub const BOOT_STAGE_NET: usize = 4;
pub const BOOT_STAGE_FS: usize = 5;
pub const BOOT_STAGE_INITPROC: usize = 6;
pub const BOOT_STAGE_SCHEDULER: usize = 7;

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn stats_enabled_for(_profile: usize) -> bool {
    false
}

#[cfg(feature = "perf_stats")]
mod enabled {
    use super::super::WaitResult;
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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
    /// 最近一次在 deferred 安全点输出 timer 快照时对应的 1024 次分段。
    static TIMER_SNAPSHOT_EPOCH: AtomicUsize = AtomicUsize::new(0);
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
    static SYSCALL_COUNT: [AtomicUsize; PERF_SYSCOUNT] =
        [const { AtomicUsize::new(0) }; PERF_SYSCOUNT];
    static SYSCALL_TICKS: [AtomicUsize; PERF_SYSCOUNT] =
        [const { AtomicUsize::new(0) }; PERF_SYSCOUNT];

    // ── Page fault per-action profile ──
    // action 0=LazyAlloc 1=FileBackedRead 2=FileBackedSharedWrite
    //        3=FileBackedWrite 4=SharedWrite 5=Cow 6=Other
    pub const PF_ACTION_NAMES: [&str; 7] = [
        "LazyAlloc",
        "FileBackedRead",
        "FileBackedSharedWrite",
        "FileBackedWrite",
        "SharedWrite",
        "Cow",
        "Other",
    ];
    static PF_ACTION_COUNT: [AtomicUsize; 7] = [const { AtomicUsize::new(0) }; 7];
    static PF_ACTION_TICKS: [AtomicUsize; 7] = [const { AtomicUsize::new(0) }; 7];

    // ── Demand page-fault phase profile ──
    pub const PF_STAGE_NAMES: [&str; 8] = [
        "trap_entry",
        "classify_vma",
        "pte_map",
        "frame_alloc",
        "zero_copy",
        "tlb_flush",
        "trap_return",
        "filemap_frame",
    ];
    static PF_STAGE_COUNT: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
    static PF_STAGE_TICKS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
    static PF_RETURN_PENDING: AtomicBool = AtomicBool::new(false);

    // ── Filemap fault phase counters ──
    pub static FILEMAP_FAULT_FRAMES: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_FAULT_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_PRIVATE_COPY_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_MAP_USER_TICKS: AtomicUsize = AtomicUsize::new(0);
    // ── Stage 0: filemap fault attribution ──
    pub static FILEMAP_READ_FAULT_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_PRIVATE_FAULT_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_SHARED_WRITE_FAULT_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_READY_HIT: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_NOT_READY_RETRY: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_BACKEND_READ_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_BACKEND_READ_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_BACKEND_READ_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_BACKEND_READ_UNDER_VM_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_BACKEND_READ_UNDER_VM_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_BACKEND_READ_UNDER_VM_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_RETRY_WAIT_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_RETRY_WAIT_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_RETRY_WAIT_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_REVALIDATE_RETRY: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_REVALIDATE_VMA_CHANGED: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_REVALIDATE_EOF_CHANGED: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_FAULT_AROUND_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_FAULT_AROUND_PAGES_REQUESTED: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_FAULT_AROUND_PAGES_MISSING: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_FAULT_AROUND_CLAIM_CONFLICTS: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_FAULT_AROUND_PAGES_PUBLISHED: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_FAULT_AROUND_PAGES_PREFETCHED: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_FAULT_AROUND_BACKEND_RUNS: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_FAULT_AROUND_USEFUL_HITS: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_FAULT_AROUND_UNUSED_DISCARDS: AtomicUsize = AtomicUsize::new(0);
    pub static FILEMAP_FAULT_AROUND_ABORTS: AtomicUsize = AtomicUsize::new(0);

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
    // ── Stage 0: exec path selection ──
    pub static EXEC_DIRECT_COUNT: AtomicUsize = AtomicUsize::new(0);
    pub static EXEC_FALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);
    pub static EXEC_DIRECT_ENOSYS_COUNT: AtomicUsize = AtomicUsize::new(0);

    // ── Stage 0: AddressSpace lock and MM switch attribution ──
    pub static VM_READ_LOCK_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static VM_READ_LOCK_WAIT_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static VM_READ_LOCK_WAIT_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static VM_READ_LOCK_HOLD_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static VM_READ_LOCK_HOLD_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static VM_WRITE_LOCK_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static VM_WRITE_LOCK_WAIT_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static VM_WRITE_LOCK_WAIT_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static VM_WRITE_LOCK_HOLD_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static VM_WRITE_LOCK_HOLD_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static VM_FLUSH_OUTSIDE_LOCK_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static VM_FLUSH_OUTSIDE_LOCK_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static TASK_SWITCH_SAME_MM: AtomicUsize = AtomicUsize::new(0);
    pub static TASK_SWITCH_DIFFERENT_MM: AtomicUsize = AtomicUsize::new(0);
    pub static TASK_SWITCH_TO_KERNEL_ONLY: AtomicUsize = AtomicUsize::new(0);
    pub static TASK_SWITCH_IDLE_NO_NEXT: AtomicUsize = AtomicUsize::new(0);

    // ── Stage 0: frame allocator attribution ──
    pub static FRAME_GLOBAL_ALLOC_LOCK_WAIT_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static FRAME_GLOBAL_ALLOC_LOCK_WAIT_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static FRAME_GLOBAL_ALLOC_LOCK_HOLD_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static FRAME_GLOBAL_ALLOC_LOCK_HOLD_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static FRAME_GLOBAL_FREE_LOCK_WAIT_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static FRAME_GLOBAL_FREE_LOCK_WAIT_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static FRAME_GLOBAL_FREE_LOCK_HOLD_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static FRAME_GLOBAL_FREE_LOCK_HOLD_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static FRAME_RESERVE_CHECK_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static FRAME_RESERVE_CHECK_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static FRAME_RESERVE_OOM_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static FRAME_ALLOC_SOURCE_FRESH: AtomicUsize = AtomicUsize::new(0);
    pub static FRAME_ALLOC_SOURCE_RECYCLED: AtomicUsize = AtomicUsize::new(0);
    pub static FRAME_CONTIG_LOCK_WAIT_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static FRAME_CONTIG_LOCK_WAIT_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static FRAME_CONTIG_LOCK_HOLD_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static FRAME_CONTIG_LOCK_HOLD_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static FRAME_CONTIG_ZERO_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static FRAME_CONTIG_PAGES: AtomicUsize = AtomicUsize::new(0);

    // ── Stage 0: heap attribution ──
    pub static HEAP_LOCK_WAIT_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static HEAP_LOCK_WAIT_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static HEAP_LOCK_HOLD_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static HEAP_LOCK_HOLD_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static HEAP_SLAB_ALLOC_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static HEAP_DIRECT_BUDDY_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static HEAP_CLASS_8_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static HEAP_CLASS_16_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static HEAP_CLASS_32_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static HEAP_CLASS_64_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static HEAP_CLASS_128_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static HEAP_CLASS_256_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static HEAP_CLASS_512_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static HEAP_CLASS_1024_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static HEAP_CLASS_2048_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static HEAP_LARGE_CALLS: AtomicUsize = AtomicUsize::new(0);

    // ── Stage 0: MM activation and scheduler placement ──
    pub static MM_ACTIVATE_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static MM_ACTIVATE_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static MM_DEACTIVATE_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static MM_DEACTIVATE_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static MM_SAME_ALREADY_ACTIVE: AtomicUsize = AtomicUsize::new(0);
    pub static MM_GENERATION_CATCHUP: AtomicUsize = AtomicUsize::new(0);
    pub static MM_ASID_ROLLOVER: AtomicUsize = AtomicUsize::new(0);
    pub static WAKE_LOCAL: AtomicUsize = AtomicUsize::new(0);
    pub static WAKE_REMOTE: AtomicUsize = AtomicUsize::new(0);
    pub static WAKE_KEEP_LAST_CPU: AtomicUsize = AtomicUsize::new(0);
    pub static WAKE_SELECT_IDLE_CPU: AtomicUsize = AtomicUsize::new(0);
    pub static WAKE_SELECT_LEAST_LOADED: AtomicUsize = AtomicUsize::new(0);
    pub static WAKE_LAST_BUSY_IDLE_AVAILABLE: AtomicUsize = AtomicUsize::new(0);
    pub static NEW_TASK_IDLE_AVAILABLE: AtomicUsize = AtomicUsize::new(0);
    pub static NEW_TASK_SELECTED_IDLE: AtomicUsize = AtomicUsize::new(0);
    pub static NEW_TASK_KEPT_BUSY_PARENT: AtomicUsize = AtomicUsize::new(0);
    pub static WAKE_TO_RUN_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static WAKE_TO_RUN_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static TASK_RUN_SLICE_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static STEAL_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
    pub static STEAL_ATTEMPTS_BY_CPU: [AtomicUsize; crate::smp::MAX_CPUS] =
        [const { AtomicUsize::new(0) }; crate::smp::MAX_CPUS];
    pub static STEAL_CANDIDATE_FOUND: AtomicUsize = AtomicUsize::new(0);
    pub static STEAL_NO_REMOTE_READY: AtomicUsize = AtomicUsize::new(0);
    pub static STEAL_NO_ELIGIBLE_CANDIDATE: AtomicUsize = AtomicUsize::new(0);
    pub static STEAL_SUCCESS: AtomicUsize = AtomicUsize::new(0);
    pub static STEAL_SUCCESS_BY_CPU: [AtomicUsize; crate::smp::MAX_CPUS] =
        [const { AtomicUsize::new(0) }; crate::smp::MAX_CPUS];
    pub static STEAL_RECHECK_FAILED: AtomicUsize = AtomicUsize::new(0);
    pub static STEAL_KTLB_SYNC_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static STEAL_KTLB_SYNC_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static STEAL_KTLB_SYNC_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static SCHED_IDLE_BUSY_LOOPS_BY_CPU: [AtomicUsize; crate::smp::MAX_CPUS] =
        [const { AtomicUsize::new(0) }; crate::smp::MAX_CPUS];
    pub static SCHED_IDLE_WAIT_LOOPS_BY_CPU: [AtomicUsize; crate::smp::MAX_CPUS] =
        [const { AtomicUsize::new(0) }; crate::smp::MAX_CPUS];

    // ── Stage 0: exec detail attribution ──
    pub static EXEC_PTLOAD_SEGMENTS: AtomicUsize = AtomicUsize::new(0);
    pub static EXEC_PTLOAD_PAGES: AtomicUsize = AtomicUsize::new(0);
    pub static EXEC_PTLOAD_FILE_BYTES: AtomicUsize = AtomicUsize::new(0);
    pub static EXEC_PREFETCH_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static EXEC_TARGET_ALLOC_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static EXEC_TARGET_ZERO_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static EXEC_PAGECACHE_COPY_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static EXEC_FALLBACK_KMAP_WAIT_TICKS: AtomicUsize = AtomicUsize::new(0);

    // DAC (Discretionary Access Control) — filesystem permission checks
    pub static DAC_SEARCH_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static DAC_VFS_LOOKUP_CALLS: AtomicUsize = AtomicUsize::new(0);

    // Fine-grained TLB counters
    pub static TLB_FLUSHES: AtomicUsize = AtomicUsize::new(0); // total
    pub static TLB_FULL: AtomicUsize = AtomicUsize::new(0); // full inval (invtlb 0x3 / sfence.vma no-arg)
    pub static TLB_PAGE: AtomicUsize = AtomicUsize::new(0); // single-page (invtlb 0x5 / sfence.vma addr)
    pub static TLB_ACTIVATE: AtomicUsize = AtomicUsize::new(0); // address-space switch
    pub static TLB_GLOBAL: AtomicUsize = AtomicUsize::new(0); // global inval (invtlb 0x0)
    pub static FRAME_ALLOC_HITS: AtomicUsize = AtomicUsize::new(0);
    pub static FRAME_FREE_HITS: AtomicUsize = AtomicUsize::new(0);
    pub static PAGE_FAULTS: AtomicUsize = AtomicUsize::new(0);
    static VFS_LOOKUPS: AtomicUsize = AtomicUsize::new(0);
    static VFS_LOOKUP_TIME_TICKS: AtomicUsize = AtomicUsize::new(0);
    static VFS_LOOKUP_TIME_COUNT: AtomicUsize = AtomicUsize::new(0);
    // Timing accumulators (raw ticks from get_time)
    static CLONE_TIME_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static PAGEFAULT_TIME_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static FRAME_ALLOC_TIME_TICKS: AtomicUsize = AtomicUsize::new(0);
    static CLONE_TIME_COUNT: AtomicUsize = AtomicUsize::new(0);
    pub static PAGEFAULT_TIME_COUNT: AtomicUsize = AtomicUsize::new(0);
    pub static FRAME_ALLOC_TIME_COUNT: AtomicUsize = AtomicUsize::new(0);

    // One-shot boot milestones. Values are elapsed raw clock ticks from the
    // earliest Rust entry marker and intentionally survive stats reset.
    static BOOT_START_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static BOOT_CONSOLE_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static BOOT_MM_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static BOOT_DRIVERS_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static BOOT_NET_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static BOOT_FS_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static BOOT_INITPROC_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static BOOT_SCHEDULER_TICKS: AtomicUsize = AtomicUsize::new(0);

    #[inline(always)]
    pub fn stats_enabled_for(profile: usize) -> bool {
        super::STATS_ON.load(Ordering::Relaxed)
            && super::STATS_PROFILE.load(Ordering::Relaxed) & profile != 0
    }

    #[inline(always)]
    fn stats_enabled() -> bool {
        stats_enabled_for(super::STATS_PROFILE_CORE)
    }

    #[inline(always)]
    fn memory_io_stats_enabled() -> bool {
        stats_enabled_for(super::STATS_PROFILE_MEMORY_IO)
    }

    #[inline(always)]
    fn network_runtime_stats_enabled() -> bool {
        stats_enabled_for(super::STATS_PROFILE_NETWORK_RUNTIME)
    }

    // ── Stage 0 recorders ──

    #[inline(always)]
    pub fn record_vm_read_lock(wait_ticks: usize, hold_ticks: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        VM_READ_LOCK_CALLS.fetch_add(1, Ordering::Relaxed);
        VM_READ_LOCK_WAIT_TICKS_TOTAL.fetch_add(wait_ticks, Ordering::Relaxed);
        update_max(&VM_READ_LOCK_WAIT_TICKS_MAX, wait_ticks);
        VM_READ_LOCK_HOLD_TICKS_TOTAL.fetch_add(hold_ticks, Ordering::Relaxed);
        update_max(&VM_READ_LOCK_HOLD_TICKS_MAX, hold_ticks);
    }

    #[inline(always)]
    pub fn record_vm_write_lock(wait_ticks: usize, hold_ticks: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        VM_WRITE_LOCK_CALLS.fetch_add(1, Ordering::Relaxed);
        VM_WRITE_LOCK_WAIT_TICKS_TOTAL.fetch_add(wait_ticks, Ordering::Relaxed);
        update_max(&VM_WRITE_LOCK_WAIT_TICKS_MAX, wait_ticks);
        VM_WRITE_LOCK_HOLD_TICKS_TOTAL.fetch_add(hold_ticks, Ordering::Relaxed);
        update_max(&VM_WRITE_LOCK_HOLD_TICKS_MAX, hold_ticks);
    }

    #[inline(always)]
    pub fn record_vm_flush_outside_lock(ticks: usize) {
        if memory_io_stats_enabled() {
            VM_FLUSH_OUTSIDE_LOCK_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
            update_max(&VM_FLUSH_OUTSIDE_LOCK_TICKS_MAX, ticks);
        }
    }

    #[inline(always)]
    pub fn record_task_switch_mm(same_mm: bool) {
        if !stats_enabled() {
            return;
        }
        if same_mm {
            TASK_SWITCH_SAME_MM.fetch_add(1, Ordering::Relaxed);
        } else {
            TASK_SWITCH_DIFFERENT_MM.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_task_switch_to_kernel_only() {
        if stats_enabled() {
            TASK_SWITCH_TO_KERNEL_ONLY.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_task_switch_idle_no_next() {
        if stats_enabled() {
            TASK_SWITCH_IDLE_NO_NEXT.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_mm_activate(ticks: usize, generation_catchup: bool) {
        if !memory_io_stats_enabled() {
            return;
        }
        MM_ACTIVATE_CALLS.fetch_add(1, Ordering::Relaxed);
        MM_ACTIVATE_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
        if generation_catchup {
            MM_GENERATION_CATCHUP.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_mm_deactivate(ticks: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        MM_DEACTIVATE_CALLS.fetch_add(1, Ordering::Relaxed);
        MM_DEACTIVATE_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_mm_same_already_active() {
        if memory_io_stats_enabled() {
            MM_SAME_ALREADY_ACTIVE.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_mm_asid_rollover() {
        if memory_io_stats_enabled() {
            MM_ASID_ROLLOVER.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_exec_direct() {
        if stats_enabled() {
            EXEC_DIRECT_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_exec_fallback() {
        if stats_enabled() {
            EXEC_FALLBACK_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_exec_direct_enosys() {
        if stats_enabled() {
            EXEC_DIRECT_ENOSYS_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_filemap_read_fault() {
        if memory_io_stats_enabled() {
            FILEMAP_READ_FAULT_CALLS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_filemap_private_fault() {
        if memory_io_stats_enabled() {
            FILEMAP_PRIVATE_FAULT_CALLS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_filemap_shared_write_fault() {
        if memory_io_stats_enabled() {
            FILEMAP_SHARED_WRITE_FAULT_CALLS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_filemap_ready_hit() {
        if memory_io_stats_enabled() {
            FILEMAP_READY_HIT.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_filemap_not_ready_retry() {
        if memory_io_stats_enabled() {
            FILEMAP_NOT_READY_RETRY.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_filemap_backend_read(ticks: usize, under_vm: bool) {
        if !memory_io_stats_enabled() {
            return;
        }
        FILEMAP_BACKEND_READ_CALLS.fetch_add(1, Ordering::Relaxed);
        FILEMAP_BACKEND_READ_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
        update_max(&FILEMAP_BACKEND_READ_TICKS_MAX, ticks);
        if under_vm {
            FILEMAP_BACKEND_READ_UNDER_VM_CALLS.fetch_add(1, Ordering::Relaxed);
            FILEMAP_BACKEND_READ_UNDER_VM_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
            update_max(&FILEMAP_BACKEND_READ_UNDER_VM_TICKS_MAX, ticks);
        }
    }

    #[inline(always)]
    pub fn record_filemap_retry_wait(ticks: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        FILEMAP_RETRY_WAIT_CALLS.fetch_add(1, Ordering::Relaxed);
        FILEMAP_RETRY_WAIT_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
        update_max(&FILEMAP_RETRY_WAIT_TICKS_MAX, ticks);
    }

    #[inline(always)]
    pub fn record_filemap_revalidate_retry(vma_changed: bool, eof_changed: bool) {
        if !memory_io_stats_enabled() {
            return;
        }
        FILEMAP_REVALIDATE_RETRY.fetch_add(1, Ordering::Relaxed);
        if vma_changed {
            FILEMAP_REVALIDATE_VMA_CHANGED.fetch_add(1, Ordering::Relaxed);
        }
        if eof_changed {
            FILEMAP_REVALIDATE_EOF_CHANGED.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_filemap_fault_around_start(requested_pages: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        FILEMAP_FAULT_AROUND_CALLS.fetch_add(1, Ordering::Relaxed);
        FILEMAP_FAULT_AROUND_PAGES_REQUESTED.fetch_add(requested_pages, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_filemap_fault_around_missing(missing_pages: usize) {
        if memory_io_stats_enabled() {
            FILEMAP_FAULT_AROUND_PAGES_MISSING.fetch_add(missing_pages, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_filemap_fault_around_claim_conflict(conflicts: usize) {
        if memory_io_stats_enabled() && conflicts != 0 {
            FILEMAP_FAULT_AROUND_CLAIM_CONFLICTS.fetch_add(conflicts, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_filemap_fault_around_backend_run() {
        if memory_io_stats_enabled() {
            FILEMAP_FAULT_AROUND_BACKEND_RUNS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_filemap_fault_around_publish(published_pages: usize, prefetched_pages: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        FILEMAP_FAULT_AROUND_PAGES_PUBLISHED.fetch_add(published_pages, Ordering::Relaxed);
        FILEMAP_FAULT_AROUND_PAGES_PREFETCHED.fetch_add(prefetched_pages, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_filemap_fault_around_useful_hit() {
        if memory_io_stats_enabled() {
            FILEMAP_FAULT_AROUND_USEFUL_HITS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_filemap_fault_around_unused_discard() {
        if memory_io_stats_enabled() {
            FILEMAP_FAULT_AROUND_UNUSED_DISCARDS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_filemap_fault_around_abort() {
        if memory_io_stats_enabled() {
            FILEMAP_FAULT_AROUND_ABORTS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_frame_global_alloc_lock(wait_ticks: usize, hold_ticks: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        FRAME_GLOBAL_ALLOC_LOCK_WAIT_TICKS_TOTAL.fetch_add(wait_ticks, Ordering::Relaxed);
        update_max(&FRAME_GLOBAL_ALLOC_LOCK_WAIT_TICKS_MAX, wait_ticks);
        FRAME_GLOBAL_ALLOC_LOCK_HOLD_TICKS_TOTAL.fetch_add(hold_ticks, Ordering::Relaxed);
        update_max(&FRAME_GLOBAL_ALLOC_LOCK_HOLD_TICKS_MAX, hold_ticks);
    }

    #[inline(always)]
    pub fn record_frame_global_free_lock(wait_ticks: usize, hold_ticks: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        FRAME_GLOBAL_FREE_LOCK_WAIT_TICKS_TOTAL.fetch_add(wait_ticks, Ordering::Relaxed);
        update_max(&FRAME_GLOBAL_FREE_LOCK_WAIT_TICKS_MAX, wait_ticks);
        FRAME_GLOBAL_FREE_LOCK_HOLD_TICKS_TOTAL.fetch_add(hold_ticks, Ordering::Relaxed);
        update_max(&FRAME_GLOBAL_FREE_LOCK_HOLD_TICKS_MAX, hold_ticks);
    }

    #[inline(always)]
    pub fn record_frame_reserve_check(ticks: usize, oom: bool) {
        if !memory_io_stats_enabled() {
            return;
        }
        FRAME_RESERVE_CHECK_CALLS.fetch_add(1, Ordering::Relaxed);
        FRAME_RESERVE_CHECK_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
        if oom {
            FRAME_RESERVE_OOM_CALLS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_frame_alloc_source(recycled: bool) {
        if !memory_io_stats_enabled() {
            return;
        }
        if recycled {
            FRAME_ALLOC_SOURCE_RECYCLED.fetch_add(1, Ordering::Relaxed);
        } else {
            FRAME_ALLOC_SOURCE_FRESH.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_frame_contig_lock(wait_ticks: usize, hold_ticks: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        FRAME_CONTIG_LOCK_WAIT_TICKS_TOTAL.fetch_add(wait_ticks, Ordering::Relaxed);
        update_max(&FRAME_CONTIG_LOCK_WAIT_TICKS_MAX, wait_ticks);
        FRAME_CONTIG_LOCK_HOLD_TICKS_TOTAL.fetch_add(hold_ticks, Ordering::Relaxed);
        update_max(&FRAME_CONTIG_LOCK_HOLD_TICKS_MAX, hold_ticks);
    }

    #[inline(always)]
    pub fn record_frame_contig_page(zero_ticks: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        FRAME_CONTIG_PAGES.fetch_add(1, Ordering::Relaxed);
        FRAME_CONTIG_ZERO_TICKS_TOTAL.fetch_add(zero_ticks, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_heap_lock(wait_ticks: usize, hold_ticks: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        HEAP_LOCK_WAIT_TICKS_TOTAL.fetch_add(wait_ticks, Ordering::Relaxed);
        update_max(&HEAP_LOCK_WAIT_TICKS_MAX, wait_ticks);
        HEAP_LOCK_HOLD_TICKS_TOTAL.fetch_add(hold_ticks, Ordering::Relaxed);
        update_max(&HEAP_LOCK_HOLD_TICKS_MAX, hold_ticks);
    }

    #[inline(always)]
    pub fn record_heap_alloc_path(class_bytes: Option<usize>) {
        if !memory_io_stats_enabled() {
            return;
        }
        match class_bytes {
            None => HEAP_DIRECT_BUDDY_CALLS.fetch_add(1, Ordering::Relaxed),
            Some(8) => HEAP_CLASS_8_CALLS.fetch_add(1, Ordering::Relaxed),
            Some(16) => HEAP_CLASS_16_CALLS.fetch_add(1, Ordering::Relaxed),
            Some(32) => HEAP_CLASS_32_CALLS.fetch_add(1, Ordering::Relaxed),
            Some(64) => HEAP_CLASS_64_CALLS.fetch_add(1, Ordering::Relaxed),
            Some(128) => HEAP_CLASS_128_CALLS.fetch_add(1, Ordering::Relaxed),
            Some(256) => HEAP_CLASS_256_CALLS.fetch_add(1, Ordering::Relaxed),
            Some(512) => HEAP_CLASS_512_CALLS.fetch_add(1, Ordering::Relaxed),
            Some(1024) => HEAP_CLASS_1024_CALLS.fetch_add(1, Ordering::Relaxed),
            Some(2048) => HEAP_CLASS_2048_CALLS.fetch_add(1, Ordering::Relaxed),
            Some(_) => HEAP_LARGE_CALLS.fetch_add(1, Ordering::Relaxed),
        };
        if class_bytes.is_some() {
            HEAP_SLAB_ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_wake_local() {
        if stats_enabled() {
            WAKE_LOCAL.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_wake_remote() {
        if stats_enabled() {
            WAKE_REMOTE.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_wake_selection(keep_last: bool, idle: bool, last_busy_idle_available: bool) {
        if !stats_enabled() {
            return;
        }
        if last_busy_idle_available {
            WAKE_LAST_BUSY_IDLE_AVAILABLE.fetch_add(1, Ordering::Relaxed);
        }
        if keep_last {
            WAKE_KEEP_LAST_CPU.fetch_add(1, Ordering::Relaxed);
        } else if idle {
            WAKE_SELECT_IDLE_CPU.fetch_add(1, Ordering::Relaxed);
        } else {
            WAKE_SELECT_LEAST_LOADED.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_new_task_placement(
        idle_available: bool,
        selected_idle: bool,
        kept_busy_parent: bool,
    ) {
        if !stats_enabled() {
            return;
        }
        if idle_available {
            NEW_TASK_IDLE_AVAILABLE.fetch_add(1, Ordering::Relaxed);
        }
        if selected_idle {
            NEW_TASK_SELECTED_IDLE.fetch_add(1, Ordering::Relaxed);
        }
        if kept_busy_parent {
            NEW_TASK_KEPT_BUSY_PARENT.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_wake_to_run(ticks: usize) {
        if !stats_enabled() {
            return;
        }
        WAKE_TO_RUN_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
        update_max(&WAKE_TO_RUN_TICKS_MAX, ticks);
    }

    #[inline(always)]
    pub fn record_task_run_slice(ticks: usize) {
        if stats_enabled() {
            TASK_RUN_SLICE_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_steal_attempt(cpu: usize) {
        if stats_enabled() {
            STEAL_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
            if cpu < STEAL_ATTEMPTS_BY_CPU.len() {
                STEAL_ATTEMPTS_BY_CPU[cpu].fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    #[inline(always)]
    pub fn record_steal_candidate() {
        if stats_enabled() {
            STEAL_CANDIDATE_FOUND.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_steal_no_remote_ready() {
        if stats_enabled() {
            STEAL_NO_REMOTE_READY.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_steal_no_eligible_candidate() {
        if stats_enabled() {
            STEAL_NO_ELIGIBLE_CANDIDATE.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_steal_success(cpu: usize) {
        if stats_enabled() {
            STEAL_SUCCESS.fetch_add(1, Ordering::Relaxed);
            if cpu < STEAL_SUCCESS_BY_CPU.len() {
                STEAL_SUCCESS_BY_CPU[cpu].fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    #[inline(always)]
    pub fn record_scheduler_idle(cpu: usize, waited: bool) {
        if !stats_enabled() || cpu >= crate::smp::MAX_CPUS {
            return;
        }
        if waited {
            SCHED_IDLE_WAIT_LOOPS_BY_CPU[cpu].fetch_add(1, Ordering::Relaxed);
        } else {
            SCHED_IDLE_BUSY_LOOPS_BY_CPU[cpu].fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_steal_recheck_failed() {
        if stats_enabled() {
            STEAL_RECHECK_FAILED.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_steal_ktlb_sync(ticks: usize) {
        if !stats_enabled() {
            return;
        }
        STEAL_KTLB_SYNC_CALLS.fetch_add(1, Ordering::Relaxed);
        STEAL_KTLB_SYNC_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
        update_max(&STEAL_KTLB_SYNC_TICKS_MAX, ticks);
    }

    #[inline(always)]
    pub fn record_exec_ptload(segments: usize, pages: usize, file_bytes: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        EXEC_PTLOAD_SEGMENTS.fetch_add(segments, Ordering::Relaxed);
        EXEC_PTLOAD_PAGES.fetch_add(pages, Ordering::Relaxed);
        EXEC_PTLOAD_FILE_BYTES.fetch_add(file_bytes, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_exec_phase(counter: &AtomicUsize, ticks: usize) {
        if memory_io_stats_enabled() {
            counter.fetch_add(ticks, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_exec_fallback_kmap_wait(ticks: usize) {
        if memory_io_stats_enabled() {
            EXEC_FALLBACK_KMAP_WAIT_TICKS.fetch_add(ticks, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_boot_stage(stage: usize) {
        let now = crate::hal::get_time();
        if stage == super::BOOT_STAGE_ENTRY {
            BOOT_START_TICKS.store(now, Ordering::Relaxed);
            return;
        }
        let elapsed = now.wrapping_sub(BOOT_START_TICKS.load(Ordering::Relaxed));
        let target = match stage {
            super::BOOT_STAGE_CONSOLE => &BOOT_CONSOLE_TICKS,
            super::BOOT_STAGE_MM => &BOOT_MM_TICKS,
            super::BOOT_STAGE_DRIVERS => &BOOT_DRIVERS_TICKS,
            super::BOOT_STAGE_NET => &BOOT_NET_TICKS,
            super::BOOT_STAGE_FS => &BOOT_FS_TICKS,
            super::BOOT_STAGE_INITPROC => &BOOT_INITPROC_TICKS,
            super::BOOT_STAGE_SCHEDULER => &BOOT_SCHEDULER_TICKS,
            _ => return,
        };
        target.store(elapsed, Ordering::Relaxed);
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
    pub static USER_UNALIGNED_TRAPS: AtomicUsize = AtomicUsize::new(0);
    pub static USER_UNALIGNED_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static USER_UNALIGNED_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static USER_UNALIGNED_LOAD_2: AtomicUsize = AtomicUsize::new(0);
    pub static USER_UNALIGNED_LOAD_4: AtomicUsize = AtomicUsize::new(0);
    pub static USER_UNALIGNED_LOAD_8: AtomicUsize = AtomicUsize::new(0);
    pub static USER_UNALIGNED_STORE_2: AtomicUsize = AtomicUsize::new(0);
    pub static USER_UNALIGNED_STORE_4: AtomicUsize = AtomicUsize::new(0);
    pub static USER_UNALIGNED_STORE_8: AtomicUsize = AtomicUsize::new(0);
    pub static USER_UNALIGNED_FLOAT_LOADS: AtomicUsize = AtomicUsize::new(0);
    pub static USER_UNALIGNED_FLOAT_STORES: AtomicUsize = AtomicUsize::new(0);

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
    pub static PC_READ_USER_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static PC_READ_USER_PAGES: AtomicUsize = AtomicUsize::new(0);
    pub static PC_READ_MISS: AtomicUsize = AtomicUsize::new(0);
    pub static PC_READ_CYCLES_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static PC_READ_HIT_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PC_READ_MISS_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PC_READ_LOOKUP_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PC_READ_MISS_FILL_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PC_READ_VALID_FILL_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PC_READ_COPY_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PC_COPY_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PC_LOOKUP_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PC_WRITE_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static PC_WRITE_PAGES: AtomicUsize = AtomicUsize::new(0);
    pub static PC_WRITE_OVERWRITE: AtomicUsize = AtomicUsize::new(0);
    pub static PC_WRITE_EVENTUALLY_FULL: AtomicUsize = AtomicUsize::new(0);
    pub static PC_WRITE_CYCLES_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static PC_WRITE_LOOKUP_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PC_WRITE_LEASE_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PC_WRITE_COPY_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PC_WRITE_COMMIT_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PC_WRITEBACK_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static PC_WRITEBACK_PAGES: AtomicUsize = AtomicUsize::new(0);
    pub static PC_WRITEBACK_CYCLES_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static PC_FALLOC_CYCLES_TOTAL: AtomicUsize = AtomicUsize::new(0);

    // ── P0: UserBuffer pwrite boundary attribution ──
    pub static PWRITE_UACCESS_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PWRITE_FILE_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PWRITE_EXT4_SETUP_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PWRITE_EXT4_POST_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PWRITE_TOTAL_COUNT: AtomicUsize = AtomicUsize::new(0);
    pub static PWRITE_VFS_MODE_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PWRITE_VFS_SEALS_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PWRITE_VFS_TOUCH_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PWRITE_MOUNT_WRITABLE_CYCLES: AtomicUsize = AtomicUsize::new(0);

    // ── P0: UserBuffer write (lseek+write) foreground boundary attribution ──
    // Splits the generic `write(2)` syscall body (440.5M ticks in the random
    // writers diagnosis) into: fd/fsize prep, uaccess, VFS mode/seals wrapper,
    // PageCache foreground (PC_WRITE_*), and offset/timestamp finish. The
    // PageCache bucket itself stays on the existing PC_WRITE_* counters so this
    // set only adds the out-of-PageCache boundaries. `WRITE_FILE_CYCLES` is the
    // whole `File::write_user` call (VFS wrapper + PageCache + finish) and equals
    // the sum of the sub-buckets recorded inside `write_user`.
    pub static WRITE_FD_PREP_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static WRITE_UACCESS_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static WRITE_FILE_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static WRITE_VFS_MODE_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static WRITE_VFS_SEALS_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static WRITE_OFFSET_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static WRITE_TOTAL_COUNT: AtomicUsize = AtomicUsize::new(0);

    // ── P0: UserBuffer read boundary attribution ──
    pub static PREAD_UACCESS_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PREAD_FILE_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PREAD_EXT4_LOGICAL_SIZE_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PREAD_EXT4_PAGE_CACHE_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static PREAD_TOTAL_COUNT: AtomicUsize = AtomicUsize::new(0);
    pub static PREAD_VFS_MODE_CYCLES: AtomicUsize = AtomicUsize::new(0);

    // ── P0: another_ext4 journal/writeback attribution ──
    pub static JOURNAL_COMMIT_COUNT: AtomicUsize = AtomicUsize::new(0);
    pub static JOURNAL_COMMIT_BYTES: AtomicUsize = AtomicUsize::new(0);
    pub static WB_DATA_WRITE_BYTES: AtomicUsize = AtomicUsize::new(0);
    pub static WB_DATA_WRITE_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static WB_ALLOC_EXTENT_PAGES: AtomicUsize = AtomicUsize::new(0);
    pub static WB_ALLOC_EXTENT_CYCLES: AtomicUsize = AtomicUsize::new(0);
    pub static WB_TX_JOURNAL_COMMIT_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static WB_TX_JOURNAL_STAGED_BLOCKS: AtomicUsize = AtomicUsize::new(0);
    pub static WB_TX_JOURNAL_TX_FIRST: AtomicUsize = AtomicUsize::new(0);
    pub static WB_TX_JOURNAL_TX_LAST: AtomicUsize = AtomicUsize::new(0);
    pub static WB_TX_JOURNAL_FLUSH_COUNT: AtomicUsize = AtomicUsize::new(0);
    pub static WB_TX_JOURNAL_FLUSH_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static WB_FLUSH_BOUNDARY_COUNT: AtomicUsize = AtomicUsize::new(0);
    pub static WB_FLUSH_BOUNDARY_TICKS: AtomicUsize = AtomicUsize::new(0);

    // ── P0: Writeback Throttling ──
    pub static WB_BG_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static WB_THROTTLE_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static WB_REDIRTY_PAGES: AtomicUsize = AtomicUsize::new(0);

    // ── P0: Block Device I/O ──
    pub static BLK_VREAD_REQS: AtomicUsize = AtomicUsize::new(0);
    pub static BLK_VREAD_SECS: AtomicUsize = AtomicUsize::new(0);
    pub static BLK_VWRITE_REQS: AtomicUsize = AtomicUsize::new(0);
    pub static BLK_VWRITE_SECS: AtomicUsize = AtomicUsize::new(0);

    // ── I/O amplification ──
    pub static DEVICE_FLUSH_COUNT: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_WRITE_REQUESTS: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_WRITE_BYTES: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_READ_REQUESTS: AtomicUsize = AtomicUsize::new(0);
    pub static WRITEBACK_BATCH_COUNT: AtomicUsize = AtomicUsize::new(0);
    pub static WRITEBACK_PAGE_COUNT: AtomicUsize = AtomicUsize::new(0);
    // ── Writeback transaction boundary attribution ──
    pub static WB_TX_DATA_WRITE_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static WB_TX_DATA_WRITE_BYTES: AtomicUsize = AtomicUsize::new(0);
    pub static WB_TX_DATA_WRITE_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static WB_TX_ALLOC_EXTENT_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static WB_TX_ALLOC_EXTENT_PAGES: AtomicUsize = AtomicUsize::new(0);
    pub static WB_TX_ALLOC_EXTENT_TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static WB_TX_BOUNDARY_FLUSH_COUNT: AtomicUsize = AtomicUsize::new(0);
    pub static WB_TX_BOUNDARY_FLUSH_TICKS: AtomicUsize = AtomicUsize::new(0);
    // ── Stage 0: VirtIO request/DMA attribution ──
    // BLK_V* counts the BlockDevice API calls. These counters count the
    // actual synchronous chunks and share() roles underneath that API.
    pub static VIRTIO_BLK_READ_CHUNKS: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_BLK_READ_BYTES: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_BLK_WRITE_CHUNKS: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_BLK_WRITE_BYTES: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_DMA_POOL_RESERVE_SUCCESS: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_DMA_POOL_RESERVE_FAIL: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_DMA_POOL_CONSUME: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_DMA_POOL_CANCEL: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_DMA_POOL_FINISH: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_DMA_SHARE_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_DMA_SHARE_DATA_POOL: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_DMA_SHARE_DATA_FALLBACK: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_DMA_SHARE_HEADER_FALLBACK: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_DMA_SHARE_STATUS_FALLBACK: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_DMA_SHARE_INDIRECT_FALLBACK: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_DMA_SHARE_OTHER_FALLBACK: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_DMA_SHARE_HEADER_POOL: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_DMA_SHARE_STATUS_POOL: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_DMA_SHARE_INDIRECT_POOL: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_DMA_BRIDGE_LOCK_WAIT_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_DMA_BRIDGE_LOCK_HOLD_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static VIRTIO_DMA_BRIDGE_LOCK_HOLD_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);

    // ── P0: 2K1000LA SATA/AHCI ──
    pub static SATA_READ_REQS: AtomicUsize = AtomicUsize::new(0);
    pub static SATA_READ_BYTES: AtomicUsize = AtomicUsize::new(0);
    pub static SATA_READ_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static SATA_READ_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static SATA_WRITE_REQS: AtomicUsize = AtomicUsize::new(0);
    pub static SATA_WRITE_BYTES: AtomicUsize = AtomicUsize::new(0);
    pub static SATA_WRITE_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static SATA_WRITE_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static SATA_FLUSH_REQS: AtomicUsize = AtomicUsize::new(0);
    pub static SATA_FLUSH_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);

    // ── P0: Network and Python/runtime attribution ──
    pub static NET_POLL_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static NET_POLL_PROGRESS: AtomicUsize = AtomicUsize::new(0);
    pub static NET_POLL_LOCK_BUSY: AtomicUsize = AtomicUsize::new(0);
    pub static NET_RX_PACKETS: AtomicUsize = AtomicUsize::new(0);
    pub static NET_RX_BYTES: AtomicUsize = AtomicUsize::new(0);
    pub static NET_RX_DROPS: AtomicUsize = AtomicUsize::new(0);
    pub static NET_TX_SUBMIT_PACKETS: AtomicUsize = AtomicUsize::new(0);
    pub static NET_TX_SUBMIT_BYTES: AtomicUsize = AtomicUsize::new(0);
    pub static NET_TX_DROPS: AtomicUsize = AtomicUsize::new(0);
    pub static NET_TX_DEFERRED_DROPS: AtomicUsize = AtomicUsize::new(0);
    pub static RUNTIME_EXEC_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static RUNTIME_EXEC_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static RUNTIME_OPENAT_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static RUNTIME_READ_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static RUNTIME_MMAP_CALLS: AtomicUsize = AtomicUsize::new(0);
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

    // ── P0: Anonymous private VMA release ──
    // Keep this window bounded to 15 counters.  The scan-step total is the
    // exact number of VecDeque entries visited by the current retain-based
    // release path and is therefore the primary O(N^2) attribution signal.
    pub static ANON_UNMAP_CALLS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static ANON_UNMAP_RANGE_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static ANON_UNMAP_AREA_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static ANON_UNMAP_REQUESTED_PAGES_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static ANON_UNMAP_RESIDENT_PAGES_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static ANON_UNMAP_ACTIVE_BEFORE_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static ANON_UNMAP_ACTIVE_BEFORE_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static ANON_UNMAP_RETAIN_SCAN_STEPS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static ANON_UNMAP_TICKS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static ANON_UNMAP_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);
    pub static ANON_UNMAP_ERRORS_TOTAL: AtomicUsize = AtomicUsize::new(0);
    pub static ANON_UNMAP_PAGES_LE_16: AtomicUsize = AtomicUsize::new(0);
    pub static ANON_UNMAP_PAGES_LE_256: AtomicUsize = AtomicUsize::new(0);
    pub static ANON_UNMAP_PAGES_LE_4096: AtomicUsize = AtomicUsize::new(0);
    pub static ANON_UNMAP_PAGES_GT_4096: AtomicUsize = AtomicUsize::new(0);

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
    pub fn record_taskq_add_ready() {
        if !stats_enabled() {
            return;
        }
        ADD_READY_TOTAL.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_taskq_add_interruptible() {
        if !stats_enabled() {
            return;
        }
        ADD_INTERRUPTIBLE_TOTAL.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_taskq_wake_interruptible() {
        if !stats_enabled() {
            return;
        }
        WAKE_INTERRUPTIBLE_TOTAL.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_taskq_dup_enqueue() {
        if !stats_enabled() {
            return;
        }
        DUPLICATE_READY_ENQUEUE.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_taskq_fetch(fair_pick: bool, scan_depth: usize) {
        if !stats_enabled() {
            return;
        }
        if fair_pick {
            FAIR_PICK_CALLS.fetch_add(1, Ordering::Relaxed);
            update_max(&FAIR_SCAN_MAX, scan_depth);
        } else {
            FAST_PATH_CALLS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_taskq_queue_lens(
        ready: usize,
        interruptible: usize,
        ready_zombie: usize,
        int_zombie: usize,
        nonzero_nice: usize,
    ) {
        if !stats_enabled() {
            return;
        }
        update_max(&READY_LEN_MAX, ready);
        update_max(&INTERRUPTIBLE_LEN_MAX, interruptible);
        update_max(&READY_ZOMBIE_MAX, ready_zombie);
        update_max(&INTERRUPTIBLE_ZOMBIE_MAX, int_zombie);
        READY_NONZERO_NICE_CUR.store(nonzero_nice, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_zombie_drain_full(scan_total: usize, calls: usize, removed: usize) {
        if !stats_enabled() {
            return;
        }
        if scan_total != 0 {
            ZOMBIE_DRAIN_SCAN_TOTAL.fetch_add(scan_total, Ordering::Relaxed);
        }
        if calls != 0 {
            ZOMBIE_DRAIN_CALLS.fetch_add(calls, Ordering::Relaxed);
        }
        if removed != 0 {
            ZOMBIE_DRAIN_REMOVED.fetch_add(removed, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_ktimer_add() {
        if !stats_enabled() {
            return;
        }
        KTIMER_ADD_TOTAL.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ktimer_len(len: usize) {
        if !stats_enabled() {
            return;
        }
        update_max(&KTIMER_LEN_MAX, len);
    }

    #[inline(always)]
    pub fn record_ktimer_pop(pop_count: usize) {
        if !stats_enabled() {
            return;
        }
        KTIMER_POP_TOTAL.fetch_add(1, Ordering::Relaxed);
        update_max(&KTIMER_POP_MAX, pop_count);
    }

    #[inline(always)]
    pub fn record_ktimer_stale_waketask() {
        if !stats_enabled() {
            return;
        }
        KTIMER_STALE_WAKETASK.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ktimer_real_wake() {
        if !stats_enabled() {
            return;
        }
        KTIMER_REAL_WAKE.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ktimer_compact(stale_removed: usize) {
        if !stats_enabled() {
            return;
        }
        KTIMER_COMPACT_CALLS.fetch_add(1, Ordering::Relaxed);
        if stale_removed != 0 {
            KTIMER_STALE_REMOVED.fetch_add(stale_removed, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_wait_with_timeout() {
        if !stats_enabled() {
            return;
        }
        WAIT_WITH_TIMEOUT_TOTAL.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_timer_irq_cost(start: usize) {
        if !stats_enabled() {
            return;
        }
        let elapsed = perf_time_now().wrapping_sub(start);
        TIMER_IRQ_TICKS_TOTAL.fetch_add(elapsed, Ordering::Relaxed);
        update_max(&TIMER_IRQ_TICKS_MAX, elapsed);
    }

    #[inline(always)]
    pub fn record_timer_pop_cost(start: usize, nodes_popped: usize) {
        if !stats_enabled() {
            return;
        }
        let elapsed = perf_time_now().wrapping_sub(start);
        TIMER_POP_NODES_TOTAL.fetch_add(nodes_popped, Ordering::Relaxed);
        TIMER_POP_TICKS_TOTAL.fetch_add(elapsed, Ordering::Relaxed);
        update_max(&TIMER_POP_TICKS_MAX, elapsed);
    }

    #[inline(always)]
    pub fn record_seccomp_check_call() {
        if !stats_enabled() {
            return;
        }
        SECCOMP_CHECK_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_seccomp_check(start: usize, bypass: bool) {
        if !stats_enabled() {
            return;
        }
        let elapsed = perf_time_now().wrapping_sub(start);
        SECCOMP_CHECK_TICKS_TOTAL.fetch_add(elapsed, Ordering::Relaxed);
        update_max(&SECCOMP_CHECK_TICKS_MAX, elapsed);
        if bypass {
            SECCOMP_DISABLED_BYPASS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_seccomp_disabled_bypass() {
        if !stats_enabled() {
            return;
        }
        SECCOMP_DISABLED_BYPASS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_syscall_enter(syscall_id: usize) {
        if stats_enabled() {
            SYSCALL_TOTAL.fetch_add(1, Ordering::Relaxed);
            if syscall_id == 173 {
                SYSCALL_GETPPID_TOTAL.fetch_add(1, Ordering::Relaxed);
            }
        }
        if network_runtime_stats_enabled() {
            match syscall_id {
                56 => {
                    RUNTIME_OPENAT_CALLS.fetch_add(1, Ordering::Relaxed);
                }
                63 => {
                    RUNTIME_READ_CALLS.fetch_add(1, Ordering::Relaxed);
                }
                221 | 281 => {
                    RUNTIME_EXEC_CALLS.fetch_add(1, Ordering::Relaxed);
                }
                222 => {
                    RUNTIME_MMAP_CALLS.fetch_add(1, Ordering::Relaxed);
                }
                _ => {}
            }
        }
    }

    #[inline(always)]
    pub fn record_runtime_exec_cost(syscall_id: usize, ticks: usize) {
        if network_runtime_stats_enabled() && matches!(syscall_id, 221 | 281) {
            RUNTIME_EXEC_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_syscall_cost_ticks(ticks: usize) {
        if !stats_enabled() {
            return;
        }
        SYSCALL_COST_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
        update_max(&SYSCALL_COST_MAX_TICKS, ticks);
    }

    #[inline(always)]
    pub fn record_trap_cost_ticks(ticks: usize) {
        if !stats_enabled() {
            return;
        }
        ECALL_TRAP_COST_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
        update_max(&ECALL_TRAP_COST_TICKS_MAX, ticks);
        update_max(&TRAP_ENTER_COST_MAX_TICKS, ticks);
    }

    #[inline(always)]
    pub fn record_user_unaligned_trap(start: usize, is_store: bool, size: usize, is_float: bool) {
        if !stats_enabled() {
            return;
        }
        let ticks = perf_time_now().wrapping_sub(start);
        USER_UNALIGNED_TRAPS.fetch_add(1, Ordering::Relaxed);
        USER_UNALIGNED_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
        update_max(&USER_UNALIGNED_TICKS_MAX, ticks);
        let class = match (is_store, size) {
            (false, 2) => Some(&USER_UNALIGNED_LOAD_2),
            (false, 4) => Some(&USER_UNALIGNED_LOAD_4),
            (false, 8) => Some(&USER_UNALIGNED_LOAD_8),
            (true, 2) => Some(&USER_UNALIGNED_STORE_2),
            (true, 4) => Some(&USER_UNALIGNED_STORE_4),
            (true, 8) => Some(&USER_UNALIGNED_STORE_8),
            _ => None,
        };
        if let Some(counter) = class {
            counter.fetch_add(1, Ordering::Relaxed);
        }
        if is_float {
            let counter = if is_store {
                &USER_UNALIGNED_FLOAT_STORES
            } else {
                &USER_UNALIGNED_FLOAT_LOADS
            };
            counter.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_getppid_cost(ticks: usize) {
        if !stats_enabled() {
            return;
        }
        GETPPID_COST_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
        update_max(&GETPPID_COST_TICKS_MAX, ticks);
    }

    #[inline(always)]
    pub fn record_context_switch() {
        if !stats_enabled() {
            return;
        }
        CONTEXT_SWITCH_TOTAL.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_reclaim_run() {
        if !memory_io_stats_enabled() {
            return;
        }
        RECLAIM_RUNS_TOTAL.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_reclaim_pages_scanned(n: usize) {
        if n == 0 {
            return;
        }
        if !memory_io_stats_enabled() {
            return;
        }
        RECLAIM_PAGES_SCANNED_TOTAL.fetch_add(n, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_reclaim_pages_freed(n: usize) {
        if n == 0 {
            return;
        }
        if !memory_io_stats_enabled() {
            return;
        }
        RECLAIM_PAGES_FREED_TOTAL.fetch_add(n, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_clock_scanned(n: usize) {
        if n == 0 {
            return;
        }
        if !memory_io_stats_enabled() {
            return;
        }
        CLOCK_SCANNED.fetch_add(n, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_clock_second_chance(n: usize) {
        if n == 0 {
            return;
        }
        if !memory_io_stats_enabled() {
            return;
        }
        CLOCK_SECOND_CHANCE.fetch_add(n, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_clock_evicted(n: usize) {
        if n == 0 {
            return;
        }
        if !memory_io_stats_enabled() {
            return;
        }
        CLOCK_EVICTED.fetch_add(n, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_heap_alloc() {
        if !memory_io_stats_enabled() {
            return;
        }
        HEAP_ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_heap_alloc_cost(ticks: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        HEAP_ALLOC_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
        update_max(&HEAP_ALLOC_TICKS_MAX, ticks);
    }

    #[inline(always)]
    pub fn record_heap_dealloc() {
        if !memory_io_stats_enabled() {
            return;
        }
        HEAP_DEALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_heap_dealloc_cost(ticks: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        HEAP_DEALLOC_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
        update_max(&HEAP_DEALLOC_TICKS_MAX, ticks);
    }

    #[inline(always)]
    pub fn record_heap_dealloc_scan_steps(steps: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        HEAP_DEALLOC_SCAN_STEPS_TOTAL.fetch_add(steps, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_anon_unmap(
        range_release: bool,
        requested_pages: usize,
        resident_pages: usize,
        active_before: usize,
        retain_scan_steps: usize,
        start_ticks: usize,
        failed: bool,
    ) {
        if !memory_io_stats_enabled() {
            return;
        }
        let elapsed = perf_time_now_for(super::STATS_PROFILE_MEMORY_IO).wrapping_sub(start_ticks);
        ANON_UNMAP_CALLS_TOTAL.fetch_add(1, Ordering::Relaxed);
        if range_release {
            ANON_UNMAP_RANGE_CALLS.fetch_add(1, Ordering::Relaxed);
        } else {
            ANON_UNMAP_AREA_CALLS.fetch_add(1, Ordering::Relaxed);
        }
        ANON_UNMAP_REQUESTED_PAGES_TOTAL.fetch_add(requested_pages, Ordering::Relaxed);
        ANON_UNMAP_RESIDENT_PAGES_TOTAL.fetch_add(resident_pages, Ordering::Relaxed);
        ANON_UNMAP_ACTIVE_BEFORE_TOTAL.fetch_add(active_before, Ordering::Relaxed);
        update_max(&ANON_UNMAP_ACTIVE_BEFORE_MAX, active_before);
        ANON_UNMAP_RETAIN_SCAN_STEPS_TOTAL.fetch_add(retain_scan_steps, Ordering::Relaxed);
        ANON_UNMAP_TICKS_TOTAL.fetch_add(elapsed, Ordering::Relaxed);
        update_max(&ANON_UNMAP_TICKS_MAX, elapsed);
        if failed {
            ANON_UNMAP_ERRORS_TOTAL.fetch_add(1, Ordering::Relaxed);
        }
        let bucket = if resident_pages <= 16 {
            &ANON_UNMAP_PAGES_LE_16
        } else if resident_pages <= 256 {
            &ANON_UNMAP_PAGES_LE_256
        } else if resident_pages <= 4096 {
            &ANON_UNMAP_PAGES_LE_4096
        } else {
            &ANON_UNMAP_PAGES_GT_4096
        };
        bucket.fetch_add(1, Ordering::Relaxed);
    }

    // ── PageCache recorders ──

    #[inline(always)]
    pub fn record_pc_read(pages: usize, cycles: usize, hit_cycles: usize, miss_cycles: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        PC_READ_CALLS.fetch_add(1, Ordering::Relaxed);
        PC_READ_PAGES.fetch_add(pages, Ordering::Relaxed);
        PC_READ_CYCLES_TOTAL.fetch_add(cycles, Ordering::Relaxed);
        PC_READ_HIT_CYCLES.fetch_add(hit_cycles, Ordering::Relaxed);
        PC_READ_MISS_CYCLES.fetch_add(miss_cycles, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_pc_read_user(pages: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        PC_READ_USER_CALLS.fetch_add(1, Ordering::Relaxed);
        PC_READ_USER_PAGES.fetch_add(pages, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_pc_read_lookup_cycles(cycles: usize) {
        if memory_io_stats_enabled() {
            PC_READ_LOOKUP_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_pc_read_miss_fill_cycles(cycles: usize) {
        if memory_io_stats_enabled() {
            PC_READ_MISS_FILL_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_pc_read_valid_fill_cycles(cycles: usize) {
        if memory_io_stats_enabled() {
            PC_READ_VALID_FILL_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_pc_read_copy_cycles(cycles: usize) {
        if memory_io_stats_enabled() {
            PC_READ_COPY_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_pc_copy_cycles(cycles: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        PC_COPY_CYCLES.fetch_add(cycles, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_pc_lookup_cycles(cycles: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        PC_LOOKUP_CYCLES.fetch_add(cycles, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_pc_write(pages: usize, full_overwrite: bool, cycles: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        PC_WRITE_CALLS.fetch_add(1, Ordering::Relaxed);
        PC_WRITE_PAGES.fetch_add(pages, Ordering::Relaxed);
        if full_overwrite {
            PC_WRITE_OVERWRITE.fetch_add(1, Ordering::Relaxed);
        }
        PC_WRITE_CYCLES_TOTAL.fetch_add(cycles, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_pc_write_lookup(cycles: usize) {
        if memory_io_stats_enabled() {
            PC_WRITE_LOOKUP_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_pc_write_copy(cycles: usize) {
        if memory_io_stats_enabled() {
            PC_WRITE_COPY_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_pc_write_commit(cycles: usize) {
        if memory_io_stats_enabled() {
            PC_WRITE_COMMIT_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_pc_write_stages(lookup: usize, lease: usize, copy: usize, commit: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        PC_WRITE_LOOKUP_CYCLES.fetch_add(lookup, Ordering::Relaxed);
        PC_WRITE_LEASE_CYCLES.fetch_add(lease, Ordering::Relaxed);
        PC_WRITE_COPY_CYCLES.fetch_add(copy, Ordering::Relaxed);
        PC_WRITE_COMMIT_CYCLES.fetch_add(commit, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_pc_write_eventually_full() {
        if !memory_io_stats_enabled() {
            return;
        }
        PC_WRITE_EVENTUALLY_FULL.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_pc_writeback(pages: usize, cycles: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        PC_WRITEBACK_CALLS.fetch_add(1, Ordering::Relaxed);
        PC_WRITEBACK_PAGES.fetch_add(pages, Ordering::Relaxed);
        PC_WRITEBACK_CYCLES_TOTAL.fetch_add(cycles, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_pc_miss() {
        if !memory_io_stats_enabled() {
            return;
        }
        PC_READ_MISS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_pc_falloc_cycles(cycles: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        PC_FALLOC_CYCLES_TOTAL.fetch_add(cycles, Ordering::Relaxed);
    }

    #[inline(always)]
    #[inline(always)]
    pub fn record_pread_uaccess(cycles: usize) {
        if memory_io_stats_enabled() {
            PREAD_UACCESS_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_pwrite_uaccess(cycles: usize) {
        if memory_io_stats_enabled() {
            PWRITE_UACCESS_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_pread_file(cycles: usize) {
        if memory_io_stats_enabled() {
            PREAD_FILE_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_pwrite_file(cycles: usize) {
        if memory_io_stats_enabled() {
            PWRITE_FILE_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_pread_ext4_logical_size(cycles: usize) {
        if memory_io_stats_enabled() {
            PREAD_EXT4_LOGICAL_SIZE_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_pwrite_ext4_setup(cycles: usize) {
        if memory_io_stats_enabled() {
            PWRITE_EXT4_SETUP_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_pread_ext4_page_cache(cycles: usize) {
        if memory_io_stats_enabled() {
            PREAD_EXT4_PAGE_CACHE_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_pwrite_ext4_post(cycles: usize) {
        if memory_io_stats_enabled() {
            PWRITE_EXT4_POST_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_pread_total_count() {
        if memory_io_stats_enabled() {
            PREAD_TOTAL_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_pwrite_total_count() {
        if memory_io_stats_enabled() {
            PWRITE_TOTAL_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_pread_vfs_mode(cycles: usize) {
        if memory_io_stats_enabled() {
            PREAD_VFS_MODE_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_pwrite_vfs_mode(cycles: usize) {
        if memory_io_stats_enabled() {
            PWRITE_VFS_MODE_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_pwrite_vfs_seals(cycles: usize) {
        if memory_io_stats_enabled() {
            PWRITE_VFS_SEALS_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_pwrite_vfs_touch(cycles: usize) {
        if memory_io_stats_enabled() {
            PWRITE_VFS_TOUCH_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_pwrite_mount_writable(cycles: usize) {
        if memory_io_stats_enabled() {
            PWRITE_MOUNT_WRITABLE_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_write_fd_prep(cycles: usize) {
        if memory_io_stats_enabled() {
            WRITE_FD_PREP_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_write_uaccess(cycles: usize) {
        if memory_io_stats_enabled() {
            WRITE_UACCESS_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_write_file(cycles: usize) {
        if memory_io_stats_enabled() {
            WRITE_FILE_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_write_vfs_mode(cycles: usize) {
        if memory_io_stats_enabled() {
            WRITE_VFS_MODE_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_write_vfs_seals(cycles: usize) {
        if memory_io_stats_enabled() {
            WRITE_VFS_SEALS_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_write_offset(cycles: usize) {
        if memory_io_stats_enabled() {
            WRITE_OFFSET_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_write_total_count() {
        if memory_io_stats_enabled() {
            WRITE_TOTAL_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_wb_bg_call() {
        if !memory_io_stats_enabled() {
            return;
        }
        WB_BG_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_wb_throttle_call() {
        if !memory_io_stats_enabled() {
            return;
        }
        WB_THROTTLE_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_wb_redirty() {
        if !memory_io_stats_enabled() {
            return;
        }
        WB_REDIRTY_PAGES.fetch_add(1, Ordering::Relaxed);
    }

    // ── Block device recorders ──

    #[inline(always)]
    pub fn record_blk_vread(sectors: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        BLK_VREAD_REQS.fetch_add(1, Ordering::Relaxed);
        BLK_VREAD_SECS.fetch_add(sectors, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_blk_vwrite(sectors: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        BLK_VWRITE_REQS.fetch_add(1, Ordering::Relaxed);
        BLK_VWRITE_SECS.fetch_add(sectors, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_journal_commit(bytes: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        JOURNAL_COMMIT_COUNT.fetch_add(1, Ordering::Relaxed);
        JOURNAL_COMMIT_BYTES.fetch_add(bytes, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_device_flush() {
        if !memory_io_stats_enabled() {
            return;
        }
        DEVICE_FLUSH_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_virtio_write(bytes: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        VIRTIO_WRITE_REQUESTS.fetch_add(1, Ordering::Relaxed);
        VIRTIO_WRITE_BYTES.fetch_add(bytes, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_virtio_read() {
        if !memory_io_stats_enabled() {
            return;
        }
        VIRTIO_READ_REQUESTS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_writeback_batch(pages: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        WRITEBACK_BATCH_COUNT.fetch_add(1, Ordering::Relaxed);
        WRITEBACK_PAGE_COUNT.fetch_add(pages, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_wb_tx_data_write(bytes: usize, ticks: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        WB_TX_DATA_WRITE_CALLS.fetch_add(1, Ordering::Relaxed);
        WB_TX_DATA_WRITE_BYTES.fetch_add(bytes, Ordering::Relaxed);
        WB_TX_DATA_WRITE_TICKS.fetch_add(ticks, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_wb_tx_alloc_extent(pages: usize, ticks: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        WB_TX_ALLOC_EXTENT_CALLS.fetch_add(1, Ordering::Relaxed);
        WB_TX_ALLOC_EXTENT_PAGES.fetch_add(pages, Ordering::Relaxed);
        WB_TX_ALLOC_EXTENT_TICKS.fetch_add(ticks, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_wb_tx_journal_commit(transaction_id: u32, staged_blocks: usize, ticks: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        let _ = WB_TX_JOURNAL_TX_FIRST.compare_exchange(
            0,
            transaction_id as usize,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
        WB_TX_JOURNAL_TX_LAST.store(transaction_id as usize, Ordering::Relaxed);
        WB_TX_JOURNAL_STAGED_BLOCKS.fetch_add(staged_blocks, Ordering::Relaxed);
        WB_TX_JOURNAL_COMMIT_TICKS.fetch_add(ticks, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_wb_tx_journal_flush(ticks: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        WB_TX_JOURNAL_FLUSH_COUNT.fetch_add(1, Ordering::Relaxed);
        WB_TX_JOURNAL_FLUSH_TICKS.fetch_add(ticks, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_wb_tx_boundary_flush(ticks: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        WB_TX_BOUNDARY_FLUSH_COUNT.fetch_add(1, Ordering::Relaxed);
        WB_TX_BOUNDARY_FLUSH_TICKS.fetch_add(ticks, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_virtio_blk_read_chunk(bytes: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        VIRTIO_BLK_READ_CHUNKS.fetch_add(1, Ordering::Relaxed);
        VIRTIO_BLK_READ_BYTES.fetch_add(bytes, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_virtio_blk_write_chunk(bytes: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        VIRTIO_BLK_WRITE_CHUNKS.fetch_add(1, Ordering::Relaxed);
        VIRTIO_BLK_WRITE_BYTES.fetch_add(bytes, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_virtio_dma_pool_reserve(success: bool) {
        if !memory_io_stats_enabled() {
            return;
        }
        if success {
            VIRTIO_DMA_POOL_RESERVE_SUCCESS.fetch_add(1, Ordering::Relaxed);
        } else {
            VIRTIO_DMA_POOL_RESERVE_FAIL.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_virtio_dma_pool_consume() {
        if memory_io_stats_enabled() {
            VIRTIO_DMA_POOL_CONSUME.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_virtio_dma_pool_cancel() {
        if memory_io_stats_enabled() {
            VIRTIO_DMA_POOL_CANCEL.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_virtio_dma_pool_finish() {
        if memory_io_stats_enabled() {
            VIRTIO_DMA_POOL_FINISH.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// `kind`: 0=data pool, 1=data fallback, 2=header, 3=status,
    /// 4=indirect descriptor table, 5=other fallback, 6=header pool,
    /// 7=status pool, 8=indirect descriptor pool.
    #[inline(always)]
    pub fn record_virtio_dma_share(kind: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        VIRTIO_DMA_SHARE_CALLS.fetch_add(1, Ordering::Relaxed);
        match kind {
            0 => VIRTIO_DMA_SHARE_DATA_POOL.fetch_add(1, Ordering::Relaxed),
            1 => VIRTIO_DMA_SHARE_DATA_FALLBACK.fetch_add(1, Ordering::Relaxed),
            2 => VIRTIO_DMA_SHARE_HEADER_FALLBACK.fetch_add(1, Ordering::Relaxed),
            3 => VIRTIO_DMA_SHARE_STATUS_FALLBACK.fetch_add(1, Ordering::Relaxed),
            4 => VIRTIO_DMA_SHARE_INDIRECT_FALLBACK.fetch_add(1, Ordering::Relaxed),
            6 => VIRTIO_DMA_SHARE_HEADER_POOL.fetch_add(1, Ordering::Relaxed),
            7 => VIRTIO_DMA_SHARE_STATUS_POOL.fetch_add(1, Ordering::Relaxed),
            8 => VIRTIO_DMA_SHARE_INDIRECT_POOL.fetch_add(1, Ordering::Relaxed),
            _ => VIRTIO_DMA_SHARE_OTHER_FALLBACK.fetch_add(1, Ordering::Relaxed),
        };
    }

    #[inline(always)]
    pub fn record_virtio_dma_bridge_lock(wait_ticks: usize, hold_ticks: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        VIRTIO_DMA_BRIDGE_LOCK_WAIT_TICKS_TOTAL.fetch_add(wait_ticks, Ordering::Relaxed);
        VIRTIO_DMA_BRIDGE_LOCK_HOLD_TICKS_TOTAL.fetch_add(hold_ticks, Ordering::Relaxed);
        update_max(&VIRTIO_DMA_BRIDGE_LOCK_HOLD_TICKS_MAX, hold_ticks);
    }

    #[inline(always)]
    pub fn record_sata_read(bytes: usize, ticks: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        SATA_READ_REQS.fetch_add(1, Ordering::Relaxed);
        SATA_READ_BYTES.fetch_add(bytes, Ordering::Relaxed);
        SATA_READ_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
        update_max(&SATA_READ_TICKS_MAX, ticks);
    }

    #[inline(always)]
    pub fn record_sata_write(bytes: usize, ticks: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        SATA_WRITE_REQS.fetch_add(1, Ordering::Relaxed);
        SATA_WRITE_BYTES.fetch_add(bytes, Ordering::Relaxed);
        SATA_WRITE_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
        update_max(&SATA_WRITE_TICKS_MAX, ticks);
    }

    #[inline(always)]
    pub fn record_sata_flush(ticks: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        SATA_FLUSH_REQS.fetch_add(1, Ordering::Relaxed);
        SATA_FLUSH_TICKS_TOTAL.fetch_add(ticks, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_net_poll(progressed: bool, lock_busy: bool) {
        if !network_runtime_stats_enabled() {
            return;
        }
        NET_POLL_CALLS.fetch_add(1, Ordering::Relaxed);
        if progressed {
            NET_POLL_PROGRESS.fetch_add(1, Ordering::Relaxed);
        }
        if lock_busy {
            NET_POLL_LOCK_BUSY.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_net_rx(bytes: usize) {
        if !network_runtime_stats_enabled() {
            return;
        }
        NET_RX_PACKETS.fetch_add(1, Ordering::Relaxed);
        NET_RX_BYTES.fetch_add(bytes, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_net_rx_drop() {
        if network_runtime_stats_enabled() {
            NET_RX_DROPS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_net_tx_submit(bytes: usize) {
        if !network_runtime_stats_enabled() {
            return;
        }
        NET_TX_SUBMIT_PACKETS.fetch_add(1, Ordering::Relaxed);
        NET_TX_SUBMIT_BYTES.fetch_add(bytes, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_net_tx_drop() {
        if network_runtime_stats_enabled() {
            NET_TX_DROPS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_net_tx_deferred_dropped() {
        if network_runtime_stats_enabled() {
            NET_TX_DEFERRED_DROPS.fetch_add(1, Ordering::Relaxed);
        }
    }

    // ── Ext4 Block Mapping recorders ──

    #[inline(always)]
    pub fn record_ext4_map_lblock() {
        if !stats_enabled() {
            return;
        }
        EXT4_MAP_LBLOCK_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ext4_map_lblock_cost(cycles: usize) {
        if !stats_enabled() {
            return;
        }
        EXT4_MAP_LBLOCK_CYCLES.fetch_add(cycles, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ext4_map_cache_hit() {
        if !stats_enabled() {
            return;
        }
        EXT4_MAP_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ext4_map_hole() {
        if !stats_enabled() {
            return;
        }
        EXT4_MAP_HOLES.fetch_add(1, Ordering::Relaxed);
    }

    // ── Ext4 Extent Tree Search recorders ──

    #[inline(always)]
    pub fn record_ext4_find_extent_call() {
        if !stats_enabled() {
            return;
        }
        EXT4_FIND_EXTENT_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ext4_find_extent_cost(cycles: usize, depth: usize, meta_reads: usize) {
        if !stats_enabled() {
            return;
        }
        EXT4_FIND_EXTENT_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        EXT4_FIND_EXTENT_DEPTH_SUM.fetch_add(depth, Ordering::Relaxed);
        EXT4_FIND_EXTENT_META_READS.fetch_add(meta_reads, Ordering::Relaxed);
    }

    // ── Ext4 PageCache Backend Batch recorders ──

    #[inline(always)]
    pub fn record_ext4_pc_readpages_calls() {
        if !stats_enabled() {
            return;
        }
        EXT4_PC_READPAGES_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ext4_pc_readpages_pages(n: usize) {
        if !stats_enabled() {
            return;
        }
        EXT4_PC_READPAGES_PAGES.fetch_add(n, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ext4_pc_readpages_runs(n: usize) {
        if !stats_enabled() {
            return;
        }
        EXT4_PC_READPAGES_RUNS.fetch_add(n, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ext4_pc_writepages_calls() {
        if !stats_enabled() {
            return;
        }
        EXT4_PC_WRITEPAGES_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ext4_pc_writepages_pages(n: usize) {
        if !stats_enabled() {
            return;
        }
        EXT4_PC_WRITEPAGES_PAGES.fetch_add(n, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ext4_pc_writepages_runs(n: usize) {
        if !stats_enabled() {
            return;
        }
        EXT4_PC_WRITEPAGES_RUNS.fetch_add(n, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ext4_pc_512b_fallback(n: usize) {
        if !stats_enabled() {
            return;
        }
        EXT4_PC_512B_FALLBACK_PAGES.fetch_add(n, Ordering::Relaxed);
    }

    // ── Ext4 Allocation recorders ──

    #[inline(always)]
    pub fn record_ext4_alloc_ensure_calls() {
        if !stats_enabled() {
            return;
        }
        EXT4_ALLOC_ENSURE_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_ext4_alloc_ensure(lblocks: usize, new_blocks: usize, cycles: usize) {
        if !stats_enabled() {
            return;
        }
        EXT4_ALLOC_LBLOCKS.fetch_add(lblocks, Ordering::Relaxed);
        EXT4_ALLOC_NEW_BLOCKS.fetch_add(new_blocks, Ordering::Relaxed);
        EXT4_ALLOC_CYCLES.fetch_add(cycles, Ordering::Relaxed);
    }

    // ── PageCache Lock Contention recorders ──

    #[inline(always)]
    pub fn record_pc_lock_hold(cycles: usize, io_miss: bool) {
        if !stats_enabled() {
            return;
        }
        PC_LOCK_HOLD_CYCLES.fetch_add(cycles, Ordering::Relaxed);
        update_max(&PC_LOCK_HOLD_MAX, cycles);
        if io_miss {
            PC_LOCK_IO_MISS_READS.fetch_add(1, Ordering::Relaxed);
        }
    }

    // ── Ext4 direct write_at recorder ──

    #[inline(always)]
    pub fn record_ext4_direct_write_at() {
        if !stats_enabled() {
            return;
        }
        EXT4_DIRECT_WRITE_AT_CALLS.fetch_add(1, Ordering::Relaxed);
    }

    /// Reset all P0+P1 performance counters (writable via /sys/kernel/stats/reset).
    pub fn reset_all_counters() {
        // Legacy lifecycle counters used by the existing snapshot formatter.
        CLONE_TOTAL.store(0, Ordering::Relaxed);
        CLONE_THREAD.store(0, Ordering::Relaxed);
        CLONE_SHARE_VM.store(0, Ordering::Relaxed);
        CLONE_STACK_ALLOC.store(0, Ordering::Relaxed);
        EXIT_THREAD.store(0, Ordering::Relaxed);
        EXIT_CLEAR_CHILD_TID.store(0, Ordering::Relaxed);
        EXIT_KEEP_TRAP.store(0, Ordering::Relaxed);
        TRAP_CACHE_STORE.store(0, Ordering::Relaxed);
        TRAP_CACHE_SKIP.store(0, Ordering::Relaxed);
        TRAP_CACHE_HIT.store(0, Ordering::Relaxed);
        TRAP_CACHE_MISS.store(0, Ordering::Relaxed);
        KSTACK_CACHE_HIT.store(0, Ordering::Relaxed);
        KSTACK_CACHE_MISS.store(0, Ordering::Relaxed);
        KSTACK_CACHE_STORE.store(0, Ordering::Relaxed);
        KSTACK_CACHE_DROP.store(0, Ordering::Relaxed);
        ZOMBIE_ENQUEUE.store(0, Ordering::Relaxed);
        ZOMBIE_DRAIN.store(0, Ordering::Relaxed);
        SCHEDULE_LOOPS.store(0, Ordering::Relaxed);
        SCHEDULE_FETCH.store(0, Ordering::Relaxed);
        SCHEDULE_IDLE.store(0, Ordering::Relaxed);
        TIMER_INTERRUPTS.store(0, Ordering::Relaxed);
        TIMER_SNAPSHOT_EPOCH.store(0, Ordering::Relaxed);
        FUTEX_WAIT.store(0, Ordering::Relaxed);
        FUTEX_WAIT_SHARED.store(0, Ordering::Relaxed);
        FUTEX_WAIT_DEADLINE.store(0, Ordering::Relaxed);
        FUTEX_WAIT_READY.store(0, Ordering::Relaxed);
        FUTEX_WAIT_TIMEOUT.store(0, Ordering::Relaxed);
        FUTEX_WAIT_INTR.store(0, Ordering::Relaxed);
        FUTEX_WAKE.store(0, Ordering::Relaxed);
        FUTEX_WAKE_SHARED.store(0, Ordering::Relaxed);
        FUTEX_WAKE_HIT.store(0, Ordering::Relaxed);
        SYSCALL_CLONE.store(0, Ordering::Relaxed);
        SYSCALL_FUTEX.store(0, Ordering::Relaxed);
        SYSCALL_MMAP.store(0, Ordering::Relaxed);
        SYSCALL_MUNMAP.store(0, Ordering::Relaxed);
        SYSCALL_MPROTECT.store(0, Ordering::Relaxed);
        SYSCALL_SET_TID_ADDRESS.store(0, Ordering::Relaxed);
        SYSCALL_SET_ROBUST_LIST.store(0, Ordering::Relaxed);
        SYSCALL_EXIT.store(0, Ordering::Relaxed);
        SYSCALL_YIELD.store(0, Ordering::Relaxed);
        LAST_SYSCALL_ID.store(0, Ordering::Relaxed);
        LAST_SYSCALL_RET.store(0, Ordering::Relaxed);
        TLB_FLUSHES.store(0, Ordering::Relaxed);
        TLB_FULL.store(0, Ordering::Relaxed);
        TLB_PAGE.store(0, Ordering::Relaxed);
        TLB_ACTIVATE.store(0, Ordering::Relaxed);
        TLB_GLOBAL.store(0, Ordering::Relaxed);
        FRAME_ALLOC_HITS.store(0, Ordering::Relaxed);
        FRAME_FREE_HITS.store(0, Ordering::Relaxed);
        PAGE_FAULTS.store(0, Ordering::Relaxed);
        VFS_LOOKUPS.store(0, Ordering::Relaxed);
        VFS_LOOKUP_TIME_TICKS.store(0, Ordering::Relaxed);
        VFS_LOOKUP_TIME_COUNT.store(0, Ordering::Relaxed);
        CLONE_TIME_TICKS.store(0, Ordering::Relaxed);
        PAGEFAULT_TIME_TICKS.store(0, Ordering::Relaxed);
        FRAME_ALLOC_TIME_TICKS.store(0, Ordering::Relaxed);
        CLONE_TIME_COUNT.store(0, Ordering::Relaxed);
        PAGEFAULT_TIME_COUNT.store(0, Ordering::Relaxed);
        FRAME_ALLOC_TIME_COUNT.store(0, Ordering::Relaxed);
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
        USER_UNALIGNED_TRAPS.store(0, Ordering::Relaxed);
        USER_UNALIGNED_TICKS_TOTAL.store(0, Ordering::Relaxed);
        USER_UNALIGNED_TICKS_MAX.store(0, Ordering::Relaxed);
        USER_UNALIGNED_LOAD_2.store(0, Ordering::Relaxed);
        USER_UNALIGNED_LOAD_4.store(0, Ordering::Relaxed);
        USER_UNALIGNED_LOAD_8.store(0, Ordering::Relaxed);
        USER_UNALIGNED_STORE_2.store(0, Ordering::Relaxed);
        USER_UNALIGNED_STORE_4.store(0, Ordering::Relaxed);
        USER_UNALIGNED_STORE_8.store(0, Ordering::Relaxed);
        USER_UNALIGNED_FLOAT_LOADS.store(0, Ordering::Relaxed);
        USER_UNALIGNED_FLOAT_STORES.store(0, Ordering::Relaxed);
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
        // Anonymous private VMA release (15-counter memory window)
        ANON_UNMAP_CALLS_TOTAL.store(0, Ordering::Relaxed);
        ANON_UNMAP_RANGE_CALLS.store(0, Ordering::Relaxed);
        ANON_UNMAP_AREA_CALLS.store(0, Ordering::Relaxed);
        ANON_UNMAP_REQUESTED_PAGES_TOTAL.store(0, Ordering::Relaxed);
        ANON_UNMAP_RESIDENT_PAGES_TOTAL.store(0, Ordering::Relaxed);
        ANON_UNMAP_ACTIVE_BEFORE_TOTAL.store(0, Ordering::Relaxed);
        ANON_UNMAP_ACTIVE_BEFORE_MAX.store(0, Ordering::Relaxed);
        ANON_UNMAP_RETAIN_SCAN_STEPS_TOTAL.store(0, Ordering::Relaxed);
        ANON_UNMAP_TICKS_TOTAL.store(0, Ordering::Relaxed);
        ANON_UNMAP_TICKS_MAX.store(0, Ordering::Relaxed);
        ANON_UNMAP_ERRORS_TOTAL.store(0, Ordering::Relaxed);
        ANON_UNMAP_PAGES_LE_16.store(0, Ordering::Relaxed);
        ANON_UNMAP_PAGES_LE_256.store(0, Ordering::Relaxed);
        ANON_UNMAP_PAGES_LE_4096.store(0, Ordering::Relaxed);
        ANON_UNMAP_PAGES_GT_4096.store(0, Ordering::Relaxed);
        // PageCache I/O (P0)
        PC_READ_CALLS.store(0, Ordering::Relaxed);
        PC_READ_PAGES.store(0, Ordering::Relaxed);
        PC_READ_USER_CALLS.store(0, Ordering::Relaxed);
        PC_READ_USER_PAGES.store(0, Ordering::Relaxed);
        PC_READ_MISS.store(0, Ordering::Relaxed);
        PC_READ_CYCLES_TOTAL.store(0, Ordering::Relaxed);
        PC_READ_HIT_CYCLES.store(0, Ordering::Relaxed);
        PC_READ_MISS_CYCLES.store(0, Ordering::Relaxed);
        PC_READ_LOOKUP_CYCLES.store(0, Ordering::Relaxed);
        PC_READ_MISS_FILL_CYCLES.store(0, Ordering::Relaxed);
        PC_READ_VALID_FILL_CYCLES.store(0, Ordering::Relaxed);
        PC_READ_COPY_CYCLES.store(0, Ordering::Relaxed);
        PC_COPY_CYCLES.store(0, Ordering::Relaxed);
        PC_LOOKUP_CYCLES.store(0, Ordering::Relaxed);
        PC_WRITE_CALLS.store(0, Ordering::Relaxed);
        PC_WRITE_PAGES.store(0, Ordering::Relaxed);
        PC_WRITE_OVERWRITE.store(0, Ordering::Relaxed);
        PC_WRITE_EVENTUALLY_FULL.store(0, Ordering::Relaxed);
        PC_WRITE_CYCLES_TOTAL.store(0, Ordering::Relaxed);
        PC_WRITE_LOOKUP_CYCLES.store(0, Ordering::Relaxed);
        PC_WRITE_LEASE_CYCLES.store(0, Ordering::Relaxed);
        PC_WRITE_COPY_CYCLES.store(0, Ordering::Relaxed);
        PC_WRITE_COMMIT_CYCLES.store(0, Ordering::Relaxed);
        PC_WRITEBACK_CALLS.store(0, Ordering::Relaxed);
        PC_WRITEBACK_PAGES.store(0, Ordering::Relaxed);
        PC_WRITEBACK_CYCLES_TOTAL.store(0, Ordering::Relaxed);
        PC_FALLOC_CYCLES_TOTAL.store(0, Ordering::Relaxed);
        PWRITE_UACCESS_CYCLES.store(0, Ordering::Relaxed);
        PWRITE_FILE_CYCLES.store(0, Ordering::Relaxed);
        PWRITE_EXT4_SETUP_CYCLES.store(0, Ordering::Relaxed);
        PWRITE_EXT4_POST_CYCLES.store(0, Ordering::Relaxed);
        PWRITE_TOTAL_COUNT.store(0, Ordering::Relaxed);
        PWRITE_VFS_MODE_CYCLES.store(0, Ordering::Relaxed);
        PWRITE_VFS_SEALS_CYCLES.store(0, Ordering::Relaxed);
        PWRITE_VFS_TOUCH_CYCLES.store(0, Ordering::Relaxed);
        PWRITE_MOUNT_WRITABLE_CYCLES.store(0, Ordering::Relaxed);
        WRITE_FD_PREP_CYCLES.store(0, Ordering::Relaxed);
        WRITE_UACCESS_CYCLES.store(0, Ordering::Relaxed);
        WRITE_FILE_CYCLES.store(0, Ordering::Relaxed);
        WRITE_VFS_MODE_CYCLES.store(0, Ordering::Relaxed);
        WRITE_VFS_SEALS_CYCLES.store(0, Ordering::Relaxed);
        WRITE_OFFSET_CYCLES.store(0, Ordering::Relaxed);
        WRITE_TOTAL_COUNT.store(0, Ordering::Relaxed);
        PREAD_UACCESS_CYCLES.store(0, Ordering::Relaxed);
        PREAD_FILE_CYCLES.store(0, Ordering::Relaxed);
        PREAD_EXT4_LOGICAL_SIZE_CYCLES.store(0, Ordering::Relaxed);
        PREAD_EXT4_PAGE_CACHE_CYCLES.store(0, Ordering::Relaxed);
        PREAD_TOTAL_COUNT.store(0, Ordering::Relaxed);
        PREAD_VFS_MODE_CYCLES.store(0, Ordering::Relaxed);
        JOURNAL_COMMIT_COUNT.store(0, Ordering::Relaxed);
        JOURNAL_COMMIT_BYTES.store(0, Ordering::Relaxed);
        WB_DATA_WRITE_BYTES.store(0, Ordering::Relaxed);
        WB_DATA_WRITE_CYCLES.store(0, Ordering::Relaxed);
        WB_ALLOC_EXTENT_PAGES.store(0, Ordering::Relaxed);
        WB_ALLOC_EXTENT_CYCLES.store(0, Ordering::Relaxed);
        WB_TX_JOURNAL_COMMIT_TICKS.store(0, Ordering::Relaxed);
        WB_TX_JOURNAL_STAGED_BLOCKS.store(0, Ordering::Relaxed);
        WB_TX_JOURNAL_TX_FIRST.store(0, Ordering::Relaxed);
        WB_TX_JOURNAL_TX_LAST.store(0, Ordering::Relaxed);
        WB_TX_JOURNAL_FLUSH_COUNT.store(0, Ordering::Relaxed);
        WB_TX_JOURNAL_FLUSH_TICKS.store(0, Ordering::Relaxed);
        WB_FLUSH_BOUNDARY_COUNT.store(0, Ordering::Relaxed);
        WB_FLUSH_BOUNDARY_TICKS.store(0, Ordering::Relaxed);
        // Writeback Throttling (P0)
        WB_BG_CALLS.store(0, Ordering::Relaxed);
        WB_THROTTLE_CALLS.store(0, Ordering::Relaxed);
        WB_REDIRTY_PAGES.store(0, Ordering::Relaxed);
        // Block Device I/O (P0)
        BLK_VREAD_REQS.store(0, Ordering::Relaxed);
        BLK_VREAD_SECS.store(0, Ordering::Relaxed);
        BLK_VWRITE_REQS.store(0, Ordering::Relaxed);
        BLK_VWRITE_SECS.store(0, Ordering::Relaxed);
        JOURNAL_COMMIT_COUNT.store(0, Ordering::Relaxed);
        JOURNAL_COMMIT_BYTES.store(0, Ordering::Relaxed);
        DEVICE_FLUSH_COUNT.store(0, Ordering::Relaxed);
        VIRTIO_WRITE_REQUESTS.store(0, Ordering::Relaxed);
        VIRTIO_WRITE_BYTES.store(0, Ordering::Relaxed);
        VIRTIO_READ_REQUESTS.store(0, Ordering::Relaxed);
        WRITEBACK_BATCH_COUNT.store(0, Ordering::Relaxed);
        WRITEBACK_PAGE_COUNT.store(0, Ordering::Relaxed);
        WB_TX_DATA_WRITE_CALLS.store(0, Ordering::Relaxed);
        WB_TX_DATA_WRITE_BYTES.store(0, Ordering::Relaxed);
        WB_TX_DATA_WRITE_TICKS.store(0, Ordering::Relaxed);
        WB_TX_ALLOC_EXTENT_CALLS.store(0, Ordering::Relaxed);
        WB_TX_ALLOC_EXTENT_PAGES.store(0, Ordering::Relaxed);
        WB_TX_ALLOC_EXTENT_TICKS.store(0, Ordering::Relaxed);
        WB_TX_JOURNAL_COMMIT_TICKS.store(0, Ordering::Relaxed);
        WB_TX_JOURNAL_STAGED_BLOCKS.store(0, Ordering::Relaxed);
        WB_TX_JOURNAL_TX_FIRST.store(0, Ordering::Relaxed);
        WB_TX_JOURNAL_TX_LAST.store(0, Ordering::Relaxed);
        WB_TX_JOURNAL_FLUSH_COUNT.store(0, Ordering::Relaxed);
        WB_TX_JOURNAL_FLUSH_TICKS.store(0, Ordering::Relaxed);
        WB_TX_BOUNDARY_FLUSH_COUNT.store(0, Ordering::Relaxed);
        WB_TX_BOUNDARY_FLUSH_TICKS.store(0, Ordering::Relaxed);
        VIRTIO_BLK_READ_CHUNKS.store(0, Ordering::Relaxed);
        VIRTIO_BLK_READ_BYTES.store(0, Ordering::Relaxed);
        VIRTIO_BLK_WRITE_CHUNKS.store(0, Ordering::Relaxed);
        VIRTIO_BLK_WRITE_BYTES.store(0, Ordering::Relaxed);
        VIRTIO_DMA_POOL_RESERVE_SUCCESS.store(0, Ordering::Relaxed);
        VIRTIO_DMA_POOL_RESERVE_FAIL.store(0, Ordering::Relaxed);
        VIRTIO_DMA_POOL_CONSUME.store(0, Ordering::Relaxed);
        VIRTIO_DMA_POOL_CANCEL.store(0, Ordering::Relaxed);
        VIRTIO_DMA_POOL_FINISH.store(0, Ordering::Relaxed);
        VIRTIO_DMA_SHARE_CALLS.store(0, Ordering::Relaxed);
        VIRTIO_DMA_SHARE_DATA_POOL.store(0, Ordering::Relaxed);
        VIRTIO_DMA_SHARE_DATA_FALLBACK.store(0, Ordering::Relaxed);
        VIRTIO_DMA_SHARE_HEADER_FALLBACK.store(0, Ordering::Relaxed);
        VIRTIO_DMA_SHARE_STATUS_FALLBACK.store(0, Ordering::Relaxed);
        VIRTIO_DMA_SHARE_INDIRECT_FALLBACK.store(0, Ordering::Relaxed);
        VIRTIO_DMA_SHARE_OTHER_FALLBACK.store(0, Ordering::Relaxed);
        VIRTIO_DMA_SHARE_HEADER_POOL.store(0, Ordering::Relaxed);
        VIRTIO_DMA_SHARE_STATUS_POOL.store(0, Ordering::Relaxed);
        VIRTIO_DMA_SHARE_INDIRECT_POOL.store(0, Ordering::Relaxed);
        VIRTIO_DMA_BRIDGE_LOCK_WAIT_TICKS_TOTAL.store(0, Ordering::Relaxed);
        VIRTIO_DMA_BRIDGE_LOCK_HOLD_TICKS_TOTAL.store(0, Ordering::Relaxed);
        VIRTIO_DMA_BRIDGE_LOCK_HOLD_TICKS_MAX.store(0, Ordering::Relaxed);
        SATA_READ_REQS.store(0, Ordering::Relaxed);
        SATA_READ_BYTES.store(0, Ordering::Relaxed);
        SATA_READ_TICKS_TOTAL.store(0, Ordering::Relaxed);
        SATA_READ_TICKS_MAX.store(0, Ordering::Relaxed);
        SATA_WRITE_REQS.store(0, Ordering::Relaxed);
        SATA_WRITE_BYTES.store(0, Ordering::Relaxed);
        SATA_WRITE_TICKS_TOTAL.store(0, Ordering::Relaxed);
        SATA_WRITE_TICKS_MAX.store(0, Ordering::Relaxed);
        SATA_FLUSH_REQS.store(0, Ordering::Relaxed);
        SATA_FLUSH_TICKS_TOTAL.store(0, Ordering::Relaxed);
        NET_POLL_CALLS.store(0, Ordering::Relaxed);
        NET_POLL_PROGRESS.store(0, Ordering::Relaxed);
        NET_POLL_LOCK_BUSY.store(0, Ordering::Relaxed);
        NET_RX_PACKETS.store(0, Ordering::Relaxed);
        NET_RX_BYTES.store(0, Ordering::Relaxed);
        NET_RX_DROPS.store(0, Ordering::Relaxed);
        NET_TX_SUBMIT_PACKETS.store(0, Ordering::Relaxed);
        NET_TX_SUBMIT_BYTES.store(0, Ordering::Relaxed);
        NET_TX_DROPS.store(0, Ordering::Relaxed);
        NET_TX_DEFERRED_DROPS.store(0, Ordering::Relaxed);
        RUNTIME_EXEC_CALLS.store(0, Ordering::Relaxed);
        RUNTIME_EXEC_TICKS_TOTAL.store(0, Ordering::Relaxed);
        RUNTIME_OPENAT_CALLS.store(0, Ordering::Relaxed);
        RUNTIME_READ_CALLS.store(0, Ordering::Relaxed);
        RUNTIME_MMAP_CALLS.store(0, Ordering::Relaxed);
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
        for i in 0..PF_ACTION_COUNT.len() {
            PF_ACTION_COUNT[i].store(0, Ordering::Relaxed);
            PF_ACTION_TICKS[i].store(0, Ordering::Relaxed);
        }
        for i in 0..PF_STAGE_COUNT.len() {
            PF_STAGE_COUNT[i].store(0, Ordering::Relaxed);
            PF_STAGE_TICKS[i].store(0, Ordering::Relaxed);
        }
        PF_RETURN_PENDING.store(false, Ordering::Relaxed);

        // ── Filemap fault phase ──
        FILEMAP_FAULT_FRAMES.store(0, Ordering::Relaxed);
        FILEMAP_FAULT_TICKS.store(0, Ordering::Relaxed);
        FILEMAP_PRIVATE_COPY_TICKS.store(0, Ordering::Relaxed);
        FILEMAP_MAP_USER_TICKS.store(0, Ordering::Relaxed);
        FILEMAP_READ_FAULT_CALLS.store(0, Ordering::Relaxed);
        FILEMAP_PRIVATE_FAULT_CALLS.store(0, Ordering::Relaxed);
        FILEMAP_SHARED_WRITE_FAULT_CALLS.store(0, Ordering::Relaxed);
        FILEMAP_READY_HIT.store(0, Ordering::Relaxed);
        FILEMAP_NOT_READY_RETRY.store(0, Ordering::Relaxed);
        FILEMAP_BACKEND_READ_CALLS.store(0, Ordering::Relaxed);
        FILEMAP_BACKEND_READ_TICKS_TOTAL.store(0, Ordering::Relaxed);
        FILEMAP_BACKEND_READ_TICKS_MAX.store(0, Ordering::Relaxed);
        FILEMAP_BACKEND_READ_UNDER_VM_CALLS.store(0, Ordering::Relaxed);
        FILEMAP_BACKEND_READ_UNDER_VM_TICKS_TOTAL.store(0, Ordering::Relaxed);
        FILEMAP_BACKEND_READ_UNDER_VM_TICKS_MAX.store(0, Ordering::Relaxed);
        FILEMAP_RETRY_WAIT_CALLS.store(0, Ordering::Relaxed);
        FILEMAP_RETRY_WAIT_TICKS_TOTAL.store(0, Ordering::Relaxed);
        FILEMAP_RETRY_WAIT_TICKS_MAX.store(0, Ordering::Relaxed);
        FILEMAP_REVALIDATE_RETRY.store(0, Ordering::Relaxed);
        FILEMAP_REVALIDATE_VMA_CHANGED.store(0, Ordering::Relaxed);
        FILEMAP_REVALIDATE_EOF_CHANGED.store(0, Ordering::Relaxed);
        FILEMAP_FAULT_AROUND_CALLS.store(0, Ordering::Relaxed);
        FILEMAP_FAULT_AROUND_PAGES_REQUESTED.store(0, Ordering::Relaxed);
        FILEMAP_FAULT_AROUND_PAGES_MISSING.store(0, Ordering::Relaxed);
        FILEMAP_FAULT_AROUND_CLAIM_CONFLICTS.store(0, Ordering::Relaxed);
        FILEMAP_FAULT_AROUND_PAGES_PUBLISHED.store(0, Ordering::Relaxed);
        FILEMAP_FAULT_AROUND_PAGES_PREFETCHED.store(0, Ordering::Relaxed);
        FILEMAP_FAULT_AROUND_BACKEND_RUNS.store(0, Ordering::Relaxed);
        FILEMAP_FAULT_AROUND_USEFUL_HITS.store(0, Ordering::Relaxed);
        FILEMAP_FAULT_AROUND_UNUSED_DISCARDS.store(0, Ordering::Relaxed);
        FILEMAP_FAULT_AROUND_ABORTS.store(0, Ordering::Relaxed);

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
        EXEC_DIRECT_COUNT.store(0, Ordering::Relaxed);
        EXEC_FALLBACK_COUNT.store(0, Ordering::Relaxed);
        EXEC_DIRECT_ENOSYS_COUNT.store(0, Ordering::Relaxed);
        VM_READ_LOCK_CALLS.store(0, Ordering::Relaxed);
        VM_READ_LOCK_WAIT_TICKS_TOTAL.store(0, Ordering::Relaxed);
        VM_READ_LOCK_WAIT_TICKS_MAX.store(0, Ordering::Relaxed);
        VM_READ_LOCK_HOLD_TICKS_TOTAL.store(0, Ordering::Relaxed);
        VM_READ_LOCK_HOLD_TICKS_MAX.store(0, Ordering::Relaxed);
        VM_WRITE_LOCK_CALLS.store(0, Ordering::Relaxed);
        VM_WRITE_LOCK_WAIT_TICKS_TOTAL.store(0, Ordering::Relaxed);
        VM_WRITE_LOCK_WAIT_TICKS_MAX.store(0, Ordering::Relaxed);
        VM_WRITE_LOCK_HOLD_TICKS_TOTAL.store(0, Ordering::Relaxed);
        VM_WRITE_LOCK_HOLD_TICKS_MAX.store(0, Ordering::Relaxed);
        VM_FLUSH_OUTSIDE_LOCK_TICKS_TOTAL.store(0, Ordering::Relaxed);
        VM_FLUSH_OUTSIDE_LOCK_TICKS_MAX.store(0, Ordering::Relaxed);
        TASK_SWITCH_SAME_MM.store(0, Ordering::Relaxed);
        TASK_SWITCH_DIFFERENT_MM.store(0, Ordering::Relaxed);
        TASK_SWITCH_TO_KERNEL_ONLY.store(0, Ordering::Relaxed);
        TASK_SWITCH_IDLE_NO_NEXT.store(0, Ordering::Relaxed);
        FRAME_GLOBAL_ALLOC_LOCK_WAIT_TICKS_TOTAL.store(0, Ordering::Relaxed);
        FRAME_GLOBAL_ALLOC_LOCK_WAIT_TICKS_MAX.store(0, Ordering::Relaxed);
        FRAME_GLOBAL_ALLOC_LOCK_HOLD_TICKS_TOTAL.store(0, Ordering::Relaxed);
        FRAME_GLOBAL_ALLOC_LOCK_HOLD_TICKS_MAX.store(0, Ordering::Relaxed);
        FRAME_GLOBAL_FREE_LOCK_WAIT_TICKS_TOTAL.store(0, Ordering::Relaxed);
        FRAME_GLOBAL_FREE_LOCK_WAIT_TICKS_MAX.store(0, Ordering::Relaxed);
        FRAME_GLOBAL_FREE_LOCK_HOLD_TICKS_TOTAL.store(0, Ordering::Relaxed);
        FRAME_GLOBAL_FREE_LOCK_HOLD_TICKS_MAX.store(0, Ordering::Relaxed);
        FRAME_RESERVE_CHECK_CALLS.store(0, Ordering::Relaxed);
        FRAME_RESERVE_CHECK_TICKS_TOTAL.store(0, Ordering::Relaxed);
        FRAME_RESERVE_OOM_CALLS.store(0, Ordering::Relaxed);
        FRAME_ALLOC_SOURCE_FRESH.store(0, Ordering::Relaxed);
        FRAME_ALLOC_SOURCE_RECYCLED.store(0, Ordering::Relaxed);
        FRAME_CONTIG_LOCK_WAIT_TICKS_TOTAL.store(0, Ordering::Relaxed);
        FRAME_CONTIG_LOCK_WAIT_TICKS_MAX.store(0, Ordering::Relaxed);
        FRAME_CONTIG_LOCK_HOLD_TICKS_TOTAL.store(0, Ordering::Relaxed);
        FRAME_CONTIG_LOCK_HOLD_TICKS_MAX.store(0, Ordering::Relaxed);
        FRAME_CONTIG_ZERO_TICKS_TOTAL.store(0, Ordering::Relaxed);
        FRAME_CONTIG_PAGES.store(0, Ordering::Relaxed);
        HEAP_LOCK_WAIT_TICKS_TOTAL.store(0, Ordering::Relaxed);
        HEAP_LOCK_WAIT_TICKS_MAX.store(0, Ordering::Relaxed);
        HEAP_LOCK_HOLD_TICKS_TOTAL.store(0, Ordering::Relaxed);
        HEAP_LOCK_HOLD_TICKS_MAX.store(0, Ordering::Relaxed);
        HEAP_SLAB_ALLOC_CALLS.store(0, Ordering::Relaxed);
        HEAP_DIRECT_BUDDY_CALLS.store(0, Ordering::Relaxed);
        HEAP_CLASS_8_CALLS.store(0, Ordering::Relaxed);
        HEAP_CLASS_16_CALLS.store(0, Ordering::Relaxed);
        HEAP_CLASS_32_CALLS.store(0, Ordering::Relaxed);
        HEAP_CLASS_64_CALLS.store(0, Ordering::Relaxed);
        HEAP_CLASS_128_CALLS.store(0, Ordering::Relaxed);
        HEAP_CLASS_256_CALLS.store(0, Ordering::Relaxed);
        HEAP_CLASS_512_CALLS.store(0, Ordering::Relaxed);
        HEAP_CLASS_1024_CALLS.store(0, Ordering::Relaxed);
        HEAP_CLASS_2048_CALLS.store(0, Ordering::Relaxed);
        HEAP_LARGE_CALLS.store(0, Ordering::Relaxed);
        MM_ACTIVATE_CALLS.store(0, Ordering::Relaxed);
        MM_ACTIVATE_TICKS_TOTAL.store(0, Ordering::Relaxed);
        MM_DEACTIVATE_CALLS.store(0, Ordering::Relaxed);
        MM_DEACTIVATE_TICKS_TOTAL.store(0, Ordering::Relaxed);
        MM_SAME_ALREADY_ACTIVE.store(0, Ordering::Relaxed);
        MM_GENERATION_CATCHUP.store(0, Ordering::Relaxed);
        MM_ASID_ROLLOVER.store(0, Ordering::Relaxed);
        WAKE_LOCAL.store(0, Ordering::Relaxed);
        WAKE_REMOTE.store(0, Ordering::Relaxed);
        WAKE_KEEP_LAST_CPU.store(0, Ordering::Relaxed);
        WAKE_SELECT_IDLE_CPU.store(0, Ordering::Relaxed);
        WAKE_SELECT_LEAST_LOADED.store(0, Ordering::Relaxed);
        WAKE_LAST_BUSY_IDLE_AVAILABLE.store(0, Ordering::Relaxed);
        NEW_TASK_IDLE_AVAILABLE.store(0, Ordering::Relaxed);
        NEW_TASK_SELECTED_IDLE.store(0, Ordering::Relaxed);
        NEW_TASK_KEPT_BUSY_PARENT.store(0, Ordering::Relaxed);
        WAKE_TO_RUN_TICKS_TOTAL.store(0, Ordering::Relaxed);
        WAKE_TO_RUN_TICKS_MAX.store(0, Ordering::Relaxed);
        TASK_RUN_SLICE_TICKS_TOTAL.store(0, Ordering::Relaxed);
        STEAL_ATTEMPTS.store(0, Ordering::Relaxed);
        for counter in &STEAL_ATTEMPTS_BY_CPU {
            counter.store(0, Ordering::Relaxed);
        }
        STEAL_CANDIDATE_FOUND.store(0, Ordering::Relaxed);
        STEAL_NO_REMOTE_READY.store(0, Ordering::Relaxed);
        STEAL_NO_ELIGIBLE_CANDIDATE.store(0, Ordering::Relaxed);
        STEAL_SUCCESS.store(0, Ordering::Relaxed);
        for counter in &STEAL_SUCCESS_BY_CPU {
            counter.store(0, Ordering::Relaxed);
        }
        STEAL_RECHECK_FAILED.store(0, Ordering::Relaxed);
        STEAL_KTLB_SYNC_CALLS.store(0, Ordering::Relaxed);
        STEAL_KTLB_SYNC_TICKS_TOTAL.store(0, Ordering::Relaxed);
        STEAL_KTLB_SYNC_TICKS_MAX.store(0, Ordering::Relaxed);
        for counter in &SCHED_IDLE_BUSY_LOOPS_BY_CPU {
            counter.store(0, Ordering::Relaxed);
        }
        for counter in &SCHED_IDLE_WAIT_LOOPS_BY_CPU {
            counter.store(0, Ordering::Relaxed);
        }
        EXEC_PTLOAD_SEGMENTS.store(0, Ordering::Relaxed);
        EXEC_PTLOAD_PAGES.store(0, Ordering::Relaxed);
        EXEC_PTLOAD_FILE_BYTES.store(0, Ordering::Relaxed);
        EXEC_PREFETCH_TICKS.store(0, Ordering::Relaxed);
        EXEC_TARGET_ALLOC_TICKS.store(0, Ordering::Relaxed);
        EXEC_TARGET_ZERO_TICKS.store(0, Ordering::Relaxed);
        EXEC_PAGECACHE_COPY_TICKS.store(0, Ordering::Relaxed);
        EXEC_FALLBACK_KMAP_WAIT_TICKS.store(0, Ordering::Relaxed);
    }

    /// Print accumulated timing stats, then reset.
    pub fn perf_dump_timings(label: &str) {
        let freq = crate::hal::get_clock_freq();
        let to_ms = |ticks: usize| -> usize {
            if freq > 0 {
                ticks.saturating_mul(1000) / freq
            } else {
                0
            }
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
        if !stats_enabled() {
            return;
        }
        CLONE_TIME_TICKS.fetch_add(ticks, Ordering::Relaxed);
        CLONE_TIME_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_pagefault_time_us(ticks: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        PAGEFAULT_TIME_TICKS.fetch_add(ticks, Ordering::Relaxed);
        PAGEFAULT_TIME_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_frame_alloc_time_us(ticks: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        FRAME_ALLOC_TIME_TICKS.fetch_add(ticks, Ordering::Relaxed);
        FRAME_ALLOC_TIME_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    /// Read the architecture clock only when the requested profile is active.
    #[inline(always)]
    pub fn perf_time_now() -> usize {
        perf_time_now_for(super::STATS_PROFILE_CORE)
    }

    #[inline(always)]
    pub fn perf_memory_io_time_now() -> usize {
        perf_time_now_for(super::STATS_PROFILE_MEMORY_IO)
    }

    #[inline(always)]
    pub fn perf_time_now_for(profile: usize) -> usize {
        if !stats_enabled_for(profile) {
            return 0;
        }
        // The diagnostic ABI exports `clock_freq_hz` for timer ticks, so every
        // sampled delta must come from the same architectural timebase.
        crate::timer::raw_ticks() as usize
    }

    fn load(counter: &AtomicUsize) -> usize {
        counter.load(Ordering::Relaxed)
    }

    fn print_snapshot(reason: &str) {
        // 聚合计数器可由任意 CPU 原子更新，但格式化快照还会读取 FS/net
        // 全局状态并输出 console；这些共享路径完成 SMP 审计前只允许 CPU0 进入。
        if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
            return;
        }
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
        let (f_calls, f_ticks, f_overlay, f_hit, f_miss, f_lock, f_inner, f_insert, f_ov_ticks) =
            crate::fs::vfs::mount::counters::find_snapshot();
        let freq = crate::hal::get_clock_freq();
        let us = |t: usize| -> usize {
            if freq > 0 {
                t.saturating_mul(1000000) / freq
            } else {
                0
            }
        };
        let f_us = if f_calls > 0 {
            us(f_ticks) / f_calls
        } else {
            0
        };
        let lock_us = if f_calls > 0 { us(f_lock) / f_calls } else { 0 };
        let inner_us = if f_miss > 0 { us(f_inner) / f_miss } else { 0 };
        let insert_us = if f_miss > 0 { us(f_insert) / f_miss } else { 0 };
        crate::println!(
            "[vfs-find] {} calls={} hit={} miss={} avg={}us lock={}us inner={}us insert={}us",
            reason,
            f_calls,
            f_hit,
            f_miss,
            f_us,
            lock_us,
            inner_us,
            insert_us
        );

        // lwext4 metadata diagnostics are only present in the legacy backend.
        #[cfg(feature = "ext4_lwext4_backend")]
        {
            let lw = crate::fs::ext4_lwext4::counters::snapshot();
            crate::println!("[lwext4] find={} find_cycles={} probe_type={} pt_cycles={} get_inode_id={} gii_enoent={} gii_cycles={} meta_cold={} meta_hot={} meta_cold_cycles={} file_open={} fo_cycles={} file_size={} file_close={} fc_cycles={} dirent={} de_cycles={} create_pre={} logical_size={} ls_cycles={} ensure_pc={} cache_hit={} cache_miss={} pc_creates={}",
                lw.0, lw.1, lw.2, lw.3, lw.4, lw.5, lw.6, lw.7, lw.8, lw.9,
                lw.10, lw.11, lw.12, lw.13, lw.14, lw.15, lw.16, lw.17, lw.18, lw.19, lw.20,
                lw.21, lw.22, lw.23);
        }

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
        if !memory_io_stats_enabled() {
            return;
        }
        TLB_FLUSHES.fetch_add(1, Ordering::Relaxed);
        TLB_FULL.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_tlb_page() {
        if !memory_io_stats_enabled() {
            return;
        }
        TLB_FLUSHES.fetch_add(1, Ordering::Relaxed);
        TLB_PAGE.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_tlb_activate() {
        if !memory_io_stats_enabled() {
            return;
        }
        // NOTE: only increments the category counter, not TLB_FLUSHES.
        // The underlying tlb_invalidate()/sfence.vma already accounts for the total.
        TLB_ACTIVATE.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_tlb_global() {
        if !memory_io_stats_enabled() {
            return;
        }
        TLB_FLUSHES.fetch_add(1, Ordering::Relaxed);
        TLB_GLOBAL.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_page_fault() {
        if !memory_io_stats_enabled() {
            return;
        }
        PAGE_FAULTS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_vfs_lookup() {
        if !memory_io_stats_enabled() {
            return;
        }
        VFS_LOOKUPS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_vfs_lookup_time_us(ticks: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        VFS_LOOKUP_TIME_TICKS.fetch_add(ticks, Ordering::Relaxed);
        VFS_LOOKUP_TIME_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_frame_alloc() {
        if !memory_io_stats_enabled() {
            return;
        }
        FRAME_ALLOC_HITS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_frame_free() {
        if !memory_io_stats_enabled() {
            return;
        }
        FRAME_FREE_HITS.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_clone(is_thread: bool, share_vm: bool, stack_allocated: bool) {
        if !stats_enabled() {
            return;
        }
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
        if !stats_enabled() {
            return;
        }
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
        if !stats_enabled() {
            return;
        }
        if stored {
            TRAP_CACHE_STORE.fetch_add(1, Ordering::Relaxed);
        } else {
            TRAP_CACHE_SKIP.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_trap_cache_take(hit: bool) {
        if !stats_enabled() {
            return;
        }
        if hit {
            TRAP_CACHE_HIT.fetch_add(1, Ordering::Relaxed);
        } else {
            TRAP_CACHE_MISS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_kstack_alloc(hit: bool) {
        if !stats_enabled() {
            return;
        }
        if hit {
            KSTACK_CACHE_HIT.fetch_add(1, Ordering::Relaxed);
        } else {
            KSTACK_CACHE_MISS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_kstack_drop(cached: bool) {
        if !stats_enabled() {
            return;
        }
        if cached {
            KSTACK_CACHE_STORE.fetch_add(1, Ordering::Relaxed);
        } else {
            KSTACK_CACHE_DROP.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_zombie_enqueue() {
        if !stats_enabled() {
            return;
        }
        ZOMBIE_ENQUEUE.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_zombie_drain(count: usize) {
        if stats_enabled() && count != 0 {
            ZOMBIE_DRAIN.fetch_add(count, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_schedule_loop(fetched: bool) {
        if !stats_enabled() {
            return;
        }
        let n = SCHEDULE_LOOPS.fetch_add(1, Ordering::Relaxed) + 1;
        if fetched {
            SCHEDULE_FETCH.fetch_add(1, Ordering::Relaxed);
        } else {
            SCHEDULE_IDLE.fetch_add(1, Ordering::Relaxed);
        }
        if crate::smp::cpu_id() == crate::smp::BOOT_CPU_ID
            && load(&CLONE_TOTAL) >= 4500
            && n % PRINT_EVERY_SCHEDULES == 0
        {
            print_snapshot("sched");
        }
    }

    #[inline(always)]
    pub fn record_timer_interrupt() {
        if !stats_enabled() {
            return;
        }
        // hard IRQ 中只允许无锁计数；console 快照由 deferred 安全点负责。
        TIMER_INTERRUPTS.fetch_add(1, Ordering::Relaxed);
    }

    /// 在 timer deferred 安全点按原有节奏输出诊断快照。
    pub fn record_deferred_timer_snapshot() {
        if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID
            || !stats_enabled()
            || load(&CLONE_TOTAL) < 4500
        {
            return;
        }
        let epoch = load(&TIMER_INTERRUPTS) / 1024;
        if epoch != 0 && TIMER_SNAPSHOT_EPOCH.fetch_max(epoch, Ordering::Relaxed) < epoch {
            print_snapshot("timer");
        }
    }

    #[inline(always)]
    pub fn record_futex_wait(shared: bool, has_deadline: bool) {
        if !stats_enabled() {
            return;
        }
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
        if !stats_enabled() {
            return;
        }
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
        if !stats_enabled() {
            return;
        }
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
        if !stats_enabled() {
            return;
        }
        if syscall_id < PERF_SYSCOUNT {
            SYSCALL_COUNT[syscall_id].fetch_add(1, Ordering::Relaxed);
            SYSCALL_TICKS[syscall_id].fetch_add(elapsed, Ordering::Relaxed);
        }
    }

    /// Read syscall count by ID (0 if out of range).
    pub fn syscall_count(id: usize) -> usize {
        if id < PERF_SYSCOUNT {
            SYSCALL_COUNT[id].load(Ordering::Relaxed)
        } else {
            0
        }
    }

    /// Read syscall total ticks by ID (0 if out of range).
    pub fn syscall_ticks(id: usize) -> usize {
        if id < PERF_SYSCOUNT {
            SYSCALL_TICKS[id].load(Ordering::Relaxed)
        } else {
            0
        }
    }

    // ── Page fault per-action recorders ──
    #[inline(always)]
    pub fn record_pagefault_action(action_tag: usize, elapsed: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        if action_tag < 7 {
            PF_ACTION_COUNT[action_tag].fetch_add(1, Ordering::Relaxed);
            PF_ACTION_TICKS[action_tag].fetch_add(elapsed, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_pagefault_stage(stage: usize, elapsed: usize) {
        if !memory_io_stats_enabled() {
            return;
        }
        if stage < PF_STAGE_COUNT.len() {
            PF_STAGE_COUNT[stage].fetch_add(1, Ordering::Relaxed);
            PF_STAGE_TICKS[stage].fetch_add(elapsed, Ordering::Relaxed);
        }
    }

    pub fn pf_stage_count(stage: usize) -> usize {
        if stage < PF_STAGE_COUNT.len() {
            PF_STAGE_COUNT[stage].load(Ordering::Relaxed)
        } else {
            0
        }
    }

    pub fn pf_stage_ticks(stage: usize) -> usize {
        if stage < PF_STAGE_TICKS.len() {
            PF_STAGE_TICKS[stage].load(Ordering::Relaxed)
        } else {
            0
        }
    }

    #[inline(always)]
    pub fn arm_pagefault_return() {
        if memory_io_stats_enabled() {
            PF_RETURN_PENDING.store(true, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn take_pagefault_return_pending() -> bool {
        memory_io_stats_enabled() && PF_RETURN_PENDING.swap(false, Ordering::Relaxed)
    }

    pub fn pf_action_count(action_tag: usize) -> usize {
        if action_tag < 7 {
            PF_ACTION_COUNT[action_tag].load(Ordering::Relaxed)
        } else {
            0
        }
    }

    pub fn pf_action_ticks(action_tag: usize) -> usize {
        if action_tag < 7 {
            PF_ACTION_TICKS[action_tag].load(Ordering::Relaxed)
        } else {
            0
        }
    }

    // ── TLB flush cycle recorders ──
    #[inline(always)]
    pub fn record_tlb_page_flush_cycles(cycles: usize) {
        if !stats_enabled() {
            return;
        }
        TLB_PAGE_FLUSH_CYCLES.fetch_add(cycles, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_tlb_full_flush_cycles(cycles: usize) {
        if !stats_enabled() {
            return;
        }
        TLB_FULL_FLUSH_CYCLES.fetch_add(cycles, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_tlb_activate_cycles(cycles: usize) {
        if !stats_enabled() {
            return;
        }
        TLB_ACTIVATE_CYCLES.fetch_add(cycles, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn record_syscall(syscall_id: usize, ret: isize) {
        if !stats_enabled() {
            return;
        }
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
pub fn record_boot_stage(_stage: usize) {}

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
pub fn record_deferred_timer_snapshot() {}

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
pub fn record_runtime_exec_cost(_syscall_id: usize, _ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_syscall_time(_syscall_id: usize, _elapsed: usize) {}

#[cfg(not(feature = "perf_stats"))]
pub fn syscall_count(_id: usize) -> usize {
    0
}

#[cfg(not(feature = "perf_stats"))]
pub fn syscall_ticks(_id: usize) -> usize {
    0
}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pagefault_action(_action_tag: usize, _elapsed: usize) {}

#[cfg(not(feature = "perf_stats"))]
pub fn pf_action_count(_action_tag: usize) -> usize {
    0
}

#[cfg(not(feature = "perf_stats"))]
pub fn pf_action_ticks(_action_tag: usize) -> usize {
    0
}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pagefault_stage(_stage: usize, _elapsed: usize) {}

#[cfg(not(feature = "perf_stats"))]
pub fn pf_stage_count(_stage: usize) -> usize {
    0
}

#[cfg(not(feature = "perf_stats"))]
pub fn pf_stage_ticks(_stage: usize) -> usize {
    0
}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn arm_pagefault_return() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn take_pagefault_return_pending() -> bool {
    false
}

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
pub fn record_vm_read_lock(_wait_ticks: usize, _hold_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_vm_write_lock(_wait_ticks: usize, _hold_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_vm_flush_outside_lock(_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_task_switch_to_kernel_only() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_task_switch_idle_no_next() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_mm_activate(_ticks: usize, _generation_catchup: bool) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_mm_deactivate(_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_mm_same_already_active() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_mm_asid_rollover() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_task_switch_mm(_same_mm: bool) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_exec_direct() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_exec_fallback() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_exec_direct_enosys() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_filemap_read_fault() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_filemap_private_fault() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_filemap_shared_write_fault() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_filemap_ready_hit() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_filemap_not_ready_retry() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_filemap_backend_read(_ticks: usize, _under_vm: bool) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_filemap_retry_wait(_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_filemap_revalidate_retry(_vma_changed: bool, _eof_changed: bool) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_filemap_fault_around_start(_requested_pages: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_filemap_fault_around_missing(_missing_pages: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_filemap_fault_around_claim_conflict(_conflicts: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_filemap_fault_around_backend_run() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_filemap_fault_around_publish(_published_pages: usize, _prefetched_pages: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_filemap_fault_around_useful_hit() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_filemap_fault_around_unused_discard() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_filemap_fault_around_abort() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_frame_global_alloc_lock(_wait_ticks: usize, _hold_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_frame_global_free_lock(_wait_ticks: usize, _hold_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_frame_reserve_check(_ticks: usize, _oom: bool) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_frame_alloc_source(_recycled: bool) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_frame_contig_lock(_wait_ticks: usize, _hold_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_frame_contig_page(_zero_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_heap_lock(_wait_ticks: usize, _hold_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_heap_alloc_path(_class_bytes: Option<usize>) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_wake_local() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_wake_remote() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_wake_selection(
    _keep_last: bool,
    _idle: bool,
    _last_busy_idle_available: bool,
) {
}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_new_task_placement(
    _idle_available: bool,
    _selected_idle: bool,
    _kept_busy_parent: bool,
) {
}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_wake_to_run(_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_task_run_slice(_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_steal_attempt(_cpu: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_steal_candidate() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_steal_no_remote_ready() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_steal_no_eligible_candidate() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_steal_success(_cpu: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_scheduler_idle(_cpu: usize, _waited: bool) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_steal_recheck_failed() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_steal_ktlb_sync(_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_exec_ptload(_segments: usize, _pages: usize, _file_bytes: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_exec_phase(_counter: &core::sync::atomic::AtomicUsize, _ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_exec_fallback_kmap_wait(_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
pub static EXEC_PREFETCH_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXEC_TARGET_ALLOC_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXEC_TARGET_ZERO_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXEC_PAGECACHE_COPY_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

macro_rules! perf_stub_counter {
    ($name:ident) => {
        #[cfg(not(feature = "perf_stats"))]
        pub static $name: core::sync::atomic::AtomicUsize =
            core::sync::atomic::AtomicUsize::new(0);
    };
}
perf_stub_counter!(VM_READ_LOCK_WAIT_TICKS_MAX);
perf_stub_counter!(VM_READ_LOCK_HOLD_TICKS_MAX);
perf_stub_counter!(VM_WRITE_LOCK_WAIT_TICKS_MAX);
perf_stub_counter!(VM_WRITE_LOCK_HOLD_TICKS_MAX);
perf_stub_counter!(VM_FLUSH_OUTSIDE_LOCK_TICKS_MAX);
perf_stub_counter!(TASK_SWITCH_TO_KERNEL_ONLY);
perf_stub_counter!(TASK_SWITCH_IDLE_NO_NEXT);
perf_stub_counter!(FRAME_GLOBAL_ALLOC_LOCK_WAIT_TICKS_TOTAL);
perf_stub_counter!(FRAME_GLOBAL_ALLOC_LOCK_WAIT_TICKS_MAX);
perf_stub_counter!(FRAME_GLOBAL_ALLOC_LOCK_HOLD_TICKS_TOTAL);
perf_stub_counter!(FRAME_GLOBAL_ALLOC_LOCK_HOLD_TICKS_MAX);
perf_stub_counter!(FRAME_GLOBAL_FREE_LOCK_WAIT_TICKS_TOTAL);
perf_stub_counter!(FRAME_GLOBAL_FREE_LOCK_WAIT_TICKS_MAX);
perf_stub_counter!(FRAME_GLOBAL_FREE_LOCK_HOLD_TICKS_TOTAL);
perf_stub_counter!(FRAME_GLOBAL_FREE_LOCK_HOLD_TICKS_MAX);
perf_stub_counter!(FRAME_RESERVE_CHECK_CALLS);
perf_stub_counter!(FRAME_RESERVE_CHECK_TICKS_TOTAL);
perf_stub_counter!(FRAME_RESERVE_OOM_CALLS);
perf_stub_counter!(FRAME_ALLOC_SOURCE_FRESH);
perf_stub_counter!(FRAME_ALLOC_SOURCE_RECYCLED);
perf_stub_counter!(FRAME_CONTIG_LOCK_WAIT_TICKS_TOTAL);
perf_stub_counter!(FRAME_CONTIG_LOCK_WAIT_TICKS_MAX);
perf_stub_counter!(FRAME_CONTIG_LOCK_HOLD_TICKS_TOTAL);
perf_stub_counter!(FRAME_CONTIG_LOCK_HOLD_TICKS_MAX);
perf_stub_counter!(FRAME_CONTIG_ZERO_TICKS_TOTAL);
perf_stub_counter!(FRAME_CONTIG_PAGES);
perf_stub_counter!(HEAP_LOCK_WAIT_TICKS_TOTAL);
perf_stub_counter!(HEAP_LOCK_WAIT_TICKS_MAX);
perf_stub_counter!(HEAP_LOCK_HOLD_TICKS_TOTAL);
perf_stub_counter!(HEAP_LOCK_HOLD_TICKS_MAX);
perf_stub_counter!(HEAP_SLAB_ALLOC_CALLS);
perf_stub_counter!(HEAP_DIRECT_BUDDY_CALLS);
perf_stub_counter!(HEAP_CLASS_8_CALLS);
perf_stub_counter!(HEAP_CLASS_16_CALLS);
perf_stub_counter!(HEAP_CLASS_32_CALLS);
perf_stub_counter!(HEAP_CLASS_64_CALLS);
perf_stub_counter!(HEAP_CLASS_128_CALLS);
perf_stub_counter!(HEAP_CLASS_256_CALLS);
perf_stub_counter!(HEAP_CLASS_512_CALLS);
perf_stub_counter!(HEAP_CLASS_1024_CALLS);
perf_stub_counter!(HEAP_CLASS_2048_CALLS);
perf_stub_counter!(HEAP_LARGE_CALLS);
perf_stub_counter!(MM_ACTIVATE_CALLS);
perf_stub_counter!(MM_ACTIVATE_TICKS_TOTAL);
perf_stub_counter!(MM_DEACTIVATE_CALLS);
perf_stub_counter!(MM_DEACTIVATE_TICKS_TOTAL);
perf_stub_counter!(MM_SAME_ALREADY_ACTIVE);
perf_stub_counter!(MM_GENERATION_CATCHUP);
perf_stub_counter!(MM_ASID_ROLLOVER);
perf_stub_counter!(WAKE_LOCAL);
perf_stub_counter!(WAKE_REMOTE);
perf_stub_counter!(WAKE_KEEP_LAST_CPU);
perf_stub_counter!(WAKE_SELECT_IDLE_CPU);
perf_stub_counter!(WAKE_SELECT_LEAST_LOADED);
perf_stub_counter!(WAKE_LAST_BUSY_IDLE_AVAILABLE);
perf_stub_counter!(NEW_TASK_IDLE_AVAILABLE);
perf_stub_counter!(NEW_TASK_SELECTED_IDLE);
perf_stub_counter!(NEW_TASK_KEPT_BUSY_PARENT);
perf_stub_counter!(WAKE_TO_RUN_TICKS_TOTAL);
perf_stub_counter!(WAKE_TO_RUN_TICKS_MAX);
perf_stub_counter!(TASK_RUN_SLICE_TICKS_TOTAL);
perf_stub_counter!(STEAL_ATTEMPTS);
#[cfg(not(feature = "perf_stats"))]
pub static STEAL_ATTEMPTS_BY_CPU: [core::sync::atomic::AtomicUsize; crate::smp::MAX_CPUS] =
    [const { core::sync::atomic::AtomicUsize::new(0) }; crate::smp::MAX_CPUS];
perf_stub_counter!(STEAL_CANDIDATE_FOUND);
perf_stub_counter!(STEAL_NO_REMOTE_READY);
perf_stub_counter!(STEAL_NO_ELIGIBLE_CANDIDATE);
perf_stub_counter!(STEAL_SUCCESS);
#[cfg(not(feature = "perf_stats"))]
pub static STEAL_SUCCESS_BY_CPU: [core::sync::atomic::AtomicUsize; crate::smp::MAX_CPUS] =
    [const { core::sync::atomic::AtomicUsize::new(0) }; crate::smp::MAX_CPUS];
perf_stub_counter!(STEAL_RECHECK_FAILED);
perf_stub_counter!(STEAL_KTLB_SYNC_CALLS);
perf_stub_counter!(STEAL_KTLB_SYNC_TICKS_TOTAL);
perf_stub_counter!(STEAL_KTLB_SYNC_TICKS_MAX);
#[cfg(not(feature = "perf_stats"))]
pub static SCHED_IDLE_BUSY_LOOPS_BY_CPU: [core::sync::atomic::AtomicUsize; crate::smp::MAX_CPUS] =
    [const { core::sync::atomic::AtomicUsize::new(0) }; crate::smp::MAX_CPUS];
#[cfg(not(feature = "perf_stats"))]
pub static SCHED_IDLE_WAIT_LOOPS_BY_CPU: [core::sync::atomic::AtomicUsize; crate::smp::MAX_CPUS] =
    [const { core::sync::atomic::AtomicUsize::new(0) }; crate::smp::MAX_CPUS];
perf_stub_counter!(EXEC_PTLOAD_SEGMENTS);
perf_stub_counter!(EXEC_PTLOAD_PAGES);
perf_stub_counter!(EXEC_PTLOAD_FILE_BYTES);
perf_stub_counter!(EXEC_FALLBACK_KMAP_WAIT_TICKS);
perf_stub_counter!(FILEMAP_BACKEND_READ_TICKS_MAX);
perf_stub_counter!(FILEMAP_BACKEND_READ_UNDER_VM_TICKS_TOTAL);
perf_stub_counter!(FILEMAP_BACKEND_READ_UNDER_VM_TICKS_MAX);
perf_stub_counter!(FILEMAP_RETRY_WAIT_CALLS);
perf_stub_counter!(FILEMAP_RETRY_WAIT_TICKS_TOTAL);
perf_stub_counter!(FILEMAP_RETRY_WAIT_TICKS_MAX);
perf_stub_counter!(FILEMAP_REVALIDATE_RETRY);
perf_stub_counter!(FILEMAP_REVALIDATE_VMA_CHANGED);
perf_stub_counter!(FILEMAP_REVALIDATE_EOF_CHANGED);
perf_stub_counter!(FILEMAP_FAULT_AROUND_CALLS);
perf_stub_counter!(FILEMAP_FAULT_AROUND_PAGES_REQUESTED);
perf_stub_counter!(FILEMAP_FAULT_AROUND_PAGES_MISSING);
perf_stub_counter!(FILEMAP_FAULT_AROUND_CLAIM_CONFLICTS);
perf_stub_counter!(FILEMAP_FAULT_AROUND_PAGES_PUBLISHED);
perf_stub_counter!(FILEMAP_FAULT_AROUND_PAGES_PREFETCHED);
perf_stub_counter!(FILEMAP_FAULT_AROUND_BACKEND_RUNS);
perf_stub_counter!(FILEMAP_FAULT_AROUND_USEFUL_HITS);
perf_stub_counter!(FILEMAP_FAULT_AROUND_UNUSED_DISCARDS);
perf_stub_counter!(FILEMAP_FAULT_AROUND_ABORTS);

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
pub fn perf_time_now() -> usize {
    0
}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn perf_memory_io_time_now() -> usize {
    0
}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn perf_time_now_for(_profile: usize) -> usize {
    0
}

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
pub fn record_taskq_queue_lens(
    _ready: usize,
    _interruptible: usize,
    _ready_zombie: usize,
    _int_zombie: usize,
    _nonzero_nice: usize,
) {
}

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
pub fn record_user_unaligned_trap(_start: usize, _is_store: bool, _size: usize, _is_float: bool) {}

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

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_anon_unmap(
    _range_release: bool,
    _requested_pages: usize,
    _resident_pages: usize,
    _active_before: usize,
    _retain_scan_steps: usize,
    _start_ticks: usize,
    _failed: bool,
) {
}

// ── PageCache recorders (no-op when perf_stats disabled) ──
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pc_read(_pages: usize, _cycles: usize, _hit_cycles: usize, _miss_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pc_read_user(_pages: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pc_read_lookup_cycles(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pc_read_miss_fill_cycles(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pc_read_valid_fill_cycles(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pc_read_copy_cycles(_cycles: usize) {}
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
pub fn record_pc_write_lookup(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pc_write_copy(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pc_write_commit(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pc_write_stages(_lookup: usize, _lease: usize, _copy: usize, _commit: usize) {}
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
pub fn record_pwrite_uaccess(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pwrite_file(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pwrite_ext4_setup(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pwrite_ext4_post(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pwrite_total_count() {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pwrite_vfs_mode(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pwrite_vfs_seals(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pwrite_vfs_touch(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pwrite_mount_writable(_cycles: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_write_fd_prep(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_write_uaccess(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_write_file(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_write_vfs_mode(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_write_vfs_seals(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_write_offset(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_write_total_count() {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pread_uaccess(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pread_file(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pread_ext4_logical_size(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pread_ext4_page_cache(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pread_total_count() {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_pread_vfs_mode(_cycles: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_journal_commit(_bytes: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_wb_data_write(_bytes: usize, _cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_wb_alloc_extent(_pages: usize, _cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_wb_tx_journal_commit(_transaction_id: u32, _staged_blocks: usize, _cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_wb_tx_journal_flush(_cycles: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_wb_flush_boundary(_cycles: usize) {}

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
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_device_flush() {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_virtio_write(_bytes: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_virtio_read() {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_writeback_batch(_pages: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_wb_tx_data_write(_bytes: usize, _ticks: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_wb_tx_alloc_extent(_pages: usize, _ticks: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_wb_tx_boundary_flush(_ticks: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_virtio_blk_read_chunk(_bytes: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_virtio_blk_write_chunk(_bytes: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_virtio_dma_pool_reserve(_success: bool) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_virtio_dma_pool_consume() {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_virtio_dma_pool_cancel() {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_virtio_dma_pool_finish() {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_virtio_dma_share(_kind: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_virtio_dma_bridge_lock(_wait_ticks: usize, _hold_ticks: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_sata_read(_bytes: usize, _ticks: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_sata_write(_bytes: usize, _ticks: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_sata_flush(_ticks: usize) {}

#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_net_poll(_progressed: bool, _lock_busy: bool) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_net_rx(_bytes: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_net_rx_drop() {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_net_tx_submit(_bytes: usize) {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_net_tx_drop() {}
#[cfg(not(feature = "perf_stats"))]
#[inline(always)]
pub fn record_net_tx_deferred_dropped() {}

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
pub static BOOT_CONSOLE_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static BOOT_MM_TICKS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static BOOT_DRIVERS_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static BOOT_NET_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static BOOT_FS_TICKS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static BOOT_INITPROC_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static BOOT_SCHEDULER_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

#[cfg(not(feature = "perf_stats"))]
pub static FAIR_PICK_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static FAST_PATH_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static FAIR_SCAN_MAX: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static DUPLICATE_READY_ENQUEUE: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ADD_READY_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ADD_INTERRUPTIBLE_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WAKE_INTERRUPTIBLE_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static READY_LEN_MAX: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static INTERRUPTIBLE_LEN_MAX: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static READY_ZOMBIE_MAX: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static INTERRUPTIBLE_ZOMBIE_MAX: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ZOMBIE_DRAIN_SCAN_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ZOMBIE_DRAIN_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ZOMBIE_DRAIN_REMOVED: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static READY_NONZERO_NICE_CUR: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

#[cfg(not(feature = "perf_stats"))]
pub static KTIMER_LEN_MAX: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static KTIMER_ADD_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static KTIMER_POP_MAX: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static KTIMER_POP_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static KTIMER_STALE_WAKETASK: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static KTIMER_REAL_WAKE: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static KTIMER_COMPACT_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static KTIMER_STALE_REMOVED: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WAIT_WITH_TIMEOUT_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

#[cfg(not(feature = "perf_stats"))]
pub static SYSCALL_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static SYSCALL_GETPPID_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static SYSCALL_COST_MAX_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static TRAP_ENTER_COST_MAX_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

#[cfg(not(feature = "perf_stats"))]
pub static GETPPID_COST_TICKS_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static GETPPID_COST_TICKS_MAX: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static SYSCALL_COST_TICKS_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ECALL_TRAP_COST_TICKS_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ECALL_TRAP_COST_TICKS_MAX: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static USER_UNALIGNED_TRAPS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static USER_UNALIGNED_TICKS_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static USER_UNALIGNED_TICKS_MAX: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static USER_UNALIGNED_LOAD_2: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static USER_UNALIGNED_LOAD_4: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static USER_UNALIGNED_LOAD_8: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static USER_UNALIGNED_STORE_2: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static USER_UNALIGNED_STORE_4: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static USER_UNALIGNED_STORE_8: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static USER_UNALIGNED_FLOAT_LOADS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static USER_UNALIGNED_FLOAT_STORES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static CONTEXT_SWITCH_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static RECLAIM_RUNS_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static RECLAIM_PAGES_SCANNED_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static RECLAIM_PAGES_FREED_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

// ── Clock Eviction stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static CLOCK_SCANNED: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static CLOCK_SECOND_CHANCE: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static CLOCK_EVICTED: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ── Timer IRQ / Pop Cost stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static TIMER_IRQ_TICKS_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static TIMER_IRQ_TICKS_MAX: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static TIMER_POP_NODES_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static TIMER_POP_TICKS_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static TIMER_POP_TICKS_MAX: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

// ── Seccomp stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static SECCOMP_CHECK_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static SECCOMP_CHECK_TICKS_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static SECCOMP_CHECK_TICKS_MAX: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static SECCOMP_DISABLED_BYPASS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

// ── Heap Allocator Cost stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static HEAP_ALLOC_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static HEAP_ALLOC_TICKS_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static HEAP_ALLOC_TICKS_MAX: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static HEAP_DEALLOC_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static HEAP_DEALLOC_TICKS_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static HEAP_DEALLOC_TICKS_MAX: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static HEAP_DEALLOC_SCAN_STEPS_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

#[cfg(not(feature = "perf_stats"))]
pub static ANON_UNMAP_CALLS_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ANON_UNMAP_RANGE_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ANON_UNMAP_AREA_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ANON_UNMAP_REQUESTED_PAGES_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ANON_UNMAP_RESIDENT_PAGES_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ANON_UNMAP_ACTIVE_BEFORE_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ANON_UNMAP_ACTIVE_BEFORE_MAX: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ANON_UNMAP_RETAIN_SCAN_STEPS_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ANON_UNMAP_TICKS_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ANON_UNMAP_TICKS_MAX: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ANON_UNMAP_ERRORS_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ANON_UNMAP_PAGES_LE_16: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ANON_UNMAP_PAGES_LE_256: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ANON_UNMAP_PAGES_LE_4096: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static ANON_UNMAP_PAGES_GT_4096: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

// ── PageCache I/O stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static PC_READ_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_READ_PAGES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_READ_USER_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_READ_USER_PAGES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_READ_MISS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_READ_CYCLES_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_READ_HIT_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_READ_MISS_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_READ_LOOKUP_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_READ_MISS_FILL_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_READ_VALID_FILL_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_READ_COPY_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_COPY_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_LOOKUP_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_WRITE_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_WRITE_PAGES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_WRITE_OVERWRITE: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_WRITE_EVENTUALLY_FULL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_WRITE_CYCLES_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_WRITE_LOOKUP_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_WRITE_LEASE_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_WRITE_COPY_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_WRITE_COMMIT_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_WRITEBACK_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_WRITEBACK_PAGES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_WRITEBACK_CYCLES_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_FALLOC_CYCLES_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PWRITE_UACCESS_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PWRITE_FILE_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PWRITE_EXT4_SETUP_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PWRITE_EXT4_POST_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PWRITE_TOTAL_COUNT: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PWRITE_VFS_MODE_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PWRITE_VFS_SEALS_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PWRITE_VFS_TOUCH_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PWRITE_MOUNT_WRITABLE_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WRITE_FD_PREP_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WRITE_UACCESS_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WRITE_FILE_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WRITE_VFS_MODE_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WRITE_VFS_SEALS_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WRITE_OFFSET_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WRITE_TOTAL_COUNT: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PREAD_UACCESS_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PREAD_FILE_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PREAD_EXT4_LOGICAL_SIZE_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PREAD_EXT4_PAGE_CACHE_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PREAD_TOTAL_COUNT: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PREAD_VFS_MODE_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

#[cfg(not(feature = "perf_stats"))]
pub static JOURNAL_COMMIT_COUNT: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static JOURNAL_COMMIT_BYTES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_DATA_WRITE_BYTES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_DATA_WRITE_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_ALLOC_EXTENT_PAGES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_ALLOC_EXTENT_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_TX_JOURNAL_COMMIT_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_TX_JOURNAL_STAGED_BLOCKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_TX_JOURNAL_TX_FIRST: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_TX_JOURNAL_TX_LAST: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_TX_JOURNAL_FLUSH_COUNT: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_TX_JOURNAL_FLUSH_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_FLUSH_BOUNDARY_COUNT: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_FLUSH_BOUNDARY_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

// ── Writeback Throttling stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static WB_BG_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_THROTTLE_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_REDIRTY_PAGES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

// ── Block Device I/O stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static BLK_VREAD_REQS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static BLK_VREAD_SECS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static BLK_VWRITE_REQS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static BLK_VWRITE_SECS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static DEVICE_FLUSH_COUNT: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static VIRTIO_WRITE_REQUESTS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static VIRTIO_WRITE_BYTES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static VIRTIO_READ_REQUESTS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WRITEBACK_BATCH_COUNT: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WRITEBACK_PAGE_COUNT: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_TX_DATA_WRITE_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_TX_DATA_WRITE_BYTES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_TX_DATA_WRITE_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_TX_ALLOC_EXTENT_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_TX_ALLOC_EXTENT_PAGES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_TX_ALLOC_EXTENT_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_TX_BOUNDARY_FLUSH_COUNT: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static WB_TX_BOUNDARY_FLUSH_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
perf_stub_counter!(VIRTIO_BLK_READ_CHUNKS);
perf_stub_counter!(VIRTIO_BLK_READ_BYTES);
perf_stub_counter!(VIRTIO_BLK_WRITE_CHUNKS);
perf_stub_counter!(VIRTIO_BLK_WRITE_BYTES);
perf_stub_counter!(VIRTIO_DMA_POOL_RESERVE_SUCCESS);
perf_stub_counter!(VIRTIO_DMA_POOL_RESERVE_FAIL);
perf_stub_counter!(VIRTIO_DMA_POOL_CONSUME);
perf_stub_counter!(VIRTIO_DMA_POOL_CANCEL);
perf_stub_counter!(VIRTIO_DMA_POOL_FINISH);
perf_stub_counter!(VIRTIO_DMA_SHARE_CALLS);
perf_stub_counter!(VIRTIO_DMA_SHARE_DATA_POOL);
perf_stub_counter!(VIRTIO_DMA_SHARE_DATA_FALLBACK);
perf_stub_counter!(VIRTIO_DMA_SHARE_HEADER_FALLBACK);
perf_stub_counter!(VIRTIO_DMA_SHARE_STATUS_FALLBACK);
perf_stub_counter!(VIRTIO_DMA_SHARE_INDIRECT_FALLBACK);
perf_stub_counter!(VIRTIO_DMA_SHARE_OTHER_FALLBACK);
perf_stub_counter!(VIRTIO_DMA_SHARE_HEADER_POOL);
perf_stub_counter!(VIRTIO_DMA_SHARE_STATUS_POOL);
perf_stub_counter!(VIRTIO_DMA_SHARE_INDIRECT_POOL);
perf_stub_counter!(VIRTIO_DMA_BRIDGE_LOCK_WAIT_TICKS_TOTAL);
perf_stub_counter!(VIRTIO_DMA_BRIDGE_LOCK_HOLD_TICKS_TOTAL);
perf_stub_counter!(VIRTIO_DMA_BRIDGE_LOCK_HOLD_TICKS_MAX);

#[cfg(not(feature = "perf_stats"))]
pub static SATA_READ_REQS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static SATA_READ_BYTES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static SATA_READ_TICKS_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static SATA_READ_TICKS_MAX: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static SATA_WRITE_REQS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static SATA_WRITE_BYTES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static SATA_WRITE_TICKS_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static SATA_WRITE_TICKS_MAX: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static SATA_FLUSH_REQS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static SATA_FLUSH_TICKS_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

#[cfg(not(feature = "perf_stats"))]
pub static NET_POLL_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static NET_POLL_PROGRESS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static NET_POLL_LOCK_BUSY: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static NET_RX_PACKETS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static NET_RX_BYTES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static NET_RX_DROPS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static NET_TX_SUBMIT_PACKETS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static NET_TX_SUBMIT_BYTES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static NET_TX_DROPS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static NET_TX_DEFERRED_DROPS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static RUNTIME_EXEC_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static RUNTIME_EXEC_TICKS_TOTAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static RUNTIME_OPENAT_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static RUNTIME_READ_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static RUNTIME_MMAP_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

// ── Ext4 Block Mapping stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_MAP_LBLOCK_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_MAP_LBLOCK_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_MAP_CACHE_HITS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_MAP_HOLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

// ── Ext4 Extent Tree Search stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_FIND_EXTENT_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_FIND_EXTENT_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_FIND_EXTENT_DEPTH_SUM: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_FIND_EXTENT_META_READS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

// ── Ext4 PageCache Backend Batch stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_PC_READPAGES_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_PC_READPAGES_PAGES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_PC_READPAGES_RUNS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_PC_WRITEPAGES_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_PC_WRITEPAGES_PAGES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_PC_WRITEPAGES_RUNS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_PC_512B_FALLBACK_PAGES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

// ── Ext4 Allocation stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_ALLOC_ENSURE_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_ALLOC_LBLOCKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_ALLOC_NEW_BLOCKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_ALLOC_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

// ── PageCache Lock Contention stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static PC_LOCK_HOLD_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_LOCK_HOLD_MAX: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PC_LOCK_IO_MISS_READS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

// ── Ext4 direct write_at stub ──
#[cfg(not(feature = "perf_stats"))]
pub static EXT4_DIRECT_WRITE_AT_CALLS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

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

#[cfg(not(feature = "perf_stats"))]
pub static FRAME_ALLOC_HITS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static FRAME_FREE_HITS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PAGE_FAULTS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PAGEFAULT_TIME_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static PAGEFAULT_TIME_COUNT: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static FRAME_ALLOC_TIME_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static FRAME_ALLOC_TIME_COUNT: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
// ── Per-syscall profiling stubs ──
#[cfg(not(feature = "perf_stats"))]
pub const PERF_SYSCOUNT: usize = 512;

// ── Filemap fault phase stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static FILEMAP_FAULT_FRAMES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static FILEMAP_FAULT_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static FILEMAP_PRIVATE_COPY_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static FILEMAP_MAP_USER_TICKS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static FILEMAP_READ_FAULT_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static FILEMAP_PRIVATE_FAULT_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static FILEMAP_SHARED_WRITE_FAULT_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static FILEMAP_READY_HIT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static FILEMAP_NOT_READY_RETRY: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static FILEMAP_BACKEND_READ_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static FILEMAP_BACKEND_READ_TICKS_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static FILEMAP_BACKEND_READ_UNDER_VM_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ── TLB flush cycle stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static TLB_PAGE_FLUSH_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static TLB_FULL_FLUSH_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static TLB_ACTIVATE_CYCLES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

// ── Execve phase cycle stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static EXECVE_MAP_ELF_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXECVE_KERNEL_MAP_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXECVE_INTERP_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXECVE_STACK_TABLES_TICKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXECVE_TEARDOWN_TICKS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXEC_DIRECT_COUNT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXEC_FALLBACK_COUNT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static EXEC_DIRECT_ENOSYS_COUNT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ── Stage 0 VM/MM switch stubs ──
#[cfg(not(feature = "perf_stats"))]
pub static VM_READ_LOCK_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static VM_READ_LOCK_WAIT_TICKS_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static VM_READ_LOCK_HOLD_TICKS_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static VM_WRITE_LOCK_CALLS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static VM_WRITE_LOCK_WAIT_TICKS_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static VM_WRITE_LOCK_HOLD_TICKS_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static VM_FLUSH_OUTSIDE_LOCK_TICKS_TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static TASK_SWITCH_SAME_MM: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(not(feature = "perf_stats"))]
pub static TASK_SWITCH_DIFFERENT_MM: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ── PF action names stub ──
#[cfg(not(feature = "perf_stats"))]
pub const PF_ACTION_NAMES: [&str; 7] = [
    "LazyAlloc",
    "FileBackedRead",
    "FileBackedSharedWrite",
    "FileBackedWrite",
    "SharedWrite",
    "Cow",
    "Other",
];

#[cfg(not(feature = "perf_stats"))]
pub const PF_STAGE_NAMES: [&str; 8] = [
    "trap_entry",
    "classify_vma",
    "pte_map",
    "frame_alloc",
    "zero_copy",
    "tlb_flush",
    "trap_return",
    "filemap_frame",
];
