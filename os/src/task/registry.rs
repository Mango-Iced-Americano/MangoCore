//! 全局任务/进程弱引用注册表。
//!
//! 注册表用于按 TID/PID 查找仍存活的 `TaskControlBlock` 和 `ProcessControlBlock`。
//! 条目保存 `Weak`，因此不会延长任务或进程生命周期；查找时顺手清理失效条目。
//!
//! # Locking
//!
//! `TASK_REGISTRY` 只保护映射表本身。函数返回 `Arc` 后立即释放注册表锁，
//! 调用方再获取任务/进程内部锁，避免注册表锁参与长锁链。

use super::{ProcessControlBlock, TaskControlBlock};
use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

struct TaskRegistry {
    tasks: BTreeMap<usize, Weak<TaskControlBlock>>,
    processes: BTreeMap<usize, Weak<ProcessControlBlock>>,
}

impl TaskRegistry {
    fn new() -> Self {
        Self {
            tasks: BTreeMap::new(),
            processes: BTreeMap::new(),
        }
    }
}

lazy_static! {
    static ref TASK_REGISTRY: Mutex<TaskRegistry> = Mutex::new(TaskRegistry::new());
}

/// 注册一个任务的 TID 到 TCB 弱引用映射。
pub fn register_task(task: &Arc<TaskControlBlock>) {
    TASK_REGISTRY
        .lock()
        .tasks
        .insert(task.tid.0, Arc::downgrade(task));
}

/// 无条件移除指定 TID 的任务注册项。
pub fn unregister_task(tid: usize) {
    TASK_REGISTRY.lock().tasks.remove(&tid);
}

/// 仅当注册项仍指向同一个 TCB 时移除任务。
///
/// # Semantics
///
/// 该函数用于 Drop 路径，避免旧 TCB 析构时误删已经复用同一 TID 的新任务。
pub fn unregister_task_if_match(task: &TaskControlBlock) {
    let mut registry = TASK_REGISTRY.lock();
    let remove = match registry
        .tasks
        .get(&task.tid.0)
        .and_then(|entry| entry.upgrade())
    {
        Some(registered) => core::ptr::eq(Arc::as_ptr(&registered), task as *const _),
        None => true,
    };
    if remove {
        registry.tasks.remove(&task.tid.0);
    }
}

/// 注册一个进程的 PID 到 PCB 弱引用映射。
pub fn register_process(process: &Arc<ProcessControlBlock>) {
    TASK_REGISTRY
        .lock()
        .processes
        .insert(process.pid, Arc::downgrade(process));
}

/// 无条件移除指定 PID 的进程注册项。
pub fn unregister_process(pid: usize) {
    TASK_REGISTRY.lock().processes.remove(&pid);
}

/// 仅当注册项仍指向同一个 PCB 时移除进程。
pub fn unregister_process_if_match(process: &ProcessControlBlock) {
    let mut registry = TASK_REGISTRY.lock();
    let remove = match registry
        .processes
        .get(&process.pid)
        .and_then(|entry| entry.upgrade())
    {
        Some(registered) => core::ptr::eq(Arc::as_ptr(&registered), process as *const _),
        None => true,
    };
    if remove {
        registry.processes.remove(&process.pid);
    }
}

/// 按 TID 查找非 zombie 任务。
///
/// # Semantics
///
/// zombie TCB 可能仍在调度队列或等待回收队列中，用户可见查找应返回 `None`。
pub fn find_task_by_tid(tid: usize) -> Option<Arc<TaskControlBlock>> {
    let task = {
        let mut registry = TASK_REGISTRY.lock();
        match registry.tasks.get(&tid).and_then(|task| task.upgrade()) {
            Some(task) => task,
            None => {
                registry.tasks.remove(&tid);
                return None;
            }
        }
    };
    let is_zombie = task.is_zombie();
    if is_zombie {
        None
    } else {
        Some(task)
    }
}

/// 按 PID 查找进程。
pub fn find_process_by_pid(pid: usize) -> Option<Arc<ProcessControlBlock>> {
    let mut registry = TASK_REGISTRY.lock();
    match registry
        .processes
        .get(&pid)
        .and_then(|process| process.upgrade())
    {
        Some(process) => Some(process),
        None => {
            registry.processes.remove(&pid);
            None
        }
    }
}

/// 返回所有仍可升级的进程引用，并清理失效注册项。
pub fn all_processes() -> Vec<Arc<ProcessControlBlock>> {
    let mut registry = TASK_REGISTRY.lock();
    let mut stale = Vec::new();
    let mut processes = Vec::new();
    for (&pid, process) in registry.processes.iter() {
        if let Some(process) = process.upgrade() {
            processes.push(process);
        } else {
            stale.push(pid);
        }
    }
    for pid in stale {
        registry.processes.remove(&pid);
    }
    processes
}

/// 返回指定进程组中的进程。
pub fn find_processes_by_pgid(pgid: usize) -> Vec<Arc<ProcessControlBlock>> {
    all_processes()
        .into_iter()
        .filter(|process| process.getpgid() == pgid)
        .collect()
}

/// 按 PID/TID 查找属于指定进程的任务。
pub fn find_task_by_pid_tid(pid: usize, tid: usize) -> Option<Arc<TaskControlBlock>> {
    find_task_by_tid(tid).filter(|task| task.pid() == pid)
}
