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
use crate::config::{PAGE_SIZE, PAGE_SIZE_BITS};
use crate::fs::page_cache::PageCache;
use crate::timer::TimeSpec;
use crate::utils::error::SyscallErr;

use super::counters;
use super::errno::from_lwext4;
use super::inode_state::{CachedMeta, Ext4InodeState};
use super::page_cache::{LwExt4PageCacheBackend, LWEXT4_SIZE_UNKNOWN};
use super::with_lwext4_global;
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

// ── Ext4OSInode ─────────────────────────────────────────────────────────

/// A VFS inode backed by an lwext4 on-disk inode identity.
pub struct Ext4OSInode {
    /// Owning filesystem (strong reference — kernel is long-lived).
    fs: Arc<super::ext4fs::Ext4FileSystem>,
    /// Real ext4 inode number (obtained via ext4_raw_inode_fill).
    inode_id: usize,
    /// State shared by every alias/VFS object for this on-disk inode.
    state: Arc<Ext4InodeState>,
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
}

// Safety: MangoCore is single-core; the circular Weak<Self> reference is
// proven safe at runtime (self_ref is only upgraded while the Arc is alive).
unsafe impl Send for Ext4OSInode {}
unsafe impl Sync for Ext4OSInode {}

impl fmt::Debug for Ext4OSInode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ext4OSInode")
            .field("path", &self.state.current_path())
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
        generation: u32,
        nlinks: usize,
    ) -> Arc<Self> {
        let state = fs.inode_state(inode_id, generation, "/", 0, nlinks);
        let logical_size = state.logical_size();
        Arc::new_cyclic(move |weak| {
            Self {
                fs,
                inode_id,
                state,
                file_type: FileType::Dir,
                self_ref: Mutex::new(Some(weak.clone())),
                page_cache: Mutex::new(None),
                logical_size,
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
        generation: u32,
        file_type: FileType,
    ) -> Arc<Self> {
        let state = fs.inode_state(
            inode_id,
            generation,
            &path,
            LWEXT4_SIZE_UNKNOWN,
            1,
        );
        let logical_size = state.logical_size();
        Arc::new_cyclic(move |weak| {
            Self {
                fs,
                inode_id,
                state,
                file_type,
                self_ref: Mutex::new(Some(weak.clone())),
                page_cache: Mutex::new(None),
                logical_size,
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
        generation: u32,
        file_type: FileType,
        inode_mode: InodeMode,
        size: usize,
        uid: u32,
        gid: u32,
        nlinks: usize,
    ) -> Arc<Self> {
        let state = fs.inode_state(inode_id, generation, &path, size, nlinks);
        state.seed_cached_meta(CachedMeta {
            mode: inode_mode,
            uid,
            gid,
        });
        let logical_size = state.logical_size();
        Arc::new_cyclic(move |weak| {
            Self {
                fs,
                inode_id,
                state,
                file_type,
                self_ref: Mutex::new(Some(weak.clone())),
                page_cache: Mutex::new(None),
                logical_size,
            }
        })
    }

    /// Return the currently valid namespace path.  The shared state is
    /// rewritten after rename, so old VFS objects never fall back to a stale
    /// pathname that may later name a different inode.
    fn live_path(&self) -> Result<String, SyscallErr> {
        self.state.current_path().ok_or(SyscallErr::ENOENT)
    }

    /// Revalidate a previously-snapshotted pathname while the caller holds
    /// `fs.lw`.  Namespace paths are mutable shared state: a rename can change
    /// them between `live_path()` and a path-based lwext4 call.  Refusing the
    /// operation on an inode/generation mismatch prevents a stale pathname
    /// from mutating a newly-created inode that reused the old name.
    fn validate_path_locked(&self, path: &str) -> Result<(), SyscallErr> {
        let current = self.fs.probe_inode_meta_locked(path)?;
        if current.inode_id != self.inode_id
            || current.generation != self.state.generation()
            || current.file_type != self.file_type
        {
            return Err(SyscallErr::EAGAIN);
        }
        Ok(())
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
        // One-time probe through the inode handle when the file is open.
        // This remains valid after rename/unlink and never recreates a stale
        // pathname as an accidental writeback target.
        let size = self
            .state
            .with_file(&self.fs, false, |file| Ok(file.file_size() as usize))?;
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
        if let Some(cached) = self.state.cached_meta() {
            counters::LWEXT4_METADATA_HOT.fetch_add(1, Ordering::Relaxed);
            let size_raw = self.logical_size.load(Ordering::Relaxed);
            let size: i64 = if size_raw == LWEXT4_SIZE_UNKNOWN {
                // Fallback: probe size (should be rare after cache-first find)
                self.logical_size_or_refresh()? as i64
            } else {
                size_raw as i64
            };
            let blocks = if self.file_type == FileType::File && size > 0 {
                // Convert to 512-byte units (POSIX stat.st_blocks semantics).
                // lwext4 does not expose ext4 i_blocks directly, so we
                // conservatively ceil-divide the file size by 512.
                (size as usize + 511) / 512
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
                nlinks: self.state.nlinks() as u64,
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
        let path = self.live_path()?;
        // C entry point: the whole probe (validate + file_mode_get + owner_get
        // + readlink/size) runs under the process-wide lwext4 gate.  The
        // tuple is returned out of the gate and the Metadata built outside.
        let (file_type, inode_mode, size, blocks, uid, gid) =
            with_lwext4_global(|| -> Result<_, SyscallErr> {
                let _lock = self.fs.lw.lock();
                self.validate_path_locked(&path)?;
                let lw_path = self.fs.lw_path(&path);
                let mut f = Ext4File::new(&lw_path, InodeTypes::EXT4_DE_UNKNOWN);
                let mode_raw = f.file_mode_get().map_err(|e| from_lwext4(e.abs()))?;
                let mapped = map_lwext4_mode(mode_raw);

                // Fetch real uid/gid from on-disk inode via ext4_owner_get
                let mut lu: u32 = 0;
                let mut lg: u32 = 0;
                let c_path = CString::new(lw_path.as_str())
                    .map_err(|_| SyscallErr::EINVAL)?;
                let c_path = c_path.into_raw();
                let owner_result = unsafe {
                    lwext4_rust::bindings::ext4_owner_get(c_path, &mut lu, &mut lg)
                };
                unsafe { let _ = CString::from_raw(c_path); }
                if owner_result != 0 {
                    return Err(from_lwext4(owner_result.abs()));
                }

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
                                // Convert to 512-byte units (POSIX stat.st_blocks semantics).
                                // lwext4 does not expose ext4 i_blocks directly.
                                (s as usize + 511) / 512
                            } else {
                                0
                            };
                            (size_i64, blks)
                        } else {
                            (0i64, 0usize)
                        }
                    }
                };
                self.state.set_cached_meta(CachedMeta {
                    mode: mapped.inode_mode,
                    uid: lu,
                    gid: lg,
                });
                Ok((mapped.file_type, mapped.inode_mode, size, blocks, lu, lg))
            })?;

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
            nlinks: self.state.nlinks() as u64,
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
                let path = self.live_path()?;
                with_lwext4_global(|| {
                    let _lock = self.fs.lw.lock();
                    self.validate_path_locked(&path)?;
                    let lw_path = self.fs.lw_path(&path);
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
                })
            }
            FileType::File => {
                // Always use PageCache (lazily created on first I/O)
                let pc = self.ensure_page_cache().ok_or(SyscallErr::EIO)?;
                let file_size = self.logical_size_or_refresh()?;
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
                pc.read_kernel(offset, &mut buf[..read_len])
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
            let path = self.live_path()?;
            let result = if path == "/" {
                self
                    .self_ref
                    .lock()
                    .as_ref()
                    .and_then(|w| w.upgrade())
                    .map(|arc| arc as Arc<dyn IndexNode>)
                    .ok_or(SyscallErr::EIO)
            } else {
                // Compute parent path
                let parent_path = match path.rfind('/') {
                    Some(0) => "/",
                    Some(pos) => &path[..pos],
                    None => "/",
                };
                let parent_path = String::from(parent_path);
                // Metadata/I/O failures must propagate; a synthetic inode 0
                // would still carry a live path and could mutate real data.
                let entry = with_lwext4_global(|| {
                    let _lock = self.fs.lw.lock();
                    self.validate_path_locked(&path)?;
                    self.fs.probe_inode_meta_locked(&parent_path)
                })?;
                Ok(Ext4OSInode::new_child_seeded(
                    self.fs.clone(),
                    parent_path,
                    entry.inode_id,
                    entry.generation,
                    FileType::Dir,
                    entry.inode_mode,
                    entry.size,
                    entry.uid,
                    entry.gid,
                    entry.nlinks,
                ) as Arc<dyn IndexNode>)
            };
            let elapsed = crate::task::perf::perf_time_now().wrapping_sub(_start);
            counters::LWEXT4_FIND_CALLS.fetch_add(1, Ordering::Relaxed);
            counters::LWEXT4_FIND_CYCLES.fetch_add(elapsed, Ordering::Relaxed);
            return result;
        }

        // Build child path
        let parent_path = self.live_path()?;
        let child_path = join_path(&parent_path, name);

        // Validate the snapshotted parent and resolve the child under the
        // same mount lock, so a concurrent directory move cannot redirect
        // this lookup through a stale pathname.
        let entry = with_lwext4_global(|| {
            let _lock = self.fs.lw.lock();
            self.validate_path_locked(&parent_path)?;
            self.fs.probe_inode_meta_locked(&child_path)
        })?;

        let result = Ok(Ext4OSInode::new_child_seeded(
            self.fs.clone(),
            child_path,
            entry.inode_id,
            entry.generation,
            entry.file_type,
            entry.inode_mode,
            entry.size,
            entry.uid,
            entry.gid,
            entry.nlinks,
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

        let path = self.live_path()?;
        with_lwext4_global(|| {
            let _lock = self.fs.lw.lock();
            self.validate_path_locked(&path)?;
            let lw_path = self.fs.lw_path(&path);
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
        })
    }

    // ── list_dirents ──────────────────────────────────────────────────

    fn list_dirents(&self) -> Result<Vec<(String, InodeId, FileType)>, SyscallErr> {
        if self.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        let path = self.live_path()?;
        with_lwext4_global(|| {
            let _lock = self.fs.lw.lock();
            self.validate_path_locked(&path)?;
            let lw_path = self.fs.lw_path(&path);
            let dir = Ext4File::new(&lw_path, InodeTypes::EXT4_DE_DIR);
            let _de_start = crate::task::perf::perf_time_now();
            let (names, types, inode_numbers) = dir.lwext4_dir_entries_with_ino()
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
                        let inode_id = inode_numbers
                            .get(i)
                            .copied()
                            .ok_or(SyscallErr::EIO)? as usize;
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
        })
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
        if self.file_type == FileType::File {
            self.state.open(&self.fs)?;
        }
        Ok(())
    }

    fn close(&self, _data: MutexGuard<FilePrivateData>) -> Result<(), SyscallErr> {
        if self.file_type != FileType::File {
            return Ok(());
        }
        if !self.state.drop_open_ref()? {
            return Ok(());
        }

        // Normal close is not a durability boundary.  A zero-link inode is
        // the exception: its pathname is gone, so dirty pages must reach the
        // still-open inode handle before final block/inode reclamation.
        if self.state.nlinks() == 0 {
            let key = (self.inode_id, self.state.generation());
            let page_cache = self.fs.page_caches.lock().get(&key).cloned();
            if let Some(page_cache) = page_cache {
                page_cache.writeback_all().map_err(|_| SyscallErr::EIO)?;
            }
        }
        let deleted = self.state.finish_last_close(&self.fs)?;
        if deleted {
            let generation = self.state.generation();
            self.fs
                .page_caches
                .lock()
                .remove(&(self.inode_id, generation));
            self.fs.forget_inode_state(self.inode_id, generation);
        }
        Ok(())
    }

    // ── sync / datasync ───────────────────────────────────────────────

    fn sync(&self) -> Result<(), SyscallErr> {
        // Flush VFS PageCache dirty pages to disk first
        if let Some(pc) = self.page_cache.lock().clone() {
            pc.writeback_all().map_err(|_| SyscallErr::EIO)?;
        }
        // Then flush lwext4 internal caches.  Regular files use their inode
        // handle so sync remains valid after namespace detach.
        if self.file_type == FileType::File {
            self.state.with_file(&self.fs, false, |file| {
                file.file_cache_flush()
                    .map_err(|error| from_lwext4(error.abs()))?;
                Ok(())
            })?;
        } else {
            let path = self.live_path()?;
            // C entry point (non-file cache flush): the File branch above goes
            // through `with_file`, which acquires the gate itself, so only this
            // direct path-based flush is gated here.
            with_lwext4_global(|| {
                let _lock = self.fs.lw.lock();
                self.validate_path_locked(&path)?;
                let lw_path = self.fs.lw_path(&path);
                let mut file = Ext4File::new(&lw_path, InodeTypes::EXT4_DE_UNKNOWN);
                file.file_cache_flush()
                    .map_err(|error| from_lwext4(error.abs()))?;
                Ok(())
            })?;
        }
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
                dst.write_from(&kbuf[..n]).map_err(|_| SyscallErr::EFAULT)
            }
            FileType::File => {
                let pc = self.ensure_page_cache().ok_or(SyscallErr::EIO)?;
                let file_size = self.logical_size_or_refresh()?;
                let read_end = (offset + len).min(file_size);
                if offset >= read_end {
                    return Ok(0);
                }
                // PageCache 使用有界 kernel bounce；faultable copy_to_user 不持有
                // PageCache 或 inode 状态锁。
                pc.read_at_user(offset, read_end - offset, dst)
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
        let n = src.read_into(&mut kbuf).map_err(|_| SyscallErr::EFAULT)?;
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
            let key = (self.inode_id, self.state.generation());
            // Fast registry hit.  The backend already references the shared
            // generation-qualified inode state, so aliases must not replace it.
            if let Some(pc) = self.fs.page_caches.lock().get(&key).cloned() {
                *cache = Some(pc.clone());
                return Some(pc);
            }

            // Local inode locks do not serialize hard-link aliases.  Build an
            // unbound candidate, then perform a second lookup while holding
            // the global registry lock.  Only the candidate that wins this
            // get-or-insert race receives a backend and becomes globally
            // visible; losers adopt the already-published PageCache.
            let candidate = PageCache::new();
            let pc = {
                let mut registry = self.fs.page_caches.lock();
                if let Some(existing) = registry.get(&key) {
                    existing.clone()
                } else {
                    let backend = Arc::new(LwExt4PageCacheBackend::new(
                        Arc::downgrade(&self.fs),
                        self.state.clone(),
                    ));
                    candidate.set_backend(backend);
                    registry.insert(key, candidate.clone());
                    counters::LWEXT4_ENSURE_PC_CREATES
                        .fetch_add(1, Ordering::Relaxed);
                    candidate
                }
            };
            *cache = Some(pc);
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
            let result = (|| {
                let old_size = self.logical_size_or_refresh()?;
                // PageCache 自身以 op_gate 排序数据写/截断；逻辑 EOF 仍由此
                // inode 的 shared logical_size 维护。
                let requested_end =
                    offset.checked_add(actual).ok_or(SyscallErr::EFBIG)?;
                let expected_new_end = requested_end.max(old_size);
                self.note_logical_size(expected_new_end);
                let n = match pc.write_kernel(
                    offset,
                    &buf[..actual],
                    old_size,
                ) {
                    Ok(n) => n,
                    Err(error) => {
                        // Roll the speculative EOF back only if no later
                        // writer superseded it inside this serialization
                        // interval.  Discard extension-only dirty pages too.
                        if self
                            .logical_size
                            .compare_exchange(
                                expected_new_end,
                                old_size,
                                Ordering::AcqRel,
                                Ordering::Acquire,
                            )
                            .is_ok()
                        {
                            pc.rollback_failed_extension(old_size);
                        }
                        return Err(error);
                    }
                };
                let actual_new_end =
                    offset.checked_add(n).ok_or(SyscallErr::EFBIG)?;
                let committed_end = actual_new_end.max(old_size);
                if n != actual
                    && self
                        .logical_size
                        .compare_exchange(
                            expected_new_end,
                            committed_end,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                {
                    pc.rollback_failed_extension(committed_end);
                } else {
                    self.note_logical_size(committed_end);
                }
                Ok(n)
            })();
            if result.is_ok() {
                // PageCache 锁已在 write_kernel 返回时释放，允许全局回写选择本 cache。
                crate::fs::page_cache::balance_dirty_pages();
            }
            return result;
        }

        // Non-File types: keep existing behavior (PageCache or direct I/O)
        if let Some(pc) = self.page_cache() {
            let old_size = self.logical_size_or_refresh()?;
            return pc.write_kernel(offset, &buf[..actual], old_size)
                .map_err(|_| SyscallErr::EIO);
        }

        // Direct I/O fallback
        let path = self.live_path()?;
        with_lwext4_global(|| {
            let _lock = self.fs.lw.lock();
            self.validate_path_locked(&path)?;
            let lw_path = self.fs.lw_path(&path);
            let mut f = Ext4File::new(&lw_path, InodeTypes::EXT4_DE_REG_FILE);
            // O_RDWR ("r+"): does not truncate. Fall back to O_RDWR|O_CREAT|O_TRUNC
            // ("w+") only if the file doesn't exist yet.
            let open_result = f.file_open(&lw_path, 0x2);
            if open_result.is_err() {
                f.file_open(&lw_path, 0x242)
                    .map_err(|e| from_lwext4(e.abs()))?;
            }
            // The FileGuard's `file_close()` runs on drop — keep it inside the
            // gate section so the C close call stays serialized.
            let guard = FileGuard::new(&mut f);
            guard.f.file_seek(offset as i64, 0)
                .map_err(|e| from_lwext4(e.abs()))?;
            let n = guard.f.file_write(&buf[..actual])
                .map_err(|e| from_lwext4(e.abs()))?;
            drop(guard);
            Ok(n)
        })
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
        let parent_path = self.live_path()?;
        let child_path = join_path(&parent_path, name);
        match file_type {
            FileType::File => {
                // C entry point: namespace mutation (existence check + create +
                // mode set + inode probe) runs under the process-wide gate.
                // `created` is returned out of the gate; PageCache registry
                // cleanup and inode construction run lock-free afterwards.
                let created = with_lwext4_global(|| {
                    let _lock = self.fs.lw.lock();
                    self.validate_path_locked(&parent_path)?;
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
                    // Use real ext4 inode number so the PageCache stays findable
                    // after rename (the real inode number is stable across renames).
                    self.fs.probe_inode_meta_locked(&child_path)
                })?;
                // inode numbers can be reused after unlink. A new file must not
                // inherit fully-valid pages from a prior inode incarnation.
                self.fs
                    .page_caches
                    .lock()
                    .remove(&(created.inode_id, created.generation));
                let inode = Ext4OSInode::new_child_seeded(
                    self.fs.clone(),
                    child_path,
                    created.inode_id,
                    created.generation,
                    FileType::File,
                    mode,
                    0,
                    0,
                    0,
                    1,
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
        } else if file_type == FileType::Pipe
            || file_type == FileType::CharDevice
            || file_type == FileType::BlockDevice
            || file_type == FileType::Socket
        {
            self.mknod(name, mode, data as u64)
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
        let parent_path = self.live_path()?;
        let child_path = join_path(&parent_path, name);
        let created = with_lwext4_global(|| {
            let _lock = self.fs.lw.lock();
            self.validate_path_locked(&parent_path)?;
            let lw_child = self.fs.lw_path(&child_path);
            let mut d = Ext4File::new(&lw_child, InodeTypes::EXT4_DE_DIR);
            d.dir_mk(&lw_child)
                .map_err(|e| from_lwext4(e.abs()))?;
            // Set mode on the newly created directory
            d.file_mode_set(mode.bits())
                .map_err(|e| from_lwext4(e.abs()))?;
            self.fs.probe_inode_meta_locked(&child_path)
        })?;
        Ok(Ext4OSInode::new_child_seeded(
            self.fs.clone(),
            child_path,
            created.inode_id,
            created.generation,
            FileType::Dir,
            mode,
            0,
            0,
            0,
            2,
        ))
    }

    fn unlink(&self, name: &str) -> Result<(), SyscallErr> {
        if self.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        if name.is_empty() || name.len() > 255 || name.contains('/') {
            return Err(SyscallErr::EINVAL);
        }
        let parent_path = self.live_path()?;
        let child_path = join_path(&parent_path, name);
        let entry = self.fs.probe_inode_meta(&child_path)?;
        if entry.file_type == FileType::Dir {
            return Err(SyscallErr::EISDIR);
        }
        let state = self
            .fs
            .lookup_inode_state(entry.inode_id, entry.generation);
        let observed_open = state.as_ref().is_some_and(|state| state.is_open());

        // A closed inode can still have dirty pages in the strong registry.
        // They must be written before immediate reclaim; an open zero-link
        // inode can keep using the same cache/handle until final close.
        if !observed_open {
            if let Some(page_cache) = self
                .fs
                .page_caches
                .lock()
                .get(&(entry.inode_id, entry.generation))
                .cloned()
            {
                page_cache.writeback_all().map_err(|_| SyscallErr::EIO)?;
            }
        }

        // C entry point: the namespace mutation (revalidation + deferred
        // remove + inode-state path removal) runs under the process-wide
        // gate.  The earlier probe_inode_meta() and dirty-page writeback stay
        // OUTSIDE: writeback enters C through `with_file` and must complete
        // before this gate section re-enters.  remaining_links/defer flags
        // are returned out so registry cleanup runs lock-free afterwards.
        let (remaining_links, defer_inode_free) =
            with_lwext4_global(|| {
                let _lock = self.fs.lw.lock();
                self.validate_path_locked(&parent_path)?;
                let current = self.fs.probe_inode_meta_locked(&child_path)?;
                if current.inode_id != entry.inode_id
                    || current.generation != entry.generation
                    || current.file_type != entry.file_type
                {
                    return Err(SyscallErr::EAGAIN);
                }
                // Re-evaluate at the same mount-lock serialization point as the C
                // namespace mutation.  An open may have started while writeback was
                // waiting, or the former last opener may have completed safely.
                let defer_inode_free = state.as_ref().is_some_and(|state| state.is_open());
                let lw_child = self.fs.lw_path(&child_path);
                let mut f = Ext4File::new(&lw_child, InodeTypes::EXT4_DE_UNKNOWN);
                let (removed_inode, remaining_links) = f
                    .file_remove_deferred(&lw_child, defer_inode_free)
                    .map_err(|error| from_lwext4(error.abs()))?;
                if removed_inode as usize != entry.inode_id {
                    return Err(SyscallErr::EIO);
                }

                if let Some(state) = state.as_ref() {
                    state.remove_path(&child_path, remaining_links as usize);
                }
                Ok((remaining_links, defer_inode_free))
            })?;
        if remaining_links == 0 && !defer_inode_free {
            self.fs
                .page_caches
                .lock()
                .remove(&(entry.inode_id, entry.generation));
            self.fs
                .forget_inode_state(entry.inode_id, entry.generation);
        }
        Ok(())
    }

    fn rmdir(&self, name: &str) -> Result<(), SyscallErr> {
        if self.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        if name.is_empty() || name.len() > 255 || name.contains('/') {
            return Err(SyscallErr::EINVAL);
        }
        let parent_path = self.live_path()?;
        let child_path = join_path(&parent_path, name);
        // Use the C-side atomic, non-recursive primitive.  It distinguishes
        // iterator errors from EOF under the mount lock and will never turn
        // an incomplete directory listing into recursive data deletion.
        with_lwext4_global(|| {
            let _lock = self.fs.lw.lock();
            self.validate_path_locked(&parent_path)?;
            let entry = self.fs.probe_inode_meta_locked(&child_path)?;
            if entry.file_type != FileType::Dir {
                return Err(SyscallErr::ENOTDIR);
            }
            let state = self
                .fs
                .lookup_inode_state(entry.inode_id, entry.generation);
            let lw_child = self.fs.lw_path(&child_path);
            let mut f = Ext4File::new(&lw_child, InodeTypes::EXT4_DE_DIR);
            f.dir_rm_empty(&lw_child)
                .map_err(|e| from_lwext4(e.abs()))?;
            if let Some(state) = state {
                state.remove_path(&child_path, 0);
            }
            self.fs
                .forget_inode_state(entry.inode_id, entry.generation);
            Ok(())
        })
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
        if old_name.is_empty()
            || old_name.len() > 255
            || old_name.contains('/')
            || new_name.is_empty()
            || new_name.len() > 255
            || new_name.contains('/')
        {
            return Err(SyscallErr::EINVAL);
        }
        let old_parent_path = self.live_path()?;
        let old_path = join_path(&old_parent_path, old_name);
        let new_parent_node = new_parent
            .as_any_ref()
            .downcast_ref::<Ext4OSInode>()
            .ok_or(SyscallErr::EXDEV)?;
        // Cross-filesystem rename not supported by lwext4
        if !Arc::ptr_eq(&self.fs, &new_parent_node.fs) {
            return Err(SyscallErr::EXDEV);
        }
        let new_parent_path = new_parent_node.live_path()?;
        let new_path = join_path(&new_parent_path, new_name);

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

        // 2. Probe target inode; only ENOENT means "no replacement target".
        let target_entry = match self.fs.probe_inode_meta(&new_path) {
            Ok(entry) => Some(entry),
            Err(SyscallErr::ENOENT) => None,
            Err(error) => return Err(error),
        };
        if let Some(tgt) = target_entry.as_ref() {
            let target_is_dir = tgt.file_type == FileType::Dir;

            // 2a. RENAME_NOREPLACE — target exists but caller forbids overwrite
            if flags & RENAME_NOREPLACE != 0 {
                return Err(SyscallErr::EEXIST);
            }

            // POSIX rename is a no-op when both names already resolve to the
            // same inode (for example, two hard links to one file).
            if tgt.inode_id == src_entry.inode_id
                && tgt.generation == src_entry.generation
            {
                return Ok(());
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
                let non_empty = with_lwext4_global(|| {
                    let _lock = self.fs.lw.lock();
                    let lw_new = self.fs.lw_path(&new_path);
                    let dir = Ext4File::new(&lw_new, InodeTypes::EXT4_DE_DIR);
                    let (names, _types) = dir
                        .lwext4_dir_entries()
                        .map_err(|e| from_lwext4(e.abs()))?;
                    Ok(names.iter().any(|b| {
                        let len = b.iter().position(|&x| x == 0).unwrap_or(b.len());
                        match core::str::from_utf8(&b[..len]) {
                            Ok(".") | Ok("..") => false,
                            // Empty/invalid records are corruption, not EOF;
                            // fail closed before any destructive operation.
                            Ok("") | Ok(_) | Err(_) => true,
                        }
                    }))
                })?;
                if non_empty {
                    return Err(SyscallErr::ENOTEMPTY);
                }
                // lwext4 has no atomic directory-overwrite primitive and its
                // recursive remover cannot preserve an open target or roll
                // back if publishing the source fails.  Fail closed until
                // the journaled C-side rename transaction is implemented.
                return Err(SyscallErr::EOPNOTSUPP);
            }
            if tgt.file_type != FileType::File {
                return Err(SyscallErr::EOPNOTSUPP);
            }
        }

        // 3. Subtree check: a directory cannot be moved into its own
        //    descendant (e.g. moving /a into /a/b → EINVAL).
        //    Path-based check: if new_parent's path lies inside old_path.
        if src_is_dir {
            let prefix = alloc::format!("{}/", old_path);
            if new_parent_path == old_path
                || new_parent_path.starts_with(&prefix)
            {
                return Err(SyscallErr::EINVAL);
            }
        }

        // ── PageCache coherence: flush dirty source/target pages ──
        let flush_one = |fs: &Arc<super::ext4fs::Ext4FileSystem>,
                         inode_id: usize,
                         generation: u32|
         -> Result<(), SyscallErr> {
            let registry = fs.page_caches.lock();
            if let Some(pc) = registry.get(&(inode_id, generation)).cloned() {
                drop(registry);
                pc.writeback_all().map_err(|_| SyscallErr::EIO)?;
            }
            Ok(())
        };

        flush_one(&self.fs, src_entry.inode_id, src_entry.generation)?;
        if let Some(target) = target_entry.as_ref() {
            flush_one(&self.fs, target.inode_id, target.generation)?;
        }

        // ── Actual rename ──
        // C entry point: the whole mutation transaction (revalidation,
        // detached-target open/remove, ext4_frename, rollback, finalize,
        // inode-state path updates) runs under the process-wide gate.  The
        // flush_one()/writeback above stay OUTSIDE the gate.  target_deleted
        // and post_rename_error are returned out so registry cleanup and
        // error surfacing run lock-free afterwards.
        let (target_deleted, post_rename_error) = with_lwext4_global(|| {
            let _lock = self.fs.lw.lock();
            self.validate_path_locked(&old_parent_path)?;
            new_parent_node.validate_path_locked(&new_parent_path)?;

            // Re-resolve both names at the mutation serialization point.  The
            // earlier probes were deliberately outside this lock so dirty-page
            // writeback could run; no destructive operation is allowed if either
            // pathname changed identity in that interval.
            let current_source = self
                .fs
                .probe_inode_meta_locked(&old_path)
                .map_err(|_| SyscallErr::EAGAIN)?;
            if current_source.inode_id != src_entry.inode_id
                || current_source.generation != src_entry.generation
                || current_source.file_type != src_entry.file_type
            {
                return Err(SyscallErr::EAGAIN);
            }
            let current_target = match self.fs.probe_inode_meta_locked(&new_path) {
                Ok(entry) => Some(entry),
                Err(SyscallErr::ENOENT) => None,
                Err(error) => return Err(error),
            };
            match (target_entry.as_ref(), current_target.as_ref()) {
                (None, None) => {}
                (Some(expected), Some(current))
                    if expected.inode_id == current.inode_id
                        && expected.generation == current.generation
                        && expected.file_type == current.file_type => {}
                _ => return Err(SyscallErr::EAGAIN),
            }

            let lw_old = self.fs.lw_path(&old_path);
            let lw_new = self.fs.lw_path(&new_path);

            let target_state = target_entry
                .as_ref()
                .and_then(|target| {
                    self.fs
                        .lookup_inode_state(target.inode_id, target.generation)
                });
            let mut detached_target: Option<(Ext4File, usize)> = None;
            if let Some(target) = target_entry.as_ref() {
                if target.file_type == FileType::File {
                    // Keep an inode descriptor across namespace replacement.  It
                    // both preserves already-open target fds and provides a
                    // rollback anchor if publishing the source fails.
                    let mut target_file = Ext4File::new(&lw_new, InodeTypes::EXT4_DE_REG_FILE);
                    target_file
                        .file_open(&lw_new, 0x2)
                        .map_err(|error| from_lwext4(error.abs()))?;
                    let mut remover = Ext4File::new(&lw_new, InodeTypes::EXT4_DE_UNKNOWN);
                    let (removed_inode, remaining_links) = remover
                        .file_remove_deferred(&lw_new, true)
                        .map_err(|error| from_lwext4(error.abs()))?;
                    if removed_inode as usize != target.inode_id {
                        target_file.file_close().ok();
                        return Err(SyscallErr::EIO);
                    }
                    detached_target = Some((target_file, remaining_links as usize));
                }
            }

            // ext4_frename handles both files and directories; ext4_dir_mv is an
            // alias of the same C function, so retrying it cannot add correctness.
            let mut f = Ext4File::new(&lw_old, InodeTypes::EXT4_DE_UNKNOWN);
            if let Err(rename_error) = f.file_rename(&lw_old, &lw_new) {
                if let Some((target_file, _)) = detached_target.as_mut() {
                    if let Err(rollback_error) = target_file.file_link_from_handle(&lw_new) {
                        log::error!(
                            "[lwext4] rename rollback failed: rename_errno={} rollback_errno={}",
                            rename_error,
                            rollback_error
                        );
                        target_file.file_close().ok();
                        return Err(SyscallErr::EIO);
                    }
                    target_file.file_close().ok();
                }
                return Err(from_lwext4(rename_error.abs()));
            }

            let mut target_deleted = None;
            let mut post_rename_error = None;
            if let Some((mut target_file, remaining_links)) = detached_target {
                let target_is_still_open = target_state
                    .as_ref()
                    .is_some_and(|state| state.is_open());
                if remaining_links == 0 && !target_is_still_open {
                    match target_file.file_finalize_unlinked() {
                        Ok(_) => {
                            target_deleted = target_entry
                                .as_ref()
                                .map(|target| (target.inode_id, target.generation));
                        }
                        Err(error) => {
                            log::error!(
                                "[lwext4] target finalize failed after rename committed: errno={}",
                                error
                            );
                            post_rename_error = Some(from_lwext4(error.abs()));
                        }
                    }
                }
                if let Err(error) = target_file.file_close() {
                    log::error!(
                        "[lwext4] detached target close failed after rename committed: errno={}",
                        error
                    );
                    if post_rename_error.is_none() {
                        post_rename_error = Some(from_lwext4(error.abs()));
                    }
                }
                if let Some(state) = target_state.as_ref() {
                    state.remove_path(&new_path, remaining_links);
                }
            }
            self.fs.rename_inode_path_prefix(&old_path, &new_path);
            Ok((target_deleted, post_rename_error))
        })?;
        if let Some((inode_id, generation)) = target_deleted {
            self.fs
                .page_caches
                .lock()
                .remove(&(inode_id, generation));
            self.fs.forget_inode_state(inode_id, generation);
        }

        // The namespace move is already committed.  Always publish its new
        // in-memory pathname before surfacing a later reclamation/close error;
        // otherwise callers would retain stale aliases to unrelated inodes.
        if let Some(error) = post_rename_error {
            return Err(error);
        }

        Ok(())
    }

    fn truncate(&self, len: usize) -> Result<(), SyscallErr> {
        if self.file_type == FileType::Dir {
            return Err(SyscallErr::EISDIR);
        }
        // 所有 alias 共享同一 PageCache；truncate 通过 op_gate.write 排空普通
        // 读写/回写，再完成后端截断与 cache prune，避免旧脏页重新扩展文件。
        let pc = self.ensure_page_cache().ok_or(SyscallErr::EIO)?;
        pc.truncate_with_backend(len, || {
            // VFS logical_size may exceed lwext4 on-disk size after extension;
            // use the pinned inode handle so ftruncate survives rename/unlink.
            self.state.with_file(&self.fs, true, |file| {
                file.file_truncate(len as u64)
                    .map_err(|error| from_lwext4(error.abs()))?;
                Ok(())
            })?;
            self.logical_size.store(len, Ordering::Release);
            Ok(())
        })?;
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
        let parent_path = self.live_path()?;
        let child_path = join_path(&parent_path, name);
        // C entry point: namespace mutation (existence check + ext4_fsymlink +
        // inode probe) runs under the process-wide gate.
        let created = with_lwext4_global(|| {
            let _lock = self.fs.lw.lock();
            self.validate_path_locked(&parent_path)?;
            // symlink(2) is create-exclusive.  ext4_fsymlink() otherwise opens
            // and overwrites an existing symlink of the same type.  The lw lock
            // keeps this existence check atomic with the following C operation.
            match self.fs.probe_inode_meta_locked(&child_path) {
                Ok(_) => return Err(SyscallErr::EEXIST),
                Err(SyscallErr::ENOENT) => {}
                Err(error) => return Err(error),
            }
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
            self.fs.probe_inode_meta_locked(&child_path)
        })?;
        Ok(Ext4OSInode::new_child_seeded(
            self.fs.clone(),
            child_path,
            created.inode_id,
            created.generation,
            FileType::SymLink,
            InodeMode::S_IFLNK | InodeMode::from_bits_truncate(0o777),
            target.len(),
            0,
            0,
            1,
        ))
    }

    fn link(&self, name: &str, other: &Arc<dyn IndexNode>) -> Result<(), SyscallErr> {
        if self.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        if name.is_empty() || name.len() > 255 || name.contains('/') {
            return Err(SyscallErr::EINVAL);
        }
        let parent_path = self.live_path()?;
        let new_path = join_path(&parent_path, name);
        let other_node = other
            .as_any_ref()
            .downcast_ref::<Ext4OSInode>()
            .ok_or(SyscallErr::EXDEV)?;
        // Cross-filesystem hard link not supported by lwext4
        if !Arc::ptr_eq(&self.fs, &other_node.fs) {
            return Err(SyscallErr::EXDEV);
        }
        let other_path = other_node.live_path()?;
        // C entry point: namespace mutation (revalidation + ext4_flink +
        // inode probe + path observation) runs under the process-wide gate.
        with_lwext4_global(|| {
            let _lock = self.fs.lw.lock();
            self.validate_path_locked(&parent_path)?;
            let source = self.fs.probe_inode_meta_locked(&other_path)?;
            if source.inode_id != other_node.inode_id
                || source.generation != other_node.state.generation()
            {
                return Err(SyscallErr::EAGAIN);
            }
            let lw_src = self.fs.lw_path(&other_path);
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
            let linked = self.fs.probe_inode_meta_locked(&other_path)?;
            other_node.state.observe_path(
                &new_path,
                linked.size,
                linked.nlinks,
            );
            Ok(())
        })
    }

    fn set_metadata(&self, metadata: &Metadata) -> Result<(), SyscallErr> {
        let timestamps_requested = metadata.atime.tv_sec != 0
            || metadata.mtime.tv_sec != 0
            || metadata.ctime.tv_sec != 0;
        let path = self.live_path()?;
        // C entry point: revalidation + all C setters (mode/owner/times) plus
        // the cached_meta decision run under the process-wide gate.  The cache
        // lock is acquired only after fs.lw and is never held while waiting
        // for fs.lw, matching the cold metadata publication order.
        with_lwext4_global(|| {
            let _lock = self.fs.lw.lock();
            self.validate_path_locked(&path)?;
            let cached = self.state.cached_meta();
            let mode_changed = cached.map_or(true, |value| value.mode != metadata.mode);
            let owner_changed = cached.map_or(true, |value| {
                value.uid != metadata.uid || value.gid != metadata.gid
            });
            if !mode_changed && !owner_changed && !timestamps_requested {
                return Ok(());
            }

            let lw_path = self.fs.lw_path(&path);
            let result = (|| -> Result<(), SyscallErr> {
                if mode_changed {
                    let mut file = Ext4File::new(&lw_path, InodeTypes::EXT4_DE_UNKNOWN);
                    file.file_mode_set(metadata.mode.bits())
                        .map_err(|error| from_lwext4(error.abs()))?;
                }

                let c_path =
                    CString::new(lw_path.as_str()).map_err(|_| SyscallErr::EINVAL)?;
                let raw = c_path.as_ptr();
                if owner_changed {
                    let rc = unsafe {
                        lwext4_rust::bindings::ext4_owner_set(
                            raw,
                            metadata.uid,
                            metadata.gid,
                        )
                    };
                    if rc != 0 {
                        return Err(from_lwext4(rc.abs()));
                    }
                }
                for (timestamp, setter) in [
                    (
                        metadata.atime.tv_sec,
                        lwext4_rust::bindings::ext4_atime_set
                            as unsafe extern "C" fn(*const core::ffi::c_char, u32) -> i32,
                    ),
                    (
                        metadata.mtime.tv_sec,
                        lwext4_rust::bindings::ext4_mtime_set
                            as unsafe extern "C" fn(*const core::ffi::c_char, u32) -> i32,
                    ),
                    (
                        metadata.ctime.tv_sec,
                        lwext4_rust::bindings::ext4_ctime_set
                            as unsafe extern "C" fn(*const core::ffi::c_char, u32) -> i32,
                    ),
                ] {
                    if timestamp != 0 {
                        let rc = unsafe { setter(raw, timestamp as u32) };
                        if rc != 0 {
                            return Err(from_lwext4(rc.abs()));
                        }
                    }
                }
                Ok(())
            })();

            if let Err(error) = result {
                // Some C setters may already have succeeded.  Never retain a
                // speculative permission/owner cache after any later failure.
                self.state.clear_cached_meta();
                return Err(error);
            }
            self.state.set_cached_meta(CachedMeta {
                mode: metadata.mode,
                uid: metadata.uid,
                gid: metadata.gid,
            });
            Ok(())
        })
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

        let parent_path = self.live_path()?;
        let child_path = join_path(&parent_path, filename);

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

        // C entry point: namespace mutation (ext4_mknod + mode set + inode
        // probe) runs under the process-wide gate.
        let created = with_lwext4_global(|| {
            let _lock = self.fs.lw.lock();
            self.validate_path_locked(&parent_path)?;
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

            self.fs.probe_inode_meta_locked(&child_path)
        })?;
        Ok(Ext4OSInode::new_child_seeded(
            self.fs.clone(),
            child_path,
            created.inode_id,
            created.generation,
            file_type,
            mode,
            0,
            0,
            0,
            1,
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
        let path = self.live_path()?;
        with_lwext4_global(|| {
            let _lock = self.fs.lw.lock();
            self.validate_path_locked(&path)?;

            let lw_path = self.fs.lw_path(&path);
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
            // lwext4 copies min(buf_len, value_len) but sets data_size = value_len.
            // When the value is larger than the buffer, return ERANGE per Linux semantics.
            if !buf.is_empty() && data_size > buf.len() {
                return Err(SyscallErr::ERANGE);
            }
            Ok(data_size)
        })
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
        let path = self.live_path()?;
        with_lwext4_global(|| {
            let _lock = self.fs.lw.lock();
            self.validate_path_locked(&path)?;

            let lw_path = self.fs.lw_path(&path);
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
        })
    }

    /// List all extended attribute names (null-separated).
    ///
    /// Wraps lwext4's `ext4_listxattr` FFI:
    ///   ext4_listxattr(path, list, size, ret_size) -> c_int
    ///
    /// If `buf` is empty, returns the total size needed.
    /// Returns `ERANGE` if the buffer is too small.
    fn listxattr(&self, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        let path = self.live_path()?;
        with_lwext4_global(|| {
            let _lock = self.fs.lw.lock();
            self.validate_path_locked(&path)?;

            let lw_path = self.fs.lw_path(&path);
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
        })
    }

    /// Remove an extended attribute.
    ///
    /// Wraps lwext4's `ext4_removexattr` FFI:
    ///   ext4_removexattr(path, name, name_len) -> c_int
    ///
    /// Returns `ENODATA` if the attribute does not exist.
    fn removexattr(&self, name: &str) -> Result<usize, SyscallErr> {
        let path = self.live_path()?;
        with_lwext4_global(|| {
            let _lock = self.fs.lw.lock();
            self.validate_path_locked(&path)?;

            let lw_path = self.fs.lw_path(&path);
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
        })
    }
}

// Strong page_caches registry keeps dirty PageCache alive after drop;
// no synchronous I/O in destructor.
