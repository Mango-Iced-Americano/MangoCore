use alloc::sync::Arc;
use alloc::vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::config::PAGE_SIZE;
use crate::fs::{PageCache, PageCacheBackend, PageCacheFault};
use crate::utils::error::SyscallErr;

struct CountingBackend {
    reads: AtomicUsize,
}

impl PageCacheBackend for CountingBackend {
    fn read_page(&self, index: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        buf.fill(0x40 + index as u8);
        Ok(buf.len())
    }

    fn write_page(&self, _index: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
        Ok(buf.len())
    }

    fn npages(&self) -> usize {
        2
    }
}

pub(super) fn test_filemap_admission_defers_backend_io() -> Result<(), &'static str> {
    let cache = PageCache::new();
    let backend = Arc::new(CountingBackend {
        reads: AtomicUsize::new(0),
    });
    cache.set_backend(backend.clone());

    let wait = match cache.try_frame_for_filemap_read(0, PAGE_SIZE * 2) {
        Err(PageCacheFault::Retry(wait)) => wait,
        _ => return Err("cold filemap read did not return a retry token"),
    };
    if backend.reads.load(Ordering::SeqCst) != 0 {
        return Err("filemap admission performed backend I/O under the VM lock");
    }
    wait.wait();
    let frame = cache
        .try_frame_for_filemap_read(0, PAGE_SIZE * 2)
        .map_err(|_| "filemap read did not become resident after lock-outside load")?;
    let first_byte = unsafe { frame.ppn.with_bytes(|bytes| bytes[0]) };
    if backend.reads.load(Ordering::SeqCst) != 1 || first_byte != 0x40 {
        return Err("filemap lock-outside read returned an invalid page");
    }

    let mut private = vec![0u8; PAGE_SIZE];
    let wait = match cache.try_copy_page_for_private(1, &mut private, PAGE_SIZE * 2) {
        Err(PageCacheFault::Retry(wait)) => wait,
        _ => return Err("cold private fault did not return a retry token"),
    };
    if backend.reads.load(Ordering::SeqCst) != 1 {
        return Err("private-fault admission performed backend I/O under the VM lock");
    }
    wait.wait();
    cache
        .try_copy_page_for_private(1, &mut private, PAGE_SIZE * 2)
        .map_err(|_| "private fault did not copy after lock-outside load")?;
    if backend.reads.load(Ordering::SeqCst) != 2 || private[0] != 0x41 {
        return Err("private lock-outside read returned an invalid page");
    }
    Ok(())
}
