//! Path-based `IndexNode` implementation backed by lwext4_rust.
//!
//! Since lwext4_rust operates on path strings rather than inode numbers,
//! each `Ext4OSInode` stores a full path from the mount root.  File
//! operations open the file by path, perform the I/O, and close.
//!
//! This is the read-only Phase 3 implementation.  All write/create/delete
//! methods return ENOSYS.  Phase 4+ will add write support via PageCache.

use alloc::ffi::CString;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::fmt;
use spin::{Mutex, MutexGuard};

use crate::fs::vfs::{
    CreateAttrs, FilePrivateData, FileType, InodeFlags, InodeId,
    InodeMode, IndexNode, Metadata,
};
use crate::fs::vfs::file::FileFlags;
use crate::fs::vfs::file_system::FileSystem;
use crate::timer::TimeSpec;
use crate::utils::error::SyscallErr;

use super::errno::from_lwext4;
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
        let _lock = self.fs.lw.lock();

        // Use probe_type (fmode_get) which works for all file types including dirs
        let mapped = self.fs.probe_type(&self.path)?;

        // Get size and block count based on file type
        let (size, blocks) = match mapped.file_type {
            FileType::Dir => {
                // Directories don't have a meaningful size in ext4
                (0i64, 0usize)
            }
            FileType::SymLink => {
                // Use ext4_readlink to get the symlink target length
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
                // Regular file: open and get size
                let mut f = Ext4File::new(&self.path, InodeTypes::EXT4_DE_REG_FILE);
                let size = match f.file_open(&self.path, 0x0) {
                    Ok(_) => f.file_size(),
                    Err(_) => 0,
                };
                f.file_close().ok();
                let size_i64 = size as i64;
                let blks = if size > 0 {
                    ((size + self.fs.block_size() as u64 - 1) / self.fs.block_size() as u64) as usize
                } else {
                    0
                };
                (size_i64, blks)
            }
        };

        Ok(Metadata {
            dev_id: self.fs.dev_id(),
            inode_id: self.inode_id,
            size,
            blk_size: self.fs.block_size(),
            blocks,
            atime: TimeSpec::new(),
            mtime: TimeSpec::new(),
            ctime: TimeSpec::new(),
            file_type: mapped.file_type,
            mode: mapped.inode_mode,
            flags: InodeFlags::empty(),
            nlinks: if mapped.file_type == FileType::Dir { 2 } else { 1 },
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

        let _lock = self.fs.lw.lock();

        match self.file_type {
            FileType::File => {
                let mut f = Ext4File::new(&self.path, InodeTypes::EXT4_DE_REG_FILE);
                // Open read-only
                f.file_open(&self.path, 0x0)
                    .map_err(|e| from_lwext4(e.abs()))?;
                let guard = FileGuard::new(&mut f);

                // Seek to offset
                guard.f.file_seek(offset as i64, 0)
                    .map_err(|e| from_lwext4(e.abs()))?;

                // Read
                let read_bytes = &mut buf[..actual];
                let n = guard.f.file_read(read_bytes)
                    .map_err(|e| from_lwext4(e.abs()))?;

                Ok(n)
            }
            FileType::SymLink => {
                // Use ext4_readlink to get the symlink target content
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
            FileType::Dir => Err(SyscallErr::EISDIR),
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
                    // Get real ext4 inode number
                    let inode_id = self.fs.get_inode_id(&child_path).unwrap_or(0);
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
        let _lock = self.fs.lw.lock();
        let mut f = Ext4File::new(&self.path, InodeTypes::EXT4_DE_UNKNOWN);
        f.file_cache_flush().map_err(|e| from_lwext4(e.abs()))?;
        Ok(())
    }

    fn datasync(&self) -> Result<(), SyscallErr> {
        self.sync()
    }

    // ── user buffer I/O (stubs for Phase 3) ───────────────────────────

    fn read_at_user(
        &self,
        _offset: usize,
        _len: usize,
        _dst: &mut crate::mm::UserBuffer,
    ) -> Result<usize, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    fn write_at_user(
        &self,
        _offset: usize,
        _len: usize,
        _src: &crate::mm::UserBuffer,
    ) -> Result<usize, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    // ═══════════════════════════════════════════════════════════════════
    //  Phase 4: write/create/delete methods (ALL GATED OUT → ENOSYS)
    // ═══════════════════════════════════════════════════════════════════

    fn write_at(
        &self,
        _offset: usize,
        _len: usize,
        _buf: &[u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    fn create(
        &self,
        _name: &str,
        _file_type: FileType,
        _mode: InodeMode,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    fn create_with_data(
        &self,
        _name: &str,
        _file_type: FileType,
        _mode: InodeMode,
        _data: usize,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    fn create_with_attrs(
        &self,
        _name: &str,
        _file_type: FileType,
        _attrs: CreateAttrs,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    fn mkdir(
        &self,
        _name: &str,
        _mode: InodeMode,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    fn unlink(&self, _name: &str) -> Result<(), SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    fn rmdir(&self, _name: &str) -> Result<(), SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    fn rename(
        &self,
        _old_name: &str,
        _new_parent: &Arc<dyn IndexNode>,
        _new_name: &str,
        _flags: u32,
    ) -> Result<(), SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    fn truncate(&self, _len: usize) -> Result<(), SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    fn resize(&self, _len: usize) -> Result<(), SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    fn symlink(
        &self,
        _name: &str,
        _target: &str,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    fn link(&self, _name: &str, _other: &Arc<dyn IndexNode>) -> Result<(), SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    fn set_metadata(&self, _metadata: &Metadata) -> Result<(), SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }
}
