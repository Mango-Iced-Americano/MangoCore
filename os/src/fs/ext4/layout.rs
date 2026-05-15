#![allow(unused)]
use crate::{
    config::PAGE_SIZE,
    fs::{
        dirent::Dirent,
        ext4::{
            block_group::Block,
            direntry::{DirEntryType, Ext4DirEntryTail},
            InodeFileType, PageCache,
        },
        inode::{self, InodeLock},
    },
    lang_items::Bytes,
    mm::UserBuffer,
    net::socket::inet::stream::inner,
    syscall::errno::ENOSYS,
    utils::error::SyscallErr,
};
use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
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

impl Drop for Ext4OSInode {
    fn drop(&mut self) {
        // special_use reference counting removed along with dirnode_ptr
    }
}

// Old File trait impl removed during VFS migration (Phase 4)
// Ext4OSInode now only implements IndexNode (see ext4fs.rs)
