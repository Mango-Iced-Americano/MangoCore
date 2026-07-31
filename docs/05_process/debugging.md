---
title: "进程与任务调试与测试映射"
category: process
status: stable
author: MangoCore Team
last_update: 2026-08-01
tags: [process, debug, scheduler, signal, futex, test]
---

# 进程与任务调试与测试映射

## 1. 状态地图

进程问题通常需要同时看 TCB、PCB 和调度队列：

| 层 | 结构 | 关键状态 |
|----|------|----------|
| 线程 | `TaskControlBlock` | tid、trap context、task status、sigmask/pending、clear_child_tid |
| 进程 | `ProcessControlBlock` | pid、threads/live count、vm/files/fs/sighand/futex、children、exit_code |
| 调度 | `RunQueue`, `TaskManager`, `Processor` | Per-CPU runnable/current/zombie、全局 interruptible/timer registry |
| 等待 | `WaitQueue`, `Completion` | waiters、timeout generation、wake path |

`TaskStatus::Zombie` 表示线程退出；`ProcessState::Zombie` 表示最后一个 live thread 退出并完成进程级收尾。两者不能互相替代。

## 2. clone/exec/exit/wait 闭环

```
clone
  -> child TCB/PCB/VM 构造
  -> publish to parent children
  -> schedule child
exec
  -> 构造新 AddressSpace
  -> 替换 VM/trap context/CLOEXEC/sighand/futex
exit
  -> exit_thread_resources()
  -> finish_exit()
wait
  -> child_exit_wait / consume zombie
```

| 症状 | 检查 |
|------|------|
| clone 成功但 wait 不到 | `publish_clone_child()` 是否执行，parent children 是否有 child |
| child 运行参数错 | child trap context 返回值、TLS、child stack |
| exec 失败后进程损坏 | 是否在可失败构造完成前替换旧 VM |
| 多线程 exec 后 sibling 仍运行 | ExecSession 门禁、owner 安全点退出与 live count ack |
| exit 后 zombie 堆积 | wait/auto-reap、pid release、zombie queue drain |
| wait 阻塞不醒 | `child_exit_wait.wake_all()` 和 wait 条件复查 |

## 3. 调度与等待

| 症状 | 首查位置 |
|------|----------|
| runnable 任务不运行 | `run_queue::fetch()`、owner CPU、nice/vruntime hint |
| 睡眠任务不醒 | WaitQueue 入队、wake path、timer generation |
| 当前任务缓存错误 | `run_tasks()` 切入发布、`finish_current_switch_out()` 切栈后清理 |
| TCB drop 崩溃 | 当前任务是否先切回 idle，再 drain zombie queue |
| 网络/timeout 依赖调度 | `run_tasks()` background poll 和 `do_wake_expired()` |

WaitQueue 的使用模式是：检查条件、入队、释放锁、切换、唤醒后复查条件。`Completion` 只适合 vfork 这类一次性完成事件。

## 4. signal/futex/IPC

| 子系统 | 关键状态 | 调试方向 |
|--------|----------|----------|
| signal | TCB pending/mask、PCB shared pending、sighand | 投递是否入队，mask 是否屏蔽，trap return 是否 delivery |
| pidfd/signalfd | fd 对象、target process、mask | fd 类型、target 状态、buffer 大小、nonblock |
| futex | private/shared key、WaitQueue、用户 word | VMA 是否 shared、物理 key 是否一致、word 是否等于 val |
| IPC | `IpcNamespace` registry、WaitQueue、用户结构体 | key/id/权限、NOWAIT、对象删除唤醒 |
| timer/rlimit | task rusage、KernelTimerQueue、deadline | 时间是否推进，到期 action 是否投递/唤醒 |

## 5. 测试映射

| 功能 | 测试 |
|------|------|
| init/exec | busybox shell、LTP exec |
| fork/clone/thread | pthread、LTP clone/clone3、libctest |
| wait/exit | LTP wait/waitid、shell 子进程 |
| scheduler | busybox 并发、unixbench、cyclictest |
| signal | LTP signal、tgkill、sigaltstack、signalfd |
| futex | pthread mutex/cond、LTP futex、libcbench |
| IPC | LTP msg/sem/shm、POSIX MQ |
| timer/rlimit/sched ABI | LTP timer、nanosleep、sched、rlimit、prctl |
