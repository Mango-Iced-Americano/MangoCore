use alloc::sync::Arc;
use core::convert::TryFrom;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::config::PAGE_SIZE;
use crate::fs::page_cache::PageCacheBackend;
use crate::utils::error::SyscallErr;

use super::errno::from_another;
use super::fs::Ext4FileSystem;
use super::lifetime::{InodeKey, InodeLifetime};

/// Mango-owned regular-file data backend for one writable ext4 inode.
pub(crate) struct AnotherExt4PageCacheBackend {
    fs: Arc<Ext4FileSystem>,
    key: InodeKey,
    lifetime: Arc<InodeLifetime>,
}

impl AnotherExt4PageCacheBackend {
    pub(crate) fn new(
        fs: Arc<Ext4FileSystem>,
        key: InodeKey,
        lifetime: Arc<InodeLifetime>,
    ) -> Self {
        lifetime.pin();
        Self { fs, key, lifetime }
    }

    fn page_offset(index: usize) -> Result<usize, SyscallErr> {
        index.checked_mul(PAGE_SIZE).ok_or(SyscallErr::EFBIG)
    }
}

impl Drop for AnotherExt4PageCacheBackend {
    fn drop(&mut self) {
        self.lifetime.unpin();
    }
}

impl PageCacheBackend for AnotherExt4PageCacheBackend {
    fn read_page(&self, index: usize, buffer: &mut [u8]) -> Result<usize, SyscallErr> {
        if buffer.len() < PAGE_SIZE {
            return Err(SyscallErr::ENOBUFS);
        }
        let offset = Self::page_offset(index)?;
        let size = self.lifetime.logical_size.load(Ordering::Acquire);
        buffer[..PAGE_SIZE].fill(0);
        if offset >= size {
            return Ok(PAGE_SIZE);
        }
        let read_len = PAGE_SIZE.min(size - offset);
        self.fs
            .inner()
            .read(
                u32::try_from(self.key.inode_id()).map_err(|_| SyscallErr::EFBIG)?,
                offset,
                &mut buffer[..read_len],
            )
            .map_err(|error| from_another(error.code()))?;
        Ok(PAGE_SIZE)
    }

    fn write_page(&self, index: usize, buffer: &[u8]) -> Result<usize, SyscallErr> {
        self.write_pages(index, &[buffer])
    }

    fn write_pages(&self, start_index: usize, pages: &[&[u8]]) -> Result<usize, SyscallErr> {
        let size = self.lifetime.logical_size.load(Ordering::Acquire);
        let inode_id = u32::try_from(self.key.inode_id()).map_err(|_| SyscallErr::EFBIG)?;
        let start_offset = Self::page_offset(start_index)?;
        if start_offset >= size {
            return Ok(0);
        }
        // Compute total bytes from all pages, clamped to logical EOF
        let raw_total: usize = pages.iter().map(|p| PAGE_SIZE.min(p.len())).sum();
        let total_bytes = raw_total.min(size.saturating_sub(start_offset));
        if total_bytes == 0 {
            return Ok(0);
        }
        // Build staging buffer for a single write_data_only call
        let mut staging = alloc::vec![0u8; total_bytes];
        let mut copied = 0;
        for page in pages.iter() {
            let page_bytes = PAGE_SIZE.min(page.len());
            let remaining = total_bytes.saturating_sub(copied);
            if remaining == 0 {
                break;
            }
            let n = page_bytes.min(remaining);
            staging[copied..copied + n].copy_from_slice(&page[..n]);
            copied += n;
        }
        // Batch-allocate blocks for the entire write range (like lwext4 delayed alloc)
        let batch_end = start_offset
            .checked_add(total_bytes)
            .ok_or(SyscallErr::EFBIG)?;
        let data_written = self
            .fs
            .inner()
            .prepare_buffered_write_with_data(
                inode_id,
                start_offset,
                total_bytes,
                batch_end as u64,
                None,
                Some(&staging[..total_bytes]),
            )
            .map_err(|error| from_another(error.code()))?;
        if !data_written {
            self.fs
                .inner()
                .write_data_only(inode_id, start_offset, &staging[..total_bytes])
                .map_err(|error| from_another(error.code()))?;
        }
        Ok(total_bytes)
    }

    fn npages(&self) -> usize {
        self.lifetime
            .logical_size
            .load(Ordering::Acquire)
            .div_ceil(PAGE_SIZE)
    }
}
