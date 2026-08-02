use alloc::sync::Arc;
use alloc::vec;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::config::PAGE_SIZE;
use crate::fs::PageCache;
use crate::utils::error::SyscallErr;

use super::write_fixture::BeforeCopyBackend;

const PAGE_INDEX: usize = 1;
const PAGE_BYTE: u8 = 0xa5;

pub(super) fn test_before_copy_reentry_keeps_payload_unpublished() -> Result<(), &'static str> {
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
        .write_with_before_copy(PAGE_INDEX * PAGE_SIZE, &payload, Some(0), move |_| {
            match callback_cache.frame_for_read(PAGE_INDEX) {
                Err(SyscallErr::EAGAIN) => callback_read_blocked.store(true, Ordering::SeqCst),
                Ok(frame) => {
                    callback_read_byte.store(frame.ppn.get_bytes_array()[0], Ordering::SeqCst)
                }
                Err(_) => return Err(SyscallErr::EIO),
            }
            callback_cache.writeback_page(PAGE_INDEX)
        })
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
