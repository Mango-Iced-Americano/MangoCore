//! 任务调度队列、等待队列和内核定时器。
//!
//! runnable 任务由 `PerCpu` 内的 `RunQueue` 管理；本模块保留 interruptible、
//! timer 等全局 registry。终态任务由退出 CPU 的 idle 回收队列持有；`WaitQueue`
//! 为文件、futex、信号和计时器
//! 路径提供条件等待；`KernelTimerQueue` 驱动超时唤醒与定时任务。
//!
//! # Locking
//!
//! `TASK_MANAGER` 只保护全局 registry。Blocked 唤醒与批量移除按
//! `TASK_MANAGER -> 单个 RunQueue` 取锁；任何路径都不得反向取锁或同时持有
//! 两个 runqueue。可能触发 TCB/PCB 析构或用户内存访问的操作必须在释放这些
//! 锁后执行。

use core::cmp::Ordering;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering as AtomicOrdering};

#[cfg(feature = "oom_handler")]
use crate::config::SYSTEM_TASK_LIMIT;
use alloc::vec::Vec;

use crate::hal::{local_irq_restore, local_irq_save};
use crate::syscall::errno::EAGAIN;
use crate::timer::TimeSpec;

use super::task::{RemoteAffinityRequest, RemoteAffinityState};
use super::{
    block_current_and_run_next_checked, block_current_and_run_next_with_lock_checked, current_task,
    discard_non_actionable_unblocked_signals, has_actionable_signal,
    signal::{has_waited_signal, queue_kernel_process_signal, wake_process_signal_waiter, Signals},
    ProcessControlBlock, TaskControlBlock, TaskStatus,
};
use crate::utils::error::SyscallErr;
use alloc::collections::{BinaryHeap, VecDeque};
use alloc::sync::{Arc, Weak};
use lazy_static::*;
use spin::Mutex;

#[cfg(feature = "oom_handler")]
/// OOM handler 使用的任务活跃位图。
pub struct ActiveTracker {
    /// 存储激活状态的位图
    bitmap: Vec<u64>,
}

#[cfg(feature = "oom_handler")]
#[allow(unused)]
impl ActiveTracker {
    /// 位图初始覆盖的任务数量。
    pub const DEFAULT_SIZE: usize = SYSTEM_TASK_LIMIT;

    /// 创建空活跃位图。
    pub fn new() -> Self {
        let len = (Self::DEFAULT_SIZE + 63) / 64;
        let mut bitmap = Vec::with_capacity(len);
        bitmap.resize(len, 0);
        Self { bitmap }
    }

    /// 确保位图能容纳指定 TID。
    pub fn ensure_capacity(&mut self, tid: usize) {
        let word = tid / 64;
        if word >= self.bitmap.len() {
            self.bitmap.resize(word + 1, 0);
        }
    }

    /// 检查指定 TID 是否被标记为活跃。
    pub fn check_active(&self, tid: usize) -> bool {
        let word = tid / 64;
        if word >= self.bitmap.len() {
            return false;
        }
        (self.bitmap[word] & (1 << (tid % 64))) != 0
    }

    /// 检查指定 TID 是否未被标记为活跃。
    pub fn check_inactive(&self, tid: usize) -> bool {
        !self.check_active(tid)
    }

    /// 标记指定 TID 为活跃。
    pub fn mark_active(&mut self, tid: usize) {
        self.ensure_capacity(tid);
        self.bitmap[tid / 64] |= 1 << (tid % 64)
    }

    /// 清除指定 TID 的活跃标记。
    pub fn mark_inactive(&mut self, tid: usize) {
        let word = tid / 64;
        if word >= self.bitmap.len() {
            return;
        }
        self.bitmap[word] &= !(1 << (tid % 64))
    }
}

#[cfg(feature = "oom_handler")]
/// 全局等待与 OOM registry。
pub struct TaskManager {
    /// 可中断睡眠任务队列。
    pub interruptible_queue: VecDeque<Arc<TaskControlBlock>>,
    /// 任务激活状态跟踪器，用于跟踪任务的激活状态，并在OOM时释放内存
    pub active_tracker: ActiveTracker,
}

#[cfg(not(feature = "oom_handler"))]
/// 全局等待 registry。
pub struct TaskManager {
    /// 可中断睡眠任务队列。
    pub interruptible_queue: VecDeque<Arc<TaskControlBlock>>,
}

fn task_ptr_eq(left: &Arc<TaskControlBlock>, right: &Arc<TaskControlBlock>) -> bool {
    Arc::as_ptr(left) == Arc::as_ptr(right)
}

static INTERRUPTIBLE_TASK_COUNT: AtomicUsize = AtomicUsize::new(0);

fn add_interruptible_count() {
    INTERRUPTIBLE_TASK_COUNT.fetch_add(1, AtomicOrdering::Relaxed);
}

fn sub_interruptible_count(count: usize) {
    if count != 0 {
        let _ = INTERRUPTIBLE_TASK_COUNT.fetch_update(
            AtomicOrdering::Relaxed,
            AtomicOrdering::Relaxed,
            |value| Some(value.saturating_sub(count)),
        );
    }
}

/// 无锁汇总所有 per-CPU runqueue 的近似长度。
pub(crate) fn ready_count_fast() -> u16 {
    super::run_queue::total_count_fast().min(u16::MAX as usize) as u16
}

/// 无锁读取 interruptible 队列计数的近似值。
pub(crate) fn interruptible_count_fast() -> u16 {
    INTERRUPTIBLE_TASK_COUNT
        .load(AtomicOrdering::Relaxed)
        .min(u16::MAX as usize) as u16
}

/// 全局等待与定时器 registry。
impl TaskManager {
    #[cfg(feature = "oom_handler")]
    /// 构造函数
    pub fn new() -> Self {
        Self {
            interruptible_queue: VecDeque::new(),
            active_tracker: ActiveTracker::new(),
        }
    }
    #[cfg(not(feature = "oom_handler"))]
    pub fn new() -> Self {
        Self {
            interruptible_queue: VecDeque::new(),
        }
    }
    /// 添加一个任务到可中断队列。
    pub fn begin_interruptible_sleep(&mut self, task: Arc<TaskControlBlock>) {
        // 固定锁序：TASK_MANAGER -> remote_affinity_request。请求槽与
        // Running -> Blocking 在同一临界区完成，远程写侧不会错过 block。
        let mut request_slot = task.remote_affinity_request.lock();
        let current = task.task_status();
        let TaskStatus::Running(cpu) = current else {
            task.fail_sched_invariant(
                "block current task",
                TaskStatus::Running(crate::smp::cpu_id()),
                current,
                TaskStatus::Blocking(crate::smp::cpu_id()),
            );
        };
        if cpu != crate::smp::cpu_id() {
            task.fail_sched_invariant(
                "block current task on owner cpu",
                TaskStatus::Running(crate::smp::cpu_id()),
                current,
                TaskStatus::Blocking(crate::smp::cpu_id()),
            );
        }
        // Blocking 表示“已登记睡眠意图但仍在 CPU 上”。只有真正切回 idle 后
        // 才能提交为 Blocked，从而关闭登记与 context switch 之间的丢唤醒窗口。
        task.require_sched_transition(current, TaskStatus::Blocking(cpu), "block current task");
        self.interruptible_queue.push_back(task.clone());
        crate::task::perf::record_taskq_add_interruptible();
        add_interruptible_count();
        let canceled = request_slot.take();
        drop(request_slot);
        // complete() 只是原子发布，不唤醒 waitqueue，因此可在
        // TASK_MANAGER 仍持有时通知请求方按 Blocking/Blocked 新状态重试。
        if let Some(request) = canceled {
            request.complete(RemoteAffinityState::Retry);
        }
    }
    /// 从可中断队列中删除一个任务。
    pub fn remove_interruptible(&mut self, task: &Arc<TaskControlBlock>) -> bool {
        if self
            .interruptible_queue
            .front()
            .map(|task_in_queue| task_ptr_eq(task_in_queue, task))
            .unwrap_or(false)
        {
            self.interruptible_queue.pop_front();
            sub_interruptible_count(1);
            return true;
        }
        if self
            .interruptible_queue
            .back()
            .map(|task_in_queue| task_ptr_eq(task_in_queue, task))
            .unwrap_or(false)
        {
            self.interruptible_queue.pop_back();
            sub_interruptible_count(1);
            return true;
        }
        let old_len = self.interruptible_queue.len();
        self.interruptible_queue
            .retain(|task_in_queue| !task_ptr_eq(task_in_queue, task));
        let removed = old_len - self.interruptible_queue.len();
        sub_interruptible_count(removed);
        removed != 0
    }
    fn enqueue_ready_batch(&mut self, tasks: Vec<Arc<TaskControlBlock>>) -> (usize, usize) {
        let mut count = 0;
        let mut targets = 0usize;
        for task in tasks.into_iter().rev() {
            match self.try_wake_interruptible(task) {
                Ok(target) => {
                    count += 1;
                    if let Some(cpu) = target {
                        targets |= 1usize << cpu;
                    }
                }
                Err(WaitQueueError::AlreadyWaken) => {}
            }
        }
        (count, targets)
    }
    /// 可中断队列中任务数量
    pub fn interruptible_count(&self) -> u16 {
        self.interruptible_queue.len() as u16
    }
    /// 尝试将任务从 interruptible registry 移动到目标 CPU 的 runqueue。
    ///
    /// `Blocking(cpu) -> Running(cpu)` 表示唤醒抢在任务切离 CPU 前发生，不需要
    /// 发布 runqueue；`Blocked -> Queued(target)` 返回目标 CPU，供外层解锁后
    /// 发送 RESCHEDULE。重复唤醒不会再次入队。
    ///
    /// # Errors
    ///
    /// 任务已经运行、排队或进入终态时返回 `WaitQueueError::AlreadyWaken`。
    ///
    /// # Locking
    ///
    /// 调用方已持有 `TASK_MANAGER` 锁；不得在外部提前修改调度状态。
    fn try_wake_interruptible(
        &mut self,
        task: Arc<TaskControlBlock>,
    ) -> Result<Option<usize>, WaitQueueError> {
        loop {
            match task.task_status() {
                TaskStatus::Blocking(cpu) => {
                    // 任务尚未切离 CPU：撤销阻塞意图即可，不能把仍在运行的
                    // kernel stack 提前放进 ready queue。
                    if task
                        .try_sched_transition(TaskStatus::Blocking(cpu), TaskStatus::Running(cpu))
                        .is_err()
                    {
                        continue;
                    }
                    if !self.remove_interruptible(&task) {
                        panic!(
                            "woken Blocking task is absent from interruptible registry: tid={}",
                            task.gettid()
                        );
                    }
                    crate::task::perf::record_wake_local();
                    return Ok(None);
                }
                TaskStatus::Blocked => {
                    let target_cpu = select_wake_cpu(&task);
                    if !self.remove_interruptible(&task) {
                        panic!(
                            "woken Blocked task is absent from interruptible registry: tid={}",
                            task.gettid()
                        );
                    }
                    // 固定锁序：TASK_MANAGER -> 一个目标 RunQueue。状态 CAS 与
                    // runnable 容器插入由 runqueue 入口在同一锁域提交。
                    super::run_queue::enqueue_woken(task, target_cpu);
                    if target_cpu == crate::smp::cpu_id() {
                        crate::task::perf::record_wake_local();
                    } else {
                        crate::task::perf::record_wake_remote();
                    }
                    return Ok(Some(target_cpu));
                }
                TaskStatus::Queued(_) | TaskStatus::Migrating | TaskStatus::Running(_) => {
                    crate::task::perf::record_taskq_dup_enqueue();
                    return Err(WaitQueueError::AlreadyWaken);
                }
                TaskStatus::New | TaskStatus::Zombie => {
                    return Err(WaitQueueError::AlreadyWaken);
                }
            }
        }
    }
    #[allow(unused)]
    /// 打印 interruptible 队列中的任务 ID，仅供诊断。
    pub fn show_interruptible(&self) {
        self.interruptible_queue.iter().for_each(|task| {
            log::error!(
                "[show_interruptible] tid: {}, pid: {}",
                task.gettid(),
                task.pid()
            );
        })
    }
}

/// 从任务允许且可调度的 CPU 中优先复用最近运行位置。
///
/// Blocked 不拥有 runqueue；本函数在取得目标队列锁前完成选择，因此不会
/// 同时持有两个 runqueue。最近 CPU 负载没有明显偏高时保留 locality，
/// 否则转向允许集合中的最低负载 CPU。
fn select_wake_cpu(task: &TaskControlBlock) -> usize {
    let last_cpu = task.last_cpu();
    let target = super::run_queue::select_runnable_cpu(task.cpus_allowed(), Some(last_cpu));
    let load = super::run_queue::nr_running(target)
        + super::processor::cpu_current_count(target);
    crate::task::perf::record_wake_selection(target == last_cpu, load == 0);
    target
}

/// 在所有调度容器锁释放后敲响远程 doorbell。
fn notify_wake_targets(targets: usize) {
    let remote = targets & crate::smp::online_cpu_mask() & !(1usize << crate::smp::cpu_id());
    if remote != 0 {
        crate::smp::request_reschedule_mask(remote).unwrap_or_else(|error| {
            panic!("failed to reschedule woken CPUs {:#x}: {}", remote, error)
        });
    }
}

fn enqueue_ready_batch(tasks: Vec<Arc<TaskControlBlock>>) -> usize {
    let (count, targets) = TASK_MANAGER.lock().enqueue_ready_batch(tasks);
    notify_wake_targets(targets);
    count
}

/// 更新 owner runqueue 中任务的 nice 快速路径计数。
pub fn update_ready_nice(task: &Arc<TaskControlBlock>, old_nice: i32, new_nice: i32) {
    super::run_queue::update_nice(task, old_nice, new_nice);
}

lazy_static! {
    /// 全局任务管理器。
    pub static ref TASK_MANAGER: Mutex<TaskManager> = Mutex::new(TaskManager::new());
}

/// 尝试把一个新任务发布到指定 CPU。
///
/// 内核栈映射必须在 runqueue 可见之前同步到目标 CPU；
/// 线程组成员登记与 `New -> Queued(cpu)` 在同一个 group-exit 门禁内提交，
/// 远程 doorbell 则必须在门禁和 runqueue 锁都释放后发送。
pub(crate) fn try_publish_task_on(task: Arc<TaskControlBlock>, cpu: usize) -> Result<(), isize> {
    assert!(cpu < crate::smp::configured_cpu_count());
    let process = task.process.clone();
    // 这是避免无意义远端内核栈同步的快速拒绝；真正关闭 group-exit/exec
    // late-clone 竞争窗口的仍是 publish_thread() 在成员锁内执行的最终检查。
    if process.thread_publish_blocked() {
        return Err(EAGAIN);
    }
    if cpu != crate::smp::BOOT_CPU_ID {
        assert!(
            crate::smp::schedulers_released(),
            "cannot publish AP task before scheduler-ready"
        );
    }
    assert_ne!(
        crate::smp::online_cpu_mask() & (1usize << cpu),
        0,
        "cannot publish task to offline CPU {}",
        cpu
    );

    if cpu != crate::smp::cpu_id() {
        crate::smp::synchronize_kernel_mapping(cpu).unwrap_or_else(|error| {
            panic!("failed to publish kernel stack to CPU {}: {:?}", cpu, error)
        });
    }
    let published = process.publish_thread(&task, || super::run_queue::publish(task.clone(), cpu));
    if !published {
        return Err(EAGAIN);
    }
    if cpu != crate::smp::cpu_id() {
        crate::smp::request_reschedule(cpu).unwrap_or_else(|error| {
            panic!("failed to wake CPU {} after remote enqueue: {}", cpu, error)
        });
    }
    Ok(())
}

/// 启动和 ktest 使用的不可失败发布入口。
pub(crate) fn publish_task_on(task: Arc<TaskControlBlock>, cpu: usize) {
    try_publish_task_on(task, cpu).unwrap_or_else(|errno| {
        panic!(
            "unexpected group-exit gate while publishing bootstrap task: errno={}",
            errno
        )
    });
}

/// 按任务 affinity 和当前负载尝试发布普通新任务。
///
/// clone/fork 已继承父线程 mask，不能再无条件投递 CPU0。新任务没有最近运行
/// 的 cache locality，允许集合存在 idle CPU 时优先唤醒该 CPU；所有 CPU 都忙
/// 时才把调用 CPU 作为 fallback locality 提示。
pub(crate) fn try_publish_task(task: Arc<TaskControlBlock>) -> Result<(), isize> {
    // 启动期的 init/ktest runner 在 CPU0 首次进入 run_tasks() 前发布，此时
    // 本 CPU 还没有 current，scheduler-entered mask 也尚未包含 bit0。
    // 这条一次性 bootstrap 路径保持显式 CPU0；普通 clone 均有 current。
    if super::processor::current_task().is_none() {
        return try_publish_task_on(task, crate::smp::BOOT_CPU_ID);
    }
    let target =
        super::run_queue::select_new_task_cpu(task.cpus_allowed(), Some(crate::smp::cpu_id()));
    try_publish_task_on(task, target)
}

/// 启动和内核内部任务使用的不可失败 affinity-aware 发布入口。
pub fn publish_task(task: Arc<TaskControlBlock>) {
    try_publish_task(task).unwrap_or_else(|errno| {
        panic!(
            "unexpected thread-group gate while publishing kernel task: errno={}",
            errno
        )
    });
}

/// 在 CPU 已切回 idle 栈后完成上一任务的状态和容器交接。
pub fn finish_switch_out(task: Arc<TaskControlBlock>, cpu: usize) {
    loop {
        match task.task_status() {
            TaskStatus::Running(owner) if owner == cpu => {
                let run_started = task
                    .run_started_ticks
                    .swap(0, AtomicOrdering::AcqRel);
                if run_started != 0 {
                    crate::task::perf::record_task_run_slice(
                        crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_CORE)
                            .wrapping_sub(run_started),
                    );
                }
                // current 槽已在 idle 栈清空，但 Running(owner) 仍是唯一
                // 权威状态。持请求槽锁直到新 runqueue owner 提交，防止
                // 远程请求落在“检查旧 Running”与“提交 Queued”之间。
                let mut request_slot = task.remote_affinity_request.lock();
                let request = request_slot.take();
                let target = if let Some(request) = request.as_ref() {
                    assert!(
                        !task.has_migration_target(),
                        "remote affinity raced with local migration: tid={}",
                        task.gettid()
                    );
                    task.store_cpus_allowed(
                        request.mask(),
                        TaskStatus::Running(cpu),
                        "apply remote running affinity",
                    );
                    request.target_cpu()
                } else {
                    task.take_migration_target().unwrap_or(cpu)
                };
                super::run_queue::requeue_after_switch(task.clone(), cpu, target);
                if let Some(request) = request {
                    // 只有 Running -> Queued(target) 已成功后才返回 Applied；
                    // 请求方因而不会在任务仍占有旧 CPU 时提前返回。
                    request.complete(RemoteAffinityState::Applied);
                }
                drop(request_slot);
                // requeue_after_switch 已释放目标队列锁；远程 doorbell 只能
                // 在任务对目标 CPU 可见之后发送。
                if target != cpu {
                    crate::smp::request_reschedule(target).unwrap_or_else(|error| {
                        panic!(
                            "failed to wake CPU {} after task migration: {}",
                            target, error
                        )
                    });
                }
                return;
            }
            TaskStatus::Blocking(owner) if owner == cpu => {
                // wake 可以并发撤销 Blocking。CAS 失败时重新读取：若变回
                // Running，本 CPU 负责重新入队；若本次赢得 CAS，则保持睡眠。
                if task
                    .try_sched_transition(TaskStatus::Blocking(cpu), TaskStatus::Blocked)
                    .is_ok()
                {
                    // B29 不把阻塞语义扩成迁移协议；任务真正睡眠时取消尚未
                    // 消费的 yield 请求，后续 wake 仍按 last_cpu 选择目标。
                    let _ = task.take_migration_target();
                    return;
                }
            }
            TaskStatus::Zombie => {
                // 终态任务不会再次 yield，不能把一次性请求带入回收路径。
                let _ = task.take_migration_target();
                // processor 已在 idle 栈撤销 current 槽；到这里才能允许
                // 非 leader exec 交换旧 leader 的 TID。
                task.process.publish_exit_inactive(&task);
                // 当前代码已运行在退出 CPU 的 idle 栈；把最后一个调度 owner
                // 交给本 CPU 回收队列，不再跨核竞争全局 TaskManager。
                super::processor::enqueue_zombie(cpu, task);
                return;
            }
            actual => task.fail_sched_invariant(
                "finish task switch-out",
                TaskStatus::Running(cpu),
                actual,
                TaskStatus::Queued(cpu),
            ),
        }
    }
}

/// 从本 CPU runqueue 取出下一个可运行任务。
pub fn fetch_task(cpu: usize) -> Option<Arc<TaskControlBlock>> {
    let task = super::run_queue::fetch_or_steal(cpu)?;
    #[cfg(feature = "oom_handler")]
    TASK_MANAGER
        .lock()
        .active_tracker
        .mark_active(task.gettid());
    Some(task)
}

/// TID 身份交换后同步 OOM 活跃位图；未启用 OOM handler 时为空操作。
pub(crate) fn rekey_active_tid(old_tid: usize, new_tid: usize) {
    #[cfg(feature = "oom_handler")]
    {
        let mut manager = TASK_MANAGER.lock();
        manager.active_tracker.mark_inactive(old_tid);
        manager.active_tracker.mark_active(new_tid);
    }
    #[cfg(not(feature = "oom_handler"))]
    let _ = (old_tid, new_tid);
}

/// 从全部 Per-CPU 回收队列移除指定 pid 的 zombie TCB。
/// 返回的 Arc 在所有容器锁外 drop，避免析构链反向进入任务子系统。
pub fn remove_zombie_tasks_by_pid(pid: usize) {
    let zombies = super::processor::remove_zombie_tasks_by_pid(pid);
    drop(zombies);
}

/// 尝试释放所有任务的内存空间，直到释放`req`页。
#[cfg(feature = "oom_handler")]
pub fn do_oom(req: usize) -> Result<(), ()> {
    let mut total_released = 0;

    loop {
        // TASK_MANAGER 只负责挑选并标记 victim；Arc 移出后立即释放锁，
        // 远端 TLB ack 绝不能发生在全局任务管理锁内。
        let task = {
            let mut manager = TASK_MANAGER.try_lock().ok_or(())?;
            let task = manager
                .interruptible_queue
                .iter()
                .find(|task| manager.active_tracker.check_active(task.gettid()))
                .cloned();
            if let Some(task) = task.as_ref() {
                manager.active_tracker.mark_inactive(task.gettid());
            }
            task
        };
        let Some(task) = task else {
            break;
        };
        let released = task.process.vm().write(|vm| vm.do_deep_clean());
        log::warn!(
            "deep clean on task: tid {}, pid {}, released: {}",
            task.gettid(),
            task.pid(),
            released
        );
        total_released += released;
        if total_released >= req {
            return Ok(());
        }
    }

    for cpu in 0..crate::smp::configured_cpu_count() {
        let ready_len = super::run_queue::stats(cpu).0;
        for index in (0..ready_len).rev() {
            let Some(task) = super::run_queue::task_at(cpu, index) else {
                continue;
            };
            let claimed = {
                let mut manager = TASK_MANAGER.try_lock().ok_or(())?;
                if manager.active_tracker.check_active(task.gettid()) {
                    manager.active_tracker.mark_inactive(task.gettid());
                    true
                } else {
                    false
                }
            };
            if !claimed {
                continue;
            }
            let released = task.process.vm().write(|vm| vm.do_shallow_clean());
            log::warn!(
                "shallow clean on task: tid {}, pid {}, released: {}",
                task.gettid(),
                task.pid(),
                released
            );
            total_released += released;
            if total_released >= req {
                return Ok(());
            };
        }
    }
    Err(())
}

#[cfg(not(feature = "oom_handler"))]
#[allow(unused)]
/// 未启用 OOM handler 时的空实现。
pub fn do_oom() {}

/// 将任务加入 interruptible 队列。
///
/// # Semantics
///
/// `Running(cpu) -> Blocking(cpu)` 与加入 interruptible registry 在同一个
/// `TASK_MANAGER` 临界区完成；真正的 `Blocked` 由 idle 侧在切栈后提交。
pub fn sleep_interruptible(task: Arc<TaskControlBlock>) {
    TASK_MANAGER.lock().begin_interruptible_sleep(task.clone());
    // group exit/exec 可能恰好在停止方看到 Running 之后、这里登记 Blocking
    // 之前发布。登记完成后复查无锁门禁，保证目标线程不会永久睡下。
    if task.process.thread_must_exit(task.gettid()) {
        let _ = wake_interruptible(task);
    }
}

/// 唤醒 interruptible 任务并加入 ready 队列。
///
/// # Semantics
///
/// 若任务仍为 `Blocking`，取消本次阻塞；若已为 `Blocked`，则唯一地移入
/// ready queue。返回值表示本次调用是否实际赢得唤醒权。
pub fn wake_interruptible(task: Arc<TaskControlBlock>) -> bool {
    crate::task::perf::record_taskq_wake_interruptible();
    let target = TASK_MANAGER.lock().try_wake_interruptible(task);
    match target {
        Ok(target) => {
            if let Some(cpu) = target {
                notify_wake_targets(1usize << cpu);
            }
            true
        }
        Err(WaitQueueError::AlreadyWaken) => false,
    }
}

/// 让 group exit 或 exec 选中的 sibling 尽快到达自己的退出安全点。
///
/// 这里结合 Linux `zap_other_threads` 的通知语义与 DragonOS 的 owner 自清理原则：
/// 只投递不可屏蔽的 SIGKILL、唤醒睡眠者并 kick 运行 CPU；线程级资源必须由
/// 目标线程自己释放。
/// 线程组门禁保证 live 成员不会处于 `New`，因此不会遗漏“已登记但未入队”的 clone。
pub(crate) fn request_sibling_exit(tasks: &[Arc<TaskControlBlock>], current_tid: usize) {
    let mut targets = 0usize;
    for task in tasks {
        if task.gettid() == current_tid || task.is_zombie() {
            continue;
        }

        {
            let mut inner = task.acquire_inner_lock();
            if !task.is_zombie() {
                inner.add_signal(Signals::SIGKILL);
            }
        }

        match task.task_status() {
            TaskStatus::Blocking(cpu) => {
                targets |= 1usize << cpu;
                let _ = wake_interruptible(task.clone());
            }
            TaskStatus::Blocked => {
                let _ = wake_interruptible(task.clone());
            }
            TaskStatus::Queued(cpu) | TaskStatus::Running(cpu) => {
                targets |= 1usize << cpu;
            }
            // queued affinity 搬迁者会自行通知最终目标；广播确保源/目标任一
            // 正在运行调度循环时都能及时重新检查 group-exit 状态。
            // Migrating 没有稳定 owner；向全部在线 CPU 发 kick，让目标在完成
            // 单队列交接后的第一个安全点观察线程组停止请求。
            TaskStatus::Migrating => targets |= crate::smp::online_cpu_mask(),
            TaskStatus::Zombie => {}
            TaskStatus::New => panic!(
                "live sibling-exit target was never published: tid={}",
                task.gettid()
            ),
        }

        // wake 可能把 Blocked 发布到与 last_cpu 不同的目标，再读一次权威 owner。
        match task.task_status() {
            TaskStatus::Queued(cpu) | TaskStatus::Running(cpu) | TaskStatus::Blocking(cpu) => {
                targets |= 1usize << cpu;
            }
            TaskStatus::Migrating => targets |= crate::smp::online_cpu_mask(),
            _ => {}
        }
    }
    notify_wake_targets(targets);
}

/// 修改仍由 interruptible registry 持有的 Blocked 任务 affinity。
///
/// `Blocked` 也可能是退出路径从 runqueue 摘除后的短暂状态，所以不能只看
/// 原子状态。registry 成员关系和状态必须在同一个 `TASK_MANAGER` 临界区内
/// 同时成立；同一把锁也串行化后续 wake 对 `cpus_allowed` 的读取和入队。
fn update_blocked_affinity(task: &Arc<TaskControlBlock>, mask: usize) -> bool {
    let manager = TASK_MANAGER.lock();
    if task.task_status() != TaskStatus::Blocked
        || !manager
            .interruptible_queue
            .iter()
            .any(|blocked| task_ptr_eq(blocked, task))
    {
        return false;
    }
    task.set_blocked_affinity(mask);
    true
}

/// 更新非 current 任务 affinity，并等待非法 Running owner 真正交接。
///
/// Blocked 与 Queued 分别由现有 registry/runqueue 锁线性化。Running 任务
/// 若仍允许 owner CPU，可在请求槽锁内直接更新；若已排除 owner，
/// 则登记请求、发 RESCHEDULE，并协作式让出 CPU，直到源 idle 完成
/// `Running(source) -> Queued(target)`。`Blocking` 只是短暂过渡态，等待其
/// 回到 Running 或稳定进入 Blocked 后复用上述路径。
pub(crate) fn set_remote_affinity(task: &Arc<TaskControlBlock>, mask: usize) -> bool {
    let caller = current_task().expect("remote affinity update requires a schedulable caller");
    assert!(
        !Arc::ptr_eq(&caller, task),
        "current task must use set_current_affinity"
    );
    drop(caller);
    let runnable = task.runnable_affinity(mask, "update remote affinity");

    loop {
        match task.task_status() {
            TaskStatus::Blocked => {
                if update_blocked_affinity(task, mask) {
                    return true;
                }
                // wake 可能先取得 TASK_MANAGER 并发布到 runqueue；若状态确实
                // 已变化就按新 owner 重试。若仍为 Blocked，它已被退出路径从
                // runnable 容器摘除且不在 registry 中，不再接受 affinity 更新。
                if task.task_status() == TaskStatus::Blocked {
                    return false;
                }
            }
            TaskStatus::Queued(_) | TaskStatus::Migrating => {
                match super::run_queue::set_queued_affinity(task, mask) {
                    Ok(target) => {
                        if let Some(cpu) = target {
                            notify_wake_targets(1usize << cpu);
                        }
                        return true;
                    }
                    Err(TaskStatus::Blocked | TaskStatus::Queued(_) | TaskStatus::Migrating) => {}
                    Err(_) => return false,
                }
            }
            TaskStatus::Blocking(_) => {
                // begin_interruptible_sleep 会在 TASK_MANAGER 与请求槽锁下
                // 决定旧请求重试；这里不猜测最终是 wake 还是 Blocked。
                crate::task::suspend_current_and_run_next();
            }
            TaskStatus::Running(owner) => {
                let target = (mask & (1usize << owner) == 0)
                    .then(|| super::run_queue::select_runnable_cpu(runnable, None));
                // 排除 owner 时才需要预先发布目标内核栈映射。不能持
                // 请求槽锁等待 TLB ack；同步期间的状态变化由下方锁内复核处理。
                if let Some(target) = target {
                    crate::smp::synchronize_kernel_mapping(target).unwrap_or_else(|error| {
                        panic!(
                            "failed to synchronize remote task {} stack to CPU {}: {:?}",
                            task.gettid(),
                            target,
                            error
                        )
                    });
                }

                let mut request_slot = task.remote_affinity_request.lock();
                if let Some(request) = request_slot.as_ref().cloned() {
                    drop(request_slot);
                    while request.state() == RemoteAffinityState::Pending {
                        crate::task::suspend_current_and_run_next();
                    }
                    continue;
                }
                if task.has_migration_target() {
                    drop(request_slot);
                    crate::smp::request_reschedule(owner).unwrap_or_else(|error| {
                        panic!(
                            "failed to finish task {} migration on CPU {}: {}",
                            task.gettid(),
                            owner,
                            error
                        )
                    });
                    crate::task::suspend_current_and_run_next();
                    continue;
                }
                if task.task_status() != TaskStatus::Running(owner) {
                    drop(request_slot);
                    continue;
                }

                let Some(target) = target else {
                    task.store_cpus_allowed(
                        mask,
                        TaskStatus::Running(owner),
                        "update allowed remote running affinity",
                    );
                    return true;
                };
                let request = Arc::new(RemoteAffinityRequest::new(mask, target));
                *request_slot = Some(request.clone());
                drop(request_slot);
                // mailbox 的 Release 发布先于 doorbell；owner 只在任务安全点
                // 切回 idle，不在 IPI handler 里直接 context switch。
                crate::smp::request_reschedule(owner).unwrap_or_else(|error| {
                    panic!(
                        "failed to request task {} affinity handoff from CPU {}: {}",
                        task.gettid(),
                        owner,
                        error
                    )
                });
                loop {
                    match request.state() {
                        RemoteAffinityState::Pending => {
                            crate::task::suspend_current_and_run_next();
                        }
                        RemoteAffinityState::Applied => return true,
                        RemoteAffinityState::Retry => break,
                    }
                }
            }
            TaskStatus::New | TaskStatus::Zombie => return false,
        }
    }
}

/// 返回 ready + interruptible 队列计数的近似值。
pub fn procs_count() -> u16 {
    ready_count_fast().saturating_add(interruptible_count_fast())
}

/// 无锁判断 ready 队列是否非空。
pub fn has_ready_task() -> bool {
    super::run_queue::total_count_fast() != 0
}

/// 返回 runqueue 诊断残留与 Per-CPU 回收队列中的 zombie 任务数量。
pub fn zombie_count() -> u16 {
    let mut count = 0u16;
    for cpu in 0..crate::smp::configured_cpu_count() {
        count = count.saturating_add(super::run_queue::stats(cpu).1 as u16);
    }
    count.saturating_add(super::processor::zombie_queue_count_fast().min(u16::MAX as usize) as u16)
}

/// 向除 initproc 以外的所有 interruptible 任务投递信号。
///
/// # Locking
///
/// 先在 `TASK_MANAGER` 锁内克隆目标任务列表，再释放锁后修改每个任务的
/// signal 状态，最后批量入 ready 队列，避免调度队列锁和任务锁长时间嵌套。
pub fn send_signal_to_interruptible(signal: Signals) -> bool {
    let manager = TASK_MANAGER.lock();
    let tasks: Vec<_> = manager
        .interruptible_queue
        .iter()
        .filter(|t| t.pid() != 1)
        .cloned()
        .collect();
    drop(manager);
    if tasks.is_empty() {
        return false;
    }
    let mut sent = false;
    for task in &tasks {
        let mut inner = task.acquire_inner_lock();
        inner.add_signal(signal);
        sent = true;
    }
    enqueue_ready_batch(tasks);
    sent
}

/// 等待队列唤醒错误。
pub enum WaitQueueError {
    /// 任务已经处于 ready 队列中。
    AlreadyWaken,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// WaitQueue 等待结果。
pub enum WaitResult {
    /// 条件满足，携带调用方定义的返回值。
    Ready(isize),
    /// 被可处理信号中断。
    Interrupted,
    /// 到达 deadline。
    TimedOut,
}

impl WaitResult {
    /// 将等待结果转换为 syscall 返回值。
    ///
    /// `Ready` 直接返回内部值；其它结果通过调用方提供的转换函数编码。
    pub fn unwrap_or_else(self, f: impl FnOnce(isize) -> isize) -> isize {
        match self {
            WaitResult::Ready(value) => value,
            WaitResult::Interrupted => f(-(SyscallErr::ERESTART as isize)),
            WaitResult::TimedOut => f(-(SyscallErr::EAGAIN as isize)),
        }
    }
}

/// 一次等待登记。
///
/// `WaitEntry` 只记录“这一轮等待是否收到通知”，不表示任务的
/// CPU/runqueue 归属。后者仍只由 `TaskStatus` 管理。把两者分开后，
/// wake 即使早于 `Running -> Blocking` 到达，通知也会留在本条目中，
/// 阻塞入口随后能撤销睡眠。
pub struct WaitEntry {
    task: Weak<TaskControlBlock>,
    state: AtomicUsize,
}

impl WaitEntry {
    const WAITING: usize = 0;
    const NOTIFIED: usize = 1;
    const CLOSED: usize = 2;

    fn new(task: Weak<TaskControlBlock>) -> Self {
        Self {
            task,
            state: AtomicUsize::new(Self::WAITING),
        }
    }

    /// 领取本轮通知权；多队列等待时只有第一个唤醒源成功。
    fn notify(&self) -> bool {
        self.state
            .compare_exchange(
                Self::WAITING,
                Self::NOTIFIED,
                AtomicOrdering::AcqRel,
                AtomicOrdering::Acquire,
            )
            .is_ok()
    }

    /// 阻塞条件只在本轮通知尚未到达时成立。
    pub(crate) fn is_waiting(&self) -> bool {
        self.state.load(AtomicOrdering::Acquire) == Self::WAITING
    }

    /// 清理多队列条目前先关闭 token，防止其它队列再次领取。
    fn close(&self) {
        let _ = self.state.compare_exchange(
            Self::WAITING,
            Self::CLOSED,
            AtomicOrdering::AcqRel,
            AtomicOrdering::Acquire,
        );
    }

    fn matches_task(&self, task: &TaskControlBlock) -> bool {
        Weak::as_ptr(&self.task) == task as *const TaskControlBlock
    }
}

/// 弱引用等待队列。
///
/// # Semantics
///
/// 队列只强持有 `WaitEntry`，条目内对任务仍是弱引用，不会延长
/// TCB 生命周期。等待者必须先在关联对象的锁内检查条件，
/// 再注册条目，最后通过 checked block 入口复查 token 并让出 CPU。
///
/// # Locking
///
/// `wake_*` 会读取原子调度状态并操作 `TASK_MANAGER`，但不会获取
/// `task.inner`。调用方不得在持有调度器锁时调用唤醒函数。
pub struct WaitQueue {
    inner: VecDeque<Arc<WaitEntry>>,
}

#[allow(unused)]
impl WaitQueue {
    /// 创建空等待队列。
    pub fn new() -> Self {
        Self {
            inner: VecDeque::new(),
        }
    }
    /// 注册等待者，但不切换 CPU。
    ///
    /// # Locking
    ///
    /// 调用方应已持有保护等待条件的锁，并在之后调用阻塞原语。
    pub fn add_task(&mut self, task: Weak<TaskControlBlock>) {
        self.inner.push_back(Arc::new(WaitEntry::new(task)));
    }

    /// 弹出一个等待者但不唤醒。
    pub fn pop_task(&mut self) -> Option<Weak<TaskControlBlock>> {
        self.inner.pop_front().map(|entry| entry.task.clone())
    }

    /// 判断等待队列是否包含给定任务弱引用。
    pub fn contains(&self, task: &Weak<TaskControlBlock>) -> bool {
        self.inner
            .iter()
            .any(|entry| Weak::as_ptr(&entry.task) == Weak::as_ptr(task))
    }

    /// 判断等待队列是否为空。
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
    /// 清理所有失效 `Weak` 条目，返回清理数量。
    pub fn compact_stale(&mut self) -> usize {
        let before = self.inner.len();
        self.inner.retain(|entry| entry.task.strong_count() > 0);
        before - self.inner.len()
    }
    /// 唤醒队列中的所有可唤醒任务。
    ///
    /// # Locking
    ///
    /// 只读取 TCB 原子调度状态，并批量把 blocked 任务移入 ready 队列。
    pub fn wake_all(&mut self) -> usize {
        self.wake_at_most(usize::MAX)
    }

    /// 唤醒不超过 `limit` 个等待任务。
    ///
    /// 返回实际唤醒或已处于 ready 状态的任务数量。
    pub fn wake_at_most(&mut self, limit: usize) -> usize {
        if limit == 0 {
            return 0;
        }
        if limit == 1 {
            return self.wake_one();
        }
        let mut tasks_to_wake = Vec::with_capacity(limit.min(self.inner.len()));
        let mut remaining = VecDeque::new();
        let mut wake_count = 0usize;
        // 遍历全部条目以同时回收 stale/closed entry。是否成功唤醒
        // 只由 entry token 决定，不能再以瞬时 TaskStatus 判定：Running 任务
        // 可能正处在“已注册条件队列、尚未登记 Blocking”的窗口。
        while let Some(entry) = self.inner.pop_front() {
            if wake_count >= limit {
                if entry.task.strong_count() > 0 {
                    remaining.push_back(entry);
                }
                continue;
            }
            if let Some(task) = Self::notify_entry(&entry) {
                wake_count += 1;
                tasks_to_wake.push(task);
            }
        }
        self.inner = remaining;
        enqueue_ready_batch(tasks_to_wake);
        wake_count
    }
    fn wake_one(&mut self) -> usize {
        // Single wake is a hot path for futex/event waiters.  It only removes
        // entries up to the first wakeable task; later stale entries are compacted
        // by future wake/finish_wait calls or the batch path.
        while let Some(entry) = self.inner.pop_front() {
            if let Some(task) = Self::notify_entry(&entry) {
                let _ = wake_interruptible(task);
                return 1;
            }
        }
        0
    }

    /// 将当前任务加入条件等待队列。
    ///
    /// # Locking
    ///
    /// 调用方已持有等待队列所属对象的锁。真正的 `Running -> Blocking`
    /// 登记由随后执行的调度阻塞入口与全局 interruptible registry 一起提交；
    /// `Blocking -> Blocked` 只能在任务切回 idle 栈后完成。
    pub fn prepare_to_wait(&mut self, task: Weak<TaskControlBlock>) -> Arc<WaitEntry> {
        let entry = Arc::new(WaitEntry::new(task));
        self.inner.push_back(entry.clone());
        entry
    }

    /// 从条件等待队列移除任务。调度状态已经由 wake 或重新切入路径处理，
    /// 这里不能再单独修改状态。
    ///
    /// 返回值表示该任务是否仍在队列中。若返回 `false`，通常说明它已经被
    /// 正常唤醒路径移除。
    pub fn finish_wait(&mut self, task: &TaskControlBlock) -> bool {
        let old_len = self.inner.len();
        self.inner.retain(|entry| {
            if entry.matches_task(task) {
                entry.close();
                false
            } else {
                true
            }
        });
        self.inner.len() != old_len
    }

    /// 结束精确的一轮等待，不会误删同一任务在其它语义上的条目。
    pub(crate) fn finish_entry(&mut self, entry: &Arc<WaitEntry>) -> bool {
        entry.close();
        let old_len = self.inner.len();
        self.inner.retain(|queued| !Arc::ptr_eq(queued, entry));
        self.inner.len() != old_len
    }

    /// 将唤醒记入 token，再返回可供调度器尝试唤醒的 TCB。
    fn notify_entry(entry: &WaitEntry) -> Option<Arc<TaskControlBlock>> {
        let task = entry.task.upgrade()?;
        // New 从未发布过等待，Zombie 也不可逆；它们只是待回收的
        // stale entry。Running/Queued 则不能排除，因为早到 wake 正会观察到这两态。
        if matches!(task.task_status(), TaskStatus::New | TaskStatus::Zombie) {
            entry.close();
            return None;
        }
        if !entry.notify() {
            return None;
        }
        // 通知一旦发布，本轮 deadline/fallback timer 即不再是唯一唤醒源。
        task.wait_timer_generation
            .fetch_add(1, AtomicOrdering::Relaxed);
        Some(task)
    }

    fn wait_event_impl<F>(
        wq: &Mutex<Self>,
        cond: &mut F,
        signal_check: bool,
        deadline: Option<TimeSpec>,
    ) -> WaitResult
    where
        F: FnMut() -> Option<isize>,
    {
        if let Some(res) = cond() {
            return WaitResult::Ready(res);
        }

        loop {
            if deadline
                .map(|deadline| TimeSpec::now() >= deadline)
                .unwrap_or(false)
            {
                return WaitResult::TimedOut;
            }

            let task = current_task().unwrap();

            // 等待队列锁只保护 entry 的登记和摘除，不能覆盖条件检查。
            // `WaitEntry` 会持久化登记后的早到通知，因此这里释放锁不会
            // 重新打开 lost-wakeup 窗口，反而允许条件检查同步推进生产者。
            let entry = wq.lock().prepare_to_wait(Arc::downgrade(&task));

            if let Some(res) = cond() {
                wq.lock().finish_entry(&entry);
                return WaitResult::Ready(res);
            }
            if deadline
                .map(|deadline| TimeSpec::now() >= deadline)
                .unwrap_or(false)
            {
                wq.lock().finish_entry(&entry);
                return WaitResult::TimedOut;
            }
            // 普通“不可中断”等待仍忽略用户信号，但不能阻止线程组生命周期
            // 前进。返回 Interrupted 让上层先正常析构 syscall 栈上的 Arc。
            if task.process.thread_must_exit(task.gettid()) {
                wq.lock().finish_entry(&entry);
                return WaitResult::Interrupted;
            }
            if signal_check {
                // 必须在不持有 task.inner 的情况下检查 actionable signal；
                // waited signal 即使被 sigmask 屏蔽也必须取消睡眠，随后由
                // sigtimedwait 在 WaitQueue 外重新领取。
                if has_waited_signal(&task) || has_actionable_signal(&task) {
                    wq.lock().finish_entry(&entry);
                    return WaitResult::Interrupted;
                }
                discard_non_actionable_unblocked_signals(&task);
            }

            // 通知若在第二次条件检查期间到达，直接重新检查业务条件，
            // 不必登记 Blocking。
            if !entry.is_waiting() {
                wq.lock().finish_entry(&entry);
                continue;
            }

            if let Some(deadline) = deadline {
                wait_with_timeout(Arc::downgrade(&task), deadline);
            }
            drop(task);

            block_current_and_run_next_checked(|task| {
                // Running -> Blocking 登记后再检查 waited signal，关闭发送方在
                // Running 状态看到 AlreadyWaken、而接收方随后真正睡下的窗口。
                // WaitEntry 额外覆盖普通队列 wake 的同类窗口：早到通知
                // 会使 is_waiting() 失败，从而撤销刚登记的 Blocking。
                let no_signal =
                    !signal_check || (!has_waited_signal(task) && !has_actionable_signal(task));
                let not_timed_out = deadline
                    .map(|deadline| TimeSpec::now() < deadline)
                    .unwrap_or(true);
                let process_alive = !task.process.thread_must_exit(task.gettid());
                entry.is_waiting() && no_signal && not_timed_out && process_alive
            });

            let task = current_task().unwrap();
            wq.lock().finish_entry(&entry);
            if deadline.is_some() {
                task.wait_timer_generation
                    .fetch_add(1, AtomicOrdering::Relaxed);
            }
        }
    }

    fn wait_event_locked_impl<T, Q, F>(
        lock: &Mutex<T>,
        mut queue_of: Q,
        cond: &mut F,
        signal_check: bool,
        deadline: Option<TimeSpec>,
    ) -> WaitResult
    where
        Q: for<'a> FnMut(&'a mut T) -> &'a mut WaitQueue,
        F: FnMut(&mut T) -> Option<isize>,
    {
        {
            let mut guard = lock.lock();
            if let Some(res) = cond(&mut guard) {
                return WaitResult::Ready(res);
            }
        }

        loop {
            let mut guard = lock.lock();
            if deadline
                .map(|deadline| TimeSpec::now() >= deadline)
                .unwrap_or(false)
            {
                return WaitResult::TimedOut;
            }

            let task = current_task().unwrap();

            let entry = queue_of(&mut guard).prepare_to_wait(Arc::downgrade(&task));
            if let Some(res) = cond(&mut guard) {
                queue_of(&mut guard).finish_entry(&entry);
                return WaitResult::Ready(res);
            }
            if deadline
                .map(|deadline| TimeSpec::now() >= deadline)
                .unwrap_or(false)
            {
                queue_of(&mut guard).finish_entry(&entry);
                return WaitResult::TimedOut;
            }
            if task.process.thread_must_exit(task.gettid()) {
                queue_of(&mut guard).finish_entry(&entry);
                return WaitResult::Interrupted;
            }
            if signal_check {
                // 持有的是调用方传入的对象锁，不能同时长期持有 task.inner。
                if has_actionable_signal(&task) {
                    queue_of(&mut guard).finish_entry(&entry);
                    return WaitResult::Interrupted;
                }
                discard_non_actionable_unblocked_signals(&task);
            }
            if let Some(deadline) = deadline {
                wait_with_timeout(Arc::downgrade(&task), deadline);
            }
            drop(task);

            block_current_and_run_next_with_lock_checked(guard, |task| {
                let no_signal = !signal_check || !has_actionable_signal(task);
                let not_timed_out = deadline
                    .map(|deadline| TimeSpec::now() < deadline)
                    .unwrap_or(true);
                let process_alive = !task.process.thread_must_exit(task.gettid());
                entry.is_waiting() && no_signal && not_timed_out && process_alive
            });

            let task = current_task().unwrap();
            let mut guard = lock.lock();
            queue_of(&mut guard).finish_entry(&entry);
            drop(guard);
            if deadline.is_some() {
                task.wait_timer_generation
                    .fetch_add(1, AtomicOrdering::Relaxed);
            }
        }
    }

    fn finish_wait_on_queues(queues: &[&Mutex<Self>], entry: &Arc<WaitEntry>) {
        // 先关闭共享 token，再逐队列删除；中间窗口内其它队列的
        // wake 只会观察到 Closed，不会把同一轮等待领取第二次。
        entry.close();
        for queue in queues {
            queue.lock().finish_entry(entry);
        }
    }

    pub fn wait_on_queues_interruptible_timeout<F>(
        queues: &[&Mutex<Self>],
        mut cond: F,
        deadline: Option<TimeSpec>,
    ) -> WaitResult
    where
        F: FnMut() -> Option<isize>,
    {
        // `cond` 可能在当前任务短暂让出 CPU 后再次执行，调用方不能依赖
        // `current_task()` 返回值在闭包中保持不变。
        if let Some(res) = cond() {
            return WaitResult::Ready(res);
        }

        if queues.is_empty() {
            // poll/epoll 没有任何 fd 时只需响应 signal 或用户 deadline。
            // 这条路径没有外部条件生产者，不应伪造 10 ms 周期唤醒。
            let wait_queue = Mutex::new(WaitQueue::new());
            return match deadline {
                Some(deadline) => {
                    Self::wait_event_interruptible_timeout(&wait_queue, &mut cond, deadline)
                }
                None => Self::wait_event_interruptible(&wait_queue, &mut cond),
            };
        }

        loop {
            if deadline
                .map(|deadline| TimeSpec::now() >= deadline)
                .unwrap_or(false)
            {
                return WaitResult::TimedOut;
            }

            let task = current_task().unwrap();
            // poll/epoll 可同时登记多个源，所有队列共享同一个
            // token；任一源的第一次通知都足以取消本轮睡眠。
            let entry = Arc::new(WaitEntry::new(Arc::downgrade(&task)));
            for queue in queues {
                queue.lock().inner.push_back(entry.clone());
            }

            if let Some(res) = cond() {
                Self::finish_wait_on_queues(queues, &entry);
                return WaitResult::Ready(res);
            }
            if deadline
                .map(|deadline| TimeSpec::now() >= deadline)
                .unwrap_or(false)
            {
                Self::finish_wait_on_queues(queues, &entry);
                return WaitResult::TimedOut;
            }
            if task.process.thread_must_exit(task.gettid()) {
                Self::finish_wait_on_queues(queues, &entry);
                return WaitResult::Interrupted;
            }
            if has_actionable_signal(&task) {
                Self::finish_wait_on_queues(queues, &entry);
                return WaitResult::Interrupted;
            }
            discard_non_actionable_unblocked_signals(&task);

            if let Some(deadline) = deadline {
                wait_with_timeout(Arc::downgrade(&task), deadline);
            }
            drop(task);

            block_current_and_run_next_checked(|task| {
                let no_signal = !has_actionable_signal(task);
                let not_timed_out = deadline
                    .map(|deadline| TimeSpec::now() < deadline)
                    .unwrap_or(true);
                entry.is_waiting() && no_signal && not_timed_out && cond().is_none()
            });

            let task = current_task().unwrap();
            Self::finish_wait_on_queues(queues, &entry);
            // deadline timer 可能在某个队列早到通知之后才被注册。
            // 清理时统一推进 generation，防止它之后唤醒新一轮无超时等待。
            if deadline.is_some() {
                task.wait_timer_generation
                    .fetch_add(1, AtomicOrdering::Relaxed);
            }
        }
    }

    /// 不可中断等待，条件满足前一直阻塞。
    ///
    /// 等价于 DragonOS 的 `wait_until`（Uninterruptible）。
    /// 适用于内核内部确定性等待（无需信号检查的场景）。
    /// 文件和网络 IO 通用；网络条件闭包只查询 readiness，不能直接 poll。
    ///
    /// # Locking
    ///
    /// `cond` 会先在无锁快速路径执行一次，并在 `prepare_to_wait` 后再次执行。
    /// 第二次检查也不持有等待队列锁；登记后的早到通知由 `WaitEntry` token
    /// 持久化，并由 checked block 在提交 Blocking 后作最终复查。
    pub fn wait_until<F>(wq: &Mutex<Self>, mut cond: F) -> isize
    where
        F: FnMut() -> Option<isize>,
    {
        match Self::wait_event_impl(wq, &mut cond, false, None) {
            WaitResult::Ready(value) => value,
            WaitResult::Interrupted => -(SyscallErr::ERESTART as isize),
            WaitResult::TimedOut => -(SyscallErr::EAGAIN as isize),
        }
    }

    /// 可中断等待，条件满足或收到可处理信号时返回。
    ///
    /// 等价于 DragonOS 的 `wait_until_interruptible`。
    /// 文件和网络 IO 通用。
    /// # Semantics
    ///
    /// 条件满足返回 `WaitResult::Ready(v)`；被信号中断返回
    /// `WaitResult::Interrupted`。
    pub fn wait_until_interruptible<F>(wq: &Mutex<Self>, mut cond: F) -> WaitResult
    where
        F: FnMut() -> Option<isize>,
    {
        Self::wait_event_impl(wq, &mut cond, true, None)
    }

    /// I/O 等待（不可中断）。
    ///
    /// 等价于 DragonOS 的 `wait_until_io`。
    pub fn wait_until_io<F>(wq: &Mutex<Self>, mut cond: F) -> isize
    where
        F: FnMut() -> Option<isize>,
    {
        match Self::wait_event_impl(wq, &mut cond, false, None) {
            WaitResult::Ready(value) => value,
            WaitResult::Interrupted => -(SyscallErr::ERESTART as isize),
            WaitResult::TimedOut => -(SyscallErr::EAGAIN as isize),
        }
    }

    /// I/O 等待（可中断）。
    ///
    /// 等价于 DragonOS 的 `wait_until_io_interruptible`。
    /// 返回值同 `wait_until_interruptible`。
    pub fn wait_until_io_interruptible<F>(wq: &Mutex<Self>, mut cond: F) -> WaitResult
    where
        F: FnMut() -> Option<isize>,
    {
        Self::wait_event_impl(wq, &mut cond, true, None)
    }

    /// 可中断等待，条件满足或收到可处理信号时返回。
    pub fn wait_event_interruptible<F>(wq: &Mutex<Self>, mut cond: F) -> WaitResult
    where
        F: FnMut() -> Option<isize>,
    {
        Self::wait_event_impl(wq, &mut cond, true, None)
    }

    /// 不可中断等待，纯事件驱动。
    ///
    /// 用于内核内部具有精确生产者通知的状态转换；调用方必须保证每次
    /// 可能满足 `cond` 的转换都会唤醒该队列。
    pub fn wait_event<F>(wq: &Mutex<Self>, mut cond: F) -> WaitResult
    where
        F: FnMut() -> Option<isize>,
    {
        Self::wait_event_impl(wq, &mut cond, false, None)
    }

    /// 不可中断等待直到条件满足或绝对 deadline 到达。
    pub fn wait_event_timeout<F>(wq: &Mutex<Self>, mut cond: F, deadline: TimeSpec) -> WaitResult
    where
        F: FnMut() -> Option<isize>,
    {
        Self::wait_event_impl(wq, &mut cond, false, Some(deadline))
    }

    /// 可中断等待直到条件满足、信号到达或绝对 deadline 到达。
    pub fn wait_event_interruptible_timeout<F>(
        wq: &Mutex<Self>,
        mut cond: F,
        deadline: TimeSpec,
    ) -> WaitResult
    where
        F: FnMut() -> Option<isize>,
    {
        Self::wait_event_impl(wq, &mut cond, true, Some(deadline))
    }

    /// 在调用方对象锁下检查条件并注册可中断等待。
    ///
    /// # Locking
    ///
    /// `queue_of` 从同一个对象锁保护的数据中返回等待队列；`cond` 也在该锁下执行。
    pub fn wait_event_interruptible_locked<T, Q, F>(
        lock: &Mutex<T>,
        queue_of: Q,
        mut cond: F,
    ) -> WaitResult
    where
        Q: for<'a> FnMut(&'a mut T) -> &'a mut WaitQueue,
        F: FnMut(&mut T) -> Option<isize>,
    {
        Self::wait_event_locked_impl(lock, queue_of, &mut cond, true, None)
    }

    /// 在调用方对象锁下检查条件并注册不可中断等待。
    pub fn wait_event_locked<T, Q, F>(lock: &Mutex<T>, queue_of: Q, mut cond: F) -> WaitResult
    where
        Q: for<'a> FnMut(&'a mut T) -> &'a mut WaitQueue,
        F: FnMut(&mut T) -> Option<isize>,
    {
        Self::wait_event_locked_impl(lock, queue_of, &mut cond, false, None)
    }

    /// 在调用方对象锁下等待条件、信号或绝对 deadline。
    pub fn wait_event_interruptible_timeout_locked<T, Q, F>(
        lock: &Mutex<T>,
        queue_of: Q,
        mut cond: F,
        deadline: TimeSpec,
    ) -> WaitResult
    where
        Q: for<'a> FnMut(&'a mut T) -> &'a mut WaitQueue,
        F: FnMut(&mut T) -> Option<isize>,
    {
        Self::wait_event_locked_impl(lock, queue_of, &mut cond, true, Some(deadline))
    }
}

/// legacy 超时等待队列中的等待者。
pub struct TimeoutWaiter {
    /// 等待任务弱引用。
    task: Weak<TaskControlBlock>,
    /// 绝对超时时间。
    timeout: TimeSpec,
}

/// 内核定时器到期动作。
pub enum TimerAction {
    /// 唤醒指定任务。
    WakeTask {
        task: Weak<TaskControlBlock>,
        generation: usize,
    },
    /// legacy ITIMER_REAL 到期后向所属进程投递 SIGALRM。
    IntervalTimerSignal {
        process: Weak<ProcessControlBlock>,
        generation: u64,
    },
    /// POSIX timer 到期后向所属进程投递信号。
    PosixTimerSignal {
        process: Weak<ProcessControlBlock>,
        timer_id: usize,
        arm_seq: u64,
    },
    // Global timerfd sweep. Individual timerfds are kept in fs::timerfd's
    // registry; this action exists only to drive high-resolution wakeups.
    TimerFdSweep {
        generation: usize,
    },
    /// TCP socket 最后一个用户 owner 消失后，由 smoltcp poll deadline 驱动的回收。
    /// route/SocketSet 所有权仍在网络 worker；timer 只发布下一次 worker 请求。
    NetPoll {
        generation: usize,
    },
}

/// 内核定时器堆节点。
pub struct KernelTimer {
    action: TimerAction,
    deadline: TimeSpec,
}

impl Ord for KernelTimer {
    fn cmp(&self, other: &Self) -> Ordering {
        Ordering::reverse(self.deadline.cmp(&other.deadline))
    }
}

impl PartialOrd for KernelTimer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Eq for KernelTimer {}

impl PartialEq for KernelTimer {
    /// 只按 deadline 判等，满足 `BinaryHeap` 排序需要。
    fn eq(&self, other: &Self) -> bool {
        self.deadline.eq(&other.deadline)
    }
}

impl KernelTimer {
    fn is_live(&self) -> bool {
        match &self.action {
            TimerAction::WakeTask {
                task, generation, ..
            } => match task.upgrade() {
                Some(task) => {
                    // WakeTask entries are intentionally not deduplicated on insertion.
                    // A newer wait bumps the generation; older entries are stale and can be
                    // dropped by compact() or ignored when their deadline expires.
                    task.wait_timer_generation.load(AtomicOrdering::Relaxed) == *generation
                }
                None => false,
            },
            TimerAction::IntervalTimerSignal {
                process,
                generation,
            } => process
                .upgrade()
                .map(|process| process.real_interval_timer_is_live(*generation, self.deadline))
                .unwrap_or(false),
            TimerAction::PosixTimerSignal {
                process,
                timer_id,
                arm_seq,
            } => match process.upgrade() {
                Some(process) => process
                    .posix_timers()
                    .get(*timer_id)
                    .map(|timer| {
                        timer.arm_seq == *arm_seq && timer.wall_deadline == Some(self.deadline)
                    })
                    .unwrap_or(false),
                None => false,
            },
            TimerAction::TimerFdSweep { generation } => {
                crate::fs::timerfd::timerfd_sweep_is_current(*generation)
            }
            TimerAction::NetPoll { generation } => {
                crate::net::config::NET_INTERFACE.tcp_cleanup_timer_is_current(*generation)
            }
        }
    }
}

/// 内核定时器优先队列。
pub struct KernelTimerQueue {
    inner: BinaryHeap<KernelTimer>,
}

impl KernelTimerQueue {
    /// 最大定时器数量，防止内存耗尽。
    const MAX_TIMERS: usize = 4096;

    /// 创建空定时器队列。
    pub fn new() -> Self {
        Self {
            inner: BinaryHeap::new(),
        }
    }

    /// 返回当前队列中的定时器数量。
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// 判断队列是否为空。
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// 返回最早 deadline 的纳秒值，队列为空时返回 0。
    pub fn earliest_deadline_ns(&self) -> u64 {
        self.inner
            .peek()
            .map(|t| t.deadline.to_ns_saturating())
            .unwrap_or(0)
    }

    fn refresh_deadline_state(&self) {
        let next_ns = self.earliest_deadline_ns();
        KERNEL_TIMER_QUEUE_NEXT_NS.store(next_ns, AtomicOrdering::Relaxed);
        KERNEL_TIMER_QUEUE_PENDING.store(!self.inner.is_empty(), AtomicOrdering::Relaxed);
    }

    /// 插入一个定时器动作。
    ///
    /// # Semantics
    ///
    /// 返回 `true` 表示新动作成为最早 deadline，调用方需要重编程硬件 timer。
    pub fn add_action(&mut self, action: TimerAction, deadline: TimeSpec) -> bool {
        let old_earliest = self.earliest_deadline_ns();
        // Keep the hot path O(log n).  WakeTask stale entries are filtered by
        // generation, while signal timers validate their generation/deadline in run_timer().
        self.inner.push(KernelTimer { action, deadline });
        if self.inner.len() > Self::MAX_TIMERS {
            self.enforce_capacity();
        }
        let new_earliest = self.earliest_deadline_ns();
        self.refresh_deadline_state();
        // Return true if this new timer is now the earliest (or if the queue
        // was empty before, meaning a previously-idle queue now has work).
        old_earliest == 0 || new_earliest < old_earliest
    }
    /// 弹出已过期定时器，最多处理一个批次。
    ///
    /// # Locking
    ///
    /// 调用方必须持有 `KERNEL_TIMER_QUEUE` 锁；返回的 callback 必须在锁外执行。
    pub fn pop_expired(&mut self, now: TimeSpec) -> Vec<KernelTimer> {
        let _pop_start = crate::task::perf::perf_time_now();
        let mut nodes = 0usize;
        const MAX_BATCH: usize = 64;
        let mut expired = Vec::new();
        while let Some(timer) = self.inner.pop() {
            if timer.deadline > now {
                self.inner.push(timer);
                break;
            }
            expired.push(timer);
            nodes += 1;
            if expired.len() >= MAX_BATCH {
                break;
            }
        }
        crate::task::perf::record_ktimer_pop(expired.len());
        self.refresh_deadline_state();
        crate::task::perf::record_timer_pop_cost(_pop_start, nodes);
        expired
    }
    /// 清理失效的 Weak、wait generation 和 POSIX timer arm 节点，释放堆槽位。
    ///
    /// 本函数在 `KERNEL_TIMER_QUEUE` 锁下读取 POSIX timer 表；反方向严格禁止，
    /// 所有装载路径都必须先释放 timer 表锁再调用 `add_kernel_timer()`。
    pub fn compact(&mut self) {
        if self.inner.is_empty() {
            self.refresh_deadline_state();
            return;
        }
        let entries: Vec<KernelTimer> = self.inner.drain().collect();
        let total = entries.len();
        let mut live_inserted = 0usize;
        for timer in entries {
            if timer.is_live() {
                self.inner.push(timer);
                live_inserted += 1;
            }
        }
        crate::task::perf::record_ktimer_compact(total - live_inserted);
        self.refresh_deadline_state();
    }
    fn enforce_capacity(&mut self) {
        self.compact();
        if self.inner.len() <= Self::MAX_TIMERS {
            return;
        }

        let mut entries: Vec<KernelTimer> = self.inner.drain().collect();
        entries.sort_unstable_by(|a, b| a.deadline.cmp(&b.deadline));
        let dropped = entries.len().saturating_sub(Self::MAX_TIMERS);
        for timer in entries.into_iter().take(Self::MAX_TIMERS) {
            self.inner.push(timer);
        }
        if dropped > 0 {
            log::warn!(
                "[KernelTimerQueue] capacity limit ({}) reached, dropped {} farthest timers",
                Self::MAX_TIMERS,
                dropped
            );
        }
    }
    /// 执行单个定时器 callback。
    ///
    /// # Locking
    ///
    /// 必须在未持有 `KERNEL_TIMER_QUEUE` 锁时调用。callback 可能获取任务锁、
    /// 调度器锁或重新插入新的 kernel timer。
    pub fn run_timer(timer: KernelTimer, now: TimeSpec) -> bool {
        match timer.action {
            TimerAction::WakeTask { task, generation } => {
                let Some(task) = task.upgrade() else {
                    return false;
                };
                task.wait_io_timer_pending
                    .store(false, AtomicOrdering::Relaxed);

                // Normal wake (deadline)

                if task.wait_timer_generation.load(AtomicOrdering::Relaxed) != generation {
                    crate::task::perf::record_ktimer_stale_waketask();
                    return false; // generation mismatch, stale
                }

                let should_wake = matches!(
                    task.task_status(),
                    super::TaskStatus::Blocking(_) | super::TaskStatus::Blocked
                );
                if should_wake && wake_interruptible(task) {
                    crate::task::perf::record_ktimer_real_wake();
                    return true;
                }
                false
            }
            TimerAction::IntervalTimerSignal {
                process,
                generation,
            } => {
                let Some(process) = process.upgrade() else {
                    return false;
                };
                let (fired, next) =
                    process.expire_real_interval_timer(generation, timer.deadline, now);
                if !fired {
                    return false;
                }
                // interval timer 锁已经释放；SignalQueue 可能扩容，唤醒还会进入
                // 调度器，因此两者都不能放回 timer owner 临界区。
                let _ = queue_kernel_process_signal(&process, Signals::SIGALRM);
                let woke = wake_process_signal_waiter(&process, Signals::SIGALRM);
                if let Some((deadline, next_generation)) = next {
                    add_kernel_timer(
                        TimerAction::IntervalTimerSignal {
                            process: Arc::downgrade(&process),
                            generation: next_generation,
                        },
                        deadline,
                    );
                }
                woke
            }
            TimerAction::PosixTimerSignal {
                process,
                timer_id,
                arm_seq,
            } => {
                let Some(process) = process.upgrade() else {
                    return false;
                };
                let mut next_timer = None;
                let generated_event = {
                    let mut timers = process.posix_timers();
                    let Some(mut timer_state) = timers.get(timer_id).cloned() else {
                        return false;
                    };
                    if timer_state.arm_seq != arm_seq
                        || timer_state.wall_deadline != Some(timer.deadline)
                    {
                        return false;
                    }
                    let expirations = if timer_state.interval.is_zero() {
                        timer_state.value = TimeSpec::new();
                        timer_state.wall_deadline = None;
                        timer_state.realtime_abs_deadline = None;
                        1
                    } else {
                        let interval_ns = timer_state.interval.to_ns_saturating().max(1) as usize;
                        let deadline_ns = timer.deadline.to_ns_saturating() as usize;
                        let elapsed_ns =
                            (now.to_ns_saturating() as usize).saturating_sub(deadline_ns);
                        let expirations = 1usize.saturating_add(elapsed_ns / interval_ns);
                        let next_ns =
                            deadline_ns.saturating_add(expirations.saturating_mul(interval_ns));
                        let deadline = TimeSpec::from_ns(next_ns);
                        if let Some(abs_deadline) = timer_state.realtime_abs_deadline {
                            let abs_ns = abs_deadline
                                .to_ns_saturating()
                                .saturating_add(expirations.saturating_mul(interval_ns) as u64);
                            timer_state.realtime_abs_deadline =
                                Some(TimeSpec::from_ns(abs_ns as usize));
                        }
                        timer_state.arm_seq = timers.alloc_arm_seq();
                        timer_state.value = timer_state.interval;
                        timer_state.wall_deadline = Some(deadline);
                        next_timer = Some((deadline, timer_state.arm_seq));
                        expirations
                    };
                    // 只在表锁内领取 timer 事件；SignalQueue 可能扩容，不能在
                    // IRQ-off 的 deferred timer 路径中嵌套进入 allocator。
                    let event = timer_state.record_expiry(timer_id, expirations);
                    *timers.get_mut(timer_id).unwrap() = timer_state;
                    event
                };
                let woke = generated_event
                    .map(|event| process.publish_posix_timer_signal(event))
                    .unwrap_or(false);
                if let Some((deadline, next_arm_seq)) = next_timer {
                    add_kernel_timer(
                        TimerAction::PosixTimerSignal {
                            process: Arc::downgrade(&process),
                            timer_id,
                            arm_seq: next_arm_seq,
                        },
                        deadline,
                    );
                }
                woke
            }
            TimerAction::TimerFdSweep { generation } => {
                if !crate::fs::timerfd::timerfd_sweep_is_current(generation) {
                    return false;
                }
                let woke = crate::fs::timerfd::wake_expired_timerfds(now) > 0;
                crate::fs::timerfd::rearm_timerfd_sweep();
                woke
            }
            TimerAction::NetPoll { generation } => {
                crate::net::config::NET_INTERFACE.run_tcp_cleanup_timer(generation)
            }
        }
    }
}
// `BinaryHeap` 是最大堆；反转排序后最早超时的等待者排在堆顶。
impl Ord for TimeoutWaiter {
    fn cmp(&self, other: &Self) -> Ordering {
        Ordering::reverse(self.timeout.cmp(&other.timeout))
    }
}

impl PartialOrd for TimeoutWaiter {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Eq for TimeoutWaiter {}

impl PartialEq for TimeoutWaiter {
    /// 只按 timeout 判等，满足堆排序需要。
    fn eq(&self, other: &Self) -> bool {
        self.timeout.eq(&other.timeout)
    }
}

/// legacy 超时等待队列。
///
/// # Semantics
///
/// 新的超时等待优先通过 `KernelTimerQueue` 驱动；本队列保留给旧路径兼容，
/// 由 `do_wake_expired()` 和 timer interrupt 路径兜底扫描。
pub struct TimeoutWaitQueue {
    /// 反转排序后的二叉堆，最早 timeout 在堆顶。
    inner: BinaryHeap<TimeoutWaiter>,
}

impl TimeoutWaitQueue {
    /// 创建空 timeout wait queue。
    pub fn new() -> Self {
        Self {
            inner: BinaryHeap::new(),
        }
    }
    /// 注册一个带绝对超时时间的任务，但不阻塞它。
    pub fn add_task(&mut self, task: Weak<TaskControlBlock>, timeout: TimeSpec) {
        self.inner.push(TimeoutWaiter { task, timeout });
        self.refresh_deadline_state();
    }

    /// 判断队列是否为空。
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    fn earliest_deadline_ns(&self) -> u64 {
        self.inner
            .peek()
            .map(|waiter| waiter.timeout.to_ns_saturating())
            .unwrap_or(0)
    }

    fn refresh_deadline_state(&self) {
        let next_ns = self.earliest_deadline_ns();
        TIMEOUT_WAITQUEUE_NEXT_NS.store(next_ns, AtomicOrdering::Relaxed);
        TIMEOUT_WAITQUEUE_PENDING.store(!self.inner.is_empty(), AtomicOrdering::Relaxed);
    }

    /// 唤醒所有已经超时的任务。
    ///
    /// # Locking
    ///
    /// 调用方持有 `TIMEOUT_WAITQUEUE` 锁。函数只收集原子状态仍为 Blocked 的
    /// TCB，随后由统一批量入口争用唯一唤醒权。
    pub fn wake_expired(&mut self, now: TimeSpec) {
        if self
            .inner
            .peek()
            .map(|waiter| waiter.timeout > now)
            .unwrap_or(true)
        {
            self.refresh_deadline_state();
            return;
        }
        let mut tasks_to_wake = Vec::new();
        while let Some(waiter) = self.inner.pop() {
            if waiter.timeout > now {
                self.inner.push(waiter);
                break;
            } else {
                match waiter.task.upgrade() {
                    Some(task)
                        if matches!(
                            task.task_status(),
                            super::TaskStatus::Blocking(_) | super::TaskStatus::Blocked
                        ) =>
                    {
                        tasks_to_wake.push(task);
                    }
                    Some(_) => continue,
                    None => continue,
                }
            }
        }
        enqueue_ready_batch(tasks_to_wake);
        self.refresh_deadline_state();
    }
    #[allow(unused)]
    /// 打印等待者 deadline，仅供诊断。
    pub fn show_waiter(&self) {
        for waiter in self.inner.iter() {
            log::error!("[show_waiter] timeout: {:?}", waiter.timeout);
        }
    }
}

lazy_static! {
    /// 全局 legacy 超时等待队列。
    pub static ref TIMEOUT_WAITQUEUE: Mutex<TimeoutWaitQueue> = Mutex::new(TimeoutWaitQueue::new());
    /// 全局内核定时器队列。
    pub static ref KERNEL_TIMER_QUEUE: Mutex<KernelTimerQueue> =
        Mutex::new(KernelTimerQueue::new());
}

static TIMEOUT_WAITQUEUE_PENDING: AtomicBool = AtomicBool::new(false);
static KERNEL_TIMER_QUEUE_PENDING: AtomicBool = AtomicBool::new(false);
static TIMEOUT_WAITQUEUE_NEXT_NS: AtomicU64 = AtomicU64::new(0);
static KERNEL_TIMER_QUEUE_NEXT_NS: AtomicU64 = AtomicU64::new(0);

// 每 CPU 使用同一周期，但各自维护绝对 deadline，避免多核共同推进一个 tick。
const SCHED_TICK_NS: u64 = 10_000_000; // 100 Hz = 10 ms

/// 按当前 CPU 的职责重编程本地 one-shot timer。
///
/// AP 只考虑自己的调度 tick；CPU0 额外承担全局 timer queue，并选择两者中
/// 更早的 deadline。调用方必须已经关闭本地中断，且不能持有 timer queue 锁。
fn rearm_local_timer() {
    let now_ns = crate::timer::now_ns();
    let mut next_ns = crate::smp::local_sched_tick_deadline();
    if crate::smp::cpu_id() == crate::smp::BOOT_CPU_ID {
        let global_deadline = KERNEL_TIMER_QUEUE.lock().earliest_deadline_ns();
        if global_deadline != 0 {
            next_ns = next_ns.min(global_deadline);
        }
    }
    next_ns = next_ns.max(now_ns.saturating_add(1));
    let delta_ns = next_ns.saturating_sub(now_ns).max(1);
    let delta_ticks = crate::timer::ns_to_ticks_ceil(delta_ns);
    crate::hal::program_timer_delta(delta_ticks);
}

/// 初始化当前 CPU 的本地调度 timer。
///
/// 必须先发布未来 deadline、写入硬件 compare，再开放 timer source；否则旧的
/// pending 电平可能在调度状态尚未就绪时进入 trap。
pub fn timer_cpu_init() {
    let flags = local_irq_save();
    let now_ns = crate::timer::now_ns();
    crate::smp::init_local_sched_tick(now_ns.saturating_add(SCHED_TICK_NS));
    rearm_local_timer();
    crate::hal::enable_local_timer_interrupt();
    local_irq_restore(flags);
}

/// 添加一个内核定时器动作。
///
/// # Locking
///
/// 函数会短暂关闭本地中断并持有 `KERNEL_TIMER_QUEUE` 锁；callback 不在此处执行。
pub fn add_kernel_timer(action: TimerAction, deadline: TimeSpec) {
    let flags = local_irq_save();
    let (new_is_earliest, timer_len) = {
        let mut queue = KERNEL_TIMER_QUEUE.lock();
        let new_is_earliest = queue.add_action(action, deadline);
        (new_is_earliest, queue.len())
    };
    crate::task::perf::record_ktimer_add();
    crate::task::perf::record_ktimer_len(timer_len);
    if new_is_earliest {
        if crate::smp::cpu_id() == crate::smp::BOOT_CPU_ID {
            if !crate::smp::local_timer_pending() {
                // hard IRQ 已把 one-shot 静默时，当前安全点会按完整队列重编程。
                rearm_local_timer();
            }
        } else {
            // 全局队列由 CPU0 驱动；AP 不能错误地把自己的 timer 指向该 deadline。
            crate::smp::request_timer_reprogram();
        }
    }
    local_irq_restore(flags);
}

/// 为任务注册一次绝对超时唤醒。
///
/// # Semantics
///
/// 只添加 timer，不阻塞任务；调用方必须随后进入对应 WaitQueue 等待。每次注册
/// 都会递增 `wait_timer_generation`，旧 timer 到期后会被识别为 stale。
pub fn wait_with_timeout(task: Weak<TaskControlBlock>, timeout: TimeSpec) {
    crate::task::perf::record_wait_with_timeout();
    let Some(task) = task.upgrade() else {
        return;
    };
    let generation = task
        .wait_timer_generation
        .fetch_add(1, AtomicOrdering::Relaxed)
        .wrapping_add(1);
    add_kernel_timer(
        TimerAction::WakeTask {
            task: Arc::downgrade(&task),
            generation,
        },
        timeout,
    );
}

/// timer hard-IRQ fast path。
///
/// # Semantics
///
/// 只清除/静默本 CPU 的 one-shot timer 并发布 per-CPU pending。这里不能获取
/// timer、task、网络或文件系统锁，也不能执行 callback 或 context switch。
pub fn timer_interrupt_handler() {
    let irq_start = crate::task::perf::perf_time_now();
    crate::hal::quiesce_local_timer_interrupt();
    crate::smp::publish_local_timer_interrupt();
    crate::task::perf::record_timer_irq_cost(irq_start);
}

/// 在关中断的任务返回或 scheduler idle 安全点处理一批 timer 工作。
///
/// 多个 hard IRQ 可以合并为一批：所有 timer action 和调度 tick 都按绝对
/// deadline 与当前时间比较，不依赖中断次数。函数返回当前任务是否应在安全点
/// 让出 CPU；idle scheduler 调用时已经处于调度上下文，可以忽略该返回值。
pub fn run_deferred_timer_work() -> bool {
    let irq_flags = local_irq_save();
    let timer_fired = crate::smp::take_local_timer_pending();
    let reprogram_requested = crate::smp::take_timer_reprogram_request();
    if !timer_fired && !reprogram_requested {
        local_irq_restore(irq_flags);
        return false;
    }

    let handler_profile_start = crate::task::processor::sched_profile_cycle_start();
    let now = crate::timer::TimeSpec::now();
    let now_ns = now.to_ns_saturating();
    let is_boot_cpu = crate::smp::cpu_id() == crate::smp::BOOT_CPU_ID;

    let mut woke_task = false;
    if is_boot_cpu {
        // 全局 callback 只由 CPU0 执行。AP 即使产生调度 tick，也不能进入
        // timeout、timerfd、网络或其他尚未完成 SMP 审计的共享路径。
        let expired_timers = { KERNEL_TIMER_QUEUE.lock().pop_expired(now) };
        for timer in expired_timers {
            if KernelTimerQueue::run_timer(timer, now) {
                woke_task = true;
            }
        }

        // legacy timeout queue 没有独立硬件 deadline，仍由 CPU0 的本地 tick 扫描。
        if TIMEOUT_WAITQUEUE_PENDING.load(AtomicOrdering::Relaxed) {
            let mut timeout_queue = TIMEOUT_WAITQUEUE.lock();
            if !timeout_queue.is_empty() {
                timeout_queue.wake_expired(now);
            } else {
                TIMEOUT_WAITQUEUE_PENDING.store(false, AtomicOrdering::Relaxed);
            }
        }

        if crate::fs::timerfd::timerfd_registry_maybe_nonempty()
            && !crate::fs::timerfd::timerfd_registry_is_empty()
            && crate::fs::timerfd::wake_expired_timerfds(now) > 0
        {
            woke_task = true;
        }
    }

    // 每个 CPU 独立推进自己的调度 tick；CPU0 只发布网络 generation，真实 poll
    // 由固定在 CPU0 的 worker 在任务上下文执行。
    let need_resched = crate::smp::advance_local_sched_tick(now_ns, SCHED_TICK_NS);
    if need_resched && is_boot_cpu {
        // scheduler tick 可能在任务安全点被消费；把周期维护请求单独交给随后
        // 恢复的 CPU0 idle 栈，避免 pending timer 清零后漏掉 housekeeping。
        crate::task::processor::request_boot_housekeeping();
        crate::net::config::NET_INTERFACE.try_poll_irq();
        // hard IRQ 只设置 deferred_wake；到这个 task/idle 安全点才允许获取
        // WaitQueue 并唤醒 worker。
        crate::net::config::NET_INTERFACE.run_deferred_net_wake();
    }

    rearm_local_timer();
    if timer_fired {
        crate::smp::complete_local_timer_deferred();
    }
    crate::task::processor::record_sched_timer_handler_cycles(handler_profile_start);
    crate::task::perf::record_deferred_timer_snapshot();
    local_irq_restore(irq_flags);
    need_resched || woke_task
}

/// 在任务现场完整、业务锁均已释放的安全点合并调度请求。
///
/// timer 与 RESCHEDULE IPI 可能同时要求让出 CPU；先完成 timer callback，再以
/// Acquire 取走本 CPU 的 IPI 提示，最后最多调度一次。整个判定窗口保持
/// IRQ-off，因而消费后到真正切换前不会有本地 handler 插入并丢失新请求。
pub fn run_task_safe_point() {
    let irq_was_enabled = crate::hal::local_irq_save();
    let task = current_task();
    let stop = task.as_ref().map(|task| {
        (
            task.process.group_exit_code(),
            task.process.thread_must_exit(task.gettid()),
        )
    });
    let exit_code = match stop {
        Some((Some(exit_code), _)) => Some(exit_code),
        Some((None, true)) => Some(Signals::SIGKILL.to_signum().unwrap() as u32),
        _ => None,
    };
    if let Some(exit_code) = exit_code {
        // exit 入口统一负责建立可响应 IPI 的清理窗口；安全点不再复制一套
        // “入口 IRQ 是否开启”的分支。noreturn 调度不会展开当前内核栈，
        // 因此必须先释放安全点刚克隆的 current Arc。
        drop(task);
        super::exit_current_and_run_next(exit_code);
    }
    if let Some(task) = task.as_ref() {
        if let Some(signal) = task.process.take_cpu_limit_signal() {
            let _ = queue_kernel_process_signal(&task.process, signal);
        }
        // trap 出口已经把本次 user/system 时间结算到 TCB/PCB；在同一个
        // 安全点领取两类 CPU timer，保证到期信号能紧接着由 do_signal() 处理。
        task.process.check_interval_cpu_timers();
        task.process.check_posix_cpu_timers(task);
    }
    // 后续可能 context switch；不能把 current 的 Arc 带过 schedule。
    drop(task);
    let timer_resched = run_deferred_timer_work();
    let ipi_resched = crate::smp::take_reschedule_request();
    if timer_resched || ipi_resched {
        crate::task::suspend_current_and_run_next();
    }
    crate::hal::local_irq_restore(irq_was_enabled);
}

/// 唤醒已过期的 legacy timeout/kernel timer。
///
/// # Semantics
///
/// 这是旧固定 tick 路径的兼容入口；硬件 IRQ 只发布 pending，完整处理主要走
/// `run_deferred_timer_work()`。
pub fn do_wake_expired() {
    let timeout_pending = TIMEOUT_WAITQUEUE_PENDING.load(AtomicOrdering::Relaxed);
    let kernel_timer_pending = KERNEL_TIMER_QUEUE_PENDING.load(AtomicOrdering::Relaxed);
    if !timeout_pending && !kernel_timer_pending {
        return;
    }

    let now_ns = crate::timer::now_ns();
    let timeout_next_ns = TIMEOUT_WAITQUEUE_NEXT_NS.load(AtomicOrdering::Relaxed);
    let kernel_next_ns = KERNEL_TIMER_QUEUE_NEXT_NS.load(AtomicOrdering::Relaxed);
    let timeout_due = timeout_pending && (timeout_next_ns == 0 || now_ns >= timeout_next_ns);
    let kernel_due = kernel_timer_pending && (kernel_next_ns == 0 || now_ns >= kernel_next_ns);
    if !timeout_due && !kernel_due {
        return;
    }

    let mut now = Some(crate::timer::TimeSpec::from_ns(now_ns as usize));
    if timeout_due {
        let flags = local_irq_save();
        let mut timeout_queue = TIMEOUT_WAITQUEUE.lock();
        if timeout_queue.is_empty() {
            TIMEOUT_WAITQUEUE_PENDING.store(false, AtomicOrdering::Relaxed);
            TIMEOUT_WAITQUEUE_NEXT_NS.store(0, AtomicOrdering::Relaxed);
        } else {
            let now = *now.get_or_insert_with(crate::timer::TimeSpec::now);
            timeout_queue.wake_expired(now);
        }
        drop(timeout_queue);
        local_irq_restore(flags);
    }

    if kernel_due {
        let expired_timers = {
            let flags = local_irq_save();
            let mut ktq = KERNEL_TIMER_QUEUE.lock();
            let expired = if ktq.is_empty() {
                KERNEL_TIMER_QUEUE_PENDING.store(false, AtomicOrdering::Relaxed);
                KERNEL_TIMER_QUEUE_NEXT_NS.store(0, AtomicOrdering::Relaxed);
                Vec::new()
            } else {
                let now = *now.get_or_insert_with(crate::timer::TimeSpec::now);
                let expired = ktq.pop_expired(now);
                // compact 涉及 BinaryHeap::drain → Vec 堆分配，降频执行：
                static COMPACT_TICK: core::sync::atomic::AtomicUsize =
                    core::sync::atomic::AtomicUsize::new(0);
                let tick = COMPACT_TICK.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                if tick % 64 == 0 && ktq.len() > KernelTimerQueue::MAX_TIMERS / 2 {
                    ktq.compact();
                }
                if ktq.is_empty() {
                    KERNEL_TIMER_QUEUE_PENDING.store(false, AtomicOrdering::Relaxed);
                    KERNEL_TIMER_QUEUE_NEXT_NS.store(0, AtomicOrdering::Relaxed);
                }
                expired
            };
            drop(ktq);
            local_irq_restore(flags);
            expired
        };

        // callback 可能重新获取 timer/task/scheduler 锁，必须在锁外执行。
        let now = now.unwrap_or_else(crate::timer::TimeSpec::now);
        for timer in expired_timers {
            KernelTimerQueue::run_timer(timer, now);
        }
    }
}

/// 获取内核计时器队列长度（诊断用，尝试获取锁）
pub fn kernel_timer_queue_len() -> Option<usize> {
    KERNEL_TIMER_QUEUE.try_lock().map(|q| q.len())
}

/// 获取任务管理器中就绪和可中断任务数量（诊断用，尝试获取锁）
pub fn task_manager_counts() -> Option<(u16, u16)> {
    Some((ready_count_fast(), interruptible_count_fast()))
}

/// 返回所有活跃的 PID 列表
pub fn all_pids() -> alloc::vec::Vec<usize> {
    super::registry::all_processes()
        .into_iter()
        .map(|process| process.pid)
        .collect()
}
