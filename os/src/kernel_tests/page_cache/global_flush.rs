use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::config::PAGE_SIZE;
use crate::fs::{flush_all_page_caches, registry_stats, PageCache, PageCacheBackend};
use crate::utils::error::SyscallErr;

const WRITE_BYTE: u8 = 0x3c;

struct RegistryReentrantWritebackBackend {
    reentered: AtomicBool,
    observed_caches: AtomicUsize,
}

impl RegistryReentrantWritebackBackend {
    fn new() -> Self {
        Self {
            reentered: AtomicBool::new(false),
            observed_caches: AtomicUsize::new(0),
        }
    }
}

impl PageCacheBackend for RegistryReentrantWritebackBackend {
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

pub(super) fn test_global_flush_releases_registry_before_writeback() -> Result<(), &'static str> {
    let cache = PageCache::new();
    let backend = Arc::new(RegistryReentrantWritebackBackend::new());
    cache.set_backend(backend.clone());
    cache
        .write(0, &[WRITE_BYTE; PAGE_SIZE], Some(0))
        .map_err(|_| "PageCache setup write failed")?;

    flush_all_page_caches().map_err(|_| "global page-cache writeback failed")?;

    if !backend.reentered.load(Ordering::SeqCst) {
        return Err("writeback backend did not re-enter the PageCache registry query");
    }
    if backend.observed_caches.load(Ordering::SeqCst) == 0 {
        return Err("registry query did not observe the cache under writeback");
    }
    Ok(())
}
