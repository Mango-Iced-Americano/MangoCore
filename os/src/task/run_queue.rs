//! Per-CPU runnable 队列及其唯一所有权操作。
//!
//! 本模块管理 `Queued(cpu)` 容器及 queued 任务的跨队列搬迁；
//! interruptible/zombie/timer registry 仍由 `TaskManager` 管理。搬迁通过短暂的
//! `Migrating` 交还唯一所有权，所有入口至多锁定一个 runqueue，且锁内不获取
//! `task.inner`。每个入队入口还必须验证目标属于任务的 `cpus_allowed`。

use super::{TaskControlBlock, TaskStatus};
use alloc::{collections::VecDeque, sync::Arc};
use core::{hint::spin_loop, sync::atomic::Ordering};

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

    fn remove_at(&mut self, index: usize) -> Arc<TaskControlBlock> {
        let task = self
            .tasks
            .remove(index)
            .expect("located runqueue entry vanished");
        self.note_removed(&task);
        task
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

/// 从允许集合中选择一个当前可调度的 CPU。
///
/// `preferred` 只表达 locality，不拥有任务。它合法且负载不超过最小值 `+1`
/// 时保留原 CPU；否则选择近似负载最小、编号最小的 CPU。所有计数都是无锁
/// 快照，只影响放置质量，真正 owner 仍由后续 runqueue 临界区提交。
pub(crate) fn select_runnable_cpu(allowed: usize, preferred: Option<usize>) -> usize {
    let online = crate::smp::online_cpu_mask();
    let schedulers = crate::smp::scheduler_cpu_mask();
    let stopped = crate::smp::stopped_cpu_mask();
    let runnable = allowed & online & schedulers & !stopped;
    assert_ne!(
        runnable, 0,
        "task has no runnable CPU: allowed={:#x} online={:#x} schedulers={:#x} stopped={:#x}",
        allowed, online, schedulers, stopped
    );

    let load = |cpu| nr_running(cpu) + super::processor::cpu_current_count(cpu);
    let minimum = (0..crate::smp::configured_cpu_count())
        .filter(|cpu| runnable & (1usize << cpu) != 0)
        .min_by_key(|cpu| (load(*cpu), *cpu))
        .expect("runnable CPU mask is empty");

    preferred
        .filter(|cpu| {
            *cpu < crate::smp::configured_cpu_count()
                && runnable & (1usize << *cpu) != 0
                && load(*cpu) <= load(minimum).saturating_add(1)
        })
        .unwrap_or(minimum)
}

/// 首次把构造完成的任务发布到目标 CPU。
pub(crate) fn publish(task: Arc<TaskControlBlock>, cpu: usize) {
    // 先验证 placement，再取得目标队列锁；失败不会留下半发布的状态或容器项。
    task.require_cpu_allowed(cpu, "publish new task");
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
    // 请求登记与实际切出之间可能隔着任意用户执行时间。当前 mask 发布后
    // 不变，但仍在最终 owner 交接处复核，给后续动态 affinity 留下硬边界。
    task.require_cpu_allowed(target_cpu, "requeue task after switch-out");
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
    // Blocked 不拥有 CPU；唤醒方选出的新 owner 必须满足任务允许集合。
    task.require_cpu_allowed(cpu, "wake blocked task");
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

/// 在 owner runqueue 内更新 queued 任务 affinity，必要时搬到另一 CPU。
///
/// 返回 `Ok(Some(cpu))` 表示调用者需在所有队列锁释放后通知目标 CPU；
/// `Ok(None)` 表示旧 owner 仍合法、只更新了 mask；`Err(status)` 表示任务已被
/// fetch、阻塞或退出。两个并发写侧通过 `Migrating` 重试并按实际完成顺序线性化。
pub(crate) fn set_queued_affinity(
    task: &Arc<TaskControlBlock>,
    mask: usize,
) -> Result<Option<usize>, TaskStatus> {
    loop {
        let source_cpu = match task.task_status() {
            TaskStatus::Queued(cpu) => cpu,
            TaskStatus::Migrating => {
                // 迁移方进入该状态前已完成所有 TLB 等待，之后不会获取
                // TASK_MANAGER；这里只等待两个短 runqueue 临界区之间的交接。
                spin_loop();
                continue;
            }
            status => return Err(status),
        };

        let target_cpu = if mask & (1usize << source_cpu) == 0 {
            let target = select_runnable_cpu(mask, None);

            // 必须在取得源 rq 锁、更不能在进入 Migrating 后等待 shootdown；
            // 同步失败时任务仍完整地留在原队列和旧 mask 下。
            crate::smp::synchronize_kernel_mapping(target).unwrap_or_else(|error| {
                panic!(
                    "failed to synchronize queued task {} stack to CPU {}: {:?}",
                    task.gettid(),
                    target,
                    error
                )
            });
            Some(target)
        } else {
            None
        };

        let mut source = state(source_cpu).run_queue.lock();
        let Some(index) = source
            .tasks
            .iter()
            .position(|queued| Arc::ptr_eq(queued, task))
        else {
            let actual = task.task_status();
            drop(source);
            match actual {
                TaskStatus::Migrating | TaskStatus::Queued(_) => continue,
                status => return Err(status),
            }
        };

        let Some(target_cpu) = target_cpu else {
            task.store_cpus_allowed(
                mask,
                TaskStatus::Queued(source_cpu),
                "update queued affinity",
            );
            return Ok(None);
        };

        // 状态先从源 owner 交给迁移调用方，再摘除容器；源 rq 锁使 fetch
        // 无法观察中间步骤。离开本临界区后任务不属于任何 rq/current。
        task.require_sched_transition(
            TaskStatus::Queued(source_cpu),
            TaskStatus::Migrating,
            "detach queued task for affinity",
        );
        let moved = source.remove_at(index);
        sub_running(source_cpu, 1);
        drop(source);

        // mask 在无 CPU owner 的窗口发布。目标状态的 AcqRel 交接保证 fetch
        // 在取得任务前看见它，且旧 owner 已经无法再次选择该 TCB。
        task.store_cpus_allowed(mask, TaskStatus::Migrating, "move queued affinity");
        task.require_cpu_allowed(target_cpu, "move queued task for affinity");

        let mut target = state(target_cpu).run_queue.lock();
        task.require_sched_transition(
            TaskStatus::Migrating,
            TaskStatus::Queued(target_cpu),
            "attach queued task after affinity",
        );
        target.push_back(moved);
        add_running(target_cpu);
        return Ok(Some(target_cpu));
    }
}

/// 从其 owner runqueue 撤回任务，并交给调用方推进退出流程。
pub(crate) fn remove(task: &Arc<TaskControlBlock>) -> bool {
    loop {
        let cpu = match task.task_status() {
            TaskStatus::Queued(cpu) => cpu,
            TaskStatus::Migrating => {
                // exit/exec 可能持有 TASK_MANAGER。迁移路径在发布 Migrating
                // 后绝不获取该锁或等待 IPI ack，所以这里不存在锁依赖环。
                spin_loop();
                continue;
            }
            _ => return false,
        };
        let mut queue = state(cpu).run_queue.lock();
        let Some(index) = queue
            .tasks
            .iter()
            .position(|queued| Arc::ptr_eq(queued, task))
        else {
            let actual = task.task_status();
            drop(queue);
            match actual {
                TaskStatus::Migrating => continue,
                TaskStatus::Queued(owner) if owner != cpu => continue,
                TaskStatus::Queued(_) => panic!(
                    "Queued task is absent from owner runqueue: tid={} cpu={}",
                    task.gettid(),
                    cpu
                ),
                _ => return false,
            }
        };
        task.require_sched_transition(
            TaskStatus::Queued(cpu),
            TaskStatus::Blocked,
            "remove queued task",
        );
        let removed = queue.remove_at(index);
        // TCB 析构可能继续释放内核栈或进程资源，不能发生在 runqueue 锁内。
        drop(queue);
        sub_running(cpu, 1);
        drop(removed);
        return true;
    }
}

/// nice hint 已更新后，修正 owner runqueue 的选择快速路径计数。
pub(crate) fn update_nice(task: &Arc<TaskControlBlock>, old_nice: i32, new_nice: i32) {
    if (old_nice == 0) == (new_nice == 0) {
        return;
    }
    loop {
        let cpu = match task.task_status() {
            TaskStatus::Queued(cpu) => cpu,
            TaskStatus::Migrating => {
                // affinity 搬队窗口不携带 owner；等目标队列接管后再校准。
                spin_loop();
                continue;
            }
            _ => return,
        };
        let mut queue = state(cpu).run_queue.lock();
        if queue.tasks.iter().any(|queued| Arc::ptr_eq(queued, task)) {
            // hint 的写入发生在取得 runqueue 锁之前；任务也可能恰在期间入队。
            // 重新计算可同时覆盖“已在队列中改 nice”和“用新 hint 刚入队”。
            queue.recompute_nice_count();
            return;
        }

        // fetch 或 affinity 搬队可能先摘走任务。先校准读到的旧 owner，修复
        // remove_at() 按新 hint 扣减旧计数的竞态，再按最新状态重新定位。
        queue.recompute_nice_count();
        let actual = task.task_status();
        drop(queue);
        match actual {
            TaskStatus::Migrating => spin_loop(),
            TaskStatus::Queued(owner) if owner != cpu => {}
            TaskStatus::Queued(_) => panic!(
                "Queued task is absent while updating nice: tid={} cpu={}",
                task.gettid(),
                cpu
            ),
            _ => return,
        }
    }
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
        .map(nr_running)
        .sum()
}

/// 返回目标 CPU 的无锁近似排队数，供不持锁的放置策略比较负载。
pub(crate) fn nr_running(cpu: usize) -> usize {
    state(cpu).nr_running.load(Ordering::Relaxed)
}

/// OOM 路径按索引克隆一个候选，避免低内存时为队列快照再次分配。
pub(crate) fn task_at(cpu: usize, index: usize) -> Option<Arc<TaskControlBlock>> {
    state(cpu).run_queue.lock().tasks.get(index).cloned()
}
