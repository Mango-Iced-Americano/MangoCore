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

/// 用于分配可回收 id 的结构体
pub struct RecycleAllocator {
    /// 当前分配的id
    current: usize,
    /// 存储已经回收的id，供后续分配使用
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
    /// 构造函数
    pub fn new() -> Self {
        RecycleAllocator {
            // 当前分配的id数量初始化为0
            current: 1,
            // 初始化为空向量
            recycled: Vec::new(),
            recycled_flags: Vec::new(),
            fresh_reuse_hint: None,
        }
    }
    /// 分配一个新的id
    pub fn alloc(&mut self) -> usize {
        // 从回收的id中取出一个，如果没有则分配一个新的
        if let Some(id) = self.alloc_recycled() {
            return id;
        }
        // 当前分配的id数量加1
        self.current += 1;
        let id = self.current - 1;
        self.ensure_flag_capacity(id);
        // 返回分配的id号
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
    pub fn last_allocated(&self) -> usize {
        self.current.saturating_sub(1)
    }
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
    /// 回收一个id
    pub fn dealloc(&mut self, id: usize) {
        // 检查id是否合法
        assert!(id < self.current);
        // 检查id是否已经被回收
        assert!(!self.is_recycled(id), "id {} has been deallocated!", id);
        // 将id回收，放入回收向量中
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
    /// 获取已经分配的id数量
    pub fn get_allocated(&self) -> usize {
        // 返回当前分配的id数量减去已经回收的id数量
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
    /// 全局 tid 分配器对象，使用 Mutex 保证线程安全
    static ref TID_ALLOCATOR: Mutex<RecycleAllocator> = Mutex::new(RecycleAllocator::new());
}

/// 用户可见的线程 ID 句柄，即 gettid() 返回的值。
pub struct TidHandle(pub usize, AtomicBool);

impl TidHandle {
    pub fn release(&self) {
        if !self.1.swap(true, Ordering::Relaxed) {
            // Normal tid_alloc() stays monotonic until the high watermark, but
            // ns_last_pid and long-running suites need released IDs recorded.
            TID_ALLOCATOR.lock().release_fresh_id(self.0);
        }
    }

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

pub fn ns_last_pid() -> usize {
    TID_ALLOCATOR.lock().last_allocated()
}

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
