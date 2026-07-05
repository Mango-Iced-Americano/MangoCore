//! lwext4-backed VFS FileSystem implementation.
//!
//! Wraps lwext4_rust's `Ext4BlockWrapper` to provide a MangoCore
//! `FileSystem` trait implementation.  All lwext4 C calls are
//! serialized through a `Mutex<Ext4BlockWrapper>` because the C
//! library uses global state internally.

use alloc::ffi::CString;
use alloc::sync::Arc;
use alloc::vec;
use core::any::Any;
use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

use crate::drivers::block::BlockDevice;
use crate::fs::vfs::{
    FileSystem, FsInfo, IndexNode, SuperBlock,
};
use crate::fs::vfs::file_system::FsPermissionPolicy;
use crate::utils::error::SyscallErr;

use super::blockdev::{MangoBlockDev, MangoKernelDevOp};
use super::errno::from_lwext4;
use super::layout::Ext4OSInode;
use lwext4_rust::blockdev::Ext4BlockWrapper;
use lwext4_rust::InodeTypes;

/// Per-instance counter for unique filesystem IDs.
static NEXT_FS_ID: AtomicUsize = AtomicUsize::new(1);

/// Back-reference type stored in inodes.
pub type Ext4FsRef = Arc<Ext4FileSystem>;

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
        let size = block_device.size_bytes().unwrap_or(0);

        // 1. Create the device bridge (MangoBlockDev → lwext4 C dev_ops)
        let mbd = MangoBlockDev {
            dev: block_device.clone(),
            pos: 0,
            size,
        };

        // 2. Mount: this calls ext4_device_register, ext4_mount, ext4_recover,
        //    ext4_journal_start internally.
        let lw = Ext4BlockWrapper::<MangoKernelDevOp>::new(mbd)
            .map_err(|e| {
                log::error!("[lwext4] failed to mount ext4 filesystem: errno={}", e);
                from_lwext4(e.abs())
            })?;

        log::info!("[lwext4] Ext4BlockWrapper created, block_size={}", crate::hal::BLOCK_SZ);

        // 3. Build FS struct
        let fs_id = NEXT_FS_ID.fetch_add(1, Ordering::Relaxed);
        let fs_info = FsInfo {
            blk_dev_id: fs_id,
            max_name_len: 255,
            features: vec!["ext4", "lwext4", "extent", "journal"],
        };

        let fs = Arc::new(Self {
            block_device,
            lw: Mutex::new(lw),
            root: Mutex::new(None),
            fs_info,
            block_size: crate::hal::BLOCK_SZ,
            fs_id,
            mounted: AtomicBool::new(true),
        });

        // 4. Create root inode (inode 2 is always root in ext4)
        let root = Ext4OSInode::new_root(fs.clone(), 2);
        *fs.root.lock() = Some(root);

        log::info!("[lwext4] filesystem ready (id={}), root inode created", fs_id);
        Ok(fs)
    }

    /// Unique per-instance device ID.
    pub(crate) fn dev_id(&self) -> usize {
        self.fs_id
    }

    /// Block size in bytes.
    pub(crate) fn block_size(&self) -> usize {
        self.block_size
    }

    /// Get the real ext4 inode number for a path using ext4_raw_inode_fill.
    ///
    /// Returns the ext4 inode number on success, or 0 on failure (with a
    /// logged warning).  Zero is used as a safe fallback since inode 0 is
    /// reserved in ext4.
    pub(crate) fn get_inode_id(&self, full_path: &str) -> Result<usize, SyscallErr> {
        let _lock = self.lw.lock();
        let c_path = CString::new(full_path).map_err(|_| SyscallErr::EINVAL)?;
        let c_path = c_path.into_raw();
        let mut ino: u32 = 0;
        let mut raw_inode: lwext4_rust::bindings::ext4_inode = unsafe { core::mem::zeroed() };
        let r = unsafe {
            lwext4_rust::bindings::ext4_raw_inode_fill(c_path, &mut ino, &mut raw_inode)
        };
        unsafe { let _ = CString::from_raw(c_path); }
        if r != 0 {
            log::warn!("[lwext4] get_inode_id failed for '{}': errno={}", full_path, r);
            return Err(from_lwext4(r.abs()));
        }
        Ok(ino as usize)
    }

    /// Call lwext4 umount during shutdown.  Idempotent — safe to call
    /// multiple times (e.g. explicit `on_umount` + `Drop`).
    fn umount(&self) {
        if !self.mounted.swap(false, Ordering::Relaxed) {
            return; // already unmounted
        }
        if let Some(mut lw) = self.lw.try_lock() {
            let _ = lw.lwext4_umount();
        }
    }

    // ── helper: check file existence & type ────────────────────────────

    /// Check whether a path exists and determine its file type.
    /// Returns `Ok(MappedType)` if it exists, `Err(SyscallErr)` otherwise.
    /// Uses `fmode_get()` which works for all inode types (files, dirs,
    /// symlinks, devices).
    pub(crate) fn probe_type(&self, full_path: &str) -> Result<super::layout::MappedType, SyscallErr> {
        let _lock = self.lw.lock();
        let mut f = lwext4_rust::Ext4File::new(full_path, InodeTypes::EXT4_DE_UNKNOWN);
        let mode = f.file_mode_get().map_err(|e| from_lwext4(e.abs()))?;
        Ok(super::layout::map_lwext4_mode(mode))
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
        let c_mp = CString::new("/").unwrap();
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

    fn on_umount(&self) {
        self.umount();
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}
