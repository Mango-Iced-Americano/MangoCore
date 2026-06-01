use super::{
    pid::{RecycleAllocator, TidHandle},
    quota::TaskQuotaGuard,
    registry,
    signal::{sigchld_requests_auto_reap, PendingSignal, SignalQueue, Sighand, Signals},
    threads::Futex,
    wake_interruptible, Completion, FsStatus, TaskControlBlock, TaskStatus, UtsNamespace,
    Rusage, WaitQueue, INITPROC,
};
use crate::fs::vfs;
use crate::mm::{AddressSpace, PageTableImpl};
use crate::utils::error::SyscallErr;
use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use core::sync::atomic::{AtomicBool, Ordering};
use alloc::string::String;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use log::warn;
use spin::{Mutex, MutexGuard};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessState {
    Running,
    Stopped,
    Zombie,
}

pub struct ProcessControlBlock {
    /// 用户可见进程 ID，即 getpid() 返回值。
    pub pid: usize,
    /// 线程组主线程 tid。
    pub leader_tid: usize,
    /// 保持进程 pid/tgid 在 zombie 被 wait 回收前不被复用。
    _pid_handle: Arc<TidHandle>,
    /// 进程生命周期 quota。clone()/fork() 成功时申请。
    /// wait_child / auto-reap / orphan-zombie-reap 时调用 release_process_quota_once()
    /// 立即释放；PCB Drop 作为兜底。
    process_quota: Mutex<Option<TaskQuotaGuard>>,
    /// 属于该进程的线程列表。
    pub threads: Mutex<Vec<Weak<TaskControlBlock>>>,
    /// 父进程 wait4() 等待子进程退出的等待队列。
    pub child_exit_wait: Mutex<WaitQueue>,
    /// CLONE_VFORK 父线程。Some 表示当前进程来自 vfork，且尚未完成。
    vfork_parent: Mutex<Option<Weak<TaskControlBlock>>>,
    /// CLONE_VFORK completion。父线程等待子进程 exec 成功或 exit。
    vfork_done: Completion,
    /// 是否被 init 收养（通过 adopt_children_by_init）。用于 finish_exit
    /// 中区分 init 直接 fork 的子进程和被收养的孤儿，只对后者自动回收。
    pub adopted_by_init: AtomicBool,
    inner: Mutex<ProcessInner>,
    signal: Mutex<ProcessSignalState>,
}

pub struct ProcessInner {
    /// 可执行文件描述符（新 VFS）。
    exe: Arc<Mutex<vfs::File>>,
    /// 当前可执行文件的稳定 key，用于 open(O_TRUNC/O_WRONLY) 返回 ETXTBSY。
    exec_key: Option<InodeBusyKey>,
    /// 可执行文件路径（用于 /proc/self/exe）。
    exe_path: String,
    /// 文件描述符表（新 VFS）。
    files: Arc<Mutex<vfs::FdTable>>,
    /// 文件系统状态（cwd 等）。
    fs: Arc<Mutex<FsStatus>>,
    /// UTS namespace 状态（hostname/domainname）。
    uts: Arc<Mutex<UtsNamespace>>,
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
    /// 会话 ID。
    pub sid: usize,
    /// 父进程。
    pub parent: Option<Weak<ProcessControlBlock>>,
    /// 进程创建后是否已经成功执行过 execve。
    pub has_execed: bool,
    /// PR_SET_CHILD_SUBREAPER 标记。Linux 语义是不被 fork/clone 继承，
    /// 但会跨 execve 保留。
    pub child_subreaper: bool,
    /// 子进程。
    pub children: Vec<Arc<ProcessControlBlock>>,
    /// 进程级生命周期状态。
    pub state: ProcessState,
    /// wait4 可回收的进程退出码。
    pub exit_code: u32,
    /// 最近一次可被 waitpid(WUNTRACED)/waitid(WSTOPPED) 观察到的停止信号。
    pub stopped_signal: Option<usize>,
    /// 停止状态是否已经被不带 WNOWAIT 的 wait 消费。
    pub stopped_reported: bool,
    /// 最近一次可被 waitpid(WCONTINUED)/waitid(WCONTINUED) 观察到的继续事件。
    pub continued_pending: bool,
    /// 进程退出时记录的 leader CPU 时间快照。
    pub rusage: Rusage,
    /// 已由 wait/waitid 回收的子进程 CPU 时间累计。
    pub child_rusage: Rusage,
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

type InodeBusyKey = (usize, vfs::InodeId);

lazy_static! {
    static ref EXEC_INODE_REFS: Mutex<BTreeMap<InodeBusyKey, usize>> =
        Mutex::new(BTreeMap::new());
    static ref WRITE_INODE_REFS: Mutex<BTreeMap<InodeBusyKey, usize>> =
        Mutex::new(BTreeMap::new());
}

fn inode_busy_key(inode: &Arc<dyn vfs::IndexNode>) -> Option<InodeBusyKey> {
    inode.metadata().ok().map(|meta| (meta.dev_id, meta.inode_id))
}

fn exec_key_from_file(file: &vfs::File) -> Option<InodeBusyKey> {
    file.metadata().ok().map(|meta| (meta.dev_id, meta.inode_id))
}

fn register_busy_key(refs: &Mutex<BTreeMap<InodeBusyKey, usize>>, key: InodeBusyKey) {
    let mut refs = refs.lock();
    let count = refs.entry(key).or_insert(0);
    *count = count.saturating_add(1);
}

fn unregister_busy_key(refs: &Mutex<BTreeMap<InodeBusyKey, usize>>, key: InodeBusyKey) {
    let mut refs = refs.lock();
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

fn register_exec_key(key: InodeBusyKey) {
    register_busy_key(&EXEC_INODE_REFS, key);
}

fn unregister_exec_key(key: InodeBusyKey) {
    unregister_busy_key(&EXEC_INODE_REFS, key);
}

pub fn is_executable_inode_busy(inode: &Arc<dyn vfs::IndexNode>) -> bool {
    let key = match inode_busy_key(inode) {
        Some(key) => key,
        None => return false,
    };
    EXEC_INODE_REFS.lock().get(&key).copied().unwrap_or(0) > 0
}

pub fn register_writable_inode(inode: &Arc<dyn vfs::IndexNode>) {
    if let Some(key) = inode_busy_key(inode) {
        register_busy_key(&WRITE_INODE_REFS, key);
    }
}

pub fn unregister_writable_inode(inode: &Arc<dyn vfs::IndexNode>) {
    if let Some(key) = inode_busy_key(inode) {
        unregister_busy_key(&WRITE_INODE_REFS, key);
    }
}

pub fn is_writable_inode_busy(inode: &Arc<dyn vfs::IndexNode>) -> bool {
    let key = match inode_busy_key(inode) {
        Some(key) => key,
        None => return false,
    };
    WRITE_INODE_REFS.lock().get(&key).copied().unwrap_or(0) > 0
}

impl ProcessControlBlock {
    /// 一次性释放进程级 clone quota。幂等，重复调用无副作用。
    /// 应在 wait_child、auto-reap、orphan-zombie-reap 路径中尽早调用，
    /// 不依赖 PCB Drop 的延迟释放。
    pub fn release_process_quota_once(&self) {
        if let Some(_guard) = self.process_quota.lock().take() {
            // guard 在此处 drop → TASK_QUOTA_USED 递减
        }
    }

    pub fn new(
        pid: usize,
        leader_tid: usize,
        pid_handle: Arc<TidHandle>,
        process_quota: TaskQuotaGuard,
        pgid: usize,
        sid: usize,
        parent: Option<Weak<ProcessControlBlock>>,
        exe: Arc<Mutex<vfs::File>>,
        exe_path: String,
        files: Arc<Mutex<vfs::FdTable>>,
        fs: Arc<Mutex<FsStatus>>,
        uts: Arc<Mutex<UtsNamespace>>,
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
            _pid_handle: pid_handle,
            process_quota: Mutex::new(Some(process_quota)),
            threads: Mutex::new(Vec::new()),
            child_exit_wait: Mutex::new(WaitQueue::new()),
            vfork_parent: Mutex::new(None),
            vfork_done: Completion::new(),
            adopted_by_init: AtomicBool::new(false),
            inner: Mutex::new(ProcessInner {
                exe,
                exec_key,
                exe_path,
                files,
                fs,
                uts,
                vm,
                sighand,
                futex,
                user_res_slot_allocator,
                pgid,
                sid,
                parent,
                has_execed: false,
                child_subreaper: false,
                children: Vec::new(),
                state: ProcessState::Running,
                exit_code: 0,
                stopped_signal: None,
                stopped_reported: false,
                continued_pending: false,
                rusage: Rusage::new(),
                child_rusage: Rusage::new(),
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

    pub fn release_pid(&self) {
        self._pid_handle.release();
    }

    pub fn pid_released(&self) -> bool {
        self._pid_handle.is_released()
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

    pub fn mark_execed(&self) {
        self.inner.lock().has_execed = true;
    }

    pub fn has_execed(&self) -> bool {
        self.inner.lock().has_execed
    }

    pub fn set_child_subreaper(&self, enabled: bool) {
        self.inner.lock().child_subreaper = enabled;
    }

    pub fn is_child_subreaper(&self) -> bool {
        self.inner.lock().child_subreaper
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

    pub fn unshare_fs(&self) -> Arc<Mutex<FsStatus>> {
        let fs_ref = self.fs();
        let copied = fs_ref.lock().clone();
        let new_fs = Arc::new(Mutex::new(copied));
        self.inner.lock().fs = new_fs.clone();
        new_fs
    }

    pub fn fs(&self) -> Arc<Mutex<FsStatus>> {
        self.inner.lock().fs.clone()
    }

    pub fn uts(&self) -> Arc<Mutex<UtsNamespace>> {
        self.inner.lock().uts.clone()
    }

    pub fn unshare_uts(&self) -> Arc<Mutex<UtsNamespace>> {
        let uts_ref = self.uts();
        let copied = uts_ref.lock().clone();
        let new_uts = Arc::new(Mutex::new(copied));
        self.inner.lock().uts = new_uts.clone();
        new_uts
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

    pub fn setsid(&self, sid: usize) -> isize {
        let mut inner = self.inner.lock();
        inner.sid = sid;
        inner.pgid = sid;
        0
    }

    pub fn getsid(&self) -> usize {
        self.inner.lock().sid
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

    #[cfg(feature = "heap_trace")]
    pub fn debug_state(&self) -> ProcessState {
        self.inner.lock().state
    }

    #[cfg(feature = "heap_trace")]
    pub fn debug_child_counts(&self) -> (usize, usize, usize) {
        let inner = self.inner.lock();
        let mut zombie_children = 0;
        let mut live_children = 0;
        for child in inner.children.iter() {
            if child.is_zombie() {
                zombie_children += 1;
            } else {
                live_children += 1;
            }
        }
        (inner.children.len(), zombie_children, live_children)
    }

    pub fn mark_zombie(&self, exit_code: u32, rusage: Rusage) -> bool {
        let mut inner = self.inner.lock();
        if inner.state == ProcessState::Zombie {
            return false;
        }
        inner.state = ProcessState::Zombie;
        inner.exit_code = exit_code;
        inner.stopped_signal = None;
        inner.stopped_reported = true;
        inner.continued_pending = false;
        inner.rusage = rusage;
        true
    }

    pub fn mark_stopped(&self, signum: usize) {
        {
            let mut inner = self.inner.lock();
            if inner.state == ProcessState::Zombie {
                return;
            }
            inner.state = ProcessState::Stopped;
            inner.stopped_signal = Some(signum);
            inner.stopped_reported = false;
            inner.continued_pending = false;
        }
        if let Some(parent) = self.parent() {
            parent.child_exit_wait.lock().wake_all();
        }
    }

    pub fn mark_continued(&self) {
        let changed = {
            let mut inner = self.inner.lock();
            if inner.state != ProcessState::Stopped {
                false
            } else {
                inner.state = ProcessState::Running;
                inner.stopped_signal = None;
                inner.stopped_reported = true;
                inner.continued_pending = true;
                true
            }
        };
        if changed {
            if let Some(parent) = self.parent() {
                parent.child_exit_wait.lock().wake_all();
            }
        }
    }

    pub fn take_stopped_status(&self, nowait: bool) -> Option<u32> {
        let mut inner = self.inner.lock();
        if inner.state != ProcessState::Stopped || inner.stopped_reported {
            return None;
        }
        let signum = inner.stopped_signal?;
        if !nowait {
            inner.stopped_reported = true;
        }
        Some(((signum as u32) << 8) | 0x7f)
    }

    pub fn take_continued_status(&self, nowait: bool) -> Option<u32> {
        let mut inner = self.inner.lock();
        if !inner.continued_pending {
            return None;
        }
        if !nowait {
            inner.continued_pending = false;
        }
        Some(0xffff)
    }

    pub fn exit_code(&self) -> u32 {
        self.inner.lock().exit_code
    }

    pub fn rusage(&self) -> Rusage {
        self.inner.lock().rusage
    }

    pub fn child_rusage(&self) -> Rusage {
        self.inner.lock().child_rusage
    }

    pub fn wait_rusage(&self) -> Rusage {
        let inner = self.inner.lock();
        let mut rusage = inner.rusage;
        rusage.add_child(inner.child_rusage);
        rusage
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
        const CHILDREN_SOFT_CAP: usize = 512;
        let mut inner = self.inner.lock();
        // 超过软上限时仅告警，不在此处静默丢弃 zombie。
        // 静默丢弃会绕过 wait4/rusage 回收语义，丢失子进程退出状态、
        // rusage 聚合和 PID 生命周期管理。
        // 正常情况下 finish_exit → wait4 会回收 zombie；
        // 若此告警持续出现，说明父进程未调用 wait4 导致僵尸堆积。
        if inner.children.len() >= CHILDREN_SOFT_CAP {
            warn!(
                "[add_child] pid={} children at soft cap ({}), possible wait4 leak",
                self.pid,
                inner.children.len(),
            );
        }
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

    pub fn wait_vfork_done_uninterruptible(&self) {
        self.vfork_done.wait_uninterruptible()
    }

    pub fn take_children(&self) -> Vec<Arc<ProcessControlBlock>> {
        let mut inner = self.inner.lock();
        core::mem::take(&mut inner.children)
    }

    fn wake_child_waiters(process: &Arc<ProcessControlBlock>) {
        process.child_exit_wait.lock().wake_all();
        if let Some(task) = process.any_live_thread() {
            let mut inner = task.acquire_inner_lock();
            if inner.task_status == TaskStatus::Interruptible {
                inner.task_status = TaskStatus::Ready;
                drop(inner);
                wake_interruptible(task);
            }
        }
    }

    fn nearest_child_reaper(
        parent: Option<Arc<ProcessControlBlock>>,
    ) -> Arc<ProcessControlBlock> {
        let mut cursor = parent;
        while let Some(process) = cursor {
            if !process.is_zombie() && process.is_child_subreaper() {
                return process;
            }
            cursor = process.parent();
        }
        INITPROC.process.clone()
    }

    fn adopt_children_by_init(children: Vec<Arc<ProcessControlBlock>>) -> bool {
        let mut live_children = Vec::new();
        let mut orphan_rusage = Rusage::new();

        for child in children {
            if child.is_zombie() {
                orphan_rusage.add_child(child.wait_rusage());
                child.set_parent(None);
                child.release_pid();
                registry::unregister_process(child.pid);
                child.release_process_quota_once();
                crate::task::remove_zombie_tasks_by_pid(child.pid);
            } else {
                child.set_parent(Some(Arc::downgrade(&INITPROC.process)));
                child.adopted_by_init.store(true, Ordering::Relaxed);
                live_children.push(child);
            }
        }

        let has_live_children = !live_children.is_empty();
        let mut initproc_inner = INITPROC.process.acquire_inner_lock();
        initproc_inner.child_rusage.add_child(orphan_rusage);
        initproc_inner.children.extend(live_children);
        has_live_children
    }

    fn adopt_children_by_reaper(
        children: Vec<Arc<ProcessControlBlock>>,
        reaper: Arc<ProcessControlBlock>,
    ) -> bool {
        if Arc::ptr_eq(&reaper, &INITPROC.process) {
            return Self::adopt_children_by_init(children);
        }

        let has_children = !children.is_empty();
        {
            let mut reaper_inner = reaper.acquire_inner_lock();
            if reaper_inner.children.try_reserve(children.len()).is_err() {
                drop(reaper_inner);
                return Self::adopt_children_by_init(children);
            }
        }

        for child in &children {
            child.set_parent(Some(Arc::downgrade(&reaper)));
            child.adopted_by_init.store(false, Ordering::Relaxed);
        }
        reaper.acquire_inner_lock().children.extend(children);
        has_children
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
        let mut rusage = exit_task.acquire_inner_lock().rusage;
        let resident_kb = self.vm().lock().resident_user_bytes() / 1024;
        rusage.update_maxrss_kb(resident_kb);
        if !self.mark_zombie(exit_code, rusage) {
            return;
        }
        // 在 mark_zombie 之后重新获取 parent: 虽然单核非抢占内核中
        // mark_zombie（仅持自旋锁）和父进程 finish_exit 之间不存在竞态，
        // 但防御性重读可避免未来引入抢占后 parent 引用变为陈旧。
        let parent_process = self.parent();
        let auto_reap = parent_process
            .as_ref()
            .map(|parent| {
                let sighand_ref = parent.sighand();
                let sighand = sighand_ref.lock();
                sigchld_requests_auto_reap(&sighand)
            })
            .unwrap_or(false);
        let old_exec_key = self.inner.lock().exec_key.take();
        if let Some(key) = old_exec_key {
            unregister_exec_key(key);
        }

        let children = self.take_children();
        let child_reaper = Self::nearest_child_reaper(parent_process.clone());
        let adopted_children = if children.is_empty() {
            false
        } else {
            Self::adopt_children_by_reaper(children, child_reaper.clone())
        };

        if let Some(parent_process) = parent_process {
            // 仅对被 init 收养的孤儿做 auto-reap；init 直接 fork 的
            // 子进程仍走正常 waitpid 路径，保证 wait/rusage 语义。
            let auto_reap = self.adopted_by_init.load(Ordering::Relaxed)
                || auto_reap
                || sigchld_requests_auto_reap(&parent_process.sighand().lock());
            if auto_reap {
                parent_process.detach_child(self.pid);
                self.set_parent(None);
                self.release_pid();
                registry::unregister_process(self.pid);
                self.release_process_quota_once();
                crate::task::remove_zombie_tasks_by_pid(self.pid);
                parent_process.child_exit_wait.lock().wake_all();
            } else {
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
            }
        } else {
            warn!("[finish_process_exit] parent is None");
        }

        if adopted_children {
            Self::wake_child_waiters(&child_reaper);
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
        registry::unregister_process_if_match(self);
    }
}
