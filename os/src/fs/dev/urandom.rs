use crate::fs::{dirent::Dirent, DiskInodeType};
use alloc::sync::Arc;
use core::any::Any;

use crate::{
    fs::{directory_tree::DirectoryTreeNode, file_trait::File, layout::Stat, StatMode},
    mm::UserBuffer,
    syscall::errno::{ENOTDIR, ESPIPE},
};

use crate::fs::vfs::{
    FilePrivateData, FileType, IndexNode, InodeFlags, InodeMode, Metadata,
};
use crate::fs::vfs::file_system::FileSystem as NewFileSystem;
use crate::fs::dev::DEV_FS;
use crate::timer::TimeSpec;
use crate::utils::error::SyscallErr;

#[derive(Debug)]
pub struct Urandom;

impl IndexNode for Urandom {
    fn read_at(
        &self,
        _offset: usize,
        _len: usize,
        _buf: &mut [u8],
        _data: spin::MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        // TODO: 实现真正的随机数生成
        Ok(0)
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
            raw_dev: crate::makedev!(1, 9),
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

#[allow(unused)]
impl File for Urandom {
    fn deep_clone(&self) -> Arc<dyn File> {
        Arc::new(Urandom {})
    }

    fn readable(&self) -> bool {
        true
    }

    fn writable(&self) -> bool {
        true
    }
    ///todo: 未实现随机，read函数什么都没做，返回0
    fn read(&self, offset: Option<&mut usize>, buf: &mut [u8]) -> usize {
        0
    }

    fn write(&self, offset: Option<&mut usize>, buf: &[u8]) -> usize {
        unreachable!()
    }

    fn r_ready(&self) -> bool {
        true
    }

    fn w_ready(&self) -> bool {
        true
    }

    fn get_size(&self) -> usize {
        todo!()
    }

    fn get_stat(&self) -> Stat {
        Stat::new(
            crate::makedev!(0, 5),
            1,
            StatMode::S_IFCHR.bits() | 0o666,
            1,
            crate::makedev!(1, 5),
            0,
            0,
            0,
            0,
        )
    }

    fn read_user(&self, offset: Option<usize>, mut buf: UserBuffer) -> usize {
        buf.clear();
        buf.len()
    }

    fn write_user(&self, offset: Option<usize>, buf: UserBuffer) -> usize {
        buf.len()
    }

    fn get_file_type(&self) -> DiskInodeType {
        DiskInodeType::File
    }

    fn info_dirtree_node(
        &self,
        dirnode_ptr: alloc::sync::Weak<crate::fs::directory_tree::DirectoryTreeNode>,
    ) {
    }

    fn get_dirtree_node(&self) -> Option<Arc<DirectoryTreeNode>> {
        todo!()
    }

    fn open(&self, flags: crate::fs::layout::OpenFlags, special_use: bool) -> Arc<dyn File> {
        Arc::new(Urandom {})
    }

    fn open_subfile(
        &self,
    ) -> Result<alloc::vec::Vec<(alloc::string::String, alloc::sync::Arc<dyn File>)>, isize> {
        Err(ENOTDIR)
    }

    fn create(&self, name: &str, file_type: DiskInodeType) -> Result<Arc<dyn File>, isize> {
        todo!()
    }

    fn link_child(&self, name: &str, child: &Self) -> Result<(), isize>
    where
        Self: Sized,
    {
        todo!()
    }

    fn unlink(&self, delete: bool) -> Result<(), isize> {
        todo!()
    }

    fn get_dirent(&self, _count: usize) -> Result<alloc::vec::Vec<Dirent>, isize> {
        Ok(alloc::vec::Vec::new())
    }

    fn lseek(&self, offset: isize, whence: crate::fs::SeekWhence) -> Result<usize, isize> {
        Err(ESPIPE)
    }

    fn modify_size(&self, diff: isize) -> Result<(), isize> {
        todo!()
    }

    fn truncate_size(&self, new_size: usize) -> Result<(), isize> {
        todo!()
    }

    fn set_timestamp(&self, ctime: Option<usize>, atime: Option<usize>, mtime: Option<usize>) {
        todo!()
    }

    fn get_single_cache(
        &self,
        offset: usize,
    ) -> Result<Arc<spin::Mutex<crate::fs::PageCache>>, ()> {
        todo!()
    }

    fn get_all_caches(
        &self,
    ) -> Result<alloc::vec::Vec<Arc<spin::Mutex<crate::fs::PageCache>>>, ()> {
        todo!()
    }

    fn oom(&self) -> usize {
        0
    }

    fn hang_up(&self) -> bool {
        todo!()
    }

    fn fcntl(&self, cmd: u32, arg: u32) -> isize {
        todo!()
    }
}
