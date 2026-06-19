/*
    此文件用于管理任务的调度
    内容与RISCV版本相同，无需修改
*/
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
/// 任务的激活状态跟踪器
pub struct ActiveTracker {
    /// 存储激活状态的位图
    bitmap: Vec<u64>,
}

#[cfg(feature = "oom_handler")]
#[allow(unused)]
impl ActiveTracker {
    /// 默认大小为128
    pub const DEFAULT_SIZE: usize = SYSTEM_TASK_LIMIT;
    /// 构造函数
    pub fn new() -> Self {
        // 计算位图长度，向上取整
        let len = (Self::DEFAULT_SIZE + 63) / 64;
        // 初始化位图
        let mut bitmap = Vec::with_capacity(len);
        // 位图全部置0
        bitmap.resize(len, 0);
        Self { bitmap }
    }
    /// 确保位图可以容纳指定 tid
    pub fn ensure_capacity(&mut self, tid: usize) {
        let word = tid / 64;
        if word >= self.bitmap.len() {
            self.bitmap.resize(word + 1, 0);
        }
    }
    /// 检查指定 tid 的任务是否处于激活状态
    pub fn check_active(&self, tid: usize) -> bool {
        let word = tid / 64;
        if word >= self.bitmap.len() {
            return false;
        }
        (self.bitmap[word] & (1 << (tid % 64))) != 0
    }
    /// 检查指定 tid 的任务是否处于非激活状态
    pub fn check_inactive(&self, tid: usize) -> bool {
        !self.check_active(tid)
    }
    /// 标记指定 tid 的任务为激活状态
    pub fn mark_active(&mut self, tid: usize) {
        self.ensure_capacity(tid);
        self.bitmap[tid / 64] |= 1 << (tid % 64)
    }
    /// 标记指定 tid 的任务为非激活状态
    pub fn mark_inactive(&mut self, tid: usize) {
        let word = tid / 64;
        if word >= self.bitmap.len() {
            return;
        }
        self.bitmap[word] &= !(1 << (tid % 64))
    }
}

#[cfg(feature = "oom_handler")]
/// 任务管理器
pub struct TaskManager {
    /// 一个双端队列，用于存储就绪态任务
    pub ready_queue: VecDeque<Arc<TaskControlBlock>>,
    /// 一个双端队列，用于存储可中断状态任务
    pub interruptible_queue: VecDeque<Arc<TaskControlBlock>>,
    zombie_queue: VecDeque<Arc<TaskControlBlock>>,
    ready_nonzero_nice_count: usize,
    /// 任务激活状态跟踪器，用于跟踪任务的激活状态，并在OOM时释放内存
    pub active_tracker: ActiveTracker,
}

#[cfg(not(feature = "oom_handler"))]
pub struct TaskManager {
    pub ready_queue: VecDeque<Arc<TaskControlBlock>>,
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

fn ready_count_fast() -> u16 {
    READY_TASK_COUNT.load(AtomicOrdering::Relaxed).min(u16::MAX as usize) as u16
}

fn interruptible_count_fast() -> u16 {
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
    /// 添加一个任务到就绪队列
    pub fn add(&mut self, task: Arc<TaskControlBlock>) {
        if task_has_nonzero_nice(&task) {
            self.ready_nonzero_nice_count += 1;
        }
        self.ready_queue.push_back(task);
        add_ready_count();
    }
    fn add_front(&mut self, task: Arc<TaskControlBlock>) {
        if task_has_nonzero_nice(&task) {
            self.ready_nonzero_nice_count += 1;
        }
        self.ready_queue.push_front(task);
        add_ready_count();
    }
    fn pop_next_ready(&mut self) -> Option<Arc<TaskControlBlock>> {
        let task = if self.ready_nonzero_nice_count == 0 {
            self.ready_queue.pop_front()
        } else {
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
    /// 从就绪队列中取出一个任务
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
    pub fn fetch(&mut self) -> Option<Arc<TaskControlBlock>> {
        self.pop_next_ready()
    }
    /// 添加一个任务到可中断队列
    pub fn add_interruptible(&mut self, task: Arc<TaskControlBlock>) {
        self.interruptible_queue.push_back(task);
        add_interruptible_count();
    }
    /// 从可中断队列中删除一个任务
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
            // 使用retain过滤掉与指定任务相同的任务
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
    /// 这个函数会将`task`从`interruptible_queue`中删除，并加入`ready_queue`。
    /// 如果一切正常的话，这个`task`将会被加入`ready_queue`。如果`task`已经被唤醒，那么什么也不会发生。
    /// # 注意
    /// 这个函数不会改变`task_status`，你应该手动改变它以保持一致性。
    pub fn wake_interruptible(&mut self, task: Arc<TaskControlBlock>) {
        match self.try_wake_interruptible(task) {
            Ok(_) => {}
            Err(_) => {}
        }
    }
    /// 这个函数会将`task`从`interruptible_queue`中删除，并加入`ready_queue`。
    /// 如果一切正常的话，这个`task`将会被加入`ready_queue`。如果`task`已经被唤醒，那么返回`Err()`。
    /// # 注意
    /// 这个函数不会改变`task_status`，你应该手动改变它以保持一致性。
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
            Err(WaitQueueError::AlreadyWaken)
        }
    }
    #[allow(unused)]
    /// 调试方法
    /// 打印就绪队列中的任务ID
    pub fn show_ready(&self) {
        self.ready_queue.iter().for_each(|task| {
            log::error!("[show_ready] tid: {}, pid: {}", task.tid.0, task.pid());
        })
    }
    #[allow(unused)]
    /// 调试方法
    /// 打印可中断队列中的任务ID
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

pub fn update_ready_nice(task: &Arc<TaskControlBlock>, old_nice: i32, new_nice: i32) {
    TASK_MANAGER
        .lock()
        .update_ready_nice(task, old_nice, new_nice);
}

lazy_static! {
    /// 全局任务管理器（带互斥锁）
    pub static ref TASK_MANAGER: Mutex<TaskManager> = Mutex::new(TaskManager::new());
}

/// 添加一个任务到任务管理器
pub fn add_task(task: Arc<TaskControlBlock>) {
    TASK_MANAGER.lock().add(task);
}

pub fn add_zombie_task(task: Arc<TaskControlBlock>) {
    TASK_MANAGER.lock().add_zombie(task);
}

/// 从任务管理器中取出一个任务
pub fn fetch_task() -> Option<Arc<TaskControlBlock>> {
    TASK_MANAGER.lock().fetch()
}

pub fn take_one_zombie_task() -> Option<Arc<TaskControlBlock>> {
    if !has_zombie_queue_tasks() {
        return None;
    }
    TASK_MANAGER.lock().take_one_zombie()
}

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
pub fn do_oom() {
    // do nothing
}

/// 这个函数会将`task`加入到`interruptible_queue`，
/// 但不会从`ready_queue`中删除。
/// 所以需要确保`task`不会出现在`ready_queue`中。
/// 在一般情况下，一个`task`在被调度后会从`ready_queue`中删除，
/// 并且你可以使用`take_current_task()`来获取当前`task`的所有权。
/// # 注意
/// 你应该找一个地方保存`task`的`Arc<TaskControlBlock>`，
/// 否则你将无法在将来使用`wake_interruptible()`来唤醒它。
/// 这个函数不会改变`task_status`，你应该手动改变它以保持一致性。
pub fn sleep_interruptible(task: Arc<TaskControlBlock>) {
    // 将任务加入可中断队列
    TASK_MANAGER.lock().add_interruptible(task);
}

/// 这个函数会将`task`从`interruptible_queue`中删除，并加入到`ready_queue`中。
/// 这个`task`会在一切正常的情况下被调度。如果`task`已经被唤醒，什么也不会发生。
/// # 注意
/// 这个函数不会改变`task_status`，你应该手动改变它以保持一致性。
pub fn wake_interruptible(task: Arc<TaskControlBlock>) {
    TASK_MANAGER.lock().wake_interruptible(task)
}

/// 从调度队列中移除一组任务。
pub fn remove_tasks_from_queues(tasks: &[Arc<TaskControlBlock>]) -> usize {
    TASK_MANAGER.lock().remove_tasks(tasks)
}

/// 返回就绪队列中的任务数量
pub fn procs_count() -> u16 {
    ready_count_fast().saturating_add(interruptible_count_fast())
}

pub fn has_ready_task() -> bool {
    READY_TASK_COUNT.load(AtomicOrdering::Relaxed) != 0
}

/// 返回僵尸任务数量
pub fn zombie_count() -> u16 {
    let manager = TASK_MANAGER.lock();
    manager.zombie_count()
}

/// Send a signal to all interruptible tasks EXCEPT initproc (pid=1).
/// Returns true if at least one task received the signal.
pub fn send_signal_to_interruptible(signal: Signals) -> bool {
    let manager = TASK_MANAGER.lock();
    let tasks: Vec<_> = manager
        .interruptible_queue
        .iter()
        .filter(|t| t.pid() != 1) // never signal initproc via Ctrl+C
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

/// 等待队列错误类型
pub enum WaitQueueError {
    /// 已经唤醒
    AlreadyWaken,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitResult {
    Ready(isize),
    Interrupted,
    TimedOut,
}

impl WaitResult {
    pub fn unwrap_or_else(self, f: impl FnOnce(isize) -> isize) -> isize {
        match self {
            WaitResult::Ready(value) => value,
            WaitResult::Interrupted => f(-(SyscallErr::ERESTART as isize)),
            WaitResult::TimedOut => f(-(SyscallErr::EAGAIN as isize)),
        }
    }
}

/// 等待队列
/// 内部是一个存储任务控制块弱引用的双端队列
pub struct WaitQueue {
    inner: VecDeque<Weak<TaskControlBlock>>,
}

#[allow(unused)]
impl WaitQueue {
    /// 构造函数
    pub fn new() -> Self {
        Self {
            inner: VecDeque::new(),
        }
    }
    /// 这个函数将一个`task`添加到 `WaitQueue`但是不会阻塞这个任务
    /// 如果想要阻塞一个`task`，使用`block_current_and_run_next()`
    pub fn add_task(&mut self, task: Weak<TaskControlBlock>) {
        // 将task添加到back端
        self.inner.push_back(task);
    }
    /// 这个函数会尝试从`WaitQueue`中弹出一个`task`，但是不会唤醒它
    pub fn pop_task(&mut self) -> Option<Weak<TaskControlBlock>> {
        // 将front端的任务弹出
        self.inner.pop_front()
    }
    /// 判断等待队列是否包含给定的task
    pub fn contains(&self, task: &Weak<TaskControlBlock>) -> bool {
        self.inner
            .iter()
            .any(|task_in_queue| Weak::as_ptr(task_in_queue) == Weak::as_ptr(task))
    }
    /// 判断等待队列是否为空
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
    /// 清理所有失效的 Weak 条目（upgrade 返回 None），返回清理数量。
    pub fn compact_stale(&mut self) -> usize {
        let before = self.inner.len();
        self.inner.retain(|task| task.strong_count() > 0);
        before - self.inner.len()
    }
    /// 这个函数将会唤醒等待队列中所有的任务，并将它们的任务状态改变为就绪态，
    /// 如果一切正常，这些任务会在将来被调度。
    /// # 警告
    /// 这个函数会为每个被唤醒的`task`调用`acquire_inner_lock`，请注意**死锁**
    pub fn wake_all(&mut self) -> usize {
        self.wake_at_most(usize::MAX)
    }
    /// 唤醒不超过`limit`个`task`，返回唤醒的`task`数量。
    /// # 警告
    /// 这个函数会为每个被唤醒的`task`调用`acquire_inner_lock`，请注意**死锁**
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
                        // 其他状态：直接丢弃（Zombie/Running 不应继续停留在等待队列中）
                        _ => drop(inner),
                    }
                }
                // 失效 Weak：直接丢弃，实现自动 compact
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
    pub fn prepare_to_wait(&mut self, task: Weak<TaskControlBlock>) {
        match task.upgrade() {
            Some(task) => {
                let mut task_inner = task.acquire_inner_lock();
                task_inner.task_status = super::TaskStatus::Interruptible;
            }
            None => return, // 不会发生
        }
        self.add_task(task);
    }
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

    // ==================== wait_until 方法族（DragonOS 架构） ====================

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
        // `cond` may be evaluated while the current task is temporarily
        // removed from the CPU, so callers must not depend on `current_task()`.
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

    /// 可中断等待，条件满足或收到信号时返回。
    ///
    /// 等价于 DragonOS 的 `wait_until_interruptible`。
    /// 文件和网络 IO 通用。
    /// - `Ok(v)`：条件满足
    /// - `Err(-ERESTART)`：被信号中断
    pub fn wait_until_interruptible<F>(wq: &Mutex<Self>, mut cond: F) -> WaitResult
    where
        F: FnMut() -> Option<isize>,
    {
        Self::wait_event_impl(wq, &mut cond, true, None, Some(Self::WAIT_IO_FALLBACK_MS))
    }

    /// IO 等待（不可中断），正确标记 iowait 以用于 CPU iowait 统计。
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

    /// IO 等待（可中断），正确标记 iowait。
    ///
    /// 等价于 DragonOS 的 `wait_until_io_interruptible`。
    /// - `Ok(v)`：条件满足
    /// - `Err(-ERESTART)`：被信号中断
    pub fn wait_until_io_interruptible<F>(wq: &Mutex<Self>, mut cond: F) -> WaitResult
    where
        F: FnMut() -> Option<isize>,
    {
        Self::wait_event_impl(wq, &mut cond, true, None, Some(Self::WAIT_IO_FALLBACK_MS))
    }

    pub fn wait_event_interruptible<F>(wq: &Mutex<Self>, mut cond: F) -> WaitResult
    where
        F: FnMut() -> Option<isize>,
    {
        Self::wait_event_impl(wq, &mut cond, true, None, None)
    }

    pub fn wait_event_timeout<F>(wq: &Mutex<Self>, mut cond: F, deadline: TimeSpec) -> WaitResult
    where
        F: FnMut() -> Option<isize>,
    {
        Self::wait_event_impl(wq, &mut cond, false, Some(deadline), None)
    }

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

    pub fn wait_event_locked<T, Q, F>(lock: &Mutex<T>, queue_of: Q, mut cond: F) -> WaitResult
    where
        Q: for<'a> FnMut(&'a mut T) -> &'a mut WaitQueue,
        F: FnMut(&mut T) -> Option<isize>,
    {
        Self::wait_event_locked_impl(lock, queue_of, &mut cond, false, None, None)
    }

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

/// 表示一个等待超时的任务
pub struct TimeoutWaiter {
    /// 任务的弱引用
    task: Weak<TaskControlBlock>,
    /// 任务超时时间
    timeout: TimeSpec,
}

//表示到达deadline后触发的动作
pub enum TimerAction {
    //唤醒task
    WakeTask {
        task: Weak<TaskControlBlock>,
        generation: usize,
        /// Some(ms) when this is an I/O fallback timer (1ms safety net);
        /// None when this is a deadline timer.  Stale fallback timers are
        /// re-armed with the current generation instead of spurious-waking.
        fallback_ms: Option<usize>,
    },
    //向某个task发送signal
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

//内核中的统一计时器，目前用于itimer_real
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
    /// 仅通过比较deadline字段
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

//计数器触发队列
pub struct KernelTimerQueue {
    inner: BinaryHeap<KernelTimer>,
}

impl KernelTimerQueue {
    /// 最大定时器数量，防止内存耗尽
    const MAX_TIMERS: usize = 4096;

    pub fn new() -> Self {
        Self {
            inner: BinaryHeap::new(),
        }
    }

    /// 返回当前队列中的定时器数量
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Returns the earliest deadline (ns), or 0 if the queue is empty.
    pub fn earliest_deadline_ns(&self) -> u64 {
        self.inner.peek().map(|t| t.deadline.to_ns_saturating()).unwrap_or(0)
    }

    pub fn add_action(&mut self, action: TimerAction, deadline: TimeSpec) -> bool {
        let old_earliest = self.earliest_deadline_ns();
        // Keep the hot path O(log n).  WakeTask stale entries are filtered by
        // generation, while signal timers validate their generation/deadline in run_timer().
        self.inner.push(KernelTimer { action, deadline });
        KERNEL_TIMER_QUEUE_PENDING.store(true, AtomicOrdering::Relaxed);
        if self.inner.len() > Self::MAX_TIMERS {
            self.enforce_capacity();
        }
        let new_earliest = self.earliest_deadline_ns();
        // Return true if this new timer is now the earliest (or if the queue
        // was empty before, meaning a previously-idle queue now has work).
        old_earliest == 0 || new_earliest < old_earliest
    }
    /// Pop all expired timers (up to a batch limit).
    /// Callers must hold the lock. Run callbacks OUTSIDE the lock.
    pub fn pop_expired(&mut self, now: TimeSpec) -> Vec<KernelTimer> {
        const MAX_BATCH: usize = 64;
        let mut expired = Vec::new();
        while let Some(timer) = self.inner.pop() {
            if timer.deadline > now {
                self.inner.push(timer);
                break;
            }
            expired.push(timer);
            if expired.len() >= MAX_BATCH {
                break;
            }
        }
        expired
    }
    /// 清理所有失效 Weak 引用的定时器条目，释放堆槽位。
    pub fn compact(&mut self) {
        if self.inner.is_empty() {
            return;
        }
        let entries: Vec<KernelTimer> = self.inner.drain().collect();
        for timer in entries {
            if timer.is_live() {
                self.inner.push(timer);
            }
        }
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
    /// Run a single timer callback. Must be called WITHOUT holding KERNEL_TIMER_QUEUE.
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
                    return false; // generation mismatch, stale
                }

                let mut inner = task.acquire_inner_lock();
                let should_wake = inner.task_status == super::TaskStatus::Interruptible;
                if should_wake {
                    inner.task_status = super::task::TaskStatus::Ready;
                }
                drop(inner);
                if should_wake {
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
// 二叉堆是最大堆，所以我们需要反转排序
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
    /// 仅通过比较timeout字段
    fn eq(&self, other: &Self) -> bool {
        self.timeout.eq(&other.timeout)
    }
}

/// 等待超时任务队列
pub struct TimeoutWaitQueue {
    /// 使用二叉堆存储任务（最大堆），按超时时间排序
    inner: BinaryHeap<TimeoutWaiter>,
}

impl TimeoutWaitQueue {
    /// 构造函数
    pub fn new() -> Self {
        Self {
            inner: BinaryHeap::new(),
        }
    }
    /// 这个函数会将一个`task`添加到`WaitQueue`但是**不会**阻塞这个任务，
    /// 如果想要阻塞一个`task`，使用`block_current_and_run_next()`函数
    pub fn add_task(&mut self, task: Weak<TaskControlBlock>, timeout: TimeSpec) {
        self.inner.push(TimeoutWaiter { task, timeout });
        TIMEOUT_WAITQUEUE_PENDING.store(true, AtomicOrdering::Relaxed);
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// 唤醒所有超时的任务
    pub fn wake_expired(&mut self, now: TimeSpec) {
        if self
            .inner
            .peek()
            .map(|waiter| waiter.timeout > now)
            .unwrap_or(true)
        {
            return;
        }
        let mut tasks_to_wake = Vec::new();
        // 循环处理超时任务
        while let Some(waiter) = self.inner.pop() {
            // 堆中剩下的任务还没有超时
            if waiter.timeout > now {
                // 若超时时间大于当前时间，说明后面的任务都没有超时
                self.inner.push(waiter);
                break;
            // 唤醒超时任务
            } else {
                // 将弱引用升级为强引用
                match waiter.task.upgrade() {
                    Some(task) => {
                        // 获取内部锁
                        let mut inner = task.acquire_inner_lock();
                        match inner.task_status {
                            // 若状态为可中断状态，改为就绪态
                            super::TaskStatus::Interruptible => {
                                inner.task_status = super::task::TaskStatus::Ready
                            }
                            // 对于处于 就绪态或运行态的任务，不需要做唤醒操作
                            // 对于处于僵尸态的任务，做唤醒操作会搞砸进程管理
                            _ => continue,
                        }
                        // 释放锁
                        drop(inner);
                        tasks_to_wake.push(task);
                    }
                    // task is dead, just ignore
                    None => continue,
                }
            }
        }
        enqueue_ready_batch(tasks_to_wake);
        if self.inner.is_empty() {
            TIMEOUT_WAITQUEUE_PENDING.store(false, AtomicOrdering::Relaxed);
        }
    }
    #[allow(unused)]
    // debug use only
    pub fn show_waiter(&self) {
        for waiter in self.inner.iter() {
            log::error!("[show_waiter] timeout: {:?}", waiter.timeout);
        }
    }
}

lazy_static! {
    /// 全局超时等待队列
    pub static ref TIMEOUT_WAITQUEUE: Mutex<TimeoutWaitQueue> = Mutex::new(TimeoutWaitQueue::new());
    /// 全局内核计时器队列
    pub static ref KERNEL_TIMER_QUEUE: Mutex<KernelTimerQueue> =
        Mutex::new(KernelTimerQueue::new());
}

static TIMEOUT_WAITQUEUE_PENDING: AtomicBool = AtomicBool::new(false);
static KERNEL_TIMER_QUEUE_PENDING: AtomicBool = AtomicBool::new(false);

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

/// (Re-)program the hardware timer to fire at the earliest of:
///   - the next sched tick, and
///   - the earliest KernelTimer deadline.
///
/// Caller must have local timer interrupts disabled before entering this
/// function.  It shares KERNEL_TIMER_QUEUE with the timer interrupt path.
fn reprogram_timer_irqoff() {
    let next_timer_ns = KERNEL_TIMER_QUEUE.lock().earliest_deadline_ns();
    program_next_event(next_timer_ns);
}

/// Initialise the timer subsystem: set the first sched tick and program the
/// hardware for the first event.
pub fn timer_subsystem_init() {
    let flags = local_irq_save();
    let now_ns = crate::timer::now_ns();
    NEXT_SCHED_TICK_NS.store(now_ns + SCHED_TICK_NS, AtomicOrdering::Relaxed);
    reprogram_timer_irqoff();
    local_irq_restore(flags);
}

/// 加入一个内核计时器动作
pub fn add_kernel_timer(action: TimerAction, deadline: TimeSpec) {
    let flags = local_irq_save();
    let new_is_earliest = KERNEL_TIMER_QUEUE.lock().add_action(action, deadline);
    if new_is_earliest {
        reprogram_timer_irqoff();
    }
    local_irq_restore(flags);
}

/// 这个函数会将一个`task`添加到全局超时等待队列中，但是不会阻塞它
/// 如果想要阻塞一个任务，使用`block_current_and_run_next()`函数
pub fn wait_with_timeout(task: Weak<TaskControlBlock>, timeout: TimeSpec) {
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

/// Unified timer interrupt handler — replaces the old fixed-tick
/// do_wake_expired() + set_next_trigger() pattern.
///
/// 1. Process all expired KernelTimers (callbacks run outside the lock).
/// 2. Advance the sched tick if its deadline has passed; do periodic
///    housekeeping (net poll, etc.) only on sched-tick boundaries.
/// 3. Re-program the hardware for the next deadline.
/// 4. Yield the CPU only if a task was woken or the sched tick demands it.
pub fn timer_interrupt_handler() {
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

    // 4. Yield if needed
    if need_resched || woke_task {
        crate::task::suspend_current_and_run_next();
    }
}

/// 唤醒全局超时等待队列中所有已超时的任务 (legacy — now handled by timer_interrupt_handler)
pub fn do_wake_expired() {
    let timeout_pending = TIMEOUT_WAITQUEUE_PENDING.load(AtomicOrdering::Relaxed);
    let kernel_timer_pending = KERNEL_TIMER_QUEUE_PENDING.load(AtomicOrdering::Relaxed);
    let timerfd_pending = crate::fs::timerfd::timerfd_registry_maybe_nonempty();
    if !timeout_pending && !kernel_timer_pending && !timerfd_pending {
        return;
    }

    let mut now = None;
    if timeout_pending {
        let flags = local_irq_save();
        let mut timeout_queue = TIMEOUT_WAITQUEUE.lock();
        if timeout_queue.is_empty() {
            TIMEOUT_WAITQUEUE_PENDING.store(false, AtomicOrdering::Relaxed);
        } else {
            let now = *now.get_or_insert_with(crate::timer::TimeSpec::now);
            timeout_queue.wake_expired(now);
        }
        drop(timeout_queue);
        local_irq_restore(flags);
    }

    if kernel_timer_pending {
        let expired_timers = {
            let flags = local_irq_save();
            let mut ktq = KERNEL_TIMER_QUEUE.lock();
            let expired = if ktq.is_empty() {
                KERNEL_TIMER_QUEUE_PENDING.store(false, AtomicOrdering::Relaxed);
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
                }
                expired
            };
            drop(ktq);
            local_irq_restore(flags);
            expired
        }; // ← lock released

        // Run callbacks outside the lock
        let now = now.unwrap_or_else(crate::timer::TimeSpec::now);
        for timer in expired_timers {
            KernelTimerQueue::run_timer(timer, now);
        }
    }

    if timerfd_pending && !crate::fs::timerfd::timerfd_registry_is_empty() {
        let now = *now.get_or_insert_with(crate::timer::TimeSpec::now);
        crate::fs::timerfd::wake_expired_timerfds(now);
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
