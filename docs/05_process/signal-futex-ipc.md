---
title: "信号、futex 与 IPC 的阻塞/唤醒协作"
category: process
status: stable
author: MangoCore Team
last_update: 2026-08-02
tags: [process, signal, futex, ipc, waitqueue]
---

# 信号、futex 与 IPC 的阻塞/唤醒协作

## 1. 源码位置

| 源码 | 作用 |
|------|------|
| `os/src/task/signal/` | signal action、pending、delivery、frame、wait |
| `os/src/syscall/process/signal.rs` | kill/tkill/tgkill、sigaction、pidfd、signalfd |
| `os/src/syscall/process/futex.rs` | futex/futex_waitv syscall 层 |
| `os/src/task/threads.rs` | 线程组 futex table、clear_child_tid 相关状态 |
| `os/src/syscall/process/ipc.rs` | SysV msg/sem/shm 与 POSIX MQ |
| `os/src/task/ipc_namespace.rs` | IPC namespace 状态对象 |
| `os/src/task/manager.rs` | WaitQueue、interruptible 队列和唤醒 |

## 2. 协作模型

信号、futex 和 IPC 看似是不同子系统，但最终都要把业务等待转换成任务阻塞与唤醒。
它们共享调度器入口，不再共享同一种队列成员模型：

```
业务对象状态
  ├── futex word + FutexTable/FutexQueue/FutexWaiter
  ├── IPC queue + WaitQueue
  └── signal pending
        ↓
任务阻塞/唤醒
  ├── block_current_and_run_next_with_lock_checked()
  └── wake_interruptible()

通用 WaitQueue
        ├── prepare_to_wait()
        ├── wake_at_most()/wake_all()
        └── finish_wait()
```

信号决定可中断等待是否提前返回；futex 和 IPC 提供业务条件；TaskManager 执行真正调度。
futex 需要跟踪 requeue 后的 current key 和 waitv 单项身份，因此使用专用 waiter；IPC 等
普通条件队列继续使用 `WaitQueue`。

统一等待结果由 `WaitResult` 表达：

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
```

futex、IPC 和 signalfd 不直接复用默认转换，而是在各自 syscall 层按 Linux 语义把 `Interrupted/TimedOut` 转成 `EINTR`、`ETIMEDOUT`、`EAGAIN` 或其他业务错误。

## 3. 统一等待结果

普通 WaitQueue 返回 `WaitResult`：

| WaitResult | 通用转换 |
|------------|----------|
| `Ready(value)` | value |
| `Interrupted` | `-ERESTART` |
| `TimedOut` | `-EAGAIN` |

futex 改写为 Linux futex 语义：

| WaitResult | futex 转换 |
|------------|------------|
| `Interrupted` | `EINTR` |
| `TimedOut` | `ETIMEDOUT` |

IPC 路径则根据 syscall 语义转换为 `EINTR`、`EAGAIN` 或业务返回值。

## 4. 信号如何打断等待

可中断等待在睡前检查：

```text
if has_actionable_signal(task):
    finish_wait()
    return Interrupted
discard_non_actionable_unblocked_signals(task)
```

睡眠期间 signal 投递会：

1. 写入 TCB 或 PCB pending queue。
2. 如果 signal 可唤醒当前 mask 下的 interruptible task，设置状态 `Ready`。
3. 调用 `wake_interruptible(task)`。

不可中断等待不检查 signal，只有条件满足或内部 wake 才返回。

等待模板中的信号检查位于入队并复查条件之后：

```rust
if signal_check {
    if has_actionable_signal(&task) {
        guard.finish_wait(task.as_ref());
        return WaitResult::Interrupted;
    }
    discard_non_actionable_unblocked_signals(&task);
}
```

这说明可中断等待不是“任意 pending signal 都返回”，而是 `has_actionable_signal()` 判断当前 mask 下可处理的信号。不可处理的未屏蔽信号会通过 `discard_non_actionable_unblocked_signals()` 清理，避免无效唤醒不断打断等待。

## 5. futex 与 signal

futex wait 使用可中断等待：

| 事件 | 返回 |
|------|------|
| futex word 不等于 val | `EAGAIN` |
| wake 移除 waiter | 0 或 waitv index |
| signal 到达 | `EINTR` |
| timeout | `ETIMEDOUT` |

`discard_non_actionable_unblocked_signals()` 用于丢弃不会打断当前等待的信号，避免 pending 队列中无效信号造成重复唤醒。

每次 futex 注册都由独立 `Arc<FutexWaiter>` 表示。wake 在 table 锁下先发布
`waiter.woken`，再让任务 runnable；signal 和 timeout 则按 waiter 的 current key 与
准确 Arc 身份撤销：

```text
wake:    woken = true -> wake_interruptible(task)
requeue: waiter.key = target -> publish into target queue
signal:  remove(current key, exact waiter) -> EINTR
timeout: remove(current key, exact waiter) -> ETIMEDOUT
```

因此 requeue 导致 source membership 消失时不会被误判为正常 wake；只有 `woken == true`
才能产生 futex 成功返回。注册完成后也不再重读最初 futex word。

## 6. IPC 与 WaitQueue

SysV message queue、semaphore 和 POSIX MQ 在资源不可用时使用 WaitQueue：

| IPC | 阻塞条件 |
|-----|----------|
| msgsnd | 队列字节数或消息数超限 |
| msgrcv | 没有匹配消息 |
| semop | semaphore 值不满足操作 |
| mq_timedsend | 队列满 |
| mq_timedreceive | 队列空 |

`IPC_NOWAIT` 或 `O_NONBLOCK` 时不进入等待，直接返回 `EAGAIN`。

SysV message queue 的阻塞发送直接使用带锁 WaitQueue 模板：

```rust
match WaitQueue::wait_event_interruptible_locked(
    &MSG_REGISTRY,
    |registry| &mut registry.wait_queue,
    |registry| try_msgsnd_locked(registry, msqid, mtype, &data),
) {
    WaitResult::Ready(value) => value,
    WaitResult::Interrupted => EINTR,
    WaitResult::TimedOut => EINTR,
}
```

semaphore 的 timed wait 在同一模板上加 deadline：

```rust
WaitQueue::wait_event_interruptible_timeout_locked(
    &SEM_REGISTRY,
    |registry| &mut registry.wait_queue,
    |registry| sem_wait_condition(registry, semid, &ops, &mut registered),
    deadline,
)
```

这两个例子说明 IPC 的业务条件由 registry 锁内闭包提供，调度和 signal/timeout 处理由 WaitQueue 模板提供。

## 7. timeout 来源

| 子系统 | timeout 解释 |
|--------|--------------|
| futex wait | 相对 timeout |
| futex wait bitset | 绝对 timeout，可选 realtime |
| futex waitv | clockid 指定 realtime/monotonic |
| semtimedop | timeout |
| mq_timedsend/receive | absolute timeout |
| sigtimedwait | timeout |
| nanosleep | relative sleep |

内部统一转成 `TimeSpec` deadline，并挂入 KernelTimerQueue 或通过等待模板检查。

## 8. shared futex 与 mmap

futex shared key 依赖 MM：

```text
non-private futex
  └── AddressSpace::futex_shared_backing(addr)
        ├── VMA 是 MAP_SHARED -> backing Arc identity + page offset
        └── 否则 -> private key
```

因此同一虚拟地址不一定是 shared futex；只有实际 shared VMA 才进入全局表。每个非空
shared queue 持有一份 backing `Arc`，避免旧队列因物理页号复用错误命中新页；VMA/PTE
校验在 VM 锁内完成，实际 table 操作在 VM 锁释放后进行。

## 9. clear_child_tid 与 futex

线程退出桥接 process 和 futex：

1. TCB 退出写 0 到用户 `clear_child_tid`。
2. 唤醒进程 private futex table。
3. 如果地址属于 shared VMA，唤醒全局 shared futex table。
4. 若写 0 fault 前后 backing 身份变化，分别尝试旧、新 shared key。

这条路径不经过 syscall futex wait 的参数解析，但使用相同 `FutexTable::wake()` 机制。

## 10. signalfd 与 pending 队列

signalfd 把信号读取转换成 fd read：

| 操作 | 行为 |
|------|------|
| read | 从线程 pending 或进程 shared pending 取 matching signal |
| poll | 有 matching pending 时返回 readable |
| mask 更新 | `signalfd4(fd>=0)` |

signalfd read 没有 pending 时返回 `EAGAIN`；是否阻塞由 fd 层通用 I/O 等待处理。

signalfd 的 read/poll 分工如下：

```rust
let Some(pending) = take_pending_signal_matching(task, mask) else {
    break;
};
```

```rust
if has_pending_signal_matching(task, self.pending_mask()) {
    Ok((EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM).bits())
} else {
    Ok(0)
}
```

read 消费 pending；poll 只观察是否有 matching pending。

## 11. pidfd 与 wait

pidfd 同时参与 signal 和 wait：

| 功能 | 路径 |
|------|------|
| `pidfd_send_signal` | pidfd -> target pid -> signal permission -> send |
| `waitid(P_PIDFD)` | pidfd -> target pid -> wait_child |
| nonblock pidfd wait | target 非 zombie 返回 `EAGAIN` |

pidfd 解析支持 `PidFd` inode 和 `/proc/[pid]` 目录 inode。

## 12. IPC namespace 与 signal

IPC namespace 决定 SysV/POSIX IPC registry；signal 权限仍按进程身份和进程树判断，不因 IPC namespace 隔离而改变。

POSIX MQ notify 会向注册进程投递信号，使用 signal 模块的 `send_process_signal_info()`。

## 13. 锁顺序约束

这些路径最容易出错的地方是持锁阻塞。实现遵循：

1. 持业务锁检查条件。
2. 把当前任务加入 WaitQueue。
3. 再次检查条件。
4. 把任务加入 interruptible queue。
5. 释放业务锁。
6. `schedule()`。

不能在持业务锁时直接长时间循环或直接睡眠而不释放锁。

这三类机制都把“事件”和“等待”拆开：signal 是 pending 队列加 trap return delivery，
futex 是用户 word 校验加专用 waiter，IPC 是 registry 状态加通用 WaitQueue。共同的调试
方法是先确认事件是否进入内核状态，再确认等待者是否在正确容器上，最后确认唤醒结果是否
由权威业务状态裁决。

它们也共享同一个锁约束：业务状态可以在锁内检查和修改，但真正可能睡眠、复制大块用户数据或触发文件/网络操作的路径必须释放锁后执行。否则 signal 打断、futex wake 或 IPC 删除都可能被持锁睡眠阻塞。

## 14. 调试核对点

| 现象 | 检查 |
|------|------|
| signal 到达但 futex 不返回 | sigmask、has_actionable_signal、wait 是否可中断 |
| IPC_NOWAIT 仍阻塞 | flags 是否正确传到业务分支 |
| shared futex 跨进程不醒 | VMA 是否 MAP_SHARED，phys key 是否一致 |
| signalfd poll 不可读 | mask 与 pending 是否相交 |
| pidfd waitid 不符合 nonblock | pidfd file flags 与 target zombie 状态 |
