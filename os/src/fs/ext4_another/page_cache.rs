use alloc::sync::{Arc, Weak};
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

/// Mango-owned regular-file data backend for one writable ext4 inode.
pub(crate) struct AnotherExt4PageCacheBackend {
    /// 后端不得反向持有文件系统；dirty lifetime 已持有缓存，强引用会形成
    /// `fs -> lifetime -> cache -> backend -> fs` 环并使独立挂载在收尾时存活。
    fs: Weak<Ext4FileSystem>,
    key: InodeKey,
    /// 写回只需要逻辑文件大小。保留该原子值而非 `InodeLifetime` 本身，既让
    /// dirty cache 能在临时 VFS inode 销毁后完成 sync，也不形成
    /// `lifetime -> dirty cache -> backend -> lifetime` 引用环。
    logical_size: Arc<AtomicUsize>,
    pending_write_end: Arc<AtomicUsize>,
    lifetime: Weak<InodeLifetime>,
    cache: Weak<crate::fs::page_cache::PageCache>,
    writeback_staging: Mutex<Vec<u8>>,
}

impl AnotherExt4PageCacheBackend {
    pub(crate) fn new(
        fs: Arc<Ext4FileSystem>,
        key: InodeKey,
        lifetime: Arc<InodeLifetime>,
        cache: Weak<crate::fs::page_cache::PageCache>,
    ) -> Self {
        Self {
            fs: Arc::downgrade(&fs),
            key,
            logical_size: lifetime.logical_size.clone(),
            pending_write_end: lifetime.pending_write_end.clone(),
            lifetime: Arc::downgrade(&lifetime),
            cache,
            writeback_staging: Mutex::new(Vec::new()),
        }
    }

    fn fs(&self) -> Result<Arc<Ext4FileSystem>, SyscallErr> {
        self.fs.upgrade().ok_or(SyscallErr::EIO)
    }

    fn page_offset(index: usize) -> Result<usize, SyscallErr> {
        index.checked_mul(PAGE_SIZE).ok_or(SyscallErr::EFBIG)
    }

    fn visible_size(&self) -> usize {
        self.logical_size
            .load(Ordering::Acquire)
            .max(self.pending_write_end.load(Ordering::Acquire))
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
        retry_budget: u8,
    ) -> Result<bool, SyscallErr> {
        let fs = self.fs()?;
        let batch_end = start_offset
            .checked_add(data.len())
            .ok_or(SyscallErr::EFBIG)?;
        let prepared = fs.run_metadata_operation(|| {
            fs.inner().prepare_buffered_write_with_data(
                inode_id,
                start_offset,
                data.len(),
                batch_end as u64,
                None,
                Some(data),
            )
        });

        match prepared {
            Ok(data_written) => {
                if !data_written {
                    fs.run_metadata_operation(|| {
                        fs.inner().write_data_only(inode_id, start_offset, data)
                    })?;
                }
                Ok(data_written)
            }
            Err(SyscallErr::E2BIG) if retry_budget != 0 => {
                // A full deferred-journal ring also reports E2BIG. Splitting
                // cannot make progress in that case, so drain once and retry
                // the original range before reducing its transaction size.
                fs.run_metadata_operation(|| fs.inner().flush_deferred_journal())?;
                self.write_staged_range(inode_id, start_offset, data, retry_budget - 1)
            }
            Err(SyscallErr::E2BIG) if data.len() > PAGE_SIZE => {
                let page_count = data.len() / PAGE_SIZE;
                let split_pages = (page_count / 2).max(1);
                let split = split_pages * PAGE_SIZE;
                let left =
                    self.write_staged_range(inode_id, start_offset, &data[..split], retry_budget)?;
                let right = self.write_staged_range(
                    inode_id,
                    start_offset + split,
                    &data[split..],
                    retry_budget,
                )?;
                Ok(left && right)
            }
            Err(error) => Err(error),
        }
    }
}

impl PageCacheBackend for AnotherExt4PageCacheBackend {
    fn on_page_dirty(&self) {
        if let (Some(lifetime), Some(cache)) = (self.lifetime.upgrade(), self.cache.upgrade()) {
            lifetime.retain_dirty_page_cache(&cache);
        }
    }

    fn read_page(&self, index: usize, buffer: &mut [u8]) -> Result<usize, SyscallErr> {
        crate::task::perf::record_ext4_pc_readpages_calls();
        crate::task::perf::record_ext4_pc_readpages_pages(1);
        if buffer.len() < PAGE_SIZE {
            return Err(SyscallErr::ENOBUFS);
        }
        let offset = Self::page_offset(index)?;
        let size = self.visible_size();
        let fs = self.fs()?;
        buffer[..PAGE_SIZE].fill(0);
        if offset >= size {
            return Ok(PAGE_SIZE);
        }
        let read_len = PAGE_SIZE.min(size - offset);
        fs
            .inner()
            .read(
                u32::try_from(self.key.inode_id()).map_err(|_| SyscallErr::EFBIG)?,
                offset,
                &mut buffer[..read_len],
            )
            .map_err(|error| from_another_op(&error, "read"))?;
        Ok(PAGE_SIZE)
    }

    fn read_pages(&self, start_index: usize, pages: &mut [&mut [u8]]) -> Result<usize, SyscallErr> {
        if pages.is_empty() {
            return Ok(0);
        }
        if pages.len() > crate::fs::page_cache::MAX_BATCH_READ_PAGES {
            return Err(SyscallErr::E2BIG);
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
        let size = self.visible_size();
        let fs = self.fs()?;
        let mut staging = Vec::new();
        staging
            .try_reserve_exact(total_bytes)
            .map_err(|_| SyscallErr::ENOMEM)?;
        staging.resize(total_bytes, 0);
        if start_offset < size {
            let read_len = total_bytes.min(size - start_offset);
            fs
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

    fn read_contiguous(
        &self,
        start_index: usize,
        buffer: &mut [u8],
    ) -> Result<usize, SyscallErr> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if buffer.len() % PAGE_SIZE != 0 {
            return Err(SyscallErr::ENOBUFS);
        }
        let pages = buffer.len() / PAGE_SIZE;
        if pages > crate::fs::page_cache::MAX_DEMAND_READ_PAGES {
            return Err(SyscallErr::E2BIG);
        }
        crate::task::perf::record_ext4_pc_readpages_calls();
        crate::task::perf::record_ext4_pc_readpages_pages(pages);

        let start_offset = Self::page_offset(start_index)?;
        let size = self.visible_size();
        buffer.fill(0);
        if start_offset >= size {
            return Ok(buffer.len());
        }
        let read_len = buffer.len().min(size - start_offset);
        let fs = self.fs()?;
        fs
            .inner()
            .read(
                u32::try_from(self.key.inode_id()).map_err(|_| SyscallErr::EFBIG)?,
                start_offset,
                &mut buffer[..read_len],
            )
            .map_err(|error| from_another_op(&error, "read"))?;
        Ok(buffer.len())
    }

    fn write_page(&self, index: usize, buffer: &[u8]) -> Result<usize, SyscallErr> {
        self.write_pages(index, &[buffer])
    }

    fn write_pages(&self, start_index: usize, pages: &[&[u8]]) -> Result<usize, SyscallErr> {
        let size = self.visible_size();
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
        let _t0 = perf::perf_time_now();
        let result = (|| -> Result<usize, SyscallErr> {
            let _data_written =
                self.write_staged_range(inode_id, start_offset, &staging[..total_bytes], 1)?;
            let _t1 = perf::perf_time_now();
            perf::record_ext4_alloc_ensure(
                (total_bytes / crate::config::PAGE_SIZE) as usize,
                0,
                _t1.wrapping_sub(_t0),
            );
            Ok(total_bytes)
        })();
        // BuildStorm creates many short-lived inodes. Retaining each batch's
        // peak allocation in its backend turns staging reuse into unbounded
        // kernel-heap retention, so release it after this writeback.
        drop(staging);
        result
    }

    fn npages(&self) -> usize {
        self.visible_size().div_ceil(PAGE_SIZE)
    }
}
