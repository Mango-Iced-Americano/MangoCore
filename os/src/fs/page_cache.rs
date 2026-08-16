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
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use spin::{Mutex, RwLock, RwLockReadGuard};

use super::vfs::IndexNode;
use crate::config::{PAGE_SIZE, PAGE_SIZE_BITS};
use crate::mm::{frame_alloc, FrameTracker};
use crate::mm::{FileVmaRmap, FileVmaSnapshot, RetryWait};
use crate::task::perf;
#[cfg(feature = "perf_stats")]
use crate::task::BlockedReason;
use crate::task::{WaitQueue, WaitResult};

// ── Global dirty page accounting ──────────────────────────────────────

/// 全局脏页计数（所有 PageCache 的总和）
static GLOBAL_DIRTY_PAGES: AtomicUsize = AtomicUsize::new(0);
/// 全局正在写回的页面计数
static GLOBAL_WRITEBACK_PAGES: AtomicUsize = AtomicUsize::new(0);
/// 后台写回互斥标志（防止并发写回）
static WRITEBACK_ACTIVE: AtomicBool = AtomicBool::new(false);

/// 后台写回启动阈值（页数，约 8MB，高于典型 4MB 测试集避免频繁触发）
const DIRTY_BACKGROUND: usize = 8192;
/// 写入者节流阈值（页数，约 64MB）
const DIRTY_THROTTLE: usize = 16384;
/// 正常后台写回批次大小
const WB_BATCH_PAGES: usize = 256;
/// 节流时的最大写回页数
const WB_BG_MAX_PAGES: usize = 256;
/// 全局脏页计数快照（用于诊断）
pub fn global_dirty_pages() -> usize {
    GLOBAL_DIRTY_PAGES.load(Ordering::Relaxed)
}

/// 全局写回页计数快照（用于诊断）
pub fn global_writeback_pages() -> usize {
    GLOBAL_WRITEBACK_PAGES.load(Ordering::Relaxed)
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
    let low_mask: u32 = (1u32 << count) - 1;
    low_mask << seg_start
}

static PAGE_CACHE_REGISTRY: Mutex<Vec<Weak<PageCache>>> = Mutex::new(Vec::new());

pub fn register_page_cache(pc: &Arc<PageCache>) {
    PAGE_CACHE_REGISTRY.lock().push(Arc::downgrade(pc));
}

pub fn flush_all_page_caches() -> Result<(), SyscallErr> {
    // Never execute backend I/O while holding the global registry lock.  A
    // backend may re-enter PageCache registration or another cache during
    // writeback; keeping the lock across that call turns a transient
    // contention into a global deadlock.
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

    let mut first_error = None;
    for page_cache in page_caches {
        if let Err(error) = page_cache.writeback_all() {
            log::error!("flush_all_page_caches: writeback failed: {:?}", error);
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    first_error.map_or(Ok(()), Err)
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

/// file fault 对 PageCache 非阻塞 admission 的结果。
pub(crate) enum PageCacheFault {
    Retry(Arc<dyn RetryWait>),
    Error(SyscallErr),
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
/// Backend staging has one MiB maximum, including explicit ELF prefetches.
pub const MAX_BATCH_READ_PAGES: usize = 256;
/// Demand faults use a bounded contiguous staging window.  Keeping this at
/// 128 KiB limits transient memory while still amortizing backend request setup.
pub const MAX_DEMAND_READ_PAGES: usize = 32;

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

    /// 批量读取一段页对齐的连续字节。
    ///
    /// 默认实现按页回退，后端可以覆盖此方法把整段读取直接交给文件系统
    /// 或块设备。`buffer` 长度必须是 PAGE_SIZE 的整数倍。
    fn read_contiguous(&self, start_index: usize, buffer: &mut [u8]) -> Result<usize, SyscallErr> {
        if buffer.len() % PAGE_SIZE != 0 {
            return Err(SyscallErr::ENOBUFS);
        }
        for (i, page) in buffer.chunks_exact_mut(PAGE_SIZE).enumerate() {
            self.read_page(start_index + i, page)?;
        }
        Ok(buffer.len())
    }

    /// 返回后端的页数
    fn npages(&self) -> usize;

    /// Publish backend-specific lifetime state as soon as a page becomes
    /// dirty. This must run after the payload copy but before the caller can
    /// drop the last VFS inode reference; mmap dirtying has no inode mutation
    /// wrapper in which to retain the cache.
    fn on_page_dirty(&self) {}
}

// ── PageEntry flags ──────────────────────────────────────────────────────

/// PageEntry flags 位定义。状态与重入标志放在同一个原子字中，避免
/// `state`/`flags` 两次发布在写回竞态下相互覆盖。
const PG_LOCKED: u32 = 1 << 0;
const PG_UPTODATE: u32 = 1 << 1;
const PG_DIRTY: u32 = 1 << 2;
const PG_WRITEBACK: u32 = 1 << 3;
const PG_ERROR: u32 = 1 << 4;
pub const PG_REFERENCED: u32 = 1 << 5;
/// 页面在写回期间被再次标记为脏（写回完成后应恢复为 Dirty）
pub const PG_REDIRTIED: u32 = 1 << 6;
/// Page admitted speculatively by filemap fault-around and not demanded yet.
const PG_FILEMAP_READAHEAD: u32 = 1 << 7;
const PG_ORTHOGONAL: u32 = PG_REFERENCED | PG_REDIRTIED | PG_FILEMAP_READAHEAD;

// ── PageEntry ────────────────────────────────────────────────────────────

/// 页面缓存条目
#[derive(Debug)]
struct PageEntry {
    /// 物理页面
    page: Arc<FrameTracker>,
    /// 保护该页 frame bytes；entries/inner 解锁后才允许获取。
    data: RwLock<()>,
    /// 页面状态和引用/重入标志的原子集合。
    flags: AtomicU32,
    /// 部分写入有效性位掩码：每 bit 对应 512B segment，1=已写入/有效
    /// 初始值取决于创建方式：populate → VALID_ALL，zero-fill → 0
    valid_mask: AtomicU32,
    /// 已安装到用户页表的 file-backed PTE 数量。
    ///
    /// 该计数只作为 rmap/unmap 的快速判定；真正的 VMA→VA 定位由 PageCache
    /// 的 i_mmap 注册表完成。PTE 安装和 zap 都在所属 VM 锁内更新它，因而不能
    /// 用它替代 PTE 或 VMA 的权威状态。
    map_count: AtomicUsize,
}

impl PageEntry {
    fn new(page: Arc<FrameTracker>, state: PageState) -> Self {
        PageEntry {
            page,
            data: RwLock::new(()),
            flags: AtomicU32::new(Self::flags_for_state(state)),
            valid_mask: AtomicU32::new(VALID_ALL),
            map_count: AtomicUsize::new(0),
        }
    }

    /// 创建一个带指定 valid_mask 的页面条目（跳过后端读取）
    /// 用于页面超出旧 EOF 的场景：valid_mask=VALID_ALL 表示全零页即有效
    fn new_with_valid_mask(page: Arc<FrameTracker>, valid_mask: u32) -> Self {
        PageEntry {
            page,
            data: RwLock::new(()),
            flags: AtomicU32::new(PG_UPTODATE),
            valid_mask: AtomicU32::new(valid_mask),
            map_count: AtomicUsize::new(0),
        }
    }

    fn state(&self) -> PageState {
        Self::decode_state(self.flags.load(Ordering::Acquire))
    }

    fn set_state(&self, state: PageState) {
        let state_flags = Self::flags_for_state(state);
        let mut old = self.flags.load(Ordering::Acquire);
        loop {
            let desired = state_flags | (old & PG_ORTHOGONAL);
            match self
                .flags
                .compare_exchange(old, desired, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(current) => old = current,
            }
        }
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

    /// CAS a decoded state while preserving orthogonal referenced/redirtied bits.
    fn compare_exchange_state(&self, old: PageState, new: PageState) -> Result<u32, u32> {
        let mut raw = self.flags.load(Ordering::Acquire);
        loop {
            if Self::decode_state(raw) != old {
                return Err(raw);
            }
            let desired = Self::flags_for_state(new) | (raw & PG_ORTHOGONAL);
            match self
                .flags
                .compare_exchange(raw, desired, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(previous) => return Ok(previous),
                Err(current) => raw = current,
            }
        }
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

    /// Acquire the per-page write lease.  The lease is separate from the
    /// data RwLock: it prevents writeback from claiming the page while a
    /// writer is copying user/kernel bytes and publishing Dirty.
    fn try_lock_for_write(&self) -> Result<Option<bool>, SyscallErr> {
        let old = self.flags();
        match Self::decode_state(old) {
            PageState::UpToDate | PageState::Dirty if old & PG_LOCKED == 0 => {
                let new = old | PG_LOCKED | PG_REFERENCED;
                match self.compare_exchange_flags(old, new) {
                    Ok(_) => Ok(Some(old & PG_DIRTY != 0)),
                    Err(_) => Ok(None),
                }
            }
            PageState::Loading | PageState::Writeback => Ok(None),
            PageState::Error => Err(SyscallErr::EIO),
            PageState::UpToDate | PageState::Dirty => Ok(None),
        }
    }

    fn write_lease_ready(&self) -> bool {
        let flags = self.flags();
        matches!(
            Self::decode_state(flags),
            PageState::UpToDate | PageState::Dirty | PageState::Error
        ) && (flags & PG_LOCKED == 0 || flags & PG_ERROR != 0)
    }

    /// Publish a completed write and return whether it transitioned a clean
    /// page to Dirty (the caller updates global dirty accounting).
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

    /// Claim a dirty page for writeback.  A writer holding PG_LOCKED is never
    /// displaced by writeback; the CAS clears Dirty and sets Writeback/Locked
    /// as one ownership transition.
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

    /// Complete a successful writeback and report whether the page was
    /// redirtied while the backend I/O was in flight.
    fn complete_writeback(&self) -> bool {
        let redirtied = self.test_and_clear_flag(PG_REDIRTIED);
        let add = if redirtied {
            PG_DIRTY
        } else {
            PG_UPTODATE | PG_REFERENCED
        };
        let _ = self
            .flags
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                Some((old | add) & !(PG_WRITEBACK | PG_LOCKED | PG_REDIRTIED))
            });
        redirtied
    }

    fn restore_dirty_after_writeback(&self) {
        let _ = self
            .flags
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                Some((old | PG_DIRTY) & !(PG_WRITEBACK | PG_LOCKED | PG_REDIRTIED))
            });
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
        // valid_mask only gains bits, so a range that is already fully valid
        // cannot transition again; avoid an unnecessary AMO on the hot path.
        if self.valid_mask.load(Ordering::Relaxed) & mask == mask {
            return false;
        }
        let old = self.valid_mask.fetch_or(mask, Ordering::Release);
        if old & mask == mask {
            return false;
        }
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

    fn mark_filemap_readahead(&self) {
        self.set_flag(PG_FILEMAP_READAHEAD);
    }

    fn consume_filemap_readahead(&self) {
        if self.test_and_clear_flag(PG_FILEMAP_READAHEAD) {
            crate::task::perf::record_filemap_fault_around_useful_hit();
        }
    }

    fn discard_filemap_readahead(&self) {
        if self.test_and_clear_flag(PG_FILEMAP_READAHEAD) {
            crate::task::perf::record_filemap_fault_around_unused_discard();
        }
    }

    /// 在 data 读锁存续期间向闭包提供页字节；借用不能逃出闭包。
    fn with_bytes<R>(&self, f: impl for<'a> FnOnce(&'a [u8]) -> R) -> R {
        let _data = self.data.read();
        // SAFETY: [Category 1/2 — aliasing/data race] `data` 的读锁允许多个只读
        // 借用而排斥写者；闭包的 HRTB 不能将该 slice 的借用带出锁的作用域。
        unsafe { f(self.bytes_unchecked()) }
    }

    /// 在 data 写锁存续期间向闭包提供页字节；借用不能逃出闭包。
    fn with_bytes_mut<R>(&self, f: impl for<'a> FnOnce(&'a mut [u8]) -> R) -> R {
        let _data = self.data.write();
        // SAFETY: [Category 1/2 — aliasing/data race] `data` 写锁在此作用域内
        // 唯一持有，排斥所有读写者；闭包的 HRTB 不能让可变 slice 逃出该作用域。
        unsafe { f(self.bytes_mut_unchecked()) }
    }

    /// 为批量 writeback 构造局部读 guard；guard 与 bytes 同寿命，不能离开本模块。
    fn read_bytes(&self) -> PageBytesReadGuard<'_> {
        PageBytesReadGuard {
            _data: self.data.read(),
            entry: self,
        }
    }

    /// # Safety
    /// 调用者必须持有 `data` 的读锁或写锁，并只把返回值约束在该锁的作用域内。
    unsafe fn bytes_unchecked(&self) -> &[u8] {
        let ptr = self.page.ppn.start_addr().direct_map_ptr() as *const u8;
        // SAFETY: PageEntry 始终持有该 frame 的 Arc；调用者持有 data 锁，保证
        // direct-map 的 PAGE_SIZE 字节可读且不存在并发写入。
        unsafe { core::slice::from_raw_parts(ptr, PAGE_SIZE) }
    }

    /// # Safety
    /// 调用者必须唯一持有 `data` 写锁，并只把返回值约束在该锁的作用域内。
    // `PageEntry` 通过 `data` 实现运行期内部可变性；调用方只能经
    // `with_bytes_mut` 在唯一写锁和 HRTB 闭包作用域内取得该借用，不能改为
    // `&mut self` 而破坏缓存条目的共享所有权模型。
    #[allow(clippy::mut_from_ref)]
    unsafe fn bytes_mut_unchecked(&self) -> &mut [u8] {
        let ptr = self.page.ppn.start_addr().direct_map_ptr();
        // SAFETY: PageEntry 始终持有该 frame 的 Arc；唯一 data 写锁排斥所有
        // 读写者，因此 direct-map 的 PAGE_SIZE 字节可独占可写。
        unsafe { core::slice::from_raw_parts_mut(ptr, PAGE_SIZE) }
    }
}

/// 批量 writeback 的私有页快照 guard。它先持有 data-read，再暴露借用到 backend
/// 调用完成，禁止把 `&[u8]` 保存到 guard 作用域之外。
struct PageBytesReadGuard<'a> {
    _data: RwLockReadGuard<'a, ()>,
    entry: &'a PageEntry,
}

impl PageBytesReadGuard<'_> {
    fn bytes(&self) -> &[u8] {
        // SAFETY: `_data` 在本 guard 的整个生命周期内持有对应 entry 的读锁；
        // 返回借用受 `&self` 限制，backend 调用结束前 guard 不会释放。
        unsafe { self.entry.bytes_unchecked() }
    }
}

// ── InnerPageCache ───────────────────────────────────────────────────────

/// PageCache 内部状态
#[derive(Debug)]
struct InnerPageCache {
    /// 页面映射: page_index → PageEntry
    pages: BTreeSet<usize>,
}

impl InnerPageCache {
    fn new() -> Self {
        InnerPageCache {
            pages: BTreeSet::new(),
        }
    }

    fn has_page(&self, index: usize) -> bool {
        self.pages.contains(&index)
    }

    fn page_count(&self) -> usize {
        self.pages.len()
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

/// Result of scanning the full read range under one entries lock.
struct ReadPlan {
    copies: Vec<ReadCopy>,
    miss_runs: Vec<MissRun>,
    needs_valid_fill: BTreeSet<usize>, // pages that exist but partially valid
}

// ── PageCache ────────────────────────────────────────────────────────────

/// 页面缓存
///
/// 为 inode 提供页面级别的缓存，管理内存中的文件数据副本。
pub struct PageCache {
    /// 普通读写/回写持读锁；截断、失效、回收和 I/O 后发布持写锁。
    /// 锁内不得进入用户态 copy、等待或调度。
    op_gate: RwLock<()>,
    /// 所有 file-backed MAP_SHARED VMA 的弱索引。注册/摘除只发生在
    /// mmap/fork/munmap/exec，不在 fault 热路径建立反向映射。
    ///
    /// `BTreeMap` 是 no_std 当前可用的有序映射；键仍是 VMA 地址身份，语义等同
    /// blueprint 的 HashMap，读侧通过 `i_mmap_seq` 重新验证并不依赖遍历顺序。
    i_mmap: Mutex<BTreeMap<usize, Weak<FileVmaRmap>>>,
    /// VMA 注册表代际；rmap walker 在无锁 VM 遍历后必须重验。
    i_mmap_seq: AtomicU64,
    /// 内部状态
    inner: Mutex<InnerPageCache>,
    /// 缓存后端
    backend: Mutex<Option<Arc<dyn PageCacheBackend>>>,
    /// 关联的 inode（弱引用）
    inode: Mutex<Option<Weak<dyn IndexNode>>>,
    /// 缓存的页面条目
    entries: Mutex<Vec<Option<Arc<PageEntry>>>>,
    /// Pages claimed by a lock-outside batch read but not published yet.
    ///
    /// This ownership directory is intentionally separate from `entries`:
    /// ordinary PageCache readers must never observe an uninitialised frame.
    /// Concurrent fault-around workers use it to coalesce the same miss and
    /// sleep on `state_wait_generation` until the owner publishes or aborts.
    batch_read_claims: Mutex<BTreeSet<usize>>,
    /// true = 页不可回收（用于 tmpfs/shmem，数据无持久化后端）
    unevictable: AtomicBool,
    /// Clock sweep 光标（second-chance eviction）
    clock_hand: AtomicUsize,
    /// `MS_ASYNC` 发布的合作式写回请求。发布者不执行 I/O；reclaim worker 在锁外
    /// 消费请求并复用正常 writeback 状态机。
    async_writeback_requested: AtomicBool,
    /// Generation for mutations that discard cache entries. Batch backend I/O
    /// runs without `op_gate`; this guard prevents stale publication after a
    /// concurrent truncate/invalidate.
    mutation_generation: AtomicUsize,
    /// Shared notification domain for Loading/Writeback completion. A single
    /// generation avoids adding a wait queue to every cached page.
    state_wait_generation: AtomicUsize,
    /// Number of tasks registered or entering the shared state wait queue.
    /// Producers use it to avoid taking the queue lock on uncontended writes.
    state_waiter_count: AtomicUsize,
    state_waiters: Mutex<WaitQueue>,
}

struct PageCacheFaultWait {
    cache: Arc<PageCache>,
    page_index: usize,
    fault_around_pages: usize,
}

/// 写入路径各阶段（lookup/lease/copy/commit）的周期累计，用于 perf 分阶段计时。
#[derive(Default)]
struct WriteStageCycles {
    lookup: usize,
    lease: usize,
    copy: usize,
    commit: usize,
}

enum WriteAttemptError {
    Busy(Arc<PageEntry>),
    Error(SyscallErr),
}

impl From<SyscallErr> for WriteAttemptError {
    fn from(error: SyscallErr) -> Self {
        Self::Error(error)
    }
}

impl RetryWait for PageCacheFaultWait {
    fn wait(&self) {
        let wait_start =
            crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
        // 此处由 trap/uaccess 外层保证已经释放 VM 锁。I/O 仍可在 op_gate
        // 读侧完成，但任何睡眠都必须发生在释放 op_gate 之后。
        let mut fault_around_attempted = false;
        loop {
            if !fault_around_attempted && self.fault_around_pages > 1 {
                fault_around_attempted = true;
                // Speculative failures must not turn a valid demand fault into
                // an error. The single-page path below remains authoritative.
                let _ = self
                    .cache
                    .sync_filemap_fault_around(self.page_index, self.fault_around_pages);
            }
            let observed = self.cache.state_wait_generation.load(Ordering::Acquire);
            // Another fault-around worker may own this miss while its backend
            // I/O is deliberately outside PageCache locks.  Do not fall
            // through to the authoritative single-page loader and duplicate
            // that read; the owner always publishes or releases before
            // advancing the shared state generation.
            if self
                .cache
                .batch_read_claims
                .lock()
                .contains(&self.page_index)
            {
                if self.cache.wait_for_state_progress(observed).is_err() {
                    crate::task::perf::record_filemap_retry_wait(
                        crate::task::perf::perf_time_now_for(
                            crate::task::perf::STATS_PROFILE_MEMORY_IO,
                        )
                        .wrapping_sub(wait_start),
                    );
                    return;
                }
                continue;
            }
            let may_load = self
                .cache
                .entries
                .lock()
                .get(self.page_index)
                .and_then(Option::as_ref)
                .map_or(true, |entry| !entry.is_fully_valid());
            let backend_start =
                crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
            let result = {
                let _op = self.cache.op_gate.read();
                self.cache
                    .get_or_create_entry(self.page_index, true, None)
                    .and_then(|entry| {
                        self.cache.ensure_fully_valid(self.page_index)?;
                        match entry.state() {
                            PageState::Loading | PageState::Writeback => Err(SyscallErr::EAGAIN),
                            PageState::Error => Err(SyscallErr::EIO),
                            PageState::UpToDate | PageState::Dirty => Ok(()),
                        }
                    })
            };
            if may_load {
                crate::task::perf::record_filemap_backend_read(
                    crate::task::perf::perf_time_now_for(
                        crate::task::perf::STATS_PROFILE_MEMORY_IO,
                    )
                    .wrapping_sub(backend_start),
                    false,
                );
            }
            match result {
                Ok(()) | Err(SyscallErr::EIO) => {
                    crate::task::perf::record_filemap_retry_wait(
                        crate::task::perf::perf_time_now_for(
                            crate::task::perf::STATS_PROFILE_MEMORY_IO,
                        )
                        .wrapping_sub(wait_start),
                    );
                    return;
                }
                Err(SyscallErr::EAGAIN) => {
                    if self.cache.wait_for_state_progress(observed).is_err() {
                        crate::task::perf::record_filemap_retry_wait(
                            crate::task::perf::perf_time_now_for(
                                crate::task::perf::STATS_PROFILE_MEMORY_IO,
                            )
                            .wrapping_sub(wait_start),
                        );
                        return;
                    }
                }
                Err(_) => {
                    crate::task::perf::record_filemap_retry_wait(
                        crate::task::perf::perf_time_now_for(
                            crate::task::perf::STATS_PROFILE_MEMORY_IO,
                        )
                        .wrapping_sub(wait_start),
                    );
                    return;
                }
            }
        }
    }
}

impl PageCache {
    /// Build a lock-outside retry token for a file-backed page fault.
    ///
    /// File mappings may observe a page in `Writeback` while the writeback
    /// worker still owns the page-cache gate.  The fault handler runs under
    /// the address-space lock, so it must return a retry token instead of
    /// translating this transient state into `EFAULT`.
    pub(crate) fn filemap_fault_wait(self: &Arc<Self>, page_index: usize) -> Arc<dyn RetryWait> {
        Arc::new(PageCacheFaultWait {
            cache: Arc::clone(self),
            page_index,
            fault_around_pages: 1,
        })
    }

    /// 创建一个不含 backend 和 inode 关联的空 PageCache，自动注册到全局列表。
    pub fn new() -> Arc<Self> {
        let pc = Arc::new(PageCache {
            op_gate: RwLock::new(()),
            i_mmap: Mutex::new(BTreeMap::new()),
            i_mmap_seq: AtomicU64::new(0),
            inner: Mutex::new(InnerPageCache::new()),
            backend: Mutex::new(None),
            inode: Mutex::new(None),
            entries: Mutex::new(Vec::new()),
            batch_read_claims: Mutex::new(BTreeSet::new()),
            unevictable: AtomicBool::new(false),
            clock_hand: AtomicUsize::new(0),
            async_writeback_requested: AtomicBool::new(false),
            mutation_generation: AtomicUsize::new(0),
            state_wait_generation: AtomicUsize::new(0),
            state_waiter_count: AtomicUsize::new(0),
            state_waiters: Mutex::new(WaitQueue::new()),
        });
        register_page_cache(&pc);
        pc
    }

    fn notify_state_progress(&self) {
        self.state_wait_generation.fetch_add(1, Ordering::Release);
        if self.state_waiter_count.load(Ordering::Acquire) != 0 {
            self.state_waiters.lock().wake_all();
        }
    }

    fn wait_for_state_progress(&self, observed: usize) -> Result<(), SyscallErr> {
        if crate::task::current_task().is_none() {
            return Err(SyscallErr::EAGAIN);
        }
        self.state_waiter_count.fetch_add(1, Ordering::AcqRel);
        #[cfg(feature = "perf_stats")]
        let _blocked_reason = crate::task::current_task()
            .map(|task| task.blocked_reason_scope(BlockedReason::PageCache));
        let result = match WaitQueue::wait_event(&self.state_waiters, || {
            (self.state_wait_generation.load(Ordering::Acquire) != observed).then_some(0)
        }) {
            WaitResult::Ready(_) => Ok(()),
            WaitResult::Interrupted => Err(SyscallErr::ERESTART),
            WaitResult::TimedOut => unreachable!("page-state wait has no deadline"),
        };
        self.state_waiter_count.fetch_sub(1, Ordering::AcqRel);
        result
    }

    fn release_batch_read_claims(&self, claimed: &[usize]) {
        if claimed.is_empty() {
            return;
        }
        let mut claims = self.batch_read_claims.lock();
        for page_index in claimed {
            claims.remove(page_index);
        }
        drop(claims);
        self.notify_state_progress();
    }

    fn wait_for_write_lease(&self, entry: &PageEntry) -> Result<(), SyscallErr> {
        if crate::task::current_task().is_none() {
            return Err(SyscallErr::EAGAIN);
        }
        self.state_waiter_count.fetch_add(1, Ordering::AcqRel);
        #[cfg(feature = "perf_stats")]
        let _blocked_reason = crate::task::current_task()
            .map(|task| task.blocked_reason_scope(BlockedReason::PageCache));
        let result = match WaitQueue::wait_event_interruptible(&self.state_waiters, || {
            entry.write_lease_ready().then_some(0)
        }) {
            WaitResult::Ready(_) => Ok(()),
            WaitResult::Interrupted => Err(SyscallErr::ERESTART),
            WaitResult::TimedOut => unreachable!("page write-lease wait has no deadline"),
        };
        self.state_waiter_count.fetch_sub(1, Ordering::AcqRel);
        result
    }

    /// 绑定用于读写持久化存储的 `PageCacheBackend`。
    pub fn set_backend(&self, backend: Arc<dyn PageCacheBackend>) {
        *self.backend.lock() = Some(backend);
    }

    /// Serialize a filesystem durability/truncate operation with ordinary
    /// page-cache readers, writers, and writeback.  The callback must not
    /// enter user space or wait on a task while this gate is held.
    pub fn with_io_gate<F, T>(&self, operation: F) -> T
    where
        F: FnOnce() -> T,
    {
        let _gate = self.op_gate.write();
        operation()
    }

    /// 关联一个 `IndexNode`（`Weak` 引用，不阻止 inode 回收）。
    pub fn set_inode(&self, inode: Weak<dyn IndexNode>) {
        *self.inode.lock() = Some(inode);
    }

    /// 设置不可回收标志（用于 tmpfs/shmem，数据无持久化后端）
    pub fn set_unevictable(&self, val: bool) {
        self.unevictable.store(val, Ordering::Release);
    }

    /// 在 VMA 已绑定所属 AddressSpace 后建立 file rmap 条目。
    pub(crate) fn register_file_vma(&self, rmap: &Arc<FileVmaRmap>) {
        let id = Arc::as_ptr(rmap) as usize;
        self.i_mmap.lock().insert(id, Arc::downgrade(rmap));
        // Release 发布新索引；walker 的 Acquire 重验保证不会把摘除后的 VMA
        // 当成权威映射使用。
        self.i_mmap_seq.fetch_add(1, Ordering::Release);
    }

    /// 在 VMA 从 VmaSet 移除前摘除其 rmap 条目。
    pub(crate) fn unregister_file_vma(&self, vma_id: usize) {
        self.i_mmap.lock().remove(&vma_id);
        self.i_mmap_seq.fetch_add(1, Ordering::Release);
    }

    /// 对映射指定文件页的所有现存共享 VMA 执行写保护/清 dirty 或 truncate zap。
    ///
    /// 先在 i_mmap 锁内升级独立 rmap 的弱引用；快照在 VM 重验完成前保留该
    /// rmap 的强引用，防止 allocator 复用其地址。它不持有 VMA 本体，因此不会
    /// 破坏 `VmaSet` 的唯一所有权。每个 VM 修改在解锁后完成 TLB ack，最后再
    /// 复查注册表序号，保证 mmap/munmap 并发不会遗漏新旧 VMA。
    fn mkclean_page(&self, page_index: usize, unmap: bool) {
        loop {
            let seq = self.i_mmap_seq.load(Ordering::Acquire);
            let mut snapshots: Vec<FileVmaSnapshot> = Vec::new();
            {
                let index = self.i_mmap.lock();
                for weak in index.values() {
                    if let Some(rmap) = weak.upgrade() {
                        let snapshot = FileVmaRmap::snapshot(&rmap);
                        let first = snapshot.file_offset >> PAGE_SIZE_BITS;
                        let pages = snapshot.end.0.saturating_sub(snapshot.start.0);
                        if page_index >= first && page_index < first.saturating_add(pages) {
                            snapshots.push(snapshot);
                        }
                    }
                }
            }
            for snapshot in snapshots {
                if let Some(vm) = snapshot.owner.upgrade() {
                    vm.mkclean_file_page(&snapshot.rmap, page_index, unmap);
                }
            }
            if self.i_mmap_seq.load(Ordering::Acquire) == seq {
                return;
            }
        }
    }

    /// 返回当前绑定的 `PageCacheBackend`（克隆 `Arc`）。
    pub fn backend(&self) -> Option<Arc<dyn PageCacheBackend>> {
        self.backend.lock().clone()
    }

    /// 返回内部控制结构记录的页帧总数（含空洞，与 `self.entries.len()` 一致）。
    pub fn page_count(&self) -> usize {
        self.inner.lock().page_count()
    }

    /// 检查指定索引处的页面条目是否存在且处于 UpToDate 状态。
    pub fn contains_page(&self, page_index: usize) -> bool {
        let entries = self.entries.lock();
        page_index < entries.len() && entries[page_index].is_some()
    }

    /// 检查指定页面索引是否在脏页集合中（需要先持 `inner` 锁）。
    pub fn is_dirty(&self, page_index: usize) -> bool {
        self.entries
            .lock()
            .get(page_index)
            .and_then(Option::as_ref)
            .is_some_and(|entry| entry.test_flag(PG_DIRTY))
    }

    /// 返回当前脏页集合的条目数（全局脏页计数同步更新）。
    pub fn dirty_count(&self) -> usize {
        self.entries
            .lock()
            .iter()
            .filter(|entry| {
                entry
                    .as_ref()
                    .is_some_and(|entry| entry.test_flag(PG_DIRTY))
            })
            .count()
    }

    /// 遍历 `entries` 数组统计非 `None` 条目数（O(n) 扫描，仅供诊断）。
    pub fn cached_page_count(&self) -> usize {
        self.entries.lock().iter().filter(|e| e.is_some()).count()
    }

    /// Evict up to `target` clean pages using clock/second-chance sweep.
    /// Only UpToDate pages held exclusively by the cache (refcount==1) are evicted.
    /// Pages with PG_REFERENCED get a second chance: the bit is cleared and the
    /// page survives this round.
    ///
    /// # Locking
    ///
    /// 获取 `self.entries` 锁并扫描，然后获取 `self.inner` 锁清理内部元数据。
    /// 锁顺序：entries → inner（与 `get_or_create_entry` 一致）。
    /// 非重入：调用者不得持有 inode 内部锁。
    ///
    /// # Semantics
    ///
    /// 返回实际回收的页数。对于 `unevictable` 缓存（tmpfs/shmem）直接返回 0。
    pub fn evict_clean_pages_clock(&self, target: usize) -> usize {
        let _op = self.op_gate.write();
        // tmpfs/shmem pages must never be evicted — no persistent backend
        if self.unevictable.load(Ordering::Acquire) {
            return 0;
        }
        let mut entries = self.entries.lock();
        let len = entries.len();
        if len == 0 {
            return 0;
        }

        let mut hand = self.clock_hand.load(Ordering::Relaxed) % len;
        // Bound sweep to prevent runaway scans when the clock loops
        let max_scan = core::cmp::min(len * 2, target.saturating_mul(16).saturating_add(64));
        let mut evicted = 0usize;
        let mut scanned = 0usize;
        let mut removed_indices: alloc::vec::Vec<usize> = alloc::vec::Vec::new();

        while evicted < target && scanned < max_scan {
            let idx = hand;
            hand = (hand + 1) % len;
            scanned += 1;

            let Some(entry) = entries[idx].as_ref() else {
                continue;
            };

            crate::task::perf::record_clock_scanned(1);

            // Only evict clean, non-busy pages
            if entry.state() != PageState::UpToDate {
                continue;
            }
            // Page must be held only by the cache (no mmap, no active user)
            if Arc::strong_count(entry) != 1 {
                continue;
            }
            // Frame must not be shared (mmap holds its own Arc<FrameTracker>)
            if Arc::strong_count(&entry.page) != 1 {
                continue;
            }

            let flags = entry.flags.load(Ordering::Acquire);
            if flags & PG_REFERENCED != 0 {
                // Second chance: clear referenced, skip this round
                entry.flags.fetch_and(!PG_REFERENCED, Ordering::AcqRel);
                crate::task::perf::record_clock_second_chance(1);
                continue;
            }

            // Safe to evict
            entry.discard_filemap_readahead();
            entries[idx] = None;
            removed_indices.push(idx);
            crate::task::perf::record_clock_evicted(1);
            crate::task::perf::record_reclaim_pages_freed(1);
            evicted += 1;
        }

        self.clock_hand.store(hand, Ordering::Relaxed);

        // Clean up inner page tracking sets
        if !removed_indices.is_empty() {
            let mut inner = self.inner.lock();
            for idx in &removed_indices {
                inner.pages.remove(idx);
            }
        }

        // Shrink trailing Nones
        while entries.last().map_or(false, |e| e.is_none()) {
            entries.pop();
        }

        evicted
    }

    /// 回收干净页面（clock/second-chance sweep）
    pub fn shrink_clean_pages(&self, max_to_free: usize) -> usize {
        self.evict_clean_pages_clock(max_to_free)
    }

    /// 返回 entries 元数据: (len, capacity, live, holes)
    pub fn entries_stats(&self) -> (usize, usize, usize, usize) {
        let entries = self.entries.lock();
        let len = entries.len();
        let cap = entries.capacity();
        let live = entries.iter().filter(|e| e.is_some()).count();
        let holes = len.saturating_sub(live);
        (len, cap, live, holes)
    }

    /// 返回指定索引处页面条目的当前 `PageState`；若索引越界或条目为 `None` 则返回 `None`。
    pub fn state_of(&self, page_index: usize) -> Option<PageState> {
        let entries = self.entries.lock();
        if page_index >= entries.len() {
            return None;
        }
        entries[page_index].as_ref().map(|e| e.state())
    }

    /// 在所属 VM 锁内记录一个 file-backed PTE 已安装。
    pub fn map_page(&self, page_index: usize) {
        let entry = self
            .entries
            .lock()
            .get(page_index)
            .and_then(Option::as_ref)
            .cloned();
        if let Some(entry) = entry {
            entry.map_count.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// 在所属 VM 锁内记录一个 file-backed PTE 已撤销。
    pub fn unmap_page(&self, page_index: usize) {
        let entry = self
            .entries
            .lock()
            .get(page_index)
            .and_then(Option::as_ref)
            .cloned();
        if let Some(entry) = entry {
            let _ = entry
                .map_count
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                    count.checked_sub(1)
                });
        }
    }

    /// 返回当前由用户 PTE 引用的数量，仅供截断/回收快速判定。
    pub fn map_count(&self, page_index: usize) -> usize {
        self.entries
            .lock()
            .get(page_index)
            .and_then(Option::as_ref)
            .map(|entry| entry.map_count.load(Ordering::Acquire))
            .unwrap_or(0)
    }

    /// 获取所有脏页索引的快照
    pub fn dirty_pages_snapshot(&self) -> alloc::vec::Vec<usize> {
        self.find_dirty_pages()
    }

    /// Scan the contiguous entry directory and derive dirty pages from the
    /// atomic page flags.  Dirty membership is intentionally not duplicated
    /// in a second mutable index: writeback ownership is claimed by the
    /// Dirty→Writeback CAS, so a stale index could otherwise resurrect or
    /// lose a page during concurrent redirty.
    fn find_dirty_pages(&self) -> Vec<usize> {
        self.entries
            .lock()
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                entry
                    .as_ref()
                    .filter(|entry| entry.test_flag(PG_DIRTY))
                    .map(|_| index)
            })
            .collect()
    }

    fn has_transient_pages(&self, start: usize, end: usize) -> bool {
        let entries = self.entries.lock();
        if entries.is_empty() || start >= entries.len() {
            return false;
        }
        let end = end.min(entries.len().saturating_sub(1));
        if start > end {
            return false;
        }
        entries[start..=end].iter().any(|entry| {
            entry.as_ref().is_some_and(|entry| {
                matches!(entry.state(), PageState::Loading | PageState::Writeback)
            })
        })
    }

    // ── 页面获取 ────────────────────────────────────────────────────

    /// 获取或创建一个页面条目
    /// `populate`: 是否从后端读取数据填充
    /// `old_file_size`: 旧文件大小，用于计算初始 valid_mask
    ///   - 页面超出旧 EOF → valid_mask=VALID_ALL，不 populate
    ///   - 页面跨越 EOF → 超出部分 valid_mask 标记，populate 后 OR 入
    ///   - 页面完全在文件中 → valid_mask=0，populate 从后端加载
    fn get_or_create_entry(
        &self,
        page_index: usize,
        populate: bool,
        old_file_size: Option<usize>,
    ) -> Result<Arc<PageEntry>, SyscallErr> {
        let init_mask = old_file_size.map_or(0, |s| initial_valid_mask(page_index, s));
        let beyond_eof = init_mask == VALID_ALL;

        let t_lock = perf::perf_time_now();
        let mut had_io_miss = false;
        {
            let entries = self.entries.lock();
            if let Some(entry) = entries.get(page_index).and_then(Option::as_ref) {
                let elapsed = perf::perf_time_now().wrapping_sub(t_lock);
                perf::record_pc_lock_hold(elapsed, false);
                entry.mark_referenced();
                return Ok(entry.clone());
            }
        }

        // 分配新帧（frame_alloc 返回零填充页）
        let _t_falloc = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
        let frame = frame_alloc().ok_or(SyscallErr::ENOMEM)?;
        perf::record_pc_falloc_cycles(
            perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(_t_falloc),
        );

        let entry = if populate && !beyond_eof {
            // 正常路径：从后端读取数据
            perf::record_pc_miss();
            had_io_miss = true;
            let entry = Arc::new(PageEntry::new(frame, PageState::UpToDate));
            if let Some(backend) = self.backend() {
                entry.with_bytes_mut(|buf| backend.read_page(page_index, buf))?;
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

        // Backend I/O 与 frame byte 初始化已经完成；entries/inner 在此之前
        // 从未跨越 data 锁。并发创建者获胜时采用其条目，丢弃本地候选即可。
        let entry_clone = {
            let mut entries = self.entries.lock();
            while entries.len() <= page_index {
                entries.push(None);
            }
            if let Some(existing) = entries[page_index].as_ref() {
                existing.clone()
            } else {
                entries[page_index] = Some(entry.clone());
                let mut inner = self.inner.lock();
                inner.pages.insert(page_index);
                entry
            }
        };

        // Clock eviction: mark page as recently referenced
        entry_clone.mark_referenced();

        let elapsed = perf::perf_time_now().wrapping_sub(t_lock);
        perf::record_pc_lock_hold(elapsed, had_io_miss);

        Ok(entry_clone)
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
    /// 内部获取 `self.entries` → `self.inner`（按序）。调用者不得持有 inode 锁。
    ///
    /// # Errors
    ///
    /// 内存分配失败返回 `ENOMEM`；后端读取失败透传后端错误。
    fn get_page_for_read(&self, page_index: usize) -> Result<Arc<PageEntry>, SyscallErr> {
        // 读取路径：始终 populate（old_file_size=None → 全量从后端加载），后续 ensure_fully_valid 补齐空洞
        let entry = self.get_or_create_entry(page_index, true, None)?;
        entry.consume_filemap_readahead();
        Ok(entry)
    }

    /// 获取页面用于写入，可选择是否从后端 populate。
    /// `old_file_size`：旧文件大小。对于 page_index * PAGE_SIZE >= old_file_size
    /// 的页面（完全超出旧 EOF），跳过 backend read_page 以减少 I/O，
    /// 帧内存保持零填充，初始 valid_mask=VALID_ALL。
    /// `full_overwrite`：该页是否被完全覆盖写入（可跳过 populate）。
    /// - `None` + `false`: 当前 populate 逻辑（部分写入时从后端读取）
    /// - `Some(size)` + `false`: 页面超出 EOF 时，zero-fill + valid_mask=VALID_ALL
    /// - `true`: 整页覆写，跳过 populate
    fn get_page_for_write_populate(
        &self,
        page_index: usize,
        old_file_size: Option<usize>,
        full_overwrite: bool,
    ) -> Result<Arc<PageEntry>, SyscallErr> {
        let beyond_eof = old_file_size
            .map(|s| page_index * PAGE_SIZE >= s)
            .unwrap_or(false);
        let populate = !full_overwrite && !beyond_eof;
        let entry = self.get_or_create_entry(page_index, populate, old_file_size)?;
        entry.consume_filemap_readahead();
        Ok(entry)
    }

    /// 在页面字节已经复制完成后发布 Dirty；Writeback 并发期间只置
    /// PG_REDIRTIED，完成者会把页面恢复为可重试的 Dirty。
    fn mark_dirty_after_copy(&self, _page_index: usize, entry: &PageEntry) {
        loop {
            let st = entry.state();
            match st {
                PageState::Dirty => break,
                PageState::UpToDate => {
                    match entry.compare_exchange_state(PageState::UpToDate, PageState::Dirty) {
                        Ok(_) => {
                            GLOBAL_DIRTY_PAGES.fetch_add(1, Ordering::Relaxed);
                            break;
                        }
                        Err(_) => continue,
                    }
                }
                PageState::Writeback => {
                    entry.set_flag(PG_REDIRTIED);
                    break;
                }
                _ => break,
            }
        }
        if let Some(backend) = self.backend() {
            backend.on_page_dirty();
        }
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
    /// 内部获取 `self.entries` → `self.inner`（按序）。
    pub fn frame_for_read(&self, page_index: usize) -> Result<Arc<FrameTracker>, SyscallErr> {
        let _op = self.op_gate.read();
        let entry = self.get_or_create_entry(page_index, true, None)?;
        // 保证部分写入的页面在映射前所有 segment 均有效
        self.ensure_fully_valid(page_index)?;
        let state = entry.state();
        match state {
            PageState::UpToDate | PageState::Dirty => {
                entry.consume_filemap_readahead();
                Ok(entry.page.clone())
            }
            PageState::Error => Err(SyscallErr::EIO),
            PageState::Loading | PageState::Writeback => Err(SyscallErr::EAGAIN),
        }
    }

    /// 返回文件映射读页，并在 `PageEntry.data` 写锁内按权威 EOF 清零末页尾部。
    ///
    /// 后端可能按磁盘块读取并带回 EOF 之后的旧字节；因此不能把 tail-zero 留给
    /// filemap 对 raw frame 的锁外写入。这里与 writeback/普通 PageCache 写入共用
    /// 同一把 data 锁，且只在 entries 锁已经释放后取得它。
    pub fn frame_for_filemap_read(
        &self,
        page_index: usize,
        authoritative_eof: usize,
    ) -> Result<Arc<FrameTracker>, SyscallErr> {
        let _op = self.op_gate.read();
        let entry = self.get_or_create_entry(page_index, true, None)?;
        self.ensure_fully_valid(page_index)?;
        match entry.state() {
            PageState::UpToDate | PageState::Dirty => {
                entry.consume_filemap_readahead();
                let page_start = page_index.saturating_mul(PAGE_SIZE);
                if authoritative_eof < page_start.saturating_add(PAGE_SIZE) {
                    let tail_start = authoritative_eof.saturating_sub(page_start).min(PAGE_SIZE);
                    entry.with_bytes_mut(|bytes| bytes[tail_start..].fill(0));
                }
                Ok(entry.page.clone())
            }
            PageState::Error => Err(SyscallErr::EIO),
            PageState::Loading | PageState::Writeback => Err(SyscallErr::EAGAIN),
        }
    }

    /// VM-lock-safe filemap read admission. This path never starts backend
    /// I/O: a missing, partial or transient page becomes a Retry token whose
    /// `wait()` method runs after the address-space lock is released.
    pub(crate) fn try_frame_for_filemap_read(
        self: &Arc<Self>,
        page_index: usize,
        authoritative_eof: usize,
    ) -> Result<Arc<FrameTracker>, PageCacheFault> {
        self.try_frame_for_filemap_read_ahead(page_index, authoritative_eof, 1)
    }

    /// VM-lock-safe filemap read admission with a bounded forward window.
    /// The current page is always first; speculative pages are only loaded by
    /// the Retry token after the address-space lock has been released.
    pub(crate) fn try_frame_for_filemap_read_ahead(
        self: &Arc<Self>,
        page_index: usize,
        authoritative_eof: usize,
        fault_around_pages: usize,
    ) -> Result<Arc<FrameTracker>, PageCacheFault> {
        let fault_around_pages = fault_around_pages.clamp(1, MAX_DEMAND_READ_PAGES);
        let retry = || {
            PageCacheFault::Retry(Arc::new(PageCacheFaultWait {
                cache: self.clone(),
                page_index,
                fault_around_pages,
            }))
        };
        let Some(_op) = self.op_gate.try_read() else {
            return Err(retry());
        };
        let entry = self
            .entries
            .lock()
            .get(page_index)
            .and_then(Option::as_ref)
            .cloned();
        let Some(entry) = entry else {
            return Err(retry());
        };
        if !entry.is_fully_valid()
            || matches!(entry.state(), PageState::Loading | PageState::Writeback)
        {
            return Err(retry());
        }
        match entry.state() {
            PageState::UpToDate | PageState::Dirty => {
                entry.consume_filemap_readahead();
                let page_start = page_index.saturating_mul(PAGE_SIZE);
                if authoritative_eof < page_start.saturating_add(PAGE_SIZE) {
                    let tail_start = authoritative_eof.saturating_sub(page_start).min(PAGE_SIZE);
                    entry.with_bytes_mut(|bytes| bytes[tail_start..].fill(0));
                }
                Ok(entry.page.clone())
            }
            PageState::Error => Err(PageCacheFault::Error(SyscallErr::EIO)),
            PageState::Loading | PageState::Writeback => Err(retry()),
        }
    }

    /// Return an already-resident filemap page without creating an entry or
    /// starting backend I/O.
    ///
    /// This is the speculative half of PTE fault-around. The demand page uses
    /// [`Self::try_frame_for_filemap_read_ahead`] so a miss can return a
    /// lock-outside retry token; adjacent pages use this method and simply stop
    /// at the first cache miss/transient state. In particular, this method must
    /// remain safe while the caller holds its address-space lock.
    pub(crate) fn try_resident_frame_for_filemap_map(
        &self,
        page_index: usize,
        authoritative_eof: usize,
    ) -> Result<Option<Arc<FrameTracker>>, SyscallErr> {
        let Some(_op) = self.op_gate.try_read() else {
            return Ok(None);
        };
        let entry = self
            .entries
            .lock()
            .get(page_index)
            .and_then(Option::as_ref)
            .cloned();
        let Some(entry) = entry else {
            return Ok(None);
        };
        if !entry.is_fully_valid()
            || matches!(entry.state(), PageState::Loading | PageState::Writeback)
        {
            return Ok(None);
        }
        match entry.state() {
            PageState::UpToDate | PageState::Dirty => {
                // Mapping an already resident adjacent page is speculative;
                // do not classify it as a useful backend readahead hit.
                crate::task::perf::record_filemap_pte_around_speculative_reuse();
                let page_start = page_index.saturating_mul(PAGE_SIZE);
                if authoritative_eof < page_start.saturating_add(PAGE_SIZE) {
                    let tail_start = authoritative_eof.saturating_sub(page_start).min(PAGE_SIZE);
                    entry.with_bytes_mut(|bytes| bytes[tail_start..].fill(0));
                }
                Ok(Some(entry.page.clone()))
            }
            PageState::Error => Err(SyscallErr::EIO),
            PageState::Loading | PageState::Writeback => Ok(None),
        }
    }

    /// 在 source `PageEntry.data` 读锁内将文件页复制到私有目标帧。
    ///
    /// `dst` 只由尚未发布到用户页表的新匿名页持有；EOF 后的字节也在同一快照中
    /// 清零，避免 private COW copy 逃出 page-cache 数据锁。
    pub fn copy_page_for_private(
        &self,
        page_index: usize,
        dst: &mut [u8],
        authoritative_eof: usize,
    ) -> Result<(), SyscallErr> {
        if dst.len() != PAGE_SIZE {
            return Err(SyscallErr::EINVAL);
        }
        let _op = self.op_gate.read();
        let entry = self.get_or_create_entry(page_index, true, None)?;
        self.ensure_fully_valid(page_index)?;
        match entry.state() {
            PageState::UpToDate | PageState::Dirty => {
                entry.consume_filemap_readahead();
                entry.with_bytes(|src| dst.copy_from_slice(src));
                let page_start = page_index.saturating_mul(PAGE_SIZE);
                if authoritative_eof < page_start.saturating_add(PAGE_SIZE) {
                    let tail_start = authoritative_eof.saturating_sub(page_start).min(PAGE_SIZE);
                    dst[tail_start..].fill(0);
                }
                Ok(())
            }
            PageState::Error => Err(SyscallErr::EIO),
            PageState::Loading | PageState::Writeback => Err(SyscallErr::EAGAIN),
        }
    }

    /// VM-lock-safe private-fault copy. Like the shared read admission above,
    /// it copies only an already-resident page and delegates all loading to a
    /// lock-outside Retry token.
    pub(crate) fn try_copy_page_for_private(
        self: &Arc<Self>,
        page_index: usize,
        dst: &mut [u8],
        authoritative_eof: usize,
    ) -> Result<(), PageCacheFault> {
        if dst.len() != PAGE_SIZE {
            return Err(PageCacheFault::Error(SyscallErr::EINVAL));
        }
        let retry = || {
            PageCacheFault::Retry(Arc::new(PageCacheFaultWait {
                cache: self.clone(),
                page_index,
                fault_around_pages: 1,
            }))
        };
        let Some(_op) = self.op_gate.try_read() else {
            return Err(retry());
        };
        let entry = self
            .entries
            .lock()
            .get(page_index)
            .and_then(Option::as_ref)
            .cloned();
        let Some(entry) = entry else {
            return Err(retry());
        };
        if !entry.is_fully_valid()
            || matches!(entry.state(), PageState::Loading | PageState::Writeback)
        {
            return Err(retry());
        }
        match entry.state() {
            PageState::UpToDate | PageState::Dirty => {
                entry.consume_filemap_readahead();
                entry.with_bytes(|src| dst.copy_from_slice(src));
                let page_start = page_index.saturating_mul(PAGE_SIZE);
                if authoritative_eof < page_start.saturating_add(PAGE_SIZE) {
                    let tail_start = authoritative_eof.saturating_sub(page_start).min(PAGE_SIZE);
                    dst[tail_start..].fill(0);
                }
                Ok(())
            }
            PageState::Error => Err(PageCacheFault::Error(SyscallErr::EIO)),
            PageState::Loading | PageState::Writeback => Err(retry()),
        }
    }

    /// Copy a byte range from an already-resident page without starting I/O.
    ///
    /// ELF demand paging uses this while the address-space write lock is held.
    /// Missing or transient pages therefore return a Retry token; its `wait()`
    /// performs the backend read after the VM lock has been released.
    pub(crate) fn try_copy_resident_range(
        self: &Arc<Self>,
        page_index: usize,
        page_offset: usize,
        dst: &mut [u8],
    ) -> Result<(), PageCacheFault> {
        let range_end = page_offset
            .checked_add(dst.len())
            .ok_or_else(|| PageCacheFault::Error(SyscallErr::EINVAL))?;
        if range_end > PAGE_SIZE {
            return Err(PageCacheFault::Error(SyscallErr::EINVAL));
        }
        let retry = || {
            PageCacheFault::Retry(Arc::new(PageCacheFaultWait {
                cache: self.clone(),
                page_index,
                fault_around_pages: 1,
            }))
        };
        let Some(_op) = self.op_gate.try_read() else {
            return Err(retry());
        };
        let entry = self
            .entries
            .lock()
            .get(page_index)
            .and_then(Option::as_ref)
            .cloned();
        let Some(entry) = entry else {
            return Err(retry());
        };
        if !entry.is_fully_valid()
            || matches!(entry.state(), PageState::Loading | PageState::Writeback)
        {
            return Err(retry());
        }
        match entry.state() {
            PageState::UpToDate | PageState::Dirty => {
                entry.consume_filemap_readahead();
                entry.with_bytes(|src| dst.copy_from_slice(&src[page_offset..range_end]));
                Ok(())
            }
            PageState::Error => Err(PageCacheFault::Error(SyscallErr::EIO)),
            PageState::Loading | PageState::Writeback => Err(retry()),
        }
    }

    /// 获取页帧用于文件映射写（如 `MAP_SHARED` file-backed page fault）。
    ///
    /// # Semantics
    ///
    /// 返回 PageCache 中的 `Arc<FrameTracker>`，自动通过 CAS 标记脏页。
    /// 写回期间被再次标记脏时通过 `PG_REDIRTIED` 标志保证数据不丢失。
    ///
    /// # Errors
    ///
    /// - `EIO`：页面状态为 `Error`
    /// - `EAGAIN`：页面状态为 `Loading` 或 `Writeback`
    ///
    /// # Locking
    ///
    /// 内部获取 `self.entries` → `self.inner`（按序）。修改全局脏页计数。
    pub fn frame_for_write(&self, page_index: usize) -> Result<Arc<FrameTracker>, SyscallErr> {
        let _op = self.op_gate.read();
        let entry = self.get_or_create_entry(page_index, true, None)?;
        // 保证部分写入的页面在映射前所有 segment 均有效
        self.ensure_fully_valid(page_index)?;
        let state = entry.state();
        if state != PageState::UpToDate && state != PageState::Dirty {
            return match state {
                PageState::Error => Err(SyscallErr::EIO),
                PageState::Loading | PageState::Writeback => Err(SyscallErr::EAGAIN),
                _ => Err(SyscallErr::EIO),
            };
        }
        entry.consume_filemap_readahead();
        self.mark_dirty_after_copy(page_index, &entry);
        Ok(entry.page.clone())
    }

    /// file shared-write fault 的 VM 锁内快路径。它只做 try-read 和既有 entry
    /// 检查；任何可能加载/等待的工作都包装为 RetryWait 交给外层锁外执行。
    pub(crate) fn try_frame_for_write(
        self: &Arc<Self>,
        page_index: usize,
    ) -> Result<Arc<FrameTracker>, PageCacheFault> {
        let Some(_op) = self.op_gate.try_read() else {
            return Err(PageCacheFault::Retry(Arc::new(PageCacheFaultWait {
                cache: self.clone(),
                page_index,
                fault_around_pages: 1,
            })));
        };
        let entry = self
            .entries
            .lock()
            .get(page_index)
            .and_then(Option::as_ref)
            .cloned();
        let Some(entry) = entry else {
            return Err(PageCacheFault::Retry(Arc::new(PageCacheFaultWait {
                cache: self.clone(),
                page_index,
                fault_around_pages: 1,
            })));
        };
        if !entry.is_fully_valid()
            || entry.state() == PageState::Loading
            || entry.state() == PageState::Writeback
        {
            return Err(PageCacheFault::Retry(Arc::new(PageCacheFaultWait {
                cache: self.clone(),
                page_index,
                fault_around_pages: 1,
            })));
        }
        match entry.state() {
            PageState::UpToDate | PageState::Dirty => {
                entry.consume_filemap_readahead();
                self.mark_dirty_after_copy(page_index, &entry);
                Ok(entry.page.clone())
            }
            PageState::Error => Err(PageCacheFault::Error(SyscallErr::EIO)),
            PageState::Loading | PageState::Writeback => unreachable!(),
        }
    }

    /// 确保页面所有 segment 均已有效（读取缺失的 segment 并合并）。
    /// 在读取或回写前调用，以保证部分写入的页面（超出 EOF 零填充页面）
    /// 在涉及未写入 segment 时不返回错误数据。
    /// 快速路径：is_fully_valid() == true 时直接返回。
    fn ensure_fully_valid(&self, page_index: usize) -> Result<(), SyscallErr> {
        let entry = {
            let entries = self.entries.lock();
            if page_index >= entries.len() {
                return Ok(());
            }
            match &entries[page_index] {
                Some(e) => e.clone(),
                None => return Ok(()),
            }
        };

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

            entry.with_bytes_mut(|dst| {
                for seg in 0..VALID_SEG_COUNT {
                    if (valid_before >> seg) & 1 == 0 {
                        let start = seg << VALID_SEG_SHIFT;
                        let end = start + (1 << VALID_SEG_SHIFT);
                        dst[start..end].copy_from_slice(&temp[start..end]);
                    }
                }
            });
        }

        entry.mark_fully_valid();
        Ok(())
    }

    // ── Batch read helpers ───────────────────────────────────────────

    /// Scan [start_page..=end_page] under ONE entries lock.
    /// HIT: mark_referenced, check is_fully_valid(), push ReadCopy.
    /// MISS: record in miss_runs (coalesced into contiguous runs).
    /// PARTIAL: if entry exists but !is_fully_valid(), push to needs_valid_fill.
    fn lookup_read_range_fast(
        &self,
        offset: usize,
        buf_len: usize,
        start_page: usize,
        end_page: usize,
    ) -> ReadPlan {
        let mut plan = ReadPlan {
            copies: Vec::new(),
            miss_runs: Vec::new(),
            needs_valid_fill: BTreeSet::new(),
        };
        let entries = self.entries.lock();

        for page_index in start_page..=end_page {
            let page_start = page_index * PAGE_SIZE;
            let read_start = offset.max(page_start);
            let read_end = (offset + buf_len).min(page_start + PAGE_SIZE);
            if read_end <= read_start {
                continue;
            }
            let sub_len = read_end - read_start;

            let dst_offset = read_start - offset;
            let page_offset = read_start - page_start;

            // Check entry existence
            if page_index < entries.len() {
                if let Some(entry) = &entries[page_index] {
                    entry.mark_referenced();
                    if entry.is_fully_valid() {
                        plan.copies.push(ReadCopy {
                            entry: entry.clone(),
                            dst_offset,
                            page_offset,
                            len: sub_len,
                        });
                        continue;
                    } else {
                        plan.needs_valid_fill.insert(page_index);
                        continue;
                    }
                }
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
        plan
    }

    /// Fill contiguous missing page runs using bounded contiguous backend reads.
    /// Uses publish-after-I/O pattern: create UpToDate entries, fill via I/O, then publish.
    fn fill_miss_runs(&self, runs: &[MissRun]) -> Result<(), SyscallErr> {
        // Backend I/O runs outside op_gate so a backend can re-enter this
        // cache.  The generation check before publication prevents stale
        // pages from being published after truncate/invalidate.
        let generation = self.mutation_generation.load(Ordering::Acquire);
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

            // 2. Read at most 128 KiB at a time. The staging buffer is owned by
            // this scope, so backend I/O remains outside PageEntry locks and
            // no raw page slices escape into a re-entrant backend callback.
            let mut offset = 0;
            while offset < run.count {
                let chunk_pages = (run.count - offset).min(MAX_DEMAND_READ_PAGES);
                let first_page = run.start_page + offset;
                let readable_pages = if first_page < backend_npages {
                    chunk_pages.min(backend_npages - first_page)
                } else {
                    0
                };
                let mut staging = Vec::new();
                staging
                    .try_reserve_exact(chunk_pages * PAGE_SIZE)
                    .map_err(|_| SyscallErr::ENOMEM)?;
                staging.resize(chunk_pages * PAGE_SIZE, 0);
                if readable_pages != 0 {
                    backend
                        .read_contiguous(first_page, &mut staging[..readable_pages * PAGE_SIZE])?;
                }
                for index in 0..chunk_pages {
                    let entry = &new_entries[offset + index].1;
                    let start = index * PAGE_SIZE;
                    entry.with_bytes_mut(|buf| {
                        buf.copy_from_slice(&staging[start..start + PAGE_SIZE]);
                    });
                    entry.valid_mask.store(VALID_ALL, Ordering::Release);
                    perf::record_pc_miss();
                }
                offset += chunk_pages;
            }

            if generation != self.mutation_generation.load(Ordering::Acquire) {
                return Err(SyscallErr::EAGAIN);
            }

            // 3. Publish: insert into entries (only if slot still empty)
            {
                let mut entries = self.entries.lock();
                let mut inner = self.inner.lock();
                for (page_index, entry) in new_entries {
                    while entries.len() <= page_index {
                        entries.push(None);
                    }
                    if entries[page_index].is_none() {
                        entries[page_index] = Some(entry.clone());
                        inner.pages.insert(page_index);
                    }
                }
            }
        }
        Ok(())
    }

    // ── 读取 ─────────────────────────────────────────────────────────

    /// 从指定偏移量读取数据
    /// 两阶段读取：持锁收集拷贝项 → 解锁拷贝数据
    pub(crate) fn read_kernel(&self, offset: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        let _t0 = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
        let miss_before = perf::PC_READ_MISS.load(core::sync::atomic::Ordering::Relaxed);
        if buf.is_empty() {
            let elapsed = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(_t0);
            perf::record_pc_read(0, elapsed, elapsed, 0);
            return Ok(0);
        }

        let start_page = offset >> PAGE_SIZE_BITS;
        let end_page = (offset + buf.len() - 1) >> PAGE_SIZE_BITS;

        // Single-page fast path: bypass Vec<CopyItem> construction
        if start_page == end_page {
            let _op = self.op_gate.read();
            let page_start = start_page << PAGE_SIZE_BITS;
            let page_offset = offset - page_start;
            let sub_len = buf.len().min(PAGE_SIZE - page_offset);
            let _t_lookup = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
            let entry = self.get_page_for_read(start_page)?;
            self.ensure_fully_valid(start_page)?;
            let had_miss =
                perf::PC_READ_MISS.load(core::sync::atomic::Ordering::Relaxed) > miss_before;
            let lookup_cycles =
                perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(_t_lookup);
            perf::record_pc_lookup_cycles(lookup_cycles);
            let _t_copy = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
            entry.with_bytes(|src| {
                buf[..sub_len].copy_from_slice(&src[page_offset..page_offset + sub_len]);
            });
            let copy_cycles =
                perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(_t_copy);
            perf::record_pc_copy_cycles(copy_cycles);
            let total_cycles =
                perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(_t0);
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
            let op = self.op_gate.read();
            let _t_lookup = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
            let plan = self.lookup_read_range_fast(offset, total_len, start_page, end_page);
            let lookup_cycles =
                perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(_t_lookup);
            perf::record_pc_lookup_cycles(lookup_cycles);

            // Fast path: all pages cached and fully valid
            if plan.miss_runs.is_empty() && plan.needs_valid_fill.is_empty() {
                let _t_copy = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
                for item in &plan.copies {
                    item.entry.with_bytes(|src| {
                        buf[item.dst_offset..item.dst_offset + item.len]
                            .copy_from_slice(&src[item.page_offset..item.page_offset + item.len]);
                    });
                }
                let copy_cycles =
                    perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(_t_copy);
                perf::record_pc_copy_cycles(copy_cycles);

                let had_miss =
                    perf::PC_READ_MISS.load(core::sync::atomic::Ordering::Relaxed) > miss_before;
                let total_cycles =
                    perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(_t0);
                let pages = end_page - start_page + 1;
                if had_miss {
                    perf::record_pc_read(pages, total_cycles, 0, total_cycles);
                } else {
                    perf::record_pc_read(pages, total_cycles, total_cycles, 0);
                }
                return Ok(total_len);
            }

            // 缺页的后端 I/O 完成后必须在 op_gate.write 下发布，不能在仍持有
            // 普通读操作锁时尝试升级。
            drop(op);

            // If we already retried, fall through to slow per-page path
            if retried {
                let _op = self.op_gate.read();
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
                    let src_start = item.page_offset;
                    item.entry.with_bytes(|src| {
                        buf[dst_offset..dst_offset + item.sub_len]
                            .copy_from_slice(&src[src_start..src_start + item.sub_len]);
                    });
                    dst_offset += item.sub_len;
                }
                let had_miss =
                    perf::PC_READ_MISS.load(core::sync::atomic::Ordering::Relaxed) > miss_before;
                let total_cycles =
                    perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(_t0);
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
                let _op = self.op_gate.read();
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
    pub(crate) fn write_kernel(
        &self,
        offset: usize,
        src: &[u8],
        old_size: usize,
    ) -> Result<usize, SyscallErr> {
        self.write_with_after_copy(offset, src, Some(old_size), |_| {})
    }

    /// 从指定偏移量写入数据。
    ///
    /// `old_file_size`: 旧文件大小，用于判断页面是否超出 EOF 以跳过不必要的后端读取。
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
    /// `after_copy` 仅在成功写入非空缓冲区后调用一次，且在脏页节流之前。
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

    /// Test-only seam which runs after page populate/lease acquisition but
    /// before payload bytes are copied. 失败时释放已获取的写租约。
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

    /// 写入口的统一外壳：持 `op_gate.read()` 调用 `write_kernel_body`，租约竞争时
    /// 通过 WaitQueue 睡眠重试（不自旋）。`before_copy` 在页面 populate/租约获取
    /// 之后、字节复制之前执行；`after_copy` 在所有租约 commit 之后执行。
    ///
    /// 两个回调都只在对应的成功路径上触发一次；Busy 重试发生在回调触发之前，
    /// 因此用 `Option` 持有 `FnOnce` 不会跨重试被重复消费。
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
        let mut before_copy = Some(before_copy);
        let mut after_copy = Some(after_copy);
        loop {
            let op = self.op_gate.read();
            match self.write_kernel_body(offset, buf, old_file_size, &mut before_copy) {
                Ok(written) => {
                    drop(op);
                    if written != 0 {
                        self.notify_state_progress();
                    }
                    if let Some(cb) = after_copy.take() {
                        cb(written);
                    }
                    balance_dirty_pages();
                    return Ok(written);
                }
                Err(WriteAttemptError::Busy(entry)) => {
                    drop(op);
                    // A multi-page attempt may have released earlier leases.
                    // Publish that progress before sleeping on the contended page.
                    self.notify_state_progress();
                    self.wait_for_write_lease(&entry)?;
                }
                Err(WriteAttemptError::Error(error)) => {
                    drop(op);
                    self.notify_state_progress();
                    return Err(error);
                }
            }
        }
    }

    /// `write_with_copy_callbacks` 的锁内实现；调用者已经持有 `op_gate.read()`。
    fn write_kernel_body<BeforeCopy>(
        &self,
        offset: usize,
        buf: &[u8],
        old_file_size: Option<usize>,
        before_copy: &mut Option<BeforeCopy>,
    ) -> Result<usize, WriteAttemptError>
    where
        BeforeCopy: FnOnce(usize) -> Result<(), SyscallErr>,
    {
        let _t0 = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
        if buf.is_empty() {
            perf::record_pc_write(
                0,
                false,
                perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(_t0),
            );
            return Ok(0);
        }

        let start_page = offset >> PAGE_SIZE_BITS;
        let end_page = (offset + buf.len() - 1) >> PAGE_SIZE_BITS;
        let mut stages = WriteStageCycles::default();

        // Single-page fast path: bypass Vec<CopyItem> construction
        if start_page == end_page {
            let page_start = start_page << PAGE_SIZE_BITS;
            let page_offset = offset - page_start;
            let sub_len = buf.len().min(PAGE_SIZE - page_offset);
            let full_page_overwrite = page_offset == 0 && sub_len == PAGE_SIZE;
            let lookup_start = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
            let entry =
                self.get_page_for_write_populate(start_page, old_file_size, full_page_overwrite)?;
            let lease_start = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
            let was_dirty = entry
                .try_lock_for_write()?
                .ok_or_else(|| WriteAttemptError::Busy(entry.clone()))?;
            stages.lookup = stages.lookup.wrapping_add(
                perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(lookup_start),
            );
            stages.lease = stages.lease.wrapping_add(
                perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(lease_start),
            );
            if let Some(cb) = before_copy.take() {
                if let Err(error) = cb(sub_len) {
                    entry.abort_write();
                    return Err(WriteAttemptError::Error(error));
                }
            }
            let copy_start = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
            entry.with_bytes_mut(|dst| {
                dst[page_offset..page_offset + sub_len].copy_from_slice(&buf[..sub_len]);
            });
            stages.copy = stages.copy.wrapping_add(
                perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(copy_start),
            );
            let became_full = entry.mark_valid_and_check_full(page_offset, sub_len);
            let commit_start = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
            if entry.commit_write() && !was_dirty {
                GLOBAL_DIRTY_PAGES.fetch_add(1, Ordering::Relaxed);
            }
            stages.commit = stages.commit.wrapping_add(
                perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(commit_start),
            );
            if became_full && !full_page_overwrite {
                perf::record_pc_write_eventually_full();
            }
            self.record_write_stages(&stages);
            perf::record_pc_write(
                1,
                full_page_overwrite,
                perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(_t0),
            );
            return Ok(sub_len);
        }

        struct CopyItem {
            entry: Arc<PageEntry>,
            page_offset: usize,
            sub_len: usize,
            full_page_overwrite: bool,
            was_dirty: bool,
        }

        let mut copies: Vec<CopyItem> = Vec::new();
        let mut total_written = 0usize;
        let mut pages = 0usize;
        let mut any_full_overwrite = false;

        let lookup_start = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
        let mut lease_cycles = 0usize;
        for page_index in start_page..=end_page {
            let page_start = page_index << PAGE_SIZE_BITS;
            let page_end = page_start + PAGE_SIZE;
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
            let entry = match self.get_page_for_write_populate(
                page_index,
                old_file_size,
                full_page_overwrite,
            ) {
                Ok(entry) => entry,
                Err(error) => {
                    for item in &copies {
                        item.entry.abort_write();
                    }
                    return Err(WriteAttemptError::Error(error));
                }
            };
            let lease_start = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
            let was_dirty = match entry.try_lock_for_write() {
                Ok(Some(was_dirty)) => was_dirty,
                Ok(None) => {
                    for item in &copies {
                        item.entry.abort_write();
                    }
                    return Err(WriteAttemptError::Busy(entry));
                }
                Err(error) => {
                    for item in &copies {
                        item.entry.abort_write();
                    }
                    return Err(WriteAttemptError::Error(error));
                }
            };
            lease_cycles = lease_cycles.wrapping_add(
                perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(lease_start),
            );
            copies.push(CopyItem {
                entry,
                page_offset,
                sub_len,
                full_page_overwrite,
                was_dirty,
            });
            total_written += sub_len;
        }
        stages.lookup = stages.lookup.wrapping_add(
            perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(lookup_start),
        );
        stages.lease = stages.lease.wrapping_add(lease_cycles);
        if let Some(cb) = before_copy.take() {
            if let Err(error) = cb(total_written) {
                for item in &copies {
                    item.entry.abort_write();
                }
                return Err(WriteAttemptError::Error(error));
            }
        }

        // Phase 2: 写入数据（无锁）
        let mut src_offset = 0;
        let copy_start = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
        let mut commit_cycles = 0usize;
        for (page_index, item) in (start_page..).zip(&copies) {
            let dst_start = item.page_offset;
            item.entry.with_bytes_mut(|dst| {
                dst[dst_start..dst_start + item.sub_len]
                    .copy_from_slice(&buf[src_offset..src_offset + item.sub_len]);
            });
            let became_full = item
                .entry
                .mark_valid_and_check_full(item.page_offset, item.sub_len);
            let commit_start = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
            if item.entry.commit_write() && !item.was_dirty {
                GLOBAL_DIRTY_PAGES.fetch_add(1, Ordering::Relaxed);
            }
            commit_cycles = commit_cycles.wrapping_add(
                perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(commit_start),
            );
            if became_full && !item.full_page_overwrite {
                perf::record_pc_write_eventually_full();
            }
            src_offset += item.sub_len;
        }
        stages.copy = stages.copy.wrapping_add(
            perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(copy_start),
        );
        stages.commit = stages.commit.wrapping_add(commit_cycles);
        self.record_write_stages(&stages);

        perf::record_pc_write(
            pages,
            any_full_overwrite,
            perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(_t0),
        );
        Ok(total_written)
    }

    /// 把 WriteStage 分阶段计数发布到现有 smp 侧 perf 计数器。
    /// lease 阶段并入 lookup（smp 的 lookup 已包含租约获取语义）。
    fn record_write_stages(&self, stages: &WriteStageCycles) {
        perf::record_pc_write_lookup(stages.lookup.wrapping_add(stages.lease));
        perf::record_pc_write_copy(stages.copy);
        perf::record_pc_write_commit(stages.commit);
    }

    // ── UserBuffer 读写 ──────────────────────────────────────────────

    /// 直接把 PageCache 页面复制到用户缓冲区，不经过 kernel bounce buffer。
    pub(crate) fn read_at_user(
        &self,
        offset: usize,
        len: usize,
        user: &mut crate::mm::UserBuffer,
    ) -> Result<usize, SyscallErr> {
        if len > user.len() {
            return Err(SyscallErr::EFAULT);
        }
        if len == 0 {
            return Ok(0);
        }
        let count = len;

        perf::record_pread_total_count();

        // Keep the user copy outside PageCache's operation gate, but avoid a
        // second kernel bounce buffer.  The page plan owns Arc<PageEntry>s,
        // so each page remains alive while UserBuffer validates/copies it.
        let end = offset.checked_add(count).ok_or(SyscallErr::EFBIG)?;
        let start_page = offset >> PAGE_SIZE_BITS;
        let end_page = (end - 1) >> PAGE_SIZE_BITS;
        if start_page == end_page {
            let _op = self.op_gate.read();
            let page_start = start_page << PAGE_SIZE_BITS;
            let page_offset = offset - page_start;
            let sub_len = count.min(PAGE_SIZE - page_offset);
            let lookup_start = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
            let entry = self.get_page_for_read(start_page)?;
            perf::record_pc_read_lookup_cycles(
                perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(lookup_start),
            );
            let valid_start = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
            self.ensure_fully_valid(start_page)?;
            perf::record_pc_read_valid_fill_cycles(
                perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(valid_start),
            );
            drop(_op);
            let uaccess_start = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
            let copied = entry
                .with_bytes(|src| {
                    user.write_from_at_nofault(0, &src[page_offset..page_offset + sub_len])
                })
                .map_err(|_| SyscallErr::EFAULT)?;
            perf::record_pread_uaccess(
                perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(uaccess_start),
            );
            perf::record_pc_read_copy_cycles(
                perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(uaccess_start),
            );
            return (copied == sub_len)
                .then_some({
                    perf::record_pc_read_user(1);
                    copied
                })
                .ok_or(SyscallErr::EFAULT);
        }

        let mut retried = false;
        loop {
            let op = self.op_gate.read();
            let lookup_start = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
            let plan = self.lookup_read_range_fast(offset, count, start_page, end_page);
            perf::record_pc_read_lookup_cycles(
                perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(lookup_start),
            );
            if plan.miss_runs.is_empty() && plan.needs_valid_fill.is_empty() {
                drop(op);
                let mut cursor = user.write_cursor();
                let mut copied_total = 0;
                let copy_start = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
                for item in &plan.copies {
                    let uaccess_start = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
                    let copied = item
                        .entry
                        .with_bytes(|src| {
                            cursor.try_write_from_nofault(
                                &src[item.page_offset..item.page_offset + item.len],
                            )
                        })
                        .map_err(|_| SyscallErr::EFAULT)?;
                    if copied != item.len {
                        return Err(SyscallErr::EFAULT);
                    }
                    perf::record_pread_uaccess(
                        perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO)
                            .wrapping_sub(uaccess_start),
                    );
                    copied_total += copied;
                }
                perf::record_pc_read_copy_cycles(
                    perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(copy_start),
                );
                perf::record_pc_read_user(end_page - start_page + 1);
                return Ok(copied_total);
            }
            drop(op);

            if retried {
                // A concurrent PageCache mutation can invalidate the batch
                // plan; resolve the remaining pages through the existing
                // single-page path without allocating a full bounce buffer.
                let mut cursor = user.write_cursor();
                let mut copied_total = 0;
                let copy_start = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
                for page_index in start_page..=end_page {
                    let page_start = page_index << PAGE_SIZE_BITS;
                    let read_start = offset.max(page_start);
                    let read_end = end.min(page_start.saturating_add(PAGE_SIZE));
                    if read_end <= read_start {
                        continue;
                    }
                    let entry = self.get_page_for_read(page_index)?;
                    let valid_start = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
                    self.ensure_fully_valid(page_index)?;
                    perf::record_pc_read_valid_fill_cycles(
                        perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO)
                            .wrapping_sub(valid_start),
                    );
                    let uaccess_start = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
                    let copied = entry
                        .with_bytes(|src| {
                            cursor.try_write_from_nofault(
                                &src[read_start - page_start..read_end - page_start],
                            )
                        })
                        .map_err(|_| SyscallErr::EFAULT)?;
                    let expected = read_end - read_start;
                    if copied != expected {
                        return Err(SyscallErr::EFAULT);
                    }
                    perf::record_pread_uaccess(
                        perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO)
                            .wrapping_sub(uaccess_start),
                    );
                    copied_total += copied;
                }
                perf::record_pc_read_copy_cycles(
                    perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(copy_start),
                );
                perf::record_pc_read_user(end_page - start_page + 1);
                return Ok(copied_total);
            }

            if !plan.needs_valid_fill.is_empty() {
                let _op = self.op_gate.read();
                let valid_start = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
                for page_index in plan.needs_valid_fill {
                    self.ensure_fully_valid(page_index)?;
                }
                perf::record_pc_read_valid_fill_cycles(
                    perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO)
                        .wrapping_sub(valid_start),
                );
            }
            if !plan.miss_runs.is_empty() {
                let miss_start = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
                self.fill_miss_runs(&plan.miss_runs)?;
                perf::record_pc_read_miss_fill_cycles(
                    perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(miss_start),
                );
            }
            retried = true;
        }
    }

    /// 直接从用户缓冲区复制到 PageCache 页面，不分配临时 kernel buffer。
    ///
    /// `UserBuffer` 在 syscall 入口已经完成 fault-in；这里使用 no-fault
    /// copy，避免在 PageCache 的操作门和页面写锁内触发缺页处理。
    pub(crate) fn write_at_user(
        &self,
        offset: usize,
        len: usize,
        user: &crate::mm::UserBuffer,
        old_size: usize,
    ) -> Result<usize, SyscallErr> {
        // Reject the complete operation before acquiring any page write lease.
        // Silently truncating to the source descriptor can otherwise dirty a
        // prefix and report success for an invalid direct-I/O request.
        if len > user.len() {
            return Err(SyscallErr::EFAULT);
        }
        if len == 0 {
            return Ok(0);
        }

        loop {
            let op = self.op_gate.read();
            match self.write_at_user_body(offset, len, user, old_size) {
                Ok(written) => {
                    drop(op);
                    if written != 0 {
                        self.notify_state_progress();
                    }
                    balance_dirty_pages();
                    return Ok(written);
                }
                Err(WriteAttemptError::Busy(entry)) => {
                    drop(op);
                    self.notify_state_progress();
                    self.wait_for_write_lease(&entry)?;
                }
                Err(WriteAttemptError::Error(error)) => {
                    drop(op);
                    self.notify_state_progress();
                    return Err(error);
                }
            }
        }
    }

    /// One direct-user write attempt while `op_gate.read()` is held. All page
    /// leases are acquired before the first user byte is copied, so lease
    /// contention can sleep and retry without publishing a partial prefix.
    fn write_at_user_body(
        &self,
        offset: usize,
        count: usize,
        user: &crate::mm::UserBuffer,
        old_size: usize,
    ) -> Result<usize, WriteAttemptError> {
        struct UserCopyItem {
            entry: Arc<PageEntry>,
            page_offset: usize,
            user_offset: usize,
            len: usize,
            was_dirty: bool,
        }

        let end = offset.checked_add(count).ok_or(SyscallErr::EFBIG)?;
        let start_page = offset >> PAGE_SIZE_BITS;
        let end_page = (end - 1) >> PAGE_SIZE_BITS;
        let mut copies: Vec<UserCopyItem> = Vec::new();

        let lookup_start = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
        for page_index in start_page..=end_page {
            let page_start = page_index << PAGE_SIZE_BITS;
            let write_start = offset.max(page_start);
            let write_end = end.min(page_start.saturating_add(PAGE_SIZE));
            let sub_len = write_end.saturating_sub(write_start);
            if sub_len == 0 {
                continue;
            }

            let page_offset = write_start - page_start;
            let full_page_overwrite = page_offset == 0 && sub_len == PAGE_SIZE;
            let entry = match self.get_page_for_write_populate(
                page_index,
                Some(old_size),
                full_page_overwrite,
            ) {
                Ok(entry) => entry,
                Err(error) => {
                    for item in &copies {
                        item.entry.abort_write();
                    }
                    return Err(WriteAttemptError::Error(error));
                }
            };
            let was_dirty = match entry.try_lock_for_write() {
                Ok(Some(was_dirty)) => was_dirty,
                Ok(None) => {
                    for item in &copies {
                        item.entry.abort_write();
                    }
                    return Err(WriteAttemptError::Busy(entry));
                }
                Err(error) => {
                    for item in &copies {
                        item.entry.abort_write();
                    }
                    return Err(WriteAttemptError::Error(error));
                }
            };
            copies.push(UserCopyItem {
                entry,
                page_offset,
                user_offset: write_start - offset,
                len: sub_len,
                was_dirty,
            });
        }
        perf::record_pc_write_lookup(
            perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(lookup_start),
        );

        let mut total_written = 0usize;
        for index in 0..copies.len() {
            let item = &copies[index];
            let copy_start = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
            let copied = item.entry.with_bytes_mut(|dst| {
                user.read_into_at_nofault(
                    item.user_offset,
                    &mut dst[item.page_offset..item.page_offset + item.len],
                )
            });
            let copied = match copied {
                Ok(copied) => copied,
                Err(_) => {
                    for pending in &copies[index..] {
                        pending.entry.abort_write();
                    }
                    return if total_written == 0 {
                        Err(WriteAttemptError::Error(SyscallErr::EFAULT))
                    } else {
                        Ok(total_written)
                    };
                }
            };
            perf::record_pc_write_copy(
                perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(copy_start),
            );
            if copied != 0 {
                item.entry.mark_valid(item.page_offset, copied);
                let commit_start = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
                if item.entry.commit_write() && !item.was_dirty {
                    GLOBAL_DIRTY_PAGES.fetch_add(1, Ordering::Relaxed);
                }
                perf::record_pc_write_commit(
                    perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO)
                        .wrapping_sub(commit_start),
                );
                total_written += copied;
            }

            if copied != item.len {
                if copied == 0 {
                    item.entry.abort_write();
                }
                for pending in &copies[index + 1..] {
                    pending.entry.abort_write();
                }
                return if total_written == 0 {
                    Err(WriteAttemptError::Error(SyscallErr::EFAULT))
                } else {
                    Ok(total_written)
                };
            }
        }
        Ok(total_written)
    }

    /// develop 命名别名：直接把 PageCache 页面复制到用户缓冲区。
    pub fn read_user(
        &self,
        offset: usize,
        len: usize,
        dst: &mut crate::mm::UserBuffer,
    ) -> Result<usize, SyscallErr> {
        self.read_at_user(offset, len, dst)
    }

    /// develop 命名别名：直接从用户缓冲区复制到 PageCache 页面。
    pub fn write_user(
        &self,
        offset: usize,
        len: usize,
        src: &crate::mm::UserBuffer,
        old_file_size: Option<usize>,
    ) -> Result<usize, SyscallErr> {
        self.write_at_user(offset, len, src, old_file_size.unwrap_or(0))
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
        if count > MAX_BATCH_READ_PAGES {
            let end_page = start_page.checked_add(count).ok_or(SyscallErr::EFBIG)?;
            let mut cursor = start_page;
            let mut read = 0;
            while cursor < end_page {
                let batch = (end_page - cursor).min(MAX_BATCH_READ_PAGES);
                read += self.sync_batch_read_pages(cursor, batch)?;
                cursor += batch;
            }
            return Ok(read);
        }

        self.sync_batch_read_pages_inner(start_page, count, None)
    }

    /// Populate one bounded forward filemap window. The demand page remains
    /// authoritative; all pages after it are tagged until first use so the
    /// diagnostic stream can distinguish useful fault-around from waste.
    fn sync_filemap_fault_around(
        &self,
        start_page: usize,
        count: usize,
    ) -> Result<usize, SyscallErr> {
        let count = count.clamp(1, MAX_DEMAND_READ_PAGES);
        crate::task::perf::record_filemap_fault_around_start(count);
        let result = self.sync_batch_read_pages_inner(start_page, count, Some(start_page));
        if result.is_err() {
            crate::task::perf::record_filemap_fault_around_abort();
        }
        result
    }

    fn sync_batch_read_pages_inner(
        &self,
        start_page: usize,
        count: usize,
        filemap_demand_page: Option<usize>,
    ) -> Result<usize, SyscallErr> {
        debug_assert!(count <= MAX_BATCH_READ_PAGES);
        let end_page = start_page.checked_add(count).ok_or(SyscallErr::EFBIG)?;

        // Readahead uses the same lock-free backend phase as batch misses so
        // a backend callback can re-enter this cache.  Generation validation
        // prevents stale pages from being published after truncate.
        let generation = self.mutation_generation.load(Ordering::Acquire);

        let backend = match self.backend() {
            Some(b) => b,
            None => return Ok(0),
        };
        let backend_npages = backend.npages();

        // Phase 1: atomically claim cache misses before allocating frames or
        // starting backend I/O.  A second batch reader skips claimed pages;
        // its demand fault waits for state progress instead of issuing the
        // same read again.
        let mut claimed = Vec::new();
        let mut claim_conflicts = 0usize;
        {
            let entries = self.entries.lock();
            let mut claims = self.batch_read_claims.lock();
            for page_index in start_page..end_page {
                if page_index < entries.len() && entries[page_index].is_some() {
                    continue;
                }
                if claims.insert(page_index) {
                    claimed.push(page_index);
                } else {
                    claim_conflicts += 1;
                }
            }
        }
        if filemap_demand_page.is_some() {
            crate::task::perf::record_filemap_fault_around_missing(claimed.len());
            crate::task::perf::record_filemap_fault_around_claim_conflict(claim_conflicts);
        }
        if claimed.is_empty() {
            return Ok(0);
        }

        struct PendingPage {
            index: usize, // absolute page index
            entry: Arc<PageEntry>,
        }
        let result = (|| -> Result<(usize, usize), SyscallErr> {
            let mut pending: Vec<PendingPage> = Vec::new();
            pending
                .try_reserve_exact(claimed.len())
                .map_err(|_| SyscallErr::ENOMEM)?;
            for &page_index in &claimed {
                let frame = frame_alloc().ok_or(SyscallErr::ENOMEM)?;
                pending.push(PendingPage {
                    index: page_index,
                    entry: Arc::new(PageEntry::new(frame, PageState::Loading)),
                });
            }

            // Phase 2: group contiguous claimed misses and let the backend
            // perform one logical read_pages call per run.  Staging keeps
            // backend callbacks outside PageEntry and PageCache locks.
            let mut cursor = 0;
            while cursor < pending.len() {
                if pending[cursor].index >= backend_npages {
                    pending[cursor].entry.with_bytes_mut(|bytes| bytes.fill(0));
                    cursor += 1;
                    continue;
                }
                let run_begin = cursor;
                cursor += 1;
                while cursor < pending.len()
                    && pending[cursor].index < backend_npages
                    && pending[cursor].index == pending[cursor - 1].index + 1
                {
                    cursor += 1;
                }
                let run_len = cursor - run_begin;
                let mut staging = Vec::new();
                staging
                    .try_reserve_exact(run_len * PAGE_SIZE)
                    .map_err(|_| SyscallErr::ENOMEM)?;
                staging.resize(run_len * PAGE_SIZE, 0);
                let mut buffers: Vec<&mut [u8]> = staging.chunks_mut(PAGE_SIZE).collect();
                let backend_start = crate::task::perf::perf_time_now_for(
                    crate::task::perf::STATS_PROFILE_MEMORY_IO,
                );
                let backend_result = backend.read_pages(pending[run_begin].index, &mut buffers);
                if filemap_demand_page.is_some() {
                    crate::task::perf::record_filemap_backend_read(
                        crate::task::perf::perf_time_now_for(
                            crate::task::perf::STATS_PROFILE_MEMORY_IO,
                        )
                        .wrapping_sub(backend_start),
                        false,
                    );
                    crate::task::perf::record_filemap_fault_around_backend_run();
                }
                backend_result?;
                drop(buffers);
                for (offset, pending_page) in pending[run_begin..cursor].iter().enumerate() {
                    let start = offset * PAGE_SIZE;
                    pending_page.entry.with_bytes_mut(|buf| {
                        buf.copy_from_slice(&staging[start..start + PAGE_SIZE])
                    });
                }
            }

            if generation != self.mutation_generation.load(Ordering::Acquire) {
                return Err(SyscallErr::EAGAIN);
            }

            // Phase 3: publish only while the mutation generation remains
            // stable.  A direct reader that won independently remains
            // authoritative; the claimed batch never overwrites it.
            let mut published_pages = 0;
            let mut prefetched_pages = 0;
            let track_use = filemap_demand_page.is_some()
                && crate::task::perf::stats_enabled_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
            {
                let mut entries = self.entries.lock();
                if generation != self.mutation_generation.load(Ordering::Acquire) {
                    return Err(SyscallErr::EAGAIN);
                }
                let mut inner = self.inner.lock();
                for p in &pending {
                    while entries.len() <= p.index {
                        entries.push(None);
                    }
                    if entries[p.index].is_none() {
                        p.entry.set_state(PageState::UpToDate);
                        if track_use && filemap_demand_page != Some(p.index) {
                            p.entry.mark_filemap_readahead();
                            prefetched_pages += 1;
                        }
                        entries[p.index] = Some(p.entry.clone());
                        inner.pages.insert(p.index);
                        published_pages += 1;
                    }
                }
            }
            Ok((published_pages, prefetched_pages))
        })();

        // Every exit after claiming pages must release ownership and wake
        // waiters, including allocation/backend errors and generation races.
        self.release_batch_read_claims(&claimed);
        let (published_pages, prefetched_pages) = result?;
        if filemap_demand_page.is_some() {
            crate::task::perf::record_filemap_fault_around_publish(
                published_pages,
                prefetched_pages,
            );
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

    /// 将指定页面索引加入脏页集合，并原子递增全局脏页计数。
    pub fn mark_page_dirty(&self, page_index: usize) {
        let entry = self
            .entries
            .lock()
            .get(page_index)
            .and_then(Option::as_ref)
            .cloned();
        if let Some(entry) = entry {
            let mut raw = entry.flags.load(Ordering::Acquire);
            loop {
                if raw & PG_DIRTY != 0 {
                    return;
                }
                let next = raw | PG_UPTODATE | PG_DIRTY;
                match entry
                    .flags
                    .compare_exchange(raw, next, Ordering::AcqRel, Ordering::Acquire)
                {
                    Ok(_) => {
                        GLOBAL_DIRTY_PAGES.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                    Err(current) => raw = current,
                }
            }
        }
    }

    /// 从脏页集合移除该索引，并原子递减全局脏页计数（写回完成后调用）。
    pub fn mark_page_writeback(&self, page_index: usize) {
        if let Some(entry) = self
            .entries
            .lock()
            .get(page_index)
            .and_then(Option::as_ref)
            .cloned()
        {
            if entry.test_and_clear_flag(PG_DIRTY) {
                GLOBAL_DIRTY_PAGES.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }

    // ── 回写 ─────────────────────────────────────────────────────────

    /// 单次回写批次的最大页面数
    const MAX_WRITEBACK_PAGES: usize = 256;

    /// 将单个脏页通过 `backend` 写回存储介质；若页面已为 `UpToDate` 则跳过。
    pub fn writeback_page(&self, page_index: usize) -> Result<(), SyscallErr> {
        self.writeback_page_impl(page_index)
    }

    /// Write back one page after claiming its Dirty → Writeback state.
    fn writeback_page_impl(&self, page_index: usize) -> Result<(), SyscallErr> {
        let _t0 = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);
        let entry = {
            let entries = self.entries.lock();
            if page_index >= entries.len() {
                perf::record_pc_writeback(
                    0,
                    perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(_t0),
                );
                return Ok(());
            }
            match &entries[page_index] {
                Some(e) => e.clone(),
                None => {
                    perf::record_pc_writeback(
                        0,
                        perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(_t0),
                    );
                    return Ok(());
                }
            }
        };

        // Claim Dirty → Writeback only if no writer owns PG_LOCKED.
        if !entry.claim_writeback() {
            perf::record_pc_writeback(
                0,
                perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(_t0),
            );
            return Ok(());
        }
        GLOBAL_DIRTY_PAGES.fetch_sub(1, Ordering::Relaxed);
        GLOBAL_WRITEBACK_PAGES.fetch_add(1, Ordering::Relaxed);

        let result = if let Some(backend) = self.backend() {
            // 写回前确保所有 segment 有效（填充部分写入的页面空洞）
            if let Err(error) = self.ensure_fully_valid(page_index) {
                // Dirty -> Writeback accounting has already been committed.
                // A populate failure must make the page retryable instead of
                // leaving it permanently stranded in Writeback.
                entry.restore_dirty_after_writeback();
                GLOBAL_DIRTY_PAGES.fetch_add(1, Ordering::Relaxed);
                GLOBAL_WRITEBACK_PAGES.fetch_sub(1, Ordering::Relaxed);
                self.notify_state_progress();
                return Err(error);
            }
            // data 读锁是 file page 的线性化点：Dirty→Writeback 后先在同一
            // 临界区 mkclean，所有共享 PTE 从此刻起都不可写且 dirty 已清；VM
            // 锁内只收集 TLB，实际 shootdown 由 AddressSpace 解锁后执行。
            let snapshot = entry.read_bytes();
            self.mkclean_page(page_index, false);
            let result = backend.write_page(page_index, snapshot.bytes());
            drop(snapshot);
            match result {
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

        perf::record_pc_writeback(
            1,
            perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(_t0),
        );
        self.notify_state_progress();
        result
    }

    /// 批量写回一段连续的脏页
    ///
    /// `start..start+count` 范围内只对实际标记为 Dirty 的页面执行写回；
    /// 非 Dirty 的页面被跳过。批次中至少一个页面被写入时，调用
    /// `backend.write_pages()` 批量提交；否则直接返回 Ok。
    fn writeback_pages_run(&self, start: usize, count: usize) -> Result<(), SyscallErr> {
        self.writeback_pages_run_impl(start, count)
    }

    /// Batch writeback after claiming each page; backend I/O runs without
    /// `op_gate` so a backend may re-enter this cache safely.
    fn writeback_pages_run_impl(&self, start: usize, count: usize) -> Result<(), SyscallErr> {
        let _t0 = perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO);

        // 第一阶段：持有 entries 锁，收集 Dirty 页面，CAS 为 Writeback
        let mut page_slices: Vec<(usize, Arc<PageEntry>)> = Vec::new();
        {
            let entries = self.entries.lock();
            let end = (start + count).min(entries.len());
            for i in start..end {
                if let Some(entry) = &entries[i] {
                    if entry.claim_writeback() {
                        GLOBAL_DIRTY_PAGES.fetch_sub(1, Ordering::Relaxed);
                        GLOBAL_WRITEBACK_PAGES.fetch_add(1, Ordering::Relaxed);
                        page_slices.push((i, entry.clone()));
                    }
                }
            }
        }

        if page_slices.is_empty() {
            return Ok(());
        }

        let restore_dirty = |pages: &[(usize, Arc<PageEntry>)]| {
            for (idx, entry) in pages {
                entry.restore_dirty_after_writeback();
                GLOBAL_DIRTY_PAGES.fetch_add(1, Ordering::Relaxed);
                GLOBAL_WRITEBACK_PAGES.fetch_sub(1, Ordering::Relaxed);
            }
            self.notify_state_progress();
        };
        let complete_writeback = |pages: &[(usize, Arc<PageEntry>)]| {
            for (idx, entry) in pages {
                if entry.complete_writeback() {
                    GLOBAL_DIRTY_PAGES.fetch_add(1, Ordering::Relaxed);
                }
                GLOBAL_WRITEBACK_PAGES.fetch_sub(1, Ordering::Relaxed);
            }
            self.notify_state_progress();
        };

        // 写回前确保所有 segment 有效（填充部分写入的页面空洞）。
        // Any populate failure must roll every page out of Writeback state.
        for (idx, _) in &page_slices {
            if let Err(error) = self.ensure_fully_valid(*idx) {
                restore_dirty(&page_slices);
                return Err(error);
            }
        }

        // 每个页在取 backend 快照前完成 wrprotect+cleandirty。rmap walker 不持
        // entries/inner 锁；它只短暂取得各 AddressSpace 的 VM 锁并由其锁外 flush。
        for (idx, entry) in &page_slices {
            let snapshot = entry.read_bytes();
            self.mkclean_page(*idx, false);
            drop(snapshot);
        }

        let result = if let Some(backend) = self.backend() {
            // CAS may have skipped a page that another writer already owns.
            // Split the pages actually acquired into contiguous sub-runs so
            // later pages can never be shifted onto an earlier file offset.
            let mut cursor = 0;
            let mut result = Ok(());
            while cursor < page_slices.len() {
                let mut end = cursor + 1;
                while end < page_slices.len() && page_slices[end].0 == page_slices[end - 1].0 + 1 {
                    end += 1;
                }
                let run = &page_slices[cursor..end];
                let write_result = {
                    let guards: Vec<PageBytesReadGuard<'_>> =
                        run.iter().map(|(_, entry)| entry.read_bytes()).collect();
                    let slices: Vec<&[u8]> = guards.iter().map(PageBytesReadGuard::bytes).collect();
                    backend.write_pages(run[0].0, &slices)
                };
                // 所有 data-read guard 已释放，之后才允许进入 inner 完成状态提交。
                match write_result {
                    Ok(_) => complete_writeback(run),
                    Err(error) => {
                        restore_dirty(&page_slices[cursor..]);
                        result = Err(error);
                        break;
                    }
                }
                cursor = end;
            }
            result
        } else {
            complete_writeback(&page_slices);
            Ok(())
        };

        perf::record_pc_writeback(
            page_slices.len(),
            perf::perf_time_now_for(perf::STATS_PROFILE_MEMORY_IO).wrapping_sub(_t0),
        );
        result
    }

    /// 收集当前所有脏页索引并按连续 run 分组，逐 run 写回。
    pub fn writeback_all(&self) -> Result<(), SyscallErr> {
        loop {
            let observed = self.state_wait_generation.load(Ordering::Acquire);
            let dirty_indices = self.find_dirty_pages();
            if dirty_indices.is_empty() {
                if !self.has_transient_pages(0, usize::MAX) {
                    return Ok(());
                }
                self.wait_for_state_progress(observed)?;
                continue;
            }

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
                self.writeback_pages_run_impl(run_start, run_end - run_start + 1)?;
            }
        }
    }

    /// Drain writeback before taking the exclusive I/O gate. Ordinary writers
    /// share the read side, so a writer may redirty a page after an earlier
    /// pass; recheck under the exclusive gate before a metadata mutation.
    pub(crate) fn writeback_all_before_io_gate(&self) -> Result<(), SyscallErr> {
        loop {
            self.writeback_all()?;
            let observed = self.state_wait_generation.load(Ordering::Acquire);
            let pending = self.with_io_gate(|| {
                !self.find_dirty_pages().is_empty() || self.has_transient_pages(0, usize::MAX)
            });
            if !pending {
                return Ok(());
            }
            if self.find_dirty_pages().is_empty() {
                self.wait_for_state_progress(observed)?;
            }
        }
    }

    /// 筛选出 `[start_index, end_index]` 范围内的脏页，按连续 run 分组写回。
    pub fn writeback_range(&self, start_index: usize, end_index: usize) -> Result<(), SyscallErr> {
        loop {
            let observed = self.state_wait_generation.load(Ordering::Acquire);
            let dirty_indices: Vec<usize> = self
                .find_dirty_pages()
                .into_iter()
                .filter(|index| *index >= start_index && *index <= end_index)
                .collect();
            if dirty_indices.is_empty() {
                if !self.has_transient_pages(start_index, end_index) {
                    return Ok(());
                }
                self.wait_for_state_progress(observed)?;
                continue;
            }

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
                self.writeback_pages_run_impl(run_start, run_end - run_start + 1)?;
            }
        }
    }

    /// 请求下一次合作式 writeback worker 回写此缓存的脏页。
    pub fn queue_writeback(&self) {
        self.async_writeback_requested
            .store(true, Ordering::Release);
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
            // concurrently consumed by another flusher. Preserve backend
            // errors instead of silently dropping them from background sync.
            self.writeback_pages_run_impl(run_start, run_end - run_start + 1)?;
            total += count;
        }
        Ok(total)
    }

    // ── 截断与失效 ──────────────────────────────────────────────────

    /// 截断 page cache 到指定大小
    pub fn truncate(&self, new_size: usize) -> Result<(), SyscallErr> {
        self.truncate_with_backend(new_size, || Ok(()))
    }

    /// Atomically order a persistent truncate between PageCache writeback and
    /// ordinary cached writes.  `persistent` runs only after confirming that
    /// no page in the discarded range is already in Writeback; cache removal
    /// is committed only after the backend operation succeeds.
    pub(crate) fn truncate_with_backend(
        &self,
        new_size: usize,
        persistent: impl FnOnce() -> Result<(), SyscallErr>,
    ) -> Result<(), SyscallErr> {
        self.writeback_all_before_io_gate()?;
        self.with_io_gate(|| self.truncate_with_backend_locked(new_size, persistent))
    }

    /// Truncate the cache and persist the backend mutation while `op_gate` is
    /// already held.  The backend callback runs before discarded cache entries
    /// are removed, so a failed persistent truncate cannot silently lose the
    /// in-memory tail.
    pub(crate) fn truncate_with_io_gate_held_and_backend<F>(
        &self,
        new_size: usize,
        persistent: F,
    ) -> Result<(), SyscallErr>
    where
        F: FnOnce() -> Result<(), SyscallErr>,
    {
        self.truncate_with_backend_locked(new_size, persistent)
    }

    fn truncate_with_backend_locked(
        &self,
        new_size: usize,
        persistent: impl FnOnce() -> Result<(), SyscallErr>,
    ) -> Result<(), SyscallErr> {
        // The caller holds the write side of op_gate; no user copy or task
        // wait is permitted in this critical section.
        self.mutation_generation.fetch_add(1, Ordering::AcqRel);
        let hole_start_page = new_size.div_ceil(PAGE_SIZE);
        // Pass A：在移除 PageCache entry 前先 zap 所有仍映射该页的 VMA。fault
        // 若已持 VM 锁会在 op_gate.try_read 处返回 Retry，绝不反向等待；每轮
        // mkclean 自带 i_mmap_seq 重验，因此 mmap/munmap 不会留下旧 PTE。
        let tail_indices: Vec<usize> = {
            let entries = self.entries.lock();
            (hole_start_page..entries.len())
                .filter(|index| {
                    entries[*index]
                        .as_ref()
                        .is_some_and(|entry| entry.map_count.load(Ordering::Acquire) != 0)
                })
                .collect()
        };
        for page_index in tail_indices {
            self.mkclean_page(page_index, true);
        }
        persistent()?;

        let tail_entry = {
            let mut entries = self.entries.lock();
            let mut inner = self.inner.lock();
            for page_index in hole_start_page..entries.len() {
                if let Some(entry) = entries[page_index].take() {
                    entry.discard_filemap_readahead();
                    if entry.state() == PageState::Dirty {
                        GLOBAL_DIRTY_PAGES.fetch_sub(1, Ordering::Relaxed);
                    }
                    inner.pages.remove(&page_index);
                }
            }
            let offset_in_page = new_size & (PAGE_SIZE - 1);
            if offset_in_page == 0 {
                None
            } else {
                entries
                    .get(new_size / PAGE_SIZE)
                    .and_then(Option::as_ref)
                    .cloned()
                    .map(|entry| (entry, offset_in_page))
            }
        };

        // entries → inner 已释放后才获取 page data lock，避免反向锁序。
        if let Some((entry, offset_in_page)) = tail_entry {
            entry.with_bytes_mut(|bytes| bytes[offset_in_page..].fill(0));
        }

        Ok(())
    }

    /// Roll back cache pages created by a failed file extension.
    ///
    /// Unlike normal truncate, this helper only discards pages that are not
    /// already in writeback.  A failed `PageCache::write()` returns before it
    /// invokes dirty balancing, so extension-only pages are normally Dirty;
    /// accounting must be undone when those speculative pages are removed.
    pub(crate) fn rollback_failed_extension(&self, restored_size: usize) {
        let _op = self.op_gate.write();
        let first_discard = restored_size.div_ceil(PAGE_SIZE);
        let mut entries = self.entries.lock();
        let mut inner = self.inner.lock();
        for page_index in first_discard..entries.len() {
            let removable = entries[page_index]
                .as_ref()
                .is_some_and(|entry| entry.state() != PageState::Writeback);
            if !removable {
                continue;
            }
            if let Some(entry) = entries[page_index].take() {
                entry.discard_filemap_readahead();
                if entry.state() == PageState::Dirty {
                    GLOBAL_DIRTY_PAGES.fetch_sub(1, Ordering::Relaxed);
                }
                inner.pages.remove(&page_index);
            }
        }
    }

    /// 收集所有页面的 FrameTracker，用于内核空间映射
    pub fn frame_trackers(&self) -> Vec<crate::mm::Frame> {
        use crate::mm::Frame;
        let entries = self.entries.lock();
        entries
            .iter()
            .filter_map(|opt| {
                opt.as_ref()
                    .map(|entry| Frame::InMemory(entry.page.clone()))
            })
            .collect()
    }

    /// 失效指定范围内的页面
    /// 如果范围内存在脏页，返回错误而不静默丢弃数据
    pub fn invalidate_range(
        &self,
        start_index: usize,
        end_index: usize,
    ) -> Result<usize, SyscallErr> {
        let _op = self.op_gate.write();
        self.mutation_generation.fetch_add(1, Ordering::AcqRel);
        // 先检查范围内是否有脏页
        {
            let entries = self.entries.lock();
            for page_index in start_index..end_index {
                if entries
                    .get(page_index)
                    .and_then(Option::as_ref)
                    .is_some_and(|entry| entry.test_flag(PG_DIRTY))
                {
                    return Err(SyscallErr::EBUSY);
                }
            }
        }

        let mut entries = self.entries.lock();
        let mut inner = self.inner.lock();
        let mut invalidated = 0;

        let end = core::cmp::min(end_index, entries.len());
        for page_index in start_index..end {
            if let Some(entry) = entries[page_index].take() {
                entry.discard_filemap_readahead();
                inner.pages.remove(&page_index);
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
pub fn maybe_background_writeback() {
    let dirty = GLOBAL_DIRTY_PAGES.load(Ordering::Relaxed);
    if dirty < DIRTY_BACKGROUND
        && !PAGE_CACHE_REGISTRY.lock().iter().any(|weak| {
            weak.upgrade()
                .is_some_and(|pc| pc.async_writeback_requested.load(Ordering::Acquire))
        })
    {
        return;
    }
    if WRITEBACK_ACTIVE.swap(true, Ordering::AcqRel) {
        return; // another caller is already flushing
    }
    let budget = if dirty >= DIRTY_THROTTLE {
        WB_BG_MAX_PAGES
    } else {
        WB_BATCH_PAGES
    };
    crate::task::perf::record_wb_bg_call();

    // Snapshot alive page caches; drop dead weak refs
    let mut reg = PAGE_CACHE_REGISTRY.lock();
    reg.retain(|w| w.strong_count() > 0);
    let caches: Vec<Arc<PageCache>> = reg.iter().filter_map(|w| w.upgrade()).collect();
    drop(reg);

    let mut remaining = budget;
    for pc in &caches {
        let requested = pc.async_writeback_requested.swap(false, Ordering::AcqRel);
        if remaining == 0 && !requested {
            break;
        }
        let page_budget = if requested {
            WB_BATCH_PAGES
        } else {
            remaining.min(WB_BATCH_PAGES)
        };
        let written = match pc.writeback_some_pages(page_budget) {
            Ok(written) => written,
            Err(error) => {
                log::error!("page-cache background writeback failed: {:?}", error);
                0
            }
        };
        remaining = remaining.saturating_sub(written);
    }

    WRITEBACK_ACTIVE.store(false, Ordering::Release);
}

/// 简化版脏页节流：写入者帮助推进写回，或者异步触发后台写回。
///
/// 在 write_kernel() 后调用此函数。
/// - 低于 DIRTY_BACKGROUND：直接返回
/// - 在 [DIRTY_BACKGROUND, DIRTY_THROTTLE) 之间：触发后台写回（非阻塞帮助）
/// - 超过 DIRTY_THROTTLE：写入者帮助完成一批写回
pub fn balance_dirty_pages() {
    let dirty = GLOBAL_DIRTY_PAGES.load(Ordering::Relaxed);
    let wb = GLOBAL_WRITEBACK_PAGES.load(Ordering::Relaxed);
    let total = dirty.saturating_add(wb);

    if total < DIRTY_BACKGROUND {
        return;
    }

    if total < DIRTY_THROTTLE {
        // Below throttle: opportunistic background flush (non-blocking)
        maybe_background_writeback();
        return;
    }

    // Above throttle: writer helps with one batch
    if !WRITEBACK_ACTIVE.swap(true, Ordering::AcqRel) {
        crate::task::perf::record_wb_throttle_call();

        let mut reg = PAGE_CACHE_REGISTRY.lock();
        reg.retain(|w| w.strong_count() > 0);
        let caches: Vec<Arc<PageCache>> = reg.iter().filter_map(|w| w.upgrade()).collect();
        drop(reg);

        for pc in &caches {
            let written = match pc.writeback_some_pages(WB_BATCH_PAGES) {
                Ok(written) => written,
                Err(error) => {
                    log::error!("page-cache throttle writeback failed: {:?}", error);
                    0
                }
            };
            if written > 0 {
                break;
            }
        }
        WRITEBACK_ACTIVE.store(false, Ordering::Release);
    }
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
                .read_block(block_id, &mut buf[start..start + self.block_size]);
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
                .write_block(block_id, &buf[start..start + self.block_size]);
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
/// Shares the inode's cluster-list storage directly, so dirty pages can still
/// be written while `FatInode::drop` is running. A `Weak<FatInode>` cannot be
/// upgraded once the strong count reaches zero, which used to lose final
/// writeback data during rename/unlink cache eviction.
pub struct FatPageCacheBackend {
    fs: alloc::sync::Arc<crate::fs::fat32::EasyFileSystem>,
    file_content: alloc::sync::Arc<spin::RwLock<crate::fs::fat32::fat_inode::FileContent>>,
    block_size: usize,
    blocks_per_page: usize,
    sec_per_clus: usize,
}

impl FatPageCacheBackend {
    pub fn new(
        fs: alloc::sync::Arc<crate::fs::fat32::EasyFileSystem>,
        file_content: alloc::sync::Arc<spin::RwLock<crate::fs::fat32::fat_inode::FileContent>>,
    ) -> Self {
        let block_size = fs.byts_per_sec as usize;
        let blocks_per_page = crate::config::PAGE_SIZE / block_size;
        let sec_per_clus = fs.sec_per_clus as usize;
        FatPageCacheBackend {
            fs,
            file_content,
            block_size,
            blocks_per_page,
            sec_per_clus,
        }
    }

    fn block_id_for_offset(&self, page_index: usize, block_off: usize) -> Option<usize> {
        let lock = self.file_content.read();
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

    /// FAT32 内部扇区号（BPB_BytsPerSec 单位）→ 设备块号 + 块内字节偏移。
    ///
    /// cdc17728 之后 BlockDevice 一律以 BLOCK_SZ(4096) 编址，FAT32 自行换算扇区
    /// （与 `bitmap.rs` 的 `Fat::sector_to_parent` 同一约定），不能再用 512 逻辑
    /// 块号直接调用 `read_block`，否则会把扇区号当成设备块号读到错误位置。
    #[inline(always)]
    fn sector_to_parent(&self, sector: usize) -> (usize, usize) {
        let sectors_per_block = crate::hal::BLOCK_SZ / self.block_size;
        let block_id = sector / sectors_per_block;
        let block_off = (sector % sectors_per_block) * self.block_size;
        (block_id, block_off)
    }
}

impl PageCacheBackend for FatPageCacheBackend {
    fn read_page(&self, index: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        if buf.len() < crate::config::PAGE_SIZE {
            return Err(SyscallErr::ENOBUFS);
        }
        for block_off in 0..self.blocks_per_page {
            let start = block_off * self.block_size;
            assert!(start + self.block_size <= crate::config::PAGE_SIZE);
            match self.block_id_for_offset(index, block_off) {
                Some(sec_id) => {
                    let (block_id, block_off_bytes) = self.sector_to_parent(sec_id);
                    let mut block = alloc::vec![0u8; crate::hal::BLOCK_SZ];
                    self.fs.block_device.read_block(block_id, &mut block);
                    buf[start..start + self.block_size].copy_from_slice(
                        &block[block_off_bytes..block_off_bytes + self.block_size],
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
        for block_off in 0..self.blocks_per_page {
            let start = block_off * self.block_size;
            assert!(start + self.block_size <= crate::config::PAGE_SIZE);
            if let Some(sec_id) = self.block_id_for_offset(index, block_off) {
                let (block_id, block_off_bytes) = self.sector_to_parent(sec_id);
                // 读-改-写：只修改目标扇区，保留同 4096 块内相邻扇区。
                let mut block = alloc::vec![0u8; crate::hal::BLOCK_SZ];
                self.fs.block_device.read_block(block_id, &mut block);
                block[block_off_bytes..block_off_bytes + self.block_size]
                    .copy_from_slice(&buf[start..start + self.block_size]);
                self.fs.block_device.write_block(block_id, &block);
            }
        }
        Ok(crate::config::PAGE_SIZE)
    }

    fn npages(&self) -> usize {
        let lock = self.file_content.read();
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
                        .read_block(block_id, &mut buf[start..start + self.block_size]);
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
                        .write_block(block_id, &buf[start..start + self.block_size]);
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
                blk.write_block(first_pblock_4k, &staging);

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
            blk.write_block(first_pblock, &staging);

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
                blk.read_block(first_pblock, &mut staging);

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
            blk.read_block(first_pblock, &mut staging);

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
