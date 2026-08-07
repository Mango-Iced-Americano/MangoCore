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
mod media;
#[cfg(feature = "ext4_another_backend")]
mod ownership;
#[cfg(feature = "ext4_another_backend")]
mod power_cut;
#[cfg(feature = "ext4_another_backend")]
mod persistence;
#[cfg(feature = "ext4_another_backend")]
mod sync;
#[cfg(feature = "ext4_another_backend")]
mod symlink;

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
                KernelTest::skip(
                    "ext4_another::clean_media_supports_metadata_lookup_and_page_reads",
                    "requires disk-backed another_ext4 fixture",
                ),
                KernelTest::skip(
                    "ext4_another::rejects_unreliable_flush_before_media_parse",
                    "requires disk-backed another_ext4 fixture",
                ),
                KernelTest::skip(
                    "ext4_another::root_inode_is_canonical_and_does_not_retain_filesystem",
                    "requires disk-backed another_ext4 fixture",
                ),
                KernelTest::skip(
                    "ext4_another::reopen_before_sync_reads_fresh_pagecache_data",
                    "requires disk-backed another_ext4 fixture",
                ),
                KernelTest::skip(
                    "ext4_another::writes_and_truncates_persist_across_independent_mounts",
                    "requires disk-backed another_ext4 fixture",
                ),
                KernelTest::skip(
                    "ext4_another::depth_one_leading_hole_writes",
                    "requires disk-backed another_ext4 fixture",
                ),
                KernelTest::skip(
                    "ext4_another::depth_one_leading_hole_truncate_succeeds",
                    "requires disk-backed another_ext4 fixture",
                ),
                KernelTest::skip(
                    "ext4_another::namespace_mutations_persist_across_independent_mounts",
                    "requires disk-backed another_ext4 fixture",
                ),
                KernelTest::skip(
                    "ext4_another::metadata_mode_persists_across_independent_mounts",
                    "requires disk-backed another_ext4 fixture",
                ),
                KernelTest::skip(
                    "ext4_another::short_symlink_persists_across_clean_remount",
                    "requires disk-backed another_ext4 fixture",
                ),
                KernelTest::skip(
                    "ext4_another::long_symlink_persists_across_clean_remount",
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
            KernelTest::new(
                "ext4_another::clean_media_supports_metadata_lookup_and_page_reads",
                media::test_clean_media_supports_metadata_lookup_and_page_reads,
            ),
            KernelTest::new(
                "ext4_another::rejects_unreliable_flush_before_media_parse",
                media::test_rejects_unreliable_flush_before_media_parse,
            ),
            KernelTest::new(
                "ext4_another::root_inode_is_canonical_and_does_not_retain_filesystem",
                ownership::test_root_inode_is_canonical_and_does_not_retain_filesystem,
            ),
            KernelTest::new(
                "ext4_another::reopen_before_sync_reads_fresh_pagecache_data",
                persistence::test_reopen_before_sync_reads_fresh_pagecache_data,
            ),
            KernelTest::new(
                "ext4_another::writes_and_truncates_persist_across_independent_mounts",
                persistence::test_writes_and_truncates_persist_across_independent_mounts,
            ),
            KernelTest::new(
                "ext4_another::depth_one_leading_hole_writes",
                persistence::test_depth_one_leading_hole_writes,
            ),
            KernelTest::new(
                "ext4_another::depth_one_leading_hole_truncate_succeeds",
                persistence::test_depth_one_leading_hole_truncate_succeeds,
            ),
            KernelTest::new(
                "ext4_another::namespace_mutations_persist_across_independent_mounts",
                persistence::test_namespace_mutations_persist_across_independent_mounts,
            ),
            KernelTest::new(
                "ext4_another::metadata_mode_persists_across_independent_mounts",
                persistence::test_metadata_mode_persists_across_independent_mounts,
            ),
            KernelTest::new(
                "ext4_another::short_symlink_persists_across_clean_remount",
                symlink::test_short_symlink_persists_across_clean_remount,
            ),
            KernelTest::new(
                "ext4_another::long_symlink_persists_across_clean_remount",
                symlink::test_long_symlink_persists_across_clean_remount,
            ),
        ]
    }
    #[cfg(not(feature = "ext4_another_backend"))]
    vec![]
}
