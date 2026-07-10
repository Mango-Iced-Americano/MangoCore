//! Generic ring buffer extracted from `os/src/net/socket/unix/ring_buffer.rs`.
//!
//! Uses `VecDeque<T>` with an explicit `capacity` bound. Supports push/pop,
//! batch push/pop, shutdown flags, and capacity queries.
//!
//! # Features
//!
//! - `"counters"`: enable global `RB_COUNT` / `RB_BYTES` atomic counters
//!   (enabled by default for kernel compatibility).

use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

// ── Optional global counters (feature-gated) ────────────────────────────

#[cfg(feature = "counters")]
static RB_COUNT: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "counters")]
static RB_BYTES: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "counters")]
pub fn rb_alive() -> usize {
    RB_COUNT.load(Ordering::Relaxed)
}

#[cfg(feature = "counters")]
pub fn rb_bytes() -> usize {
    RB_BYTES.load(Ordering::Relaxed)
}

// ────────────────────────────────────────────────────────────────────────
//  RingBuffer<T>
// ────────────────────────────────────────────────────────────────────────

/// Generic bounded queue backed by `VecDeque`.
///
/// Capacity limits the maximum number of items; writes beyond capacity
/// fail (return `None`). Shutdown flags use `AtomicBool` with
/// `Release`/`Acquire` ordering for cross-thread visibility.
#[derive(Debug)]
pub struct RingBuffer<T> {
    deque: VecDeque<T>,
    /// Maximum number of items this buffer can hold.
    capacity: usize,
    /// Peer has shut down reading (Release store, Acquire load).
    recv_shutdown: AtomicBool,
    /// Local write side has been shut down (Release store, Acquire load).
    send_shutdown: AtomicBool,
}

impl<T> RingBuffer<T> {
    pub fn new(capacity: usize) -> Self {
        #[cfg(feature = "counters")]
        {
            RB_COUNT.fetch_add(1, Ordering::Relaxed);
            RB_BYTES.fetch_add(capacity, Ordering::Relaxed);
        }
        Self {
            deque: VecDeque::with_capacity(capacity),
            capacity,
            recv_shutdown: AtomicBool::new(false),
            send_shutdown: AtomicBool::new(false),
        }
    }

    // ── Push (produce) ──────────────────────────────────────────────

    /// Push one item. Returns `Some(())` on success, `None` if full.
    pub fn push(&mut self, item: T) -> Option<()> {
        if self.deque.len() >= self.capacity {
            return None;
        }
        self.deque.push_back(item);
        Some(())
    }

    /// Push a slice of items atomically (all or nothing).
    /// Returns `Some(())` on success, `None` if insufficient space.
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

    // ── Pop (consume) ───────────────────────────────────────────────

    /// Pop one item. Returns `None` if empty.
    pub fn pop(&mut self) -> Option<T> {
        self.deque.pop_front()
    }

    /// Pop up to `buf.len()` items into `buf`.
    /// Returns the actual number of items read.
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

    // ── Drop on full (Unix socket behaviour) ────────────────────────

    /// Push an item, dropping the oldest item if the buffer is full.
    pub fn push_drop_oldest(&mut self, item: T) {
        while self.deque.len() >= self.capacity {
            self.deque.pop_front();
        }
        self.deque.push_back(item);
    }

    // ── Query ───────────────────────────────────────────────────────

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
        self.capacity.saturating_sub(self.deque.len())
    }

    pub fn cap(&self) -> usize {
        self.capacity
    }

    // ── Shutdown ────────────────────────────────────────────────────

    /// Mark that the peer has shut down reading.
    ///
    /// Uses `Release` ordering so all prior writes are visible to the
    /// reader's `Acquire` load on `is_recv_shutdown()`.
    pub fn set_recv_shutdown(&self) {
        self.recv_shutdown.store(true, Ordering::Release);
    }

    /// Check whether the peer has shut down reading.
    pub fn is_recv_shutdown(&self) -> bool {
        self.recv_shutdown.load(Ordering::Acquire)
    }

    /// Mark that local writes have been shut down.
    pub fn set_send_shutdown(&self) {
        self.send_shutdown.store(true, Ordering::Release);
    }

    /// Check whether the local write side has been shut down.
    pub fn is_send_shutdown(&self) -> bool {
        self.send_shutdown.load(Ordering::Acquire)
    }
}

impl<T> Drop for RingBuffer<T> {
    fn drop(&mut self) {
        #[cfg(feature = "counters")]
        {
            RB_COUNT.fetch_sub(1, Ordering::Relaxed);
            RB_BYTES.fetch_sub(self.capacity, Ordering::Relaxed);
        }
    }
}

// ────────────────────────────────────────────────────────────────────────
//  Tests
// ────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_empty_buffer() {
        let rb: RingBuffer<i32> = RingBuffer::new(16);
        assert!(rb.is_empty());
        assert!(!rb.is_full());
        assert_eq!(rb.len(), 0);
        assert_eq!(rb.free_len(), 16);
        assert_eq!(rb.cap(), 16);
    }

    #[test]
    fn push_then_pop_returns_value() {
        let mut rb = RingBuffer::new(4);
        assert_eq!(rb.push(42), Some(()));
        assert_eq!(rb.len(), 1);
        assert_eq!(rb.pop(), Some(42));
        assert!(rb.is_empty());
    }

    #[test]
    fn push_until_full_then_reject() {
        let mut rb = RingBuffer::new(3);
        assert_eq!(rb.push(1), Some(()));
        assert_eq!(rb.push(2), Some(()));
        assert_eq!(rb.push(3), Some(()));
        assert!(rb.is_full());
        assert_eq!(rb.push(4), None); // rejected
        assert_eq!(rb.len(), 3);
    }

    #[test]
    fn pop_from_empty_returns_none() {
        let mut rb: RingBuffer<i32> = RingBuffer::new(8);
        assert_eq!(rb.pop(), None);
    }

    #[test]
    fn push_slice_all_or_nothing() {
        let mut rb = RingBuffer::new(5);
        // fits
        assert_eq!(rb.push_slice(&[1, 2, 3]), Some(()));
        assert_eq!(rb.len(), 3);
        // doesn't fit (need 3 more, only 2 free)
        assert_eq!(rb.push_slice(&[4, 5, 6]), None);
        assert_eq!(rb.len(), 3); // unchanged
    }

    #[test]
    fn pop_slice_reads_correctly() {
        let mut rb = RingBuffer::new(8);
        rb.push(10);
        rb.push(20);
        rb.push(30);
        let mut buf = [0i32; 5];
        let n = rb.pop_slice(&mut buf);
        assert_eq!(n, 3);
        assert_eq!(buf[0], 10);
        assert_eq!(buf[1], 20);
        assert_eq!(buf[2], 30);
        assert_eq!(buf[3], 0); // untouched
        assert!(rb.is_empty());
    }

    #[test]
    fn pop_slice_with_smaller_buf() {
        let mut rb = RingBuffer::new(8);
        for i in 0..5 {
            rb.push(i);
        }
        let mut buf = [0i32; 3];
        let n = rb.pop_slice(&mut buf);
        assert_eq!(n, 3);
        assert_eq!(rb.len(), 2);
    }

    #[test]
    fn wrap_around_fill_drain_refill() {
        // simulate circular usage: fill, drain partially, refill
        let mut rb = RingBuffer::new(4);
        // fill
        for i in 0..4 {
            assert_eq!(rb.push(i), Some(()));
        }
        // drain 2
        assert_eq!(rb.pop(), Some(0));
        assert_eq!(rb.pop(), Some(1));
        assert_eq!(rb.len(), 2);
        // refill 2
        assert_eq!(rb.push(4), Some(()));
        assert_eq!(rb.push(5), Some(()));
        assert_eq!(rb.len(), 4);
        // drain all
        assert_eq!(rb.pop(), Some(2));
        assert_eq!(rb.pop(), Some(3));
        assert_eq!(rb.pop(), Some(4));
        assert_eq!(rb.pop(), Some(5));
        assert!(rb.is_empty());
    }

    #[test]
    fn len_free_len_cap_consistent() {
        let mut rb = RingBuffer::new(10);
        assert_eq!(rb.len(), 0);
        assert_eq!(rb.free_len(), 10);
        assert_eq!(rb.cap(), 10);

        rb.push(1);
        rb.push(2);
        assert_eq!(rb.len(), 2);
        assert_eq!(rb.free_len(), 8);
        assert_eq!(rb.len() + rb.free_len(), rb.cap());
    }

    #[test]
    fn shutdown_prevents_push() {
        let mut rb = RingBuffer::new(4);
        rb.set_send_shutdown();
        assert!(rb.is_send_shutdown());
        // push still works (shutdown is advisory for higher layers)
        assert_eq!(rb.push(1), Some(()));
    }

    #[test]
    fn shutdown_flags_independent() {
        let rb: RingBuffer<i32> = RingBuffer::new(4);
        assert!(!rb.is_recv_shutdown());
        assert!(!rb.is_send_shutdown());
        rb.set_recv_shutdown();
        assert!(rb.is_recv_shutdown());
        assert!(!rb.is_send_shutdown());
        rb.set_send_shutdown();
        assert!(rb.is_recv_shutdown());
        assert!(rb.is_send_shutdown());
    }

    #[test]
    fn drop_on_full_pops_oldest() {
        let mut rb = RingBuffer::new(3);
        rb.push(1);
        rb.push(2);
        rb.push(3);
        rb.push_drop_oldest(4);
        // oldest (1) should be dropped
        assert_eq!(rb.len(), 3);
        assert_eq!(rb.pop(), Some(2));
        assert_eq!(rb.pop(), Some(3));
        assert_eq!(rb.pop(), Some(4));
    }

    #[test]
    fn drop_on_full_multiple() {
        let mut rb = RingBuffer::new(2);
        rb.push(1);
        rb.push(2);
        rb.push_drop_oldest(3);
        rb.push_drop_oldest(4);
        assert_eq!(rb.pop(), Some(3));
        assert_eq!(rb.pop(), Some(4));
    }

    #[test]
    fn zero_capacity_edge() {
        let mut rb: RingBuffer<i32> = RingBuffer::new(0);
        assert!(rb.is_full());
        assert!(rb.is_empty());
        assert_eq!(rb.push(1), None);
        assert_eq!(rb.pop(), None);
        assert_eq!(rb.free_len(), 0);
    }

    #[test]
    fn large_capacity() {
        let mut rb = RingBuffer::new(1024);
        for i in 0..512 {
            assert_eq!(rb.push(i), Some(()));
        }
        assert_eq!(rb.len(), 512);
        for i in 0..512 {
            assert_eq!(rb.pop(), Some(i));
        }
        assert!(rb.is_empty());
    }

    #[test]
    fn drop_cleans_up() {
        // Basic smoke test: verify that RingBuffer can be dropped without panic
        let rb: RingBuffer<String> = RingBuffer::new(4);
        drop(rb);
    }
}
