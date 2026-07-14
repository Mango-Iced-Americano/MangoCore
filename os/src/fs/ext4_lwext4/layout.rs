//! Path-based `IndexNode` implementation backed by lwext4_rust.
//!
//! Since lwext4_rust operates on path strings rather than inode numbers,
//! each `Ext4OSInode` stores a full path from the mount root.  File
//! operations open the file by path, perform the I/O, and close.
//!
//! Phase 3 (read-only) and Phase 4 (write/create/delete) are complete.
//! All write methods delegate directly to lwext4 C calls; no PageCache layer
//! is involved yet (that is a separate project).

use alloc::ffi::CString;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::ffi::CStr;
use core::fmt;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::{Mutex, MutexGuard};

use crate::fs::vfs::{
    CreateAttrs, FilePrivateData, FileType, InodeFlags, InodeId,
    InodeMode, IndexNode, Metadata,
};
use crate::fs::vfs::file::FileFlags;
use crate::fs::vfs::file_system::FileSystem;
use crate::fs::page_cache::PageCache;
use crate::timer::TimeSpec;
use crate::utils::error::SyscallErr;

use super::counters;
use super::errno::from_lwext4;
use super::page_cache::{LwExt4PageCacheBackend, LWEXT4_SIZE_UNKNOWN};
use lwext4_rust::{Ext4File, InodeTypes};

/// Result of mapping lwext4 mode bits to MangoCore types.
pub(crate) struct MappedType {
    pub file_type: FileType,
    pub inode_mode: InodeMode,
}

/// Map lwext4 raw mode bits (`Ext4File::file_mode_get()`) to MangoCore
/// `FileType` and `InodeMode`.
pub(crate) fn map_lwext4_mode(mode_raw: u32) -> MappedType {
    let type_bits = mode_raw & 0xF000;
    let file_type = match type_bits {
        0x8000 => FileType::File,        // S_IFREG
        0x4000 => FileType::Dir,          // S_IFDIR
        0xA000 => FileType::SymLink,     // S_IFLNK
        0x2000 => FileType::CharDevice,  // S_IFCHR
        0x6000 => FileType::BlockDevice, // S_IFBLK
        0x1000 => FileType::Pipe,        // S_IFIFO
        0xC000 => FileType::Socket,      // S_IFSOCK
        _ => {
            log::warn!("[lwext4] unknown mode type bits 0x{:x}, assuming File", type_bits);
            FileType::File
        }
    };
    let inode_mode = InodeMode::from_bits_truncate(mode_raw);
    MappedType { file_type, inode_mode }
}

/// Build a child path from parent.
fn join_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        alloc::format!("/{}", name)
    } else {
        alloc::format!("{}/{}", parent, name)
    }
}

/// DJB2 hash — fallback inode ID when ext4_raw_inode_fill fails.
fn hash_path(path: &str) -> usize {
    let mut hash: usize = 5381;
    for b in path.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(b as usize);
    }
    hash
}

// ── FileGuard: RAII close for Ext4File handles ──────────────────────────

/// Automatically calls `file_close()` on drop to prevent resource leaks
/// when `?` early-returns from error paths.
struct FileGuard<'a> {
    f: &'a mut Ext4File,
}

impl<'a> FileGuard<'a> {
    fn new(f: &'a mut Ext4File) -> Self {
        Self { f }
    }
}

impl<'a> Drop for FileGuard<'a> {
    fn drop(&mut self) {
        self.f.file_close().ok();
    }
}

/// Cached inode metadata — avoids per-write lwext4 probing.
/// Built on first `metadata()` call, updated by `set_metadata()`.
/// Timestamps are not cached (always fresh from `TimeSpec::new()`).
#[derive(Clone, Copy)]
struct CachedMeta {
    mode: InodeMode,
    uid: u32,
    gid: u32,
}

// ── Ext4OSInode ─────────────────────────────────────────────────────────

/// A VFS inode backed by a path on an lwext4-mounted filesystem.
pub struct Ext4OSInode {
    /// Owning filesystem (strong reference — kernel is long-lived).
    fs: Arc<super::ext4fs::Ext4FileSystem>,
    /// Full path from mount root, e.g. `/bin/busybox`.
    path: String,
    /// Real ext4 inode number (obtained via ext4_raw_inode_fill).
    inode_id: usize,
    /// Cached file type (always known at construction time).
    file_type: FileType,
    /// Weak self-reference for `find(".")`.
    self_ref: Mutex<Option<Weak<Ext4OSInode>>>,
    /// On-demand PageCache — created on first `ensure_page_cache()` call.
    page_cache: Mutex<Option<Arc<PageCache>>>,
    /// Shared logical file size — prevents writeback from corrupting
    /// file size (1-byte write must not produce 4KB file).
    /// Lazily probed from lwext4 on first I/O via `logical_size_or_refresh()`.
    logical_size: Arc<AtomicUsize>,
    /// Cached inode metadata — avoids lwext4 probe on every touch_modified().
    cached_meta: Mutex<Option<CachedMeta>>,
}

// Safety: MangoCore is single-core; the circular Weak<Self> reference is
// proven safe at runtime (self_ref is only upgraded while the Arc is alive).
unsafe impl Send for Ext4OSInode {}
unsafe impl Sync for Ext4OSInode {}

impl fmt::Debug for Ext4OSInode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ext4OSInode")
            .field("path", &self.path)
            .field("inode_id", &self.inode_id)
            .field("file_type", &self.file_type)
            .finish()
    }
}

impl Ext4OSInode {
    /// Create a root inode (inode 2).
    pub(crate) fn new_root(
        fs: Arc<super::ext4fs::Ext4FileSystem>,
        inode_id: usize,
    ) -> Arc<Self> {
        Arc::new_cyclic(|weak| {
            Self {
                fs,
                path: String::from("/"),
                inode_id,
                file_type: FileType::Dir,
                self_ref: Mutex::new(Some(weak.clone())),
                page_cache: Mutex::new(None),
                logical_size: Arc::new(AtomicUsize::new(LWEXT4_SIZE_UNKNOWN)),
                cached_meta: Mutex::new(None),
            }
        })
    }

    /// Create a non-root inode by probing the path.
    ///
    /// Returns an `Arc<Self>` with `self_ref` correctly wired.
    fn new_child(
        fs: Arc<super::ext4fs::Ext4FileSystem>,
        path: String,
        inode_id: usize,
        file_type: FileType,
    ) -> Arc<Self> {
        Arc::new_cyclic(|weak| {
            Self {
                fs,
                path,
                inode_id,
                file_type,
                self_ref: Mutex::new(Some(weak.clone())),
                page_cache: Mutex::new(None),
                logical_size: Arc::new(AtomicUsize::new(LWEXT4_SIZE_UNKNOWN)),
                cached_meta: Mutex::new(None),
            }
        })
    }

    /// Create a non-root inode with pre-resolved metadata.
    /// The `cached_meta` and `logical_size` are seeded from the lookup
    /// cache, so subsequent `metadata()` calls hit the hot path (0 FFI).
    pub(crate) fn new_child_seeded(
        fs: Arc<super::ext4fs::Ext4FileSystem>,
        path: String,
        inode_id: usize,
        file_type: FileType,
        inode_mode: InodeMode,
        size: usize,
        uid: u32,
        gid: u32,
    ) -> Arc<Self> {
        Arc::new_cyclic(|weak| {
            Self {
                fs,
                path,
                inode_id,
                file_type,
                self_ref: Mutex::new(Some(weak.clone())),
                page_cache: Mutex::new(None),
                logical_size: Arc::new(AtomicUsize::new(size)),
                cached_meta: Mutex::new(Some(CachedMeta {
                    mode: inode_mode,
                    uid,
                    gid,
                })),
            }
        })
    }

    /// Get logical file size, lazily probing from lwext4 on first call.
    fn logical_size_or_refresh(&self) -> Result<usize, SyscallErr> {
        let _start = crate::task::perf::perf_time_now();
        let cached = self.logical_size.load(Ordering::Relaxed);
        if cached != LWEXT4_SIZE_UNKNOWN {
            counters::LWEXT4_LOGICAL_SIZE_CALLS.fetch_add(1, Ordering::Relaxed);
            counters::LWEXT4_LOGICAL_SIZE_CYCLES.fetch_add(
                crate::task::perf::perf_time_now().wrapping_sub(_start),
                Ordering::Relaxed,
            );
            return Ok(cached);
        }
        // One-time probe: open + file_size + close
        let size = {
            let _lock = self.fs.lw.lock();
            let lw_path = self.fs.lw_path(&self.path);
            let mut f = Ext4File::new(&lw_path, InodeTypes::EXT4_DE_REG_FILE);
            if f.file_open(&lw_path, 0x0).is_err() {
                // File doesn't exist yet — store 0 to avoid re-probing on
                // every write_at call. The file will be created on first
                // writeback (via O_RDWR|O_CREAT|O_TRUNC in write_page).
                self.logical_size.store(0, Ordering::Relaxed);
                counters::LWEXT4_LOGICAL_SIZE_CALLS.fetch_add(1, Ordering::Relaxed);
                counters::LWEXT4_LOGICAL_SIZE_CYCLES.fetch_add(
                    crate::task::perf::perf_time_now().wrapping_sub(_start),
                    Ordering::Relaxed,
                );
                return Ok(0);
            }
            let s = f.file_size() as usize;
            f.file_close().ok();
            s
        };
        self.logical_size.store(size, Ordering::Relaxed);
        counters::LWEXT4_LOGICAL_SIZE_CALLS.fetch_add(1, Ordering::Relaxed);
        counters::LWEXT4_LOGICAL_SIZE_CYCLES.fetch_add(
            crate::task::perf::perf_time_now().wrapping_sub(_start),
            Ordering::Relaxed,
        );
        Ok(size)
    }

    /// Update logical size if `new_size` is larger (fetch_max semantic).
    fn note_logical_size(&self, new_size: usize) {
        let mut prev = self.logical_size.load(Ordering::Relaxed);
        if prev == LWEXT4_SIZE_UNKNOWN {
            prev = 0;
        }
        while new_size > prev {
            match self.logical_size.compare_exchange_weak(
                prev, new_size, Ordering::Relaxed, Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => prev = actual,
            }
        }
    }
}

impl IndexNode for Ext4OSInode {
    // ═══════════════════════════════════════════════════════════════════
    //  Phase 3: read-only methods (KEPT)
    // ═══════════════════════════════════════════════════════════════════

    // ── metadata ─────────────────────────────────────────────────────

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        // Fast path: return from cache for regular files.
        // Saves ~5 lwext4 FFI calls (file_mode_get/file_open/file_size/file_close)
        // on every touch_modified() after write.
        if let Some(ref cached) = *self.cached_meta.lock() {
            counters::LWEXT4_METADATA_HOT.fetch_add(1, Ordering::Relaxed);
            let size_raw = self.logical_size.load(Ordering::Relaxed);
            let size: i64 = if size_raw == LWEXT4_SIZE_UNKNOWN {
                // Fallback: probe size (should be rare after cache-first find)
                self.logical_size_or_refresh().unwrap_or(0) as i64
            } else {
                size_raw as i64
            };
            let blocks = if self.file_type == FileType::File && size > 0 {
                (size as usize + self.fs.block_size() - 1) / self.fs.block_size()
            } else { 0 };
            return Ok(Metadata {
                dev_id: self.fs.dev_id(),
                inode_id: self.inode_id,
                size,
                blk_size: self.fs.block_size(),
                blocks,
                atime: TimeSpec::new(),
                mtime: TimeSpec::new(),
                ctime: TimeSpec::new(),
                file_type: self.file_type,
                mode: cached.mode,
                flags: InodeFlags::empty(),
                nlinks: if self.file_type == FileType::Dir { 2 } else { 1 },
                uid: cached.uid,
                gid: cached.gid,
                raw_dev: 0,
            });
        }

        // Cold path: probe from lwext4 and cache the result.
        // Inline probe type and mode inside a single lock scope.
        // Do NOT call self.fs.probe_type() which would re-lock self.fs.lw
        // and cause a spin::Mutex deadlock (spin::Mutex is not reentrant).
        let _cold_start = crate::task::perf::perf_time_now();
        counters::LWEXT4_METADATA_COLD.fetch_add(1, Ordering::Relaxed);
        let (file_type, inode_mode, size, blocks, uid, gid) = {
            let _lock = self.fs.lw.lock();
            let lw_path = self.fs.lw_path(&self.path);
            let mut f = Ext4File::new(&lw_path, InodeTypes::EXT4_DE_UNKNOWN);
            let mode_raw = f.file_mode_get().map_err(|e| from_lwext4(e.abs()))?;
            let mapped = map_lwext4_mode(mode_raw);

            // Fetch real uid/gid from on-disk inode via ext4_owner_get
            let mut lu: u32 = 0;
            let mut lg: u32 = 0;
            let c_path = CString::new(lw_path.as_str())
                .map_err(|_| SyscallErr::EINVAL)?;
            let c_path = c_path.into_raw();
            unsafe {
                lwext4_rust::bindings::ext4_owner_get(c_path, &mut lu, &mut lg);
            }
            unsafe { let _ = CString::from_raw(c_path); }

            let (size, blocks) = match mapped.file_type {
                FileType::Dir => (0i64, 0usize),
                FileType::SymLink => {
                    let mut rbuf = [0u8; 256];
                    let mut rcnt: usize = 0;
                    let c_path = CString::new(lw_path.as_str())
                        .map_err(|_| SyscallErr::EINVAL)?;
                    let c_path = c_path.into_raw();
                    let r = unsafe {
                        lwext4_rust::bindings::ext4_readlink(
                            c_path,
                            rbuf.as_mut_ptr() as *mut _,
                            255,
                            &mut rcnt,
                        )
                    };
                    unsafe { let _ = CString::from_raw(c_path); }
                    if r != 0 {
                        (0i64, 0usize)
                    } else {
                        (rcnt as i64, 0usize)
                    }
                }
                _ => {
                    let _fo_start = crate::task::perf::perf_time_now();
                    let mut ff = Ext4File::new(&lw_path, InodeTypes::EXT4_DE_REG_FILE);
                    let open_ok = ff.file_open(&lw_path, 0x0).is_ok();
                    counters::LWEXT4_FILE_OPEN_CALLS.fetch_add(1, Ordering::Relaxed);
                    counters::LWEXT4_FILE_OPEN_CYCLES.fetch_add(
                        crate::task::perf::perf_time_now().wrapping_sub(_fo_start),
                        Ordering::Relaxed,
                    );
                    if open_ok {
                        let s = ff.file_size();
                        counters::LWEXT4_FILE_SIZE_CALLS.fetch_add(1, Ordering::Relaxed);
                        let _fc_start = crate::task::perf::perf_time_now();
                        ff.file_close().ok();
                        counters::LWEXT4_FILE_CLOSE_CALLS.fetch_add(1, Ordering::Relaxed);
                        counters::LWEXT4_FILE_CLOSE_CYCLES.fetch_add(
                            crate::task::perf::perf_time_now().wrapping_sub(_fc_start),
                            Ordering::Relaxed,
                        );
                        let size_i64 = s as i64;
                        let blks = if s > 0 {
                            ((s as usize + self.fs.block_size() - 1)
                                / self.fs.block_size())
                        } else {
                            0
                        };
                        (size_i64, blks)
                    } else {
                        (0i64, 0usize)
                    }
                }
            };
            (mapped.file_type, mapped.inode_mode, size, blocks, lu, lg)
        };

        // Cache mode for hot path
        *self.cached_meta.lock() = Some(CachedMeta {
            mode: inode_mode,
            uid,
            gid,
        });

        counters::LWEXT4_METADATA_COLD_CYCLES.fetch_add(
            crate::task::perf::perf_time_now().wrapping_sub(_cold_start),
            Ordering::Relaxed,
        );

        // Metadata construction does NOT hold the lw lock.
        Ok(Metadata {
            dev_id: self.fs.dev_id(),
            inode_id: self.inode_id,
            size,
            blk_size: self.fs.block_size(),
            blocks,
            atime: TimeSpec::new(),
            mtime: TimeSpec::new(),
            ctime: TimeSpec::new(),
            file_type,
            mode: inode_mode,
            flags: InodeFlags::empty(),
            nlinks: if file_type == FileType::Dir { 2 } else { 1 },
            uid,
            gid,
            raw_dev: 0,
        })
    }

    // ── read_at ──────────────────────────────────────────────────────

    fn read_at(
        &self,
        offset: usize,
        len: usize,
        buf: &mut [u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        if buf.is_empty() || len == 0 {
            return Ok(0);
        }
        let actual = len.min(buf.len());

        match self.file_type {
            FileType::Dir => Err(SyscallErr::EISDIR),
            FileType::SymLink => {
                // Use ext4_readlink to get the symlink target content
                let _lock = self.fs.lw.lock();
                let lw_path = self.fs.lw_path(&self.path);
                let mut rbuf = [0u8; 256];
                let mut rcnt: usize = 0;
                let c_path = CString::new(lw_path.as_str())
                    .map_err(|_| SyscallErr::EINVAL)?;
                let c_path = c_path.into_raw();
                let r = unsafe {
                    lwext4_rust::bindings::ext4_readlink(
                        c_path,
                        rbuf.as_mut_ptr() as *mut _,
                        255,
                        &mut rcnt,
                    )
                };
                unsafe { let _ = CString::from_raw(c_path); }
                if r != 0 {
                    return Err(from_lwext4(r.abs()));
                }
                if offset >= rcnt {
                    return Ok(0);
                }
                let n = (rcnt - offset).min(actual);
                buf[..n].copy_from_slice(&rbuf[offset..offset + n]);
                Ok(n)
            }
            FileType::File => {
                // Always use PageCache (lazily created on first I/O)
                let pc = self.ensure_page_cache().ok_or(SyscallErr::EIO)?;
                let file_size = self.logical_size_or_refresh().unwrap_or(0);
                let read_end = (offset + actual).min(file_size);
                if offset >= read_end {
                    return Ok(0);
                }
                let read_len = read_end - offset;
                // ── Readahead: batch prefetch sequential pages (like legacy ext4) ──
                if let FilePrivateData::Readahead { ra_state } = &*_data {
                    let start_page = offset >> crate::config::PAGE_SIZE_BITS;
                    let end_page = (offset + actual.saturating_sub(1)) >> crate::config::PAGE_SIZE_BITS;
                    let req_pages = end_page.saturating_sub(start_page) + 1;
                    let mut ra = ra_state.lock();
                    pc.maybe_readahead(start_page, &mut ra, req_pages);
                }
                pc.read(offset, &mut buf[..read_len])
                    .map_err(|_| SyscallErr::EIO)
            }
            _ => Err(SyscallErr::EINVAL),
        }
    }

    // ── find ─────────────────────────────────────────────────────────

    fn find(&self, name: &str) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        let _start = crate::task::perf::perf_time_now();
        // Validate parent is a directory
        if self.file_type != FileType::Dir && name != "." && name != ".." {
            let elapsed = crate::task::perf::perf_time_now().wrapping_sub(_start);
            counters::LWEXT4_FIND_CALLS.fetch_add(1, Ordering::Relaxed);
            counters::LWEXT4_FIND_CYCLES.fetch_add(elapsed, Ordering::Relaxed);
            return Err(SyscallErr::ENOTDIR);
        }
        if name.is_empty() {
            let elapsed = crate::task::perf::perf_time_now().wrapping_sub(_start);
            counters::LWEXT4_FIND_CALLS.fetch_add(1, Ordering::Relaxed);
            counters::LWEXT4_FIND_CYCLES.fetch_add(elapsed, Ordering::Relaxed);
            return Err(SyscallErr::ENOENT);
        }
        if name.len() > 255 {
            let elapsed = crate::task::perf::perf_time_now().wrapping_sub(_start);
            counters::LWEXT4_FIND_CALLS.fetch_add(1, Ordering::Relaxed);
            counters::LWEXT4_FIND_CYCLES.fetch_add(elapsed, Ordering::Relaxed);
            return Err(SyscallErr::ENAMETOOLONG);
        }
        if name.contains('/') {
            let elapsed = crate::task::perf::perf_time_now().wrapping_sub(_start);
            counters::LWEXT4_FIND_CALLS.fetch_add(1, Ordering::Relaxed);
            counters::LWEXT4_FIND_CYCLES.fetch_add(elapsed, Ordering::Relaxed);
            return Err(SyscallErr::EINVAL);
        }

        // "." — return self via Weak upgrade
        if name == "." {
            let result = self
                .self_ref
                .lock()
                .as_ref()
                .and_then(|w| w.upgrade())
                .map(|arc| arc as Arc<dyn IndexNode>)
                .ok_or(SyscallErr::EIO);
            let elapsed = crate::task::perf::perf_time_now().wrapping_sub(_start);
            counters::LWEXT4_FIND_CALLS.fetch_add(1, Ordering::Relaxed);
            counters::LWEXT4_FIND_CYCLES.fetch_add(elapsed, Ordering::Relaxed);
            return result;
        }

        // ".." — return parent, or self if already at root
        if name == ".." {
            let result = if self.path == "/" {
                self
                    .self_ref
                    .lock()
                    .as_ref()
                    .and_then(|w| w.upgrade())
                    .map(|arc| arc as Arc<dyn IndexNode>)
                    .ok_or(SyscallErr::EIO)
            } else {
                // Compute parent path
                let parent_path = match self.path.rfind('/') {
                    Some(0) => "/",
                    Some(pos) => &self.path[..pos],
                    None => "/",
                };
                let parent_path = String::from(parent_path);
                // Use probe_inode_meta (cached) instead of get_inode_id
                let entry = self
                    .fs
                    .probe_inode_meta(&parent_path)
                    .unwrap_or_else(|_| {
                        super::ext4fs::LookupCacheEntry {
                            inode_id: 0,
                            file_type: FileType::Dir,
                            inode_mode: InodeMode::S_IFDIR
                                | InodeMode::from_bits_truncate(0o755),
                            size: 0,
                            uid: 0,
                            gid: 0,
                        }
                    });
                Ok(Ext4OSInode::new_child_seeded(
                    self.fs.clone(),
                    parent_path,
                    entry.inode_id,
                    FileType::Dir,
                    entry.inode_mode,
                    entry.size,
                    entry.uid,
                    entry.gid,
                ) as Arc<dyn IndexNode>)
            };
            let elapsed = crate::task::perf::perf_time_now().wrapping_sub(_start);
            counters::LWEXT4_FIND_CALLS.fetch_add(1, Ordering::Relaxed);
            counters::LWEXT4_FIND_CYCLES.fetch_add(elapsed, Ordering::Relaxed);
            return result;
        }

        // Build child path
        let child_path = join_path(&self.path, name);

        // Single lwext4 probe
        let entry = self.fs.probe_inode_meta(&child_path)?;

        let result = Ok(Ext4OSInode::new_child_seeded(
            self.fs.clone(),
            child_path,
            entry.inode_id,
            entry.file_type,
            entry.inode_mode,
            entry.size,
            entry.uid,
            entry.gid,
        ) as Arc<dyn IndexNode>);
        let elapsed = crate::task::perf::perf_time_now().wrapping_sub(_start);
        counters::LWEXT4_FIND_CALLS.fetch_add(1, Ordering::Relaxed);
        counters::LWEXT4_FIND_CYCLES.fetch_add(elapsed, Ordering::Relaxed);
        counters::LWEXT4_FIND_CACHE_MISS.fetch_add(1, Ordering::Relaxed);
        result
    }

    // ── list ─────────────────────────────────────────────────────────

    fn list(&self) -> Result<Vec<String>, SyscallErr> {
        if self.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }

        let _lock = self.fs.lw.lock();
        let lw_path = self.fs.lw_path(&self.path);
        let dir = Ext4File::new(&lw_path, InodeTypes::EXT4_DE_DIR);

        let _de_start = crate::task::perf::perf_time_now();
        let (names, _types) = dir
            .lwext4_dir_entries()
            .map_err(|e| from_lwext4(e.abs()))?;
        counters::LWEXT4_DIR_ENTRIES_CALLS.fetch_add(1, Ordering::Relaxed);
        counters::LWEXT4_DIR_ENTRIES_CYCLES.fetch_add(
            crate::task::perf::perf_time_now().wrapping_sub(_de_start),
            Ordering::Relaxed,
        );

        let mut result = Vec::with_capacity(names.len());
        for name_bytes in &names {
            // Null-terminated bytes; find the first 0 or use full length
            let len = name_bytes
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(name_bytes.len());
            if let Ok(s) = core::str::from_utf8(&name_bytes[..len]) {
                if !s.is_empty() {
                    result.push(String::from(s));
                }
            }
        }

        Ok(result)
    }

    // ── list_dirents ──────────────────────────────────────────────────

    fn list_dirents(&self) -> Result<Vec<(String, InodeId, FileType)>, SyscallErr> {
        if self.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        let _lock = self.fs.lw.lock();
        let lw_path = self.fs.lw_path(&self.path);
        let dir = Ext4File::new(&lw_path, InodeTypes::EXT4_DE_DIR);
        let _de_start = crate::task::perf::perf_time_now();
        let (names, types) = dir.lwext4_dir_entries()
            .map_err(|e| from_lwext4(e.abs()))?;
        counters::LWEXT4_DIR_ENTRIES_CALLS.fetch_add(1, Ordering::Relaxed);
        counters::LWEXT4_DIR_ENTRIES_CYCLES.fetch_add(
            crate::task::perf::perf_time_now().wrapping_sub(_de_start),
            Ordering::Relaxed,
        );

        let mut result = Vec::with_capacity(names.len());
        for (i, name_bytes) in names.iter().enumerate() {
            let len = name_bytes.iter().position(|&b| b == 0).unwrap_or(name_bytes.len());
            if let Ok(s) = core::str::from_utf8(&name_bytes[..len]) {
                if !s.is_empty() {
                    let child_path = join_path(&self.path, s);
                    // Use hash-based pseudo inode ID to avoid re-locking
                    // self.fs.lw (spin::Mutex is not reentrant).
                    let inode_id = hash_path(&child_path);
                    let ft = match types.get(i).unwrap_or(&InodeTypes::EXT4_DE_UNKNOWN) {
                        InodeTypes::EXT4_DE_REG_FILE => FileType::File,
                        InodeTypes::EXT4_DE_DIR => FileType::Dir,
                        InodeTypes::EXT4_DE_SYMLINK => FileType::SymLink,
                        InodeTypes::EXT4_DE_CHRDEV => FileType::CharDevice,
                        InodeTypes::EXT4_DE_BLKDEV => FileType::BlockDevice,
                        InodeTypes::EXT4_DE_FIFO => FileType::Pipe,
                        InodeTypes::EXT4_DE_SOCK => FileType::Socket,
                        _ => FileType::File,
                    };
                    result.push((String::from(s), inode_id, ft));
                }
            }
        }
        Ok(result)
    }

    // ── fs ───────────────────────────────────────────────────────────

    fn fs(&self) -> Arc<dyn FileSystem> {
        self.fs.clone()
    }

    // ── as_any_ref ───────────────────────────────────────────────────

    fn as_any_ref(&self) -> &dyn Any {
        self
    }

    // ── open / close ──────────────────────────────────────────────────

    fn open(
        &self,
        _data: MutexGuard<FilePrivateData>,
        _flags: &FileFlags,
    ) -> Result<(), SyscallErr> {
        Ok(())
    }

    fn close(&self, _data: MutexGuard<FilePrivateData>) -> Result<(), SyscallErr> {
        // No-op: explicit sync/fsync is the durability boundary, not close.
        Ok(())
    }

    // ── sync / datasync ───────────────────────────────────────────────

    fn sync(&self) -> Result<(), SyscallErr> {
        // Flush VFS PageCache dirty pages to disk first
        if let Some(pc) = self.page_cache.lock().clone() {
            pc.writeback_all().map_err(|_| SyscallErr::EIO)?;
        }
        // Then flush lwext4 internal caches
        let _lock = self.fs.lw.lock();
        let lw_path = self.fs.lw_path(&self.path);
        let mut f = Ext4File::new(&lw_path, InodeTypes::EXT4_DE_UNKNOWN);
        f.file_cache_flush().map_err(|e| from_lwext4(e.abs()))?;
        Ok(())
    }

    fn datasync(&self) -> Result<(), SyscallErr> {
        self.sync()
    }

    // ── user buffer I/O ───────────────────────────────────────────────

    fn supports_user_buffer_io(&self) -> bool {
        // Regular files can use PageCache → UserBuffer direct I/O.
        // Directories and symlinks are handled by their own paths.
        self.file_type == FileType::File
    }

    fn read_at_user(
        &self,
        offset: usize,
        len: usize,
        dst: &mut crate::mm::UserBuffer,
    ) -> Result<usize, SyscallErr> {
        match self.file_type {
            FileType::Dir => Err(SyscallErr::EISDIR),
            FileType::SymLink => {
                // Symlinks: use existing read_at path (reads link target content)
                let actual_len = len.min(dst.len());
                let mut kbuf = alloc::vec![0u8; actual_len];
                let dummy = spin::Mutex::new(FilePrivateData::Unused);
                let guard = dummy.lock();
                let n = self.read_at(offset, actual_len, &mut kbuf, guard)?;
                dst.write(&kbuf[..n]);
                Ok(n)
            }
            FileType::File => {
                let pc = self.ensure_page_cache().ok_or(SyscallErr::EIO)?;
                let file_size = self.logical_size_or_refresh().unwrap_or(0);
                let read_end = (offset + len).min(file_size);
                if offset >= read_end {
                    return Ok(0);
                }
                // Direct PageCache → UserBuffer: ONE copy, no intermediate kbuf
                pc.read_user(offset, read_end - offset, dst)
                    .map_err(|_| SyscallErr::EIO)
            }
            _ => Err(SyscallErr::EINVAL),
        }
    }

    fn write_at_user(
        &self,
        offset: usize,
        len: usize,
        src: &crate::mm::UserBuffer,
    ) -> Result<usize, SyscallErr> {
        let actual_len = len.min(src.len());
        let mut kbuf = alloc::vec![0u8; actual_len];
        let n = src.read(&mut kbuf);
        let actual = len.min(n);
        let dummy = spin::Mutex::new(FilePrivateData::Unused);
        let guard = dummy.lock();
        self.write_at(offset, actual, &kbuf[..actual], guard)
    }

    // ── page_cache ────────────────────────────────────────────────────

    /// Read-only query of existing page cache (no lazy creation).
    fn page_cache(&self) -> Option<Arc<PageCache>> {
        self.page_cache.lock().clone()
    }

    /// Ensure a PageCache exists, creating one if necessary.
    ///
    /// Called by mmap page-fault and other VFS paths that need file data
    /// pages.  The backend delegates I/O to lwext4's file API.
    fn ensure_page_cache(&self) -> Option<Arc<PageCache>> {
        counters::LWEXT4_ENSURE_PC_CALLS.fetch_add(1, Ordering::Relaxed);
        let mut cache = self.page_cache.lock();
        if cache.is_none() {
            // Check global registry for existing PageCache (shared across inode instances)
            {
                let registry = self.fs.page_caches.lock();
                if let Some(pc) = registry.get(&self.inode_id) {
                    *cache = Some(pc.clone());
                    return cache.clone();
                }
            }
            // No existing PageCache — create new one and register
            let backend = Arc::new(LwExt4PageCacheBackend::new(
                Arc::downgrade(&self.fs),
                self.path.clone(),
                self.logical_size.clone(),
                self.fs.lw_path(&self.path),
            ));
            let pc = PageCache::new();
            pc.set_backend(backend);
            // Register in global strong registry — dirty pages survive dentry eviction
            self.fs.page_caches.lock().insert(self.inode_id, pc.clone());
            *cache = Some(pc);
            counters::LWEXT4_ENSURE_PC_CREATES.fetch_add(1, Ordering::Relaxed);
        }
        cache.clone()
    }

    // ═══════════════════════════════════════════════════════════════════
    //  Phase 4: write/create/delete methods
    // ═══════════════════════════════════════════════════════════════════

    fn write_at(
        &self,
        offset: usize,
        len: usize,
        buf: &[u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        if self.file_type == FileType::Dir {
            return Err(SyscallErr::EISDIR);
        }
        if buf.is_empty() || len == 0 {
            return Ok(0);
        }
        let actual = len.min(buf.len());

        // Regular files: always use PageCache (lazily created on first I/O)
        if self.file_type == FileType::File {
            let pc = self.ensure_page_cache().ok_or(SyscallErr::EIO)?;
            let old_size = self.logical_size_or_refresh().unwrap_or(0);
            // Pre-publish expected EOF so writeback inside balance_dirty_pages()
            // (triggered by pc.write()) sees the new file size instead of
            // clamping writes to the old EOF (e.g. 0 for a new file).
            // Without this, a balance_dirty_pages() → write_pages() call
            // during pc.write() would see the stale EOF, return Ok(0), and
            // permanently clean dirty pages without writing them.
            let expected_new_end = (offset + actual).max(old_size);
            self.note_logical_size(expected_new_end);
            let n = pc.write(offset, &buf[..actual], Some(old_size))
                .map_err(|_| SyscallErr::EIO)?;
            // If the write was partial, note_logical_size is a fetch_max —
            // this is a no-op when the full write succeeded.
            let actual_new_end = offset + n;
            self.note_logical_size(actual_new_end);
            return Ok(n);
        }

        // Non-File types: keep existing behavior (PageCache or direct I/O)
        if let Some(pc) = self.page_cache() {
            return pc.write(offset, &buf[..actual], None)
                .map_err(|_| SyscallErr::EIO);
        }

        // Direct I/O fallback
        let _lock = self.fs.lw.lock();
        let lw_path = self.fs.lw_path(&self.path);
        let mut f = Ext4File::new(&lw_path, InodeTypes::EXT4_DE_REG_FILE);
        // O_RDWR ("r+"): does not truncate. Fall back to O_RDWR|O_CREAT|O_TRUNC
        // ("w+") only if the file doesn't exist yet.
        let open_result = f.file_open(&lw_path, 0x2);
        if open_result.is_err() {
            f.file_open(&lw_path, 0x242)
                .map_err(|e| from_lwext4(e.abs()))?;
        }
        let guard = FileGuard::new(&mut f);
        guard.f.file_seek(offset as i64, 0)
            .map_err(|e| from_lwext4(e.abs()))?;
        let n = guard.f.file_write(&buf[..actual])
            .map_err(|e| from_lwext4(e.abs()))?;
        drop(guard);
        Ok(n)
    }

    fn create(
        &self,
        name: &str,
        file_type: FileType,
        mode: InodeMode,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        if self.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        if name.is_empty() || name.len() > 255 || name.contains('/') {
            return Err(SyscallErr::EINVAL);
        }
        let child_path = join_path(&self.path, name);
        match file_type {
            FileType::File => {
                let _lock = self.fs.lw.lock();
                let lw_child = self.fs.lw_path(&child_path);
                let mut f = Ext4File::new(&lw_child, InodeTypes::EXT4_DE_REG_FILE);
                if f.check_inode_exist(&lw_child, InodeTypes::EXT4_DE_REG_FILE) {
                    return Err(SyscallErr::EEXIST);
                }
                f.file_open(&lw_child, 0x242)
                    .map_err(|e| from_lwext4(e.abs()))?;
                {
                    let mut guard = FileGuard::new(&mut f);
                    drop(guard);
                }
                f.file_mode_set(mode.bits())
                    .map_err(|e| from_lwext4(e.abs()))?;
                drop(_lock);
                // Use real ext4 inode number so the PageCache stays findable
                // after rename (the real inode number is stable across renames;
                // hash_path changes when the path changes).
                let real_inode = self
                    .fs
                    .probe_inode_meta(&child_path)
                    .map(|e| e.inode_id)
                    .unwrap_or_else(|_| hash_path(&child_path));
                let inode = Ext4OSInode::new_child_seeded(
                    self.fs.clone(),
                    child_path,
                    real_inode,
                    FileType::File,
                    mode,
                    0,
                    0,
                    0,
                );
                Ok(inode)
            }
            FileType::Dir => self.mkdir(name, mode),
            _ => Err(SyscallErr::EINVAL),
        }
    }

    fn create_with_data(
        &self,
        name: &str,
        file_type: FileType,
        mode: InodeMode,
        data: usize,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        if file_type == FileType::SymLink {
            // data is a *const c_char null-terminated string in kernel space
            // SAFETY: caller guarantees data is a valid null-terminated C string
            // c_char type varies across Rust nightly versions; core::ffi::c_char is always correct
            let target_bytes = unsafe { CStr::from_ptr(data as *const core::ffi::c_char) };
            let target = target_bytes.to_str().map_err(|_| SyscallErr::EINVAL)?;
            self.symlink(name, target)
        } else {
            self.create(name, file_type, mode)
        }
    }

    fn create_with_attrs(
        &self,
        name: &str,
        file_type: FileType,
        attrs: CreateAttrs,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        let full_mode = InodeMode::from(file_type) | (attrs.mode & InodeMode::S_IALLUGO);
        let inode = self.create(name, file_type, full_mode)?;
        let mut meta = inode.metadata()?;
        meta.uid = attrs.uid;
        meta.gid = attrs.gid;
        meta.mode = full_mode;
        inode.set_metadata(&meta)?;
        Ok(inode)
    }

    fn mkdir(
        &self,
        name: &str,
        mode: InodeMode,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        if self.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        if name.is_empty() || name.len() > 255 || name.contains('/') {
            return Err(SyscallErr::EINVAL);
        }
        let child_path = join_path(&self.path, name);
        let _lock = self.fs.lw.lock();
        let lw_child = self.fs.lw_path(&child_path);
        let mut d = Ext4File::new(&lw_child, InodeTypes::EXT4_DE_DIR);
        d.dir_mk(&lw_child)
            .map_err(|e| from_lwext4(e.abs()))?;
        // Set mode on the newly created directory
        d.file_mode_set(mode.bits())
            .map_err(|e| from_lwext4(e.abs()))?;
        drop(_lock);
        let real_inode = self
            .fs
            .probe_inode_meta(&child_path)
            .map(|e| e.inode_id)
            .unwrap_or_else(|_| hash_path(&child_path));
        Ok(Ext4OSInode::new_child_seeded(
            self.fs.clone(),
            child_path,
            real_inode,
            FileType::Dir,
            mode,
            0,
            0,
            0,
        ))
    }

    fn unlink(&self, name: &str) -> Result<(), SyscallErr> {
        if self.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        let child_path = join_path(&self.path, name);
        let _lock = self.fs.lw.lock();
        let lw_child = self.fs.lw_path(&child_path);
        // Verify the file exists (file_remove tolerates ENOENT in lwext4)
        // Use EXT4_DE_UNKNOWN to accept any non-directory type (files, symlinks, FIFOs, etc.)
        let mut probe = Ext4File::new(&lw_child, InodeTypes::EXT4_DE_UNKNOWN);
        if !probe.check_inode_exist(&lw_child, InodeTypes::EXT4_DE_UNKNOWN) {
            return Err(SyscallErr::ENOENT);
        }
        let mut f = Ext4File::new(&lw_child, InodeTypes::EXT4_DE_UNKNOWN);
        let r = f.file_remove(&lw_child);
        if r.is_err() {
            return Err(from_lwext4(r.unwrap_err().abs()));
        }
        Ok(())
    }

    fn rmdir(&self, name: &str) -> Result<(), SyscallErr> {
        if self.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        let child_path = join_path(&self.path, name);
        let lw_child;
        let mut candidates;

        // ── Phase 1: list directory entries under lock ──
        {
            let _lock = self.fs.lw.lock();
            lw_child = self.fs.lw_path(&child_path);
            let dir = Ext4File::new(&lw_child, InodeTypes::EXT4_DE_DIR);
            let _de_start = crate::task::perf::perf_time_now();
            let (entries, types) = dir
                .lwext4_dir_entries()
                .map_err(|e| from_lwext4(e.abs()))?;
            counters::LWEXT4_DIR_ENTRIES_CALLS.fetch_add(1, Ordering::Relaxed);
            counters::LWEXT4_DIR_ENTRIES_CYCLES.fetch_add(
                crate::task::perf::perf_time_now().wrapping_sub(_de_start),
                Ordering::Relaxed,
            );

            // Collect non-trivial child names, skipping EXT4_DE_UNKNOWN
            // (inode freed but directory entry survived).
            candidates = Vec::new();
            for (b, t) in entries.iter().zip(types.iter()) {
                if *t == InodeTypes::EXT4_DE_UNKNOWN {
                    continue;
                }
                let len = b.iter().position(|&x| x == 0).unwrap_or(b.len());
                let s = core::str::from_utf8(&b[..len]).unwrap_or("");
                if s == "." || s == ".." || s.is_empty() {
                    continue;
                }
                candidates.push(alloc::string::String::from(s));
            }
        } // _lock dropped here

        // ── Phase 2: verify each candidate's inode still exists ──
        // A directory entry may have a valid file_type byte but a
        // freed inode (inode == 0 on disk).  The EXT4_DE_UNKNOWN
        // check above catches type==0, but the type byte is set at
        // file creation and never cleared when the inode is freed.
        // We must probe the inode directly to distinguish orphans.
        let has_real = candidates.iter().any(|s| {
            let entry_path = alloc::format!("{}/{}", lw_child, s);
            let path = self.fs.lw_path(&entry_path);
            let mut probe = Ext4File::new(&path, InodeTypes::EXT4_DE_UNKNOWN);
            probe.check_inode_exist(&path, InodeTypes::EXT4_DE_UNKNOWN)
        });
        if has_real {
            return Err(SyscallErr::ENOTEMPTY);
        }

        // ── Phase 3: all children are orphans — remove the dir ──
        {
            let _lock = self.fs.lw.lock();
            let lw_child = self.fs.lw_path(&child_path);
            let mut f = Ext4File::new(&lw_child, InodeTypes::EXT4_DE_DIR);
            f.dir_rm(&lw_child)
                .map_err(|e| from_lwext4(e.abs()))?;
        }
        Ok(())
    }

    fn rename(
        &self,
        old_name: &str,
        new_parent: &Arc<dyn IndexNode>,
        new_name: &str,
        flags: u32,
    ) -> Result<(), SyscallErr> {
        use crate::fs::vfs::RENAME_NOREPLACE;

        if self.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        let old_path = join_path(&self.path, old_name);
        let new_parent_node = new_parent
            .as_any_ref()
            .downcast_ref::<Ext4OSInode>()
            .ok_or(SyscallErr::EXDEV)?;
        // Cross-filesystem rename not supported by lwext4
        if !Arc::ptr_eq(&self.fs, &new_parent_node.fs) {
            return Err(SyscallErr::EXDEV);
        }
        let new_path = join_path(&new_parent_node.path, new_name);

        // ── Pre-rename safety checks (Linux rename semantics) ──
        // These MUST run BEFORE any destructive operations (target removal,
        // PageCache flush, lwext4 rename).  Without them the lwext4 C call
        // may succeed in cases that Linux would reject.

        // 0. Same source and destination → no-op
        if old_path == new_path {
            return Ok(());
        }

        // 1. Probe source inode — fail fast if source doesn't exist
        let src_entry = self.fs.probe_inode_meta(&old_path)
            .map_err(|_| SyscallErr::ENOENT)?;
        let src_is_dir = src_entry.file_type == FileType::Dir;

        // 2. Probe target inode (may not exist — `ok()` swallows ENOENT)
        if let Ok(tgt) = self.fs.probe_inode_meta(&new_path) {
            let target_is_dir = tgt.file_type == FileType::Dir;

            // 2a. RENAME_NOREPLACE — target exists but caller forbids overwrite
            if flags & RENAME_NOREPLACE != 0 {
                return Err(SyscallErr::EEXIST);
            }

            // 2b. Type-mismatch checks (Linux rename(2) semantics)
            //     source=dir  && target=non-dir  → ENOTDIR
            //     source=file && target=dir      → EISDIR
            if src_is_dir && !target_is_dir {
                return Err(SyscallErr::ENOTDIR);
            }
            if !src_is_dir && target_is_dir {
                return Err(SyscallErr::EISDIR);
            }

            // 2c. Non-empty target directory → ENOTEMPTY
            //     Only directories can be overwritten when empty; non-empty
            //     dirs are rejected before any destructive work.
            if target_is_dir {
                let non_empty = {
                    let _lock = self.fs.lw.lock();
                    let lw_new = self.fs.lw_path(&new_path);
                    let dir = Ext4File::new(&lw_new, InodeTypes::EXT4_DE_DIR);
                    let (names, types) = dir
                        .lwext4_dir_entries()
                        .map_err(|e| from_lwext4(e.abs()))?;
                    names.iter().zip(types.iter()).any(|(b, t)| {
                        if *t == InodeTypes::EXT4_DE_UNKNOWN {
                            return false;
                        }
                        let len = b.iter().position(|&x| x == 0).unwrap_or(b.len());
                        let s = core::str::from_utf8(&b[..len]).unwrap_or("");
                        s != "." && s != ".." && !s.is_empty()
                    })
                };
                if non_empty {
                    return Err(SyscallErr::ENOTEMPTY);
                }
            }
        }

        // 3. Subtree check: a directory cannot be moved into its own
        //    descendant (e.g. moving /a into /a/b → EINVAL).
        //    Path-based check: if new_parent's path lies inside old_path.
        if src_is_dir {
            let prefix = alloc::format!("{}/", old_path);
            if new_parent_node.path == old_path
                || new_parent_node.path.starts_with(&prefix)
            {
                return Err(SyscallErr::EINVAL);
            }
        }

        // ── PageCache coherence: flush dirty pages before rename ──
        // The PageCache may hold dirty write data for the source (or target)
        // file.  If we rename without flushing, those dirty pages will later
        // be written back using the old path, potentially recreating the file.
        // After Fix 1, the inode_id is the real ext4 inode (stable across
        // renames), so the PageCache is findable by inode_id — but the
        // backend still stores the old lw_path.  Flush to disk first, then
        // invalidate the cache so a fresh one with the new path is created
        // on next access.
        let flush_one = |fs: &Arc<super::ext4fs::Ext4FileSystem>, path: &str| -> Result<usize, SyscallErr> {
            // Get the inode_id: try real inode first, fall back to hash_path
            // (for files created before Fix 1).
            let inode_id = fs
                .probe_inode_meta(path)
                .map(|e| e.inode_id)
                .unwrap_or_else(|_| hash_path(path));
            let registry = fs.page_caches.lock();
            if let Some(pc) = registry.get(&inode_id).cloned() {
                drop(registry);
                pc.writeback_all().map_err(|_| SyscallErr::EIO)?;
            }
            Ok(inode_id)
        };

        let old_inode_id = flush_one(&self.fs, &old_path)?;
        let new_inode_id = flush_one(&self.fs, &new_path)?;

        // ── Actual rename ──
        let _lock = self.fs.lw.lock();
        let lw_old = self.fs.lw_path(&old_path);
        let lw_new = self.fs.lw_path(&new_path);

        // Handle target-exists before rename.
        // Use a short-lived probe to check existence, then drop it before
        // any mutation so that fresh Ext4File objects are used for removal.
        let target_exists = {
            let mut probe = Ext4File::new(&lw_new, InodeTypes::EXT4_DE_UNKNOWN);
            probe.check_inode_exist(&lw_new, InodeTypes::EXT4_DE_UNKNOWN)
        };
        if target_exists {
            if flags & RENAME_NOREPLACE != 0 {
                return Err(SyscallErr::EEXIST);
            }
            // Normal rename (no RENAME_NOREPLACE): remove target with
            // a fresh Ext4File — never reuse the probe object.
            let target_is_dir = {
                let mut p = Ext4File::new(&lw_new, InodeTypes::EXT4_DE_DIR);
                p.check_inode_exist(&lw_new, InodeTypes::EXT4_DE_DIR)
            };
            if target_is_dir {
                let mut d = Ext4File::new(&lw_new, InodeTypes::EXT4_DE_DIR);
                d.dir_rm(&lw_new)
                    .map_err(|e| from_lwext4(e.abs()))?;
            } else {
                let mut f = Ext4File::new(&lw_new, InodeTypes::EXT4_DE_UNKNOWN);
                f.file_remove(&lw_new)
                    .map_err(|e| from_lwext4(e.abs()))?;
            }
        }

        // Try file_rename first; if it fails (likely because source is a
        // directory), fall back to dir_mv.
        let mut f = Ext4File::new(&lw_old, InodeTypes::EXT4_DE_UNKNOWN);
        let r = f.file_rename(&lw_old, &lw_new);
        if r.is_err() {
            let mut d = Ext4File::new(&lw_old, InodeTypes::EXT4_DE_DIR);
            d.dir_mv(&lw_old, &lw_new)
                .map_err(|e| from_lwext4(e.abs()))?;
        }
        drop(_lock);

        // ── PageCache coherence: invalidate stale entries after rename ──
        // The source file's PageCache backend still references the old
        // lw_path.  Remove it from the registry; the next access to the
        // file under its new name will create a fresh PageCache with the
        // correct path.
        self.fs.page_caches.lock().remove(&old_inode_id);
        // Also remove the overwritten target's cache (its inode is gone).
        self.fs.page_caches.lock().remove(&new_inode_id);

        Ok(())
    }

    fn truncate(&self, len: usize) -> Result<(), SyscallErr> {
        if self.file_type == FileType::Dir {
            return Err(SyscallErr::EISDIR);
        }
        let _lock = self.fs.lw.lock();
        let lw_path = self.fs.lw_path(&self.path);
        let mut f = Ext4File::new(&lw_path, InodeTypes::EXT4_DE_REG_FILE);
        f.file_open(&lw_path, 0x2)
            .map_err(|e| from_lwext4(e.abs()))?;
        let guard = FileGuard::new(&mut f);
        guard.f.file_truncate(len as u64)
            .map_err(|e| from_lwext4(e.abs()))?;
        drop(guard);
        self.logical_size.store(len, Ordering::Relaxed);
        Ok(())
    }

    fn resize(&self, len: usize) -> Result<(), SyscallErr> {
        self.truncate(len)
    }

    fn symlink(
        &self,
        name: &str,
        target: &str,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        if self.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        if name.is_empty() || name.len() > 255 || name.contains('/') {
            return Err(SyscallErr::EINVAL);
        }
        let child_path = join_path(&self.path, name);
        let _lock = self.fs.lw.lock();
        let lw_child = self.fs.lw_path(&child_path);
        // ext4_fsymlink(target, path): target = destination, path = new symlink
        // NOTE: target is NOT translated — it is user-data, VFS-semantic.
        let c_target = CString::new(target).map_err(|_| SyscallErr::EINVAL)?;
        let c_path = CString::new(lw_child.as_str()).map_err(|_| SyscallErr::EINVAL)?;
        let c_target_raw = c_target.into_raw();
        let c_path_raw = c_path.into_raw();
        let r = unsafe { lwext4_rust::bindings::ext4_fsymlink(c_target_raw, c_path_raw) };
        unsafe {
            let _ = CString::from_raw(c_target_raw);
            let _ = CString::from_raw(c_path_raw);
        }
        if r != 0 {
            return Err(from_lwext4(r.abs()));
        }
        drop(_lock);
        let real_inode = self
            .fs
            .probe_inode_meta(&child_path)
            .map(|e| e.inode_id)
            .unwrap_or_else(|_| hash_path(&child_path));
        Ok(Ext4OSInode::new_child_seeded(
            self.fs.clone(),
            child_path,
            real_inode,
            FileType::SymLink,
            InodeMode::S_IFLNK | InodeMode::from_bits_truncate(0o777),
            target.len(),
            0,
            0,
        ))
    }

    fn link(&self, name: &str, other: &Arc<dyn IndexNode>) -> Result<(), SyscallErr> {
        if self.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        let new_path = join_path(&self.path, name);
        let other_node = other
            .as_any_ref()
            .downcast_ref::<Ext4OSInode>()
            .ok_or(SyscallErr::EXDEV)?;
        // Cross-filesystem hard link not supported by lwext4
        if !Arc::ptr_eq(&self.fs, &other_node.fs) {
            return Err(SyscallErr::EXDEV);
        }
        let _lock = self.fs.lw.lock();
        let lw_src = self.fs.lw_path(&other_node.path);
        let lw_new = self.fs.lw_path(&new_path);
        // ext4_flink(path, hardlink_path): path = source, hardlink_path = new link
        let c_src = CString::new(lw_src.as_str()).map_err(|_| SyscallErr::EINVAL)?;
        let c_new = CString::new(lw_new.as_str()).map_err(|_| SyscallErr::EINVAL)?;
        let c_src_raw = c_src.into_raw();
        let c_new_raw = c_new.into_raw();
        let r = unsafe { lwext4_rust::bindings::ext4_flink(c_src_raw, c_new_raw) };
        unsafe {
            let _ = CString::from_raw(c_src_raw);
            let _ = CString::from_raw(c_new_raw);
        }
        if r != 0 {
            return Err(from_lwext4(r.abs()));
        }
        Ok(())
    }

    fn set_metadata(&self, metadata: &Metadata) -> Result<(), SyscallErr> {
        // Check if only timestamps changed (touch_modified after write).
        // In that case, just update the cache — skip all lwext4 FFI calls.
        // Actual time persistence happens at sync() time.
        let time_only = {
            if let Some(ref cached) = *self.cached_meta.lock() {
                cached.mode == metadata.mode
                    && cached.uid == metadata.uid
                    && cached.gid == metadata.gid
            } else {
                false
            }
        };

        // Always update cache
        *self.cached_meta.lock() = Some(CachedMeta {
            mode: metadata.mode,
            uid: metadata.uid,
            gid: metadata.gid,
        });

        // Time-only: skip lwext4 FFI, defer to sync()
        if time_only {
            return Ok(());
        }

        // Mode/owner changed: do full lwext4 write-through
        let _lock = self.fs.lw.lock();
        let lw_path = self.fs.lw_path(&self.path);

        // 1. chmod — fmode_set is a standalone operation (path-based, no open needed)
        let mut f = Ext4File::new(&lw_path, InodeTypes::EXT4_DE_UNKNOWN);
        f.file_mode_set(metadata.mode.bits())
            .map_err(|e| from_lwext4(e.abs()))?;

        let c_path = CString::new(lw_path.as_str()).map_err(|_| SyscallErr::EINVAL)?;
        let raw = c_path.into_raw();

        // 2. chown — ext4_owner_set(path, uid: u32, gid: u32) -> c_int
        //    Non-root callers will get EPERM; silently ignore.
        let _ =
            unsafe { lwext4_rust::bindings::ext4_owner_set(raw, metadata.uid, metadata.gid) };

        // 3. utime — ext4_{atime,mtime,ctime}_set(path, timestamp: u32) -> c_int
        //    Only set timestamps with non-zero tv_sec (caller signals intent).
        if metadata.atime.tv_sec != 0 {
            let _ = unsafe {
                lwext4_rust::bindings::ext4_atime_set(raw, metadata.atime.tv_sec as u32)
            };
        }
        if metadata.mtime.tv_sec != 0 {
            let _ = unsafe {
                lwext4_rust::bindings::ext4_mtime_set(raw, metadata.mtime.tv_sec as u32)
            };
        }
        if metadata.ctime.tv_sec != 0 {
            let _ = unsafe {
                lwext4_rust::bindings::ext4_ctime_set(raw, metadata.ctime.tv_sec as u32)
            };
        }

        unsafe { let _ = CString::from_raw(raw); }
        Ok(())
    }

    // ── mknod ───────────────────────────────────────────────────────────

    /// Create a special file (FIFO, char/block device, socket).
    ///
    /// Wraps lwext4's `ext4_mknod` FFI:
    ///   ext4_mknod(path, filetype: c_int, dev: u32) -> c_int
    ///
    /// Filetype mapping: EXT4_DE_CHRDEV=3, EXT4_DE_BLKDEV=4,
    /// EXT4_DE_FIFO=5, EXT4_DE_SOCK=6
    fn mknod(
        &self,
        filename: &str,
        mode: InodeMode,
        dev_t: u64,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        if self.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        if filename.is_empty() || filename.len() > 255 || filename.contains('/') {
            return Err(SyscallErr::EINVAL);
        }

        let child_path = join_path(&self.path, filename);

        // Map InodeMode::S_IFMT to lwext4 EXT4_DE_* file type constant
        let mode_bits = mode & InodeMode::S_IFMT;
        let (lw_type, file_type): (i32, FileType) = if mode_bits == InodeMode::S_IFIFO {
            (5, FileType::Pipe)  // EXT4_DE_FIFO
        } else if mode_bits == InodeMode::S_IFCHR {
            (3, FileType::CharDevice) // EXT4_DE_CHRDEV
        } else if mode_bits == InodeMode::S_IFBLK {
            (4, FileType::BlockDevice) // EXT4_DE_BLKDEV
        } else if mode_bits == InodeMode::S_IFSOCK {
            (6, FileType::Socket) // EXT4_DE_SOCK
        } else {
            return Err(SyscallErr::EINVAL);
        };

        let _lock = self.fs.lw.lock();
        let lw_child = self.fs.lw_path(&child_path);

        let c_path = CString::new(lw_child.as_str()).map_err(|_| SyscallErr::EINVAL)?;
        let c_path = c_path.into_raw();
        let r = unsafe {
            lwext4_rust::bindings::ext4_mknod(c_path, lw_type, dev_t as u32)
        };
        unsafe { let _ = CString::from_raw(c_path); }
        if r != 0 {
            return Err(from_lwext4(r.abs()));
        }

        // Set permission bits on the new special file
        let mut f = Ext4File::new(&lw_child, InodeTypes::EXT4_DE_UNKNOWN);
        f.file_mode_set(mode.bits())
            .map_err(|e| from_lwext4(e.abs()))?;

        let h = hash_path(&child_path);
        Ok(Ext4OSInode::new_child_seeded(
            self.fs.clone(),
            child_path,
            h,
            file_type,
            mode,
            0,
            0,
            0,
        ))
    }

    // ── extended attributes ─────────────────────────────────────────────

    /// Get an extended attribute value.
    ///
    /// Wraps lwext4's `ext4_getxattr` FFI:
    ///   ext4_getxattr(path, name, name_len, buf, buf_size, data_size) -> c_int
    ///
    /// If `buf` is empty, returns the attribute value size without writing.
    /// Returns `ENODATA` if the attribute does not exist.
    fn getxattr(&self, name: &str, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        let _lock = self.fs.lw.lock();

        let lw_path = self.fs.lw_path(&self.path);
        let c_path = CString::new(lw_path.as_str()).map_err(|_| SyscallErr::EINVAL)?;
        let c_path = c_path.into_raw();

        let mut data_size: usize = 0;
        let buf_ptr: *mut core::ffi::c_void = if buf.is_empty() {
            core::ptr::null_mut()
        } else {
            buf.as_mut_ptr() as *mut _
        };
        let r = unsafe {
            lwext4_rust::bindings::ext4_getxattr(
                c_path,
                name.as_ptr() as *const _,
                name.len(),
                buf_ptr,
                buf.len(),
                &mut data_size,
            )
        };
        unsafe { let _ = CString::from_raw(c_path); }

        if r != 0 {
            return Err(from_lwext4(r.abs()));
        }
        Ok(data_size)
    }

    /// Set an extended attribute value.
    ///
    /// Wraps lwext4's `ext4_setxattr` FFI:
    ///   ext4_setxattr(path, name, name_len, data, data_size) -> c_int
    ///
    /// Flags: XATTR_CREATE(1) fails if attr exists, XATTR_REPLACE(2)
    /// fails if attr does not exist, 0 = create-or-replace.
    fn setxattr(
        &self,
        name: &str,
        value: &[u8],
        flags: u32,
    ) -> Result<usize, SyscallErr> {
        let _lock = self.fs.lw.lock();

        let lw_path = self.fs.lw_path(&self.path);
        let c_path = CString::new(lw_path.as_str()).map_err(|_| SyscallErr::EINVAL)?;
        let c_path = c_path.into_raw();

        // Handle XATTR_CREATE / XATTR_REPLACE semantics
        if flags == 1 {
            // XATTR_CREATE: fail if attribute already exists
            let mut _ds: usize = 0;
            let probe = unsafe {
                lwext4_rust::bindings::ext4_getxattr(
                    c_path,
                    name.as_ptr() as *const _,
                    name.len(),
                    core::ptr::null_mut(),
                    0,
                    &mut _ds,
                )
            };
            if probe == 0 {
                unsafe { let _ = CString::from_raw(c_path); }
                return Err(SyscallErr::EEXIST);
            }
        } else if flags == 2 {
            // XATTR_REPLACE: fail if attribute does not exist
            let mut _ds: usize = 0;
            let probe = unsafe {
                lwext4_rust::bindings::ext4_getxattr(
                    c_path,
                    name.as_ptr() as *const _,
                    name.len(),
                    core::ptr::null_mut(),
                    0,
                    &mut _ds,
                )
            };
            if probe != 0 {
                unsafe { let _ = CString::from_raw(c_path); }
                return Err(SyscallErr::ENODATA);
            }
        }

        let r = unsafe {
            lwext4_rust::bindings::ext4_setxattr(
                c_path,
                name.as_ptr() as *const _,
                name.len(),
                value.as_ptr() as *const _,
                value.len(),
            )
        };
        unsafe { let _ = CString::from_raw(c_path); }

        if r != 0 {
            return Err(from_lwext4(r.abs()));
        }
        Ok(0)
    }

    /// List all extended attribute names (null-separated).
    ///
    /// Wraps lwext4's `ext4_listxattr` FFI:
    ///   ext4_listxattr(path, list, size, ret_size) -> c_int
    ///
    /// If `buf` is empty, returns the total size needed.
    /// Returns `ERANGE` if the buffer is too small.
    fn listxattr(&self, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        let _lock = self.fs.lw.lock();

        let lw_path = self.fs.lw_path(&self.path);
        let c_path = CString::new(lw_path.as_str()).map_err(|_| SyscallErr::EINVAL)?;
        let c_path = c_path.into_raw();

        let mut ret_size: usize = 0;
        let list_ptr: *mut core::ffi::c_char = if buf.is_empty() {
            core::ptr::null_mut()
        } else {
            buf.as_mut_ptr() as *mut _
        };
        let r = unsafe {
            lwext4_rust::bindings::ext4_listxattr(
                c_path,
                list_ptr,
                buf.len(),
                &mut ret_size,
            )
        };
        unsafe { let _ = CString::from_raw(c_path); }

        if r != 0 {
            return Err(from_lwext4(r.abs()));
        }
        Ok(ret_size)
    }

    /// Remove an extended attribute.
    ///
    /// Wraps lwext4's `ext4_removexattr` FFI:
    ///   ext4_removexattr(path, name, name_len) -> c_int
    ///
    /// Returns `ENODATA` if the attribute does not exist.
    fn removexattr(&self, name: &str) -> Result<usize, SyscallErr> {
        let _lock = self.fs.lw.lock();

        let lw_path = self.fs.lw_path(&self.path);
        let c_path = CString::new(lw_path.as_str()).map_err(|_| SyscallErr::EINVAL)?;
        let c_path = c_path.into_raw();

        let r = unsafe {
            lwext4_rust::bindings::ext4_removexattr(
                c_path,
                name.as_ptr() as *const _,
                name.len(),
            )
        };
        unsafe { let _ = CString::from_raw(c_path); }

        if r != 0 {
            return Err(from_lwext4(r.abs()));
        }
        Ok(0)
    }
}

// Strong page_caches registry keeps dirty PageCache alive after drop;
// no synchronous I/O in destructor.
