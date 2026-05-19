use super::{registry, TaskControlBlock, TaskStatus};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use spin::{Mutex, MutexGuard};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessState {
    Running,
    Zombie,
}

pub struct ProcessControlBlock {
    /// 用户可见进程 ID，即 getpid() 返回值。
    pub pid: usize,
    /// 线程组主线程 tid。
    pub leader_tid: usize,
    /// 属于该进程的线程列表。
    pub threads: Mutex<Vec<Weak<TaskControlBlock>>>,
    inner: Mutex<ProcessInner>,
}

pub struct ProcessInner {
    /// 进程组 ID。
    pub pgid: usize,
    /// 父进程。
    pub parent: Option<Weak<ProcessControlBlock>>,
    /// 子进程。
    pub children: Vec<Arc<ProcessControlBlock>>,
    /// 进程级生命周期状态。
    pub state: ProcessState,
    /// wait4 可回收的进程退出码。
    pub exit_code: u32,
}

impl ProcessControlBlock {
    pub fn new(
        pid: usize,
        leader_tid: usize,
        pgid: usize,
        parent: Option<Weak<ProcessControlBlock>>,
    ) -> Self {
        Self {
            pid,
            leader_tid,
            threads: Mutex::new(Vec::new()),
            inner: Mutex::new(ProcessInner {
                pgid,
                parent,
                children: Vec::new(),
                state: ProcessState::Running,
                exit_code: 0,
            }),
        }
    }

    pub fn acquire_inner_lock(&self) -> MutexGuard<ProcessInner> {
        self.inner.lock()
    }

    pub fn add_thread(&self, task: &Arc<TaskControlBlock>) {
        self.threads.lock().push(Arc::downgrade(task));
    }

    pub fn remove_thread(&self, tid: usize) {
        self.threads.lock().retain(|thread| {
            thread
                .upgrade()
                .map(|task| task.tid.0 != tid)
                .unwrap_or(false)
        });
    }

    pub fn threads(&self) -> Vec<Arc<TaskControlBlock>> {
        let mut threads = self.threads.lock();
        let mut live_threads = Vec::new();
        threads.retain(|thread| {
            if let Some(task) = thread.upgrade() {
                live_threads.push(task);
                true
            } else {
                false
            }
        });
        live_threads
    }

    pub fn any_live_thread(&self) -> Option<Arc<TaskControlBlock>> {
        self.threads().into_iter().find(|task| {
            let inner = task.acquire_inner_lock();
            inner.task_status != TaskStatus::Zombie
        })
    }

    pub fn live_thread_count(&self) -> usize {
        self.threads()
            .into_iter()
            .filter(|task| task.acquire_inner_lock().task_status != TaskStatus::Zombie)
            .count()
    }

    pub fn setpgid(&self, pgid: usize) -> isize {
        if (pgid as isize) < 0 {
            return -1;
        }
        self.inner.lock().pgid = pgid;
        0
    }

    pub fn getpgid(&self) -> usize {
        self.inner.lock().pgid
    }

    pub fn parent(&self) -> Option<Arc<ProcessControlBlock>> {
        self.inner
            .lock()
            .parent
            .as_ref()
            .and_then(|parent| parent.upgrade())
    }

    pub fn parent_pid(&self) -> usize {
        self.parent().map(|parent| parent.pid).unwrap_or(0)
    }

    pub fn is_zombie(&self) -> bool {
        self.inner.lock().state == ProcessState::Zombie
    }

    pub fn mark_zombie(&self, exit_code: u32) -> bool {
        let mut inner = self.inner.lock();
        if inner.state == ProcessState::Zombie {
            return false;
        }
        inner.state = ProcessState::Zombie;
        inner.exit_code = exit_code;
        true
    }

    pub fn exit_code(&self) -> u32 {
        self.inner.lock().exit_code
    }

    pub fn add_child(&self, child: Arc<ProcessControlBlock>) -> Result<(), isize> {
        let mut inner = self.inner.lock();
        if inner.children.try_reserve(1).is_err() {
            return Err(crate::syscall::errno::ENOMEM);
        }
        inner.children.push(child);
        Ok(())
    }

    pub fn detach_child(&self, child_pid: usize) {
        self.inner
            .lock()
            .children
            .retain(|child| child.pid != child_pid);
    }

    pub fn set_parent(&self, parent: Option<Weak<ProcessControlBlock>>) {
        self.inner.lock().parent = parent;
    }
}

impl Drop for ProcessControlBlock {
    fn drop(&mut self) {
        registry::unregister_process(self.pid);
    }
}
