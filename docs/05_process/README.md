---
title: "进程与任务子系统 (Process and Task Subsystem)"
category: process
status: stable
author: MangoCore Team
last_update: 2026-07-28
tags: [process, task, scheduler, signal, futex]
---

# 进程与任务子系统

## 概述

MangoCore 的执行实体分为线程级 `TaskControlBlock` 和进程级 `ProcessControlBlock`。任务是调度实体，持有内核栈、trap context、线程私有信号状态和调度状态；进程持有地址空间、文件描述符表、文件系统状态、命名空间对象、信号处理表、futex 表、子进程关系和进程级生命周期状态。

调度器位于 `task/run_queue.rs`、`task/manager.rs` 和 `task/processor.rs`。每个 CPU
拥有独立 RunQueue、current 槽和 idle context；全局 TaskManager 只保留
interruptible/zombie/timer registry。AP 已进入精简本地调度循环，但当前只有 focused
ktest 的短 kernel-only 任务可显式远程入队；生产任务和 blocked wake 仍固定 CPU0。
默认 nice 为 0 的本地队列按 FIFO 取任务；存在非零 nice 任务时进入简化公平选择路径。

## 依据范围

| 主题 | 主要源码 |
|------|----------|
| 任务结构 | `os/src/task/task.rs` |
| 进程结构 | `os/src/task/process.rs` |
| runnable 队列 | `os/src/task/run_queue.rs` |
| 等待、回收和 timer registry | `os/src/task/manager.rs` |
| 调度主循环 | `os/src/task/processor.rs` |
| clone/unshare/setns | `os/src/syscall/process/clone.rs` |
| exec | `os/src/syscall/process/exec.rs`, `os/src/task/task.rs` |
| exit/wait | `os/src/syscall/process/lifecycle.rs`, `os/src/task/mod.rs` |
| signal/pidfd/signalfd | `os/src/syscall/process/signal.rs`, `os/src/task/signal/` |
| futex | `os/src/syscall/process/futex.rs`, `os/src/task/threads.rs` |
| IPC | `os/src/syscall/process/ipc.rs` |

## 架构

```
+-------------------------------------------------------------+
| syscall/process/*                                           |
| clone exec exit wait signal futex ipc ids time misc         |
+-------------------------------------------------------------+
| ProcessControlBlock                                         |
| vm files fs uts net mnt ipc sighand futex children state    |
+-------------------------------------------------------------+
| TaskControlBlock                                            |
| tid kstack trap context signal mask task status sched state |
+-------------------------------------------------------------+
| Per-CPU RunQueue + Processor | global TaskManager           |
| runnable/current/idle        | wait/zombie/timer registry   |
+-------------------------------------------------------------+
| HAL switch/trap/timer                                       |
+-------------------------------------------------------------+
```

## 核心对象

| 对象 | 粒度 | 主要职责 |
|------|------|----------|
| `TaskControlBlock` | 线程 | 调度实体、内核栈、trap context、线程信号 mask/pending、TLS/clear_child_tid、调度字段 |
| `ProcessControlBlock` | 进程 | 地址空间、fd table、cwd/root、命名空间、信号处理表、futex 表、子进程关系和 wait 队列 |
| `RunQueue` | 每 CPU | `Queued(cpu)` 成员关系、FIFO/nice-aware fetch |
| `TaskManager` | 全局 registry | interruptible/zombie/timer 管理和唤醒协调 |
| `Processor` | 每 CPU | 当前任务、idle task context、调度主循环 |
| `WaitQueue` | 阻塞原语 | futex、epoll、eventfd、timer 等等待路径复用 |
| `Completion` | 单次完成通知 | `CLONE_VFORK` 等一次性等待场景 |

## 状态

| 状态 | 枚举 | 说明 |
|------|------|------|
| 任务未发布 | `TaskStatus::New` | 已构造但尚未进入调度器 |
| 任务排队 | `TaskStatus::Queued(cpu)` | 由 CPU `cpu` 的 runqueue 拥有；当前仍固定 CPU0 |
| 任务运行 | `TaskStatus::Running(cpu)` | 由 CPU `cpu` 的 current slot 拥有；当前仍固定 CPU0 |
| 任务准备阻塞 | `TaskStatus::Blocking(cpu)` | 已登记到 interruptible registry，但仍由 CPU `cpu` 执行；早到 wake 可取消阻塞 |
| 任务阻塞 | `TaskStatus::Blocked` | 已切离 CPU，位于 interruptible registry，可被唤醒或信号打断 |
| 任务僵尸 | `TaskStatus::Zombie` | 线程退出后的回收状态 |
| 进程运行 | `ProcessState::Running` | 进程仍可调度或拥有活动线程 |
| 进程停止 | `ProcessState::Stopped` | signal/ptrace 相关停止状态 |
| 进程僵尸 | `ProcessState::Zombie` | 进程级退出完成，等待父进程观察或自动回收 |

## 文档索引

| 文档 | 内容 |
|------|------|
| `README.md` | 进程与任务总览、核心对象、状态 |
| `architecture.md` | 进程与任务架构详解，覆盖 TCB/PCB/调度/clone/exec/exit/signal/futex/IPC |
| `task-control-block.md` | TCB 字段、状态、创建和退出资源 |
| `process-control-block.md` | PCB 字段、ProcessInner、资源共享和 finish_exit |
| `scheduler.md` | per-CPU RunQueue、全局 registry、fetch 策略、主循环 |
| `waitqueue-completion.md` | WaitQueue、WaitResult、Completion 和使用者 |
| `task-process-scheduler.md` | TCB/PCB 字段、调度队列、WaitQueue、主循环 |
| `clone-exec-exit-wait.md` | clone/clone3、execve、exit、wait、vfork |
| `clone-and-namespace.md` | clone flags、资源共享、namespace、pidfd、vfork |
| `exec.md` | execve/execveat、shebang、argv/envp、ELF 装载 |
| `exit-wait.md` | exit、exit_group、finish_exit、wait4/waitid |
| `signal-futex-ipc.md` | signal、pidfd、signalfd、futex、SysV IPC、POSIX MQ |
| `signal.md` | signal 模块、投递、sigreturn、pidfd/signalfd |
| `futex.md` | futex key、wait/wake/requeue、waitv、clear_child_tid |
| `ipc.md` | SysV shm/sem/msg、POSIX MQ、IPC namespace |
| `time-sched-rlimit.md` | 时间 syscall、任务 timer、sched ABI、rlimit/prctl |
| `debugging.md` | 进程状态地图、clone/exec/exit/wait、调度、signal/futex/IPC 调试与测试映射 |
