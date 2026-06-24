//! 页面缓存 — VFS 层的数据缓存机制
//!
//! 对标 DragonOS `kernel/src/filesystem/page_cache.rs` 的 `PageCache`。
//!
//! 设计思想：
//! - `PageCacheBackend` trait：将 PageCache 桥接到具体的存储后端（块设备、inode 等）
//! - `PageState` 状态机：Loading → UpToDate ↔ Dirty → Writeback → UpToDate
//! - 两阶段读写：持锁收集拷贝项，解锁后拷贝到/从用户缓冲区，避免死锁
//! - 脏页追踪：dirty_pages BTreeSet 跟踪所有脏页
//! - 回写机制：单页回写 + 范围回写
//!
//! 注意：当前实现为精简版，不做异步 IO、VMA 反向映射等高级特性。

use crate::utils::error::SyscallErr;
use alloc::collections::BTreeSet;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use spin::Mutex;

use super::vfs::IndexNode;
use crate::config::{PAGE_SIZE, PAGE_SIZE_BITS};
use crate::mm::{frame_alloc, FrameTracker};
use crate::task::perf;

static PAGE_CACHE_REGISTRY: Mutex<Vec<Weak<PageCache>>> = Mutex::new(Vec::new());

pub fn register_page_cache(pc: &Arc<PageCache>) {
    PAGE_CACHE_REGISTRY.lock().push(Arc::downgrade(pc));
}

pub fn flush_all_page_caches() {
    PAGE_CACHE_REGISTRY.lock().retain(|weak| {
        if let Some(pc) = weak.upgrade() {
            let _ = pc.writeback_all();
            true
        } else {
            false
        }
    });
}

/// Evict clean pages from all registered caches. Called periodically
/// to prevent unbounded PageCache growth.
pub fn evict_all_clean_pages(max_per_cache: usize) -> usize {
    let mut total = 0;
    PAGE_CACHE_REGISTRY.lock().retain(|weak| {
        if let Some(pc) = weak.upgrade() {
            total += pc.evict_clean_pages(max_per_cache);
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
    let mut tlen = 0; let mut tcap = 0; let mut tlive = 0; let mut tholes = 0;
    let reg = PAGE_CACHE_REGISTRY.lock();
    for weak in reg.iter() {
        if let Some(pc) = weak.upgrade() {
            let (len, cap, live, holes) = pc.entries_stats();
            tlen += len; tcap += cap; tlive += live; tholes += holes;
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

    /// 返回后端的页数
    fn npages(&self) -> usize;
}

// ── PageEntry ────────────────────────────────────────────────────────────

/// 页面缓存条目
#[derive(Debug)]
struct PageEntry {
    /// 物理页面
    page: Arc<FrameTracker>,
    /// 页面状态
    state: AtomicU8,
}

impl PageEntry {
    fn new(page: Arc<FrameTracker>, state: PageState) -> Self {
        PageEntry {
            page,
            state: AtomicU8::new(state as u8),
        }
    }

    fn state(&self) -> PageState {
        Self::decode_state(self.state.load(Ordering::Acquire))
    }

    fn set_state(&self, state: PageState) {
        self.state.store(state as u8, Ordering::Release);
    }

    fn decode_state(raw: u8) -> PageState {
        match raw {
            0 => PageState::Loading,
            1 => PageState::UpToDate,
            2 => PageState::Dirty,
            3 => PageState::Writeback,
            4 => PageState::Error,
            _ => PageState::Error,
        }
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

// ── InnerPageCache ───────────────────────────────────────────────────────

/// PageCache 内部状态
#[derive(Debug)]
struct InnerPageCache {
    /// 页面映射: page_index → PageEntry
    pages: BTreeSet<usize>,
    /// 脏页索引
    dirty_pages: BTreeSet<usize>,
}

impl InnerPageCache {
    fn new() -> Self {
        InnerPageCache {
            pages: BTreeSet::new(),
            dirty_pages: BTreeSet::new(),
        }
    }

    fn has_page(&self, index: usize) -> bool {
        self.pages.contains(&index)
    }

    fn mark_dirty(&mut self, index: usize) {
        self.dirty_pages.insert(index);
    }

    fn clear_dirty(&mut self, index: usize) {
        self.dirty_pages.remove(&index);
    }

    fn page_count(&self) -> usize {
        self.pages.len()
    }
}

// ── PageCache ────────────────────────────────────────────────────────────

/// 页面缓存
///
/// 为 inode 提供页面级别的缓存，管理内存中的文件数据副本。
pub struct PageCache {
    /// 内部状态
    inner: Mutex<InnerPageCache>,
    /// 缓存后端
    backend: Mutex<Option<Arc<dyn PageCacheBackend>>>,
    /// 关联的 inode（弱引用）
    inode: Mutex<Option<Weak<dyn IndexNode>>>,
    /// 缓存的页面条目
    entries: Mutex<Vec<Option<Arc<PageEntry>>>>,
    /// true = 页不可回收（用于 tmpfs/shmem，数据无持久化后端）
    unevictable: AtomicBool,
}

impl PageCache {
    /// 创建新的 PageCache
    pub fn new() -> Arc<Self> {
        let pc = Arc::new(PageCache {
            inner: Mutex::new(InnerPageCache::new()),
            backend: Mutex::new(None),
            inode: Mutex::new(None),
            entries: Mutex::new(Vec::new()),
            unevictable: AtomicBool::new(false),
        });
        register_page_cache(&pc);
        pc
    }

    /// 设置后端
    pub fn set_backend(&self, backend: Arc<dyn PageCacheBackend>) {
        *self.backend.lock() = Some(backend);
    }

    /// 设置关联的 inode
    pub fn set_inode(&self, inode: Weak<dyn IndexNode>) {
        *self.inode.lock() = Some(inode);
    }

    /// 设置不可回收标志（用于 tmpfs/shmem，数据无持久化后端）
    pub fn set_unevictable(&self, val: bool) {
        self.unevictable.store(val, Ordering::Release);
    }

    /// 获取后端
    pub fn backend(&self) -> Option<Arc<dyn PageCacheBackend>> {
        self.backend.lock().clone()
    }

    /// 获取页数
    pub fn page_count(&self) -> usize {
        self.inner.lock().page_count()
    }

    /// 检查页面是否在缓存中
    pub fn contains_page(&self, page_index: usize) -> bool {
        let entries = self.entries.lock();
        page_index < entries.len() && entries[page_index].is_some()
    }

    /// 检查页面是否为脏
    pub fn is_dirty(&self, page_index: usize) -> bool {
        self.inner.lock().dirty_pages.contains(&page_index)
    }

    /// 获取脏页数量
    pub fn dirty_count(&self) -> usize {
        self.inner.lock().dirty_pages.len()
    }

    /// 获取缓存中的页面数量
    pub fn cached_page_count(&self) -> usize {
        self.entries.lock().iter().filter(|e| e.is_some()).count()
    }

    /// Evict up to `target` clean pages that are held only by the cache.
    /// Checks: UpToDate state, not in dirty set, PageEntry refcount==1,
    /// AND FrameTracker refcount==1 (protects mmap'd pages).
    /// Returns the number evicted.
    pub fn evict_clean_pages(&self, target: usize) -> usize {
        // tmpfs/shmem pages must never be evicted — no persistent backend
        if self.unevictable.load(Ordering::Acquire) {
            return 0;
        }
        let mut entries = self.entries.lock();
        let mut inner = self.inner.lock();
        let mut evicted = 0;

        for i in 0..entries.len() {
            if evicted >= target {
                break;
            }
            if let Some(entry) = &entries[i] {
                crate::task::perf::record_reclaim_pages_scanned(1);
                if entry.state() != PageState::UpToDate {
                    continue;
                }
                if inner.dirty_pages.contains(&i) {
                    continue;
                }
                if Arc::strong_count(entry) != 1 {
                    continue;
                }
                if Arc::strong_count(&entry.page) != 1 {
                    continue;
                }
                // Safe to evict — only the cache holds this page, not mmap'd
                inner.pages.remove(&i);
                inner.dirty_pages.remove(&i);
                entries[i] = None;
                crate::task::perf::record_reclaim_pages_freed(1);
                evicted += 1;
            }
        }

        // Shrink trailing Nones
        while entries.last().map_or(false, |e| e.is_none()) {
            entries.pop();
        }

        evicted
    }

    /// 回收干净页面（仅释放 UpToDate 且无外部引用的页）
    pub fn shrink_clean_pages(&self, max_to_free: usize) -> usize {
        self.evict_clean_pages(max_to_free)
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

    /// 获取页面状态
    pub fn state_of(&self, page_index: usize) -> Option<PageState> {
        let entries = self.entries.lock();
        if page_index >= entries.len() {
            return None;
        }
        entries[page_index].as_ref().map(|e| e.state())
    }

    /// 获取所有脏页索引的快照
    pub fn dirty_pages_snapshot(&self) -> alloc::vec::Vec<usize> {
        self.inner.lock().dirty_pages.iter().copied().collect()
    }

    // ── 页面获取 ────────────────────────────────────────────────────

    /// 获取或创建一个页面条目
    fn get_or_create_entry(
        &self,
        page_index: usize,
        populate: bool,
    ) -> Result<Arc<PageEntry>, SyscallErr> {
        let mut entries = self.entries.lock();

        // 扩展 entries 数组
        while entries.len() <= page_index {
            entries.push(None);
        }

        if let Some(entry) = &entries[page_index] {
            return Ok(entry.clone());
        }

        // 分配新帧
        let _t_falloc = perf::perf_time_now();
        let frame = frame_alloc().ok_or(SyscallErr::ENOMEM)?;
        let entry = Arc::new(PageEntry::new(frame, PageState::UpToDate));
        perf::record_pc_falloc_cycles(perf::perf_time_now().wrapping_sub(_t_falloc));

        // 从后端填充数据
        if populate {
            perf::record_pc_miss();
            if let Some(backend) = self.backend() {
                let buf = entry.as_slice_mut();
                if let Err(e) = backend.read_page(page_index, buf) {
                    return Err(e);
                }
            }
        }

        let entry_clone = entry.clone();
        entries[page_index] = Some(entry);

        let mut inner = self.inner.lock();
        inner.pages.insert(page_index);

        Ok(entry_clone)
    }

    /// 获取页面用于读取
    pub fn get_page_for_read(&self, page_index: usize) -> Result<Arc<PageEntry>, SyscallErr> {
        self.get_or_create_entry(page_index, true)
    }

    /// 获取页面用于写入
    pub fn get_page_for_write(&self, page_index: usize) -> Result<Arc<PageEntry>, SyscallErr> {
        self.get_page_for_write_populate(page_index, true)
    }

    /// 获取页面用于写入，可选择是否从后端 populate。
    /// populate=false 用于整页覆盖写：直接分配新帧并标记 dirty，
    /// 跳过 backend read_page 减少 I/O。
    fn get_page_for_write_populate(
        &self,
        page_index: usize,
        populate: bool,
    ) -> Result<Arc<PageEntry>, SyscallErr> {
        let entry = self.get_or_create_entry(page_index, populate)?;
        let mut inner = self.inner.lock();
        inner.mark_dirty(page_index);
        if entry.state() == PageState::UpToDate {
            entry.set_state(PageState::Dirty);
        }
        Ok(entry)
    }

    /// 获取页帧用于文件映射读（如 MAP_PRIVATE file-backed page fault）。
    /// 返回 PageCache 中的 `Arc<FrameTracker>`，不标记脏。
    /// 只允许 UpToDate 或 Dirty 状态的页帧。
    pub fn frame_for_read(&self, page_index: usize) -> Result<Arc<FrameTracker>, SyscallErr> {
        let entry = self.get_or_create_entry(page_index, true)?;
        let state = entry.state();
        match state {
            PageState::UpToDate | PageState::Dirty => Ok(entry.page.clone()),
            PageState::Error => Err(SyscallErr::EIO),
            PageState::Loading | PageState::Writeback => Err(SyscallErr::EAGAIN),
        }
    }

    /// 获取页帧用于文件映射写（如 MAP_SHARED file-backed page fault）。
    /// 返回 PageCache 中的 `Arc<FrameTracker>`，自动标记脏页。
    /// 只允许 UpToDate 或 Dirty 状态的页帧。
    pub fn frame_for_write(&self, page_index: usize) -> Result<Arc<FrameTracker>, SyscallErr> {
        let entry = self.get_or_create_entry(page_index, true)?;
        let state = entry.state();
        if state != PageState::UpToDate && state != PageState::Dirty {
            return match state {
                PageState::Error => Err(SyscallErr::EIO),
                PageState::Loading | PageState::Writeback => Err(SyscallErr::EAGAIN),
                _ => Err(SyscallErr::EIO),
            };
        }
        let mut inner = self.inner.lock();
        inner.mark_dirty(page_index);
        if state == PageState::UpToDate {
            entry.set_state(PageState::Dirty);
        }
        Ok(entry.page.clone())
    }

    // ── 读取 ─────────────────────────────────────────────────────────

    /// 从指定偏移量读取数据
    /// 两阶段读取：持锁收集拷贝项 → 解锁拷贝数据
    pub fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        let _t0 = perf::perf_time_now();
        let miss_before = perf::PC_READ_MISS.load(core::sync::atomic::Ordering::Relaxed);
        if buf.is_empty() {
            let elapsed = perf::perf_time_now().wrapping_sub(_t0);
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
            let _t_lookup = perf::perf_time_now();
            let entry = self.get_page_for_read(start_page)?;
            let had_miss = perf::PC_READ_MISS.load(core::sync::atomic::Ordering::Relaxed) > miss_before;
            let lookup_cycles = perf::perf_time_now().wrapping_sub(_t_lookup);
            perf::record_pc_lookup_cycles(lookup_cycles);
            let _t_copy = perf::perf_time_now();
            let src = entry.as_slice();
            buf[..sub_len].copy_from_slice(&src[page_offset..page_offset + sub_len]);
            let copy_cycles = perf::perf_time_now().wrapping_sub(_t_copy);
            perf::record_pc_copy_cycles(copy_cycles);
            let total_cycles = perf::perf_time_now().wrapping_sub(_t0);
            if had_miss {
                perf::record_pc_read(1, total_cycles, 0, total_cycles);
            } else {
                perf::record_pc_read(1, total_cycles, total_cycles, 0);
            }
            return Ok(sub_len);
        }

        // Phase 1: 收集拷贝项（持锁）
        struct CopyItem {
            entry: Arc<PageEntry>,
            page_offset: usize,
            sub_len: usize,
        }

        let mut copies: Vec<CopyItem> = Vec::new();
        let mut total_read = 0usize;
        let mut pages = 0usize;

        let _t_lookup = perf::perf_time_now();
        for page_index in start_page..=end_page {
            let page_start = page_index << PAGE_SIZE_BITS;
            let page_end = page_start + PAGE_SIZE;
            let read_start = core::cmp::max(offset, page_start);
            let read_end = core::cmp::min(offset + buf.len(), page_end);
            let sub_len = read_end.saturating_sub(read_start);

            if sub_len == 0 {
                continue;
            }

            pages += 1;
            let entry = self.get_page_for_read(page_index)?;
            copies.push(CopyItem {
                entry,
                page_offset: read_start - page_start,
                sub_len,
            });
            total_read += sub_len;
        }
        let had_miss = perf::PC_READ_MISS.load(core::sync::atomic::Ordering::Relaxed) > miss_before;
        let lookup_cycles = perf::perf_time_now().wrapping_sub(_t_lookup);
        perf::record_pc_lookup_cycles(lookup_cycles);

        // Phase 2: 拷贝数据（无锁）
        let _t_copy = perf::perf_time_now();
        let mut dst_offset = 0;
        for item in &copies {
            let src = item.entry.as_slice();
            let src_start = item.page_offset;
            buf[dst_offset..dst_offset + item.sub_len]
                .copy_from_slice(&src[src_start..src_start + item.sub_len]);
            dst_offset += item.sub_len;
        }
        let copy_cycles = perf::perf_time_now().wrapping_sub(_t_copy);
        perf::record_pc_copy_cycles(copy_cycles);

        let total_cycles = perf::perf_time_now().wrapping_sub(_t0);
        if had_miss {
            perf::record_pc_read(pages, total_cycles, 0, total_cycles);
        } else {
            perf::record_pc_read(pages, total_cycles, total_cycles, 0);
        }
        Ok(total_read)
    }

    // ── 写入 ─────────────────────────────────────────────────────────

    /// 从指定偏移量写入数据
    /// 两阶段写入：持锁收集目标页 → 解锁写入数据
    pub fn write(&self, offset: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
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
            let entry = self.get_page_for_write_populate(start_page, !full_page_overwrite)?;
            let dst = entry.as_slice_mut();
            dst[page_offset..page_offset + sub_len].copy_from_slice(&buf[..sub_len]);
            perf::record_pc_write(1, full_page_overwrite, perf::perf_time_now().wrapping_sub(_t0));
            return Ok(sub_len);
        }

        struct CopyItem {
            entry: Arc<PageEntry>,
            page_offset: usize,
            sub_len: usize,
        }

        let mut copies: Vec<CopyItem> = Vec::new();
        let mut total_written = 0usize;
        let mut pages = 0usize;
        let mut any_full_overwrite = false;

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
            if full_page_overwrite { any_full_overwrite = true; }
            let entry = self.get_page_for_write_populate(page_index, !full_page_overwrite)?;
            copies.push(CopyItem {
                entry,
                page_offset,
                sub_len,
            });
            total_written += sub_len;
        }

        // Phase 2: 写入数据（无锁）
        let mut src_offset = 0;
        for item in &copies {
            let dst = item.entry.as_slice_mut();
            let dst_start = item.page_offset;
            dst[dst_start..dst_start + item.sub_len]
                .copy_from_slice(&buf[src_offset..src_offset + item.sub_len]);
            src_offset += item.sub_len;
        }

        perf::record_pc_write(pages, any_full_overwrite, perf::perf_time_now().wrapping_sub(_t0));
        Ok(total_written)
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
        if len == 0 {
            return Ok(0);
        }

        let start_page = offset >> PAGE_SIZE_BITS;
        let end_page = (offset + len - 1) >> PAGE_SIZE_BITS;

        // Single-page fast path: bypass Vec<CopyItem> construction
        if start_page == end_page {
            let page_start = start_page << PAGE_SIZE_BITS;
            let page_offset = offset - page_start;
            let sub_len = len.min(PAGE_SIZE - page_offset);
            let entry = self.get_page_for_read(start_page)?;
            let src = entry.as_slice();
            dst.write_at(0, &src[page_offset..page_offset + sub_len]);
            return Ok(sub_len);
        }

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
            let read_end = core::cmp::min(offset + len, page_end);
            let sub_len = read_end.saturating_sub(read_start);

            if sub_len == 0 {
                continue;
            }

            let entry = self.get_page_for_read(page_index)?;
            copies.push(CopyItem {
                entry,
                page_offset: read_start - page_start,
                sub_len,
            });
            total_read += sub_len;
        }

        // Phase 2: copy page data into UserBuffer (no locks held)
        let mut dst_offset = 0;
        for item in &copies {
            let src = item.entry.as_slice();
            let src_start = item.page_offset;
            dst.write_at(dst_offset, &src[src_start..src_start + item.sub_len]);
            dst_offset += item.sub_len;
        }

        Ok(total_read)
    }

    /// 从 UserBuffer 写入数据到指定偏移量。
    /// 两阶段写入：持锁收集目标页 → 解锁从 UserBuffer 拷贝。
    /// `len` 由调用者计算，不从此 buffer 的长度推断。
    pub fn write_user(
        &self,
        offset: usize,
        len: usize,
        src: &crate::mm::UserBuffer,
    ) -> Result<usize, SyscallErr> {
        if len == 0 {
            return Ok(0);
        }

        let start_page = offset >> PAGE_SIZE_BITS;
        let end_page = (offset + len - 1) >> PAGE_SIZE_BITS;

        // Single-page fast path: bypass Vec<CopyItem> construction
        if start_page == end_page {
            let page_start = start_page << PAGE_SIZE_BITS;
            let page_offset = offset - page_start;
            let sub_len = len.min(PAGE_SIZE - page_offset);
            let full_page_overwrite = page_offset == 0 && sub_len == PAGE_SIZE;
            let entry = self.get_page_for_write_populate(start_page, !full_page_overwrite)?;
            let dst = entry.as_slice_mut();
            src.read_at(0, &mut dst[page_offset..page_offset + sub_len]);
            return Ok(sub_len);
        }

        struct CopyItem {
            entry: Arc<PageEntry>,
            page_offset: usize,
            sub_len: usize,
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
            let entry = self.get_page_for_write_populate(page_index, !full_page_overwrite)?;
            copies.push(CopyItem {
                entry,
                page_offset: write_start - page_start,
                sub_len,
            });
            total_written += sub_len;
        }

        // Phase 2: copy data from UserBuffer into pages (no locks held)
        let mut src_offset = 0;
        for item in &copies {
            let dst = item.entry.as_slice_mut();
            let dst_start = item.page_offset;
            src.read_at(src_offset, &mut dst[dst_start..dst_start + item.sub_len]);
            src_offset += item.sub_len;
        }

        Ok(total_written)
    }

    // ── 脏页管理 ────────────────────────────────────────────────────

    /// 标记页面为脏
    pub fn mark_page_dirty(&self, page_index: usize) {
        self.inner.lock().mark_dirty(page_index);
    }

    /// 标记页面为正在写回
    pub fn mark_page_writeback(&self, page_index: usize) {
        let mut inner = self.inner.lock();
        inner.clear_dirty(page_index);
    }

    // ── 回写 ─────────────────────────────────────────────────────────

    /// 单次回写批次的最大页面数
    const MAX_WRITEBACK_PAGES: usize = 32;

    /// 写回单个页面
    pub fn writeback_page(&self, page_index: usize) -> Result<(), SyscallErr> {
        let _t0 = perf::perf_time_now();
        let entry = {
            let entries = self.entries.lock();
            if page_index >= entries.len() {
                perf::record_pc_writeback(0, perf::perf_time_now().wrapping_sub(_t0));
                return Ok(());
            }
            match &entries[page_index] {
                Some(e) => e.clone(),
                None => {
                    perf::record_pc_writeback(0, perf::perf_time_now().wrapping_sub(_t0));
                    return Ok(());
                }
            }
        };

        if entry.state() != PageState::Dirty {
            perf::record_pc_writeback(0, perf::perf_time_now().wrapping_sub(_t0));
            return Ok(());
        }

        // 标记为 Writeback
        entry.set_state(PageState::Writeback);

        let result = if let Some(backend) = self.backend() {
            let data = entry.as_slice();
            let result = backend.write_page(page_index, data);
            match result {
                Ok(_) => {
                    entry.set_state(PageState::UpToDate);
                    self.inner.lock().clear_dirty(page_index);
                    Ok(())
                }
                Err(e) => {
                    // Writeback failed: keep page Dirty so it can be retried
                    entry.set_state(PageState::Dirty);
                    Err(e)
                }
            }
        } else {
            entry.set_state(PageState::UpToDate);
            self.inner.lock().clear_dirty(page_index);
            Ok(())
        };

        perf::record_pc_writeback(1, perf::perf_time_now().wrapping_sub(_t0));
        result
    }

    /// 批量写回一段连续的脏页
    ///
    /// `start..start+count` 范围内只对实际标记为 Dirty 的页面执行写回；
    /// 非 Dirty 的页面被跳过。批次中至少一个页面被写入时，调用
    /// `backend.write_pages()` 批量提交；否则直接返回 Ok。
    fn writeback_pages_run(&self, start: usize, count: usize) -> Result<(), SyscallErr> {
        let _t0 = perf::perf_time_now();

        // 第一阶段：持有 entries 锁，收集 Dirty 页面，标记为 Writeback
        let mut page_slices: Vec<(usize, Arc<PageEntry>)> = Vec::new();
        {
            let entries = self.entries.lock();
            let end = (start + count).min(entries.len());
            for i in start..end {
                if let Some(entry) = &entries[i] {
                    if entry.state() == PageState::Dirty {
                        entry.set_state(PageState::Writeback);
                        page_slices.push((i, entry.clone()));
                    }
                }
            }
        }

        if page_slices.is_empty() {
            return Ok(());
        }

        // 构建连续的 &[u8] 切片（start 即为第一个 Dirty 页的实际索引）
        // 注意：调用者保证该范围内不存在非 Dirty 页空洞，否则需拆分 run
        let actual_start = page_slices[0].0;
        let slices: Vec<&[u8]> = page_slices.iter().map(|(_, e)| e.as_slice()).collect();

        let result = if let Some(backend) = self.backend() {
            let write_result = backend.write_pages(actual_start, &slices);
            match write_result {
                Ok(_) => {
                    let mut inner = self.inner.lock();
                    for (idx, entry) in &page_slices {
                        entry.set_state(PageState::UpToDate);
                        inner.clear_dirty(*idx);
                    }
                    Ok(())
                }
                Err(e) => {
                    // 写回失败：恢复为 Dirty 状态以便重试
                    for (_, entry) in &page_slices {
                        entry.set_state(PageState::Dirty);
                    }
                    Err(e)
                }
            }
        } else {
            let mut inner = self.inner.lock();
            for (idx, entry) in &page_slices {
                entry.set_state(PageState::UpToDate);
                inner.clear_dirty(*idx);
            }
            Ok(())
        };

        perf::record_pc_writeback(slices.len(), perf::perf_time_now().wrapping_sub(_t0));
        result
    }

    /// 写回所有脏页
    pub fn writeback_all(&self) -> Result<(), SyscallErr> {
        let dirty_indices: Vec<usize> = {
            let inner = self.inner.lock();
            inner.dirty_pages.iter().copied().collect()
        };

        if dirty_indices.is_empty() {
            return Ok(());
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
        Ok(())
    }

    /// 写回指定范围的脏页
    pub fn writeback_range(&self, start_index: usize, end_index: usize) -> Result<(), SyscallErr> {
        let dirty_indices: Vec<usize> = {
            let inner = self.inner.lock();
            inner
                .dirty_pages
                .range(start_index..=end_index)
                .copied()
                .collect()
        };

        if dirty_indices.is_empty() {
            return Ok(());
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
        Ok(())
    }

    // ── 截断与失效 ──────────────────────────────────────────────────

    /// 截断 page cache 到指定大小
    pub fn truncate(&self, new_size: usize) -> Result<(), SyscallErr> {
        let hole_start_page = (new_size + PAGE_SIZE - 1) >> PAGE_SIZE_BITS;

        // 收集需要移除的页面索引
        let to_remove: Vec<usize> = {
            let entries = self.entries.lock();
            (hole_start_page..entries.len()).collect()
        };

        let mut entries = self.entries.lock();
        let mut inner = self.inner.lock();
        for page_index in to_remove {
            if page_index < entries.len() {
                entries[page_index] = None;
            }
            inner.pages.remove(&page_index);
            inner.dirty_pages.remove(&page_index);
        }

        Ok(())
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
        // 先检查范围内是否有脏页
        {
            let inner = self.inner.lock();
            for page_index in start_index..end_index {
                if inner.dirty_pages.contains(&page_index) {
                    return Err(SyscallErr::EBUSY);
                }
            }
        }

        let mut entries = self.entries.lock();
        let mut inner = self.inner.lock();
        let mut invalidated = 0;

        let end = core::cmp::min(end_index, entries.len());
        for page_index in start_index..end {
            if entries[page_index].is_some() {
                entries[page_index] = None;
                inner.pages.remove(&page_index);
                inner.dirty_pages.remove(&page_index);
                invalidated += 1;
            }
        }

        Ok(invalidated)
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
    /// 创建新的块设备后端
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
                        .read_block(blk_id, &mut blk_buf);
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
                    .read_block(blk_id, &mut blk_buf);
                blk_buf[blk_off..blk_off + self.block_size]
                    .copy_from_slice(&buf[start..start + self.block_size]);
                self.fs
                    .block_device
                    .write_block(blk_id, &blk_buf);
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

/// EXT4 文件系统专用 PageCache 后端
///
/// 通过弱引用访问 Ext4FileSystem + inode_num，将页面偏移动态映射为物理块号。
/// 仅用于普通文件数据，不用于元数据（目录/bitmap/inode table）。
pub struct Ext4PageCacheBackend {
    ext4fs: alloc::sync::Weak<crate::fs::ext4::ext4fs::Ext4FileSystem>,
    inode_num: u32,
    block_size: usize,
    blocks_per_page: usize,
}

impl Ext4PageCacheBackend {
    pub fn new(
        ext4fs: alloc::sync::Weak<crate::fs::ext4::ext4fs::Ext4FileSystem>,
        inode_num: u32,
    ) -> Self {
        let fs = ext4fs.upgrade().expect("Ext4PageCacheBackend: ext4fs dropped");
        let block_size = fs.block_size;
        let blocks_per_page = crate::config::PAGE_SIZE / block_size;
        Ext4PageCacheBackend {
            ext4fs: ext4fs.clone(),
            inode_num,
            block_size,
            blocks_per_page,
        }
    }

    fn block_id_for_offset(&self, page_index: usize, block_off: usize) -> Option<usize> {
        let fs = self.ext4fs.upgrade()?;
        let ino_ref = fs.get_inode_ref(self.inode_num);
        let lblock = (page_index * self.blocks_per_page + block_off) as u32;
        match fs.get_pblock_idx(&ino_ref, lblock) {
            Ok(pblock) => Some(pblock as usize),
            Err(_) => None, // hole
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
                    crate::fs::ext4::counters::inc_counter!(crate::fs::ext4::counters::DATA_BLOCK_READ);
                    crate::fs::ext4::counters::inc_counter!(crate::fs::ext4::counters::BLOCK_READ_TOTAL);
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
                    crate::fs::ext4::counters::inc_counter!(crate::fs::ext4::counters::DATA_BLOCK_WRITE);
                    crate::fs::ext4::counters::inc_counter!(crate::fs::ext4::counters::BLOCK_WRITE_TOTAL);
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
        // 只有 block_size == PAGE_SIZE 时才能做物理块级的批量合并
        if self.blocks_per_page != 1 {
            let mut written = 0;
            for (i, page) in pages.iter().enumerate() {
                written += self.write_page(start_index + i, page)?;
            }
            return Ok(written);
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
        }

        Ok(pages.len() * crate::config::PAGE_SIZE)
    }
}
