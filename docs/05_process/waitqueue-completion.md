---
title: "WaitQueue、KernelTimerQueue 与 Completion"
category: process
status: stable
author: MangoCore Team
last_update: 2026-07-27
tags: [process, waitqueue, completion, timer]
---

# WaitQueue、KernelTimerQueue 与 Completion

## 1. 源码位置

等待原语主要位于：

| 文件 | 内容 |
|------|------|
| `os/src/task/manager.rs` | `WaitQueue`, `WaitResult`, `KernelTimerQueue`, `TimerAction` |
| `os/src/task/completion.rs` | `Completion` |
| `os/src/task/mod.rs` | block/schedule 基础入口 |
| `os/src/task/sleep.rs` | sleep syscall 使用的等待封装 |

WaitQueue 被 futex、eventfd、epoll、socket/file I/O、child wait、timer 等路径复用。

## 2. WaitQueue 数据结构

```rust
pub struct WaitQueue {
    inner: VecDeque<Weak<TaskControlBlock>>,
}
```

完整结构和基础操作位于 `task/manager.rs`：

```rust
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

pub struct WaitQueue {
    inner: VecDeque<Weak<TaskControlBlock>>,
}

impl WaitQueue {
    pub fn new() -> Self {
        Self {
            inner: VecDeque::new(),
        }
    }

    pub fn add_task(&mut self, task: Weak<TaskControlBlock>) {
        self.inner.push_back(task);
    }

    pub fn pop_task(&mut self) -> Option<Weak<TaskControlBlock>> {
        self.inner.pop_front()
    }

    pub fn compact_stale(&mut self) -> usize {
        let before = self.inner.len();
        self.inner.retain(|task| task.strong_count() > 0);
        before - self.inner.len()
    }
}
```

WaitQueue 保存 `Weak<TaskControlBlock>`，因此等待队列本身不会阻止 task 生命周期结束。唤醒时需要 `upgrade()`，失效项可由 `compact_stale()` 清除。

它保存弱引用，而不是强引用：

| 设计点 | 作用 |
|--------|------|
| `Weak<TaskControlBlock>` | 等待队列不延长任务生命周期 |
| `VecDeque` | FIFO 风格唤醒 |
| `compact_stale()` | 清理已经 drop 的任务 weak entry |

等待队列本身不持有外部锁；不同使用者通常把它放进自己的 `Mutex` 中。

## 3. WaitResult

```rust
pub enum WaitResult {
    Ready(isize),
    Interrupted,
    TimedOut,
}
```

| 结果 | 语义 |
|------|------|
| `Ready(value)` | 条件满足，返回调用者指定值 |
| `Interrupted` | 有可处理信号 |
| `TimedOut` | deadline 到期 |

`unwrap_or_else()` 默认把 `Interrupted` 转成 `-ERESTART`，把 `TimedOut` 转成 `-EAGAIN`。futex 另有专用转换，将 interrupted 转 `EINTR`、timeout 转 `ETIMEDOUT`。

## 4. 基础队列操作

| 方法 | 行为 |
|------|------|
| `add_task()` | 加入 weak task，不改变状态 |
| `pop_task()` | 弹出 weak task，不唤醒 |
| `contains()` | 按 weak 指针比较 |
| `is_empty()` | 是否为空 |
| `compact_stale()` | 删除 strong_count 为 0 的 entry |
| `prepare_to_wait()` | 只加入 weak task，不改变调度状态 |
| `finish_wait()` | 只从 WaitQueue 移除当前任务，不改变调度状态 |

`prepare_to_wait()` 只把任务放入 WaitQueue；真正的
`Running(cpu) -> Blocking(cpu)` 与加入 interruptible registry 由
`block_current_and_run_next_*()` 在 `TASK_MANAGER` 临界区完成。

## 5. wake_one 与 wake_at_most

`wake_one()` 是 futex/event 等热路径：

1. 从队头开始弹出 weak entry。
2. 跳过失效 weak。
3. 若任务状态为 `Blocking/Blocked`，递增 `wait_timer_generation` 并选为唤醒候选。
4. 调用 `TASK_MANAGER.try_wake_interruptible(task)`，由调度器在锁内 CAS
   `Blocking(cpu) -> Running(cpu)` 取消早到阻塞，或 CAS
   `Blocked -> Queued(CPU0)` 并移动容器。
5. 返回唤醒数量 1。

`wake_at_most(limit)` 会遍历所有 entry，以便顺手 compact stale entry；唤醒超过 limit 后保留剩余可等待任务。
已经处于 `Queued/Running` 的旧 WaitQueue 条目只按“事件已经到达”计数并丢弃，
不会重复入队。`New/Zombie` 条目同样丢弃。

## 6. wait_event_impl

WaitQueue 的主要等待模板：

```text
wait_event_impl(wq, cond, signal_check, deadline, fallback_ms)
  ├── 先执行 cond，若 Ready 直接返回
  └── loop:
        ├── deadline 检查
        ├── current_task()
        ├── wq.prepare_to_wait(task)
        ├── 再次 cond 检查
        ├── deadline 再检查
        ├── signal 检查
        ├── 挂 deadline timer 或 fallback timer
        ├── block_current_and_run_next_with_lock_checked()
        ├── finish_wait()
        ├── 清 fallback active generation
        └── refresh_real_timer()
```

条件在入队前后各检查一次，避免条件刚满足却已经睡眠导致丢失唤醒。

源码主干如下：

```rust
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
```

这里的 `fallback_ms` 只用于 I/O fallback timer；普通 deadline 直接注册 `wait_with_timeout()`。

## 7. fallback timer

无 deadline 的 I/O wait 使用 fallback timer：

```rust
const WAIT_IO_FALLBACK_MS: usize = 10;
```

相关字段在 TCB 上：

| 字段 | 作用 |
|------|------|
| `wait_io_timer_pending` | 防止同一任务重复挂 fallback timer |
| `wait_timer_generation` | 新 wait 递增，旧 timer 失效 |
| `wait_io_fallback_active_generation` | 标记当前 fallback wait 的 generation |

fallback timer 触发时，如果发现 generation 过期或任务尚未真正进入 Interruptible，会重新挂 timer 而不是错误唤醒或丢弃。

## 8. locked wait 模板

`wait_event_locked_impl(lock, queue_of, cond, ...)` 用于等待队列嵌在某个对象锁内的场景。

流程特点：

1. 先持业务锁检查条件。
2. 入队后再次检查条件。
3. 需要睡眠时调用 `block_current_and_run_next_with_lock_checked(guard, ...)`。
4. 该函数保证任务进入 interruptible queue 后再释放业务锁。
5. 醒来后重新持业务锁 `finish_wait()`。

这防止“释放业务锁”和“进入睡眠队列”之间丢失唤醒。

带锁版本在持业务锁时把 task 加入业务对象内部的 WaitQueue：

```rust
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
```

`normal_wake_result` 用于 futex：被 wake 方从队列中移除后，等待模板用这个值区分“条件满足返回”和“被 wake 返回”。

## 9. 多队列等待

`wait_on_queues_interruptible_timeout()` 支持一个任务同时挂多个 WaitQueue，典型用于 poll/epoll 类路径。

流程：

1. 若 queues 为空，构造临时 WaitQueue 并用 fallback 轮询。
2. 非空时，将当前任务加入所有队列。
3. 条件满足、超时或信号到达时，从所有队列 `finish_wait()`。
4. 阻塞期间使用 `block_current_and_run_next_checked()`。

该函数要求 `cond` 不依赖 `current_task()`，因为它可能在当前任务暂时离开 CPU 时被评估。

## 10. KernelTimerQueue

`KernelTimerQueue` 使用 `BinaryHeap<KernelTimer>`，通过反转比较实现最早 deadline 优先。

| 项 | 值 |
|----|----|
| 最大 timer 数 | 4096 |
| pop batch | 最多 64 个 expired timer |
| 过量处理 | compact 后仍超限则丢弃 deadline 最远的 timer |

全局状态还记录 earliest deadline 和 pending 标志，供 timer interrupt 编程使用。

## 11. TimerAction

当前 timer action：

| Action | 用途 |
|--------|------|
| `WakeTask` | deadline/fallback wait 唤醒任务 |
| `SendSignal` | `ITIMER_REAL` 等向任务投递信号 |
| `PosixTimerSignal` | POSIX timer 到期投递信号 |
| `TimerFdSweep` | 驱动 timerfd registry 唤醒 |

timer callback 必须在不持有 `KERNEL_TIMER_QUEUE` 锁时运行。`pop_expired()` 只取出节点，真正 `run_timer()` 在锁外执行。

定时器 action 与队列结构如下：

```rust
pub enum TimerAction {
    WakeTask {
        task: Weak<TaskControlBlock>,
        generation: usize,
        fallback_ms: Option<usize>,
    },
    SendSignal {
        task: Weak<TaskControlBlock>,
        signal: Signals,
        generation: usize,
    },
    PosixTimerSignal {
        task: Weak<TaskControlBlock>,
        timer_id: usize,
        signal: Signals,
        generation: usize,
    },
    TimerFdSweep {
        generation: usize,
    },
}

pub struct KernelTimer {
    action: TimerAction,
    deadline: TimeSpec,
}

pub struct KernelTimerQueue {
    inner: BinaryHeap<KernelTimer>,
}

impl KernelTimerQueue {
    const MAX_TIMERS: usize = 4096;

    pub fn add_action(&mut self, action: TimerAction, deadline: TimeSpec) -> bool {
        let old_earliest = self.earliest_deadline_ns();
        self.inner.push(KernelTimer { action, deadline });
        if self.inner.len() > Self::MAX_TIMERS {
            self.enforce_capacity();
        }
        let new_earliest = self.earliest_deadline_ns();
        self.refresh_deadline_state();
        old_earliest == 0 || new_earliest < old_earliest
    }
}
```

`KernelTimer` 的 `Ord` 以 deadline 反序比较，使 `BinaryHeap` 顶部成为最早 deadline。`MAX_TIMERS` 防止 timer storm 耗尽内存。

## 12. WakeTask generation

`TimerAction::WakeTask` 会比较 `task.wait_timer_generation`。不匹配表示旧 timer：

| timer 类型 | 旧 timer 行为 |
|------------|---------------|
| deadline timer | 直接丢弃 |
| fallback timer | 如果任务仍在新一轮 fallback wait，可重新挂当前 generation timer |

这避免旧 timeout 唤醒新等待，也避免 fallback timer 在任务还没进入 Interruptible 时被消费。

## 13. Completion

`Completion` 位于 `os/src/task/completion.rs`，用于一次性完成通知。典型使用是 `CLONE_VFORK`：

```rust
struct CompletionInner {
    done: bool,
    wait_queue: WaitQueue,
}

pub struct Completion {
    inner: Mutex<CompletionInner>,
}

impl Completion {
    pub fn complete(&self) -> bool {
        let mut inner = self.inner.lock();
        if inner.done {
            return false;
        }
        inner.done = true;
        inner.wait_queue.wake_all();
        true
    }

    pub fn wait_interruptible(&self) -> super::WaitResult {
        WaitQueue::wait_event_interruptible_locked(
            &self.inner,
            |inner| &mut inner.wait_queue,
            |inner| inner.done.then_some(0),
        )
    }

    pub fn wait_uninterruptible(&self) {
        let _ = WaitQueue::wait_event_locked(
            &self.inner,
            |inner| &mut inner.wait_queue,
            |inner| inner.done.then_some(0),
        );
    }
}
```

| 方法 | 语义 |
|------|------|
| `new()` | 初始未完成 |
| `complete()` | 标记完成并唤醒等待者 |
| `wait_uninterruptible()` | 不可中断等待完成 |

vfork 子进程 exec 成功或 exit 时调用 `ProcessControlBlock::complete_vfork()`，父线程等待 `vfork_done`。

## 14. 使用者地图

| 使用者 | 等待对象 |
|--------|----------|
| futex | private futex table / shared futex table 中的 WaitQueue |
| wait4/waitid | `ProcessControlBlock.child_exit_wait` |
| eventfd/epoll/timerfd | 文件系统事件队列 |
| socket/file I/O | 各自 wait queue 或通用 wait wrapper |
| nanosleep/clock_nanosleep | kernel timer + sleep helper |
| vfork | Completion |

WaitQueue 的正确使用模式是“检查条件、入队、释放相关锁、切换、被唤醒后复查条件”。只在入队前检查一次条件会丢唤醒；持业务锁睡眠会阻塞唤醒方；唤醒后不复查条件会把虚假唤醒当成成功。`*_locked` 变体就是为了解决条件检查和业务锁释放之间的竞态。

Completion 比 WaitQueue 更窄：它只表达一次性事件已经发生，典型场景是 vfork 子进程 exec/exit 后释放父线程。Completion 不承载复杂条件，也不区分多个资源状态；如果等待条件依赖队列长度、fd readiness 或 signal mask，应使用 WaitQueue。

## 15. 调试核对点

| 现象 | 检查 |
|------|------|
| 等待永不返回 | 入队后条件复查、fallback timer generation |
| 唤醒后重复入 ready queue | `try_wake_interruptible()` duplicate enqueue |
| timeout 提前或延后 | deadline 与 generation，la64 futex bias |
| timer queue 无限增长 | `MAX_TIMERS`、compact、fallback pending 位 |
| vfork 父进程卡住 | `complete_vfork()` 是否执行 |
