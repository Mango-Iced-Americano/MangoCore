//! PageCache locking and publication regressions.

use alloc::vec;
use alloc::vec::Vec;

use crate::kernel_tests::runner::KernelTest;

mod batch_prefetch;
mod global_flush;
mod read_reentry;
mod user_read;
mod user_write;
mod write_copy;
mod write_fixture;
mod writeback_retry;

/// Returns PageCache regressions in their established execution order.
pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new(
            "page_cache::read_page_reenters_same_cache",
            read_reentry::test_read_page_reenters_same_cache,
        ),
        KernelTest::new(
            "page_cache::before_copy_reentry_keeps_payload_unpublished",
            write_copy::test_before_copy_reentry_keeps_payload_unpublished,
        ),
        KernelTest::new(
            "page_cache::batch_prefetch_preserves_reentrant_dirty_winner",
            batch_prefetch::test_batch_prefetch_preserves_reentrant_dirty_winner,
        ),
        KernelTest::new(
            "page_cache::write_user_rejects_short_source_without_mutation",
            user_write::test_write_user_rejects_short_source_without_mutation,
        ),
        KernelTest::new(
            "page_cache::read_user_single_page_unaligned",
            user_read::test_read_user_single_page_unaligned,
        ),
        KernelTest::new(
            "page_cache::read_user_multi_page_multi_segment",
            user_read::test_read_user_multi_page_multi_segment,
        ),
        KernelTest::new(
            "page_cache::read_user_multi_page_unaligned_segments",
            user_read::test_read_user_multi_page_unaligned_segments,
        ),
        KernelTest::new(
            "page_cache::read_user_rejects_short_destination",
            user_read::test_read_user_rejects_short_destination,
        ),
        KernelTest::new(
            "page_cache::read_user_fills_partial_valid_page",
            user_read::test_read_user_fills_partial_valid_page,
        ),
        KernelTest::new(
            "page_cache::read_user_returns_eagain_during_loading_reentry",
            user_read::test_read_user_returns_eagain_during_loading_reentry,
        ),
        KernelTest::new(
            "page_cache::writeback_retries_transient_eagain",
            writeback_retry::test_writeback_retries_transient_eagain,
        ),
        KernelTest::with_timeout(
            "page_cache::global_flush_releases_registry_before_writeback",
            global_flush::test_global_flush_releases_registry_before_writeback,
            1_000,
        ),
    ]
}
