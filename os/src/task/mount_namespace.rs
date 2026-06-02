//! Mount namespace stub with unique ID.
//!
//! Operations are NOT isolated — this only provides a unique ID
//! so that CLONE_NEWNS / setns / procfs work without rejecting the flag.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};
use lazy_static::lazy_static;

/// Mount namespace stub.  Operations are NOT isolated — this only
/// provides a unique ID so that CLONE_NEWNS / setns / procfs work.
#[derive(Debug)]
pub struct MountNamespace {
    pub id: u64,
}

lazy_static! {
    pub static ref INIT_MOUNT_NAMESPACE: Arc<MountNamespace> =
        Arc::new(MountNamespace { id: 0 });
}

static NEXT_MNT_NS_ID: AtomicU64 = AtomicU64::new(1);

impl MountNamespace {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            id: NEXT_MNT_NS_ID.fetch_add(1, Ordering::Relaxed),
        })
    }
}
