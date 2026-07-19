//! Shared runtime state for one on-disk lwext4 inode.
//!
//! lwext4's public namespace helpers are path based, while an open
//! `ext4_file` is an inode handle (`mountpoint + inode number`).  Keeping one
//! handle while VFS files are open is therefore what makes rename/unlink
//! independent from a stale pathname.

use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

use crate::fs::vfs::InodeMode;
use crate::utils::error::SyscallErr;
use lwext4_rust::{Ext4File, InodeTypes};

use super::errno::from_lwext4;
use super::ext4fs::Ext4FileSystem;
use super::page_cache::LWEXT4_SIZE_UNKNOWN;

/// Metadata shared by every VFS alias of one on-disk inode.
///
/// Timestamps are intentionally not cached: callers synthesize or persist
/// them separately.  Keeping mode and ownership on the generation-qualified
/// inode state prevents one hard-link alias from serving stale permissions
/// after another alias changes them.
#[derive(Clone, Copy)]
pub(crate) struct CachedMeta {
    pub(crate) mode: InodeMode,
    pub(crate) uid: u32,
    pub(crate) gid: u32,
}

struct CachedMetaCache {
    value: Option<CachedMeta>,
    /// A lookup result captured before this state was published may seed the
    /// initial cache once.  After any authoritative set or invalidation,
    /// delayed lookup objects must not republish their older snapshot.
    accepts_seed: bool,
}

pub(crate) struct Ext4InodeState {
    inode_id: usize,
    generation: u32,
    /// Every currently-known live alias.  Hard links may give one inode
    /// multiple independent names; removing one must not invalidate VFS
    /// objects that can continue through another alias.
    paths: Mutex<BTreeSet<String>>,
    logical_size: Arc<AtomicUsize>,
    cached_meta: Mutex<CachedMetaCache>,
    nlinks: AtomicUsize,
    open_count: AtomicUsize,
    pending_delete: AtomicBool,
    /// Shared inode handle while at least one distinct VFS `File` is open.
    handle: Mutex<Option<Ext4File>>,
}

// `Ext4File` contains a raw mount-point pointer.  All access is serialized by
// the owning filesystem's `lw` lock and the state-local handle lock, and the
// mount outlives every inode state.
unsafe impl Send for Ext4InodeState {}
unsafe impl Sync for Ext4InodeState {}

impl Ext4InodeState {
    pub(crate) fn new(
        inode_id: usize,
        generation: u32,
        path: String,
        size: usize,
        nlinks: usize,
    ) -> Arc<Self> {
        let mut paths = BTreeSet::new();
        paths.insert(path);
        Arc::new(Self {
            inode_id,
            generation,
            paths: Mutex::new(paths),
            logical_size: Arc::new(AtomicUsize::new(size)),
            cached_meta: Mutex::new(CachedMetaCache {
                value: None,
                accepts_seed: true,
            }),
            nlinks: AtomicUsize::new(nlinks),
            open_count: AtomicUsize::new(0),
            pending_delete: AtomicBool::new(false),
            handle: Mutex::new(None),
        })
    }

    pub(crate) fn inode_id(&self) -> usize {
        self.inode_id
    }

    pub(crate) fn generation(&self) -> u32 {
        self.generation
    }

    pub(crate) fn logical_size(&self) -> Arc<AtomicUsize> {
        self.logical_size.clone()
    }

    pub(crate) fn cached_meta(&self) -> Option<CachedMeta> {
        self.cached_meta.lock().value
    }

    /// Populate lookup metadata without overwriting a newer value already
    /// published by another alias.
    pub(crate) fn seed_cached_meta(&self, metadata: CachedMeta) {
        let mut cached = self.cached_meta.lock();
        if cached.accepts_seed && cached.value.is_none() {
            cached.value = Some(metadata);
            cached.accepts_seed = false;
        }
    }

    pub(crate) fn set_cached_meta(&self, metadata: CachedMeta) {
        let mut cached = self.cached_meta.lock();
        cached.value = Some(metadata);
        cached.accepts_seed = false;
    }

    pub(crate) fn clear_cached_meta(&self) {
        let mut cached = self.cached_meta.lock();
        cached.value = None;
        cached.accepts_seed = false;
    }

    pub(crate) fn current_path(&self) -> Option<String> {
        self.paths.lock().iter().next().cloned()
    }

    /// Record an alias discovered through lookup or successful link creation.
    pub(crate) fn observe_path(&self, path: &str, size: usize, nlinks: usize) {
        self.paths.lock().insert(String::from(path));
        self.nlinks.store(nlinks, Ordering::Release);
        self.pending_delete.store(nlinks == 0, Ordering::Release);
        if self.logical_size.load(Ordering::Relaxed) == LWEXT4_SIZE_UNKNOWN {
            self.logical_size.store(size, Ordering::Relaxed);
        }
    }

    /// Rewrite one known pathname and every known descendant after moving a
    /// directory.  Hard-link aliases outside the moved prefix remain intact.
    pub(crate) fn rename_path_prefix(&self, old_path: &str, new_path: &str) {
        let mut paths = self.paths.lock();
        let replacements: alloc::vec::Vec<_> = paths
            .iter()
            .filter_map(|path| {
                let replacement = if path == old_path {
                    String::from(new_path)
                } else if path.starts_with(old_path)
                    && path.as_bytes().get(old_path.len()) == Some(&b'/')
                {
                    alloc::format!("{}{}", new_path, &path[old_path.len()..])
                } else {
                    return None;
                };
                Some((path.clone(), replacement))
            })
            .collect();
        if !replacements.is_empty() {
            for (old, new) in replacements {
                paths.remove(&old);
                paths.insert(new);
            }
            self.pending_delete.store(false, Ordering::Release);
        }
    }

    pub(crate) fn remove_path(&self, removed_path: &str, remaining_links: usize) {
        let mut paths = self.paths.lock();
        paths.remove(removed_path);
        if remaining_links == 0 {
            paths.clear();
        }
        self.nlinks.store(remaining_links, Ordering::Release);
        self.pending_delete
            .store(remaining_links == 0, Ordering::Release);
    }

    pub(crate) fn nlinks(&self) -> usize {
        self.nlinks.load(Ordering::Acquire)
    }

    pub(crate) fn is_open(&self) -> bool {
        self.handle.lock().is_some()
    }

    /// Pin the inode identity with a read/write lwext4 descriptor.  Opening
    /// with O_RDWR does not mutate the file and lets later writable opens
    /// share the same descriptor; read-only mounts still reject actual writes.
    pub(crate) fn open(&self, fs: &Ext4FileSystem) -> Result<(), SyscallErr> {
        // Publish the in-progress reference before waiting for the mount lock
        // so namespace removal will defer reclaim.  The handle mutex then
        // makes concurrent first-open attempts share the descriptor that
        // whichever opener initializes successfully.
        self.open_count.fetch_add(1, Ordering::AcqRel);
        let path = match self.current_path() {
            Some(path) => path,
            None => {
                self.open_count.fetch_sub(1, Ordering::AcqRel);
                return Err(SyscallErr::ENOENT);
            }
        };
        let lw_path = fs.lw_path(&path);
        let _lw = fs.lw.lock();
        let mut handle = self.handle.lock();
        if handle.is_some() {
            return Ok(());
        }
        let mut file = Ext4File::new(&lw_path, InodeTypes::EXT4_DE_REG_FILE);
        if let Err(error) = file.file_open(&lw_path, 0x2) {
            self.open_count.fetch_sub(1, Ordering::AcqRel);
            return Err(from_lwext4(error.abs()));
        }
        let actual_generation = match file.inode_generation() {
            Ok(generation) => generation,
            Err(error) => {
                file.file_close().ok();
                self.open_count.fetch_sub(1, Ordering::AcqRel);
                return Err(from_lwext4(error.abs()));
            }
        };
        if file.inode_id() as usize != self.inode_id
            || actual_generation != self.generation
        {
            file.file_close().ok();
            self.open_count.fetch_sub(1, Ordering::AcqRel);
            return Err(SyscallErr::EIO);
        }
        *handle = Some(file);
        Ok(())
    }

    /// Run one file operation against the persistent open handle when
    /// available, otherwise open a temporary descriptor through a known live
    /// alias.  No create fallback is allowed: stale paths must never recreate
    /// a renamed or unlinked file.
    pub(crate) fn with_file<T>(
        &self,
        fs: &Ext4FileSystem,
        writable: bool,
        operation: impl FnOnce(&mut Ext4File) -> Result<T, SyscallErr>,
    ) -> Result<T, SyscallErr> {
        let path = self.current_path();
        let _lw = fs.lw.lock();
        let mut handle = self.handle.lock();
        if let Some(file) = handle.as_mut() {
            return operation(file);
        }

        let path = path.ok_or(SyscallErr::ENOENT)?;
        let lw_path = fs.lw_path(&path);
        let mut temporary = Ext4File::new(&lw_path, InodeTypes::EXT4_DE_REG_FILE);
        let flags = if writable { 0x2 } else { 0x0 };
        temporary
            .file_open(&lw_path, flags)
            .map_err(|error| from_lwext4(error.abs()))?;
        let actual_generation = match temporary.inode_generation() {
            Ok(generation) => generation,
            Err(error) => {
                temporary.file_close().ok();
                return Err(from_lwext4(error.abs()));
            }
        };
        if temporary.inode_id() as usize != self.inode_id
            || actual_generation != self.generation
        {
            temporary.file_close().ok();
            return Err(SyscallErr::EIO);
        }
        let result = operation(&mut temporary);
        temporary.file_close().ok();
        result
    }

    /// Drop one non-final VFS open reference.  A final reference remains
    /// visible as open until `finish_last_close()` reaches the serialization
    /// point under the lwext4 mount lock.  This prevents unlink/rename from
    /// reclaiming the inode while final-close writeback is still in flight.
    pub(crate) fn drop_open_ref(&self) -> Result<bool, SyscallErr> {
        loop {
            let count = self.open_count.load(Ordering::Acquire);
            if count == 0 {
                return Err(SyscallErr::EIO);
            }
            if count == 1 {
                return Ok(true);
            }
            if self
                .open_count
                .compare_exchange_weak(
                    count,
                    count - 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return Ok(false);
            }
        }
    }

    /// Close the persistent descriptor and, for a zero-link inode, finish
    /// truncation/bitmap release only after dirty pages have been written.
    /// Returns whether the inode was finally deleted.
    pub(crate) fn finish_last_close(
        &self,
        fs: &Ext4FileSystem,
    ) -> Result<bool, SyscallErr> {
        let _lw = fs.lw.lock();
        let mut handle = self.handle.lock();

        // A new open may have arrived while the prospective final closer was
        // writing dirty pages.  In that case this close is no longer final;
        // release only its own reference and leave the shared handle pinned.
        match self.open_count.compare_exchange(
            1,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {}
            Err(count) if count > 1 => {
                self.open_count.fetch_sub(1, Ordering::AcqRel);
                return Ok(false);
            }
            Err(_) => return Err(SyscallErr::EIO),
        }

        let file = match handle.as_mut() {
            Some(file) => file,
            None => {
                self.open_count.fetch_add(1, Ordering::AcqRel);
                return Err(SyscallErr::EIO);
            }
        };
        let deleted = self.pending_delete.load(Ordering::Acquire)
            && self.nlinks.load(Ordering::Acquire) == 0;
        if deleted {
            if let Err(error) = file.file_finalize_unlinked() {
                self.open_count.fetch_add(1, Ordering::AcqRel);
                return Err(from_lwext4(error.abs()));
            }
        }
        if let Err(error) = file.file_close() {
            self.open_count.fetch_add(1, Ordering::AcqRel);
            return Err(from_lwext4(error.abs()));
        }
        *handle = None;
        Ok(deleted)
    }
}
