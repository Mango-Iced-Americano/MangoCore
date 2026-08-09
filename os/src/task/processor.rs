//! Per-CPU 处理器状态和调度主循环。
//!
//! 每个 CPU 通过 `PerCpu.task_state` 持有自己的 current 槽和 idle 上下文。
//! CPU0 额外负责全局 housekeeping；AP 只处理本地调度状态和 IPI。普通
//! 用户任务仍默认发布到 CPU0；AP 只接收受控的 kernel-only 任务，以及 SMP
//! ktest 显式发布或在 yield 安全点迁入的无共享 I/O 用户探针。
//!
//! # Locking
//!
//! 本 CPU 的 `processor` 锁只保护当前任务槽和 idle 上下文。切换前必须释放该锁，
//! 否则切回调度器时会形成自锁。

use super::run_queue::RunQueue;
use super::{__switch, do_wake_expired};
use super::{fetch_task, finish_switch_out};
use super::{TaskContext, TaskControlBlock, TaskStatus};
use crate::hal::PageTableImpl;
use crate::mm::{AddressSpace, UserVmContext};
use crate::net::config::NET_INTERFACE;
use alloc::{collections::VecDeque, sync::Arc, vec::Vec};
use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

const BACKGROUND_NET_POLL_INTERVAL: usize = 64;
/// idle 起点的空值；正常的单调微秒时间不可能达到该值。
const IDLE_TIME_INACTIVE: u64 = u64::MAX;

/// CPU0 周期 housekeeping 的可合并发布位。
///
/// timer 工作可能在任务安全点消费本地调度 tick，随后才切回 idle 栈。单独保存
/// 这个事件，避免 idle 调度器再次检查 timer 时因为 pending 已清空而漏做维护。
static BOOT_HOUSEKEEPING_PENDING: AtomicBool = AtomicBool::new(true);

/// 由 CPU0 timer 安全点发布一次周期 housekeeping。
pub(crate) fn request_boot_housekeeping() {
    debug_assert_eq!(crate::smp::cpu_id(), crate::smp::BOOT_CPU_ID);
    BOOT_HOUSEKEEPING_PENDING.store(true, Ordering::Release);
}

/// 只由 CPU0 idle 栈消费可合并的周期 housekeeping 请求。
fn take_boot_housekeeping() -> bool {
    debug_assert_eq!(crate::smp::cpu_id(), crate::smp::BOOT_CPU_ID);
    BOOT_HOUSEKEEPING_PENDING.swap(false, Ordering::Acquire)
}

#[cfg(all(feature = "boot_la_uboot_dmw", feature = "bringup_trace"))]
static BOARD_FIRST_TASK_SWITCH: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// 单个 CPU 独占的调度上下文。
struct Processor {
    /// 当前正在运行的任务
    current: Option<Arc<TaskControlBlock>>,
    /// 空闲任务的上下文，用于在任务切换时保存和恢复状态
    idle_task_cx: TaskContext,
}

impl Processor {
    const fn new() -> Self {
        Self {
            // 初始化时处理器为空闲
            current: None,
            // 空闲任务的上下文
            idle_task_cx: TaskContext::zero_init(),
        }
    }
    /// 返回 idle 任务上下文指针，供 `__switch` 保存/恢复寄存器。
    fn get_idle_task_cx_ptr(&mut self) -> *mut TaskContext {
        &mut self.idle_task_cx as *mut _
    }

    /// 在 context switch 已完成后取出上一任务。
    ///
    /// # Semantics
    ///
    /// 只能由 idle 调度循环调用；任务仍在自身内核栈上时提前清空会破坏
    /// current slot 的所有权语义。
    fn take_current(&mut self) -> Option<Arc<TaskControlBlock>> {
        // 将current字段置空，并返回其中的值
        self.current.take()
    }
    /// 克隆当前正在运行的任务。
    pub fn current(&self) -> Option<Arc<TaskControlBlock>> {
        self.current.as_ref().map(Arc::clone)
    }
}

/// 嵌入 `PerCpu` 的任务调度状态。
///
/// PID/TID 在 current 槽有效期内不变，因此保留无锁快照。身份、进程组和
/// 页表 token 可在运行期修改，读取时必须访问 TCB/PCB 的权威原子 hint。
pub(crate) struct CpuTaskState {
    processor: Mutex<Processor>,
    /// 本 CPU 唯一拥有的 runnable 容器；跨核代码只能锁定一个目标队列。
    pub(crate) run_queue: Mutex<RunQueue>,
    /// 本地排队任务数的无锁近似值，不包含 current。
    /// 精确成员关系仍以 `run_queue` 锁内的队列为准。
    pub(crate) nr_running: AtomicUsize,
    /// 已切回本 CPU idle 栈、等待本地释放最后调度 Arc 的终态任务。
    local_zombies: Mutex<VecDeque<Arc<TaskControlBlock>>>,
    /// `local_zombies` 的无锁精确计数；只用于避免空队列热路径取锁和诊断。
    nr_zombies: AtomicUsize,
    /// 当前 CPU 仍可直接返回的用户地址空间；Arc 固定精确的旧 MM，避免 exec
    /// 替换 `process.vm` 后无法清除旧 MM 的 active bit。
    active_user_vm: Mutex<Option<Arc<AddressSpace<PageTableImpl>>>>,
    /// 本 CPU 是否持有 current；仅供放置策略估算负载。
    current_present: AtomicBool,
    current_pid: AtomicUsize,
    current_tid: AtomicUsize,
    /// 0 表示无记录，实际 syscall id 存为 id + 1。
    current_syscall_id: AtomicUsize,
    /// 本 CPU 完成的底层上下文切换次数，task->idle 与 idle->task 分别计一次。
    context_switches: AtomicUsize,
    /// 任务上一次实际运行在其它 CPU、本次在本 CPU 开始运行的次数。
    migrations: AtomicUsize,
    /// 本 CPU 从其它 runqueue 成功取得任务并直接接管为 current 的次数。
    steals: AtomicUsize,
    /// 本 CPU runqueue 曾达到的最大排队任务数，不包含 current。
    run_queue_peak: AtomicUsize,
    /// 当前 CPU 实际执行用户态代码的累计微秒数。
    user_time_us: AtomicU64,
    /// 当前 CPU 为任务执行内核态代码的累计微秒数。
    system_time_us: AtomicU64,
    /// 当前 CPU 执行 idle 调度上下文的累计微秒数。
    idle_time_us: AtomicU64,
    /// 当前 idle 区间的单调时钟起点；`IDLE_TIME_INACTIVE` 表示正在运行任务。
    idle_since_us: AtomicU64,
    /// idle 起止更新的序列号；奇数表示本 CPU 正在改写区间状态。
    idle_sequence: AtomicU64,
}

/// `/proc/stat` 使用的单个 CPU 时间快照，单位为微秒。
#[derive(Clone, Copy, Default)]
pub(crate) struct CpuTimeSnapshot {
    pub(crate) user_us: u64,
    pub(crate) system_us: u64,
    pub(crate) idle_us: u64,
}

/// panic/STOP 等不可等待上下文可读取的任务侧诊断快照。
///
/// 原子字段只提供某一时刻的 best-effort 观察，不能替代 current/runqueue
/// 锁内的不变量判断。`active_mm_lock_busy` 为真时，`active_mm_id` 必须视为未知。
pub(crate) struct CpuTaskDiagnostics {
    pub(crate) current_present: bool,
    pub(crate) current_pid: usize,
    pub(crate) current_tid: usize,
    pub(crate) current_syscall_id: Option<usize>,
    pub(crate) nr_running: usize,
    pub(crate) nr_zombies: usize,
    pub(crate) active_mm_id: usize,
    pub(crate) active_mm_lock_busy: bool,
    pub(crate) context_switches: usize,
    pub(crate) migrations: usize,
    pub(crate) steals: usize,
    pub(crate) run_queue_peak: usize,
}

impl CpuTaskState {
    pub(crate) const fn new() -> Self {
        Self {
            processor: Mutex::new(Processor::new()),
            run_queue: Mutex::new(RunQueue::new()),
            nr_running: AtomicUsize::new(0),
            local_zombies: Mutex::new(VecDeque::new()),
            nr_zombies: AtomicUsize::new(0),
            active_user_vm: Mutex::new(None),
            current_present: AtomicBool::new(false),
            current_pid: AtomicUsize::new(0),
            current_tid: AtomicUsize::new(0),
            current_syscall_id: AtomicUsize::new(0),
            context_switches: AtomicUsize::new(0),
            migrations: AtomicUsize::new(0),
            steals: AtomicUsize::new(0),
            run_queue_peak: AtomicUsize::new(0),
            user_time_us: AtomicU64::new(0),
            system_time_us: AtomicU64::new(0),
            idle_time_us: AtomicU64::new(0),
            idle_since_us: AtomicU64::new(IDLE_TIME_INACTIVE),
            idle_sequence: AtomicU64::new(0),
        }
    }

    /// 开始一个 idle 调度上下文时间区间，仅由本 CPU idle 栈调用。
    fn begin_idle_time(&self) {
        let sequence = self.idle_sequence.fetch_add(1, Ordering::AcqRel);
        debug_assert_eq!(sequence & 1, 0, "CPU idle accounting writer overlapped");
        let now = crate::timer::get_time_us() as u64;
        let previous = self.idle_since_us.swap(now, Ordering::Release);
        debug_assert_eq!(previous, IDLE_TIME_INACTIVE, "CPU idle interval nested");
        self.idle_sequence.fetch_add(1, Ordering::Release);
    }

    /// 在任务即将成为 current 前结束 idle 区间。
    fn end_idle_time(&self) {
        let sequence = self.idle_sequence.fetch_add(1, Ordering::AcqRel);
        debug_assert_eq!(sequence & 1, 0, "CPU idle accounting writer overlapped");
        let now = crate::timer::get_time_us() as u64;
        let since = self
            .idle_since_us
            .swap(IDLE_TIME_INACTIVE, Ordering::AcqRel);
        debug_assert_ne!(since, IDLE_TIME_INACTIVE, "CPU left idle without entry");
        if since != IDLE_TIME_INACTIVE {
            self.idle_time_us
                .fetch_add(now.saturating_sub(since), Ordering::Relaxed);
        }
        self.idle_sequence.fetch_add(1, Ordering::Release);
    }

    /// 读取累计时间，并把尚未闭合的当前 idle 区间计入快照。
    fn read_cpu_time(&self) -> CpuTimeSnapshot {
        let idle_us = loop {
            let sequence = self.idle_sequence.load(Ordering::Acquire);
            if sequence & 1 != 0 {
                spin_loop();
                continue;
            }
            let accumulated = self.idle_time_us.load(Ordering::Relaxed);
            let idle_since = self.idle_since_us.load(Ordering::Relaxed);
            let active = if idle_since == IDLE_TIME_INACTIVE {
                0
            } else {
                (crate::timer::get_time_us() as u64).saturating_sub(idle_since)
            };
            if self.idle_sequence.load(Ordering::Acquire) == sequence {
                break accumulated.saturating_add(active);
            }
        };
        CpuTimeSnapshot {
            user_us: self.user_time_us.load(Ordering::Relaxed),
            system_us: self.system_time_us.load(Ordering::Relaxed),
            idle_us,
        }
    }

    pub(crate) fn record_migration(&self) {
        self.migrations.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_steal(&self) {
        self.steals.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_run_queue_len(&self, len: usize) {
        self.run_queue_peak.fetch_max(len, Ordering::Relaxed);
    }

    fn record_context_switch(&self) {
        self.context_switches.fetch_add(1, Ordering::Relaxed);
    }

    /// 不等待 processor、runqueue 或地址空间锁地读取诊断信息。
    /// `active_user_vm` 只做一次 `try_lock()`，失败即把 MM 标记为未知。
    pub(crate) fn read_diagnostics(&self) -> CpuTaskDiagnostics {
        let (active_mm_id, active_mm_lock_busy) = match self.active_user_vm.try_lock() {
            Some(active) => (active.as_ref().map(|vm| vm.mm_id()).unwrap_or(0), false),
            None => (0, true),
        };
        CpuTaskDiagnostics {
            // PID/TID 在 current_present 的 Release 发布前写入；Acquire 后读取即可
            // 获得一份诊断 hint。远端 CPU 仍可能继续切换，所以不承诺跨字段原子性。
            current_present: self.current_present.load(Ordering::Acquire),
            current_pid: self.current_pid.load(Ordering::Relaxed),
            current_tid: self.current_tid.load(Ordering::Relaxed),
            current_syscall_id: self
                .current_syscall_id
                .load(Ordering::Relaxed)
                .checked_sub(1),
            nr_running: self.nr_running.load(Ordering::Relaxed),
            nr_zombies: self.nr_zombies.load(Ordering::Acquire),
            active_mm_id,
            active_mm_lock_busy,
            context_switches: self.context_switches.load(Ordering::Relaxed),
            migrations: self.migrations.load(Ordering::Relaxed),
            steals: self.steals.load(Ordering::Relaxed),
            run_queue_peak: self.run_queue_peak.load(Ordering::Relaxed),
        }
    }
}

/// 在返回用户态前切换本 CPU 的活跃地址空间。
///
/// 名称与 Linux `switch_mm()` 对齐。槽锁只用于交换 Arc，不跨 VM 锁、ASID
/// rollover 或 IPI 等待；旧 MM 先 leave，新 MM 再以 generation 协议进入。
pub(crate) fn switch_user_vm(vm: Arc<AddressSpace<PageTableImpl>>) -> UserVmContext {
    let cpu = crate::smp::cpu_id();
    let state = crate::smp::local_task_state();
    let (same_mm, previous) = {
        let mut active = state.active_user_vm.lock();
        let same_mm = active
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &vm));
        (same_mm, if same_mm { None } else { active.take() })
    };
    crate::task::perf::record_task_switch_mm(same_mm);

    if same_mm {
        crate::task::perf::record_mm_same_already_active();
        return vm.activate_on(cpu);
    }
    if let Some(previous) = previous {
        previous.deactivate_on(cpu);
    }

    let context = vm.activate_on(cpu);
    let mut active = state.active_user_vm.lock();
    assert!(active.is_none(), "active user MM changed during switch");
    *active = Some(vm);
    context
}

/// 在 idle 栈上切离本 CPU 的用户地址空间。
///
/// `deactivate_on()` 内的完整屏障和 active-bit 清除发生在 current owner
/// 改变之前；之后该任务若再次运行，必须重新经过 `switch_user_vm()`。
fn leave_user_vm(cpu: usize) {
    debug_assert_eq!(cpu, crate::smp::cpu_id());
    let active = crate::smp::task_state(cpu).active_user_vm.lock().take();
    if let Some(active) = active {
        active.deactivate_on(cpu);
    }
}

/// 在退出 CPU 的 idle 栈上接收终态任务的最后一个调度 owner。
pub(crate) fn enqueue_zombie(cpu: usize, task: Arc<TaskControlBlock>) {
    assert_eq!(
        task.task_status(),
        TaskStatus::Zombie,
        "only terminal tasks may enter a local zombie queue"
    );
    let state = crate::smp::task_state(cpu);
    let mut queue = state.local_zombies.lock();
    queue.push_back(task);
    state.nr_zombies.fetch_add(1, Ordering::Release);
    drop(queue);
    super::perf::record_zombie_enqueue();
}

fn take_local_zombies(cpu: usize, limit: usize) -> Vec<Arc<TaskControlBlock>> {
    let state = crate::smp::task_state(cpu);
    // 先按原子计数在锁外分配承接容器。并发入队可能使真实长度
    // 更大，本轮只取快照中已发布的数量，剩余任务由下一轮回收。
    let capacity = limit.min(state.nr_zombies.load(Ordering::Acquire));
    if capacity == 0 {
        return Vec::new();
    }
    let mut zombies = Vec::with_capacity(capacity);
    let mut queue = state.local_zombies.lock();
    let count = capacity.min(queue.len());
    for _ in 0..count {
        zombies.push(
            queue
                .pop_front()
                .expect("local zombie count diverged from its queue"),
        );
    }
    let previous = state.nr_zombies.fetch_sub(count, Ordering::AcqRel);
    assert!(previous >= count, "local zombie count underflow");
    zombies
}

/// 返回全部 Per-CPU zombie 队列的无锁近似总数。
pub(crate) fn zombie_queue_count_fast() -> usize {
    (0..crate::smp::configured_cpu_count())
        .map(|cpu| {
            crate::smp::task_state(cpu)
                .nr_zombies
                .load(Ordering::Acquire)
        })
        .sum()
}

/// 无锁判断是否仍有 CPU-local zombie Arc 等待回收。
pub fn has_zombie_queue_tasks_fast() -> bool {
    zombie_queue_count_fast() != 0
}

/// 跨 CPU 依次取出 zombie；任一时刻只持有一个本地回收队列锁。
pub fn take_zombie_tasks(limit: usize) -> Vec<Arc<TaskControlBlock>> {
    let mut zombies = Vec::with_capacity(limit.min(zombie_queue_count_fast()));
    for cpu in 0..crate::smp::configured_cpu_count() {
        let remaining = limit.saturating_sub(zombies.len());
        if remaining == 0 {
            break;
        }
        zombies.extend(take_local_zombies(cpu, remaining));
    }
    zombies
}

/// 从所有本地回收队列移除指定进程的 TCB，并把析构责任交给调用方。
pub(crate) fn remove_zombie_tasks_by_pid(pid: usize) -> Vec<Arc<TaskControlBlock>> {
    let mut zombies = Vec::with_capacity(zombie_queue_count_fast());
    for cpu in 0..crate::smp::configured_cpu_count() {
        let state = crate::smp::task_state(cpu);
        loop {
            // 锁内只从容器摘取一个 Arc；Vec 扩容和最终析构均在锁外。
            let zombie = {
                let mut queue = state.local_zombies.lock();
                let Some(index) = queue.iter().position(|task| task.process.pid == pid) else {
                    break;
                };
                let task = queue.remove(index).expect("located local zombie vanished");
                let previous = state.nr_zombies.fetch_sub(1, Ordering::AcqRel);
                assert!(previous != 0, "local zombie count underflow");
                task
            };
            zombies.push(zombie);
        }
    }
    zombies
}

/// 只由本 CPU idle 循环调用；TCB 的最后一个调度 Arc 在这里安全释放。
fn drain_local_zombies(cpu: usize, limit: usize) -> usize {
    let zombies = take_local_zombies(cpu, limit);
    let drained = zombies.len();
    drop(zombies);
    if drained != 0 {
        super::perf::record_zombie_drain(drained);
        super::perf::record_zombie_drain_full(0, 1, drained);
    }
    drained
}

/// 执行一轮由 CPU0 本地 10ms scheduler tick 驱动的全局维护。
///
/// 该函数只在 idle 栈、IRQ-off 且未持有调度锁时调用。调度 tick 使用可合并
/// 发布位，因此长临界区之后最多补做一轮，不按遗漏 tick 数追赶形成维护风暴。
fn run_boot_housekeeping(cpu: usize, schedule_tick: &mut usize, sched_profile: bool) {
    *schedule_tick = schedule_tick.wrapping_add(1);

    // 每个周期读取一个 UART 字符。RV64 的 SBI ecall 最多按 100Hz 执行，
    // 不再随 context switch 或空队列 busy loop 放大。
    let stage_t0 = sched_profile_start(sched_profile);
    let ch = crate::hal::console_getchar() as u8;
    if ch != 0xFF {
        if crate::trace::check_magic_key(ch, "schedule") {
            // check_magic_key -> dump_from -> shutdown, never returns.
        } else {
            crate::trace::stash_char(ch);
            crate::fs::dev::tty::Teletype::receive_stashed();
        }
    }
    sched_record_stage(
        sched_profile,
        &SCHED_STAGE_CONSOLE_CALLS,
        &SCHED_STAGE_CONSOLE_CYCLES_TOTAL,
        &SCHED_STAGE_CONSOLE_CYCLES_MAX,
        stage_t0,
    );

    // legacy timeout queue 尚未完全由精确 timer 取代；固定在 CPU0 tick 扫描，
    // 保持早期网络等待与旧 wait-IO fallback 的有界唤醒延迟。
    let stage_t0 = sched_profile_start(sched_profile);
    do_wake_expired();
    sched_record_stage(
        sched_profile,
        &SCHED_STAGE_WAKE_EXPIRED_CALLS,
        &SCHED_STAGE_WAKE_EXPIRED_CYCLES_TOTAL,
        &SCHED_STAGE_WAKE_EXPIRED_CYCLES_MAX,
        stage_t0,
    );

    // DeviceStack try_lock 失败只在下一个 scheduler tick 重新提交，避免 poll
    // worker 立即重试并饿死真正的锁持有者。
    NET_INTERFACE.run_deferred_poll_retry();
    if *schedule_tick % BACKGROUND_NET_POLL_INTERVAL == 0 {
        let stage_t0 = sched_profile_start(sched_profile);
        NET_INTERFACE.request_poll();
        sched_record_stage(
            sched_profile,
            &SCHED_STAGE_NET_POLL_CALLS,
            &SCHED_STAGE_NET_POLL_CYCLES_TOTAL,
            &SCHED_STAGE_NET_POLL_CYCLES_MAX,
            stage_t0,
        );
    }

    let rec_t0 = sched_profile_start(sched_profile);
    crate::fs::reclaim::maybe_reclaim_fs_caches();
    sched_record_stage(
        sched_profile,
        &SCHED_STAGE_RECLAIM_CALLS,
        &SCHED_STAGE_RECLAIM_CYCLES_TOTAL,
        &SCHED_STAGE_RECLAIM_CYCLES_MAX,
        rec_t0,
    );
    if sched_profile {
        let rec_dt = sched_rdcycle().saturating_sub(rec_t0);
        SCHED_RECLAIM_CALL_CYCLES_TOTAL.fetch_add(rec_dt, SchedOrdering::Relaxed);
        sched_atomic_max(&SCHED_RECLAIM_CALL_CYCLES_MAX, rec_dt);
    }

    // on_umount 在 registry 锁外执行；每 1.28s 最多处理一个 Dying backend。
    if *schedule_tick % 128 == 0 {
        crate::fs::vfs::drain_one_dying_lifecycle();
    }

    // 每 64 个真实 scheduler tick 扫描一次本地 runqueue 诊断状态。
    if *schedule_tick % 64 == 0 {
        let stage_t0 = sched_profile_start(sched_profile);
        let (_, ready_z, nnice) = super::run_queue::stats(cpu);
        crate::task::perf::record_taskq_queue_lens(
            crate::task::manager::ready_count_fast() as usize,
            crate::task::manager::interruptible_count_fast() as usize,
            ready_z,
            0,
            nnice,
        );
        sched_record_stage(
            sched_profile,
            &SCHED_STAGE_TASKQ_STATS_CALLS,
            &SCHED_STAGE_TASKQ_STATS_CYCLES_TOTAL,
            &SCHED_STAGE_TASKQ_STATS_CYCLES_MAX,
            stage_t0,
        );
    } else {
        crate::task::perf::record_taskq_queue_lens(
            crate::task::manager::ready_count_fast() as usize,
            crate::task::manager::interruptible_count_fast() as usize,
            0,
            0,
            0,
        );
    }

    let stage_t0 = sched_profile_start(sched_profile);
    super::threads::compact_shared_futex();
    sched_record_stage(
        sched_profile,
        &SCHED_STAGE_FUTEX_COMPACT_CALLS,
        &SCHED_STAGE_FUTEX_COMPACT_CYCLES_TOTAL,
        &SCHED_STAGE_FUTEX_COMPACT_CYCLES_MAX,
        stage_t0,
    );
}

/// 运行调度主循环。
///
/// # Semantics
///
/// CPU0 循环执行全局 housekeeping 与本地取任务；AP 进入不触碰共享子系统的
/// 精简循环。两者共用同一个 dispatch/current/context-switch 实现。
///
/// # Locking
///
/// 调用 `__switch` 前必须释放本 CPU processor 锁；被切入任务后该函数直到任务主动
/// `schedule()` 回 idle 才会继续执行。
pub fn run_tasks() -> ! {
    let cpu = crate::smp::cpu_id();
    let task_state = crate::smp::local_task_state();
    crate::smp::mark_local_scheduler_entered();
    // Per-CPU 时间从本地调度器接管 CPU 的时刻开始；AP 等待 scheduler release
    // 的早期启动时间不伪装成用户、系统或 idle CPU 时间。
    task_state.begin_idle_time();
    if cpu != crate::smp::BOOT_CPU_ID {
        run_secondary_scheduler(cpu, task_state);
    }

    let mut schedule_tick = 0usize;
    // 与 AP 一样让 idle 调度边界始终从 IRQ-off 开始。每轮只打开一个短窗口
    // 交付已经 pending 的 hard IRQ，完整维护仍在关中断 idle 栈上执行。
    let _ = crate::hal::local_irq_save();
    loop {
        crate::hal::local_irq_restore(true);
        let irq_was_enabled = crate::hal::local_irq_save();
        debug_assert!(irq_was_enabled);
        // RESCHEDULE 只是一条可合并提示；真正的任务可见性由发送端先入队、
        // 后发 IPI 的 Release 顺序和下面的 runqueue 锁保证。
        let _ = crate::smp::take_reschedule_request();
        let _ = super::run_deferred_timer_work();
        // TCB 的最后一个 Arc 可能在持有进程锁时消失；KernelStack::drop 只把
        // 缓存溢出的 slot 登记到退休队列。此处尚未获取任何调度/子系统锁，
        // 可以安全等待远端 TLB ack，再释放 frame 并归还 slot。
        let _ = crate::hal::reclaim_retired_kernel_stacks(16);
        let sched_profile = sched_profile_enabled();
        if sched_profile {
            SCHED_LOOPS.fetch_add(1, SchedOrdering::Relaxed);
        }
        let loop_t0 = sched_profile_start(sched_profile);
        let housekeeping_due = take_boot_housekeeping();
        if housekeeping_due {
            run_boot_housekeeping(cpu, &mut schedule_tick, sched_profile);
        }
        // 当前任务退出后先进入专用 zombie 队列；切回 idle 后即可安全 drop。
        // 这样避免把不可运行的 TCB 塞进 runqueue 再扫描剔除。
        let stage_t0 = sched_profile_start(sched_profile);
        let _ = drain_local_zombies(cpu, 64);
        sched_record_stage(
            sched_profile,
            &SCHED_STAGE_ZOMBIE_QUEUE_CALLS,
            &SCHED_STAGE_ZOMBIE_QUEUE_CYCLES_TOTAL,
            &SCHED_STAGE_ZOMBIE_QUEUE_CYCLES_MAX,
            stage_t0,
        );
        let stage_t0 = sched_profile_start(sched_profile);
        // 取任务时不持有 processor，避免形成 processor -> TASK_MANAGER 的
        // 嵌套锁顺序；fetch 成功后再单独发布 current slot。
        let next_task = fetch_task(cpu);
        sched_record_stage(
            sched_profile,
            &SCHED_STAGE_FETCH_TASK_CALLS,
            &SCHED_STAGE_FETCH_TASK_CYCLES_TOTAL,
            &SCHED_STAGE_FETCH_TASK_CYCLES_MAX,
            stage_t0,
        );
        if housekeeping_due {
            let stage_t0 = sched_profile_start(sched_profile);
            if let Some((ready_len, interruptible_len)) = super::task_manager_counts() {
                sched_record_queue_sample(ready_len as u64, interruptible_len as u64);
            }
            sched_record_stage(
                sched_profile,
                &SCHED_STAGE_QUEUE_SAMPLE_CALLS,
                &SCHED_STAGE_QUEUE_SAMPLE_CYCLES_TOTAL,
                &SCHED_STAGE_QUEUE_SAMPLE_CYCLES_MAX,
                stage_t0,
            );
        }
        if sched_profile {
            if next_task.is_some() {
                SCHED_FETCH.fetch_add(1, SchedOrdering::Relaxed);
            } else {
                SCHED_IDLE.fetch_add(1, SchedOrdering::Relaxed);
            }
        }
        super::perf::record_schedule_loop(next_task.is_some());
        if let Some(task) = next_task {
            dispatch_task(cpu, task_state, task, sched_profile, loop_t0);
        } else {
            // 没有就绪的任务 → CPU idle
            let stage_t0 = sched_profile_start(sched_profile);
            super::perf::record_scheduler_idle(cpu, true);
            sched_record_stage(
                sched_profile,
                &SCHED_STAGE_IDLE_CALLS,
                &SCHED_STAGE_IDLE_CYCLES_TOTAL,
                &SCHED_STAGE_IDLE_CYCLES_MAX,
                stage_t0,
            );
            sched_record_loop_cycles(sched_profile, loop_t0);
            // 当前仍为 IRQ-off；并发发布者在入队后发送 IPI，本地 one-shot timer
            // 最迟 10ms 到期。两者都会使架构 wait 返回，下一轮先打开 IRQ
            // 交付 handler，因此 check -> wait 窗口不会丢失唤醒。
            crate::hal::cpu_wait_for_interrupt();
        }
    }
}

/// AP 调度循环只接触本地 runqueue/current/zombie、无锁 IPI deferred state。
///
/// AP timer 只驱动本地调度 tick；全局 timeout、console、net、FS 和 futex
/// housekeeping 继续由 CPU0 独占。每 CPU 只回收自己已经切回 idle 的 zombie。
/// 空队列检查与 wait 都在 IRQ-off 窗口内，远程 enqueue 后的 doorbell 或本地 tick
/// 都必定使 wait 返回。
///
/// AP 上的 timer callback 不会进入共享子系统；普通用户任务仍需等待共享 FS/net/
/// driver 审计完成后再解除默认 CPU0 affinity。
fn run_secondary_scheduler(cpu: usize, task_state: &'static CpuTaskState) -> ! {
    let _ = crate::hal::local_irq_save();
    loop {
        // 短暂打开全局 IRQ，使已经 pending 的 IPI 进入 hard handler；随后
        // 立即关中断，在 idle 栈安全点优先处理 STOP 和 deferred reason。
        crate::hal::local_irq_restore(true);
        let irq_was_enabled = crate::hal::local_irq_save();
        debug_assert!(irq_was_enabled);
        let _ = crate::smp::service_secondary_ipi_work();
        // timer hard IRQ 与 IPI 一样只发布无锁状态；在 idle 栈、尚未取得
        // runqueue/processor 锁时推进本地 tick 并重编程 one-shot。
        let _ = super::run_deferred_timer_work();
        // 上一任务已经切回本 CPU idle 栈；只在这里释放本地 zombie Arc，
        // 避免 AP 退出路径再竞争全局 TaskManager 或等待 CPU0 代为回收。
        let _ = drain_local_zombies(cpu, 64);

        // kernel-only AP 任务不参与 CPU0 的 OOM active tracker。优先取本地
        // 任务；本地为空时只向一个 victim 窃取一个 affinity 允许的任务。
        let next_task = super::run_queue::fetch(cpu).or_else(|| super::run_queue::steal(cpu));
        super::perf::record_schedule_loop(next_task.is_some());
        if let Some(task) = next_task {
            dispatch_task(cpu, task_state, task, false, 0);
        } else {
            super::perf::record_task_switch_idle_no_next();
            // 关中断检查到空队列后再 wait；并发发布者先入队后发 IPI，
            // 所以 check→wait 窗口内到达的 doorbell 不会丢失。
            super::perf::record_scheduler_idle(cpu, true);
            crate::hal::cpu_wait_for_interrupt();
        }
    }
}

/// 把已由本地 runqueue claim 的任务发布到 current，并完成一次往返切换。
///
/// CPU0 与 AP 必须共用这个入口，避免两套 current 发布顺序逐渐分叉。
fn dispatch_task(
    cpu: usize,
    task_state: &'static CpuTaskState,
    task: Arc<TaskControlBlock>,
    sched_profile: bool,
    loop_t0: u64,
) {
    if task.is_kernel_only() {
        crate::task::perf::record_task_switch_to_kernel_only();
    }
    #[cfg(all(feature = "boot_la_uboot_dmw", feature = "bringup_trace"))]
    let trace_first_switch =
        cpu == crate::smp::BOOT_CPU_ID && !BOARD_FIRST_TASK_SWITCH.swap(true, Ordering::Relaxed);
    let stage_t0 = sched_profile_start(sched_profile);
    // 先在不持有 processor 锁时更新任务时间并取得上下文指针，避免形成
    // `processor -> task.inner` 的反向锁序。
    let next_task_cx_ptr = {
        let mut task_inner = task.acquire_inner_lock();
        task_inner.update_process_times_schedule_in();
        &task_inner.task_cx as *const TaskContext
    };
    let mut processor = task_state.processor.lock();
    let idle_task_cx_ptr = processor.get_idle_task_cx_ptr();
    task_state.current_pid.store(task.pid(), Ordering::Relaxed);
    task_state
        .current_tid
        .store(task.gettid(), Ordering::Relaxed);
    processor.current = Some(task);
    // 先写入权威 current 槽再发布负载提示；提示的瞬时误差只影响放置质量，
    // 不参与 owner 正确性判断。
    task_state.current_present.store(true, Ordering::Release);
    drop(processor);
    sched_record_stage(
        sched_profile,
        &SCHED_STAGE_SWITCH_PREP_CALLS,
        &SCHED_STAGE_SWITCH_PREP_CYCLES_TOTAL,
        &SCHED_STAGE_SWITCH_PREP_CYCLES_MAX,
        stage_t0,
    );
    if sched_profile {
        SCHED_SWITCHES.fetch_add(1, SchedOrdering::Relaxed);
    }
    sched_record_loop_cycles(sched_profile, loop_t0);
    #[cfg(all(feature = "boot_la_uboot_dmw", feature = "bringup_trace"))]
    if trace_first_switch {
        let (resume_pc, resume_sp) = unsafe { (&*next_task_cx_ptr).bringup_resume_state() };
        println!(
            "[bringup][sched:01] switching idle -> init: pid={} tid={} task_cx={:#x} resume_pc={:#x} expected_pc={:#x} resume_sp={:#x}",
            current_pid(),
            current_tid(),
            next_task_cx_ptr as usize,
            resume_pc,
            crate::hal::trap_return as usize,
            resume_sp
        );
    }
    // 两个上下文都由 current/idle 槽保持存活，且 processor 锁已经释放。
    // 从这一点开始 CPU 将执行 current 任务，不再属于 idle 上下文。
    task_state.end_idle_time();
    unsafe {
        task_state.record_context_switch();
        crate::task::perf::record_context_switch();
        __switch(idle_task_cx_ptr, next_task_cx_ptr);
    }
    // `__switch` 返回说明 CPU 已重新使用 idle 栈；先打开 idle 计时，再做
    // current/MM/zombie 收尾，使 idle 调度上下文的执行时间不会丢失。
    task_state.begin_idle_time();
    finish_current_switch_out(cpu);
    #[cfg(all(feature = "boot_la_uboot_dmw", feature = "bringup_trace"))]
    if trace_first_switch {
        println!("[bringup][sched:02] first init context returned to idle scheduler");
    }
}

/// 清空与本 CPU current 槽位同生命周期的无锁快照。
fn clear_current_task_cache(task_state: &CpuTaskState) {
    task_state.current_pid.store(0, Ordering::Relaxed);
    task_state.current_tid.store(0, Ordering::Relaxed);
    task_state.current_syscall_id.store(0, Ordering::Relaxed);
}

/// 在 idle 栈上收回 current slot，并把上一任务交给调度状态机收尾。
fn finish_current_switch_out(cpu: usize) {
    let task_state = crate::smp::local_task_state();
    // 已经运行在 idle 栈，但 current 仍指向旧任务。先完成 MM 切离屏障，
    // 再改变 current/runqueue owner，使 membarrier 与 PTE 快照不会漏掉它。
    leave_user_vm(cpu);
    let task = {
        let mut processor = task_state.processor.lock();
        clear_current_task_cache(task_state);
        let task = processor
            .take_current()
            .expect("idle resumed without a current task");
        task_state.current_present.store(false, Ordering::Release);
        task
    };
    finish_switch_out(task, cpu);
}

/// 获取当前正在运行任务的 `Arc`。
///
/// # Semantics
///
/// 从本 CPU 的 current 槽位克隆强引用。不再发布裸指针，因此返回的
/// `Arc` 可以安全跨越普通函数调用，但仍不应跨非返回的 context switch。
#[inline(always)]
pub fn current_task() -> Option<Arc<TaskControlBlock>> {
    crate::smp::local_task_state().processor.lock().current()
}

/// panic 诊断使用的非阻塞 current 读取。
///
/// `Err(())` 表示 CPU-local 尚未安装或本 CPU processor 锁已被占用；
/// `Ok(None)` 才表示已经确认 current 槽为空。
pub(crate) fn try_current_task() -> Result<Option<Arc<TaskControlBlock>>, ()> {
    let task_state = crate::smp::try_local_task_state().ok_or(())?;
    let processor = task_state.processor.try_lock().ok_or(())?;
    Ok(processor.current())
}

/// 精确查询指定 CPU 的 current 槽，供 SMP 所有权诊断使用。
pub(crate) fn cpu_has_current(cpu: usize) -> bool {
    crate::smp::task_state(cpu)
        .processor
        .lock()
        .current
        .is_some()
}

/// 返回指定 CPU 的无锁 current 计数，供放置策略计算近似负载。
pub(crate) fn cpu_current_count(cpu: usize) -> usize {
    usize::from(
        crate::smp::task_state(cpu)
            .current_present
            .load(Ordering::Acquire),
    )
}

/// 把当前任务刚结算的执行时间记到它实际运行的本地 CPU。
///
/// 这与 PCB 的批量线程组账户相互独立：PCB 批次可能跨迁移累积，不能拿来
/// 反推 per-CPU 归属；这里必须在每次 trap/schedule 计时时立即累加。
pub(crate) fn account_local_cpu_time(user_us: usize, system_us: usize) {
    let state = crate::smp::local_task_state();
    if user_us != 0 {
        state
            .user_time_us
            .fetch_add(user_us as u64, Ordering::Relaxed);
    }
    if system_us != 0 {
        state
            .system_time_us
            .fetch_add(system_us as u64, Ordering::Relaxed);
    }
}

/// 返回指定逻辑 CPU 的无锁时间快照。
pub(crate) fn cpu_time_snapshot(cpu: usize) -> CpuTimeSnapshot {
    assert!(
        cpu < crate::smp::configured_cpu_count(),
        "CPU time snapshot requested for unconfigured CPU {}",
        cpu
    );
    crate::smp::task_state(cpu).read_cpu_time()
}

/// 取得触发本次用户 trap 的 current 任务，并验证 CPU 所有权。
///
/// current 的 `Arc` 只在 processor 锁内克隆；状态检查在锁外读取原子值，
/// 因此不会把 current 锁带入 syscall、缺页或任务切换路径。每次调用都重新
/// 读取 CPU ID，使未来任务在 syscall 内 yield 后迁移时也校验恢复它的 CPU。
pub(crate) fn current_trap_task() -> Arc<TaskControlBlock> {
    let cpu = crate::smp::cpu_id();
    assert_ne!(
        crate::smp::online_cpu_mask() & (1usize << cpu),
        0,
        "user trap arrived on offline CPU {}",
        cpu
    );
    let task = current_task()
        .unwrap_or_else(|| panic!("user trap arrived without current task on CPU {}", cpu));
    match task.task_status() {
        TaskStatus::Running(owner) if owner == cpu => task,
        status => panic!(
            "user trap owner mismatch: cpu={}, tid={}, status={:?}",
            cpu,
            task.gettid(),
            status
        ),
    }
}

#[inline(always)]
pub fn current_pid() -> usize {
    crate::smp::try_local_task_state()
        .map(|state| state.current_pid.load(Ordering::Relaxed))
        .unwrap_or(0)
}

#[inline(always)]
pub fn current_tid() -> usize {
    crate::smp::try_local_task_state()
        .map(|state| state.current_tid.load(Ordering::Relaxed))
        .unwrap_or(0)
}

/// 当前 exec 任务接管进程 PID 后，同步本 CPU 的无锁诊断快照。
///
/// 身份重键不经过 context switch，因此不能等到下一次 dispatch 才更新。调用者
/// 必须是本 CPU current，且不能持有 registry、线程组或任务管理锁。
pub(crate) fn update_current_tid(task: &TaskControlBlock, old_tid: usize) {
    let task_state = crate::smp::local_task_state();
    let processor = task_state.processor.lock();
    let current = processor
        .current
        .as_ref()
        .expect("exec identity changed without a current task");
    assert!(
        core::ptr::eq(Arc::as_ptr(current), task as *const _),
        "exec identity changed a non-current task"
    );
    assert_eq!(
        task_state.current_tid.load(Ordering::Relaxed),
        old_tid,
        "per-CPU current TID diverged before exec identity change"
    );
    task_state
        .current_tid
        .store(task.gettid(), Ordering::Relaxed);
}

#[inline(always)]
pub fn current_parent_pid() -> usize {
    current_task()
        .map(|task| task.process.parent_pid())
        .unwrap_or(0)
}

#[inline(always)]
pub fn current_pgid() -> usize {
    current_task()
        .map(|task| task.process.getpgid())
        .unwrap_or(0)
}

#[inline(always)]
pub fn current_sid() -> usize {
    current_task()
        .map(|task| task.process.getsid())
        .unwrap_or(0)
}

#[inline(always)]
pub fn current_uid() -> u32 {
    current_task().map(|task| task.uid()).unwrap_or(0)
}

#[inline(always)]
pub fn current_euid() -> u32 {
    current_task().map(|task| task.euid()).unwrap_or(0)
}

#[inline(always)]
pub fn current_suid() -> u32 {
    current_task().map(|task| task.suid()).unwrap_or(0)
}

#[inline(always)]
pub fn current_gid() -> u32 {
    current_task().map(|task| task.gid()).unwrap_or(0)
}

#[inline(always)]
pub fn current_egid() -> u32 {
    current_task().map(|task| task.egid()).unwrap_or(0)
}

#[inline(always)]
pub fn current_sgid() -> u32 {
    current_task().map(|task| task.sgid()).unwrap_or(0)
}

/// 获取当前系统调用名称（用于 OOM 诊断）。
pub fn current_syscall_name() -> &'static str {
    let Some(task_state) = crate::smp::try_local_task_state() else {
        return "<none>";
    };
    match task_state.current_syscall_id.load(Ordering::Relaxed) {
        0 => "<none>",
        id => crate::syscall::syscall_name(id - 1),
    }
}

/// 设置当前系统调用 ID。
///
/// # Semantics
///
/// 默认性能构建不会记录该字段；仅在 `heap_trace` 或 `perf_stats` 构建中写入。
#[inline(always)]
pub fn set_current_syscall_id(id: Option<usize>) {
    if cfg!(any(feature = "heap_trace", feature = "perf_stats")) {
        if let Some(task_state) = crate::smp::try_local_task_state() {
            task_state
                .current_syscall_id
                .store(id.map(|id| id + 1).unwrap_or(0), Ordering::Relaxed);
        }
    }
}

/// 获取当前任务的用户态页表 token。
#[inline(always)]
pub fn try_current_user_token() -> Option<usize> {
    current_task().map(|task| task.process.user_token())
}

/// 获取当前任务的用户态页表 token。
///
/// # Panics
///
/// 当前处理器没有运行任务时 panic。
#[inline(always)]
pub fn current_user_token() -> usize {
    try_current_user_token().unwrap()
}

/// 从当前任务切换回 idle 调度上下文。
///
/// # Semantics
///
/// `switched_task_cx_ptr` 必须指向当前任务的 `TaskContext`，用于保存被切出时
/// 的 callee-saved 寄存器。函数在 idle 再次调度该任务时返回。
pub fn schedule(switched_task_cx_ptr: *mut TaskContext) {
    // `TaskContext` / 切换汇编不保存 sstatus.SIE 或 CRMD.IE。任务可能
    // 从 syscall 受控窗口带着开中断状态进入；必须在获取任何 scheduler
    // 锁之前先快照并关闭，让 idle 始终接管 IRQ-off CPU。
    //
    // idle 调度器在整个调度循环期间保持 IRQ 关闭。当前任务被切走后
    // `__switch` 不会直接恢复其 IRQ 状态；只有当该任务再次被调度回来
    // 并从 `schedule()` 返回时，才通过 `local_irq_restore` 还原之前的
    // 中断状态。
    let irq_was_enabled = crate::hal::local_irq_save();
    // block/yield/exit 可能在再次返回用户态前切走。prepare_current_switch()
    // 已结算并冲刷 CPU 时间，因此这里补做一次 CPU timer 检查。检查函数会
    // 在进入 signal queue/runqueue 前释放 timer 锁，并且临时 Arc 在真正
    // context switch 前释放。
    if let Some(task) = current_task() {
        task.process.check_interval_cpu_timers();
        task.process.check_posix_cpu_timers(&task);
    }
    // idle 上下文必须来自正在执行该任务的同一 CPU；任务迁移只能
    // 发生在下次 dispatch 之前，不能在一次 context switch 中途更换归属。
    let idle_task_cx_ptr = crate::smp::local_task_state()
        .processor
        .lock()
        .get_idle_task_cx_ptr();
    if sched_profile_enabled() {
        SCHED_SWITCHES.fetch_add(1, SchedOrdering::Relaxed);
    }
    // Safety: `switched_task_cx_ptr` is provided by the currently running TCB
    // and `idle_task_cx_ptr` points into this CPU's idle context. The local
    // processor lock is not held across the assembly context switch.
    unsafe {
        crate::smp::local_task_state().record_context_switch();
        crate::task::perf::record_context_switch();
        __switch(switched_task_cx_ptr, idle_task_cx_ptr);
    }
    // 只有原任务被再次调度时 `__switch` 才会返回。idle 切回任务时
    // 仍为关中断，因此这里才恢复该任务自己的入口快照。
    crate::hal::local_irq_restore(irq_was_enabled);
}

// ── sched debug profile counters ────────────────────────────────────────
use core::sync::atomic::{AtomicU64, Ordering as SchedOrdering};

static SCHED_PROFILE_ENABLED: AtomicBool = AtomicBool::new(false);
static SCHED_LOOPS: AtomicU64 = AtomicU64::new(0);
static SCHED_FETCH: AtomicU64 = AtomicU64::new(0);
static SCHED_IDLE: AtomicU64 = AtomicU64::new(0);
static SCHED_SWITCHES: AtomicU64 = AtomicU64::new(0);
static SCHED_TIMER_INTS: AtomicU64 = AtomicU64::new(0);
static SCHED_LOOP_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_LOOP_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_RECLAIM_CALL_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_RECLAIM_CALL_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_READY_LEN_SUM: AtomicU64 = AtomicU64::new(0);
static SCHED_READY_LEN_SAMPLES: AtomicU64 = AtomicU64::new(0);
static SCHED_READY_LEN_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_INTERRUPTIBLE_LEN_SUM: AtomicU64 = AtomicU64::new(0);
static SCHED_INTERRUPTIBLE_LEN_SAMPLES: AtomicU64 = AtomicU64::new(0);
static SCHED_INTERRUPTIBLE_LEN_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_CONSOLE_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_CONSOLE_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_CONSOLE_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_WAKE_EXPIRED_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_WAKE_EXPIRED_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_WAKE_EXPIRED_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_NET_POLL_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_NET_POLL_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_NET_POLL_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_RECLAIM_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_RECLAIM_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_RECLAIM_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_ZOMBIE_QUEUE_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_ZOMBIE_QUEUE_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_ZOMBIE_QUEUE_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_TASKQ_STATS_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_TASKQ_STATS_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_TASKQ_STATS_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_FUTEX_COMPACT_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_FUTEX_COMPACT_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_FUTEX_COMPACT_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_FETCH_TASK_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_FETCH_TASK_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_FETCH_TASK_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_QUEUE_SAMPLE_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_QUEUE_SAMPLE_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_QUEUE_SAMPLE_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_SWITCH_PREP_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_SWITCH_PREP_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_SWITCH_PREP_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_IDLE_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_IDLE_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_STAGE_IDLE_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_TIMER_TRAP_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_TIMER_TRAP_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_TIMER_HANDLER_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_TIMER_HANDLER_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_PROGRAM_TIMER_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_PROGRAM_TIMER_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_PROGRAM_TIMER_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);
static SCHED_SBI_SET_TIMER_CALLS: AtomicU64 = AtomicU64::new(0);
static SCHED_SBI_SET_TIMER_CYCLES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SCHED_SBI_SET_TIMER_CYCLES_MAX: AtomicU64 = AtomicU64::new(0);

#[inline(always)]
fn sched_profile_enabled() -> bool {
    SCHED_PROFILE_ENABLED.load(SchedOrdering::Relaxed)
}

#[inline(always)]
fn sched_rdcycle() -> u64 {
    #[cfg(target_arch = "riscv64")]
    {
        let cycles: usize;
        // Safety: `rdcycle` only reads the architectural cycle counter and
        // writes the output register.
        unsafe {
            core::arch::asm!("rdcycle {}", out(reg) cycles);
        }
        cycles as u64
    }
    #[cfg(target_arch = "loongarch64")]
    {
        let lo: usize;
        let hi: usize;
        // Safety: `rdtime.d` only reads the architectural timer and writes the
        // two output registers.
        unsafe {
            core::arch::asm!("rdtime.d {}, {}", out(reg) lo, out(reg) hi);
        }
        lo as u64
    }
    #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
    {
        0
    }
}
fn sched_atomic_max(slot: &AtomicU64, v: u64) {
    let mut cur = slot.load(SchedOrdering::Relaxed);
    while v > cur {
        match slot.compare_exchange_weak(cur, v, SchedOrdering::Relaxed, SchedOrdering::Relaxed) {
            Ok(_) => break,
            Err(n) => cur = n,
        }
    }
}

#[inline(always)]
fn sched_profile_start(enabled: bool) -> u64 {
    if enabled {
        sched_rdcycle()
    } else {
        0
    }
}

#[inline(always)]
pub(crate) fn sched_profile_cycle_start() -> u64 {
    sched_profile_start(sched_profile_enabled())
}

#[inline(always)]
fn sched_record_cycles(enabled: bool, total: &AtomicU64, max: &AtomicU64, start: u64) {
    if !enabled {
        return;
    }
    let dt = sched_rdcycle().saturating_sub(start);
    total.fetch_add(dt, SchedOrdering::Relaxed);
    sched_atomic_max(max, dt);
}

#[inline(always)]
fn sched_record_stage(
    enabled: bool,
    calls: &AtomicU64,
    total: &AtomicU64,
    max: &AtomicU64,
    start: u64,
) {
    if !enabled {
        return;
    }
    calls.fetch_add(1, SchedOrdering::Relaxed);
    sched_record_cycles(true, total, max, start);
}

#[inline(always)]
fn sched_record_loop_cycles(enabled: bool, start: u64) {
    sched_record_cycles(
        enabled,
        &SCHED_LOOP_CYCLES_TOTAL,
        &SCHED_LOOP_CYCLES_MAX,
        start,
    );
}

#[inline(always)]
fn sched_record_queue_sample(ready_len: u64, interruptible_len: u64) {
    if !sched_profile_enabled() {
        return;
    }
    SCHED_READY_LEN_SUM.fetch_add(ready_len, SchedOrdering::Relaxed);
    SCHED_READY_LEN_SAMPLES.fetch_add(1, SchedOrdering::Relaxed);
    sched_atomic_max(&SCHED_READY_LEN_MAX, ready_len);
    SCHED_INTERRUPTIBLE_LEN_SUM.fetch_add(interruptible_len, SchedOrdering::Relaxed);
    SCHED_INTERRUPTIBLE_LEN_SAMPLES.fetch_add(1, SchedOrdering::Relaxed);
    sched_atomic_max(&SCHED_INTERRUPTIBLE_LEN_MAX, interruptible_len);
}

#[inline(always)]
pub(crate) fn record_sched_timer_interrupt() {
    if !sched_profile_enabled() {
        return;
    }
    SCHED_TIMER_INTS.fetch_add(1, SchedOrdering::Relaxed);
}

#[inline(always)]
pub(crate) fn record_sched_timer_trap_cycles(start: u64) {
    sched_record_cycles(
        sched_profile_enabled(),
        &SCHED_TIMER_TRAP_CYCLES_TOTAL,
        &SCHED_TIMER_TRAP_CYCLES_MAX,
        start,
    );
}

#[inline(always)]
pub(crate) fn record_sched_timer_handler_cycles(start: u64) {
    sched_record_cycles(
        sched_profile_enabled(),
        &SCHED_TIMER_HANDLER_CYCLES_TOTAL,
        &SCHED_TIMER_HANDLER_CYCLES_MAX,
        start,
    );
}

#[inline(always)]
pub(crate) fn record_sched_program_timer_cycles(start: u64) {
    if !sched_profile_enabled() {
        return;
    }
    SCHED_PROGRAM_TIMER_CALLS.fetch_add(1, SchedOrdering::Relaxed);
    sched_record_cycles(
        true,
        &SCHED_PROGRAM_TIMER_CYCLES_TOTAL,
        &SCHED_PROGRAM_TIMER_CYCLES_MAX,
        start,
    );
}

#[inline(always)]
pub(crate) fn record_sched_sbi_set_timer_cycles(start: u64) {
    if !sched_profile_enabled() {
        return;
    }
    SCHED_SBI_SET_TIMER_CALLS.fetch_add(1, SchedOrdering::Relaxed);
    sched_record_cycles(
        true,
        &SCHED_SBI_SET_TIMER_CYCLES_TOTAL,
        &SCHED_SBI_SET_TIMER_CYCLES_MAX,
        start,
    );
}

#[inline(always)]
fn reset_stage(calls: &AtomicU64, total: &AtomicU64, max: &AtomicU64) {
    calls.store(0, SchedOrdering::Relaxed);
    total.store(0, SchedOrdering::Relaxed);
    max.store(0, SchedOrdering::Relaxed);
}

pub fn reset_sched_profile() {
    SCHED_PROFILE_ENABLED.store(false, SchedOrdering::Relaxed);
    SCHED_LOOPS.store(0, SchedOrdering::Relaxed);
    SCHED_FETCH.store(0, SchedOrdering::Relaxed);
    SCHED_IDLE.store(0, SchedOrdering::Relaxed);
    SCHED_SWITCHES.store(0, SchedOrdering::Relaxed);
    SCHED_TIMER_INTS.store(0, SchedOrdering::Relaxed);
    SCHED_LOOP_CYCLES_TOTAL.store(0, SchedOrdering::Relaxed);
    SCHED_LOOP_CYCLES_MAX.store(0, SchedOrdering::Relaxed);
    SCHED_RECLAIM_CALL_CYCLES_TOTAL.store(0, SchedOrdering::Relaxed);
    SCHED_RECLAIM_CALL_CYCLES_MAX.store(0, SchedOrdering::Relaxed);
    SCHED_READY_LEN_SUM.store(0, SchedOrdering::Relaxed);
    SCHED_READY_LEN_SAMPLES.store(0, SchedOrdering::Relaxed);
    SCHED_READY_LEN_MAX.store(0, SchedOrdering::Relaxed);
    SCHED_INTERRUPTIBLE_LEN_SUM.store(0, SchedOrdering::Relaxed);
    SCHED_INTERRUPTIBLE_LEN_SAMPLES.store(0, SchedOrdering::Relaxed);
    SCHED_INTERRUPTIBLE_LEN_MAX.store(0, SchedOrdering::Relaxed);
    reset_stage(
        &SCHED_STAGE_CONSOLE_CALLS,
        &SCHED_STAGE_CONSOLE_CYCLES_TOTAL,
        &SCHED_STAGE_CONSOLE_CYCLES_MAX,
    );
    reset_stage(
        &SCHED_STAGE_WAKE_EXPIRED_CALLS,
        &SCHED_STAGE_WAKE_EXPIRED_CYCLES_TOTAL,
        &SCHED_STAGE_WAKE_EXPIRED_CYCLES_MAX,
    );
    reset_stage(
        &SCHED_STAGE_NET_POLL_CALLS,
        &SCHED_STAGE_NET_POLL_CYCLES_TOTAL,
        &SCHED_STAGE_NET_POLL_CYCLES_MAX,
    );
    reset_stage(
        &SCHED_STAGE_RECLAIM_CALLS,
        &SCHED_STAGE_RECLAIM_CYCLES_TOTAL,
        &SCHED_STAGE_RECLAIM_CYCLES_MAX,
    );
    reset_stage(
        &SCHED_STAGE_ZOMBIE_QUEUE_CALLS,
        &SCHED_STAGE_ZOMBIE_QUEUE_CYCLES_TOTAL,
        &SCHED_STAGE_ZOMBIE_QUEUE_CYCLES_MAX,
    );
    reset_stage(
        &SCHED_STAGE_TASKQ_STATS_CALLS,
        &SCHED_STAGE_TASKQ_STATS_CYCLES_TOTAL,
        &SCHED_STAGE_TASKQ_STATS_CYCLES_MAX,
    );
    reset_stage(
        &SCHED_STAGE_FUTEX_COMPACT_CALLS,
        &SCHED_STAGE_FUTEX_COMPACT_CYCLES_TOTAL,
        &SCHED_STAGE_FUTEX_COMPACT_CYCLES_MAX,
    );
    reset_stage(
        &SCHED_STAGE_FETCH_TASK_CALLS,
        &SCHED_STAGE_FETCH_TASK_CYCLES_TOTAL,
        &SCHED_STAGE_FETCH_TASK_CYCLES_MAX,
    );
    reset_stage(
        &SCHED_STAGE_QUEUE_SAMPLE_CALLS,
        &SCHED_STAGE_QUEUE_SAMPLE_CYCLES_TOTAL,
        &SCHED_STAGE_QUEUE_SAMPLE_CYCLES_MAX,
    );
    reset_stage(
        &SCHED_STAGE_SWITCH_PREP_CALLS,
        &SCHED_STAGE_SWITCH_PREP_CYCLES_TOTAL,
        &SCHED_STAGE_SWITCH_PREP_CYCLES_MAX,
    );
    reset_stage(
        &SCHED_STAGE_IDLE_CALLS,
        &SCHED_STAGE_IDLE_CYCLES_TOTAL,
        &SCHED_STAGE_IDLE_CYCLES_MAX,
    );
    SCHED_TIMER_TRAP_CYCLES_TOTAL.store(0, SchedOrdering::Relaxed);
    SCHED_TIMER_TRAP_CYCLES_MAX.store(0, SchedOrdering::Relaxed);
    SCHED_TIMER_HANDLER_CYCLES_TOTAL.store(0, SchedOrdering::Relaxed);
    SCHED_TIMER_HANDLER_CYCLES_MAX.store(0, SchedOrdering::Relaxed);
    reset_stage(
        &SCHED_PROGRAM_TIMER_CALLS,
        &SCHED_PROGRAM_TIMER_CYCLES_TOTAL,
        &SCHED_PROGRAM_TIMER_CYCLES_MAX,
    );
    reset_stage(
        &SCHED_SBI_SET_TIMER_CALLS,
        &SCHED_SBI_SET_TIMER_CYCLES_TOTAL,
        &SCHED_SBI_SET_TIMER_CYCLES_MAX,
    );
    SCHED_PROFILE_ENABLED.store(true, SchedOrdering::Relaxed);
}

pub fn disable_sched_profile() {
    SCHED_PROFILE_ENABLED.store(false, SchedOrdering::Relaxed);
}

fn dump_stage(label: &str, calls: &AtomicU64, total: &AtomicU64, max: &AtomicU64) {
    println!(
        "sched_stage_{} calls={} cycles_total={} cycles_max={}",
        label,
        calls.load(SchedOrdering::Relaxed),
        total.load(SchedOrdering::Relaxed),
        max.load(SchedOrdering::Relaxed)
    );
}

pub fn dump_sched_profile(label: &str) {
    println!("[sched_profile] {}", label);
    println!(
        "sched_profile enabled={}",
        SCHED_PROFILE_ENABLED.load(SchedOrdering::Relaxed) as usize
    );
    println!(
        "sched loops={} fetch={} idle={} switches={} timer_ints={}",
        SCHED_LOOPS.load(SchedOrdering::Relaxed),
        SCHED_FETCH.load(SchedOrdering::Relaxed),
        SCHED_IDLE.load(SchedOrdering::Relaxed),
        SCHED_SWITCHES.load(SchedOrdering::Relaxed),
        SCHED_TIMER_INTS.load(SchedOrdering::Relaxed)
    );
    println!("sched loop_cycles_total={} loop_cycles_max={} reclaim_call_cycles_total={} reclaim_call_cycles_max={}",
        SCHED_LOOP_CYCLES_TOTAL.load(SchedOrdering::Relaxed), SCHED_LOOP_CYCLES_MAX.load(SchedOrdering::Relaxed),
        SCHED_RECLAIM_CALL_CYCLES_TOTAL.load(SchedOrdering::Relaxed), SCHED_RECLAIM_CALL_CYCLES_MAX.load(SchedOrdering::Relaxed));
    let ready_samples = SCHED_READY_LEN_SAMPLES.load(SchedOrdering::Relaxed);
    let interruptible_samples = SCHED_INTERRUPTIBLE_LEN_SAMPLES.load(SchedOrdering::Relaxed);
    println!("sched ready_len_sum={} ready_len_samples={} ready_len_max={} ready_len_avg={} interruptible_len_sum={} interruptible_len_samples={} interruptible_len_max={} interruptible_len_avg={}",
        SCHED_READY_LEN_SUM.load(SchedOrdering::Relaxed),
        ready_samples,
        SCHED_READY_LEN_MAX.load(SchedOrdering::Relaxed),
        if ready_samples > 0 {
            SCHED_READY_LEN_SUM.load(SchedOrdering::Relaxed) / ready_samples
        } else {
            0
        },
        SCHED_INTERRUPTIBLE_LEN_SUM.load(SchedOrdering::Relaxed),
        interruptible_samples,
        SCHED_INTERRUPTIBLE_LEN_MAX.load(SchedOrdering::Relaxed),
        if interruptible_samples > 0 {
            SCHED_INTERRUPTIBLE_LEN_SUM.load(SchedOrdering::Relaxed) / interruptible_samples
        } else {
            0
        });
    dump_stage(
        "console",
        &SCHED_STAGE_CONSOLE_CALLS,
        &SCHED_STAGE_CONSOLE_CYCLES_TOTAL,
        &SCHED_STAGE_CONSOLE_CYCLES_MAX,
    );
    dump_stage(
        "wake_expired",
        &SCHED_STAGE_WAKE_EXPIRED_CALLS,
        &SCHED_STAGE_WAKE_EXPIRED_CYCLES_TOTAL,
        &SCHED_STAGE_WAKE_EXPIRED_CYCLES_MAX,
    );
    dump_stage(
        "net_poll",
        &SCHED_STAGE_NET_POLL_CALLS,
        &SCHED_STAGE_NET_POLL_CYCLES_TOTAL,
        &SCHED_STAGE_NET_POLL_CYCLES_MAX,
    );
    dump_stage(
        "reclaim",
        &SCHED_STAGE_RECLAIM_CALLS,
        &SCHED_STAGE_RECLAIM_CYCLES_TOTAL,
        &SCHED_STAGE_RECLAIM_CYCLES_MAX,
    );
    dump_stage(
        "zombie_queue",
        &SCHED_STAGE_ZOMBIE_QUEUE_CALLS,
        &SCHED_STAGE_ZOMBIE_QUEUE_CYCLES_TOTAL,
        &SCHED_STAGE_ZOMBIE_QUEUE_CYCLES_MAX,
    );
    dump_stage(
        "taskq_stats",
        &SCHED_STAGE_TASKQ_STATS_CALLS,
        &SCHED_STAGE_TASKQ_STATS_CYCLES_TOTAL,
        &SCHED_STAGE_TASKQ_STATS_CYCLES_MAX,
    );
    dump_stage(
        "futex_compact",
        &SCHED_STAGE_FUTEX_COMPACT_CALLS,
        &SCHED_STAGE_FUTEX_COMPACT_CYCLES_TOTAL,
        &SCHED_STAGE_FUTEX_COMPACT_CYCLES_MAX,
    );
    dump_stage(
        "fetch_task",
        &SCHED_STAGE_FETCH_TASK_CALLS,
        &SCHED_STAGE_FETCH_TASK_CYCLES_TOTAL,
        &SCHED_STAGE_FETCH_TASK_CYCLES_MAX,
    );
    dump_stage(
        "queue_sample",
        &SCHED_STAGE_QUEUE_SAMPLE_CALLS,
        &SCHED_STAGE_QUEUE_SAMPLE_CYCLES_TOTAL,
        &SCHED_STAGE_QUEUE_SAMPLE_CYCLES_MAX,
    );
    dump_stage(
        "switch_prep",
        &SCHED_STAGE_SWITCH_PREP_CALLS,
        &SCHED_STAGE_SWITCH_PREP_CYCLES_TOTAL,
        &SCHED_STAGE_SWITCH_PREP_CYCLES_MAX,
    );
    dump_stage(
        "idle",
        &SCHED_STAGE_IDLE_CALLS,
        &SCHED_STAGE_IDLE_CYCLES_TOTAL,
        &SCHED_STAGE_IDLE_CYCLES_MAX,
    );
    println!(
        "sched_timer trap_cycles_total={} trap_cycles_max={} handler_cycles_total={} handler_cycles_max={} program_timer_calls={} program_timer_cycles_total={} program_timer_cycles_max={} sbi_set_timer_calls={} sbi_set_timer_cycles_total={} sbi_set_timer_cycles_max={}",
        SCHED_TIMER_TRAP_CYCLES_TOTAL.load(SchedOrdering::Relaxed),
        SCHED_TIMER_TRAP_CYCLES_MAX.load(SchedOrdering::Relaxed),
        SCHED_TIMER_HANDLER_CYCLES_TOTAL.load(SchedOrdering::Relaxed),
        SCHED_TIMER_HANDLER_CYCLES_MAX.load(SchedOrdering::Relaxed),
        SCHED_PROGRAM_TIMER_CALLS.load(SchedOrdering::Relaxed),
        SCHED_PROGRAM_TIMER_CYCLES_TOTAL.load(SchedOrdering::Relaxed),
        SCHED_PROGRAM_TIMER_CYCLES_MAX.load(SchedOrdering::Relaxed),
        SCHED_SBI_SET_TIMER_CALLS.load(SchedOrdering::Relaxed),
        SCHED_SBI_SET_TIMER_CYCLES_TOTAL.load(SchedOrdering::Relaxed),
        SCHED_SBI_SET_TIMER_CYCLES_MAX.load(SchedOrdering::Relaxed)
    );
}
