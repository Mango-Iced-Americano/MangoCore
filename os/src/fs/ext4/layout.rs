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
    collections::BTreeMap,
    format,
    string::{String, ToString},
    sync::Arc,
    sync::Weak,
    vec::Vec,
};
use spin::{Mutex, RwLock};

use core::{
    convert::{TryFrom, TryInto},
    fmt::Debug,
    mem, panic,
    ptr::{addr_of, addr_of_mut, read},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use super::{
    direntry::Ext4DirEntry,
    ext4fs::Ext4FileSystem,
    file::{Ext4FileContent, Ext4FileContentWrapper},
    Ext4Inode, Ext4InodeRef, InodePerm,
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

// ── Ext4OSInode: VFS-facing ext4 inode object ──
//
// 设计参考 DragonOS kernel/src/filesystem/ext4/inode.rs:
//   - LockedExt4Inode(Mutex<Ext4Inode>)
//   - children: BTreeMap<DName, Arc<LockedExt4Inode>>  (强引用)
//   - cached_file_size, metadata_dirty
//
// MangoCore 差异:
//   - children 使用 Arc<dyn IndexNode> 加速 lookup；通过 ext4fs Weak 避免循环引用
//   - inode data 使用 Arc<Mutex<Ext4InodeRef>> (底层磁盘快照)
//   - DragonOS 底层是 another_ext4, 当前内核自己实现 ext4 磁盘逻辑
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

    // ── Phase 2: children cache (reference: DragonOS Ext4Inode.children) ──
    //
    // 目录 inode 维护的子项缓存，加速同目录 repeated lookup/find/readlink。
    // 使用 Weak<dyn IndexNode> 避免循环引用（parent → child → parent）。
    // 所有目录修改操作（create/symlink/mkdir/unlink/rmdir/rename）必须维护一致性。
    //
    // 非目录 inode 此字段为空且不使用。
    pub(super) children: Mutex<BTreeMap<String, alloc::sync::Arc<dyn crate::fs::vfs::IndexNode>>>,

    // ── Phase 4: negative dentry cache (version-based invalidation) ──
    pub(super) negative_dentry: Mutex<BTreeMap<String, u64>>,
    pub(super) dir_version: AtomicU64,

    // ── Phase 3: per-inode metadata cache ──
    // DragonOS 参考: Ext4Inode.cached_file_size / metadata_dirty
    // cached_symlink_target: MangoCore 针对 fast symlink 的增强
    // cached_file_size: u64::MAX = unset sentinel
    pub(super) cached_file_size: AtomicU64,
    pub(super) cached_symlink_target: Mutex<Option<alloc::string::String>>,
    pub(super) metadata_dirty: AtomicBool,
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
        let (ino, links, is_dir) = {
            let guard = self.inode.lock();
            (guard.inode_num, guard.inode.links_count(), guard.inode.is_dir())
        };
        if links == 0 {
            // truncate_inode(0) 释放所有数据块，失败则跳过后续 inode 号释放
            if self.ext4fs.truncate_inode(&mut *self.inode.lock(), 0).is_ok() {
                self.ext4fs.ialloc_free_inode(ino, is_dir);
            }
            self.ext4fs.unregister_page_cache(ino);
            self.ext4fs.remove_inode_object(ino);
            self.ext4fs.inode_cache.lock().remove(&ino);
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

        {
            let registry = self.ext4fs.page_caches.lock();
            if let Some(weak) = registry.get(&inode_num) {
                if let Some(pc) = weak.upgrade() {
                    *cache_opt = Some(pc.clone());
                    return Some(pc);
                }
            }
        }

        let backend = Arc::new(Ext4PageCacheBackend::new(
            Arc::downgrade(&self.ext4fs),
            inode_num,
        ));
        let pc = NewPageCache::new();
        pc.set_backend(backend);
        self.ext4fs
            .page_caches
            .lock()
            .insert(inode_num, Arc::downgrade(&pc));
        *cache_opt = Some(pc.clone());
        Some(pc)
    }
}

// Old File trait impl removed during VFS migration (Phase 4)
// Ext4OSInode now only implements IndexNode (see ext4fs.rs)
