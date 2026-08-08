//! Final-barrier error-precedence regression for another_ext4 lifetime sync.

#[cfg(feature = "ext4_another_backend")]
use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(feature = "ext4_another_backend")]
use crate::fs::ext4_another::Ext4FileSystem;
#[cfg(feature = "ext4_another_backend")]
use crate::utils::error::SyscallErr;

#[cfg(feature = "ext4_another_backend")]
pub(super) fn test_partial_reclaim_still_runs_final_barrier_and_keeps_scoped_error(
) -> Result<(), &'static str> {
    let completed_reclaims = AtomicUsize::new(0);
    let final_flushes = AtomicUsize::new(0);
    let result = Ext4FileSystem::complete_lifetime_sync(
        || Ok(()),
        || {
            completed_reclaims.fetch_add(1, Ordering::SeqCst);
            Err(SyscallErr::EIO)
        },
        || {
            final_flushes.fetch_add(1, Ordering::SeqCst);
            Err(SyscallErr::ENOSPC)
        },
    );

    if completed_reclaims.load(Ordering::SeqCst) != 1 {
        return Err("lifetime sync fixture did not model partial reclaim progress");
    }
    if final_flushes.load(Ordering::SeqCst) != 1 {
        return Err("partial reclaim failure skipped the final device barrier");
    }
    match result {
        Err(SyscallErr::EIO) => Ok(()),
        _ => Err("final barrier error overrode the scoped reclaim error"),
    }
}
