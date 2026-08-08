use super::layout::BAD_BLOCK;
use super::BlockDevice;
use alloc::{collections::VecDeque, sync::Arc, vec::Vec};
use spin::{Mutex, MutexGuard};

const VACANT_CLUS_CACHE_SIZE: usize = 64;
const FAT_ENTRY_FREE: u32 = 0;
const FAT_ENTRY_RESERVED_TO_END: u32 = 0x0FFF_FFF8;
pub const EOC: u32 = 0x0FFF_FFFF;
/// *In-memory* data structure
/// 内存内的fat数据结构.
/// FAT32 normally keeps mirrored FAT copies. Reads use the active copy selected
/// by BPB_ExtFlags, and updates are mirrored unless the BPB explicitly disables
/// mirroring.
pub struct Fat {
    /// The first block id of FAT.
    /// In FAT32, this is equal to bpb.rsvd_sec_cnt
    start_block_id: usize,
    /// size fo sector in bytes copied from BPB
    byts_per_sec: usize,
    /// Number of sectors occupied by one FAT copy.
    sectors_per_fat: usize,
    /// Number of FAT copies recorded in the BPB.
    num_fats: usize,
    /// FAT copy used for reads when BPB_ExtFlags disables mirroring.
    active_fat: usize,
    /// Whether writes must be mirrored to every FAT copy.
    mirror_writes: bool,
    /// The total number of FAT entries
    max_cluster_exclusive: usize,
    /// The queue used to store known vacant clusters
    vacant_clus: Mutex<VecDeque<u32>>,
    /// The final unused cluster id we found
    hint: Mutex<usize>,
}

impl Fat {
    /// 把 FAT32 扇区号换算为 BlockDevice 的父块号 + 块内字节偏移。
    ///
    /// BlockDevice 的 `read_block`/`write_block` 以 BLOCK_SZ(4096) 字节为单位
    /// 编址，而 FAT32 内部所有扇区号（FAT 起始、FAT 扇区偏移）均以
    /// BPB_BytsPerSec(512) 为单位，因此访问磁盘前必须先做换算。
    #[inline(always)]
    fn sector_to_parent(&self, sector: usize) -> (usize, usize) {
        let sectors_per_block = crate::hal::BLOCK_SZ / self.byts_per_sec;
        let block_id = sector / sectors_per_block;
        let block_off = (sector % sectors_per_block) * self.byts_per_sec;
        (block_id, block_off)
    }

    /// 读取一个 FAT 扇区（byts_per_sec 字节）：整块(4096)读入后切片。
    fn read_fat_sector(&self, block_device: &Arc<dyn BlockDevice>, sector: usize, buf: &mut [u8]) {
        assert_eq!(buf.len(), self.byts_per_sec);
        let (block_id, block_off) = self.sector_to_parent(sector);
        let mut block = alloc::vec![0u8; crate::hal::BLOCK_SZ];
        block_device.read_block(block_id, &mut block).ok();
        buf.copy_from_slice(&block[block_off..block_off + self.byts_per_sec]);
    }

    /// 写入一个 FAT 扇区（byts_per_sec 字节）：整块读出 → 修改 → 整块写回，
    /// 避免破坏同一 4096 字节块内相邻扇区的数据。
    fn write_fat_sector(&self, block_device: &Arc<dyn BlockDevice>, sector: usize, buf: &[u8]) {
        assert_eq!(buf.len(), self.byts_per_sec);
        let (block_id, block_off) = self.sector_to_parent(sector);
        let mut block = alloc::vec![0u8; crate::hal::BLOCK_SZ];
        block_device.read_block(block_id, &mut block).ok();
        block[block_off..block_off + self.byts_per_sec].copy_from_slice(buf);
        block_device.write_block(block_id, &block).ok();
    }

    /// Read one FAT32 entry.
    fn read_fat_entry(
        &self,
        block_device: &Arc<dyn BlockDevice>,
        fat_sector_offset: usize,
        offset: usize,
    ) -> u32 {
        assert!(fat_sector_offset < self.sectors_per_fat);
        assert!(offset
            .checked_add(4)
            .is_some_and(|end| end <= self.byts_per_sec));
        let sector =
            self.start_block_id + self.active_fat * self.sectors_per_fat + fat_sector_offset;
        let mut buf = alloc::vec![0u8; self.byts_per_sec];
        self.read_fat_sector(block_device, sector, &mut buf);
        u32::from_le_bytes([
            buf[offset],
            buf[offset + 1],
            buf[offset + 2],
            buf[offset + 3],
        ])
    }

    fn write_fat_entry(
        &self,
        block_device: &Arc<dyn BlockDevice>,
        fat_sector_offset: usize,
        offset: usize,
        val: u32,
    ) {
        assert!(fat_sector_offset < self.sectors_per_fat);
        assert!(offset
            .checked_add(4)
            .is_some_and(|end| end <= self.byts_per_sec));
        let first_fat = if self.mirror_writes {
            0
        } else {
            self.active_fat
        };
        let fat_count = if self.mirror_writes { self.num_fats } else { 1 };
        for fat_index in first_fat..first_fat + fat_count {
            let sector = self.start_block_id + fat_index * self.sectors_per_fat + fat_sector_offset;
            let mut buf = alloc::vec![0u8; self.byts_per_sec];
            self.read_fat_sector(block_device, sector, &mut buf);
            let old = u32::from_le_bytes([
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            ]);
            let updated = (old & 0xF000_0000) | (val & EOC);
            buf[offset..offset + 4].copy_from_slice(&updated.to_le_bytes());
            self.write_fat_sector(block_device, sector, &buf);
        }
    }

    /// 获取当前fat表项指向的的下一个簇号
    pub fn get_next_clus_num(
        &self,
        current_clus_num: u32,
        block_device: &Arc<dyn BlockDevice>,
    ) -> u32 {
        self.read_fat_entry(
            block_device,
            self.this_fat_sector_offset(current_clus_num),
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
        sectors_per_fat: usize,
        num_fats: usize,
        ext_flags: u16,
        cluster_count: usize,
    ) -> Self {
        assert!(byts_per_sec >= 512 && byts_per_sec.is_power_of_two());
        assert!(sectors_per_fat > 0);
        assert!(num_fats > 0);
        let mirror_writes = ext_flags & 0x0080 == 0;
        let active_fat = if mirror_writes {
            0
        } else {
            (ext_flags & 0x000f) as usize
        };
        assert!(active_fat < num_fats, "active FAT index is out of range");
        let max_cluster_exclusive = cluster_count
            .checked_add(2)
            .expect("FAT cluster count overflow");
        let fat_entry_capacity = sectors_per_fat
            .checked_mul(byts_per_sec)
            .and_then(|bytes| bytes.checked_div(4))
            .expect("FAT entry capacity overflow");
        assert!(
            max_cluster_exclusive <= fat_entry_capacity,
            "data cluster count exceeds FAT capacity"
        );
        Self {
            start_block_id: rsvd_sec_cnt,
            byts_per_sec,
            sectors_per_fat,
            num_fats,
            active_fat,
            mirror_writes,
            // FAT cluster numbers start at 2, so N data clusters occupy IDs
            // 2..N+2 rather than 0..N.
            max_cluster_exclusive,
            vacant_clus: Mutex::new(VecDeque::new()),
            hint: Mutex::new(2),
        }
    }

    #[inline(always)]
    pub fn this_fat_sector_offset(&self, clus_num: u32) -> usize {
        let fat_offset = clus_num * 4;
        (fat_offset / self.byts_per_sec as u32) as usize
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
            self.this_fat_sector_offset(current),
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
        **hlock = if free_clus_id as usize + 1 < self.max_cluster_exclusive {
            free_clus_id as usize + 1
        } else {
            2
        };

        self.set_next_clus(block_device, last, free_clus_id);
        Some(free_clus_id)
    }

    fn get_next_free_clus(&self, start: u32, block_device: &Arc<dyn BlockDevice>) -> Option<u32> {
        let start = start.max(2).min(self.max_cluster_exclusive as u32);
        for clus_id in start..self.max_cluster_exclusive as u32 {
            if FAT_ENTRY_FREE == self.get_next_clus_num(clus_id, block_device) {
                return Some(clus_id);
            }
        }
        for clus_id in 2..start {
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
