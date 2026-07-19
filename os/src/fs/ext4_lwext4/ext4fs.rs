//! lwext4-backed VFS FileSystem implementation.
//!
//! Wraps lwext4_rust's `Ext4BlockWrapper` to provide a MangoCore
//! `FileSystem` trait implementation.  All lwext4 C calls are
//! serialized through a `Mutex<Ext4BlockWrapper>` because the C
//! library uses global state internally.

use alloc::collections::BTreeMap;
use alloc::ffi::CString;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec;
use core::any::Any;
use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

use crate::drivers::block::BlockDevice;
use crate::fs::vfs::{
    FileSystem, FsInfo, FileType, IndexNode, InodeMode, SuperBlock,
};
use crate::fs::vfs::file_system::FsPermissionPolicy;
use crate::utils::error::SyscallErr;

use super::blockdev::{MangoBlockDev, MangoKernelDevOp};
use super::errno::from_lwext4;
use super::inode_state::Ext4InodeState;
use super::layout::Ext4OSInode;
use lwext4_rust::blockdev::Ext4BlockWrapper;
use lwext4_rust::InodeTypes;

/// Per-instance counter for unique filesystem IDs.
static NEXT_FS_ID: AtomicUsize = AtomicUsize::new(1);

/// Back-reference type stored in inodes.
pub type Ext4FsRef = Arc<Ext4FileSystem>;

/// Cached result of a single `ext4_raw_inode_fill()` probe.
#[derive(Debug)]
pub(crate) struct LookupCacheEntry {
    pub inode_id: usize,
    pub generation: u32,
    pub file_type: FileType,
    pub inode_mode: InodeMode,
    pub size: usize,
    pub uid: u32,
    pub gid: u32,
    pub nlinks: usize,
}

/// lwext4-based ext4 filesystem.
///
/// Holds:
/// - the underlying block device (for reference)
/// - a `Mutex<Ext4BlockWrapper>` serializing all lwext4 C calls
/// - a cached root inode
pub struct Ext4FileSystem {
    /// Underlying block device (kept alive while mounted).
    #[allow(dead_code)]
    block_device: Arc<dyn BlockDevice>,
    /// Wrapped lwext4 instance.  Mutex because lwext4 C code is not reentrant.
    pub(crate) lw: Mutex<Ext4BlockWrapper<MangoKernelDevOp>>,
    /// Root inode (cached after first access).
    root: Mutex<Option<Arc<Ext4OSInode>>>,
    /// Filesystem info from superblock / probing.
    fs_info: FsInfo,
    /// Block size in bytes (always 4096 for MangoCore).
    block_size: usize,
    /// Unique per-instance device ID.
    fs_id: usize,
    /// Whether the filesystem is currently mounted (for idempotent umount).
    mounted: AtomicBool,
    /// Unique lwext4-internal mount point, e.g. "/e1/", "/e2/".
    /// Generated from the atomic counter at mount time.  All VFS paths
    /// passed to lwext4 C API are prefixed with this via `lw_path()`.
    lw_mount_point: String,
    /// PageCache registry keyed by (inode_id, generation) — shares PageCache
    /// across aliases without crossing an inode-number reuse boundary.
    /// Strong Arc registry — keeps dirty PageCache alive after last
    /// inode reference is dropped (dentry eviction).  Without this,
    /// dirty pages are lost when dentry cache pressure evicts inodes.
    pub(crate) page_caches:
        Mutex<BTreeMap<(usize, u32), Arc<crate::fs::page_cache::PageCache>>>,
    /// Weak runtime-state registry keyed by ext4 inode number + generation.
    /// All path aliases and independently-created VFS inode objects share
    /// the same open handle, pathname updates, link count, and logical EOF.
    pub(crate) inode_states: Mutex<BTreeMap<(usize, u32), Weak<Ext4InodeState>>>,
}

// Safety: MangoCore is single-core; lwext4 C global state is only accessed
// from this context.  The Mutex serialises access but `Send`/`Sync` are
// required by `Arc<dyn FileSystem>`.
unsafe impl Send for Ext4FileSystem {}
unsafe impl Sync for Ext4FileSystem {}

impl fmt::Debug for Ext4FileSystem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ext4FileSystem")
            .field("fs_id", &self.fs_id)
            .field("block_size", &self.block_size)
            .finish()
    }
}

impl Ext4FileSystem {
    /// Open and mount an ext4 filesystem on the given block device via lwext4.
    ///
    /// Returns `Err(SyscallErr)` on mount failure instead of panicking.
    pub fn open_ext4rs(block_device: Arc<dyn BlockDevice>) -> Result<Arc<Self>, SyscallErr> {
        Self::open_ext4rs_with_options(block_device, false)
    }

    /// Open and mount an ext4 filesystem with an explicit access mode.
    ///
    /// `read_only` is passed through to lwext4 so it cannot perform journal
    /// recovery writes while mounting an `MS_RDONLY` device.
    pub fn open_ext4rs_with_options(
        block_device: Arc<dyn BlockDevice>,
        read_only: bool,
    ) -> Result<Arc<Self>, SyscallErr> {
        const EXT4_FEATURE_INCOMPAT_RECOVER: u32 = 0x0004;

        if read_only {
            if let Some(identity) = crate::fs::filesystem::ext4_identity(&block_device) {
                if identity.incompatible_features & EXT4_FEATURE_INCOMPAT_RECOVER != 0 {
                    log::error!(
                        "[lwext4][ro] refusing filesystem that requires journal recovery"
                    );
                    return Err(SyscallErr::EROFS);
                }
            }
        }

        let size = block_device.size_bytes().unwrap_or(0);

        // 1. Create the device bridge (MangoBlockDev → lwext4 C dev_ops)
        let mbd = MangoBlockDev {
            dev: block_device.clone(),
            pos: 0,
            size,
            read_only,
            blocked_writes: 0,
        };

        // 2. Mount: this calls ext4_device_register, ext4_mount, ext4_recover,
        //    ext4_journal_start internally.
        //    Use unique device name AND unique mount point so that multiple ext4
        //    filesystems can coexist without path collisions in lwext4's internal
        //    mount table.  VFS paths are prefixed with the mount point via
        //    `lw_path()` before being passed to any lwext4 C API.
        let fs_id = NEXT_FS_ID.fetch_add(1, Ordering::Relaxed);
        let dev_name = alloc::format!("e{}", fs_id);
        let mount_point = alloc::format!("/e{}/", fs_id);
        let lw = Ext4BlockWrapper::<MangoKernelDevOp>::new_with_names_and_read_only(
            mbd,
            &dev_name,
            &mount_point,
            read_only,
        )
        .map_err(|e| {
            log::error!(
                "[lwext4] failed to mount ext4 filesystem (id={}): errno={}",
                fs_id,
                e
            );
            from_lwext4(e.saturating_abs())
        })?;

        log::info!(
            "[lwext4] Ext4BlockWrapper created, block_size={}, read_only={}",
            crate::hal::BLOCK_SZ,
            read_only
        );

        // 3. Build FS struct
        let fs_info = FsInfo {
            blk_dev_id: fs_id,
            max_name_len: 255,
            features: vec!["ext4", "lwext4", "extent"],
        };

        let fs = Arc::new(Self {
            block_device,
            lw: Mutex::new(lw),
            root: Mutex::new(None),
            fs_info,
            block_size: crate::hal::BLOCK_SZ,
            fs_id,
            mounted: AtomicBool::new(true),
            lw_mount_point: mount_point,
            page_caches: Mutex::new(BTreeMap::new()),
            inode_states: Mutex::new(BTreeMap::new()),
        });

        // 4. Create root inode (inode 2 is always root in ext4).
        let root_meta = fs.probe_inode_meta("/")?;
        let root = Ext4OSInode::new_root(
            fs.clone(),
            root_meta.inode_id,
            root_meta.generation,
            root_meta.nlinks,
        );
        *fs.root.lock() = Some(root);

        log::info!("[lwext4] filesystem ready (id={}), root inode created", fs_id);
        Ok(fs)
    }

    /// Translate a VFS path (e.g. "/bin/sh") into the lwext4-internal path
    /// using this instance's unique mount point (e.g. "/e1/bin/sh").
    ///
    /// The root VFS path "/" maps to the mount point with trailing slash
    /// (e.g. "/e1/"), which is what lwext4 expects for directory operations.
    pub(crate) fn lw_path(&self, vfs_path: &str) -> String {
        if vfs_path == "/" {
            return self.lw_mount_point.clone();
        }
        alloc::format!("{}{}", self.lw_mount_point, &vfs_path[1..])
    }

    /// Unique per-instance device ID.
    pub(crate) fn dev_id(&self) -> usize {
        self.fs_id
    }

    /// Block size in bytes.
    pub(crate) fn block_size(&self) -> usize {
        self.block_size
    }

    pub(crate) fn inode_state(
        &self,
        inode_id: usize,
        generation: u32,
        path: &str,
        size: usize,
        nlinks: usize,
    ) -> Arc<Ext4InodeState> {
        let mut states = self.inode_states.lock();
        let key = (inode_id, generation);
        if let Some(state) = states.get(&key).and_then(Weak::upgrade) {
            state.observe_path(path, size, nlinks);
            return state;
        }
        let state = Ext4InodeState::new(
            inode_id,
            generation,
            String::from(path),
            size,
            nlinks,
        );
        states.insert(key, Arc::downgrade(&state));
        state
    }

    pub(crate) fn lookup_inode_state(
        &self,
        inode_id: usize,
        generation: u32,
    ) -> Option<Arc<Ext4InodeState>> {
        self.inode_states
            .lock()
            .get(&(inode_id, generation))
            .and_then(Weak::upgrade)
    }

    pub(crate) fn forget_inode_state(&self, inode_id: usize, generation: u32) {
        self.inode_states.lock().remove(&(inode_id, generation));
    }

    /// Update every live inode-state pathname affected by a namespace move.
    /// This includes already-open directory objects and cached descendants.
    pub(crate) fn rename_inode_path_prefix(&self, old_path: &str, new_path: &str) {
        let states: alloc::vec::Vec<_> = self
            .inode_states
            .lock()
            .values()
            .filter_map(Weak::upgrade)
            .collect();
        for state in states {
            state.rename_path_prefix(old_path, new_path);
        }
    }

    /// Get the real ext4 inode number for a path using ext4_raw_inode_fill.
    ///
    /// Returns the ext4 inode number on success, or 0 on failure (with a
    /// logged warning).  Zero is used as a safe fallback since inode 0 is
    /// reserved in ext4.
    pub(crate) fn get_inode_id(&self, full_path: &str) -> Result<usize, SyscallErr> {
        let _start = crate::task::perf::perf_time_now();
        let _lock = self.lw.lock();
        let lw_path = self.lw_path(full_path);
        let c_path = CString::new(lw_path).map_err(|_| SyscallErr::EINVAL)?;
        let c_path = c_path.into_raw();
        let mut ino: u32 = 0;
        let mut raw_inode: lwext4_rust::bindings::ext4_inode = unsafe { core::mem::zeroed() };
        let r = unsafe {
            lwext4_rust::bindings::ext4_raw_inode_fill(c_path, &mut ino, &mut raw_inode)
        };
        unsafe { let _ = CString::from_raw(c_path); }
        let elapsed = crate::task::perf::perf_time_now().wrapping_sub(_start);
        super::counters::LWEXT4_GET_INODE_ID_CALLS.fetch_add(1, Ordering::Relaxed);
        super::counters::LWEXT4_GET_INODE_ID_CYCLES.fetch_add(elapsed, Ordering::Relaxed);
        if r != 0 {
            // Only log at debug level — ENOENT is expected during create/mkdir
            // pre-checks and would spam serial at warn/error.
            log::debug!("[lwext4] get_inode_id failed for '{}': errno={}", full_path, r);
            super::counters::LWEXT4_GET_INODE_ID_ENOENT.fetch_add(1, Ordering::Relaxed);
            return Err(from_lwext4(r.abs()));
        }
        Ok(ino as usize)
    }

    /// Call lwext4 umount during shutdown.  Idempotent — safe to retry after
    /// a partial lower-level teardown or call again after success.
    fn umount(&self) -> Result<(), SyscallErr> {
        if !self.mounted.load(Ordering::Acquire) {
            return Ok(()); // already unmounted
        }
        let mut lw = self.lw.lock();
        match lw.lwext4_umount() {
            Ok(_) => {
                self.mounted.store(false, Ordering::Release);
                Ok(())
            }
            Err(error) => {
                let mapped = from_lwext4(error.saturating_abs());
                log::error!(
                    "[lwext4] umount failed: raw errno={}, mapped={:?}",
                    error,
                    mapped
                );
                Err(mapped)
            }
        }
    }

    // ── helper: check file existence & type ────────────────────────────

    /// Check whether a path exists and determine its file type.
    /// Returns `Ok(MappedType)` if it exists, `Err(SyscallErr)` otherwise.
    /// Uses `fmode_get()` which works for all inode types (files, dirs,
    /// symlinks, devices).
    pub(crate) fn probe_type(&self, full_path: &str) -> Result<super::layout::MappedType, SyscallErr> {
        let _start = crate::task::perf::perf_time_now();
        let _lock = self.lw.lock();
        let lw_path = self.lw_path(full_path);
        let mut f = lwext4_rust::Ext4File::new(&lw_path, InodeTypes::EXT4_DE_UNKNOWN);
        let mode = f.file_mode_get().map_err(|e| from_lwext4(e.abs()))?;
        let elapsed = crate::task::perf::perf_time_now().wrapping_sub(_start);
        super::counters::LWEXT4_PROBE_TYPE_CALLS.fetch_add(1, Ordering::Relaxed);
        super::counters::LWEXT4_PROBE_TYPE_CYCLES.fetch_add(elapsed, Ordering::Relaxed);
        Ok(super::layout::map_lwext4_mode(mode))
    }

    /// Single lwext4 FFI: fill raw inode, extract ALL metadata, cache result.
    /// Uses `ext4_raw_inode_fill(path, &ret_ino, &raw_inode)` — ONE call.
    pub(crate) fn probe_inode_meta(
        &self,
        path: &str,
    ) -> Result<LookupCacheEntry, SyscallErr> {
        let _lock = self.lw.lock();
        self.probe_inode_meta_locked(path)
    }

    /// Variant for callers that already hold `self.lw` across validation and
    /// a following namespace operation.
    pub(crate) fn probe_inode_meta_locked(
        &self,
        path: &str,
    ) -> Result<LookupCacheEntry, SyscallErr> {
        let lw_path = self.lw_path(path);
        let c_path =
            CString::new(lw_path.as_str()).map_err(|_| SyscallErr::EINVAL)?;
        let c_path = c_path.into_raw();
        let mut ret_ino: u32 = 0;
        let mut raw_inode: lwext4_rust::bindings::ext4_inode =
            unsafe { core::mem::zeroed() };
        let r = unsafe {
            lwext4_rust::bindings::ext4_raw_inode_fill(
                c_path,
                &mut ret_ino,
                &mut raw_inode,
            )
        };
        unsafe {
            let _ = CString::from_raw(c_path);
        }
        if r != 0 {
            return Err(from_lwext4(r.abs()));
        }
        let mode_raw = raw_inode.mode as u32;
        let mapped = super::layout::map_lwext4_mode(mode_raw);
        let size = (raw_inode.size_lo as usize)
            | ((raw_inode.size_hi as usize) << 32);
        let uid = raw_inode.uid as u32
            | unsafe { ((raw_inode.osd2.linux2.uid_high as u32) << 16) };
        let gid = raw_inode.gid as u32
            | unsafe { ((raw_inode.osd2.linux2.gid_high as u32) << 16) };
        let entry = LookupCacheEntry {
            inode_id: ret_ino as usize,
            generation: raw_inode.generation,
            file_type: mapped.file_type,
            inode_mode: mapped.inode_mode,
            size,
            uid,
            gid,
            nlinks: raw_inode.links_count as usize,
        };
        Ok(entry)
    }
}

impl FileSystem for Ext4FileSystem {
    fn root_inode(&self) -> Arc<dyn IndexNode> {
        self.root
            .lock()
            .as_ref()
            .expect("Ext4FileSystem: root inode not initialized")
            .clone()
    }

    fn info(&self) -> FsInfo {
        FsInfo {
            blk_dev_id: self.fs_info.blk_dev_id,
            max_name_len: self.fs_info.max_name_len,
            features: self.fs_info.features.clone(),
        }
    }

    fn name(&self) -> &str {
        "ext4"
    }

    fn super_block(&self) -> SuperBlock {
        // Read actual filesystem stats from lwext4 via ext4_mount_point_stats.
        let _lock = self.lw.lock();
        let mut stats: lwext4_rust::bindings::ext4_mount_stats =
            unsafe { core::mem::zeroed() };
        let c_mp = CString::new(self.lw_mount_point.as_str()).unwrap();
        let c_mp = c_mp.into_raw();
        unsafe {
            lwext4_rust::bindings::ext4_mount_point_stats(c_mp, &mut stats);
        }
        unsafe { let _ = CString::from_raw(c_mp); }

        SuperBlock {
            f_type: 0xEF53,
            f_bsize: stats.block_size as u64,
            f_blocks: stats.blocks_count,
            f_bfree: stats.free_blocks_count,
            f_bavail: stats.free_blocks_count,
            f_files: stats.inodes_count as u64,
            f_ffree: stats.free_inodes_count as u64,
            f_fsid: [0; 2],
            f_namelen: 255,
            f_frsize: stats.block_size as u64,
            flags: 0,
            f_spare: [0; 4],
        }
    }

    fn statfs(&self, _inode: &Arc<dyn IndexNode>) -> Result<SuperBlock, SyscallErr> {
        Ok(self.super_block())
    }

    fn support_readahead(&self) -> bool {
        // lwext4 has its own internal caching; skip VFS readahead for now
        false
    }

    fn permission_policy(&self) -> FsPermissionPolicy {
        FsPermissionPolicy::Dac
    }

    fn on_umount(&self) -> Result<(), SyscallErr> {
        // The registry intentionally holds dirty PageCaches after dentry/file
        // eviction.  Drain this filesystem's caches before stopping lwext4;
        // never hold the registry lock across block I/O.
        let caches: alloc::vec::Vec<_> =
            self.page_caches.lock().values().cloned().collect();
        for cache in caches {
            if let Err(error) = cache.writeback_all() {
                log::error!(
                    "[lwext4] refusing umount after PageCache writeback failure: {:?}",
                    error
                );
                return Err(error);
            }
        }
        // lwext4_umount first disables its internal write-back cache, then
        // stops the journal and detaches the block device.  Preserve all VFS
        // ownership until that transaction succeeds so a failed drain remains
        // retryable and cannot leave dangling C registrations.
        self.umount()?;

        // Ext4OSInode owns a strong fs Arc, while the filesystem caches the
        // root inode.  Break that fs → root → fs cycle only after lwext4 is
        // fully detached.  Move registry contents out under their locks and
        // drop them afterwards, avoiding destructor work while locks are held.
        let root = self.root.lock().take();
        let page_caches = core::mem::take(&mut *self.page_caches.lock());
        let inode_states = core::mem::take(&mut *self.inode_states.lock());
        drop(page_caches);
        drop(inode_states);
        drop(root);
        Ok(())
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}
