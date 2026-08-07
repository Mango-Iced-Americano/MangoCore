use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::convert::TryFrom;
use core::fmt;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use crate::drivers::block::BlockDevice;
use crate::fs::vfs::{FileSystem, FsInfo, IndexNode, SuperBlock};
use crate::utils::error::SyscallErr;

use super::blockdev::MangoBlockDevice;
use super::errno::from_another;
use super::inode::Ext4Inode;
use super::lifetime::{CachedTimestamps, InodeKey, InodeLifetime};

static NEXT_FS_ID: AtomicUsize = AtomicUsize::new(1);

/// Live writable another_ext4 instances for global `sync(2)`.
pub(crate) static EXT4_REGISTRY: Mutex<Vec<Weak<Ext4FileSystem>>> = Mutex::new(Vec::new());

/// One writable another_ext4 filesystem instance.
pub struct Ext4FileSystem {
    ext4: Arc<another_ext4::Ext4>,
    fs_id: usize,
    read_only: bool,
    root: Mutex<Option<Arc<Ext4Inode>>>,
    /// Per-filesystem records keyed by `(filesystem, inode, generation)`.
    pub(crate) lifetimes: Mutex<alloc::collections::BTreeMap<InodeKey, Arc<InodeLifetime>>>,
}

impl fmt::Debug for Ext4FileSystem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnotherExt4FileSystem")
            .field("fs_id", &self.fs_id)
            .finish()
    }
}

impl Ext4FileSystem {
    /// Opens a writable filesystem only when its persistence barrier is reliable.
    pub fn open(block_device: Arc<dyn BlockDevice>) -> Result<Arc<Self>, SyscallErr> {
        Self::open_with_options(block_device, false)
    }

    /// Open an ext4 filesystem with the mount's requested mutability.
    ///
    /// Read-only mounts use another_ext4's checked loader, which refuses dirty
    /// journals/orphan state instead of attempting recovery through a blocked
    /// device wrapper. Writable mounts require a reliable persistence barrier.
    pub fn open_with_options(
        block_device: Arc<dyn BlockDevice>,
        read_only: bool,
    ) -> Result<Arc<Self>, SyscallErr> {
        if !read_only && !block_device.supports_reliable_flush() {
            return Err(SyscallErr::EROFS);
        }
        let device = Arc::new(MangoBlockDevice::new(block_device));
        let ext4 = if read_only {
            another_ext4::Ext4::load_read_only_checked(device)
        } else {
            another_ext4::Ext4::load_writable(device)
        }
        .map_err(|error| from_another(error.code()))?;
        let fs = Arc::new(Self {
            ext4: Arc::new(ext4),
            fs_id: NEXT_FS_ID.fetch_add(1, Ordering::Relaxed),
            read_only,
            root: Mutex::new(None),
            lifetimes: Mutex::new(alloc::collections::BTreeMap::new()),
        });
        let root = Ext4Inode::new_root(&fs, another_ext4::EXT4_ROOT_INO)?;
        *fs.root.lock() = Some(root);
        if !read_only {
            EXT4_REGISTRY.lock().push(Arc::downgrade(&fs));
        }
        Ok(fs)
    }

    pub(crate) fn inner(&self) -> &another_ext4::Ext4 {
        &self.ext4
    }

    pub(crate) const fn fs_id(&self) -> usize {
        self.fs_id
    }

    /// Flush all Mango-owned regular-file data before the final device barrier.
    pub(crate) fn sync_all(&self) -> Result<(), SyscallErr> {
        if self.read_only {
            return Ok(());
        }
        self.sync_lifetimes()
    }

    pub(crate) fn flush_device(&self) -> Result<(), SyscallErr> {
        if self.read_only {
            return Ok(());
        }
        self.inner()
            .flush_device()
            .map_err(|error| from_another(error.code()))
    }

    pub(crate) fn commit_lifetime_timestamps(
        &self,
        inode_id: u32,
        lifetime: &InodeLifetime,
    ) -> Result<Option<CachedTimestamps>, SyscallErr> {
        let Some(timestamps) = lifetime.dirty_timestamps() else {
            return Ok(None);
        };
        self.inner()
            .setattr(
                inode_id,
                another_ext4::SetAttr {
                    mtime: Some(
                        u32::try_from(timestamps.mtime().tv_sec)
                            .map_err(|_| SyscallErr::EFBIG)?,
                    ),
                    ctime: Some(
                        u32::try_from(timestamps.ctime().tv_sec)
                            .map_err(|_| SyscallErr::EFBIG)?,
                    ),
                    ..Default::default()
                },
            )
            .map_err(|error| from_another(error.code()))?;
        Ok(Some(timestamps))
    }
}

/// Sync every live another_ext4 instance and report the first persistence error.
pub(crate) fn sync_all_instances() -> Result<(), SyscallErr> {
    let live = {
        let mut registry = EXT4_REGISTRY.lock();
        let live: Vec<Arc<Ext4FileSystem>> = registry.iter().filter_map(Weak::upgrade).collect();
        registry.retain(|weak| weak.strong_count() > 0);
        live
    };

    let mut first_error = None;
    for fs in live {
        if let Err(error) = fs.sync_all() {
            log::error!(
                "another_ext4: global sync failed for filesystem {}: {:?}",
                fs.fs_id(),
                error
            );
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Shutdown every live another_ext4 instance: sync data, then clear
/// FEATURE_INCOMPAT_RECOVER so the next boot does not see a dirty journal.
pub(crate) fn shutdown_all_instances() {
    let live = {
        let mut registry = EXT4_REGISTRY.lock();
        let live: Vec<Arc<Ext4FileSystem>> = registry.iter().filter_map(Weak::upgrade).collect();
        registry.retain(|weak| weak.strong_count() > 0);
        live
    };

    for fs in live {
        if let Err(error) = fs.sync_all() {
            log::error!(
                "another_ext4: sync before shutdown failed for filesystem {}: {:?}",
                fs.fs_id(),
                error
            );
            // Do NOT clear RECOVER if sync failed — data may be incomplete
            continue;
        }
        if let Err(error) = fs.inner().shutdown_writable() {
            log::error!(
                "another_ext4: shutdown_writable failed for filesystem {}: {:?}",
                fs.fs_id(),
                error
            );
        }
    }
}

impl FileSystem for Ext4FileSystem {
    fn root_inode(&self) -> Arc<dyn IndexNode> {
        self.root
            .lock()
            .as_ref()
            .map_or_else(|| unreachable!(), Clone::clone)
    }

    fn info(&self) -> FsInfo {
        FsInfo {
            blk_dev_id: self.fs_id,
            max_name_len: 255,
            features: if self.read_only {
                alloc::vec!["ext4", "another_ext4", "readonly"]
            } else {
                alloc::vec!["ext4", "another_ext4", "writable"]
            },
        }
    }

    fn name(&self) -> &str {
        "ext4"
    }

    fn super_block(&self) -> SuperBlock {
        let super_block = self.inner().super_block().ok();
        match super_block {
            Some(super_block) => SuperBlock {
                f_type: 0xEF53,
                f_bsize: another_ext4::BLOCK_SIZE as u64,
                f_blocks: super_block.block_count(),
                f_bfree: super_block.free_blocks_count(),
                f_bavail: super_block.free_blocks_count(),
                f_files: super_block.inode_count() as u64,
                f_ffree: super_block.free_inodes_count() as u64,
                f_fsid: [0; 2],
                f_namelen: 255,
                f_frsize: another_ext4::BLOCK_SIZE as u64,
                flags: 0,
                f_spare: [0; 4],
            },
            None => SuperBlock::default(),
        }
    }

    fn on_umount(&self) -> Result<(), SyscallErr> {
        if self.read_only {
            return Ok(());
        }
        self.sync_all()?;
        self.inner()
            .shutdown_writable()
            .map_err(|failure| from_another(failure.code()))
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}
