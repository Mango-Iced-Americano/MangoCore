//! PageCache synchronization regressions migrated from develop.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::config::PAGE_SIZE;
use crate::fs::{flush_all_page_caches, registry_stats, PageCache, PageCacheBackend};
use crate::kernel_tests::runner::KernelTest;
use crate::utils::error::SyscallErr;

struct RegistryReentrantBackend {
    reentered: AtomicBool,
    observed_caches: AtomicUsize,
}

impl PageCacheBackend for RegistryReentrantBackend {
    fn read_page(&self, _index: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        buf.fill(0);
        Ok(buf.len())
    }

    fn write_page(&self, _index: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
        let (_, _, alive, _) = registry_stats();
        self.observed_caches.store(alive, Ordering::SeqCst);
        self.reentered.store(true, Ordering::SeqCst);
        Ok(buf.len())
    }

    fn npages(&self) -> usize {
        1
    }
}

struct TransientBackend {
    writes: AtomicUsize,
}

impl PageCacheBackend for TransientBackend {
    fn read_page(&self, _index: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        buf.fill(0);
        Ok(buf.len())
    }

    fn write_page(&self, _index: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
        let attempt = self.writes.fetch_add(1, Ordering::SeqCst);
        if attempt < 3 {
            return Err(SyscallErr::EAGAIN);
        }
        Ok(buf.len())
    }

    fn npages(&self) -> usize {
        1
    }
}

pub(crate) fn tests() -> alloc::vec::Vec<KernelTest> {
    alloc::vec![
        KernelTest::with_timeout(
            "page_cache::global_flush_releases_registry_before_writeback",
            test_global_flush_releases_registry_before_writeback,
            1_000,
        ),
        KernelTest::new(
            "page_cache::writeback_retries_transient_eagain",
            test_writeback_retries_transient_eagain,
        ),
    ]
}

fn test_global_flush_releases_registry_before_writeback() -> Result<(), &'static str> {
    let cache = PageCache::new();
    let backend = Arc::new(RegistryReentrantBackend {
        reentered: AtomicBool::new(false),
        observed_caches: AtomicUsize::new(0),
    });
    cache.set_backend(backend.clone());
    cache
        .write_kernel(0, &[0x3c; PAGE_SIZE], 0)
        .map_err(|_| "PageCache setup write failed")?;

    flush_all_page_caches().map_err(|_| "global page-cache writeback failed")?;
    if !backend.reentered.load(Ordering::SeqCst) {
        return Err("writeback backend did not re-enter the registry");
    }
    if backend.observed_caches.load(Ordering::SeqCst) == 0 {
        return Err("registry re-entry did not observe the live cache");
    }
    Ok(())
}

fn test_writeback_retries_transient_eagain() -> Result<(), &'static str> {
    let cache = PageCache::new();
    let backend = Arc::new(TransientBackend {
        writes: AtomicUsize::new(0),
    });
    cache.set_backend(backend.clone());
    cache
        .write_kernel(0, &[0xa5; PAGE_SIZE], 0)
        .map_err(|_| "PageCache setup write failed")?;

    cache
        .writeback_all()
        .map_err(|_| "writeback did not retry transient EAGAIN")?;
    if backend.writes.load(Ordering::SeqCst) != 4 {
        return Err("writeback retry count was incorrect");
    }
    if !cache.dirty_pages_snapshot().is_empty() {
        return Err("successful retry left the page dirty");
    }
    Ok(())
}
