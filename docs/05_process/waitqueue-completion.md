---
title: "WaitQueue、KernelTimerQueue 与 Completion"
category: process
status: stable
author: MangoCore Team
last_update: 2026-07-29
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
    inner: VecDeque<Arc<WaiterState>>,
}
```

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
| `add_task()` | 加入 weak task，不改变状态 |
| `pop_task()` | 弹出 weak task，不唤醒 |
| `contains()` | 按 weak 指针比较 |
| `is_empty()` | 是否为空 |
| `compact_stale()` | 删除 strong_count 为 0 的 entry |
| `prepare_to_wait()` | 创建并注册一个 `Arc<WaiterState>`，初态为 `Idle` |
| `finish_wait()` | 关闭并删除指定 task 的 waiter；兼容手工 futex 等调用者 |

`prepare_to_wait()` 不再以 `TaskStatus` 传递通知；真正从 CPU 切走仍由 `block_current_and_run_next_*()` 完成。

## 5. wake_one 与 wake_at_most

`wake_one()` 是 futex/event 等热路径：

1. 从队头移除一个 `Arc<WaiterState>`，使该队列不再持有它。
2. 对 `Idle` 或 `Sleeping` waiter 原子写入 `Notified`；`Notified`/`Closed` 条目被跳过。
3. 若 task 已处于 `Interruptible`，改为 `Ready`、递增 `wait_timer_generation`，并批量放入 ready queue。
4. 若 task 仍在运行，`Notified` 状态会阻止它随后睡眠。

`wake_at_most(limit)` 保持 FIFO 限额语义；同一 waiter 出现在多队列时，只有第一个通知能获胜。

## 6. wait_event_impl

WaitQueue 的主要等待模板：

```text
wait_event_impl(wq, cond, signal_check, deadline)
  ├── 检查 cond
  └── loop:
        ├── 在队列锁内 prepare_to_wait() 并复查 cond/deadline/signal
        ├── 按需挂 deadline timer，释放队列锁
        ├── CAS Idle -> Sleeping；若已 Notified 则跳过 block
        ├── block_current_and_run_next_with_lock_checked(..., waiter.is_sleeping)
        ├── Closed -> 从队列移除 waiter -> refresh_real_timer
        └── 最终复查 cond，再返回 Ready/TimedOut/Interrupted 或重试
```

唤醒方在移出 waiter 后写入 `Notified`。因此 wake 发生在释放队列锁、CAS 或实际 block 之间时，等待方仍能观察通知，不能被调度器对 `TaskStatus` 的写入覆盖。

超时或可处理信号路径先写 `Closed`，再从所有相关队列移除 waiter，最后复查条件；普通 deadline 继续通过 `wait_with_timeout()` 和 `wait_timer_generation` 管理。

## 7. locked wait 模板

`wait_event_locked_impl(lock, queue_of, cond, ...)` 用于等待队列嵌在某个对象锁内的场景。

带锁版本也遵循相同握手：在业务锁内注册并复查条件，释放业务锁后 CAS `Idle -> Sleeping`，再以新获取的 guard 调用 `block_current_and_run_next_with_lock_checked()`。其检查闭包额外要求 waiter 仍为 `Sleeping`。

返回时先记录 `Notified`，关闭并移除 waiter，随后在业务锁内做最终条件检查。`normal_wake_result` 仍用于 futex：条件不满足但 waiter 已通知时，返回该 wake 结果。

## 8. 多队列等待

`wait_on_queues_interruptible_timeout()` 支持一个任务同时挂多个 WaitQueue，典型用于 poll/epoll 类路径。

流程：

1. 非空时创建一个 `Arc<WaiterState>`，并把同一个 waiter 注册到所有 source queue。
2. 各 source queue 竞争将它设为 `Notified`；第一个通知获胜。
3. 条件满足、超时或信号到达时，先写 `Closed`，再从全部队列删除该 waiter。
4. 阻塞期间使用 `block_current_and_run_next_checked()`，其检查闭包要求 waiter 仍为 `Sleeping`。

该函数要求 `cond` 不依赖 `current_task()`，因为它可能在当前任务暂时离开 CPU 时被评估。

## 9. KernelTimerQueue

`KernelTimerQueue` 使用 `BinaryHeap<KernelTimer>`，通过反转比较实现最早 deadline 优先。

| 项 | 值 |
|----|----|
| 最大 timer 数 | 4096 |
| pop batch | 最多 64 个 expired timer |
| 过量处理 | compact 后仍超限则丢弃 deadline 最远的 timer |

全局状态还记录 earliest deadline 和 pending 标志，供 timer interrupt 编程使用。

## 10. TimerAction

当前 timer action：

| Action | 用途 |
|--------|------|
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

## 11. WakeTask generation

`TimerAction::WakeTask` 会比较 `task.wait_timer_generation`。不匹配表示旧 timer：

旧 generation 的 deadline timer 会直接丢弃。这避免旧 timeout 唤醒新等待；无 deadline 的等待由 Waiter/Waker one-shot 握手支持无限期阻塞，直至显式唤醒、信号或条件满足。

## 12. Completion

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

## 13. 使用者地图

| 使用者 | 等待对象 |
|--------|----------|
| futex | private futex table / shared futex table 中的 WaitQueue |
| wait4/waitid | `ProcessControlBlock.child_exit_wait` |
| eventfd/epoll/timerfd | 文件系统事件队列 |
| socket/file I/O | 各自 wait queue 或通用 wait wrapper |
| nanosleep/clock_nanosleep | kernel timer + sleep helper |
| vfork | Completion |

WaitQueue 的正确使用模式是“检查条件、入队、释放相关锁、CAS arm、切换、关闭并复查条件”。只在入队前检查一次条件会丢唤醒；跳过 `Idle -> Sleeping` 的通知检查会让调度器状态覆盖 wake；唤醒后不复查条件会把虚假唤醒当成成功。`*_locked` 变体将条件检查和注册保持在同一业务锁内。

Completion 比 WaitQueue 更窄：它只表达一次性事件已经发生，典型场景是 vfork 子进程 exec/exit 后释放父线程。Completion 不承载复杂条件，也不区分多个资源状态；如果等待条件依赖队列长度、fd readiness 或 signal mask，应使用 WaitQueue。

## 14. 调试核对点

| 现象 | 检查 |
|------|------|
| 等待永不返回 | waiter 是否注册、`Idle -> Sleeping` 是否观察到 `Notified`、条件复查 |
| 唤醒后重复入 ready queue | `try_wake_interruptible()` duplicate enqueue |
| timeout 提前或延后 | deadline 与 generation，la64 futex bias |
| timer queue 无限增长 | `MAX_TIMERS`、compact、deadline generation |
| vfork 父进程卡住 | `complete_vfork()` 是否执行 |
