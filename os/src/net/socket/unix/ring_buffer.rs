//! Unix 域 socket 环形缓冲区
//!
//! 使用 `Mutex<VecDeque<T>>` 实现的通用环形缓冲区。
//! 相比 DragonOS 的原子 head/tail + RwSem 方案大幅简化。
//!
//! # 注意
//! - 不是固定容量的循环缓冲区（VecDeque 内部可自动扩容），
//!   但通过 `capacity` 字段限制消息总数
//! - 支持 shutdown 标志位

use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicBool, Ordering};

/// 通用环形缓冲区
#[derive(Debug)]
pub struct RingBuffer<T> {
    /// 实际数据存储
    deque: VecDeque<T>,
    /// 最大容量（消息/字节数）
    capacity: usize,
    /// 对端已关闭读取（本端写入应失败）
    recv_shutdown: AtomicBool,
    /// 本端已关闭写入（对端读取将得到 EOF）
    send_shutdown: AtomicBool,
}

impl<T> RingBuffer<T> {
    /// 创建新的环形缓冲区
    ///
    /// `capacity` 是最大元素数限制（非字节数）。
    pub fn new(capacity: usize) -> Self {
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

    /// 缓冲区是否为空
    pub fn is_empty(&self) -> bool {
        self.deque.is_empty()
    }

    /// 缓冲区是否已满
    pub fn is_full(&self) -> bool {
        self.deque.len() >= self.capacity
    }

    /// 当前元素数
    pub fn len(&self) -> usize {
        self.deque.len()
    }

    /// 剩余可用空间
    pub fn free_len(&self) -> usize {
        self.capacity - self.deque.len()
    }

    /// 最大容量
    pub fn cap(&self) -> usize {
        self.capacity
    }

    // ── Shutdown ──────────────────────────────────────

    /// 设置对端关闭了读取（本端继续写入时将得到 EPIPE）
    pub fn set_recv_shutdown(&self) {
        self.recv_shutdown.store(true, Ordering::Release);
    }

    /// 对端是否已关闭读取
    pub fn is_recv_shutdown(&self) -> bool {
        self.recv_shutdown.load(Ordering::Acquire)
    }

    /// 设置本端关闭了写入（对端读取将得到 EOF）
    pub fn set_send_shutdown(&self) {
        self.send_shutdown.store(true, Ordering::Release);
    }

    /// 本端是否已关闭写入
    pub fn is_send_shutdown(&self) -> bool {
        self.send_shutdown.load(Ordering::Acquire)
    }
}
