#![allow(unused)]
use core::arch::asm;
use core::ptr::addr_of;

use super::block_group::{Block, Ext4BlockGroup};
use super::direntry::{Ext4DirEntry, Ext4DirSearchResult};
use super::path::path_check;
use super::superblock::SUPERBLOCK_OFFSET;
use super::*;
use super::{superblock::Ext4Superblock, BlockCacheManager, BlockDevice, Cache};
use crate::drivers::BLOCK_DEVICE;
use crate::fs::cache::BufferCache;
use crate::fs::ext4::error::{Errno, Ext4Error};
use crate::fs::filesystem::FS_Type;
use crate::hal::BLOCK_SZ;
use alloc::{sync::Arc, vec::Vec};
use layout::Ext4OSInode;
use spin::Mutex;
type SuperBlock = Ext4Superblock;

/// Ext4文件系统对象实例
pub struct Ext4FileSystem {
    /// 块设备
    pub block_device: Arc<dyn BlockDevice>,
    /// 超级块信息
    pub superblock: SuperBlock,
    /// 块大小
    pub block_size: usize,
    /// 缓存管理器
    pub cache_mgr: Arc<Mutex<BlockCacheManager>>,
    /// Weak self-reference，用于从 &self 获取 Arc<Self>
    __self_ref: spin::Mutex<alloc::sync::Weak<Ext4FileSystem>>,
}

impl Ext4FileSystem {
    // Opens and loads an Ext4 from the `block_device`.
    // 针对ext4rs原有的方法的方法，可能需要修改
    pub fn open_ext4rs(
        block_device: Arc<dyn BlockDevice>,
        index_cache_mgr: Arc<Mutex<BlockCacheManager>>,
    ) -> Arc<Self> {
        // 读取超级块
        let block = Block::load_superblock(block_device.clone(), 0);
        let superblock = block.read_offset_as_superblock(SUPERBLOCK_OFFSET);
        let block_size = superblock.clone().block_size() as usize;
        let cache_mgr = index_cache_mgr.clone();
        Arc::new_cyclic(|weak| Ext4FileSystem {
            block_device,
            superblock,
            block_size,
            cache_mgr,
            __self_ref: spin::Mutex::new(weak.clone()),
        })
    }
    /// with dir result search path offset
    /// # 参数
    /// + path: 路径
    /// + parent_inode_num: 父目录Inode节点号
    /// + create: 是否创建目标文件
    /// + ftype: 文件类型
    /// + name_off: 路径中当前处理部分的偏移量,用来记录已经处理的路径部分的偏移量
    /// # 返回值
    /// + 目标文件的Inode节点号
    pub fn generic_open(
        &self,
        path: &str,
        parent_inode_num: &mut u32,
        create: bool,
        ftype: u16,
        name_off: &mut u32,
    ) -> Result<u32, isize> {
        let mut is_goal = false;

        let mut parent = parent_inode_num;

        let mut search_path = path;

        let mut dir_search_result = Ext4DirSearchResult::new(Ext4DirEntry::default());

        loop {
            // 路径可能包含多个斜杠
            // 每遇到一个就跳过一个，并将偏移量 name_off 加 1
            while search_path.starts_with('/') {
                *name_off += 1; // Skip the slash
                search_path = &search_path[1..];
            }
            // 使用 path_check 检查当前路径，并返回当前部分的长度 len
            let len = path_check(search_path, &mut is_goal);

            // 路径中的当前部分
            // 比如usr
            // 或者lib
            // 亦或者1.txt之类的
            let current_path = &search_path[..len];

            // 路径长度若为 0 或者路径为空
            // 退出
            if len == 0 || search_path.is_empty() {
                break;
            }

            search_path = &search_path[len..];

            // 使用dir_find_entry查找当前父目录下是否存在current_path对应的文件或者目录
            let r = self.dir_find_entry(*parent, current_path, &mut dir_search_result);
            match r {
                Ok(_) => {
                    println!(
                        "[kernel generic_open] Find in parent {:x?} r {:?} name {:?}",
                        parent, r, current_path
                    );
                }
                Err(errno) => {
                    //println!("[failed in ext4fs generic_open function!] {:?}", errno)
                }
            }

            // 查找失败
            if let Err(e) = r {
                if e.error() != Errno::ENOENT || !create {
                    println!("[kernel generic_open] No such file or directory");
                }

                // 创建新 inode
                let mut inode_mode = 0;
                if is_goal {
                    inode_mode = ftype;
                } else {
                    inode_mode = InodeFileType::S_IFDIR.bits();
                }

                let new_inode_ref = self.create(*parent, current_path, inode_mode)?;

                // Update parent the new inode
                *parent = new_inode_ref.inode_num;

                // Update dir_search_result to reflect the new inode
                dir_search_result.dentry.inode = new_inode_ref.inode_num;

                continue;
            }

            if is_goal {
                break;
            } else {
                // 更新父目录Inode节点号
                *parent = dir_search_result.dentry.inode;
            }
            *name_off += len as u32;
        }

        // 下面的两行好像一模一样？？？？
        // 目标文件已找到时退出
        // 返回找到的inode号
        if is_goal {
            return Ok(dir_search_result.dentry.inode);
        }

        Ok(dir_search_result.dentry.inode)
    }
    pub fn open_old(
        block_device: Arc<dyn BlockDevice>,
        index_cache_mgr: Arc<Mutex<BlockCacheManager>>,
    ) -> Arc<Self> {
        // 块缓存管理器
        // 读取的数据会被缓存，也就是说放在内存中
        // 这样下次再读取的时候就不用再从磁盘中读取了
        // 速度会快很多
        let ext4_cache_mgr = index_cache_mgr.clone();
        index_cache_mgr
            .lock()
            // 获取第0块的缓存
            .get_block_cache(0, &block_device)
            .lock()
            // 获取超级块
            .read(SUPERBLOCK_OFFSET, |super_block: &SuperBlock| {
                // 创建ext4实例
                let ext4fs = Self {
                    block_device,
                    superblock: super_block.clone(),
                    block_size: super_block.block_size() as usize,
                    cache_mgr: ext4_cache_mgr,
                    __self_ref: spin::Mutex::new(alloc::sync::Weak::new()),
                };
                ext4fs.test_info();
                let ext4_arc = Arc::new(ext4fs);
                *ext4_arc.__self_ref.lock() = Arc::downgrade(&ext4_arc);
                ext4_arc
            })
    }
    pub fn alloc_blocks(&self, blocks: usize) -> Vec<usize> {
        if blocks == 0 {
            return Vec::new();
        }

        let sblk = &self.superblock;
        let blocks_per_group = sblk.blocks_per_group() as usize;
        let bg_count = sblk.block_group_count() as usize;

        for bgid in 0..bg_count {
            let mut bg = Ext4BlockGroup::load_new(self.block_device.clone(), sblk, bgid, self.block_size);
            let free = bg.get_free_blocks_count() as usize;
            if free < blocks {
                continue;
            }

            let bmp_blk = bg.get_block_bitmap_block(sblk) as usize;
            let bmp = Block::load_offset(self.block_device.clone(), bmp_blk * self.block_size, self.block_size);
            let bit_cnt = blocks_per_group.min(bmp.data.len() * 8);

            // Find a contiguous range of free blocks
            let mut run_start: Option<usize> = None;
            let mut run_len = 0;
            for idx in 0..bit_cnt {
                if crate::fs::ext4::bitmap::ext4_bmap_is_bit_clr(&bmp.data, idx as u32) {
                    if run_start.is_none() {
                        run_start = Some(idx);
                    }
                    run_len += 1;
                    if run_len >= blocks {
                        let start = run_start.unwrap();
                        // Mark blocks as used in bitmap
                        let mut data = bmp.data.clone();
                        for i in start..start + blocks {
                            crate::fs::ext4::bitmap::ext4_bmap_bit_set(&mut data, i as u32);
                        }
                        // Update csum & write bitmap back
                        bg.set_block_group_balloc_bitmap_csum(sblk, &data);
                        // log::warn!("[WRITE_CALLER] alloc_blocks: write block_bitmap block={}, start={}, len={}", bmp_blk, run_start.unwrap(), blocks);
                        self.block_device.write_block(bmp_blk, &data);

                        // Update block group free count
                        bg.set_free_blocks_count((free - blocks) as u32);
                        let mut sb = *sblk;
                        let sb_free = sb.free_blocks_count();
                        sb.set_free_blocks_count(sb_free - blocks as u64);
                        sb.sync_to_disk_with_csum(self.block_device.clone());
                        bg.sync_to_disk_with_csum(self.block_device.clone(), bgid, &sb, self.block_size);

                        let base = self.get_block_of_bgid(bgid as u32) as usize + start;
                        return (base..base + blocks).collect();
                    }
                } else {
                    run_start = None;
                    run_len = 0;
                }
            }
        }

        println!(
            "[ext4 alloc_blocks] Cannot find {} contiguous free blocks, returning empty",
            blocks
        );
        Vec::new()
    }
    #[allow(unused)]
    pub fn dir_mk(&self, path: &str) -> Result<usize, isize> {
        let mut nameoff = 0;

        let filetype = InodeFileType::S_IFDIR;

        // todo get this path's parent

        // start from root
        let mut parent = ROOT_INODE;

        let r = self.generic_open(path, &mut parent, true, filetype.bits(), &mut nameoff);
        Ok(EOK)
    }
    pub fn unlink(
        &self,
        parent: &mut Ext4InodeRef,
        child: &mut Ext4InodeRef,
        name: &str,
    ) -> Result<usize, isize> {
        log::debug!(
            "[debug_low_unlink] entering: parent_ino={}, child_ino={}, name={}",
            parent.inode_num,
            child.inode_num,
            name
        );
        log::debug!(
            "[debug_low_unlink] parent_mode={:#o}, child_mode={:#o}",
            parent.inode.mode,
            child.inode.mode
        );
        self.dir_remove_entry(parent, name)?;

        let is_dir = child.inode.is_dir();

        self.ialloc_free_inode(child.inode_num, is_dir);

        Ok(EOK)
    }
}

impl Ext4FileSystem {
    pub fn get_superblock_test(block_device: Arc<dyn BlockDevice>) -> Ext4Superblock {
        let superblock_pre = Block::load_offset(block_device, 0, 4096);
        let superblock: Ext4Superblock = superblock_pre.read_offset_as(1024);
        superblock
    }

    pub fn get_superblock(&self) -> Ext4Superblock {
        self.superblock
    }

    pub fn get_block_group(&self, blk_grp_idx: usize) -> Ext4BlockGroup {
        let block_device = self.block_device.clone();
        Ext4BlockGroup::load_new(block_device, &self.superblock, blk_grp_idx, self.block_size)
    }

    pub fn print_block_group(&self, blk_grp_idx: usize) {
        let blk_per_grp = self.superblock.blocks_per_group();
        let blk_per_grp = blk_per_grp as usize;
        // inode表长
        let inode_size = self.superblock.inode_size();
        let inodes_per_grp = self.superblock.inodes_per_group;
        let ino_table_len = (inodes_per_grp as usize) * (inode_size as usize) / self.block_size;
        self.get_block_group(blk_grp_idx).dump_block_group_info(
            blk_grp_idx,
            blk_per_grp,
            ino_table_len,
        );
    }
    fn test_info(&self) {
        self.superblock.dump_info();
        self.print_block_group(0);
        self.print_block_group(1);
        self.print_block_group(2);
        self.print_block_group(3);
        // 尝试比较超级块内容
        assert!(self.superblock == Ext4FileSystem::get_superblock_test(BLOCK_DEVICE.clone()));
        // self.test_get_file("remove.lua");
        // self.test_get_file("/remove.lua");
        // self.test_get_file("/busybox_cmd.txt");
        // self.test_get_file("/1.txt");
        // println!("Finish the test");
    }
}

// ── 旧 VFS trait（过渡用，FAT32 迁移后删除） ──────────────────────────

impl crate::fs::directory_tree::VFS for Ext4FileSystem {
    fn alloc_blocks(&self, blocks: usize) -> alloc::vec::Vec<usize> {
        self.alloc_blocks(blocks)
    }
    fn get_filesystem_type(&self) -> FS_Type {
        FS_Type::Ext4
    }
    fn block_size(&self) -> usize {
        self.block_size
    }
}

// ── 新 VFS trait 实现 ────────────────────────────────────────────────

use crate::fs::inode::InodeLock;
use crate::fs::vfs::file_system::{FileSystem as NewFileSystem, FsInfo, SuperBlock as VfsSuperBlock};
use crate::fs::vfs::file::FileFlags as VfsFileFlags;
use crate::fs::vfs::index_node::IndexNode;
use crate::fs::vfs::{
    FilePrivateData, FileType as VfsFileType, InodeFlags, InodeId, InodeMode, Metadata,
};
use crate::timer::TimeSpec;
use crate::utils::error::SyscallErr;

impl layout::Ext4OSInode {
    /// 创建新 VFS 兼容的 Ext4OSInode（内部使用，接收具体类型）
    pub fn new_vfs(
        inode_ref: alloc::sync::Arc<spin::Mutex<Ext4InodeRef>>,
        ext4fs: alloc::sync::Arc<Ext4FileSystem>,
    ) -> alloc::sync::Arc<dyn IndexNode> {
        alloc::sync::Arc::new(Self {
            inode_lock: alloc::sync::Arc::new(spin::RwLock::new(InodeLock {})),
            readable: true,
            writable: true,
            special_use: true,
            append: false,
            inode: inode_ref,
            offset: spin::Mutex::new(0),
            ext4fs,
            file_cache_manager: alloc::sync::Arc::new(PageCacheManager::new()),
        })
    }
}

fn disk_inode_to_vfs_type(ft: InodeFileType) -> VfsFileType {
    match ft {
        InodeFileType::S_IFREG => VfsFileType::File,
        InodeFileType::S_IFDIR => VfsFileType::Dir,
        InodeFileType::S_IFLNK => VfsFileType::SymLink,
        InodeFileType::S_IFCHR => VfsFileType::CharDevice,
        InodeFileType::S_IFBLK => VfsFileType::BlockDevice,
        InodeFileType::S_IFSOCK => VfsFileType::Socket,
        InodeFileType::S_IFIFO => VfsFileType::Pipe,
        _ => VfsFileType::File,
    }
}

fn vfs_type_to_inode_mode(ft: VfsFileType) -> u16 {
    match ft {
        VfsFileType::File => InodeFileType::S_IFREG.bits(),
        VfsFileType::Dir => InodeFileType::S_IFDIR.bits(),
        VfsFileType::SymLink => InodeFileType::S_IFLNK.bits(),
        VfsFileType::CharDevice => InodeFileType::S_IFCHR.bits(),
        VfsFileType::BlockDevice => InodeFileType::S_IFBLK.bits(),
        VfsFileType::Socket => InodeFileType::S_IFSOCK.bits(),
        VfsFileType::Pipe => InodeFileType::S_IFIFO.bits(),
        _ => InodeFileType::S_IFREG.bits(),
    }
}

impl IndexNode for layout::Ext4OSInode {
    fn read_at(
        &self,
        offset: usize,
        len: usize,
        buf: &mut [u8],
        _data: spin::MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        let inode_num = self.inode.lock().inode_num;
        let read_len = len.min(buf.len());
        self.ext4fs
            .read_at(inode_num, offset, &mut buf[..read_len])
            .map_err(|_| SyscallErr::EIO)
    }

    fn write_at(
        &self,
        offset: usize,
        len: usize,
        buf: &[u8],
        _data: spin::MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        let inode_num = self.inode.lock().inode_num;
        let write_len = len.min(buf.len());
        self.ext4fs
            .write_at(inode_num, offset, &buf[..write_len])
            .map_err(|_| SyscallErr::EIO)?;
        Ok(write_len)
    }

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        let inode_lock = self.inode.lock();
        let inode = &inode_lock.inode;
        let ft = inode.file_type();
        Ok(Metadata {
            dev_id: 0,
            inode_id: inode_lock.inode_num as InodeId,
            size: inode.size() as i64,
            blk_size: self.ext4fs.block_size,
            blocks: inode.blocks_count() as usize,
            atime: TimeSpec {
                tv_sec: inode.atime() as usize,
                tv_nsec: 0,
            },
            mtime: TimeSpec {
                tv_sec: inode.mtime() as usize,
                tv_nsec: 0,
            },
            ctime: TimeSpec {
                tv_sec: inode.ctime() as usize,
                tv_nsec: 0,
            },
            file_type: disk_inode_to_vfs_type(ft),
            mode: InodeMode::from_bits_truncate(inode.mode() as u32),
            flags: InodeFlags::empty(),
            nlinks: inode.links_count() as u64,
            uid: inode.uid() as u32,
            gid: inode.gid() as u32,
            raw_dev: 0,
        })
    }

    fn find(&self, name: &str) -> Result<alloc::sync::Arc<dyn IndexNode>, SyscallErr> {
        let inode_num = self.inode.lock().inode_num;
        let mut result = Ext4DirSearchResult::new(Ext4DirEntry::default());
        self.ext4fs
            .dir_find_entry(inode_num, name, &mut result)
            .map_err(|_| SyscallErr::ENOENT)?;
        let child_ref = self.ext4fs.get_inode_ref(result.dentry.inode);
        Ok(layout::Ext4OSInode::new_vfs(
            alloc::sync::Arc::new(spin::Mutex::new(child_ref)),
            self.ext4fs.clone(),
        ))
    }

    fn list(&self) -> Result<alloc::vec::Vec<alloc::string::String>, SyscallErr> {
        let inode_num = self.inode.lock().inode_num;
        let entries = self
            .ext4fs
            .dir_get_entries(inode_num)
            .map_err(|_| SyscallErr::EIO)?;
        let names: alloc::vec::Vec<alloc::string::String> = entries
            .iter()
            .map(|e| e.get_name())
            .filter(|n| n != "." && n != "..")
            .collect();
        Ok(names)
    }

    fn create(
        &self,
        name: &str,
        file_type: VfsFileType,
        _mode: InodeMode,
    ) -> Result<alloc::sync::Arc<dyn IndexNode>, SyscallErr> {
        let parent = self.inode.lock().inode_num;
        let inode_mode = vfs_type_to_inode_mode(file_type);
        let new_ref = self
            .ext4fs
            .create(parent, name, inode_mode)
            .map_err(|e| {
                if e == crate::syscall::errno::ENOENT {
                    SyscallErr::ENOENT
                } else if e == crate::syscall::errno::EEXIST {
                    SyscallErr::EEXIST
                } else {
                    SyscallErr::ENOSYS
                }
            })?;
        Ok(layout::Ext4OSInode::new_vfs(
            alloc::sync::Arc::new(spin::Mutex::new(new_ref)),
            self.ext4fs.clone(),
        ))
    }

    fn symlink(
        &self,
        name: &str,
        target: &str,
    ) -> Result<alloc::sync::Arc<dyn IndexNode>, SyscallErr> {
        let parent = self.inode.lock().inode_num;
        let inode_mode = InodeFileType::S_IFLNK.bits();
        let new_ref = self
            .ext4fs
            .create(parent, name, inode_mode)
            .map_err(|_| SyscallErr::ENOSYS)?;
        // 写入符号链接目标
        let target_bytes = target.as_bytes();
        self.ext4fs
            .write_at(new_ref.inode_num, 0, target_bytes)
            .map_err(|_| SyscallErr::EIO)?;
        Ok(layout::Ext4OSInode::new_vfs(
            alloc::sync::Arc::new(spin::Mutex::new(new_ref)),
            self.ext4fs.clone(),
        ))
    }

    fn link(
        &self,
        _name: &str,
        _other: &alloc::sync::Arc<dyn IndexNode>,
    ) -> Result<(), SyscallErr> {
        Err(SyscallErr::ENOSYS) // ext4 硬链接暂时不支持
    }

    fn unlink(&self, name: &str) -> Result<(), SyscallErr> {
        let parent_num = self.inode.lock().inode_num;
        // 通过 dir_find_entry 找到子 inode
        let mut result = Ext4DirSearchResult::new(Ext4DirEntry::default());
        self.ext4fs
            .dir_find_entry(parent_num, name, &mut result)
            .map_err(|_| SyscallErr::ENOENT)?;
        let child_num = result.dentry.inode;
        let mut child_ref = self.ext4fs.get_inode_ref(child_num);
        self.ext4fs
            .unlink(
                &mut self.inode.lock(),
                &mut child_ref,
                name,
            )
            .map_err(|_| SyscallErr::EIO)?;
        Ok(())
    }

    fn rmdir(&self, name: &str) -> Result<(), SyscallErr> {
        // 先检查是否为目录
        let mut result = Ext4DirSearchResult::new(Ext4DirEntry::default());
        let parent_num = self.inode.lock().inode_num;
        self.ext4fs
            .dir_find_entry(parent_num, name, &mut result)
            .map_err(|_| SyscallErr::ENOENT)?;
        let child_ref = self.ext4fs.get_inode_ref(result.dentry.inode);
        if !child_ref.inode.is_dir() {
            return Err(SyscallErr::ENOTDIR);
        }
        // 检查目录是否为空
        let entries = self
            .ext4fs
            .dir_get_entries(result.dentry.inode)
            .map_err(|_| SyscallErr::EIO)?;
        let non_dot = entries
            .iter()
            .filter(|e| {
                let n = e.get_name();
                n != "." && n != ".."
            })
            .count();
        if non_dot > 0 {
            return Err(SyscallErr::ENOTEMPTY);
        }
        // 删除
        self.ext4fs
            .unlink(
                &mut self.inode.lock(),
                &mut self.ext4fs.get_inode_ref(result.dentry.inode),
                name,
            )
            .map_err(|_| SyscallErr::EIO)?;
        Ok(())
    }

    fn resize(&self, len: usize) -> Result<(), SyscallErr> {
        let mut inode_ref = self.inode.lock();
        self.ext4fs
            .truncate_inode(&mut inode_ref, len as u64)
            .map_err(|_| SyscallErr::EIO)?;
        Ok(())
    }

    fn fs(&self) -> alloc::sync::Arc<dyn NewFileSystem> {
        self.ext4fs.clone() as alloc::sync::Arc<dyn NewFileSystem>
    }

    fn as_any_ref(&self) -> &dyn core::any::Any {
        self
    }
}

impl core::fmt::Debug for Ext4FileSystem {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Ext4FileSystem")
            .field("block_size", &self.block_size)
            .finish()
    }
}

impl NewFileSystem for Ext4FileSystem {
    fn root_inode(&self) -> alloc::sync::Arc<dyn crate::fs::vfs::IndexNode> {
        let self_arc = self
            .__self_ref
            .lock()
            .upgrade()
            .expect("Ext4FileSystem::root_inode called but fs not in Arc");
        let root_ref = self.get_inode_ref(ROOT_INODE);
        layout::Ext4OSInode::new_vfs(
            alloc::sync::Arc::new(spin::Mutex::new(root_ref)),
            self_arc,
        )
    }

    fn info(&self) -> FsInfo {
        FsInfo {
            blk_dev_id: 0,
            max_name_len: 255,
            features: alloc::vec!["ext4", "extent", "sparse"],
        }
    }

    fn name(&self) -> &str {
        "ext4"
    }

    fn super_block(&self) -> VfsSuperBlock {
        let sb = &self.superblock;
        VfsSuperBlock {
            f_type: 0xef53,
            f_bsize: sb.block_size() as u64,
            f_blocks: sb.blocks_count() as u64,
            f_bfree: sb.free_blocks_count(),
            f_bavail: sb.free_blocks_count(),
            f_files: sb.total_inodes() as u64,
            f_ffree: sb.free_inodes_count() as u64,
            f_fsid: [0xef53, 0],
            f_namelen: 255,
            f_frsize: sb.block_size() as u64,
            flags: 0,
            f_spare: [0; 4],
        }
    }

    fn as_any_ref(&self) -> &dyn core::any::Any {
        self
    }
}
