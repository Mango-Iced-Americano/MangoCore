use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::config::{SYSTEM_TASK_LIMIT, SYSTEM_TASK_SOFT_LIMIT};
use crate::println;
use crate::syscall::errno::EAGAIN;

static TASK_QUOTA_USED: AtomicUsize = AtomicUsize::new(0);
static TASK_QUOTA_SOFT_WARNED: AtomicBool = AtomicBool::new(false);

#[must_use]
pub(crate) struct TaskQuotaGuard {
    _private: (),
}

impl TaskQuotaGuard {
    pub(crate) fn try_acquire() -> Result<Self, isize> {
        let mut current = TASK_QUOTA_USED.load(Ordering::Acquire);
        loop {
            if current >= SYSTEM_TASK_LIMIT {
                println!(
                    "[task_quota] HARD LIMIT hit: used={}/{} returning EAGAIN",
                    current, SYSTEM_TASK_LIMIT
                );
                return Err(EAGAIN);
            }

            let next = current + 1;
            match TASK_QUOTA_USED.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    if next >= SYSTEM_TASK_SOFT_LIMIT
                        && !TASK_QUOTA_SOFT_WARNED.swap(true, Ordering::AcqRel)
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

    pub(crate) fn acquire_for_init() -> Self {
        Self::try_acquire().unwrap_or_else(|_| panic!("initproc cannot acquire task quota"))
    }
}

impl Drop for TaskQuotaGuard {
    fn drop(&mut self) {
        let old = TASK_QUOTA_USED.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(old > 0, "task quota underflow");
        if old <= SYSTEM_TASK_SOFT_LIMIT {
            TASK_QUOTA_SOFT_WARNED.store(false, Ordering::Release);
        }
    }
}

pub(crate) fn allocated_task_count() -> usize {
    TASK_QUOTA_USED.load(Ordering::Acquire)
}
