use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

use crate::fs::{PageCache, PageCacheBackend};
use crate::utils::error::SyscallErr;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum WritebackCall {
    Pages { start_index: usize, page_count: usize },
    Page { index: usize },
}

struct ObservedBackend {
    inner: Arc<dyn PageCacheBackend>,
    calls: Arc<Mutex<Vec<WritebackCall>>>,
}

impl PageCacheBackend for ObservedBackend {
    fn read_page(&self, index: usize, buffer: &mut [u8]) -> Result<usize, SyscallErr> {
        self.inner.read_page(index, buffer)
    }

    fn write_page(&self, index: usize, buffer: &[u8]) -> Result<usize, SyscallErr> {
        self.calls.lock().push(WritebackCall::Page { index });
        self.inner.write_page(index, buffer)
    }

    fn write_pages(&self, start_index: usize, pages: &[&[u8]]) -> Result<usize, SyscallErr> {
        self.calls.lock().push(WritebackCall::Pages {
            start_index,
            page_count: pages.len(),
        });
        self.inner.write_pages(start_index, pages)
    }

    fn npages(&self) -> usize {
        self.inner.npages()
    }
}

pub(super) struct PageCacheBackendSwapGuard<'cache> {
    cache: &'cache PageCache,
    original: Arc<dyn PageCacheBackend>,
    calls: Arc<Mutex<Vec<WritebackCall>>>,
}

impl<'cache> PageCacheBackendSwapGuard<'cache> {
    pub(super) fn install(cache: &'cache PageCache) -> Result<Self, &'static str> {
        let original = cache.backend().ok_or("page-cache backend missing")?;
        let calls = Arc::new(Mutex::new(Vec::new()));
        cache.set_backend(Arc::new(ObservedBackend {
            inner: original.clone(),
            calls: calls.clone(),
        }));
        Ok(Self {
            cache,
            original,
            calls,
        })
    }

    pub(super) fn snapshot_calls(&self) -> Vec<WritebackCall> {
        self.calls.lock().clone()
    }
}

impl Drop for PageCacheBackendSwapGuard<'_> {
    fn drop(&mut self) {
        self.cache.set_backend(self.original.clone());
    }
}
