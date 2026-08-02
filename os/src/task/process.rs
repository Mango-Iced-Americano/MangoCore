//! 进程控制块与进程级生命周期。
//!
//! `ProcessControlBlock` 保存线程组共享状态：地址空间、fd table、namespace、
//! 信号动作、进程级 pending signal、child tree、wait 状态和 zombie 回收信息。
//! 线程级运行状态保存在 `TaskControlBlock` 中。
//!
//! # Locking
//!
//! `ProcessControlBlock::inner` 保护进程结构性状态，`signal` 单独保护进程共享
//! pending signal，`rlimits` 保护完整的资源限制 pair，`thread_group` 则把线程
//! 成员关系与 group-exit 发布门禁放在同一锁域。涉及调度队列、父子关系和资源
//! 析构时，遵循“锁内移动 Arc/记录状态，锁外执行唤醒或析构”的顺序。

use super::{
    pid::{RecycleAllocator, TidHandle},
    quota::TaskQuotaGuard,
    registry,
    signal::{
        queue_process_signal_info, sigchld_requests_auto_reap, wake_process_signal_waiter,
        PendingSignal, SigInfo, Sighand, SignalQueue, Signals,
    },
    threads::FutexTable,
    wake_interruptible, Completion, FsStatus, IpcNamespace, MountNamespace, NetNamespace, Rusage,
    TaskControlBlock, UtsNamespace, WaitQueue, WaitResult, INITPROC,
};
use crate::config::{SYSTEM_TASK_LIMIT, USER_STACK_SIZE};
use crate::fs::vfs;
use crate::mm::{AddressSpace, AddressSpaceInner, PageTableImpl, UserVmContext};
use crate::signal_type;
use crate::timer::{TimeSpec, USEC_PER_SEC};
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

/// 一项资源限制的软、硬上限。
///
/// 两个字段始终由 `ProcessControlBlock::rlimits` 的同一个锁保护，调用方不得
/// 拆成两个临界区读取或发布。
#[derive(Clone, Copy, Debug)]
pub(crate) struct LimitPair {
    pub(crate) soft: usize,
    pub(crate) hard: usize,
}

impl LimitPair {
    pub(crate) const fn new(soft: usize, hard: usize) -> Self {
        Self { soft, hard }
    }
}

/// 线程组共享的资源限制。
///
/// Linux 把 rlimit 放在线程组共享状态中；MangoCore 对应放在 PCB，使同一
/// 进程的所有 TCB 观察同一个 owner。NOFILE 暂不在本结构内，因为它仍与
/// fd table 及 `CLONE_FILES` 的生命周期耦合。
#[derive(Clone, Copy, Debug)]
pub(crate) struct ProcessLimits {
    /// 整个线程组可消耗的 CPU 秒数。
    pub(crate) cpu: LimitPair,
    /// 普通文件可增长到的最大字节数。
    pub(crate) fsize: LimitPair,
    /// 用户栈限制；当前只保存 ABI 状态。
    pub(crate) stack: LimitPair,
    /// core dump 限制；当前用于 wait status 的 WCOREDUMP 位。
    pub(crate) core: LimitPair,
    /// 进程数限制；当前只保存 ABI 状态。
    pub(crate) nproc: LimitPair,
    /// 可锁定内存字节数。
    pub(crate) memlock: LimitPair,
    /// 实时 pending signal 数量上限。
    pub(crate) sigpending: LimitPair,
    /// 非特权 nice 调整上限；当前只保存 ABI 状态。
    pub(crate) nice: LimitPair,
    /// 非特权实时调度优先级上限。
    pub(crate) rtprio: LimitPair,
}

impl Default for ProcessLimits {
    fn default() -> Self {
        Self {
            cpu: LimitPair::new(usize::MAX, usize::MAX),
            fsize: LimitPair::new(usize::MAX, usize::MAX),
            stack: LimitPair::new(USER_STACK_SIZE, USER_STACK_SIZE),
            core: LimitPair::new(0, usize::MAX),
            nproc: LimitPair::new(SYSTEM_TASK_LIMIT, SYSTEM_TASK_LIMIT),
            memlock: LimitPair::new(usize::MAX, usize::MAX),
            sigpending: LimitPair::new(usize::MAX, usize::MAX),
            nice: LimitPair::new(usize::MAX, usize::MAX),
            rtprio: LimitPair::new(0, 0),
        }
    }
}

/// 线程组 CPU 限额的无锁热路径状态。
///
/// `user_us`/`system_us` 供 ABI 查询，`runtime_us` 作为单一权威总量判定
/// CPU limit；`next_expiry_us` 是 rlimit owner 发布的最近阈值。真正修改
/// soft limit 和生成信号只在用户返回安全点完成，trap 记账路径不获取
/// PCB mutex，也不做 seqlock 自旋。
struct ProcessCpuAccount {
    /// 已冲刷的线程组用户态 CPU 时间。
    user_us: AtomicU64,
    /// 已冲刷的线程组内核态 CPU 时间。
    system_us: AtomicU64,
    /// user + system 的权威饱和总量，专供限额到期判断。
    runtime_us: AtomicU64,
    next_expiry_us: AtomicU64,
    expiry_pending: AtomicBool,
}

impl ProcessCpuAccount {
    fn limit_us(limit_secs: usize) -> u64 {
        if limit_secs == usize::MAX {
            u64::MAX
        } else {
            (limit_secs as u64)
                .saturating_mul(USEC_PER_SEC as u64)
                .min(u64::MAX - 1)
        }
    }

    fn next_expiry(limit: LimitPair) -> u64 {
        Self::limit_us(limit.soft).min(Self::limit_us(limit.hard))
    }

    fn new(limit: LimitPair) -> Self {
        Self {
            user_us: AtomicU64::new(0),
            system_us: AtomicU64::new(0),
            runtime_us: AtomicU64::new(0),
            next_expiry_us: AtomicU64::new(Self::next_expiry(limit)),
            expiry_pending: AtomicBool::new(false),
        }
    }

    fn add_saturating(counter: &AtomicU64, delta: u64) -> u64 {
        let previous = counter
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                Some(value.saturating_add(delta))
            })
            .unwrap();
        previous.saturating_add(delta)
    }
}

/// 单个 POSIX timer 的进程级状态。
///
/// `arm_seq` 标识最近一次装载；内核 timer 节点必须同时匹配 timer ID、
/// `arm_seq` 和 deadline，才能修改本对象。这样删除并复用相同 ID 后，旧节点
/// 也不能误命中新 timer。
#[derive(Clone)]
pub struct PosixTimer {
    pub clock_id: usize,
    pub signal: Signals,
    pub signal_value: usize,
    pub interval: TimeSpec,
    pub value: TimeSpec,
    /// 转换到内核 monotonic 域的 wall timer 到期点。
    pub wall_deadline: Option<TimeSpec>,
    /// CLOCK_REALTIME 类绝对 timer 的原始墙钟目标。
    pub realtime_abs_deadline: Option<TimeSpec>,
    /// CPU clock 域内的绝对到期时间；wall timer 必须保持为 None。
    pub cpu_deadline_us: Option<u64>,
    pub arm_seq: u64,
    /// None 表示 wall clock；thread timer 保存对象身份而不是可复用 TID。
    cpu_clock: Option<PosixCpuClock>,
    overrun: usize,
}

#[derive(Clone)]
enum PosixCpuClock {
    Process,
    Thread(Weak<TaskControlBlock>),
}

impl PosixTimer {
    /// 单个进程可同时持有的 POSIX timer 数量，也限定安全点栈上事件批次。
    pub(crate) const MAX_COUNT: usize = 32;
    const OVERRUN_MAX: usize = i32::MAX as usize;

    fn new(
        clock_id: usize,
        signal: Signals,
        signal_value: usize,
        cpu_clock: Option<PosixCpuClock>,
    ) -> Self {
        Self {
            clock_id,
            signal,
            signal_value,
            interval: TimeSpec::new(),
            value: TimeSpec::new(),
            wall_deadline: None,
            realtime_abs_deadline: None,
            cpu_deadline_us: None,
            arm_seq: 0,
            cpu_clock,
            overrun: 0,
        }
    }

    pub(crate) fn new_wall(clock_id: usize, signal: Signals, signal_value: usize) -> Self {
        Self::new(clock_id, signal, signal_value, None)
    }

    pub(crate) fn new_process_cpu(clock_id: usize, signal: Signals, signal_value: usize) -> Self {
        Self::new(clock_id, signal, signal_value, Some(PosixCpuClock::Process))
    }

    pub(crate) fn new_thread_cpu(
        clock_id: usize,
        signal: Signals,
        signal_value: usize,
        creator: &Arc<TaskControlBlock>,
    ) -> Self {
        Self::new(
            clock_id,
            signal,
            signal_value,
            Some(PosixCpuClock::Thread(Arc::downgrade(creator))),
        )
    }

    pub(crate) fn is_cpu_clock(&self) -> bool {
        self.cpu_clock.is_some()
    }

    /// 把用户 timespec 上取整到 MangoCore CPU 记账的微秒域，非零值不会变成 0。
    pub(crate) fn cpu_duration_us(value: TimeSpec) -> u64 {
        let ns = value.to_ns_saturating();
        ns / 1_000 + u64::from(ns % 1_000 != 0)
    }

    /// 采样本 timer 的 CPU clock；thread owner 已退出时返回 None。
    pub(crate) fn cpu_time_us(&self, process: &ProcessControlBlock) -> Option<u64> {
        match self.cpu_clock.as_ref()? {
            PosixCpuClock::Process => Some(process.cpu_runtime_us()),
            PosixCpuClock::Thread(target) => target
                .upgrade()
                .filter(|task| !task.is_zombie())
                .map(|task| task.cpu_time_us()),
        }
    }

    fn targets_current_thread(&self, current: &Arc<TaskControlBlock>) -> bool {
        match self.cpu_clock.as_ref() {
            Some(PosixCpuClock::Process) => true,
            Some(PosixCpuClock::Thread(target)) => target
                .upgrade()
                .filter(|task| !task.is_zombie())
                .map(|task| Arc::ptr_eq(&task, current))
                .unwrap_or(false),
            None => false,
        }
    }

    pub(crate) fn pending_signal(&self) -> Option<PendingSignal> {
        if self.signal.is_empty() {
            return None;
        }
        Some(PendingSignal {
            signal: self.signal,
            siginfo: SigInfo::new_with_sender_value(
                self.signal.to_signum().unwrap(),
                0,
                SigInfo::SI_TIMER as usize,
                0,
                self.signal_value,
            ),
        })
    }

    pub fn reset_overrun(&mut self) {
        self.overrun = 0;
    }

    pub fn add_overrun(&mut self, count: usize) {
        self.overrun = self.overrun.saturating_add(count).min(Self::OVERRUN_MAX);
    }

    pub fn overrun(&self) -> usize {
        self.overrun
    }
}

enum PosixTimerSlot {
    Vacant,
    /// timer_create 已保留 ID，但 timerid 尚未成功写回用户态。
    Reserved,
    Active(PosixTimer),
}

/// 同一线程组共享的 POSIX timer 表。
///
/// slot 预留使 `timer_create()` 无需跨 faultable copyout 持锁，同时避免另一个
/// CPU 抢占同一 ID。`next_arm_seq` 是整个表共享的单调序列，不能按 slot 重置。
pub(crate) struct PosixTimerTable {
    slots: Vec<PosixTimerSlot>,
    next_arm_seq: u64,
}

impl PosixTimerTable {
    fn new() -> Self {
        Self {
            slots: Vec::new(),
            next_arm_seq: 1,
        }
    }

    pub(crate) fn reserve_id(&mut self) -> Result<usize, SyscallErr> {
        if let Some((id, slot)) = self
            .slots
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| matches!(slot, PosixTimerSlot::Vacant))
        {
            *slot = PosixTimerSlot::Reserved;
            return Ok(id);
        }
        if self.slots.len() >= PosixTimer::MAX_COUNT {
            return Err(SyscallErr::EAGAIN);
        }
        self.slots.try_reserve(1).map_err(|_| SyscallErr::ENOMEM)?;
        self.slots.push(PosixTimerSlot::Reserved);
        Ok(self.slots.len() - 1)
    }

    pub(crate) fn publish_reserved(&mut self, id: usize, timer: PosixTimer) -> bool {
        match self.slots.get_mut(id) {
            Some(slot @ PosixTimerSlot::Reserved) => {
                *slot = PosixTimerSlot::Active(timer);
                true
            }
            _ => false,
        }
    }

    pub(crate) fn cancel_reservation(&mut self, id: usize) {
        if let Some(slot @ PosixTimerSlot::Reserved) = self.slots.get_mut(id) {
            *slot = PosixTimerSlot::Vacant;
        }
    }

    pub(crate) fn get(&self, id: usize) -> Option<&PosixTimer> {
        match self.slots.get(id) {
            Some(PosixTimerSlot::Active(timer)) => Some(timer),
            _ => None,
        }
    }

    pub(crate) fn get_mut(&mut self, id: usize) -> Option<&mut PosixTimer> {
        match self.slots.get_mut(id) {
            Some(PosixTimerSlot::Active(timer)) => Some(timer),
            _ => None,
        }
    }

    pub(crate) fn remove(&mut self, id: usize) -> bool {
        match self.slots.get_mut(id) {
            Some(slot @ PosixTimerSlot::Active(_)) => {
                *slot = PosixTimerSlot::Vacant;
                true
            }
            _ => false,
        }
    }

    pub(crate) fn slot_count(&self) -> usize {
        self.slots.len()
    }

    pub(crate) fn alloc_arm_seq(&mut self) -> u64 {
        let seq = self.next_arm_seq;
        self.next_arm_seq = self.next_arm_seq.wrapping_add(1);
        if self.next_arm_seq == 0 {
            self.next_arm_seq = 1;
        }
        seq
    }

    fn has_live_cpu_timer(&self) -> bool {
        self.slots.iter().any(|slot| match slot {
            PosixTimerSlot::Active(timer) if timer.cpu_deadline_us.is_some() => {
                match timer.cpu_clock.as_ref() {
                    Some(PosixCpuClock::Process) => true,
                    Some(PosixCpuClock::Thread(target)) => target
                        .upgrade()
                        .map(|task| !task.is_zombie())
                        .unwrap_or(false),
                    None => false,
                }
            }
            _ => false,
        })
    }

    fn clear(&mut self) {
        // exec 后仍复用同一个 PCB；不能重置 next_arm_seq，否则 exec 前遗留的
        // heap 节点可能与新映像复用的 timer ID/序号形成 ABA。
        self.slots.clear();
    }
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
    /// rlimit 的线程组共享 owner；消费者只复制所需标量，不跨其他锁持有。
    rlimits: Mutex<ProcessLimits>,
    /// RLIMIT_CPU 的线程组累计和到期提示；热路径只访问原子字段。
    cpu_account: ProcessCpuAccount,
    /// POSIX timer 是线程组共享对象，独立锁避免 timer callback 争用 process.inner。
    posix_timers: Mutex<PosixTimerTable>,
    /// CPU timer 安全点的无锁 fast hint；权威状态仍在 posix_timers 锁内。
    posix_cpu_timers_active: AtomicBool,
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
    /// 进程退出时记录的线程组资源快照。
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

    /// 取得 rlimit owner guard，供 `prlimit()` 在一次临界区内提交完整 pair。
    pub(crate) fn rlimits(&self) -> MutexGuard<'_, ProcessLimits> {
        self.rlimits.lock()
    }

    /// fork 使用的原子快照；返回后不再持有父进程的任何锁。
    pub(crate) fn rlimits_snapshot(&self) -> ProcessLimits {
        *self.rlimits.lock()
    }

    /// 获取当前进程的 POSIX timer 表。
    ///
    /// guard 不得跨用户访存、timer queue 插入、信号唤醒或其它等待点。
    pub(crate) fn posix_timers(&self) -> MutexGuard<'_, PosixTimerTable> {
        self.posix_timers.lock()
    }

    /// 调用方持有 timer 表锁时同步 CPU timer fast hint。
    ///
    /// hint 与权威 slot 状态在同一临界区发布，arm/clear/scanner 不会互相覆盖。
    pub(crate) fn sync_posix_cpu_timer_hint(&self, timers: &PosixTimerTable) {
        self.posix_cpu_timers_active
            .store(timers.has_live_cpu_timer(), Ordering::Release);
    }

    fn clear_posix_timers(&self) {
        let mut timers = self.posix_timers.lock();
        timers.clear();
        self.posix_cpu_timers_active.store(false, Ordering::Release);
    }

    /// 在任务安全点推进由 CPU 消耗驱动的 POSIX timer。
    ///
    /// 当前线程和进程累计先在锁外采样；表锁只负责唯一领取到期状态。到期事件
    /// 使用固定栈数组带出锁，再进入可能扩容的 signal queue 和调度器。
    pub(crate) fn check_posix_cpu_timers(self: &Arc<Self>, current: &Arc<TaskControlBlock>) {
        if !self.posix_cpu_timers_active.load(Ordering::Acquire) {
            return;
        }

        let process_now_us = self.cpu_runtime_us();
        let thread_now_us = current.cpu_time_us();
        let mut events = [None; PosixTimer::MAX_COUNT];
        let mut event_count = 0usize;
        {
            let mut timers = self.posix_timers.lock();
            for slot in &mut timers.slots {
                let PosixTimerSlot::Active(timer) = slot else {
                    continue;
                };
                let Some(deadline_us) = timer.cpu_deadline_us else {
                    continue;
                };
                if !timer.targets_current_thread(current) {
                    continue;
                }
                let now_us = match timer.cpu_clock.as_ref() {
                    Some(PosixCpuClock::Process) => process_now_us,
                    Some(PosixCpuClock::Thread(_)) => thread_now_us,
                    None => continue,
                };
                if now_us < deadline_us {
                    continue;
                }

                if timer.interval.is_zero() {
                    timer.value = TimeSpec::new();
                    timer.cpu_deadline_us = None;
                } else {
                    let interval_us = PosixTimer::cpu_duration_us(timer.interval);
                    let missed = now_us.saturating_sub(deadline_us) / interval_us;
                    let expirations = missed.saturating_add(1);
                    let signal_pending = !timer.signal.is_empty()
                        && self.shared_pending_hint().contains(timer.signal);
                    let overruns = missed.saturating_add(signal_pending as u64);
                    timer.add_overrun(overruns.min(usize::MAX as u64) as usize);
                    timer.value = timer.interval;
                    timer.cpu_deadline_us =
                        Some(deadline_us.saturating_add(expirations.saturating_mul(interval_us)));
                }

                if let Some(event) = timer.pending_signal() {
                    events[event_count] = Some(event);
                    event_count += 1;
                }
            }
            self.sync_posix_cpu_timer_hint(&timers);
        }

        for event in events.iter().take(event_count).flatten().copied() {
            let _ = queue_process_signal_info(self, event.signal, event.siginfo);
            let _ = wake_process_signal_waiter(self, event.signal);
        }
    }

    /// 把一个线程的已结算 CPU 时间批量计入线程组。
    ///
    /// 调用方先在 `task.inner` 下领取批次并释放锁；本方法仍严格只做原子操作，
    /// 不能获取 rlimit/signal 锁或直接投递信号。
    pub(crate) fn account_cpu_time(&self, user_us: usize, system_us: usize) {
        let delta_us = user_us.saturating_add(system_us) as u64;
        if delta_us == 0 {
            return;
        }
        if user_us != 0 {
            ProcessCpuAccount::add_saturating(&self.cpu_account.user_us, user_us as u64);
        }
        if system_us != 0 {
            ProcessCpuAccount::add_saturating(&self.cpu_account.system_us, system_us as u64);
        }
        // 分项先更新，总量再参与限额判断。若本批触发到期，随后对 pending
        // 的 Release 发布会把这两个分项一并带到安全点；三个计数都只单调增加。
        let runtime = ProcessCpuAccount::add_saturating(&self.cpu_account.runtime_us, delta_us);
        let next = self.cpu_account.next_expiry_us.load(Ordering::Acquire);
        if next != u64::MAX && runtime >= next {
            self.cpu_account
                .expiry_pending
                .store(true, Ordering::Release);
        }
    }

    /// 返回线程组 user+system CPU 时间的单调权威总量。
    pub(crate) fn cpu_runtime_us(&self) -> u64 {
        self.cpu_account.runtime_us.load(Ordering::Acquire)
    }

    /// 返回已冲刷的线程组 user/system CPU 时间。
    ///
    /// 活进程快照允许分别观察到并发批次的新值或旧值，和 Linux 的 SMP
    /// `getrusage` 采样约定一致；已结算的本地尾数最多为 1ms，当前仍在运行
    /// 的 CPU 区间会在下一 trap/tick 结算。最后一个线程完成 AcqRel
    /// live-token 退出链后，本快照包含所有强制冲刷。
    pub(crate) fn cpu_rusage(&self) -> Rusage {
        Rusage::from_cpu_us(
            self.cpu_account.user_us.load(Ordering::Acquire),
            self.cpu_account.system_us.load(Ordering::Acquire),
        )
    }

    /// 在 rlimit owner 已更新 CPU pair 后发布新的无锁到期阈值。
    pub(crate) fn rearm_cpu_limit(&self, limit: LimitPair) {
        let next = ProcessCpuAccount::next_expiry(limit);
        self.cpu_account
            .next_expiry_us
            .store(next, Ordering::Release);
        let expired =
            next != u64::MAX && self.cpu_account.runtime_us.load(Ordering::Acquire) >= next;
        if expired {
            self.cpu_account
                .expiry_pending
                .store(true, Ordering::Release);
        }
    }

    /// 在返回用户态的安全点领取一次线程组 CPU 限额信号。
    ///
    /// soft limit 命中后按 Linux 语义推进一秒，因此持续超限会每秒再次产生
    /// SIGXCPU；hard limit 优先并停止后续检查。返回后已经释放 rlimit 锁。
    pub(crate) fn take_cpu_limit_signal(&self) -> Option<Signals> {
        if !self
            .cpu_account
            .expiry_pending
            .swap(false, Ordering::AcqRel)
        {
            return None;
        }

        let runtime = self.cpu_account.runtime_us.load(Ordering::Acquire);
        let mut limits = self.rlimits.lock();
        let hard_us = ProcessCpuAccount::limit_us(limits.cpu.hard);
        let hard_expired = limits.cpu.hard != usize::MAX && runtime >= hard_us;
        let signal = if hard_expired {
            Some(Signals::SIGKILL)
        } else {
            let soft_us = ProcessCpuAccount::limit_us(limits.cpu.soft);
            if limits.cpu.soft != usize::MAX && runtime >= soft_us {
                limits.cpu.soft = limits.cpu.soft.saturating_add(1);
                Some(Signals::SIGXCPU)
            } else {
                None
            }
        };
        let next = if hard_expired {
            u64::MAX
        } else {
            ProcessCpuAccount::next_expiry(limits.cpu)
        };
        self.cpu_account
            .next_expiry_us
            .store(next, Ordering::Release);
        drop(limits);

        // 覆盖“检查期间其它 CPU 又跨过下一阈值”的窗口；并发记账者若在
        // 此后发生，也会读取新阈值并自行重新发布 pending。
        if next != u64::MAX && self.cpu_account.runtime_us.load(Ordering::Acquire) >= next {
            self.cpu_account
                .expiry_pending
                .store(true, Ordering::Release);
        }
        signal
    }

    /// 返回 RLIMIT_FSIZE 的 soft limit，锁不会跨入文件系统路径。
    pub(crate) fn fsize_limit(&self) -> usize {
        self.rlimits.lock().fsize.soft
    }

    /// 返回 RLIMIT_MEMLOCK 的 soft limit，锁不会跨入 VM 修改路径。
    pub(crate) fn memlock_limit(&self) -> usize {
        self.rlimits.lock().memlock.soft
    }

    /// 返回 RLIMIT_SIGPENDING 的 soft limit，锁不会跨入线程 signal queue。
    pub(crate) fn sigpending_limit(&self) -> usize {
        self.rlimits.lock().sigpending.soft
    }

    /// 返回 RLIMIT_CORE 的 soft limit，锁不会跨入线程 dumpable 状态。
    pub(crate) fn core_limit(&self) -> usize {
        self.rlimits.lock().core.soft
    }

    /// 返回 RLIMIT_RTPRIO 的 soft limit，锁不会跨入线程调度状态。
    pub(crate) fn rtprio_limit(&self) -> usize {
        self.rlimits.lock().rtprio.soft
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
    pub(crate) fn new(
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
        rlimits: ProcessLimits,
    ) -> Self {
        let cpu_account = ProcessCpuAccount::new(rlimits.cpu);
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
            rlimits: Mutex::new(rlimits),
            cpu_account,
            posix_timers: Mutex::new(PosixTimerTable::new()),
            posix_cpu_timers_active: AtomicBool::new(false),
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
        crate::syscall::fs::close_cloexec_and_release_fcntl_locks(self.pid, &mut files.lock());

        let sighand = if sighand_shared {
            Arc::new(Mutex::new(Sighand::from_existing(&old_sighand.lock())))
        } else {
            old_sighand
        };
        sighand.lock().reset_for_exec();

        // POSIX timer 不跨 exec 保留。旧 KernelTimer 节点只持 PCB Weak，之后
        // 取得 timer 表锁时会看到空表并自行失效。
        self.clear_posix_timers();

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
        let (state, exit_rusage, child_rusage) = {
            let inner = self.inner.lock();
            (inner.state, inner.rusage, inner.child_rusage)
        };
        // zombie 的 rusage 是最后线程退出时保存的稳定快照；stop/continue
        // 事件发生在活进程上，只能读取仍在增长的 PCB CPU 账户。
        let mut rusage = if state == ProcessState::Zombie {
            exit_rusage
        } else {
            self.cpu_rusage()
        };
        // wait4/waitid 返回 RUSAGE_BOTH：不仅包含直接 child，也包含它已经
        // 回收的后代。这里先释放 child.inner，再做纯值合并。
        rusage.add_child(child_rusage);
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
        // 最后一条 live token 只会在每个 sibling 强制冲刷后归零，因此 zombie
        // 快照必须取 PCB 总量，不能再退回“最后退出线程的 TCB rusage”。
        let mut rusage = self.cpu_rusage();
        let resident_kb = self.vm().read(|vm| vm.resident_user_bytes()) / 1024;
        rusage.update_maxrss_kb(resident_kb);
        if !self.mark_zombie(exit_code, rusage) {
            return;
        }
        // PCB 会作为 zombie 保留到 wait；必须现在删除 timer，而不能等 PCB Drop。
        self.clear_posix_timers();
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
