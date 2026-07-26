---
title: "调度器与 run_tasks 主循环"
category: process
status: stable
author: MangoCore Team
last_update: 2026-07-26
tags: [process, scheduler, task-manager, processor]
---

# 调度器与 run_tasks 主循环

## 1. 源码位置

调度相关代码位于：

| 文件 | 作用 |
|------|------|
| `os/src/task/manager.rs` | `TaskManager`、ready/interruptible/zombie queue、WaitQueue、KernelTimerQueue |
| `os/src/task/processor.rs` | `Processor`、`run_tasks()`、current task 快速缓存、`schedule()` |
| `os/src/task/mod.rs` | `suspend_current_and_run_next()`、block/exit 调度入口 |
| `os/src/hal/*` | `__switch` 汇编上下文切换 |

调度器当前仍按 CPU0 单核运行模型组织。ready queue 为主要运行队列；
timer hard IRQ 只发布 per-CPU pending，真正的 timeout 处理和是否切换
延后到 trap-return/scheduler 安全点。显式 yield/block/exit 仍直接进入切换边界。

## 2. TaskManager

`TaskManager` 定义在 `os/src/task/manager.rs:85` 或 `manager.rs:97`。启用 `oom_handler` 时多一个 `active_tracker` 字段；普通构建字段如下：

```rust
pub struct TaskManager {
    pub ready_queue: VecDeque<Arc<TaskControlBlock>>,
    pub interruptible_queue: VecDeque<Arc<TaskControlBlock>>,
    zombie_queue: VecDeque<Arc<TaskControlBlock>>,
    ready_nonzero_nice_count: usize,
}
```

| 字段 | 说明 |
|------|------|
| `ready_queue` | `VecDeque<Arc<TaskControlBlock>>`，可运行任务 |
| `interruptible_queue` | 可中断睡眠任务 |
| `zombie_queue` | 当前任务退出后等待切栈 drop 的 TCB |
| `ready_nonzero_nice_count` | ready queue 中非零 nice 任务数量 |
| `active_tracker` | `oom_handler` 特性下用于 OOM 回收选择 |

全局实例：

```rust
pub static ref TASK_MANAGER: Mutex<TaskManager> = Mutex::new(TaskManager::new());
```

## 3. ready queue 选择策略

`pop_next_ready()` 有两个路径：

| 条件 | 策略 |
|------|------|
| `ready_nonzero_nice_count == 0` | FIFO fast path，`pop_front()` |
| 存在非零 nice | 扫描 ready queue，选 `(sched_vruntime, sched_nice, tid)` 最小任务 |

nice-aware 路径只在需要时扫描。`sched_nice_hint` 用于快速判断任务是否非零 nice。

这条路径在单核 ready queue 上实现简化公平选择，不维护 Linux CFS 的红黑树 runqueue 或多核负载均衡状态。

## 4. Processor

`Processor` 保存当前 CPU 状态：

```rust
pub struct Processor {
    current: Option<Arc<TaskControlBlock>>,
    idle_task_cx: TaskContext,
}
```

| 方法 | 说明 |
|------|------|
| `take_current()` | 取出当前任务，用于 block/yield/exit |
| `current()` | clone 当前任务 |
| `is_vacant()` | 当前 CPU 是否无任务 |
| `get_idle_task_cx_ptr()` | 获取 idle context 指针，供 `__switch` 使用 |

`PROCESSOR` 也是全局 `Mutex<Processor>`。

### 4.1 Processor 与 current cache

`Processor` 定义在 `os/src/task/processor.rs:22`。`current` 持有当前运行任务的强引用，`idle_task_cx` 是 idle 调度上下文。`processor.rs:58` 之后的一组 `CURRENT_*` 原子缓存提供 syscall 热路径查询：

```rust
static CURRENT_TASK_PTR: AtomicPtr<TaskControlBlock>;
static CURRENT_PID: AtomicUsize;
static CURRENT_TID: AtomicUsize;
static CURRENT_PARENT_PID: AtomicUsize;
static CURRENT_USER_TOKEN: AtomicUsize;
static CURRENT_UID: AtomicUsize;
static CURRENT_EUID: AtomicUsize;
static CURRENT_SUID: AtomicUsize;
static CURRENT_GID: AtomicUsize;
static CURRENT_EGID: AtomicUsize;
static CURRENT_SGID: AtomicUsize;
static CURRENT_PGID: AtomicUsize;
static CURRENT_SID: AtomicUsize;
static CURRENT_SYSCALL_ID: AtomicUsize;
```

这些缓存不是进程状态的唯一来源。真实所有权仍在 TCB/PCB 中；缓存只是在任务切入时发布，在切走时清零或更新。调试 getpid/getuid/uaccess token 异常时，应同时检查 `run_tasks()` 切入任务时的缓存写入和 `take_current_task()` 切走时的清理。

## 5. current task 快速缓存

`processor.rs` 维护一组原子缓存：

| 缓存 | 用途 |
|------|------|
| `CURRENT_TASK_PTR` | syscall 热路径快速得到当前 TCB |
| `CURRENT_PID/TID/PARENT_PID` | getpid/gettid/getppid |
| `CURRENT_USER_TOKEN` | uaccess 获取当前用户页表 token |
| `CURRENT_UID/CURRENT_EUID/CURRENT_SUID/CURRENT_GID/CURRENT_EGID/CURRENT_SGID` | 权限检查热路径 |
| `CURRENT_PGID/SID` | 进程组、会话查询 |
| `CURRENT_SYSCALL_ID` | heap_trace/perf_stats 下诊断 OOM syscall |

`run_tasks()` 在切入任务前写入这些缓存；`take_current_task()` 在切走当前任务时清零。

`current_task_ref()` 返回 `'static` 引用，但依赖单核和 `PROCESSOR.current` 持有强引用的事实。调用者不能跨调度点保存该引用。
syscall 受控中断窗口中，timer/IPI hard path 不得解引用这个裸指针；
只有迁移到 per-CPU current slot 并删除伪造静态生命周期后才能放宽。

## 6. run_tasks 主循环阶段

`run_tasks()` 每轮执行：

```text
schedule_tick += 1
  ├── console poll
  ├── do_wake_expired()
  ├── NET_INTERFACE.try_poll()        每 64 tick
  ├── fs::reclaim::maybe_reclaim_fs_caches()
  ├── drain zombie_queue
  ├── 每 64 tick 清理旧 ready/interruptible zombie 并记录队列统计
  ├── compact_shared_futex()
  ├── fetch_task()
  ├── queue sample / perf
  ├── switch to task
  └── idle path: NET_INTERFACE.poll() 或 spin_loop()
```

调度循环承担了若干后台维护职责，不能把它理解成单纯的 “while fetch ready task”。

这些阶段的顺序也有意义。先处理 console、timeout、net poll 和 reclaim，是为了在选择下一个 ready task 前尽量把外部事件转化为 ready 状态；先 drain zombie queue，是为了让已经退出并切回 idle 的任务尽快释放资源；最后才 `fetch_task()`，避免刚被唤醒的任务还要多等一轮。

调度循环里所有后台动作都必须短小，不应长期持有业务锁。它运行在单核内核的关键路径上，任何长时间操作都会推迟所有用户任务和 timeout wake。因此 PageCache reclaim、网络 poll、shared futex compact 都采用有限预算或降频策略。

## 7. 控制台轮询

rv64 上 `console_getchar()` 是 SBI ecall，因此每 64 tick 才轮询一次；非 rv64 每轮轮询。

字符处理优先级：

1. magic key，触发 trace dump 和 shutdown。
2. VINTR，例如 Ctrl+C，向前台/阻塞任务投递 `SIGINT`。
3. 普通字符，缓存给 TTY 并唤醒读者。

## 8. 网络与文件系统后台维护

调度循环周期性调用：

| 操作 | 频率 |
|------|------|
| `NET_INTERFACE.try_poll()` | 每 64 tick |
| idle 时 `NET_INTERFACE.poll()` | 每 64 idle tick |
| `fs::reclaim::maybe_reclaim_fs_caches()` | 每轮 |

网络 syscall 自己也会 poll；调度循环中的 poll 是后台兜底，避免没有 socket syscall 时网络状态完全不推进。

## 9. zombie queue

当前任务退出时仍运行在自己的内核栈上，不能立即 drop 最后一个 `Arc<TaskControlBlock>`。退出路径会：

```text
exit_current_and_run_next()
  ├── do_exit()
  ├── add_zombie_task(task)
  └── schedule(idle)
```

调度循环回到 idle 后通过 `take_zombie_tasks(64)` 批量取出并 drop。另有每 64 tick 兜底扫描 ready/interruptible queue 中异常残留的 zombie。

## 10. 上下文切换

切换到新任务：

1. `fetch_task()` 从 ready queue 取任务。
2. 锁住 task inner。
3. 若已是 Zombie，跳过。
4. 设置 `task_status = Running`。
5. `update_process_times_schedule_in()`。
6. 写入 current task 原子缓存。
7. `processor.current = Some(task)`。
8. 调用 `__switch(idle_task_cx_ptr, next_task_cx_ptr)`。

任务主动让出或阻塞时，`schedule(task_cx_ptr)` 切回 idle context。

### 10.1 中断状态不属于 `TaskContext`

双架构 `TaskContext`/切换汇编只保存 `ra`、`sp` 和 callee-saved GPR，
不保存 RISC-V `sstatus.SIE` 或 LoongArch `CRMD.IE`。B14 之后，用户
syscall 可在受控区间带着开中断状态 yield/block。`schedule()` 因此：

1. 在获取 `PROCESSOR` 锁前记住当前任务的中断状态并关闭中断；
2. 以 IRQ-off 状态切回 legacy idle scheduler；
3. 原任务再次被切入时，在 `__switch` 返回后恢复它自己的快照。

legacy `run_tasks()` 当前整个 housekeeping 循环仍保持 IRQ-off，因为 console、
network poll、FS reclaim 等共享路径尚未完成 IRQ 并发审计。后续 per-CPU
idle 将使用独立的“关中断—重查工作—架构 wait”协议，不会盲目将这个
旧循环扩大为开中断区间。

## 11. yield 与 block

`suspend_current_and_run_next()`：

```text
take_current_task()
task_status = Ready
update_process_times_schedule_out()
add_task(task)
schedule(task_cx_ptr)
```

`block_current_and_run_next()`：

```text
take_current_task()
task_status = Interruptible
update_process_times_schedule_out()
sleep_interruptible(task)
schedule(task_cx_ptr)
```

带 `_checked` 的版本会在加入 interruptible queue 后复查条件，避免“检查条件”和“真正睡眠”之间丢失唤醒。

## 12. interruptible queue 唤醒

`wake_interruptible(task)` 通过 `TaskManager::try_wake_interruptible()`：

1. 如果任务在 interruptible queue，移除并 `add_front()` 到 ready queue。
2. 如果不在 ready queue，也加入 ready queue。
3. 如果已经在 ready queue，记录 duplicate enqueue 并返回 `AlreadyWaken`。

`WaitQueue::wake_*()` 通常先把 task status 改为 `Ready`，再调用调度器唤醒。

## 13. OOM 回收调度协作

启用 `oom_handler` 时：

| 队列 | 回收动作 |
|------|----------|
| interruptible active task | `do_deep_clean()` |
| ready active task | `do_shallow_clean()` |

`ActiveTracker` 在 `fetch()` 时标记被调度任务 active，OOM 回收后 mark inactive。

## 14. perf/profile 状态

`processor.rs` 有调度 profile 计数器，记录：

| 类别 | 示例 |
|------|------|
| loop | loops、fetch、idle、switches |
| queue | ready/interrupible 长度 sample |
| stage | console、wake_expired、net_poll、reclaim、zombie drain、fetch、idle |
| timer | timer trap、handler、program timer |

这些用于诊断调度退化，不改变调度决策。

## 15. 调试核对点

| 现象 | 检查 |
|------|------|
| 任务不再运行 | 是否停在 interruptible queue，WaitQueue 是否唤醒 |
| ready queue 中出现 zombie | zombie queue drain 和兜底扫描 |
| getpid/getuid 返回旧值 | current hint 是否在切换或身份更新时刷新 |
| 非零 nice 任务饿死 | `ready_nonzero_nice_count` 是否更新 |
| 网络等待无 syscall 时卡住 | 调度循环后台 `try_poll/poll` 是否执行 |
