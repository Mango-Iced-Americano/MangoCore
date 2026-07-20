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
        for (index, page) in pages.iter().enumerate() {
            if page.len() < PAGE_SIZE {
                return Err(SyscallErr::ENOBUFS);
            }
            let page_index = start_index.checked_add(index).ok_or(SyscallErr::EFBIG)?;
            let offset = Self::page_offset(page_index)?;
            if offset >= size {
                continue;
            }
            let write_len = PAGE_SIZE.min(size - offset);
            self.fs
                .inner()
                .write_data_only(inode_id, offset, &page[..write_len])
                .map_err(|error| from_another(error.code()))?;
        }
        self.fs
            .inner()
            .commit_inode_size(inode_id, size as u64, None)
            .map_err(|error| from_another(error.code()))?;
        Ok(pages.len() * PAGE_SIZE)
    }

    fn npages(&self) -> usize {
        self.lifetime
            .logical_size
            .load(Ordering::Acquire)
            .div_ceil(PAGE_SIZE)
    }
}
