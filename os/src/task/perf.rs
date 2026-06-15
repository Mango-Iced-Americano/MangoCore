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

    fn load(counter: &AtomicUsize) -> usize {
        counter.load(Ordering::Relaxed)
    }

    fn print_snapshot(reason: &str) {
        crate::println!(
            "[perf] {} clone={} thread={} share_vm={} stack_alloc={} exit={} clear_tid={} keep_trap={} trap_store={} trap_skip={} trap_hit={} trap_miss={} kstack_hit={} kstack_miss={} kstack_store={} kstack_drop={} zombie_enq={} zombie_drain={} sched={} fetch={} idle={} timer={} fut_wait={} fut_wait_shared={} fut_wait_deadline={} fut_ready={} fut_timeout={} fut_intr={} fut_wake={} fut_wake_shared={} fut_wake_hit={} sc_clone={} sc_futex={} sc_mmap={} sc_munmap={} sc_mprotect={} sc_settid={} sc_robust={} sc_exit={} sc_yield={} last_sys={} last_ret={}",
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
            load(&LAST_SYSCALL_ID),
            load(&LAST_SYSCALL_RET),
        );
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
