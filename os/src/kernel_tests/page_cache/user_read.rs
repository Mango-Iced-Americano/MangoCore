use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::convert::TryFrom;

use crate::config::PAGE_SIZE;
use crate::fs::{PageCache, PageCacheBackend};
use crate::mm::UserBuffer;
use crate::utils::error::SyscallErr;

const PAGE_BASE: u8 = 0x40;
const WRITE_BYTE: u8 = 0xa5;
const VALID_OFFSET: usize = PAGE_SIZE / 8;
const VALID_LEN: usize = PAGE_SIZE / 8;

struct ReadBackend;

impl PageCacheBackend for ReadBackend {
    fn read_page(&self, index: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        let index = u8::try_from(index).map_err(|_| SyscallErr::EIO)?;
        buf.fill(PAGE_BASE.wrapping_add(index));
        Ok(buf.len())
    }

    fn write_page(&self, _index: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
        Ok(buf.len())
    }

    fn npages(&self) -> usize {
        8
    }
}

fn single_segment_user_buffer(len: usize) -> UserBuffer {
    UserBuffer::new(vec![Box::leak(vec![0; len].into_boxed_slice())])
}

fn read_user_buffer(buffer: &UserBuffer, dst: &mut [u8]) -> Result<(), &'static str> {
    if buffer.read(dst) != dst.len() {
        return Err("UserBuffer did not return the expected number of bytes");
    }
    Ok(())
}

fn expected_read_pattern(offset: usize, len: usize) -> Vec<u8> {
    (offset..offset + len)
        .map(|position| PAGE_BASE.wrapping_add((position / PAGE_SIZE) as u8))
        .collect()
}

pub(super) fn test_read_user_single_page_unaligned() -> Result<(), &'static str> {
    let cache = PageCache::new();
    cache.set_backend(Arc::new(ReadBackend));
    let offset = PAGE_SIZE + 37;
    let mut dst = single_segment_user_buffer(97);

    let copied = cache
        .read_user(offset, dst.len(), &mut dst)
        .map_err(|_| "single-page read_user failed")?;
    if copied != dst.len() {
        return Err("single-page read_user returned a short copy");
    }

    let mut actual = vec![0; copied];
    read_user_buffer(&dst, &mut actual)?;
    if actual != vec![PAGE_BASE.wrapping_add(1); copied] {
        return Err("single-page read_user copied unexpected bytes");
    }
    Ok(())
}

pub(super) fn test_read_user_multi_page_multi_segment() -> Result<(), &'static str> {
    let cache = PageCache::new();
    cache.set_backend(Arc::new(ReadBackend));
    let offset = PAGE_SIZE - 32;
    let mut dst = UserBuffer::new(vec![
        Box::leak(vec![0; 17].into_boxed_slice()),
        Box::leak(vec![0; 31].into_boxed_slice()),
        Box::leak(vec![0; 48].into_boxed_slice()),
    ]);

    let copied = cache
        .read_user(offset, dst.len(), &mut dst)
        .map_err(|_| "multi-page read_user failed")?;
    if copied != dst.len() {
        return Err("multi-page read_user returned a short copy");
    }

    let mut expected = vec![PAGE_BASE; 32];
    expected.extend_from_slice(&vec![PAGE_BASE.wrapping_add(1); 64]);
    let mut actual = vec![0; copied];
    read_user_buffer(&dst, &mut actual)?;
    if actual != expected {
        return Err("multi-page read_user did not preserve UserBuffer segment order");
    }
    Ok(())
}

pub(super) fn test_read_user_multi_page_unaligned_segments() -> Result<(), &'static str> {
    let mut cursor_dst = UserBuffer::new(vec![
        Box::leak(vec![0; 3].into_boxed_slice()),
        Box::leak(vec![0; 0].into_boxed_slice()),
        Box::leak(vec![0; 5].into_boxed_slice()),
        Box::leak(vec![0; 2].into_boxed_slice()),
    ]);
    let mut cursor = cursor_dst.write_cursor();
    if cursor.write_from(&[0, 1, 2, 3]) != 4 {
        return Err("UserBuffer cursor returned a short first chunk");
    }
    if cursor.write_from(&[4, 5, 6, 7, 8, 9, 10]) != 6 {
        return Err("UserBuffer cursor did not stop at the short destination");
    }
    let mut cursor_actual = [0; 10];
    read_user_buffer(&cursor_dst, &mut cursor_actual)?;
    if cursor_actual != [0, 1, 2, 3, 4, 5, 6, 7, 8, 9] {
        return Err("UserBuffer cursor did not preserve segment order");
    }

    let cache = PageCache::new();
    cache.set_backend(Arc::new(ReadBackend));
    let offset = PAGE_SIZE - 23;
    let len = PAGE_SIZE * 2 + 113;
    let mut dst = UserBuffer::new(vec![
        Box::leak(vec![0; 13].into_boxed_slice()),
        Box::leak(vec![0; PAGE_SIZE - 17].into_boxed_slice()),
        Box::leak(vec![0; 29].into_boxed_slice()),
        Box::leak(vec![0; PAGE_SIZE - 31].into_boxed_slice()),
        Box::leak(vec![0; 119].into_boxed_slice()),
    ]);

    let copied = cache
        .read_user(offset, len, &mut dst)
        .map_err(|_| "unaligned multi-page read_user failed")?;
    if copied != len {
        return Err("unaligned multi-page read_user returned a short copy");
    }

    let mut actual = vec![0; copied];
    read_user_buffer(&dst, &mut actual)?;
    if actual != expected_read_pattern(offset, len) {
        return Err(
            "unaligned multi-page read_user crossed a source or segment boundary incorrectly",
        );
    }
    Ok(())
}

pub(super) fn test_read_user_rejects_short_destination() -> Result<(), &'static str> {
    let cache = PageCache::new();
    cache.set_backend(Arc::new(ReadBackend));
    let mut dst = single_segment_user_buffer(1);

    match cache.read_user(0, dst.len() + 1, &mut dst) {
        Err(SyscallErr::EFAULT) => {}
        Err(_) => return Err("short UserBuffer destination returned the wrong error"),
        Ok(_) => return Err("short UserBuffer destination unexpectedly succeeded"),
    }

    let mut actual = [0u8; 1];
    read_user_buffer(&dst, &mut actual)?;
    if actual != [0] {
        return Err("short UserBuffer destination was partially modified");
    }
    Ok(())
}

pub(super) fn test_read_user_fills_partial_valid_page() -> Result<(), &'static str> {
    let cache = PageCache::new();
    cache.set_backend(Arc::new(ReadBackend));
    let page_index = 2;
    let page_offset = page_index * PAGE_SIZE;
    cache
        .write(
            page_offset + VALID_OFFSET,
            &vec![WRITE_BYTE; VALID_LEN],
            Some(page_offset + PAGE_SIZE),
        )
        .map_err(|_| "partial-valid PageCache setup write failed")?;
    let mut dst = single_segment_user_buffer(PAGE_SIZE);

    let copied = cache
        .read_user(page_offset, dst.len(), &mut dst)
        .map_err(|_| "partial-valid read_user failed")?;
    if copied != PAGE_SIZE {
        return Err("partial-valid read_user returned a short copy");
    }

    let mut actual = vec![0; copied];
    read_user_buffer(&dst, &mut actual)?;
    let page_index = u8::try_from(page_index).map_err(|_| "partial-valid page index is invalid")?;
    let expected_page_byte = PAGE_BASE.wrapping_add(page_index);
    if actual[..VALID_OFFSET] != vec![expected_page_byte; VALID_OFFSET]
        || actual[VALID_OFFSET..VALID_OFFSET + VALID_LEN] != vec![WRITE_BYTE; VALID_LEN]
        || actual[VALID_OFFSET + VALID_LEN..]
            != vec![expected_page_byte; PAGE_SIZE - VALID_OFFSET - VALID_LEN]
    {
        return Err("partial-valid read_user did not merge backend and dirty bytes");
    }
    Ok(())
}

pub(super) fn test_read_user_returns_eagain_during_loading_reentry() -> Result<(), &'static str> {
    let cache = PageCache::new();
    cache.set_backend(Arc::new(ReadBackend));
    let mut nested_dst = single_segment_user_buffer(1);

    let written = cache
        .write_with_before_copy(0, &vec![WRITE_BYTE; PAGE_SIZE], Some(0), |_| {
            match cache.read_user(0, nested_dst.len(), &mut nested_dst) {
                Err(SyscallErr::EAGAIN) => Ok(()),
                _ => Err(SyscallErr::EIO),
            }
        })
        .map_err(|_| "read_user did not return EAGAIN during loading re-entry")?;
    if written != PAGE_SIZE {
        return Err("read_user did not report EAGAIN to the loading re-entry");
    }
    Ok(())
}
