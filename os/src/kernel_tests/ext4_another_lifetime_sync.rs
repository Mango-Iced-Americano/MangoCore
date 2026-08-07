//! Generation/lifetime sync error-precedence regression for another_ext4.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::fs::ext4_another::Ext4FileSystem;
use crate::kernel_tests::runner::KernelTest;
use crate::utils::error::SyscallErr;

pub(crate) fn tests() -> alloc::vec::Vec<KernelTest> {
    alloc::vec![KernelTest::new(
        "ext4_another_lifetime::partial_reclaim_runs_final_barrier",
        test_partial_reclaim_runs_final_barrier,
    )]
}

fn test_partial_reclaim_runs_final_barrier() -> Result<(), &'static str> {
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
        return Err("lifetime sync did not run the reclaim phase");
    }
    if final_flushes.load(Ordering::SeqCst) != 1 {
        return Err("lifetime sync skipped the final device barrier");
    }
    match result {
        Err(SyscallErr::EIO) => Ok(()),
        _ => Err("final barrier error replaced the scoped reclaim error"),
    }
}
