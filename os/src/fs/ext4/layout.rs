#![allow(unused)]
use crate::{
    config::PAGE_SIZE,
    fs::{
        directory_tree::DirectoryTreeNode,
        dirent::Dirent,
        ext4::{
            block_group::Block,
            direntry::{DirEntryType, Ext4DirEntryTail},
            InodeFileType, PageCache,
        },
        file_trait::File,
        inode::{self, InodeLock},
        DiskInodeType, OpenFlags, SeekWhence, Stat, StatMode,
    },
    lang_items::Bytes,
    mm::UserBuffer,
    net::socket::inet::stream::inner,
    syscall::errno::{EINVAL, EIO, ENOENT, ENOMEM, ENOTDIR, ENOTEMPTY, ENOSYS},
    utils::error::SyscallErr,
};
use alloc::{
    format,
    string::{String, ToString},
    sync::{Arc, Weak},
    vec::Vec,
};
use spin::{Mutex, RwLock};

use core::{
    convert::{TryFrom, TryInto},
    fmt::Debug,
    mem, panic,
    ptr::{addr_of, addr_of_mut, read},
};

use super::{
    direntry::Ext4DirEntry,
    ext4fs::Ext4FileSystem,
    file::{Ext4FileContent, Ext4FileContentWrapper},
    Cache, Ext4Inode, Ext4InodeRef, InodePerm, PageCacheManager,
};

// use crate::timer::get_time;
// use crate::hal::arch::riscv::rv_board::CLOCK_FREQ;

// 可能后续会用到？
pub enum ExtType {
    Ext2,
    Ext3,
    Ext4,
}

// 对Ext4Inode的一层封装，用于构成与OSInode同级别的结构体
pub struct Ext4OSInode {
    /// 是否可读
    pub(super) readable: bool,
    /// 是否可写
    pub(super) writable: bool,
    /// 被进程使用的计数
    pub(super) special_use: bool,
    /// 是否追加
    pub(super) append: bool,
    /// 具体的Inode
    pub(super) inode: Arc<Mutex<Ext4InodeRef>>,
    /// 文件偏移
    pub(super) offset: Mutex<usize>,
    /// ext4fs实例
    pub(super) ext4fs: Arc<Ext4FileSystem>,
    /// inode锁
    pub(super) inode_lock: Arc<RwLock<InodeLock>>,
    /// 文件缓存
    pub(super) file_cache_manager: Arc<PageCacheManager>,
}

impl core::fmt::Debug for Ext4OSInode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Ext4OSInode")
            .field("inode_num", &self.inode.lock().inode_num)
            .finish()
    }
}

impl Ext4OSInode {
    // 只在获取根目录时使用
    pub fn new(root_inode: Ext4InodeRef, ext4fs: Arc<Ext4FileSystem>) -> Arc<dyn File> {
        Arc::new(Self {
            inode_lock: Arc::new(RwLock::new(InodeLock {})),
            readable: true,
            writable: true,
            special_use: true,
            append: false,
            inode: Arc::new(Mutex::new(root_inode)),
            offset: Mutex::new(0),
            ext4fs,
            file_cache_manager: Arc::new(PageCacheManager::new()),
        })
    }
}

// Minimal File trait impl — only enough for old VFS in directory_tree.rs to compile.
// Full removal pending Phase 5 directory_tree.rs cleanup.
impl File for Ext4OSInode {
    fn deep_clone(&self) -> Arc<dyn File> {
        Arc::new(Self {
            inode_lock: Arc::new(RwLock::new(InodeLock {})),
            readable: self.readable,
            writable: self.writable,
            special_use: self.special_use,
            append: self.append,
            inode: self.inode.clone(),
            offset: Mutex::new(*self.offset.lock()),
            ext4fs: self.ext4fs.clone(),
            file_cache_manager: self.file_cache_manager.clone(),
        })
    }
    fn readable(&self) -> bool { true }
    fn writable(&self) -> bool { true }
    fn read(&self, _offset: Option<&mut usize>, _buffer: &mut [u8]) -> usize { 0 }
    fn write(&self, _offset: Option<&mut usize>, _buffer: &[u8]) -> usize { 0 }
    fn r_ready(&self) -> bool { false }
    fn w_ready(&self) -> bool { false }
    fn read_user(&self, _offset: Option<usize>, _buf: UserBuffer) -> usize { 0 }
    fn write_user(&self, _offset: Option<usize>, _buf: UserBuffer) -> usize { 0 }
    fn get_size(&self) -> usize { 0 }
    fn get_stat(&self) -> Stat {
        Stat::new(0, 0, 0, 0, 0, 0, 0, 0, 0)
    }
    fn get_file_type(&self) -> DiskInodeType {
        let ft = self.inode.lock().inode.file_type();
        match ft {
            InodeFileType::S_IFDIR => DiskInodeType::Directory,
            InodeFileType::S_IFREG => DiskInodeType::File,
            InodeFileType::S_IFLNK => DiskInodeType::Link,
            _ => DiskInodeType::File,
        }
    }
    fn info_dirtree_node(&self, _dirnode_ptr: Weak<DirectoryTreeNode>) {}
    fn get_dirtree_node(&self) -> Option<Arc<DirectoryTreeNode>> { None }
    fn open(&self, flags: OpenFlags, special_use: bool) -> Arc<dyn File> {
        Arc::new(Self {
            readable: flags.contains(OpenFlags::O_RDONLY) || flags.contains(OpenFlags::O_RDWR),
            writable: flags.contains(OpenFlags::O_WRONLY) || flags.contains(OpenFlags::O_RDWR),
            special_use,
            append: flags.contains(OpenFlags::O_APPEND),
            inode: self.inode.clone(),
            offset: Mutex::new(0),
            ext4fs: self.ext4fs.clone(),
            inode_lock: self.inode_lock.clone(),
            file_cache_manager: self.file_cache_manager.clone(),
        })
    }
    fn open_subfile(&self) -> Result<Vec<(String, Arc<dyn File>)>, isize> {
        Ok(Vec::new())
    }
    fn create(&self, _name: &str, _file_type: DiskInodeType) -> Result<Arc<dyn File>, isize> {
        Err(ENOSYS)
    }
    fn link_child(&self, _name: &str, _child: &Self) -> Result<(), isize> {
        Ok(())
    }
    fn read_link(&self) -> alloc::string::String {
        alloc::string::String::new()
    }
    fn unlink(&self, _delete: bool) -> Result<(), isize> {
        Ok(())
    }
    fn get_dirent(&self, _count: usize) -> Result<Vec<Dirent>, isize> {
        Ok(Vec::new())
    }
    fn lseek(&self, _offset: isize, _whence: SeekWhence) -> Result<usize, isize> {
        Ok(0)
    }
    fn modify_size(&self, _diff: isize) -> Result<(), isize> { Ok(()) }
    fn truncate_size(&self, _new_size: usize) -> Result<(), isize> { Ok(()) }
    fn set_timestamp(&self, _ctime: Option<usize>, _atime: Option<usize>, _mtime: Option<usize>) {}
    fn get_single_cache(&self, _offset: usize) -> Result<Arc<Mutex<PageCache>>, ()> {
        Err(())
    }
    fn get_all_caches(&self) -> Result<Vec<Arc<Mutex<PageCache>>>, ()> {
        Err(())
    }
    fn oom(&self) -> usize { 0 }
    fn hang_up(&self) -> bool { false }
    fn fcntl(&self, _cmd: u32, _arg: u32) -> isize { 0 }
}

impl Drop for Ext4OSInode {
    fn drop(&mut self) {
        // special_use reference counting removed along with dirnode_ptr
    }
}

// Old File trait impl removed during VFS migration (Phase 4)
// Ext4OSInode now only implements IndexNode (see ext4fs.rs)
