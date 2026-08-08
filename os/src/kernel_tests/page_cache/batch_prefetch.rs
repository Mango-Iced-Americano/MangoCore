use alloc::sync::{Arc, Weak};
use alloc::vec;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use spin::Mutex;

use crate::config::PAGE_SIZE;
use crate::fs::{PageCache, PageCacheBackend};
use crate::utils::error::SyscallErr;

const PAGE_INDEX: usize = 1;
const PAGE_BYTE: u8 = 0xa5;
const PREFETCH_BYTE: u8 = 0x5a;

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

    fn reenter_full_page_write(&self, index: usize) -> Result<(), SyscallErr> {
        let cache = self
            .cache
            .lock()
            .as_ref()
            .and_then(Weak::upgrade)
            .ok_or(SyscallErr::EIO)?;
        cache.write(index * PAGE_SIZE, &vec![PAGE_BYTE; PAGE_SIZE], Some(0))?;
        Ok(())
    }
}

impl PageCacheBackend for BatchReentrantBackend {
    fn read_page(&self, _index: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        buf.fill(PREFETCH_BYTE);
        Ok(buf.len())
    }

    fn read_pages(&self, start: usize, pages: &mut [&mut [u8]]) -> Result<usize, SyscallErr> {
        if self
            .reentered
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            self.reenter_full_page_write(start)?;
        }
        for page in pages.iter_mut() {
            (*page).fill(PREFETCH_BYTE);
        }
        Ok(pages.len() * PAGE_SIZE)
    }

    fn write_page(&self, _index: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
        self.written_byte
            .store(*buf.first().ok_or(SyscallErr::EIO)?, Ordering::SeqCst);
        self.writes.fetch_add(1, Ordering::SeqCst);
        Ok(buf.len())
    }

    fn npages(&self) -> usize {
        PAGE_INDEX + 1
    }
}

pub(super) fn test_batch_prefetch_preserves_reentrant_dirty_winner() -> Result<(), &'static str> {
    let cache = PageCache::new();
    let backend = Arc::new(BatchReentrantBackend::new());
    backend.attach_cache(&cache);
    cache.set_backend(backend.clone());

    cache
        .sync_batch_read_pages(PAGE_INDEX, 1)
        .map_err(|_| "PageCache batch prefetch failed during backend re-entry")?;
    let cached_byte = cache
        .frame_for_read(PAGE_INDEX)
        .map_err(|_| "PageCache did not retain a page after batch prefetch")?
        .ppn
        .get_bytes_array()[0];
    cache
        .writeback_page(PAGE_INDEX)
        .map_err(|_| "PageCache writeback failed after batch prefetch")?;

    if !backend.reentered.load(Ordering::SeqCst) {
        return Err("batch backend did not re-enter PageCache");
    }
    if backend.writes.load(Ordering::SeqCst) != 1
        || backend.written_byte.load(Ordering::SeqCst) != PAGE_BYTE
    {
        return Err("batch prefetch discarded the re-entrant dirty winner before writeback");
    }
    if cached_byte != PAGE_BYTE {
        return Err("batch prefetch overwrote the re-entrant winner payload");
    }
    Ok(())
}
