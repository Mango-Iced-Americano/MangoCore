# tgkill

`tgkill` 是一个精确控制线程级别信号发送的系统调用。相比于 `kill` 和 `tkill`，`tgkill` 明确区分线程组 ID (`tgid`) 与线程 ID (`tid`)，从而能准确地向指定进程/线程发信号，用于线程间通信、控制和同步。

## NPUcore 的 tgkill 实现

我们实现的 `tgkill` 系统调用参考了 Linux 的设计，允许用户向同一线程组中的特定线程发送信号。

## 接口定义

```rust
pub fn sys_tgkill(tgid: usize, tid: usize, sig: usize) -> isize
```

- `tgid`: 线程组 ID，等同于目标进程的进程号（pid）。
- `tid`: 线程 ID，必须是属于该 `tgid` 的线程。
- `sig`: 发送的信号编号。

返回值：

- 成功返回 `SUCCESS`；
- 信号非法返回 `EINVAL`；
- 找不到目标线程或线程不属于该进程组，返回 `ESRCH`。

## 行为描述

1. 首先验证信号是否合法；
2. 在系统任务列表中查找指定 `tgid` 所代表的任务；
3. 如果任务存在且 `tid` 匹配该任务自身的 `pid`，则将信号添加至其信号队列；
4. 如果目标任务当前处于 `Interruptible` 状态（可中断的睡眠状态），则主动将其唤醒；
5. 若 `tid` 与任务不匹配，则记录警告，但不返回错误；
6. 若找不到任务，返回 `ESRCH` 错误。

## 实现细节

我们为每个 `Task` 维护一个 `inner` 内部状态结构，其中包括信号队列、当前任务状态等：

```rust
struct TaskControlBlockInner {
    pub sigmask: Signals,      // 当前阻塞的信号集合
    pub sigpending: Signals,   // 当前等待处理的信号集合
    pub task_status: TaskStatus,   // 任务状态
    // ...
}
```

信号通过 `add_signal` 添加到队列：

```rust
inner.add_signal(signal);
```

在任务阻塞状态下，我们仅支持 `Interruptible` 状态的任务被信号唤醒。当检测到目标任务处于此状态时，会切换其状态为 `Ready` 并唤醒：

```rust
if inner.task_status == TaskStatus::Interruptible {
    inner.task_status = TaskStatus::Ready;
    drop(inner);
    wake_interruptible(task);
}
```

## 数据结构与调度器配合

信号的添加和唤醒必须保证原子性，故 `inner` 结构通过锁保护。调度器支持查询和唤醒指定 `Task`，从而与 `nanosleep`, `futex`等系统调用形成一致的阻塞/唤醒机制。

## 典型使用场景

- 用户空间线程库向某个工作线程发送中断信号；
- 实现 `pthread_kill`；
- 与 `set_tid_address` 等机制联用，用于线程退出通知；
- 与 `waitpid` 联动处理子线程状态。

## 注意事项

- `tgid` 和 `tid` 必须一致才会生效，否则即使任务存在也不会发信号；
- 当前实现中 `tgkill` 仅支持向目标任务发送信号，不支持广播或信号处理回调；
- 若任务处于不可中断状态或正在运行，信号将暂存在信号队列中，等待调度器调度时处理；
- `signal.is_empty()` 判断信号是否为 0，若是则不触发任何操作。