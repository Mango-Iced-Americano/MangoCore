use crate::hal::{trap_cx_bottom_from_tid, ustack_bottom_from_tid};
use alloc::vec::Vec;
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
pub struct TidHandle(pub usize);

/// 分配一个用户可见 tid。
pub fn tid_alloc() -> TidHandle {
    TidHandle(TID_ALLOCATOR.lock().alloc())
}

impl Drop for TidHandle {
    fn drop(&mut self) {
        TID_ALLOCATOR.lock().dealloc(self.0);
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
