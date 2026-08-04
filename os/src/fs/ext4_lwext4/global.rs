//! Global serialization gate for the lwext4 C library.
//!
//! The lwext4 C library keeps **global** state (device registry, mount table,
//! block cache, journal, orphan list — see `lwext4_rust` bindings and the
//! upstream C sources).  Each `Ext4FileSystem` instance owns its own
//! `Mutex<Ext4BlockWrapper>` (`fs.lw`), so that lock alone serializes the C
//! entry points of *one* mount.  With multiple mounts on different CPUs, two
//! instances could enter C global tables concurrently through separate
//! instance locks.  `LWEXT4_GLOBAL` is a single process-wide mutex that all C
//! entry points must hold in addition to their instance lock.
//!
//! # Lock order (documented for every C-entry wrapper)
//!
//! The lwext4 C-library global gate is nested between the Rust PageCache
//! layers and the per-instance/per-inode locks:
//!
//! ```text
//! PageCache op_gate → PageEntry.data → LWEXT4_GLOBAL → fs.lw
//!     → Ext4InodeState.handle / cached_meta / paths / inode_states
//! ```
//!
//! Rules that keep the gate sound:
//! - Namespace-mutating paths (rename/unlink/create/mkdir/rmdir/symlink/link/
//!   mknod) complete PageCache flush **before** acquiring `LWEXT4_GLOBAL`;
//!   dirty-page writeback enters C through `with_file`, which acquires the
//!   gate itself, so flush must finish before the mutation section re-enters.
//! - `LWEXT4_GLOBAL` (a non-reentrant `spin::Mutex`) must never be held across
//!   faultable user accesses, IPI/TLB ack waits, context switches, or
//!   `OUTPUT_LOCK`.  Every wrapper below only runs kernel-bounce I/O and
//!   short Rust-side state updates under the gate.
//! - The C→Rust device down-calls in `blockdev.rs` (`MangoKernelDevOp`) run
//!   *inside* a C call that already holds the gate and must **not** re-acquire
//!   it (they never do).
//!
//! Helpers that only run under an already-held gate (`probe_inode_meta_locked`,
//! `validate_path_locked`, `Ext4InodeState::with_file`,
//! `FileGuard::new`/`file_close`) must NOT acquire `LWEXT4_GLOBAL` themselves;
//! only their public entry points do.

use spin::Mutex;

/// Process-wide gate serializing every lwext4 C entry point.
static LWEXT4_GLOBAL: Mutex<()> = Mutex::new(());

/// Run `f` while holding the process-wide lwext4 C-library gate.
///
/// Every C entry point wrapper acquires this gate first, then its per-instance
/// `fs.lw` lock, then any inode-state locks, per the documented order above.
pub(crate) fn with_lwext4_global<R>(f: impl FnOnce() -> R) -> R {
    let _g = LWEXT4_GLOBAL.lock();
    f()
}
