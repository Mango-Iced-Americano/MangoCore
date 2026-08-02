//! 进程控制块与进程级生命周期。
//!
//! `ProcessControlBlock` 保存线程组共享状态：地址空间、fd table、namespace、
//! 信号动作、进程级 pending signal、child tree、wait 状态和 zombie 回收信息。
//! 线程级运行状态保存在 `TaskControlBlock` 中。
//!
//! # Locking
//!
//! `ProcessControlBlock::inner` 保护进程结构性状态，`signal` 单独保护进程共享
//! pending signal，`thread_group` 则把线程成员关系与 group-exit 发布门禁放在
//! 同一锁域。涉及调度队列、父子关系和资源析构时，遵循“锁内移动 Arc/记录状态，
//! 锁外执行唤醒或析构”的顺序。

use super::{
    pid::{RecycleAllocator, TidHandle},
    quota::TaskQuotaGuard,
    registry,
    signal::{sigchld_requests_auto_reap, PendingSignal, Sighand, SignalQueue, Signals},
    threads::FutexTable,
    wake_interruptible, Completion, FsStatus, IpcNamespace, MountNamespace, NetNamespace, Rusage,
    TaskControlBlock, UtsNamespace, WaitQueue, WaitResult, INITPROC,
};
use crate::fs::vfs;
use crate::mm::{AddressSpace, AddressSpaceInner, PageTableImpl, UserVmContext};
use crate::signal_type;
use crate::utils::error::SyscallErr;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use lazy_static::lazy_static;
use log::warn;
use spin::{Mutex, MutexGuard};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 进程级生命周期状态。
pub enum ProcessState {
    /// 至少还有线程可运行或可等待。
    Running,
    /// 因默认 stop 信号或 ptrace stop 停止。
    Stopped,
    /// 进程已退出，等待父进程 wait 回收。
    Zombie,
}

/// 进程控制块。
pub struct ProcessControlBlock {
    /// 用户可见进程 ID，即 getpid() 返回值。
    pub pid: usize,
    /// 保持进程 pid/tgid 在 zombie 被 wait 回收前不被复用。
    pid_handle: Arc<TidHandle>,
    /// 进程生命周期 quota。clone()/fork() 成功时申请。
    /// wait_child / auto-reap / orphan-zombie-reap 时调用 release_process_quota_once()
    /// 立即释放；PCB Drop 作为兜底。
    process_quota: Mutex<Option<TaskQuotaGuard>>,
    /// 线程成员关系与首次发布门禁。
    thread_group: Mutex<ThreadGroupState>,
    /// 统一 group-exit 退出码的无锁快照。
    ///
    /// 0 表示尚未退出，其余值为 `exit_code + 1`。编码到 u64 后，u32 的全部
    /// 退出码都可表示；任务安全点只需一次 Acquire load，不必在每次 syscall
    /// 返回时争用线程组锁。
    group_exit_code: AtomicU64,
    /// 当前被计入存活线程数的线程数量。
    live_threads: AtomicUsize,
    /// 正在执行多线程 exec 的 owner tid；`usize::MAX` 表示没有临时 exec 会话。
    ///
    /// 线程组锁保存权威会话，本字段只供 syscall 返回等高频安全点无锁判断。
    exec_owner_tid: AtomicUsize,
    /// 保留 trap context 页映射、可被复用的用户资源槽位。
    trap_context_cache: Mutex<Vec<usize>>,
    /// 父进程 wait4() 等待子进程退出的等待队列。
    pub child_exit_wait: Mutex<WaitQueue>,
    /// CLONE_VFORK 父线程。Some 表示当前进程来自 vfork，且尚未完成。
    vfork_parent: Mutex<Option<Weak<TaskControlBlock>>>,
    /// CLONE_VFORK completion。父线程等待子进程 exec 成功或 exit。
    vfork_done: Completion,
    /// 是否被 init 收养（通过 adopt_children_by_init）。用于 finish_exit
    /// 中区分 init 直接 fork 的子进程和被收养的孤儿，只对后者自动回收。
    pub adopted_by_init: AtomicBool,
    pgid_hint: AtomicUsize,
    sid_hint: AtomicUsize,
    parent_pid_hint: AtomicUsize,
    user_token_hint: AtomicUsize,
    inner: Mutex<ProcessInner>,
    signal: Mutex<ProcessSignalState>,
    shared_pending_hint: AtomicU64,
}

/// 由 `process.inner` 保护的进程共享状态。
pub struct ProcessInner {
    /// 可执行文件描述符（新 VFS）。
    exe: Arc<Mutex<Arc<vfs::File>>>,
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
    /// 网络命名空间。
    net: Arc<NetNamespace>,
    /// 挂载命名空间（stub，不隔离）。
    mnt: Arc<MountNamespace>,
    /// IPC 命名空间（stub，不隔离）。
    ipc: Arc<IpcNamespace>,
    /// 虚拟内存空间。
    vm: Arc<AddressSpace<PageTableImpl>>,
    /// 信号处理函数表。
    sighand: Arc<Mutex<Sighand>>,
    /// private futex 等待表。
    futex: Arc<Mutex<FutexTable>>,
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
    /// PTRACE_ATTACH tracer pid. This does not change process parentage.
    pub ptrace_tracer_pid: Option<usize>,
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
}

/// 由 `process.thread_group` 保护的线程成员表。
///
/// 首次发布的最终检查和 group-exit 提交都在持有这把锁时读取/写入
/// `group_exit_code`。因此退出方要么先关闭发布门禁，要么一定能在成员快照中
/// 看到已经进入 runqueue 的线程；group exit 与 exec 临时会话也由本锁串行化。
/// 普通安全点只读取原子快照，不取得本锁。
struct ThreadGroupState {
    members: Vec<Weak<TaskControlBlock>>,
    /// exec 只临时关闭 clone 发布门；owner 安装新映像后会重新开放。
    exec: Option<ExecState>,
}

struct ExecState {
    owner_tid: usize,
    siblings_done: Arc<Completion>,
}

/// 多线程 exec 的临时停止会话。
///
/// owner 等待所有 sibling 在各自 CPU 完成线程级清理后，才能安装新地址空间；
/// `finish()` 只有在 live 计数收缩为 owner 一人时才重新开放 clone。
#[must_use = "exec session must wait for siblings and be finished"]
pub(crate) struct ExecSession {
    process: Arc<ProcessControlBlock>,
    owner_tid: usize,
    siblings_done: Arc<Completion>,
}

type InodeBusyKey = (usize, vfs::InodeId);
const TRAP_CONTEXT_CACHE_LIMIT: usize = 256;
const NO_EXEC_OWNER: usize = usize::MAX;

impl ExecSession {
    pub(crate) fn wait(&self) {
        // 普通信号不能取消 exec；永久 group exit 可以让 owner 提前结束等待，
        // install_exec_image() 会在提交新映像前识别并放弃本次 exec。
        let _ = self.siblings_done.wait_killable();
    }

    pub(crate) fn finish(self) {
        let mut group = self.process.thread_group.lock();
        let live = self.process.live_thread_count();
        assert!(
            live == 1 || self.process.is_group_exiting(),
            "exec gate reopened before sibling cleanup completed: live={}",
            live
        );
        let state = group.exec.as_ref().expect("missing active exec session");
        assert!(
            state.owner_tid == self.owner_tid
                && Arc::ptr_eq(&state.siblings_done, &self.siblings_done),
            "stale exec session tried to reopen the thread group"
        );
        group.exec = None;
        // 在同一锁域内先清除权威会话，再发布无锁快照。后续 publish_thread()
        // 取得本锁后才能放入新线程，不会看到“门已开、owner 仍旧”的中间态。
        self.process
            .exec_owner_tid
            .store(NO_EXEC_OWNER, Ordering::Release);
    }
}

lazy_static! {
    static ref EXEC_INODE_REFS: Mutex<BTreeMap<InodeBusyKey, usize>> = Mutex::new(BTreeMap::new());
    static ref WRITE_INODE_REFS: Mutex<BTreeMap<InodeBusyKey, usize>> = Mutex::new(BTreeMap::new());
}

fn inode_busy_key(inode: &Arc<dyn vfs::IndexNode>) -> Option<InodeBusyKey> {
    let inode_id = inode.metadata().ok()?.inode_id;
    Some((inode.fs().identity_key(), inode_id))
}

fn exec_key_from_file(file: &vfs::File) -> Option<InodeBusyKey> {
    inode_busy_key(&file.inode)
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

/// 记录一个正在以可写方式打开的 inode。
pub fn register_writable_inode(inode: &Arc<dyn vfs::IndexNode>) {
    if let Some(key) = inode_busy_key(inode) {
        register_busy_key(&WRITE_INODE_REFS, key);
    }
}

/// 取消一个可写 inode 引用计数。
pub fn unregister_writable_inode(inode: &Arc<dyn vfs::IndexNode>) {
    if let Some(key) = inode_busy_key(inode) {
        unregister_busy_key(&WRITE_INODE_REFS, key);
    }
}

/// 判断 inode 是否正在被可写打开。
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

    /// 创建新的进程控制块。
    ///
    /// # Semantics
    ///
    /// 构造时会注册当前可执行文件的 `exec_key`，用于 `ETXTBSY` 兼容检查。
    /// 返回的 PCB 尚未自动注册到全局 registry，调用方需要在 clone/fork 发布路径中完成。
    ///
    /// # Locking
    ///
    /// 只短暂读取 `exe` 和 `vm` 锁，不会进入等待点。
    pub fn new(
        pid: usize,
        pid_handle: Arc<TidHandle>,
        process_quota: TaskQuotaGuard,
        pgid: usize,
        sid: usize,
        parent: Option<Weak<ProcessControlBlock>>,
        exe: Arc<Mutex<Arc<vfs::File>>>,
        exe_path: String,
        files: Arc<Mutex<vfs::FdTable>>,
        fs: Arc<Mutex<FsStatus>>,
        uts: Arc<Mutex<UtsNamespace>>,
        net: Arc<NetNamespace>,
        mnt: Arc<MountNamespace>,
        ipc: Arc<IpcNamespace>,
        vm: Arc<AddressSpace<PageTableImpl>>,
        sighand: Arc<Mutex<Sighand>>,
        futex: Arc<Mutex<FutexTable>>,
        user_res_slot_allocator: Arc<Mutex<RecycleAllocator>>,
    ) -> Self {
        let exec_key = {
            let lock = exe.lock();
            exec_key_from_file(&lock)
        };
        if let Some(key) = exec_key {
            register_exec_key(key);
        }
        let net_for_registry = net.clone();
        let parent_pid_hint = parent
            .as_ref()
            .and_then(|parent| parent.upgrade())
            .map(|parent| parent.pid)
            .unwrap_or(0);
        // 构造 PCB 只发布页表对象，不代表任何 CPU 已经进入该 MM；真正的 CPU
        // 驻留登记统一发生在返回用户态前的 `activate_user_vm()`。
        let user_token = vm.read(|vm| vm.token());
        let pcb = Self {
            pid,
            pid_handle,
            process_quota: Mutex::new(Some(process_quota)),
            thread_group: Mutex::new(ThreadGroupState {
                members: Vec::new(),
                exec: None,
            }),
            group_exit_code: AtomicU64::new(0),
            live_threads: AtomicUsize::new(0),
            exec_owner_tid: AtomicUsize::new(NO_EXEC_OWNER),
            trap_context_cache: Mutex::new(Vec::new()),
            child_exit_wait: Mutex::new(WaitQueue::new()),
            vfork_parent: Mutex::new(None),
            vfork_done: Completion::new(),
            adopted_by_init: AtomicBool::new(false),
            pgid_hint: AtomicUsize::new(pgid),
            sid_hint: AtomicUsize::new(sid),
            parent_pid_hint: AtomicUsize::new(parent_pid_hint),
            user_token_hint: AtomicUsize::new(user_token),
            inner: Mutex::new(ProcessInner {
                exe,
                exec_key,
                exe_path,
                files,
                fs,
                uts,
                net,
                mnt,
                ipc,
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
                ptrace_tracer_pid: None,
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
            }),
            shared_pending_hint: AtomicU64::new(0),
        };
        super::net_namespace::register_ns_for_pid(pid, &net_for_registry);
        pcb
    }

    /// 获取进程内部状态锁。
    pub fn acquire_inner_lock(&self) -> MutexGuard<ProcessInner> {
        self.inner.lock()
    }

    /// 释放进程 PID/TGID。
    pub fn release_pid(&self) {
        self.pid_handle.release();
    }

    /// 返回 PID 是否已经释放。
    pub fn pid_released(&self) -> bool {
        self.pid_handle.is_released()
    }

    /// 克隆 PID/TGID 的分配器句柄，供非 leader exec 接管身份。
    pub(crate) fn pid_handle(&self) -> Arc<TidHandle> {
        self.pid_handle.clone()
    }

    pub fn exe(&self) -> Arc<Mutex<Arc<vfs::File>>> {
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

    pub fn replace_exe(&self, exe: Arc<vfs::File>) {
        let new_key = exec_key_from_file(&*exe);
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

    pub fn net(&self) -> Arc<NetNamespace> {
        self.inner.lock().net.clone()
    }

    pub fn unshare_net(&self) -> Arc<NetNamespace> {
        let new_ns = NetNamespace::new_isolated();
        self.set_net(new_ns.clone());
        super::net_namespace::register_ns_for_pid(self.pid, &new_ns);
        new_ns
    }

    /// 替换当前进程的网络命名空间。
    pub fn set_net(&self, net: Arc<NetNamespace>) {
        super::net_namespace::register_ns_for_pid(self.pid, &net);
        self.inner.lock().net = net;
    }

    pub fn mnt(&self) -> Arc<MountNamespace> {
        self.inner.lock().mnt.clone()
    }

    pub fn set_mnt(&self, mnt: Arc<MountNamespace>) {
        self.inner.lock().mnt = mnt;
    }

    pub fn ipc(&self) -> Arc<IpcNamespace> {
        self.inner.lock().ipc.clone()
    }

    pub fn set_ipc(&self, ipc: Arc<IpcNamespace>) {
        self.inner.lock().ipc = ipc;
    }

    pub fn vm(&self) -> Arc<AddressSpace<PageTableImpl>> {
        self.inner.lock().vm.clone()
    }

    /// 替换当前地址空间。
    ///
    /// # Semantics
    ///
    /// `execve` 使用该接口提交新 `AddressSpaceInner`。提交时会清空 trap context 槽位缓存、
    /// 更新无锁 user token hint，并刷新当前 CPU 上缓存的当前进程 token。
    pub fn replace_vm(&self, vm: AddressSpaceInner<PageTableImpl>) {
        let token = vm.token();
        self.trap_context_cache.lock().clear();
        self.inner.lock().vm = Arc::new(AddressSpace::new(vm));
        self.user_token_hint.store(token, Ordering::Relaxed);
    }

    pub fn user_token(&self) -> usize {
        self.user_token_hint.load(Ordering::Relaxed)
    }

    /// 返回用户态前登记本 CPU 对当前 MM 的可见性，并取得权威页表根/ASID 快照。
    ///
    /// trap-return 必须调用本入口，不能只读取无锁 token hint；登记与 generation
    /// 检查需要和页表修改共用 VM 锁，才能闭合“加入 mask 与修改方快照”的竞态。
    pub(crate) fn activate_user_vm(&self) -> UserVmContext {
        let vm = self.vm();
        super::processor::switch_user_vm(vm)
    }

    pub fn sighand(&self) -> Arc<Mutex<Sighand>> {
        self.inner.lock().sighand.clone()
    }

    pub fn futex(&self) -> Arc<Mutex<FutexTable>> {
        self.inner.lock().futex.clone()
    }

    /// 为已经进入提交阶段的 `execve` 隔离并重置进程资源。
    ///
    /// 调用前必须先通过 [`ExecSession`] 停止同线程组的其他线程。其它 PCB 仍可能
    /// 通过 `CLONE_FILES`、`CLONE_SIGHAND` 或 `CLONE_VM` 持有旧对象，因此只有
    /// 引用唯一时才能原地修改；共享对象必须先复制或替换，不能污染其它进程。
    pub(crate) fn reset_exec_resources(&self) -> Result<(), SyscallErr> {
        assert_eq!(
            self.live_thread_count(),
            1,
            "exec resources reset while sibling threads are still live"
        );

        let (files_shared, sighand_shared, old_files, old_sighand) = {
            let inner = self.inner.lock();
            (
                Arc::strong_count(&inner.files) > 1,
                Arc::strong_count(&inner.sighand) > 1,
                inner.files.clone(),
                inner.sighand.clone(),
            )
        };

        let files = if files_shared {
            Arc::new(Mutex::new(old_files.lock().try_clone()?))
        } else {
            old_files
        };
        crate::syscall::fs::close_cloexec_and_release_fcntl_locks(
            self.pid,
            &mut files.lock(),
        );

        let sighand = if sighand_shared {
            Arc::new(Mutex::new(Sighand::from_existing(&old_sighand.lock())))
        } else {
            old_sighand
        };
        sighand.lock().reset_for_exec();

        let mut inner = self.inner.lock();
        inner.files = files;
        inner.sighand = sighand;
        // private futex key 属于旧地址空间；新映像不能继承或清空共享 PCB 的等待表。
        let old_futex =
            core::mem::replace(&mut inner.futex, Arc::new(Mutex::new(FutexTable::new())));
        drop(inner);
        // FutexTable 析构会释放 waiter、Weak 和容器存储，可能进入 allocator；
        // 不能把这条析构链放在 process.inner 锁内。
        drop(old_futex);
        Ok(())
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

    /// 在线程组门禁内提交“成员登记 + 首次调度发布”。
    ///
    /// `publish` 只能取得一个 runqueue 的短锁并完成 `New -> Queued(cpu)`，不得
    /// 等待 IPI/TLB ack。这样 group exit/exec 要么先关闭门禁并拒绝本线程，
    /// 要么在本线程已经进入成员表和 runqueue 后取得锁并把它纳入停止快照。
    pub(crate) fn publish_thread(
        &self,
        task: &Arc<TaskControlBlock>,
        publish: impl FnOnce(),
    ) -> bool {
        let mut group = self.thread_group.lock();
        if self.group_exit_code.load(Ordering::Acquire) != 0 || group.exec.is_some() {
            return false;
        }
        assert!(
            !task.thread_live_counted.load(Ordering::Acquire),
            "thread published twice: tid={}",
            task.gettid()
        );
        group.members.push(Arc::downgrade(task));
        task.thread_live_counted.store(true, Ordering::Release);
        self.live_threads.fetch_add(1, Ordering::Relaxed);
        publish();
        true
    }

    /// 在线程完成全部线程级清理后发布退出 ack。
    ///
    /// # Semantics
    ///
    /// 返回 `Some(remaining)` 表示本次调用消费了该线程唯一的 live token；
    /// `remaining == 0` 的线程独占进程级退出收尾。AcqRel RMW 组成退出链，
    /// 保证最后一个线程开始释放共享 PCB/MM 前能观察到所有 sibling 的清理。
    pub fn remove_thread(&self, task: &TaskControlBlock) -> Option<usize> {
        if !task.thread_live_counted.swap(false, Ordering::AcqRel) {
            return None;
        }
        let previous = self.live_threads.fetch_sub(1, Ordering::AcqRel);
        assert_ne!(previous, 0, "live-thread count underflow");
        let remaining = previous - 1;
        let siblings_done = {
            let mut group = self.thread_group.lock();
            let compact_threshold = remaining.saturating_mul(4).saturating_add(128);
            if group.members.len() > compact_threshold {
                group.members.retain(|thread| {
                    thread
                        .upgrade()
                        .map(|task| task.thread_live_counted.load(Ordering::Acquire))
                        .unwrap_or(false)
                });
            }
            // live token 在全部用户资源和 TLB 清理后才递减。计数到 1 表示只剩
            // exec owner，可在释放线程组锁后唤醒它安装新地址空间。
            (remaining == 1)
                .then(|| group.exec.as_ref().map(|state| state.siblings_done.clone()))
                .flatten()
        };
        if let Some(siblings_done) = siblings_done {
            siblings_done.complete();
        }
        Some(remaining)
    }

    /// 返回当前仍计为 live 的线程列表，并清理失效弱引用。
    pub fn threads(&self) -> Vec<Arc<TaskControlBlock>> {
        let mut group = self.thread_group.lock();
        let mut live_threads = Vec::new();
        group.members.retain(|thread| {
            if let Some(task) = thread.upgrade() {
                if task.thread_live_counted.load(Ordering::Acquire) {
                    live_threads.push(task);
                    true
                } else {
                    false
                }
            } else {
                false
            }
        });
        live_threads
    }

    /// 返回任意一个非 zombie live 线程。
    pub fn any_live_thread(&self) -> Option<Arc<TaskControlBlock>> {
        self.threads().into_iter().find(|task| !task.is_zombie())
    }

    /// 返回 live-thread 计数。
    pub fn live_thread_count(&self) -> usize {
        self.live_threads.load(Ordering::Acquire)
    }

    /// 返回成员弱引用槽位数和当前可升级槽位数，仅供内核统计。
    pub fn thread_slot_stats(&self) -> (usize, usize) {
        let group = self.thread_group.lock();
        (
            group.members.len(),
            group
                .members
                .iter()
                .filter(|thread| thread.upgrade().is_some())
                .count(),
        )
    }

    /// 尝试缓存一个 trap context 槽位以便线程复用。
    ///
    /// # Semantics
    ///
    /// group exit、exec sibling 清理或当前线程已经是最后一个 live 成员时拒绝
    /// 缓存，避免即将被替换/释放的地址空间继续保留用户资源页。
    pub fn try_cache_trap_context_slot(&self, slot: usize) -> bool {
        // exit ack 现在位于线程资源清理末尾，调用本函数时当前线程仍计入 live。
        // 与 begin_exec()/begin_group_exit() 共用线程组锁，关闭“检查通过后才发布
        // exec、随后把 trap 页缓存进外部共享旧 VM”的窗口。
        let group = self.thread_group.lock();
        if self.is_group_exiting() || group.exec.is_some() || self.live_thread_count() <= 1 {
            super::perf::record_trap_cache_store(false);
            return false;
        }
        let mut cache = self.trap_context_cache.lock();
        if cache.len() >= TRAP_CONTEXT_CACHE_LIMIT || cache.iter().any(|cached| *cached == slot) {
            super::perf::record_trap_cache_store(false);
            return false;
        }
        cache.push(slot);
        super::perf::record_trap_cache_store(true);
        true
    }

    /// 从 trap context 缓存中取走指定槽位。
    pub fn take_cached_trap_context_slot(&self, slot: usize) -> bool {
        let mut cache = self.trap_context_cache.lock();
        if let Some(pos) = cache.iter().position(|cached| *cached == slot) {
            cache.swap_remove(pos);
            super::perf::record_trap_cache_take(true);
            true
        } else {
            super::perf::record_trap_cache_take(false);
            false
        }
    }

    pub fn setpgid(&self, pgid: usize) -> isize {
        if (pgid as isize) < 0 {
            return -1;
        }
        self.inner.lock().pgid = pgid;
        self.pgid_hint.store(pgid, Ordering::Relaxed);
        0
    }

    pub fn getpgid(&self) -> usize {
        self.pgid_hint.load(Ordering::Relaxed)
    }

    pub fn setsid(&self, sid: usize) -> isize {
        let mut inner = self.inner.lock();
        inner.sid = sid;
        inner.pgid = sid;
        self.sid_hint.store(sid, Ordering::Relaxed);
        self.pgid_hint.store(sid, Ordering::Relaxed);
        0
    }

    pub fn getsid(&self) -> usize {
        self.sid_hint.load(Ordering::Relaxed)
    }

    pub fn parent(&self) -> Option<Arc<ProcessControlBlock>> {
        self.inner
            .lock()
            .parent
            .as_ref()
            .and_then(|parent| parent.upgrade())
    }

    pub fn parent_pid(&self) -> usize {
        self.parent_pid_hint.load(Ordering::Relaxed)
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

    /// 标记进程进入 zombie 状态。
    ///
    /// # Semantics
    ///
    /// 首次成功转换返回 `true`；重复调用返回 `false`。调用方负责随后唤醒父进程
    /// wait 队列和执行资源回收。
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

    /// 标记进程被 stop 信号停止，并唤醒父进程或 tracer 的 wait 队列。
    pub fn mark_stopped(&self, signum: usize) {
        let tracer_pid = {
            let inner = self.inner.lock();
            inner.ptrace_tracer_pid
        };
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
        if let Some(tracer_pid) = tracer_pid {
            if let Some(tracer) = registry::find_process_by_pid(tracer_pid) {
                tracer.child_exit_wait.lock().wake_all();
            }
        }
    }

    /// 标记进程继续运行，并生成一次 wait 可见的 continued 事件。
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

    /// 取出一次 wait 可见的 stopped 状态。
    ///
    /// `nowait = true` 时只观察状态，不消费。
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

    /// 建立 ptrace attach 状态并让 tracee 进入 stopped。
    pub fn ptrace_attach(&self, tracer_pid: usize, stop_signum: usize) -> Result<(), SyscallErr> {
        {
            let mut inner = self.inner.lock();
            if inner.state == ProcessState::Zombie {
                return Err(SyscallErr::ESRCH);
            }
            if inner.ptrace_tracer_pid.is_some() {
                return Err(SyscallErr::EPERM);
            }
            inner.ptrace_tracer_pid = Some(tracer_pid);
            inner.state = ProcessState::Stopped;
            inner.stopped_signal = Some(stop_signum);
            inner.stopped_reported = false;
            inner.continued_pending = false;
        }
        if let Some(tracer) = registry::find_process_by_pid(tracer_pid) {
            tracer.child_exit_wait.lock().wake_all();
        }
        Ok(())
    }

    /// 取消 ptrace attach 状态并继续 tracee。
    pub fn ptrace_detach(&self, tracer_pid: usize) -> Result<(), SyscallErr> {
        {
            let mut inner = self.inner.lock();
            if inner.ptrace_tracer_pid != Some(tracer_pid) {
                return Err(SyscallErr::ESRCH);
            }
            inner.ptrace_tracer_pid = None;
        }
        self.mark_continued();
        Ok(())
    }

    pub fn ptrace_traced_by(&self, tracer_pid: usize) -> bool {
        self.inner.lock().ptrace_tracer_pid == Some(tracer_pid)
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

    /// 将信号加入进程共享 pending 队列。
    ///
    /// # Locking
    ///
    /// 只持有 `signal` 锁，不持有任何任务锁。`shared_pending_hint` 在锁释放前更新，
    /// 供等待路径无锁快速判断。
    pub fn enqueue_process_signal(&self, pending: PendingSignal) {
        let pending_bits = {
            let mut state = self.signal.lock();
            let _ = state.shared_pending.enqueue(pending);
            state.shared_pending.pending().bits() as u64
        };
        self.shared_pending_hint
            .store(pending_bits, Ordering::Relaxed);
    }

    /// 返回进程共享 pending signal 位图。
    pub fn shared_pending(&self) -> Signals {
        self.signal.lock().shared_pending.pending()
    }

    /// 返回进程共享 pending signal 的无锁 hint。
    pub fn shared_pending_hint(&self) -> Signals {
        Signals::from_bits_truncate(
            self.shared_pending_hint.load(Ordering::Relaxed) as signal_type!()
        )
    }

    /// 从进程共享 pending 队列移除一个信号。
    pub fn take_shared_signal(&self, signal: Signals) -> bool {
        let (removed, pending_bits) = {
            let mut state = self.signal.lock();
            let removed = state.shared_pending.remove_signal(signal);
            (removed, state.shared_pending.pending().bits() as u64)
        };
        self.shared_pending_hint
            .store(pending_bits, Ordering::Relaxed);
        removed
    }

    /// 从进程共享 pending 队列取出第一个属于 `set` 的信号。
    pub fn take_shared_matching(&self, set: Signals) -> Option<PendingSignal> {
        let (pending, pending_bits) = {
            let mut state = self.signal.lock();
            let pending = state.shared_pending.dequeue_matching(set);
            (pending, state.shared_pending.pending().bits() as u64)
        };
        self.shared_pending_hint
            .store(pending_bits, Ordering::Relaxed);
        pending
    }

    /// 临时关闭 clone 发布门，并取得多线程 exec 需要停止的 sibling 快照。
    ///
    /// 新映像尚未安装；调用者只能请求 sibling 在各自 CPU 自行退出，然后等待
    /// `ExecSession`。group exit 或另一场 exec 已经关闭门禁时返回 `EAGAIN`。
    pub(crate) fn begin_exec(
        self: &Arc<Self>,
        owner_tid: usize,
    ) -> Result<(ExecSession, Vec<Arc<TaskControlBlock>>), isize> {
        let siblings_done = Arc::new(Completion::new());
        let mut siblings = Vec::new();
        let mut group = self.thread_group.lock();
        if self.group_exit_code.load(Ordering::Relaxed) != 0 || group.exec.is_some() {
            return Err(crate::syscall::errno::EAGAIN);
        }
        siblings
            .try_reserve(group.members.len())
            .map_err(|_| crate::syscall::errno::ENOMEM)?;

        let mut owner_is_live = false;
        group.members.retain(|thread| {
            if let Some(task) = thread.upgrade() {
                if task.thread_live_counted.load(Ordering::Acquire) {
                    if task.gettid() == owner_tid {
                        owner_is_live = true;
                    } else {
                        siblings.push(task);
                    }
                    return true;
                }
            }
            false
        });
        assert!(
            owner_is_live,
            "exec owner is not a live thread-group member"
        );

        group.exec = Some(ExecState {
            owner_tid,
            siblings_done: siblings_done.clone(),
        });
        // 权威会话已经在成员锁内建立，再用 Release 发布安全点快照。
        self.exec_owner_tid.store(owner_tid, Ordering::Release);
        drop(group);

        // 不能只凭快照为空判定完成：退出线程可能已经清除自己的 live token，
        // 但尚未递减进程计数。只有权威计数为 1 才能确认 owner 独占旧地址空间；
        // 否则退出线程会在 remove_thread() 把计数降到 1 后完成该事件。
        if self.live_thread_count() == 1 {
            siblings_done.complete();
        }
        Ok((
            ExecSession {
                process: self.clone(),
                owner_tid,
                siblings_done,
            },
            siblings,
        ))
    }

    /// 当前 tid 是否是 active exec 必须停止的 sibling。
    pub(crate) fn exec_stops_thread(&self, tid: usize) -> bool {
        let owner = self.exec_owner_tid.load(Ordering::Acquire);
        owner != NO_EXEC_OWNER && owner != tid
    }

    /// 当前线程是否必须因永久 group exit 或另一线程的 exec 而退出。
    pub(crate) fn thread_must_exit(&self, tid: usize) -> bool {
        self.is_group_exiting() || self.exec_stops_thread(tid)
    }

    /// clone 首次发布是否已被永久退出或临时 exec 门禁关闭。
    pub(crate) fn thread_publish_blocked(&self) -> bool {
        self.is_group_exiting() || self.exec_owner_tid.load(Ordering::Acquire) != NO_EXEC_OWNER
    }

    /// 原子关闭新线程发布门禁，并取得需要停止的 live-thread 快照。
    ///
    /// 第一个调用者决定统一退出码；后续并发 fatal signal/exit_group 复用它。
    pub fn begin_group_exit(&self, exit_code: u32) -> (u32, Vec<Arc<TaskControlBlock>>) {
        let mut group = self.thread_group.lock();
        let encoded = self.group_exit_code.load(Ordering::Relaxed);
        let exit_code = if encoded == 0 {
            // 在成员锁内先 Release 发布门禁，再形成 live 成员快照。发布方取得
            // 同一把锁后一定会观察到非零值，不能落到本次快照之后。
            self.group_exit_code
                .store(u64::from(exit_code) + 1, Ordering::Release);
            exit_code
        } else {
            (encoded - 1) as u32
        };
        let mut live_threads = Vec::new();
        group.members.retain(|thread| {
            if let Some(task) = thread.upgrade() {
                if task.thread_live_counted.load(Ordering::Acquire) {
                    live_threads.push(task);
                    true
                } else {
                    false
                }
            } else {
                false
            }
        });
        (exit_code, live_threads)
    }

    /// 返回线程组是否正在退出。
    pub fn is_group_exiting(&self) -> bool {
        self.group_exit_code.load(Ordering::Acquire) != 0
    }

    /// 返回线程组退出码。
    pub fn group_exit_code(&self) -> Option<u32> {
        let encoded = self.group_exit_code.load(Ordering::Acquire);
        if encoded == 0 {
            None
        } else {
            Some((encoded - 1) as u32)
        }
    }

    /// 添加 waitable 子进程。
    ///
    /// # Errors
    ///
    /// children 列表扩容失败时返回 `-ENOMEM`，调用方必须回滚尚未发布的 clone。
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

    /// 从 child tree 中移除指定子进程。
    pub fn detach_child(&self, child_pid: usize) {
        self.inner
            .lock()
            .children
            .retain(|child| child.pid != child_pid);
    }

    /// 更新父进程引用和无锁 parent-pid hint。
    pub fn set_parent(&self, parent: Option<Weak<ProcessControlBlock>>) {
        let parent_pid = parent
            .as_ref()
            .and_then(|parent| parent.upgrade())
            .map(|parent| parent.pid)
            .unwrap_or(0);
        self.inner.lock().parent = parent;
        self.parent_pid_hint.store(parent_pid, Ordering::Relaxed);
    }

    /// 记录 `CLONE_VFORK` 父线程。
    pub fn set_vfork_parent(&self, parent: &Arc<TaskControlBlock>) {
        *self.vfork_parent.lock() = Some(Arc::downgrade(parent));
    }

    /// 完成 vfork，同步唤醒等待的父线程。
    pub fn complete_vfork(&self) {
        let mut parent = self.vfork_parent.lock();
        if parent.is_none() {
            return;
        }
        *parent = None;
        drop(parent);
        self.vfork_done.complete();
    }

    /// 等待 vfork 子进程完成；线程组停止请求可以提前中止父线程等待。
    pub fn wait_vfork_done_killable(&self) -> WaitResult {
        self.vfork_done.wait_killable()
    }

    /// 取走所有子进程列表。
    pub fn take_children(&self) -> Vec<Arc<ProcessControlBlock>> {
        let mut inner = self.inner.lock();
        core::mem::take(&mut inner.children)
    }

    fn wake_child_waiters(process: &Arc<ProcessControlBlock>) {
        process.child_exit_wait.lock().wake_all();
        if let Some(task) = process.any_live_thread() {
            let _ = wake_interruptible(task);
        }
    }

    fn nearest_child_reaper(parent: Option<Arc<ProcessControlBlock>>) -> Arc<ProcessControlBlock> {
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

    /// 关闭进程 fd table 中的所有文件。
    ///
    /// # Locking
    ///
    /// 先复制 fd 列表，再逐个 drop fd，避免遍历时修改 fd table 迭代器状态。
    pub fn close_files_on_exit(&self) {
        let files_ref = self.files();
        let mut fd_table = files_ref.lock();
        let open_fds: Vec<usize> = fd_table.iter().map(|(i, _f)| i).collect();
        for fd in open_fds {
            if let Ok(file) = fd_table.drop_fd(fd) {
                crate::syscall::fs::release_flock_for_file_if_last(&file);
            }
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
        let resident_kb = self.vm().read(|vm| vm.resident_user_bytes()) / 1024;
        rusage.update_maxrss_kb(resident_kb);
        if !self.mark_zombie(exit_code, rusage) {
            return;
        }
        // 在 mark_zombie 之后重新获取 parent：其它 CPU 可能同时完成 reparent，
        // 因此不能沿用进入 finish_exit() 前取得的父进程快照。
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
                let exit_signal = exit_task.exit_signal();
                if !exit_signal.is_empty() {
                    if let Some(parent_task) = parent_process.any_live_thread() {
                        let mut parent_inner = parent_task.acquire_inner_lock();
                        parent_inner.add_signal(exit_signal);
                        drop(parent_inner);
                        let _ = wake_interruptible(parent_task);
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
            vm.write(|vm| vm.release_for_zombie());
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
