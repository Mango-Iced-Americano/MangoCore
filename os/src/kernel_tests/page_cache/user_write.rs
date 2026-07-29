use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec;

use crate::config::PAGE_SIZE;
use crate::fs::PageCache;
use crate::mm::UserBuffer;
use crate::utils::error::SyscallErr;

use super::write_fixture::BeforeCopyBackend;

const PAGE_INDEX: usize = 1;
const PAGE_BYTE: u8 = 0xa5;
const READ_LEN: usize = 64;

pub(super) fn test_write_user_rejects_short_source_without_mutation() -> Result<(), &'static str> {
    let cache = PageCache::new();
    let backend = Arc::new(BeforeCopyBackend::new());
    cache.set_backend(backend.clone());
    cache
        .write(PAGE_INDEX * PAGE_SIZE, &vec![PAGE_BYTE; PAGE_SIZE], Some(0))
        .map_err(|_| "PageCache setup write failed")?;

    let state_before = cache.state_of(PAGE_INDEX);
    let dirty_count_before = cache.dirty_count();
    if state_before.is_none() || !cache.is_dirty(PAGE_INDEX) {
        return Err("PageCache setup did not create a dirty cached page");
    }

    let source = UserBuffer::new(vec![Box::leak(vec![0; 1].into_boxed_slice())]);
    match cache.write_user(PAGE_INDEX * PAGE_SIZE, source.len() + 1, &source, Some(0)) {
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
