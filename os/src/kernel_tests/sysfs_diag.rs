//! Focused sysfs diagnostics registration tests.

use alloc::vec;

#[cfg(all(feature = "perf_diag", feature = "ext4_another_backend"))]
use crate::fs::sysfs::{files, SysFS};
#[cfg(all(feature = "perf_diag", feature = "ext4_another_backend"))]
use crate::fs::vfs::IndexNode;
use crate::kernel_tests::runner::KernelTest;

pub fn tests() -> alloc::vec::Vec<KernelTest> {
    #[cfg(all(feature = "perf_diag", feature = "ext4_another_backend"))]
    {
        vec![KernelTest::new(
            "sysfs_diag::another_ext4_prepare_stats_endpoint_is_registered",
            test_another_ext4_prepare_stats_endpoint_is_registered,
        )]
    }

    #[cfg(not(all(feature = "perf_diag", feature = "ext4_another_backend")))]
    vec![]
}

#[cfg(all(feature = "perf_diag", feature = "ext4_another_backend"))]
fn test_another_ext4_prepare_stats_endpoint_is_registered() -> Result<(), &'static str> {
    let sysfs = SysFS::new();
    files::register_all(sysfs.root()).map_err(|_| "sysfs diagnostics registration failed")?;
    let kernel = sysfs
        .root()
        .find("kernel")
        .map_err(|_| "perf_diag did not register /sys/kernel")?;
    let stats = kernel
        .find("stats")
        .map_err(|_| "perf_diag did not register /sys/kernel/stats")?;
    stats
        .find("another_ext4")
        .map_err(|_| "perf_diag did not register /sys/kernel/stats/another_ext4")?;
    Ok(())
}
