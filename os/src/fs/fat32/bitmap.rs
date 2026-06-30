use super::layout::BAD_BLOCK;
use super::BlockDevice;
use crate::hal::BLOCK_SZ;
use alloc::{collections::VecDeque, sync::Arc, vec::Vec};
use spin::{Mutex, MutexGuard};

const VACANT_CLUS_CACHE_SIZE: usize = 64;
const FAT_ENTRY_FREE: u32 = 0;
const FAT_ENTRY_RESERVED_TO_END: u32 = 0x0FFF_FFF8;
pub const EOC: u32 = 0x0FFF_FFFF;
const SECTORS_PER_BLOCK: usize = BLOCK_SZ / 512;
/// *In-memory* data structure
/// 内存内的fat数据结构.
/// 在Fat32文件系统中，有两个fat表，这里只使用第一张fat表
///
/// # Limitations
///
/// 未实现 FAT 检错功能：当前仅读取第一张 FAT 表，不利用第二张 FAT 表
/// 进行数据完整性校验。Exit condition: 实现双 FAT 表交叉验证或至少检测不一致。
pub struct Fat {
    /// The first block id of FAT.
    /// In FAT32, this is equal to bpb.rsvd_sec_cnt
    start_block_id: usize,
    /// size fo sector in bytes copied from BPB
    byts_per_sec: usize,
    /// The total number of FAT entries
    tot_ent: usize,
    /// The queue used to store known vacant clusters
    vacant_clus: Mutex<VecDeque<u32>>,
    /// The final unused cluster id we found
    hint: Mutex<usize>,
}

impl Fat {
    /// 从 FAT 表中读取指定扇区的 u32 entry（直接块设备读，替代旧 BlockCacheManager）
    fn read_fat_entry(&self, block_device: &Arc<dyn BlockDevice>, sec_num: usize, offset: usize) -> u32 {
        // FAT 扇区是 512 字节，BlockDevice 以 BLOCK_SZ(4096) 为单位
        let block_id = sec_num / SECTORS_PER_BLOCK;
        let sector_off = (sec_num % SECTORS_PER_BLOCK) * 512;
        let mut buf = alloc::vec![0u8; BLOCK_SZ];
        block_device.read_block(block_id, &mut buf);
        let off = sector_off + offset;
        u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
    }

    fn write_fat_entry(&self, block_device: &Arc<dyn BlockDevice>, sec_num: usize, offset: usize, val: u32) {
        let block_id = sec_num / SECTORS_PER_BLOCK;
        let sector_off = (sec_num % SECTORS_PER_BLOCK) * 512;
        let mut buf = alloc::vec![0u8; BLOCK_SZ];
        block_device.read_block(block_id, &mut buf);
        let off = sector_off + offset;
        buf[off..off + 4].copy_from_slice(&val.to_le_bytes());
        block_device.write_block(block_id, &buf);
    }

    /// 获取当前fat表项指向的的下一个簇号
    pub fn get_next_clus_num(
        &self,
        current_clus_num: u32,
        block_device: &Arc<dyn BlockDevice>,
    ) -> u32 {
        self.read_fat_entry(
            block_device,
            self.this_fat_sec_num(current_clus_num),
            self.this_fat_ent_offset(current_clus_num),
        ) & EOC
    }

    /// Get all cluster numbers after the current cluster number
    pub fn get_all_clus_num(
        &self,
        mut current_clus_num: u32,
        block_device: &Arc<dyn BlockDevice>,
    ) -> Vec<u32> {
        let mut v = Vec::with_capacity(8);
        loop {
            v.push(current_clus_num);
            current_clus_num = self.get_next_clus_num(current_clus_num, &block_device);
            if [BAD_BLOCK, FAT_ENTRY_FREE].contains(&current_clus_num)
                || current_clus_num >= FAT_ENTRY_RESERVED_TO_END
            {
                break;
            }
        }
        v
    }

    /// Constructor for fat
    pub fn new(
        rsvd_sec_cnt: usize,
        byts_per_sec: usize,
        clus: usize,
    ) -> Self {
        Self {
            start_block_id: rsvd_sec_cnt,
            byts_per_sec,
            tot_ent: clus,
            vacant_clus: Mutex::new(VecDeque::new()),
            hint: Mutex::new(0),
        }
    }

    #[inline(always)]
    pub fn this_fat_sec_num(&self, clus_num: u32) -> usize {
        let fat_offset = clus_num * 4;
        (self.start_block_id as u32 + (fat_offset / (self.byts_per_sec as u32))) as usize
    }

    #[inline(always)]
    pub fn this_fat_ent_offset(&self, clus_num: u32) -> usize {
        let fat_offset = clus_num * 4;
        (fat_offset % (self.byts_per_sec as u32)) as usize
    }

    /// 将簇项从当前指向下一个
    fn set_next_clus(&self, block_device: &Arc<dyn BlockDevice>, current: Option<u32>, next: u32) {
        if current.is_none() {
            return;
        }
        let current = current.unwrap();
        self.write_fat_entry(
            block_device,
            self.this_fat_sec_num(current),
            self.this_fat_ent_offset(current),
            next,
        )
    }

    pub fn alloc(
        &self,
        block_device: &Arc<dyn BlockDevice>,
        alloc_num: usize,
        mut last: Option<u32>,
    ) -> Vec<u32> {
        let mut allocated_cluster = Vec::with_capacity(alloc_num);
        let mut hlock = self.hint.lock();
        for _ in 0..alloc_num {
            last = self.alloc_one(block_device, last, &mut hlock);
            if last.is_none() {
                log::error!("[alloc]: alloc error, last: {:?}", last);
                break;
            }
            allocated_cluster.push(last.unwrap());
        }
        self.set_next_clus(block_device, last, EOC);
        allocated_cluster
    }

    fn alloc_one(
        &self,
        block_device: &Arc<dyn BlockDevice>,
        last: Option<u32>,
        hlock: &mut MutexGuard<usize>,
    ) -> Option<u32> {
        if last.is_some() {
            let next_cluster_of_current = self.get_next_clus_num(last.unwrap(), block_device);
            debug_assert!(next_cluster_of_current >= FAT_ENTRY_RESERVED_TO_END);
        }

        if let Some(free_clus_id) = self.vacant_clus.lock().pop_back() {
            self.set_next_clus(block_device, last, free_clus_id);
            return Some(free_clus_id);
        }

        let start = **hlock;
        let free_clus_id = self.get_next_free_clus(start as u32, block_device);
        if free_clus_id.is_none() {
            return None;
        }
        let free_clus_id = free_clus_id.unwrap();
        **hlock = (free_clus_id + 1) as usize % self.tot_ent;

        self.set_next_clus(block_device, last, free_clus_id);
        Some(free_clus_id)
    }

    fn get_next_free_clus(&self, start: u32, block_device: &Arc<dyn BlockDevice>) -> Option<u32> {
        for clus_id in start..self.tot_ent as u32 {
            if FAT_ENTRY_FREE == self.get_next_clus_num(clus_id, block_device) {
                return Some(clus_id);
            }
        }
        for clus_id in 0..start {
            if FAT_ENTRY_FREE == self.get_next_clus_num(clus_id, block_device) {
                return Some(clus_id);
            }
        }
        None
    }

    pub fn free(
        &self,
        block_device: &Arc<dyn BlockDevice>,
        cluster_list: Vec<u32>,
        last: Option<u32>,
    ) {
        let mut lock = self.vacant_clus.lock();
        for cluster_id in cluster_list {
            self.set_next_clus(block_device, Some(cluster_id), FAT_ENTRY_FREE);
            if lock.len() < VACANT_CLUS_CACHE_SIZE {
                lock.push_back(cluster_id);
            }
        }
        self.set_next_clus(block_device, last, EOC);
    }
}
