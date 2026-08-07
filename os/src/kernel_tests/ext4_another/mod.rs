//! another_ext4 lifecycle and durability contract tests.
//!
//! These tests are compiled for both architectures.  The normal zero-drive
//! ktest fixture registers them as explicit skips; a disk-backed ktest or the
//! BuildStorm fixture executes the same tests against an independent mount.

use alloc::vec;

use crate::kernel_tests::runner::KernelTest;

#[cfg(feature = "ext4_another_backend")]
mod fixtures;
#[cfg(feature = "ext4_another_backend")]
mod power_cut;
#[cfg(feature = "ext4_another_backend")]
mod sync;

pub fn tests() -> alloc::vec::Vec<KernelTest> {
    #[cfg(feature = "ext4_another_backend")]
    {
        if crate::drivers::block::get_block_device(0).is_none() {
            return vec![
                KernelTest::skip(
                    "ext4_another::fsync_and_syncfs_surface_flush_failures",
                    "requires disk-backed another_ext4 fixture",
                ),
                KernelTest::skip(
                    "ext4_another::global_sys_sync_persists_across_unwrapped_device_view",
                    "requires disk-backed another_ext4 fixture",
                ),
                KernelTest::skip(
                    "ext4_another::close_does_not_trigger_durability_and_later_fsync_persists",
                    "requires disk-backed another_ext4 fixture",
                ),
                KernelTest::skip(
                    "ext4_another::unsynced_close_power_cut_replays_consistently",
                    "requires disk-backed another_ext4 fixture",
                ),
                KernelTest::skip(
                    "ext4_another::close_then_clean_unmount_persists_and_clears_recover",
                    "requires disk-backed another_ext4 fixture",
                ),
            ];
        }
        return vec![
            KernelTest::new(
                "ext4_another::fsync_and_syncfs_surface_flush_failures",
                sync::test_fsync_and_syncfs_surface_flush_failures,
            ),
            KernelTest::new(
                "ext4_another::global_sys_sync_persists_across_unwrapped_device_view",
                sync::test_global_sys_sync_persists_across_unwrapped_device_view,
            ),
            KernelTest::new(
                "ext4_another::close_does_not_trigger_durability_and_later_fsync_persists",
                sync::test_close_does_not_trigger_durability_and_later_fsync_persists,
            ),
            KernelTest::new(
                "ext4_another::unsynced_close_power_cut_replays_consistently",
                power_cut::test_unsynced_close_power_cut_replays_consistently,
            ),
            KernelTest::new(
                "ext4_another::close_then_clean_unmount_persists_and_clears_recover",
                power_cut::test_close_then_clean_unmount_persists_and_clears_recover,
            ),
        ]
    }
    #[cfg(not(feature = "ext4_another_backend"))]
    vec![]
}
