//! lwext4-based ext4 filesystem driver (read-only Phase 3).
//!
//! Replace the legacy hand-written ext4 driver with the lightweight lwext4
//! C library behind the `lwext4` feature flag.  Only mount, metadata, find,
//! list, and read_at are implemented — no writes, creates, or deletes.
pub mod blockdev;
pub mod counters;
pub mod errno;
pub mod ext4fs;
pub(crate) mod global;
pub(crate) mod inode_state;
pub mod layout;
pub mod page_cache;

pub(crate) use global::with_lwext4_global;
