//! `/proc/sys/vm` 风格的内存调参状态。
//!
//! 这些值主要服务 Linux 兼容测试和 procfs 输出；它们不直接改变底层帧分配器，
//! 但会影响 mmap/brk 的 overcommit 判断和 ABI 可见的内存统计。
//!
//! # Concurrency
//!
//! 所有 tunable 使用 relaxed atomic。它们是软限制/诊断值，不参与跨 CPU 数据同步。

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::config::PAGE_SIZE;

// Keep the ABI-visible commit window conservative. The QEMU kernels have less
// useful user mmap space and lower OOM recovery headroom than the raw RAM size
// suggests, and LTP tunable tests size their stress loops from CommitLimit.
#[cfg(all(target_arch = "loongarch64", feature = "boot_la_uboot_dmw"))]
const COMMIT_LIMIT_CAP_KB: usize = 64 * 1024;
#[cfg(not(all(target_arch = "loongarch64", feature = "boot_la_uboot_dmw")))]
const COMMIT_LIMIT_CAP_KB: usize = 512 * 1024;
const DEFAULT_OVERCOMMIT_MEMORY: usize = 0;
const DEFAULT_OVERCOMMIT_RATIO: usize = 50;
const DEFAULT_MAX_MAP_COUNT: usize = 65_530;
const DEFAULT_MIN_FREE_KBYTES: usize = 1_024;

static OVERCOMMIT_MEMORY: AtomicUsize = AtomicUsize::new(DEFAULT_OVERCOMMIT_MEMORY);
static OVERCOMMIT_RATIO: AtomicUsize = AtomicUsize::new(DEFAULT_OVERCOMMIT_RATIO);
static MAX_MAP_COUNT: AtomicUsize = AtomicUsize::new(DEFAULT_MAX_MAP_COUNT);
static MIN_FREE_KBYTES: AtomicUsize = AtomicUsize::new(DEFAULT_MIN_FREE_KBYTES);
static PANIC_ON_OOM: AtomicUsize = AtomicUsize::new(0);

/// 返回 overcommit 策略：0=heuristic，1=always，2=strict。
pub fn overcommit_memory() -> usize {
    OVERCOMMIT_MEMORY.load(Ordering::Relaxed)
}

/// 设置 overcommit 策略。
///
/// # Errors
///
/// 仅接受 Linux 兼容的 `0..=2`，其他值返回 `false`。
pub fn set_overcommit_memory(value: usize) -> bool {
    if value > 2 {
        return false;
    }
    OVERCOMMIT_MEMORY.store(value, Ordering::Relaxed);
    true
}

/// 返回 strict overcommit 使用的百分比。
pub fn overcommit_ratio() -> usize {
    OVERCOMMIT_RATIO.load(Ordering::Relaxed)
}

/// 设置 strict overcommit 使用的百分比。
pub fn set_overcommit_ratio(value: usize) {
    OVERCOMMIT_RATIO.store(value, Ordering::Relaxed);
}

/// 返回单进程用户 VMA 数量软上限。
pub fn max_map_count() -> usize {
    MAX_MAP_COUNT.load(Ordering::Relaxed)
}

/// 设置单进程用户 VMA 数量软上限。
///
/// # Errors
///
/// `0` 不是有效上限，返回 `false`。
pub fn set_max_map_count(value: usize) -> bool {
    if value == 0 {
        return false;
    }
    MAX_MAP_COUNT.store(value, Ordering::Relaxed);
    true
}

/// 返回 ABI 可见的最小空闲内存目标，单位 KiB。
pub fn min_free_kbytes() -> usize {
    MIN_FREE_KBYTES.load(Ordering::Relaxed)
}

/// 设置 ABI 可见的最小空闲内存目标，单位 KiB。
pub fn set_min_free_kbytes(value: usize) {
    MIN_FREE_KBYTES.store(value, Ordering::Relaxed);
}

/// 返回 OOM 时是否 panic 的兼容开关。
pub fn panic_on_oom() -> usize {
    PANIC_ON_OOM.load(Ordering::Relaxed)
}

/// 设置 OOM panic 兼容开关。
pub fn set_panic_on_oom(value: usize) {
    PANIC_ON_OOM.store(value, Ordering::Relaxed);
}

/// 返回当前 overcommit 允许的提交上限，单位字节。
pub fn commit_limit_bytes() -> usize {
    reported_memory_bytes()
        .saturating_mul(overcommit_ratio())
        .saturating_div(100)
        .min(COMMIT_LIMIT_CAP_KB.saturating_mul(1024))
}

/// 返回当前 overcommit 提交上限，单位 KiB。
pub fn commit_limit_kbytes() -> usize {
    commit_limit_bytes() / 1024
}

/// 返回当前任务地址空间已提交用户映射大小，单位 KiB。
pub fn committed_as_kbytes() -> usize {
    let vm = match crate::task::current_task_ref() {
        Some(task) => task.process.vm(),
        None => return 0,
    };
    let committed = vm.lock().committed_bytes() / 1024;
    committed
}

/// 判断新增提交量是否满足当前 overcommit 策略。
pub fn overcommit_allows(current_committed_bytes: usize, additional_bytes: usize) -> bool {
    match overcommit_memory() {
        1 => true,
        2 => current_committed_bytes.saturating_add(additional_bytes) <= commit_limit_bytes(),
        _ => additional_bytes <= reported_memory_bytes(),
    }
}

/// 返回 ABI 可见的总内存，单位 KiB。
pub fn total_memory_kbytes() -> usize {
    crate::hal::firmware::usable_memory_size() / 1024
}

fn reported_memory_bytes() -> usize {
    total_memory_kbytes().saturating_mul(1024)
}

/// 返回当前帧分配器可见的空闲物理内存，单位 KiB。
pub fn free_memory_kbytes() -> usize {
    crate::mm::unallocated_frames().saturating_mul(PAGE_SIZE) / 1024
}
