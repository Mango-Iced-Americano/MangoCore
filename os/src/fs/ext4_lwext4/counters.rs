//! lwext4 performance probe counters.
//!
//! Instrument the lwext4 C-FFI code paths to diagnose why LTP test rate
//! dropped after migrating from the hand-written ext4 driver.
//! Counters use relaxed atomics — diagnostic-only, no synchronisation.

use core::sync::atomic::{AtomicUsize, Ordering};

// ── Metadata probe counters ─────────────────────────────────────────────

pub static LWEXT4_FIND_CALLS: AtomicUsize = AtomicUsize::new(0);
pub static LWEXT4_FIND_CYCLES: AtomicUsize = AtomicUsize::new(0);
pub static LWEXT4_FIND_CACHE_HIT: AtomicUsize = AtomicUsize::new(0);
pub static LWEXT4_FIND_CACHE_MISS: AtomicUsize = AtomicUsize::new(0);
pub static LWEXT4_PROBE_TYPE_CALLS: AtomicUsize = AtomicUsize::new(0);
pub static LWEXT4_PROBE_TYPE_CYCLES: AtomicUsize = AtomicUsize::new(0);
pub static LWEXT4_GET_INODE_ID_CALLS: AtomicUsize = AtomicUsize::new(0);
pub static LWEXT4_GET_INODE_ID_ENOENT: AtomicUsize = AtomicUsize::new(0);
pub static LWEXT4_GET_INODE_ID_CYCLES: AtomicUsize = AtomicUsize::new(0);
pub static LWEXT4_METADATA_COLD: AtomicUsize = AtomicUsize::new(0);
pub static LWEXT4_METADATA_HOT: AtomicUsize = AtomicUsize::new(0);
pub static LWEXT4_METADATA_COLD_CYCLES: AtomicUsize = AtomicUsize::new(0);
pub static LWEXT4_FILE_OPEN_CALLS: AtomicUsize = AtomicUsize::new(0);
pub static LWEXT4_FILE_OPEN_CYCLES: AtomicUsize = AtomicUsize::new(0);
pub static LWEXT4_FILE_SIZE_CALLS: AtomicUsize = AtomicUsize::new(0);
pub static LWEXT4_FILE_CLOSE_CALLS: AtomicUsize = AtomicUsize::new(0);
pub static LWEXT4_FILE_CLOSE_CYCLES: AtomicUsize = AtomicUsize::new(0);
pub static LWEXT4_DIR_ENTRIES_CALLS: AtomicUsize = AtomicUsize::new(0);
pub static LWEXT4_DIR_ENTRIES_CYCLES: AtomicUsize = AtomicUsize::new(0);
/// Pre-creation `get_inode_id()` calls (before create/mkdir/symlink/mknod).
pub static LWEXT4_CREATE_PRE_CHECK: AtomicUsize = AtomicUsize::new(0);
pub static LWEXT4_LOGICAL_SIZE_CALLS: AtomicUsize = AtomicUsize::new(0);
pub static LWEXT4_LOGICAL_SIZE_CYCLES: AtomicUsize = AtomicUsize::new(0);
pub static LWEXT4_ENSURE_PC_CALLS: AtomicUsize = AtomicUsize::new(0);
pub static LWEXT4_ENSURE_PC_CREATES: AtomicUsize = AtomicUsize::new(0);

// ── Helpers ──────────────────────────────────────────────────────────────

#[inline(always)]
fn load(c: &AtomicUsize) -> usize {
    c.load(Ordering::Relaxed)
}

/// Reset all counters to 0.
pub fn reset() {
    LWEXT4_FIND_CALLS.store(0, Ordering::Relaxed);
    LWEXT4_FIND_CYCLES.store(0, Ordering::Relaxed);
    LWEXT4_FIND_CACHE_HIT.store(0, Ordering::Relaxed);
    LWEXT4_FIND_CACHE_MISS.store(0, Ordering::Relaxed);
    LWEXT4_PROBE_TYPE_CALLS.store(0, Ordering::Relaxed);
    LWEXT4_PROBE_TYPE_CYCLES.store(0, Ordering::Relaxed);
    LWEXT4_GET_INODE_ID_CALLS.store(0, Ordering::Relaxed);
    LWEXT4_GET_INODE_ID_ENOENT.store(0, Ordering::Relaxed);
    LWEXT4_GET_INODE_ID_CYCLES.store(0, Ordering::Relaxed);
    LWEXT4_METADATA_COLD.store(0, Ordering::Relaxed);
    LWEXT4_METADATA_HOT.store(0, Ordering::Relaxed);
    LWEXT4_METADATA_COLD_CYCLES.store(0, Ordering::Relaxed);
    LWEXT4_FILE_OPEN_CALLS.store(0, Ordering::Relaxed);
    LWEXT4_FILE_OPEN_CYCLES.store(0, Ordering::Relaxed);
    LWEXT4_FILE_SIZE_CALLS.store(0, Ordering::Relaxed);
    LWEXT4_FILE_CLOSE_CALLS.store(0, Ordering::Relaxed);
    LWEXT4_FILE_CLOSE_CYCLES.store(0, Ordering::Relaxed);
    LWEXT4_DIR_ENTRIES_CALLS.store(0, Ordering::Relaxed);
    LWEXT4_DIR_ENTRIES_CYCLES.store(0, Ordering::Relaxed);
    LWEXT4_CREATE_PRE_CHECK.store(0, Ordering::Relaxed);
    LWEXT4_LOGICAL_SIZE_CALLS.store(0, Ordering::Relaxed);
    LWEXT4_LOGICAL_SIZE_CYCLES.store(0, Ordering::Relaxed);
    LWEXT4_ENSURE_PC_CALLS.store(0, Ordering::Relaxed);
    LWEXT4_ENSURE_PC_CREATES.store(0, Ordering::Relaxed);
}

/// Snapshot all counters for `print_snapshot()`.
pub fn snapshot() -> (
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
) {
    (
        load(&LWEXT4_FIND_CALLS),
        load(&LWEXT4_FIND_CYCLES),
        load(&LWEXT4_PROBE_TYPE_CALLS),
        load(&LWEXT4_PROBE_TYPE_CYCLES),
        load(&LWEXT4_GET_INODE_ID_CALLS),
        load(&LWEXT4_GET_INODE_ID_ENOENT),
        load(&LWEXT4_GET_INODE_ID_CYCLES),
        load(&LWEXT4_METADATA_COLD),
        load(&LWEXT4_METADATA_HOT),
        load(&LWEXT4_METADATA_COLD_CYCLES),
        load(&LWEXT4_FILE_OPEN_CALLS),
        load(&LWEXT4_FILE_OPEN_CYCLES),
        load(&LWEXT4_FILE_SIZE_CALLS),
        load(&LWEXT4_FILE_CLOSE_CALLS),
        load(&LWEXT4_FILE_CLOSE_CYCLES),
        load(&LWEXT4_DIR_ENTRIES_CALLS),
        load(&LWEXT4_DIR_ENTRIES_CYCLES),
        load(&LWEXT4_CREATE_PRE_CHECK),
        load(&LWEXT4_LOGICAL_SIZE_CALLS),
        load(&LWEXT4_LOGICAL_SIZE_CYCLES),
        load(&LWEXT4_ENSURE_PC_CALLS),
        load(&LWEXT4_FIND_CACHE_HIT),
        load(&LWEXT4_FIND_CACHE_MISS),
        load(&LWEXT4_ENSURE_PC_CREATES),
    )
}
