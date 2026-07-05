//! PageCache backend for lwext4 VFS adapter.
//!
//! Unlike the legacy `Ext4PageCacheBackend` (which does extent tree lookups
//! and block-level I/O), this backend delegates to lwext4's file API. It does
//! NOT need to know about ext4 on-disk structures — lwext4 handles block
//! mapping internally.
//!
//! Slow but correct — each page I/O does fopen/fseek/fread/fwrite/fclose.
//! For batching, `read_pages`/`write_pages` open once and do sequential I/O.

use alloc::sync::Weak;
use alloc::string::String;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::config::PAGE_SIZE;
use crate::fs::page_cache::PageCacheBackend;
use crate::utils::error::SyscallErr;

use lwext4_rust::{Ext4File, InodeTypes};

use super::errno::from_lwext4;

/// PageCache backend that reads/writes pages through lwext4 file API.
///
/// Holds a `Weak` reference to the owning filesystem (for lwext4 C call
/// serialization) and a cached file size (`Cell<usize>`) refreshed on each
/// `npages()` call.
pub struct LwExt4PageCacheBackend {
    /// Weak reference to the owning filesystem (for lwext4 C call serialization)
    fs: Weak<super::ext4fs::Ext4FileSystem>,
    /// Full path from mount root, e.g. "/bin/busybox"
    path: String,
    /// Cached file size (refreshed on each `npages()` call)
    size: AtomicUsize,
}

impl LwExt4PageCacheBackend {
    /// Create a new backend for the given file path.
    ///
    /// Probes the current file size on construction so `npages()` returns a
    /// sensible value even before the first I/O.
    pub fn new(fs: Weak<super::ext4fs::Ext4FileSystem>, path: String) -> Self {
        let size = if let Some(fs_arc) = fs.upgrade() {
            let _lock = fs_arc.lw.lock();
            let mut f = Ext4File::new(&path, InodeTypes::EXT4_DE_REG_FILE);
            if f.file_open(&path, 0x0).is_ok() {
                let s = f.file_size() as usize;
                f.file_close().ok();
                s
            } else {
                0
            }
        } else {
            0
        };
        Self {
            fs,
            path,
            size: AtomicUsize::new(size),
        }
    }
}

impl PageCacheBackend for LwExt4PageCacheBackend {
    fn read_page(&self, index: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        if buf.len() < PAGE_SIZE {
            return Err(SyscallErr::ENOBUFS);
        }
        let fs = self.fs.upgrade().ok_or(SyscallErr::EIO)?;
        let offset = index * PAGE_SIZE;
        let read_len = PAGE_SIZE.min(self.size.load(Ordering::Relaxed).saturating_sub(offset));
        if read_len == 0 {
            // Past EOF — fill with zeros
            buf[..PAGE_SIZE].fill(0);
            return Ok(PAGE_SIZE);
        }
        let _lock = fs.lw.lock();
        let mut f = Ext4File::new(&self.path, InodeTypes::EXT4_DE_REG_FILE);
        f.file_open(&self.path, 0x0)
            .map_err(|e| from_lwext4(e.abs()))?;
        f.file_seek(offset as i64, 0)
            .map_err(|e| from_lwext4(e.abs()))?;
        let n = f.file_read(&mut buf[..read_len])
            .map_err(|e| from_lwext4(e.abs()))?;
        // Zero-fill the remainder of the page (last partial page or sparse)
        if n < PAGE_SIZE {
            buf[n..PAGE_SIZE].fill(0);
        }
        f.file_close().ok();
        Ok(PAGE_SIZE)
    }

    fn write_page(&self, index: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
        if buf.len() < PAGE_SIZE {
            return Err(SyscallErr::ENOBUFS);
        }
        let fs = self.fs.upgrade().ok_or(SyscallErr::EIO)?;
        let offset = index * PAGE_SIZE;
        let write_len = PAGE_SIZE.min(buf.len());
        let _lock = fs.lw.lock();
        let mut f = Ext4File::new(&self.path, InodeTypes::EXT4_DE_REG_FILE);
        // Try O_RDWR ("r+") first, fall back to O_RDWR|O_CREAT|O_TRUNC ("w+")
        if f.file_open(&self.path, 0x2).is_err() {
            f.file_open(&self.path, 0x242)
                .map_err(|e| from_lwext4(e.abs()))?;
        }
        f.file_seek(offset as i64, 0)
            .map_err(|e| from_lwext4(e.abs()))?;
        let n = f.file_write(&buf[..write_len])
            .map_err(|e| from_lwext4(e.abs()))?;
        f.file_close().ok();
        // Update cached size so npages() reflects the new length
        let new_end = (offset + n).max(self.size.load(Ordering::Relaxed));
        self.size.store(new_end, Ordering::Relaxed);
        Ok(n)
    }

    fn read_pages(
        &self,
        start_index: usize,
        pages: &mut [&mut [u8]],
    ) -> Result<usize, SyscallErr> {
        let fs = self.fs.upgrade().ok_or(SyscallErr::EIO)?;
        let _lock = fs.lw.lock();
        let mut f = Ext4File::new(&self.path, InodeTypes::EXT4_DE_REG_FILE);
        f.file_open(&self.path, 0x0)
            .map_err(|e| from_lwext4(e.abs()))?;
        let start_offset = start_index * PAGE_SIZE;
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
        f.file_close().ok();
        Ok(total)
    }

    fn write_pages(
        &self,
        start_index: usize,
        pages: &[&[u8]],
    ) -> Result<usize, SyscallErr> {
        let fs = self.fs.upgrade().ok_or(SyscallErr::EIO)?;
        let _lock = fs.lw.lock();
        let mut f = Ext4File::new(&self.path, InodeTypes::EXT4_DE_REG_FILE);
        if f.file_open(&self.path, 0x2).is_err() {
            f.file_open(&self.path, 0x242)
                .map_err(|e| from_lwext4(e.abs()))?;
        }
        let start_offset = start_index * PAGE_SIZE;
        f.file_seek(start_offset as i64, 0)
            .map_err(|e| from_lwext4(e.abs()))?;

        let mut total = 0;
        for (i, page) in pages.iter().enumerate() {
            let write_len = PAGE_SIZE.min(page.len());
            let n = f.file_write(&page[..write_len])
                .map_err(|e| from_lwext4(e.abs()))?;
            total += n;
            let new_end = (start_offset + (i + 1) * PAGE_SIZE).max(self.size.load(Ordering::Relaxed));
            self.size.store(new_end, Ordering::Relaxed);
        }
        f.file_close().ok();
        Ok(total)
    }

    fn npages(&self) -> usize {
        // Refresh size from lwext4
        if let Some(fs) = self.fs.upgrade() {
            let _lock = fs.lw.lock();
            let mut f = Ext4File::new(&self.path, InodeTypes::EXT4_DE_REG_FILE);
            if f.file_open(&self.path, 0x0).is_ok() {
                let s = f.file_size() as usize;
                self.size.store(s, Ordering::Relaxed);
                f.file_close().ok();
            }
        }
        (self.size.load(Ordering::Relaxed) + PAGE_SIZE - 1) / PAGE_SIZE
    }
}
