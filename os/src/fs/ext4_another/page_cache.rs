use alloc::sync::Arc;
use alloc::vec::Vec;
use core::convert::TryFrom;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use crate::config::PAGE_SIZE;
use crate::fs::page_cache::PageCacheBackend;
use crate::task::perf;
use crate::utils::error::SyscallErr;

use super::errno::{from_another, from_another_op};
use super::fs::Ext4FileSystem;
use super::lifetime::{InodeKey, InodeLifetime};

/// Mango-owned regular-file data backend for one writable ext4 inode.
pub(crate) struct AnotherExt4PageCacheBackend {
    fs: Arc<Ext4FileSystem>,
    key: InodeKey,
    logical_size: Arc<AtomicUsize>,
    writeback_staging: Mutex<Vec<u8>>,
}

impl AnotherExt4PageCacheBackend {
    pub(crate) fn new(
        fs: Arc<Ext4FileSystem>,
        key: InodeKey,
        lifetime: Arc<InodeLifetime>,
    ) -> Self {
        Self {
            fs,
            key,
            logical_size: lifetime.logical_size.clone(),
            writeback_staging: Mutex::new(Vec::new()),
        }
    }

    fn page_offset(index: usize) -> Result<usize, SyscallErr> {
        index.checked_mul(PAGE_SIZE).ok_or(SyscallErr::EFBIG)
    }
}

impl PageCacheBackend for AnotherExt4PageCacheBackend {
    fn read_page(&self, index: usize, buffer: &mut [u8]) -> Result<usize, SyscallErr> {
        crate::task::perf::record_ext4_pc_readpages_calls();
        crate::task::perf::record_ext4_pc_readpages_pages(1);
        if buffer.len() < PAGE_SIZE {
            return Err(SyscallErr::ENOBUFS);
        }
        let offset = Self::page_offset(index)?;
        let size = self.logical_size.load(Ordering::Acquire);
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
            .map_err(|error| from_another_op(&error, "read"))?;
        Ok(PAGE_SIZE)
    }

    fn read_pages(
        &self,
        start_index: usize,
        pages: &mut [&mut [u8]],
    ) -> Result<usize, SyscallErr> {
        crate::task::perf::record_ext4_pc_readpages_calls();
        crate::task::perf::record_ext4_pc_readpages_pages(pages.len());
        for page in pages.iter() {
            if page.len() < PAGE_SIZE {
                return Err(SyscallErr::ENOBUFS);
            }
        }
        if pages.is_empty() {
            return Ok(0);
        }

        let start_offset = Self::page_offset(start_index)?;
        let total_bytes = pages.len().checked_mul(PAGE_SIZE).ok_or(SyscallErr::EFBIG)?;
        let size = self.logical_size.load(Ordering::Acquire);
        let mut staging = alloc::vec![0u8; total_bytes];
        if start_offset < size {
            let read_len = total_bytes.min(size - start_offset);
            self.fs
                .inner()
                .read(
                    u32::try_from(self.key.inode_id()).map_err(|_| SyscallErr::EFBIG)?,
                    start_offset,
                    &mut staging[..read_len],
                )
                .map_err(|error| from_another_op(&error, "read"))?;
        }

        for (index, page) in pages.iter_mut().enumerate() {
            let offset = index * PAGE_SIZE;
            page[..PAGE_SIZE].copy_from_slice(&staging[offset..offset + PAGE_SIZE]);
        }
        Ok(total_bytes)
    }

    fn write_page(&self, index: usize, buffer: &[u8]) -> Result<usize, SyscallErr> {
        self.write_pages(index, &[buffer])
    }

    fn write_pages(&self, start_index: usize, pages: &[&[u8]]) -> Result<usize, SyscallErr> {
        let size = self.logical_size.load(Ordering::Acquire);
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
        crate::task::perf::record_ext4_pc_writepages_calls();
        crate::task::perf::record_ext4_pc_writepages_pages(pages.len());
        // Keep the pool lock short: storage I/O may block, so move the reusable
        // allocation out before preparing or committing the write.
        let mut staging = core::mem::take(&mut *self.writeback_staging.lock());
        staging.clear();
        staging.resize(total_bytes, 0);
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
        let result = (|| -> Result<usize, SyscallErr> {
            // Batch-allocate blocks for the entire write range (like lwext4 delayed alloc)
            let batch_end = start_offset
                .checked_add(total_bytes)
                .ok_or(SyscallErr::EFBIG)?;
            let _t0 = perf::perf_time_now();
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
            let _t1 = perf::perf_time_now();
            perf::record_ext4_alloc_ensure(
                (total_bytes / crate::config::PAGE_SIZE) as usize,
                0,
                _t1.wrapping_sub(_t0),
            );
            if !data_written {
                self.fs
                    .inner()
                    .write_data_only(inode_id, start_offset, &staging[..total_bytes])
                    .map_err(|error| from_another_op(&error, "write_data_only"))?;
            }
            let _t2 = perf::perf_time_now();
            #[cfg(feature = "perf_diag")]
            crate::println!(
                "[ext4_another] write_pages ino={} pages={} total_bytes={} prepare_cycles={} commit_cycles={} direct={}",
                inode_id,
                pages.len(),
                total_bytes,
                _t1.wrapping_sub(_t0),
                _t2.wrapping_sub(_t1),
                data_written,
            );
            Ok(total_bytes)
        })();
        *self.writeback_staging.lock() = staging;
        result
    }

    fn npages(&self) -> usize {
        self.logical_size.load(Ordering::Acquire)
            .div_ceil(PAGE_SIZE)
    }
}
