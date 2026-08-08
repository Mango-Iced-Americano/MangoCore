---
title: "WaitQueue、KernelTimerQueue 与 Completion"
category: process
status: stable
author: MangoCore Team
last_update: 2026-08-03
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

WaitQueue 被 eventfd、epoll、IPC、socket/file I/O、child wait、timer 等路径复用。futex
从 B64 起使用能跟随 requeue 的专用 `FutexWaiter`，不再复用本结构。

## 2. WaitQueue 数据结构

```rust
pub struct WaitEntry {
    task: Weak<TaskControlBlock>,
    state: AtomicUsize, // Waiting -> Notified | Closed
}

pub struct WaitQueue {
    inner: VecDeque<Arc<WaitEntry>>,
}
```

`WaitEntry` 是一次性通知 token；队列强持有 entry，entry 只弱引用 TCB。
因此队列能在任务还是 `Running` 时留下早到 wake，同时不会延长任务生命周期。

WaitEntry 和 TaskStatus 有意分层：

| 设计点 | 作用 |
|--------|------|
| `WaitEntry::Waiting/Notified/Closed` | 只表示本轮通知是否已被领取 |
| `TaskStatus` | 唯一表示 CPU/current/runqueue ownership |
| entry 内的 `Weak<TaskControlBlock>` | 队列不延长任务生命周期 |
| `VecDeque<Arc<WaitEntry>>` | FIFO 唤醒，并让多队列共享同一 token |
| `compact_stale()` | 清理已经 drop 的 TCB entry |

每个 `WaiterState`（Waiter）持有 task 的 `Weak<TaskControlBlock>` 和一个原子状态字；队列持有 waiter 的 `Arc`，但不延长 task 生命周期。唤醒路径（Waker）通过该状态字发布一次性通知，失效 task 的 waiter 仍可由 `compact_stale()` 清除。

等待协议使用四态 one-shot 握手：

| 状态 | 含义 |
|------|------|
| `Idle` | 已注册到队列，尚未尝试睡眠 |
| `Sleeping` | 释放队列锁后已 arm，允许进入调度器睡眠路径 |
| `Notified` | 某一个 wake source 已获胜；后续 source 不会重复唤醒 |
| `Closed` | signal、timeout 或完成路径已取消 waiter，拒绝后续通知 |

`WaitQueue` 本身不持有外部锁；不同使用者通常把它放进自己的 `Mutex` 中。

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
| `add_task()` | 为 weak task 创建 entry，不改变调度状态 |
| `pop_task()` | 弹出 entry 并返回 weak task，不唤醒 |
| `contains()` | 按 weak 指针比较 |
| `is_empty()` | 是否为空 |
| `compact_stale()` | 删除 strong_count 为 0 的 entry |
| `prepare_to_wait()` | 注册并返回本轮 `Arc<WaitEntry>`（创建并注册一个 waiter，初态为 `Idle`） |
| `finish_entry()` | 关闭并精确移除本轮 entry，不改变调度状态 |
| `finish_wait()` | 关闭并删除指定 task 的 waiter；兼容手工 futex 等调用者 |

`prepare_to_wait()` 只登记条件等待和一次性 token；真正的
`Running(cpu) -> Blocking(cpu)` 与加入 interruptible registry 由
`block_current_and_run_next_*()` 在 `TASK_MANAGER` 临界区完成。
`prepare_to_wait()` 不再以 `TaskStatus` 传递通知；真正从 CPU 切走仍由 `block_current_and_run_next_*()` 完成。

## 5. wake_one 与 wake_at_most

`wake_one()` 是 event/IPC 等热路径：

1. 从队头开始弹出 entry，跳过失效 TCB 或已关闭 token。
2. CAS `Waiting -> Notified`；同一 entry 在多个队列中只有一个唤醒源能成功。
3. 递增 `wait_timer_generation`，使本轮旧 deadline/fallback timer 失效。
4. 调用 `TASK_MANAGER.try_wake_interruptible(task)`，由调度器在锁内 CAS
   `Blocking(cpu) -> Running(cpu)` 取消早到阻塞，或 CAS
   `Blocked -> Queued(target)` 并移动容器。
5. 返回唤醒数量 1。

`wake_at_most(limit)` 会遍历所有 entry，以便顺手 compact stale entry；唤醒超过 limit 后保留剩余可等待任务。
`Running/Queued` 不再被当成“已唤醒”的依据：Running 可能正位于注册与
`Blocking` 之间，必须先固化 token；调度器会独立抑制重复入队。
`New/Zombie` 是不可等待的终端/stale entry，直接丢弃。

从队头移除一个 `Arc<WaiterState>`，使该队列不再持有它。对 `Idle` 或 `Sleeping` waiter 原子写入 `Notified`；`Notified`/`Closed` 条目被跳过。若 task 已处于 `Interruptible`，改为 `Ready`、递增 `wait_timer_generation`，并批量放入 ready queue。若 task 仍在运行，`Notified` 状态会阻止它随后睡眠。`wake_at_most(limit)` 保持 FIFO 限额语义；同一 waiter 出现在多队列时，只有第一个通知能获胜。

## 6. wait_event_impl

WaitQueue 的主要等待模板：

```text
wait_event_impl(wq, cond, signal_check, deadline)
  ├── 检查 cond
  └── loop:
        ├── deadline 检查
        ├── current_task()
        ├── entry = wq.prepare_to_wait(task)
        ├── 再次 cond 检查
        ├── deadline 再检查
        ├── signal 检查
        ├── 挂 deadline timer 或 fallback timer
        ├── checked block 复查 entry/signal/deadline
        ├── finish_entry(entry)
        └── 清 fallback active generation
```

条件在入队前后各检查一次。登记 entry 后立即释放等待队列锁，第二次条件检查
可以同步推进生产者并通知同一个队列；该通知由 token 持久化，不依赖瞬时
`TaskStatus`。最后，checked block 在任务已发布 `Blocking(cpu)` 后再读取 token，
因此生产者不论在注册后、Blocking 前，还是真正 Blocked 后通知都不会丢失。

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
        if !entry.is_waiting() {
            wq.lock().finish_entry(&entry);
            continue;
        }
        if signal_check {
            if has_actionable_signal(&task) {
                wq.lock().finish_entry(&entry);
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

        block_current_and_run_next_checked(|task| {
            let no_signal = !signal_check || !has_actionable_signal(task);
            let not_timed_out = deadline
                .map(|deadline| TimeSpec::now() < deadline)
                .unwrap_or(true);
            let process_alive = !task.process.thread_must_exit(task.gettid());
            entry.is_waiting() && no_signal && not_timed_out && process_alive
        });

        let task = current_task().unwrap();
        wq.lock().finish_entry(&entry);
        if deadline.is_some() {
            task.wait_timer_generation.fetch_add(1, Relaxed);
        }
        task.wait_io_fallback_active_generation
            .store(0, AtomicOrdering::Release);
    }
}
```

普通 wait 与 locked wait 的锁域不同：普通 wait 的队列锁只保护 entry 容器；
`wait_event_locked_impl()` 则显式持有调用方传入的业务锁检查条件，并通过
`block_current_and_run_next_with_lock_checked()` 原子衔接业务锁释放与睡眠。

这里的 `fallback_ms` 只用于尚未完成生产者迁移的 I/O 路径；普通 deadline
直接注册 `wait_with_timeout()`。有 deadline 的一轮等待结束后会再推进
generation，避免旧 timer 在下一轮无超时等待中造成假唤醒。

wait_event_impl 的另一种等价描述（四态握手视角）：

```text
        ├── 在队列锁内 prepare_to_wait() 并复查 cond/deadline/signal
        ├── 按需挂 deadline timer，释放队列锁
        ├── CAS Idle -> Sleeping；若已 Notified 则跳过 block
        ├── block_current_and_run_next_with_lock_checked(..., waiter.is_sleeping)
        ├── Closed -> 从队列移除 waiter -> refresh_real_timer
        └── 最终复查 cond，再返回 Ready/TimedOut/Interrupted 或重试
```

唤醒方在移出 waiter 后写入 `Notified`。因此 wake 发生在释放队列锁、CAS 或实际 block 之间时，等待方仍能观察通知，不能被调度器对 `TaskStatus` 的写入覆盖。

超时或可处理信号路径先写 `Closed`，再从所有相关队列移除 waiter，最后复查条件；普通 deadline 继续通过 `wait_with_timeout()` 和 `wait_timer_generation` 管理。

## 7. fallback timer

无 deadline 的通用 I/O wait 目前仍使用过渡 fallback timer：

```rust
const WAIT_IO_FALLBACK_MS: usize = 10;
```

相关字段在 TCB 上：

| 字段 | 作用 |
|------|------|
| `wait_io_timer_pending` | 防止同一任务重复挂 fallback timer |
| `wait_timer_generation` | 新 wait 递增，旧 timer 失效 |
| `wait_io_fallback_active_generation` | 标记当前 fallback wait 的 generation |

fallback timer 触发时，如果发现 generation 过期或任务尚未真正登记
`Blocking/Blocked`，会重新挂 timer 而不是错误唤醒或丢弃。它不再用于弥补
WaitQueue 核心的注册竞争；只有在 FS/Net 生产者漏通知修复全部融合并验证后，
才能删除这个过渡层。

## 8. locked wait 模板

`wait_event_locked_impl(lock, queue_of, cond, ...)` 用于等待队列嵌在某个对象锁内的场景。

带锁版本也遵循相同握手：在业务锁内注册并复查条件，释放业务锁后 CAS `Idle -> Sleeping`，再以新获取的 guard 调用 `block_current_and_run_next_with_lock_checked()`。其检查闭包额外要求 waiter 仍为 `Sleeping`。

1. 先持业务锁检查条件。
2. 入队后再次检查条件。
3. 需要睡眠时调用 `block_current_and_run_next_with_lock_checked(guard, ...)`。
4. 该函数保证任务进入 interruptible queue 后再释放业务锁。
5. 醒来后重新持业务锁，精确 `finish_entry()` 本轮 token。

这防止“释放业务锁”和“进入睡眠队列”之间丢失唤醒。

带锁版本在持业务锁时把 task 加入业务对象内部的 WaitQueue：

```rust
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
        if signal_check {
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
    }
}
```

通用模板只在业务条件返回值时产生 `Ready`，不会再把“已经不在原队列”解释成正常 wake。
这种成员关系推断对可迁移 waiter 不成立：futex requeue 也会移除 source 成员，却不应返回
成功。因此 futex 使用独立的 current-key 与 `woken` 状态机。

## 9. 多队列等待

`wait_on_queues_interruptible_timeout()` 支持一个任务同时挂多个 WaitQueue，典型用于 poll/epoll 类路径。

流程：

1. 若 queues 为空，只等待 signal 或真实 deadline，不再构造 10 ms 周期唤醒。
2. 非空时，创建一个共享 entry/waiter，并把它加入所有队列。
3. 条件满足、超时或信号到达时，先写 `Closed`/close token，再从所有队列精确移除。
4. 阻塞期间使用 `block_current_and_run_next_checked()`。

从开发分支的四态握手视角：

1. 非空时创建一个 `Arc<WaiterState>`，并把同一个 waiter 注册到所有 source queue。
2. 各 source queue 竞争将它设为 `Notified`；第一个通知获胜。
3. 条件满足、超时或信号到达时，先写 `Closed`，再从全部队列删除该 waiter。
4. 阻塞期间使用 `block_current_and_run_next_checked()`，其检查闭包要求 waiter 仍为 `Sleeping`。

返回时先记录 `Notified`，关闭并移除 waiter，随后在业务锁内做最终条件检查。`normal_wake_result` 仍用于 futex：条件不满足但 waiter 已通知时，返回该 wake 结果。

该函数要求 `cond` 不依赖 `current_task()`，因为它可能在当前任务暂时离开
CPU 时被评估。共享 token 使多个源并发 wake 时仍只有一个通知能被领取。

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
| `IntervalTimerSignal` | `ITIMER_REAL` 向所属进程投递 `SIGALRM` |

| `WakeTask` | deadline wait 唤醒任务 |
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
    },
    IntervalTimerSignal {
        process: Weak<ProcessControlBlock>,
        generation: u64,
    },
    PosixTimerSignal {
        process: Weak<ProcessControlBlock>,
        timer_id: usize,
        arm_seq: u64,
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

`PosixTimerSignal` 不缓存 signal；回调必须在 PCB timer 表锁内重新读取当前对象并同时匹配
`timer_id + arm_seq + deadline`。这样 rearm、delete/recreate、exec 或退出产生的 stale
heap 节点不能投递旧配置的信号。周期重装和调度器唤醒都在释放 timer 表锁后完成。

`KernelTimer` 的 `Ord` 以 deadline 反序比较，使 `BinaryHeap` 顶部成为最早 deadline。`MAX_TIMERS` 防止 timer storm 耗尽内存。

## 12. WakeTask generation

`TimerAction::WakeTask` 会比较 `task.wait_timer_generation`。不匹配表示旧 timer：

| timer 类型 | 旧 timer 行为 |
|------------|---------------|
| deadline timer | 直接丢弃 |
| fallback timer | 如果任务仍在新一轮 fallback wait，可重新挂当前 generation timer |

这避免旧 timeout 唤醒新等待，也避免 fallback timer 在任务还没完成
`Running -> Blocking/Blocked` 登记时被提前消费。

旧 generation 的 deadline timer 会直接丢弃。这避免旧 timeout 唤醒新等待；无 deadline 的等待由 Waiter/Waker one-shot 握手支持无限期阻塞，直至显式唤醒、信号或条件满足。

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

    pub fn wait_killable(&self) -> super::WaitResult {
        WaitQueue::wait_event_locked(
            &self.inner,
            |inner| &mut inner.wait_queue,
            |inner| inner.done.then_some(0),
        )
    }
}
```

| 方法 | 语义 |
|------|------|
| `new()` | 初始未完成 |
| `complete()` | 标记完成并唤醒等待者 |
| `wait_killable()` | 忽略普通信号；线程组退出/exec 停止请求可中断 |

vfork 子进程 exec 成功或 exit 时调用 `ProcessControlBlock::complete_vfork()`，父线程等待
`vfork_done`。B41 的 exec owner 也用独立 Completion 等待 sibling 完成用户映射和 TLB
清理。生命周期中断返回 `Interrupted` 前，WaitQueue 必须先摘除 waiter；调用层随后释放
syscall 栈上的 `Arc` 并进入任务安全点。

## 14. 使用者地图

| 使用者 | 等待对象 |
|--------|----------|
| futex | 专用 `FutexTable -> FutexQueue -> Arc<FutexWaiter>` |
| wait4/waitid | `ProcessControlBlock.child_exit_wait` |
| eventfd/epoll/timerfd | 文件系统事件队列 |
| socket/file I/O | 各自 wait queue 或通用 wait wrapper |
| nanosleep/clock_nanosleep | kernel timer + sleep helper |
| vfork / 多线程 exec | Completion |

WaitQueue 的正确使用模式是“检查条件、入队、释放相关锁、切换、被唤醒后复查条件”。只在入队前检查一次条件会丢唤醒；持业务锁睡眠会阻塞唤醒方；唤醒后不复查条件会把虚假唤醒当成成功。`*_locked` 变体就是为了解决条件检查和业务锁释放之间的竞态。需要
requeue 或一次任务多项注册时，应使用带独立身份和当前位置的专用等待对象。

WaitQueue 的正确使用模式是“检查条件、入队、释放相关锁、CAS arm、切换、关闭并复查条件”。只在入队前检查一次条件会丢唤醒；跳过 `Idle -> Sleeping` 的通知检查会让调度器状态覆盖 wake；唤醒后不复查条件会把虚假唤醒当成成功。`*_locked` 变体将条件检查和注册保持在同一业务锁内。

Completion 比 WaitQueue 更窄：它只表达一次性事件已经发生，典型场景是 vfork 子进程
exec/exit 后释放父线程，或多线程 exec 的 sibling 全部离开 CPU current 槽。
live count 另行证明资源清理完成，不能代替这个 inactive 条件。Completion 不承载
复杂条件，也不区分多个资源状态；如果等待条件依赖队列长度、fd readiness 或 signal mask，
应使用 WaitQueue。

## 15. 调试核对点

| 现象 | 检查 |
|------|------|
| 等待永不返回 | waiter 是否注册、`Idle -> Sleeping` 是否观察到 `Notified`、条件复查 |
| 唤醒后重复入 ready queue | `try_wake_interruptible()` duplicate enqueue |
| timeout 提前或延后 | deadline 与 generation，la64 futex bias |
| timer queue 无限增长 | `MAX_TIMERS`、compact、deadline generation |
| vfork 父进程卡住 | `complete_vfork()` 是否执行 |
