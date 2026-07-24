//! In-kernel contract tests for the feature-gated another_ext4 writable bridge.

use alloc::vec;

use crate::kernel_tests::runner::KernelTest;

#[cfg(feature = "ext4_another_backend")]
mod fixtures;
#[cfg(feature = "ext4_another_backend")]
mod mapped_overwrite;
#[cfg(feature = "ext4_another_backend")]
mod media;
#[cfg(feature = "ext4_another_backend")]
mod ownership;
#[cfg(feature = "ext4_another_backend")]
mod persistence;
#[cfg(feature = "ext4_another_backend")]
mod recording_device;
#[cfg(feature = "ext4_another_backend")]
mod sync;
#[cfg(feature = "ext4_another_backend")]
mod symlink;
#[cfg(feature = "ext4_another_backend")]
mod writeback_observer;

/// Returns all another_ext4 bridge tests.
pub fn tests() -> alloc::vec::Vec<KernelTest> {
    #[cfg(feature = "ext4_another_backend")]
    {
        vec![
            KernelTest::new(
                "ext4_another::fully_mapped_overwrite_uses_fast_path",
                mapped_overwrite::test_fully_mapped_overwrite_uses_fast_path,
            ),
            KernelTest::new(
                "ext4_another::pure_overwrite_performs_no_allocation",
                mapped_overwrite::test_pure_overwrite_performs_no_allocation,
            ),
            KernelTest::new(
                "ext4_another::sparse_write_retains_fallback",
                mapped_overwrite::test_sparse_write_retains_fallback,
            ),
            KernelTest::new(
                "ext4_another::extending_write_retains_allocation",
                mapped_overwrite::test_extending_write_retains_allocation,
            ),
            KernelTest::new(
                "ext4_another::rejects_unreliable_flush_before_media_parse",
                media::test_rejects_unreliable_flush_before_media_parse,
            ),
            KernelTest::new(
                "ext4_another::clean_media_supports_metadata_lookup_and_page_reads",
                media::test_clean_media_supports_metadata_lookup_and_page_reads,
            ),
            KernelTest::new(
                "ext4_another::writes_and_truncates_persist_across_independent_mounts",
                persistence::test_writes_and_truncates_persist_across_independent_mounts,
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
                "ext4_another::fsync_and_syncfs_surface_flush_failures",
                sync::test_fsync_and_syncfs_surface_flush_failures,
            ),
            KernelTest::new(
                "ext4_another::global_sys_sync_persists_across_unwrapped_device_view",
                sync::test_global_sys_sync_persists_across_unwrapped_device_view,
            ),
            KernelTest::new(
                "ext4_another::root_inode_is_canonical_and_does_not_retain_filesystem",
                ownership::test_root_inode_is_canonical_and_does_not_retain_filesystem,
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
