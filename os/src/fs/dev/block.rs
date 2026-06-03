use alloc::sync::Arc;
use core::fmt;
use spin::{Mutex, MutexGuard};

use crate::drivers::block::BlockDevice;
use crate::fs::vfs::{
    FilePrivateData, FileType, IndexNode, Metadata,
};
use crate::fs::vfs::file_system::FileSystem;
use crate::hal::BLOCK_SZ;
use crate::timer::TimeSpec;
use crate::utils::error::SyscallErr;

const VIRTIO_BLK_MAJOR: u64 = 254;

pub struct BlockDevInode {
    inner: Arc<dyn BlockDevice>,
    raw_dev: u64,
    pub label: &'static str,
}

impl BlockDevInode {
    pub fn new(inner: Arc<dyn BlockDevice>, minor: u64, label: &'static str) -> Arc<Self> {
        Arc::new(Self {
            inner,
            raw_dev: crate::fs::dev::mkdev(VIRTIO_BLK_MAJOR, minor),
            label,
        })
    }
}

impl fmt::Debug for BlockDevInode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlockDevInode")
            .field("label", &self.label)
            .finish()
    }
}

impl IndexNode for BlockDevInode {
    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        let now = TimeSpec::new();
        Ok(Metadata {
            dev_id: 0,
            inode_id: crate::fs::vfs::generate_inode_id(),
            size: 0,
            blk_size: BLOCK_SZ,
            blocks: 0,
            atime: now,
            mtime: now,
            ctime: now,
            file_type: FileType::BlockDevice,
            mode: crate::fs::vfs::InodeMode::S_IFBLK
                | crate::fs::vfs::InodeMode::from_bits_truncate(0o660),
            nlinks: 1,
            uid: 0,
            gid: 0,
            raw_dev: self.raw_dev,
            flags: crate::fs::vfs::InodeFlags::empty(),
        })
    }

    fn read_at(
        &self,
        offset: usize,
        len: usize,
        buf: &mut [u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        if offset % BLOCK_SZ != 0 || len % BLOCK_SZ != 0 {
            return Err(SyscallErr::EINVAL);
        }
        let start_block = offset / BLOCK_SZ;
        let end_block = (offset + len + BLOCK_SZ - 1) / BLOCK_SZ;
        let mut temp = alloc::vec![0u8; (end_block - start_block) * BLOCK_SZ];
        for (i, chunk) in temp.chunks_mut(BLOCK_SZ).enumerate() {
            self.inner.read_block(start_block + i, chunk);
        }
        let rel = offset % BLOCK_SZ;
        let copy = core::cmp::min(len, temp.len().saturating_sub(rel));
        buf[..copy].copy_from_slice(&temp[rel..rel + copy]);
        Ok(copy)
    }

    fn write_at(
        &self,
        offset: usize,
        len: usize,
        buf: &[u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        if offset % BLOCK_SZ != 0 || len % BLOCK_SZ != 0 {
            return Err(SyscallErr::EINVAL);
        }
        let start_block = offset / BLOCK_SZ;
        let end_block = (offset + len + BLOCK_SZ - 1) / BLOCK_SZ;
        let block_bytes = (end_block - start_block) * BLOCK_SZ;
        let mut temp = alloc::vec![0u8; block_bytes];
        for (i, chunk) in temp.chunks_mut(BLOCK_SZ).enumerate() {
            self.inner.read_block(start_block + i, chunk);
        }
        let rel = offset % BLOCK_SZ;
        let copy = core::cmp::min(len, temp.len().saturating_sub(rel));
        temp[rel..rel + copy].copy_from_slice(&buf[..copy]);
        for (i, chunk) in temp.chunks(BLOCK_SZ).enumerate() {
            self.inner.write_block(start_block + i, chunk);
        }
        Ok(copy)
    }

    fn ioctl(
        &self,
        _cmd: u32,
        _data: usize,
        _private_data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        Err(SyscallErr::ENOTTY)
    }

    fn fs(&self) -> Arc<dyn FileSystem> {
        crate::fs::dev::DEV_FS.clone()
    }

    fn as_any_ref(&self) -> &dyn core::any::Any {
        self
    }
}
