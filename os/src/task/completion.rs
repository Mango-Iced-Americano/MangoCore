use super::WaitQueue;
use spin::Mutex;

struct CompletionInner {
    done: bool,
    wait_queue: WaitQueue,
}

/// 一次性完成事件，用于 vfork 这类”等待另一条路径提交或退出”的同步。
pub struct Completion {
    inner: Mutex<CompletionInner>,
}

impl Completion {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(CompletionInner {
                done: false,
                wait_queue: WaitQueue::new(),
            }),
        }
    }

    pub fn complete(&self) -> bool {
        let mut inner = self.inner.lock();
        if inner.done {
            return false;
        }
        inner.done = true;
        inner.wait_queue.wake_all();
        true
    }

    pub fn is_completed(&self) -> bool {
        self.inner.lock().done
    }

    pub fn wait_interruptible(&self) -> super::WaitResult {
        WaitQueue::wait_event_interruptible_locked(
            &self.inner,
            |inner| &mut inner.wait_queue,
            |inner| inner.done.then_some(0),
        )
    }

    /// 不可中断地等待完成事件。
    /// 无 signal check、无 deadline，只会在 complete() 后返回。
    pub fn wait_uninterruptible(&self) {
        let _ = WaitQueue::wait_event_locked(
            &self.inner,
            |inner| &mut inner.wait_queue,
            |inner| inner.done.then_some(0),
        );
    }
}
