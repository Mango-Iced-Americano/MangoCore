use alloc::sync::{Arc, Weak};
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use crate::fs::{PageCache, PageCacheBackend};
use crate::utils::error::SyscallErr;

const PAGE_INDEX: usize = 1;
const PAGE_BYTE: u8 = 0xa5;
const READ_LEN: usize = 64;

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
}

impl PageCacheBackend for ReentrantBackend {
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

pub(super) fn test_read_page_reenters_same_cache() -> Result<(), &'static str> {
    let cache = PageCache::new();
    let backend = Arc::new(ReentrantBackend::new());
    backend.attach_cache(&cache);
    cache.set_backend(backend.clone());

    let frame = cache
        .frame_for_read(PAGE_INDEX)
        .map_err(|_| "PageCache direct read failed during backend re-entry")?;
    if !backend.reentered() {
        return Err("backend did not re-enter PageCache");
    }
    if frame.ppn.get_bytes_array()[..READ_LEN] != [PAGE_BYTE; READ_LEN] {
        return Err("PageCache returned an unexpected payload");
    }
    Ok(())
}
