#![allow(unused)]
use core::arch::asm;
use core::ptr::addr_of;

use crate::fs::fat32::FatInode;
use crate::fs::filesystem::FS_Type;
use crate::hal::{self, BLOCK_SZ};

use super::{layout::BPB, Cache};
use super::{BlockCacheManager, BlockDevice, DiskInodeType, Fat};
use crate::fs::directory_tree::VFS;
use crate::fs::vfs::file_system::{FileSystem, FsInfo, SuperBlock};
use crate::fs::vfs::IndexNode;
use alloc::{sync::Arc, vec::Vec};
use core::any::Any;

pub struct EasyFileSystem {
    /// 块设备，实际上是一个指向硬件设备的指针
    pub block_device: Arc<dyn BlockDevice>,
    /// FAT32文件系统的FAT表
    pub fat: Fat,
    /// 在root目录之后的第一个数据扇区
    pub data_area_start_block: u32,
    /// This is set to the cluster number of the first cluster of the root directory,
    /// 根目录的第一个簇的簇号，通常为2，但不一定是2
    pub root_clus: u32,
    /// 每簇扇区数，对于SD卡来说通常为8
    pub sec_per_clus: u8,
    /// 每扇区字节数，对于SD卡来说通常为512
    pub byts_per_sec: u16,
    /// 自身弱引用，用于 FileSystem trait 的 root_inode 方法
    __self_ref: spin::Mutex<Option<alloc::sync::Weak<EasyFileSystem>>>,
}

impl EasyFileSystem {
    pub fn first_data_sector(&self) -> u32 {
        self.data_area_start_block
    }
    #[inline(always)]
    pub fn clus_size(&self) -> u32 {
        self.byts_per_sec as u32 * self.sec_per_clus as u32
    }
}

impl EasyFileSystem {
    /// 对于一个给定的簇号，计算其第一个扇区
    /// # 参数
    /// + `clus_num`: 簇号
    /// # 返回值
    /// 扇区号
    #[inline(always)]
    pub fn first_sector_of_cluster(&self, clus_num: u32) -> u32 {
        // 首先比较每簇扇区数中1的数量，因为是8,所以只有1个（0b100）
        debug_assert_eq!(self.sec_per_clus.count_ones(), 1);
        // 然后比较簇号，看是否大于等于2,因为前两个簇0和1已经被占用
        debug_assert!(clus_num >= 2);
        // 获取第一个数据扇区
        let start_block = self.data_area_start_block;
        // 获取偏移量
        // 计算公式为 ：
        // (簇号 - 2) * 每簇扇区数 =
        // (簇号 - 2) * 8
        let offset_blocks = (clus_num - 2) * self.sec_per_clus as u32;
        // 第一个扇区号即为
        // root目录后的第一个数据扇区号 + 偏移量
        start_block + offset_blocks
    }
    /// 打开文件系统对象
    /// # 参数
    /// + `block_device`: 指向硬件设备（存储设备）的指针
    /// + `index_cache_mgr`: fat cache manager
    pub fn open(
        block_device: Arc<dyn BlockDevice>,
        index_cache_mgr: Arc<spin::Mutex<BlockCacheManager>>,
    ) -> Arc<Self> {
        let fat_cache_mgr = index_cache_mgr.clone();

        // 先读取 BPB 获取文件系统参数
        let (root_clus, sec_per_clus, byts_per_sec, data_area_start_block,
             rsvd_sec_cnt, data_sector_count) =
            index_cache_mgr
                .lock()
                .get_block_cache(0, &block_device)
                .lock()
                .read(0, |super_block: &BPB| {
                    debug_assert!(super_block.byts_per_sec as usize == hal::BLOCK_SZ);
                    debug_assert!(BlockCacheManager::CACHE_SZ % super_block.byts_per_sec as usize == 0);
                    debug_assert!(super_block.is_valid(), "Error loading EFS!");
                    (
                        super_block.root_clus,
                        super_block.sec_per_clus,
                        super_block.byts_per_sec,
                        super_block.first_data_sector(),
                        super_block.rsvd_sec_cnt as usize,
                        super_block.data_sector_count(),
                    )
                });

        // 用 Arc::new_cyclic 初始化 __self_ref
        Arc::new_cyclic(|weak| {
            Self {
                block_device,
                fat: Fat::new(
                    rsvd_sec_cnt,
                    byts_per_sec as usize,
                    (data_sector_count / sec_per_clus as u32) as usize,
                    fat_cache_mgr,
                ),
                root_clus,
                sec_per_clus,
                byts_per_sec,
                data_area_start_block,
                __self_ref: spin::Mutex::new(Some(weak.clone())),
            }
        })
    }
    pub fn alloc_blocks(&self, blocks: usize) -> Vec<usize> {
        let sec_per_clus = self.sec_per_clus as usize;
        let alloc_num = (blocks - 1 + sec_per_clus) / sec_per_clus;
        let clus = self.fat.alloc(&self.block_device, alloc_num, None);
        debug_assert_eq!(clus.len(), alloc_num);
        let mut block_ids = Vec::<usize>::with_capacity(alloc_num * sec_per_clus);
        for clus_id in clus {
            let first_sec = self.first_sector_of_cluster(clus_id) as usize;
            for offset in 0..sec_per_clus {
                block_ids.push(first_sec + offset);
            }
        }
        block_ids
    }
}

impl core::fmt::Debug for EasyFileSystem {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EasyFileSystem")
            .field("root_clus", &self.root_clus)
            .field("byts_per_sec", &self.byts_per_sec)
            .field("sec_per_clus", &self.sec_per_clus)
            .finish()
    }
}

// Old VFS trait impl — kept for directory_tree.rs compatibility (Phase 5 will remove)
impl VFS for EasyFileSystem {
    fn alloc_blocks(&self, blocks: usize) -> Vec<usize> {
        self.alloc_blocks(blocks)
    }
    fn get_filesystem_type(&self) -> FS_Type {
        FS_Type::Fat32
    }
    fn block_size(&self) -> usize {
        BLOCK_SZ
    }
}

impl FileSystem for EasyFileSystem {
    fn root_inode(&self) -> Arc<dyn IndexNode> {
        let arc_self = self.__self_ref.lock().as_ref()
            .and_then(|w| w.upgrade())
            .expect("EasyFileSystem: __self_ref not initialized");
        FatInode::new(
            self.root_clus,
            DiskInodeType::Directory,
            None,
            None,
            arc_self,
        )
    }

    fn info(&self) -> FsInfo {
        FsInfo {
            blk_dev_id: 0,
            max_name_len: 255,
            features: alloc::vec!["fat32"],
        }
    }

    fn name(&self) -> &str {
        "fat32"
    }

    fn super_block(&self) -> SuperBlock {
        SuperBlock {
            f_type: 0x4d44,
            f_bsize: self.byts_per_sec as u64,
            f_blocks: 0,
            f_bfree: 0,
            f_bavail: 0,
            f_files: 0,
            f_ffree: 0,
            f_fsid: [0; 2],
            f_namelen: 255,
            f_frsize: self.byts_per_sec as u64,
            flags: 0,
            f_spare: [0; 4],
        }
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}
