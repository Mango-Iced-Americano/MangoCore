//! 任务数量 quota。
//!
//! clone/fork 路径通过 `TaskQuotaGuard` 预留一个任务或进程生命周期名额，Drop
//! 时归还。硬上限返回 Linux 兼容的 `-EAGAIN`，软上限只打印一次告警。

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::config::{SYSTEM_TASK_LIMIT, SYSTEM_TASK_SOFT_LIMIT};
use crate::println;
use crate::syscall::errno::EAGAIN;

static TASK_QUOTA_USED: AtomicUsize = AtomicUsize::new(0);
static TASK_QUOTA_SOFT_WARNED: AtomicBool = AtomicBool::new(false);

/// 硬上限：由 FDT 派生的可用 RAM 大小对编译期上限做运行时钳制。
fn task_limit() -> usize {
    let ram = crate::hal::firmware::usable_memory_size();
    let stack = crate::config::KERNEL_STACK_SIZE;
    let ram_limit = ram / (stack * 4);
    SYSTEM_TASK_LIMIT.min(ram_limit.max(512))
}

#[must_use]
/// 已占用任务 quota 的 RAII 句柄。
///
/// # Semantics
///
/// 持有该 guard 期间 `TASK_QUOTA_USED` 计数保持增加；guard drop 时自动递减。
/// 因此 clone/fork 的失败回滚路径只需要丢弃 guard。
pub(crate) struct TaskQuotaGuard {
    _private: (),
}

impl TaskQuotaGuard {
    /// 尝试获取一个任务 quota。
    ///
    /// # Errors
    ///
    /// 达到 `SYSTEM_TASK_LIMIT` 时返回 `-EAGAIN`。
    pub(crate) fn try_acquire() -> Result<Self, isize> {
        let limit = task_limit();
        let mut current = TASK_QUOTA_USED.load(Ordering::Relaxed);
        loop {
            if current >= limit {
                println!(
                    "[task_quota] HARD LIMIT hit: used={}/{} returning EAGAIN",
                    current, limit
                );
                return Err(EAGAIN);
            }

            let next = current + 1;
            match TASK_QUOTA_USED.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    if next >= SYSTEM_TASK_SOFT_LIMIT
                        && !TASK_QUOTA_SOFT_WARNED.swap(true, Ordering::Relaxed)
                    {
                        println!(
                            "[task_quota] SOFT LIMIT reached: used={}/{}",
                            next, SYSTEM_TASK_LIMIT
                        );
                    }
                    return Ok(Self { _private: () });
                }
                Err(observed) => current = observed,
            }
        }
    }

    /// 为 initproc 获取 quota。
    ///
    /// # Panics
    ///
    /// initproc 无法获取 quota 时 panic，因为系统无法继续启动。
    pub(crate) fn acquire_for_init() -> Self {
        Self::try_acquire().unwrap_or_else(|_| panic!("initproc cannot acquire task quota"))
    }
}

impl Drop for TaskQuotaGuard {
    fn drop(&mut self) {
        let old = TASK_QUOTA_USED.fetch_sub(1, Ordering::Relaxed);
        debug_assert!(old > 0, "task quota underflow");
        if old <= SYSTEM_TASK_SOFT_LIMIT {
            TASK_QUOTA_SOFT_WARNED.store(false, Ordering::Relaxed);
        }
    }
}

/// 返回当前已占用的任务 quota 数。
pub(crate) fn allocated_task_count() -> usize {
    TASK_QUOTA_USED.load(Ordering::Relaxed)
}
