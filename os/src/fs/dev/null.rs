use alloc::sync::Arc;
use core::any::Any;

use crate::fs::vfs::{
    FilePrivateData, FileType, IndexNode, InodeFlags, InodeMode, Metadata,
};
use crate::fs::vfs::file_system::FileSystem as NewFileSystem;
use crate::fs::dev::DEV_FS;
use crate::timer::TimeSpec;
use crate::utils::error::SyscallErr;

/// Data Sink
/// Data written to the `/dev/null` special files is discarded.
/// Reads  from `/dev/null` always return end of file (i.e., read(2) returns 0)
#[derive(Debug)]
pub struct Null;

impl IndexNode for Null {
    fn read_at(
        &self,
        _offset: usize,
        _len: usize,
        _buf: &mut [u8],
        _data: spin::MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        Ok(0) // 总是返回 EOF
    }

    fn read_at_user(
        &self,
        _offset: usize,
        _len: usize,
        _dst: &mut crate::mm::UserBuffer,
    ) -> Result<usize, SyscallErr> {
        Ok(0) // EOF, same semantics as read_at
    }

    fn write_at(
        &self,
        _offset: usize,
        _len: usize,
        buf: &[u8],
        _data: spin::MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        Ok(buf.len()) // 丢弃所有写入数据
    }

    fn write_at_user(
        &self,
        _offset: usize,
        len: usize,
        _src: &crate::mm::UserBuffer,
    ) -> Result<usize, SyscallErr> {
        // discard — /dev/null semantics: ignore all written data
        Ok(len)
    }

    fn supports_user_buffer_io(&self) -> bool {
        true
    }

    fn is_discard_write(&self) -> bool {
        true
    }

    fn resize(&self, _len: usize) -> Result<(), SyscallErr> {
        Ok(()) // /dev/null 无需实际截断，O_TRUNC 不报错即可
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
            raw_dev: crate::makedev!(1, 3),
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
