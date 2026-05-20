use super::{
    pid::RecycleAllocator,
    registry,
    signal::{PendingSignal, SignalQueue, Sighand, Signals},
    threads::Futex,
    FsStatus, TaskControlBlock, TaskStatus, WaitQueue, WaitResult,
};
use crate::fs::vfs;
use crate::mm::{AddressSpace, PageTableImpl};
use alloc::sync::{Arc, Weak};
use alloc::string::String;
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
    /// 父进程 wait4() 等待子进程退出的等待队列。
    pub child_exit_wait: Mutex<WaitQueue>,
    /// CLONE_VFORK completion。父线程等待子进程 exec 成功或 exit。
    vfork: Mutex<VforkState>,
    inner: Mutex<ProcessInner>,
    signal: Mutex<ProcessSignalState>,
}

struct VforkState {
    /// CLONE_VFORK 父线程。Some 表示当前进程来自 vfork，且尚未完成。
    parent: Option<Weak<TaskControlBlock>>,
    /// completion 状态。true 表示子进程已经 exec 成功或 exit。
    done: bool,
    wait_queue: WaitQueue,
}

pub struct ProcessInner {
    /// 可执行文件描述符（新 VFS）。
    exe: Arc<Mutex<vfs::File>>,
    /// 可执行文件路径（用于 /proc/self/exe）。
    exe_path: String,
    /// 文件描述符表（新 VFS）。
    files: Arc<Mutex<vfs::FdTable>>,
    /// 文件系统状态（cwd 等）。
    fs: Arc<Mutex<FsStatus>>,
    /// 虚拟内存空间。
    vm: Arc<Mutex<AddressSpace<PageTableImpl>>>,
    /// 信号处理函数表。
    sighand: Arc<Mutex<Sighand>>,
    /// private futex 等待表。
    futex: Arc<Mutex<Futex>>,
    /// 同一地址空间内的用户资源槽位分配器。
    user_res_slot_allocator: Arc<Mutex<RecycleAllocator>>,
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

pub struct ProcessSignalState {
    /// kill(pid) / killpg() 这类进程级投递产生的共享 pending signal。
    pub shared_pending: SignalQueue,
    /// exit_group() 设置的线程组退出码。
    pub group_exit_code: Option<u32>,
    /// 线程组是否已经进入 group exit。
    pub group_exiting: bool,
}

impl ProcessControlBlock {
    pub fn new(
        pid: usize,
        leader_tid: usize,
        pgid: usize,
        parent: Option<Weak<ProcessControlBlock>>,
        exe: Arc<Mutex<vfs::File>>,
        exe_path: String,
        files: Arc<Mutex<vfs::FdTable>>,
        fs: Arc<Mutex<FsStatus>>,
        vm: Arc<Mutex<AddressSpace<PageTableImpl>>>,
        sighand: Arc<Mutex<Sighand>>,
        futex: Arc<Mutex<Futex>>,
        user_res_slot_allocator: Arc<Mutex<RecycleAllocator>>,
    ) -> Self {
        Self {
            pid,
            leader_tid,
            threads: Mutex::new(Vec::new()),
            child_exit_wait: Mutex::new(WaitQueue::new()),
            vfork: Mutex::new(VforkState {
                parent: None,
                done: false,
                wait_queue: WaitQueue::new(),
            }),
            inner: Mutex::new(ProcessInner {
                exe,
                exe_path,
                files,
                fs,
                vm,
                sighand,
                futex,
                user_res_slot_allocator,
                pgid,
                parent,
                children: Vec::new(),
                state: ProcessState::Running,
                exit_code: 0,
            }),
            signal: Mutex::new(ProcessSignalState {
                shared_pending: SignalQueue::empty(),
                group_exit_code: None,
                group_exiting: false,
            }),
        }
    }

    pub fn acquire_inner_lock(&self) -> MutexGuard<ProcessInner> {
        self.inner.lock()
    }

    pub fn exe(&self) -> Arc<Mutex<vfs::File>> {
        self.inner.lock().exe.clone()
    }

    pub fn exe_path(&self) -> String {
        self.inner.lock().exe_path.clone()
    }

    pub fn set_exe_path(&self, exe_path: String) {
        self.inner.lock().exe_path = exe_path;
    }

    pub fn replace_exe(&self, exe: vfs::File) {
        self.inner.lock().exe = Arc::new(Mutex::new(exe));
    }

    pub fn files(&self) -> Arc<Mutex<vfs::FdTable>> {
        self.inner.lock().files.clone()
    }

    pub fn fs(&self) -> Arc<Mutex<FsStatus>> {
        self.inner.lock().fs.clone()
    }

    pub fn vm(&self) -> Arc<Mutex<AddressSpace<PageTableImpl>>> {
        self.inner.lock().vm.clone()
    }

    pub fn replace_vm(&self, vm: AddressSpace<PageTableImpl>) {
        self.inner.lock().vm = Arc::new(Mutex::new(vm));
    }

    pub fn sighand(&self) -> Arc<Mutex<Sighand>> {
        self.inner.lock().sighand.clone()
    }

    pub fn futex(&self) -> Arc<Mutex<Futex>> {
        self.inner.lock().futex.clone()
    }

    pub fn user_res_slot_allocator(&self) -> Arc<Mutex<RecycleAllocator>> {
        self.inner.lock().user_res_slot_allocator.clone()
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

    pub fn enqueue_process_signal(&self, pending: PendingSignal) {
        let _ = self.signal.lock().shared_pending.enqueue(pending);
    }

    pub fn shared_pending(&self) -> Signals {
        self.signal.lock().shared_pending.pending()
    }

    pub fn take_shared_signal(&self, signal: Signals) -> bool {
        self.signal.lock().shared_pending.remove_signal(signal)
    }

    pub fn take_shared_matching(&self, set: Signals) -> Option<PendingSignal> {
        self.signal.lock().shared_pending.dequeue_matching(set)
    }

    pub fn request_group_exit(&self, exit_code: u32) {
        let mut state = self.signal.lock();
        state.group_exiting = true;
        state.group_exit_code = Some(exit_code);
    }

    pub fn is_group_exiting(&self) -> bool {
        self.signal.lock().group_exiting
    }

    pub fn group_exit_code(&self) -> Option<u32> {
        self.signal.lock().group_exit_code
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

    pub fn set_vfork_parent(&self, parent: &Arc<TaskControlBlock>) {
        let mut vfork = self.vfork.lock();
        vfork.parent = Some(Arc::downgrade(parent));
        vfork.done = false;
    }

    pub fn complete_vfork(&self) {
        let mut vfork = self.vfork.lock();
        if vfork.parent.is_none() || vfork.done {
            return;
        }
        vfork.parent = None;
        vfork.done = true;
        vfork.wait_queue.wake_all();
    }

    pub fn wait_vfork_done_interruptible(&self) -> WaitResult {
        WaitQueue::wait_event_interruptible_locked(
            &self.vfork,
            |state| &mut state.wait_queue,
            |state| state.done.then_some(0),
        )
    }

    pub fn take_children(&self) -> Vec<Arc<ProcessControlBlock>> {
        let mut inner = self.inner.lock();
        core::mem::take(&mut inner.children)
    }

    pub fn close_files_on_exit(&self) {
        let files_ref = self.files();
        let mut fd_table = files_ref.lock();
        let open_fds: Vec<usize> = fd_table.iter().map(|(i, _f)| i).collect();
        for fd in open_fds {
            let _ = fd_table.drop_fd(fd);
        }
    }
}

impl Drop for ProcessControlBlock {
    fn drop(&mut self) {
        registry::unregister_process(self.pid);
    }
}
