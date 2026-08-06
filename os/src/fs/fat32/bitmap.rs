use super::layout::BAD_BLOCK;
use super::BlockDevice;
use alloc::{collections::VecDeque, sync::Arc, vec::Vec};
use spin::Mutex;

const VACANT_CLUS_CACHE_SIZE: usize = 64;
const FAT_ENTRY_FREE: u32 = 0;
const FAT_ENTRY_RESERVED_TO_END: u32 = 0x0FFF_FFF8;
pub const EOC: u32 = 0x0FFF_FFFF;

/// 卷级 FAT 变更状态的唯一所有物。
///
/// SMP 并发下，空闲簇搜索、占用标记、链指针修改、回收缓存与空闲计数必须属于
/// 同一个临界区：Linux VFAT 用 per-superblock `fat_lock` 串行化全部 FAT 表修改，
/// DragonOS 同样以卷级 `fat_lock: Mutex<()>` 修复并发簇分配。本项目没有 Linux 的
/// FAT buffer cache，同一 FAT 扇区的读-改-写无法原子合成，因此把 `hint` 与
/// `vacant_clus` 合并为单一 `Mutex<FatMutationState>`，让 alloc 与 free 互斥，
/// 并覆盖到所有 FAT 副本的扇区写回完成。
struct FatMutationState {
    /// 回收的空闲簇缓存（最多 `VACANT_CLUS_CACHE_SIZE` 项）。
    vacant_clus: VecDeque<u32>,
    /// 下次空闲扫描起点。
    hint: usize,
}

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
    /// 卷级 FAT 变更事务状态：alloc 与 free 在此锁内串行。
    mutation: Mutex<FatMutationState>,
}

impl Fat {
    /// Read one FAT32 entry. The mount path has already adapted `block_device`
    /// so block IDs are expressed in BPB_BytsPerSec units.
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
        block_device.read_block(sector, &mut buf);
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
            block_device.read_block(sector, &mut buf);
            let old = u32::from_le_bytes([
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            ]);
            let updated = (old & 0xF000_0000) | (val & EOC);
            buf[offset..offset + 4].copy_from_slice(&updated.to_le_bytes());
            block_device.write_block(sector, &buf);
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
            mutation: Mutex::new(FatMutationState {
                vacant_clus: VecDeque::new(),
                hint: 2,
            }),
        }
    }

    #[inline(always)]
    pub fn this_fat_sector_offset(&self, clus_num: u32) -> usize {
        let fat_offset = clus_num * 4;
        (fat_offset / self.byts_per_sec as u32) as usize
    }

    /// 卷的簇号上界（不含）：有效数据簇范围为 `2..max_cluster_exclusive`。
    ///
    /// 供零盘 ktest 的簇表完整性扫描使用（越界、双归属、环检测）。
    #[inline(always)]
    pub fn max_cluster_exclusive(&self) -> usize {
        self.max_cluster_exclusive
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

    /// 分配 `alloc_num` 个连续链接的新簇，返回新簇列表（不含原有尾簇）。
    ///
    /// 整个分配在 `mutation` 锁内完成：空闲簇搜索、把候选簇先写 EOC、把前驱
    /// 链接到候选、更新 hint/回收缓存，全部在锁内串行，任何其他分配/释放都
    /// 必须等所有 FAT 副本写回后才能进入。候选先写 EOC 再链接前驱，保证无锁
    /// 链读取者永远不会看到前驱指向一个仍为 FREE 的簇。
    pub fn alloc(
        &self,
        block_device: &Arc<dyn BlockDevice>,
        alloc_num: usize,
        mut last: Option<u32>,
    ) -> Vec<u32> {
        let mut allocated_cluster = Vec::with_capacity(alloc_num);
        let mut state = self.mutation.lock();
        for _ in 0..alloc_num {
            last = self.alloc_one(block_device, last, &mut state);
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
        state: &mut spin::MutexGuard<'_, FatMutationState>,
    ) -> Option<u32> {
        if last.is_some() {
            let next_cluster_of_current = self.get_next_clus_num(last.unwrap(), block_device);
            debug_assert!(next_cluster_of_current >= FAT_ENTRY_RESERVED_TO_END);
        }

        // 优先复用回收缓存；缓存为空才扫描 FAT。取出的簇已被 free() 标为 FREE，
        // 且仍在 mutation 锁内，其他 CPU 不可能同时分配它。
        if let Some(free_clus_id) = state.vacant_clus.pop_back() {
            // 与扫描路径一致：先写候选 EOC 再链接前驱。alloc() 末尾还会把最后一
            // 个候选统一写 EOC，但这里必须先占位——否则在"last 已链接"与"候选写
            // EOC"之间的窗口里，无锁链读取者会看到 last 指向一个仍为 FREE(0) 的簇。
            self.set_next_clus(block_device, Some(free_clus_id), EOC);
            self.set_next_clus(block_device, last, free_clus_id);
            return Some(free_clus_id);
        }

        let start = state.hint;
        let free_clus_id = self.get_next_free_clus(start as u32, block_device);
        if free_clus_id.is_none() {
            return None;
        }
        let free_clus_id = free_clus_id.unwrap();
        state.hint = if free_clus_id as usize + 1 < self.max_cluster_exclusive {
            free_clus_id as usize + 1
        } else {
            2
        };

        // 先写候选 EOC（原子占位），再链接前驱；批次末尾的 EOC 由 alloc() 统一写。
        self.set_next_clus(block_device, Some(free_clus_id), EOC);
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

    /// 释放一条簇链的后缀。
    ///
    /// 调用方传入的 `cluster_list` 是链上**从尾部逆序 pop** 得到的后缀：首元素是
    /// 链末簇，末元素是紧邻保留尾簇的簇（dealloc_clus 从 `clus_list` 尾部逐个 pop，
    /// 第一个 pop 出来的就是链尾）。本函数在同一 `mutation` 锁内先断链：把保留尾簇
    /// 写 EOC，使无锁读取者不再从保留链进入待释放后缀；随后逐项写 FREE，最后把
    /// 回收簇加入缓存。
    pub fn free(
        &self,
        block_device: &Arc<dyn BlockDevice>,
        cluster_list: Vec<u32>,
        last: Option<u32>,
    ) {
        let mut state = self.mutation.lock();
        // 1) 先断链：保留尾簇指向 EOC，释放后缀从活跃链分离。
        //    调用方语义：cluster_list 逆序（首项=链末簇，末项=紧邻保留尾簇的簇）。
        //    必须验证保留尾簇的 next 确实指向释放后缀的"末项"，防止按错误顺序释放
        //    导致链损坏。
        if let Some(retained) = last {
            let next_of_retained = self.get_next_clus_num(retained, block_device);
            if cluster_list.last().copied() != Some(next_of_retained)
                && !(next_of_retained >= FAT_ENTRY_RESERVED_TO_END && cluster_list.is_empty())
            {
                // 逆序列表末项应等于保留尾簇当前指向；若保留尾已指向 EOC/EOF 且
                // 列表为空则无需断链。这里只防御明显不一致，不覆盖全部损坏情形。
                debug_assert!(
                    cluster_list.last().copied() == Some(next_of_retained)
                        || (next_of_retained >= FAT_ENTRY_RESERVED_TO_END
                            && cluster_list.is_empty()),
                    "free: retained tail does not point at the freed suffix head"
                );
            }
            self.set_next_clus(block_device, Some(retained), EOC);
        }
        // 2) 锁内逐项写 FREE。cluster_list 按 tail-first 逆序（首项是链末簇）。首项
        //    的 next 必须是 EOC/保留值（alloc 写入的链尾标记）；其余簇的 next 指向
        //    已经被本循环处理（写 FREE）的后继簇号，这是 tail-first 释放的正常形态，
        //    不是 double-free。
        for (index, cluster_id) in cluster_list.iter().enumerate() {
            debug_assert!(*cluster_id >= 2 && *cluster_id < self.max_cluster_exclusive as u32);
            if index == 0 {
                let tail_next = self.get_next_clus_num(*cluster_id, block_device);
                debug_assert!(
                    tail_next >= FAT_ENTRY_RESERVED_TO_END,
                    "free: freed suffix tail {} is not EOC (next={:#x})",
                    cluster_id,
                    tail_next
                );
            }
            self.set_next_clus(block_device, Some(*cluster_id), FAT_ENTRY_FREE);
            if state.vacant_clus.len() < VACANT_CLUS_CACHE_SIZE {
                state.vacant_clus.push_back(*cluster_id);
            }
        }
    }
}
