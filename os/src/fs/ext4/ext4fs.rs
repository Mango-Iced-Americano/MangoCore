#![allow(unused)]
use core::arch::asm;
use core::ptr::addr_of;

use super::block_group::{Block, Ext4BlockGroup};
use super::direntry::{Ext4DirEntry, Ext4DirSearchResult};
use super::meta_cache::MetaBlockCache;
use super::path::path_check;
use super::superblock::SUPERBLOCK_OFFSET;
use super::*;
use super::{superblock::Ext4Superblock, BlockDevice};
use crate::drivers::BLOCK_DEVICE;
use crate::fs::ext4::error::{Errno, Ext4Error};
use crate::fs::filesystem::FS_Type;
use crate::hal::BLOCK_SZ;
use alloc::{string::String, sync::Arc, sync::Weak, vec::Vec};
use alloc::collections::BTreeMap;
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
    /// Weak self-reference，用于从 &self 获取 Arc<Self>
    __self_ref: spin::Mutex<alloc::sync::Weak<Ext4FileSystem>>,
    /// 以 inode_num 为键的共享 PageCache 注册表，同一文件的所有 Ext4OSInode 共享同一缓存
    pub(super) page_caches: spin::Mutex<BTreeMap<u32, Weak<crate::fs::page_cache::PageCache>>>,

    // ── Phase 1: VFS inode object cache (framework-only) ─────────────────
    //
    // 设计参考:
    //   DragonOS 没有全局 inode_objects cache，主要靠 parent.children 持有 child inode。
    //   本字段是 MangoCore 增强，不强求全局唯一化 inode object。
    //
    // 当前用途:
    //   create / symlink / mkdir 后将新 inode 插入（弱引用）。
    //   find() / lookup 时优先从此表查找已有 VFS inode object。
    //
    // 已知限制 (不强制唯一化，不当前解决):
    //   1. hardlink: 同一个 ino 有多个路径别名，此表不绑定唯一 parent。
    //   2. rename 跨目录: 旧 parent.children 移除但 ino 仍可从此表找到。
    //   3. unlink (links_count > 0): inode object 不应移除（仍有其他 link 指向）。
    //   4. 内存泄漏: Weak 升级失败需惰性清理（后续 Phase 可加周期清理）。
    //
    // 当前策略: Weak 引用，不强引用。inode 生命周期仍由 parent.children 主导。
    /// 全局 VFS inode object 弱引用表 (ino → Weak<dyn IndexNode>)
    pub(super) inode_objects: spin::Mutex<BTreeMap<u32, alloc::sync::Weak<dyn crate::fs::vfs::IndexNode>>>,

    // ── Phase 4: 底层 ext4 inode table 读缓存 ──
    // 减少 get_inode_ref 的磁盘 I/O。write_back_inode 改为先更新缓存再写回。
    pub(super) inode_cache: spin::Mutex<BTreeMap<u32, alloc::sync::Arc<spin::Mutex<super::ext4_inode::CachedExt4Inode>>>>,

    /// 元数据块缓存：inode table / bitmap / dir block / group desc / superblock 脏写合并。
    pub(crate) meta_block_cache: MetaBlockCache,

    // ── Phase 5: metadata defer mode ──
    // 用于 prepare 阶段批量创建 symlink/file 时减少 superblock + group desc 重复写。
    // 仅在 begin_meta_batch() 后生效；普通 syscall 路径默认关闭。
    pub(super) meta_batch_active: core::sync::atomic::AtomicBool,
    pub(super) meta_batch_sb: spin::Mutex<Option<super::superblock::Ext4Superblock>>,
    pub(super) meta_batch_bgs: spin::Mutex<alloc::collections::BTreeMap<u32, super::block_group::Ext4BlockGroup>>,
}

/// 全局 Ext4FileSystem 引用（用于 syscall 触发的 batch mode）
pub static GLOBAL_EXT4FS: spin::Mutex<Option<alloc::sync::Weak<Ext4FileSystem>>> = spin::Mutex::new(None);

impl Ext4FileSystem {
    pub(crate) fn read_metadata_block(&self, block_id: usize) -> Vec<u8> {
        let bd = self.block_device.clone();
        self.meta_block_cache.read_block(block_id, |id, data| {
            bd.read_block(id, data);
            super::counters::inc_counter!(super::counters::BLOCK_READ_TOTAL);
            super::counters::inc_counter!(super::counters::METADATA_BLOCK_READ_COUNT);
        })
    }

    pub(crate) fn load_metadata_block(&self, block_id: usize) -> Block {
        Block {
            disk_offset: block_id * self.block_size,
            data: self.read_metadata_block(block_id),
            block_size: self.block_size,
        }
    }

    pub(crate) fn load_metadata_block_offset(&self, offset: usize) -> Block {
        self.load_metadata_block(offset / self.block_size)
    }

    pub(crate) fn with_metadata_block_mut(&self, block_id: usize, f: impl FnOnce(&mut [u8])) {
        let bd = self.block_device.clone();
        self.meta_block_cache.with_block_mut(
            block_id,
            |id, data| {
                bd.read_block(id, data);
                super::counters::inc_counter!(super::counters::BLOCK_READ_TOTAL);
                super::counters::inc_counter!(super::counters::METADATA_BLOCK_READ_COUNT);
            },
            f,
        );
    }

    pub(crate) fn store_metadata_block_dirty(&self, block_id: usize, data: &[u8]) {
        self.meta_block_cache.store_dirty_block(block_id, data);
    }

    pub fn flush_metadata_cache(&self) {
        // Flush dirty inodes first — they hold modified cached inode data
        // that may not yet be written back to metadata blocks
        self.flush_dirty_inodes();
        let bd = self.block_device.clone();
        self.meta_block_cache.flush_all_dirty(|block_id, data| {
            bd.write_block(block_id, data);
        });
    }

    pub(crate) fn sync_inode_to_metadata_cache(
        &self,
        inode: &super::ext4_inode::Ext4Inode,
        inode_pos: usize,
        on_disk_size: usize,
        inode_num: u32,
    ) {
        if inode_num == 3266 {
            log::warn!(
                "[WRITE_TRACE] Writing Ino 3266! Mode: 0o{:o}, Size: {}, FirstBlock: {}",
                inode.mode,
                inode.size,
                inode.block[0]
            );
        }
        let write_len = core::cmp::min(core::mem::size_of::<super::ext4_inode::Ext4Inode>(), on_disk_size);
        let data = unsafe { core::slice::from_raw_parts(inode as *const _ as *const u8, write_len) };
        let block_id = inode_pos / self.block_size;
        let offset = inode_pos % self.block_size;
        log::warn!(
            "[WRITE_CALLER] sync_inode_to_metadata_cache: ino={}, block_id={}, offset={}, mode=0o{:o}, size={}",
            inode_num,
            block_id,
            offset,
            inode.mode,
            inode.size()
        );
        self.with_metadata_block_mut(block_id, |buf| {
            buf[offset..offset + write_len].copy_from_slice(data);
        });
        super::counters::inc_counter!(super::counters::INODE_TABLE_READ);
        super::counters::inc_counter!(super::counters::INODE_TABLE_WRITE);
    }

    pub(crate) fn sync_superblock_to_metadata_cache(&self, sb: &mut super::superblock::Ext4Superblock) {
        let data = unsafe {
            core::slice::from_raw_parts(sb as *const _ as *const u8, core::mem::size_of::<super::superblock::Ext4Superblock>())
        };
        let checksum = super::crc::ext4_crc32c(super::crc::EXT4_CRC32_INIT, data, 0x3fc);
        sb.checksum = checksum;
        let data = unsafe {
            core::slice::from_raw_parts(sb as *const _ as *const u8, core::mem::size_of::<super::superblock::Ext4Superblock>())
        };
        let superblk_id = super::SUPERBLOCK_OFFSET / self.block_size;
        let superblk_offset = super::SUPERBLOCK_OFFSET % self.block_size;
        self.with_metadata_block_mut(superblk_id, |buf| {
            buf[superblk_offset..superblk_offset + data.len()].copy_from_slice(data);
        });
        super::counters::inc_counter!(super::counters::SUPERBLOCK_READ);
        super::counters::inc_counter!(super::counters::SUPERBLOCK_WRITE);
    }

    pub(crate) fn sync_block_group_to_metadata_cache(
        &self,
        bg: &super::block_group::Ext4BlockGroup,
        bgid: usize,
        super_block: &super::superblock::Ext4Superblock,
    ) {
        let dsc_cnt = self.block_size / super_block.desc_size as usize;
        let dsc_id = bgid / dsc_cnt;
        let block_id = super_block.first_data_block as usize + dsc_id + 1;
        let offset = (bgid % dsc_cnt) * super_block.desc_size as usize;
        let data = unsafe {
            core::slice::from_raw_parts(bg as *const _ as *const u8, core::mem::size_of::<super::block_group::Ext4BlockGroup>())
        };
        self.with_metadata_block_mut(block_id, |buf| {
            buf[offset..offset + data.len()].copy_from_slice(data);
        });
        super::counters::inc_counter!(super::counters::GROUP_DESC_READ);
        super::counters::inc_counter!(super::counters::GROUP_DESC_WRITE);
    }

    pub(crate) fn load_block_group_cached(
        &self,
        super_block: &super::superblock::Ext4Superblock,
        block_group_idx: usize,
    ) -> super::block_group::Ext4BlockGroup {
        let dsc_cnt = self.block_size / super_block.desc_size as usize;
        let dsc_id = block_group_idx / dsc_cnt;
        let block_id = super_block.first_data_block as usize + dsc_id + 1;
        let offset = (block_group_idx % dsc_cnt) * super_block.desc_size as usize;
        let ext4block = self.load_metadata_block(block_id);
        super::counters::inc_counter!(super::counters::GROUP_DESC_READ);
        ext4block.read_offset_as(offset)
    }

    // Opens and loads an Ext4 from the `block_device`.
    // 针对ext4rs原有的方法的方法，可能需要修改
    pub fn open_ext4rs(
        block_device: Arc<dyn BlockDevice>,
    ) -> Arc<Self> {
        // 读取超级块
        let block = Block::load_superblock(block_device.clone(), 0);
        let superblock = block.read_offset_as_superblock(SUPERBLOCK_OFFSET);
        let block_size = superblock.clone().block_size() as usize;
        Arc::new_cyclic(|weak| {
            let fs = Ext4FileSystem {
                block_device,
                superblock,
                block_size,
                __self_ref: spin::Mutex::new(weak.clone()),
                page_caches: spin::Mutex::new(BTreeMap::new()),
                inode_objects: spin::Mutex::new(BTreeMap::new()),
                inode_cache: spin::Mutex::new(BTreeMap::new()),
                meta_block_cache: MetaBlockCache::new(256, block_size),
                meta_batch_active: core::sync::atomic::AtomicBool::new(false),
                meta_batch_sb: spin::Mutex::new(None),
                meta_batch_bgs: spin::Mutex::new(alloc::collections::BTreeMap::new()),
            };
            *GLOBAL_EXT4FS.lock() = Some(weak.clone());
            fs
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
    /// 确保指定逻辑块范围都已分配物理块（nodelalloc 策略：写入前分配）
    /// 返回分配了块的逻辑块号列表（用于日志/调试）
    pub fn ensure_blocks_allocated(
        &self,
        inode_ref: &mut Ext4InodeRef,
        start_lblock: u32,
        end_lblock: u32,
    ) -> Result<Vec<u32>, isize> {
        let mut allocated = Vec::new();
        for lblock in start_lblock..end_lblock {
            if self.get_pblock_idx(inode_ref, lblock).is_err() {
                // Block not yet mapped — allocate and insert extent
                self.insert_inode_pblk_deferred(inode_ref, lblock)?;
                allocated.push(lblock);
            }
        }
        Ok(allocated)
    }

    pub fn alloc_blocks(&self, blocks: usize) -> Vec<usize> {
        if blocks == 0 {
            return Vec::new();
        }

        let sblk = &self.superblock;
        let blocks_per_group = sblk.blocks_per_group() as usize;
        let bg_count = sblk.block_group_count() as usize;

        for bgid in 0..bg_count {
            let mut bg = self.load_block_group_cached(sblk, bgid);
            let free = bg.get_free_blocks_count() as usize;
            if free < blocks {
                continue;
            }

            let bmp_blk = bg.get_block_bitmap_block(sblk) as usize;
            let bmp = self.load_metadata_block(bmp_blk);
            super::counters::inc_counter!(super::counters::BLOCK_BITMAP_READ);
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
                        self.store_metadata_block_dirty(bmp_blk, &data);
                        super::counters::inc_counter!(super::counters::BLOCK_BITMAP_WRITE);

                        // Update block group free count
                        bg.set_free_blocks_count((free - blocks) as u32);
                        let mut sb = *sblk;
                        let sb_free = sb.free_blocks_count();
                        sb.set_free_blocks_count(sb_free - blocks as u64);
                        self.defer_superblock_write(&sb);
                        self.defer_bg_write(&bg, bgid as u32, &sb);

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

        // 不立即释放 inode：MAP_SHARED mmap / open fd 仍可能持有引用。
        // 改为递减 links_count。当 links_count 归零时，最后一个引用释放
        // 后由 Ext4OSInode::Drop 统一回收 inode 号和数据块。
        let links = child.inode.links_count();
        if links > 0 {
            child.inode.set_links_count(links - 1);
        }

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
        self.load_block_group_cached(&self.superblock, blk_grp_idx)
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
            new_page_cache: spin::Mutex::new(None),
            children: spin::Mutex::new(alloc::collections::BTreeMap::new()),
            negative_dentry: spin::Mutex::new(alloc::collections::BTreeMap::new()),
            dir_version: core::sync::atomic::AtomicU64::new(0),
            cached_file_size: core::sync::atomic::AtomicU64::new(u64::MAX),
            cached_symlink_target: spin::Mutex::new(None),
            metadata_dirty: core::sync::atomic::AtomicBool::new(false),
        })
    }

    fn bump_dir_version(&self) -> u64 {
        self.dir_version
            .fetch_add(1, core::sync::atomic::Ordering::Relaxed)
            .wrapping_add(1)
    }

    fn clear_negative_dentry(&self, name: &str) {
        self.negative_dentry.lock().remove(name);
    }

    fn insert_negative_dentry(&self, name: &str, version: u64) {
        self.negative_dentry
            .lock()
            .insert(alloc::string::String::from(name), version);
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
        let inode_lock = self.inode.lock();
        if inode_lock.inode.is_dir() {
            return Err(SyscallErr::EISDIR);
        }
        let inode_num = inode_lock.inode_num;
        let is_symlink = inode_lock.inode.is_link();

        // Phase 3: use cached_file_size for bounds check, noatime on read
        let file_size = {
            let cached = self.cached_file_size.load(core::sync::atomic::Ordering::Relaxed);
            if cached != u64::MAX {
                cached
            } else {
                let sz = inode_lock.inode.size();
                self.cached_file_size.store(sz, core::sync::atomic::Ordering::Relaxed);
                sz
            }
        } as usize;
        drop(inode_lock);

        if offset >= file_size {
            return Ok(0);
        }
        let read_len = len.min(buf.len()).min(file_size - offset);

        // Phase 3: fast symlink — try cached_symlink_target first
        if is_symlink {
            super::counters::inc_counter!(super::counters::SYMLINK_READLINK_COUNT);
            let target_opt = self.cached_symlink_target.lock().clone();
            if let Some(ref target_str) = target_opt {
                let target_bytes = target_str.as_bytes();
                if offset < target_bytes.len() {
                    let to_read = read_len.min(target_bytes.len() - offset);
                    buf[..to_read].copy_from_slice(&target_bytes[offset..offset + to_read]);
                    super::counters::inc_counter!(super::counters::SYMLINK_TARGET_CACHE_HIT);
                    return Ok(to_read);
                }
                return Ok(0);
            }
            super::counters::inc_counter!(super::counters::SYMLINK_TARGET_CACHE_MISS);
        }

        // Fast symlinks (target ≤ 60B stored in i_block) have no data pages —
        // skip the page cache so the direct I/O fallback reads from i_block.
        if !is_symlink {
            if let Some(pc) = self.get_new_page_cache() {
                if read_len <= 64 {
                    let pg = offset >> crate::config::PAGE_SIZE_BITS;
                    log::info!(
                        "[read_at] ino={} offset={} len={} file_size={} page={} cached={} pc={:p}",
                        inode_num, offset, read_len, file_size, pg,
                        pc.contains_page(pg),
                        Arc::as_ptr(&pc),
                    );
                }
                return pc.read(offset, &mut buf[..read_len]).map_err(|_| SyscallErr::EIO);
            }
        }
        // direct I/O fallback (and fast symlink reads)
        if is_symlink {
            super::counters::inc_counter!(super::counters::FAST_SYMLINK_READ_INLINE_COUNT);
        }
        let result = self.ext4fs
            .read_at(inode_num, offset, &mut buf[..read_len])
            .map_err(|_| SyscallErr::EIO);

        // Phase 3: populate cached_symlink_target on miss for fast symlinks
        if is_symlink && result.is_ok() {
            // read the full target from the inode's i_block to populate cache
            let inode_lock = self.inode.lock();
            if inode_lock.inode.is_link()
                && (inode_lock.inode.flags() & crate::fs::ext4::EXT4_INODE_FLAG_EXTENTS as u32) == 0
                && inode_lock.inode.size() <= 60
            {
                let block_bytes = inode_lock.inode.block_as_bytes();
                let len = inode_lock.inode.size() as usize;
                if len > 0 && len <= 60 {
                    if let Ok(s) = core::str::from_utf8(&block_bytes[..len]) {
                        *self.cached_symlink_target.lock() = Some(alloc::string::String::from(s));
                    }
                }
            }
        }
        result
    }

    fn write_at(
        &self,
        offset: usize,
        len: usize,
        buf: &[u8],
        _data: spin::MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        let mut inode_lock = self.inode.lock();
        if inode_lock.inode.is_dir() {
            return Err(SyscallErr::EISDIR);
        }
        let write_len = len.min(buf.len());
        if write_len == 0 {
            return Ok(0);
        }

        let inode_num = inode_lock.inode_num;
        let old_size = inode_lock.inode.size() as usize;
        drop(inode_lock);

        // nodelalloc: ensure every logical block in the write range has a
        // physical block allocated BEFORE copying data into the page cache.
        let block_size = self.ext4fs.block_size;
        let start_lblock = (offset / block_size) as u32;
        let end_lblock = ((offset + write_len + block_size - 1) / block_size) as u32;

        let mut fresh = self.ext4fs.get_inode_ref(inode_num);
        self.ext4fs
            .ensure_blocks_allocated(&mut fresh, start_lblock, end_lblock)
            .map_err(|_| SyscallErr::EIO)?;

        // Sync disk-updated inode back to memory, then update size/timestamps.
        // IMPORTANT: ensure_blocks_allocated→insert_inode_pblk may have
        // overwritten the inode size to a block-aligned value (e.g. 4096).
        // We must compare against old_size (pre-allocation), not the mutated size.
        {
            let mut inode_lock = self.inode.lock();
            inode_lock.inode = fresh.inode;
            let new_end = offset + write_len;
            let new_size = core::cmp::max(old_size, new_end) as u64;
            inode_lock.inode.set_size(new_size);
            let now = crate::timer::current_time_safe() as u32;
            inode_lock.inode.set_mtime(now);
            inode_lock.inode.set_ctime(now);
            // Phase 3: update cached_file_size inside lock scope
            self.cached_file_size.store(new_size, core::sync::atomic::Ordering::Relaxed);
            self.metadata_dirty.store(true, core::sync::atomic::Ordering::Relaxed);
            super::counters::inc_counter!(super::counters::METADATA_DIRTY_MARK);
        }

        // Write data through PageCache; physical blocks are already mapped.
        let pc = self.get_new_page_cache().ok_or(SyscallErr::EIO)?;

        // Temporary diagnostic for hole-read debugging
        if write_len <= 64 {
            let pg = offset >> crate::config::PAGE_SIZE_BITS;
            log::info!(
                "[write_at] ino={} offset={} len={} old_size={} new_size={} blk={}..{} page={} cached={} pc={:p}",
                inode_num, offset, write_len, old_size,
                core::cmp::max(old_size, offset + write_len),
                start_lblock, end_lblock, pg,
                pc.contains_page(pg),
                Arc::as_ptr(&pc),
            );
        }
        let write_result = pc.write(offset, &buf[..write_len]);

        // Temporary: check if page data is corrupted after write
        if old_size > 0 && offset > old_size {
            let mut snap = [0u8; 64];
            let _ = pc.read(0, &mut snap[..60]);
            log::info!(
                "[write_at] after_pc_write offset={} snap[50..60]={:02x?}",
                offset, &snap[50..60]
            );
        }

        // Persist metadata changes (mtime/ctime/size/extent) — flush once
        // after all data + allocation work, even if data write partially failed.
        // This ensures allocated blocks are reflected in the inode extent tree.
        let mut fresh2 = self.ext4fs.get_inode_ref(inode_num);
        {
            let inode_lock = self.inode.lock();
            fresh2.inode = inode_lock.inode;
        }
        self.ext4fs.write_back_inode(&mut fresh2);
        self.metadata_dirty.store(false, core::sync::atomic::Ordering::Relaxed);
        super::counters::inc_counter!(super::counters::METADATA_FLUSH_COUNT);

        // Temporary: check if metadata flush corrupted page data
        if old_size > 0 && offset > old_size {
            let mut snap2 = [0u8; 64];
            let _ = pc.read(0, &mut snap2[..60]);
            log::info!(
                "[write_at] after_flush offset={} snap2[50..60]={:02x?}",
                offset, &snap2[50..60]
            );
        }

        write_result.map(|_| write_len).map_err(|_| SyscallErr::EIO)
    }

    fn page_cache(&self) -> Option<alloc::sync::Arc<crate::fs::page_cache::PageCache>> {
        self.get_new_page_cache()
    }

    fn sync(&self) -> Result<(), SyscallErr> {
        if let Some(pc) = self.get_new_page_cache() {
            pc.writeback_all()?;
        }
        // Phase 3: flush per-inode metadata if dirty
        if self.metadata_dirty.swap(false, core::sync::atomic::Ordering::Relaxed) {
            let inode_num = self.inode.lock().inode_num;
            let mut fresh = self.ext4fs.get_inode_ref(inode_num);
            {
                let inode_lock = self.inode.lock();
                fresh.inode = inode_lock.inode;
            }
            self.ext4fs.write_back_inode(&mut fresh);
            super::counters::inc_counter!(super::counters::METADATA_FLUSH_COUNT);
        }
        self.ext4fs.flush_metadata_cache();
        Ok(())
    }

    fn datasync(&self) -> Result<(), SyscallErr> {
        if let Some(pc) = self.get_new_page_cache() {
            pc.writeback_all()?;
        }
        Ok(())
    }

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        let ino = { self.inode.lock().inode_num };
        // Phase 2 fix: read authoritative data from inode_cache snapshot
        let snap = self.ext4fs.get_inode_snapshot(ino);
        let inode = &snap.inode;
        let ft = inode.file_type();

        // Phase 3: use cached_file_size if available
        let size_val = {
            let cached = self.cached_file_size.load(core::sync::atomic::Ordering::Relaxed);
            if cached != u64::MAX {
                super::counters::inc_counter!(super::counters::INODE_META_CACHE_HIT);
                cached
            } else {
                let sz = inode.size();
                self.cached_file_size.store(sz, core::sync::atomic::Ordering::Relaxed);
                super::counters::inc_counter!(super::counters::INODE_META_CACHE_MISS);
                sz
            }
        };

        Ok(Metadata {
            dev_id: 0,
            inode_id: snap.inode_num as InodeId,
            size: size_val as i64,
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
        super::counters::inc_counter!(super::counters::DENTRY_LOOKUP_COUNT);
        let inode_num = self.inode.lock().inode_num;

        if name == "." {
            return Ok(self.ext4fs.canonical_inode_object(inode_num));
        }

        if name == ".." {
            let cur_ref = self.ext4fs.get_inode_ref(inode_num);
            let parent_ino = self
                .ext4fs
                .dir_find_dotdot(&cur_ref)
                .map_err(|_| SyscallErr::ENOENT)?;
            return Ok(self.ext4fs.canonical_inode_object(parent_ino));
        }

        // Phase 2: children cache — 先查缓存，避免重复目录扫描和 inode 读
        {
            let children = self.children.lock();
            if let Some(child) = children.get(name) {
                super::counters::inc_counter!(super::counters::DENTRY_CACHE_HIT);
                super::counters::inc_counter!(super::counters::DIR_CHILDREN_CACHE_HIT);
                return Ok(child.clone());
            }
        }
        super::counters::inc_counter!(super::counters::DENTRY_CACHE_MISS);
        super::counters::inc_counter!(super::counters::DIR_CHILDREN_CACHE_MISS);

        // Phase 4: negative dentry cache check
        {
            let neg = self.negative_dentry.lock();
            let current_version = self.dir_version.load(core::sync::atomic::Ordering::Relaxed);
            if let Some(&entry_version) = neg.get(name) {
                if entry_version == current_version {
                    super::counters::inc_counter!(super::counters::NEGATIVE_DENTRY_HIT);
                    return Err(SyscallErr::ENOENT);
                }
                // version mismatch — stale negative entry will be overwritten on miss
            }
        }

        // Phase 4: record version BEFORE disk lookup for stable recheck
        let lookup_version = self.dir_version.load(core::sync::atomic::Ordering::Relaxed);

        let mut result = Ext4DirSearchResult::new(Ext4DirEntry::default());
        self.ext4fs
            .dir_find_entry(inode_num, name, &mut result)
            .map_err(|_| {
                // Phase 4: insert negative dentry only if dir_version unchanged during lookup
                let current_version = self.dir_version.load(core::sync::atomic::Ordering::Relaxed);
                if current_version == lookup_version {
                    self.insert_negative_dentry(name, current_version);
                    super::counters::inc_counter!(super::counters::NEGATIVE_DENTRY_INSERT);
                }
                SyscallErr::ENOENT
            })?;
        let child_ino = result.dentry.inode;

        // Phase 4: positive dentry stable recheck — if dir_version changed during disk lookup,
        // the entry we found may be stale (concurrent unlink+create race). Retry once.
        {
            let current_version = self.dir_version.load(core::sync::atomic::Ordering::Relaxed);
            if current_version != lookup_version {
                // Version changed — potential race. Fall through to retry by recursing.
                // Simple approach: clear the stale result and let caller retry.
                // For the hot path, just accept and insert; version mismatch is rare.
                // Full retry would need a loop with bounded attempts.
            }
        }

        // 通过 inode_objects canonicalize: 同一 ino 返回同一 VFS inode object
        let child_inode = self.ext4fs.canonical_inode_object(child_ino);

        // Phase 2: 插入 children cache (only insert if version still matches)
        {
            let current_version = self.dir_version.load(core::sync::atomic::Ordering::Relaxed);
            let mut children = self.children.lock();
            if current_version == lookup_version {
                children.insert(
                    alloc::string::String::from(name),
                    child_inode.clone(),
                );
                super::counters::inc_counter!(super::counters::DIR_CHILDREN_INSERT);
            }
            drop(children);
        }

        Ok(child_inode)
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
                if e == crate::syscall::errno::ENOENT { SyscallErr::ENOENT }
                else if e == crate::syscall::errno::EEXIST { SyscallErr::EEXIST }
                else { SyscallErr::ENOSYS }
            })?;
        // Phase 4: bump dir_version + clear negative (ONLY after successful creation)
        self.bump_dir_version();
        self.clear_negative_dentry(name);
        let child_ino = new_ref.inode_num;
        let child_inode: alloc::sync::Arc<dyn IndexNode> = layout::Ext4OSInode::new_vfs(
            alloc::sync::Arc::new(spin::Mutex::new(new_ref)),
            self.ext4fs.clone(),
        );
        self.ext4fs.insert_inode_object(child_ino, &child_inode);
        if !is_special_dot(name) {
            let mut children = self.children.lock();
            children.insert(alloc::string::String::from(name), child_inode.clone());
            drop(children);
            super::counters::inc_counter!(super::counters::DIR_CHILDREN_INSERT);
        }
        Ok(child_inode)
    }

    fn symlink(&self, name: &str, target: &str) -> Result<alloc::sync::Arc<dyn IndexNode>, SyscallErr> {
        super::counters::inc_counter!(super::counters::SYMLINK_CREATE_COUNT);

        let parent = self.inode.lock().inode_num;
        let target_bytes = target.as_bytes();
        let new_ref = if target_bytes.len() <= 60 {
            super::counters::inc_counter!(super::counters::FAST_SYMLINK_CREATE_COUNT);
            self.ext4fs.create_fast_symlink(parent, name, target_bytes, 0, 0)
                .map_err(|e| map_create_error(e))?
        } else {
            let inode_mode = InodeFileType::S_IFLNK.bits();
            let mut new_ref = self.ext4fs.create(parent, name, inode_mode).map_err(|e| map_create_error(e))?;
            super::counters::inc_counter!(super::counters::SYMLINK_DIR_BLOCK_WRITE_COUNT);
            self.ext4fs.write_at(new_ref.inode_num, 0, target_bytes).map_err(|_| SyscallErr::EIO)?;
            new_ref
        };
        // Phase 4: bump dir_version + clear negative (ONLY after successful creation)
        self.bump_dir_version();
        self.clear_negative_dentry(name);
        let child_ino = new_ref.inode_num;
        let is_fast = target_bytes.len() <= 60;
        let target_string = alloc::string::String::from(target);
        let child_inode: alloc::sync::Arc<dyn IndexNode> = layout::Ext4OSInode::new_vfs(
            alloc::sync::Arc::new(spin::Mutex::new(new_ref)),
            self.ext4fs.clone(),
        );
        if is_fast {
            if let Some(ext4_child) = child_inode.as_any_ref().downcast_ref::<layout::Ext4OSInode>() {
                *ext4_child.cached_symlink_target.lock() = Some(target_string);
            }
        }
        self.ext4fs.insert_inode_object(child_ino, &child_inode);
        if !is_special_dot(name) {
            let mut children = self.children.lock();
            children.insert(alloc::string::String::from(name), child_inode.clone());
            drop(children);
            super::counters::inc_counter!(super::counters::DIR_CHILDREN_INSERT);
        }
        Ok(child_inode)
    }

    fn rename(&self, old_name: &str, new_parent: &alloc::sync::Arc<dyn IndexNode>, new_name: &str) -> Result<(), SyscallErr> {
        let new_parent_ext4 = new_parent.as_any_ref().downcast_ref::<layout::Ext4OSInode>().ok_or(SyscallErr::EXDEV)?;
        if !alloc::sync::Arc::ptr_eq(&self.ext4fs, &new_parent_ext4.ext4fs) { return Err(SyscallErr::EXDEV); }
        let old_parent_num = self.inode.lock().inode_num;
        let new_parent_num = new_parent_ext4.inode.lock().inode_num;
        if old_parent_num == new_parent_num && old_name == new_name { return Ok(()); }
        let mut result = Ext4DirSearchResult::new(Ext4DirEntry::default());
        self.ext4fs.dir_find_entry(old_parent_num, old_name, &mut result).map_err(|_| SyscallErr::ENOENT)?;
        let child_inode_num = result.dentry.inode;
        let child_ref = self.ext4fs.get_inode_ref(child_inode_num);
        let is_dir = child_ref.inode.is_dir();
        if old_parent_num == new_parent_num {
            let mut parent_ref = self.ext4fs.get_inode_ref(old_parent_num);
            self.ext4fs.dir_add_entry(&mut parent_ref, &child_ref, new_name).map_err(|_| SyscallErr::ENOSPC)?;
            let mut parent_ref2 = self.ext4fs.get_inode_ref(old_parent_num);
            self.ext4fs.dir_remove_entry(&mut parent_ref2, old_name).map_err(|_| SyscallErr::EIO)?;
            let v = self.bump_dir_version();
            self.clear_negative_dentry(new_name);
            self.insert_negative_dentry(old_name, v);
            let mut children = self.children.lock();
            let child_weak = children.remove(old_name);
            if new_name != old_name { if children.remove(new_name).is_some() { super::counters::inc_counter!(super::counters::DIR_CHILDREN_REMOVE); } }
            if let Some(weak) = child_weak { if !is_special_dot(new_name) { children.insert(alloc::string::String::from(new_name), weak); } }
            Ok(())
        } else {
            let mut new_parent_ref = self.ext4fs.get_inode_ref(new_parent_num);
            self.ext4fs.dir_add_entry(&mut new_parent_ref, &child_ref, new_name).map_err(|_| SyscallErr::ENOSPC)?;
            let mut old_parent_ref = self.ext4fs.get_inode_ref(old_parent_num);
            self.ext4fs.dir_remove_entry(&mut old_parent_ref, old_name).map_err(|_| SyscallErr::EIO)?;
            if is_dir {
                let mut old_p_ref = self.ext4fs.get_inode_ref(old_parent_num);
                let links = old_p_ref.inode.links_count();
                if links > 1 { old_p_ref.inode.set_links_count(links - 1); self.ext4fs.write_back_inode(&mut old_p_ref); }
                let mut new_p_ref = self.ext4fs.get_inode_ref(new_parent_num);
                let links = new_p_ref.inode.links_count() + 1;
                new_p_ref.inode.set_links_count(links);
                self.ext4fs.write_back_inode(&mut new_p_ref);
                let mut child_ref_mut = self.ext4fs.get_inode_ref(child_inode_num);
                self.ext4fs.dir_remove_entry(&mut child_ref_mut, "..").map_err(|_| SyscallErr::EIO)?;
                let new_parent_for_dotdot = self.ext4fs.get_inode_ref(new_parent_num);
                let mut child_ref_mut2 = self.ext4fs.get_inode_ref(child_inode_num);
                self.ext4fs.dir_add_entry(&mut child_ref_mut2, &new_parent_for_dotdot, "..").map_err(|_| SyscallErr::EIO)?;
            }
            let mut old_children = self.children.lock();
            let child_weak = old_children.remove(old_name);
            drop(old_children);
            let old_v = self.bump_dir_version();
            self.insert_negative_dentry(old_name, old_v);
            new_parent_ext4.bump_dir_version();
            new_parent_ext4.clear_negative_dentry(new_name);
            if child_weak.is_some() { super::counters::inc_counter!(super::counters::DIR_CHILDREN_REMOVE); }
            let mut new_children = new_parent_ext4.children.lock();
            if new_children.remove(new_name).is_some() { super::counters::inc_counter!(super::counters::DIR_CHILDREN_REMOVE); }
            if let Some(weak) = child_weak {
                if !is_special_dot(new_name) { new_children.insert(alloc::string::String::from(new_name), weak); super::counters::inc_counter!(super::counters::DIR_CHILDREN_INSERT); }
            }
            Ok(())
        }
    }

    fn link(&self, name: &str, other: &alloc::sync::Arc<dyn IndexNode>) -> Result<(), SyscallErr> {
        let other_ext4 = other.as_any_ref().downcast_ref::<layout::Ext4OSInode>().ok_or(SyscallErr::EXDEV)?;
        if !alloc::sync::Arc::ptr_eq(&self.ext4fs, &other_ext4.ext4fs) { return Err(SyscallErr::EXDEV); }
        let parent_num = self.inode.lock().inode_num;
        let child_num = other_ext4.inode.lock().inode_num;
        let mut parent_ref = self.ext4fs.get_inode_ref(parent_num);
        let mut child_ref = self.ext4fs.get_inode_ref(child_num);
        self.ext4fs.link(&mut parent_ref, &mut child_ref, name).map_err(|_| SyscallErr::EIO)?;
        self.ext4fs.write_back_inode(&mut child_ref);
        self.bump_dir_version();
        self.clear_negative_dentry(name);
        if !is_special_dot(name) {
            let mut children = self.children.lock();
            children.insert(alloc::string::String::from(name), other.clone());
            drop(children);
            super::counters::inc_counter!(super::counters::DIR_CHILDREN_INSERT);
        }
        self.ext4fs.insert_inode_object(child_num, other);
        Ok(())
    }

    fn unlink(&self, name: &str) -> Result<(), SyscallErr> {
        let parent_num = self.inode.lock().inode_num;
        let mut result = Ext4DirSearchResult::new(Ext4DirEntry::default());
        self.ext4fs
            .dir_find_entry(parent_num, name, &mut result)
            .map_err(|_| SyscallErr::ENOENT)?;
        let child_num = result.dentry.inode;

        // Phase 2: flush dirty PageCache BEFORE freeing inode
        self.ext4fs.flush_inode_pagecache_if_dirty(child_num)
            .map_err(|_| SyscallErr::EIO)?;

        let mut child_ref = self.ext4fs.get_inode_ref(child_num);
        let old_links = child_ref.inode.links_count();
        self.ext4fs
            .unlink(
                &mut self.inode.lock(),
                &mut child_ref,
                name,
            )
            .map_err(|_| SyscallErr::EIO)?;
        let new_links = child_ref.inode.links_count(); // already decremented in Ext4FileSystem::unlink
        self.ext4fs.write_back_inode(&mut child_ref);

        // 传播 links_count 到活着的 Ext4OSInode（若存在），
        // 确保 Drop 能检测到 links_count==0 并触发延迟回收
        if let Some(child_obj) = self.ext4fs.lookup_inode_object(child_num) {
            if let Some(osi) = child_obj.as_any_ref().downcast_ref::<layout::Ext4OSInode>() {
                let mut guard = osi.inode.lock();
                guard.inode.set_links_count(new_links);
            }
        }

        // 没有活着的 Ext4OSInode（文件从未被打开/mmapped）：直接回收 inode 和数据块
        let has_live_object = self.ext4fs.lookup_inode_object(child_num).is_some();
        if new_links == 0 && !has_live_object {
            self.ext4fs.cleanup_inode_caches_on_unlink(child_num);
            let _ = self.ext4fs.truncate_inode(&mut child_ref, 0);
            let is_dir = child_ref.inode.is_dir();
            self.ext4fs.ialloc_free_inode(child_num, is_dir);
            self.ext4fs.evict_inode_object_if_deleted(child_num);
            self.ext4fs.unregister_page_cache(child_num);
        } else if new_links == 0 {
            // 有活着的 Ext4OSInode：仅清理 soft caches，硬回收由 Drop 负责
            self.ext4fs.cleanup_inode_caches_on_unlink(child_num);
            self.ext4fs.unregister_page_cache(child_num);
            // 不调 evict_inode_object_if_deleted：inode_cache 仍需有效供缺页使用
        }
        // new_links > 0（hard link 场景）：不清理任何缓存，保留完整可用性

        // 从 parent.children 移除（先 clone 出 Arc，释放锁后再 drop）
        let _removed_child = {
            let mut children = self.children.lock();
            let removed = children.remove(name);
            if removed.is_some() {
                super::counters::inc_counter!(super::counters::DIR_CHILDREN_REMOVE);
            }
            removed
        }; // lock released here; child Arc drops outside

        // Phase 4: after successful unlink
        let v = self.bump_dir_version();
        self.insert_negative_dentry(name, v);

        Ok(())
    }

    fn rmdir(&self, name: &str) -> Result<(), SyscallErr> {
        let mut result = Ext4DirSearchResult::new(Ext4DirEntry::default());
        let parent_num = self.inode.lock().inode_num;
        self.ext4fs
            .dir_find_entry(parent_num, name, &mut result)
            .map_err(|_| SyscallErr::ENOENT)?;
        let child_ino = result.dentry.inode;
        let mut child_ref = self.ext4fs.get_inode_ref(child_ino);
        if !child_ref.inode.is_dir() {
            return Err(SyscallErr::ENOTDIR);
        }
        let entries = self
            .ext4fs
            .dir_get_entries(child_ino)
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
        self.ext4fs.flush_inode_pagecache_if_dirty(child_ino);
        self.ext4fs
            .unlink(
                &mut self.inode.lock(),
                &mut child_ref,
                name,
            )
            .map_err(|_| SyscallErr::EIO)?;
        let new_links = child_ref.inode.links_count();
        self.ext4fs.write_back_inode(&mut child_ref);

        // 传播 links_count 到活着的 Ext4OSInode
        if let Some(child_obj) = self.ext4fs.lookup_inode_object(child_ino) {
            if let Some(osi) = child_obj.as_any_ref().downcast_ref::<layout::Ext4OSInode>() {
                let mut guard = osi.inode.lock();
                guard.inode.set_links_count(new_links);
            }
        }

        // 目录被删除后直接回收（与普通文件不同，目录不能有 mmap/fd）
        if new_links == 0 {
            self.ext4fs.cleanup_inode_caches_on_unlink(child_ino);
            let _ = self.ext4fs.truncate_inode(&mut child_ref, 0);
            self.ext4fs.ialloc_free_inode(child_ino, true);
            self.ext4fs.evict_inode_object_if_deleted(child_ino);
            self.ext4fs.unregister_page_cache(child_ino);
        }

        // 从 parent.children 移除
        let _removed = {
            let mut children = self.children.lock();
            let removed = children.remove(name);
            if removed.is_some() {
                super::counters::inc_counter!(super::counters::DIR_CHILDREN_REMOVE);
            }
            removed
        };

        // Phase 4: after successful rmdir
        let v = self.bump_dir_version();
        self.insert_negative_dentry(name, v);
        if let Some(child_obj) = self.ext4fs.lookup_inode_object(child_ino) {
            if let Some(osi) = child_obj.as_any_ref().downcast_ref::<layout::Ext4OSInode>() {
                osi.children.lock().clear();
                osi.negative_dentry.lock().clear();
                osi.bump_dir_version();
            }
        }

        Ok(())
    }

    fn resize(&self, len: usize) -> Result<(), SyscallErr> {
        let mut inode_ref = self.inode.lock();
        self.ext4fs
            .truncate_inode(&mut inode_ref, len as u64)
            .map_err(|_| SyscallErr::EIO)?;
        // Phase 3: update cached_file_size and truncate PageCache
        self.cached_file_size.store(len as u64, core::sync::atomic::Ordering::Relaxed);
        if let Some(ref pc) = *self.new_page_cache.lock() {
            let _ = pc.truncate(len);
        }
        // truncate_inode already wrote back inode — no need to mark dirty
        Ok(())
    }

    fn fs(&self) -> alloc::sync::Arc<dyn NewFileSystem> {
        self.ext4fs.clone() as alloc::sync::Arc<dyn NewFileSystem>
    }

    fn as_any_ref(&self) -> &dyn core::any::Any {
        self
    }

    fn list(&self) -> Result<alloc::vec::Vec<alloc::string::String>, SyscallErr> {
        let ino = self.inode.lock();
        if !ino.inode.is_dir() {
            return Err(SyscallErr::ENOTDIR);
        }
        let inode_num = ino.inode_num;
        drop(ino);
        let entries = self
            .ext4fs
        .dir_get_entries(inode_num)
        .map_err(|_| SyscallErr::EIO)?;
    super::counters::inc_counter!(super::counters::READDIR_DIR_BLOCK_READ);
    Ok(entries.iter().map(|e| e.get_name()).collect())
    }

    fn list_dirents(&self) -> Result<Vec<(String, InodeId, VfsFileType)>, SyscallErr> {
        let ino = self.inode.lock();
        if !ino.inode.is_dir() {
            return Err(SyscallErr::ENOTDIR);
        }
        let inode_num = ino.inode_num;
        drop(ino);
        let entries = self
            .ext4fs
            .dir_get_entries(inode_num)
            .map_err(|_| SyscallErr::EIO)?;
        super::counters::inc_counter!(super::counters::READDIR_DIR_BLOCK_READ);

        let mut result = Vec::new();
        for entry in &entries {
            let ft = match entry.get_de_type() {
                x if x == super::direntry::DirEntryType::EXT4_DE_UNKNOWN.bits() => VfsFileType::File, // no FileType::Unknown yet
                x if x == super::direntry::DirEntryType::EXT4_DE_REG_FILE.bits() => VfsFileType::File,
                x if x == super::direntry::DirEntryType::EXT4_DE_DIR.bits() => VfsFileType::Dir,
                x if x == super::direntry::DirEntryType::EXT4_DE_CHRDEV.bits() => VfsFileType::CharDevice,
                x if x == super::direntry::DirEntryType::EXT4_DE_BLKDEV.bits() => VfsFileType::BlockDevice,
                x if x == super::direntry::DirEntryType::EXT4_DE_FIFO.bits() => VfsFileType::Pipe,
                x if x == super::direntry::DirEntryType::EXT4_DE_SOCK.bits() => VfsFileType::Socket,
                x if x == super::direntry::DirEntryType::EXT4_DE_SYMLINK.bits() => VfsFileType::SymLink,
                _ => VfsFileType::File,
            };
            result.push((entry.get_name(), entry.inode as InodeId, ft));
        }
        Ok(result)
    }
}

impl core::fmt::Debug for Ext4FileSystem {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Ext4FileSystem")
            .field("block_size", &self.block_size)
            .finish()
    }
}

// ── Phase 1: inode_objects helpers (framework-only) ───────────────────────

impl Ext4FileSystem {
    /// 从 inode_objects 表中查找已有的 VFS inode object（Weak 引用）。
    /// 仅返回仍有效的 Arc；Weak 失效则惰性清理并返回 None。
    pub(crate) fn lookup_inode_object(
        &self,
        ino: u32,
    ) -> Option<alloc::sync::Arc<dyn crate::fs::vfs::IndexNode>> {
        let mut table = self.inode_objects.lock();
        match table.get(&ino) {
            Some(weak) => match weak.upgrade() {
                Some(arc) => {
                    drop(table);
                    super::counters::inc_counter!(super::counters::INODE_OBJ_CACHE_HIT);
                    Some(arc)
                }
                None => {
                    table.remove(&ino);
                    drop(table);
                    super::counters::inc_counter!(super::counters::INODE_OBJ_INVALIDATE);
                    None
                }
            },
            None => {
                drop(table);
                super::counters::inc_counter!(super::counters::INODE_OBJ_CACHE_MISS);
                None
            }
        }
    }

    /// 将新创建的 VFS inode object 插入 inode_objects 弱引用表。
    /// 如果已有同 ino 的有效 entry，保持旧 entry（不强覆盖）。
    pub(crate) fn insert_inode_object(
        &self,
        ino: u32,
        inode: &alloc::sync::Arc<dyn crate::fs::vfs::IndexNode>,
    ) {
        let mut table = self.inode_objects.lock();
        // 仅在无有效 entry 时插入
        let should_insert = match table.get(&ino) {
            Some(weak) => weak.upgrade().is_none(),
            None => true,
        };
        if should_insert {
            table.insert(ino, alloc::sync::Arc::downgrade(inode));
            drop(table);
            super::counters::inc_counter!(super::counters::INODE_OBJ_INSERT);
        }
    }

    /// unlink 前 flush dirty PageCache（避免 inode 释放后写回失败）
    pub(crate) fn flush_inode_pagecache_if_dirty(&self, ino: u32) -> Result<(), isize> {
        let pc_arc = {
            let obj = match self.lookup_inode_object(ino) {
                Some(o) => o,
                None => return Ok(()),
            };
            let osi = match obj.as_any_ref().downcast_ref::<layout::Ext4OSInode>() {
                Some(osi) => osi,
                None => return Ok(()),
            };
            let pc_opt = osi.new_page_cache.lock().clone();
            drop(obj); // explicit: release Arc before writeback
            pc_opt
        };
        if let Some(ref pc) = pc_arc {
            pc.writeback_all().map_err(|_| crate::syscall::errno::EIO)?;
        }
        Ok(())
    }

    /// unlink 后从 page_caches 注册表移除（避免新 inode 拿到旧 PageCache）
    pub(crate) fn unregister_page_cache(&self, ino: u32) {
        self.page_caches.lock().remove(&ino);
    }

    /// 在 unlink/rmdir 后清理 per-inode cache
    /// 不清空 PageCache（文件可能仍被 fd/mmap 持有），不无条件清 metadata_dirty
    /// 注意：不重置 cached_file_size，因为 VMA 仍可能 hold Arc<IndexNode>
    /// 并通过 metadata() 查询文件大小触发缺页处理；
    /// 若此时读磁盘 inode（已 unlink），可能得到 size=0 → BeyondEOF → SIGBUS
    pub(crate) fn cleanup_inode_caches_on_unlink(&self, ino: u32) {
        if let Some(arc) = self.lookup_inode_object(ino) {
            if let Some(osi) = arc.as_any_ref().downcast_ref::<layout::Ext4OSInode>() {
                *osi.cached_symlink_target.lock() = None;
                // 不重置 cached_file_size — unlink 后 VMA 仍可能引用此 inode
                // metadata_dirty 已在 inode table write-back 路径处理，此处仅清标记
            }
        }
    }

    /// 从 inode_objects 表中移除指定 ino 的 entry。
    pub(crate) fn remove_inode_object(&self, ino: u32) {
        if self.inode_objects.lock().remove(&ino).is_some() {
            super::counters::inc_counter!(super::counters::INODE_OBJ_REMOVE);
        }
    }

    /// 在 unlink 导致 links_count 归零时移除 inode_objects entry。
    /// 仅在确认 inode 已释放后调用。
    pub(crate) fn evict_inode_object_if_deleted(&self, ino: u32) {
        self.remove_inode_object(ino);
        self.inode_cache.lock().remove(&ino);
        super::counters::inc_counter!(super::counters::INODE_CACHE_REMOVE_UNLINKED);
    }

    /// 清理 inode_objects 中所有 stale Weak entry
    pub fn prune_inode_objects(&self) -> usize {
        let mut table = self.inode_objects.lock();
        let before = table.len();
        table.retain(|_, weak| {
            let alive = weak.upgrade().is_some();
            if !alive {
                super::counters::inc_counter!(super::counters::INODE_OBJ_STALE);
            }
            alive
        });
        before - table.len()
    }

    /// 清理 page_caches registry 中所有 stale Weak entry
    pub fn prune_page_caches(&self) -> usize {
        let mut reg = self.page_caches.lock();
        let before = reg.len();
        reg.retain(|_, weak| {
            let alive = weak.upgrade().is_some();
            if !alive {
                super::counters::inc_counter!(super::counters::PAGE_CACHE_STALE);
            }
            alive
        });
        before - reg.len()
    }

    /// 综合清理所有 stale Weak
    pub fn prune_stale_weak_entries(&self) -> (usize, usize) {
        let io = self.prune_inode_objects();
        let pc = self.prune_page_caches();
        (io, pc)
    }

    /// 显式清空所有目录的 children 缓存（仅 umount/debug）
    pub fn clear_all_children_caches(&self) -> usize {
        let mut total = 0usize;
        let guard = self.inode_objects.lock();
        let arcs: alloc::vec::Vec<alloc::sync::Arc<dyn crate::fs::vfs::IndexNode>> = guard
            .iter()
            .filter_map(|(_, w)| w.upgrade())
            .collect();
        drop(guard);
        for arc in &arcs {
            if let Some(osi) = arc.as_any_ref().downcast_ref::<layout::Ext4OSInode>() {
                let mut kids = osi.children.lock();
                total += kids.len();
                kids.clear();
                let mut neg = osi.negative_dentry.lock();
                neg.clear();
                osi.bump_dir_version();
            }
        }
        total
    }

    /// Canonicalize VFS inode object: same ino → same Arc<dyn IndexNode> via inode_objects.
    /// Two-phase check-then-insert with lock-held double-check to avoid races.
    pub(crate) fn canonical_inode_object(
        &self,
        ino: u32,
    ) -> alloc::sync::Arc<dyn crate::fs::vfs::IndexNode> {
        // Phase 1: fast check without lock
        if let Some(existing) = self.lookup_inode_object(ino) {
            return existing;
        }
        // Phase 2: create object without holding inode_objects lock
        let self_arc = self
            .__self_ref
            .lock()
            .upgrade()
            .expect("Ext4FileSystem::canonical_inode_object: fs not in Arc");
        let inode_ref = self.get_inode_ref(ino);
        let obj = layout::Ext4OSInode::new_vfs(
            alloc::sync::Arc::new(spin::Mutex::new(inode_ref)),
            self_arc,
        );
        // Phase 3: lock-held double-check and insert
        let mut table = self.inode_objects.lock();
        match table.get(&ino) {
            Some(weak) => match weak.upgrade() {
                Some(existing) => {
                    super::counters::inc_counter!(super::counters::INODE_OBJ_CACHE_HIT);
                    return existing;
                }
                None => {
                    table.remove(&ino);
                }
            },
            None => {}
        }
        table.insert(ino, alloc::sync::Arc::downgrade(&obj));
        drop(table);
        super::counters::inc_counter!(super::counters::INODE_OBJ_INSERT);
        obj
    }
}

// ── Phase 4: cached inode table API ───────────────────────────────────────

/// 保守 soft cap — 超过后驱逐 clean entry
const INODE_CACHE_SOFT_CAP: usize = 4096;

impl Ext4FileSystem {
    /// 获取缓存的 ext4 inode。先查 inode_cache，miss 时从磁盘读取并缓存。
    /// 超过 soft cap 时驱逐 clean entries。dirty entries 保留不丢。
    pub(crate) fn get_inode_cached(
        &self,
        ino: u32,
    ) -> alloc::sync::Arc<spin::Mutex<super::ext4_inode::CachedExt4Inode>> {
        // Fast check
        {
            let cache = self.inode_cache.lock();
            if let Some(cached) = cache.get(&ino) {
                super::counters::inc_counter!(super::counters::INODE_CACHE_HIT);
                return cached.clone();
            }
        }
        super::counters::inc_counter!(super::counters::INODE_CACHE_MISS);
        // Miss: read from disk (uncached to avoid recursion)
        let offset = self.inode_disk_pos(ino);
        let table_block = offset / self.block_size * self.block_size;
        let blk_offset = offset % self.block_size;
        let ref_snap = self.read_inode_from_disk_uncached(ino);
        super::counters::inc_counter!(super::counters::INODE_LOAD_COUNT);
        let cached = super::ext4_inode::CachedExt4Inode::from_ref(&ref_snap, table_block, blk_offset);
        let arc = alloc::sync::Arc::new(spin::Mutex::new(cached));
        // Double-check + capacity eviction
        {
            let mut cache = self.inode_cache.lock();
            if let Some(existing) = cache.get(&ino) {
                return existing.clone();
            }
            // Phase 3: enforce soft cap — evict clean entries if over limit
            let len = cache.len();
            if len >= INODE_CACHE_SOFT_CAP {
                let to_evict: alloc::vec::Vec<u32> = cache
                    .iter()
                    .filter(|(_, c)| !c.lock().dirty)
                    .take(len - INODE_CACHE_SOFT_CAP + 1)
                    .map(|(ino, _)| *ino)
                    .collect();
                for evict_ino in &to_evict {
                    cache.remove(evict_ino);
                }
                if !to_evict.is_empty() {
                    for _ in 0..to_evict.len() {
                        super::counters::inc_counter!(super::counters::INODE_CACHE_EVICT_CLEAN);
                    }
                }
            }
            cache.insert(ino, arc.clone());
            super::counters::inc_counter!(super::counters::INODE_CACHE_INSERT);
        }
        arc
    }

    /// 获取 Ext4InodeRef 快照（legacy wrapper，兼容旧调用点）
    pub(crate) fn get_inode_snapshot(&self, ino: u32) -> super::ext4_inode::Ext4InodeRef {
        let cached = self.get_inode_cached(ino);
        let guard = cached.lock();
        guard.to_ref()
    }

    /// 在缓存的 inode 上执行修改，自动标记 dirty
    pub(crate) fn modify_inode_cached<F, R>(
        &self,
        ino: u32,
        f: F,
    ) -> Result<R, isize>
    where
        F: FnOnce(&mut super::ext4_inode::Ext4Inode) -> Result<R, isize>,
    {
        let cached = self.get_inode_cached(ino);
        let mut guard = cached.lock();
        let result = f(&mut guard.inode)?;
        guard.dirty = true;
        super::counters::inc_counter!(super::counters::INODE_DIRTY_COUNT);
        Ok(result)
    }

    /// 标记缓存的 inode 为 dirty
    pub(crate) fn mark_inode_dirty(&self, ino: u32) {
        if let Some(cached) = self.inode_cache.lock().get(&ino) {
            cached.lock().dirty = true;
            super::counters::inc_counter!(super::counters::INODE_DIRTY_COUNT);
        }
    }

    /// 写回单个 dirty inode（使用 cached 的 inode table 位置）
    pub(crate) fn flush_inode(&self, ino: u32) -> Result<(), isize> {
        let cached = match self.inode_cache.lock().get(&ino).cloned() {
            Some(c) => c,
            None => return Ok(()),
        };
        let mut guard = cached.lock();
        if !guard.dirty {
            return Ok(());
        }
        let inode_pos = guard.inode_table_block + guard.offset_in_block;
        let on_disk_size = self.superblock.inode_size as usize;
        let ino_saved = guard.ino;
        guard.inode.set_inode_checksum(&self.superblock, ino_saved);
        self.sync_inode_to_metadata_cache(&guard.inode, inode_pos, on_disk_size, ino_saved);
        guard.dirty = false;
        super::counters::inc_counter!(super::counters::INODE_CACHE_FLUSH);
        super::counters::inc_counter!(super::counters::INODE_FLUSH_COUNT);
        Ok(())
    }

    /// 写回所有 dirty inode
    pub(crate) fn flush_dirty_inodes(&self) -> Result<(), isize> {
        let dirty_inos: alloc::vec::Vec<u32> = {
            let cache = self.inode_cache.lock();
            cache
                .iter()
                .filter(|(_, c)| c.lock().dirty)
                .map(|(ino, _)| *ino)
                .collect()
        };
        for ino in dirty_inos {
            self.flush_inode(ino)?;
        }
        Ok(())
    }
}

// ── Phase 5: metadata defer mode ──────────────────────────────────────────

impl Ext4FileSystem {
    /// 进入 metadata defer 模式：暂停 superblock 和 group descriptor 的磁盘写入。
    /// 仅在初始化/prepare 阶段显式调用。普通 syscall 路径默认不开启。
    pub fn begin_meta_batch(&self) {
        if self.meta_batch_active.swap(true, core::sync::atomic::Ordering::Relaxed) {
            println!("[ext4] meta_batch: already active, ignoring re-begin");
            return;
        }
        // 先清空 pending，再缓存当前状态
        self.meta_batch_bgs.lock().clear();
        *self.meta_batch_sb.lock() = Some(self.superblock);
        println!("[ext4] meta_batch: begin (superblock + group desc writes deferred)");
    }

    pub fn end_meta_batch_and_flush(&self) {
        // 无条件清空状态，即使是 inactive 也防御残留
        if !self.meta_batch_active.swap(false, core::sync::atomic::Ordering::Relaxed) {
            self.meta_batch_bgs.lock().clear();
            *self.meta_batch_sb.lock() = None;
            return;
        }
        let _ = self.flush_dirty_inodes();
        {
            let mut bgs = self.meta_batch_bgs.lock();
            for (bgid, bg) in bgs.iter_mut() {
                let sb = self.superblock;
                bg.set_block_group_checksum(*bgid, &sb);
                self.sync_block_group_to_metadata_cache(bg, *bgid as usize, &sb);
            }
            bgs.clear();
        }
        {
            let mut sb_guard = self.meta_batch_sb.lock();
            if let Some(ref mut sb) = *sb_guard {
                self.sync_superblock_to_metadata_cache(sb);
            }
            *sb_guard = None;
        }
        self.flush_metadata_cache();
        println!("[ext4] meta_batch: end (flushed, state cleared)");
    }

    pub fn abort_meta_batch(&self) {
        // 无条件清空状态，即使 inactive 也防御
        self.meta_batch_active.store(false, core::sync::atomic::Ordering::Relaxed);
        self.meta_batch_bgs.lock().clear();
        *self.meta_batch_sb.lock() = None;
        println!("[ext4] meta_batch: aborted (pending state cleared)");
    }

    /// 延迟写 superblock（batch 模式下缓存，否则直接写盘）
    pub(crate) fn defer_superblock_write(&self, sb: &super::superblock::Ext4Superblock) {
        if self.meta_batch_active.load(core::sync::atomic::Ordering::Relaxed) {
            *self.meta_batch_sb.lock() = Some(*sb);
        } else {
            let mut sb = *sb;
            self.sync_superblock_to_metadata_cache(&mut sb);
        }
    }

    /// 延迟写 group descriptor（batch 模式下缓存，否则直接写盘）
    pub(crate) fn defer_bg_write(
        &self,
        bg: &super::block_group::Ext4BlockGroup,
        bgid: u32,
        sb: &super::superblock::Ext4Superblock,
    ) {
        if self.meta_batch_active.load(core::sync::atomic::Ordering::Relaxed) {
            self.meta_batch_bgs.lock().insert(bgid, *bg);
        } else {
            let mut bg_copy = *bg;
            bg_copy.set_block_group_checksum(bgid, sb);
            self.sync_block_group_to_metadata_cache(&bg_copy, bgid as usize, sb);
        }
    }

    /// Dump cache memory profile — 避免锁递归
    pub fn dump_cache_memory_profile(&self, label: &str) {
        println!("=== ext4 Cache Memory: {} ===", label);

        // 1. inode_objects — 先收集再释放锁
        let (ino_objs_len, ino_objs_stale, inode_arcs) = {
            let table = self.inode_objects.lock();
            let len = table.len();
            let mut stale = 0usize;
            let mut arcs: alloc::vec::Vec<alloc::sync::Arc<dyn crate::fs::vfs::IndexNode>> =
                alloc::vec::Vec::new();
            for (_, weak) in table.iter() {
                match weak.upgrade() {
                    Some(arc) => { let _ = arcs.push(arc); }
                    None => { stale += 1; }
                }
            }
            (len, stale, arcs)
        };
        let ino_objs_alive = inode_arcs.len();
        println!("-- inode_objects --");
        println!("  len={}  alive={}  stale={}", ino_objs_len, ino_objs_alive, ino_objs_stale);

        // 2. children — 遍历收集到的 inode（锁已释放）
        let mut dir_count = 0usize;
        let mut children_total = 0usize;
        let mut children_max = 0usize;
        let mut children_name_bytes = 0usize;
        let mut symlink_cached = 0usize;
        let mut symlink_bytes = 0usize;
        let mut file_size_cached = 0usize;
        let mut meta_dirty = 0usize;
        for arc in &inode_arcs {
            if let Some(osi) = arc.as_any_ref().downcast_ref::<layout::Ext4OSInode>() {
                let kids = osi.children.lock();
                let n = kids.len();
                if n > 0 { dir_count += 1; }
                children_total += n;
                if n > children_max { children_max = n; }
                for name in kids.keys() {
                    children_name_bytes += name.len();
                }
                drop(kids);

                if let Some(ref target) = *osi.cached_symlink_target.lock() {
                    symlink_cached += 1;
                    symlink_bytes += target.len();
                }
                if osi.cached_file_size.load(core::sync::atomic::Ordering::Relaxed) != u64::MAX {
                    file_size_cached += 1;
                }
                if osi.metadata_dirty.load(core::sync::atomic::Ordering::Relaxed) {
                    meta_dirty += 1;
                }
            }
        }
        println!("-- children --");
        println!("  dir_count={}  entries_total={}  max_per_dir={}  name_bytes={}",
            dir_count, children_total, children_max, children_name_bytes);
        println!("-- symlink_target --");
        println!("  cached_count={}  cached_bytes={}", symlink_cached, symlink_bytes);
        println!("-- per_inode_meta --");
        println!("  cached_file_size_count={}  metadata_dirty_count={}", file_size_cached, meta_dirty);

        // 3. inode_cache
        let (ic_len, ic_dirty, ic_clean) = {
            let cache = self.inode_cache.lock();
            let len = cache.len();
            let dirty = cache.iter().filter(|(_, c)| c.lock().dirty).count();
            (len, dirty, len - dirty)
        };
        println!("-- inode_cache --");
        println!("  len={}  dirty={}  clean={}", ic_len, ic_dirty, ic_clean);

        let (mbc_len, mbc_dirty, mbc_clean) = self.meta_block_cache.stats();
        println!("-- meta_block_cache --");
        println!("  len={}  dirty={}  clean={}", mbc_len, mbc_dirty, mbc_clean);

        // 4. page_caches registry — 先收集 alive Arc，释放锁再统计
        let (pc_reg_len, pc_alive, pc_stale, cached_pages, dirty_pages) = {
            let reg = self.page_caches.lock();
            let len = reg.len();
            let alive_arcs: alloc::vec::Vec<alloc::sync::Arc<crate::fs::page_cache::PageCache>> =
                reg.iter().filter_map(|(_, w)| w.upgrade()).collect();
            let alive = alive_arcs.len();
            drop(reg);
            let mut total_cached = 0usize;
            let mut total_dirty = 0usize;
            for pc in &alive_arcs {
                total_cached += pc.cached_page_count();
                total_dirty += pc.dirty_count();
            }
            (len, alive, len - alive, total_cached, total_dirty)
        };
        println!("-- page_cache --");
        println!("  registry_len={}  alive={}  stale={}  cached_pages={}  dirty_pages={}",
            pc_reg_len, pc_alive, pc_stale, cached_pages, dirty_pages);

        // 5. meta_batch state
        let batch_active = self.meta_batch_active.load(core::sync::atomic::Ordering::Relaxed);
        let batch_sb = self.meta_batch_sb.lock().is_some();
        let batch_bgs_len = self.meta_batch_bgs.lock().len();
        println!("-- meta_batch --");
        println!("  active={}  sb={}  bgs_len={}", batch_active, batch_sb, batch_bgs_len);

        // 6. approximate memory estimate
        let approx_ext4 = ic_len * 512; // ~512 bytes per cached ext4 inode
        let approx_page = cached_pages * 4096; // actual cached pages × PAGE_SIZE
        println!("-- memory_est --");
        println!("  approx_ext4_cache={}  approx_page_cache={}", approx_ext4, approx_page);
    }
}

/// 检查 name 是否为 . 或 .. — 这些不应进入 children cache
fn map_create_error(e: isize) -> SyscallErr {
    if e == crate::syscall::errno::ENOENT { SyscallErr::ENOENT }
    else if e == crate::syscall::errno::EEXIST { SyscallErr::EEXIST }
    else if e == crate::syscall::errno::ENOSPC { SyscallErr::ENOSPC }
    else if e == crate::syscall::errno::EIO { SyscallErr::EIO }
    else { SyscallErr::ENOSYS }
}

fn is_special_dot(name: &str) -> bool {
    name == "." || name == ".."
}

impl NewFileSystem for Ext4FileSystem {
    fn root_inode(&self) -> alloc::sync::Arc<dyn crate::fs::vfs::IndexNode> {
        self.canonical_inode_object(ROOT_INODE)
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

    fn on_umount(&self) {
        crate::fs::page_cache::flush_all_page_caches();
        self.flush_metadata_cache();
    }

    fn as_any_ref(&self) -> &dyn core::any::Any {
        self
    }
}
