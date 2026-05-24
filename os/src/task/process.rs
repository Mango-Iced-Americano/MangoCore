use super::{
    pid::RecycleAllocator,
    registry,
    signal::{PendingSignal, SignalQueue, Sighand, Signals},
    threads::Futex,
    wake_interruptible, Completion, FsStatus, TaskControlBlock, TaskStatus, WaitQueue, WaitResult,
    INITPROC,
};
use crate::fs::vfs;
use crate::mm::{AddressSpace, PageTableImpl};
use crate::utils::error::SyscallErr;
use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::string::String;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use log::warn;
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
    /// CLONE_VFORK 父线程。Some 表示当前进程来自 vfork，且尚未完成。
    vfork_parent: Mutex<Option<Weak<TaskControlBlock>>>,
    /// CLONE_VFORK completion。父线程等待子进程 exec 成功或 exit。
    vfork_done: Completion,
    inner: Mutex<ProcessInner>,
    signal: Mutex<ProcessSignalState>,
}

pub struct ProcessInner {
    /// 可执行文件描述符（新 VFS）。
    exe: Arc<Mutex<vfs::File>>,
    /// 当前可执行文件的稳定 key，用于 open(O_TRUNC/O_WRONLY) 返回 ETXTBSY。
    exec_key: Option<ExecInodeKey>,
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
    /// 进程 leader 的调度策略兼容快照，供 zombie 子进程在 wait 前被查询。
    pub sched_policy: usize,
    pub sched_priority: i32,
    /// SCHED_RESET_ON_FORK 的进程级兼容标记，用于覆盖测试框架中非 leader fork 的路径。
    pub sched_reset_on_fork: bool,
    pub sched_nice: i32,
    pub sched_runtime: u64,
    pub sched_deadline: u64,
    pub sched_period: u64,
}

pub struct ProcessSignalState {
    /// kill(pid) / killpg() 这类进程级投递产生的共享 pending signal。
    pub shared_pending: SignalQueue,
    /// exit_group() 设置的线程组退出码。
    pub group_exit_code: Option<u32>,
    /// 线程组是否已经进入 group exit。
    pub group_exiting: bool,
}

type ExecInodeKey = (usize, vfs::InodeId);

lazy_static! {
    static ref EXEC_INODE_REFS: Mutex<BTreeMap<ExecInodeKey, usize>> =
        Mutex::new(BTreeMap::new());
}

fn exec_key_from_file(file: &vfs::File) -> Option<ExecInodeKey> {
    file.metadata().ok().map(|meta| (meta.dev_id, meta.inode_id))
}

fn register_exec_key(key: ExecInodeKey) {
    let mut refs = EXEC_INODE_REFS.lock();
    let count = refs.entry(key).or_insert(0);
    *count = count.saturating_add(1);
}

fn unregister_exec_key(key: ExecInodeKey) {
    let mut refs = EXEC_INODE_REFS.lock();
    let remove = if let Some(count) = refs.get_mut(&key) {
        if *count > 1 {
            *count -= 1;
            false
        } else {
            true
        }
    } else {
        false
    };
    if remove {
        refs.remove(&key);
    }
}

pub fn is_executable_inode_busy(inode: &Arc<dyn vfs::IndexNode>) -> bool {
    let key = match inode.metadata() {
        Ok(meta) => (meta.dev_id, meta.inode_id),
        Err(_) => return false,
    };
    EXEC_INODE_REFS.lock().get(&key).copied().unwrap_or(0) > 0
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
        let exec_key = {
            let lock = exe.lock();
            exec_key_from_file(&lock)
        };
        if let Some(key) = exec_key {
            register_exec_key(key);
        }
        Self {
            pid,
            leader_tid,
            threads: Mutex::new(Vec::new()),
            child_exit_wait: Mutex::new(WaitQueue::new()),
            vfork_parent: Mutex::new(None),
            vfork_done: Completion::new(),
            inner: Mutex::new(ProcessInner {
                exe,
                exec_key,
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
                sched_policy: 0,
                sched_priority: 0,
                sched_reset_on_fork: false,
                sched_nice: 0,
                sched_runtime: 0,
                sched_deadline: 0,
                sched_period: 0,
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
        let new_key = exec_key_from_file(&exe);
        let mut inner = self.inner.lock();
        if inner.exec_key != new_key {
            if let Some(old_key) = inner.exec_key.take() {
                unregister_exec_key(old_key);
            }
            if let Some(key) = new_key {
                register_exec_key(key);
            }
            inner.exec_key = new_key;
        }
        inner.exe = Arc::new(Mutex::new(exe));
    }

    pub fn files(&self) -> Arc<Mutex<vfs::FdTable>> {
        self.inner.lock().files.clone()
    }

    pub fn unshare_files(&self) -> Result<Arc<Mutex<vfs::FdTable>>, SyscallErr> {
        let files_ref = self.files();
        let copied = files_ref.lock().try_clone()?;
        let new_files = Arc::new(Mutex::new(copied));
        self.inner.lock().files = new_files.clone();
        Ok(new_files)
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

    pub fn sched_reset_on_fork(&self) -> bool {
        self.inner.lock().sched_reset_on_fork
    }

    pub fn set_sched_reset_on_fork(&self, reset: bool) {
        self.inner.lock().sched_reset_on_fork = reset;
    }

    pub fn sched_state(&self) -> (usize, i32, bool, i32, u64, u64, u64) {
        let inner = self.inner.lock();
        (
            inner.sched_policy,
            inner.sched_priority,
            inner.sched_reset_on_fork,
            inner.sched_nice,
            inner.sched_runtime,
            inner.sched_deadline,
            inner.sched_period,
        )
    }

    pub fn set_sched_state(
        &self,
        policy: usize,
        priority: i32,
        reset_on_fork: bool,
        nice: i32,
        runtime: u64,
        deadline: u64,
        period: u64,
    ) {
        let mut inner = self.inner.lock();
        inner.sched_policy = policy;
        inner.sched_priority = priority;
        inner.sched_reset_on_fork = reset_on_fork;
        inner.sched_nice = nice;
        inner.sched_runtime = runtime;
        inner.sched_deadline = deadline;
        inner.sched_period = period;
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
        *self.vfork_parent.lock() = Some(Arc::downgrade(parent));
    }

    pub fn complete_vfork(&self) {
        let mut parent = self.vfork_parent.lock();
        if parent.is_none() {
            return;
        }
        *parent = None;
        drop(parent);
        self.vfork_done.complete();
    }

    pub fn wait_vfork_done_interruptible(&self) -> WaitResult {
        self.vfork_done.wait_interruptible()
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
        fd_table.release_backing_storage();
    }

    /// 完成进程级退出收尾。
    ///
    /// 线程级清理已经由 TaskControlBlock::exit_thread_resources() 完成；
    /// 这里只负责进程 zombie、父进程 wait 唤醒、孤儿进程转交 initproc
    /// 以及进程资源关闭。
    pub fn finish_exit(&self, exit_task: &TaskControlBlock, exit_code: u32) {
        self.complete_vfork();
        if !self.mark_zombie(exit_code) {
            return;
        }
        let old_exec_key = self.inner.lock().exec_key.take();
        if let Some(key) = old_exec_key {
            unregister_exec_key(key);
        }

        if let Some(parent_process) = self.parent() {
            parent_process.child_exit_wait.lock().wake_all();
            if !exit_task.exit_signal.is_empty() {
                if let Some(parent_task) = parent_process.any_live_thread() {
                    let mut parent_inner = parent_task.acquire_inner_lock();
                    parent_inner.add_signal(exit_task.exit_signal);

                    if parent_inner.task_status == TaskStatus::Interruptible {
                        parent_inner.task_status = TaskStatus::Ready;
                        drop(parent_inner);
                        wake_interruptible(parent_task);
                    }
                }
            }
        } else {
            warn!("[finish_process_exit] parent is None");
        }

        let children = self.take_children();
        if !children.is_empty() {
            let mut initproc_inner = INITPROC.process.acquire_inner_lock();
            for child in children {
                child.set_parent(Some(Arc::downgrade(&INITPROC.process)));
                initproc_inner.children.push(child);
            }
            drop(initproc_inner);
            INITPROC.process.child_exit_wait.lock().wake_all();
            if let Some(init_task) = INITPROC.process.any_live_thread() {
                let mut init_inner = init_task.acquire_inner_lock();
                if init_inner.task_status == TaskStatus::Interruptible {
                    init_inner.task_status = TaskStatus::Ready;
                    drop(init_inner);
                    wake_interruptible(init_task);
                }
            }
        }

        let vm = self.vm();
        if Arc::strong_count(&vm) <= 2 {
            vm.lock().release_for_zombie();
        }
        self.close_files_on_exit();
    }
}

impl Drop for ProcessControlBlock {
    fn drop(&mut self) {
        if let Some(key) = self.inner.get_mut().exec_key.take() {
            unregister_exec_key(key);
        }
        registry::unregister_process(self.pid);
    }
}
