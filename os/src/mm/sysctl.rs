use core::sync::atomic::{AtomicUsize, Ordering};

use crate::config::{MEMORY_SIZE, PAGE_SIZE};

const REPORTED_MEMORY_CAP_KB: usize = 512 * 1024;
// Keep the ABI-visible commit window conservative. The QEMU kernels have less
// useful user mmap space and lower OOM recovery headroom than the raw RAM size
// suggests, and LTP tunable tests size their stress loops from CommitLimit.
const COMMIT_LIMIT_CAP_KB: usize = 64 * 1024;
const DEFAULT_OVERCOMMIT_MEMORY: usize = 0;
const DEFAULT_OVERCOMMIT_RATIO: usize = 50;
const DEFAULT_MAX_MAP_COUNT: usize = 65_530;
const DEFAULT_MIN_FREE_KBYTES: usize = 1_024;

static OVERCOMMIT_MEMORY: AtomicUsize = AtomicUsize::new(DEFAULT_OVERCOMMIT_MEMORY);
static OVERCOMMIT_RATIO: AtomicUsize = AtomicUsize::new(DEFAULT_OVERCOMMIT_RATIO);
static MAX_MAP_COUNT: AtomicUsize = AtomicUsize::new(DEFAULT_MAX_MAP_COUNT);
static MIN_FREE_KBYTES: AtomicUsize = AtomicUsize::new(DEFAULT_MIN_FREE_KBYTES);
static PANIC_ON_OOM: AtomicUsize = AtomicUsize::new(0);

pub fn overcommit_memory() -> usize {
    OVERCOMMIT_MEMORY.load(Ordering::Relaxed)
}

pub fn set_overcommit_memory(value: usize) -> bool {
    if value > 2 {
        return false;
    }
    OVERCOMMIT_MEMORY.store(value, Ordering::Relaxed);
    true
}

pub fn overcommit_ratio() -> usize {
    OVERCOMMIT_RATIO.load(Ordering::Relaxed)
}

pub fn set_overcommit_ratio(value: usize) {
    OVERCOMMIT_RATIO.store(value, Ordering::Relaxed);
}

pub fn max_map_count() -> usize {
    MAX_MAP_COUNT.load(Ordering::Relaxed)
}

pub fn set_max_map_count(value: usize) -> bool {
    if value == 0 {
        return false;
    }
    MAX_MAP_COUNT.store(value, Ordering::Relaxed);
    true
}

pub fn min_free_kbytes() -> usize {
    MIN_FREE_KBYTES.load(Ordering::Relaxed)
}

pub fn set_min_free_kbytes(value: usize) {
    MIN_FREE_KBYTES.store(value, Ordering::Relaxed);
}

pub fn panic_on_oom() -> usize {
    PANIC_ON_OOM.load(Ordering::Relaxed)
}

pub fn set_panic_on_oom(value: usize) {
    PANIC_ON_OOM.store(value, Ordering::Relaxed);
}

pub fn commit_limit_bytes() -> usize {
    reported_memory_bytes()
        .saturating_mul(overcommit_ratio())
        .saturating_div(100)
        .min(COMMIT_LIMIT_CAP_KB.saturating_mul(1024))
}

pub fn commit_limit_kbytes() -> usize {
    commit_limit_bytes() / 1024
}

pub fn committed_as_kbytes() -> usize {
    let vm = match crate::task::current_task_ref() {
        Some(task) => task.process.vm(),
        None => return 0,
    };
    let committed = vm.lock().committed_bytes() / 1024;
    committed
}

pub fn overcommit_allows(current_committed_bytes: usize, additional_bytes: usize) -> bool {
    match overcommit_memory() {
        1 => true,
        2 => {
            current_committed_bytes
                .saturating_add(additional_bytes)
                <= commit_limit_bytes()
        }
        _ => additional_bytes <= reported_memory_bytes(),
    }
}

pub fn total_memory_kbytes() -> usize {
    (MEMORY_SIZE / 1024).min(REPORTED_MEMORY_CAP_KB)
}

fn reported_memory_bytes() -> usize {
    total_memory_kbytes().saturating_mul(1024)
}

pub fn free_memory_kbytes() -> usize {
    crate::mm::unallocated_frames()
        .saturating_mul(PAGE_SIZE)
        / 1024
}
