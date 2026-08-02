//! 线程控制块与 clone/exec 资源管理。
//!
//! `TaskControlBlock` 是调度实体，保存内核栈、trap context 槽位、线程私有信号、
//! rlimit/身份/调度兼容状态等；`ProcessControlBlock` 保存线程组共享资源。
//!
//! # Locking
//!
//! `TaskControlBlock::inner` 保护线程私有可变状态。等待和信号路径在检查
//! `has_actionable_signal()` 前必须释放 `task.inner`，避免信号处理和调度唤醒路径
//! 形成锁顺序反转。

use super::perf;
use super::pid::RecycleAllocator;
use super::process::ProcessControlBlock;
use super::quota::TaskQuotaGuard;
use super::registry;
use super::signal::*;
use super::threads::{futex_wake_shared, FutexTable, SharedFutexKey};
use super::TaskContext;
use super::{
    tid_alloc, trap_cx_bottom_from_slot, ustack_bottom_from_slot, IpcNamespace, MountNamespace,
    NetNamespace, TidHandle, INIT_IPC_NAMESPACE, INIT_MOUNT_NAMESPACE,
};
use crate::config::{MMAP_BASE, PAGE_SIZE, SYSTEM_TASK_LIMIT, USER_STACK_SIZE};
use crate::fs::vfs;
use crate::fs::{vfs_lookup_absolute, vfs_root};
use crate::hal::TrapImpl;
use crate::hal::{kstack_alloc, KernelStack};
use crate::hal::{trap_handler, TrapContext};
use crate::mm::PageTableImpl;
use crate::mm::{
    AddressSpace, AddressSpaceInner, FaultAccess, PhysPageNum, VirtAddr, KERNEL_SPACE,
};
use crate::syscall::errno::{EAGAIN, EFAULT, EISDIR, ENOEXEC, ENOMEM};
use crate::syscall::{shm_clone_attachments, CloneFlags};
use crate::timer::{ITimerVal, TimeSpec, TimeVal, USEC_PER_SEC};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::{self, Debug, Formatter};
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering};
use log::warn;
use spin::{Mutex, MutexGuard};

const TASK_CAP_FULL_SET: u64 = (1u64 << 41) - 1;
const DEFAULT_TIMER_SLACK_NS: usize = 50_000;
static ACTIVE_SECCOMP_TASKS: AtomicUsize = AtomicUsize::new(0);

#[inline(always)]
pub fn any_seccomp_enabled() -> bool {
    ACTIVE_SECCOMP_TASKS.load(Ordering::Relaxed) != 0
}

#[derive(Clone, Copy, Debug)]
pub struct SeccompFilterInsn {
    pub code: u16,
    pub jt: u8,
    pub jf: u8,
    pub k: u32,
}

fn default_task_comm() -> [u8; 16] {
    let mut comm = [0u8; 16];
    comm[..8].copy_from_slice(b"initproc");
    comm
}

fn default_groups() -> Arc<Vec<u32>> {
    let mut groups = Vec::new();
    groups.push(0);
    Arc::new(groups)
}

fn cpu_limit_to_us(limit_secs: usize) -> Option<usize> {
    if limit_secs == usize::MAX {
        None
    } else {
        Some(limit_secs.saturating_mul(USEC_PER_SEC))
    }
}

#[derive(Clone)]
/// 任务的文件系统状态
pub struct FsStatus {
    /// 当前工作目录的文件（新 VFS）
    pub working_inode: Arc<vfs::File>,
    /// 当前工作目录的绝对路径字符串（用于 getcwd，避免依赖 broken 的 absolute_path()）
    pub working_path: String,
    /// chroot 的根目录 inode，如果设置了 chroot。
    /// None 表示使用全局 VFS 根目录。
    pub root_inode: Option<Arc<dyn vfs::IndexNode>>,
    /// Process file mode creation mask.
    pub umask: u32,
}

#[derive(Clone)]
pub struct UtsNamespace {
    pub nodename: [u8; 65],
    pub domainname: [u8; 65],
}

impl UtsNamespace {
    pub fn new() -> Self {
        let mut nodename = [0u8; 65];
        nodename[..8].copy_from_slice(b"blossom\0");
        Self {
            nodename,
            domainname: [0; 65],
        }
    }
}

use super::net_namespace::INIT_NET_NAMESPACE;

/// 没有待处理的协作式迁移请求。
const NO_MIGRATION_TARGET: usize = usize::MAX;

#[repr(usize)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
/// 远程 affinity 请求的完成状态。
pub(crate) enum RemoteAffinityState {
    Pending = 0,
    Applied = 1,
    Retry = 2,
}

/// 运行中任务由 owner CPU 在切栈后执行的 affinity 请求。
///
/// mask 和 target 在装入 TCB 请求槽前已完全构造；`state` 只用来让
/// 请求方区分“owner 已交接”和“遇到状态竞争，应重试”。
pub(crate) struct RemoteAffinityRequest {
    mask: usize,
    target_cpu: usize,
    state: AtomicUsize,
}

impl RemoteAffinityRequest {
    pub(crate) fn new(mask: usize, target_cpu: usize) -> Self {
        Self {
            mask,
            target_cpu,
            state: AtomicUsize::new(RemoteAffinityState::Pending as usize),
        }
    }

    pub(crate) fn mask(&self) -> usize {
        self.mask
    }

    pub(crate) fn target_cpu(&self) -> usize {
        self.target_cpu
    }

    pub(crate) fn state(&self) -> RemoteAffinityState {
        match self.state.load(Ordering::Acquire) {
            value if value == RemoteAffinityState::Pending as usize => RemoteAffinityState::Pending,
            value if value == RemoteAffinityState::Applied as usize => RemoteAffinityState::Applied,
            value if value == RemoteAffinityState::Retry as usize => RemoteAffinityState::Retry,
            value => panic!("invalid remote affinity state: {}", value),
        }
    }

    /// 以 Release 发布 owner 交接结果；一个请求只能完成一次。
    pub(crate) fn complete(&self, state: RemoteAffinityState) {
        assert_ne!(state, RemoteAffinityState::Pending);
        if let Err(previous) = self.state.compare_exchange(
            RemoteAffinityState::Pending as usize,
            state as usize,
            Ordering::Release,
            Ordering::Acquire,
        ) {
            panic!("remote affinity request completed twice: {}", previous);
        }
    }
}

/// 任务控制块
pub struct TaskControlBlock {
    /// 用户可见线程 ID，即 `gettid()` 返回值。
    ///
    /// 普通生命周期内保持不变；非 leader 线程成功 exec 后会接管进程 PID。
    /// 高频读取只访问该原子值，不为安全点引入身份锁。
    tid: AtomicUsize,
    /// 当前 TID 的分配器所有权句柄。
    ///
    /// 只有 exec 身份接管会替换它；数值真值仍是上面的 `tid`。
    tid_handle: Mutex<Arc<TidHandle>>,
    // 不可变字段
    /// 同一地址空间内 trap context / 默认用户栈的资源槽位
    pub user_res_slot: usize,
    /// Whether this task owns its user_res_slot. ktest tasks set this false
    /// to avoid double-free of shared slot 0.
    pub owns_user_res_slot: bool,
    /// 所属用户可见进程
    pub process: Arc<ProcessControlBlock>,
    /// 内核栈
    pub kstack: KernelStack,
    /// kernel-only 任务的首次执行入口；普通用户任务始终为 `None`。
    kernel_entry: Option<fn()>,
    /// 用户栈基址
    pub ustack_base: usize,
    /// Whether this task owns a kernel-managed default user stack area.
    pub user_stack_allocated: AtomicBool,
    /// Whether this task is counted in its process live-thread counter.
    pub(crate) thread_live_counted: AtomicBool,
    /// Whether this task contributes to ACTIVE_SECCOMP_TASKS.
    seccomp_counted: AtomicBool,
    uid_hint: AtomicUsize,
    euid_hint: AtomicUsize,
    suid_hint: AtomicUsize,
    gid_hint: AtomicUsize,
    egid_hint: AtomicUsize,
    sgid_hint: AtomicUsize,
    /// 进程最终退出时通知父进程的信号。
    ///
    /// 非 leader exec 接管线程组后必须恢复为 SIGCHLD。
    exit_signal: Mutex<Signals>,
    /// CLONE_THREAD 额外占用的 quota；成为唯一 leader 后立即释放。
    thread_quota: Mutex<Option<TaskQuotaGuard>>,
    // 可变字段
    /// 任务内部状态，使用互斥锁保护
    inner: Mutex<TaskControlBlockInner>,
    /// 调度状态的唯一真值。状态和 CPU owner 编码在同一个原子字中，避免
    /// `task.inner` 与运行队列分别维护状态而产生短暂漂移。
    sched_state: AtomicUsize,
    /// 最近一次真正取得该任务的 CPU。
    ///
    /// `Blocked` 不拥有 CPU，这个字段只为重新唤醒提供局部性提示，不参与
    /// runnable/current 唯一所有权判定；真实 owner 始终由 `sched_state` 给出。
    last_cpu: AtomicUsize,
    /// 允许任务取得调度所有权的逻辑 CPU 位图。
    ///
    /// 创建路径由 `New -> Queued` 状态交接首次发布。运行期 current 线程在
    /// syscall 安全点修改；稳定 Blocked 线程则只能在 interruptible registry
    /// 锁内修改。若新 mask 排除当前 CPU，写入必须先于 `migration_target`
    /// 发布，并立刻进入调度，不能只改 mask 后继续执行。
    cpus_allowed: AtomicUsize,
    /// 下一次协作式 yield 完成切栈后要进入的 CPU。
    ///
    /// 该字段只是一次性请求，绝不表示当前 owner；当前 owner 仍只由
    /// `sched_state` 决定。请求在源 CPU 的 idle 栈上被原子取走；仅登记请求
    /// 不改变 owner，任务必须随后真正进入调度切换。
    migration_target: AtomicUsize,
    /// 串行化远程 Running affinity 请求与本地 block/exit/自迁移。
    ///
    /// 该锁只保护一个短小请求槽，禁止跨 IPI、TLB 同步、context
    /// switch 或其它等待点持有。
    pub(super) remote_affinity_request: Mutex<Option<Arc<RemoteAffinityRequest>>>,
    // 可共享&可变字段
    /// I/O 等待定时器是否已挂入 KERNEL_TIMER_QUEUE。
    /// 为 true 时，wait_io_core_with_queue 不再添加第二个定时器（Option B），
    /// 防止在 log=off 的高频 loopback accept/connect 循环中 KERNEL_TIMER_QUEUE 无限增长。
    /// 定时器触发后，run_timer 会无条件清回 false（Option A）。
    pub wait_io_timer_pending: AtomicBool,
    /// Generation for timeout wake timers.  Each newly armed wake timer bumps
    /// this value so older stale timers can expire without waking the task.
    pub wait_timer_generation: AtomicUsize,
    /// Non-zero when the task is sleeping in a fallback wait (wait_event_impl
    /// with fallback_ms). Stores the generation of the current fallback timer.
    /// Zero when not in a fallback wait. Used by stale timer callbacks to
    /// re-arm instead of spurious-wake.
    pub wait_io_fallback_active_generation: AtomicUsize,
    /// 常见 nice=0 runqueue 路径使用的无锁调度提示。
    pub sched_nice_hint: AtomicI32,
    /// runqueue 选择使用的 vruntime 快照，避免持队列锁再获取 `task.inner`。
    pub sched_vruntime_hint: AtomicU64,
}

/// 任务控制块内部状态
pub struct TaskControlBlockInner {
    /// 信号掩码
    pub sigmask: Signals,
    /// sigsuspend 临时替换 mask 时保存的旧 mask，由 sigreturn 恢复。
    pub sigmask_to_restore: Option<Signals>,
    /// 待处理信号
    pub sigpending: SignalQueue,
    /// Signal set that sigwaitinfo/sigtimedwait is currently waiting for.
    pub signal_wait_mask: Signals,
    /// 备用信号栈，每线程独立
    pub signal_stack: SignalStack,
    /// 陷阱上下文的物理页号
    pub trap_cx_ppn: PhysPageNum,
    /// 任务上下文
    pub task_cx: TaskContext,
    /// POSIX 调度策略兼容字段。Per-CPU 调度器仍采用 FIFO/nice-aware 选择，
    /// 本字段只用于 syscall 语义回读，不决定真实调度类。
    pub sched_policy: usize,
    /// POSIX 调度优先级兼容字段。
    pub sched_priority: i32,
    /// SCHED_RESET_ON_FORK 兼容标记。
    pub sched_reset_on_fork: bool,
    /// sched_attr 兼容回读字段，当前不参与真实调度。
    pub sched_nice: i32,
    /// 简化 CFS 兼容用虚拟运行量。仅用于 ready 队列选择，不作为用户 ABI 暴露。
    pub sched_vruntime: u64,
    pub sched_runtime: u64,
    pub sched_deadline: u64,
    pub sched_period: u64,
    /// Linux I/O priority compatibility state.
    /// ABI-visible only; it does not affect actual I/O scheduling.
    pub ioprio_class: usize,
    pub ioprio_prio: usize,
    /// RLIMIT_RTPRIO 兼容字段，供非 root 实时调度权限检查使用。
    pub rtprio_limit_cur: usize,
    pub rtprio_limit_max: usize,
    /// RLIMIT_NICE 兼容字段，供 LTP 权限类用例回读。
    pub nice_limit_cur: usize,
    pub nice_limit_max: usize,
    /// RLIMIT_SIGPENDING 兼容字段，用于实时信号 pending 队列限额语义。
    pub sigpending_limit_cur: usize,
    pub sigpending_limit_max: usize,
    /// RLIMIT_STACK 兼容字段。当前用户栈仍按固定槽位映射，这里只保存 ABI 可见限制。
    pub stack_limit_cur: usize,
    pub stack_limit_max: usize,
    /// RLIMIT_MEMLOCK 兼容字段，供 mlock/mlockall 权限和限额类用例使用。
    pub memlock_limit_cur: usize,
    pub memlock_limit_max: usize,
    /// RLIMIT_FSIZE 兼容字段，用于限制普通文件写入长度。
    pub fsize_limit_cur: usize,
    pub fsize_limit_max: usize,
    /// RLIMIT_NPROC 兼容字段，当前仅保存 ABI 可见状态。
    pub nproc_limit_cur: usize,
    pub nproc_limit_max: usize,
    /// RLIMIT_CPU 兼容字段。单位为秒，usize::MAX 表示 unlimited。
    pub cpu_limit_cur: usize,
    pub cpu_limit_max: usize,
    pub cpu_limit_sigxcpu_sent: bool,
    /// RLIMIT_CORE 兼容字段。MangoCore 不生成 core 文件，但 wait status 需要按该值暴露 WCOREDUMP。
    pub core_limit_cur: usize,
    pub core_limit_max: usize,
    /// Linux personality ABI state. MangoCore does not alter layout/exec policy based on it yet.
    pub personality: usize,
    /// Parent-death signal configured by prctl(PR_SET_PDEATHSIG).
    pub pdeath_signal: usize,
    /// Dumpable state used by prctl(PR_GET/SET_DUMPABLE).
    pub dumpable: usize,
    /// Linux task comm, capped at 16 bytes including the trailing NUL.
    pub task_comm: [u8; 16],
    /// Timer slack compatibility state in nanoseconds.
    pub timer_slack_ns: usize,
    pub timer_slack_default_ns: usize,
    /// Minimal ptrace(TRACEME) compatibility state for signal-delivery stops.
    /// Full debugger register/memory access semantics are not implemented yet.
    pub ptrace_traceme: bool,
    /// POSIX 用户/组 ID 兼容字段，供 LTP 权限类用例和 capability 查询使用。
    pub uid: u32,
    pub euid: u32,
    pub suid: u32,
    pub fsuid: u32,
    pub gid: u32,
    pub egid: u32,
    pub sgid: u32,
    pub fsgid: u32,
    /// Process umask for file mode creation (default 0o022).
    pub umask: u32,
    /// Linux supplementary group list, used by getgroups/setgroups compatibility.
    pub groups: Arc<Vec<u32>>,
    /// Linux capability 兼容字段。当前内核只做权限语义判定，不实现真实权能隔离。
    pub cap_effective: u64,
    pub cap_permitted: u64,
    pub cap_inheritable: u64,
    pub cap_bounding: u64,
    /// prctl(NO_NEW_PRIVS/THP_DISABLE/securebits/CAP_AMBIENT) ABI-visible state.
    pub no_new_privs: bool,
    pub thp_disabled: bool,
    pub securebits: usize,
    pub cap_ambient: u64,
    /// Minimal seccomp ABI state. The stored filter is copied from user memory
    /// at PR_SET_SECCOMP time so forked children can inherit it safely.
    pub seccomp_mode: usize,
    pub seccomp_filter: Vec<SeccompFilterInsn>,
    /// 用于清理子进程的线程ID
    pub clear_child_tid: usize,
    /// 鲁棒列表，用于管理鲁棒互斥锁
    pub robust_list: RobustList,
    /// 资源使用情况
    pub rusage: Rusage,
    /// 任务的时钟信息
    pub clock: ProcClock,
    /// 定时器
    pub timer: [ITimerVal; 3],
    /// ITIMER_REAL 的真实时间到期点
    pub real_timer_deadline: Option<TimeSpec>,
    /// ITIMER_REAL 的版本号，用于让旧TimerQueue节点失效
    pub real_timer_generation: usize,
    /// POSIX timer 的最小兼容实现。当前按创建线程保存，用于 LTP timer/clock 用例。
    pub posix_timers: Vec<Option<PosixTimer>>,
    /// OOM killer pending 标志：分配器已耗尽，本进程将在 trap_return 时被杀死
    pub pending_oom_kill: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct PosixTimer {
    pub clock_id: usize,
    pub signal: Signals,
    pub interval: TimeSpec,
    pub value: TimeSpec,
    pub deadline: Option<TimeSpec>,
    /// Original absolute deadline for CLOCK_REALTIME-style POSIX timers.
    /// Relative timers and monotonic timers must not move on wall-clock jumps.
    pub realtime_abs_deadline: Option<TimeSpec>,
    pub generation: usize,
    overrun: usize,
}

impl PosixTimer {
    const OVERRUN_MAX: usize = i32::MAX as usize;

    pub fn new(clock_id: usize, signal: Signals) -> Self {
        Self {
            clock_id,
            signal,
            interval: TimeSpec::new(),
            value: TimeSpec::new(),
            deadline: None,
            realtime_abs_deadline: None,
            generation: 0,
            overrun: 0,
        }
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

#[derive(Clone, Copy, Debug)]
/// 表示任务的鲁棒列表
/// 用于管理鲁棒互斥锁
pub struct RobustList {
    /// 链表头
    pub head: usize,
    /// 链表长度
    pub len: usize,
}

impl RobustList {
    // from strace
    // 默认的链表头大小
    pub const HEAD_SIZE: usize = 24;
}

impl Default for RobustList {
    /// 初始化方法
    fn default() -> Self {
        Self {
            // 链表头
            head: 0,
            // 链表长度
            len: Self::HEAD_SIZE,
        }
    }
}

#[repr(C)]
/// 进程时钟
/// 表示任务的时钟信息
pub struct ProcClock {
    /// 上次进入用户态的时间
    last_enter_u_mode: TimeVal,
    /// 上次进入内核态的时间
    last_enter_s_mode: TimeVal,
    //  上次更新real计时器的时间
    pub last_real_timer_update: TimeVal,
}

impl ProcClock {
    /// 构造函数
    pub fn new() -> Self {
        // 获取当前时间
        let now = TimeVal::now();
        Self {
            last_enter_u_mode: now,
            last_enter_s_mode: now,
            last_real_timer_update: now,
        }
    }
}

#[allow(unused)]
#[derive(Clone, Copy)]
#[repr(C)]
/// 资源使用情况
pub struct Rusage {
    /// 用户CPU时间
    pub ru_utime: TimeVal, /* user CPU time used */
    /// 系统CPU时间
    pub ru_stime: TimeVal, /* system CPU time used */
    /// 以下字段未实现，用于后续扩展
    ru_maxrss: isize, // NOT IMPLEMENTED /* maximum resident set size */
    ru_ixrss: isize,    // NOT IMPLEMENTED /* integral shared memory size */
    ru_idrss: isize,    // NOT IMPLEMENTED /* integral unshared data size */
    ru_isrss: isize,    // NOT IMPLEMENTED /* integral unshared stack size */
    ru_minflt: isize,   // NOT IMPLEMENTED /* page reclaims (soft page faults) */
    ru_majflt: isize,   // NOT IMPLEMENTED /* page faults (hard page faults) */
    ru_nswap: isize,    // NOT IMPLEMENTED /* swaps */
    ru_inblock: isize,  // NOT IMPLEMENTED /* block input operations */
    ru_oublock: isize,  // NOT IMPLEMENTED /* block output operations */
    ru_msgsnd: isize,   // NOT IMPLEMENTED /* IPC messages sent */
    ru_msgrcv: isize,   // NOT IMPLEMENTED /* IPC messages received */
    ru_nsignals: isize, // NOT IMPLEMENTED /* signals received */
    ru_nvcsw: isize,    // NOT IMPLEMENTED /* voluntary context switches */
    ru_nivcsw: isize,   // NOT IMPLEMENTED /* involuntary context switches */
}

impl Rusage {
    /// 构造函数
    pub fn new() -> Self {
        Self {
            // 初始化为0
            ru_utime: TimeVal::new(),
            // 初始化为0
            ru_stime: TimeVal::new(),
            ru_maxrss: 0,
            ru_ixrss: 0,
            ru_idrss: 0,
            ru_isrss: 0,
            ru_minflt: 0,
            ru_majflt: 0,
            ru_nswap: 0,
            ru_inblock: 0,
            ru_oublock: 0,
            ru_msgsnd: 0,
            ru_msgrcv: 0,
            ru_nsignals: 0,
            ru_nvcsw: 0,
            ru_nivcsw: 0,
        }
    }

    pub fn add_cpu(&mut self, other: Rusage) {
        self.ru_utime = self.ru_utime + other.ru_utime;
        self.ru_stime = self.ru_stime + other.ru_stime;
    }

    pub fn add_child(&mut self, other: Rusage) {
        self.add_cpu(other);
        self.ru_maxrss = self.ru_maxrss.max(other.ru_maxrss);
    }

    pub fn update_maxrss_kb(&mut self, rss_kb: usize) {
        let rss_kb = rss_kb.min(isize::MAX as usize) as isize;
        self.ru_maxrss = self.ru_maxrss.max(rss_kb);
    }
}

const SCHED_NICE_0_LOAD: u64 = 1024;
const SCHED_NICE_TO_WEIGHT: [u64; 40] = [
    88761, 71755, 56483, 46273, 36291, 29154, 23254, 18705, 14949, 11916, 9548, 7620, 6100, 4904,
    3906, 3121, 2501, 1991, 1586, 1277, 1024, 820, 655, 526, 423, 335, 272, 215, 172, 137, 110, 87,
    70, 56, 45, 36, 29, 23, 18, 15,
];

fn sched_vruntime_delta_us(nice: i32, runtime_us: usize) -> u64 {
    if runtime_us == 0 {
        return 0;
    }
    if nice == 0 {
        return runtime_us as u64;
    }
    let nice = nice.clamp(-20, 19);
    let weight = SCHED_NICE_TO_WEIGHT[(nice + 20) as usize];
    (runtime_us as u64)
        .saturating_mul(SCHED_NICE_0_LOAD)
        .checked_div(weight)
        .unwrap_or(0)
        .max(1)
}

impl Debug for Rusage {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!(
            "(ru_utime:{:?}, ru_stime:{:?})",
            self.ru_utime, self.ru_stime
        ))
    }
}

impl TaskControlBlockInner {
    /// 在 TCB inner 独占借用期间访问陷阱上下文。
    ///
    /// trap context 物理页由当前 TCB 独占；返回生命周期必须绑定到 `&mut self`，
    /// 不能把底层直映区产生的引用伪装成可越过 inner guard 的 `'static mut`。
    pub fn trap_context_mut(&mut self) -> &mut TrapContext {
        self.trap_cx_ppn.get_mut()
    }
    /// 添加信号
    pub fn add_signal(&mut self, signal: Signals) {
        let _ = self.sigpending.enqueue_signal(signal, 0);
    }
    /// 添加带 si_code 的信号，用于硬件异常转化出的同步 fault signal。
    pub fn add_signal_with_code(&mut self, signal: Signals, si_code: u32) {
        let _ = self.sigpending.enqueue_signal(signal, si_code as usize);
    }
    /// 在进入陷阱时更新进程时间
    pub fn update_process_times_enter_trap(&mut self) {
        // 获取当前时间
        let now = TimeVal::now();
        // 更新上次进入内核态的时间
        self.clock.last_enter_s_mode = now;
        // 计算时间差
        let diff = now - self.clock.last_enter_u_mode;
        if diff.is_zero() {
            return;
        }
        // 更新用户CPU时间
        self.rusage.ru_utime = self.rusage.ru_utime + diff;
        self.sched_vruntime = self
            .sched_vruntime
            .saturating_add(sched_vruntime_delta_us(self.sched_nice, diff.to_us()));
        // 更新虚拟定时器
        self.update_itimer_virtual_if_exists(diff);
        // 更新性能分析定时器
        self.update_itimer_prof_if_exists(diff);
        self.enforce_cpu_rlimit();
    }
    /// 在离开陷阱时更新进程时间
    pub fn update_process_times_leave_trap(&mut self, _trap_cause: TrapImpl) {
        let now = TimeVal::now();
        self.account_system_time_until(now);
        self.clock.last_enter_u_mode = now;
    }

    /// 任务在内核态主动让出 CPU 前，先结算本次内核态运行时间。
    pub fn update_process_times_schedule_out(&mut self) {
        self.account_system_time_until(TimeVal::now());
    }

    /// 任务被重新调度进来后，重置内核态计时起点，避免把离 CPU 时间算入 stime。
    pub fn update_process_times_schedule_in(&mut self) {
        self.clock.last_enter_s_mode = TimeVal::now();
    }

    fn account_system_time_until(&mut self, now: TimeVal) {
        let diff = now - self.clock.last_enter_s_mode;
        if diff.is_zero() {
            return;
        }
        self.rusage.ru_stime = self.rusage.ru_stime + diff;
        self.update_itimer_prof_if_exists(diff);
        self.enforce_cpu_rlimit();
        self.clock.last_enter_s_mode = now;
    }

    fn enforce_cpu_rlimit(&mut self) {
        if self.cpu_limit_cur == usize::MAX && self.cpu_limit_max == usize::MAX {
            return;
        }
        let cpu_us = self
            .rusage
            .ru_utime
            .to_us()
            .saturating_add(self.rusage.ru_stime.to_us());
        if let Some(hard_us) = cpu_limit_to_us(self.cpu_limit_max) {
            if cpu_us >= hard_us {
                log::warn!(
                    "[sigkill_diag] cpu rlimit exceeded cpu_us={} hard_us={}",
                    cpu_us,
                    hard_us
                );
                self.add_signal(Signals::SIGKILL);
                return;
            }
        }
        if self.cpu_limit_sigxcpu_sent {
            return;
        }
        if let Some(soft_us) = cpu_limit_to_us(self.cpu_limit_cur) {
            if cpu_us >= soft_us {
                self.cpu_limit_sigxcpu_sent = true;
                self.add_signal(Signals::SIGXCPU);
            }
        }
    }
    /// 更新实时定时器
    pub fn update_itimer_real_if_exists(&mut self, diff: TimeVal) {
        // 如果当前定时器不为0
        if !self.timer[0].it_value.is_zero() {
            // 更新定时器
            self.timer[0].it_value = self.timer[0].it_value - diff;
            // 如果定时器为0
            if self.timer[0].it_value.is_zero() {
                // 添加信号
                self.add_signal(Signals::SIGALRM);
                // 重置定时器
                self.timer[0].it_value = self.timer[0].it_interval;
            }
        }
    }
    /// 更新虚拟定时器
    /// 与上面的更新实时定时器类似
    /// 但是发送的信号是SIGVTALRM
    pub fn update_itimer_virtual_if_exists(&mut self, diff: TimeVal) {
        if !self.timer[1].it_value.is_zero() {
            self.timer[1].it_value = self.timer[1].it_value - diff;
            if self.timer[1].it_value.is_zero() {
                self.add_signal(Signals::SIGVTALRM);
                self.timer[1].it_value = self.timer[1].it_interval;
            }
        }
    }
    /// 更新性能分析定时器
    /// 与上面的更新实时定时器类似
    /// 但是发送的信号是SIGPROF
    pub fn update_itimer_prof_if_exists(&mut self, diff: TimeVal) {
        if !self.timer[2].it_value.is_zero() {
            self.timer[2].it_value = self.timer[2].it_value - diff;
            if self.timer[2].it_value.is_zero() {
                self.add_signal(Signals::SIGPROF);
                self.timer[2].it_value = self.timer[2].it_interval;
            }
        }
    }

    pub fn refresh_real_timer(&mut self) {
        if self.real_timer_deadline.is_none() {
            return;
        }
        let now = TimeVal::now();
        let diff = now - self.clock.last_real_timer_update;
        self.update_itimer_real_if_exists(diff);
        // 更新锚点，防止重复计算
        self.clock.last_real_timer_update = now;
    }
}

fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

impl TaskControlBlock {
    fn write_clear_child_tid_word(
        &self,
        addr: usize,
    ) -> Result<(Option<SharedFutexKey>, Option<SharedFutexKey>), isize> {
        let bytes = 0u32.to_ne_bytes();
        let vm = self.process.vm();
        vm.write(|vm| {
            let base_va = VirtAddr::from(addr);
            let uses_shared_key = vm.futex_mapping_is_shared(base_va)?;
            let before_key = if uses_shared_key {
                // clear_child_tid 可指向尚未 fault-in 的 shared 页。此时
                // 没有旧队列可唤醒，故只在 backing 已存在时记录它。
                vm.futex_shared_backing(base_va)
                    .ok()
                    .flatten()
                    .map(|backing| SharedFutexKey::new(backing, base_va.page_offset()))
            } else {
                None
            };

            if base_va.page_offset() + bytes.len() <= PAGE_SIZE {
                let pa = vm.fault_in_user_va(base_va, FaultAccess::Store)?;
                let page_offset = pa.page_offset();
                let page = pa.floor().get_bytes_array();
                page[page_offset..page_offset + bytes.len()].copy_from_slice(&bytes);
                let after_key = if uses_shared_key {
                    let backing = vm.futex_shared_backing(base_va)?.ok_or(EFAULT)?;
                    Some(SharedFutexKey::new(backing, page_offset))
                } else {
                    None
                };
                return Ok((before_key, after_key));
            }

            for (offset, byte) in bytes.iter().enumerate() {
                let va = addr.checked_add(offset).map(VirtAddr::from).ok_or(EFAULT)?;
                let pa = vm.fault_in_user_va(va, FaultAccess::Store)?;
                pa.floor().get_bytes_array()[pa.page_offset()] = *byte;
            }
            let after_key = if uses_shared_key {
                let backing = vm.futex_shared_backing(base_va)?.ok_or(EFAULT)?;
                Some(SharedFutexKey::new(backing, base_va.page_offset()))
            } else {
                None
            };
            Ok((before_key, after_key))
        })
    }

    /// 获取任务内部状态的互斥锁
    pub fn acquire_inner_lock(&self) -> MutexGuard<TaskControlBlockInner> {
        self.inner.lock()
    }
    /// Acquire 读取任务当前的调度所有权。
    pub fn task_status(&self) -> TaskStatus {
        TaskStatus::decode(self.sched_state.load(Ordering::Acquire))
    }
    /// 返回最近一次运行 CPU，供 blocked wake 选择原 CPU。
    pub(crate) fn last_cpu(&self) -> usize {
        self.last_cpu.load(Ordering::Acquire)
    }
    /// 在任务成为本 CPU current 前记录运行位置。
    pub(crate) fn note_running_cpu(&self, cpu: usize) {
        self.last_cpu.store(cpu, Ordering::Release);
    }
    /// 返回任务当前允许运行的逻辑 CPU 位图。
    pub(crate) fn cpus_allowed(&self) -> usize {
        // Acquire 同时覆盖两种发布方式：首次发布依赖调度状态的 AcqRel
        // 交接；运行期更新则与各 owner 路径的 Release 写入配对。
        self.cpus_allowed.load(Ordering::Acquire)
    }

    /// 校验运行期 affinity，并返回当前真正可调度的 CPU 子集。
    pub(crate) fn runnable_affinity(&self, mask: usize, operation: &'static str) -> usize {
        let configured_mask = (1usize << crate::smp::configured_cpu_count()) - 1;
        assert_ne!(mask, 0, "{} cannot publish an empty mask", operation);
        assert_eq!(
            mask & !configured_mask,
            0,
            "{} mask {:#x} contains unconfigured CPUs",
            operation,
            mask
        );
        let runnable = mask
            & crate::smp::online_cpu_mask()
            & crate::smp::scheduler_cpu_mask()
            & !crate::smp::stopped_cpu_mask();
        assert_ne!(runnable, 0, "{} has no online scheduler CPU", operation);
        runnable
    }

    /// 在调用方已经取得调度所有权后发布新的 affinity。
    ///
    /// `expected` 把 mask 写入绑定到一个精确状态：Blocked 写侧持有
    /// `TASK_MANAGER`，Queued 写侧持有 owner runqueue，Migrating 写侧则是
    /// 唯一迁移调用方。Release 保证后续 owner 的 Acquire 状态交接能看到 mask。
    pub(crate) fn store_cpus_allowed(
        &self,
        mask: usize,
        expected: TaskStatus,
        operation: &'static str,
    ) {
        self.runnable_affinity(mask, operation);
        assert_eq!(
            self.task_status(),
            expected,
            "{} lost task ownership",
            operation
        );
        self.cpus_allowed.store(mask, Ordering::Release);
    }
    /// 判断目标 CPU 是否属于任务的允许集合。
    pub(crate) fn is_cpu_allowed(&self, cpu: usize) -> bool {
        cpu < crate::smp::configured_cpu_count() && self.cpus_allowed() & (1usize << cpu) != 0
    }
    /// 在任务首次发布前设置其允许 CPU 集合。
    ///
    /// 调用方必须独占 mask 写入权和首次发布权，不得与
    /// `New -> Queued/Zombie` 并发。TCB 可以已登记弱引用，但尚未进入
    /// current/runqueue；`New` 检查是误用防御，不是并发串行化手段。
    /// mask 随后由 runqueue 状态交接发布；这不是运行期 affinity 接口。
    pub(crate) fn set_initial_cpus_allowed(&self, mask: usize) {
        assert_eq!(
            self.task_status(),
            TaskStatus::New,
            "initial CPU affinity can only be set for a New task"
        );
        let configured_mask = (1usize << crate::smp::configured_cpu_count()) - 1;
        assert_ne!(mask, 0, "task CPU affinity cannot be empty");
        assert_eq!(
            mask & !configured_mask,
            0,
            "task CPU affinity {:#x} contains unconfigured CPUs",
            mask
        );
        self.cpus_allowed.store(mask, Ordering::Relaxed);
    }

    /// 修改 current 线程的运行期 affinity，并登记必要的自迁移。
    ///
    /// 返回 true 表示新 mask 已排除当前 CPU；调用者必须立即在同一个 syscall
    /// 安全点切回 idle。此接口只处理 current；远程 Blocked/Queued 分别由
    /// registry/runqueue 入口处理，不能复用 current 的一次性迁移请求。
    pub(crate) fn set_current_affinity(&self, mask: usize) -> bool {
        let source_cpu = crate::smp::cpu_id();
        let runnable = self.runnable_affinity(mask, "update current affinity");
        assert_eq!(
            self.task_status(),
            TaskStatus::Running(source_cpu),
            "runtime affinity update requires the local Running task"
        );
        let current = super::processor::current_task()
            .expect("runtime affinity update without a local current task");
        assert!(
            core::ptr::eq(Arc::as_ptr(&current), self),
            "only the current task may update runtime affinity"
        );
        let target_cpu = (mask & (1usize << source_cpu) == 0)
            .then(|| super::run_queue::select_runnable_cpu(runnable, None));

        // 目标栈映射必须先完成；在此之前既不发布新 mask，也不发布迁移目标，
        // 远端 CPU 因而不可能提前取得该任务。
        if let Some(target_cpu) = target_cpu {
            crate::smp::synchronize_kernel_mapping(target_cpu).unwrap_or_else(|error| {
                panic!(
                    "failed to synchronize task {} kernel stack to CPU {}: {:?}",
                    self.gettid(),
                    target_cpu,
                    error
                )
            });
        }
        // 远程写侧与 current 自写侧可以并发。后取得请求槽锁的本地
        // syscall 线性化在后：它撤销旧远程请求，再发布自己的 mask/target。
        let mut request_slot = self.remote_affinity_request.lock();
        assert_eq!(
            self.task_status(),
            TaskStatus::Running(source_cpu),
            "current affinity owner changed before publication"
        );
        assert_eq!(
            self.migration_target.load(Ordering::Acquire),
            NO_MIGRATION_TARGET,
            "runtime affinity update raced with a pending migration"
        );
        let superseded = request_slot.take();
        // Release mask 在 Release target 之前；源 idle 以 Acquire 取走 target
        // 后，require_cpu_allowed() 必然能看到与该目标配套的新 mask。
        self.cpus_allowed.store(mask, Ordering::Release);
        if let Some(target_cpu) = target_cpu {
            self.publish_migration_target(target_cpu);
        }
        drop(request_slot);
        if let Some(request) = superseded {
            request.complete(RemoteAffinityState::Retry);
        }
        target_cpu.is_some()
    }

    /// 在 interruptible registry 已经稳定拥有 Blocked 任务时更新其 affinity。
    ///
    /// 调用方必须持有 `TASK_MANAGER`，并已确认同一个 TCB 仍在 registry 中；
    /// 这样后续 wake 才不可能越过本次写入、按旧 mask 发布到 runqueue。
    pub(crate) fn set_blocked_affinity(&self, mask: usize) {
        self.store_cpus_allowed(mask, TaskStatus::Blocked, "update blocked affinity");
    }

    fn publish_migration_target(&self, target_cpu: usize) {
        if let Err(previous) = self.migration_target.compare_exchange(
            NO_MIGRATION_TARGET,
            target_cpu,
            Ordering::Release,
            Ordering::Acquire,
        ) {
            panic!(
                "task {} already has migration target CPU {}",
                self.gettid(),
                previous
            );
        }
    }
    /// 请求槽锁保护下检查是否已有本地协作式迁移。
    pub(crate) fn has_migration_target(&self) -> bool {
        self.migration_target.load(Ordering::Acquire) != NO_MIGRATION_TARGET
    }
    /// 在调度所有权改变前验证目标 CPU。
    pub(crate) fn require_cpu_allowed(&self, cpu: usize, operation: &'static str) {
        assert!(
            self.is_cpu_allowed(cpu),
            "{} selected disallowed CPU: tid={} cpu={} allowed={:#x}",
            operation,
            self.gettid(),
            cpu,
            self.cpus_allowed()
        );
    }
    /// 请求任务在下一次协作式 yield 后迁移到目标 CPU。
    ///
    /// 为避免远程请求与阻塞/退出状态竞争，本阶段只允许尚未发布的 `New`
    /// 任务，或正在调用本函数的 current 任务设置请求。目标内核栈映射先完成
    /// TLB 同步，再用 Release 发布请求，idle 收尾侧不会看到未同步的目标。
    pub(crate) fn request_migration(&self, target_cpu: usize) {
        assert!(target_cpu < crate::smp::configured_cpu_count());
        // 请求只表达未来目标，不改变 owner；在同步目标内核栈之前先拒绝
        // 越过 `cpus_allowed` 的请求，避免留下无意义的远端 TLB 工作。
        self.require_cpu_allowed(target_cpu, "request task migration");
        let target_bit = 1usize << target_cpu;
        assert_ne!(
            crate::smp::online_cpu_mask() & target_bit,
            0,
            "cannot migrate task to offline CPU {}",
            target_cpu
        );
        assert_ne!(
            crate::smp::scheduler_cpu_mask() & target_bit,
            0,
            "cannot migrate task before CPU {} enters scheduler",
            target_cpu
        );

        match self.task_status() {
            TaskStatus::New => {}
            TaskStatus::Running(source_cpu) if source_cpu == crate::smp::cpu_id() => {
                let current = super::processor::current_task()
                    .expect("running task requested migration without local current");
                assert!(
                    core::ptr::eq(Arc::as_ptr(&current), self),
                    "only the current task may request its own migration"
                );
                assert_ne!(source_cpu, target_cpu, "migration target equals source CPU");
            }
            status => panic!(
                "migration request requires New or local Running task: tid={}, status={:?}",
                self.gettid(),
                status
            ),
        }

        crate::smp::synchronize_kernel_mapping(target_cpu).unwrap_or_else(|error| {
            panic!(
                "failed to synchronize task {} kernel stack to CPU {}: {:?}",
                self.gettid(),
                target_cpu,
                error
            )
        });
        // 迁移目标与远程 affinity 请求共享同一个串行化域，不能让
        // idle 收尾同时看到两个不同的目标。本地 current 写侧后获锁，因而可线性化在后。
        let mut request_slot = self.remote_affinity_request.lock();
        match self.task_status() {
            TaskStatus::New => {}
            TaskStatus::Running(source_cpu) if source_cpu == crate::smp::cpu_id() => {}
            status => panic!(
                "migration owner changed before publication: tid={}, status={:?}",
                self.gettid(),
                status
            ),
        }
        let superseded = request_slot.take();
        self.publish_migration_target(target_cpu);
        drop(request_slot);
        if let Some(request) = superseded {
            request.complete(RemoteAffinityState::Retry);
        }
    }
    /// 取走一次性迁移请求；只能在任务已经切回源 CPU idle 栈后调用。
    pub(crate) fn take_migration_target(&self) -> Option<usize> {
        let target = self
            .migration_target
            .swap(NO_MIGRATION_TARGET, Ordering::Acquire);
        (target != NO_MIGRATION_TARGET).then_some(target)
    }
    /// 尝试完成一次精确的调度状态迁移。
    ///
    /// AcqRel 同时发布旧 owner 的写入，并让新 owner 观察到此前发布的数据。
    #[must_use = "调度 CAS 的失败状态必须显式处理"]
    pub(crate) fn try_sched_transition(
        &self,
        current: TaskStatus,
        next: TaskStatus,
    ) -> Result<(), TaskStatus> {
        self.sched_state
            .compare_exchange(
                current.encode(),
                next.encode(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(TaskStatus::decode)
    }
    /// 执行不允许失败的状态迁移。
    ///
    /// 这里处理的是调度所有权交接，而不是普通业务竞争。失败后继续运行可能让
    /// 同一个 TCB 同时出现在 current slot 和 runqueue 中，因此所有构建都立即
    /// 停止；允许失败的重复唤醒必须直接使用 [`Self::try_sched_transition`]。
    pub(crate) fn require_sched_transition(
        &self,
        current: TaskStatus,
        next: TaskStatus,
        operation: &'static str,
    ) {
        if let Err(actual) = self.try_sched_transition(current, next) {
            self.fail_sched_invariant(operation, current, actual, next);
        }
    }
    /// 终止无法安全恢复的所有权错误。
    ///
    /// 这类失败与“另一唤醒方已经赢得 CAS”不同：继续运行会让任务同时属于
    /// 两个 owner，或从所有队列中消失，因此 release 构建也不能静默降级。
    pub(crate) fn fail_sched_invariant(
        &self,
        operation: &'static str,
        expected: TaskStatus,
        actual: TaskStatus,
        next: TaskStatus,
    ) -> ! {
        panic!(
            "scheduler ownership invariant failed in {}: tid={}, expected={:?}, actual={:?}, next={:?}",
            operation,
            self.gettid(),
            expected,
            actual,
            next
        );
    }
    /// 原子状态已经进入不可逆终态时返回 true。
    pub fn is_zombie(&self) -> bool {
        self.task_status() == TaskStatus::Zombie
    }
    /// 把未排队的任务推进到终态。
    ///
    /// `Queued` 必须先由运行队列移除并变成 `Blocked`；直接从 Queued 结束会让
    /// 队列中残留一个终态 TCB，因此这里将其视为所有权错误。
    pub(crate) fn mark_zombie(&self, operation: &'static str) -> bool {
        // 终态与远程 Running affinity 必须在同一请求槽上串行化：
        // 一旦 Zombie 先发布，远程写侧只能收到 Retry，不能再安装迁移请求。
        let mut request_slot = self.remote_affinity_request.lock();
        let current = self.task_status();
        let changed = match current {
            TaskStatus::New | TaskStatus::Blocked | TaskStatus::Running(_) => {
                self.require_sched_transition(current, TaskStatus::Zombie, operation);
                true
            }
            TaskStatus::Zombie => false,
            TaskStatus::Queued(_) | TaskStatus::Migrating | TaskStatus::Blocking(_) => self
                .fail_sched_invariant(operation, TaskStatus::Blocked, current, TaskStatus::Zombie),
        };
        let canceled = request_slot.take();
        drop(request_slot);
        if let Some(request) = canceled {
            request.complete(RemoteAffinityState::Retry);
        }
        changed
    }
    pub fn account_seccomp_enabled(&self) {
        if !self.seccomp_counted.swap(true, Ordering::Relaxed) {
            ACTIVE_SECCOMP_TASKS.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn unaccount_seccomp_enabled(&self) {
        if self.seccomp_counted.swap(false, Ordering::Relaxed) {
            ACTIVE_SECCOMP_TASKS.fetch_sub(1, Ordering::Relaxed);
        }
    }
    /// 获取陷阱上下文的用户虚拟地址
    pub fn trap_cx_user_va(&self) -> usize {
        trap_cx_bottom_from_slot(self.user_res_slot)
    }
    /// 获取用户栈的用户虚拟地址
    pub fn ustack_bottom_va(&self) -> usize {
        ustack_bottom_from_slot(self.user_res_slot)
    }

    /// 释放线程级资源，并把当前线程标记为 zombie。
    ///
    /// 这里不处理父子进程、进程 zombie、fd/vm 整体回收等进程级生命周期，
    /// 那些属于 ProcessControlBlock 的退出收尾。返回 true 仅表示本线程在
    /// 完成全部清理后消费了最后一个 live token，应由调用者执行进程级收尾。
    pub(crate) fn exit_thread_resources(&self, exit_code: u32) -> bool {
        let clear_child_tid = {
            let mut inner = self.acquire_inner_lock();
            if self.is_zombie() {
                return false;
            }
            inner.update_process_times_schedule_out();
            if !self.mark_zombie("exit") {
                return false;
            }
            let clear_child_tid = inner.clear_child_tid;
            inner.clear_child_tid = 0;
            inner.robust_list = RobustList::default();
            clear_child_tid
        };

        if clear_child_tid != 0 {
            match self.write_clear_child_tid_word(clear_child_tid) {
                Ok((before_key, after_key)) => {
                    self.process.futex().lock().wake(clear_child_tid, 1);
                    if let Some(key) = before_key.clone() {
                        futex_wake_shared(key, 1);
                    }
                    // fault-in 期间映射可能换了 backing；仅在身份不同时
                    // 再唤醒新队列，避免对同一 word 重复扫描全局表。
                    if after_key != before_key {
                        if let Some(key) = after_key {
                            futex_wake_shared(key, 1);
                        }
                    }
                }
                Err(errno) => {
                    log::warn!("invalid clear_child_tid: {}", errno);
                }
            };
        }

        let keep_trap_context = self.process.try_cache_trap_context_slot(self.user_res_slot);
        super::perf::record_exit_thread(clear_child_tid != 0, keep_trap_context);
        if self.owns_user_res_slot {
            let vm = self.process.vm();
            vm.write(|vm| {
                if keep_trap_context {
                    vm.dealloc_user_res_keep_trap(
                        self.user_res_slot,
                        self.user_stack_allocated.load(Ordering::Relaxed),
                    );
                } else {
                    vm.dealloc_user_res_with_stack(
                        self.user_res_slot,
                        self.user_stack_allocated.load(Ordering::Relaxed),
                    );
                }
            });
        }

        // live token 就是 sibling stop ack，必须最后发布。若提前递减，另一 CPU
        // 可能在本线程仍访问 clear_child_tid/VM 时释放共享地址空间。
        matches!(self.process.remove_thread(self), Some(0))
    }

    /// 创建 initproc 的首个任务。
    ///
    /// # Semantics
    ///
    /// 该构造器只用于内核启动阶段加载 `/init`。普通 fork/clone 必须走
    /// `sys_clone()`，exec 必须走 `load_elf()`。
    pub fn new(elf: Arc<vfs::File>) -> Arc<Self> {
        macro_rules! init_task_trace {
            ($($arg:tt)*) => {
                #[cfg(all(feature = "board_2k1000", feature = "board_bringup_trace"))]
                println!("[bringup][tcb] {}", format_args!($($arg)*));
            };
        }

        // 将ELF文件映射到内核空间
        init_task_trace!("01 map init ELF into kernel space");
        let elf_data = elf.map_to_kernel_space(MMAP_BASE);
        if elf_data.is_empty() {
            panic!("[TCB::new] initproc ELF is empty");
        }
        init_task_trace!("02 init ELF mapped: {} bytes", elf_data.len());
        // 带有ELF程序头/跳板的用户地址空间（AddressSpaceInner）
        // 解析ELF文件，初始化内存映射
        let (mut memory_set, _user_heap, elf_info) =
            AddressSpaceInner::<PageTableImpl>::from_elf(elf_data)
                .expect("initproc ELF is invalid");
        init_task_trace!("03 ELF parsed: user entry={:#x}", elf_info.entry);
        // 在内核空间中删除ELF区域
        crate::mm::remove_kernel_mapping_synchronized(VirtAddr::from(MMAP_BASE).floor()).unwrap();
        init_task_trace!("04 temporary kernel ELF mapping removed");

        // 获取用户资源槽位分配器
        let user_res_slot_allocator = Arc::new(Mutex::new(RecycleAllocator::new()));
        // 在内核空间中分配一个用户可见 tid 和一个内核栈
        let tid_handle = tid_alloc();
        // 分配当前地址空间内的用户资源槽位
        let user_res_slot = user_res_slot_allocator.lock().alloc();
        // 初始进程的 pid/pgid 与主线程 tid 相同
        let pid = tid_handle.0;
        let pgid = tid_handle.0;
        // 分配内核栈
        let kstack = kstack_alloc();
        // 获取内核栈的顶部
        let kstack_top = kstack.get_top();
        init_task_trace!(
            "05 ids and kernel stack allocated: pid={} slot={} kstack_top={:#x}",
            pid,
            user_res_slot,
            kstack_top
        );

        // 为当前线程分配用户资源，并保留 trap context PPN，避免再次页表遍历。
        let trap_cx_ppn = memory_set
            .alloc_user_res_with_trap_ppn(user_res_slot, true)
            .expect("init task user resource allocation failed");
        init_task_trace!("06 user stack and trap context pages allocated");

        // 构造初始进程的 argc/argv/envp 栈
        let init_sp = {
            let argv_vec = alloc::vec![alloc::string::String::from("/init")];
            let envp_vec = alloc::vec![
                alloc::string::String::from("PATH=/:/bin:/sbin:/usr/bin:/tools/bin"),
                alloc::string::String::from("PWD=/"),
                alloc::string::String::from("HOME=/root"),
            ];
            memory_set
                .create_elf_tables(
                    ustack_bottom_from_slot(user_res_slot),
                    &argv_vec,
                    &envp_vec,
                    &elf_info,
                )
                .expect("init task stack setup failed")
        };
        init_task_trace!("07 argc/argv/envp stack created: user_sp={:#x}", init_sp);
        // 初始化新 VFS 文件描述符表
        let mut fd_table = vfs::FdTable::new();
        // The kernel bootstrap mounts devfs before creating PID1, then opens
        // /dev/tty for stdin/stdout/stderr (fd 0/1/2). Ktest's independent
        // constructor deliberately skips this userspace-only bootstrap.
        let tty_inode = vfs_lookup_absolute("/dev/tty").unwrap();
        let tty_file = vfs::File::new(tty_inode, vfs::FileFlags::O_RDWR).unwrap();
        fd_table.alloc_fd(tty_file, false).unwrap();
        let tty_inode = vfs_lookup_absolute("/dev/tty").unwrap();
        let tty_file = vfs::File::new(tty_inode, vfs::FileFlags::O_RDWR).unwrap();
        fd_table.alloc_fd(tty_file, false).unwrap();
        let tty_inode = vfs_lookup_absolute("/dev/tty").unwrap();
        let tty_file = vfs::File::new(tty_inode, vfs::FileFlags::O_RDWR).unwrap();
        fd_table.alloc_fd(tty_file, false).unwrap();
        init_task_trace!("08 /dev/tty attached to fd 0, 1 and 2");

        // 初始化工作目录为根目录
        let root_inode = vfs_root().mountpoint_root_inode();
        let cwd = vfs::File::new(
            root_inode,
            vfs::FileFlags::O_RDONLY | vfs::FileFlags::O_DIRECTORY,
        )
        .unwrap();

        let process_quota = TaskQuotaGuard::acquire_for_init();

        let process = Arc::new(ProcessControlBlock::new(
            pid,
            tid_handle.clone(),
            process_quota,
            pgid,
            pgid,
            None,
            Arc::new(Mutex::new(elf)),
            String::new(),
            Arc::new(Mutex::new(fd_table)),
            Arc::new(Mutex::new(FsStatus {
                working_inode: cwd,
                working_path: String::from("/"),
                root_inode: None,
                umask: 0,
            })),
            Arc::new(Mutex::new(UtsNamespace::new())),
            INIT_NET_NAMESPACE.clone(),
            INIT_MOUNT_NAMESPACE.clone(),
            INIT_IPC_NAMESPACE.clone(),
            Arc::new(AddressSpace::new(memory_set)),
            Arc::new(Mutex::new(Sighand::new())),
            Arc::new(Mutex::new(FutexTable::new())),
            user_res_slot_allocator,
        ));
        init_task_trace!("09 process control block created");

        // 创建任务控制块
        let task_control_block = Arc::new(Self {
            tid: AtomicUsize::new(tid_handle.0),
            tid_handle: Mutex::new(tid_handle),
            user_res_slot,
            owns_user_res_slot: true,
            process,
            kstack,
            kernel_entry: None,
            ustack_base: ustack_bottom_from_slot(user_res_slot),
            user_stack_allocated: AtomicBool::new(true),
            thread_live_counted: AtomicBool::new(false),
            seccomp_counted: AtomicBool::new(false),
            uid_hint: AtomicUsize::new(0),
            euid_hint: AtomicUsize::new(0),
            suid_hint: AtomicUsize::new(0),
            gid_hint: AtomicUsize::new(0),
            egid_hint: AtomicUsize::new(0),
            sgid_hint: AtomicUsize::new(0),
            exit_signal: Mutex::new(Signals::empty()),
            thread_quota: Mutex::new(None),
            wait_io_timer_pending: AtomicBool::new(false),
            wait_timer_generation: AtomicUsize::new(0),
            wait_io_fallback_active_generation: AtomicUsize::new(0),
            sched_nice_hint: AtomicI32::new(0),
            sched_vruntime_hint: AtomicU64::new(0),
            sched_state: AtomicUsize::new(TaskStatus::New.encode()),
            last_cpu: AtomicUsize::new(usize::MAX),
            cpus_allowed: AtomicUsize::new(1usize << crate::smp::BOOT_CPU_ID),
            migration_target: AtomicUsize::new(NO_MIGRATION_TARGET),
            remote_affinity_request: Mutex::new(None),
            inner: Mutex::new(TaskControlBlockInner {
                sigmask: Signals::empty(),
                sigmask_to_restore: None,
                sigpending: SignalQueue::empty(),
                signal_wait_mask: Signals::empty(),
                signal_stack: SignalStack::disabled(),
                trap_cx_ppn,
                task_cx: TaskContext::goto_trap_return(kstack_top),
                sched_policy: 0,
                sched_priority: 0,
                sched_reset_on_fork: false,
                sched_nice: 0,
                sched_vruntime: 0,
                sched_runtime: 0,
                sched_deadline: 0,
                sched_period: 0,
                ioprio_class: 2,
                ioprio_prio: 4,
                rtprio_limit_cur: 0,
                rtprio_limit_max: 0,
                nice_limit_cur: usize::MAX,
                nice_limit_max: usize::MAX,
                sigpending_limit_cur: usize::MAX,
                sigpending_limit_max: usize::MAX,
                stack_limit_cur: USER_STACK_SIZE,
                stack_limit_max: USER_STACK_SIZE,
                memlock_limit_cur: usize::MAX,
                memlock_limit_max: usize::MAX,
                fsize_limit_cur: usize::MAX,
                fsize_limit_max: usize::MAX,
                nproc_limit_cur: SYSTEM_TASK_LIMIT,
                nproc_limit_max: SYSTEM_TASK_LIMIT,
                cpu_limit_cur: usize::MAX,
                cpu_limit_max: usize::MAX,
                cpu_limit_sigxcpu_sent: false,
                core_limit_cur: 0,
                core_limit_max: usize::MAX,
                personality: 0,
                pdeath_signal: 0,
                dumpable: 1,
                task_comm: default_task_comm(),
                timer_slack_ns: DEFAULT_TIMER_SLACK_NS,
                timer_slack_default_ns: DEFAULT_TIMER_SLACK_NS,
                ptrace_traceme: false,
                uid: 0,
                euid: 0,
                suid: 0,
                fsuid: 0,
                gid: 0,
                egid: 0,
                sgid: 0,
                fsgid: 0,
                umask: 0o022,
                groups: default_groups(),
                cap_effective: TASK_CAP_FULL_SET,
                cap_permitted: TASK_CAP_FULL_SET,
                cap_inheritable: 0,
                cap_bounding: TASK_CAP_FULL_SET,
                no_new_privs: false,
                thp_disabled: false,
                securebits: 0,
                cap_ambient: 0,
                seccomp_mode: 0,
                seccomp_filter: Vec::new(),
                clear_child_tid: 0,
                robust_list: RobustList::default(),
                rusage: Rusage::new(),
                clock: ProcClock::new(),
                timer: [ITimerVal::new(); 3],
                real_timer_deadline: None,
                real_timer_generation: 0,
                posix_timers: Vec::new(),
                pending_oom_kill: false,
            }),
        });
        registry::register_process(&task_control_block.process);
        registry::register_task(&task_control_block);
        init_task_trace!("10 task registered in process and task registries");
        // 准备用户空间的陷阱上下文
        let kernel_token = KERNEL_SPACE.lock().token();
        {
            let mut inner = task_control_block.acquire_inner_lock();
            let trap_cx = inner.trap_context_mut();
            // 初始化陷阱上下文
            *trap_cx = TrapContext::app_init_context(
                elf_info.entry,
                init_sp,
                kernel_token,
                kstack_top,
                trap_handler as usize,
            );
            init_task_trace!(
                "11 initial trap context ready: pc={:#x} sp={:#x}",
                trap_cx.gp.pc,
                trap_cx.gp.sp
            );
        }
        task_control_block
    }

    /// 为独立 ktest 进程创建最小化 TCB。
    ///
    /// # Semantics
    ///
    /// 该构造器不会解析 ELF、分配用户内存或设置 fd table。
    /// 它只建立内核执行上下文，并立即登记 TID 弱引用；调用方仍负责在加入
    /// 线程组和 runqueue 前设置精确的 `cpus_allowed`。普通入口是
    /// `spawn_ktest_task_on()`；SMP 生命周期用例可暂不发布 sibling，
    /// 以验证 group-exit 的最终发布门禁。
    pub(crate) fn new_ktest_independent(
        tid_handle: Arc<TidHandle>,
        process: Arc<ProcessControlBlock>,
        kstack: KernelStack,
        task_cx: TaskContext,
        kernel_entry: fn(),
    ) -> Arc<Self> {
        let task = Arc::new(Self {
            tid: AtomicUsize::new(tid_handle.0),
            tid_handle: Mutex::new(tid_handle),
            user_res_slot: 0,
            owns_user_res_slot: false,
            process,
            kstack,
            kernel_entry: Some(kernel_entry),
            ustack_base: 0,
            user_stack_allocated: AtomicBool::new(false),
            thread_live_counted: AtomicBool::new(false),
            seccomp_counted: AtomicBool::new(false),
            uid_hint: AtomicUsize::new(0),
            euid_hint: AtomicUsize::new(0),
            suid_hint: AtomicUsize::new(0),
            gid_hint: AtomicUsize::new(0),
            egid_hint: AtomicUsize::new(0),
            sgid_hint: AtomicUsize::new(0),
            exit_signal: Mutex::new(Signals::empty()),
            thread_quota: Mutex::new(None),
            wait_io_timer_pending: AtomicBool::new(false),
            wait_timer_generation: AtomicUsize::new(0),
            wait_io_fallback_active_generation: AtomicUsize::new(0),
            sched_nice_hint: AtomicI32::new(0),
            sched_vruntime_hint: AtomicU64::new(0),
            sched_state: AtomicUsize::new(TaskStatus::New.encode()),
            last_cpu: AtomicUsize::new(usize::MAX),
            cpus_allowed: AtomicUsize::new(1usize << crate::smp::BOOT_CPU_ID),
            migration_target: AtomicUsize::new(NO_MIGRATION_TARGET),
            remote_affinity_request: Mutex::new(None),
            inner: Mutex::new(TaskControlBlockInner {
                sigmask: Signals::empty(),
                sigmask_to_restore: None,
                sigpending: SignalQueue::empty(),
                signal_wait_mask: Signals::empty(),
                signal_stack: SignalStack::disabled(),
                trap_cx_ppn: PhysPageNum(0),
                task_cx,
                sched_policy: 0,
                sched_priority: 0,
                sched_reset_on_fork: false,
                sched_nice: 0,
                sched_vruntime: 0,
                sched_runtime: 0,
                sched_deadline: 0,
                sched_period: 0,
                ioprio_class: 2,
                ioprio_prio: 4,
                rtprio_limit_cur: 0,
                rtprio_limit_max: 0,
                nice_limit_cur: usize::MAX,
                nice_limit_max: usize::MAX,
                sigpending_limit_cur: usize::MAX,
                sigpending_limit_max: usize::MAX,
                stack_limit_cur: USER_STACK_SIZE,
                stack_limit_max: USER_STACK_SIZE,
                memlock_limit_cur: usize::MAX,
                memlock_limit_max: usize::MAX,
                fsize_limit_cur: usize::MAX,
                fsize_limit_max: usize::MAX,
                nproc_limit_cur: SYSTEM_TASK_LIMIT,
                nproc_limit_max: SYSTEM_TASK_LIMIT,
                cpu_limit_cur: usize::MAX,
                cpu_limit_max: usize::MAX,
                cpu_limit_sigxcpu_sent: false,
                core_limit_cur: 0,
                core_limit_max: usize::MAX,
                personality: 0,
                pdeath_signal: 0,
                dumpable: 1,
                task_comm: default_task_comm(),
                timer_slack_ns: DEFAULT_TIMER_SLACK_NS,
                timer_slack_default_ns: DEFAULT_TIMER_SLACK_NS,
                ptrace_traceme: false,
                uid: 0,
                euid: 0,
                suid: 0,
                fsuid: 0,
                gid: 0,
                egid: 0,
                sgid: 0,
                fsgid: 0,
                umask: 0o022,
                groups: default_groups(),
                cap_effective: TASK_CAP_FULL_SET,
                cap_permitted: TASK_CAP_FULL_SET,
                cap_inheritable: 0,
                cap_bounding: TASK_CAP_FULL_SET,
                no_new_privs: false,
                thp_disabled: false,
                securebits: 0,
                cap_ambient: 0,
                seccomp_mode: 0,
                seccomp_filter: Vec::new(),
                clear_child_tid: 0,
                robust_list: RobustList::default(),
                rusage: Rusage::new(),
                clock: ProcClock::new(),
                timer: [ITimerVal::new(); 3],
                real_timer_deadline: None,
                real_timer_generation: 0,
                posix_timers: Vec::new(),
                pending_oom_kill: false,
            }),
        });
        // 与生产 clone 路径保持同一生命周期不变量：TCB 一旦构造完成，
        // 按 TID 查询就必须可见；调度发布失败只影响线程组/runqueue 归属。
        registry::register_task(&task);
        task
    }

    /// 返回 kernel-only 任务自己的不可变入口。
    pub(crate) fn kernel_entry(&self) -> Option<fn()> {
        self.kernel_entry
    }

    /// 停止同线程组 sibling，并一次性安装已经完整构造的新执行映像。
    ///
    /// 所有可能失败的 ELF 解析和用户栈构造都在调用本函数前完成；从
    /// `begin_exec()` 起关闭 clone 门，直到旧 MM 已无人执行、共享进程状态全部
    /// 替换完成才重新开放。远端线程始终在自己的 CPU 安全点释放资源。
    fn install_exec_image(
        &self,
        elf: Arc<vfs::File>,
        old_vm: Arc<AddressSpace<PageTableImpl>>,
        old_stack_allocated: bool,
        new_vm: AddressSpaceInner<PageTableImpl>,
        new_trap_ppn: PhysPageNum,
        new_trap_cx: TrapContext,
    ) -> Result<(), isize> {
        let (exec, siblings) = self.process.begin_exec(self.gettid())?;
        super::request_sibling_exit(&siblings, self.gettid());
        // noreturn sibling 调度不会析构 owner 的 syscall 栈；等待前主动释放快照 Arc。
        drop(siblings);
        exec.wait();

        // 永久 group exit 可以覆盖临时 exec。此时旧映像仍未提交，先重开临时门；
        // 永久退出门仍保持关闭，上层随后走统一的当前线程退出路径。
        if self.process.is_group_exiting() {
            exec.finish();
            return Err(EAGAIN);
        }

        // Linux 在 de-thread 之后先解除跨 PCB 资源共享，再修改 CLOEXEC、sighand
        // 和 private futex。此处已经越过可回滚点；OOM 会结束当前进程，不能让
        // 已被停止的 sibling 恢复到旧映像。
        if let Err(error) = self.process.reset_exec_resources() {
            exec.finish();
            return Err(-(error as isize));
        }

        {
            let mut inner = self.acquire_inner_lock();
            inner.trap_cx_ppn = new_trap_ppn;
            *inner.trap_context_mut() = new_trap_cx;
            inner.clear_child_tid = 0;
            inner.robust_list = RobustList::default();
            inner.signal_stack = SignalStack::disabled();
        }
        self.user_stack_allocated.store(true, Ordering::Relaxed);

        if Arc::strong_count(&old_vm) > 2 {
            // 其它 PCB 仍共享旧 VM（如 CLONE_VM/vfork）时，只撤销本线程旧槽；
            // 同一 PCB 的 sibling 已在 completion 前分别撤销自己的用户资源。
            old_vm.write(|vm| {
                vm.dealloc_user_res_with_stack(self.user_res_slot, old_stack_allocated)
            });
        }
        self.process.replace_exe(elf);
        self.process.replace_vm(new_vm);
        exec.finish();
        self.become_group_leader();
        Ok(())
    }

    /// 非 leader exec 成功后接管进程 PID/TGID。
    ///
    /// ExecSession 已经结束，当前线程是唯一 live 成员；这里不再修改线程组状态，
    /// 只事务性更新 TCB/registry，再刷新两个由 TID 派生的本地索引。
    pub(crate) fn become_group_leader(&self) {
        let old_tid = self.gettid();
        let leader_tid = self.pid();
        // Linux exec 总是把新线程组 leader 的父进程退出通知恢复为 SIGCHLD。
        *self.exit_signal.lock() = Signals::SIGCHLD;
        if old_tid == leader_tid {
            return;
        }
        registry::exchange_exec_tids(self);
        super::processor::update_current_tid(self, old_tid);
        super::manager::rekey_active_tid(old_tid, leader_tid);
        // 当前 TCB 不再是“额外线程”；进程本身的 quota 仍由 PCB 持有。
        let thread_quota = self.thread_quota.lock().take();
        drop(thread_quota);
    }

    /// 返回进程最终退出时由本线程发送给父进程的信号。
    pub(crate) fn exit_signal(&self) -> Signals {
        *self.exit_signal.lock()
    }

    /// 加载ELF文件
    pub fn load_elf(
        &self,
        elf: Arc<vfs::File>,
        argv_vec: &Vec<String>,
        envp_vec: &Vec<String>,
    ) -> Result<(), isize> {
        if elf.is_dir() {
            return Err(EISDIR);
        }
        // 旧 VM 必须完整保留到 exec gate 关闭且 sibling 全部确认退出。
        // 同一 PCB 的线程不会各自长期持有 VM Arc，因此 Arc 计数不能证明没有
        // 并发线程；在 gate 建立前提前 recycle 会与 late CLONE_THREAD 竞争。
        let current_vm = self.process.vm();
        let old_stack_allocated = self.user_stack_allocated.load(Ordering::Relaxed);

        // 将ELF文件映射到内核空间
        let _t_kmap = perf::perf_time_now();
        let elf_data = elf.map_to_kernel_space(MMAP_BASE);
        if elf_data.is_empty() {
            log::error!("[load_elf] ELF file is empty (size=0)");
            return Err(ENOEXEC);
        }
        let _kmap_ticks = perf::perf_time_now().wrapping_sub(_t_kmap);
        perf::EXECVE_KERNEL_MAP_TICKS.fetch_add(_kmap_ticks, Ordering::Relaxed);
        // 带有ELF程序头/跳板/陷阱上下文/用户栈的用户地址空间（AddressSpaceInner）
        let _t_map = perf::perf_time_now();
        let load_result = AddressSpaceInner::from_elf(elf_data);
        let _map_ticks = perf::perf_time_now().wrapping_sub(_t_map);
        perf::EXECVE_MAP_ELF_TICKS.fetch_add(_map_ticks, Ordering::Relaxed);

        // 清除临时映射
        let _t_teardown = perf::perf_time_now();
        // ELF 内容只在解析期间映射进共享内核页表；清 PTE 后必须等远端
        // shootdown ack，再让文件映射 frame 回到分配器。
        crate::mm::remove_kernel_mapping_synchronized(VirtAddr::from(MMAP_BASE).floor()).unwrap();
        let _td_ticks = perf::perf_time_now().wrapping_sub(_t_teardown);
        perf::EXECVE_TEARDOWN_TICKS.fetch_add(_td_ticks, Ordering::Relaxed);

        let (mut memory_set, program_break, elf_info) = match load_result {
            Ok(result) => result,
            Err(e) => return Err(e),
        };
        // 为 glibc 分配用户 heap 空间（0x1c0000 ~ 0x1c4000）
        use crate::mm::{MapPermission, VirtAddr};

        let page_size = 0x1000;
        let heap_start = align_up(program_break, page_size);
        let heap_end = heap_start + 0x20000; // 64KiB
        memory_set.insert_framed_area(
            VirtAddr::from(heap_start),
            VirtAddr::from(heap_end),
            MapPermission::R | MapPermission::W | MapPermission::U,
        );
        // 为当前线程分配用户资源，并保留 trap context PPN，避免再次页表遍历。
        let _t_stack = perf::perf_time_now();
        let trap_cx_ppn = memory_set
            .alloc_user_res_with_trap_ppn(self.user_res_slot, true)
            .map_err(|_| ENOMEM)?;
        // 创建ELF参数表
        let user_sp =
            memory_set.create_elf_tables(self.ustack_bottom_va(), argv_vec, envp_vec, &elf_info)?;
        let _stack_ticks = perf::perf_time_now().wrapping_sub(_t_stack);
        perf::EXECVE_STACK_TABLES_TICKS.fetch_add(_stack_ticks, Ordering::Relaxed);
        // 初始化陷阱上下文
        let trap_cx = TrapContext::app_init_context(
            if let Some(interp_entry) = elf_info.interp_entry {
                interp_entry
            } else {
                elf_info.entry
            },
            // 用户栈指针
            user_sp,
            // 内核页表令牌
            KERNEL_SPACE.lock().token(),
            // 内核栈顶
            self.kstack.get_top(),
            // 陷阱处理函数地址
            trap_handler as usize,
        );
        self.install_exec_image(
            elf,
            current_vm,
            old_stack_allocated,
            memory_set,
            trap_cx_ppn,
            trap_cx,
        )
    }

    /// 加载ELF文件（零拷贝路径：直接通过 PageCache 映射，无需内核空间临时映射）。
    /// 若文件所在文件系统无 PageCache，返回 `ENOSYS` 以触发回退到 `load_elf`。
    pub fn load_elf_direct(
        &self,
        elf: Arc<vfs::File>,
        argv_vec: &Vec<String>,
        envp_vec: &Vec<String>,
    ) -> Result<(), isize> {
        if elf.is_dir() {
            return Err(EISDIR);
        }
        // 即使当前看起来只有一个 live thread，也不能在建立 exec gate 前回收
        // 旧 VM；late CLONE_THREAD 可能在检查后发布并继续执行旧映像。
        let current_vm = self.process.vm();
        let old_stack_allocated = self.user_stack_allocated.load(Ordering::Relaxed);

        // 直接从 inode 和 PageCache 解析 ELF 并映射到用户地址空间
        let _t_map = perf::perf_time_now();
        let load_result = AddressSpaceInner::from_elf_inode(elf.clone());
        let _map_ticks = perf::perf_time_now().wrapping_sub(_t_map);
        perf::EXECVE_MAP_ELF_TICKS.fetch_add(_map_ticks, Ordering::Relaxed);
        // 零拷贝路径无需 kmap 和 teardown —— 记录零值用于性能对比
        perf::EXECVE_KERNEL_MAP_TICKS.fetch_add(0, Ordering::Relaxed);
        perf::EXECVE_TEARDOWN_TICKS.fetch_add(0, Ordering::Relaxed);

        let (mut memory_set, program_break, elf_info) = match load_result {
            Ok(result) => result,
            Err(e) => return Err(e),
        };
        // 为 glibc 分配用户 heap 空间
        use crate::mm::{MapPermission, VirtAddr};

        let page_size = 0x1000;
        let heap_start = align_up(program_break, page_size);
        let heap_end = heap_start + 0x20000; // 64KiB
        memory_set.insert_framed_area(
            VirtAddr::from(heap_start),
            VirtAddr::from(heap_end),
            MapPermission::R | MapPermission::W | MapPermission::U,
        );
        // 为当前线程分配用户资源
        let _t_stack = perf::perf_time_now();
        let trap_cx_ppn = memory_set
            .alloc_user_res_with_trap_ppn(self.user_res_slot, true)
            .map_err(|_| ENOMEM)?;
        // 创建ELF参数表
        let user_sp =
            memory_set.create_elf_tables(self.ustack_bottom_va(), argv_vec, envp_vec, &elf_info)?;
        let _stack_ticks = perf::perf_time_now().wrapping_sub(_t_stack);
        perf::EXECVE_STACK_TABLES_TICKS.fetch_add(_stack_ticks, Ordering::Relaxed);
        // 初始化陷阱上下文
        let trap_cx = TrapContext::app_init_context(
            if let Some(interp_entry) = elf_info.interp_entry {
                interp_entry
            } else {
                elf_info.entry
            },
            user_sp,
            KERNEL_SPACE.lock().token(),
            self.kstack.get_top(),
            trap_handler as usize,
        );
        self.install_exec_image(
            elf,
            current_vm,
            old_stack_allocated,
            memory_set,
            trap_cx_ppn,
            trap_cx,
        )
    }

    /// 创建新的任务控制块
    pub fn sys_clone(
        self: &Arc<TaskControlBlock>,
        flags: CloneFlags,
        stack: *const u8,
        tls: usize,
        exit_signal: Signals,
    ) -> Result<Arc<TaskControlBlock>, isize> {
        let quota = TaskQuotaGuard::try_acquire()?;

        // ---- 保持父PCB锁
        let mut parent_inner = self.acquire_inner_lock();
        // 当前调度器不实现真实 RT 调度，FIFO/RR 只作为 syscall 兼容状态。
        // fork 时将子任务降回 normal，可避免测试进程间泄漏伪 RT 状态。
        let reset_sched_on_fork = parent_inner.sched_reset_on_fork
            || self.process.sched_reset_on_fork()
            || matches!(parent_inner.sched_policy, 1 | 2);
        let child_sched_policy = if reset_sched_on_fork {
            0
        } else {
            parent_inner.sched_policy
        };
        let child_sched_priority = if reset_sched_on_fork {
            0
        } else {
            parent_inner.sched_priority
        };
        let child_sched_nice = if reset_sched_on_fork {
            0
        } else {
            parent_inner.sched_nice
        };
        let child_sched_runtime = if reset_sched_on_fork {
            0
        } else {
            parent_inner.sched_runtime
        };
        let child_sched_deadline = if reset_sched_on_fork {
            0
        } else {
            parent_inner.sched_deadline
        };
        let child_sched_period = if reset_sched_on_fork {
            0
        } else {
            parent_inner.sched_period
        };
        let parent_trap_cx = *parent_inner.trap_context_mut();
        // fork/clone 的 PTE 操作可能触发远端 shootdown。先保存所需上下文并
        // 释放 task.inner，确保后续等待 ack 时不跨普通锁。
        drop(parent_inner);
        // 复制用户空间（包括陷阱上下文）
        let share_vm = flags.contains(CloneFlags::CLONE_VM);
        let parent_vm = self.process.vm();
        let memory_set = if share_vm {
            parent_vm.clone() // 共享虚拟内存空间（线程）
        } else {
            // 复制地址空间（进程）
            crate::mm::frame_reserve(16);
            let copied = parent_vm.write(|vm| {
                AddressSpaceInner::from_existing_user(vm, self.user_res_slot, &parent_trap_cx)
            })?;
            Arc::new(AddressSpace::new(copied))
        };

        // 共享地址空间时，trap context 的虚拟地址也共享，必须复用同一个用户资源槽位分配器。
        // fork 复制出独立地址空间时，子进程沿用当前线程的 slot：slot 是地址空间内布局索引，
        // 不是全局线程 ID，独立地址空间之间可以重复使用同一个 slot 号。
        let user_res_slot_allocator = if share_vm {
            self.process.user_res_slot_allocator()
        } else {
            let allocator = self.process.user_res_slot_allocator();
            let cloned_allocator = allocator.lock().clone();
            Arc::new(Mutex::new(cloned_allocator))
        };
        // 在内核空间分配一个用户可见 tid 和一个内核栈
        let tid_handle = tid_alloc();
        let user_res_slot = if share_vm {
            user_res_slot_allocator.lock().alloc()
        } else {
            self.user_res_slot
        };
        let user_stack_allocated =
            !share_vm || (stack.is_null() && !flags.contains(CloneFlags::CLONE_VFORK));
        super::perf::record_clone(
            flags.contains(CloneFlags::CLONE_THREAD),
            share_vm,
            user_stack_allocated,
        );
        let (process, thread_quota) = if flags.contains(CloneFlags::CLONE_THREAD) {
            (self.process.clone(), Some(quota))
        } else {
            let parent_process = if flags.contains(CloneFlags::CLONE_PARENT) {
                self.process.parent()
            } else {
                Some(self.process.clone())
            };
            let files = if flags.contains(CloneFlags::CLONE_FILES) {
                self.process.files()
            } else {
                Arc::new(Mutex::new(
                    self.process
                        .files()
                        .lock()
                        .try_clone()
                        .map_err(|e| e as isize)?,
                ))
            };
            let fs = if flags.contains(CloneFlags::CLONE_FS) {
                self.process.fs()
            } else {
                Arc::new(Mutex::new(self.process.fs().lock().clone()))
            };
            let uts = if flags.contains(CloneFlags::CLONE_NEWUTS) {
                Arc::new(Mutex::new(self.process.uts().lock().clone()))
            } else {
                self.process.uts()
            };
            let net = if flags.contains(CloneFlags::CLONE_NEWNET) {
                NetNamespace::new_isolated()
            } else {
                self.process.net().clone()
            };
            let mnt = if flags.contains(CloneFlags::CLONE_NEWNS) {
                MountNamespace::new()
            } else {
                self.process.mnt()
            };
            let ipc = if flags.contains(CloneFlags::CLONE_NEWIPC) {
                IpcNamespace::new()
            } else {
                self.process.ipc()
            };
            let sighand = if flags.contains(CloneFlags::CLONE_SIGHAND) {
                self.process.sighand()
            } else {
                let sighand = self.process.sighand();
                let lock = sighand.lock();
                Arc::new(Mutex::new(Sighand::from_existing(&lock)))
            };
            let futex = if share_vm {
                self.process.futex()
            } else {
                Arc::new(Mutex::new(FutexTable::new()))
            };
            (
                Arc::new(ProcessControlBlock::new(
                    tid_handle.0,
                    tid_handle.clone(),
                    quota,
                    self.process.getpgid(),
                    self.process.getsid(),
                    parent_process.as_ref().map(Arc::downgrade),
                    self.process.exe(),
                    self.process.exe_path(),
                    files,
                    fs,
                    uts,
                    net,
                    mnt,
                    ipc,
                    memory_set.clone(),
                    sighand,
                    futex,
                    user_res_slot_allocator.clone(),
                )),
                None,
            )
        };
        // 分配内核栈
        let kstack = kstack_alloc();
        let kstack_top = kstack.get_top();

        // 共享 VM 的任务需要独立 trap context；用户栈只在未指定 child stack 时分配。
        let trap_cx_va = trap_cx_bottom_from_slot(user_res_slot);
        let trap_cx_ppn = if share_vm {
            if flags.contains(CloneFlags::CLONE_THREAD) {
                process.take_cached_trap_context_slot(user_res_slot);
            }
            let allocation = memory_set.write(|vm| {
                let result = vm.alloc_user_res_with_trap_ppn(user_res_slot, user_stack_allocated);
                if result.is_err() {
                    vm.dealloc_user_res_with_stack(user_res_slot, user_stack_allocated);
                }
                result
            });
            match allocation {
                Ok(ppn) => ppn,
                Err(err) => {
                    warn!(
                        "[sys_clone] failed to allocate trap context: slot={}, va={:#x}, err={:?}",
                        user_res_slot, trap_cx_va, err
                    );
                    user_res_slot_allocator.lock().dealloc(user_res_slot);
                    return Err(ENOMEM);
                }
            }
        } else {
            // fork copied the parent's trap context into the new address space already.
            match memory_set.read(|vm| vm.translate(VirtAddr::from(trap_cx_va).into())) {
                Some(ppn) => ppn,
                None => {
                    warn!(
                        "[sys_clone] trap context is not mapped after fork copy: slot={}, va={:#x}",
                        user_res_slot, trap_cx_va
                    );
                    memory_set.write(|vm| {
                        vm.dealloc_user_res_with_stack(user_res_slot, user_stack_allocated)
                    });
                    user_res_slot_allocator.lock().dealloc(user_res_slot);
                    return Err(ENOMEM);
                }
            }
        };

        // MM 更新及其远端 ack 已全部结束，现在重新取得父任务快照，构造
        // 子 TCB 时不会再进入任何 PTE 修改路径。
        let parent_inner = self.acquire_inner_lock();
        // 创建任务控制块
        let task_control_block = Arc::new(TaskControlBlock {
            // 基础标识信息
            tid: AtomicUsize::new(tid_handle.0),
            tid_handle: Mutex::new(tid_handle),
            user_res_slot,
            owns_user_res_slot: true,
            process,
            kstack,
            kernel_entry: None,
            ustack_base: if !stack.is_null() {
                stack as usize
            } else {
                ustack_bottom_from_slot(user_res_slot)
            },
            user_stack_allocated: AtomicBool::new(user_stack_allocated),
            thread_live_counted: AtomicBool::new(false),
            seccomp_counted: AtomicBool::new(false),
            uid_hint: AtomicUsize::new(parent_inner.uid as usize),
            euid_hint: AtomicUsize::new(parent_inner.euid as usize),
            suid_hint: AtomicUsize::new(parent_inner.suid as usize),
            gid_hint: AtomicUsize::new(parent_inner.gid as usize),
            egid_hint: AtomicUsize::new(parent_inner.egid as usize),
            sgid_hint: AtomicUsize::new(parent_inner.sgid as usize),
            exit_signal: Mutex::new(exit_signal),
            thread_quota: Mutex::new(thread_quota),
            wait_io_timer_pending: AtomicBool::new(false),
            wait_timer_generation: AtomicUsize::new(0),
            wait_io_fallback_active_generation: AtomicUsize::new(0),
            sched_nice_hint: AtomicI32::new(child_sched_nice),
            sched_vruntime_hint: AtomicU64::new(0),
            sched_state: AtomicUsize::new(TaskStatus::New.encode()),
            last_cpu: AtomicUsize::new(usize::MAX),
            // Linux affinity 是 per-thread 属性；fork 子线程/进程继承父 mask，
            // exec 则复用同一 TCB，自然保留该值。
            cpus_allowed: AtomicUsize::new(self.cpus_allowed()),
            migration_target: AtomicUsize::new(NO_MIGRATION_TARGET),
            remote_affinity_request: Mutex::new(None),
            inner: Mutex::new(TaskControlBlockInner {
                // clone
                sigpending: SignalQueue::empty(),
                signal_wait_mask: Signals::empty(),
                signal_stack: if share_vm {
                    SignalStack::disabled()
                } else {
                    parent_inner.signal_stack
                },
                // new
                rusage: Rusage::new(),
                clock: ProcClock::new(),
                clear_child_tid: 0,
                robust_list: RobustList::default(),
                timer: [ITimerVal::new(); 3],
                real_timer_deadline: None,
                real_timer_generation: 0,
                posix_timers: Vec::new(),
                sigmask: parent_inner.sigmask,
                sigmask_to_restore: None,
                // compute
                trap_cx_ppn,
                task_cx: TaskContext::goto_trap_return(kstack_top),
                // constants
                sched_policy: child_sched_policy,
                sched_priority: child_sched_priority,
                sched_reset_on_fork: false,
                sched_nice: child_sched_nice,
                sched_vruntime: 0,
                sched_runtime: child_sched_runtime,
                sched_deadline: child_sched_deadline,
                sched_period: child_sched_period,
                ioprio_class: parent_inner.ioprio_class,
                ioprio_prio: parent_inner.ioprio_prio,
                rtprio_limit_cur: parent_inner.rtprio_limit_cur,
                rtprio_limit_max: parent_inner.rtprio_limit_max,
                nice_limit_cur: parent_inner.nice_limit_cur,
                nice_limit_max: parent_inner.nice_limit_max,
                sigpending_limit_cur: parent_inner.sigpending_limit_cur,
                sigpending_limit_max: parent_inner.sigpending_limit_max,
                stack_limit_cur: parent_inner.stack_limit_cur,
                stack_limit_max: parent_inner.stack_limit_max,
                memlock_limit_cur: parent_inner.memlock_limit_cur,
                memlock_limit_max: parent_inner.memlock_limit_max,
                fsize_limit_cur: parent_inner.fsize_limit_cur,
                fsize_limit_max: parent_inner.fsize_limit_max,
                nproc_limit_cur: parent_inner.nproc_limit_cur,
                nproc_limit_max: parent_inner.nproc_limit_max,
                cpu_limit_cur: parent_inner.cpu_limit_cur,
                cpu_limit_max: parent_inner.cpu_limit_max,
                cpu_limit_sigxcpu_sent: false,
                core_limit_cur: parent_inner.core_limit_cur,
                core_limit_max: parent_inner.core_limit_max,
                personality: parent_inner.personality,
                pdeath_signal: 0,
                dumpable: parent_inner.dumpable,
                task_comm: parent_inner.task_comm,
                timer_slack_ns: parent_inner.timer_slack_ns,
                timer_slack_default_ns: parent_inner.timer_slack_ns,
                ptrace_traceme: false,
                uid: parent_inner.uid,
                euid: parent_inner.euid,
                suid: parent_inner.suid,
                fsuid: parent_inner.fsuid,
                gid: parent_inner.gid,
                egid: parent_inner.egid,
                sgid: parent_inner.sgid,
                fsgid: parent_inner.fsgid,
                umask: parent_inner.umask,
                groups: parent_inner.groups.clone(),
                cap_effective: parent_inner.cap_effective,
                cap_permitted: parent_inner.cap_permitted,
                cap_inheritable: parent_inner.cap_inheritable,
                cap_bounding: parent_inner.cap_bounding,
                no_new_privs: parent_inner.no_new_privs,
                thp_disabled: parent_inner.thp_disabled,
                securebits: parent_inner.securebits,
                cap_ambient: parent_inner.cap_ambient,
                seccomp_mode: parent_inner.seccomp_mode,
                seccomp_filter: parent_inner.seccomp_filter.clone(),
                pending_oom_kill: false,
            }),
        });
        // 初始化陷阱上下文
        {
            let mut inner = task_control_block.acquire_inner_lock();
            let trap_cx = inner.trap_context_mut();
            // 共享 VM 时新分配的 trap context 为空，需要从父任务当前上下文复制。
            if share_vm {
                *trap_cx = parent_trap_cx;
            }
            // we also do not need to prepare parameters on stack, musl has done it for us
            // 处理用户栈指针
            if !stack.is_null() {
                trap_cx.gp.sp = stack as usize;
            }
            // 设置线程寄存器
            if flags.contains(CloneFlags::CLONE_SETTLS) {
                // thread local storage
                // 线程局部存储
                trap_cx.gp.tp = tls;
            }
            // 对于子进程，fork返回0
            trap_cx.gp.a0 = 0;
            // 修改陷阱上下文中的内核栈指针
            trap_cx.kernel_sp = kstack_top;
        }
        if !flags.contains(CloneFlags::CLONE_THREAD) && !flags.contains(CloneFlags::CLONE_NEWIPC) {
            shm_clone_attachments(self.pid(), task_control_block.pid())?;
        }
        if parent_inner.seccomp_mode != 0 {
            task_control_block.account_seccomp_enabled();
        }
        if !flags.contains(CloneFlags::CLONE_THREAD) {
            task_control_block.process.set_sched_state(
                child_sched_policy,
                child_sched_priority,
                false,
                child_sched_nice,
                child_sched_runtime,
                child_sched_deadline,
                child_sched_period,
            );
            registry::register_process(&task_control_block.process);
        }
        registry::register_task(&task_control_block);
        // 返回
        Ok(task_control_block)
        // ---- 释放父PCB锁
    }
    /// Publish a successfully initialized clone into the waitable child tree.
    /// `CLONE_THREAD` tasks are not waitable children and are only scheduled.
    pub fn publish_clone_child(
        self: &Arc<TaskControlBlock>,
        child: Arc<TaskControlBlock>,
        flags: CloneFlags,
    ) -> Result<(), isize> {
        if flags.contains(CloneFlags::CLONE_THREAD) {
            return Ok(());
        }
        if flags.contains(CloneFlags::CLONE_PARENT) {
            let parent = child.process.parent();
            if let Some(parent) = parent {
                parent.add_child(child.process.clone())?;
            } else {
                warn!("[publish_clone_child] CLONE_PARENT target parent is gone");
            }
        } else {
            self.process.add_child(child.process.clone())?;
        }
        Ok(())
    }

    /// Drop resources allocated for a clone that has not been published.
    pub fn cleanup_unpublished_clone(&self, shared_vm: bool) {
        if shared_vm {
            self.process.vm().write(|vm| {
                vm.dealloc_user_res_with_stack(
                    self.user_res_slot,
                    self.user_stack_allocated.load(Ordering::Relaxed),
                )
            });
        }
    }

    /// 获取用户可见线程 ID。
    pub fn gettid(&self) -> usize {
        self.tid.load(Ordering::Acquire)
    }

    /// 用新的分配器句柄替换 TID，并返回旧句柄。
    ///
    /// 只允许 task registry 在非 leader exec 的重键事务中调用。registry 锁让
    /// 按 TID 查找看不到半完成状态，Release 写则让无锁诊断读者最终观察到新值。
    pub(super) fn replace_tid(
        &self,
        expected_tid: usize,
        new_handle: Arc<TidHandle>,
    ) -> Arc<TidHandle> {
        assert_eq!(
            self.gettid(),
            expected_tid,
            "unexpected TID before exec swap"
        );
        let mut handle = self.tid_handle.lock();
        assert_eq!(
            handle.0, expected_tid,
            "TID value and allocator handle diverged"
        );
        let old_handle = core::mem::replace(&mut *handle, new_handle);
        self.tid.store(handle.0, Ordering::Release);
        old_handle
    }

    /// 获取用户可见进程 ID。
    pub fn getpid(&self) -> usize {
        self.process.pid
    }
    /// 获取用户可见进程 ID。
    pub fn pid(&self) -> usize {
        self.process.pid
    }

    pub fn uid(&self) -> u32 {
        self.uid_hint.load(Ordering::Relaxed) as u32
    }

    pub fn euid(&self) -> u32 {
        self.euid_hint.load(Ordering::Relaxed) as u32
    }

    pub fn suid(&self) -> u32 {
        self.suid_hint.load(Ordering::Relaxed) as u32
    }

    pub fn gid(&self) -> u32 {
        self.gid_hint.load(Ordering::Relaxed) as u32
    }

    pub fn egid(&self) -> u32 {
        self.egid_hint.load(Ordering::Relaxed) as u32
    }

    pub fn sgid(&self) -> u32 {
        self.sgid_hint.load(Ordering::Relaxed) as u32
    }

    pub fn sched_nice(&self) -> i32 {
        self.sched_nice_hint.load(Ordering::Relaxed)
    }

    pub fn store_identity_hint(
        &self,
        uid: u32,
        euid: u32,
        suid: u32,
        gid: u32,
        egid: u32,
        sgid: u32,
    ) {
        self.uid_hint.store(uid as usize, Ordering::Relaxed);
        self.euid_hint.store(euid as usize, Ordering::Relaxed);
        self.suid_hint.store(suid as usize, Ordering::Relaxed);
        self.gid_hint.store(gid as usize, Ordering::Relaxed);
        self.egid_hint.store(egid as usize, Ordering::Relaxed);
        self.sgid_hint.store(sgid as usize, Ordering::Relaxed);
    }

    /// 获取线程组 ID（当前简化为进程 ID）
    pub fn tgid(&self) -> usize {
        self.process.pid
    }
    /// 尝试获取内部锁，用于 panic 诊断等不可阻塞场景
    pub fn try_inner(&self) -> Option<spin::MutexGuard<TaskControlBlockInner>> {
        self.inner.try_lock()
    }
    /// 设置进程组ID
    pub fn setpgid(&self, pgid: usize) -> isize {
        self.process.setpgid(pgid)
    }
    // 获取进程组ID
    pub fn getpgid(&self) -> usize {
        self.process.getpgid()
    }
    /// 获取用户空间的token
    pub fn get_user_token(&self) -> usize {
        self.process.user_token()
    }
}

impl Drop for TaskControlBlock {
    /// 当任务控制块被销毁时，释放用户资源槽位
    fn drop(&mut self) {
        self.unaccount_seccomp_enabled();
        registry::unregister_task_if_match(self);
        self.process.remove_thread(self);
        if self.owns_user_res_slot {
            self.process
                .user_res_slot_allocator()
                .lock()
                .dealloc(self.user_res_slot);
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
/// 任务的调度所有权状态。
///
/// `Queued(cpu)` 和 `Running(cpu)` 直接携带 owner。`Migrating` 只覆盖 queued
/// 任务已经离开源队列、尚未进入目标队列的短窗口；此时唯一 owner 是迁移调用方，
/// 状态本身不携带 CPU，避免把尚未取得任务的目标误写成 owner。
pub enum TaskStatus {
    New,
    Queued(usize),
    Running(usize),
    /// 当前任务已经登记到 interruptible registry，但尚未真正切离 CPU。
    ///
    /// 唤醒方可以把它恢复为 `Running(cpu)`，从而取消本次阻塞；idle 侧只有在
    /// context switch 完成后，才会把仍处于此状态的任务提交为 `Blocked`。
    Blocking(usize),
    Blocked,
    Migrating,
    Zombie,
}

impl TaskStatus {
    /// 低三位：状态tag
    /// 高位：CPU号（仅在 Queued/Running/Blocking 状态下有效）
    const TAG_BITS: usize = 3;
    const TAG_MASK: usize = (1 << Self::TAG_BITS) - 1;
    const NEW_TAG: usize = 0;
    const BLOCKED_TAG: usize = 1;
    const QUEUED_TAG: usize = 2;
    const RUNNING_TAG: usize = 3;
    const BLOCKING_TAG: usize = 4;
    const ZOMBIE_TAG: usize = 5;
    const MIGRATING_TAG: usize = 6;

    const fn encode(self) -> usize {
        match self {
            Self::New => Self::NEW_TAG,
            Self::Blocked => Self::BLOCKED_TAG,
            Self::Queued(cpu) => (cpu << Self::TAG_BITS) | Self::QUEUED_TAG,
            Self::Running(cpu) => (cpu << Self::TAG_BITS) | Self::RUNNING_TAG,
            Self::Blocking(cpu) => (cpu << Self::TAG_BITS) | Self::BLOCKING_TAG,
            Self::Zombie => Self::ZOMBIE_TAG,
            Self::Migrating => Self::MIGRATING_TAG,
        }
    }

    fn decode(raw: usize) -> Self {
        match raw & Self::TAG_MASK {
            Self::NEW_TAG => Self::New,
            Self::BLOCKED_TAG => Self::Blocked,
            Self::QUEUED_TAG => Self::Queued(raw >> Self::TAG_BITS),
            Self::RUNNING_TAG => Self::Running(raw >> Self::TAG_BITS),
            Self::BLOCKING_TAG => Self::Blocking(raw >> Self::TAG_BITS),
            Self::ZOMBIE_TAG => Self::Zombie,
            Self::MIGRATING_TAG => Self::Migrating,
            tag => panic!("invalid scheduler state tag: {}", tag),
        }
    }
}
