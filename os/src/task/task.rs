use super::pid::RecycleAllocator;
use super::process::ProcessControlBlock;
use super::quota::TaskQuotaGuard;
use super::registry;
use super::signal::*;
use super::threads::{futex_wake_shared, Futex};
use super::TaskContext;
use super::{tid_alloc, trap_cx_bottom_from_slot, ustack_bottom_from_slot, TidHandle, INIT_IPC_NAMESPACE, INIT_MOUNT_NAMESPACE, IpcNamespace, MountNamespace, NetNamespace};
use crate::config::{MMAP_BASE, SYSTEM_TASK_LIMIT, USER_STACK_SIZE};
use crate::fs::vfs;
use crate::fs::{vfs_lookup_absolute, vfs_root};
use crate::hal::TrapImpl;
use crate::hal::{kstack_alloc, KernelStack};
use crate::hal::{trap_handler, TrapContext};
use crate::mm::PageTableImpl;
use crate::mm::{AddressSpace, FaultAccess, PhysPageNum, VirtAddr, KERNEL_SPACE};
use crate::syscall::CloneFlags;
use crate::syscall::errno::{EISDIR, ENOEXEC, ENOMEM};
use crate::timer::{ITimerVal, TimeSpec, TimeVal, USEC_PER_SEC};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::{self, Debug, Formatter};
use core::sync::atomic::AtomicBool;
use log::{trace, warn};
use spin::{Mutex, MutexGuard};

const TASK_CAP_FULL_SET: u64 = (1u64 << 41) - 1;
const DEFAULT_TIMER_SLACK_NS: usize = 50_000;

fn default_task_comm() -> [u8; 16] {
    let mut comm = [0u8; 16];
    comm[..8].copy_from_slice(b"initproc");
    comm
}

fn default_groups() -> Vec<u32> {
    let mut groups = Vec::new();
    groups.push(0);
    groups
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

/// 任务控制块
pub struct TaskControlBlock {
    // 不可变字段
    /// 用户可见线程 ID，即 gettid() 返回值
    pub tid: Arc<TidHandle>,
    /// 同一地址空间内 trap context / 默认用户栈的资源槽位
    pub user_res_slot: usize,
    /// 所属用户可见进程
    pub process: Arc<ProcessControlBlock>,
    /// 内核栈
    pub kstack: KernelStack,
    /// 用户栈基址
    pub ustack_base: usize,
    /// 退出信号
    pub exit_signal: Signals,
    /// CLONE_THREAD 线程的 quota。非线程 clone 的 quota 在 PCB 上。
    _thread_quota: Option<TaskQuotaGuard>,
    // 可变字段
    /// 任务内部状态，使用互斥锁保护
    inner: Mutex<TaskControlBlockInner>,
    // 可共享&可变字段
    /// I/O 等待定时器是否已挂入 KERNEL_TIMER_QUEUE。
    /// 为 true 时，wait_io_core_with_queue 不再添加第二个定时器（Option B），
    /// 防止在 log=off 的高频 loopback accept/connect 循环中 KERNEL_TIMER_QUEUE 无限增长。
    /// 定时器触发后，run_timer 会无条件清回 false（Option A）。
    pub wait_io_timer_pending: AtomicBool,
}

/// 任务控制块内部状态
pub struct TaskControlBlockInner {
    /// 信号掩码
    pub sigmask: Signals,
    /// sigsuspend 临时替换 mask 时保存的旧 mask，由 sigreturn 恢复。
    pub sigmask_to_restore: Option<Signals>,
    /// 待处理信号
    pub sigpending: SignalQueue,
    /// 备用信号栈，每线程独立
    pub signal_stack: SignalStack,
    /// 陷阱上下文的物理页号
    pub trap_cx_ppn: PhysPageNum,
    /// 任务上下文
    pub task_cx: TaskContext,
    /// 任务状态
    pub task_status: TaskStatus,
    /// POSIX 调度策略兼容字段。当前调度器仍是单核轮转，这里用于 syscall 语义回读。
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
    /// membarrier PRIVATE_EXPEDITED compatibility registration.
    /// MangoCore is single-core, so the barrier itself is a no-op after registration.
    pub membarrier_private_expedited_registered: bool,
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
    /// POSIX 用户/组 ID 兼容字段，供 LTP 权限类用例和 capability 查询使用。
    pub uid: u32,
    pub euid: u32,
    pub suid: u32,
    pub fsuid: u32,
    pub gid: u32,
    pub egid: u32,
    pub sgid: u32,
    pub fsgid: u32,
    /// Linux supplementary group list, used by getgroups/setgroups compatibility.
    pub groups: Vec<u32>,
    /// Linux capability 兼容字段。当前内核只做权限语义判定，不实现真实权能隔离。
    pub cap_effective: u64,
    pub cap_permitted: u64,
    pub cap_inheritable: u64,
    pub cap_bounding: u64,
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
    pub generation: usize,
}

impl PosixTimer {
    pub fn new(clock_id: usize, signal: Signals) -> Self {
        Self {
            clock_id,
            signal,
            interval: TimeSpec::new(),
            value: TimeSpec::new(),
            deadline: None,
            generation: 0,
        }
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
    3906, 3121, 2501, 1991, 1586, 1277, 1024, 820, 655, 526, 423, 335, 272, 215, 172, 137, 110,
    87, 70, 56, 45, 36, 29, 23, 18, 15,
];

fn sched_vruntime_delta_us(nice: i32, runtime_us: usize) -> u64 {
    if runtime_us == 0 {
        return 0;
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
    /// 获取陷阱上下文
    pub fn get_trap_cx(&self) -> &'static mut TrapContext {
        self.trap_cx_ppn.get_mut()
    }
    /// 获取任务状态
    fn get_status(&self) -> TaskStatus {
        self.task_status
    }
    /// 判断是否为僵尸态
    pub fn is_zombie(&self) -> bool {
        self.get_status() == TaskStatus::Zombie
    }
    /// 添加信号
    pub fn add_signal(&mut self, signal: Signals) {
        let _ = self.sigpending.enqueue_signal(signal, 0);
    }
    /// 在进入陷阱时更新进程时间
    pub fn update_process_times_enter_trap(&mut self) {
        // 获取当前时间
        let now = TimeVal::now();
        // 更新上次进入内核态的时间
        self.clock.last_enter_s_mode = now;
        // 计算时间差
        let diff = now - self.clock.last_enter_u_mode;
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
    pub fn update_process_times_leave_trap(&mut self, trap_cause: TrapImpl) {
        let now = TimeVal::now();
        if trap_cause.is_timer() {
            let diff = now - self.clock.last_enter_s_mode;
            self.rusage.ru_stime = self.rusage.ru_stime + diff;
            self.update_itimer_prof_if_exists(diff);
            self.enforce_cpu_rlimit();
        }
        self.clock.last_enter_u_mode = now;
    }

    fn enforce_cpu_rlimit(&mut self) {
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
                log::info!("Task's real timer expired, sending SIGALRM");
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
        let now = TimeVal::now();
        let diff = now - self.clock.last_real_timer_update;
        log::debug!("real_timer refreshing...");
        self.update_itimer_real_if_exists(diff);
        // 更新锚点，防止重复计算
        self.clock.last_real_timer_update = now;
    }
}

fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

impl TaskControlBlock {
    fn write_clear_child_tid_word(&self, addr: usize) -> Result<(Option<usize>, usize), isize> {
        let bytes = 0u32.to_ne_bytes();
        let vm = self.process.vm();
        let mut vm = vm.lock();
        let base_va = VirtAddr::from(addr);
        let before_key = vm
            .translate(base_va.floor())
            .map(|ppn| (ppn.0 << 12) + base_va.page_offset());
        let mut after_key = None;
        for (offset, byte) in bytes.iter().enumerate() {
            let va = VirtAddr::from(addr + offset);
            let pa = vm.fault_in_user_va(va, FaultAccess::Store)?;
            if offset == 0 {
                after_key = Some((pa.floor().0 << 12) + pa.page_offset());
            }
            pa.floor().get_bytes_array()[pa.page_offset()] = *byte;
        }
        after_key
            .map(|key| (before_key, key))
            .ok_or(crate::syscall::errno::EFAULT)
    }

    /// 获取任务内部状态的互斥锁
    pub fn acquire_inner_lock(&self) -> MutexGuard<TaskControlBlockInner> {
        self.inner.lock()
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
    /// 那些属于 ProcessControlBlock 的退出收尾。
    pub(crate) fn exit_thread_resources(&self, exit_code: u32) -> bool {
        log::trace!(
            "[exit_thread] Trying to exit tid {} pid {} with {}",
            self.tid.0,
            self.pid(),
            exit_code
        );
        let clear_child_tid = {
            let mut inner = self.acquire_inner_lock();
            if inner.task_status == TaskStatus::Zombie {
                return false;
            }
            inner.task_status = TaskStatus::Zombie;
            let clear_child_tid = inner.clear_child_tid;
            inner.clear_child_tid = 0;
            inner.robust_list = RobustList::default();
            clear_child_tid
        };

        if clear_child_tid != 0 {
            log::debug!(
                "[do_exit] do futex wake on clear_child_tid: {:X}",
                clear_child_tid
            );
            match self.write_clear_child_tid_word(clear_child_tid) {
                Ok((before_key, after_key)) => {
                    self.process.futex().lock().wake(clear_child_tid, 1);
                    if let Some(before_key) = before_key {
                        futex_wake_shared(before_key, 1);
                    }
                    if before_key != Some(after_key) {
                        futex_wake_shared(after_key, 1);
                    }
                }
                Err(_) => log::warn!("invalid clear_child_tid"),
            };
        }

        self.process
            .vm()
            .lock()
            .dealloc_user_res(self.user_res_slot);

        log::info!(
            "[exit_thread] tid {} pid {} exited with {}",
            self.tid.0,
            self.pid(),
            exit_code
        );
        crate::utils::stats::print_resource_stats(Some(self));
        true
    }

    /// !!!!!!!!!!!!!!!!WARNING!!!!!!!!!!!!!!!!!!!!!
    /// 当前仅用于initproc加载。如果在其他地方使用，必须更改bin_path。
    /// 任务创建（仅用于initproc）
    pub fn new(elf: vfs::File) -> Arc<Self> {
        // 将ELF文件映射到内核空间
        let elf_data = elf.map_to_kernel_space(MMAP_BASE);
        log::debug!(
            "[TCB::new] elf_data.len() = {} (first 16 bytes: {:02X?})",
            elf_data.len(),
            &elf_data[..16.min(elf_data.len())]
        );
        if elf_data.is_empty() {
            panic!("[TCB::new] initproc ELF is empty");
        }
        // 带有ELF程序头/跳板的用户地址空间（AddressSpace）
        // 解析ELF文件，初始化内存映射
        let (mut memory_set, _user_heap, elf_info) =
            AddressSpace::<PageTableImpl>::from_elf(elf_data).expect("initproc ELF is invalid");
        // 在内核空间中删除ELF区域
        crate::mm::KERNEL_SPACE
            .lock()
            .remove_area_with_start_vpn(VirtAddr::from(MMAP_BASE).floor())
            .unwrap();

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

        // 为当前线程分配用户资源
        memory_set.alloc_user_res(user_res_slot, true);
        // 获取陷阱上下文的物理页号
        let trap_cx_ppn = memory_set
            .translate(VirtAddr::from(trap_cx_bottom_from_slot(user_res_slot)).into())
            .unwrap();
        log::trace!("[TCB::new]trap_cx_ppn{:?}", trap_cx_ppn);

        // 初始化新 VFS 文件描述符表
        let mut fd_table = vfs::FdTable::new();
        // 打开 /dev/tty 并分配 stdin/stdout/stderr（fd 0, 1, 2）
        let tty_inode = vfs_lookup_absolute("/dev/tty").unwrap();
        let tty_file = vfs::File::new(tty_inode, vfs::FileFlags::O_RDWR).unwrap();
        fd_table.alloc_fd(tty_file, false).unwrap();
        let tty_inode = vfs_lookup_absolute("/dev/tty").unwrap();
        let tty_file = vfs::File::new(tty_inode, vfs::FileFlags::O_RDWR).unwrap();
        fd_table.alloc_fd(tty_file, false).unwrap();
        let tty_inode = vfs_lookup_absolute("/dev/tty").unwrap();
        let tty_file = vfs::File::new(tty_inode, vfs::FileFlags::O_RDWR).unwrap();
        fd_table.alloc_fd(tty_file, false).unwrap();

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
            tid_handle.0,
            tid_handle.clone(),
            process_quota,
            pgid,
            pgid,
            None,
            Arc::new(Mutex::new(elf)),
            String::new(),
            Arc::new(Mutex::new(fd_table)),
            Arc::new(Mutex::new(FsStatus {
                working_inode: Arc::new(cwd),
                working_path: String::from("/"),
            })),
            Arc::new(Mutex::new(UtsNamespace::new())),
            INIT_NET_NAMESPACE.clone(),
            INIT_MOUNT_NAMESPACE.clone(),
            INIT_IPC_NAMESPACE.clone(),
            Arc::new(Mutex::new(memory_set)),
            Arc::new(Mutex::new(Sighand::new())),
            Arc::new(Mutex::new(Futex::new())),
            user_res_slot_allocator,
        ));

        // 创建任务控制块
        let task_control_block = Arc::new(Self {
            tid: tid_handle,
            user_res_slot,
            process,
            kstack,
            ustack_base: ustack_bottom_from_slot(user_res_slot),
            exit_signal: Signals::empty(),
            _thread_quota: None,
            wait_io_timer_pending: AtomicBool::new(false),
            inner: Mutex::new(TaskControlBlockInner {
                sigmask: Signals::empty(),
                sigmask_to_restore: None,
                sigpending: SignalQueue::empty(),
                signal_stack: SignalStack::disabled(),
                trap_cx_ppn,
                task_cx: TaskContext::goto_trap_return(kstack_top),
                task_status: TaskStatus::Ready,
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
                membarrier_private_expedited_registered: false,
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
                uid: 0,
                euid: 0,
                suid: 0,
                fsuid: 0,
                gid: 0,
                egid: 0,
                sgid: 0,
                fsgid: 0,
                groups: default_groups(),
                cap_effective: TASK_CAP_FULL_SET,
                cap_permitted: TASK_CAP_FULL_SET,
                cap_inheritable: 0,
                cap_bounding: TASK_CAP_FULL_SET,
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
        task_control_block.process.add_thread(&task_control_block);
        registry::register_process(&task_control_block.process);
        registry::register_task(&task_control_block);
        // 准备用户空间的陷阱上下文
        let trap_cx = task_control_block.acquire_inner_lock().get_trap_cx();
        // 初始化陷阱上下文
        *trap_cx = TrapContext::app_init_context(
            elf_info.entry,
            ustack_bottom_from_slot(user_res_slot),
            KERNEL_SPACE.lock().token(),
            kstack_top,
            trap_handler as usize,
        );
        trace!("[new] trap_cx:{:?}", *trap_cx);
        task_control_block
    }

    /// 加载ELF文件
    pub fn load_elf(
        &self,
        elf: vfs::File,
        argv_vec: &Vec<String>,
        envp_vec: &Vec<String>,
    ) -> Result<(), isize> {
        if elf.is_dir() {
            return Err(EISDIR);
        }
        // 旧 VM 没有被其他 CLONE_VM 进程共享时，可以先释放用户数据页，
        // 避免新旧内存集同时存在导致双倍内存压力触发 OOM。
        // 如果旧 VM 被共享（典型是 CLONE_VM | CLONE_VFORK），exec 必须先
        // 构造新地址空间，提交时再让当前进程脱离共享 VM，不能破坏父进程。
        let current_vm = self.process.vm();
        if Arc::strong_count(&current_vm) <= 2 {
            current_vm.lock().recycle_data_pages();
        }

        // 将ELF文件映射到内核空间
        let elf_data = elf.map_to_kernel_space(MMAP_BASE);
        if elf_data.is_empty() {
            log::error!("[load_elf] ELF file is empty (size=0)");
            return Err(ENOEXEC);
        }
        // 带有ELF程序头/跳板/陷阱上下文/用户栈的用户地址空间（AddressSpace）
        let load_result = AddressSpace::from_elf(elf_data);

        // 清除临时映射
        crate::mm::KERNEL_SPACE
            .lock()
            .remove_area_with_start_vpn(VirtAddr::from(MMAP_BASE).floor())
            .unwrap();

        let (mut memory_set, program_break, elf_info) = match load_result {
            Ok(result) => result,
            Err(e) => return Err(e),
        };
        log::trace!("[load_elf] ELF file mapped");

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
        log::info!(
            "[load_elf] mapped user heap from program_break: {:#x} ~ {:#x}",
            heap_start,
            heap_end
        );

        // 为当前线程分配用户资源
        memory_set.alloc_user_res(self.user_res_slot, true);
        // 创建ELF参数表
        let user_sp =
            memory_set.create_elf_tables(self.ustack_bottom_va(), argv_vec, envp_vec, &elf_info)?;
        log::trace!("[load_elf] user sp after pushing parameters: {:X}", user_sp);
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
        let other_threads: Vec<_> = self
            .process
            .threads()
            .into_iter()
            .filter(|task| task.tid.0 != self.tid.0)
            .collect();
        for task in &other_threads {
            // execve 会杀掉同线程组的其他线程，但保留当前 process。
            task.exit_thread_resources(Signals::SIGKILL.to_signum().unwrap() as u32);
        }
        super::remove_tasks_from_queues(&other_threads);

        {
            // **** 保持当前PCB锁
            let mut inner = self.acquire_inner_lock();
            // 更新陷阱上下文的物理页号
            inner.trap_cx_ppn = (&memory_set)
                .translate(VirtAddr::from(self.trap_cx_user_va()).into())
                .unwrap();
            // 更新任务上下文
            *inner.get_trap_cx() = trap_cx;
            // 重置clear_child_tid
            inner.clear_child_tid = 0;
            // 重置robust_list
            inner.robust_list = RobustList::default();
            // execve disables the alternate signal stack.
            inner.signal_stack = SignalStack::disabled();
        }
        if Arc::strong_count(&current_vm) > 2 {
            // CLONE_VM/vfork 子进程 exec 后会脱离旧地址空间。这里仅从旧 VM
            // 中移除当前线程的 trap context/默认栈映射，不释放 slot 号本身；
            // 新 VM 仍使用同一个 user_res_slot，避免父 VM 留下孤儿映射。
            current_vm.lock().dealloc_user_res(self.user_res_slot);
        }
        // 更新可执行文件描述符
        self.process.replace_exe(elf);
        // 清理资源 — 关闭所有 CLOEXEC 文件描述符
        self.process.files().lock().close_cloexec();
        // 替换内存映射
        self.process.replace_vm(memory_set);
        // 清空信号处理函数表
        self.process.sighand().lock().reset();
        // 清空futex
        self.process.futex().lock().clear();
        Ok(())
        // **** 释放当前PCB锁
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
        let parent_inner = self.acquire_inner_lock();
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
        // 复制用户空间（包括陷阱上下文）
        let share_vm = flags.contains(CloneFlags::CLONE_VM);
        let parent_vm = self.process.vm();
        let memory_set = if share_vm {
            parent_vm.clone() // 共享虚拟内存空间（线程）
        } else {
            // 复制地址空间（进程）
            crate::mm::frame_reserve(16);
            let parent_trap_cx = *parent_inner.get_trap_cx();
            let copied = AddressSpace::from_existing_user(
                &mut parent_vm.lock(),
                self.user_res_slot,
                &parent_trap_cx,
            )?;
            Arc::new(Mutex::new(copied))
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
                Arc::new(Mutex::new(Futex::new()))
            };
            (
                Arc::new(ProcessControlBlock::new(
                    tid_handle.0,
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
        if share_vm {
            memory_set.lock().alloc_user_res(
                user_res_slot,
                stack.is_null() && !flags.contains(CloneFlags::CLONE_VFORK),
            );
        }
        // 获取陷阱上下文的物理页号
        let trap_cx_va = trap_cx_bottom_from_slot(user_res_slot);
        let trap_cx_ppn = match memory_set
            .lock()
            .translate(VirtAddr::from(trap_cx_va).into())
        {
            Some(ppn) => ppn,
            None => {
                warn!(
                    "[sys_clone] trap context is not mapped after alloc_user_res: slot={}, va={:#x}",
                    user_res_slot, trap_cx_va
                );
                memory_set.lock().dealloc_user_res(user_res_slot);
                user_res_slot_allocator.lock().dealloc(user_res_slot);
                return Err(ENOMEM);
            }
        };

        // 创建任务控制块
        let task_control_block = Arc::new(TaskControlBlock {
            // 基础标识信息
            tid: tid_handle,
            user_res_slot,
            process,
            kstack,
            ustack_base: if !stack.is_null() {
                stack as usize
            } else {
                ustack_bottom_from_slot(user_res_slot)
            },
            exit_signal,
            _thread_quota: thread_quota,
            wait_io_timer_pending: AtomicBool::new(false),
            inner: Mutex::new(TaskControlBlockInner {
                // clone
                sigpending: SignalQueue::empty(),
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
                task_status: TaskStatus::Ready,
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
                membarrier_private_expedited_registered: parent_inner
                    .membarrier_private_expedited_registered,
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
                uid: parent_inner.uid,
                euid: parent_inner.euid,
                suid: parent_inner.suid,
                fsuid: parent_inner.fsuid,
                gid: parent_inner.gid,
                egid: parent_inner.egid,
                sgid: parent_inner.sgid,
                fsgid: parent_inner.fsgid,
                groups: parent_inner.groups.clone(),
                cap_effective: parent_inner.cap_effective,
                cap_permitted: parent_inner.cap_permitted,
                cap_inheritable: parent_inner.cap_inheritable,
                cap_bounding: parent_inner.cap_bounding,
                pending_oom_kill: false,
            }),
        });
        // 初始化陷阱上下文
        let trap_cx = task_control_block.acquire_inner_lock().get_trap_cx();
        // 共享 VM 时新分配的 trap context 为空，需要从父任务当前上下文复制。
        if share_vm {
            *trap_cx = *parent_inner.get_trap_cx();
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
        task_control_block.process.add_thread(&task_control_block);
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
            self.process.vm().lock().dealloc_user_res(self.user_res_slot);
        }
    }

    /// 获取用户可见线程 ID。
    pub fn gettid(&self) -> usize {
        self.tid.0
    }

        /// 获取用户可见进程 ID。
    pub fn getpid(&self) -> usize {
        self.process.pid
    }
    /// 获取用户可见进程 ID。
    pub fn pid(&self) -> usize {
        self.process.pid
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
        self.process.vm().lock().token()
    }
}

impl Drop for TaskControlBlock {
    /// 当任务控制块被销毁时，释放用户资源槽位
    fn drop(&mut self) {
        registry::unregister_task_if_match(self);
        self.process.remove_thread(self.tid.0);
        self.process
            .user_res_slot_allocator()
            .lock()
            .dealloc(self.user_res_slot);
    }
}

#[derive(Copy, Clone, PartialEq, Debug)]
/// 任务状态
pub enum TaskStatus {
    /// 就绪态
    Ready,
    /// 运行态
    Running,
    /// 僵尸态
    Zombie,
    /// 可中断态
    Interruptible,
}
