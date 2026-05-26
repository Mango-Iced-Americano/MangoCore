use crate::hal::{trap_cx_bottom_from_tid, ustack_bottom_from_tid};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use lazy_static::*;
use spin::Mutex;

/// 用于分配可回收 id 的结构体
#[derive(Clone)]
pub struct RecycleAllocator {
    /// 当前分配的id
    current: usize,
    /// 存储已经回收的id，供后续分配使用
    recycled: Vec<usize>,
}

impl RecycleAllocator {
    /// 构造函数
    pub fn new() -> Self {
        RecycleAllocator {
            // 当前分配的id数量初始化为0
            current: 1,
            // 初始化为空向量
            recycled: Vec::new(),
        }
    }
    /// 分配一个新的id
    pub fn alloc(&mut self) -> usize {
        // 从回收的id中取出一个，如果没有则分配一个新的
        if let Some(id) = self.recycled.pop() {
            id
        } else {
            // 当前分配的id数量加1
            self.current += 1;
            // 返回分配的id号
            self.current - 1
        }
    }
    /// 分配一个新的id，不立即复用已回收id。
    ///
    /// 用户可见 pid/tid 过早复用会让并发创建线程的测试观察到重复 TID。
    pub fn alloc_fresh(&mut self) -> usize {
        self.current += 1;
        self.current - 1
    }
    pub fn last_allocated(&self) -> usize {
        self.current.saturating_sub(1)
    }
    pub fn set_next_alloc_hint(&mut self, next: usize) {
        let next = next.max(1);
        if next >= self.current {
            self.current = next;
            return;
        }
        if let Some(pos) = self.recycled.iter().position(|id| *id == next) {
            let id = self.recycled.remove(pos);
            self.recycled.push(id);
        }
    }
    /// 回收一个id
    pub fn dealloc(&mut self, id: usize) {
        // 检查id是否合法
        assert!(id < self.current);
        // 检查id是否已经被回收
        assert!(
            !self.recycled.iter().any(|i| *i == id),
            "id {} has been deallocated!",
            id
        );
        // 将id回收，放入回收向量中
        self.recycled.push(id);
    }
    /// 获取已经分配的id数量
    pub fn get_allocated(&self) -> usize {
        // 返回当前分配的id数量减去已经回收的id数量
        self.current - self.recycled.len()
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
        if !self.1.swap(true, Ordering::AcqRel) {
            TID_ALLOCATOR.lock().dealloc(self.0);
        }
    }

    pub fn is_released(&self) -> bool {
        self.1.load(Ordering::Acquire)
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
