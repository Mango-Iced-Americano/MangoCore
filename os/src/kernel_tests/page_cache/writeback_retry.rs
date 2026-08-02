use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::config::PAGE_SIZE;
use crate::fs::{PageCache, PageCacheBackend};
use crate::utils::error::SyscallErr;

const TRANSIENT_EAGAIN_ATTEMPTS: usize = 3;

struct TransientWritebackBackend {
    writes: AtomicUsize,
}

impl TransientWritebackBackend {
    const fn new() -> Self {
        Self {
            writes: AtomicUsize::new(0),
        }
    }

    fn writes(&self) -> usize {
        self.writes.load(Ordering::SeqCst)
    }
}

impl PageCacheBackend for TransientWritebackBackend {
    fn read_page(&self, _index: usize, buffer: &mut [u8]) -> Result<usize, SyscallErr> {
        buffer.fill(0);
        Ok(buffer.len())
    }

    fn write_page(&self, _index: usize, buffer: &[u8]) -> Result<usize, SyscallErr> {
        let attempt = self.writes.fetch_add(1, Ordering::SeqCst);
        if attempt < TRANSIENT_EAGAIN_ATTEMPTS {
            return Err(SyscallErr::EAGAIN);
        }
        Ok(buffer.len())
    }

    fn npages(&self) -> usize {
        1
    }
}

pub(super) fn test_writeback_retries_transient_eagain() -> Result<(), &'static str> {
    let cache = PageCache::new();
    let backend = Arc::new(TransientWritebackBackend::new());
    cache.set_backend(backend.clone());
    cache
        .write(0, &[0xA5; PAGE_SIZE], Some(0))
        .map_err(|_| "PageCache setup write failed")?;

    cache
        .writeback_all()
        .map_err(|_| "writeback leaked transient EAGAIN")?;

    if backend.writes() != TRANSIENT_EAGAIN_ATTEMPTS + 1 {
        return Err("writeback did not retry each transient EAGAIN");
    }
    if !cache.dirty_pages_snapshot().is_empty() {
        return Err("successful retry did not clear the dirty page");
    }
    Ok(())
}
