#![allow(unused)]
use super::DiskInodeType;
use crate::fs::fat32::dir_iter::*;
use crate::fs::fat32::layout::{FATDirEnt, FATDiskInodeType, FATLongDirEnt, FATShortDirEnt};
use crate::fs::fat32::EasyFileSystem;
use crate::fs::inode::InodeLock;
use crate::fs::inode::InodeTime;
use crate::fs::page_cache::{FatPageCacheBackend, PageCache as NewPageCache, PageCacheBackend};
use crate::fs::vfs::{
    FilePrivateData, FileType, IndexNode, InodeFlags, InodeId, InodeMode, Metadata,
};
use crate::fs::vfs::file_system::FileSystem as NewFileSystem;
use crate::utils::error::SyscallErr;
use crate::timer::TimeSpec;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::convert::TryInto;
use core::ops::Mul;
use spin::*;

/// 文件内容 FileContent
pub struct FileContent {
    /// 对于FAT32，size 需要从FAT计算
    /// 所以需要遍历FAT32来获取size
    pub(crate) size: u32,
    /// 簇列表
    pub(crate) clus_list: Vec<u32>,
    /// 如果该文件是个目录，那么
    /// hint 会记录最后一个目录项的位置（第一个字节为0x00）
    hint: u32,
}

impl FileContent {
    /// 获取文件大小
    /// # 返回值
    /// 文件大小
    #[inline(always)]
    pub fn get_file_size(&self) -> u32 {
        self.size
    }
}
macro_rules! div_ceil {
    ($mult:expr,$deno:expr) => {
        ($mult - 1 + $deno) / $deno
    };
}

/* *ClusLi was DiskInode*
 * Even old New York, was New Amsterdam...
 * Why they changed it I can't say.
 * People just like it better that way.*/
/// The functionality of ClusLi & Inode can be merged.
/// The struct for file information
/// 上面这段描述可能是来自最早的文件系统实现，我也不知道怎么翻译
pub struct FatInode {
    /// inode 锁: for normal operation
    inode_lock: RwLock<InodeLock>,
    /// 文件内容
    pub(crate) file_content: RwLock<FileContent>,
    /// 新页面缓存（替代旧 file_cache_mgr，逐步迁移）
    /// 用 Option 做懒初始化，首次使用前需要调用 init_new_page_cache
    new_page_cache: Mutex<Option<Arc<NewPageCache>>>,
    /// 指向自身的弱引用（用于 FatPageCacheBackend）
    self_weak: Mutex<Option<alloc::sync::Weak<FatInode>>>,
    /// 文件类型
    file_type: Mutex<DiskInodeType>,
    /// 父目录的inode
    parent_dir: Mutex<Option<(Arc<Self>, u32)>>,
    /// 文件系统实例
    fs: Arc<EasyFileSystem>,
    /// 保存时间的结构体
    time: Mutex<InodeTime>,
    /// Info Inode to delete file content
    deleted: Mutex<bool>,
}

impl core::fmt::Debug for FatInode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let inode_num = self
            .get_inode_num_lock(&self.file_content.read())
            .unwrap_or(0);
        f.debug_struct("FatInode")
            .field("inode_num", &inode_num)
            .field("file_type", &self.get_file_type())
            .field("size", &self.get_file_size())
            .finish()
    }
}

/// 将 VFS FileType 转换为 FAT32 DiskInodeType
fn vfs_type_to_fat_disk_type(ft: FileType) -> DiskInodeType {
    match ft {
        FileType::File => DiskInodeType::File,
        FileType::Dir => DiskInodeType::Directory,
        FileType::SymLink => DiskInodeType::Link,
        FileType::CharDevice => DiskInodeType::Character,
        FileType::BlockDevice => DiskInodeType::Block,
        FileType::Socket => DiskInodeType::Socket,
        FileType::Pipe => DiskInodeType::FIFO,
        _ => DiskInodeType::File,
    }
}

/// 将 FAT32 DiskInodeType 转换为 VFS FileType
fn fat_disk_type_to_vfs_type(dt: DiskInodeType) -> FileType {
    match dt {
        DiskInodeType::File => FileType::File,
        DiskInodeType::Directory => FileType::Dir,
        DiskInodeType::Link => FileType::SymLink,
        DiskInodeType::Character => FileType::CharDevice,
        DiskInodeType::Block => FileType::BlockDevice,
        DiskInodeType::Socket => FileType::Socket,
        DiskInodeType::FIFO => FileType::Pipe,
        DiskInodeType::Unknown => {
            log::error!("fat_disk_type_to_vfs_type: unknown disk type, falling back to File");
            FileType::File
        }
    }
}

impl FatInode {
    /// 初始化 self_weak（在 Arc 构造完成后调用）
    pub fn init_self_weak(self: &Arc<Self>) {
        *self.self_weak.lock() = Some(Arc::downgrade(self));
    }

    /// 获取或初始化新 PageCache（懒初始化，线程安全）
    fn get_new_page_cache(&self) -> Arc<NewPageCache> {
        let mut cache_opt = self.new_page_cache.lock();
        if let Some(ref pc) = *cache_opt {
            return pc.clone();
        }
        let backend = Arc::new(FatPageCacheBackend::new(
            self.fs.clone(),
            self.self_weak
                .lock()
                .as_ref()
                .expect("FatInode::init_self_weak must be called before get_new_page_cache"),
        ));
        let pc = NewPageCache::new();
        pc.set_backend(backend);
        *cache_opt = Some(pc.clone());
        pc
    }
}

impl Drop for FatInode {
    /// 在删除该inode之前，写回脏页并更新父目录
    fn drop(&mut self) {
        // 写回新 PageCache 的所有脏页（简单且正确：直接写回全部）
        if let Some(ref pc) = *self.new_page_cache.lock() {
            let _ = pc.writeback_all();
        }

        if *self.deleted.lock() {
            // Clear size
            let mut lock = self.file_content.write();
            let length = lock.clus_list.len();
            self.dealloc_clus(&mut lock, length);
        } else {
            if self.parent_dir.lock().is_none() {
                return;
            }
            let par_dir_lock = self.parent_dir.lock();
            let (parent_dir, offset) = par_dir_lock.as_ref().unwrap();

            let par_inode_lock = parent_dir.write();
            let dir_ent = parent_dir.get_dir_ent(&par_inode_lock, *offset).unwrap();
            let mut short_dir_ent = *dir_ent.get_short_ent().unwrap();
            // Modify size
            short_dir_ent.file_size = self.get_file_size();
            // Modify fst cluster
            short_dir_ent.set_fst_clus(
                self.get_first_clus_lock(&self.file_content.read())
                    .unwrap_or(0),
            );
            // Modify time
            // todo!
            log::debug!("[Inode drop]: new_ent: {:?}", short_dir_ent);
            // Write back
            parent_dir
                .set_dir_ent(&par_inode_lock, *offset, dir_ent)
                .unwrap();
        }
    }
}

/// 构造函数
impl FatInode {
    /// Inode 的构造函数
    /// # 参数
    /// + `fst_clus`: 文件的第一个簇
    /// + `file_type`: 文件类型
    /// + `size`: NOTE: the `size` field should be set to `None` for a directory
    /// + `parent_dir`: 父目录
    /// + `fs`: 文件系统实例
    /// # 返回值
    /// 指向inode的指针
    pub fn new(
        fst_clus: u32,
        file_type: DiskInodeType,
        size: Option<u32>,
        parent_dir: Option<(Arc<Self>, u32)>,
        fs: Arc<EasyFileSystem>,
    ) -> Arc<Self> {
        let clus_list = match fst_clus {
            0 => Vec::new(),
            _ => fs.fat.get_all_clus_num(fst_clus, &fs.block_device),
        };

        let size = size.unwrap_or_else(|| clus_list.len() as u32 * fs.clus_size());
        let hint = 0;

        let file_content = RwLock::new(FileContent {
            size,
            clus_list,
            hint,
        });
        let parent_dir = Mutex::new(parent_dir);
        let time = InodeTime::new();
        let inode = Arc::new(FatInode {
            inode_lock: RwLock::new(InodeLock {}),
            file_content,
            new_page_cache: Mutex::new(None),
            self_weak: Mutex::new(None),
            file_type: Mutex::new(file_type),
            parent_dir,
            fs,
            time: Mutex::new(time),
            deleted: Mutex::new(false),
        });
        // Arc 构造完成后，初始化 self_weak
        inode.init_self_weak();

        // 初始化 hint
        if file_type == DiskInodeType::Directory {
            inode.set_hint();
        }
        inode
    }
}

/// 基本功能
impl FatInode {
    /// 获取第一个簇
    /// # 参数
    /// + `lock`: 目标文件内容的锁
    /// # 返回值
    /// 如果簇列表非空，会返回第一个簇
    /// 否则返回空
    fn get_first_clus_lock(&self, lock: &RwLockReadGuard<FileContent>) -> Option<u32> {
        // 获取簇列表
        let clus_list = &lock.clus_list;
        // 非空返回第一个簇号
        if !clus_list.is_empty() {
            Some(clus_list[0])
        } else {
            None
        }
    }
    /// 获取根据大小向上取整后所需的簇数
    /// # 返回值
    /// The number representing the number of clusters
    fn total_clus(&self, size: u32) -> u32 {
        //size.div_ceil(self.fs.clus_size())
        let clus_sz = self.fs.clus_size();
        div_ceil!(size, clus_sz)
        //(size - 1 + clus_sz) / clus_sz
    }

    /// 用于新 PageCache 后端：通过闭包访问簇列表（持读锁期间）
    pub(crate) fn with_clus_list<R>(&self, f: impl FnOnce(&[u32]) -> R) -> R {
        let lock = self.file_content.read();
        f(&lock.clus_list)
    }

}

/// File Content Operation
/// 文件内容操作相关方法
impl FatInode {
    /// 分配需要的簇
    /// 需要尽可能多的分配簇，然后追加到`lock`中的`clus_list`中
    /// # 参数
    /// + `lock`: 目标文件内容（锁）
    /// + `alloc_num`: 需要分配的簇数
    fn alloc_clus(&self, lock: &mut RwLockWriteGuard<FileContent>, alloc_num: usize) {
        let clus_list = &mut lock.clus_list;
        let mut new_clus_list = self.fs.fat.alloc(
            &self.fs.block_device,
            alloc_num,
            clus_list.last().map(|clus| *clus),
        );
        clus_list.append(&mut new_clus_list);
    }
    /// 从lock中的clus_list释放一定数量的簇
    /// 当要释放的数量超过可用数量时，`clus_list` 会被清空
    /// # 参数
    /// + `lock`: 目标文件内容（锁）
    /// + `dealloc_num`: 需要释放的簇数
    fn dealloc_clus(&self, lock: &mut RwLockWriteGuard<FileContent>, dealloc_num: usize) {
        let clus_list = &mut lock.clus_list;
        let dealloc_num = dealloc_num.min(clus_list.len());
        let mut dealloc_list = Vec::<u32>::with_capacity(dealloc_num);
        for _ in 0..dealloc_num {
            dealloc_list.push(clus_list.pop().unwrap());
        }
        self.fs.fat.free(
            &self.fs.block_device,
            dealloc_list,
            clus_list.last().map(|x| *x),
        );
    }
}

/// Directory Operation
impl FatInode {
    /// A Constructor for `DirIter`(See `dir_iter.rs/DirIter` for details).
    /// # Arguments
    /// + `inode_lock`: The lock of inode
    /// + `offset`: The start offset of iterator
    /// + `mode`: The mode of iterator
    /// + `forward`: The direction of the iterator iteration
    /// # Return Value
    /// Pointer to iterator
    fn dir_iter<'a, 'b>(
        &'a self,
        inode_lock: &'a RwLockWriteGuard<'b, InodeLock>,
        offset: Option<u32>,
        mode: DirIterMode,
        forward: bool,
    ) -> DirIter<'a, 'b> {
        debug_assert!(self.is_dir(), "this isn't a directory");
        DirIter::new(inode_lock, offset, mode, forward, self)
    }
    /// Set the offset of the last entry in the directory file(first byte is 0x00) to hint
    fn set_hint(&self) {
        let inode_lock = self.write();
        let mut iter = self.dir_iter(&inode_lock, None, DirIterMode::Enum, FORWARD);
        loop {
            let dir_ent = iter.next();
            if dir_ent.is_none() {
                // Means iter reachs the end of file
                let mut lock = self.file_content.write();
                lock.hint = lock.size;
                return;
            }
            let dir_ent = dir_ent.unwrap();
            if dir_ent.last_and_unused() {
                let mut lock = self.file_content.write();
                lock.hint = iter.get_offset().unwrap();
                return;
            }
        }
    }
    /// Check if current file is an empty directory
    /// If a file contains only "." and "..", we consider it to be an empty directory
    /// # Arguments
    /// + `inode_lock`: The lock of inode
    /// # Return Value
    /// Bool result
    /// Expand directory file's size(a cluster)
    /// # Arguments
    /// + `inode_lock`: The lock of inode
    /// # Return Value
    /// Default is Ok
    fn expand_dir_size(&self, inode_lock: &RwLockWriteGuard<InodeLock>) -> Result<(), ()> {
        let diff_size = self.fs.clus_size();
        self.modify_size_lock(inode_lock, diff_size as isize, false);
        Ok(())
    }
    /// Shrink directory file's size to fit `hint`.
    /// For directory files, it has at least one cluster, which should be noted.
    /// # Arguments
    /// + `inode_lock`: The lock of inode
    /// # Return Value
    /// Default is Ok
    fn shrink_dir_size(&self, inode_lock: &RwLockWriteGuard<InodeLock>) -> Result<(), ()> {
        let lock = self.file_content.read();
        let new_size = div_ceil!(lock.hint, self.fs.clus_size())
            .mul(self.fs.clus_size())
            .max(self.fs.clus_size());
        /*lock
        .hint
        .div_ceil(self.fs.clus_size())
        .mul(self.fs.clus_size())
        // For directory file, it has at least one cluster
        .max(self.fs.clus_size());*/
        let diff_size = new_size as isize - lock.size as isize;
        drop(lock);
        self.modify_size_lock(inode_lock, diff_size as isize, false);
        Ok(())
    }
    /// Allocate directory entries required for new file.
    /// The allocated directory entries is a contiguous segment.
    /// # Arguments
    /// + `inode_lock`: The lock of inode
    /// + `alloc_num`: Required number of directory entries
    /// # Return Value
    /// It will return lock anyway.
    /// If successful, it will also return the offset of the last allocated entry.
    fn alloc_dir_ent(
        &self,
        inode_lock: &RwLockWriteGuard<InodeLock>,
        alloc_num: usize,
    ) -> Result<u32, ()> {
        let offset = self.file_content.read().hint;
        let mut iter = self.dir_iter(inode_lock, None, DirIterMode::Enum, FORWARD);
        iter.set_iter_offset(offset);
        let mut found_free_dir_ent = 0;
        loop {
            let dir_ent = iter.next();
            if dir_ent.is_none() {
                if self.expand_dir_size(&mut iter.inode_lock).is_err() {
                    log::error!("[alloc_dir_ent]expand directory size error");
                    return Err(());
                }
                continue;
            }
            // We assume that all entries after `hint` are valid
            // That's why we use `hint`. It can reduce the cost of iterating over used entries
            found_free_dir_ent += 1;
            if found_free_dir_ent >= alloc_num {
                let offset = iter.get_offset().unwrap();
                // Set hint
                // Set next entry to last_and_unused
                if iter.next().is_some() {
                    iter.write_to_current_ent(&FATDirEnt::unused_and_last_entry());
                    let mut lock = self.file_content.write();
                    lock.hint = iter.get_offset().unwrap();
                } else {
                    // Means iter reachs the end of file
                    let mut lock = self.file_content.write();
                    lock.hint = lock.size;
                }
                return Ok(offset);
            }
        }
    }
    /// Get a directory entries.
    /// # Arguments
    /// + `inode_lock`: The lock of inode
    /// + `offset`: The offset of entry
    /// # Return Value
    /// If successful, it will return a `FATDirEnt`(See `layout.rs/FATDirEnt` for details)
    /// Otherwise, it will return Error
    /// # Warning
    /// This function will lock self's `file_content`, may cause deadlock
    fn get_dir_ent(
        &self,
        inode_lock: &RwLockWriteGuard<InodeLock>,
        offset: u32,
    ) -> Result<FATDirEnt, ()> {
        let mut dir_ent = FATDirEnt::empty();
        if self.read_at_block_cache_wlock(inode_lock, offset as usize, dir_ent.as_bytes_mut())
            != dir_ent.as_bytes().len()
        {
            return Err(());
        }
        Ok(dir_ent)
    }
    /// Write the directory entry back to the file contents.
    /// # Arguments
    /// + `inode_lock`: The lock of inode
    /// + `offset`: The offset of file to write
    /// + `dir_ent`: The buffer needs to write back
    /// # Return Value
    /// If successful, it will return Ok.
    /// Otherwise, it will return Error.
    /// # Warning
    /// This function will lock self's `file_content`, may cause deadlock
    fn set_dir_ent(
        &self,
        inode_lock: &RwLockWriteGuard<InodeLock>,
        offset: u32,
        dir_ent: FATDirEnt,
    ) -> Result<(), ()> {
        if self.write_at_block_cache_lock(inode_lock, offset as usize, dir_ent.as_bytes())
            != dir_ent.as_bytes().len()
        {
            return Err(());
        }
        Ok(())
    }
    /// Get directory entries, including short and long entries
    /// # Arguments
    /// + `inode_lock`: The lock of inode
    /// + `offset`: The offset of short entry
    /// # Return Value
    /// If successful, it returns a pair of a short directory entry and a long directory entry list.
    /// Otherwise, it will return Error.
    /// # Warning
    /// This function will lock self's `file_content`, may cause deadlock
    fn get_all_dir_ent(
        &self,
        inode_lock: &RwLockWriteGuard<InodeLock>,
        offset: u32,
    ) -> Result<(FATShortDirEnt, Vec<FATLongDirEnt>), ()> {
        debug_assert!(self.is_dir());
        let short_ent: FATShortDirEnt;
        let mut long_ents = Vec::<FATLongDirEnt>::with_capacity(5);

        let mut iter = self.dir_iter(inode_lock, Some(offset), DirIterMode::Enum, BACKWARD);

        short_ent = *iter.current_clone().unwrap().get_short_ent().unwrap();

        // Check if this directory entry is only a short directory entry
        {
            let dir_ent = iter.next();
            // First directory entry
            if dir_ent.is_none() {
                return Ok((short_ent, long_ents));
            }
            let dir_ent = dir_ent.unwrap();
            // Short directory entry
            if !dir_ent.is_long() {
                return Ok((short_ent, long_ents));
            }
        }

        // Get long dir_ents
        loop {
            let dir_ent = iter.current_clone();
            if dir_ent.is_none() {
                return Err(());
            }
            let dir_ent = dir_ent.unwrap();
            if dir_ent.get_long_ent().is_none() {
                return Err(());
            }
            long_ents.push(*dir_ent.get_long_ent().unwrap());
            if dir_ent.is_last_long_dir_ent() {
                break;
            }
        }
        Ok((short_ent, long_ents))
    }
    /// Delete derectory entries, including short and long entries.
    /// # Arguments
    /// + `inode_lock`: The lock of inode
    /// + `offset`: The offset of short entry
    /// # Return Value
    /// If successful, it will return Ok.
    /// Otherwise, it will return Error.
    /// # Warning
    /// This function will lock self's `file_content`, may cause deadlock.
    fn delete_dir_ent(
        &self,
        inode_lock: &RwLockWriteGuard<InodeLock>,
        offset: u32,
    ) -> Result<(), ()> {
        debug_assert!(self.is_dir());
        let mut iter = self.dir_iter(inode_lock, Some(offset), DirIterMode::Used, BACKWARD);

        iter.write_to_current_ent(&FATDirEnt::unused_not_last_entry());
        // Check if this directory entry is only a short directory entry
        {
            let dir_ent = iter.next();
            // First directory entry
            if dir_ent.is_none() {
                return Ok(());
            }
            let dir_ent = dir_ent.unwrap();
            // Short directory entry
            if !dir_ent.is_long() {
                return Ok(());
            }
        }
        // Remove long dir_ents
        loop {
            let dir_ent = iter.current_clone();
            if dir_ent.is_none() {
                return Err(());
            }
            let dir_ent = dir_ent.unwrap();
            if !dir_ent.is_long() {
                return Err(());
            }
            iter.write_to_current_ent(&FATDirEnt::unused_not_last_entry());
            iter.next();
            if dir_ent.is_last_long_dir_ent() {
                break;
            }
        }
        // Modify hint
        // We use new iterate mode
        let mut iter = self.dir_iter(
            inode_lock,
            Some(self.file_content.read().hint),
            DirIterMode::Enum,
            BACKWARD,
        );
        loop {
            let dir_ent = iter.next();
            if dir_ent.is_none() {
                // Indicates that the file is empty
                self.file_content.write().hint = 0;
                break;
            }
            let dir_ent = dir_ent.unwrap();
            if dir_ent.unused() {
                self.file_content.write().hint = iter.get_offset().unwrap();
                iter.write_to_current_ent(&FATDirEnt::unused_and_last_entry());
            } else {
                // Represents `iter` pointer to a used entry
                break;
            }
        }
        // Modify file size
        self.shrink_dir_size(inode_lock)
    }
    /// Create new disk space for derectory entries, including short and long entries.
    /// # Arguments
    /// + `inode_lock`: The lock of inode
    /// + `short_ent`: short entry
    /// + `long_ents`: list of long entries
    /// # Return Value
    /// If successful, it will return Ok.
    /// Otherwise, it will return Error.
    /// # Warning
    /// This function will lock self's `file_content`, may cause deadlock.
    fn create_dir_ent(
        &self,
        inode_lock: &RwLockWriteGuard<InodeLock>,
        short_ent: FATShortDirEnt,
        long_ents: Vec<FATLongDirEnt>,
    ) -> Result<u32, ()> {
        debug_assert!(self.is_dir());
        let short_ent_offset = match self.alloc_dir_ent(inode_lock, 1 + long_ents.len()) {
            Ok(offset) => offset,
            Err(_) => return Err(()),
        };
        // We have graranteed we have alloc enough entries
        // So we use Enum mode
        let mut iter = self.dir_iter(
            inode_lock,
            Some(short_ent_offset),
            DirIterMode::Enum,
            BACKWARD,
        );

        iter.write_to_current_ent(&FATDirEnt {
            short_entry: short_ent,
        });
        for long_ent in long_ents {
            iter.next();
            iter.write_to_current_ent(&FATDirEnt {
                long_entry: long_ent,
            });
        }
        Ok(short_ent_offset)
    }
    /// Modify current directory file's ".." directory entry
    /// # Arguments
    /// + `inode_lock`: The lock of inode
    /// + `parent_dir_clus_num`: The first cluster number of the parent directory
    /// # Return Value
    /// If successful, it will return Ok.
    /// Otherwise, it will return Error.
    /// # Warning
    /// This function will lock self's `file_content`, may cause deadlock
    fn modify_parent_dir_entry(
        &self,
        inode_lock: &RwLockWriteGuard<InodeLock>,
        parent_dir_clus_num: u32,
    ) -> Result<(), ()> {
        debug_assert!(self.is_dir());
        let mut iter = self.dir_iter(inode_lock, None, DirIterMode::Used, FORWARD);
        loop {
            let dir_ent = iter.next();
            if dir_ent.is_none() {
                break;
            }
            let mut dir_ent = dir_ent.unwrap();
            if dir_ent.get_name() == ".." {
                dir_ent.set_fst_clus(parent_dir_clus_num);
                iter.write_to_current_ent(&dir_ent);
                return Ok(());
            }
        }
        Err(())
    }
}

/// Create
impl FatInode {
    /// Construct short and long entries
    /// # Arguments
    /// + `parent_dir`: The pointer to parent directory
    /// + `parent_inode_lock`: the lock of parent's inode
    /// + `name`: File name
    /// + `fst_clus`: The first cluster of constructing file
    /// + `file_type`: The file type of constructing file
    /// # Return Value
    /// A pair of a short directory entry and a list of long name entries
    /// # Warning
    /// This function will lock the `file_content` of the parent directory, may cause deadlock
    fn gen_dir_ent(
        parent_dir: &Arc<Self>,
        parent_inode_lock: &RwLockWriteGuard<InodeLock>,
        name: &String,
        fst_clus: u32,
        file_type: DiskInodeType,
    ) -> (FATShortDirEnt, Vec<FATLongDirEnt>) {
        // Generate name slices
        let (short_name_slice, long_name_slices) =
            Self::gen_name_slice(parent_dir, parent_inode_lock, &name);
        // Generate short entry
        let short_ent = FATShortDirEnt::from_name(short_name_slice, fst_clus, file_type);
        // Generate long entries
        let long_ent_num = long_name_slices.len();
        let long_ents = long_name_slices
            .iter()
            .enumerate()
            .map(|(i, slice)| FATLongDirEnt::from_name_slice(i + 1 == long_ent_num, i + 1, *slice))
            .collect();
        (short_ent, long_ents)
    }

    /// 从一个目录项创建文件.
    /// # 参数
    /// + `parent_dir`: the parent directory inode pointer
    /// + `ent`: the short entry as the source of information
    /// + `offset`: the offset of the short directory entry in the `parent_dir`
    /// # 返回值
    /// 指向Inode的指针
    pub fn from_fat_ent(parent_dir: &Arc<Self>, ent: &FATShortDirEnt, offset: u32) -> Arc<Self> {
        Self::new(
            ent.get_first_clus(),
            if ent.is_dir() {
                DiskInodeType::Directory
            } else {
                DiskInodeType::File
            },
            if ent.is_file() {
                Some(ent.file_size)
            } else {
                None
            },
            Some((parent_dir.clone(), offset)),
            parent_dir.fs.clone(),
        )
    }

    /// Fill out an empty directory with only the '.' & '..' entries.
    /// # Arguments
    /// + `parent_dir`: the pointer of parent directory inode
    /// + `current_dir`: the pointer of new directory inode
    /// + `fst_clus`: the first cluster number of current file
    fn fill_empty_dir(parent_dir: &Arc<Self>, current_dir: &Arc<Self>, fst_clus: u32) {
        let current_inode_lock = current_dir.write();
        let mut iter = current_dir.dir_iter(&current_inode_lock, None, DirIterMode::Enum, FORWARD);
        let mut short_name: [u8; 11] = [' ' as u8; 11];
        //.
        iter.next();
        short_name[0] = '.' as u8;
        iter.write_to_current_ent(&FATDirEnt {
            short_entry: FATShortDirEnt::from_name(
                short_name,
                fst_clus as u32,
                DiskInodeType::Directory,
            ),
        });
        //..
        iter.next();
        short_name[1] = '.' as u8;
        iter.write_to_current_ent(&FATDirEnt {
            short_entry: FATShortDirEnt::from_name(
                short_name,
                parent_dir
                    .get_first_clus_lock(&parent_dir.file_content.read())
                    .unwrap(),
                DiskInodeType::Directory,
            ),
        });
        //add "unused and last" sign
        iter.next();
        iter.write_to_current_ent(&FATDirEnt::unused_and_last_entry());
    }
}

// ls and find local
impl FatInode {
    /// ls - General Purose file filterer
    /// # Arguments
    /// + `inode_lock`: The lock of inode
    /// # WARNING
    /// The definition of OFFSET is CHANGED for this item.
    /// It should point to the NEXT USED entry whether it as a long entry whenever possible or a short entry if no long ones exist.
    /// # Return value
    /// On success, the function returns `Ok(_)`. On failure, multiple chances exist: either the Vec is empty, or the Result is `Err(())`.
    /// # Implementation Information
    /// The iterator stops at the last available item when it reaches the end,
    /// returning `None` from then on,
    /// so relying on the offset of the last item to decide whether it has reached an end is not recommended.
    #[inline(always)]
    pub fn ls_lock(
        &self,
        inode_lock: &RwLockWriteGuard<InodeLock>,
    ) -> Result<Vec<(String, FATShortDirEnt)>, ()> {
        if !self.is_dir() {
            return Err(());
        }
        Ok(self
            .dir_iter(inode_lock, None, DirIterMode::Used, FORWARD)
            .walk()
            .collect())
    }
    /// find `req_name` in current directory file
    /// # Arguments
    /// + `inode_lock`: The lock of inode
    /// + `req_name`: required file name
    /// # Return value
    /// On success, the function returns `Ok(_)`. On failure, multiple chances exist: either the Vec is empty, or the Result is `Err(())`.
    pub fn find_local_lock(
        &self,
        inode_lock: &RwLockWriteGuard<InodeLock>,
        req_name: String,
    ) -> Result<Option<(String, FATShortDirEnt, u32)>, ()> {
        if !self.is_dir() {
            return Err(());
        }
        log::debug!("[find_local] name: {:?}", req_name);
        let mut walker = self
            .dir_iter(inode_lock, None, DirIterMode::Used, FORWARD)
            .walk();
        match walker.find(|(name, _)| {
            name.len() == req_name.len() && name.as_str().eq_ignore_ascii_case(req_name.as_str())
        }) {
            Some((name, short_ent)) => {
                log::trace!("[find_local] Query name: {} found", req_name);
                Ok(Some((name, short_ent, walker.iter.get_offset().unwrap())))
            }
            None => {
                log::trace!("[find_local] Query name: {} not found", req_name);
                Ok(None)
            }
        }
    }
}

impl FatInode {
    /// Get self's file content lock
    /// # Return Value
    /// a lock of file content
    #[inline(always)]
    pub fn read(&self) -> RwLockReadGuard<InodeLock> {
        self.inode_lock.read()
    }
    #[inline(always)]
    pub fn write(&self) -> RwLockWriteGuard<InodeLock> {
        self.inode_lock.write()
    }
    /// Get file type
    #[inline(always)]
    pub fn get_file_type(&self) -> DiskInodeType {
        *self.file_type.lock()
    }
    #[inline(always)]
    pub fn get_file_size_wlock(&self, _inode_lock: &RwLockWriteGuard<InodeLock>) -> u32 {
        self.get_file_size()
    }
    #[inline(always)]
    pub fn get_file_size(&self) -> u32 {
        self.file_content.read().get_file_size()
    }
    /// Check if file type is directory
    /// # Return Value
    /// Bool result
    #[inline(always)]
    pub fn is_dir(&self) -> bool {
        self.get_file_type() == DiskInodeType::Directory
    }
    /// Check if file type is file
    /// # Return Value
    /// Bool result
    #[inline(always)]
    pub fn is_file(&self) -> bool {
        self.get_file_type() == DiskInodeType::File
    }
    /// 获取Inode号
    /// 方便起见，将第一个扇区号作为inode号
    /// # 参数
    /// + `lock`: The lock of target file content
    /// # 返回值
    /// If cluster list isn't empty, it will return the first sector number.
    /// Otherwise it will return None.
    #[inline(always)]
    pub fn get_inode_num_lock(&self, lock: &RwLockReadGuard<FileContent>) -> Option<u32> {
        self.get_first_clus_lock(lock)
            .map(|clus| self.fs.first_sector_of_cluster(clus))
    }
    /// do same thing as read_at_block_cache_rlock but params different
    pub fn read_at_block_cache_wlock(
        &self,
        _inode_lock: &RwLockWriteGuard<InodeLock>,
        offset: usize,
        buf: &mut [u8],
    ) -> usize {
        let size = self.file_content.read().size as usize;
        let end = (offset + buf.len()).min(size);
        if offset >= end {
            return 0;
        }
        let read_len = end - offset;
        let pc = self.get_new_page_cache();
        match pc.read(offset, &mut buf[..read_len]) {
            Ok(n) => n,
            Err(_) => 0,
        }
    }

    /// 将缓冲区内容写入文件内
    /// 这将会从offset指定的偏移量位置开始写直到buffer写完。
    /// 并且当写入的大小超过文件结束位置的时候，将会修改文件的大小。
    /// # 参数
    /// + `inode_lock`: inode锁
    /// + `offset`: The start offset in file
    /// + `buf`: The buffer to write data
    /// # 返回值
    /// The number of number of bytes write.
    pub fn write_at_block_cache_lock(
        &self,
        inode_lock: &RwLockWriteGuard<InodeLock>,
        offset: usize,
        buf: &[u8],
    ) -> usize {
        let old_size = self.get_file_size() as usize;
        let diff_len = buf.len() as isize + offset as isize - old_size as isize;
        if diff_len > 0 as isize {
            self.modify_size_lock(inode_lock, diff_len, false);
        }
        let write_end = (offset + buf.len()).min(self.get_file_size() as usize);
        if offset >= write_end {
            return 0;
        }
        let write_len = write_end - offset;
        let pc = self.get_new_page_cache();
        match pc.write(offset, &buf[..write_len]) {
            Ok(n) => n,
            Err(_) => 0,
        }
    }

    /// Delete the short and the long entry of `self` from `parent_dir`
    /// # 返回值
    /// 执行成功返回Ok
    /// 否则返回Err
    /// # 警告
    /// 这个函数会给parent_dir上锁，可能会导致死锁
    pub fn delete_self_dir_ent(&self) -> Result<(), ()> {
        if let Some((par_inode, offset)) = &*self.parent_dir.lock() {
            return par_inode.delete_dir_ent(&par_inode.write(), *offset);
        }
        Err(())
    }

    /// Delete the file from the disk.
    /// deallocating both the directory entries and the occupied clusters.
    pub fn unlink_lock(
        &self,
        _inode_lock: &RwLockWriteGuard<InodeLock>,
        delete: bool,
    ) -> Result<(), isize> {
        log::debug!(
            "[delete_from_disk] inode: {:?}, type: {:?}",
            self.get_inode_num_lock(&self.file_content.read()),
            self.file_type
        );
        // Remove directory entries
        if self.parent_dir.lock().is_none() {
            return Ok(());
        }
        if self.delete_self_dir_ent().is_err() {
            panic!()
        }
        if delete {
            *self.deleted.lock() = true;
        }
        *self.parent_dir.lock() = None;
        Ok(())
    }

    /// 改变当前文件的大小
    /// This operation is ignored if the result size is negative
    /// # 参数
    /// + `inode_lock`: inode锁
    /// + `diff`: file 大小的改变量
    pub fn modify_size_lock(&self, _inode_lock: &RwLockWriteGuard<InodeLock>, diff: isize, _clear: bool) {
        let mut lock = self.file_content.write();

        debug_assert!(diff.saturating_add(lock.size as isize) >= 0);

        let old_size = lock.size;
        let new_size = (lock.size as isize + diff) as u32;

        let old_clus_num = self.total_clus(old_size) as usize;
        let new_clus_num = self.total_clus(new_size) as usize;

        if diff > 0 {
            self.alloc_clus(&mut lock, new_clus_num - old_clus_num);
        } else {
            self.dealloc_clus(&mut lock, old_clus_num - new_clus_num);
        }

        lock.size = new_size;
    }

    pub fn is_empty_dir_lock(&self, inode_lock: &RwLockWriteGuard<InodeLock>) -> bool {
        if !self.is_dir() {
            return false;
        }
        let iter = self
            .dir_iter(inode_lock, None, DirIterMode::Used, FORWARD)
            .walk();
        for (name, _) in iter {
            if [".", ".."].contains(&name.as_str()) == false {
                return false;
            }
        }
        true
    }

    /// Construct a \[u8,11\] corresponding to the short directory entry name
    pub fn gen_short_name_slice(
        parent_dir: &Arc<Self>,
        parent_inode_lock: &RwLockWriteGuard<InodeLock>,
        name: &String,
    ) -> [u8; 11] {
        let short_name = FATDirEnt::gen_short_name_prefix(name.clone());
        if short_name.len() == 0 || short_name.find(' ').unwrap_or(8) == 0 {
            panic!("illegal short name");
        }

        let mut short_name_slice = [0u8; 11];
        short_name_slice.copy_from_slice(&short_name.as_bytes()[0..11]);

        let iter = parent_dir.dir_iter(parent_inode_lock, None, DirIterMode::Short, FORWARD);
        FATDirEnt::gen_short_name_numtail(iter.collect(), &mut short_name_slice);
        short_name_slice
    }
    /// Construct short and long entries name slices
    pub fn gen_name_slice(
        parent_dir: &Arc<Self>,
        parent_inode_lock: &RwLockWriteGuard<InodeLock>,
        name: &String,
    ) -> ([u8; 11], Vec<[u16; 13]>) {
        let short_name_slice = Self::gen_short_name_slice(parent_dir, parent_inode_lock, name);

        let long_ent_num = div_ceil!(name.len(), 13);
        let mut long_name_slices = Vec::<[u16; 13]>::with_capacity(long_ent_num);
        for i in 0..long_ent_num {
            long_name_slices.push(Self::gen_long_name_slice(name, i));
        }

        (short_name_slice, long_name_slices)
    }
    /// Construct a \[u16,13\] corresponding to the `long_ent_num`'th 13-u16 or shorter name slice
    pub fn gen_long_name_slice(name: &String, long_ent_index: usize) -> [u16; 13] {
        let mut v: Vec<u16> = name.encode_utf16().collect();
        debug_assert!(long_ent_index * 13 < v.len());
        while v.len() < (long_ent_index + 1) * 13 {
            v.push(0);
        }
        let start = long_ent_index * 13;
        let end = (long_ent_index + 1) * 13;
        v[start..end].try_into().expect("should be able to cast")
    }
}

// ── 新 VFS 辅助方法 ──────────────────────────────────────────────────────

/// 在指定父目录下创建新文件/目录（内部辅助函数）
fn fat_do_create(
    parent: &Arc<FatInode>,
    name: &str,
    file_type: FileType,
) -> Result<Arc<FatInode>, ()> {
    let disk_type = vfs_type_to_fat_disk_type(file_type);

    if !parent.is_dir() || name.len() >= 256 {
        return Err(());
    }

    // 先获取 inode 锁（遵循锁顺序：inode_lock → FAT lock）
    let parent_inode_lock = parent.write();

    // 为目录分配首簇（inode_lock 已持有，FAT 锁在 fat.alloc 内部获取）
    let fst_clus = if disk_type == DiskInodeType::Directory {
        let clus = parent.fs.fat.alloc(&parent.fs.block_device, 1, None);
        if clus.is_empty() {
            return Err(());
        }
        clus[0]
    } else {
        0
    };

    let (short_ent, long_ents) = FatInode::gen_dir_ent(
        parent,
        &parent_inode_lock,
        &name.to_string(),
        fst_clus,
        disk_type,
    );

    let short_ent_offset =
        parent.create_dir_ent(&parent_inode_lock, short_ent, long_ents)?;

    let current_file = FatInode::from_fat_ent(parent, &short_ent, short_ent_offset);

    if disk_type == DiskInodeType::Directory {
        current_file.file_content.write().hint =
            2 * core::mem::size_of::<FATDirEnt>() as u32;
        FatInode::fill_empty_dir(parent, &current_file, fst_clus);
    }

    log::debug!(
        "[fat_do_create] parent_inode: {:?}, name: {:?}, file_type: {:?}",
        parent.get_inode_num_lock(&parent.file_content.read()),
        name,
        disk_type
    );

    Ok(current_file)
}

// ── IndexNode trait 实现 ─────────────────────────────────────────────────

impl IndexNode for FatInode {
    fn read_at(
        &self,
        offset: usize,
        len: usize,
        buf: &mut [u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        let file_size = self.get_file_size() as usize;
        let end = (offset + len).min(file_size);
        if offset >= end {
            return Ok(0);
        }
        let read_len = end - offset;
        let pc = self.get_new_page_cache();
        pc.read(offset, &mut buf[..read_len]).map_err(|_| SyscallErr::EIO)
    }

    fn write_at(
        &self,
        offset: usize,
        len: usize,
        buf: &[u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        let write_len = len.min(buf.len());
        if write_len == 0 {
            return Ok(0);
        }
        let old_size = self.get_file_size() as usize;
        let diff = write_len as isize + offset as isize - old_size as isize;
        if diff > 0 {
            let inode_lock = self.write();
            self.modify_size_lock(&inode_lock, diff, false);
        }
        let write_end = (offset + write_len).min(self.get_file_size() as usize);
        if offset >= write_end {
            return Ok(0);
        }
        let actual_len = write_end - offset;
        let pc = self.get_new_page_cache();
        pc.write(offset, &buf[..actual_len]).map_err(|_| SyscallErr::EIO)
    }

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        let file_type = self.get_file_type();
        let ft = fat_disk_type_to_vfs_type(file_type);
        let time = self.time.lock();
        Ok(Metadata {
            dev_id: 0,
            inode_id: self
                .get_inode_num_lock(&self.file_content.read())
                .unwrap_or(0) as InodeId,
            size: self.get_file_size() as i64,
            blk_size: self.fs.byts_per_sec as usize,
            blocks: self.get_file_size() as usize / self.fs.byts_per_sec as usize,
            atime: TimeSpec {
                tv_sec: *time.access_time() as usize,
                tv_nsec: 0,
            },
            mtime: TimeSpec {
                tv_sec: *time.modify_time() as usize,
                tv_nsec: 0,
            },
            ctime: TimeSpec {
                tv_sec: *time.create_time() as usize,
                tv_nsec: 0,
            },
            file_type: ft,
            mode: InodeMode::S_IRWXUGO,
            nlinks: 1,
            uid: 0,
            gid: 0,
            flags: InodeFlags::empty(),
            raw_dev: 0,
        })
    }

    fn find(&self, name: &str) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        if !self.is_dir() {
            return Err(SyscallErr::ENOTDIR);
        }
        let self_arc = self
            .self_weak
            .lock()
            .as_ref()
            .and_then(|w| w.upgrade())
            .ok_or(SyscallErr::EIO)?;
        let inode_lock = self_arc.write();
        match self.find_local_lock(&inode_lock, name.to_string()) {
            Ok(Some((_, short_ent, offset))) => {
                let child = FatInode::from_fat_ent(&self_arc, &short_ent, offset);
                Ok(child)
            }
            _ => Err(SyscallErr::ENOENT),
        }
    }

    fn get_entry_name(&self, ino: InodeId) -> Result<String, SyscallErr> {
        if !self.is_dir() {
            return Err(SyscallErr::ENOTDIR);
        }
        // Check "." — the directory's own inode_id
        let self_ino = self
            .get_inode_num_lock(&self.file_content.read())
            .unwrap_or(0) as InodeId;
        if self_ino == ino {
            return Ok(alloc::string::String::from("."));
        }
        // Iterate directory entries and match by inode_id
        let inode_lock = self.write();
        for (name, short_ent) in self
            .dir_iter(&inode_lock, None, DirIterMode::Used, FORWARD)
            .walk()
        {
            let child_ino = self.fs.first_sector_of_cluster(short_ent.get_first_clus()) as InodeId;
            if child_ino == ino {
                return Ok(name);
            }
        }
        // Check ".." — parent's inode_id
        let parent_ino = self
            .find("..")
            .and_then(|p| p.metadata())
            .map(|m| m.inode_id)
            .unwrap_or(0);
        if parent_ino == ino {
            return Ok(alloc::string::String::from(".."));
        }
        Err(SyscallErr::ENOENT)
    }

    fn list(&self) -> Result<alloc::vec::Vec<String>, SyscallErr> {
        if !self.is_dir() {
            return Err(SyscallErr::ENOTDIR);
        }
        let inode_lock = self.write();
        self.ls_lock(&inode_lock)
            .map(|entries| entries.into_iter().map(|(name, _)| name).collect())
            .map_err(|_| SyscallErr::EIO)
    }

    fn create(
        &self,
        name: &str,
        file_type: FileType,
        _mode: InodeMode,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        let self_arc = self
            .self_weak
            .lock()
            .as_ref()
            .and_then(|w| w.upgrade())
            .ok_or(SyscallErr::EIO)?;
        fat_do_create(&self_arc, name, file_type)
            .map(|child| child as Arc<dyn IndexNode>)
            .map_err(|_| SyscallErr::EIO)
    }

    fn unlink(&self, name: &str) -> Result<(), SyscallErr> {
        let child = self.find(name)?;
        let child_fat = child
            .as_any_ref()
            .downcast_ref::<FatInode>()
            .ok_or(SyscallErr::EIO)?;
        let md = child.metadata()?;
        if md.file_type == FileType::Dir {
            return Err(SyscallErr::EISDIR);
        }
        let child_arc = child_fat
            .self_weak
            .lock()
            .as_ref()
            .and_then(|w| w.upgrade())
            .ok_or(SyscallErr::EIO)?;
        let inode_lock = child_arc.write();
        child_arc
            .unlink_lock(&inode_lock, true)
            .map_err(|_| SyscallErr::EIO)
    }

    fn rmdir(&self, name: &str) -> Result<(), SyscallErr> {
        let child = self.find(name)?;
        let child_fat = child
            .as_any_ref()
            .downcast_ref::<FatInode>()
            .ok_or(SyscallErr::EIO)?;
        let md = child.metadata()?;
        if md.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        let child_arc = child_fat
            .self_weak
            .lock()
            .as_ref()
            .and_then(|w| w.upgrade())
            .ok_or(SyscallErr::EIO)?;
        let inode_lock = child_arc.write();
        if !child_arc.is_empty_dir_lock(&inode_lock) {
            return Err(SyscallErr::ENOTEMPTY);
        }
        child_arc
            .unlink_lock(&inode_lock, true)
            .map_err(|_| SyscallErr::EIO)
    }

    fn resize(&self, len: usize) -> Result<(), SyscallErr> {
        let old_size = self.get_file_size() as usize;
        if len == old_size {
            return Ok(());
        }
        let diff = len as isize - old_size as isize;
        let inode_lock = self.write();
        self.modify_size_lock(&inode_lock, diff, false);
        // 截断新 PageCache 超出新大小的页面
        if len < old_size {
            if let Some(ref pc) = *self.new_page_cache.lock() {
                let _ = pc.truncate(len);
            }
        }
        Ok(())
    }

    fn fs(&self) -> Arc<dyn NewFileSystem> {
        self.fs.clone()
    }

    fn page_cache(&self) -> Option<Arc<super::super::page_cache::PageCache>> {
        Some(self.get_new_page_cache())
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}
