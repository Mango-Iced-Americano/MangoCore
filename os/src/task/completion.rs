//! 一次性完成事件。
//!
//! `Completion` 用于 `vfork` 这类父路径必须等待子路径 `execve` 或退出的同步点。
//! 它只表达“完成过一次”，不会自动复位，也不携带返回值。
//!
//! # Locking
//!
//! 内部 `Mutex` 同时保护完成状态和等待队列。等待路径通过
//! `WaitQueue::wait_event_*_locked` 在同一把锁下检查条件并注册等待者，
//! 避免完成事件与入队之间丢失唤醒。

use super::WaitQueue;
use spin::Mutex;

struct CompletionInner {
    done: bool,
    wait_queue: WaitQueue,
}

/// 一次性完成事件。
pub struct Completion {
    inner: Mutex<CompletionInner>,
}

impl Completion {
    /// 创建未完成状态的 `Completion`。
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(CompletionInner {
                done: false,
                wait_queue: WaitQueue::new(),
            }),
        }
    }

    /// 标记事件完成并唤醒所有等待者。
    ///
    /// # Semantics
    ///
    /// 首次调用返回 `true`；事件已经完成时返回 `false` 且不重复唤醒。
    pub fn complete(&self) -> bool {
        let mut inner = self.inner.lock();
        if inner.done {
            return false;
        }
        inner.done = true;
        inner.wait_queue.wake_all();
        true
    }

    /// 返回事件是否已经完成。
    pub fn is_completed(&self) -> bool {
        self.inner.lock().done
    }

    /// 可中断地等待完成事件。
    ///
    /// # Semantics
    ///
    /// 完成后返回 `WaitResult::Ready(0)`；等待期间遇到可处理信号时返回
    /// `WaitResult::Interrupted`。
    ///
    /// # Locking
    ///
    /// 调用者不得持有会被完成路径反向获取的锁。等待队列和完成位在
    /// `inner` 锁下同时检查。
    pub fn wait_interruptible(&self) -> super::WaitResult {
        WaitQueue::wait_event_interruptible_locked(
            &self.inner,
            |inner| &mut inner.wait_queue,
            |inner| inner.done.then_some(0),
        )
    }

    /// 不可中断地等待完成事件。
    ///
    /// # Semantics
    ///
    /// 不检查信号，也没有 deadline，只会在 `complete()` 后返回。`vfork`
    /// 父路径使用该接口，避免父进程因可处理信号在内核中自旋而阻止子进程运行。
    pub fn wait_uninterruptible(&self) {
        let _ = WaitQueue::wait_event_locked(
            &self.inner,
            |inner| &mut inner.wait_queue,
            |inner| inner.done.then_some(0),
        );
    }
}
