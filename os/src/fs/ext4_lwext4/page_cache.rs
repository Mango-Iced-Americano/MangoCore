//! PageCache backend for lwext4 VFS adapter.
//!
//! Unlike the legacy `Ext4PageCacheBackend` (which does extent tree lookups
//! and block-level I/O), this backend delegates to lwext4's file API. It does
//! NOT need to know about ext4 on-disk structures — lwext4 handles block
//! mapping internally.
//!
//! Slow but correct — each page I/O does fopen/fseek/fread/fwrite/fclose.
//! For batching, `read_pages`/`write_pages` open once and do sequential I/O.

use alloc::sync::{Arc, Weak};
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::config::PAGE_SIZE;
use crate::fs::page_cache::PageCacheBackend;
use crate::utils::error::SyscallErr;

use super::errno::from_lwext4;
use super::inode_state::Ext4InodeState;

/// Sentinel: size has not been fetched from lwext4 yet.
/// usize::MAX is chosen because no real file can be > 2^64 bytes,
/// and on a 64-bit system usize::MAX = u64::MAX.
pub(crate) const LWEXT4_SIZE_UNKNOWN: usize = usize::MAX;

/// PageCache backend that reads/writes pages through lwext4 file API.
///
/// Holds a `Weak` reference to the owning filesystem (for lwext4 C call
/// serialization) and a shared logical file size (updated by write operations,
/// lazily probed from lwext4 on first read).
pub struct LwExt4PageCacheBackend {
    /// Weak reference to the owning filesystem (for lwext4 C call serialization)
    fs: Weak<super::ext4fs::Ext4FileSystem>,
    /// Shared inode identity/path/open-handle state.
    state: Arc<Ext4InodeState>,
    /// Shared logical file size — writeback clamps to this to prevent
    /// 1-byte writes from producing 4KB files. Lazily refreshed on first I/O.
    logical_size: Arc<AtomicUsize>,
}

impl LwExt4PageCacheBackend {
    /// Create a new backend for the given file path.
    ///
    /// `logical_size` is shared with the parent `Ext4OSInode`. It starts as
    /// `LWEXT4_SIZE_UNKNOWN` and is lazily refreshed on first I/O.
    pub fn new(fs: Weak<super::ext4fs::Ext4FileSystem>, state: Arc<Ext4InodeState>) -> Self {
        let logical_size = state.logical_size();
        Self {
            fs,
            state,
            logical_size,
        }
    }

    /// Atomic fetch_max update for `logical_size`, treating UNKNOWN as 0.
    fn note_logical_size_atomic(logical_size: &AtomicUsize, new_size: usize) {
        let mut prev = logical_size.load(Ordering::Relaxed);
        if prev == LWEXT4_SIZE_UNKNOWN {
            prev = 0;
        }
        while new_size > prev {
            match logical_size.compare_exchange_weak(
                prev,
                new_size,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => prev = actual,
            }
        }
    }
}

impl PageCacheBackend for LwExt4PageCacheBackend {
    fn read_page(&self, index: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        crate::task::perf::record_pc_miss();
        if buf.len() < PAGE_SIZE {
            return Err(SyscallErr::ENOBUFS);
        }
        let fs = self.fs.upgrade().ok_or(SyscallErr::EIO)?;
        let offset = index * PAGE_SIZE;
        self.state.with_file(&fs, false, |f| {
            // Snapshot physical EOF from lwext4 — NOT the shared logical_size
            // which may reflect VFS-level extensions not yet materialized.
            let physical_eof = f.file_size() as usize;
            if offset >= physical_eof {
                // Page wholly beyond physical EOF — pure hole, zero-fill
                buf[..PAGE_SIZE].fill(0);
                return Ok(PAGE_SIZE);
            }
            let physical_len = PAGE_SIZE.min(physical_eof - offset);
            f.file_seek(offset as i64, 0)
                .map_err(|error| from_lwext4(error.abs()))?;
            let n = f
                .file_read(&mut buf[..physical_len])
                .map_err(|error| from_lwext4(error.abs()))?;
            // Zero-fill the remainder of the page (tail of last partial page)
            if n < PAGE_SIZE {
                buf[n..PAGE_SIZE].fill(0);
            }
            Ok(PAGE_SIZE)
        })
    }

    fn write_page(&self, index: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
        if buf.len() < PAGE_SIZE {
            return Err(SyscallErr::ENOBUFS);
        }
        let fs = self.fs.upgrade().ok_or(SyscallErr::EIO)?;
        let offset = index.checked_mul(PAGE_SIZE).ok_or(SyscallErr::EFBIG)?;
        let raw_len = PAGE_SIZE.min(buf.len());
        let eof = self.logical_size.load(Ordering::Relaxed);
        let write_len = if eof == LWEXT4_SIZE_UNKNOWN {
            raw_len
        } else {
            raw_len.min(eof.saturating_sub(offset))
        };
        if write_len == 0 {
            // A dirty page outside the published EOF is a caller-ordering
            // violation.  Do not report success and silently discard it.
            return Err(SyscallErr::EIO);
        }
        self.state.with_file(&fs, true, |f| {
            if offset > f.file_size() as usize {
                f.file_truncate(offset as u64)
                    .map_err(|error| from_lwext4(error.abs()))?;
            }
            f.file_seek(offset as i64, 0)
                .map_err(|error| from_lwext4(error.abs()))?;
            let n = f
                .file_write(&buf[..write_len])
                .map_err(|error| from_lwext4(error.abs()))?;
            if n != write_len {
                return Err(SyscallErr::EIO);
            }
            // The wrapper keeps lwext4's block cache in write-back mode for
            // the lifetime of the mount.  Partial data blocks therefore stay
            // in that lower cache after ext4_fwrite() returns.  A successful
            // PageCacheBackend write must nevertheless be visible after the
            // upper PageCache evicts this page, because ext4_fread() performs
            // direct block reads.  Flush before allowing PageCache to mark
            // the page clean.
            f.file_cache_flush()
                .map_err(|error| from_lwext4(error.abs()))?;
            // fetch_max: update logical_size if we extended the file
            Self::note_logical_size_atomic(&self.logical_size, offset + n);
            Ok(n)
        })
    }

    fn read_pages(&self, start_index: usize, pages: &mut [&mut [u8]]) -> Result<usize, SyscallErr> {
        crate::task::perf::record_pc_miss();
        let fs = self.fs.upgrade().ok_or(SyscallErr::EIO)?;
        self.state.with_file(&fs, false, |f| {
            // Snapshot physical EOF from lwext4 once — avoid seeking/reading
            // past it for pages that map to holes beyond actual inode size.
            let physical_eof = f.file_size() as usize;
            for (i, page) in pages.iter_mut().enumerate() {
                let page_offset = (start_index + i) * PAGE_SIZE;
                if page_offset >= physical_eof {
                    // Page wholly beyond physical EOF — zero-fill
                    let page_len = page.len();
                    page[..PAGE_SIZE.min(page_len)].fill(0);
                } else {
                    let physical_len = PAGE_SIZE.min(physical_eof - page_offset);
                    f.file_seek(page_offset as i64, 0)
                        .map_err(|error| from_lwext4(error.abs()))?;
                    let n = f
                        .file_read(&mut page[..physical_len])
                        .map_err(|error| from_lwext4(error.abs()))?;
                    if n < PAGE_SIZE {
                        let page_len = page.len();
                        page[n..PAGE_SIZE.min(page_len)].fill(0);
                    }
                }
            }
            Ok(pages.len() * PAGE_SIZE)
        })
    }

    fn write_pages(&self, start_index: usize, pages: &[&[u8]]) -> Result<usize, SyscallErr> {
        let fs = self.fs.upgrade().ok_or(SyscallErr::EIO)?;
        let start_offset = start_index * PAGE_SIZE;
        self.state.with_file(&fs, true, |f| {
            // Compute total raw bytes from all pages
            let raw_total: usize = pages.iter().map(|p| PAGE_SIZE.min(p.len())).sum();
            // Clamp to logical EOF to avoid writing a full page for a partial last page
            let eof = self.logical_size.load(Ordering::Relaxed);
            let total_bytes = if eof == LWEXT4_SIZE_UNKNOWN {
                raw_total
            } else {
                raw_total.min(eof.saturating_sub(start_offset))
            };
            if total_bytes == 0 {
                if raw_total > 0 {
                    // Safety net: dirty pages exist but EOF clamps them to 0.
                    // This should not happen after the write-ordering fix
                    // (note_logical_size is now called before pc.write()).
                    // If triggered, it indicates a different caller forgot to
                    // update logical_size before triggering writeback.
                    log::warn!(
                        "[lwext4-wb] skipping dirty writeback for {}: \
                         EOF={} but {} dirty bytes at offset {} ({} pages)",
                        self.state.inode_id(),
                        eof,
                        raw_total,
                        start_offset,
                        pages.len()
                    );
                    return Err(SyscallErr::EIO);
                }
                return Ok(0);
            }

            // Build staging buffer: concatenate all pages for a single file_write.
            // This avoids per-page ext4_fwrite → ext4_trans_start/stop overhead:
            // 4MB file drops from ~1024 journal transactions to ~4.
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

            if start_offset > f.file_size() as usize {
                f.file_truncate(start_offset as u64)
                    .map_err(|error| from_lwext4(error.abs()))?;
            }
            f.file_seek(start_offset as i64, 0)
                .map_err(|error| from_lwext4(error.abs()))?;
            let n = f
                .file_write(&staging[..total_bytes])
                .map_err(|error| from_lwext4(error.abs()))?;
            if n != total_bytes {
                return Err(SyscallErr::EIO);
            }

            // See write_page(): this is one flush per contiguous writeback
            // batch, not one flush per page.  Returning success before this
            // point would let an upper-cache eviction expose stale disk data.
            f.file_cache_flush()
                .map_err(|error| from_lwext4(error.abs()))?;

            // Single logical_size update for entire batch
            Self::note_logical_size_atomic(&self.logical_size, start_offset + n);
            Ok(n)
        })
    }

    fn npages(&self) -> usize {
        // Refresh size from lwext4 on first call
        if self.logical_size.load(Ordering::Relaxed) == LWEXT4_SIZE_UNKNOWN {
            if let Some(fs) = self.fs.upgrade() {
                if let Ok(s) =
                    self.state
                        .with_file(&fs, false, |file| Ok(file.file_size() as usize))
                {
                    self.logical_size.store(s, Ordering::Relaxed);
                }
            }
        }
        let size = self.logical_size.load(Ordering::Relaxed);
        if size == LWEXT4_SIZE_UNKNOWN {
            return 0;
        }
        (size + PAGE_SIZE - 1) / PAGE_SIZE
    }
}
