#![allow(unused)]
use crate::{
    config::PAGE_SIZE,
    fs::{
        dirent::Dirent,
        ext4::{
            block_group::Block,
            direntry::{DirEntryType, Ext4DirEntryTail},
            InodeFileType,
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
    Cache, Ext4Inode, Ext4InodeRef, InodePerm,
};
use crate::fs::page_cache::{Ext4PageCacheBackend, PageCache as NewPageCache, PageCacheBackend};

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
    /// 新 PageCache（懒初始化，仅用于普通文件数据）
    pub(super) new_page_cache: Mutex<Option<Arc<NewPageCache>>>,
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
        if let Some(ref pc) = *self.new_page_cache.lock() {
            let _ = pc.writeback_all();
        }
    }
}

impl Ext4OSInode {
    /// 获取或初始化新 PageCache（懒初始化，线程安全）
    /// 仅用于普通文件，目录返回 None
    pub fn get_new_page_cache(&self) -> Option<Arc<NewPageCache>> {
        {
            let ino_ref = self.inode.lock();
            if ino_ref.inode.is_dir() {
                return None;
            }
        }
        let mut cache_opt = self.new_page_cache.lock();
        if let Some(ref pc) = *cache_opt {
            return Some(pc.clone());
        }
        let inode_num = self.inode.lock().inode_num;
        let backend = Arc::new(Ext4PageCacheBackend::new(
            Arc::downgrade(&self.ext4fs),
            inode_num,
        ));
        let pc = NewPageCache::new();
        pc.set_backend(backend);
        *cache_opt = Some(pc.clone());
        Some(pc)
    }
}

// Old File trait impl removed during VFS migration (Phase 4)
// Ext4OSInode now only implements IndexNode (see ext4fs.rs)
