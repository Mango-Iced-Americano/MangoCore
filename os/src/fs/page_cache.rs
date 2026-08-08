//! 页面缓存 — VFS 层的数据缓存机制
//!
//! 对标 DragonOS `kernel/src/filesystem/page_cache.rs` 的 `PageCache`。
//!
//! 设计思想：
//! - `PageCacheBackend` trait：将 PageCache 桥接到具体的存储后端（块设备、inode 等）
//! - `PageState` 状态机：Loading → UpToDate ↔ Dirty → Writeback → UpToDate
//! - 两阶段读写：持锁收集拷贝项，解锁后拷贝到/从用户缓冲区，避免死锁
//! - 脏页追踪：每页原子 dirty 标志扫描脏页
//! - 回写机制：单页回写 + 范围回写
//!
//! # Limitations
//!
//! 当前实现仅支持同步 I/O 模型：不含异步 I/O 提交/完成队列、不含 VMA 反向映射
//! （`map_pages` / `fault` 回调）、不含 `O_DIRECT` 绕过 PageCache 的路径。

use crate::utils::error::SyscallErr;
use alloc::collections::BTreeSet;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use spin::{Mutex, RwLock};

use super::vfs::IndexNode;
use crate::config::{PAGE_SIZE, PAGE_SIZE_BITS};
use crate::mm::{frame_alloc, frame_alloc_uninit, FrameTracker};
use crate::task::perf;

// ── Global dirty page accounting ──────────────────────────────────────

/// 全局脏页计数（所有 PageCache 的总和）
static GLOBAL_DIRTY_PAGES: AtomicUsize = AtomicUsize::new(0);
/// 全局正在写回的页面计数
static GLOBAL_WRITEBACK_PAGES: AtomicUsize = AtomicUsize::new(0);
/// 后台写回互斥标志（防止并发写回）
static WRITEBACK_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Number of sub-page writes since the last periodic dirty-pressure check.
static SMALL_WRITE_BALANCE_COUNTER: AtomicUsize = AtomicUsize::new(0);

// ── 三段脏页写回水位 ─────────────────────────────────────────────────
///
/// 水位从低到高，替换此前单一 65536 页（256MB）门槛。
/// 起始值由 Oracle 分析建议，最终值由双架构长写测试校准。
///
/// background（32MB）：触发异步后台写回，不阻塞写入者。
const DIRTY_BACKGROUND_PAGES: usize = 8_192;
/// throttle（64MB）：写入者帮助完成一批写回后再继续写入。
const DIRTY_THROTTLE_PAGES: usize = 16_384;
/// emergency（128MB）：写入者强制同步写回直到低于此水位。
const DIRTY_EMERGENCY_PAGES: usize = 32_768;

/// 后台写回仍然要求脏页占空闲页一定比例（避免在大量空闲内存时
/// 过早触发写回）。
const DIRTY_BACKGROUND_FREE_RATIO_NUMERATOR: usize = 1;
const DIRTY_BACKGROUND_FREE_RATIO_DENOMINATOR: usize = 4;

/// 阈值也受当前空闲物理页帧数约束；紧急写回仅在脏页超过空闲页
/// 的 75% 或超过绝对紧急门槛时触发。
const DIRTY_EMERGENCY_FREE_RATIO_NUMERATOR: usize = 3;
const DIRTY_EMERGENCY_FREE_RATIO_DENOMINATOR: usize = 4;

/// 正常后台写回批次大小（单次最多写回的连续脏页数）
const WB_BATCH_PAGES: usize = 256;
/// 后台写回单轮总预算（跨所有 PageCache 的累计上限）
const WB_BG_MAX_PAGES: usize = 256;
/// Synchronous writeback waits through short-lived page or backend ownership.
const WRITEBACK_RETRY_LIMIT: usize = 100;
/// Small writes only need periodic dirty-pressure checks because the first
/// dirty watermark is 32 MiB.
const BALANCE_EVERY_N_SMALL_WRITES: usize = 16;

/// 全局脏页计数快照（用于诊断）
pub fn global_dirty_pages() -> usize {
    GLOBAL_DIRTY_PAGES.load(Ordering::Relaxed)
}

/// 全局写回页计数快照（用于诊断）
pub fn global_writeback_pages() -> usize {
    GLOBAL_WRITEBACK_PAGES.load(Ordering::Relaxed)
}

#[inline]
fn balance_dirty_pages_for_write(write_len: usize) {
    if write_len >= PAGE_SIZE
        || SMALL_WRITE_BALANCE_COUNTER.fetch_add(1, Ordering::Relaxed)
            % BALANCE_EVERY_N_SMALL_WRITES
            == 0
    {
        balance_dirty_pages();
    }
}

// ── Partial-write validity tracking constants ───────────────────────────
/// 每个 segment 的字节数（512B），用于 partial-write 粒度跟踪
const VALID_SEG_SHIFT: usize = 9;
/// 每页的 segment 数量（4096 / 512 = 8）
const VALID_SEG_COUNT: usize = PAGE_SIZE >> VALID_SEG_SHIFT;
/// 所有 segment 均有效的掩码（8 segments = 0xFF）
const VALID_ALL: u32 = 0xFF;

/// 根据页面在文件中的位置计算初始 valid_mask。
/// 页面超出旧 EOF → VALID_ALL（零填充即有效数据）；
/// 页面跨越 EOF → 仅超出部分为有效零填充；
/// 页面在旧文件内 → 0（数据尚未从后端加载）。
fn initial_valid_mask(page_index: usize, old_file_size: usize) -> u32 {
    let page_start = page_index * PAGE_SIZE;
    if page_start >= old_file_size {
        return VALID_ALL; // entirely beyond EOF → all zeros = valid
    }
    let page_end = page_start + PAGE_SIZE;
    if old_file_size < page_end {
        // page spans EOF: bytes beyond EOF are valid zeros
        let zero_start = old_file_size - page_start;
        return mask_for_range(zero_start, PAGE_SIZE - zero_start);
    }
    0 // existing file page: old data not loaded yet
}

/// 计算 [page_offset, page_offset+len) 区间覆盖的 segment 位掩码
/// 部分覆盖的 segment 也会被标记为有效
fn mask_for_range(page_offset: usize, len: usize) -> u32 {
    if len == 0 {
        return 0;
    }
    let seg_start = page_offset >> VALID_SEG_SHIFT;
    let seg_end =
        ((page_offset + len + (1 << VALID_SEG_SHIFT) - 1) >> VALID_SEG_SHIFT).min(VALID_SEG_COUNT);
    if seg_start >= VALID_SEG_COUNT {
        return 0;
    }
    let count = seg_end - seg_start;
    let low_mask: u32 = if count == 8 {
        VALID_ALL
    } else {
        (1u32 << count) - 1
    };
    low_mask << seg_start
}

static PAGE_CACHE_REGISTRY: Mutex<Vec<Weak<PageCache>>> = Mutex::new(Vec::new());

pub fn register_page_cache(pc: &Arc<PageCache>) {
    PAGE_CACHE_REGISTRY.lock().push(Arc::downgrade(pc));
}

pub fn flush_all_page_caches() -> Result<(), SyscallErr> {
    let page_caches: Vec<Arc<PageCache>> = {
        let mut registry = PAGE_CACHE_REGISTRY.lock();
        let mut page_caches = Vec::new();
        registry.retain(|weak| match weak.upgrade() {
            Some(pc) => {
                page_caches.push(pc);
                true
            }
            None => false,
        });
        page_caches
    };

    let mut first_err: Option<SyscallErr> = None;
    for page_cache in page_caches {
        if let Err(e) = page_cache.writeback_all() {
            log::error!("flush_all_page_caches: writeback_all failed: {:?}", e);
            if first_err.is_none() {
                first_err = Some(e);
            }
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Evict clean pages from all registered caches using clock/second-chance.
/// Called periodically to prevent unbounded PageCache growth.
pub fn evict_all_clean_pages(max_per_cache: usize) -> usize {
    let mut total = 0;
    PAGE_CACHE_REGISTRY.lock().retain(|weak| {
        if let Some(pc) = weak.upgrade() {
            total += pc.evict_clean_pages_clock(max_per_cache);
            true
        } else {
            false
        }
    });
    total
}

/// 返回全局 PageCache registry 统计: (len, capacity, alive, stale)
pub fn registry_stats() -> (usize, usize, usize, usize) {
    let reg = PAGE_CACHE_REGISTRY.lock();
    let len = reg.len();
    let cap = reg.capacity();
    let alive = reg.iter().filter(|w| w.upgrade().is_some()).count();
    let stale = len.saturating_sub(alive);
    (len, cap, alive, stale)
}

/// 聚合所有 alive PageCache 的 entries 统计: (total_len, total_cap, total_live, total_holes)
pub fn entries_global_stats() -> (usize, usize, usize, usize) {
    let mut tlen = 0;
    let mut tcap = 0;
    let mut tlive = 0;
    let mut tholes = 0;
    let reg = PAGE_CACHE_REGISTRY.lock();
    for weak in reg.iter() {
        if let Some(pc) = weak.upgrade() {
            let (len, cap, live, holes) = pc.entries_stats();
            tlen += len;
            tcap += cap;
            tlive += live;
            tholes += holes;
        }
    }
    (tlen, tcap, tlive, tholes)
}

// ── PageState ────────────────────────────────────────────────────────────

/// 页面状态，对标 Linux 的 `PG_*` 标志组合
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PageState {
    /// 页面正在从后端加载
    Loading = 0,
    /// 页面数据是最新的
    UpToDate = 1,
    /// 页面有未写回的脏数据
    Dirty = 2,
    /// 页面正在写回
    Writeback = 3,
    /// 页面发生 I/O 错误
    Error = 4,
}

// ── RaState (read-ahead state) ───────────────────────────────────────────

/// 顺序读预取状态，对标 Linux `file_ra_state`
///
/// 每次 cache miss 时更新；检测到顺序访问后 ramps up 预取窗口。
/// 当前为 per-inode（存储在 FilePrivateData 中，通过 read_at 路径传入）。
#[derive(Debug, Clone)]
pub struct RaState {
    /// 上次访问的最后一页索引
    pub prev_page: usize,
    /// 当前顺序读预取窗口大小（页数）
    pub ra_size: usize,
}

/// 最小预取页数（冷启动窗口）
pub const MIN_RA_PAGES: usize = 4;
/// 最大预取页数（= IO_CHUNK_SIZE / PAGE_SIZE = 64）
pub const MAX_RA_PAGES: usize = 128;

impl RaState {
    pub fn new() -> Self {
        RaState {
            prev_page: 0,
            ra_size: MIN_RA_PAGES,
        }
    }
}

impl Default for RaState {
    fn default() -> Self {
        Self::new()
    }
}

// ── PageCacheBackend ─────────────────────────────────────────────────────

/// 页面缓存后端 trait
/// 具体的存储后端（块设备、inode 等）需要实现此 trait
pub trait PageCacheBackend: Send + Sync {
    /// 从后端读取一页数据到 buf（buf 长度为 PAGE_SIZE）
    fn read_page(&self, index: usize, buf: &mut [u8]) -> Result<usize, SyscallErr>;

    /// 将 buf 中的数据写入后端的一页
    fn write_page(&self, index: usize, buf: &[u8]) -> Result<usize, SyscallErr>;

    /// 批量写回连续页面：start_index..start_index+pages.len()
    /// 默认回退为逐页调用 write_page；支持合并写入的后端（如 EXT4）应覆盖此方法
    fn write_pages(&self, start_index: usize, pages: &[&[u8]]) -> Result<usize, SyscallErr> {
        let mut written = 0;
        for (i, page) in pages.iter().enumerate() {
            written += self.write_page(start_index + i, page)?;
        }
        Ok(written)
    }

    /// 批量读取连续页面：start_index..start_index+pages.len()
    /// pages 为可变切片（每个元素长度 = PAGE_SIZE），从后端批量读取填充。
    /// 默认回退为逐页调用 read_page；支持合并读取的后端（如 EXT4）应覆盖此方法
    fn read_pages(&self, start_index: usize, pages: &mut [&mut [u8]]) -> Result<usize, SyscallErr> {
        for (i, page) in pages.iter_mut().enumerate() {
            self.read_page(start_index + i, *page)?;
        }
        Ok(pages.len() * PAGE_SIZE)
    }

    /// 返回后端的页数
    fn npages(&self) -> usize;
}

// ── PageEntry flags ──────────────────────────────────────────────────────

/// PageEntry flags are orthogonal, mirroring the Linux page flags model.
const PG_LOCKED: u32 = 1 << 0;
const PG_UPTODATE: u32 = 1 << 1;
const PG_DIRTY: u32 = 1 << 2;
const PG_WRITEBACK: u32 = 1 << 3;
const PG_ERROR: u32 = 1 << 4;
pub const PG_REFERENCED: u32 = 1 << 5;
/// 页面在写回期间被再次标记为脏（写回完成后应恢复为 Dirty）
pub const PG_REDIRTIED: u32 = 1 << 6;

// ── PageEntry ────────────────────────────────────────────────────────────

/// 页面缓存条目
#[derive(Debug)]
struct PageEntry {
    /// 物理页面
    page: Arc<FrameTracker>,
    /// Packed page state and flags.  State is derived from the orthogonal bits.
    flags: AtomicU32,
    /// 部分写入有效性位掩码：每 bit 对应 512B segment，1=已写入/有效
    /// 初始值取决于创建方式：populate → VALID_ALL，zero-fill → 0
    valid_mask: AtomicU32,
}

const _: () = assert!(core::mem::size_of::<PageEntry>() == 16);

impl PageEntry {
    fn new(page: Arc<FrameTracker>, state: PageState) -> Self {
        PageEntry {
            page,
            flags: AtomicU32::new(Self::flags_for_state(state)),
            valid_mask: AtomicU32::new(VALID_ALL),
        }
    }

    /// 创建一个带指定 valid_mask 的页面条目（跳过后端读取）
    /// 用于页面超出旧 EOF 的场景：valid_mask=VALID_ALL 表示全零页即有效
    fn new_with_valid_mask(page: Arc<FrameTracker>, valid_mask: u32) -> Self {
        PageEntry {
            page,
            flags: AtomicU32::new(PG_UPTODATE),
            valid_mask: AtomicU32::new(valid_mask),
        }
    }

    fn state(&self) -> PageState {
        Self::decode_state(self.flags.load(Ordering::Acquire))
    }

    fn set_state(&self, state: PageState) {
        self.flags
            .store(Self::flags_for_state(state), Ordering::Release);
    }

    const fn flags_for_state(state: PageState) -> u32 {
        match state {
            PageState::Loading => PG_LOCKED,
            PageState::UpToDate => PG_UPTODATE,
            PageState::Dirty => PG_UPTODATE | PG_DIRTY,
            PageState::Writeback => PG_UPTODATE | PG_WRITEBACK | PG_LOCKED,
            PageState::Error => PG_ERROR,
        }
    }

    fn decode_state(flags: u32) -> PageState {
        if flags & PG_ERROR != 0 {
            PageState::Error
        } else if flags & PG_LOCKED != 0 && flags & PG_UPTODATE == 0 {
            PageState::Loading
        } else if flags & PG_WRITEBACK != 0 {
            PageState::Writeback
        } else if flags & PG_DIRTY != 0 {
            PageState::Dirty
        } else {
            PageState::UpToDate
        }
    }

    fn flags(&self) -> u32 {
        self.flags.load(Ordering::Acquire)
    }

    fn compare_exchange_flags(&self, old: u32, new: u32) -> Result<u32, u32> {
        self.flags
            .compare_exchange(old, new, Ordering::AcqRel, Ordering::Acquire)
    }

    // ── Page flags ──────────────────────────────────────────────────

    fn set_flag(&self, flag: u32) {
        self.flags.fetch_or(flag, Ordering::Release);
    }

    fn clear_flag(&self, flag: u32) {
        self.flags.fetch_and(!flag, Ordering::Release);
    }

    fn test_flag(&self, flag: u32) -> bool {
        (self.flags.load(Ordering::Acquire) & flag) != 0
    }

    /// Test-and-clear a flag atomically. Returns true if the flag was set.
    fn test_and_clear_flag(&self, flag: u32) -> bool {
        let old = self.flags.fetch_and(!flag, Ordering::AcqRel);
        (old & flag) != 0
    }

    fn is_dirty(&self) -> bool {
        self.test_flag(PG_DIRTY)
    }

    fn clear_dirty(&self) -> bool {
        self.test_and_clear_flag(PG_DIRTY)
    }

    fn try_lock_for_write(&self) -> Result<bool, SyscallErr> {
        const MAX_RETRIES: usize = 100;

        for _ in 0..MAX_RETRIES {
            let old = self.flags();
            match Self::decode_state(old) {
                PageState::UpToDate | PageState::Dirty => {
                    let new = old | PG_LOCKED | PG_REFERENCED;
                    if self.compare_exchange_flags(old, new).is_ok() {
                        return Ok(old & PG_DIRTY != 0);
                    }
                    core::hint::spin_loop();
                }
                PageState::Writeback => core::hint::spin_loop(),
                PageState::Loading => return Err(SyscallErr::EAGAIN),
                PageState::Error => return Err(SyscallErr::EIO),
            }
        }

        Err(SyscallErr::EAGAIN)
    }

    fn commit_write(&self) -> bool {
        let mut old = self.flags();
        loop {
            let new = (old | PG_UPTODATE | PG_DIRTY | PG_REFERENCED) & !PG_LOCKED;
            match self.compare_exchange_flags(old, new) {
                Ok(_) => return old & PG_DIRTY == 0,
                Err(current) => old = current,
            }
        }
    }

    fn abort_write(&self) {
        self.clear_flag(PG_LOCKED);
    }

    fn claim_writeback(&self) -> bool {
        let mut old = self.flags();
        loop {
            if old & (PG_DIRTY | PG_LOCKED) != PG_DIRTY {
                return false;
            }
            let new = (old & !PG_DIRTY) | PG_WRITEBACK | PG_LOCKED;
            match self.compare_exchange_flags(old, new) {
                Ok(_) => return true,
                Err(current) => old = current,
            }
        }
    }

    fn complete_writeback(&self) -> bool {
        let redirtied = self.test_and_clear_flag(PG_REDIRTIED);
        let add = if redirtied {
            PG_DIRTY
        } else {
            PG_UPTODATE | PG_REFERENCED
        };
        self.flags
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                Some((old | add) & !(PG_WRITEBACK | PG_LOCKED | PG_REDIRTIED))
            })
            .ok();
        redirtied
    }

    fn restore_dirty_after_writeback(&self) {
        self.flags
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                Some((old | PG_DIRTY) & !(PG_WRITEBACK | PG_LOCKED | PG_REDIRTIED))
            })
            .ok();
    }

    /// 检查所有 segment 是否均已有效
    fn is_fully_valid(&self) -> bool {
        self.valid_mask.load(Ordering::Acquire) == VALID_ALL
    }

    /// 标记写入范围内覆盖的 segment 为有效
    fn mark_valid(&self, page_offset: usize, len: usize) {
        let mask = mask_for_range(page_offset, len);
        if mask != 0 {
            self.valid_mask.fetch_or(mask, Ordering::Release);
        }
    }

    /// 标记写入范围内覆盖的 segment 为有效，并返回此操作后全部 segment 是否均已有效。
    /// 用于检测 sequential writes 逐步填满页面的场景。
    fn mark_valid_and_check_full(&self, page_offset: usize, len: usize) -> bool {
        let mask = mask_for_range(page_offset, len);
        if mask == 0 {
            return self.is_fully_valid();
        }
        // valid_mask only gains bits, so a fully covered range cannot transition again.
        if self.valid_mask.load(Ordering::Relaxed) & mask == mask {
            return false;
        }
        let old = self.valid_mask.fetch_or(mask, Ordering::Release);
        (old | mask) == VALID_ALL
    }

    /// 标记整个页面为有效（用于 ensure_fully_valid 完成后）
    fn mark_fully_valid(&self) {
        self.valid_mask.store(VALID_ALL, Ordering::Release);
    }

    /// 标记页面已被引用（clock eviction 的 second-chance 位）
    fn mark_referenced(&self) {
        self.flags.fetch_or(PG_REFERENCED, Ordering::Release);
    }

    /// 获取指向页数据的指针
    fn as_slice(&self) -> &[u8] {
        self.page.ppn.get_bytes_array()
    }

    /// 获取指向页数据的可变指针
    fn as_slice_mut(&self) -> &mut [u8] {
        self.page.ppn.get_bytes_array()
    }
}

// ── Page-entry directory ─────────────────────────────────────────────────

/// Simple contiguous entry directory. A single mutex keeps publication,
/// removal, and sparse-hole accounting coherent without unsafe pointer
/// publication or permanent radix-node allocations.
struct PageEntries {
    entries: RwLock<Vec<Option<Arc<PageEntry>>>>,
}

impl PageEntries {
    fn new() -> Self {
        Self {
            entries: RwLock::new(Vec::new()),
        }
    }

    fn get(&self, index: usize) -> Option<Arc<PageEntry>> {
        self.entries
            .read()
            .get(index)
            .and_then(|entry| entry.clone())
    }

    fn insert_if_absent(&self, index: usize, candidate: Arc<PageEntry>) -> Arc<PageEntry> {
        let mut entries = self.entries.write();
        while entries.len() <= index {
            entries.push(None);
        }
        match entries[index].as_ref() {
            Some(existing) => existing.clone(),
            None => {
                entries[index] = Some(candidate.clone());
                candidate
            }
        }
    }

    fn remove_if(&self, index: usize, expected: &Arc<PageEntry>) -> Option<Arc<PageEntry>> {
        let mut entries = self.entries.write();
        let entry = entries.get_mut(index)?;
        if entry
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, expected))
        {
            entry.take()
        } else {
            None
        }
    }

    fn live_count(&self) -> usize {
        self.entries
            .read()
            .iter()
            .filter(|entry| entry.is_some())
            .count()
    }

    fn snapshot(&self) -> Vec<(usize, Arc<PageEntry>)> {
        self.entries
            .read()
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| entry.clone().map(|entry| (index, entry)))
            .collect()
    }

    fn stats(&self) -> (usize, usize, usize, usize) {
        let entries = self.entries.read();
        let len = entries.len();
        let live = entries.iter().filter(|entry| entry.is_some()).count();
        (len, entries.capacity(), live, len.saturating_sub(live))
    }
}

// ── Batch read planning types ──────────────────────────────────────────

/// A single page copy instruction collected under entries lock, executed without lock.
struct ReadCopy {
    entry: Arc<PageEntry>,
    dst_offset: usize,  // offset into destination buffer
    page_offset: usize, // offset within the page
    len: usize,
}

/// Contiguous missing page range for batch fill.
struct MissRun {
    start_page: usize,
    count: usize,
}

/// Result of scanning a read range under bounded entries locks.
struct ReadPlan {
    copies: Vec<ReadCopy>,
    miss_runs: Vec<MissRun>,
    // Pages are scanned once in ascending order, so this remains sorted and unique.
    needs_valid_fill: BTreeSet<usize>, // pages that exist but partially valid
}

struct WriteLease {
    page_index: usize,
    entry: Arc<PageEntry>,
    was_dirty: bool,
    newly_installed: bool,
}

#[derive(Default)]
struct WriteStageCycles {
    lookup: usize,
    lease: usize,
    copy: usize,
    commit: usize,
}

// ── PageCache ────────────────────────────────────────────────────────────

/// 单次 backend.read_pages() 批次的页数上限（1 MiB staging）。
///
/// 约束后端 read_pages 的 staging 堆分配大小；与 `fill_miss_runs`
/// 中的 local const 保持一致（不可超过后端 MAX_BATCH_PAGES 上限）。
const MAX_BATCH_PAGES: usize = 256;

/// 页面缓存
///
/// 为 inode 提供页面级别的缓存，管理内存中的文件数据副本。
pub struct PageCache {
    /// 缓存后端
    backend: Mutex<Option<Arc<dyn PageCacheBackend>>>,
    /// 关联的 inode（弱引用）
    inode: Mutex<Option<Weak<dyn IndexNode>>>,
    /// 稀疏页目录，由一个 `Mutex<Vec<Option<Arc<PageEntry>>>>` 维护。
    entries: PageEntries,
    /// true = 页不可回收（用于 tmpfs/shmem，数据无持久化后端）
    unevictable: AtomicBool,
    /// Clock sweep 光标（second-chance eviction）
    clock_hand: AtomicUsize,
    /// Write-balance suppression flag, set by write_without_balance.
    suppress_balance: AtomicBool,
}

impl PageCache {
    /// 创建一个不含 backend 和 inode 关联的空 PageCache，自动注册到全局列表。
    pub fn new() -> Arc<Self> {
        let pc = Arc::new(PageCache {
            backend: Mutex::new(None),
            inode: Mutex::new(None),
            entries: PageEntries::new(),
            unevictable: AtomicBool::new(false),
            clock_hand: AtomicUsize::new(0),
            suppress_balance: AtomicBool::new(false),
        });
        register_page_cache(&pc);
        pc
    }

    /// 绑定用于读写持久化存储的 `PageCacheBackend`。
    pub fn set_backend(&self, backend: Arc<dyn PageCacheBackend>) {
        *self.backend.lock() = Some(backend);
    }

    /// 关联一个 `IndexNode`（`Weak` 引用，不阻止 inode 回收）。
    pub fn set_inode(&self, inode: Weak<dyn IndexNode>) {
        *self.inode.lock() = Some(inode);
    }

    /// 设置不可回收标志（用于 tmpfs/shmem，数据无持久化后端）
    pub fn set_unevictable(&self, val: bool) {
        self.unevictable.store(val, Ordering::Release);
    }

    /// 返回当前绑定的 `PageCacheBackend`（克隆 `Arc`）。
    pub fn backend(&self) -> Option<Arc<dyn PageCacheBackend>> {
        self.backend.lock().clone()
    }

    /// 返回当前缓存的页面条目数。
    pub fn page_count(&self) -> usize {
        self.entries.live_count()
    }

    /// 检查指定索引处的页面条目是否存在且处于 UpToDate 状态。
    pub fn contains_page(&self, page_index: usize) -> bool {
        self.entries.get(page_index).is_some()
    }

    /// 检查指定页面索引是否为脏页。
    pub fn is_dirty(&self, page_index: usize) -> bool {
        self.entries
            .get(page_index)
            .is_some_and(|entry| entry.is_dirty())
    }

    /// 返回当前脏页数（O(n) 扫描，仅供诊断）。
    pub fn dirty_count(&self) -> usize {
        self.find_dirty_pages().len()
    }

    /// 遍历 `entries` 数组统计非 `None` 条目数（O(n) 扫描，仅供诊断）。
    pub fn cached_page_count(&self) -> usize {
        self.entries.live_count()
    }

    /// Evict up to `target` clean pages using clock/second-chance sweep.
    /// Only UpToDate pages held exclusively by the cache (refcount==1) are evicted.
    /// Pages with PG_REFERENCED get a second chance: the bit is cleared and the
    /// page survives this round.
    ///
    /// # Locking
    ///
    /// 扫描目录快照，并仅在移除候选时锁定对应的页槽。
    /// 非重入：调用者不得持有 inode 内部锁。
    ///
    /// # Semantics
    ///
    /// 返回实际回收的页数。对于 `unevictable` 缓存（tmpfs/shmem）直接返回 0。
    pub fn evict_clean_pages_clock(&self, target: usize) -> usize {
        // tmpfs/shmem pages must never be evicted — no persistent backend
        if self.unevictable.load(Ordering::Acquire) {
            return 0;
        }
        let entries = self.entries.snapshot();
        if entries.is_empty() {
            return 0;
        }

        let len = entries.len();
        let mut hand = self.clock_hand.load(Ordering::Relaxed) % len;
        // Bound sweep to prevent runaway scans when the clock loops
        let max_scan = core::cmp::min(len * 2, target.saturating_mul(16).saturating_add(64));
        let mut evicted = 0usize;
        let mut scanned = 0usize;
        while evicted < target && scanned < max_scan {
            let (idx, entry) = &entries[hand];
            hand = (hand + 1) % len;
            scanned += 1;

            crate::task::perf::record_clock_scanned(1);

            // Only evict clean, non-busy pages
            if entry.state() != PageState::UpToDate {
                continue;
            }
            // The snapshot owns one temporary Arc in addition to the cache
            // slot. Any third owner is an mmap or active user of this page.
            if Arc::strong_count(entry) != 2 {
                continue;
            }
            // Frame must not be shared (mmap holds its own Arc<FrameTracker>)
            if Arc::strong_count(&entry.page) != 1 {
                continue;
            }

            let flags = entry.flags();
            if flags & PG_REFERENCED != 0 {
                // Second chance: clear referenced, skip this round
                entry.test_and_clear_flag(PG_REFERENCED);
                crate::task::perf::record_clock_second_chance(1);
                continue;
            }

            // Safe to evict
            if self.entries.remove_if(*idx, entry).is_some() {
                crate::task::perf::record_clock_evicted(1);
                crate::task::perf::record_reclaim_pages_freed(1);
                evicted += 1;
            }
        }

        self.clock_hand.store(hand, Ordering::Relaxed);

        evicted
    }

    /// 回收干净页面（clock/second-chance sweep）
    pub fn shrink_clean_pages(&self, max_to_free: usize) -> usize {
        self.evict_clean_pages_clock(max_to_free)
    }

    /// 返回 entries 元数据: (len, capacity, live, holes)
    pub fn entries_stats(&self) -> (usize, usize, usize, usize) {
        self.entries.stats()
    }

    /// 返回指定索引处页面条目的当前 `PageState`；若索引越界或条目为 `None` 则返回 `None`。
    pub fn state_of(&self, page_index: usize) -> Option<PageState> {
        self.entries.get(page_index).map(|entry| entry.state())
    }

    /// 获取所有脏页索引的快照
    pub fn dirty_pages_snapshot(&self) -> alloc::vec::Vec<usize> {
        self.find_dirty_pages()
    }

    /// 扫描 entries 获取脏页索引；写路径只更新每页原子标志。
    fn find_dirty_pages(&self) -> Vec<usize> {
        self.entries
            .snapshot()
            .into_iter()
            .filter_map(|(index, entry)| entry.is_dirty().then_some(index))
            .collect()
    }

    fn has_transient_pages(&self, start_index: usize, end_index: usize) -> bool {
        self.entries.snapshot().into_iter().any(|(index, entry)| {
            index >= start_index
                && index <= end_index
                && matches!(entry.state(), PageState::Loading | PageState::Writeback)
        })
    }

    // ── 页面获取 ────────────────────────────────────────────────────

    /// 获取或创建一个页面条目
    /// `populate`: 是否从后端读取数据填充
    /// `old_file_size`: 旧文件大小，用于计算初始 valid_mask
    ///   - 页面超出旧 EOF → valid_mask=VALID_ALL，不 populate
    ///   - 页面跨越 EOF → 超出部分 valid_mask 标记，populate 后 OR 入
    ///   - 页面完全在文件中 → valid_mask=0，populate 从后端加载
    ///
    /// 既有页查找只获取目标页槽；帧分配和后端 I/O 均在所有页槽锁外完成。
    fn get_or_create_entry(
        &self,
        page_index: usize,
        populate: bool,
        old_file_size: Option<usize>,
    ) -> Result<Arc<PageEntry>, SyscallErr> {
        let init_mask = old_file_size.map_or(0, |s| initial_valid_mask(page_index, s));
        let beyond_eof = init_mask == VALID_ALL;

        let _t_lookup = perf::perf_time_now();
        let existing = self.entries.get(page_index);
        let lookup_lock_hold = perf::perf_time_now().wrapping_sub(_t_lookup);
        perf::record_pc_lock_hold(lookup_lock_hold, false);
        if let Some(entry) = existing {
            entry.mark_referenced();
            return Ok(entry);
        }

        // 分配和 populate 均在 PageCache 锁外进行；失败时不留下缓存元数据。
        let _t_falloc = perf::perf_time_now();
        let frame = frame_alloc().ok_or(SyscallErr::ENOMEM)?;
        perf::record_pc_falloc_cycles(perf::perf_time_now().wrapping_sub(_t_falloc));

        let candidate = if populate && !beyond_eof {
            // 正常路径：从后端读取数据
            perf::record_pc_miss();
            let entry = Arc::new(PageEntry::new(frame, PageState::UpToDate));
            if let Some(backend) = self.backend() {
                let buf = entry.as_slice_mut();
                backend.read_page(page_index, buf)?;
            }
            // 页面跨越 EOF：populate 从后端读取文件内的数据，超出部分为零填充
            // 需要 OR 入超出部分的初始 valid_mask
            if init_mask != 0 {
                entry.valid_mask.fetch_or(init_mask, Ordering::Release);
            }
            entry
        } else if beyond_eof {
            // 页面超出旧 EOF：零填充（frame_alloc 已零填充），valid_mask=VALID_ALL。
            // 跳过 backend read — 文件可见区域在此页面内全为零是正确行为。
            Arc::new(PageEntry::new_with_valid_mask(frame, VALID_ALL))
        } else {
            // 整页覆写（populate=false）：跳过 backend read，后续写入覆盖全部内容
            Arc::new(PageEntry::new(frame, PageState::UpToDate))
        };

        let _t_publish = perf::perf_time_now();
        let winner = self.entries.insert_if_absent(page_index, candidate);
        let publish_lock_hold = perf::perf_time_now().wrapping_sub(_t_publish);
        perf::record_pc_lock_hold(publish_lock_hold, false);

        // Clock eviction: mark the published or concurrent winner as recently referenced.
        winner.mark_referenced();
        Ok(winner)
    }

    /// 获取页面用于读取。
    ///
    /// # Semantics
    ///
    /// 始终从后端 populate（`old_file_size=None` → 全量从后端加载）。
    /// 后续由 `ensure_fully_valid` 补齐部分写入的空洞。
    ///
    /// # Locking
    ///
    /// 读取目标页槽；调用者不得持有 inode 内部锁。
    ///
    /// # Errors
    ///
    /// 内存分配失败返回 `ENOMEM`；后端读取失败透传后端错误。
    pub fn get_page_for_read(&self, page_index: usize) -> Result<Arc<PageEntry>, SyscallErr> {
        // 读取路径：始终 populate（old_file_size=None → 全量从后端加载），后续 ensure_fully_valid 补齐空洞
        let entry = self.get_or_create_entry(page_index, true, None)?;
        match entry.state() {
            PageState::UpToDate | PageState::Dirty => Ok(entry),
            PageState::Loading | PageState::Writeback => Err(SyscallErr::EAGAIN),
            PageState::Error => Err(SyscallErr::EIO),
        }
    }

    fn prepare_write_lease(
        &self,
        page_index: usize,
        old_file_size: Option<usize>,
        full_overwrite: bool,
        stages: Option<&mut WriteStageCycles>,
    ) -> Result<WriteLease, SyscallErr> {
        let mut stages = stages;
        let lookup_start = stages.as_ref().map(|_| perf::perf_memory_io_time_now());
        let existing = self.entries.get(page_index);
        if let (Some(stages), Some(start)) = (stages.as_deref_mut(), lookup_start) {
            stages.lookup = stages
                .lookup
                .wrapping_add(perf::perf_memory_io_time_now().wrapping_sub(start));
        }

        let lease_start = stages.as_ref().map(|_| perf::perf_memory_io_time_now());
        let result = (|| {
            if let Some(entry) = existing {
                return self.acquire_existing_write_lease(page_index, entry);
            }

            let init_mask = old_file_size.map_or(0, |size| initial_valid_mask(page_index, size));
            let beyond_eof = init_mask == VALID_ALL;
            let populate = !full_overwrite && !beyond_eof;

            let _t_falloc = perf::perf_time_now();
            let frame = if full_overwrite {
                // Safety: the caller copies exactly PAGE_SIZE bytes into this new Loading entry
                // before commit_write_lease can publish it as readable. On an error,
                // abort_write_lease removes the entry before any reader can observe its contents.
                unsafe { frame_alloc_uninit() }
            } else {
                frame_alloc()
            }
            .ok_or(SyscallErr::ENOMEM)?;
            perf::record_pc_falloc_cycles(perf::perf_time_now().wrapping_sub(_t_falloc));
            let candidate = Arc::new(PageEntry::new(frame, PageState::Loading));
            if populate {
                perf::record_pc_miss();
                if let Some(backend) = self.backend() {
                    backend.read_page(page_index, candidate.as_slice_mut())?;
                }
            }
            if init_mask != 0 {
                candidate.valid_mask.fetch_or(init_mask, Ordering::Release);
            }

            let winner = self.entries.insert_if_absent(page_index, candidate.clone());
            let newly_installed = Arc::ptr_eq(&winner, &candidate);

            if newly_installed {
                winner.mark_referenced();
                return Ok(WriteLease {
                    page_index,
                    entry: winner,
                    was_dirty: false,
                    newly_installed: true,
                });
            }

            self.acquire_existing_write_lease(page_index, winner)
        })();
        if let (Some(stages), Some(start)) = (stages.as_deref_mut(), lease_start) {
            stages.lease = stages
                .lease
                .wrapping_add(perf::perf_memory_io_time_now().wrapping_sub(start));
        }

        result
    }

    fn acquire_existing_write_lease(
        &self,
        page_index: usize,
        entry: Arc<PageEntry>,
    ) -> Result<WriteLease, SyscallErr> {
        let was_dirty = entry.try_lock_for_write()?;
        Ok(WriteLease {
            page_index,
            entry,
            was_dirty,
            newly_installed: false,
        })
    }

    fn commit_write_lease(&self, lease: &WriteLease) {
        if lease.entry.commit_write() && !lease.was_dirty {
            GLOBAL_DIRTY_PAGES.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn abort_write_lease(&self, lease: &WriteLease) {
        if !lease.newly_installed {
            lease.entry.abort_write();
            return;
        }

        self.entries.remove_if(lease.page_index, &lease.entry);
    }

    /// 获取页帧用于文件映射读（如 `MAP_PRIVATE` file-backed page fault）。
    ///
    /// # Semantics
    ///
    /// 返回 PageCache 中的 `Arc<FrameTracker>`，不标记脏。只允许 `UpToDate`
    /// 或 `Dirty` 状态的页帧；其他状态返回对应错误。
    ///
    /// # Errors
    ///
    /// - `EIO`：页面状态为 `Error`
    /// - `EAGAIN`：页面状态为 `Loading` 或 `Writeback`
    ///
    /// # Locking
    ///
    /// 读取目标页槽。
    pub fn frame_for_read(&self, page_index: usize) -> Result<Arc<FrameTracker>, SyscallErr> {
        let entry = self.get_page_for_read(page_index)?;
        // 保证部分写入的页面在映射前所有 segment 均有效
        self.ensure_fully_valid(page_index)?;
        let state = entry.state();
        match state {
            PageState::UpToDate | PageState::Dirty => Ok(entry.page.clone()),
            PageState::Error => Err(SyscallErr::EIO),
            PageState::Loading | PageState::Writeback => Err(SyscallErr::EAGAIN),
        }
    }

    /// 获取页帧用于文件映射写（如 `MAP_SHARED` file-backed page fault）。
    pub fn frame_for_write(&self, page_index: usize) -> Result<Arc<FrameTracker>, SyscallErr> {
        let entry = self.get_or_create_entry(page_index, true, None)?;
        match entry.state() {
            PageState::Loading | PageState::Writeback => return Err(SyscallErr::EAGAIN),
            PageState::Error => return Err(SyscallErr::EIO),
            PageState::UpToDate | PageState::Dirty => {}
        }
        self.ensure_fully_valid(page_index)?;

        match entry.state() {
            PageState::Dirty => Ok(entry.page.clone()),
            PageState::UpToDate => {
                let mut old = entry.flags();
                loop {
                    if PageEntry::decode_state(old) != PageState::UpToDate {
                        return Err(SyscallErr::EAGAIN);
                    }
                    match entry.compare_exchange_flags(old, old | PG_DIRTY) {
                        Ok(_) => {
                            GLOBAL_DIRTY_PAGES.fetch_add(1, Ordering::Relaxed);
                            return Ok(entry.page.clone());
                        }
                        Err(current) => old = current,
                    }
                }
            }
            PageState::Loading | PageState::Writeback => Err(SyscallErr::EAGAIN),
            PageState::Error => Err(SyscallErr::EIO),
        }
    }

    /// 确保页面所有 segment 均已有效（读取缺失的 segment 并合并）。
    /// 在读取或回写前调用，以保证部分写入的页面（超出 EOF 零填充页面）
    /// 在涉及未写入 segment 时不返回错误数据。
    /// 快速路径：is_fully_valid() == true 时直接返回。
    fn ensure_fully_valid(&self, page_index: usize) -> Result<(), SyscallErr> {
        let Some(entry) = self.entries.get(page_index) else {
            return Ok(());
        };

        match entry.state() {
            PageState::Loading => return Err(SyscallErr::EAGAIN),
            PageState::Error => return Err(SyscallErr::EIO),
            PageState::UpToDate | PageState::Dirty | PageState::Writeback => {}
        }

        if entry.is_fully_valid() {
            return Ok(());
        }

        // 读取后端的完整页面数据，仅覆盖无效 segment
        if let Some(backend) = self.backend() {
            let valid_before = entry.valid_mask.load(Ordering::Acquire);
            // 双重检查：可能在等待锁期间已被其他路径填充完整
            if valid_before == VALID_ALL {
                return Ok(());
            }

            let mut temp = alloc::vec![0u8; PAGE_SIZE];
            backend.read_page(page_index, &mut temp)?;

            let dst = entry.as_slice_mut();
            for seg in 0..VALID_SEG_COUNT {
                if (valid_before >> seg) & 1 == 0 {
                    let start = seg << VALID_SEG_SHIFT;
                    let end = start + (1 << VALID_SEG_SHIFT);
                    dst[start..end].copy_from_slice(&temp[start..end]);
                }
            }
        }

        entry.mark_fully_valid();
        Ok(())
    }

    // ── Batch read helpers ───────────────────────────────────────────

    /// Scan [start_page..=end_page] through bounded batches of directory slots.
    /// HIT: mark referenced, check is_fully_valid(), push ReadCopy.
    /// MISS: record in miss_runs (coalesced into contiguous runs).
    /// PARTIAL: if entry exists but !is_fully_valid(), push to needs_valid_fill.
    fn lookup_read_range_fast(
        &self,
        offset: usize,
        buf_len: usize,
        start_page: usize,
        end_page: usize,
    ) -> Result<ReadPlan, SyscallErr> {
        let end = offset.checked_add(buf_len).ok_or(SyscallErr::EFBIG)?;
        let page_count = end_page - start_page + 1;
        let mut plan = ReadPlan {
            copies: Vec::with_capacity(page_count),
            miss_runs: Vec::with_capacity(page_count),
            needs_valid_fill: BTreeSet::new(),
        };
        for page_index in start_page..=end_page {
            let page_start = page_index * PAGE_SIZE;
            let read_start = offset.max(page_start);
            let read_end = end.min(page_start.saturating_add(PAGE_SIZE));
            if read_end <= read_start {
                continue;
            }
            let sub_len = read_end - read_start;

            let dst_offset = read_start - offset;
            let page_offset = read_start - page_start;

            // Check entry existence
            if let Some(entry) = self.entries.get(page_index) {
                match entry.state() {
                    PageState::Loading | PageState::Writeback => {
                        return Err(SyscallErr::EAGAIN);
                    }
                    PageState::Error => return Err(SyscallErr::EIO),
                    PageState::UpToDate | PageState::Dirty => {}
                }
                entry.mark_referenced();
                if entry.is_fully_valid() {
                    plan.copies.push(ReadCopy {
                        entry,
                        dst_offset,
                        page_offset,
                        len: sub_len,
                    });
                    continue;
                }
                plan.needs_valid_fill.insert(page_index);
                continue;
            }

            // Miss: page not in cache — coalesce into contiguous runs
            if let Some(last) = plan.miss_runs.last_mut() {
                if last.start_page + last.count == page_index {
                    last.count += 1;
                } else {
                    plan.miss_runs.push(MissRun {
                        start_page: page_index,
                        count: 1,
                    });
                }
            } else {
                plan.miss_runs.push(MissRun {
                    start_page: page_index,
                    count: 1,
                });
            }
        }
        Ok(plan)
    }

    /// Fill contiguous missing page runs using backend.read_pages().
    /// Uses publish-after-I/O pattern: create UpToDate entries, fill via I/O, then publish.
    fn fill_miss_runs(&self, runs: &[MissRun]) -> Result<(), SyscallErr> {
        const MAX_BATCH_PAGES: usize = 256;
        let backend = self.backend().ok_or(SyscallErr::EIO)?;
        let backend_npages = backend.npages();

        for run in runs {
            // 1. Alloc frames for all pages in this run
            let mut new_entries: Vec<(usize, Arc<PageEntry>)> = Vec::with_capacity(run.count);
            for i in 0..run.count {
                let page_index = run.start_page + i;
                let frame = frame_alloc().ok_or(SyscallErr::ENOMEM)?;
                new_entries.push((
                    page_index,
                    Arc::new(PageEntry::new(frame, PageState::UpToDate)),
                ));
            }

            // 2. Call read_pages() in bounded sub-batches (<= MAX_BATCH_PAGES pages,
            //    1 MiB staging) within backend range, mirroring Linux readahead
            //    batching. A caller-sized staging buffer is unbounded: a single
            //    26 MiB read previously forced one 26 MiB heap allocation.
            let mut i = 0;
            while i < new_entries.len() {
                let start = new_entries[i].0;
                if start >= backend_npages {
                    // Hole past EOF: zero-fill (frame_alloc already zeroed), mark fully valid
                    new_entries[i]
                        .1
                        .valid_mask
                        .store(VALID_ALL, Ordering::Release);
                    i += 1;
                    continue;
                }
                let run_start = start;
                let mut bufs: Vec<&mut [u8]> = Vec::new();
                while i < new_entries.len()
                    && new_entries[i].0 == run_start + bufs.len()
                    && new_entries[i].0 < backend_npages
                    && bufs.len() < MAX_BATCH_PAGES
                {
                    // SAFETY: we own the only mutable ref to this frame (not yet published to entries)
                    unsafe {
                        bufs.push(&mut *(new_entries[i].1.as_slice_mut() as *mut [u8]));
                    }
                    i += 1;
                }
                let n = backend.read_pages(run_start, &mut bufs)?;
                // Pages fully within the read result are fully valid.
                // bufs covers new_entries[first_idx .. first_idx + bufs.len()]
                let first_idx = i - bufs.len();
                let full_pages = (n / PAGE_SIZE).min(bufs.len());
                for j in 0..full_pages {
                    new_entries[first_idx + j]
                        .1
                        .valid_mask
                        .store(VALID_ALL, Ordering::Release);
                }
                // Record miss for perf
                perf::record_pc_miss();
            }

            // 3. Publish: insert into entries (only if slot still empty)
            for (page_index, entry) in new_entries {
                self.entries.insert_if_absent(page_index, entry);
            }
        }
        Ok(())
    }

    // ── 读取 ─────────────────────────────────────────────────────────

    /// 从指定偏移量读取数据
    /// 两阶段读取：持锁收集拷贝项 → 解锁拷贝数据
    pub fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        let _t0 = perf::perf_memory_io_time_now();
        let miss_before = perf::PC_READ_MISS.load(core::sync::atomic::Ordering::Relaxed);
        if buf.is_empty() {
            let elapsed = perf::perf_memory_io_time_now().wrapping_sub(_t0);
            perf::record_pc_read(0, elapsed, elapsed, 0);
            return Ok(0);
        }

        let start_page = offset >> PAGE_SIZE_BITS;
        let end_page = (offset + buf.len() - 1) >> PAGE_SIZE_BITS;

        // Single-page fast path: bypass Vec<CopyItem> construction
        if start_page == end_page {
            let page_start = start_page << PAGE_SIZE_BITS;
            let page_offset = offset - page_start;
            let sub_len = buf.len().min(PAGE_SIZE - page_offset);
            let _t_lookup = perf::perf_memory_io_time_now();
            let entry = self.get_page_for_read(start_page)?;
            self.ensure_fully_valid(start_page)?;
            let had_miss =
                perf::PC_READ_MISS.load(core::sync::atomic::Ordering::Relaxed) > miss_before;
            let lookup_cycles = perf::perf_memory_io_time_now().wrapping_sub(_t_lookup);
            perf::record_pc_lookup_cycles(lookup_cycles);
            let _t_copy = perf::perf_memory_io_time_now();
            let src = entry.as_slice();
            buf[..sub_len].copy_from_slice(&src[page_offset..page_offset + sub_len]);
            let copy_cycles = perf::perf_memory_io_time_now().wrapping_sub(_t_copy);
            perf::record_pc_copy_cycles(copy_cycles);
            let total_cycles = perf::perf_memory_io_time_now().wrapping_sub(_t0);
            if had_miss {
                perf::record_pc_read(1, total_cycles, 0, total_cycles);
            } else {
                perf::record_pc_read(1, total_cycles, total_cycles, 0);
            }
            return Ok(sub_len);
        }

        // Multi-page: batch lookup with ONE entries lock, retry if misses
        let mut retried = false;
        let total_len = buf.len();
        loop {
            let _t_lookup = perf::perf_memory_io_time_now();
            let plan = self.lookup_read_range_fast(offset, total_len, start_page, end_page)?;
            let lookup_cycles = perf::perf_memory_io_time_now().wrapping_sub(_t_lookup);
            perf::record_pc_lookup_cycles(lookup_cycles);

            // Fast path: all pages cached and fully valid
            if plan.miss_runs.is_empty() && plan.needs_valid_fill.is_empty() {
                let _t_copy = perf::perf_memory_io_time_now();
                for item in &plan.copies {
                    let src = item.entry.as_slice();
                    buf[item.dst_offset..item.dst_offset + item.len]
                        .copy_from_slice(&src[item.page_offset..item.page_offset + item.len]);
                }
                let copy_cycles = perf::perf_memory_io_time_now().wrapping_sub(_t_copy);
                perf::record_pc_copy_cycles(copy_cycles);

                let had_miss =
                    perf::PC_READ_MISS.load(core::sync::atomic::Ordering::Relaxed) > miss_before;
                let total_cycles = perf::perf_memory_io_time_now().wrapping_sub(_t0);
                let pages = end_page - start_page + 1;
                if had_miss {
                    perf::record_pc_read(pages, total_cycles, 0, total_cycles);
                } else {
                    perf::record_pc_read(pages, total_cycles, total_cycles, 0);
                }
                return Ok(total_len);
            }

            // If we already retried, fall through to slow per-page path
            if retried {
                struct CopyItem {
                    entry: Arc<PageEntry>,
                    page_offset: usize,
                    sub_len: usize,
                }

                let mut copies: Vec<CopyItem> = Vec::new();
                let mut total_read = 0usize;
                for page_index in start_page..=end_page {
                    let page_start = page_index << PAGE_SIZE_BITS;
                    let page_end = page_start + PAGE_SIZE;
                    let read_start = core::cmp::max(offset, page_start);
                    let read_end = core::cmp::min(offset + buf.len(), page_end);
                    let sub_len = read_end.saturating_sub(read_start);

                    if sub_len == 0 {
                        continue;
                    }

                    let entry = self.get_page_for_read(page_index)?;
                    self.ensure_fully_valid(page_index)?;
                    copies.push(CopyItem {
                        entry,
                        page_offset: read_start - page_start,
                        sub_len,
                    });
                    total_read += sub_len;
                }
                let mut dst_offset = 0;
                for item in &copies {
                    let src = item.entry.as_slice();
                    let src_start = item.page_offset;
                    buf[dst_offset..dst_offset + item.sub_len]
                        .copy_from_slice(&src[src_start..src_start + item.sub_len]);
                    dst_offset += item.sub_len;
                }
                let had_miss =
                    perf::PC_READ_MISS.load(core::sync::atomic::Ordering::Relaxed) > miss_before;
                let total_cycles = perf::perf_memory_io_time_now().wrapping_sub(_t0);
                let pages = end_page - start_page + 1;
                if had_miss {
                    perf::record_pc_read(pages, total_cycles, 0, total_cycles);
                } else {
                    perf::record_pc_read(pages, total_cycles, total_cycles, 0);
                }
                return Ok(total_read);
            }

            // First iteration: fill misses and valid gaps, then retry once
            if !plan.needs_valid_fill.is_empty() {
                for &page_index in &plan.needs_valid_fill {
                    self.ensure_fully_valid(page_index)?;
                }
            }
            if !plan.miss_runs.is_empty() {
                self.fill_miss_runs(&plan.miss_runs)?;
            }
            retried = true;
        }
    }

    // ── 写入 ─────────────────────────────────────────────────────────

    /// 从指定偏移量写入数据
    /// 两阶段写入：持锁收集目标页 → 解锁写入数据
    /// `old_file_size`: 旧文件大小，用于判断页面是否超出 EOF 以跳过不必要的后端读取
    pub fn write(
        &self,
        offset: usize,
        buf: &[u8],
        old_file_size: Option<usize>,
    ) -> Result<usize, SyscallErr> {
        self.write_with_after_copy(offset, buf, old_file_size, |_| {})
    }

    /// 从指定偏移量写入数据，并在所有数据及有效位发布后执行回调。
    ///
    /// `after_copy` 仅在成功写入非空缓冲区后调用一次，且在性能统计和脏页节流之前。
    pub(crate) fn write_with_after_copy<F>(
        &self,
        offset: usize,
        buf: &[u8],
        old_file_size: Option<usize>,
        after_copy: F,
    ) -> Result<usize, SyscallErr>
    where
        F: FnOnce(usize),
    {
        self.write_with_copy_callbacks(offset, buf, old_file_size, |_| Ok(()), after_copy)
    }

    /// Test-only seam which runs after Loading publication but before payload bytes are copied.
    pub(crate) fn write_with_before_copy<F>(
        &self,
        offset: usize,
        buf: &[u8],
        old_file_size: Option<usize>,
        before_copy: F,
    ) -> Result<usize, SyscallErr>
    where
        F: FnOnce(usize) -> Result<(), SyscallErr>,
    {
        self.write_with_copy_callbacks(offset, buf, old_file_size, before_copy, |_| {})
    }

    fn write_with_copy_callbacks<BeforeCopy, AfterCopy>(
        &self,
        offset: usize,
        buf: &[u8],
        old_file_size: Option<usize>,
        before_copy: BeforeCopy,
        after_copy: AfterCopy,
    ) -> Result<usize, SyscallErr>
    where
        BeforeCopy: FnOnce(usize) -> Result<(), SyscallErr>,
        AfterCopy: FnOnce(usize),
    {
        self.write_with_copy_callbacks_locked(offset, buf, old_file_size, before_copy, after_copy)
    }

    /// Copy data after per-page leases have serialized every target page.
    fn write_with_copy_callbacks_locked<BeforeCopy, AfterCopy>(
        &self,
        offset: usize,
        buf: &[u8],
        old_file_size: Option<usize>,
        before_copy: BeforeCopy,
        after_copy: AfterCopy,
    ) -> Result<usize, SyscallErr>
    where
        BeforeCopy: FnOnce(usize) -> Result<(), SyscallErr>,
        AfterCopy: FnOnce(usize),
    {
        let _t0 = perf::perf_time_now();
        if buf.is_empty() {
            perf::record_pc_write(0, false, perf::perf_time_now().wrapping_sub(_t0));
            return Ok(0);
        }

        let start_page = offset >> PAGE_SIZE_BITS;
        let end_page = (offset + buf.len() - 1) >> PAGE_SIZE_BITS;

        // Single-page fast path: bypass Vec<CopyItem> construction
        if start_page == end_page {
            let page_start = start_page << PAGE_SIZE_BITS;
            let page_offset = offset - page_start;
            let sub_len = buf.len().min(PAGE_SIZE - page_offset);
            let full_page_overwrite = page_offset == 0 && sub_len == PAGE_SIZE;
            let lease =
                self.prepare_write_lease(start_page, old_file_size, full_page_overwrite, None)?;
            if let Err(error) = before_copy(sub_len) {
                self.abort_write_lease(&lease);
                return Err(error);
            }
            let dst = lease.entry.as_slice_mut();
            dst[page_offset..page_offset + sub_len].copy_from_slice(&buf[..sub_len]);
            let became_full = lease.entry.mark_valid_and_check_full(page_offset, sub_len);
            self.commit_write_lease(&lease);
            after_copy(sub_len);
            if became_full && !full_page_overwrite {
                perf::record_pc_write_eventually_full();
            }
            perf::record_pc_write(
                1,
                full_page_overwrite,
                perf::perf_time_now().wrapping_sub(_t0),
            );
            balance_dirty_pages_for_write(sub_len);
            return Ok(sub_len);
        }

        struct CopyItem {
            lease: WriteLease,
            page_offset: usize,
            sub_len: usize,
            full_page_overwrite: bool,
        }

        let mut copies: Vec<CopyItem> = Vec::new();
        let mut total_written = 0usize;
        let mut pages = 0usize;
        let mut any_full_overwrite = false;

        for page_index in start_page..=end_page {
            let page_start = page_index << PAGE_SIZE_BITS;
            let page_end = page_start.saturating_add(PAGE_SIZE);
            let write_start = core::cmp::max(offset, page_start);
            let write_end = core::cmp::min(offset + buf.len(), page_end);
            let sub_len = write_end.saturating_sub(write_start);

            if sub_len == 0 {
                continue;
            }

            pages += 1;
            let page_offset = write_start - page_start;
            let full_page_overwrite = page_offset == 0 && sub_len == PAGE_SIZE;
            if full_page_overwrite {
                any_full_overwrite = true;
            }
            let lease = match self.prepare_write_lease(
                page_index,
                old_file_size,
                full_page_overwrite,
                None,
            ) {
                Ok(lease) => lease,
                Err(error) => {
                    for item in &copies {
                        self.abort_write_lease(&item.lease);
                    }
                    return Err(error);
                }
            };
            copies.push(CopyItem {
                lease,
                page_offset,
                sub_len,
                full_page_overwrite,
            });
            total_written += sub_len;
        }

        // Phase 2: 写入数据（无锁）
        if let Err(error) = before_copy(total_written) {
            for item in &copies {
                self.abort_write_lease(&item.lease);
            }
            return Err(error);
        }
        let mut src_offset = 0;
        let mut eventually_full = 0usize;
        for item in &copies {
            let dst = item.lease.entry.as_slice_mut();
            let dst_start = item.page_offset;
            dst[dst_start..dst_start + item.sub_len]
                .copy_from_slice(&buf[src_offset..src_offset + item.sub_len]);
            let became_full = item
                .lease
                .entry
                .mark_valid_and_check_full(item.page_offset, item.sub_len);
            if became_full && !item.full_page_overwrite {
                eventually_full += 1;
            }
            src_offset += item.sub_len;
        }

        for item in &copies {
            self.commit_write_lease(&item.lease);
        }
        after_copy(total_written);
        for _ in 0..eventually_full {
            perf::record_pc_write_eventually_full();
        }
        perf::record_pc_write(
            pages,
            any_full_overwrite,
            perf::perf_time_now().wrapping_sub(_t0),
        );
        if !self.suppress_balance.load(Ordering::Relaxed) {
            balance_dirty_pages_for_write(total_written);
        }
        Ok(total_written)
    }

    /// Write data without triggering global dirty-page balancing.
    pub fn write_without_balance(
        &self,
        offset: usize,
        buf: &[u8],
        old_file_size: Option<usize>,
    ) -> Result<usize, SyscallErr> {
        self.suppress_balance.store(true, Ordering::Relaxed);
        let result = self.write_with_copy_callbacks(offset, buf, old_file_size, |_| Ok(()), |_| {});
        self.suppress_balance.store(false, Ordering::Relaxed);
        result
    }

    /// Roll back a failed extension: discard dirty pages past `old_size`.
    pub fn rollback_failed_extension(&self, old_size: usize) {
        let start_page = (old_size + crate::config::PAGE_SIZE - 1) >> crate::config::PAGE_SIZE_BITS;
        for (page_index, entry) in self.entries.snapshot() {
            if page_index >= start_page && entry.state() == PageState::Dirty {
                if entry.clear_dirty() {
                    GLOBAL_DIRTY_PAGES.fetch_sub(1, Ordering::Relaxed);
                }
                self.entries.remove_if(page_index, &entry);
            }
        }
    }

    /// Truncate with a backend callback.
    pub fn truncate_with_backend<F>(&self, new_size: usize, backend: F) -> Result<(), SyscallErr>
    where
        F: FnOnce() -> Result<(), SyscallErr>,
    {
        let hole_start_page =
            (new_size + crate::config::PAGE_SIZE - 1) >> crate::config::PAGE_SIZE_BITS;
        for (page_index, entry) in self.entries.snapshot() {
            if page_index >= hole_start_page {
                if entry.clear_dirty() {
                    GLOBAL_DIRTY_PAGES.fetch_sub(1, Ordering::Relaxed);
                }
                self.entries.remove_if(page_index, &entry);
            }
        }
        backend()?;
        Ok(())
    }

    // ── UserBuffer 读写 ──────────────────────────────────────────────

    /// 从指定偏移量读取数据到 UserBuffer。
    /// 两阶段读取：持锁收集拷贝项 → 解锁拷贝到 UserBuffer。
    /// `len` 由调用者按文件大小等限制，不从此 buffer 的长度推断。
    pub fn read_user(
        &self,
        offset: usize,
        len: usize,
        dst: &mut crate::mm::UserBuffer,
    ) -> Result<usize, SyscallErr> {
        if len > dst.len() {
            return Err(SyscallErr::EFAULT);
        }
        if len == 0 {
            return Ok(0);
        }

        let end = offset.checked_add(len).ok_or(SyscallErr::EFBIG)?;
        let start_page = offset >> PAGE_SIZE_BITS;
        let end_page = (end - 1) >> PAGE_SIZE_BITS;

        // Single-page fast path: bypass Vec<CopyItem> construction
        if start_page == end_page {
            let page_start = start_page << PAGE_SIZE_BITS;
            let page_offset = offset - page_start;
            let sub_len = len.min(PAGE_SIZE - page_offset);
            let _t_lookup = perf::perf_memory_io_time_now();
            let entry = self.get_page_for_read(start_page)?;
            let lookup_cycles = perf::perf_memory_io_time_now().wrapping_sub(_t_lookup);
            perf::record_pc_read_lookup_cycles(lookup_cycles);
            let _t_valid_fill = perf::perf_memory_io_time_now();
            self.ensure_fully_valid(start_page)?;
            let valid_fill_cycles = perf::perf_memory_io_time_now().wrapping_sub(_t_valid_fill);
            perf::record_pc_read_valid_fill_cycles(valid_fill_cycles);
            let src = entry.as_slice();
            let _t_copy = perf::perf_memory_io_time_now();
            let copied = dst.write_at(0, &src[page_offset..page_offset + sub_len]);
            let copy_cycles = perf::perf_memory_io_time_now().wrapping_sub(_t_copy);
            perf::record_pc_read_copy_cycles(copy_cycles);
            if copied != sub_len {
                return Err(SyscallErr::EFAULT);
            }
            perf::record_pc_read_user(1);
            return Ok(sub_len);
        }

        // Multi-page with batch lookup + retry
        let mut retried = false;
        loop {
            let _t_lookup = perf::perf_memory_io_time_now();
            let plan = self.lookup_read_range_fast(offset, len, start_page, end_page)?;
            let lookup_cycles = perf::perf_memory_io_time_now().wrapping_sub(_t_lookup);
            perf::record_pc_read_lookup_cycles(lookup_cycles);

            if plan.miss_runs.is_empty() && plan.needs_valid_fill.is_empty() {
                // All hits: copy to UserBuffer
                let mut cursor = dst.write_cursor();
                for item in &plan.copies {
                    let src = item.entry.as_slice();
                    let _t_copy = perf::perf_memory_io_time_now();
                    let copied =
                        cursor.write_from(&src[item.page_offset..item.page_offset + item.len]);
                    let copy_cycles = perf::perf_memory_io_time_now().wrapping_sub(_t_copy);
                    perf::record_pc_read_copy_cycles(copy_cycles);
                    if copied != item.len {
                        return Err(SyscallErr::EFAULT);
                    }
                }
                perf::record_pc_read_user(plan.copies.len());
                return Ok(plan.copies.iter().map(|c| c.len).sum());
            }

            if retried {
                // Fallback per-page
                struct CopyItem {
                    entry: Arc<PageEntry>,
                    page_offset: usize,
                    sub_len: usize,
                }

                let mut copies: Vec<CopyItem> = Vec::new();
                for page_index in start_page..=end_page {
                    let page_start = page_index << PAGE_SIZE_BITS;
                    let page_end = page_start.saturating_add(PAGE_SIZE);
                    let read_start = core::cmp::max(offset, page_start);
                    let read_end = core::cmp::min(end, page_end);
                    let sub_len = read_end.saturating_sub(read_start);

                    if sub_len == 0 {
                        continue;
                    }

                    let _t_lookup = perf::perf_memory_io_time_now();
                    let entry = self.get_page_for_read(page_index)?;
                    let lookup_cycles = perf::perf_memory_io_time_now().wrapping_sub(_t_lookup);
                    perf::record_pc_read_lookup_cycles(lookup_cycles);
                    let _t_valid_fill = perf::perf_memory_io_time_now();
                    self.ensure_fully_valid(page_index)?;
                    let valid_fill_cycles =
                        perf::perf_memory_io_time_now().wrapping_sub(_t_valid_fill);
                    perf::record_pc_read_valid_fill_cycles(valid_fill_cycles);
                    copies.push(CopyItem {
                        entry,
                        page_offset: read_start - page_start,
                        sub_len,
                    });
                }
                let mut cursor = dst.write_cursor();
                for item in &copies {
                    let src = item.entry.as_slice();
                    let _t_copy = perf::perf_memory_io_time_now();
                    let copied =
                        cursor.write_from(&src[item.page_offset..item.page_offset + item.sub_len]);
                    let copy_cycles = perf::perf_memory_io_time_now().wrapping_sub(_t_copy);
                    perf::record_pc_read_copy_cycles(copy_cycles);
                    if copied != item.sub_len {
                        return Err(SyscallErr::EFAULT);
                    }
                }
                perf::record_pc_read_user(copies.len());
                return Ok(copies.iter().map(|c| c.sub_len).sum());
            }

            if !plan.needs_valid_fill.is_empty() {
                for &page_index in &plan.needs_valid_fill {
                    let _t_valid_fill = perf::perf_memory_io_time_now();
                    self.ensure_fully_valid(page_index)?;
                    let valid_fill_cycles =
                        perf::perf_memory_io_time_now().wrapping_sub(_t_valid_fill);
                    perf::record_pc_read_valid_fill_cycles(valid_fill_cycles);
                }
            }
            if !plan.miss_runs.is_empty() {
                let _t_miss_fill = perf::perf_memory_io_time_now();
                self.fill_miss_runs(&plan.miss_runs)?;
                let miss_fill_cycles = perf::perf_memory_io_time_now().wrapping_sub(_t_miss_fill);
                perf::record_pc_read_miss_fill_cycles(miss_fill_cycles);
            }
            retried = true;
        }
    }

    /// 从 UserBuffer 写入数据到指定偏移量。
    /// 两阶段写入：持锁收集目标页 → 解锁从 UserBuffer 拷贝。
    /// `len` 由调用者计算，不从此 buffer 的长度推断。
    /// `old_file_size`: 旧文件大小，用于判断页面是否超出 EOF 以跳过不必要的后端读取
    pub fn write_user(
        &self,
        offset: usize,
        len: usize,
        src: &crate::mm::UserBuffer,
        old_file_size: Option<usize>,
    ) -> Result<usize, SyscallErr> {
        if len > src.len() {
            return Err(SyscallErr::EFAULT);
        }
        if len == 0 {
            return Ok(0);
        }

        let write_start = perf::perf_memory_io_time_now();
        let mut stages = WriteStageCycles::default();
        let start_page = offset >> PAGE_SIZE_BITS;
        let end_page = (offset + len - 1) >> PAGE_SIZE_BITS;

        // Single-page fast path: bypass Vec<CopyItem> construction
        if start_page == end_page {
            let page_start = start_page << PAGE_SIZE_BITS;
            let page_offset = offset - page_start;
            let sub_len = len.min(PAGE_SIZE - page_offset);
            let full_page_overwrite = page_offset == 0 && sub_len == PAGE_SIZE;
            let lease = self.prepare_write_lease(
                start_page,
                old_file_size,
                full_page_overwrite,
                Some(&mut stages),
            )?;
            let copy_start = perf::perf_memory_io_time_now();
            let dst = lease.entry.as_slice_mut();
            src.read_at(0, &mut dst[page_offset..page_offset + sub_len]);
            let became_full = lease.entry.mark_valid_and_check_full(page_offset, sub_len);
            stages.copy = stages
                .copy
                .wrapping_add(perf::perf_memory_io_time_now().wrapping_sub(copy_start));
            let commit_start = perf::perf_memory_io_time_now();
            self.commit_write_lease(&lease);
            stages.commit = stages
                .commit
                .wrapping_add(perf::perf_memory_io_time_now().wrapping_sub(commit_start));
            if became_full && !full_page_overwrite {
                perf::record_pc_write_eventually_full();
            }
            perf::record_pc_write(
                1,
                full_page_overwrite,
                perf::perf_memory_io_time_now().wrapping_sub(write_start),
            );
            perf::record_pc_write_stages(stages.lookup, stages.lease, stages.copy, stages.commit);
            balance_dirty_pages_for_write(sub_len);
            return Ok(sub_len);
        }

        struct CopyItem {
            lease: WriteLease,
            page_offset: usize,
            sub_len: usize,
            full_page_overwrite: bool,
        }

        let mut copies: Vec<CopyItem> = Vec::new();
        let mut total_written = 0usize;

        for page_index in start_page..=end_page {
            let page_start = page_index << PAGE_SIZE_BITS;
            let page_end = page_start + PAGE_SIZE;
            let write_start = core::cmp::max(offset, page_start);
            let write_end = core::cmp::min(offset + len, page_end);
            let sub_len = write_end.saturating_sub(write_start);

            if sub_len == 0 {
                continue;
            }

            let page_offset = write_start - page_start;
            let full_page_overwrite = page_offset == 0 && sub_len == PAGE_SIZE;
            let lease = match self.prepare_write_lease(
                page_index,
                old_file_size,
                full_page_overwrite,
                Some(&mut stages),
            ) {
                Ok(lease) => lease,
                Err(error) => {
                    for item in &copies {
                        self.abort_write_lease(&item.lease);
                    }
                    return Err(error);
                }
            };
            copies.push(CopyItem {
                lease,
                page_offset: write_start - page_start,
                sub_len,
                full_page_overwrite,
            });
            total_written += sub_len;
        }

        // Phase 2: copy data from UserBuffer into pages (no locks held)
        let copy_start = perf::perf_memory_io_time_now();
        let mut src_offset = 0;
        for item in &copies {
            let dst = item.lease.entry.as_slice_mut();
            let dst_start = item.page_offset;
            src.read_at(src_offset, &mut dst[dst_start..dst_start + item.sub_len]);
            let became_full = item
                .lease
                .entry
                .mark_valid_and_check_full(item.page_offset, item.sub_len);
            if became_full && !item.full_page_overwrite {
                perf::record_pc_write_eventually_full();
            }
            src_offset += item.sub_len;
        }
        stages.copy = stages
            .copy
            .wrapping_add(perf::perf_memory_io_time_now().wrapping_sub(copy_start));

        let commit_start = perf::perf_memory_io_time_now();
        for item in &copies {
            self.commit_write_lease(&item.lease);
        }
        stages.commit = stages
            .commit
            .wrapping_add(perf::perf_memory_io_time_now().wrapping_sub(commit_start));
        perf::record_pc_write(
            copies.len(),
            copies.iter().all(|item| item.full_page_overwrite),
            perf::perf_memory_io_time_now().wrapping_sub(write_start),
        );
        perf::record_pc_write_stages(stages.lookup, stages.lease, stages.copy, stages.commit);
        balance_dirty_pages_for_write(total_written);
        Ok(total_written)
    }

    // ── 顺序读预取 (readahead) ─────────────────────────────────────────

    /// 同步批量预取页面：分配条目并通过 backend.read_pages() 批量从后端读取。
    ///
    /// 对 [start_page .. start_page+count) 中尚未缓存的页面：
    /// 1. 分配帧并创建 Loading 状态的 PageEntry
    /// 2. 通过 backend.read_pages() 批量读取
    /// 3. 标记为 UpToDate 并插入 entries
    ///
    /// 超出后端 npages() 的页面会被零填充（sparse file hole）。
    pub fn sync_batch_read_pages(
        &self,
        start_page: usize,
        count: usize,
    ) -> Result<usize, SyscallErr> {
        if count == 0 {
            return Ok(0);
        }

        let backend = match self.backend() {
            Some(b) => b,
            None => return Ok(0),
        };
        let backend_npages = backend.npages();

        // Phase 1: 收集需要新建的页面，已缓存的跳过
        struct PendingPage {
            index: usize, // absolute page index
            entry: Arc<PageEntry>,
        }
        let missing_indices: Vec<usize> = (start_page..start_page + count)
            .filter(|page_index| self.entries.get(*page_index).is_none())
            .collect();
        let mut pending: Vec<PendingPage> = Vec::new();
        for page_index in missing_indices {
            let frame = frame_alloc().ok_or(SyscallErr::ENOMEM)?;
            let entry = Arc::new(PageEntry::new(frame, PageState::Loading));
            pending.push(PendingPage {
                index: page_index,
                entry,
            });
        }

        if pending.is_empty() {
            return Ok(0);
        }

        // Phase 2: 将 pending 按索引连续性拆成多个 run，每个 run 调用一次 read_pages
        // 跳过已缓存的页会制造空洞，不能假设 pending 索引连续
        let mut i = 0;
        while i < pending.len() {
            // 跳过超出 backend 范围的页
            if pending[i].index >= backend_npages {
                i += 1;
                continue;
            }
            // 收集一个连续 run，按 MAX_BATCH_PAGES 分片（超出即开启下一子批），
            // 与 fill_miss_runs 一致，避免越过后端 read_pages 的批次上限。
            let run_start = pending[i].index;
            let mut run_bufs: Vec<&mut [u8]> = Vec::new();
            while i < pending.len()
                && pending[i].index < backend_npages
                && pending[i].index == run_start + run_bufs.len()
                && run_bufs.len() < MAX_BATCH_PAGES
            {
                // SAFETY: 我们拥有这些帧的唯一可变引用
                unsafe {
                    run_bufs.push(&mut *(pending[i].entry.as_slice_mut() as *mut [u8]));
                }
                i += 1;
            }
            if !run_bufs.is_empty() {
                backend.read_pages(run_start, &mut run_bufs)?;
            }
        }

        // Phase 3: 零填充超出 backend npages 的页面（sparse file holes）
        for p in &pending {
            if p.index >= backend_npages {
                p.entry.as_slice_mut().fill(0);
            }
        }

        // Phase 4: only publish pages whose target slot is still empty.
        for p in &pending {
            p.entry.set_state(PageState::UpToDate);
            self.entries.insert_if_absent(p.index, p.entry.clone());
        }

        Ok(count * PAGE_SIZE)
    }

    /// 检查顺序访问模式并预取后续页面。
    ///
    /// 对标 Linux `mm/readahead.c::page_cache_sync_ra()` 的 on-demand 预取：
    /// - 检测顺序访问（page_index == prev_page+1 或 page_index == prev_page）
    /// - 顺序访问时指数增加窗口：ra_size = min(ra_size * 2, MAX_RA_PAGES)
    /// - 非顺序访问时重置窗口：ra_size = MIN_RA_PAGES
    /// - 更新 prev_page 记录
    /// - 批量预取 ahead pages
    pub fn maybe_readahead(&self, page_index: usize, ra: &mut RaState, req_pages: usize) {
        let sequential = page_index == ra.prev_page + 1 || page_index == ra.prev_page;

        if !sequential {
            ra.ra_size = MIN_RA_PAGES;
        } else {
            ra.ra_size = (ra.ra_size * 2).min(MAX_RA_PAGES);
        }
        ra.prev_page = page_index + req_pages.saturating_sub(1);

        // 预取 ahead pages
        let ahead_start = page_index + req_pages;
        let backend_npages = self.backend().map(|b| b.npages()).unwrap_or(0);
        let ahead_end = (ahead_start + ra.ra_size).min(backend_npages);

        if ahead_end > ahead_start {
            let _ = self.sync_batch_read_pages(ahead_start, ahead_end - ahead_start);
        }
    }

    // ── 脏页管理 ────────────────────────────────────────────────────

    /// 标记指定页面为脏页，并原子递增全局脏页计数。
    pub fn mark_page_dirty(&self, page_index: usize) {
        let entry = self.entries.get(page_index);
        let Some(entry) = entry else {
            return;
        };

        let mut old = entry.flags();
        loop {
            match PageEntry::decode_state(old) {
                PageState::UpToDate => match entry.compare_exchange_flags(old, old | PG_DIRTY) {
                    Ok(_) => {
                        GLOBAL_DIRTY_PAGES.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                    Err(current) => old = current,
                },
                PageState::Dirty => return,
                PageState::Writeback => {
                    entry.set_flag(PG_REDIRTIED);
                    return;
                }
                PageState::Loading | PageState::Error => return,
            }
        }
    }

    /// 清除指定页面的脏页标记（写回完成后调用）。
    pub fn mark_page_writeback(&self, page_index: usize) {
        if let Some(entry) = self.entries.get(page_index) {
            entry.clear_dirty();
        }
    }

    // ── 回写 ─────────────────────────────────────────────────────────

    /// 单次回写批次的最大页面数
    const MAX_WRITEBACK_PAGES: usize = 256;

    /// 将单个脏页通过 `backend` 写回存储介质；若页面已为 `UpToDate` 则跳过。
    pub fn writeback_page(&self, page_index: usize) -> Result<(), SyscallErr> {
        self.writeback_page_impl(page_index)
    }

    /// Write back one page after its Dirty → Writeback CAS claims it.
    fn writeback_page_impl(&self, page_index: usize) -> Result<(), SyscallErr> {
        let _t0 = perf::perf_time_now();
        let Some(entry) = self.entries.get(page_index) else {
            perf::record_pc_writeback(0, perf::perf_time_now().wrapping_sub(_t0));
            return Ok(());
        };

        // Atomically claim a cleanable dirty page. The page remains locked until
        // the backend result publishes either UpToDate or a redirtied retry.
        if entry.claim_writeback() {
            GLOBAL_DIRTY_PAGES.fetch_sub(1, Ordering::Relaxed);
            GLOBAL_WRITEBACK_PAGES.fetch_add(1, Ordering::Relaxed);
        } else {
            perf::record_pc_writeback(0, perf::perf_time_now().wrapping_sub(_t0));
            return Ok(());
        }

        let result = if let Some(backend) = self.backend() {
            // 写回前确保所有 segment 有效（填充部分写入的页面空洞）
            let write_result = match self.ensure_fully_valid(page_index) {
                Ok(()) => self.writeback_backend_page_with_retry(
                    backend.as_ref(),
                    page_index,
                    entry.as_slice(),
                ),
                Err(error) => Err(error),
            };
            match write_result {
                Ok(_) => {
                    // Writeback succeeded: check PG_REDIRTIED
                    if entry.complete_writeback() {
                        // Redirtied during writeback → restore to Dirty
                        crate::task::perf::record_wb_redirty();
                        GLOBAL_DIRTY_PAGES.fetch_add(1, Ordering::Relaxed);
                    }
                    GLOBAL_WRITEBACK_PAGES.fetch_sub(1, Ordering::Relaxed);
                    Ok(())
                }
                Err(e) => {
                    // Writeback failed: restore to Dirty for retry
                    entry.restore_dirty_after_writeback();
                    GLOBAL_DIRTY_PAGES.fetch_add(1, Ordering::Relaxed);
                    GLOBAL_WRITEBACK_PAGES.fetch_sub(1, Ordering::Relaxed);
                    Err(e)
                }
            }
        } else {
            if entry.complete_writeback() {
                GLOBAL_DIRTY_PAGES.fetch_add(1, Ordering::Relaxed);
            }
            GLOBAL_WRITEBACK_PAGES.fetch_sub(1, Ordering::Relaxed);
            Ok(())
        };

        perf::record_pc_writeback(1, perf::perf_time_now().wrapping_sub(_t0));
        result
    }

    fn writeback_backend_page_with_retry(
        &self,
        backend: &dyn PageCacheBackend,
        page_index: usize,
        page: &[u8],
    ) -> Result<usize, SyscallErr> {
        for attempt in 0..WRITEBACK_RETRY_LIMIT {
            match backend.write_page(page_index, page) {
                Err(SyscallErr::EAGAIN) if attempt + 1 < WRITEBACK_RETRY_LIMIT => {
                    if crate::task::current_task_ref().is_some() {
                        crate::task::suspend_current_and_run_next();
                    } else {
                        core::hint::spin_loop();
                    }
                }
                result => return result,
            }
        }
        Err(SyscallErr::EAGAIN)
    }

    fn writeback_backend_pages_with_retry(
        &self,
        backend: &dyn PageCacheBackend,
        start_index: usize,
        pages: &[&[u8]],
    ) -> Result<usize, SyscallErr> {
        for attempt in 0..WRITEBACK_RETRY_LIMIT {
            match backend.write_pages(start_index, pages) {
                Err(SyscallErr::EAGAIN) if attempt + 1 < WRITEBACK_RETRY_LIMIT => {
                    if crate::task::current_task_ref().is_some() {
                        crate::task::suspend_current_and_run_next();
                    } else {
                        core::hint::spin_loop();
                    }
                }
                result => return result,
            }
        }
        Err(SyscallErr::EAGAIN)
    }

    /// 批量写回一段连续的脏页
    ///
    /// `start..start+count` 范围内只对实际标记为 Dirty 的页面执行写回；
    /// 非 Dirty 的页面被跳过。批次中至少一个页面被写入时，调用
    /// `backend.write_pages()` 批量提交；否则直接返回 Ok。
    fn writeback_pages_run(&self, start: usize, count: usize) -> Result<(), SyscallErr> {
        let _t0 = perf::perf_time_now();

        // 第一阶段：从各页槽收集 Dirty 页面，CAS 为 Writeback
        let mut page_slices: Vec<(usize, Arc<PageEntry>)> = Vec::new();
        let end = start.saturating_add(count);
        for i in start..end {
            if let Some(entry) = self.entries.get(i) {
                if entry.claim_writeback() {
                    GLOBAL_DIRTY_PAGES.fetch_sub(1, Ordering::Relaxed);
                    GLOBAL_WRITEBACK_PAGES.fetch_add(1, Ordering::Relaxed);
                    page_slices.push((i, entry));
                }
            }
        }

        if page_slices.is_empty() {
            return Ok(());
        }

        // 写回前确保所有 segment 有效（填充部分写入的页面空洞）
        // If ensure_fully_valid fails, restore pages to Dirty so they can be retried.
        if let Err(e) = (|| {
            for (idx, _) in &page_slices {
                self.ensure_fully_valid(*idx)?;
            }
            Ok(())
        })() {
            for (_, entry) in &page_slices {
                entry.restore_dirty_after_writeback();
                GLOBAL_DIRTY_PAGES.fetch_add(1, Ordering::Relaxed);
                GLOBAL_WRITEBACK_PAGES.fetch_sub(1, Ordering::Relaxed);
            }
            return Err(e);
        }

        // 构建连续的 &[u8] 切片（start 即为第一个 Dirty 页的实际索引）
        // Invariant: 调用者保证该范围内不存在非 Dirty 页空洞，否则需拆分 run。
        let actual_start = page_slices[0].0;
        let slices: Vec<&[u8]> = page_slices.iter().map(|(_, e)| e.as_slice()).collect();

        let result = if let Some(backend) = self.backend() {
            let write_result =
                self.writeback_backend_pages_with_retry(backend.as_ref(), actual_start, &slices);
            match write_result {
                Ok(_) => {
                    for (_, entry) in &page_slices {
                        if entry.complete_writeback() {
                            // Redirtied during writeback → restore to Dirty
                            GLOBAL_DIRTY_PAGES.fetch_add(1, Ordering::Relaxed);
                        }
                        GLOBAL_WRITEBACK_PAGES.fetch_sub(1, Ordering::Relaxed);
                    }
                    Ok(())
                }
                Err(e) => {
                    // 写回失败：恢复为 Dirty 状态以便重试
                    for (_, entry) in &page_slices {
                        entry.restore_dirty_after_writeback();
                        GLOBAL_DIRTY_PAGES.fetch_add(1, Ordering::Relaxed);
                        GLOBAL_WRITEBACK_PAGES.fetch_sub(1, Ordering::Relaxed);
                    }
                    Err(e)
                }
            }
        } else {
            for (_, entry) in &page_slices {
                if entry.complete_writeback() {
                    GLOBAL_DIRTY_PAGES.fetch_add(1, Ordering::Relaxed);
                }
                GLOBAL_WRITEBACK_PAGES.fetch_sub(1, Ordering::Relaxed);
            }
            Ok(())
        };

        let writeback_cycles = perf::perf_time_now().wrapping_sub(_t0);
        perf::record_pc_writeback(slices.len(), writeback_cycles);
        if result.is_ok() {
            perf::record_writeback_batch(slices.len());
            #[cfg(feature = "perf_diag")]
            crate::println!(
                "[wb_txn] writeback_run start_page={} pages={} ticks={}",
                actual_start,
                slices.len(),
                writeback_cycles,
            );
        }
        result
    }

    /// 收集当前所有脏页索引并按连续 run 分组，逐 run 写回。
    pub fn writeback_all(&self) -> Result<(), SyscallErr> {
        for attempt in 0..WRITEBACK_RETRY_LIMIT {
            let dirty_indices = self.find_dirty_pages();
            if dirty_indices.is_empty() {
                if !self.has_transient_pages(0, usize::MAX) {
                    return Ok(());
                }
                if crate::task::current_task_ref().is_some() {
                    crate::task::suspend_current_and_run_next();
                } else {
                    core::hint::spin_loop();
                }
                continue;
            }

            // 将连续的脏页分组为 run，按批次调用 writeback_pages_run
            let mut i = 0;
            while i < dirty_indices.len() {
                let run_start = dirty_indices[i];
                let mut run_end = run_start;
                let mut count = 1;
                i += 1;
                while i < dirty_indices.len()
                    && count < Self::MAX_WRITEBACK_PAGES
                    && dirty_indices[i] == run_end + 1
                {
                    run_end = dirty_indices[i];
                    count += 1;
                    i += 1;
                }
                self.writeback_pages_run(run_start, run_end - run_start + 1)?;
            }

            if attempt + 1 < WRITEBACK_RETRY_LIMIT {
                if crate::task::current_task_ref().is_some() {
                    crate::task::suspend_current_and_run_next();
                } else {
                    core::hint::spin_loop();
                }
            }
        }
        Err(SyscallErr::EAGAIN)
    }

    /// 筛选出 `[start_index, end_index]` 范围内的脏页，按连续 run 分组写回。
    pub fn writeback_range(&self, start_index: usize, end_index: usize) -> Result<(), SyscallErr> {
        for attempt in 0..WRITEBACK_RETRY_LIMIT {
            let dirty_indices: Vec<usize> = self
                .find_dirty_pages()
                .into_iter()
                .filter(|index| *index >= start_index && *index <= end_index)
                .collect();
            if dirty_indices.is_empty() {
                if !self.has_transient_pages(start_index, end_index) {
                    return Ok(());
                }
                if crate::task::current_task_ref().is_some() {
                    crate::task::suspend_current_and_run_next();
                } else {
                    core::hint::spin_loop();
                }
                continue;
            }

            // 将连续的脏页分组为 run，按批次调用 writeback_pages_run
            let mut i = 0;
            while i < dirty_indices.len() {
                let run_start = dirty_indices[i];
                let mut run_end = run_start;
                let mut count = 1;
                i += 1;
                while i < dirty_indices.len()
                    && count < Self::MAX_WRITEBACK_PAGES
                    && dirty_indices[i] == run_end + 1
                {
                    run_end = dirty_indices[i];
                    count += 1;
                    i += 1;
                }
                self.writeback_pages_run(run_start, run_end - run_start + 1)?;
            }

            if attempt + 1 < WRITEBACK_RETRY_LIMIT {
                if crate::task::current_task_ref().is_some() {
                    crate::task::suspend_current_and_run_next();
                } else {
                    core::hint::spin_loop();
                }
            }
        }
        Err(SyscallErr::EAGAIN)
    }

    /// 批量写回脏页，最多写回 `budget` 页。返回实际写回的页数。
    ///
    /// 用于后台合作式写回：收集连续脏页 run，持锁收集 → 解锁 → I/O。
    /// 达到预算或脏页耗尽时停止。
    pub fn writeback_some_pages(&self, budget: usize) -> Result<usize, SyscallErr> {
        if budget == 0 {
            return Ok(0);
        }
        let dirty_indices = self.find_dirty_pages();
        if dirty_indices.is_empty() {
            return Ok(0);
        }

        let mut total = 0;
        let mut i = 0;
        while i < dirty_indices.len() && total < budget {
            let run_start = dirty_indices[i];
            let mut run_end = run_start;
            let mut count = 1;
            i += 1;
            while i < dirty_indices.len()
                && total + count < budget
                && count < Self::MAX_WRITEBACK_PAGES
                && dirty_indices[i] == run_end + 1
            {
                run_end = dirty_indices[i];
                count += 1;
                i += 1;
            }
            // writeback_pages_run uses CAS — some pages may have been
            // concurrently consumed by another flusher. Tolerate
            // partial progress and continue.
            self.writeback_pages_run(run_start, run_end - run_start + 1)?;
            total += count;
        }
        Ok(total)
    }

    // ── 截断与失效 ──────────────────────────────────────────────────

    /// 截断 page cache 到指定大小
    pub fn truncate(&self, new_size: usize) -> Result<(), SyscallErr> {
        self.truncate_locked(new_size, true)
    }

    fn truncate_locked(
        &self,
        new_size: usize,
        mark_zeroed_tail_dirty: bool,
    ) -> Result<(), SyscallErr> {
        let hole_start_page = (new_size + PAGE_SIZE - 1) >> PAGE_SIZE_BITS;

        for (page_index, entry) in self.entries.snapshot() {
            if page_index >= hole_start_page {
                if entry.clear_dirty() {
                    GLOBAL_DIRTY_PAGES.fetch_sub(1, Ordering::Relaxed);
                }
                self.entries.remove_if(page_index, &entry);
            }
        }

        let tail_start = new_size % PAGE_SIZE;
        let tail_entry = if new_size > 0 && tail_start > 0 {
            let last_page_idx = (new_size - 1) >> PAGE_SIZE_BITS;
            self.entries
                .get(last_page_idx)
                .map(|entry| (last_page_idx, tail_start, entry))
        } else {
            None
        };

        if let Some((_, tail_start, entry)) = tail_entry {
            let was_dirty = entry.try_lock_for_write()?;
            entry.as_slice_mut()[tail_start..].fill(0);
            if mark_zeroed_tail_dirty {
                if entry.commit_write() && !was_dirty {
                    GLOBAL_DIRTY_PAGES.fetch_add(1, Ordering::Relaxed);
                }
            } else {
                entry.abort_write();
            }
        }

        Ok(())
    }

    /// 收集所有页面的 FrameTracker，用于内核空间映射
    pub fn frame_trackers(&self) -> Vec<crate::mm::Frame> {
        use crate::mm::Frame;
        self.entries
            .snapshot()
            .into_iter()
            .map(|(_, entry)| Frame::InMemory(entry.page.clone()))
            .collect()
    }

    /// 失效指定范围内的页面
    /// 如果范围内存在脏页，返回错误而不静默丢弃数据
    pub fn invalidate_range(
        &self,
        start_index: usize,
        end_index: usize,
    ) -> Result<usize, SyscallErr> {
        let mut invalidated = 0;

        let entries: Vec<(usize, Arc<PageEntry>)> = self
            .entries
            .snapshot()
            .into_iter()
            .filter(|(index, _)| *index >= start_index && *index < end_index)
            .collect();
        if entries.iter().any(|(_, entry)| entry.is_dirty()) {
            return Err(SyscallErr::EBUSY);
        }
        for (page_index, entry) in entries {
            if self.entries.remove_if(page_index, &entry).is_some() {
                invalidated += 1;
            }
        }

        Ok(invalidated)
    }
}

// ── Cooperative Writeback & Dirty Throttling ──────────────────────────────
///
/// 合作式后台写回：驱动周期性写回（每 reclaim 周期调用一次）。
/// 无新内核线程 — 利用现有 reclaim hook 调度。
/// Cooperative background writeback, called every reclaim tick.
///
/// Triggers when dirty pages reach the background watermark AND occupy
/// at least the configured fraction of free physical frames.
pub fn maybe_background_writeback() {
    let dirty = GLOBAL_DIRTY_PAGES.load(Ordering::Relaxed);
    if dirty < DIRTY_BACKGROUND_PAGES {
        return;
    }
    let free = crate::mm::unallocated_frames();
    if dirty.saturating_mul(DIRTY_BACKGROUND_FREE_RATIO_DENOMINATOR)
        < free.saturating_mul(DIRTY_BACKGROUND_FREE_RATIO_NUMERATOR)
    {
        return;
    }
    if WRITEBACK_ACTIVE.swap(true, Ordering::AcqRel) {
        return; // another caller is already flushing
    }
    crate::task::perf::record_wb_bg_call();

    // Snapshot alive page caches; drop dead weak refs
    let mut reg = PAGE_CACHE_REGISTRY.lock();
    reg.retain(|w| w.strong_count() > 0);
    let caches: Vec<Arc<PageCache>> = reg.iter().filter_map(|w| w.upgrade()).collect();
    drop(reg);

    let mut remaining = WB_BG_MAX_PAGES;
    for pc in &caches {
        if remaining == 0 {
            break;
        }
        let written = match pc.writeback_some_pages(remaining.min(WB_BATCH_PAGES)) {
            Ok(written) => written,
            Err(e) => {
                log::error!("background page-cache writeback failed: {:?}", e);
                0
            }
        };
        remaining = remaining.saturating_sub(written);
    }

    WRITEBACK_ACTIVE.store(false, Ordering::Release);
}

/// Three-tier dirty-page balancing, called after every write()/write_user().
///
/// Tier 1 (below DIRTY_BACKGROUND_PAGES): return immediately.
/// Tier 2 ([DIRTY_BACKGROUND_PAGES, DIRTY_THROTTLE_PAGES)): trigger async
///   background writeback without blocking the writer.
/// Tier 3 (>= DIRTY_THROTTLE_PAGES): the writer helps by synchronously
///   flushing one batch before returning (throttle path).
pub fn balance_dirty_pages() {
    let dirty = GLOBAL_DIRTY_PAGES.load(Ordering::Relaxed);

    // Tier 1 — below background: nothing to do.
    if dirty < DIRTY_BACKGROUND_PAGES {
        return;
    }

    // Tier 2 — background: fire-and-forget async writeback.
    if dirty < DIRTY_THROTTLE_PAGES {
        let free = crate::mm::unallocated_frames();
        if dirty.saturating_mul(DIRTY_BACKGROUND_FREE_RATIO_DENOMINATOR)
            >= free.saturating_mul(DIRTY_BACKGROUND_FREE_RATIO_NUMERATOR)
        {
            maybe_background_writeback();
        }
        return;
    }

    // Tier 3 — throttle: the writer synchronously writes back one batch.
    maybe_background_writeback();
}

/// Returns true when dirty pressure is critical (at or above emergency
/// watermark, AND dirty occupies >= 75% of free physical frames).
///
/// Used by the scheduler reclaim path to decide whether to force
/// additional writeback beyond the normal background cycle.
fn dirty_memory_pressure_is_critical(dirty: usize) -> bool {
    if dirty < DIRTY_EMERGENCY_PAGES {
        return false;
    }
    let free = crate::mm::unallocated_frames();
    dirty.saturating_mul(DIRTY_EMERGENCY_FREE_RATIO_DENOMINATOR)
        >= free.saturating_mul(DIRTY_EMERGENCY_FREE_RATIO_NUMERATOR)
}

// ── BlockPageCacheBackend ────────────────────────────────────────────────

/// 基于块设备的 PageCache 后端
#[allow(dead_code)]
pub struct BlockPageCacheBackend {
    /// 块设备
    block_device: Arc<dyn crate::drivers::block::BlockDevice>,
    /// 每个块的字节数
    block_size: usize,
    /// 每页的块数
    blocks_per_page: usize,
    /// 总块数
    total_blocks: usize,
    /// 页内块号 → 物理块号的映射闭包
    /// 闭包签名: Fn(page_index, block_offset_in_page) -> Option<block_id>
    block_mapper:
        Mutex<Option<alloc::boxed::Box<dyn Fn(usize, usize) -> Option<usize> + Send + Sync>>>,
}

impl BlockPageCacheBackend {
    /// 创建绑定到指定 `BlockDevice` 的后端，将页面索引映射到磁盘块号。
    pub fn new(
        block_device: Arc<dyn crate::drivers::block::BlockDevice>,
        block_size: usize,
        total_blocks: usize,
    ) -> Self {
        let blocks_per_page = PAGE_SIZE / block_size;
        assert!(blocks_per_page > 0, "PAGE_SIZE must be >= block_size");
        BlockPageCacheBackend {
            block_device,
            block_size,
            blocks_per_page,
            total_blocks,
            block_mapper: Mutex::new(None),
        }
    }

    /// 设置块号映射函数
    pub fn set_block_mapper<F>(&self, mapper: F)
    where
        F: Fn(usize, usize) -> Option<usize> + Send + Sync + 'static,
    {
        *self.block_mapper.lock() = Some(alloc::boxed::Box::new(mapper));
    }

    /// 将页面索引映射到起始块号
    fn page_to_block(&self, page_index: usize, block_offset: usize) -> Option<usize> {
        if let Some(ref mapper) = *self.block_mapper.lock() {
            mapper(page_index, block_offset)
        } else {
            // 默认：页面索引直接作为块号（简单映射）
            let block_id = page_index * self.blocks_per_page + block_offset;
            if block_id < self.total_blocks {
                Some(block_id)
            } else {
                None
            }
        }
    }
}

impl PageCacheBackend for BlockPageCacheBackend {
    fn read_page(&self, index: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        if buf.len() < PAGE_SIZE {
            return Err(SyscallErr::ENOBUFS);
        }

        for block_off in 0..self.blocks_per_page {
            let block_id = self
                .page_to_block(index, block_off)
                .ok_or(SyscallErr::EINVAL)?;
            let start = block_off * self.block_size;
            assert!(start + self.block_size <= PAGE_SIZE);
            self.block_device
                .read_block(block_id, &mut buf[start..start + self.block_size])
                .map_err(|_| SyscallErr::EIO)?;
        }

        Ok(PAGE_SIZE)
    }

    fn write_page(&self, index: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
        if buf.len() < PAGE_SIZE {
            return Err(SyscallErr::ENOBUFS);
        }

        for block_off in 0..self.blocks_per_page {
            let block_id = self
                .page_to_block(index, block_off)
                .ok_or(SyscallErr::EINVAL)?;
            let start = block_off * self.block_size;
            assert!(start + self.block_size <= PAGE_SIZE);
            self.block_device
                .write_block(block_id, &buf[start..start + self.block_size])
                .map_err(|_| SyscallErr::EIO)?;
        }

        Ok(PAGE_SIZE)
    }

    fn npages(&self) -> usize {
        (self.total_blocks + self.blocks_per_page - 1) / self.blocks_per_page
    }
}

// ── FatPageCacheBackend ─────────────────────────────────────────────────

/// FAT32 文件系统专用的 PageCache 后端
///
/// 通过弱引用访问 FatInode，将页面偏移动态映射为扇区号。
/// 读/写时临时持有读锁，自动适应 cluster list 变化。
pub struct FatPageCacheBackend {
    fs: alloc::sync::Arc<crate::fs::fat32::EasyFileSystem>,
    inode: alloc::sync::Weak<crate::fs::fat32::FatInode>,
    block_size: usize,
    blocks_per_page: usize,
    sec_per_clus: usize,
}

impl FatPageCacheBackend {
    pub fn new(
        fs: alloc::sync::Arc<crate::fs::fat32::EasyFileSystem>,
        inode: &alloc::sync::Weak<crate::fs::fat32::FatInode>,
    ) -> Self {
        let block_size = fs.byts_per_sec as usize;
        let blocks_per_page = crate::config::PAGE_SIZE / block_size;
        let sec_per_clus = fs.sec_per_clus as usize;
        FatPageCacheBackend {
            fs,
            inode: inode.clone(),
            block_size,
            blocks_per_page,
            sec_per_clus,
        }
    }

    fn block_id_for_offset(&self, page_index: usize, block_off: usize) -> Option<usize> {
        let inode = self.inode.upgrade()?;
        let lock = inode.file_content.read();
        let clus_list = &lock.clus_list;
        let block_index = page_index * self.blocks_per_page + block_off;
        let cluster_id = block_index / self.sec_per_clus;
        if cluster_id >= clus_list.len() {
            return None;
        }
        let offset = block_index % self.sec_per_clus;
        let start_block = self.fs.first_sector_of_cluster(clus_list[cluster_id]) as usize;
        Some(start_block + offset)
    }
}

impl PageCacheBackend for FatPageCacheBackend {
    fn read_page(&self, index: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        if buf.len() < crate::config::PAGE_SIZE {
            return Err(SyscallErr::ENOBUFS);
        }
        let ratio = crate::config::PAGE_SIZE / self.block_size;
        for block_off in 0..self.blocks_per_page {
            let start = block_off * self.block_size;
            assert!(start + self.block_size <= crate::config::PAGE_SIZE);
            match self.block_id_for_offset(index, block_off) {
                Some(sec_id) => {
                    // FAT32 sector 是 512 字节，BlockDevice 以 PAGE_SIZE/BLOCK_SZ(4096) 为单位
                    let blk_id = sec_id / ratio;
                    let blk_off = (sec_id % ratio) * self.block_size;
                    let mut blk_buf = alloc::vec![0u8; crate::config::PAGE_SIZE];
                    self.fs
                        .block_device
                        .read_block(blk_id, &mut blk_buf)
                        .map_err(|_| SyscallErr::EIO)?;
                    buf[start..start + self.block_size]
                        .copy_from_slice(&blk_buf[blk_off..blk_off + self.block_size]);
                }
                None => {
                    buf[start..start + self.block_size].fill(0);
                }
            }
        }
        Ok(crate::config::PAGE_SIZE)
    }

    fn write_page(&self, index: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
        if buf.len() < crate::config::PAGE_SIZE {
            return Err(SyscallErr::ENOBUFS);
        }
        let ratio = crate::config::PAGE_SIZE / self.block_size;
        for block_off in 0..self.blocks_per_page {
            let start = block_off * self.block_size;
            assert!(start + self.block_size <= crate::config::PAGE_SIZE);
            if let Some(sec_id) = self.block_id_for_offset(index, block_off) {
                let blk_id = sec_id / ratio;
                let blk_off = (sec_id % ratio) * self.block_size;
                let mut blk_buf = alloc::vec![0u8; crate::config::PAGE_SIZE];
                self.fs
                    .block_device
                    .read_block(blk_id, &mut blk_buf)
                    .map_err(|_| SyscallErr::EIO)?;
                blk_buf[blk_off..blk_off + self.block_size]
                    .copy_from_slice(&buf[start..start + self.block_size]);
                self.fs
                    .block_device
                    .write_block(blk_id, &blk_buf)
                    .map_err(|_| SyscallErr::EIO)?;
            }
        }
        Ok(crate::config::PAGE_SIZE)
    }

    fn npages(&self) -> usize {
        let inode = match self.inode.upgrade() {
            Some(i) => i,
            None => return 0,
        };
        let lock = inode.file_content.read();
        let total_blocks = lock.clus_list.len() * self.sec_per_clus;
        drop(lock);
        (total_blocks + self.blocks_per_page - 1) / self.blocks_per_page
    }
}

// ── Ext4PageCacheBackend ─────────────────────────────────────────────────

/// Extent lookup cache: avoids repeated extent tree walks for sequential block access.
/// Uses lock-free atomics — just a hint cache, reset on miss.
struct Ext4MapCache {
    valid: AtomicBool,
    inode_num: AtomicU32,
    lblock_start: AtomicU32,
    /// u32::MAX sentinel for holes
    pblock_start: AtomicU32,
    lblock_count: AtomicU32,
}

impl Ext4MapCache {
    fn new() -> Self {
        Ext4MapCache {
            valid: AtomicBool::new(false),
            inode_num: AtomicU32::new(0),
            lblock_start: AtomicU32::new(0),
            pblock_start: AtomicU32::new(0),
            lblock_count: AtomicU32::new(0),
        }
    }
}

/// EXT4 文件系统专用 PageCache 后端
///
/// 通过弱引用访问 Ext4FileSystem + inode_num，将页面偏移动态映射为物理块号。
/// 仅用于普通文件数据，不用于元数据（目录/bitmap/inode table）。
pub struct Ext4PageCacheBackend {
    ext4fs: alloc::sync::Weak<crate::fs::ext4::ext4fs::Ext4FileSystem>,
    inode_num: u32,
    block_size: usize,
    blocks_per_page: usize,
    /// Extent lookup hint cache: avoids repeated extent tree walks for sequential block access
    map_cache: Ext4MapCache,
}

impl Ext4PageCacheBackend {
    pub fn new(
        ext4fs: alloc::sync::Weak<crate::fs::ext4::ext4fs::Ext4FileSystem>,
        inode_num: u32,
    ) -> Self {
        let fs = ext4fs
            .upgrade()
            .expect("Ext4PageCacheBackend: ext4fs dropped");
        let block_size = fs.block_size;
        let blocks_per_page = crate::config::PAGE_SIZE / block_size;
        Ext4PageCacheBackend {
            ext4fs: ext4fs.clone(),
            inode_num,
            block_size,
            blocks_per_page,
            map_cache: Ext4MapCache::new(),
        }
    }

    fn block_id_for_offset(&self, page_index: usize, block_off: usize) -> Option<usize> {
        let lblock = (page_index * self.blocks_per_page + block_off) as u32;

        // Fast path: check extent hint cache
        if self.map_cache.valid.load(Ordering::Relaxed)
            && self.map_cache.inode_num.load(Ordering::Relaxed) == self.inode_num
        {
            let cache_start = self.map_cache.lblock_start.load(Ordering::Relaxed);
            let cache_count = self.map_cache.lblock_count.load(Ordering::Relaxed);
            if lblock >= cache_start && lblock < cache_start.saturating_add(cache_count) {
                crate::task::perf::record_ext4_map_cache_hit();
                let pstart = self.map_cache.pblock_start.load(Ordering::Relaxed);
                if pstart == u32::MAX {
                    return None; // cached hole
                }
                return Some(pstart as usize + (lblock - cache_start) as usize);
            }
        }

        // Slow path: extent tree lookup
        crate::task::perf::record_ext4_map_lblock();
        let fs = self.ext4fs.upgrade()?;
        let ino_ref = fs.get_inode_ref(self.inode_num);
        let _t = perf::perf_time_now();
        match fs.get_pblock_with_extent(&ino_ref, lblock) {
            Ok((pblock, ext_first, ext_len)) => {
                let elapsed = perf::perf_time_now().wrapping_sub(_t);
                perf::record_ext4_map_lblock_cost(elapsed);
                // Cache the full extent range
                self.map_cache.valid.store(true, Ordering::Relaxed);
                self.map_cache
                    .inode_num
                    .store(self.inode_num, Ordering::Relaxed);
                self.map_cache
                    .lblock_start
                    .store(ext_first, Ordering::Relaxed);
                self.map_cache
                    .pblock_start
                    .store(pblock - (lblock - ext_first), Ordering::Relaxed);
                self.map_cache
                    .lblock_count
                    .store(ext_len, Ordering::Relaxed);
                Some(pblock as usize)
            }
            Err(_) => {
                let elapsed = perf::perf_time_now().wrapping_sub(_t);
                perf::record_ext4_map_lblock_cost(elapsed);
                crate::task::perf::record_ext4_map_hole();
                // Cache the hole (single block only — extent range doesn't cover holes)
                self.map_cache.valid.store(true, Ordering::Relaxed);
                self.map_cache
                    .inode_num
                    .store(self.inode_num, Ordering::Relaxed);
                self.map_cache.lblock_start.store(lblock, Ordering::Relaxed);
                self.map_cache
                    .pblock_start
                    .store(u32::MAX, Ordering::Relaxed);
                self.map_cache.lblock_count.store(1, Ordering::Relaxed);
                None
            }
        }
    }
}

impl PageCacheBackend for Ext4PageCacheBackend {
    fn read_page(&self, index: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        if buf.len() < crate::config::PAGE_SIZE {
            return Err(SyscallErr::ENOBUFS);
        }
        let fs = self.ext4fs.upgrade().ok_or(SyscallErr::EIO)?;
        for block_off in 0..self.blocks_per_page {
            let start = block_off * self.block_size;
            assert!(start + self.block_size <= crate::config::PAGE_SIZE);
            match self.block_id_for_offset(index, block_off) {
                Some(block_id) => {
                    fs.block_device
                        .read_block(block_id, &mut buf[start..start + self.block_size])
                        .map_err(|_| SyscallErr::EIO)?;
                    crate::fs::ext4::counters::inc_counter!(
                        crate::fs::ext4::counters::DATA_BLOCK_READ
                    );
                    crate::fs::ext4::counters::inc_counter!(
                        crate::fs::ext4::counters::BLOCK_READ_TOTAL
                    );
                }
                None => {
                    buf[start..start + self.block_size].fill(0);
                }
            }
        }
        Ok(crate::config::PAGE_SIZE)
    }

    fn write_page(&self, index: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
        if buf.len() < crate::config::PAGE_SIZE {
            return Err(SyscallErr::ENOBUFS);
        }
        let fs = self.ext4fs.upgrade().ok_or(SyscallErr::EIO)?;
        for block_off in 0..self.blocks_per_page {
            let start = block_off * self.block_size;
            assert!(start + self.block_size <= crate::config::PAGE_SIZE);
            match self.block_id_for_offset(index, block_off) {
                Some(block_id) => {
                    fs.block_device
                        .write_block(block_id, &buf[start..start + self.block_size])
                        .map_err(|_| SyscallErr::EIO)?;
                    crate::fs::ext4::counters::inc_counter!(
                        crate::fs::ext4::counters::DATA_BLOCK_WRITE
                    );
                    crate::fs::ext4::counters::inc_counter!(
                        crate::fs::ext4::counters::BLOCK_WRITE_TOTAL
                    );
                }
                None => {
                    // Unmapped block — cannot write; keep page dirty for retry
                    return Err(SyscallErr::EIO);
                }
            }
        }
        Ok(crate::config::PAGE_SIZE)
    }

    fn npages(&self) -> usize {
        let fs = match self.ext4fs.upgrade() {
            Some(fs) => fs,
            None => return 0,
        };
        let ino_ref = fs.get_inode_ref(self.inode_num);
        let file_size = ino_ref.inode.size() as usize;
        (file_size + crate::config::PAGE_SIZE - 1) / crate::config::PAGE_SIZE
    }

    fn write_pages(&self, start_index: usize, pages: &[&[u8]]) -> Result<usize, SyscallErr> {
        crate::task::perf::record_ext4_pc_writepages_calls();
        crate::task::perf::record_ext4_pc_writepages_pages(pages.len());
        // 当 blocks_per_page > 1 时，打平所有 lblock，按物理连续分组批量写入
        if self.blocks_per_page != 1 {
            let fs = self.ext4fs.upgrade().ok_or(SyscallErr::EIO)?;
            let blk = &fs.block_device;
            let block_sz = self.block_size;
            let bpp = self.blocks_per_page;
            let mut run_count = 0usize;

            // 阶段 1：打平所有 (page_idx, block_off, pblock)
            let mut block_list: Vec<(usize, usize, usize)> = Vec::new();
            for (i, page) in pages.iter().enumerate() {
                if page.len() < crate::config::PAGE_SIZE {
                    return Err(SyscallErr::ENOBUFS);
                }
                let page_index = start_index + i;
                for bo in 0..bpp {
                    match self.block_id_for_offset(page_index, bo) {
                        Some(pblock) => block_list.push((i, bo, pblock)),
                        None => return Err(SyscallErr::EIO),
                    }
                }
            }

            // 阶段 2：按物理连续分组为 run，从页面收集数据后逐块写入
            let mut i = 0;
            while i < block_list.len() {
                let first_pblock = block_list[i].2;
                let run_start = i;
                let mut run_len = 1;
                let mut expected_pblock = first_pblock + 1;
                i += 1;
                while i < block_list.len() && block_list[i].2 == expected_pblock {
                    run_len += 1;
                    expected_pblock = block_list[i].2 + 1;
                    i += 1;
                }

                // 收集 run 中各块数据到 staging buffer
                let staging_size = run_len * block_sz;
                let mut staging: Vec<u8> = alloc::vec![0u8; staging_size];
                for j in 0..run_len {
                    let (page_idx, block_off, _) = block_list[run_start + j];
                    let src_start = block_off * block_sz;
                    let dst_start = j * block_sz;
                    staging[dst_start..dst_start + block_sz]
                        .copy_from_slice(&pages[page_idx][src_start..src_start + block_sz]);
                }

                // 批量写入块设备：将整个 run 作为一次 write_block 调用
                // pblock 是 512B 单位，需转为 BLOCK_SZ 单位（pblock / blocks_per_page）
                // staging 按 512B 拼接，因 run 跨整页边界，staging_size 必为 BLOCK_SZ 的整数倍
                let first_pblock_4k = first_pblock / bpp;
                blk.write_block(first_pblock_4k, &staging)
                    .map_err(|_| SyscallErr::EIO)?;

                // 更新计数器（等价于逐块调用）
                for _ in 0..run_len {
                    crate::fs::ext4::counters::inc_counter!(
                        crate::fs::ext4::counters::DATA_BLOCK_WRITE
                    );
                    crate::fs::ext4::counters::inc_counter!(
                        crate::fs::ext4::counters::BLOCK_WRITE_TOTAL
                    );
                }
                run_count += 1;
            }

            crate::task::perf::record_ext4_pc_writepages_runs(run_count);
            return Ok(pages.len() * crate::config::PAGE_SIZE);
        }

        let fs = self.ext4fs.upgrade().ok_or(SyscallErr::EIO)?;
        let block_sz = self.block_size;

        // 第一阶段：解析所有物理块号，验证无空洞
        let mut block_map: Vec<(usize, usize)> = Vec::new();
        for (i, page) in pages.iter().enumerate() {
            let page_index = start_index + i;
            if page.len() < crate::config::PAGE_SIZE {
                return Err(SyscallErr::ENOBUFS);
            }
            let pblock = match self.block_id_for_offset(page_index, 0) {
                Some(pb) => pb,
                None => return Err(SyscallErr::EIO),
            };
            block_map.push((i, pblock));
        }

        // 第二阶段：将物理连续块分组为 run，通过 staging buffer 批量写入
        let mut i = 0;
        let mut run_count = 0usize;
        let blk = &fs.block_device;
        while i < block_map.len() {
            let first_pblock = block_map[i].1;
            let mut run_len = 1;
            let mut expected_pblock = first_pblock + 1;
            i += 1;
            while i < block_map.len() && block_map[i].1 == expected_pblock {
                run_len += 1;
                expected_pblock = block_map[i].1 + 1;
                i += 1;
            }

            // 构建 staging buffer 并复制页面数据
            let staging_size = run_len * block_sz;
            let mut staging: Vec<u8> = alloc::vec![0u8; staging_size];
            let base_run_idx = i - run_len;
            for j in 0..run_len {
                let page_idx = block_map[base_run_idx + j].0;
                let dst_start = j * block_sz;
                staging[dst_start..dst_start + block_sz]
                    .copy_from_slice(&pages[page_idx][..block_sz]);
            }

            // 批量写入块设备
            blk.write_block(first_pblock, &staging)
                .map_err(|_| SyscallErr::EIO)?;

            // 更新计数器（等价于逐块调用）
            for _ in 0..run_len {
                crate::fs::ext4::counters::inc_counter!(
                    crate::fs::ext4::counters::DATA_BLOCK_WRITE
                );
                crate::fs::ext4::counters::inc_counter!(
                    crate::fs::ext4::counters::BLOCK_WRITE_TOTAL
                );
            }
            run_count += 1;
        }

        crate::task::perf::record_ext4_pc_writepages_runs(run_count);
        Ok(pages.len() * crate::config::PAGE_SIZE)
    }

    fn read_pages(&self, start_index: usize, pages: &mut [&mut [u8]]) -> Result<usize, SyscallErr> {
        crate::task::perf::record_ext4_pc_readpages_calls();
        crate::task::perf::record_ext4_pc_readpages_pages(pages.len());
        // 当 blocks_per_page > 1 时，打平所有 lblock，按物理连续分组批量读取
        if self.blocks_per_page != 1 {
            let fs = self.ext4fs.upgrade().ok_or(SyscallErr::EIO)?;
            let blk = &fs.block_device;
            let block_sz = self.block_size;
            let bpp = self.blocks_per_page;

            // 阶段 1：打平所有 (page_idx, block_off, pblock_opt)
            let mut block_list: Vec<(usize, usize, Option<usize>)> = Vec::new();
            for (i, page) in pages.iter().enumerate() {
                if page.len() < crate::config::PAGE_SIZE {
                    return Err(SyscallErr::ENOBUFS);
                }
                let page_index = start_index + i;
                for bo in 0..bpp {
                    let pblock = self.block_id_for_offset(page_index, bo);
                    block_list.push((i, bo, pblock));
                }
            }

            // 阶段 2：按物理连续分组为 run，批量读取后分散到各页面
            let mut i = 0;
            let mut run_count = 0usize;
            while i < block_list.len() {
                // 跳过空洞（零填充在循环外统一处理）
                if block_list[i].2.is_none() {
                    i += 1;
                    continue;
                }

                let first_pblock = block_list[i].2.unwrap();
                let run_start = i;
                let mut run_len = 1;
                let mut expected_pblock = first_pblock + 1;
                i += 1;
                while i < block_list.len() {
                    match block_list[i].2 {
                        Some(pb) if pb == expected_pblock => {
                            run_len += 1;
                            expected_pblock = pb + 1;
                            i += 1;
                        }
                        _ => break,
                    }
                }

                // 批量读取到 staging buffer
                let staging_size = run_len * block_sz;
                let mut staging: Vec<u8> = alloc::vec![0u8; staging_size];
                blk.read_block(first_pblock, &mut staging)
                    .map_err(|_| SyscallErr::EIO)?;

                // 分散到各页面的对应 block_off 位置
                for j in 0..run_len {
                    let (page_idx, block_off, _) = block_list[run_start + j];
                    let src_start = j * block_sz;
                    let dst_start = block_off * block_sz;
                    pages[page_idx][dst_start..dst_start + block_sz]
                        .copy_from_slice(&staging[src_start..src_start + block_sz]);
                }

                // 更新计数器
                for _ in 0..run_len {
                    crate::fs::ext4::counters::inc_counter!(
                        crate::fs::ext4::counters::DATA_BLOCK_READ
                    );
                    crate::fs::ext4::counters::inc_counter!(
                        crate::fs::ext4::counters::BLOCK_READ_TOTAL
                    );
                }
                run_count += 1;
            }

            // 阶段 3：零填充所有空洞对应的 page 位置
            for (page_idx, block_off, pblock_opt) in &block_list {
                if pblock_opt.is_none() {
                    let dst_start = block_off * block_sz;
                    pages[*page_idx][dst_start..dst_start + block_sz].fill(0);
                }
            }

            crate::task::perf::record_ext4_pc_readpages_runs(run_count);
            return Ok(pages.len() * crate::config::PAGE_SIZE);
        }

        let fs = self.ext4fs.upgrade().ok_or(SyscallErr::EIO)?;
        let block_sz = self.block_size;

        // 第一阶段：解析所有物理块号，区分映射块和空洞
        // block_map: (page_index_in_batch, physical_block_option)
        let mut block_map: Vec<(usize, Option<usize>)> = Vec::new();
        for (i, page) in pages.iter().enumerate() {
            let page_index = start_index + i;
            if page.len() < crate::config::PAGE_SIZE {
                return Err(SyscallErr::ENOBUFS);
            }
            let pblock = self.block_id_for_offset(page_index, 0);
            block_map.push((i, pblock));
        }

        // 第二阶段：将物理连续块分组为 run，批量读取
        // 只对映射块做批量读取；空洞单独零填充
        let mut idx = 0;
        let mut run_count = 0usize;
        let blk = &fs.block_device;
        while idx < block_map.len() {
            // 跳过空洞（零填充在循环外统一处理）
            if block_map[idx].1.is_none() {
                idx += 1;
                continue;
            }

            // 找到连续物理块的 run
            let first_pblock = block_map[idx].1.unwrap();
            let run_start = idx;
            let mut run_len = 1;
            let mut expected_pblock = first_pblock + 1;
            idx += 1;
            while idx < block_map.len() {
                match block_map[idx].1 {
                    Some(pb) if pb == expected_pblock => {
                        run_len += 1;
                        expected_pblock = pb + 1;
                        idx += 1;
                    }
                    _ => break,
                }
            }

            // 批量读取物理连续块到 staging buffer
            let staging_size = run_len * block_sz;
            let mut staging: Vec<u8> = alloc::vec![0u8; staging_size];
            blk.read_block(first_pblock, &mut staging)
                .map_err(|_| SyscallErr::EIO)?;

            // 将 staging buffer 中的数据拷贝到各页面
            for j in 0..run_len {
                let page_idx = block_map[run_start + j].0;
                let src_start = j * block_sz;
                pages[page_idx][..block_sz]
                    .copy_from_slice(&staging[src_start..src_start + block_sz]);
            }

            // 更新计数器
            for _ in 0..run_len {
                crate::fs::ext4::counters::inc_counter!(crate::fs::ext4::counters::DATA_BLOCK_READ);
                crate::fs::ext4::counters::inc_counter!(
                    crate::fs::ext4::counters::BLOCK_READ_TOTAL
                );
            }
            run_count += 1;
        }

        // 第三阶段：零填充所有空洞页面
        for (page_idx, pblock_opt) in &block_map {
            if pblock_opt.is_none() {
                pages[*page_idx].fill(0);
            }
        }

        crate::task::perf::record_ext4_pc_readpages_runs(run_count);
        Ok(pages.len() * crate::config::PAGE_SIZE)
    }
}
