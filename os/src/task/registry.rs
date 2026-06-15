use super::{ProcessControlBlock, TaskControlBlock, TaskStatus};
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

pub fn register_task(task: &Arc<TaskControlBlock>) {
    TASK_REGISTRY
        .lock()
        .tasks
        .insert(task.tid.0, Arc::downgrade(task));
}

pub fn unregister_task(tid: usize) {
    TASK_REGISTRY.lock().tasks.remove(&tid);
}

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

pub fn register_process(process: &Arc<ProcessControlBlock>) {
    TASK_REGISTRY
        .lock()
        .processes
        .insert(process.pid, Arc::downgrade(process));
}

pub fn unregister_process(pid: usize) {
    TASK_REGISTRY.lock().processes.remove(&pid);
}

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
    let is_zombie = { task.acquire_inner_lock().task_status == TaskStatus::Zombie };
    if is_zombie {
        None
    } else {
        Some(task)
    }
}

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

pub fn find_processes_by_pgid(pgid: usize) -> Vec<Arc<ProcessControlBlock>> {
    all_processes()
        .into_iter()
        .filter(|process| process.getpgid() == pgid)
        .collect()
}

pub fn find_task_by_pid_tid(pid: usize, tid: usize) -> Option<Arc<TaskControlBlock>> {
    find_task_by_tid(tid).filter(|task| task.pid() == pid)
}
