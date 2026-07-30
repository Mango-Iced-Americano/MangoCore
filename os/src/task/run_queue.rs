//! Per-CPU runnable 队列及其唯一所有权操作。
//!
//! 本模块只管理 `Queued(cpu)` 任务；interruptible/zombie/timer registry 仍由
//! `TaskManager` 管理。所有入口至多锁定一个 runqueue，且锁内不获取
//! `task.inner`。普通任务仍首次发布到 CPU0；blocked 任务可以回到最近运行 CPU。

use super::{TaskControlBlock, TaskStatus};
use alloc::{collections::VecDeque, sync::Arc};
use core::sync::atomic::Ordering;

/// 单个 CPU 独占的 runnable 容器。
pub(crate) struct RunQueue {
    tasks: VecDeque<Arc<TaskControlBlock>>,
    nonzero_nice_count: usize,
}

impl RunQueue {
    pub(crate) const fn new() -> Self {
        Self {
            tasks: VecDeque::new(),
            nonzero_nice_count: 0,
        }
    }

    fn note_inserted(&mut self, task: &Arc<TaskControlBlock>) {
        if task.sched_nice_hint.load(Ordering::Relaxed) != 0 {
            self.nonzero_nice_count += 1;
        }
    }

    fn note_removed(&mut self, task: &Arc<TaskControlBlock>) {
        if task.sched_nice_hint.load(Ordering::Relaxed) != 0 {
            self.nonzero_nice_count = self.nonzero_nice_count.saturating_sub(1);
        }
    }

    fn push_back(&mut self, task: Arc<TaskControlBlock>) {
        self.note_inserted(&task);
        self.tasks.push_back(task);
        crate::task::perf::record_taskq_add_ready();
    }

    fn push_front(&mut self, task: Arc<TaskControlBlock>) {
        self.note_inserted(&task);
        self.tasks.push_front(task);
        crate::task::perf::record_taskq_add_ready();
    }

    fn pop_next(&mut self) -> Option<Arc<TaskControlBlock>> {
        let task = if self.nonzero_nice_count == 0 {
            crate::task::perf::record_taskq_fetch(false, 0);
            self.tasks.pop_front()
        } else {
            let scan_depth = self.tasks.len();
            crate::task::perf::record_taskq_fetch(true, scan_depth);
            let mut best = 0usize;
            let mut best_key = sched_pick_key(self.tasks.front()?);
            for (index, task) in self.tasks.iter().enumerate().skip(1) {
                let key = sched_pick_key(task);
                if key < best_key {
                    best = index;
                    best_key = key;
                }
            }
            self.tasks.remove(best)
        }?;
        self.note_removed(&task);
        Some(task)
    }

    fn recompute_nice_count(&mut self) {
        self.nonzero_nice_count = self
            .tasks
            .iter()
            .filter(|task| task.sched_nice_hint.load(Ordering::Relaxed) != 0)
            .count();
    }
}

fn sched_pick_key(task: &Arc<TaskControlBlock>) -> (u64, i32, usize) {
    (
        task.sched_vruntime_hint.load(Ordering::Relaxed),
        task.sched_nice_hint.load(Ordering::Relaxed),
        task.gettid(),
    )
}

fn state(cpu: usize) -> &'static super::processor::CpuTaskState {
    crate::smp::task_state(cpu)
}

fn add_running(cpu: usize) {
    state(cpu).nr_running.fetch_add(1, Ordering::Relaxed);
}

fn sub_running(cpu: usize, count: usize) {
    if count != 0 {
        let _ = state(cpu).nr_running.fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |value| Some(value.saturating_sub(count)),
        );
    }
}

/// 首次把构造完成的任务发布到目标 CPU。
pub(crate) fn publish(task: Arc<TaskControlBlock>, cpu: usize) {
    let mut queue = state(cpu).run_queue.lock();
    task.require_sched_transition(TaskStatus::New, TaskStatus::Queued(cpu), "publish new task");
    queue.push_back(task);
    add_running(cpu);
}

/// 把已经切回源 idle 栈的运行任务交给目标 runqueue。
///
/// 源 current 已在调用前清空，因此这里只锁目标队列就能完成唯一所有权交接；
/// `source_cpu != target_cpu` 时即为一次协作式迁移。
pub(crate) fn requeue_after_switch(
    task: Arc<TaskControlBlock>,
    source_cpu: usize,
    target_cpu: usize,
) {
    let mut queue = state(target_cpu).run_queue.lock();
    task.require_sched_transition(
        TaskStatus::Running(source_cpu),
        TaskStatus::Queued(target_cpu),
        "requeue task after switch-out",
    );
    queue.push_back(task);
    add_running(target_cpu);
}

/// 唤醒方在持有 interruptible registry 锁时提交 Blocked -> Queued。
pub(crate) fn enqueue_woken(task: Arc<TaskControlBlock>, cpu: usize) {
    let mut queue = state(cpu).run_queue.lock();
    task.require_sched_transition(
        TaskStatus::Blocked,
        TaskStatus::Queued(cpu),
        "wake blocked task",
    );
    queue.push_front(task);
    add_running(cpu);
}

/// 从本 CPU 队列取得下一个任务，并在同一锁域交接为 current owner。
pub(crate) fn fetch(cpu: usize) -> Option<Arc<TaskControlBlock>> {
    let mut queue = state(cpu).run_queue.lock();
    let task = queue.pop_next()?;
    sub_running(cpu, 1);
    task.require_sched_transition(
        TaskStatus::Queued(cpu),
        TaskStatus::Running(cpu),
        "fetch ready task",
    );
    // 从这一刻起任务已由本 CPU current 路径唯一拥有。后续 Running ->
    // Blocking -> Blocked 的 AcqRel 状态链会把该提示发布给远程唤醒方。
    task.note_running_cpu(cpu);
    Some(task)
}

/// 从其 owner runqueue 撤回任务，并交给调用方推进退出流程。
pub(crate) fn remove(task: &Arc<TaskControlBlock>) -> bool {
    let TaskStatus::Queued(cpu) = task.task_status() else {
        return false;
    };
    let mut queue = state(cpu).run_queue.lock();
    let Some(index) = queue.tasks.iter().position(|queued| Arc::ptr_eq(queued, task)) else {
        if task.task_status() == TaskStatus::Queued(cpu) {
            panic!("Queued task is absent from owner runqueue: tid={} cpu={}", task.gettid(), cpu);
        }
        return false;
    };
    task.require_sched_transition(
        TaskStatus::Queued(cpu),
        TaskStatus::Blocked,
        "remove queued task",
    );
    let removed = queue.tasks.remove(index).expect("located runqueue entry vanished");
    queue.note_removed(&removed);
    // TCB 析构可能继续释放内核栈或进程资源，不能发生在 runqueue 锁内。
    drop(queue);
    sub_running(cpu, 1);
    drop(removed);
    true
}

/// nice hint 已更新后，修正 owner runqueue 的选择快速路径计数。
pub(crate) fn update_nice(task: &Arc<TaskControlBlock>, old_nice: i32, new_nice: i32) {
    if (old_nice == 0) == (new_nice == 0) {
        return;
    }
    let TaskStatus::Queued(cpu) = task.task_status() else {
        return;
    };
    let mut queue = state(cpu).run_queue.lock();
    if !queue.tasks.iter().any(|queued| Arc::ptr_eq(queued, task)) {
        return;
    }
    // hint 的写入发生在取得 runqueue 锁之前；任务也可能恰在期间入队。
    // 重新计算可同时覆盖“已在队列中改 nice”和“用新 hint 刚入队”两种时序。
    queue.recompute_nice_count();
}

/// 返回指定 CPU 的精确队列统计，供诊断与 focused test 使用。
pub(crate) fn stats(cpu: usize) -> (usize, usize, usize) {
    let queue = state(cpu).run_queue.lock();
    let zombies = queue.tasks.iter().filter(|task| task.is_zombie()).count();
    (queue.tasks.len(), zombies, queue.nonzero_nice_count)
}

/// 返回所有 per-CPU runqueue 的无锁近似总长度。
pub(crate) fn total_count_fast() -> usize {
    (0..crate::smp::configured_cpu_count())
        .map(|cpu| state(cpu).nr_running.load(Ordering::Relaxed))
        .sum()
}

/// OOM 路径按索引克隆一个候选，避免低内存时为队列快照再次分配。
pub(crate) fn task_at(cpu: usize, index: usize) -> Option<Arc<TaskControlBlock>> {
    state(cpu).run_queue.lock().tasks.get(index).cloned()
}
