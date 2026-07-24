//! Unix 域 socket 环形缓冲区
//!
//! 使用 `Mutex<VecDeque<T>>` 实现的通用环形缓冲区。
//! 相比 DragonOS 的原子 head/tail + RwSem 方案大幅简化。
//!
//! # Limitations
//!
//! - 不是固定容量的循环缓冲区（`VecDeque` 内部可自动扩容），
//!   但通过 `capacity` 字段限制消息总数
//! - 支持 shutdown 标志位（`Release`/`Acquire` 语义）

use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static RB_COUNT: AtomicUsize = AtomicUsize::new(0);
static RB_BYTES: AtomicUsize = AtomicUsize::new(0);
pub fn rb_alive() -> usize {
    RB_COUNT.load(Ordering::Relaxed)
}
pub fn rb_bytes() -> usize {
    RB_BYTES.load(Ordering::Relaxed)
}

/// 通用环形缓冲区
#[derive(Debug)]
pub struct RingBuffer<T> {
    deque: VecDeque<T>,
    /// 最大容量（消息/字节数）
    capacity: usize,
    /// 对端已关闭读取（`Release` store，`Acquire` load）
    recv_shutdown: AtomicBool,
    /// 本端已关闭写入（`Release` store，`Acquire` load）
    send_shutdown: AtomicBool,
}

impl<T> RingBuffer<T> {
    pub fn new(capacity: usize) -> Self {
        RB_COUNT.fetch_add(1, Ordering::Relaxed);
        RB_BYTES.fetch_add(capacity, Ordering::Relaxed);
        Self {
            deque: VecDeque::with_capacity(capacity),
            capacity,
            recv_shutdown: AtomicBool::new(false),
            send_shutdown: AtomicBool::new(false),
        }
    }

    // ── 写入（生产） ───────────────────────────────────

    /// 尝试推入一个元素。缓冲区满时返回 `None`。
    pub fn push(&mut self, item: T) -> Option<()> {
        if self.deque.len() >= self.capacity {
            return None;
        }
        self.deque.push_back(item);
        Some(())
    }

    /// 尝试批量推入多个元素。空间不足时返回 `None`，且不修改缓冲区。
    pub fn push_slice(&mut self, items: &[T]) -> Option<()>
    where
        T: Clone,
    {
        if self.deque.len() + items.len() > self.capacity {
            return None;
        }
        for item in items {
            self.deque.push_back(item.clone());
        }
        Some(())
    }

    // ── 读取（消费） ───────────────────────────────────

    /// 尝试弹出一个元素。缓冲区为空时返回 `None`。
    pub fn pop(&mut self) -> Option<T> {
        self.deque.pop_front()
    }

    /// 尝试批量弹出元素到 `buf`，返回实际读取的元素数。
    pub fn pop_slice(&mut self, buf: &mut [T]) -> usize
    where
        T: Default,
    {
        let n = buf.len().min(self.deque.len());
        for i in 0..n {
            buf[i] = self.deque.pop_front().unwrap_or_default();
        }
        n
    }

    // ── 查询 ───────────────────────────────────────────

    pub fn is_empty(&self) -> bool {
        self.deque.is_empty()
    }

    pub fn is_full(&self) -> bool {
        self.deque.len() >= self.capacity
    }

    pub fn len(&self) -> usize {
        self.deque.len()
    }

    pub fn free_len(&self) -> usize {
        self.capacity - self.deque.len()
    }

    pub fn cap(&self) -> usize {
        self.capacity
    }

    // ── Shutdown ──────────────────────────────────────

    /// 设置对端关闭了读取。
    ///
    /// 使用 `Release` 语义确保本端在设置此标志之前的所有写入对 `is_recv_shutdown`
    /// 的 `Acquire` 读取可见（本端后续 `write()` → `EPIPE` 路径）。
    pub fn set_recv_shutdown(&self) {
        self.recv_shutdown.store(true, Ordering::Release);
    }

    /// 查询对端是否已关闭读取（`Acquire` 语义，配对 `set_recv_shutdown` 的 `Release`）。
    pub fn is_recv_shutdown(&self) -> bool {
        self.recv_shutdown.load(Ordering::Acquire)
    }

    /// 设置本端关闭了写入。
    ///
    /// 使用 `Release` 语义确保对端 `Acquire` load 后能观察到本端不再生产新数据。
    pub fn set_send_shutdown(&self) {
        self.send_shutdown.store(true, Ordering::Release);
    }

    /// 查询本端是否已关闭写入（`Acquire` 语义，配对 `set_send_shutdown` 的 `Release`）。
    pub fn is_send_shutdown(&self) -> bool {
        self.send_shutdown.load(Ordering::Acquire)
    }
}

impl<T> Drop for RingBuffer<T> {
    fn drop(&mut self) {
        RB_COUNT.fetch_sub(1, Ordering::Relaxed);
        RB_BYTES.fetch_sub(self.capacity, Ordering::Relaxed);
    }
}
