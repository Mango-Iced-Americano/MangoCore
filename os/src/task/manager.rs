//! 任务调度队列、等待队列和内核定时器。
//!
//! 调度器维护 ready/interruptible/zombie 三类任务队列；`WaitQueue` 在文件、
//! futex、信号和计时器路径中提供条件等待；`KernelTimerQueue` 驱动超时唤醒、
//! POSIX timer、timerfd sweep 与调度 tick。
//!
//! # Locking
//!
//! `TASK_MANAGER` 只保护调度队列。任何可能触发 TCB/PCB 析构或用户内存访问的
//! 操作都应在释放 `TASK_MANAGER` 后进行。`WaitQueue` 的条件闭包不得反向获取
//! 已由调用方持有的不可重入锁。

use core::cmp::Ordering;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering as AtomicOrdering};

#[cfg(feature = "oom_handler")]
use crate::config::SYSTEM_TASK_LIMIT;
use alloc::vec::Vec;

use crate::hal::{local_irq_restore, local_irq_save};
use crate::timer::{TimeSpec, TimeVal};

use super::{
    block_current_and_run_next_checked, block_current_and_run_next_with_lock_checked, current_task,
    current_task_ref, discard_non_actionable_unblocked_signals, has_actionable_signal,
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
/// 全局调度队列状态。
pub struct TaskManager {
    /// 就绪态任务队列。
    pub ready_queue: VecDeque<Arc<TaskControlBlock>>,
    /// 可中断睡眠任务队列。
    pub interruptible_queue: VecDeque<Arc<TaskControlBlock>>,
    zombie_queue: VecDeque<Arc<TaskControlBlock>>,
    ready_nonzero_nice_count: usize,
    /// 任务激活状态跟踪器，用于跟踪任务的激活状态，并在OOM时释放内存
    pub active_tracker: ActiveTracker,
}

#[cfg(not(feature = "oom_handler"))]
/// 全局调度队列状态。
pub struct TaskManager {
    /// 就绪态任务队列。
    pub ready_queue: VecDeque<Arc<TaskControlBlock>>,
    /// 可中断睡眠任务队列。
    pub interruptible_queue: VecDeque<Arc<TaskControlBlock>>,
    zombie_queue: VecDeque<Arc<TaskControlBlock>>,
    ready_nonzero_nice_count: usize,
}

fn sched_pick_key(task: &Arc<TaskControlBlock>) -> (u64, i32, usize) {
    let inner = task.acquire_inner_lock();
    (inner.sched_vruntime, inner.sched_nice, task.gettid())
}

fn task_has_nonzero_nice(task: &Arc<TaskControlBlock>) -> bool {
    task.sched_nice_hint.load(AtomicOrdering::Relaxed) != 0
}

fn count_ready_nonzero_nice(queue: &VecDeque<Arc<TaskControlBlock>>) -> usize {
    queue
        .iter()
        .filter(|task| task_has_nonzero_nice(task))
        .count()
}

fn pop_fair_ready(queue: &mut VecDeque<Arc<TaskControlBlock>>) -> Option<Arc<TaskControlBlock>> {
    let mut best_index = 0usize;
    let mut best_key = sched_pick_key(queue.front()?);
    for (index, task) in queue.iter().enumerate().skip(1) {
        let key = sched_pick_key(task);
        if key < best_key {
            best_index = index;
            best_key = key;
        }
    }
    queue.remove(best_index)
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

static READY_TASK_COUNT: AtomicUsize = AtomicUsize::new(0);
static INTERRUPTIBLE_TASK_COUNT: AtomicUsize = AtomicUsize::new(0);
static ZOMBIE_QUEUE_COUNT: AtomicUsize = AtomicUsize::new(0);

fn add_ready_count() {
    READY_TASK_COUNT.fetch_add(1, AtomicOrdering::Relaxed);
}

fn sub_ready_count(count: usize) {
    if count != 0 {
        let _ = READY_TASK_COUNT.fetch_update(
            AtomicOrdering::Relaxed,
            AtomicOrdering::Relaxed,
            |value| Some(value.saturating_sub(count)),
        );
    }
}

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

/// 无锁读取 ready 队列计数的近似值。
pub(crate) fn ready_count_fast() -> u16 {
    READY_TASK_COUNT.load(AtomicOrdering::Relaxed).min(u16::MAX as usize) as u16
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

/// 简化的 nice-aware 调度器。
impl TaskManager {
    #[cfg(feature = "oom_handler")]
    /// 构造函数
    pub fn new() -> Self {
        Self {
            ready_queue: VecDeque::new(),
            interruptible_queue: VecDeque::new(),
            zombie_queue: VecDeque::new(),
            ready_nonzero_nice_count: 0,
            active_tracker: ActiveTracker::new(),
        }
    }
    #[cfg(not(feature = "oom_handler"))]
    pub fn new() -> Self {
        Self {
            ready_queue: VecDeque::new(),
            interruptible_queue: VecDeque::new(),
            zombie_queue: VecDeque::new(),
            ready_nonzero_nice_count: 0,
        }
    }
    /// 添加一个任务到就绪队列。
    ///
    /// # Locking
    ///
    /// 调用方已持有 `TASK_MANAGER` 锁；函数不获取任务内部锁。
    pub fn add(&mut self, task: Arc<TaskControlBlock>) {
        if task_has_nonzero_nice(&task) {
            self.ready_nonzero_nice_count += 1;
        }
        self.ready_queue.push_back(task);
        crate::task::perf::record_taskq_add_ready();
        add_ready_count();
    }
    fn add_front(&mut self, task: Arc<TaskControlBlock>) {
        if task_has_nonzero_nice(&task) {
            self.ready_nonzero_nice_count += 1;
        }
        self.ready_queue.push_front(task);
        crate::task::perf::record_taskq_add_ready();
        add_ready_count();
    }
    fn pop_next_ready(&mut self) -> Option<Arc<TaskControlBlock>> {
        let task = if self.ready_nonzero_nice_count == 0 {
            crate::task::perf::record_taskq_fetch(false, 0);
            self.ready_queue.pop_front()
        } else {
            let scan_depth = self.ready_queue.len();
            crate::task::perf::record_taskq_fetch(true, scan_depth);
            pop_fair_ready(&mut self.ready_queue)
        }?;
        sub_ready_count(1);
        if task_has_nonzero_nice(&task) {
            self.ready_nonzero_nice_count = self.ready_nonzero_nice_count.saturating_sub(1);
        }
        Some(task)
    }
    fn recompute_ready_nice_count(&mut self) {
        self.ready_nonzero_nice_count = count_ready_nonzero_nice(&self.ready_queue);
    }
    fn note_ready_removed(&mut self, task: &Arc<TaskControlBlock>) {
        if task_has_nonzero_nice(task) {
            self.ready_nonzero_nice_count = self.ready_nonzero_nice_count.saturating_sub(1);
        }
    }
    /// 从就绪队列中逐出一个僵尸任务（零堆分配）。
    /// 每次调用最多 remove 一个元素，drop 发生在锁外。
    fn take_one_ready_zombie(&mut self) -> Option<Arc<TaskControlBlock>> {
        if self
            .ready_queue
            .front()
            .map(|task| task.acquire_inner_lock().is_zombie())
            .unwrap_or(false)
        {
            let zombie = self.ready_queue.pop_front();
            if let Some(task) = zombie.as_ref() {
                self.note_ready_removed(task);
                sub_ready_count(1);
            }
            return zombie;
        }
        if self
            .ready_queue
            .back()
            .map(|task| task.acquire_inner_lock().is_zombie())
            .unwrap_or(false)
        {
            let zombie = self.ready_queue.pop_back();
            if let Some(task) = zombie.as_ref() {
                self.note_ready_removed(task);
                sub_ready_count(1);
            }
            return zombie;
        }
        for i in 0..self.ready_queue.len() {
            if self.ready_queue[i].acquire_inner_lock().is_zombie() {
                let zombie = self.ready_queue.remove(i).unwrap();
                self.note_ready_removed(&zombie);
                sub_ready_count(1);
                return Some(zombie);
            }
        }
        None
    }
    /// 从可中断队列中逐出一个僵尸任务（零堆分配）。
    fn take_one_interruptible_zombie(&mut self) -> Option<Arc<TaskControlBlock>> {
        for i in 0..self.interruptible_queue.len() {
            if self.interruptible_queue[i].acquire_inner_lock().is_zombie() {
                let zombie = self.interruptible_queue.remove(i);
                if zombie.is_some() {
                    sub_interruptible_count(1);
                }
                return zombie;
            }
        }
        None
    }
    fn add_zombie(&mut self, task: Arc<TaskControlBlock>) {
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
    /// 从就绪 + 可中断队列中移除属于指定 pid 的所有 zombie TCB。
    /// 返回收集到的 zombie Arc，由调用者负责在锁外 drop。
    fn remove_zombie_tasks_by_pid(&mut self, pid: usize) -> alloc::vec::Vec<Arc<TaskControlBlock>> {
        let mut zombies = alloc::vec::Vec::new();
        let old_ready_len = self.ready_queue.len();
        self.ready_queue.retain(|task| {
            let is_match = task.acquire_inner_lock().is_zombie() && task.process.pid == pid;
            if is_match {
                zombies.push(task.clone());
                false
            } else {
                true
            }
        });
        sub_ready_count(old_ready_len - self.ready_queue.len());
        let old_interruptible_len = self.interruptible_queue.len();
        self.interruptible_queue.retain(|task| {
            let is_match = task.acquire_inner_lock().is_zombie() && task.process.pid == pid;
            if is_match {
                zombies.push(task.clone());
                false
            } else {
                true
            }
        });
        sub_interruptible_count(old_interruptible_len - self.interruptible_queue.len());
        self.recompute_ready_nice_count();
        zombies
    }
    fn update_ready_nice(&mut self, task: &Arc<TaskControlBlock>, old_nice: i32, new_nice: i32) {
        if (old_nice == 0) == (new_nice == 0) {
            return;
        }
        if !self
            .ready_queue
            .iter()
            .any(|queued| task_ptr_eq(queued, task))
        {
            return;
        }
        if old_nice == 0 {
            self.ready_nonzero_nice_count += 1;
        } else {
            self.ready_nonzero_nice_count = self.ready_nonzero_nice_count.saturating_sub(1);
        }
    }
    /// 从就绪队列中取出下一个可运行任务。
    #[cfg(feature = "oom_handler")]
    pub fn fetch(&mut self) -> Option<Arc<TaskControlBlock>> {
        match self.pop_next_ready() {
            Some(task) => {
                // 标记任务为激活状态
                self.active_tracker.mark_active(task.tid.0);
                Some(task)
            }
            None => None,
        }
    }
    #[cfg(not(feature = "oom_handler"))]
    /// 从就绪队列中取出下一个可运行任务。
    pub fn fetch(&mut self) -> Option<Arc<TaskControlBlock>> {
        self.pop_next_ready()
    }

    /// 添加一个任务到可中断队列。
    pub fn add_interruptible(&mut self, task: Arc<TaskControlBlock>) {
        self.interruptible_queue.push_back(task);
        crate::task::perf::record_taskq_add_interruptible();
        add_interruptible_count();
    }
    /// 从可中断队列中删除一个任务。
    pub fn drop_interruptible(&mut self, task: &Arc<TaskControlBlock>) -> bool {
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
    fn enqueue_ready_batch(&mut self, tasks: Vec<Arc<TaskControlBlock>>) -> usize {
        if tasks.is_empty() {
            return 0;
        }
        let ptrs = sorted_task_ptrs(&tasks);
        let old_interruptible_len = self.interruptible_queue.len();
        self.interruptible_queue
            .retain(|task| !task_ptr_in(&ptrs, task));
        sub_interruptible_count(old_interruptible_len - self.interruptible_queue.len());
        let count = tasks.len();
        for task in tasks.into_iter().rev() {
            self.add_front(task);
        }
        count
    }
    /// 从调度器的 ready / interruptible 队列中移除一组任务。
    /// 线程组退出和 exec 清理只能通过这个入口调整队列，避免业务层直接扫描队列。
    pub fn remove_tasks(&mut self, tasks: &[Arc<TaskControlBlock>]) -> usize {
        let ptrs = sorted_task_ptrs(tasks);

        let old_ready_len = self.ready_queue.len();
        self.ready_queue.retain(|task| !task_ptr_in(&ptrs, task));
        let removed_ready = old_ready_len - self.ready_queue.len();
        sub_ready_count(removed_ready);
        self.recompute_ready_nice_count();
        let old_interruptible_len = self.interruptible_queue.len();
        self.interruptible_queue
            .retain(|task| !task_ptr_in(&ptrs, task));
        let removed_interruptible = old_interruptible_len - self.interruptible_queue.len();
        sub_interruptible_count(removed_interruptible);

        removed_ready + removed_interruptible
    }
    /// 就绪队列中任务数量
    pub fn ready_count(&self) -> u16 {
        self.ready_queue.len() as u16
    }
    /// 可中断队列中任务数量
    pub fn interruptible_count(&self) -> u16 {
        self.interruptible_queue.len() as u16
    }
    /// 僵尸任务数量（遍历就绪+可中断队列）
    pub fn zombie_count(&self) -> u16 {
        let mut count = 0u16;
        for t in self
            .ready_queue
            .iter()
            .chain(self.interruptible_queue.iter())
        {
            if t.acquire_inner_lock().is_zombie() {
                count += 1;
            }
        }
        count
    }
    /// 将任务从 interruptible 队列移动到 ready 队列。
    ///
    /// # Semantics
    ///
    /// 若任务已经在 ready 队列中，函数静默返回。调用方必须先把
    /// `task_status` 改为 `Ready`，本函数只维护调度队列。
    pub fn wake_interruptible(&mut self, task: Arc<TaskControlBlock>) {
        crate::task::perf::record_taskq_wake_interruptible();
        match self.try_wake_interruptible(task) {
            Ok(_) => {}
            Err(_) => {}
        }
    }
    /// 尝试将任务从 interruptible 队列移动到 ready 队列。
    ///
    /// # Errors
    ///
    /// 任务已经在 ready 队列中时返回 `WaitQueueError::AlreadyWaken`。
    ///
    /// # Locking
    ///
    /// 调用方已持有 `TASK_MANAGER` 锁；本函数不会改变 `task_status`。
    pub fn try_wake_interruptible(
        &mut self,
        task: Arc<TaskControlBlock>,
    ) -> Result<(), WaitQueueError> {
        // 从可中断队列中删除指定任务
        if self.drop_interruptible(&task) {
            self.add_front(task);
            return Ok(());
        }
        // 如果任务不在就绪队列中，将其加入就绪队列
        if !self
            .ready_queue
            .iter()
            .any(|task_in_queue| Arc::as_ptr(task_in_queue) == Arc::as_ptr(&task))
        {
            self.add_front(task);
            Ok(())
        } else {
            crate::task::perf::record_taskq_dup_enqueue();
            Err(WaitQueueError::AlreadyWaken)
        }
    }
    #[allow(unused)]
    /// 打印 ready 队列中的任务 ID，仅供诊断。
    pub fn show_ready(&self) {
        self.ready_queue.iter().for_each(|task| {
            log::error!("[show_ready] tid: {}, pid: {}", task.tid.0, task.pid());
        })
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

fn enqueue_ready_batch(tasks: Vec<Arc<TaskControlBlock>>) -> usize {
    TASK_MANAGER.lock().enqueue_ready_batch(tasks)
}

/// 更新 ready 队列中任务的 nice 快速路径计数。
pub fn update_ready_nice(task: &Arc<TaskControlBlock>, old_nice: i32, new_nice: i32) {
    TASK_MANAGER
        .lock()
        .update_ready_nice(task, old_nice, new_nice);
}

lazy_static! {
    /// 全局任务管理器。
    pub static ref TASK_MANAGER: Mutex<TaskManager> = Mutex::new(TaskManager::new());
}

/// 添加一个任务到 ready 队列。
pub fn add_task(task: Arc<TaskControlBlock>) {
    TASK_MANAGER.lock().add(task);
}

/// 添加一个任务到显式 zombie 回收队列。
pub fn add_zombie_task(task: Arc<TaskControlBlock>) {
    TASK_MANAGER.lock().add_zombie(task);
}

/// 从 ready 队列取出下一个可运行任务。
pub fn fetch_task() -> Option<Arc<TaskControlBlock>> {
    TASK_MANAGER.lock().fetch()
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

/// 从就绪队列中逐出一个僵尸任务（零堆分配），返回后在锁外 drop。
/// 调度循环中每轮调用一次，逐步排空。
pub fn take_one_ready_zombie() -> Option<Arc<TaskControlBlock>> {
    TASK_MANAGER.lock().take_one_ready_zombie()
}

/// 从可中断队列中逐出一个僵尸任务（零堆分配），返回后在锁外 drop。
pub fn take_one_interruptible_zombie() -> Option<Arc<TaskControlBlock>> {
    TASK_MANAGER.lock().take_one_interruptible_zombie()
}

/// 从调度队列中移除属于指定 pid 的所有 zombie TCB，
/// 在锁外 drop，避免 TCB::drop() 在持有 TASK_MANAGER 锁时执行析构链。
pub fn remove_zombie_tasks_by_pid(pid: usize) {
    let zombies = TASK_MANAGER.lock().remove_zombie_tasks_by_pid(pid);
    drop(zombies);
}

/// 尝试释放所有任务的内存空间，直到释放`req`页。
#[cfg(feature = "oom_handler")]
pub fn do_oom(req: usize) -> Result<(), ()> {
    let mut manager = match TASK_MANAGER.try_lock() {
        Some(manager) => manager,
        None => return Err(()),
    };
    let mut total_released = 0;
    let interruptible_len = manager.interruptible_queue.len();
    for idx in 0..interruptible_len {
        let task = manager.interruptible_queue[idx].clone();
        if !manager.active_tracker.check_active(task.tid.0) {
            continue;
        }
        let released = task.process.vm().lock().do_deep_clean();
        log::warn!(
            "deep clean on task: tid {}, pid {}, released: {}",
            task.tid.0,
            task.pid(),
            released
        );
        manager.active_tracker.mark_inactive(task.tid.0);
        total_released += released;
        if total_released >= req {
            return Ok(());
        };
    }
    let ready_len = manager.ready_queue.len();
    for idx in (0..ready_len).rev() {
        let task = manager.ready_queue[idx].clone();
        if !manager.active_tracker.check_active(task.tid.0) {
            continue;
        }
        let released = task.process.vm().lock().do_shallow_clean();
        log::warn!(
            "shallow clean on task: tid {}, pid {}, released: {}",
            task.tid.0,
            task.pid(),
            released
        );
        manager.active_tracker.mark_inactive(task.tid.0);
        total_released += released;
        if total_released >= req {
            return Ok(());
        };
    }
    Err(())
}

#[cfg(not(feature = "oom_handler"))]
#[allow(unused)]
/// 未启用 OOM handler 时的空实现。
pub fn do_oom() {
}

/// 将任务加入 interruptible 队列。
///
/// # Semantics
///
/// 函数不会从 ready 队列删除任务，也不会修改 `task_status`。调用方通常在
/// 当前任务已被取出 ready 队列后，把状态设为 `Interruptible`，再调用本函数。
pub fn sleep_interruptible(task: Arc<TaskControlBlock>) {
    TASK_MANAGER.lock().add_interruptible(task);
}

/// 唤醒 interruptible 任务并加入 ready 队列。
///
/// # Semantics
///
/// 函数只维护调度队列，不修改 `task_status`。
pub fn wake_interruptible(task: Arc<TaskControlBlock>) {
    TASK_MANAGER.lock().wake_interruptible(task)
}

/// 从调度队列中移除一组任务。
pub fn remove_tasks_from_queues(tasks: &[Arc<TaskControlBlock>]) -> usize {
    TASK_MANAGER.lock().remove_tasks(tasks)
}

/// 返回 ready + interruptible 队列计数的近似值。
pub fn procs_count() -> u16 {
    ready_count_fast().saturating_add(interruptible_count_fast())
}

/// 无锁判断 ready 队列是否非空。
pub fn has_ready_task() -> bool {
    READY_TASK_COUNT.load(AtomicOrdering::Relaxed) != 0
}

/// 返回 ready/interruptible 队列中的 zombie 任务数量。
pub fn zombie_count() -> u16 {
    let manager = TASK_MANAGER.lock();
    manager.zombie_count()
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
        if inner.task_status == TaskStatus::Interruptible {
            inner.task_status = TaskStatus::Ready;
        }
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
/// `wake_*` 会获取被唤醒任务的 `task.inner` 并操作 `TASK_MANAGER`。调用方
/// 不应在持有同一任务锁或调度器锁时调用唤醒函数。
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
    /// 会获取每个任务的 `task.inner`，并批量把任务移入 ready 队列。调用方不得
    /// 已持有这些任务的内部锁。
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
                Some(task) => {
                    let mut inner = task.acquire_inner_lock();
                    match inner.task_status {
                        super::TaskStatus::Interruptible => {
                            if wake_count < limit {
                                inner.task_status = super::task::TaskStatus::Ready;
                                drop(inner);
                                task.wait_timer_generation.fetch_add(1, AtomicOrdering::Relaxed);
                                wake_count += 1;
                                tasks_to_wake.push(task);
                            } else {
                                drop(inner);
                                remaining.push_back(Arc::downgrade(&task));
                            }
                        }
                        super::TaskStatus::Ready => {
                            if wake_count < limit {
                                wake_count += 1;
                                drop(inner);
                                task.wait_timer_generation.fetch_add(1, AtomicOrdering::Relaxed);
                            } else {
                                drop(inner);
                                remaining.push_back(Arc::downgrade(&task));
                            }
                        }
                        // Zombie/Running 不应继续停留在等待队列中，直接丢弃。
                        _ => drop(inner),
                    }
                }
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
            let mut inner = task.acquire_inner_lock();
            match inner.task_status {
                super::TaskStatus::Interruptible => {
                    inner.task_status = super::task::TaskStatus::Ready;
                    drop(inner);
                    task.wait_timer_generation.fetch_add(1, AtomicOrdering::Relaxed);
                    let _ = TASK_MANAGER.lock().try_wake_interruptible(task);
                    return 1;
                }
                super::TaskStatus::Ready => {
                    drop(inner);
                    task.wait_timer_generation.fetch_add(1, AtomicOrdering::Relaxed);
                    return 1;
                }
                _ => drop(inner),
            }
        }
        0
    }

    /// 将当前任务标记为 interruptible 并加入等待队列。
    ///
    /// # Locking
    ///
    /// 调用方已持有等待队列所属对象的锁；本函数会短暂获取 `task.inner`。
    pub fn prepare_to_wait(&mut self, task: Weak<TaskControlBlock>) {
        match task.upgrade() {
            Some(task) => {
                let mut task_inner = task.acquire_inner_lock();
                task_inner.task_status = super::TaskStatus::Interruptible;
            }
            None => return,
        }
        self.add_task(task);
    }

    /// 从等待队列移除任务，并把仍处于 interruptible 的任务恢复为 ready。
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
        let mut task_inner = task.acquire_inner_lock();
        if task_inner.task_status == super::TaskStatus::Interruptible {
            task_inner.task_status = super::TaskStatus::Ready;
        }
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

            let task = current_task_ref().unwrap();
            wq.lock().finish_wait(task);
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

            let task = current_task_ref().unwrap();
            let mut guard = lock.lock();
            let removed = queue_of(&mut guard).finish_wait(task);
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

            let task = current_task_ref().unwrap();
            Self::finish_wait_on_queues(queues, task);
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
    /// `cond` 在不持有等待队列锁时执行；调用方负责在闭包内轮询底层对象状态。
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
            TimerAction::WakeTask { task, generation, .. } => match task.upgrade() {
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
        self.inner.peek().map(|t| t.deadline.to_ns_saturating()).unwrap_or(0)
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
            TimerAction::WakeTask { task, generation, fallback_ms } => {
                let Some(task) = task.upgrade() else { return false };
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
                        let current =
                            task.wait_timer_generation.load(AtomicOrdering::Relaxed);
                        if active == current {
                            // Task waiting with new generation but no timer armed
                            if !task.wait_io_timer_pending.swap(true, AtomicOrdering::Relaxed) {
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
                    // But first: check if the task is actually interruptible.
                    // If not, the timer fired between arm (in wait_event_impl)
                    // and the task becoming Interruptible (in
                    // block_current_and_run_next_with_lock_checked).
                    // Re-arm instead of consuming the timer, or the task
                    // will sleep forever with no wakeup.
                    let inner = task.acquire_inner_lock();
                    if inner.task_status != super::TaskStatus::Interruptible {
                        drop(inner);
                        if !task.wait_io_timer_pending.swap(true, AtomicOrdering::Relaxed) {
                            let new_gen = task.wait_timer_generation.fetch_add(1, AtomicOrdering::Relaxed) + 1;
                            add_kernel_timer(
                                TimerAction::WakeTask {
                                    task: Arc::downgrade(&task),
                                    generation: new_gen,
                                    fallback_ms: Some(ms),
                                },
                                TimeSpec::now() + TimeSpec::from_ms(ms),
                            );
                            task.wait_io_fallback_active_generation.store(new_gen, AtomicOrdering::Release);
                        }
                        return false;
                    }
                    drop(inner);
                    // Task is Interruptible — fall through to normal wake below
                }

                // Normal wake (deadline or current fallback)

                if task.wait_timer_generation.load(AtomicOrdering::Relaxed) != generation {
                    crate::task::perf::record_ktimer_stale_waketask();
                    return false; // generation mismatch, stale
                }

                let mut inner = task.acquire_inner_lock();
                let should_wake = inner.task_status == super::TaskStatus::Interruptible;
                if should_wake {
                    inner.task_status = super::task::TaskStatus::Ready;
                }
                drop(inner);
                if should_wake {
                    crate::task::perf::record_ktimer_real_wake();
                    wake_interruptible(task);
                }
                should_wake
            }
            TimerAction::SendSignal {
                task,
                signal,
                generation,
            } => {
                if signal.is_empty() {
                    return false;
                }
                let Some(task) = task.upgrade() else { return false };
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
                            let interval =
                                TimeSpec::from_us(inner.timer[0].it_interval.to_us());
                            let deadline = now + interval;
                            inner.real_timer_generation =
                                inner.real_timer_generation.wrapping_add(1);
                            let next_generation = inner.real_timer_generation;
                            inner.real_timer_deadline = Some(deadline);
                            inner.timer[0].it_value = inner.timer[0].it_interval;
                            next_real_timer = Some((deadline, next_generation));
                        }
                    }
                    if signal.wakes_interruptible(inner.sigmask, inner.signal_wait_mask, true)
                        && inner.task_status == super::TaskStatus::Interruptible
                    {
                        inner.task_status = super::TaskStatus::Ready;
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
                let Some(task) = task.upgrade() else { return false };
                let mut should_wake = false;
                let mut next_timer = None;
                    {
                        let mut inner = task.acquire_inner_lock();
                        let signal_pending =
                            !signal.is_empty() && inner.sigpending.contains(signal);
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
                            let interval_ns =
                                timer_state.interval.to_ns_saturating().max(1) as usize;
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
                            if signal.wakes_interruptible(
                                inner.sigmask,
                                inner.signal_wait_mask,
                                true,
                            ) && inner.task_status == super::TaskStatus::Interruptible
                            {
                                inner.task_status = super::TaskStatus::Ready;
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
        self.inner.peek().map(|waiter| waiter.timeout.to_ns_saturating()).unwrap_or(0)
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
    /// 调用方持有 `TIMEOUT_WAITQUEUE` 锁。函数会获取任务内部锁并批量入 ready
    /// 队列，不应在持有其它任务锁时调用。
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
                    Some(task) => {
                        let mut inner = task.acquire_inner_lock();
                        match inner.task_status {
                            super::TaskStatus::Interruptible => {
                                inner.task_status = super::task::TaskStatus::Ready
                            }
                            // Ready/Running 不需要唤醒；Zombie 不能重新入队。
                            _ => continue,
                        }
                        drop(inner);
                        tasks_to_wake.push(task);
                    }
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
    let next_timer_ns = if next_timer_ns == 0 { u64::MAX } else { next_timer_ns };

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
    if new_is_earliest {
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

/// 统一 timer interrupt 处理入口。
///
/// # Semantics
///
/// 先弹出过期 kernel timers 并在锁外执行 callback，再处理 legacy timeout queue
/// 和 timerfd，最后推进调度 tick 并按需让出 CPU。
pub fn timer_interrupt_handler() {
    let handler_profile_start = crate::task::processor::sched_profile_cycle_start();
    let _irq_start = crate::task::perf::perf_time_now();
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
        crate::net::config::NET_INTERFACE.try_poll();
        need_resched = true;
    }

    // 3. Re-program hardware
    reprogram_timer_irqoff();

    crate::task::perf::record_timer_irq_cost(_irq_start);

    // 4. Yield if needed
    crate::task::processor::record_sched_timer_handler_cycles(handler_profile_start);
    if need_resched || woke_task {
        crate::task::suspend_current_and_run_next();
    }
}

/// 唤醒已过期的 legacy timeout/kernel timer。
///
/// # Semantics
///
/// 这是旧固定 tick 路径的兼容入口；新的硬件 timer 中断主要走
/// `timer_interrupt_handler()`。
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
