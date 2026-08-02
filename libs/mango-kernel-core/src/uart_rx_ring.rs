//! Fixed-capacity single-producer/single-consumer byte ring for IRQ RX paths.

use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

/// A bounded byte ring with one reserved slot to distinguish full from empty.
///
/// The producer writes a byte then Release-publishes `head`; the consumer
/// Acquire-loads `head`, reads that byte, then Release-publishes `tail`.
/// Consequently, an IRQ producer and task-context consumer never access the
/// same slot concurrently. `CAPACITY` must be greater than one.
pub struct ByteRing<const CAPACITY: usize> {
    bytes: [AtomicU8; CAPACITY],
    head: AtomicUsize,
    tail: AtomicUsize,
}

impl<const CAPACITY: usize> ByteRing<CAPACITY> {
    /// Construct an empty ring.
    pub const fn new() -> Self {
        assert!(CAPACITY > 1, "UART RX ring needs a reserved slot");
        Self {
            bytes: [const { AtomicU8::new(0) }; CAPACITY],
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    const fn next(index: usize) -> usize {
        (index + 1) % CAPACITY
    }

    /// Push one byte from the sole producer. Returns false without overwriting
    /// unread data when the ring is full.
    pub fn push(&self, byte: u8) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let next = Self::next(head);
        if next == self.tail.load(Ordering::Acquire) {
            return false;
        }
        self.bytes[head].store(byte, Ordering::Relaxed);
        self.head.store(next, Ordering::Release);
        true
    }

    /// Pop one byte in FIFO order from the sole consumer.
    pub fn pop(&self) -> Option<u8> {
        let tail = self.tail.load(Ordering::Relaxed);
        if tail == self.head.load(Ordering::Acquire) {
            return None;
        }
        let byte = self.bytes[tail].load(Ordering::Relaxed);
        self.tail.store(Self::next(tail), Ordering::Release);
        Some(byte)
    }

    /// Return true when the producer can enqueue another byte.
    pub fn has_space(&self) -> bool {
        let head = self.head.load(Ordering::Acquire);
        Self::next(head) != self.tail.load(Ordering::Acquire)
    }
}

impl<const CAPACITY: usize> Default for ByteRing<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::ByteRing;

    #[test]
    fn preserves_fifo_order_across_wraparound() {
        let ring = ByteRing::<4>::new();

        assert!(ring.push(b'a'));
        assert!(ring.push(b'b'));
        assert_eq!(ring.pop(), Some(b'a'));
        assert!(ring.push(b'c'));
        assert!(ring.push(b'd'));

        assert_eq!(ring.pop(), Some(b'b'));
        assert_eq!(ring.pop(), Some(b'c'));
        assert_eq!(ring.pop(), Some(b'd'));
        assert_eq!(ring.pop(), None);
    }

    #[test]
    fn rejects_new_byte_when_ring_is_full() {
        let ring = ByteRing::<4>::new();

        assert!(ring.push(b'a'));
        assert!(ring.push(b'b'));
        assert!(ring.push(b'c'));
        assert!(!ring.push(b'd'));

        assert_eq!(ring.pop(), Some(b'a'));
        assert_eq!(ring.pop(), Some(b'b'));
        assert_eq!(ring.pop(), Some(b'c'));
    }
}
