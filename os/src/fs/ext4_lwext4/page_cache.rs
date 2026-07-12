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
use alloc::string::String;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::config::PAGE_SIZE;
use crate::fs::page_cache::PageCacheBackend;
use crate::utils::error::SyscallErr;

use lwext4_rust::{Ext4File, InodeTypes};

use super::errno::from_lwext4;

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
    /// Full path from mount root, e.g. "/bin/busybox"
    path: String,
    /// lwext4-internal path with mount point prefix, e.g. "/e1/bin/busybox".
    /// Pre-computed at construction time from `fs.lw_path(&path)`.
    lw_path: String,
    /// Shared logical file size — writeback clamps to this to prevent
    /// 1-byte writes from producing 4KB files. Lazily refreshed on first I/O.
    logical_size: Arc<AtomicUsize>,
}

impl LwExt4PageCacheBackend {
    /// Create a new backend for the given file path.
    ///
    /// `logical_size` is shared with the parent `Ext4OSInode`. It starts as
    /// `LWEXT4_SIZE_UNKNOWN` and is lazily refreshed on first I/O.
    pub fn new(
        fs: Weak<super::ext4fs::Ext4FileSystem>,
        path: String,
        logical_size: Arc<AtomicUsize>,
        lw_path: String,
    ) -> Self {
        Self {
            fs,
            path,
            lw_path,
            logical_size,
        }
    }

    /// Ensure `logical_size` is known, probing from lwext4 on first call.
    fn ensure_size_known(&self, fs: &Arc<super::ext4fs::Ext4FileSystem>) -> Result<usize, SyscallErr> {
        let cached = self.logical_size.load(Ordering::Relaxed);
        if cached != LWEXT4_SIZE_UNKNOWN {
            return Ok(cached);
        }
        let _lock = fs.lw.lock();
        let mut f = Ext4File::new(&self.lw_path, InodeTypes::EXT4_DE_REG_FILE);
        let size = if f.file_open(&self.lw_path, 0x0).is_ok() {
            let s = f.file_size() as usize;
            f.file_close().ok();
            s
        } else {
            0
        };
        self.logical_size.store(size, Ordering::Relaxed);
        Ok(size)
    }

    /// Atomic fetch_max update for `logical_size`, treating UNKNOWN as 0.
    fn note_logical_size_atomic(logical_size: &AtomicUsize, new_size: usize) {
        let mut prev = logical_size.load(Ordering::Relaxed);
        if prev == LWEXT4_SIZE_UNKNOWN {
            prev = 0;
        }
        while new_size > prev {
            match logical_size.compare_exchange_weak(
                prev, new_size, Ordering::Relaxed, Ordering::Relaxed,
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

        // Lazy refresh: probe file size from lwext4 on first call
        let file_size = self.ensure_size_known(&fs).unwrap_or(0);
        let read_len = PAGE_SIZE.min(file_size.saturating_sub(offset));
        if read_len == 0 {
            // Past EOF — fill with zeros
            buf[..PAGE_SIZE].fill(0);
            return Ok(PAGE_SIZE);
        }
        let _lock = fs.lw.lock();
        let mut f = Ext4File::new(&self.lw_path, InodeTypes::EXT4_DE_REG_FILE);
        f.file_open(&self.lw_path, 0x0)
            .map_err(|e| from_lwext4(e.abs()))?;
        // Use closure to ensure file_close() on all error paths
        let result = (|| -> Result<usize, SyscallErr> {
            f.file_seek(offset as i64, 0)
                .map_err(|e| from_lwext4(e.abs()))?;
            let n = f.file_read(&mut buf[..read_len])
                .map_err(|e| from_lwext4(e.abs()))?;
            // Zero-fill the remainder of the page (last partial page or sparse)
            if n < PAGE_SIZE {
                buf[n..PAGE_SIZE].fill(0);
            }
            Ok(PAGE_SIZE)
        })();
        f.file_close().ok();
        result
    }

    fn write_page(&self, index: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
        if buf.len() < PAGE_SIZE {
            return Err(SyscallErr::ENOBUFS);
        }
        let fs = self.fs.upgrade().ok_or(SyscallErr::EIO)?;
        let offset = index * PAGE_SIZE;
        let write_len = PAGE_SIZE.min(buf.len());
        let _lock = fs.lw.lock();
        let mut f = Ext4File::new(&self.lw_path, InodeTypes::EXT4_DE_REG_FILE);
        // Try O_RDWR ("r+") first, fall back to O_RDWR|O_CREAT|O_TRUNC ("w+")
        if f.file_open(&self.lw_path, 0x2).is_err() {
            f.file_open(&self.lw_path, 0x242)
                .map_err(|e| from_lwext4(e.abs()))?;
        }
        // Use closure to ensure file_close() on all error paths
        let result = (|| -> Result<usize, SyscallErr> {
            f.file_seek(offset as i64, 0)
                .map_err(|e| from_lwext4(e.abs()))?;
            let n = f.file_write(&buf[..write_len])
                .map_err(|e| from_lwext4(e.abs()))?;
            // fetch_max: update logical_size if we extended the file
            Self::note_logical_size_atomic(&self.logical_size, offset + n);
            Ok(n)
        })();
        f.file_close().ok();
        result
    }

    fn read_pages(
        &self,
        start_index: usize,
        pages: &mut [&mut [u8]],
    ) -> Result<usize, SyscallErr> {
        crate::task::perf::record_pc_miss();
        let fs = self.fs.upgrade().ok_or(SyscallErr::EIO)?;
        let _lock = fs.lw.lock();
        let mut f = Ext4File::new(&self.lw_path, InodeTypes::EXT4_DE_REG_FILE);
        f.file_open(&self.lw_path, 0x0)
            .map_err(|e| from_lwext4(e.abs()))?;
        let start_offset = start_index * PAGE_SIZE;
        let result = (|| -> Result<usize, SyscallErr> {
            f.file_seek(start_offset as i64, 0)
                .map_err(|e| from_lwext4(e.abs()))?;
            let mut total = 0;
            for page in pages.iter_mut() {
                let read_len = PAGE_SIZE.min(page.len());
                let n = f.file_read(&mut page[..read_len])
                    .map_err(|e| from_lwext4(e.abs()))?;
                if n < PAGE_SIZE {
                    let page_len = page.len();
                    page[n..PAGE_SIZE.min(page_len)].fill(0);
                }
                total += n;
            }
            Ok(total)
        })();
        f.file_close().ok();
        result
    }

    fn write_pages(
        &self,
        start_index: usize,
        pages: &[&[u8]],
    ) -> Result<usize, SyscallErr> {
        let fs = self.fs.upgrade().ok_or(SyscallErr::EIO)?;
        let _lock = fs.lw.lock();
        let mut f = Ext4File::new(&self.lw_path, InodeTypes::EXT4_DE_REG_FILE);
        if f.file_open(&self.lw_path, 0x2).is_err() {
            f.file_open(&self.lw_path, 0x242)
                .map_err(|e| from_lwext4(e.abs()))?;
        }
        let start_offset = start_index * PAGE_SIZE;
        let result = (|| -> Result<usize, SyscallErr> {
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
                        self.lw_path, eof, raw_total, start_offset,
                        pages.len()
                    );
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
                if remaining == 0 { break; }
                let n = page_bytes.min(remaining);
                staging[copied..copied + n].copy_from_slice(&page[..n]);
                copied += n;
            }

            f.file_seek(start_offset as i64, 0)
                .map_err(|e| from_lwext4(e.abs()))?;
            let n = f.file_write(&staging[..total_bytes])
                .map_err(|e| from_lwext4(e.abs()))?;

            // Single logical_size update for entire batch
            Self::note_logical_size_atomic(&self.logical_size, start_offset + n);
            Ok(n)
        })();
        f.file_close().ok();
        result
    }

    fn npages(&self) -> usize {
        // Refresh size from lwext4 on first call
        if self.logical_size.load(Ordering::Relaxed) == LWEXT4_SIZE_UNKNOWN {
            if let Some(fs) = self.fs.upgrade() {
                let _lock = fs.lw.lock();
                let mut f = Ext4File::new(&self.lw_path, InodeTypes::EXT4_DE_REG_FILE);
                if f.file_open(&self.lw_path, 0x0).is_ok() {
                    let s = f.file_size() as usize;
                    self.logical_size.store(s, Ordering::Relaxed);
                    f.file_close().ok();
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
