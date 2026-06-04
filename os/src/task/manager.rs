/*
    此文件用于管理任务的调度
    内容与RISCV版本相同，无需修改
*/
use core::cmp::Ordering;
use core::sync::atomic::Ordering as AtomicOrdering;

#[cfg(feature = "oom_handler")]
use crate::config::SYSTEM_TASK_LIMIT;
use alloc::vec::Vec;

use crate::timer::{TimeSpec, TimeVal};

use super::{
    block_current_and_run_next_checked, block_current_and_run_next_with_lock_checked, current_task,
    discard_non_actionable_unblocked_signals, has_actionable_signal, signal::{SigInfo, Signals},
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
    ready_nonzero_nice_count: usize,
    /// 任务激活状态跟踪器，用于跟踪任务的激活状态，并在OOM时释放内存
    pub active_tracker: ActiveTracker,
}

#[cfg(not(feature = "oom_handler"))]
pub struct TaskManager {
    pub ready_queue: VecDeque<Arc<TaskControlBlock>>,
    pub interruptible_queue: VecDeque<Arc<TaskControlBlock>>,
    ready_nonzero_nice_count: usize,
}

fn sched_pick_key(task: &Arc<TaskControlBlock>) -> (u64, i32, usize) {
    let inner = task.acquire_inner_lock();
    (inner.sched_vruntime, inner.sched_nice, task.gettid())
}

fn task_has_nonzero_nice(task: &Arc<TaskControlBlock>) -> bool {
    task.acquire_inner_lock().sched_nice != 0
}

fn count_ready_nonzero_nice(queue: &VecDeque<Arc<TaskControlBlock>>) -> usize {
    queue.iter().filter(|task| task_has_nonzero_nice(task)).count()
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

/// 简化的 nice-aware 调度器。
impl TaskManager {
    #[cfg(feature = "oom_handler")]
    /// 构造函数
    pub fn new() -> Self {
        Self {
            ready_queue: VecDeque::new(),
            interruptible_queue: VecDeque::new(),
            ready_nonzero_nice_count: 0,
            active_tracker: ActiveTracker::new(),
        }
    }
    #[cfg(not(feature = "oom_handler"))]
    pub fn new() -> Self {
        Self {
            ready_queue: VecDeque::new(),
            interruptible_queue: VecDeque::new(),
            ready_nonzero_nice_count: 0,
        }
    }
    /// 添加一个任务到就绪队列
    pub fn add(&mut self, task: Arc<TaskControlBlock>) {
        if task_has_nonzero_nice(&task) {
            self.ready_nonzero_nice_count += 1;
        }
        self.ready_queue.push_back(task);
    }
    fn pop_next_ready(&mut self) -> Option<Arc<TaskControlBlock>> {
        let task = if self.ready_nonzero_nice_count == 0 {
            self.ready_queue.pop_front()
        } else {
            pop_fair_ready(&mut self.ready_queue)
        }?;
        if task_has_nonzero_nice(&task) {
            self.ready_nonzero_nice_count = self.ready_nonzero_nice_count.saturating_sub(1);
        }
        Some(task)
    }
    fn recompute_ready_nice_count(&mut self) {
        self.ready_nonzero_nice_count = count_ready_nonzero_nice(&self.ready_queue);
    }
    /// 从就绪队列中逐出一个僵尸任务（零堆分配）。
    /// 每次调用最多 remove 一个元素，drop 发生在锁外。
    fn take_one_ready_zombie(&mut self) -> Option<Arc<TaskControlBlock>> {
        for i in 0..self.ready_queue.len() {
            if self.ready_queue[i].acquire_inner_lock().is_zombie() {
                let zombie = self.ready_queue.remove(i).unwrap();
                self.recompute_ready_nice_count();
                return Some(zombie);
            }
        }
        None
    }
    /// 从可中断队列中逐出一个僵尸任务（零堆分配）。
    fn take_one_interruptible_zombie(&mut self) -> Option<Arc<TaskControlBlock>> {
        for i in 0..self.interruptible_queue.len() {
            if self.interruptible_queue[i].acquire_inner_lock().is_zombie() {
                return self.interruptible_queue.remove(i);
            }
        }
        None
    }
    /// 从就绪 + 可中断队列中移除属于指定 pid 的所有 zombie TCB。
    /// 返回收集到的 zombie Arc，由调用者负责在锁外 drop。
    fn remove_zombie_tasks_by_pid(&mut self, pid: usize) -> alloc::vec::Vec<Arc<TaskControlBlock>> {
        let mut zombies = alloc::vec::Vec::new();
        self.ready_queue.retain(|task| {
            let is_match = task.acquire_inner_lock().is_zombie() && task.process.pid == pid;
            if is_match {
                zombies.push(task.clone());
                false
            } else {
                true
            }
        });
        self.interruptible_queue.retain(|task| {
            let is_match = task.acquire_inner_lock().is_zombie() && task.process.pid == pid;
            if is_match {
                zombies.push(task.clone());
                false
            } else {
                true
            }
        });
        self.recompute_ready_nice_count();
        zombies
    }
    fn update_ready_nice(&mut self, task: &Arc<TaskControlBlock>, old_nice: i32, new_nice: i32) {
        if (old_nice == 0) == (new_nice == 0) {
            return;
        }
        if !self.ready_queue.iter().any(|queued| task_ptr_eq(queued, task)) {
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
    }
    /// 从可中断队列中删除一个任务
    pub fn drop_interruptible(&mut self, task: &Arc<TaskControlBlock>) {
        self.interruptible_queue
            // 使用retain过滤掉与指定任务相同的任务
            .retain(|task_in_queue| Arc::as_ptr(task_in_queue) != Arc::as_ptr(task));
    }
    fn enqueue_ready_batch(&mut self, tasks: Vec<Arc<TaskControlBlock>>) -> usize {
        if tasks.is_empty() {
            return 0;
        }
        let ptrs = sorted_task_ptrs(&tasks);
        self.interruptible_queue
            .retain(|task| !task_ptr_in(&ptrs, task));
        let count = tasks.len();
        for task in tasks {
            self.add(task);
        }
        count
    }
    /// 从调度器的 ready / interruptible 队列中移除一组任务。
    /// 线程组退出和 exec 清理只能通过这个入口调整队列，避免业务层直接扫描队列。
    pub fn remove_tasks(&mut self, tasks: &[Arc<TaskControlBlock>]) -> usize {
        let ptrs = sorted_task_ptrs(tasks);

        let old_ready_len = self.ready_queue.len();
        self.ready_queue
            .retain(|task| !task_ptr_in(&ptrs, task));
        self.recompute_ready_nice_count();
        let old_interruptible_len = self.interruptible_queue.len();
        self.interruptible_queue
            .retain(|task| !task_ptr_in(&ptrs, task));

        (old_ready_len - self.ready_queue.len())
            + (old_interruptible_len - self.interruptible_queue.len())
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
            Err(_) => {
                log::trace!("[wake_interruptible] already waken");
            }
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
        self.drop_interruptible(&task);
        // 如果任务不在就绪队列中，将其加入就绪队列
        if !self
            .ready_queue
            .iter()
            .any(|task_in_queue| Arc::as_ptr(task_in_queue) == Arc::as_ptr(&task))
        {
            self.add(task);
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

/// 从任务管理器中取出一个任务
pub fn fetch_task() -> Option<Arc<TaskControlBlock>> {
    TASK_MANAGER.lock().fetch()
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
    let manager = TASK_MANAGER.lock();
    manager.ready_count() + manager.interruptible_count()
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
        let mut tasks_to_wake = Vec::new();
        let mut remaining = VecDeque::new();
        // 遍历全部条目以自动 compact 失效 Weak，但只唤醒 ≤limit 个任务。
        while let Some(task) = self.inner.pop_front() {
            match task.upgrade() {
                Some(task) => {
                    let mut inner = task.acquire_inner_lock();
                    match inner.task_status {
                        super::TaskStatus::Interruptible => {
                            if tasks_to_wake.len() < limit {
                                inner.task_status = super::task::TaskStatus::Ready;
                                drop(inner);
                                tasks_to_wake.push(task);
                            } else {
                                drop(inner);
                                remaining.push_back(Arc::downgrade(&task));
                            }
                        }
                        // 非 Interruptible 状态：直接丢弃（不应在等待队列中）
                        _ => drop(inner),
                    }
                }
                // 失效 Weak：直接丢弃，实现自动 compact
                None => {}
            }
        }
        self.inner = remaining;
        enqueue_ready_batch(tasks_to_wake)
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
    pub fn finish_wait(&mut self, task: &Arc<TaskControlBlock>) -> bool {
        let old_len = self.inner.len();
        self.inner
            .retain(|task_in_queue| Weak::as_ptr(task_in_queue) != Arc::as_ptr(task));
        let removed = self.inner.len() != old_len;
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
                guard.finish_wait(&task);
                return WaitResult::Ready(res);
            }
            if deadline
                .map(|deadline| TimeSpec::now() >= deadline)
                .unwrap_or(false)
            {
                guard.finish_wait(&task);
                return WaitResult::TimedOut;
            }
            if signal_check {
                if has_actionable_signal(&task) {
                    guard.finish_wait(&task);
                    return WaitResult::Interrupted;
                }
                discard_non_actionable_unblocked_signals(&task);
            }

            if let Some(deadline) = deadline {
                wait_with_timeout(Arc::downgrade(&task), deadline);
            } else if let Some(ms) = fallback_ms {
                if !task
                    .wait_io_timer_pending
                    .swap(true, AtomicOrdering::AcqRel)
                {
                    wait_with_timeout(
                        Arc::downgrade(&task),
                        TimeSpec::now() + TimeSpec::from_ms(ms),
                    );
                }
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
                queue_of(&mut guard).finish_wait(&task);
                return WaitResult::Ready(res);
            }
            if deadline
                .map(|deadline| TimeSpec::now() >= deadline)
                .unwrap_or(false)
            {
                queue_of(&mut guard).finish_wait(&task);
                return WaitResult::TimedOut;
            }
            if signal_check {
                if has_actionable_signal(&task) {
                    queue_of(&mut guard).finish_wait(&task);
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

    fn finish_wait_on_queues(queues: &[&Mutex<Self>], task: &Arc<TaskControlBlock>) {
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
                        let fallback = TimeSpec::now() + TimeSpec::from_ms(Self::WAIT_IO_FALLBACK_MS);
                        if fallback < deadline {
                            fallback
                        } else {
                            deadline
                        }
                    }
                    None => TimeSpec::now() + TimeSpec::from_ms(Self::WAIT_IO_FALLBACK_MS),
                };
                match Self::wait_event_interruptible_timeout(&wait_queue, &mut cond, next_deadline) {
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
                Self::finish_wait_on_queues(queues, &task);
                return WaitResult::Ready(res);
            }
            if deadline
                .map(|deadline| TimeSpec::now() >= deadline)
                .unwrap_or(false)
            {
                Self::finish_wait_on_queues(queues, &task);
                return WaitResult::TimedOut;
            }
            if has_actionable_signal(&task) {
                Self::finish_wait_on_queues(queues, &task);
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

    pub fn wait_event_timeout<F>(
        wq: &Mutex<Self>,
        mut cond: F,
        deadline: TimeSpec,
    ) -> WaitResult
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

    pub fn wait_event_locked<T, Q, F>(
        lock: &Mutex<T>,
        queue_of: Q,
        mut cond: F,
    ) -> WaitResult
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

//计数器触发队列
pub struct KernelTimerQueue {
    inner: BinaryHeap<KernelTimer>,
}

/// 判断两个 TimerAction 是否指向同一个"槽位"（用于去重）：
/// - WakeTask：比较 task 指针
/// - SendSignal：比较 task 指针 + 信号类型
fn same_action_slot(a: &TimerAction, b: &TimerAction) -> bool {
    match (a, b) {
        (TimerAction::WakeTask { task: ta, .. }, TimerAction::WakeTask { task: tb, .. }) => {
            Weak::as_ptr(ta) == Weak::as_ptr(tb)
        }
        (
            TimerAction::SendSignal {
                task: ta,
                signal: sa,
                ..
            },
            TimerAction::SendSignal {
                task: tb,
                signal: sb,
                ..
            },
        ) => Weak::as_ptr(ta) == Weak::as_ptr(tb) && *sa == *sb,
        _ => false,
    }
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

    pub fn add_action(&mut self, action: TimerAction, deadline: TimeSpec) {
        // 去重：扫描已有条目，相同 slot 只保留 deadline 最早的
        // 用 Option 包装 action，避免 borrow checker 的移动语义问题
        let old_entries: Vec<KernelTimer> = self.inner.drain().collect();
        let mut action = Some(action);
        for entry in old_entries {
            if let Some(ref new_action) = action {
                if same_action_slot(&entry.action, new_action) {
                    // 相同 slot，保留 deadline 更早的
                    if deadline < entry.deadline {
                        // 新的更早：替换旧的
                        self.inner.push(KernelTimer {
                            action: action.take().unwrap(),
                            deadline,
                        });
                    } else {
                        // 旧得更早：保留旧的，丢弃新的
                        self.inner.push(entry);
                        action = None;
                    }
                    continue;
                }
            }
            self.inner.push(entry);
        }
        // action 未被消耗 → 没有匹配的 slot，直接加入
        if let Some(action) = action {
            self.inner.push(KernelTimer { action, deadline });
        }

        // 容量上限：丢弃 deadline 最远的条目
        while self.inner.len() > Self::MAX_TIMERS {
            if let Some(t) = self.inner.pop() {
                log::warn!(
                    "[KernelTimerQueue] capacity limit ({}) reached, discarding deadline={:?}",
                    Self::MAX_TIMERS,
                    t.deadline
                );
            }
        }
    }
    pub fn wake_expired(&mut self, now: TimeSpec) {
        while let Some(timer) = self.inner.pop() {
            if timer.deadline > now {
                self.inner.push(timer);
                break;
            }
            self.run_timer(timer, now);
        }
    }
    /// 清理所有失效 Weak 引用的定时器条目，释放堆槽位。
    pub fn compact(&mut self) {
        if self.inner.is_empty() {
            return;
        }
        let entries: Vec<KernelTimer> = self.inner.drain().collect();
        for timer in entries {
            let task = match &timer.action {
                TimerAction::WakeTask { task, .. }
                | TimerAction::SendSignal { task, .. }
                | TimerAction::PosixTimerSignal { task, .. } => task,
            };
            if task.strong_count() > 0 {
                self.inner.push(timer);
            }
        }
    }
    fn run_timer(&mut self, timer: KernelTimer, now: TimeSpec) {
        match timer.action {
            TimerAction::WakeTask {
                task,
                generation: _,
            } => {
                if let Some(task) = task.upgrade() {
                    // Option A：无条件清除 pending 标志。
                    // 无论任务是否已被提前唤醒，定时器既已触发，槽位即释放。
                    task.wait_io_timer_pending
                        .store(false, AtomicOrdering::Release);

                    let mut inner = task.acquire_inner_lock();
                    let should_wake = inner.task_status == super::TaskStatus::Interruptible;
                    if should_wake {
                        inner.task_status = super::task::TaskStatus::Ready;
                    }
                    drop(inner);
                    if should_wake {
                        wake_interruptible(task);
                    }
                }
            }
            TimerAction::SendSignal {
                task,
                signal,
                generation,
            } => {
                if signal.is_empty() {
                    return;
                }
                if let Some(task) = task.upgrade() {
                    let mut should_wake = false;
                    let mut next_real_timer = None;
                    {
                        let mut inner = task.acquire_inner_lock();
                        if signal == Signals::SIGALRM {
                            if inner.real_timer_generation != generation
                                || inner.real_timer_deadline != Some(timer.deadline)
                            {
                                return;
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
                        if inner.task_status == super::TaskStatus::Interruptible {
                            inner.task_status = super::TaskStatus::Ready;
                            should_wake = true;
                        }
                    }
                    if should_wake {
                        wake_interruptible(task.clone());
                    }
                    if let Some((deadline, next_generation)) = next_real_timer {
                        self.add_action(
                            TimerAction::SendSignal {
                                task: Arc::downgrade(&task),
                                signal,
                                generation: next_generation,
                            },
                            deadline,
                        );
                    }
                }
            }
            TimerAction::PosixTimerSignal {
                task,
                timer_id,
                signal,
                generation,
            } => {
                if let Some(task) = task.upgrade() {
                    let mut should_wake = false;
                    let mut next_timer = None;
                    {
                        let mut inner = task.acquire_inner_lock();
                        let signal_pending =
                            !signal.is_empty() && inner.sigpending.contains(signal);
                        let Some(Some(timer_state)) = inner.posix_timers.get_mut(timer_id) else {
                            return;
                        };
                        if timer_state.generation != generation
                            || timer_state.deadline != Some(timer.deadline)
                        {
                            return;
                        }
                        if timer_state.interval.is_zero() {
                            timer_state.value = TimeSpec::new();
                            timer_state.deadline = None;
                        } else {
                            let interval_ns = timer_state.interval.to_ns().max(1);
                            let deadline_ns = timer.deadline.to_ns();
                            let elapsed_ns = now.to_ns().saturating_sub(deadline_ns);
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
                            timer_state.generation = timer_state.generation.wrapping_add(1);
                            timer_state.value = timer_state.interval;
                            timer_state.deadline = Some(deadline);
                            next_timer = Some((deadline, timer_state.generation));
                        }
                        if !signal.is_empty() {
                            let _ = inner
                                .sigpending
                                .enqueue_signal(signal, SigInfo::SI_TIMER as usize);
                            if inner.task_status == super::TaskStatus::Interruptible {
                                inner.task_status = super::TaskStatus::Ready;
                                should_wake = true;
                            }
                        }
                    }
                    if should_wake {
                        wake_interruptible(task.clone());
                    }
                    if let Some((deadline, next_generation)) = next_timer {
                        self.add_action(
                            TimerAction::PosixTimerSignal {
                                task: Arc::downgrade(&task),
                                timer_id,
                                signal,
                                generation: next_generation,
                            },
                            deadline,
                        );
                    }
                }
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
    }
    /// 唤醒所有超时的任务
    pub fn wake_expired(&mut self, now: TimeSpec) {
        let mut tasks_to_wake = Vec::new();
        // 循环处理超时任务
        while let Some(waiter) = self.inner.pop() {
            // 堆中剩下的任务还没有超时
            if waiter.timeout > now {
                // 若超时时间大于当前时间，说明后面的任务都没有超时
                log::trace!(
                    "[wake_expired] no more expired, next pending task timeout: {:?}, now: {:?}",
                    waiter.timeout,
                    now
                );
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
                        log::trace!(
                            "[wake_expired] tid: {}, pid: {}, timeout: {:?}",
                            task.tid.0,
                            task.pid(),
                            waiter.timeout
                        );
                        tasks_to_wake.push(task);
                    }
                    // task is dead, just ignore
                    None => continue,
                }
            }
        }
        enqueue_ready_batch(tasks_to_wake);
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

/// 加入一个内核计时器动作
pub fn add_kernel_timer(action: TimerAction, deadline: TimeSpec) {
    KERNEL_TIMER_QUEUE.lock().add_action(action, deadline);
}

/// 这个函数会将一个`task`添加到全局超时等待队列中，但是不会阻塞它
/// 如果想要阻塞一个任务，使用`block_current_and_run_next()`函数
pub fn wait_with_timeout(task: Weak<TaskControlBlock>, timeout: TimeSpec) {
    KERNEL_TIMER_QUEUE.lock().add_action(
        TimerAction::WakeTask {
            task,
            generation: 0,
        },
        timeout,
    )
}

/// 唤醒全局超时等待队列中所有已超时的任务
pub fn do_wake_expired() {
    let now = crate::timer::TimeSpec::now();
    TIMEOUT_WAITQUEUE.lock().wake_expired(now);
    let mut ktq = KERNEL_TIMER_QUEUE.lock();
    ktq.wake_expired(now);
    // compact 涉及 BinaryHeap::drain → Vec 堆分配，降频执行：
    // 仅在队列半满时每 64 个 tick 执行一次。
    static COMPACT_TICK: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    let tick = COMPACT_TICK.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    if tick % 64 == 0 && ktq.len() > KernelTimerQueue::MAX_TIMERS / 2 {
        ktq.compact();
    }
    drop(ktq);
    crate::fs::timerfd::wake_expired_timerfds(now);
}

/// 获取内核计时器队列长度（诊断用，尝试获取锁）
pub fn kernel_timer_queue_len() -> Option<usize> {
    KERNEL_TIMER_QUEUE.try_lock().map(|q| q.len())
}

/// 获取任务管理器中就绪和可中断任务数量（诊断用，尝试获取锁）
pub fn task_manager_counts() -> Option<(u16, u16)> {
    TASK_MANAGER
        .try_lock()
        .map(|m| (m.ready_count(), m.interruptible_count()))
}

/// 返回所有活跃的 PID 列表
pub fn all_pids() -> alloc::vec::Vec<usize> {
    super::registry::all_processes()
        .into_iter()
        .map(|process| process.pid)
        .collect()
}
