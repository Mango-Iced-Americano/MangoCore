use alloc::sync::Arc;
use alloc::vec::Vec;
use core::convert::TryFrom;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use crate::config::PAGE_SIZE;
use crate::fs::page_cache::PageCacheBackend;
use crate::task::perf;
use crate::utils::error::SyscallErr;

use super::errno::from_another_op;
use super::fs::Ext4FileSystem;
use super::lifetime::{InodeKey, InodeLifetime};

/// A journal writer briefly owns the metadata mutation domain while it stages
/// or commits a transaction.  Page-cache writeback is allowed to observe that
/// window as `EAGAIN`; it must yield and retry instead of exporting the
/// transient admission failure to a user write.
const BACKEND_EAGAIN_RETRY_LIMIT: usize = 128;

fn yield_backend_retry() {
    if crate::task::current_task().is_some() {
        crate::task::suspend_current_and_run_next();
    } else {
        core::hint::spin_loop();
    }
}

/// Mango-owned regular-file data backend for one writable ext4 inode.
pub(crate) struct AnotherExt4PageCacheBackend {
    fs: Arc<Ext4FileSystem>,
    key: InodeKey,
    lifetime: Arc<InodeLifetime>,
    writeback_staging: Mutex<Vec<u8>>,
}

impl AnotherExt4PageCacheBackend {
    pub(crate) fn new(
        fs: Arc<Ext4FileSystem>,
        key: InodeKey,
        lifetime: Arc<InodeLifetime>,
    ) -> Self {
        lifetime.pin();
        Self {
            fs,
            key,
            lifetime,
            writeback_staging: Mutex::new(Vec::new()),
        }
    }

    fn page_offset(index: usize) -> Result<usize, SyscallErr> {
        index.checked_mul(PAGE_SIZE).ok_or(SyscallErr::EFBIG)
    }

    fn retry_eagain<T, F>(&self, mut operation: F) -> Result<T, SyscallErr>
    where
        F: FnMut() -> Result<T, SyscallErr>,
    {
        for attempt in 0..BACKEND_EAGAIN_RETRY_LIMIT {
            match operation() {
                Err(SyscallErr::EAGAIN) if attempt + 1 < BACKEND_EAGAIN_RETRY_LIMIT => {
                    yield_backend_retry();
                }
                result => return result,
            }
        }
        Err(SyscallErr::EAGAIN)
    }

    /// Prepare and commit one contiguous writeback range.  A large dirty-page
    /// batch may exceed the journal credit/ring reservation even though each
    /// smaller range is valid.  Split only on the reservation-specific E2BIG
    /// error; all other errors remain visible to PageCache for retry/accounting.
    fn write_staged_range(
        &self,
        inode_id: u32,
        start_offset: usize,
        data: &[u8],
    ) -> Result<bool, SyscallErr> {
        let batch_end = start_offset
            .checked_add(data.len())
            .ok_or(SyscallErr::EFBIG)?;
        let prepared = self.retry_eagain(|| {
            self.fs
                .inner()
                .prepare_buffered_write_with_data(
                    inode_id,
                    start_offset,
                    data.len(),
                    batch_end as u64,
                    None,
                    Some(data),
                )
                .map_err(|error| from_another_op(&error, "prepare_buffered_write"))
        });

        match prepared {
            Ok(data_written) => {
                if !data_written {
                    self.retry_eagain(|| {
                        self.fs
                            .inner()
                            .write_data_only(inode_id, start_offset, data)
                            .map_err(|error| from_another_op(&error, "write_data_only"))
                    })?;
                }
                Ok(data_written)
            }
            Err(SyscallErr::E2BIG) if data.len() > PAGE_SIZE => {
                let page_count = data.len() / PAGE_SIZE;
                let split_pages = (page_count / 2).max(1);
                let split = split_pages * PAGE_SIZE;
                let left = self.write_staged_range(inode_id, start_offset, &data[..split])?;
                let right = self.write_staged_range(inode_id, start_offset + split, &data[split..])?;
                Ok(left && right)
            }
            Err(error) => Err(error),
        }
    }
}

impl Drop for AnotherExt4PageCacheBackend {
    fn drop(&mut self) {
        self.lifetime.unpin();
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
            .map_err(|error| from_another_op(&error, "read"))?;
        Ok(PAGE_SIZE)
    }

    fn read_pages(
        &self,
        start_index: usize,
        pages: &mut [&mut [u8]],
    ) -> Result<usize, SyscallErr> {
        if pages.is_empty() {
            return Ok(0);
        }
        if pages.iter().any(|page| page.len() < PAGE_SIZE) {
            return Err(SyscallErr::ENOBUFS);
        }
        crate::task::perf::record_ext4_pc_readpages_calls();
        crate::task::perf::record_ext4_pc_readpages_pages(pages.len());
        let start_offset = Self::page_offset(start_index)?;
        let total_bytes = pages
            .len()
            .checked_mul(PAGE_SIZE)
            .ok_or(SyscallErr::EFBIG)?;
        let size = self.lifetime.logical_size.load(Ordering::Acquire);
        let mut staging = Vec::new();
        staging
            .try_reserve_exact(total_bytes)
            .map_err(|_| SyscallErr::ENOMEM)?;
        staging.resize(total_bytes, 0);
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
        crate::task::perf::record_ext4_pc_writepages_calls();
        crate::task::perf::record_ext4_pc_writepages_pages(pages.len());
        // Reuse staging storage, but do not hold its mutex across backend I/O.
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
        let _t0 = perf::perf_time_now();
        let result = (|| -> Result<usize, SyscallErr> {
            let _data_written = self.write_staged_range(inode_id, start_offset, &staging[..total_bytes])?;
            let _t1 = perf::perf_time_now();
            perf::record_ext4_alloc_ensure(
                (total_bytes / crate::config::PAGE_SIZE) as usize,
                0,
                _t1.wrapping_sub(_t0),
            );
            let _t2 = perf::perf_time_now();
#[cfg(feature = "perf_diag")]
            crate::println!(
                "[ext4_another] write_pages ino={} pages={} total_bytes={} prepare_cycles={} commit_cycles={} direct={}",
                inode_id,
                pages.len(),
                total_bytes,
                _t1.wrapping_sub(_t0),
                _t2.wrapping_sub(_t1),
                _data_written,
            );
            Ok(total_bytes)
        })();
        *self.writeback_staging.lock() = staging;
        result
    }

    fn npages(&self) -> usize {
        self.lifetime
            .logical_size
            .load(Ordering::Acquire)
            .div_ceil(PAGE_SIZE)
    }
}
