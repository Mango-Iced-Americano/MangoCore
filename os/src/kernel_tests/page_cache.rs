//! PageCache locking regressions.

use alloc::boxed::Box;
use alloc::sync::{Arc, Weak};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use spin::Mutex;

use crate::config::PAGE_SIZE;
use crate::fs::{PageCache, PageCacheBackend};
use crate::kernel_tests::runner::KernelTest;
use crate::mm::UserBuffer;
use crate::utils::error::SyscallErr;

const PAGE_INDEX: usize = 1;
const PAGE_BYTE: u8 = 0xa5;
const PREFETCH_BYTE: u8 = 0x5a;
const READ_LEN: usize = 64;

/// A backend which makes one nested lookup while its initial page read is in flight.
struct ReentrantBackend {
    cache: Mutex<Option<Weak<PageCache>>>,
    reentered: AtomicBool,
}

impl ReentrantBackend {
    fn new() -> Self {
        Self {
            cache: Mutex::new(None),
            reentered: AtomicBool::new(false),
        }
    }

    fn attach_cache(&self, cache: &Arc<PageCache>) {
        *self.cache.lock() = Some(Arc::downgrade(cache));
    }

    fn reentered(&self) -> bool {
        self.reentered.load(Ordering::SeqCst)
    }

    fn reenter_cache(&self, index: usize) -> Result<(), SyscallErr> {
        let cache = {
            let cache_ref = self.cache.lock();
            cache_ref
                .as_ref()
                .and_then(Weak::upgrade)
                .ok_or(SyscallErr::EIO)?
        };

        cache.frame_for_read(index).map(|_| ())
    }
}

impl PageCacheBackend for ReentrantBackend {
    fn read_page(&self, index: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        if self
            .reentered
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            self.reenter_cache(index)?;
        }

        buf.fill(PAGE_BYTE);
        Ok(buf.len())
    }

    fn write_page(&self, _index: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
        Ok(buf.len())
    }

    fn npages(&self) -> usize {
        PAGE_INDEX + 1
    }
}

struct BeforeCopyBackend {
    writes: AtomicUsize,
    written_byte: AtomicU8,
}

impl BeforeCopyBackend {
    fn new() -> Self {
        Self {
            writes: AtomicUsize::new(0),
            written_byte: AtomicU8::new(0),
        }
    }

    fn writes(&self) -> usize {
        self.writes.load(Ordering::SeqCst)
    }

    fn written_byte(&self) -> u8 {
        self.written_byte.load(Ordering::SeqCst)
    }
}

impl PageCacheBackend for BeforeCopyBackend {
    fn read_page(&self, _index: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        buf.fill(0);
        Ok(buf.len())
    }

    fn write_page(&self, _index: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
        let first_byte = *buf.first().ok_or(SyscallErr::EIO)?;
        self.written_byte.store(first_byte, Ordering::SeqCst);
        self.writes.fetch_add(1, Ordering::SeqCst);
        Ok(buf.len())
    }

    fn npages(&self) -> usize {
        PAGE_INDEX + 1
    }
}

struct BatchReentrantBackend {
    cache: Mutex<Option<Weak<PageCache>>>,
    reentered: AtomicBool,
    writes: AtomicUsize,
    written_byte: AtomicU8,
}

impl BatchReentrantBackend {
    fn new() -> Self {
        Self {
            cache: Mutex::new(None),
            reentered: AtomicBool::new(false),
            writes: AtomicUsize::new(0),
            written_byte: AtomicU8::new(0),
        }
    }

    fn attach_cache(&self, cache: &Arc<PageCache>) {
        *self.cache.lock() = Some(Arc::downgrade(cache));
    }

    fn reentered(&self) -> bool {
        self.reentered.load(Ordering::SeqCst)
    }

    fn writes(&self) -> usize {
        self.writes.load(Ordering::SeqCst)
    }

    fn written_byte(&self) -> u8 {
        self.written_byte.load(Ordering::SeqCst)
    }

    fn reenter_full_page_write(&self, index: usize) -> Result<(), SyscallErr> {
        let cache = {
            let cache_ref = self.cache.lock();
            cache_ref
                .as_ref()
                .and_then(Weak::upgrade)
                .ok_or(SyscallErr::EIO)?
        };
        let payload = vec![PAGE_BYTE; PAGE_SIZE];
        cache.write(index * PAGE_SIZE, &payload, Some(0)).map(|_| ())
    }
}

impl PageCacheBackend for BatchReentrantBackend {
    fn read_page(&self, _index: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        buf.fill(PREFETCH_BYTE);
        Ok(buf.len())
    }

    fn read_pages(&self, start_index: usize, pages: &mut [&mut [u8]]) -> Result<usize, SyscallErr> {
        if self
            .reentered
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            self.reenter_full_page_write(start_index)?;
        }
        for page in pages.iter_mut() {
            (*page).fill(PREFETCH_BYTE);
        }
        Ok(pages.len() * PAGE_SIZE)
    }

    fn write_page(&self, _index: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
        let first_byte = *buf.first().ok_or(SyscallErr::EIO)?;
        self.written_byte.store(first_byte, Ordering::SeqCst);
        self.writes.fetch_add(1, Ordering::SeqCst);
        Ok(buf.len())
    }

    fn npages(&self) -> usize {
        PAGE_INDEX + 1
    }
}

/// Returns PageCache regressions that require a re-entrant backend read.
pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new(
            "page_cache::read_page_reenters_same_cache",
            test_read_page_reenters_same_cache,
        ),
        KernelTest::new(
            "page_cache::before_copy_reentry_keeps_payload_unpublished",
            test_before_copy_reentry_keeps_payload_unpublished,
        ),
        KernelTest::new(
            "page_cache::batch_prefetch_preserves_reentrant_dirty_winner",
            test_batch_prefetch_preserves_reentrant_dirty_winner,
        ),
        KernelTest::new(
            "page_cache::write_user_rejects_short_source_without_mutation",
            test_write_user_rejects_short_source_without_mutation,
        ),
    ]
}

/// A missing page may be loaded by a backend that looks up the same page once.
fn test_read_page_reenters_same_cache() -> Result<(), &'static str> {
    let cache = PageCache::new();
    let backend = Arc::new(ReentrantBackend::new());
    backend.attach_cache(&cache);
    cache.set_backend(backend.clone());

    let frame = cache
        .frame_for_read(PAGE_INDEX)
        .map_err(|_| "PageCache direct read failed during backend re-entry")?;
    let payload = frame.ppn.get_bytes_array();

    if !backend.reentered() {
        return Err("backend did not re-enter PageCache");
    }
    if payload[..READ_LEN] != [PAGE_BYTE; READ_LEN] {
        return Err("PageCache returned an unexpected payload");
    }

    Ok(())
}

fn test_before_copy_reentry_keeps_payload_unpublished() -> Result<(), &'static str> {
    let cache = PageCache::new();
    let backend = Arc::new(BeforeCopyBackend::new());
    cache.set_backend(backend.clone());
    let read_blocked = Arc::new(AtomicBool::new(false));
    let read_byte = Arc::new(AtomicU8::new(0));
    let callback_cache = cache.clone();
    let callback_read_blocked = read_blocked.clone();
    let callback_read_byte = read_byte.clone();
    let payload = vec![PAGE_BYTE; PAGE_SIZE];

    cache
        .write_with_before_copy(
            PAGE_INDEX * PAGE_SIZE,
            &payload,
            Some(0),
            move |_| {
                match callback_cache.frame_for_read(PAGE_INDEX) {
                    Err(SyscallErr::EAGAIN) => callback_read_blocked.store(true, Ordering::SeqCst),
                    Ok(frame) => callback_read_byte
                        .store(frame.ppn.get_bytes_array()[0], Ordering::SeqCst),
                    Err(_) => return Err(SyscallErr::EIO),
                }
                callback_cache.writeback_page(PAGE_INDEX)
            },
        )
        .map_err(|_| "PageCache write failed during before-copy re-entry")?;
    cache
        .writeback_page(PAGE_INDEX)
        .map_err(|_| "PageCache writeback failed after payload copy")?;

    if backend.writes() != 1 || backend.written_byte() != PAGE_BYTE {
        return Err("re-entrant writeback consumed the page before its payload was copied");
    }
    if !read_blocked.load(Ordering::SeqCst) {
        if read_byte.load(Ordering::SeqCst) == 0 {
            return Err("re-entrant read observed invalid zero data before payload copy");
        }
        return Err("re-entrant read observed a page before payload copy");
    }

    Ok(())
}

fn test_batch_prefetch_preserves_reentrant_dirty_winner() -> Result<(), &'static str> {
    let cache = PageCache::new();
    let backend = Arc::new(BatchReentrantBackend::new());
    backend.attach_cache(&cache);
    cache.set_backend(backend.clone());

    cache
        .sync_batch_read_pages(PAGE_INDEX, 1)
        .map_err(|_| "PageCache batch prefetch failed during backend re-entry")?;
    let frame = cache
        .frame_for_read(PAGE_INDEX)
        .map_err(|_| "PageCache did not retain a page after batch prefetch")?;
    let cached_byte = frame.ppn.get_bytes_array()[0];
    cache
        .writeback_page(PAGE_INDEX)
        .map_err(|_| "PageCache writeback failed after batch prefetch")?;

    if !backend.reentered() {
        return Err("batch backend did not re-enter PageCache");
    }
    if backend.writes() != 1 || backend.written_byte() != PAGE_BYTE {
        return Err("batch prefetch discarded the re-entrant dirty winner before writeback");
    }
    if cached_byte != PAGE_BYTE {
        return Err("batch prefetch overwrote the re-entrant winner payload");
    }

    Ok(())
}

fn test_write_user_rejects_short_source_without_mutation() -> Result<(), &'static str> {
    let cache = PageCache::new();
    let backend = Arc::new(BeforeCopyBackend::new());
    cache.set_backend(backend.clone());
    let payload = vec![PAGE_BYTE; PAGE_SIZE];
    cache
        .write(PAGE_INDEX * PAGE_SIZE, &payload, Some(0))
        .map_err(|_| "PageCache setup write failed")?;

    let state_before = cache.state_of(PAGE_INDEX);
    let dirty_count_before = cache.dirty_count();
    if state_before.is_none() || !cache.is_dirty(PAGE_INDEX) {
        return Err("PageCache setup did not create a dirty cached page");
    }

    let source = UserBuffer::new(vec![Box::leak(vec![0; 1].into_boxed_slice())]);
    match cache.write_user(
        PAGE_INDEX * PAGE_SIZE,
        source.len() + 1,
        &source,
        Some(0),
    ) {
        Err(SyscallErr::EFAULT) => {}
        Err(_) => return Err("short UserBuffer write returned the wrong error"),
        Ok(_) => return Err("short UserBuffer write unexpectedly succeeded"),
    }

    let frame = cache
        .frame_for_read(PAGE_INDEX)
        .map_err(|_| "short UserBuffer write changed the cached page state")?;
    if frame.ppn.get_bytes_array()[..READ_LEN] != [PAGE_BYTE; READ_LEN] {
        return Err("short UserBuffer write changed the cached payload");
    }
    if cache.state_of(PAGE_INDEX) != state_before {
        return Err("short UserBuffer write changed the cached page state");
    }
    if !cache.is_dirty(PAGE_INDEX) || cache.dirty_count() != dirty_count_before {
        return Err("short UserBuffer write changed dirty-page membership");
    }

    cache
        .writeback_page(PAGE_INDEX)
        .map_err(|_| "PageCache writeback failed after short UserBuffer write")?;
    if backend.writes() != 1 || backend.written_byte() != PAGE_BYTE {
        return Err("short UserBuffer write changed subsequent writeback data");
    }

    Ok(())
}
