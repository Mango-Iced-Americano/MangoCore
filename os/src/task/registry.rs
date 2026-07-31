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
        .insert(task.gettid(), Arc::downgrade(task));
}

/// 仅当注册项仍指向同一个 TCB 时移除任务。
///
/// # Semantics
///
/// 该函数用于 Drop 路径，避免旧 TCB 析构时误删已经复用同一 TID 的新任务。
pub fn unregister_task_if_match(task: &TaskControlBlock) {
    let tid = task.gettid();
    let mut registry = TASK_REGISTRY.lock();
    let remove = match registry
        .tasks
        .get(&tid)
        .and_then(|entry| entry.upgrade())
    {
        Some(registered) => core::ptr::eq(Arc::as_ptr(&registered), task as *const _),
        None => true,
    };
    if remove {
        registry.tasks.remove(&tid);
    }
}

/// 让非 leader exec 调用者接管进程 PID，并同步重键任务注册表。
///
/// 如果旧 leader TCB 尚在 zombie queue 中，它接管调用者的旧 TID handle；
/// 如果已经析构，旧 handle 会在注册表锁外释放。这样任意时刻都只有一个 TCB
/// 拥有进程 PID，迟到的旧 TCB 析构也不会删除新 leader 的注册项。
pub(crate) fn exchange_exec_tids(owner: &TaskControlBlock) {
    let old_tid = owner.gettid();
    let leader_handle = owner.process.pid_handle();
    let leader_tid = leader_handle.0;
    assert_ne!(old_tid, leader_tid, "leader exec must not exchange its TID");

    let (former_leader, displaced_handle) = {
        let mut registry = TASK_REGISTRY.lock();
        let owner_ref = registry
            .tasks
            .get(&old_tid)
            .and_then(Weak::upgrade)
            .expect("exec owner is missing from task registry");
        assert!(
            core::ptr::eq(Arc::as_ptr(&owner_ref), owner as *const _),
            "exec owner TID points to another task"
        );

        let former_leader = registry.tasks.get(&leader_tid).and_then(Weak::upgrade);
        if let Some(task) = former_leader.as_ref() {
            assert!(
                Arc::ptr_eq(&task.process, &owner.process),
                "process PID points to another process task"
            );
            assert!(task.is_zombie(), "exec replaced a live group leader");
        }

        let old_owner_handle = owner.replace_tid(old_tid, leader_handle);
        let displaced_handle = if let Some(task) = former_leader.as_ref() {
            task.replace_tid(leader_tid, old_owner_handle)
        } else {
            old_owner_handle
        };

        registry.tasks.remove(&old_tid);
        registry
            .tasks
            .insert(leader_tid, Arc::downgrade(&owner_ref));
        (former_leader, displaced_handle)
    };

    // Arc/TidHandle 的析构可能回入 registry 或 TID allocator，必须位于锁外。
    drop(former_leader);
    drop(displaced_handle);
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
