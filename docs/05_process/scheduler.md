---
title: "调度器与 run_tasks 主循环"
category: process
status: stable
author: MangoCore Team
last_update: 2026-07-28
tags: [process, scheduler, task-manager, processor]
---

# 调度器与 run_tasks 主循环

## 1. 源码位置

调度相关代码位于：

| 文件 | 作用 |
|------|------|
| `os/src/task/run_queue.rs` | Per-CPU `RunQueue`、FIFO/nice-aware 选择和 owner 迁移 |
| `os/src/task/manager.rs` | interruptible/zombie/timer registry、WaitQueue、KernelTimerQueue |
| `os/src/task/processor.rs` | Per-CPU `CpuTaskState/Processor`、`run_tasks()`、`schedule()` |
| `os/src/task/mod.rs` | `suspend_current_and_run_next()`、block/exit 调度入口 |
| `os/src/hal/*` | `__switch` 汇编上下文切换 |

调度器当前处于 SMP 过渡阶段：current 槽、idle context 和 runnable 队列已按 CPU
拆分，但新任务/唤醒任务仍固定进入 CPU0，AP 尚未进入调度循环；
timer hard IRQ 只发布 per-CPU pending，真正的 timeout 处理和是否切换
延后到 trap-return/scheduler 安全点。显式 yield/block/exit 仍直接进入切换边界。

## 2. TaskManager 与 Per-CPU RunQueue

`TaskManager` 不再拥有 runnable 容器。启用 `oom_handler` 时多一个
`active_tracker` 字段；普通构建字段如下：

```rust
pub struct TaskManager {
    pub interruptible_queue: VecDeque<Arc<TaskControlBlock>>,
    zombie_queue: VecDeque<Arc<TaskControlBlock>>,
}
```

| 字段 | 说明 |
|------|------|
| `interruptible_queue` | 可中断睡眠任务 |
| `zombie_queue` | 当前任务退出后等待切栈 drop 的 TCB |
| `active_tracker` | `oom_handler` 特性下用于 OOM 回收选择 |

全局实例：

```rust
pub static ref TASK_MANAGER: Mutex<TaskManager> = Mutex::new(TaskManager::new());
```

每个 `CpuTaskState` 独占一个 `Mutex<RunQueue>` 和近似队列长度
`nr_running`。这里的计数只表示队列成员，不包含 current；后续负载选择必须再计入
current 槽，不能把它直接解释为完整 CPU load。

## 3. RunQueue 选择策略

`RunQueue::pop_next()` 有两个路径：

| 条件 | 策略 |
|------|------|
| `nonzero_nice_count == 0` | 本 CPU FIFO fast path，`pop_front()` |
| 存在非零 nice | 扫描本 CPU 队列，选 `(vruntime_hint, nice_hint, tid)` 最小任务 |

nice-aware 路径只在需要时扫描。`sched_nice_hint` 和 `sched_vruntime_hint` 都是原子
快照，因此选择路径不在持有 runqueue 锁时获取 `task.inner`。

这条路径在每 CPU `VecDeque` 上实现简化公平选择，不维护 Linux CFS 的红黑树或
调度域。B18 只完成容器拆分，生产 owner 仍固定 CPU0。

B15 先建立 `Queued(cpu)/Running(cpu)` 所有权协议，B18 再把容器放入对应
`PerCpu`。状态 CAS 与队列操作均由 `run_queue.rs` 的专用入口提交；普通业务代码
不能直接 push/pop。跨核任务执行和目标选择仍未开放。

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

`Processor` 不再有全局实例。每个 `PerCpu` 内嵌一个 `CpuTaskState`，后者用
本 CPU 的 `Mutex<Processor>` 保存 current 槽和 idle context。CPU-local
寄存器选出 `PerCpu` 后，调度路径只能访问所属 CPU 的 `Processor`。

### 4.1 CpuTaskState 与 current 槽

`CpuTaskState` 的布局为：

```rust
pub(crate) struct CpuTaskState {
    processor: Mutex<Processor>,
    run_queue: Mutex<RunQueue>,
    nr_running: AtomicUsize,
    current_pid: AtomicUsize,
    current_tid: AtomicUsize,
    current_syscall_id: AtomicUsize,
}
```

PID/TID 在 current 槽存续期间不变，因此保留 Per-CPU 无锁快照；syscall ID
仅用于诊断。父 PID、UID/GID、PGID/SID 和用户页表 token 都可能在任务运行期
变化，查询时直接读取 TCB/PCB 的权威原子 hint，不再维护需要跨路径刷新的影子缓存。

## 5. current task 查询

`current_task()` 先由 CPU-local 寄存器定位本 CPU 的 `CpuTaskState`，再在
`processor` 锁内克隆 current `Arc`，离开函数前释放锁。这样返回值具有真实的
引用计数生命周期，不再依赖全局裸指针或伪造的 `'static` 引用。

panic 诊断不能等待普通锁，也可能发生在 CPU-local 寄存器安装前，因此使用
`try_current_task()`：先验证寄存器值确实落在 `PER_CPUS` 数组中，再 `try_lock()`。
CPU-local 不可用或锁正被持有时返回不可用状态，不触发二次 panic。

调用者可以在普通函数调用期间持有返回的 `Arc`，但在 `schedule()` 或
`asm!(noreturn)` 等永不返回边界前必须显式 `drop`。上下文切换不会展开原 Rust
栈帧，若把本地 `Arc` 带过边界，它的析构函数将永远没有机会运行。

## 6. run_tasks 主循环阶段

`run_tasks()` 每轮执行：

```text
schedule_tick += 1
  ├── console poll
  ├── do_wake_expired()
  ├── NET_INTERFACE.try_poll()        每 64 tick
  ├── fs::reclaim::maybe_reclaim_fs_caches()
  ├── drain zombie_queue
  ├── 每 64 tick 清理 interruptible zombie 并记录本地/全局队列统计
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
  ├── TaskStatus = Zombie（Processor.current 仍持有 Arc）
  ├── schedule(idle)
  └── idle: finish_switch_out() -> zombie queue
```

调度循环回到 idle 后通过 `take_zombie_tasks(64)` 批量取出并 drop。另有每 64 tick 兜底扫描 ready/interruptible queue 中异常残留的 zombie。

## 10. 上下文切换

切换到新任务：

1. `fetch_task(cpu)` 只锁本 CPU `RunQueue` 并取出任务。
2. 同一 runqueue 临界区 CAS `Queued(cpu) -> Running(cpu)`；只有成功者得到任务。
3. 锁住 task inner，执行 `update_process_times_schedule_in()`。
4. 写入本 CPU 不变的 PID/TID 快照。
5. 在本 CPU `processor` 锁内执行 `current = Some(task)`，随后立即释放锁。
6. 调用 `__switch(idle_task_cx_ptr, next_task_cx_ptr)`。

任务主动让出或阻塞时，`schedule(task_cx_ptr)` 切回 idle context。

### 10.1 中断状态不属于 `TaskContext`

双架构 `TaskContext`/切换汇编只保存 `ra`、`sp` 和 callee-saved GPR，
不保存 RISC-V `sstatus.SIE` 或 LoongArch `CRMD.IE`。B14 之后，用户
syscall 可在受控区间带着开中断状态 yield/block。`schedule()` 因此：

1. 在获取本 CPU processor 锁前记住当前任务的中断状态并关闭中断；
2. 以 IRQ-off 状态切回 legacy idle scheduler；
3. 原任务再次被切入时，在 `__switch` 返回后恢复它自己的快照。

legacy `run_tasks()` 当前整个 housekeeping 循环仍保持 IRQ-off，因为 console、
network poll、FS reclaim 等共享路径尚未完成 IRQ 并发审计。后续 per-CPU
idle 将使用独立的“关中断—重查工作—架构 wait”协议，不会盲目将这个
旧循环扩大为开中断区间。

## 11. yield 与 block

`suspend_current_and_run_next()`：

```text
update_process_times_schedule_out()
schedule(task_cx_ptr)
idle: clear current -> Running(cpu) -> Queued(cpu) + ready enqueue
```

`block_current_and_run_next()`：

```text
update_process_times_schedule_out()
sleep_interruptible(task): Running(cpu) -> Blocking(cpu) + registry enqueue
schedule(task_cx_ptr)
idle: clear current -> Blocking(cpu) -> Blocked
```

带 `_checked` 的版本会在加入 interruptible registry 后复查条件。若条件已经满足，
统一 wake 入口执行 `Blocking(cpu) -> Running(cpu)`，仅取消阻塞而不提前入队；任务
仍会切回 idle，再由 `finish_switch_out()` 完成 `Running(cpu) -> Queued(cpu)`。

## 12. interruptible queue 唤醒

`wake_interruptible(task)` 通过 `TaskManager::try_wake_interruptible()`：

1. 若任务尚未切离 CPU，在 `TASK_MANAGER` 锁内 CAS
   `Blocking(cpu) -> Running(cpu)`，从 registry 移除，但不加入 ready queue。
2. 若任务已经切离 CPU，持 `TASK_MANAGER` 从 registry 移除，再按固定锁序取得
   CPU0 的单个 `RunQueue`，提交 `Blocked -> Queued(CPU0)` 并加入队首。
3. CAS 失败说明其他路径已经唤醒或任务已不再可唤醒，返回
   `AlreadyWaken`，绝不再次插入队列。

`WaitQueue::wake_*()` 只筛选原子状态并把候选交给调度器，不能在外部先写状态。
`TASK_MANAGER -> 单个 RunQueue` 是唯一允许的嵌套顺序；任何路径都不得反向取锁或
同时持有两个 runqueue。

## 12.1 状态与队列不变量

- `sched_state` 是调度状态唯一真值，`task.inner` 不保留影子字段；
- 一个任务最多属于一个 per-CPU runqueue 或一个 current slot；interruptible registry
  只是等待登记簿，`Blocking(cpu)` 期间会有意与 current slot 重叠，但不拥有执行权；
- 本 CPU `Processor.current` 只能在真实 context switch 回到 idle 栈后清空；yield、block、exit
  都不能在仍使用自身内核栈时提前取走 current Arc；
- `Queued(cpu)` 退出前必须先从相应队列移除并转为 `Blocked`；直接转 Zombie 会 fail-stop；
- 必成功的 publish/fetch/switch-out 迁移在所有构建中 fail-stop；只有重复 wake 使用
  允许失败的 CAS 并返回 `AlreadyWaken`；
- runqueue 锁内不获取 `task.inner`；公平选择只读取原子 nice/vruntime hint。
- publish/fetch/yield 只锁一个 runqueue；Blocked wake 和批量 remove 才按固定顺序
  `TASK_MANAGER -> 单个 RunQueue`，并且锁不跨 context switch、析构或等待点。

## 13. OOM 回收调度协作

启用 `oom_handler` 时：

| 队列 | 回收动作 |
|------|----------|
| interruptible active task | `do_deep_clean()` |
| ready active task | `do_shallow_clean()` |

`ActiveTracker` 在 `fetch_task()` 时标记被调度任务 active，OOM 回收后 mark inactive。

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
| runqueue 中出现 zombie | 检查是否绕过 `remove()` 直接执行 `Queued -> Zombie` |
| getpid/gettid 返回旧值 | 本 CPU current 槽与 PID/TID 快照是否同步发布、清理 |
| getuid/getpgid/token 返回旧值 | TCB/PCB 权威原子 hint 是否在 setter 中更新 |
| 非零 nice 任务饿死 | owner RunQueue 的 `nonzero_nice_count` 与原子 hint 是否更新 |
| 网络等待无 syscall 时卡住 | 调度循环后台 `try_poll/poll` 是否执行 |
