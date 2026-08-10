use alloc::sync::Arc;
use alloc::vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::config::PAGE_SIZE;
use crate::fs::{PageCache, PageCacheBackend, PageCacheFault, MAX_DEMAND_READ_PAGES};
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

struct FaultAroundBackend {
    calls: AtomicUsize,
    pages: AtomicUsize,
}

impl PageCacheBackend for FaultAroundBackend {
    fn read_page(&self, index: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        buf.fill(index as u8);
        Ok(buf.len())
    }

    fn read_pages(&self, start: usize, pages: &mut [&mut [u8]]) -> Result<usize, SyscallErr> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.pages.fetch_add(pages.len(), Ordering::SeqCst);
        for (offset, page) in pages.iter_mut().enumerate() {
            page.fill((start + offset) as u8);
        }
        Ok(pages.len() * PAGE_SIZE)
    }

    fn write_page(&self, _index: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
        Ok(buf.len())
    }

    fn npages(&self) -> usize {
        MAX_DEMAND_READ_PAGES * 2
    }
}

pub(super) fn test_filemap_fault_around_is_bounded_and_lock_outside() -> Result<(), &'static str> {
    let cache = PageCache::new();
    let backend = Arc::new(FaultAroundBackend {
        calls: AtomicUsize::new(0),
        pages: AtomicUsize::new(0),
    });
    cache.set_backend(backend.clone());

    let wait = match cache.try_frame_for_filemap_read_ahead(
        0,
        MAX_DEMAND_READ_PAGES * PAGE_SIZE,
        usize::MAX,
    ) {
        Err(PageCacheFault::Retry(wait)) => wait,
        _ => return Err("cold filemap fault-around did not return a retry token"),
    };
    if backend.calls.load(Ordering::SeqCst) != 0 {
        return Err("filemap fault-around performed I/O during VM-lock admission");
    }
    wait.wait();
    if backend.calls.load(Ordering::SeqCst) != 1
        || backend.pages.load(Ordering::SeqCst) != MAX_DEMAND_READ_PAGES
    {
        return Err("filemap fault-around did not issue one bounded contiguous read");
    }

    let last = MAX_DEMAND_READ_PAGES - 1;
    let frame = cache
        .try_frame_for_filemap_read(last, MAX_DEMAND_READ_PAGES * PAGE_SIZE)
        .map_err(|_| "last fault-around page was not resident")?;
    let byte = unsafe { frame.ppn.with_bytes(|bytes| bytes[0]) };
    if byte != last as u8 {
        return Err("fault-around page payload did not match its file offset");
    }
    Ok(())
}
