//! PID/TID 分配和用户资源槽位地址计算。
//!
//! MangoCore 使用同一个可回收分配器管理用户可见 TID，以及同一地址空间内的
//! trap context / 默认用户栈槽位编号。TID 复用策略偏向延迟复用，以降低并发
//! clone/fork 测试观察到重复 ID 的概率。

use crate::hal::{trap_cx_bottom_from_tid, ustack_bottom_from_tid};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use lazy_static::*;
use spin::Mutex;

/// Matches the `/proc/sys/kernel/pid_max` value exposed by procfs.
pub const DEFAULT_PID_MAX: usize = 32_768;
const RESERVED_PID_REUSE_FLOOR: usize = 300;
const FRESH_REUSE_WATERMARK: usize = DEFAULT_PID_MAX - 1024;

/// 可回收整数 ID 分配器。
///
/// # Semantics
///
/// `alloc()` 会优先消费回收列表；`alloc_fresh()` 在 PID/TID 路径上尽量保持
/// 单调增长，仅在接近 `pid_max` 或收到 `ns_last_pid` hint 时复用旧 ID。
pub struct RecycleAllocator {
    /// 下一个线性分配 ID。
    current: usize,
    /// 已回收 ID 的栈。
    recycled: Vec<usize>,
    /// O(1) membership bitmap for `recycled`.
    recycled_flags: Vec<bool>,
    /// One-shot reuse request from `/proc/sys/kernel/ns_last_pid`.
    fresh_reuse_hint: Option<usize>,
}

impl Clone for RecycleAllocator {
    fn clone(&self) -> Self {
        Self {
            current: self.current,
            recycled: self.recycled.clone(),
            recycled_flags: self.recycled_flags.clone(),
            fresh_reuse_hint: self.fresh_reuse_hint,
        }
    }
}

impl RecycleAllocator {
    /// 创建从 ID 1 开始分配的新分配器。
    pub fn new() -> Self {
        RecycleAllocator {
            current: 1,
            recycled: Vec::new(),
            recycled_flags: Vec::new(),
            fresh_reuse_hint: None,
        }
    }

    /// 分配一个 ID，允许立即复用回收 ID。
    pub fn alloc(&mut self) -> usize {
        if let Some(id) = self.alloc_recycled() {
            return id;
        }
        self.current += 1;
        let id = self.current - 1;
        self.ensure_flag_capacity(id);
        id
    }

    /// 分配一个新的id，不立即复用已回收id。
    ///
    /// Linux/DragonOS 在 PID 空间高位使用循环分配并跳过低位保留 PID。
    /// 这里保留线性快路径，避免用户可见 pid/tid 过早复用让并发创建
    /// 线程的测试观察到重复 TID；接近 pid_max 后再消费已释放 ID。
    pub fn alloc_fresh(&mut self) -> usize {
        if let Some(id) = self.fresh_reuse_hint.take() {
            if id < self.current && self.is_recycled(id) {
                self.mark_recycled(id, false);
                return id;
            }
        }
        if self.current >= FRESH_REUSE_WATERMARK {
            if let Some(id) = self.alloc_recycled_for_fresh() {
                return id;
            }
        }
        self.current += 1;
        let id = self.current - 1;
        self.ensure_flag_capacity(id);
        id
    }

    /// 返回最近一次线性分配或 hint 设置后用户可见的最后 PID。
    pub fn last_allocated(&self) -> usize {
        self.current.saturating_sub(1)
    }

    /// 设置下一次新分配的 hint，用于 `/proc/sys/kernel/ns_last_pid`。
    ///
    /// # Semantics
    ///
    /// hint 大于等于当前水位时直接推进水位；hint 指向已回收 ID 时，仅下一次
    /// `alloc_fresh()` 可消费它。
    pub fn set_next_alloc_hint(&mut self, next: usize) {
        let next = next.max(1);
        if next >= self.current {
            self.current = next;
            self.ensure_flag_capacity(next);
            self.fresh_reuse_hint = None;
            return;
        }
        if self.is_recycled(next) {
            self.fresh_reuse_hint = Some(next);
        }
    }

    /// 回收一个 ID，使其可被普通 `alloc()` 立即复用。
    ///
    /// # Panics
    ///
    /// ID 未分配或重复回收时 panic。调用方必须保证 `TidHandle`/槽位生命周期
    /// 只释放一次。
    pub fn dealloc(&mut self, id: usize) {
        assert!(id < self.current);
        assert!(!self.is_recycled(id), "id {} has been deallocated!", id);
        self.mark_recycled(id, true);
        self.recycled.push(id);
    }
    /// Mark an ID allocated by `alloc_fresh()` as reusable only by an explicit
    /// `ns_last_pid` hint or by the high-watermark cyclic PID path.
    pub fn release_fresh_id(&mut self, id: usize) {
        assert!(id < self.current);
        if !self.is_recycled(id) {
            self.mark_recycled(id, true);
            self.recycled.push(id);
        }
    }

    /// 返回当前已分配且尚未回收的 ID 数量。
    pub fn get_allocated(&self) -> usize {
        let recycled_count = self.recycled_flags.iter().filter(|flag| **flag).count();
        self.current
            .saturating_sub(1)
            .saturating_sub(recycled_count)
    }

    fn ensure_flag_capacity(&mut self, id: usize) {
        if id >= self.recycled_flags.len() {
            self.recycled_flags.resize(id + 1, false);
        }
    }

    fn is_recycled(&self, id: usize) -> bool {
        self.recycled_flags.get(id).copied().unwrap_or(false)
    }

    fn mark_recycled(&mut self, id: usize, value: bool) {
        self.ensure_flag_capacity(id);
        self.recycled_flags[id] = value;
    }

    fn alloc_recycled(&mut self) -> Option<usize> {
        while let Some(id) = self.recycled.pop() {
            if self.is_recycled(id) {
                self.mark_recycled(id, false);
                return Some(id);
            }
        }
        None
    }

    fn alloc_recycled_for_fresh(&mut self) -> Option<usize> {
        let mut skipped_reserved = Vec::new();
        let mut allocated = None;
        while let Some(id) = self.recycled.pop() {
            if !self.is_recycled(id) {
                continue;
            }
            if id >= RESERVED_PID_REUSE_FLOOR {
                self.mark_recycled(id, false);
                allocated = Some(id);
                break;
            } else {
                skipped_reserved.push(id);
            }
        }
        while let Some(id) = skipped_reserved.pop() {
            self.recycled.push(id);
        }
        allocated
    }
}

lazy_static! {
    /// 全局 TID 分配器。
    ///
    /// # Locking
    ///
    /// 所有 TID 分配和释放必须通过这把 `Mutex` 串行化。
    static ref TID_ALLOCATOR: Mutex<RecycleAllocator> = Mutex::new(RecycleAllocator::new());
}

/// 用户可见的线程 ID 句柄，即 gettid() 返回的值。
pub struct TidHandle(pub usize, AtomicBool);

impl TidHandle {
    /// 释放 TID。
    ///
    /// # Semantics
    ///
    /// 该操作幂等；第一次释放会把 ID 记入延迟复用集合，后续调用无副作用。
    pub fn release(&self) {
        if !self.1.swap(true, Ordering::Relaxed) {
            // Normal tid_alloc() stays monotonic until the high watermark, but
            // ns_last_pid and long-running suites need released IDs recorded.
            TID_ALLOCATOR.lock().release_fresh_id(self.0);
        }
    }

    /// 返回该 TID 是否已经释放。
    pub fn is_released(&self) -> bool {
        self.1.load(Ordering::Relaxed)
    }
}

/// 分配一个用户可见 tid。
pub fn tid_alloc() -> Arc<TidHandle> {
    Arc::new(TidHandle(
        TID_ALLOCATOR.lock().alloc_fresh(),
        AtomicBool::new(false),
    ))
}

/// 返回 `/proc/sys/kernel/ns_last_pid` 兼容值。
pub fn ns_last_pid() -> usize {
    TID_ALLOCATOR.lock().last_allocated()
}

/// 设置下一次 TID 分配 hint。
pub fn set_ns_last_pid(last_pid: usize) {
    TID_ALLOCATOR
        .lock()
        .set_next_alloc_hint(last_pid.saturating_add(1));
}

impl Drop for TidHandle {
    fn drop(&mut self) {
        self.release();
    }
}

/// 根据地址空间内用户资源槽位计算 trap context 地址。
pub fn trap_cx_bottom_from_slot(slot: usize) -> usize {
    trap_cx_bottom_from_tid(slot)
}

/// 根据地址空间内用户资源槽位计算默认用户栈底地址。
pub fn ustack_bottom_from_slot(slot: usize) -> usize {
    ustack_bottom_from_tid(slot)
}
