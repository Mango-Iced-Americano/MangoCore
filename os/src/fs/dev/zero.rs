use alloc::sync::Arc;
use core::any::Any;

use crate::fs::dev::DEV_FS;
use crate::fs::vfs::file_system::FileSystem as NewFileSystem;
use crate::fs::vfs::{FilePrivateData, FileType, IndexNode, InodeFlags, InodeMode, Metadata};
use crate::timer::TimeSpec;
use crate::utils::error::SyscallErr;

/// Data Sink
/// Data written to the `/dev/zero` special files is discarded.
/// Reads from `/dev/zero` always return  bytes  containing  zero (`'\0'` characters).
#[derive(Debug)]
pub struct Zero;

impl IndexNode for Zero {
    fn read_at(
        &self,
        _offset: usize,
        _len: usize,
        buf: &mut [u8],
        _data: spin::MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        buf.fill(0);
        Ok(buf.len())
    }

    fn write_at(
        &self,
        _offset: usize,
        _len: usize,
        buf: &[u8],
        _data: spin::MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        Ok(buf.len())
    }

    fn read_at_user(
        &self,
        _offset: usize,
        len: usize,
        dst: &mut crate::mm::UserBuffer,
    ) -> Result<usize, SyscallErr> {
        let n = dst.fill_at(0, len, 0);
        Ok(n)
    }

    fn write_at_user(
        &self,
        _offset: usize,
        len: usize,
        _src: &crate::mm::UserBuffer,
    ) -> Result<usize, SyscallErr> {
        // discard — same semantics as /dev/null write
        Ok(len)
    }

    fn supports_user_buffer_io(&self) -> bool {
        true
    }

    fn is_discard_write(&self) -> bool {
        true
    }

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        Ok(Metadata {
            dev_id: 0,
            inode_id: 0,
            size: 0,
            blk_size: 0,
            blocks: 0,
            atime: TimeSpec::new(),
            mtime: TimeSpec::new(),
            ctime: TimeSpec::new(),
            file_type: FileType::CharDevice,
            mode: InodeMode::S_IFCHR | InodeMode::from_bits_truncate(0o666),
            nlinks: 1,
            uid: 0,
            gid: 0,
            flags: InodeFlags::empty(),
            raw_dev: crate::makedev!(1, 5),
        })
    }

    fn is_stream(&self) -> bool {
        true
    }

    fn fs(&self) -> Arc<dyn NewFileSystem> {
        DEV_FS.clone()
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}
