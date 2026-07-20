use alloc::string::String;
use alloc::sync::Arc;
use core::fmt;
use spin::{Mutex, MutexGuard};

use crate::drivers::block::BlockDevice;
use crate::fs::vfs::file_system::FileSystem;
use crate::fs::vfs::{FilePrivateData, FileType, IndexNode, Metadata};
use crate::hal::BLOCK_SZ;
use crate::timer::TimeSpec;
use crate::utils::error::SyscallErr;

const VIRTIO_BLK_MAJOR: u64 = 254;

const BLKGETSIZE64: u32 = 0x8008_1272;
const BLKSSZGET: u32 = 0x1268;
const DEV_LOGICAL_SECTOR_SIZE: i32 = 512;

pub struct BlockDevInode {
    pub inner: Arc<dyn BlockDevice>,
    raw_dev: u64,
    pub label: String,
}

impl BlockDevInode {
    pub fn new(inner: Arc<dyn BlockDevice>, minor: u64, label: String) -> Arc<Self> {
        Arc::new(Self {
            inner,
            raw_dev: crate::fs::dev::mkdev(VIRTIO_BLK_MAJOR, minor),
            label,
        })
    }

    fn size_usize(&self) -> Option<usize> {
        self.inner.size_bytes().map(|b| b as usize)
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
        let dev_size = self.inner.size_bytes().unwrap_or(0);
        Ok(Metadata {
            dev_id: 0,
            inode_id: crate::fs::vfs::generate_inode_id(),
            size: dev_size as i64,
            blk_size: BLOCK_SZ,
            blocks: (dev_size as usize) / BLOCK_SZ,
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
        let total = core::cmp::min(len, buf.len());
        if total == 0 {
            return Ok(0);
        }

        if let Some(dev_size) = self.size_usize() {
            if offset >= dev_size {
                return Ok(0);
            }
        }

        let mut bounce = alloc::vec![0u8; BLOCK_SZ];
        let mut done = 0;

        while done < total {
            let pos = offset + done;
            let block_id = pos / BLOCK_SZ;
            let in_block = pos % BLOCK_SZ;
            let n = (BLOCK_SZ - in_block).min(total - done);

            if let Some(dev_size) = self.size_usize() {
                if pos >= dev_size {
                    break;
                }
                let n = n.min(dev_size - pos);
                if n == 0 {
                    break;
                }
                self.inner
                    .read_block(block_id, &mut bounce)
                    .map_err(|_| SyscallErr::EIO)?;
                buf[done..done + n].copy_from_slice(&bounce[in_block..in_block + n]);
                done += n;
                break;
            }

            self.inner
                .read_block(block_id, &mut bounce)
                .map_err(|_| SyscallErr::EIO)?;
            buf[done..done + n].copy_from_slice(&bounce[in_block..in_block + n]);
            done += n;
        }

        Ok(done)
    }

    fn write_at(
        &self,
        offset: usize,
        len: usize,
        buf: &[u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        let total = core::cmp::min(len, buf.len());
        if total == 0 {
            return Ok(0);
        }

        if let Some(dev_size) = self.size_usize() {
            if offset >= dev_size {
                return Err(SyscallErr::ENOSPC);
            }
            if total > dev_size - offset {
                return Err(SyscallErr::ENOSPC);
            }
        }

        let mut bounce = alloc::vec![0u8; BLOCK_SZ];
        let mut done = 0;

        while done < total {
            let pos = offset + done;
            let block_id = pos / BLOCK_SZ;
            let in_block = pos % BLOCK_SZ;
            let n = (BLOCK_SZ - in_block).min(total - done);

            if in_block == 0 && n == BLOCK_SZ {
                self.inner
                    .write_block(block_id, &buf[done..done + n])
                    .map_err(|_| SyscallErr::EIO)?;
            } else {
                self.inner
                    .read_block(block_id, &mut bounce)
                    .map_err(|_| SyscallErr::EIO)?;
                bounce[in_block..in_block + n].copy_from_slice(&buf[done..done + n]);
                self.inner
                    .write_block(block_id, &bounce)
                    .map_err(|_| SyscallErr::EIO)?;
            }

            done += n;
        }

        Ok(done)
    }

    fn ioctl(
        &self,
        cmd: u32,
        data: usize,
        _private_data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        match cmd {
            BLKGETSIZE64 => {
                let size = self.inner.size_bytes().ok_or(SyscallErr::ENOTTY)?;
                let token = crate::task::current_task()
                    .ok_or(SyscallErr::ENOTTY)?
                    .get_user_token();
                match crate::mm::translated_refmut(token, data as *mut u64) {
                    Ok(r) => {
                        *r = size;
                        Ok(0)
                    }
                    Err(_) => Err(SyscallErr::EFAULT),
                }
            }
            BLKSSZGET => {
                let token = crate::task::current_task()
                    .ok_or(SyscallErr::ENOTTY)?
                    .get_user_token();
                match crate::mm::translated_refmut(token, data as *mut i32) {
                    Ok(r) => {
                        *r = DEV_LOGICAL_SECTOR_SIZE;
                        Ok(0)
                    }
                    Err(_) => Err(SyscallErr::EFAULT),
                }
            }
            _ => Err(SyscallErr::ENOTTY),
        }
    }

    fn fs(&self) -> Arc<dyn FileSystem> {
        crate::fs::dev::DEV_FS.clone()
    }

    fn resize(&self, _len: usize) -> Result<(), SyscallErr> {
        Ok(())
    }

    fn as_any_ref(&self) -> &dyn core::any::Any {
        self
    }
}
