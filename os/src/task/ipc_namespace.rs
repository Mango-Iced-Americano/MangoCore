//! IPC namespace stub with unique ID.
//!
//! Operations are NOT isolated — this only provides a unique ID
//! so that CLONE_NEWIPC / setns / procfs work without rejecting the flag.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};
use lazy_static::lazy_static;

/// IPC namespace stub.  Operations are NOT isolated — this only
/// provides a unique ID so that CLONE_NEWIPC / setns / procfs work.
#[derive(Debug)]
pub struct IpcNamespace {
    pub id: u64,
}

lazy_static! {
    pub static ref INIT_IPC_NAMESPACE: Arc<IpcNamespace> =
        Arc::new(IpcNamespace { id: 0 });
}

static NEXT_IPC_NS_ID: AtomicU64 = AtomicU64::new(1);

impl IpcNamespace {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            id: NEXT_IPC_NS_ID.fetch_add(1, Ordering::Relaxed),
        })
    }
}
