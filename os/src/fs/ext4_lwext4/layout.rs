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

use super::errno::from_lwext4;
use super::page_cache::LwExt4PageCacheBackend;
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
            }
        })
    }
}

impl IndexNode for Ext4OSInode {
    // ═══════════════════════════════════════════════════════════════════
    //  Phase 3: read-only methods (KEPT)
    // ═══════════════════════════════════════════════════════════════════

    // ── metadata ─────────────────────────────────────────────────────

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        // Inline probe type and mode inside a single lock scope.
        // Do NOT call self.fs.probe_type() which would re-lock self.fs.lw
        // and cause a spin::Mutex deadlock (spin::Mutex is not reentrant).
        let (file_type, inode_mode, size, blocks) = {
            let _lock = self.fs.lw.lock();
            let mut f = Ext4File::new(&self.path, InodeTypes::EXT4_DE_UNKNOWN);
            let mode_raw = f.file_mode_get().map_err(|e| from_lwext4(e.abs()))?;
            let mapped = map_lwext4_mode(mode_raw);

            let (size, blocks) = match mapped.file_type {
                FileType::Dir => (0i64, 0usize),
                FileType::SymLink => {
                    let mut rbuf = [0u8; 256];
                    let mut rcnt: usize = 0;
                    let c_path = CString::new(self.path.as_str())
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
                    let mut ff = Ext4File::new(&self.path, InodeTypes::EXT4_DE_REG_FILE);
                    if ff.file_open(&self.path, 0x0).is_ok() {
                        let s = ff.file_size();
                        ff.file_close().ok();
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
            (mapped.file_type, mapped.inode_mode, size, blocks)
        };

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
            uid: 0,
            gid: 0,
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
                let mut rbuf = [0u8; 256];
                let mut rcnt: usize = 0;
                let c_path = CString::new(self.path.as_str())
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
                // Try PageCache first
                if let Some(pc) = self.page_cache() {
                    return pc.read(offset, &mut buf[..actual])
                        .map_err(|_| SyscallErr::EIO);
                }
                // Direct I/O fallback
                let _lock = self.fs.lw.lock();
                let mut f = Ext4File::new(&self.path, InodeTypes::EXT4_DE_REG_FILE);
                f.file_open(&self.path, 0x0)
                    .map_err(|e| from_lwext4(e.abs()))?;
                let guard = FileGuard::new(&mut f);
                guard.f.file_seek(offset as i64, 0)
                    .map_err(|e| from_lwext4(e.abs()))?;
                let read_bytes = &mut buf[..actual];
                let n = guard.f.file_read(read_bytes)
                    .map_err(|e| from_lwext4(e.abs()))?;
                Ok(n)
            }
            _ => Err(SyscallErr::EINVAL),
        }
    }

    // ── find ─────────────────────────────────────────────────────────

    fn find(&self, name: &str) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        // Validate parent is a directory
        if self.file_type != FileType::Dir && name != "." && name != ".." {
            return Err(SyscallErr::ENOTDIR);
        }
        if name.is_empty() {
            return Err(SyscallErr::ENOENT);
        }
        if name.len() > 255 {
            return Err(SyscallErr::ENAMETOOLONG);
        }
        if name.contains('/') {
            return Err(SyscallErr::EINVAL);
        }

        // "." — return self via Weak upgrade
        if name == "." {
            return self
                .self_ref
                .lock()
                .as_ref()
                .and_then(|w| w.upgrade())
                .map(|arc| arc as Arc<dyn IndexNode>)
                .ok_or(SyscallErr::EIO);
        }

        // ".." — return parent, or self if already at root
        if name == ".." {
            if self.path == "/" {
                return self
                    .self_ref
                    .lock()
                    .as_ref()
                    .and_then(|w| w.upgrade())
                    .map(|arc| arc as Arc<dyn IndexNode>)
                    .ok_or(SyscallErr::EIO);
            }
            // Compute parent path
            let parent_path = match self.path.rfind('/') {
                Some(0) => "/",
                Some(pos) => &self.path[..pos],
                None => "/",
            };
            let parent_path = String::from(parent_path);
            // Get real inode number for parent
            let inode_id = self.fs.get_inode_id(&parent_path)
                .unwrap_or(0);
            return Ok(Ext4OSInode::new_child(
                self.fs.clone(),
                parent_path,
                inode_id,
                FileType::Dir,
            ));
        }

        // Build child path
        let child_path = join_path(&self.path, name);

        // Probe type
        let mapped = self.fs.probe_type(&child_path)?;

        // Get real inode number from ext4_raw_inode_fill
        let inode_id = self.fs.get_inode_id(&child_path)
            .unwrap_or(0);

        Ok(Ext4OSInode::new_child(
            self.fs.clone(),
            child_path,
            inode_id,
            mapped.file_type,
        ))
    }

    // ── list ─────────────────────────────────────────────────────────

    fn list(&self) -> Result<Vec<String>, SyscallErr> {
        if self.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }

        let _lock = self.fs.lw.lock();
        let dir = Ext4File::new(&self.path, InodeTypes::EXT4_DE_DIR);

        let (names, _types) = dir
            .lwext4_dir_entries()
            .map_err(|e| from_lwext4(e.abs()))?;

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
        let dir = Ext4File::new(&self.path, InodeTypes::EXT4_DE_DIR);
        let (names, types) = dir.lwext4_dir_entries()
            .map_err(|e| from_lwext4(e.abs()))?;

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
        let mut f = Ext4File::new(&self.path, InodeTypes::EXT4_DE_UNKNOWN);
        f.file_cache_flush().map_err(|e| from_lwext4(e.abs()))?;
        Ok(())
    }

    fn datasync(&self) -> Result<(), SyscallErr> {
        self.sync()
    }

    // ── user buffer I/O ───────────────────────────────────────────────

    fn read_at_user(
        &self,
        offset: usize,
        len: usize,
        dst: &mut crate::mm::UserBuffer,
    ) -> Result<usize, SyscallErr> {
        let actual_len = len.min(dst.len());
        let mut kbuf = alloc::vec![0u8; actual_len];
        let dummy = spin::Mutex::new(FilePrivateData::Unused);
        let guard = dummy.lock();
        let n = self.read_at(offset, actual_len, &mut kbuf, guard)?;
        dst.write(&kbuf[..n]);
        Ok(n)
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
        let mut cache = self.page_cache.lock();
        if cache.is_none() {
            let backend = Arc::new(LwExt4PageCacheBackend::new(
                Arc::downgrade(&self.fs),
                self.path.clone(),
            ));
            let pc = PageCache::new();
            pc.set_backend(backend);
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

        // Try PageCache first
        if let Some(pc) = self.page_cache() {
            return pc.write(offset, &buf[..actual], None)
                .map_err(|_| SyscallErr::EIO);
        }

        // Direct I/O fallback
        let _lock = self.fs.lw.lock();
        let mut f = Ext4File::new(&self.path, InodeTypes::EXT4_DE_REG_FILE);
        // O_RDWR ("r+"): does not truncate. Fall back to O_RDWR|O_CREAT|O_TRUNC
        // ("w+") only if the file doesn't exist yet.
        let open_result = f.file_open(&self.path, 0x2);
        if open_result.is_err() {
            f.file_open(&self.path, 0x242)
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
        let child_inode_id = self
            .fs
            .get_inode_id(&child_path)
            .unwrap_or_else(|_| hash_path(&child_path));
        match file_type {
            FileType::File => {
                let _lock = self.fs.lw.lock();
                let mut f = Ext4File::new(&child_path, InodeTypes::EXT4_DE_REG_FILE);
                if f.check_inode_exist(&child_path, InodeTypes::EXT4_DE_REG_FILE) {
                    return Err(SyscallErr::EEXIST);
                }
                f.file_open(&child_path, 0x242)
                    .map_err(|e| from_lwext4(e.abs()))?;
                {
                    let mut guard = FileGuard::new(&mut f);
                    drop(guard);
                }
                let _ = f.file_mode_set(mode.bits());
                drop(_lock);
                Ok(Ext4OSInode::new_child(
                    self.fs.clone(),
                    child_path,
                    child_inode_id,
                    FileType::File,
                ))
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
        let inode = self.create(name, file_type, attrs.mode)?;
        let mut meta = inode.metadata()?;
        meta.uid = attrs.uid;
        meta.gid = attrs.gid;
        inode.set_metadata(&meta).ok();
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
        let child_inode_id = self
            .fs
            .get_inode_id(&child_path)
            .unwrap_or_else(|_| hash_path(&child_path));
        let _lock = self.fs.lw.lock();
        let mut d = Ext4File::new(&child_path, InodeTypes::EXT4_DE_DIR);
        d.dir_mk(&child_path)
            .map_err(|e| from_lwext4(e.abs()))?;
        // Set mode on the newly created directory
        let _ = d.file_mode_set(mode.bits());
        drop(_lock);
        Ok(Ext4OSInode::new_child(
            self.fs.clone(),
            child_path,
            child_inode_id,
            FileType::Dir,
        ))
    }

    fn unlink(&self, name: &str) -> Result<(), SyscallErr> {
        if self.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        let child_path = join_path(&self.path, name);
        let _lock = self.fs.lw.lock();
        // Verify the file exists (file_remove tolerates ENOENT in lwext4)
        // Use EXT4_DE_UNKNOWN to accept any non-directory type (files, symlinks, FIFOs, etc.)
        let mut probe = Ext4File::new(&child_path, InodeTypes::EXT4_DE_UNKNOWN);
        if !probe.check_inode_exist(&child_path, InodeTypes::EXT4_DE_UNKNOWN) {
            return Err(SyscallErr::ENOENT);
        }
        let mut f = Ext4File::new(&child_path, InodeTypes::EXT4_DE_UNKNOWN);
        let r = f.file_remove(&child_path);
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
        let _lock = self.fs.lw.lock();
        // Check directory is empty (lwext4 dir_rm is recursive)
        let dir = Ext4File::new(&child_path, InodeTypes::EXT4_DE_DIR);
        let (entries, _) = dir
            .lwext4_dir_entries()
            .map_err(|e| from_lwext4(e.abs()))?;
        let has_children = entries.iter().any(|b| {
            let len = b.iter().position(|&x| x == 0).unwrap_or(b.len());
            let s = core::str::from_utf8(&b[..len]).unwrap_or("");
            s != "." && s != ".." && !s.is_empty()
        });
        if has_children {
            return Err(SyscallErr::ENOTEMPTY);
        }
        let mut f = Ext4File::new(&child_path, InodeTypes::EXT4_DE_DIR);
        f.dir_rm(&child_path)
            .map_err(|e| from_lwext4(e.abs()))?;
        Ok(())
    }

    fn rename(
        &self,
        old_name: &str,
        new_parent: &Arc<dyn IndexNode>,
        new_name: &str,
        _flags: u32,
    ) -> Result<(), SyscallErr> {
        if self.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        let old_path = join_path(&self.path, old_name);
        let new_parent_node = new_parent
            .as_any_ref()
            .downcast_ref::<Ext4OSInode>()
            .ok_or(SyscallErr::EXDEV)?;
        let new_path = join_path(&new_parent_node.path, new_name);
        let _lock = self.fs.lw.lock();
        // Try file_rename first; if it fails (likely because source is a
        // directory), fall back to dir_mv.
        let mut f = Ext4File::new(&old_path, InodeTypes::EXT4_DE_UNKNOWN);
        let r = f.file_rename(&old_path, &new_path);
        if r.is_err() {
            let mut d = Ext4File::new(&old_path, InodeTypes::EXT4_DE_DIR);
            d.dir_mv(&old_path, &new_path)
                .map_err(|e| from_lwext4(e.abs()))?;
        }
        Ok(())
    }

    fn truncate(&self, len: usize) -> Result<(), SyscallErr> {
        if self.file_type == FileType::Dir {
            return Err(SyscallErr::EISDIR);
        }
        let _lock = self.fs.lw.lock();
        let mut f = Ext4File::new(&self.path, InodeTypes::EXT4_DE_REG_FILE);
        f.file_open(&self.path, 0x2)
            .map_err(|e| from_lwext4(e.abs()))?;
        let guard = FileGuard::new(&mut f);
        guard.f.file_truncate(len as u64)
            .map_err(|e| from_lwext4(e.abs()))?;
        drop(guard);
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
        let child_inode_id = self
            .fs
            .get_inode_id(&child_path)
            .unwrap_or_else(|_| hash_path(&child_path));
        let _lock = self.fs.lw.lock();
        // ext4_fsymlink(target, path): target = destination, path = new symlink
        let c_target = CString::new(target).map_err(|_| SyscallErr::EINVAL)?;
        let c_path = CString::new(child_path.as_str()).map_err(|_| SyscallErr::EINVAL)?;
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
        Ok(Ext4OSInode::new_child(
            self.fs.clone(),
            child_path,
            child_inode_id,
            FileType::SymLink,
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
        let _lock = self.fs.lw.lock();
        // ext4_flink(path, hardlink_path): path = source, hardlink_path = new link
        let c_src = CString::new(other_node.path.as_str()).map_err(|_| SyscallErr::EINVAL)?;
        let c_new = CString::new(new_path.as_str()).map_err(|_| SyscallErr::EINVAL)?;
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
        let _lock = self.fs.lw.lock();

        // 1. chmod — fmode_set is a standalone operation (path-based, no open needed)
        let mut f = Ext4File::new(&self.path, InodeTypes::EXT4_DE_UNKNOWN);
        f.file_mode_set(metadata.mode.bits())
            .map_err(|e| from_lwext4(e.abs()))?;

        let c_path = CString::new(self.path.as_str()).map_err(|_| SyscallErr::EINVAL)?;
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
        let child_inode_id = self
            .fs
            .get_inode_id(&child_path)
            .unwrap_or_else(|_| hash_path(&child_path));

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

        let c_path = CString::new(child_path.as_str()).map_err(|_| SyscallErr::EINVAL)?;
        let c_path = c_path.into_raw();
        let r = unsafe {
            lwext4_rust::bindings::ext4_mknod(c_path, lw_type, dev_t as u32)
        };
        unsafe { let _ = CString::from_raw(c_path); }
        if r != 0 {
            return Err(from_lwext4(r.abs()));
        }

        // Set permission bits on the new special file
        let mut f = Ext4File::new(&child_path, InodeTypes::EXT4_DE_UNKNOWN);
        let _ = f.file_mode_set(mode.bits());

        Ok(Ext4OSInode::new_child(
            self.fs.clone(),
            child_path,
            child_inode_id,
            file_type,
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

        let c_path = CString::new(self.path.as_str()).map_err(|_| SyscallErr::EINVAL)?;
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

        let c_path = CString::new(self.path.as_str()).map_err(|_| SyscallErr::EINVAL)?;
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

        let c_path = CString::new(self.path.as_str()).map_err(|_| SyscallErr::EINVAL)?;
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

        let c_path = CString::new(self.path.as_str()).map_err(|_| SyscallErr::EINVAL)?;
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
