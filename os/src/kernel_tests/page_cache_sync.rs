//! PageCache synchronization regressions migrated from develop.

use alloc::sync::{Arc, Weak};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

use crate::config::PAGE_SIZE;
use crate::fs::{flush_all_page_caches, registry_stats, PageCache, PageCacheBackend};
use crate::kernel_tests::runner::KernelTest;
use crate::utils::error::SyscallErr;

fn frame_first_byte(frame: &crate::mm::Frame) -> u8 {
    match frame {
        crate::mm::Frame::InMemory(frame) => unsafe {
            *frame.ppn.start_addr().direct_map_ptr()
        },
        _ => 0,
    }
}

const REENTRY_PAGE: usize = 1;

struct ReentrantReadBackend {
    cache: Mutex<Option<Weak<PageCache>>>,
    reentered: AtomicBool,
}

impl ReentrantReadBackend {
    fn attach(&self, cache: &Arc<PageCache>) {
        *self.cache.lock() = Some(Arc::downgrade(cache));
    }
}

impl PageCacheBackend for ReentrantReadBackend {
    fn read_page(&self, index: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        if self
            .reentered
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let cache = self
                .cache
                .lock()
                .as_ref()
                .and_then(Weak::upgrade)
                .ok_or(SyscallErr::EIO)?;
            cache.frame_for_read(index)?;
        }
        buf.fill(0xa5);
        Ok(buf.len())
    }

    fn write_page(&self, _index: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
        Ok(buf.len())
    }

    fn npages(&self) -> usize {
        REENTRY_PAGE + 1
    }
}

struct BatchReentrantBackend {
    cache: Mutex<Option<Weak<PageCache>>>,
    reentered: AtomicBool,
    writes: AtomicUsize,
    written_byte: AtomicUsize,
}

impl BatchReentrantBackend {
    fn attach(&self, cache: &Arc<PageCache>) {
        *self.cache.lock() = Some(Arc::downgrade(cache));
    }
}

impl PageCacheBackend for BatchReentrantBackend {
    fn read_page(&self, index: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        if self
            .reentered
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let cache = self
                .cache
                .lock()
                .as_ref()
                .and_then(Weak::upgrade)
                .ok_or(SyscallErr::EIO)?;
            cache
                .write_kernel(index * PAGE_SIZE, &[0x5a; PAGE_SIZE], 0)
                .map_err(|_| SyscallErr::EIO)?;
        }
        buf.fill(0xa5);
        Ok(buf.len())
    }

    fn write_page(&self, _index: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.written_byte
            .store(buf.first().copied().unwrap_or(0) as usize, Ordering::SeqCst);
        Ok(buf.len())
    }

    fn npages(&self) -> usize {
        REENTRY_PAGE + 1
    }
}

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
        KernelTest::new(
            "page_cache::read_backend_reentry_does_not_deadlock",
            test_read_backend_reentry_does_not_deadlock,
        ),
        KernelTest::new(
            "page_cache::batch_prefetch_preserves_reentrant_dirty_winner",
            test_batch_prefetch_preserves_reentrant_dirty_winner,
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

fn test_read_backend_reentry_does_not_deadlock() -> Result<(), &'static str> {
    let cache = PageCache::new();
    let backend = Arc::new(ReentrantReadBackend {
        cache: Mutex::new(None),
        reentered: AtomicBool::new(false),
    });
    backend.attach(&cache);
    cache.set_backend(backend.clone());
    let frame = cache
        .frame_for_read(REENTRY_PAGE)
        .map_err(|_| "re-entrant backend read failed")?;
    if !backend.reentered.load(Ordering::SeqCst) {
        return Err("backend did not re-enter PageCache");
    }
    if frame_first_byte(&crate::mm::Frame::InMemory(frame)) != 0xa5 {
        return Err("re-entrant backend returned an unexpected payload");
    }
    Ok(())
}

fn test_batch_prefetch_preserves_reentrant_dirty_winner() -> Result<(), &'static str> {
    let cache = PageCache::new();
    let backend = Arc::new(BatchReentrantBackend {
        cache: Mutex::new(None),
        reentered: AtomicBool::new(false),
        writes: AtomicUsize::new(0),
        written_byte: AtomicUsize::new(0),
    });
    backend.attach(&cache);
    cache.set_backend(backend.clone());
    cache
        .sync_batch_read_pages(REENTRY_PAGE, 1)
        .map_err(|_| "batch prefetch failed during backend re-entry")?;
    let frame = cache
        .frame_for_read(REENTRY_PAGE)
        .map_err(|_| "batch prefetch did not retain its winner")?;
    if frame_first_byte(&crate::mm::Frame::InMemory(frame)) != 0x5a {
        return Err("batch prefetch overwrote the re-entrant dirty winner");
    }
    cache
        .writeback_page(REENTRY_PAGE)
        .map_err(|_| "re-entrant dirty winner writeback failed")?;
    if backend.writes.load(Ordering::SeqCst) != 1
        || backend.written_byte.load(Ordering::SeqCst) != 0x5a
    {
        return Err("batch prefetch discarded the re-entrant dirty payload");
    }
    Ok(())
}
