---
title: "调度器与 run_tasks 主循环"
category: process
status: stable
author: MangoCore Team
last_update: 2026-08-01
tags: [process, scheduler, task-manager, processor]
---

# 调度器与 run_tasks 主循环

## 1. 源码位置

调度相关代码位于：

| 文件 | 作用 |
|------|------|
| `os/src/task/run_queue.rs` | Per-CPU `RunQueue`、FIFO/nice-aware 选择和 owner 操作 |
| `os/src/task/manager.rs` | interruptible/timer registry、WaitQueue、KernelTimerQueue |
| `os/src/task/processor.rs` | Per-CPU current/runqueue/zombie 状态、`run_tasks()`、`schedule()` |
| `os/src/task/mod.rs` | `suspend_current_and_run_next()`、block/exit 调度入口 |
| `os/src/hal/*` | `__switch` 汇编上下文切换 |

调度器当前处于 SMP 过渡阶段：current 槽、idle context 和 runnable 队列已按 CPU
拆分，AP 已在 scheduler-ready 后进入精简本地调度循环；普通任务的初始 mask 仍为
CPU0-only，但首次发布已按任务 affinity 和近似负载选点。focused ktest 的短 kernel-only
任务可通过同一通用入口远程入队，并能在阻塞后
由统一 wake 路径回到最近运行 CPU。B28 还允许一个由 ktest 明确构造的用户探针
发布到 CPU1，验证真实 trap/yield/exit；B29 再让同一探针先在 CPU0 运行，并在真实
`sched_yield` 安全点唯一交给 CPU1。B31 已为每个 TCB 增加 `cpus_allowed`，所有入队
路径都将它作为硬约束；B32 的只读 `sched_getaffinity()` 已返回该 per-thread mask。
B34 又完成当前线程的运行期写侧：新 mask 排除本 CPU 时，syscall 会在同一安全点把
Running owner 迁到最低负载的合法 CPU。B35 继续闭合非 current 的稳定 Blocked 状态：
registry 锁内更新 mask，后续 wake 直接按新允许集发布，不需要迁移 owner。B36 又闭合稳定
Queued 状态：owner 仍合法时只更新 mask，排除 owner 时经短暂 `Migrating` 搬到新的单一
runqueue。B37 又让 clone/fork 继承的 mask 决定新任务的合法候选，并以调用 CPU 或
`last_cpu` 作为 locality 提示。B38 不扩张状态机，用 per-task 请求槽让远程
Running owner 在安全点真正交出 current；Blocking 则等待其稳定为 Running 或
Blocked 后复用对应协议。
B39 又给每个在线 CPU 建立独立 100Hz 绝对调度 tick；timer hard IRQ 仍只发布
per-CPU pending，真正的 tick 推进和是否切换延后到 trap-return/scheduler 安全点。
全局 kernel timer/timeout/timerfd/net poll 继续由 CPU0 独占。B33 已让远端 RESCHEDULE 在用户 trap-return
消费：handler 只置位，统一安全点与 timer 请求合并后最多切换一次。显式
yield/block/exit 仍直接进入切换边界。
B40 在同一安全点增加永久 group-exit 检查：只读取进程级原子退出码，命中后由
当前 owner 在本 CPU 清理并发布 live-token ack；请求 CPU 不再摘除或销毁远端
Running TCB。首次 clone 发布也与线程成员登记共用 group-exit 门禁。
B41 继续让多线程 exec 复用这个 owner 自清理安全点，但使用可恢复的临时 exec
会话和 Completion。B83 又将资源清理的 live ack 与 idle 撤销 current 槽的
inactive ack 分开：owner 同时等到 live count 降为 1 且全部 sibling inactive 后
才替换 MM，随后重新开放线程创建。
B51 在每个 `CpuTaskState` 中增加当前活跃用户 MM 的 Arc。用户 trap-return 通过
`switch_user_vm()` 安装它；任务真正切回 idle 栈后，调度器在改变 current owner 前
调用 `leave_user_vm()`，让 MM 的 active CPU mask 精确反映仍可直接返回用户态的 CPU。
槽锁只交换 Arc，不跨 VM 锁、ASID rollover 或 IPI 等待。

## 2. TaskManager 与 Per-CPU RunQueue

`TaskManager` 不再拥有 runnable 容器。启用 `oom_handler` 时多一个
`active_tracker` 字段；普通构建字段如下：

```rust
pub struct TaskManager {
    pub interruptible_queue: VecDeque<Arc<TaskControlBlock>>,
}
```

| 字段 | 说明 |
|------|------|
| `interruptible_queue` | 可中断睡眠任务 |
| `active_tracker` | `oom_handler` 特性下用于 OOM 回收选择 |

全局实例：

```rust
pub static ref TASK_MANAGER: Mutex<TaskManager> = Mutex::new(TaskManager::new());
```

每个 `CpuTaskState` 独占 `Processor`、`RunQueue`、`local_zombies` 和
`active_user_vm`，并保留 `nr_running` / `current_present` / `nr_zombies` 原子提示。
`active_user_vm` 的 Arc 固定精确旧 MM，供 exec 后切离；`nr_running` 只表示队列
成员，`current_present` 由 current 槽安装/清空路径以
Release 更新；B37 用两者之和估算放置负载。它们都不参与 owner 正确性判断，瞬时误差
最多造成次优选点。`nr_zombies` 只用于空队列快路径和诊断，真实
Arc 归属由对应 CPU 的 `local_zombies` 锁保护。

TCB 的 `last_cpu` 是最近一次成功完成 `Queued(cpu) -> Running(cpu)` 的运行位置。
它只为 `Blocked` 任务重新唤醒提供局部性提示，不是 owner；真实 runnable/current
归属始终由 `sched_state` 和对应 CPU 的容器共同决定。

TCB 的 `migration_target` 也不是 owner。它只是一项一次性请求：任务真正切回源 CPU
idle 栈后才被取走，并决定本次 `Running(source) -> Queued(target)` 的目标；仅登记请求
不会修改 owner，调用者还必须进入真实的调度切换。

TCB 的 `remote_affinity_request` 是另一个受锁的单槽，只串行化“远程排除
Running owner”与本地 block/exit/自迁移。槽中 mask/target 是不变请求，
`Pending/Applied/Retry` 是请求结果，不是 TaskStatus，也不表示新 owner。

TCB 的 `cpus_allowed` 是逻辑 CPU 位图，表达任务可以取得哪些 CPU 的 owner。
它与 `sched_state` 职责不同：前者是允许集合，后者仍是当前 owner 唯一真值。创建路径
在 `New` 状态写入初值；B34 允许 current `Running(cpu)` 在 syscall 安全点更新。运行期
读写以 Release/Acquire 配对；若新 mask 排除当前 CPU，写侧必须先同步目标内核栈映射，
再发布 mask 和一次性 `migration_target`，随后立即调度。

B35 对稳定 Blocked 使用另一条更短的协议：Blocked 没有 current/runqueue owner，写侧只需
在 `TASK_MANAGER` 锁内同时确认状态和 registry 成员关系，再发布 mask。wake 取得同一锁后
读取新值并选择队列。

B36 对稳定 Queued 使用 owner runqueue 作为 placement 锁。只有新 mask 排除 owner 时才进入
`Migrating`；该状态表示 TCB 已离开源队列、尚未进入目标队列，唯一 owner 是同步迁移调用方，
不是某个 CPU。mask 在这段无容器窗口发布，再由目标 runqueue 接管。
B38 的 Running 路径不借用 `Migrating`：任务切回 idle 前仍是 `Running(source)`，
切栈后直接由目标 runqueue 提交为 `Queued(target)`。

## 3. RunQueue 选择策略

`RunQueue::pop_next()` 有两个路径：

| 条件 | 策略 |
|------|------|
| `nonzero_nice_count == 0` | 本 CPU FIFO fast path，`pop_front()` |
| 存在非零 nice | 扫描本 CPU 队列，选 `(vruntime_hint, nice_hint, tid)` 最小任务 |

nice-aware 路径只在需要时扫描。`sched_nice_hint` 和 `sched_vruntime_hint` 都是原子
快照，因此选择路径不在持有 runqueue 锁时获取 `task.inner`。

这条路径在每 CPU `VecDeque` 上实现简化公平选择，不维护 Linux CFS 的红黑树或
调度域。普通任务仍从 CPU0-only mask 起步；显式设置过 affinity 的父线程 clone/fork
时，子任务会继承该 mask，并由 B37 的通用选择器取得合法首次 owner。受控 ktest 任务也
走同一入口，单 bit mask 仍保证它精确到达指定 AP。

B15 先建立 `Queued(cpu)/Running(cpu)` 所有权协议，B18 再把容器放入对应
`PerCpu`。状态 CAS 与队列操作均由 `run_queue.rs` 的专用入口提交；普通业务代码
不能直接 push/pop。B19 通过 `spawn_ktest_task_on()` 验证显式远程执行；B20 又让
这些任务走真实 Completion/WaitQueue 阻塞，并通过生产 wake 入口回到 `last_cpu`。
内核初始 affinity 约束已生效，current 线程可在 syscall 中收紧或扩展自己的 mask，远程
稳定 Blocked 线程可在 wake 前更新 mask，稳定 Queued 线程也可被搬到新 owner；B37 已统一
新任务与 wake 的 locality/负载选择，B38 已让远程 Running/Blocking 走 owner
安全点交接。默认全核 mask 和 work stealing 仍未开放。

### 3.1 首次发布与精确目标入口

`publish_task(task)` 是普通新任务入口。启动期尚无 current 的 init/ktest runner 显式发布到
CPU0；其余调用从 `cpus_allowed & online & scheduler & !stopped` 中选择目标：preferred
CPU 合法且负载不超过最小值 `+1` 时保留 locality，否则选择
`nr_running + current_present` 最小、CPU ID 最小的候选。clone/fork 因而不会再把继承了
非 CPU0 mask 的子任务错误投递到 CPU0。

`publish_task_on(task, cpu)` 是首次发布的统一生产入口，kernel-only ktest 和 B28
用户探针不再各自复制远程入队协议。顺序固定为：

1. 验证目标 CPU 已 configured、online，AP 还必须越过 scheduler-ready；
2. 快速检查进程没有进入 group exit；若目标是远端 CPU，再完成动态内核栈映射
   的 TLB 同步；
3. 取得 `process.thread_group` 锁并再次检查退出码，在同一门禁内登记成员、
   live token，并只取一个目标 runqueue 提交 `New -> Queued(cpu)`；
4. 释放 thread-group/runqueue 锁后，才发送 `RESCHEDULE` doorbell。

`publish_task_on()` 本身仍是精确目标提交原语，不做负载选择；普通
`publish_task()` 已在 B37 按 affinity/locality/近似负载选择目标，然后调用该原语。
普通任务的默认 mask 仍是 bit0，因此“放置器已通用化”不等于“默认用户任务已全核化”。
普通 clone 使用可失败的 `try_publish_task_on()`：最终门禁已关闭时返回 `EAGAIN`，
并由 syscall 层清理尚未发布的用户资源；启动/ktest wrapper 仍把拒绝视为不变量错误。

### 3.2 显式 yield 后迁移

`TaskControlBlock::request_migration(target)` 当前只接受两类调用者：尚未发布的 `New`
任务创建路径，或本 CPU 的 current Running 任务。入口先验证目标属于
`cpus_allowed`、已 online 且进入 scheduler，再同步目标的 kernel-stack TLB，最后才
Release 发布一次性目标。

任务调用 `schedule()` 后仍保持 `Running(source)`，直到源 idle 栈恢复并清空 source
current。`finish_switch_out()` 此时取走目标，并调用：

```text
requeue_after_switch(task, source, target)
    lock target runqueue
    Running(source) -> Queued(target)
    enqueue target
    unlock
    if remote: RESCHEDULE IPI
```

因此整个迁移只锁目标 runqueue，不需要 `Migrating` 状态，也没有同时锁源/目标队列。
目标 CPU fetch 时再执行 `Queued(target) -> Running(target)` 并更新 `last_cpu`。若任务真正
进入 Blocked 或 Zombie，未消费请求会被丢弃；本节点不改变 blocked wake 的目标语义。

### 3.3 运行期 affinity

#### Current 线程自迁移

`sched_setaffinity(0, ...)` 或严格等于 current TID 的调用走 B34 自迁移协议：

1. 按 Linux raw ABI 把用户 mask 先视为零，再复制 `min(cpusetsize, sizeof(usize))`
   个低位字节；短 mask 合法，超出 configured CPU 的位在求交时忽略；
2. 验证目标 TCB 就是本 CPU current，状态必须精确为 `Running(source)`，且不存在未消费
   的 `migration_target`；正 TID 不再回退成进程内任意线程；
3. 若新 mask 仍包含 source，只用 Release 发布 mask，不制造无意义切换；
4. 否则在 `mask & online & scheduler & !stopped` 中，按
   `nr_running + current` 选择负载最小、CPU ID 最小的目标；该负载只是放置时快照，
   不承担 owner 正确性；
5. 先完成目标 kernel-stack 映射同步，再依次 Release 发布 mask 和 target；syscall 释放
   TCB `Arc` 后立即调用 `suspend_current_and_run_next()`；
6. 源 idle 以 Acquire 取走 target，只锁目标 runqueue 完成
   `Running(source) -> Queued(target)`，锁外发送 RESCHEDULE；syscall 在目标 CPU 恢复后
   才向用户态返回成功。

#### 远程稳定 Blocked 更新

B35 只在目标没有 current/runqueue owner 时开放远程写侧：

1. syscall 完成用户拷贝、configured/runnable mask、严格 TID 和权限校验；
2. `update_blocked_affinity()` 取得 `TASK_MANAGER`，同时确认状态精确为 `Blocked`，且
   interruptible registry 中仍存在同一 TCB 指针；
3. TCB 在该锁内以 Release 写入 mask，然后释放锁；不获取 runqueue、不发送 IPI；
4. 后续 wake 取得同一 `TASK_MANAGER`，以 Acquire 读取 mask，优先复用仍合法的 `last_cpu`，
   否则选允许集中的最低编号 CPU，再走既有 `Blocked -> Queued(target)` 入口。

状态和成员关系必须一起检查：exit/exec 会先从 registry 摘除任务，稍后才把短暂的 Blocked
状态改为 Zombie；仅检查状态会对正在退出的线程伪装成功。并发 wake 与写侧由同一锁线性化：
写侧先拿锁则 wake 使用新 mask，wake 先拿锁则目标已变 Queued，写侧返回 `EOPNOTSUPP`。

#### 远程稳定 Queued 迁移

B36 在目标精确处于 `Queued(source)` 时执行：

1. 若新 mask 仍包含 source，取得 source runqueue 后同时确认状态和同一 TCB 成员关系，
   在锁内 Release 发布 mask，不改变队列或 `nr_running`；
2. 若新 mask 排除 source，先在不持调度锁时选择最低负载合法 CPU，并同步目标 kernel-stack
   映射；同步失败前任务仍完整留在旧 mask 和源队列；
3. 取得 source runqueue，复核成员后提交 `Queued(source) -> Migrating`，摘除节点和源计数，
   然后释放源锁；
4. `Migrating` 的唯一 owner 发布新 mask，再取得 target runqueue，提交
   `Migrating -> Queued(target)`、插入节点和目标计数；
5. 释放目标锁后才发送 RESCHEDULE。若另一 affinity、fetch 或 exit/exec 先改变 owner，入口
   依据最新状态重试或明确失败，不能在失败返回前部分写入 mask。

这条路径从不同时持有两个 runqueue，也不增加 per-task 锁、IPI reason 或第二套迁移容器。
目标栈 TLB 同步必须发生在进入 `Migrating` 前；否则 exit/remove 等待迁移时可能间接等待 IPI
ack 并破坏锁依赖。`update_nice()` 若在 hint 写入后读到旧 owner，会先重算旧队列派生计数，再
按最新状态追到新 owner，避免 `nonzero_nice_count` 漂移。

#### 远程 Running/Blocking owner 交接

B38 对远程 Running 任务分两种情况：

1. 新 mask 仍包含 owner：在 `remote_affinity_request` 锁内复核精确
   `Running(owner)`、无旧请求且无 `migration_target`，然后 Release 发布 mask；
   任务可继续执行，不制造无意义切换。
2. 新 mask 排除 owner：锁外选择合法 target 并完成 kernel-stack TLB 同步；
   锁内再次复核 owner，安装不变的 mask/target 请求；解锁后发
   RESCHEDULE，请求方协作式 yield。源 CPU 切回 idle 后持请求槽锁，发布新
   mask，只锁一个 target runqueue 提交 `Running(source) -> Queued(target)`，
   然后才将请求标记为 Applied。

`Blocking(owner)` 是登记阻塞到真正切栈之间的短窗口，远程写侧不为它
新建状态，而是让出 CPU 并等待其回到 Running 或稳定进入 Blocked。
`begin_interruptible_sleep()` 按 `TASK_MANAGER -> remote_affinity_request` 锁序撤销
旧请求并发布 Retry；current 自写侧和 Zombie 转换也取得同一请求槽，
避免遗留永远 Pending 的请求。

请求槽锁与 target runqueue 在 idle 收尾中会以
`remote_affinity_request -> 单个 RunQueue` 嵌套，以关闭“请求槽复核”与“owner 提交”
之间的窗口。不存在 RunQueue 反向取请求槽的路径；该锁也不跨 IPI、
TLB ack、context switch 或请求方等待点。

### 3.4 跨 CPU group exit

B40 沿用已有六态调度状态，不增加 `Exiting/WakePending` 等第二状态机：

1. 第一个退出者在线程组锁内固定退出码、关闭 clone 发布门禁并取得 live 成员快照；
2. sibling 获得私有 `SIGKILL`；Blocking/Blocked 复用 interruptible wake，
   Queued/Running owner 只收到 RESCHEDULE；
3. 每个 owner 在自己的任务安全点执行线程级清理，最后递减 live token；
4. 最后一个 ack 执行进程级 `finish_exit()`，其他 CPU 不会远程释放其内核栈或 MM 资源。

`sleep_interruptible()` 在 `TASK_MANAGER` 内提交 `Running -> Blocking` 后，释放锁并复查
线程组停止快照。若停止方恰好先看到 Running、任务随后才登记 Blocking，这次复查会
撤销睡眠；若退出/exec 发布更晚，停止方会看到 Blocking/Blocked 并执行 wake。两种次序
至少有一方负责唤醒，且没有把 thread-group 锁与 `TASK_MANAGER` 嵌套。

永久 group exit 不由发起者同步等待 completion：每个线程的 live token 就是 ack，
最后一个 ack 自然拥有 PCB/MM 收尾权。

### 3.5 多线程 exec 临时停止

B41 不增加 `TaskStatus`，也不让 exec owner 从远端 runqueue 摘除 sibling：

1. owner 在线程组锁内安装唯一 `ExecSession`，同时关闭 `CLONE_THREAD` 首次发布；
2. sibling 收到 SIGKILL/wake/RESCHEDULE，在所属 CPU 的 `run_task_safe_point()` 自行退出；
3. 每个 sibling 完成用户映射撤销和 TLB shootdown 后才递减 live token；
4. 计数变成 1 时 `Completion` 唤醒 owner，owner 此时才安装新 trap context 和 MM；
5. 安装完成后清除临时会话，线程发布门重新开放。

普通用户信号不能取消这个等待，否则已停止一部分 sibling 后无法回滚旧线程组；永久
group exit 可以覆盖 exec。所有 WaitQueue/Completion 等待路径都会识别线程组生命周期
停止请求，从等待队列中摘除自己并返回调用层释放栈上 `Arc`，避免 Blocked sibling 让
exec owner 永久等待。

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

B56 的 `CpuTaskDiagnostics` 另外读取 current PID/TID、排队数和 zombie 数的原子 hint；
`active_user_vm` 只做一次 `try_lock()` 并复制稳定 MM ID。它和外层 `CpuDiagnostics` 都是
best-effort 输出，不能替代 processor/runqueue 锁或调度状态机的 owner 判定。

调用者可以在普通函数调用期间持有返回的 `Arc`，但在 `schedule()` 或
`asm!(noreturn)` 等永不返回边界前必须显式 `drop`。上下文切换不会展开原 Rust
栈帧，若把本地 `Arc` 带过边界，它的析构函数将永远没有机会运行。

## 6. run_tasks 主循环阶段

CPU0 的 `run_tasks()` 每轮执行：

```text
schedule_tick += 1
  ├── console poll
  ├── do_wake_expired()
  ├── NET_INTERFACE.try_poll()        每 64 tick
  ├── fs::reclaim::maybe_reclaim_fs_caches()
  ├── drain 本 CPU local_zombies
  ├── 每 64 tick 清理 interruptible zombie 并记录本地/全局队列统计
  ├── compact_shared_futex()
  ├── fetch_task()
  ├── queue sample / perf
  ├── switch to task
  └── idle path: NET_INTERFACE.poll() 或 spin_loop()
```

调度循环承担了若干后台维护职责，不能把它理解成单纯的 “while fetch ready task”。

这些阶段的顺序也有意义。先处理 console、timeout、net poll 和 reclaim，是为了在选择下一个 ready task 前尽量把外部事件转化为 ready 状态；先 drain 本 CPU local zombies，是为了让已经退出并切回 idle 的任务尽快释放资源；最后才 `fetch_task()`，避免刚被唤醒的任务还要多等一轮。

调度循环里所有后台动作都必须短小，不应长期持有业务锁。CPU0 仍是
全局 housekeeping 关键路径，长时间操作会推迟 CPU0 用户任务和 timeout wake。
因此 PageCache reclaim、网络 poll、shared futex compact 都采用有限预算或降频策略。

AP 走独立的精简分支，只执行：

```text
短暂开放 IPI/timer → 立即关中断 → 处理 STOP/deferred reason
  → 推进本 CPU sched tick 并重编程本地 one-shot
  → drain 本 CPU local_zombies
  → fetch 本 CPU RunQueue
  → 共用 dispatch_task()/current/__switch/switch-out
  → 空队列时在 IRQ-off 窗口重查后执行 wfi/idle
```

AP 只推进本地调度 tick，不执行全局 timeout、kernel timer callback、console、network、
FS reclaim 或 OOM active tracker。B39 的无 syscall 用户忙循环已证明 CPU1 timer 可以在
trap-return 安全点把 current 交给同核 helper。远程
发布者遵守“先入队、释放 runqueue 锁、再发 RESCHEDULE”，因此空队列检查到 wait
之间到达的 doorbell 或 timer pending 会唤醒 CPU，不会丢失 wakeup。

### 6.1 Per-CPU tick 与 CPU0 全局 timer

`PerCpu.sched_tick_deadline_ns` 是所属 CPU 的绝对纳秒 deadline。安全点只推进一次到期
quantum；若因关中断落后一周期以上，直接从当前时间建立下一周期，不循环追赶旧 tick。

本地 one-shot 的选择规则是：

| CPU | 下一硬件 deadline |
|------|-------------------|
| CPU0 | `min(本地 sched tick, 全局 KernelTimerQueue 最早项)` |
| AP | 本地 sched tick |

任意 AP 插入新的最早全局 timer 时，不会错误地重编程自己的硬件 timer，而是在释放 queue
锁后向 CPU0 发布 `TIMER_REPROGRAM`。CPU0 的安全点再读取权威队列；hard IPI 不取锁、不执行
callback。该设计仍是安全点抢占，timer 打断长 syscall 时只记录 pending，任务切换和 callback
要等 syscall 返回或其它明确安全点。

调度/timer 性能计数仍可在所有 CPU 上用 relaxed atomic 聚合；会继续读取 FS/net 全局状态并
打印 console 的格式化快照仅由 CPU0 触发。该限制避免 AP 本地 tick 绕过共享子系统门禁。

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

## 9. Per-CPU zombie 回收

当前任务退出时仍运行在自己的内核栈上，不能立即 drop 最后一个 `Arc<TaskControlBlock>`。退出路径会：

```text
exit_current_and_run_next()
  ├── finish_current_exit()
  ├── TaskStatus = Zombie（Processor.current 仍持有 Arc）
  ├── schedule(idle)
  └── idle: finish_switch_out() -> owner CPU local_zombies
```

`finish_current_switch_out()` 已经位于 idle 栈。它先让旧 current 的用户 MM leave，
再清空 current 槽并释放 processor 锁，最后把 `Zombie` TCB 交给退出 CPU 的
`local_zombies`。CPU0 和 AP 都在自己的 idle 循环、
下一次 dispatch 之前取出并 drop，因此 AP 退出不再竞争全局 `TASK_MANAGER`。

父进程 wait/auto-reap 需要按 pid 同步清理时，先单独摘取 interruptible zombie，
再按 CPU 依次扫描本地回收队列；任一时刻只持有一把容器锁，承接 Vec
的扩容和 TCB 析构都在锁外。PCB 的 wait-visible zombie 状态与这个 TCB
对象寿命队列仍是两层独立语义。

B21 后，TCB 析构只把缓存溢出的内核栈 slot 登记到固定退休队列；
CPU0 下一次 idle 安全点在无普通锁状态下撤销映射、等待全核 TLB
ack、释放 frame，再归还 slot。曾在 AP 使用的 TCB 不再由测试保留到关机，栈地址可以在
协议完成后真实复用。

## 10. 上下文切换

切换到新任务：

1. `fetch_task(cpu)` 只锁本 CPU `RunQueue` 并取出任务。
2. 同一 runqueue 临界区 CAS `Queued(cpu) -> Running(cpu)`；只有成功者得到任务。
3. 锁住 task inner，执行 `update_process_times_schedule_in()`。
4. 写入本 CPU 不变的 PID/TID 快照。
5. 在本 CPU `processor` 锁内执行 `current = Some(task)`，随后立即释放锁。
6. 调用 `__switch(idle_task_cx_ptr, next_task_cx_ptr)`。

任务主动让出或阻塞时，`schedule(task_cx_ptr)` 切回 idle context。
idle 恢复后固定执行：`leave_user_vm()` → 清除 current → `finish_switch_out()`。
只有再次到达用户 trap-return 的任务才会重新 enter MM 并检查 generation；内核线程
不会伪造 active bit。

### 10.1 中断状态不属于 `TaskContext`

双架构 `TaskContext`/切换汇编只保存 `ra`、`sp` 和 callee-saved GPR，
不保存 RISC-V `sstatus.SIE` 或 LoongArch `CRMD.IE`。B14 之后，用户
syscall 可在受控区间带着开中断状态 yield/block。`schedule()` 因此：

1. 在获取本 CPU processor 锁前记住当前任务的中断状态并关闭中断；
2. 以 IRQ-off 状态切回本 CPU idle scheduler；
3. 原任务再次被切入时，在 `__switch` 返回后恢复它自己的快照。

CPU0 的 housekeeping 循环仍保持 IRQ-off，因为 console、network poll、FS reclaim
等共享路径尚未完成 IRQ 并发审计。AP 已采用独立的“关中断—重查工作—架构 wait”
协议，但 B19 kernel-only 任务运行期间也保持 IRQ-off；STOP/RESCHEDULE 最长延迟到
该短函数返回或主动 yield，不能据此开放无界通用内核线程。

用户返回侧使用唯一的 `run_task_safe_point()`：

```text
保存入口 IRQ 状态并关中断
  → 完成 deferred timer 工作
  → Acquire 取走本 CPU RESCHEDULE 提示
  → timer || IPI 时最多 suspend 一次
  → 任务恢复后还原入口 IRQ 状态
```

`take_reschedule_request()` 只消费 PerCpu 提示，不取调度锁；真正
`Running(cpu) -> Queued(target)` 仍在任务切回 idle 栈、current 清空后由既有 switch-out
路径完成。这样没有新增调度状态，也没有让 hard IRQ 直接进入 runqueue/context switch。

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
2. 若任务已经切离 CPU，持 `TASK_MANAGER` 从 registry 移除，在 `cpus_allowed`、
   online、scheduler-ready 且未 STOP 的交集中优先选择 `last_cpu`，否则选交集
   最低位；随后按固定锁序取得
   该目标的单个 `RunQueue`，提交 `Blocked -> Queued(target)` 并加入队首。
3. CAS 失败说明其他路径已经唤醒或任务已不再可唤醒，返回
   `AlreadyWaken`，绝不再次插入队列。
4. 单个 wake 返回目标 CPU，批量 wake 聚合目标 mask；外层释放 `TASK_MANAGER` 和
   runqueue 后才发送 `RESCHEDULE`。本地目标不发 IPI，远端 AP 由 doorbell 退出 idle。

`WaitQueue::wake_*()` 先以 CAS 领取一次性 entry token，再把 TCB 交给调度器。
它不能在外部改写 TaskStatus；早到 wake 的 token 会在 checked block 中撤销
随后登记的 `Blocking`。
`TASK_MANAGER -> 单个 RunQueue` 是唯一允许的嵌套顺序；任何路径都不得反向取锁或
同时持有两个 runqueue。

## 12.1 状态与队列不变量

- `sched_state` 是调度状态唯一真值，`task.inner` 不保留影子字段；
- `last_cpu` 只是无 owner 含义的唤醒提示，不能代替 `Queued/Running(cpu)`；
- `cpus_allowed` 只是 owner 允许集合；任何 `Queued(cpu)` 和 `Running(cpu)` 中的
  `cpu` 都必须属于该集合；
- `Migrating` 只允许出现在 queued 跨队列搬运的短窗口；此时 TCB 不在任何 runqueue/current，
  且迁移调用方不得等待 IPI、获取 `TASK_MANAGER` 或释放最后一个 `Arc`；
- 一个任务最多属于一个 per-CPU runqueue 或一个 current slot；interruptible registry
  只是等待登记簿，`Blocking(cpu)` 期间会有意与 current slot 重叠，但不拥有执行权；
- 本 CPU `Processor.current` 只能在真实 context switch 回到 idle 栈后清空；yield、block、exit
  都不能在仍使用自身内核栈时提前取走 current Arc；
- `Queued(cpu)` 退出前必须先从相应队列移除并转为 `Blocked`；直接转 Zombie 会 fail-stop；
- 必成功的 publish/fetch/switch-out 迁移在所有构建中 fail-stop；只有重复 wake 使用
  允许失败的 CAS 并返回 `AlreadyWaken`；
- yield 迁移只能在源 current 已于 idle 栈清空后交给目标队列；`migration_target` 不能直接
  修改 owner，也不能由任意远程 CPU 写入；
- runqueue 锁内不获取 `task.inner`；公平选择只读取原子 nice/vruntime hint。
- publish/fetch/yield/Queued affinity 任何时刻只锁一个 runqueue；Blocked wake 和批量 remove 按固定顺序
  `TASK_MANAGER -> 单个 RunQueue`，并且锁不跨 context switch、析构或等待点。
- 远程 wake 的 IPI 只能在 `TASK_MANAGER` 与所有目标 runqueue 都释放后发送。

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
