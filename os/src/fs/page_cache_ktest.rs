use alloc::{sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use super::{
    global_dirty_pages, global_writeback_pages, PageCache, PageCacheBackend, PageEntry, PageState,
    GLOBAL_DIRTY_PAGES, GLOBAL_WRITEBACK_PAGES,
};
use crate::{config::PAGE_SIZE, utils::error::SyscallErr};

const PAGE_BYTES: [u8; 3] = [0x11, 0x22, 0x33];

#[derive(Clone)]
struct WriteBatch {
    start: usize,
    pages: Vec<Vec<u8>>,
}

struct WritebackBackend {
    fail_reads: AtomicBool,
    fail_writes: AtomicBool,
    batches: Mutex<Vec<WriteBatch>>,
}

impl WritebackBackend {
    fn new(fail_reads: bool, fail_writes: bool) -> Self {
        Self {
            fail_reads: AtomicBool::new(fail_reads),
            fail_writes: AtomicBool::new(fail_writes),
            batches: Mutex::new(Vec::new()),
        }
    }

    fn set_fail_reads(&self, fail: bool) {
        self.fail_reads.store(fail, Ordering::SeqCst);
    }

    fn set_fail_writes(&self, fail: bool) {
        self.fail_writes.store(fail, Ordering::SeqCst);
    }

    fn batches(&self) -> Vec<WriteBatch> {
        self.batches.lock().clone()
    }
}

impl PageCacheBackend for WritebackBackend {
    fn read_page(&self, index: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        if self.fail_reads.load(Ordering::SeqCst) {
            return Err(SyscallErr::EIO);
        }
        buf.fill(PAGE_BYTES.get(index).copied().unwrap_or(0));
        Ok(buf.len())
    }

    fn write_page(&self, index: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
        self.write_pages(index, &[buf])
    }

    fn write_pages(&self, start: usize, pages: &[&[u8]]) -> Result<usize, SyscallErr> {
        self.batches.lock().push(WriteBatch {
            start,
            pages: pages.iter().map(|page| page.to_vec()).collect(),
        });
        if self.fail_writes.load(Ordering::SeqCst) {
            return Err(SyscallErr::EIO);
        }
        Ok(pages.len() * PAGE_SIZE)
    }

    fn npages(&self) -> usize {
        PAGE_BYTES.len()
    }
}

fn fill_dirty_pages(cache: &Arc<PageCache>) -> Result<(), &'static str> {
    let mut payload = Vec::with_capacity(PAGE_BYTES.len() * PAGE_SIZE);
    for byte in PAGE_BYTES {
        payload.extend_from_slice(&vec![byte; PAGE_SIZE]);
    }
    cache
        .write(0, &payload, Some(0))
        .map(|_| ())
        .map_err(|_| "PageCache setup write failed")
}

fn entry(cache: &PageCache, index: usize) -> Result<Arc<PageEntry>, &'static str> {
    let entries = cache.entries.lock();
    entries
        .get(index)
        .and_then(|entry| entry.as_ref().cloned())
        .ok_or("PageCache entry missing")
}

fn claim_without_clearing_dirty(cache: &PageCache, index: usize) -> Result<(), &'static str> {
    let entry = entry(cache, index)?;
    {
        let _entries = cache.entries.lock();
        entry
            .compare_exchange_state(PageState::Dirty as u8, PageState::Writeback as u8)
            .map_err(|_| "PageCache competing claim did not acquire Dirty page")?;
    }
    GLOBAL_DIRTY_PAGES.fetch_sub(1, Ordering::Relaxed);
    GLOBAL_WRITEBACK_PAGES.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

fn complete_competing_claim(cache: &PageCache, index: usize) -> Result<(), &'static str> {
    let entry = entry(cache, index)?;
    {
        let _entries = cache.entries.lock();
        entry.set_state(PageState::UpToDate);
    }
    GLOBAL_WRITEBACK_PAGES.fetch_sub(1, Ordering::Relaxed);
    cache.inner.lock().clear_dirty(index);
    Ok(())
}

fn restore_claim(cache: &PageCache, index: usize) -> Result<(), &'static str> {
    let entry = entry(cache, index)?;
    if entry.state() != PageState::Writeback {
        return Ok(());
    }
    {
        let _entries = cache.entries.lock();
        entry.set_state(PageState::Dirty);
    }
    GLOBAL_DIRTY_PAGES.fetch_add(1, Ordering::Relaxed);
    GLOBAL_WRITEBACK_PAGES.fetch_sub(1, Ordering::Relaxed);
    cache.inner.lock().mark_dirty(index);
    Ok(())
}

fn page_is_dirty(cache: &PageCache, index: usize, dirty: usize, writeback: usize) -> bool {
    cache.state_of(index) == Some(PageState::Dirty)
        && cache.dirty_pages_snapshot().as_slice() == [index]
        && global_dirty_pages() == dirty
        && global_writeback_pages() == writeback
}

fn batch_matches(batch: &WriteBatch, start: usize, byte: u8) -> bool {
    batch.start == start
        && batch.pages.len() == 1
        && batch.pages[0].len() == PAGE_SIZE
        && batch.pages[0].iter().all(|&value| value == byte)
}

impl PageCache {
    pub(crate) fn ktest_writeback_splits_noncontiguous_claims() -> Result<(), &'static str> {
        let dirty_before = global_dirty_pages();
        let writeback_before = global_writeback_pages();
        let cache = PageCache::new();
        let backend = Arc::new(WritebackBackend::new(false, false));
        cache.set_backend(backend.clone());
        fill_dirty_pages(&cache)?;
        claim_without_clearing_dirty(&cache, 1)?;

        let writeback_result = cache.writeback_all();
        let batches = backend.batches();
        complete_competing_claim(&cache, 1)?;

        if writeback_result.is_err()
            || batches.len() != 2
            || !batch_matches(&batches[0], 0, PAGE_BYTES[0])
            || !batch_matches(&batches[1], 2, PAGE_BYTES[2])
            || global_dirty_pages() != dirty_before
            || global_writeback_pages() != writeback_before
        {
            return Err("writeback sent noncontiguous claimed pages in one backend batch");
        }
        Ok(())
    }

    pub(crate) fn ktest_writeback_failures_restore_dirty_pages() -> Result<(), &'static str> {
        let dirty_before = global_dirty_pages();
        let writeback_before = global_writeback_pages();
        let cache = PageCache::new();
        let backend = Arc::new(WritebackBackend::new(true, false));
        cache.set_backend(backend.clone());
        cache
            .write(0, &vec![PAGE_BYTES[0]; PAGE_SIZE], Some(0))
            .map_err(|_| "PageCache setup write failed")?;
        entry(&cache, 0)?.valid_mask.store(0, Ordering::Release);

        let prepare_failed = cache.writeback_range(0, 0).is_err();
        let restored_after_prepare = page_is_dirty(&cache, 0, dirty_before + 1, writeback_before);
        if !restored_after_prepare {
            restore_claim(&cache, 0)?;
        }

        backend.set_fail_reads(false);
        backend.set_fail_writes(true);
        let io_failed = cache.writeback_range(0, 0).is_err();
        let restored_after_io = page_is_dirty(&cache, 0, dirty_before + 1, writeback_before);
        if !restored_after_io {
            restore_claim(&cache, 0)?;
        }

        backend.set_fail_writes(false);
        let retry_succeeded = cache.writeback_range(0, 0).is_ok();
        let batches = backend.batches();
        let clean_after_retry = cache.state_of(0) == Some(PageState::UpToDate)
            && cache.dirty_pages_snapshot().is_empty()
            && global_dirty_pages() == dirty_before
            && global_writeback_pages() == writeback_before;

        if !prepare_failed {
            return Err("prepare failure did not reach writeback");
        }
        if !restored_after_prepare {
            return Err("prepare failure did not restore Dirty state and counters");
        }
        if !io_failed {
            return Err("backend failure did not reach writeback");
        }
        if !restored_after_io {
            return Err("backend failure did not restore Dirty state and counters");
        }
        if !retry_succeeded {
            return Err("restored Dirty page was not eligible for retry");
        }
        if batches.len() != 2
            || !batch_matches(&batches[0], 0, PAGE_BYTES[0])
            || !batch_matches(&batches[1], 0, PAGE_BYTES[0])
        {
            return Err("retry did not preserve backend batch start or payload identity");
        }
        if !clean_after_retry {
            return Err("successful retry did not restore global writeback accounting");
        }
        Ok(())
    }
}
