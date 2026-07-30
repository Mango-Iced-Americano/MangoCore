//! 任务调度队列、等待队列和内核定时器。
//!
//! runnable 任务由 `PerCpu` 内的 `RunQueue` 管理；本模块保留 interruptible、
//! zombie 和 timer 等全局 registry。`WaitQueue` 为文件、futex、信号和计时器
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
use crate::timer::{TimeSpec, TimeVal};

use super::{
    block_current_and_run_next_checked, block_current_and_run_next_with_lock_checked, current_task,
    discard_non_actionable_unblocked_signals, has_actionable_signal,
    signal::{SigInfo, Signals},
    TaskControlBlock, TaskStatus,
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
/// 全局等待与回收 registry。
pub struct TaskManager {
    /// 可中断睡眠任务队列。
    pub interruptible_queue: VecDeque<Arc<TaskControlBlock>>,
    zombie_queue: VecDeque<Arc<TaskControlBlock>>,
    /// 任务激活状态跟踪器，用于跟踪任务的激活状态，并在OOM时释放内存
    pub active_tracker: ActiveTracker,
}

#[cfg(not(feature = "oom_handler"))]
/// 全局等待与回收 registry。
pub struct TaskManager {
    /// 可中断睡眠任务队列。
    pub interruptible_queue: VecDeque<Arc<TaskControlBlock>>,
    zombie_queue: VecDeque<Arc<TaskControlBlock>>,
}

fn task_ptr_eq(left: &Arc<TaskControlBlock>, right: &Arc<TaskControlBlock>) -> bool {
    Arc::as_ptr(left) == Arc::as_ptr(right)
}

fn task_ptr(task: &Arc<TaskControlBlock>) -> usize {
    Arc::as_ptr(task) as usize
}

fn sorted_task_ptrs(tasks: &[Arc<TaskControlBlock>]) -> Vec<usize> {
    let mut ptrs: Vec<usize> = tasks.iter().map(task_ptr).collect();
    ptrs.sort_unstable();
    ptrs.dedup();
    ptrs
}

fn task_ptr_in(ptrs: &[usize], task: &Arc<TaskControlBlock>) -> bool {
    ptrs.binary_search(&task_ptr(task)).is_ok()
}

static INTERRUPTIBLE_TASK_COUNT: AtomicUsize = AtomicUsize::new(0);
static ZOMBIE_QUEUE_COUNT: AtomicUsize = AtomicUsize::new(0);

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

fn add_zombie_queue_count() {
    ZOMBIE_QUEUE_COUNT.fetch_add(1, AtomicOrdering::Relaxed);
}

fn sub_zombie_queue_count(count: usize) {
    if count != 0 {
        let _ = ZOMBIE_QUEUE_COUNT.fetch_update(
            AtomicOrdering::Relaxed,
            AtomicOrdering::Relaxed,
            |value| Some(value.saturating_sub(count)),
        );
    }
}

fn has_zombie_queue_tasks() -> bool {
    ZOMBIE_QUEUE_COUNT.load(AtomicOrdering::Relaxed) != 0
}

/// 无锁判断显式 zombie 队列是否可能非空。
pub fn has_zombie_queue_tasks_fast() -> bool {
    has_zombie_queue_tasks()
}

/// 返回专用 zombie 回收队列的无锁近似长度。
///
/// 精确取出仍必须经过 `TASK_MANAGER` 锁；该值只用于调度诊断以及等待 idle
/// 已完成 current→zombie 所有权交接，不能据此直接释放任何 TCB。
pub(crate) fn zombie_queue_count_fast() -> usize {
    ZOMBIE_QUEUE_COUNT.load(AtomicOrdering::Acquire)
}

/// 全局等待、回收与定时器 registry。
impl TaskManager {
    #[cfg(feature = "oom_handler")]
    /// 构造函数
    pub fn new() -> Self {
        Self {
            interruptible_queue: VecDeque::new(),
            zombie_queue: VecDeque::new(),
            active_tracker: ActiveTracker::new(),
        }
    }
    #[cfg(not(feature = "oom_handler"))]
    pub fn new() -> Self {
        Self {
            interruptible_queue: VecDeque::new(),
            zombie_queue: VecDeque::new(),
        }
    }
    /// 从可中断队列中逐出一个僵尸任务（零堆分配）。
    fn take_one_interruptible_zombie(&mut self) -> Option<Arc<TaskControlBlock>> {
        for i in 0..self.interruptible_queue.len() {
            if self.interruptible_queue[i].is_zombie() {
                let zombie = self.interruptible_queue.remove(i);
                if zombie.is_some() {
                    sub_interruptible_count(1);
                }
                return zombie;
            }
        }
        None
    }
    fn enqueue_zombie(&mut self, task: Arc<TaskControlBlock>) {
        super::perf::record_zombie_enqueue();
        self.zombie_queue.push_back(task);
        add_zombie_queue_count();
    }
    fn take_one_zombie(&mut self) -> Option<Arc<TaskControlBlock>> {
        let zombie = self.zombie_queue.pop_front();
        if zombie.is_some() {
            sub_zombie_queue_count(1);
        }
        zombie
    }
    fn take_zombies(&mut self, limit: usize) -> Vec<Arc<TaskControlBlock>> {
        let mut zombies = Vec::with_capacity(limit.min(self.zombie_queue.len()));
        while zombies.len() < limit {
            let Some(task) = self.zombie_queue.pop_front() else {
                break;
            };
            zombies.push(task);
        }
        sub_zombie_queue_count(zombies.len());
        zombies
    }
    /// 从全局等待/回收 registry 移除属于指定 pid 的所有 zombie TCB。
    /// 返回收集到的 zombie Arc，由调用者负责在锁外 drop。
    fn remove_zombie_tasks_by_pid(&mut self, pid: usize) -> alloc::vec::Vec<Arc<TaskControlBlock>> {
        let mut zombies = alloc::vec::Vec::new();
        let old_interruptible_len = self.interruptible_queue.len();
        self.interruptible_queue.retain(|task| {
            let is_match = task.is_zombie() && task.process.pid == pid;
            if is_match {
                zombies.push(task.clone());
                false
            } else {
                true
            }
        });
        sub_interruptible_count(old_interruptible_len - self.interruptible_queue.len());
        let old_zombie_len = self.zombie_queue.len();
        self.zombie_queue.retain(|task| {
            if task.process.pid == pid {
                zombies.push(task.clone());
                false
            } else {
                true
            }
        });
        sub_zombie_queue_count(old_zombie_len - self.zombie_queue.len());
        zombies
    }

    /// 添加一个任务到可中断队列。
    pub fn begin_interruptible_sleep(&mut self, task: Arc<TaskControlBlock>) {
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
        self.interruptible_queue.push_back(task);
        crate::task::perf::record_taskq_add_interruptible();
        add_interruptible_count();
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
    /// 从 interruptible registry 中移除一组任务。
    fn remove_interruptible_tasks(&mut self, tasks: &[Arc<TaskControlBlock>]) -> usize {
        let ptrs = sorted_task_ptrs(tasks);
        let old_interruptible_len = self.interruptible_queue.len();
        self.interruptible_queue
            .retain(|task| !task_ptr_in(&ptrs, task));
        let removed_interruptible = old_interruptible_len - self.interruptible_queue.len();
        sub_interruptible_count(removed_interruptible);
        removed_interruptible
    }
    /// 可中断队列中任务数量
    pub fn interruptible_count(&self) -> u16 {
        self.interruptible_queue.len() as u16
    }
    /// 可中断 registry 中尚未清理的僵尸任务数量。
    pub fn interruptible_zombie_count(&self) -> u16 {
        self.interruptible_queue
            .iter()
            .filter(|task| task.is_zombie())
            .count()
            .min(u16::MAX as usize) as u16
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
                task.tid.0,
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
    super::run_queue::select_runnable_cpu(task.cpus_allowed(), Some(last_cpu))
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

/// 首次发布一个新任务到指定 CPU。
///
/// 内核栈映射必须在 runqueue 可见之前同步到目标 CPU；
/// 远程 doorbell 则必须在 runqueue 锁释放后发送。
pub(crate) fn publish_task_on(task: Arc<TaskControlBlock>, cpu: usize) {
    assert!(cpu < crate::smp::configured_cpu_count());
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
    super::run_queue::publish(task, cpu);
    if cpu != crate::smp::cpu_id() {
        crate::smp::request_reschedule(cpu).unwrap_or_else(|error| {
            panic!("failed to wake CPU {} after remote enqueue: {}", cpu, error)
        });
    }
}

/// 按任务 affinity 和当前负载发布普通新任务。
///
/// clone/fork 已继承父线程 mask，不能再无条件投递 CPU0。调用 CPU 仅作为
/// locality 提示；若它不在 mask 中或明显过载，选择器会返回其它合法 CPU。
pub fn publish_task(task: Arc<TaskControlBlock>) {
    // 启动期的 init/ktest runner 在 CPU0 首次进入 run_tasks() 前发布，此时
    // 本 CPU 还没有 current，scheduler-entered mask 也尚未包含 bit0。
    // 这条一次性 bootstrap 路径保持显式 CPU0；普通 clone 均有 current。
    if super::processor::current_task().is_none() {
        publish_task_on(task, crate::smp::BOOT_CPU_ID);
        return;
    }
    let target = super::run_queue::select_runnable_cpu(
        task.cpus_allowed(),
        Some(crate::smp::cpu_id()),
    );
    publish_task_on(task, target)
}

/// 在 CPU 已切回 idle 栈后完成上一任务的状态和容器交接。
pub fn finish_switch_out(task: Arc<TaskControlBlock>, cpu: usize) {
    loop {
        match task.task_status() {
            TaskStatus::Running(owner) if owner == cpu => {
                let target = task.take_migration_target().unwrap_or(cpu);
                super::run_queue::requeue_after_switch(task, cpu, target);
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
                TASK_MANAGER.lock().enqueue_zombie(task);
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
    let task = super::run_queue::fetch(cpu)?;
    #[cfg(feature = "oom_handler")]
    TASK_MANAGER.lock().active_tracker.mark_active(task.tid.0);
    Some(task)
}

/// 从显式 zombie 队列取出一个任务。
pub fn take_one_zombie_task() -> Option<Arc<TaskControlBlock>> {
    if !has_zombie_queue_tasks() {
        return None;
    }
    TASK_MANAGER.lock().take_one_zombie()
}

/// 从显式 zombie 队列批量取出任务。
pub fn take_zombie_tasks(limit: usize) -> Vec<Arc<TaskControlBlock>> {
    if limit == 0 || !has_zombie_queue_tasks() {
        return Vec::new();
    }
    TASK_MANAGER.lock().take_zombies(limit)
}

/// 从可中断队列中逐出一个僵尸任务（零堆分配），返回后在锁外 drop。
pub fn take_one_interruptible_zombie() -> Option<Arc<TaskControlBlock>> {
    TASK_MANAGER.lock().take_one_interruptible_zombie()
}

/// 从 interruptible 和专用 zombie registry 移除指定 pid 的所有 zombie TCB，
/// 在锁外 drop，避免 TCB::drop() 在持有 TASK_MANAGER 锁时执行析构链。
pub fn remove_zombie_tasks_by_pid(pid: usize) {
    let zombies = TASK_MANAGER.lock().remove_zombie_tasks_by_pid(pid);
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
                .find(|task| manager.active_tracker.check_active(task.tid.0))
                .cloned();
            if let Some(task) = task.as_ref() {
                manager.active_tracker.mark_inactive(task.tid.0);
            }
            task
        };
        let Some(task) = task else {
            break;
        };
        let released = task.process.vm().write(|vm| vm.do_deep_clean());
        log::warn!(
            "deep clean on task: tid {}, pid {}, released: {}",
            task.tid.0,
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
                if manager.active_tracker.check_active(task.tid.0) {
                    manager.active_tracker.mark_inactive(task.tid.0);
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
                task.tid.0,
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
    TASK_MANAGER.lock().begin_interruptible_sleep(task);
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

/// 更新非 current 任务 affinity；当前支持稳定 Blocked 与 Queued。
///
/// Blocked 由 `TASK_MANAGER` 与 wake 串行化；Queued 由 owner runqueue 与 fetch
/// 串行化，必要时经过短暂 `Migrating` 搬队。两条路径都在锁释放后才发送
/// RESCHEDULE。远程 Running/Blocking 仍由后续的 CPU 停止协议处理。
pub(crate) fn set_remote_affinity(task: &Arc<TaskControlBlock>, mask: usize) -> bool {
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
            _ => return false,
        }
    }
}

/// 从调度队列中移除一组任务。
pub fn remove_tasks_from_queues(tasks: &[Arc<TaskControlBlock>]) -> usize {
    // 固定 TASK_MANAGER -> 单个 RunQueue 的锁序，避免从 rq 摘除后扩大无锁的
    // Blocked 退出窗口。Migrating 后不会反向取得 TASK_MANAGER 或等待 IPI ack，
    // 因此 remove() 即使重定位 owner，也只等待两个短 runqueue 临界区。
    let mut manager = TASK_MANAGER.lock();
    let mut removed = manager.remove_interruptible_tasks(tasks);
    for task in tasks {
        removed += usize::from(super::run_queue::remove(task));
    }
    removed
}

/// 返回 ready + interruptible 队列计数的近似值。
pub fn procs_count() -> u16 {
    ready_count_fast().saturating_add(interruptible_count_fast())
}

/// 无锁判断 ready 队列是否非空。
pub fn has_ready_task() -> bool {
    super::run_queue::total_count_fast() != 0
}

/// 返回 ready/interruptible 队列中的 zombie 任务数量。
pub fn zombie_count() -> u16 {
    let manager = TASK_MANAGER.lock();
    let mut count = manager.interruptible_zombie_count();
    drop(manager);
    for cpu in 0..crate::smp::configured_cpu_count() {
        count = count.saturating_add(super::run_queue::stats(cpu).1 as u16);
    }
    count
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

/// 弱引用等待队列。
///
/// # Semantics
///
/// 队列只保存 `Weak<TaskControlBlock>`，不会延长任务生命周期。等待者必须先在
/// 关联对象的锁内检查条件，再调用 `prepare_to_wait()`，最后通过
/// `block_current_and_run_next_*` 让出 CPU。
///
/// # Locking
///
/// `wake_*` 会读取原子调度状态并操作 `TASK_MANAGER`，但不会获取
/// `task.inner`。调用方不得在持有调度器锁时调用唤醒函数。
pub struct WaitQueue {
    inner: VecDeque<Weak<TaskControlBlock>>,
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
        self.inner.push_back(task);
    }

    /// 弹出一个等待者但不唤醒。
    pub fn pop_task(&mut self) -> Option<Weak<TaskControlBlock>> {
        self.inner.pop_front()
    }

    /// 判断等待队列是否包含给定任务弱引用。
    pub fn contains(&self, task: &Weak<TaskControlBlock>) -> bool {
        self.inner
            .iter()
            .any(|task_in_queue| Weak::as_ptr(task_in_queue) == Weak::as_ptr(task))
    }

    /// 判断等待队列是否为空。
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
    /// 清理所有失效 `Weak` 条目，返回清理数量。
    pub fn compact_stale(&mut self) -> usize {
        let before = self.inner.len();
        self.inner.retain(|task| task.strong_count() > 0);
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
        // 遍历全部条目以自动 compact 失效 Weak，但只唤醒 ≤limit 个任务。
        while let Some(task) = self.inner.pop_front() {
            match task.upgrade() {
                Some(task) => match task.task_status() {
                    super::TaskStatus::Blocking(_) | super::TaskStatus::Blocked => {
                        if wake_count < limit {
                            task.wait_timer_generation
                                .fetch_add(1, AtomicOrdering::Relaxed);
                            wake_count += 1;
                            tasks_to_wake.push(task);
                        } else {
                            remaining.push_back(Arc::downgrade(&task));
                        }
                    }
                    super::TaskStatus::Queued(_)
                    | super::TaskStatus::Migrating
                    | super::TaskStatus::Running(_) => {
                        if wake_count < limit {
                            wake_count += 1;
                            task.wait_timer_generation
                                .fetch_add(1, AtomicOrdering::Relaxed);
                        } else {
                            remaining.push_back(Arc::downgrade(&task));
                        }
                    }
                    // New/Zombie 不应继续停留在等待队列中，直接丢弃。
                    _ => {}
                },
                None => {}
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
        while let Some(waiter) = self.inner.pop_front() {
            let task = match waiter.upgrade() {
                Some(task) => task,
                None => continue,
            };
            match task.task_status() {
                super::TaskStatus::Blocking(_) | super::TaskStatus::Blocked => {
                    task.wait_timer_generation
                        .fetch_add(1, AtomicOrdering::Relaxed);
                    let _ = wake_interruptible(task);
                    return 1;
                }
                super::TaskStatus::Queued(_)
                | super::TaskStatus::Migrating
                | super::TaskStatus::Running(_) => {
                    task.wait_timer_generation
                        .fetch_add(1, AtomicOrdering::Relaxed);
                    return 1;
                }
                _ => {}
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
    pub fn prepare_to_wait(&mut self, task: Weak<TaskControlBlock>) {
        if let Some(task) = task.upgrade() {
            self.add_task(Arc::downgrade(&task));
        }
    }

    /// 从条件等待队列移除任务。调度状态已经由 wake 或重新切入路径处理，
    /// 这里不能再单独修改状态。
    ///
    /// 返回值表示该任务是否仍在队列中。若返回 `false`，通常说明它已经被
    /// 正常唤醒路径移除。
    pub fn finish_wait(&mut self, task: &TaskControlBlock) -> bool {
        let task_ptr = task as *const TaskControlBlock;
        let removed = if self
            .inner
            .back()
            .map(|task_in_queue| Weak::as_ptr(task_in_queue) == task_ptr)
            .unwrap_or(false)
        {
            self.inner.pop_back();
            true
        } else if self
            .inner
            .front()
            .map(|task_in_queue| Weak::as_ptr(task_in_queue) == task_ptr)
            .unwrap_or(false)
        {
            self.inner.pop_front();
            true
        } else {
            let old_len = self.inner.len();
            self.inner
                .retain(|task_in_queue| Weak::as_ptr(task_in_queue) != task_ptr);
            self.inner.len() != old_len
        };
        removed
    }

    /// 兜底定时器的超时毫秒数，防止丢失唤醒导致永久阻塞。
    const WAIT_IO_FALLBACK_MS: usize = 10;

    fn wait_event_impl<F>(
        wq: &Mutex<Self>,
        cond: &mut F,
        signal_check: bool,
        deadline: Option<TimeSpec>,
        fallback_ms: Option<usize>,
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

            let mut guard = wq.lock();
            guard.prepare_to_wait(Arc::downgrade(&task));

            if let Some(res) = cond() {
                guard.finish_wait(task.as_ref());
                return WaitResult::Ready(res);
            }
            if deadline
                .map(|deadline| TimeSpec::now() >= deadline)
                .unwrap_or(false)
            {
                guard.finish_wait(task.as_ref());
                return WaitResult::TimedOut;
            }
            if signal_check {
                // 必须在不持有 task.inner 的情况下检查 actionable signal；
                // 这里仅持有等待队列锁，`has_actionable_signal` 自行短暂取任务锁。
                if has_actionable_signal(&task) {
                    guard.finish_wait(task.as_ref());
                    return WaitResult::Interrupted;
                }
                discard_non_actionable_unblocked_signals(&task);
            }

            if let Some(deadline) = deadline {
                wait_with_timeout(Arc::downgrade(&task), deadline);
            } else if let Some(ms) = fallback_ms {
                if !task
                    .wait_io_timer_pending
                    .swap(true, AtomicOrdering::Relaxed)
                {
                    // I/O fallback timer: arm with fallback_ms set so stale
                    // fallback timers can be detected and re-armed in run_timer().
                    // Using add_kernel_timer directly instead of wait_with_timeout
                    // because wait_with_timeout always sets fallback_ms to None,
                    // which causes stale fallback timers to be silently dropped
                    // instead of re-armed, leading to permanent task blockage.
                    let generation = task
                        .wait_timer_generation
                        .fetch_add(1, AtomicOrdering::Relaxed)
                        .wrapping_add(1);
                    add_kernel_timer(
                        TimerAction::WakeTask {
                            task: Arc::downgrade(&task),
                            generation,
                            fallback_ms: Some(ms),
                        },
                        TimeSpec::now() + TimeSpec::from_ms(ms),
                    );
                }
                // Record the generation for stale-timer detection.
                // Always set (even when pending was already true), so that
                // stale fallback timers know the task is still in a
                // fallback wait and can re-arm with current generation.
                let gen = task.wait_timer_generation.load(AtomicOrdering::Relaxed);
                task.wait_io_fallback_active_generation
                    .store(gen, AtomicOrdering::Release);
            }
            drop(task);

            block_current_and_run_next_with_lock_checked(guard, |task| {
                let no_signal = !signal_check || !has_actionable_signal(task);
                let not_timed_out = deadline
                    .map(|deadline| TimeSpec::now() < deadline)
                    .unwrap_or(true);
                no_signal && not_timed_out
            });

            let task = current_task().unwrap();
            wq.lock().finish_wait(&task);
            task.wait_io_fallback_active_generation
                .store(0, AtomicOrdering::Release);
            task.acquire_inner_lock().refresh_real_timer();
        }
    }

    fn wait_event_locked_impl<T, Q, F>(
        lock: &Mutex<T>,
        mut queue_of: Q,
        cond: &mut F,
        signal_check: bool,
        deadline: Option<TimeSpec>,
        normal_wake_result: Option<isize>,
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

            queue_of(&mut guard).prepare_to_wait(Arc::downgrade(&task));
            if let Some(res) = cond(&mut guard) {
                queue_of(&mut guard).finish_wait(task.as_ref());
                return WaitResult::Ready(res);
            }
            if deadline
                .map(|deadline| TimeSpec::now() >= deadline)
                .unwrap_or(false)
            {
                queue_of(&mut guard).finish_wait(task.as_ref());
                return WaitResult::TimedOut;
            }
            if signal_check {
                // 持有的是调用方传入的对象锁，不能同时长期持有 task.inner。
                if has_actionable_signal(&task) {
                    queue_of(&mut guard).finish_wait(task.as_ref());
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
                no_signal && not_timed_out
            });

            let task = current_task().unwrap();
            let mut guard = lock.lock();
            let removed = queue_of(&mut guard).finish_wait(&task);
            drop(guard);
            task.acquire_inner_lock().refresh_real_timer();

            if !removed {
                if let Some(res) = normal_wake_result {
                    return WaitResult::Ready(res);
                }
            }
        }
    }

    fn finish_wait_on_queues(queues: &[&Mutex<Self>], task: &TaskControlBlock) {
        for queue in queues {
            queue.lock().finish_wait(task);
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
            let wait_queue = Mutex::new(WaitQueue::new());
            loop {
                let next_deadline = match deadline {
                    Some(deadline) if TimeSpec::now() >= deadline => return WaitResult::TimedOut,
                    Some(deadline) => {
                        let fallback =
                            TimeSpec::now() + TimeSpec::from_ms(Self::WAIT_IO_FALLBACK_MS);
                        if fallback < deadline {
                            fallback
                        } else {
                            deadline
                        }
                    }
                    None => TimeSpec::now() + TimeSpec::from_ms(Self::WAIT_IO_FALLBACK_MS),
                };
                match Self::wait_event_interruptible_timeout(&wait_queue, &mut cond, next_deadline)
                {
                    WaitResult::Ready(value) => return WaitResult::Ready(value),
                    WaitResult::Interrupted => return WaitResult::Interrupted,
                    WaitResult::TimedOut => {
                        if deadline
                            .map(|deadline| TimeSpec::now() >= deadline)
                            .unwrap_or(false)
                        {
                            return WaitResult::TimedOut;
                        }
                    }
                }
            }
        }

        loop {
            if deadline
                .map(|deadline| TimeSpec::now() >= deadline)
                .unwrap_or(false)
            {
                return WaitResult::TimedOut;
            }

            let task = current_task().unwrap();
            for queue in queues {
                queue.lock().prepare_to_wait(Arc::downgrade(&task));
            }

            if let Some(res) = cond() {
                Self::finish_wait_on_queues(queues, task.as_ref());
                return WaitResult::Ready(res);
            }
            if deadline
                .map(|deadline| TimeSpec::now() >= deadline)
                .unwrap_or(false)
            {
                Self::finish_wait_on_queues(queues, task.as_ref());
                return WaitResult::TimedOut;
            }
            if has_actionable_signal(&task) {
                Self::finish_wait_on_queues(queues, task.as_ref());
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
                no_signal && not_timed_out && cond().is_none()
            });

            let task = current_task().unwrap();
            Self::finish_wait_on_queues(queues, &task);
            task.acquire_inner_lock().refresh_real_timer();
        }
    }

    /// 不可中断等待，条件满足前一直阻塞。
    ///
    /// 等价于 DragonOS 的 `wait_until`（Uninterruptible）。
    /// 适用于内核内部确定性等待（无需信号检查的场景）。
    /// 文件和网络 IO 通用——`NET_INTERFACE.poll()` 等操作由调用者在 `cond` 闭包中处理。
    ///
    /// # Locking
    ///
    /// `cond` 会先在无锁快速路径执行一次，并在 `prepare_to_wait` 后持有该
    /// 等待队列锁再检查一次以闭合 lost-wakeup 窗口。因此条件闭包只能查询或
    /// 消费底层状态，禁止通知或再次获取同一个等待队列锁。
    pub fn wait_until<F>(wq: &Mutex<Self>, mut cond: F) -> isize
    where
        F: FnMut() -> Option<isize>,
    {
        match Self::wait_event_impl(wq, &mut cond, false, None, Some(Self::WAIT_IO_FALLBACK_MS)) {
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
        Self::wait_event_impl(wq, &mut cond, true, None, Some(Self::WAIT_IO_FALLBACK_MS))
    }

    /// I/O 等待（不可中断）。
    ///
    /// 等价于 DragonOS 的 `wait_until_io`。
    pub fn wait_until_io<F>(wq: &Mutex<Self>, mut cond: F) -> isize
    where
        F: FnMut() -> Option<isize>,
    {
        match Self::wait_event_impl(wq, &mut cond, false, None, Some(Self::WAIT_IO_FALLBACK_MS)) {
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
        Self::wait_event_impl(wq, &mut cond, true, None, Some(Self::WAIT_IO_FALLBACK_MS))
    }

    /// 可中断等待，不启用 fallback timer。
    pub fn wait_event_interruptible<F>(wq: &Mutex<Self>, mut cond: F) -> WaitResult
    where
        F: FnMut() -> Option<isize>,
    {
        Self::wait_event_impl(wq, &mut cond, true, None, None)
    }

    /// 不可中断等待直到条件满足或绝对 deadline 到达。
    pub fn wait_event_timeout<F>(wq: &Mutex<Self>, mut cond: F, deadline: TimeSpec) -> WaitResult
    where
        F: FnMut() -> Option<isize>,
    {
        Self::wait_event_impl(wq, &mut cond, false, Some(deadline), None)
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
        Self::wait_event_impl(wq, &mut cond, true, Some(deadline), None)
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
        Self::wait_event_locked_impl(lock, queue_of, &mut cond, true, None, None)
    }

    /// 在调用方对象锁下检查条件并注册不可中断等待。
    pub fn wait_event_locked<T, Q, F>(lock: &Mutex<T>, queue_of: Q, mut cond: F) -> WaitResult
    where
        Q: for<'a> FnMut(&'a mut T) -> &'a mut WaitQueue,
        F: FnMut(&mut T) -> Option<isize>,
    {
        Self::wait_event_locked_impl(lock, queue_of, &mut cond, false, None, None)
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
        Self::wait_event_locked_impl(lock, queue_of, &mut cond, true, Some(deadline), None)
    }

    /// 带正常唤醒返回值的 locked wait。
    ///
    /// # Semantics
    ///
    /// 如果任务从等待队列中被正常唤醒而 `cond` 尚未返回值，则返回
    /// `WaitResult::Ready(normal_wake_result)`。
    pub fn wait_event_interruptible_timeout_locked_with_wake_result<T, Q, F>(
        lock: &Mutex<T>,
        queue_of: Q,
        mut cond: F,
        deadline: Option<TimeSpec>,
        normal_wake_result: isize,
    ) -> WaitResult
    where
        Q: for<'a> FnMut(&'a mut T) -> &'a mut WaitQueue,
        F: FnMut(&mut T) -> Option<isize>,
    {
        Self::wait_event_locked_impl(
            lock,
            queue_of,
            &mut cond,
            true,
            deadline,
            Some(normal_wake_result),
        )
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
        /// Some(ms) when this is an I/O fallback timer (1ms safety net);
        /// None when this is a deadline timer.  Stale fallback timers are
        /// re-armed with the current generation instead of spurious-waking.
        fallback_ms: Option<usize>,
    },
    /// 向指定任务投递信号。
    SendSignal {
        task: Weak<TaskControlBlock>,
        signal: Signals,
        generation: usize,
    },
    // POSIX timer 到期后向创建线程投递信号。
    PosixTimerSignal {
        task: Weak<TaskControlBlock>,
        timer_id: usize,
        signal: Signals,
        generation: usize,
    },
    // Global timerfd sweep. Individual timerfds are kept in fs::timerfd's
    // registry; this action exists only to drive high-resolution wakeups.
    TimerFdSweep {
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
            TimerAction::SendSignal { task, .. } | TimerAction::PosixTimerSignal { task, .. } => {
                task.strong_count() > 0
            }
            TimerAction::TimerFdSweep { generation } => {
                crate::fs::timerfd::timerfd_sweep_is_current(*generation)
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
    /// 清理所有失效 Weak 引用的定时器条目，释放堆槽位。
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
            TimerAction::WakeTask {
                task,
                generation,
                fallback_ms,
            } => {
                let Some(task) = task.upgrade() else {
                    return false;
                };
                task.wait_io_timer_pending
                    .store(false, AtomicOrdering::Relaxed);

                if let Some(ms) = fallback_ms {
                    // I/O fallback timer — check if stale
                    let active = task
                        .wait_io_fallback_active_generation
                        .load(AtomicOrdering::Acquire);
                    if active == 0 {
                        return false; // task not in fallback wait, discard
                    }
                    if active != generation {
                        // Stale timer. If task is in a new fallback wait,
                        // re-arm with current generation instead of waking.
                        let current = task.wait_timer_generation.load(AtomicOrdering::Relaxed);
                        if active == current {
                            // Task waiting with new generation but no timer armed
                            if !task
                                .wait_io_timer_pending
                                .swap(true, AtomicOrdering::Relaxed)
                            {
                                let new_gen = task
                                    .wait_timer_generation
                                    .fetch_add(1, AtomicOrdering::Relaxed)
                                    + 1;
                                add_kernel_timer(
                                    TimerAction::WakeTask {
                                        task: Arc::downgrade(&task),
                                        generation: new_gen,
                                        fallback_ms: Some(ms),
                                    },
                                    TimeSpec::now() + TimeSpec::from_ms(ms),
                                );
                                task.wait_io_fallback_active_generation
                                    .store(new_gen, AtomicOrdering::Release);
                            }
                        }
                        return false; // Don't wake — stale timer
                    }
                    // active == generation: current fallback, wake normally
                    //
                    // 先确认任务已经登记阻塞意图。若仍为 Running，说明 timer
                    // 触发在 wait_event_impl arm 与 sleep_interruptible 之间，
                    // Re-arm instead of consuming the timer, or the task
                    // will sleep forever with no wakeup.
                    if !matches!(
                        task.task_status(),
                        super::TaskStatus::Blocking(_) | super::TaskStatus::Blocked
                    ) {
                        if !task
                            .wait_io_timer_pending
                            .swap(true, AtomicOrdering::Relaxed)
                        {
                            let new_gen = task
                                .wait_timer_generation
                                .fetch_add(1, AtomicOrdering::Relaxed)
                                + 1;
                            add_kernel_timer(
                                TimerAction::WakeTask {
                                    task: Arc::downgrade(&task),
                                    generation: new_gen,
                                    fallback_ms: Some(ms),
                                },
                                TimeSpec::now() + TimeSpec::from_ms(ms),
                            );
                            task.wait_io_fallback_active_generation
                                .store(new_gen, AtomicOrdering::Release);
                        }
                        return false;
                    }
                    // Blocking 和 Blocked 都交给统一 CAS 唤醒入口处理。
                }

                // Normal wake (deadline or current fallback)

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
            TimerAction::SendSignal {
                task,
                signal,
                generation,
            } => {
                if signal.is_empty() {
                    return false;
                }
                let Some(task) = task.upgrade() else {
                    return false;
                };
                let mut should_wake = false;
                let mut next_real_timer = None;
                {
                    let mut inner = task.acquire_inner_lock();
                    if signal == Signals::SIGALRM {
                        if inner.real_timer_generation != generation
                            || inner.real_timer_deadline != Some(timer.deadline)
                        {
                            return false;
                        }
                    }
                    inner.add_signal(signal);
                    if signal == Signals::SIGALRM {
                        if inner.timer[0].it_interval.is_zero() {
                            inner.real_timer_deadline = None;
                            inner.timer[0].it_value = TimeVal::new();
                        } else {
                            let interval = TimeSpec::from_us(inner.timer[0].it_interval.to_us());
                            let deadline = now + interval;
                            inner.real_timer_generation =
                                inner.real_timer_generation.wrapping_add(1);
                            let next_generation = inner.real_timer_generation;
                            inner.real_timer_deadline = Some(deadline);
                            inner.timer[0].it_value = inner.timer[0].it_interval;
                            next_real_timer = Some((deadline, next_generation));
                        }
                    }
                    if signal.wakes_interruptible(inner.sigmask, inner.signal_wait_mask, true) {
                        should_wake = true;
                    }
                }
                if should_wake {
                    wake_interruptible(task.clone());
                }
                if let Some((deadline, next_generation)) = next_real_timer {
                    add_kernel_timer(
                        TimerAction::SendSignal {
                            task: Arc::downgrade(&task),
                            signal,
                            generation: next_generation,
                        },
                        deadline,
                    );
                }
                should_wake
            }
            TimerAction::PosixTimerSignal {
                task,
                timer_id,
                signal,
                generation,
            } => {
                let Some(task) = task.upgrade() else {
                    return false;
                };
                let mut should_wake = false;
                let mut next_timer = None;
                {
                    let mut inner = task.acquire_inner_lock();
                    let signal_pending = !signal.is_empty() && inner.sigpending.contains(signal);
                    let Some(Some(timer_state)) = inner.posix_timers.get_mut(timer_id) else {
                        return false;
                    };
                    if timer_state.generation != generation
                        || timer_state.deadline != Some(timer.deadline)
                    {
                        return false;
                    }
                    if timer_state.interval.is_zero() {
                        timer_state.value = TimeSpec::new();
                        timer_state.deadline = None;
                        timer_state.realtime_abs_deadline = None;
                    } else {
                        let interval_ns = timer_state.interval.to_ns_saturating().max(1) as usize;
                        let deadline_ns = timer.deadline.to_ns_saturating() as usize;
                        let elapsed_ns =
                            (now.to_ns_saturating() as usize).saturating_sub(deadline_ns);
                        let expirations = 1usize.saturating_add(elapsed_ns / interval_ns);
                        let missed = if signal_pending {
                            expirations
                        } else {
                            expirations.saturating_sub(1)
                        };
                        timer_state.add_overrun(missed);
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
                        timer_state.generation = timer_state.generation.wrapping_add(1);
                        timer_state.value = timer_state.interval;
                        timer_state.deadline = Some(deadline);
                        next_timer = Some((deadline, timer_state.generation));
                    }
                    if !signal.is_empty() {
                        let _ = inner
                            .sigpending
                            .enqueue_signal(signal, SigInfo::SI_TIMER as usize);
                        if signal.wakes_interruptible(inner.sigmask, inner.signal_wait_mask, true) {
                            should_wake = true;
                        }
                    }
                }
                if should_wake {
                    wake_interruptible(task.clone());
                }
                if let Some((deadline, next_generation)) = next_timer {
                    add_kernel_timer(
                        TimerAction::PosixTimerSignal {
                            task: Arc::downgrade(&task),
                            timer_id,
                            signal,
                            generation: next_generation,
                        },
                        deadline,
                    );
                }
                should_wake
            }
            TimerAction::TimerFdSweep { generation } => {
                if !crate::fs::timerfd::timerfd_sweep_is_current(generation) {
                    return false;
                }
                let woke = crate::fs::timerfd::wake_expired_timerfds(now) > 0;
                crate::fs::timerfd::rearm_timerfd_sweep();
                woke
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

// ── High-res / sched tick state ──
static NEXT_SCHED_TICK_NS: AtomicU64 = AtomicU64::new(0);
const SCHED_TICK_NS: u64 = 10_000_000; // 100 Hz = 10 ms

fn program_next_event(next_timer_ns: u64) {
    let now_ns = crate::timer::now_ns();
    let next_sched_ns = NEXT_SCHED_TICK_NS.load(AtomicOrdering::Relaxed);
    let next_timer_ns = if next_timer_ns == 0 {
        u64::MAX
    } else {
        next_timer_ns
    };

    let next_ns = next_timer_ns.min(next_sched_ns.max(now_ns.saturating_add(1)));
    let delta_ns = next_ns.saturating_sub(now_ns).max(1);
    let delta_ticks = crate::timer::ns_to_ticks_ceil(delta_ns);

    crate::hal::program_timer_delta(delta_ticks);
}

/// 重编程硬件 timer，使其在下一次调度 tick 或最早 kernel timer deadline 触发。
///
/// # Locking
///
/// 调用方必须已经关闭本地 timer interrupt，因为本函数会读取
/// `KERNEL_TIMER_QUEUE` 并与 timer interrupt 路径共享状态。
fn reprogram_timer_irqoff() {
    let next_timer_ns = KERNEL_TIMER_QUEUE.lock().earliest_deadline_ns();
    program_next_event(next_timer_ns);
}

/// 初始化内核定时器子系统。
///
/// 设置第一个调度 tick，并把硬件 timer 编程到首个事件。
pub fn timer_subsystem_init() {
    let flags = local_irq_save();
    let now_ns = crate::timer::now_ns();
    NEXT_SCHED_TICK_NS.store(now_ns + SCHED_TICK_NS, AtomicOrdering::Relaxed);
    reprogram_timer_irqoff();
    local_irq_restore(flags);
}

/// 添加一个内核定时器动作。
///
/// # Locking
///
/// 函数会短暂关闭本地中断并持有 `KERNEL_TIMER_QUEUE` 锁；callback 不在此处执行。
pub fn add_kernel_timer(action: TimerAction, deadline: TimeSpec) {
    let flags = local_irq_save();
    let new_is_earliest = KERNEL_TIMER_QUEUE.lock().add_action(action, deadline);
    crate::task::perf::record_ktimer_add();
    let timer_len = { KERNEL_TIMER_QUEUE.lock().len() };
    crate::task::perf::record_ktimer_len(timer_len);
    if new_is_earliest && !crate::smp::local_timer_pending() {
        // hard IRQ 已把 one-shot 静默时，安全点即将按完整队列重新编程；
        // 此处只发布更早的软件 deadline，避免长 syscall 中反复触发到期 IRQ。
        reprogram_timer_irqoff();
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
            fallback_ms: None,
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
    if !crate::smp::take_local_timer_pending() {
        local_irq_restore(irq_flags);
        return false;
    }

    let handler_profile_start = crate::task::processor::sched_profile_cycle_start();
    let now = crate::timer::TimeSpec::now();
    let now_ns = now.to_ns_saturating();

    // 1. Expired kernel timers
    let expired_timers = { KERNEL_TIMER_QUEUE.lock().pop_expired(now) };
    let mut woke_task = false;
    for timer in expired_timers {
        if KernelTimerQueue::run_timer(timer, now) {
            woke_task = true;
        }
    }

    // Also handle timeout-waitqueue expiry (kept for compatibility)
    if TIMEOUT_WAITQUEUE_PENDING.load(AtomicOrdering::Relaxed) {
        let mut timeout_queue = TIMEOUT_WAITQUEUE.lock();
        if !timeout_queue.is_empty() {
            timeout_queue.wake_expired(now);
        } else {
            TIMEOUT_WAITQUEUE_PENDING.store(false, AtomicOrdering::Relaxed);
        }
    }

    // timerfd
    if crate::fs::timerfd::timerfd_registry_maybe_nonempty()
        && !crate::fs::timerfd::timerfd_registry_is_empty()
    {
        if crate::fs::timerfd::wake_expired_timerfds(now) > 0 {
            woke_task = true;
        }
    }

    // 2. Sched tick
    let mut need_resched = false;
    let mut next_tick = NEXT_SCHED_TICK_NS.load(AtomicOrdering::Relaxed);
    if now_ns >= next_tick {
        // Advance tick, but don't let it fall behind by more than one period.
        next_tick = next_tick.saturating_add(SCHED_TICK_NS);
        if now_ns >= next_tick {
            next_tick = now_ns.saturating_add(SCHED_TICK_NS);
        }
        NEXT_SCHED_TICK_NS.store(next_tick, AtomicOrdering::Relaxed);

        // Periodic housekeeping — only once per sched tick
        crate::net::config::NET_INTERFACE.try_poll_irq();
        need_resched = true;
    }

    // 3. Re-program hardware
    reprogram_timer_irqoff();
    crate::smp::complete_local_timer_deferred();
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
