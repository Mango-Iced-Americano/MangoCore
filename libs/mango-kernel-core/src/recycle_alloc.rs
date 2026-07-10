//! Recyclable integer-ID allocator.
//!
//! Extracted from `os/src/task/pid.rs::RecycleAllocator`.
//! Pure logic — no kernel dependencies, no I/O, no global state.
//!
//! The allocator maintains a monotonically-increasing counter and a stack of
//! freed IDs. `alloc()` prefers recycled IDs; `alloc_fresh()` prefers fresh
//! (monotonic) IDs and only falls back to recycling when approaching the
//! high watermark or when an explicit hint is set.

use alloc::vec::Vec;

/// Default maximum PID exposed via `/proc/sys/kernel/pid_max`.
pub const DEFAULT_PID_MAX: usize = 32_768;
const RESERVED_PID_REUSE_FLOOR: usize = 300;
const FRESH_REUSE_WATERMARK: usize = DEFAULT_PID_MAX - 1024;

/// A stack-+bitmap ID allocator with recycling semantics.
///
/// # Semantics
///
/// - `alloc()` prioritizes IDs from the recycled stack.
/// - `alloc_fresh()` keeps IDs monotonically increasing, only consuming
///   recycled IDs when `current >= FRESH_REUSE_WATERMARK` or when a
///   `fresh_reuse_hint` points to a recycled ID.
#[derive(Debug)]
pub struct RecycleAllocator {
    /// Next linear allocation ID.
    current: usize,
    /// Stack of freed IDs.
    recycled: Vec<usize>,
    /// O(1) membership bitmap for `recycled`.
    recycled_flags: Vec<bool>,
    /// One-shot reuse request (e.g. from `/proc/sys/kernel/ns_last_pid`).
    fresh_reuse_hint: Option<usize>,
}

impl Clone for RecycleAllocator {
    fn clone(&self) -> Self {
        Self {
            current: self.current,
            recycled: self.recycled.clone(),
            recycled_flags: self.recycled_flags.clone(),
            fresh_reuse_hint: self.fresh_reuse_hint,
        }
    }
}

impl Default for RecycleAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl RecycleAllocator {
    /// Create a new allocator starting at ID 1.
    pub fn new() -> Self {
        RecycleAllocator {
            current: 1,
            recycled: Vec::new(),
            recycled_flags: Vec::new(),
            fresh_reuse_hint: None,
        }
    }

    /// Allocate an ID, reusing a freed ID if one is available.
    pub fn alloc(&mut self) -> usize {
        if let Some(id) = self.alloc_recycled() {
            return id;
        }
        self.current += 1;
        let id = self.current - 1;
        self.ensure_flag_capacity(id);
        id
    }

    /// Allocate a fresh ID, avoiding recycled IDs when possible.
    ///
    /// Prefers monotonic growth. Falls back to recycled IDs only when
    /// `current >= FRESH_REUSE_WATERMARK` or when the `fresh_reuse_hint`
    /// points to a recycled ID.
    pub fn alloc_fresh(&mut self) -> usize {
        if let Some(id) = self.fresh_reuse_hint.take() {
            if id < self.current && self.is_recycled(id) {
                self.mark_recycled(id, false);
                return id;
            }
        }
        if self.current >= FRESH_REUSE_WATERMARK {
            if let Some(id) = self.alloc_recycled_for_fresh() {
                return id;
            }
        }
        self.current += 1;
        let id = self.current - 1;
        self.ensure_flag_capacity(id);
        id
    }

    /// Return the most-recently allocated (or hinted) ID.
    pub fn last_allocated(&self) -> usize {
        self.current.saturating_sub(1)
    }

    /// Set the hint for the next `alloc_fresh()` call.
    ///
    /// If `next >= self.current`, the counter is advanced directly.
    /// Otherwise, if `next` points to a recycled ID it becomes a one-shot hint.
    pub fn set_next_alloc_hint(&mut self, next: usize) {
        let next = next.max(1);
        if next >= self.current {
            self.current = next;
            self.ensure_flag_capacity(next);
            self.fresh_reuse_hint = None;
            return;
        }
        if self.is_recycled(next) {
            self.fresh_reuse_hint = Some(next);
        }
    }

    /// Return an ID to the recycled pool.
    ///
    /// # Panics
    ///
    /// Panics if `id` was never allocated or is already recycled.
    pub fn dealloc(&mut self, id: usize) {
        assert!(id < self.current);
        assert!(!self.is_recycled(id), "id {} has been deallocated!", id);
        self.mark_recycled(id, true);
        self.recycled.push(id);
    }

    /// Mark a previously-`alloc_fresh`'d ID as recyclable.
    ///
    /// Idempotent — silently ignores already-recycled IDs.
    pub fn release_fresh_id(&mut self, id: usize) {
        assert!(id < self.current);
        if !self.is_recycled(id) {
            self.mark_recycled(id, true);
            self.recycled.push(id);
        }
    }

    /// Count of IDs currently allocated (not recycled).
    pub fn get_allocated(&self) -> usize {
        let recycled_count = self.recycled_flags.iter().filter(|flag| **flag).count();
        self.current
            .saturating_sub(1)
            .saturating_sub(recycled_count)
    }

    /// Expose whether an ID is in the recycled pool (testing / diagnostics).
    pub fn is_recycled(&self, id: usize) -> bool {
        self.recycled_flags.get(id).copied().unwrap_or(false)
    }

    /// Mark an ID as recycled or not (testing / diagnostics).
    pub fn mark_recycled(&mut self, id: usize, value: bool) {
        self.ensure_flag_capacity(id);
        self.recycled_flags[id] = value;
    }

    // ── private helpers ────────────────────────────────────────────────

    fn ensure_flag_capacity(&mut self, id: usize) {
        if id >= self.recycled_flags.len() {
            self.recycled_flags.resize(id + 1, false);
        }
    }

    fn alloc_recycled(&mut self) -> Option<usize> {
        while let Some(id) = self.recycled.pop() {
            if self.is_recycled(id) {
                self.mark_recycled(id, false);
                return Some(id);
            }
        }
        None
    }

    fn alloc_recycled_for_fresh(&mut self) -> Option<usize> {
        let mut skipped_reserved = Vec::new();
        let mut allocated = None;
        while let Some(id) = self.recycled.pop() {
            if !self.is_recycled(id) {
                continue;
            }
            if id >= RESERVED_PID_REUSE_FLOOR {
                self.mark_recycled(id, false);
                allocated = Some(id);
                break;
            } else {
                skipped_reserved.push(id);
            }
        }
        while let Some(id) = skipped_reserved.pop() {
            self.recycled.push(id);
        }
        allocated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_alloc_returns_one() {
        let mut a = RecycleAllocator::new();
        assert_eq!(a.alloc(), 1);
    }

    #[test]
    fn alloc_increasing_ids() {
        let mut a = RecycleAllocator::new();
        assert_eq!(a.alloc(), 1);
        assert_eq!(a.alloc(), 2);
        assert_eq!(a.alloc(), 3);
    }

    #[test]
    fn dealloc_then_alloc_reuses() {
        let mut a = RecycleAllocator::new();
        let _ = a.alloc(); // 1
        let id2 = a.alloc(); // 2
        let _ = a.alloc(); // 3
        a.dealloc(id2);
        assert_eq!(a.alloc(), 2);
    }

    #[test]
    fn alloc_fresh_skips_recycled() {
        let mut a = RecycleAllocator::new();
        let id1 = a.alloc(); // 1
        let id2 = a.alloc(); // 2
        a.dealloc(id1);
        a.dealloc(id2);
        // alloc_fresh should ignore the recycled stack (below watermark)
        let fresh = a.alloc_fresh();
        assert!(fresh > 2, "alloc_fresh should not reuse recycled IDs when below watermark, got {}", fresh);
    }

    #[test]
    fn set_next_alloc_hint_advances_current() {
        let mut a = RecycleAllocator::new();
        a.set_next_alloc_hint(100);
        // current = 100 means next alloc will be 100
        assert_eq!(a.alloc(), 100);
    }

    #[test]
    fn set_next_alloc_hint_below_current_with_recycled() {
        let mut a = RecycleAllocator::new();
        let ids: Vec<usize> = (0..10).map(|_| a.alloc()).collect();
        a.dealloc(ids[3]); // recycle id 4
        a.set_next_alloc_hint(ids[3]); // id 4 < current, but is recycled → one-shot hint
        // alloc_fresh should consume the hint
        assert_eq!(a.alloc_fresh(), ids[3]);
        // Next alloc_fresh should continue from current (fresh, no hint)
        let next = a.alloc_fresh();
        assert_eq!(next, 11);
    }

    #[test]
    #[should_panic(expected = "has been deallocated")]
    fn double_dealloc_panics() {
        let mut a = RecycleAllocator::new();
        let id = a.alloc();
        a.dealloc(id);
        a.dealloc(id); // should panic
    }

    #[test]
    fn alloc_does_not_duplicate() {
        let mut a = RecycleAllocator::new();
        let mut seen = Vec::new();
        for _ in 0..1000 {
            let id = a.alloc();
            assert!(!seen.contains(&id), "duplicate id {}", id);
            seen.push(id);
        }
        // Free half and re-allocate
        for i in 0..500 {
            a.dealloc(seen[i]);
        }
        let mut reused = 0;
        for _ in 0..500 {
            let id = a.alloc();
            if seen.contains(&id) {
                reused += 1;
            } else {
                seen.push(id);
            }
        }
        assert!(reused > 0, "no IDs were reused after dealloc");
    }

    #[test]
    fn alloc_fresh_uses_recycled_above_watermark() {
        // Create an allocator past the FRESH_REUSE_WATERMARK
        let mut a = RecycleAllocator::new();
        a.current = FRESH_REUSE_WATERMARK;
        a.current += 1;
        // Allocate fresh to consume past-watermark positions
        let _ = a.alloc_fresh();
        let id = a.alloc_fresh();
        a.dealloc(id);
        // Now alloc_fresh should recycle (above watermark)
        let reused = a.alloc_fresh();
        assert_eq!(reused, id);
    }

    #[test]
    fn release_fresh_id_is_idempotent() {
        let mut a = RecycleAllocator::new();
        let id = a.alloc_fresh();
        a.release_fresh_id(id);
        a.release_fresh_id(id); // should not panic
        assert!(a.is_recycled(id));
    }

    #[test]
    fn get_allocated_counts_correctly() {
        let mut a = RecycleAllocator::new();
        // current = 1, no recycled → 0 allocated
        assert_eq!(a.get_allocated(), 0);
        let _ = a.alloc(); // id=1
        assert_eq!(a.get_allocated(), 1);
        let id2 = a.alloc(); // id=2
        assert_eq!(a.get_allocated(), 2);
        a.dealloc(id2);
        assert_eq!(a.get_allocated(), 1);
    }

    #[test]
    fn last_allocated_tracks() {
        let mut a = RecycleAllocator::new();
        a.alloc(); // 1
        a.alloc(); // 2
        assert_eq!(a.last_allocated(), 2);
    }

    #[test]
    fn default_creates_new() {
        let a = RecycleAllocator::default();
        assert_eq!(a.current, 1);
        assert!(a.recycled.is_empty());
    }

    #[test]
    fn clone_is_independent() {
        let mut a = RecycleAllocator::new();
        a.alloc(); // 1
        let id = a.alloc(); // 2
        a.dealloc(id);
        let mut b = a.clone();
        // a still has id 2 recycled
        assert_eq!(a.alloc(), 2);
        // b is independent
        assert_eq!(b.alloc(), 2);
    }
}
