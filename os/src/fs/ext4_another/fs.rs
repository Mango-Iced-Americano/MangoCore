use alloc::sync::Arc;
use core::any::Any;
use core::fmt;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use crate::drivers::block::BlockDevice;
use crate::fs::vfs::{FileSystem, FsInfo, IndexNode, SuperBlock};
use crate::utils::error::SyscallErr;

use super::blockdev::MangoBlockDevice;
use super::errno::from_another;
use super::inode::Ext4Inode;
use super::lifetime::{InodeKey, InodeLifetime};

static NEXT_FS_ID: AtomicUsize = AtomicUsize::new(1);

/// One writable another_ext4 filesystem instance.
pub struct Ext4FileSystem {
    ext4: Arc<another_ext4::Ext4>,
    fs_id: usize,
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
        if !block_device.supports_reliable_flush() {
            return Err(SyscallErr::EROFS);
        }
        let device = Arc::new(MangoBlockDevice::new(block_device));
        let ext4 = another_ext4::Ext4::load_writable(device)
            .map_err(|error| from_another(error.code()))?;
        let fs = Arc::new(Self {
            ext4: Arc::new(ext4),
            fs_id: NEXT_FS_ID.fetch_add(1, Ordering::Relaxed),
            root: Mutex::new(None),
            lifetimes: Mutex::new(alloc::collections::BTreeMap::new()),
        });
        *fs.root.lock() = Some(Ext4Inode::new(fs.clone(), another_ext4::EXT4_ROOT_INO)?);
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
        self.sync_lifetimes()
    }

    pub(crate) fn flush_device(&self) -> Result<(), SyscallErr> {
        self.inner()
            .flush_device()
            .map_err(|error| from_another(error.code()))
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
            features: alloc::vec!["ext4", "another_ext4", "writable"],
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

    fn on_umount(&self) {
        if let Err(error) = self.sync_all() {
            log::error!("another_ext4: writeback before unmount failed: {:?}", error);
            return;
        }
        if let Err(error) = self
            .inner()
            .shutdown_writable()
            .map_err(|failure| from_another(failure.code()))
        {
            log::error!("another_ext4: clean shutdown failed: {:?}", error);
        }
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}
