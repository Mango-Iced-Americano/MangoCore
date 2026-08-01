---
title: "MangoCore SMP 锁序与中断上下文约束"
category: architecture
status: proposed
owner: MangoCore Team
last_updated: 2026-07-31
tags: [smp, locking, irq, preemption, scheduler, tlb]
related_docs:
  - "docs/10_plan/smp-8core-implementation.md"
  - "docs/10_plan/smp-agent-execution-spec.md"
  - "docs/01_architecture/boot-and-trap.md"
  - "docs/05_process/scheduler.md"
---

# MangoCore SMP 锁序与中断上下文约束

本文定义 SMP 改造期间的目标锁契约。`status: proposed` 表示这些规则是实施门禁，
不表示当前单核代码已经满足。每个引入或改变锁关系的批次都必须同步本文，并用实际调用链验证。

## 1. 基础原语前置条件

在 AP 允许处理中断或多个 CPU 进入共享内核路径前，先提供统一的 `IrqSaveSpinLock`：

- guard 创建时保存本 CPU 原中断状态并关闭本地中断；
- guard 销毁时恢复它保存的状态，不得无条件开中断；
- 同一 CPU 上嵌套 guard 必须严格按 LIFO 销毁；
- 跨 CPU 互斥由锁本身保证，关本地中断只解决本 CPU 的 IRQ 重入；
- 是否同时增加 `preempt_depth` 由实现模型统一决定，不能把 `irq_depth` 当成正确性的替代品；
- guard 不得跨 context switch、yield、睡眠或远端 ack 等待点。

B39 已在不取得任何普通锁的 hard-IRQ fast path 上开放 AP 本地 timer；deferred AP 分支只
推进 CPU-local tick。`IrqSaveSpinLock` 的完整门禁仍约束 console、设备和其它 IRQ 可达共享
对象，不能因 timer 已开放而外推这些子系统已经多核安全。

## 2. 上下文能力

| 上下文 | 普通锁 | irq-save 锁 | 分配/睡眠 | 等待远端 ack |
|---|---|---|---|---|
| boot BSP/AP park | 仅已初始化对象 | 可以 | 发布堆后才可以 | 仅有界启动握手 |
| hard IRQ / IPI | 禁止 | 仅最小、已证明的叶子锁 | 禁止 | 禁止 |
| 调度/idle 栈 | 可以 | 可以 | 不得在持锁时睡眠 | 释放普通锁后可以 |
| 普通任务/系统调用 | 可以 | 可以 | 释放自旋锁后可以 | 释放 MM/PTE 锁后可以 |
| panic/STOP | 禁止等待普通锁 | 只用 try/raw fallback | 禁止 | 仅有界原子 ack |

IPI handler 只能访问 per-CPU mailbox、固定 shootdown slot 和无锁诊断计数；不得获取
runqueue、task.inner、MM/PTE、timer、VFS、网络或设备业务锁。

## 3. 部分序而非虚假的总序

MangoCore 不采用“给所有锁编号后允许任意嵌套”的总序。以下路径必须拆成
“锁内改变状态—释放—执行下一阶段”，从结构上消除双锁依赖：

1. **唤醒目标态**：统一入口在当前调度锁内裁决
   `Blocking(cpu) -> Running(cpu)` 或 `Blocked -> Queued(cpu)`；拆分 per-CPU
   runqueue 后只锁一个目标队列，释放 runqueue 后才发 IPI。
2. **调度**：task.inner 与 runqueue 不得嵌套；状态转移 API 是唯一调度状态真值来源。
3. **迁移/偷取**：任何时刻只持有一个 runqueue 锁；从 victim 取出后释放，再按 CAS
   结果进入目标队列，失败则回滚到合法状态。
4. **页表失效**：MM/PTE 锁内修改并记录 `MmuGather`，释放锁后本地 flush、发送
   shootdown、等待 ack；ack 完成后才能释放 frame/页表页/ASID。
5. **timer 重编程**：timer queue 锁内更新最早 deadline，释放锁后向 CPU0 发送
   `TIMER_REPROGRAM`；IPI handler 只保留原子请求，不能读取 timer queue。
6. **console**：全局 irq-save console 锁是叶子锁；持有时不得获取其他锁。
   panic 路径不等待该锁，直接走原始 UART/SBI fallback。
7. **lwext4**：跨实例全局锁位于 C 调用外层，保护区内只允许同步块 I/O，禁止
   yield、任务事件等待或调用会反向获取 VFS 高层锁的路径。
8. **线程组退出**：首次发布允许 `thread_group -> 单个 RunQueue` 的短嵌套；
   group-exit 快照释放 `thread_group` 后才取得 task/registry 锁、唤醒或发送 IPI。
   不存在 RunQueue 反向获取 thread-group 锁的路径。

### 3.1 B15 历史过渡约束

B15 尚未拆分 per-CPU runqueue 时，ready/interruptible 容器曾由单一
`TASK_MANAGER` 保护。该实现只用于说明状态机的演进背景，已由 B18 的 3.3 节取代，
不得再作为新增调用路径的锁序依据。

WaitQueue 的 `wake_*` 以 `WaitQueue -> TASK_MANAGER -> 单个 RunQueue` 的单向顺序
调用；反向获取不存在，且该路径不获取 `task.inner`。B20 已把 IPI 通知与容器迁移
分段：锁内只完成 registry/runqueue 交接，释放全部调度锁后才敲远程 doorbell。

B15 新增的 publish、wake、block 和 switch-out 状态迁移都不在 `TASK_MANAGER`
内获取 `task.inner`。当时登记的 nice-aware 锁内读取技术债已在 B18 通过原子
nice/vruntime hint 消除。

### 3.2 B17 Per-CPU current 约束

B17 已把全局 `PROCESSOR` 拆为每个 `PerCpu` 独占的 `CpuTaskState`。本 CPU
processor 锁只保护 current `Arc` 和 idle context；hard IRQ/IPI 不获取该锁，
panic 诊断只能使用 `try_lock()`。

- `current_task()` 在锁内只克隆 `Arc`，返回前释放锁；
- dispatch 先单独取得 `task.inner` 中的 context 指针，再获取 processor 锁发布
  current，禁止形成 `processor -> task.inner`；
- processor 锁必须在 `__switch` 前释放，不能跨 context switch；
- current 槽只能在已回到所属 CPU 的 idle 栈后清空；
- `schedule()`、退出和架构 `noreturn` 路径前必须释放本地 current `Arc`，因为旧
  Rust 栈帧不会被展开。

B17 本身没有引入 runqueue 双锁、远程 enqueue 或任务迁移；其后的 B18 已拆出
per-CPU RunQueue，但仍保持生产任务 owner 为 CPU0。

### 3.3 B18 Per-CPU RunQueue 约束

B18 删除全局 runnable 容器。每个 `CpuTaskState` 独占一个 `RunQueue`，其锁只保护
该 CPU 的 `Queued(cpu)` 成员关系和 nice 快速路径计数；`nr_running` 只是排队任务数的
无锁近似值，不包含 current，也不替代锁内成员关系。

- publish、fetch、yield 后重新入队只获取一个 owner runqueue；
- nice-aware 选择只读取 TCB 的原子 nice/vruntime hint，不在 runqueue 锁内获取
  `task.inner`；
- Blocked 唤醒先持有 `TASK_MANAGER` 从 interruptible registry 移除任务，再按
  `TASK_MANAGER -> 单个目标 RunQueue` 提交 `Blocked -> Queued(cpu)`；
- 批量移除也采用同一方向，并逐个定位 owner；任何时刻不得同时持有两个 runqueue；
- 从 runqueue 撤回的 `Arc` 必须先释放队列锁，再执行 drop；
- B18 当时仍固定目标为 CPU0；B37 已用 affinity-aware 通用放置取代该历史限制，
  但远程 enqueue 的唯一 owner 仍由 runqueue 锁和任务状态确立。

### 3.4 B19 AP 调度与内核栈发布约束

B19 只为 focused ktest 的 kernel-only 任务开放显式目标 CPU，不改变普通任务的 CPU0
策略。其跨核发布顺序固定为：

1. CPU0 在 `KERNEL_SPACE` 锁内建立动态 kernel stack 映射并释放锁；
2. 不持有 MM/PTE/runqueue 锁发送 `KERNEL_TLB_SYNC`，等待目标本地 flush ack；
3. ack 完成后只锁目标的一个 runqueue，提交 `New -> Queued(cpu)` 并释放锁；
4. 最后发送 `RESCHEDULE` doorbell，IPI handler 只置位；AP idle 或运行中用户任务的
   trap-return 安全点随后消费，不在 hard IRQ 内 fetch。

AP 安装页表根时可以短暂取得 `KERNEL_SPACE` 锁；此时 CPU0 只在 scheduler-ready
屏障等待且不持锁。AP dispatch 前只锁自己的 runqueue；`dispatch_task()` 先后取得
`task.inner` 和本地 processor，但两把锁不嵌套，也不跨 `__switch`。任务切回 idle
后先释放 processor 锁，再把 Zombie 加入受锁的全局 `TASK_MANAGER`，因此没有
`processor -> TASK_MANAGER` 嵌套。

这个批次没有两个 runqueue 的同时持有、迁移或 work stealing。AP 任务入口也不得
访问尚未审计的 console、FS、NET、设备和用户 MM；这些能力约束不能用锁本身替代。

### 3.5 B20 远程 blocked wake 约束

B20 不新增调度状态。`last_cpu` 只记录最近一次成功 fetch 的 CPU；B31 又用
`cpus_allowed` 约束哪些 CPU 可以取得 owner。任务真正阻塞后，统一 wake 入口按以下
顺序重新发布：

1. 持有 `TASK_MANAGER`，确认状态为 `Blocked` 并从 interruptible registry 移除；
2. 计算 `cpus_allowed & online & scheduler & !stopped`，对候选 CPU 的
   `nr_running + current_present` 做无锁负载估算；`last_cpu` 合法且不高于最小负载 `+1`
   时优先保留局部性，否则选最小负载，同负载选最低 CPU 编号；
3. 在 `TASK_MANAGER -> 一个目标 RunQueue` 锁序下提交 `Blocked -> Queued(target)`；
4. 释放目标 RunQueue，再释放 `TASK_MANAGER`；批量路径只保留目标 CPU bitmask；
5. 外层排除本 CPU 后发送 `RESCHEDULE`，IPI handler 只置 per-CPU 原子提示；目标在 AP
   idle 或用户 trap-return 安全点消费。

`Blocking(cpu)` 的提前 wake 仍只恢复 `Running(cpu)`，不入 runqueue、不发 IPI；idle
侧随后把它重新排入本地队列。批量 wake 每次调用 `enqueue_woken()` 都在函数返回前
释放该目标队列，因此循环不会同时持有两个 runqueue。当前该远程能力只对受控
kernel-only AP 任务完成验证。初始 affinity 已作为入队硬约束，B34 的本地 current 写侧
不持 task.inner/runqueue 锁完成目标选择和内核栈同步，发布 mask/target 后立即进入既有安全点。

B35 复用同一 `TASK_MANAGER` 锁串行化稳定 Blocked 线程的 affinity 与 wake：写侧必须在锁内
同时确认精确 `Blocked` 状态和同一 TCB 指针仍在 registry，随后 Release 发布 mask；wake 取得
同一锁后以 Acquire 读取并选择目标。只检查状态不够，因为线程退出清理摘除 registry 后、
标记 Zombie 前存在短暂 Blocked 窗口。该路径不获取 runqueue，也不搬 owner；远程
Running/Blocking 修改在 B35 当时尚未实现，后续由 B38 的请求槽协议闭合。

### 3.6 B36 稳定 Queued affinity 搬队约束

B36 不为 queued 搬队同时锁定源/目标 runqueue。顺序固定为：

1. 不持调度锁选择合法目标，并完成目标 kernel-stack TLB 同步；
2. 只锁 source，复核 `Queued(source)` 和精确 TCB 成员关系，提交
   `Queued(source) -> Migrating` 后摘除节点、释放 source；
3. `Migrating` 的同步调用方 Release 发布新 mask；
4. 只锁 target，提交 `Migrating -> Queued(target)` 并插入节点、释放 target；
5. 所有队列锁释放后才发送 RESCHEDULE。

`Migrating` 后禁止获取 `TASK_MANAGER`、等待 IPI/TLB ack 或进入析构；因此退出清理
即使在 `TASK_MANAGER` 内短暂等待搬队完成，也不存在反向依赖。nice 更新读到
旧 owner 时，必须先在旧队列锁内校准派生计数，再按最新状态重新定位。`Queued(cpu)` 状态下
若同一 TCB 不在该 owner 队列，且该队列锁仍由检查方持有，应 fail-stop；不能把真实容器损坏
误判为迁移，因为迁移回该 CPU 同样必须先取得这把锁。

### 3.6.1 B37 affinity-aware 通用放置约束

B37 把新任务发布、Blocked wake 和 current 自迁移的目标选择收敛到同一函数。
选择器只依赖 per-CPU 原子提示，不依赖 processor 锁：

- `nr_running` 只近似表示已排队数，`current_present` 只近似表示 current 槽非空；
- 两个值只影响放置质量，不证明任务 owner，也不取代目标 runqueue 锁内的
  affinity/状态复核；
- `current_present` 在 current 槽安装后 Release 置位，在 idle 栈取回 current 后
  Release 清位；读侧用 Acquire 取样；
- 选择器不获取 `TASK_MANAGER`、processor 或 runqueue 锁，不等待 IPI/TLB ack，
  因此可在既有 `TASK_MANAGER -> 单个 RunQueue` 顺序中使用；
- BSP 在 scheduler-ready mask 发布前创建 init/ktest runner 时显式放到 CPU0，
  这是启动时序例外，不是普通任务的隐式回退。

### 3.6.2 B38 远程 Running/Blocking affinity 锁序

B38 不增加调度状态，而是在 TCB 中增加受锁的单个
`remote_affinity_request` 槽。该槽不是 owner 容器；任务切回 idle 前仍保持
`Running(source)`，切栈后才直接交给 `Queued(target)`。固定锁序为：

```text
task.inner -> remote_affinity_request -> 单个 RunQueue
TASK_MANAGER -> remote_affinity_request -> 单个 RunQueue
```

具体约束：

- `exit_thread_resources()` 可在持有 `task.inner` 时调用 `mark_zombie()`，因此
  `task.inner -> remote_affinity_request` 是显式锁序；不存在请求槽反向获取
  `task.inner` 的路径；
- `begin_interruptible_sleep()` 由外层持有 `TASK_MANAGER`，再获取请求槽，
  在同一临界区完成 `Running -> Blocking` 和旧请求 Retry；
- 源 idle 的 `finish_switch_out()` 持有请求槽，再由
  `requeue_after_switch()` 短暂取得一个 target runqueue；直到
  `Running(source) -> Queued(target)` 完成后才发布 Applied；
- runqueue 入口不获取请求槽，因此没有 `RunQueue -> remote_affinity_request`
  的反向路径；
- target kernel-stack TLB 同步必须在获取请求槽前完成；IPI 发送、
  请求方协作式 yield 和 context switch 也都发生在解锁后；
- `RemoteAffinityRequest::complete()` 只做单次 CAS 和 Release 发布，不获取
  WaitQueue/`TASK_MANAGER`，所以可在上述短临界区中调用；请求方用 Acquire
  读取 Applied/Retry。

远程写侧看到 `Blocking` 时不取上述任何锁，而是协作式让出 CPU，
等待状态稳定为 Running、Blocked 或 Zombie 后重试。两个并发写侧会通过
单槽串行化，但 B38 focused 只动态验证单请求方；多写侧压力仍是后续门禁。

### 3.6.3 B39 Per-CPU tick 与全局 timer owner

每个 `PerCpu` 独占 `sched_tick_deadline_ns`，只有所属 CPU 在关中断安全点推进。硬件
timer 到期时只静默本地 one-shot、置 `timer_pending` 并返回；hard IRQ 不取得
`KERNEL_TIMER_QUEUE`、runqueue、task 或网络锁。

CPU0 是全局 kernel timer queue 的唯一执行者：

1. 任意 CPU 在 queue 锁内插入动作并计算它是否成为最早 deadline；
2. 释放 queue 锁后，CPU0 可直接重编程本地硬件；AP 则先 Release 置
   `timer_reprogram_requested`，再发送 `TIMER_REPROGRAM`；
3. CPU0 安全点 Acquire 消费 timer/reprogram 标志，短暂取得 queue 锁弹出到期项后立即解锁；
4. callback、timeout/timerfd 和网络 poll 均在锁外执行，最后按最新 queue deadline 与
   CPU0 本地 tick 的较小值重编程；
5. AP deferred 分支不取得全局 timer queue，只推进本地 tick 并重编程自己的 one-shot。

性能计数器可以由 AP 原子累加，但格式化快照会读取 FS/net 全局诊断状态并输出 console，
因此 `print_snapshot` 及 timer/scheduler 周期快照在共享子系统完成 SMP 审计前只允许 CPU0
执行。不能因计数器本身是 atomic，就把整个诊断调用链视为 IRQ-safe 或 SMP-safe。

直接发布 reprogram 标志是为了覆盖 CPU0 以 IRQ-off 状态轮询 idle 的窗口；IPI doorbell
用于尽快打断用户/内核执行。多个请求可以合并，因为 queue 保存权威绝对 deadline，安全点
每次都重新读取最早项。该协议不提供任意内核点抢占：长 syscall 中到达的 timer/IPI 仍等到
既有任务安全点才执行 callback 或切换。

### 3.6.4 B40 group-exit 门禁与 stop ack

首次发布固定采用：

```text
远端内核栈同步
  -> thread_group
       -> 一个目标 RunQueue：成员登记 + New -> Queued(cpu)
  -> 解锁
  -> RESCHEDULE IPI
```

group-exit 固定采用：

```text
thread_group：发布退出码 + 克隆 live 成员 Arc
  -> 解锁
  -> 逐任务短持 task.inner 投递 SIGKILL
  -> TASK_MANAGER/单个 RunQueue 唤醒 Blocked
  -> 解锁后聚合发送 RESCHEDULE
```

`sleep_interruptible()` 的登记后复查只读原子 group-exit/exec 快照，不取得
thread-group 锁，因此不会形成 `thread_group <-> TASK_MANAGER` 环。退出线程在没有上述锁时完成 user-memory/TLB
清理，最后以 AcqRel live-thread 递减发布 ack；观察到 1→0 的唯一线程才执行 PCB/MM
收尾。任何 ack、IPI 或 context switch 等待点都不持有 thread-group、task.inner、
TASK_MANAGER 或 runqueue 锁。

### 3.6.5 B41 exec 会话与 Completion

exec 的临时门禁固定采用：

```text
构造未发布的新 AddressSpace
  -> thread_group：安装 ExecSession + 克隆 live sibling Arc
  -> 解锁
  -> 逐 sibling 投递 SIGKILL/wake/RESCHEDULE
  -> 释放快照 Arc
  -> Completion 等待（不持任何内核锁）
  -> owner 独占旧 MM 后安装新映像
  -> thread_group：清除 ExecSession 并重新开放 clone
```

关键约束：

- `publish_thread()` 只在持有 `thread_group` 时同时检查永久 group exit 和临时 exec，
  因此成员登记、`New -> Queued` 与关门操作具有单一线性化顺序；
- `remove_thread()` 在用户资源撤销和 TLB flush/ack 完成后才以 AcqRel 递减 live
  count；只有权威计数变为 1 才完成 exec Completion，不能用成员快照为空代替；
- `remove_thread()` 可在 `thread_group` 内克隆 Completion，但必须解锁后
  `complete()`，因为唤醒会进入 WaitQueue/RunQueue；
- exec owner 的等待、IPI/TLB ack 和 context switch 都不持有 `thread_group`、
  `TASK_MANAGER`、task.inner 或 runqueue；
- WaitQueue 在自身条件锁内识别生命周期停止请求，先摘除 waiter 再返回
  `Interrupted`；调用层释放 syscall 栈上的 `Arc` 后才进入安全点；
- vfork child 已经 publish 后，父线程被生命周期请求中止只能返回 `StopCaller`，
  不能调用 unpublished cleanup。
- `reset_exec_resources()` 只在 live count 为 1 后运行。它在 `process.inner` 内读取
  fd table/sighand 的共享状态和快照，释放锁后再复制、关闭 CLOEXEC 与重置信号；
  重新取得锁只用于安装最终对象；
- 被替换的 futex table 必须先移出 `ProcessInner`，释放 `process.inner` 后再析构。
  WaitQueue 析构可能释放任务引用，禁止让这条析构链在进程锁内执行。

永久 group exit 可以在 exec owner 等待期间发布。安全点优先消费永久退出码；owner
醒来后放弃新映像并清除临时会话，但永久发布门仍保持关闭。

### 3.6.6 B43 exec 身份接管

非 leader exec 的身份更新固定采用：

```text
exec.finish()：释放 thread_group 锁并重新开放 clone
  -> task registry：校验 owner/旧 leader，交换 TidHandle，重键 weak entry
  -> 解锁
  -> 析构旧 leader 临时 Arc 和被替换的 TidHandle
  -> Per-CPU current TID
  -> OOM active tracker
  -> 释放 owner 的额外 thread quota
```

关键约束：

- live count 已在安装新映像前收缩为 1，因此 `exec.finish()` 后不再存在可与身份接管并发
  发布的同 PCB sibling；身份交换不需要嵌套 `thread_group` 和 task registry；
- registry 锁内可以短持单个 TCB 的 `tid_handle` 锁，但不得析构 TCB、`TidHandle`，
  也不得取得 processor、`TASK_MANAGER` 或 runqueue 锁；
- `TaskControlBlock::Drop` 只在“TID 键仍指向当前 TCB”时删除 registry 项。旧 leader
  迟到析构时即使数值已经交换，也不能删除新 leader 的 PID 项；
- processor current hint 与 OOM tracker 是 TID 派生索引，只能在 registry 事务完成
  后更新；这段路径不包含 context switch、IPI ack 或其它等待点。

### 3.7 B21 内核栈退休与 shootdown 锁序

TCB 最后一个 `Arc` 可能在 `wait`/进程锁保护区内消失，因此 `KernelStack::drop` 不能
直接取得页表锁或等待远端 CPU。缓存未满时它只把仍保持映射的 slot 放回
`KSTACK_CACHE`；缓存溢出时只短暂取得固定容量 `KSTACK_RETIRE_QUEUE` 并登记 slot，
两把锁不嵌套。

CPU0 idle 调度循环在尚未取得 processor、runqueue 或子系统锁时按以下顺序回收：

1. 取得退休队列锁弹出一个 slot，并立即释放队列锁；
2. 在 `KERNEL_SPACE` 锁内摘下 mapping、清除 PTE，但继续持有其中的 frame；
3. 释放 `KERNEL_SPACE` 后发送 shootdown，并在不持普通锁时等待 ack；
4. ack 完成后释放 frame；最后单独取得 slot allocator 锁归还 ID。

等待窗口临时开中断只用于让本 CPU 响应并发 IPI；hard timer 仍遵守 deferred 协议，不能
在 MM 层直接执行 timer callback。当前退休队列由 CPU0 生命周期路径消费；未来若允许 AP
并发完成普通进程回收，需要重新审查容量、所有者和批处理策略。

### 3.8 B22/B23/B51/B52 用户 MM 驻留与 shootdown 锁序

B22 的 trap-return 激活登记、B23 的 PTE 修改侧和 B51 的切离登记由同一个
`AddressSpace` 串行化：

1. 激活侧在 VM 锁内先把 CPU 加入 `active_cpus`，再读取 generation；落后时完成
   本地全用户失效并更新 observed，最后重查 generation；
2. 修改侧在同一 VM 锁内通过 `UserMapper` 修改 PTE，由 `MmuGather` 记录失效范围和
   退休 frame；`seal()` 推进 generation、校验 active CPU mask 快照并生成 `TlbFlush`；
3. 修改侧释放 VM 锁后，`TlbFlush::execute()` 才执行本地失效、发送 IPI/RFENCE、
   等待远端 ack；B52 的固定 slot 只携带 ASID、起始 VPN 和不超过 64 的页数，handler
   扫描固定 8 个槽且不获取普通锁，跨度更大时仍走 `USER_TLB_SYNC` 全刷；
4. 任务已经切回 idle 栈后，切离侧在改变 current/runqueue owner 前执行完整屏障，
   再在 VM 锁内清除本 CPU active bit；
5. 全部目标 ack 后才 drop retired frame。错误路径也必须保留这一顺序，不能退回
   “清 PTE 后立即释放”。

`read()` 只向闭包提供不可变引用；`write()/try_write()` 在锁内调用
`MmuGather::seal()` 取得 `TlbFlush`，再由块作用域析构 guard。这个接口不暴露可变
guard，是“先解锁再等 ack”的类型级门禁，不依赖每个调用点人工记住 `drop()`。

禁止在 VM 锁内等待 user-TLB ack。目标 CPU 可能已经关闭本地 IRQ并在 page fault 中等待
同一 VM 锁；发起者若持锁等它处理 IPI，会形成 `VM lock -> ack -> target VM lock` 环。
等待者临时开放 IRQ只能解决“两个无锁等待者互相成为 IPI 目标”，不能修复持普通锁等待。

`active_cpus` 与 `generation` 是不同 Atomic；各自的 Acquire/Release 不自动组成完整的
join/leave-vs-update 顺序。当前正确性来自共同 VM 锁，不来自对跨原子传递的猜测。
若 writer 在 leave 前取快照，它会包含该 CPU 并等待 ack；若 leave 先完成，writer
不再发送 IPI，但仍推进 generation，CPU 下次 enter 时必须补刷。若未来要把激活、切离
或目标快照改成 lockless，必须重新证明这两种次序，不能只增强某一个 Atomic 的内存序。

### 3.9 B44 membarrier 锁序

PRIVATE_EXPEDITED 的注册状态属于 `AddressSpace`。目标选择和 CPU enter/leave 沿用
B22/B23/B51/B52 的 VM 锁，而远端同步固定发生在解锁后：

```text
lock VM -> snapshot active CPU mask -> unlock VM
        -> pre full fence -> publish request -> IPI/fence/ack -> post full fence
```

快照先于新 CPU 激活时，新 CPU 在同一 VM 锁之后执行 enter full fence；激活先于快照时，
该 CPU 已进入 mask 并收到 IPI。CPU 若先完成 leave，切离 full fence 已提供有序点，因而
无需继续留在目标集合。IPI handler 只读取本 CPU request、执行 fence 并 Release 发布 ack，
不分配、不取普通锁。等待复用通用 `IpiWaitIrqGuard`，调用方不得持有 VM、runqueue、
task.inner 或其它普通锁。

### 3.10 B45 trap context 借用边界

trap context 页由对应 TCB 拥有，Rust 可变访问只能通过
`TaskControlBlockInner::trap_context_mut(&mut self)` 完成。返回引用的生命周期绑定到
`task.inner` guard；禁止把直映区指针包装成 `'static mut`，也禁止 current-task helper
从临时 guard 中返回引用。

LoongArch 用户未对齐访存固定分为：

```text
task.inner：快照 PC/store 源寄存器
  -> 解锁
  -> 用户指令和数据 copyin/copyout
  -> task.inner：校验 PC、提交 load 结果并推进 PC
```

用户访存可能缺页并进入 MM/TLB 同步，不能跨越它持有 `task.inner`。trap return 最后把
用户 trap context 地址交给汇编是明确的 owner 边界：Rust guard 已释放，当前任务仍由本
CPU current 槽独占，汇编立即恢复并离开内核。

### 3.11 B46 sigreturn 恢复锁序

`sys_sigreturn()` 固定分为三段：

```text
task.inner：快照用户 SP
  -> 解锁
  -> UserPtr 读取 sigmask / machine context / 架构扩展
  -> task.inner：一次提交用户寄存器与 sigmask
```

当前线程在 syscall 内仍是 live trap frame 的唯一执行 owner。远端信号只追加 pending；
exec、group-exit 和 affinity 请求由 owner 在返回安全点消费，不会越过锁改写 trap
frame。因此锁外 user read 不要求再增加 trap generation 或第二套状态机。全部读取成功
后才提交，畸形 frame 不会留下部分恢复状态。

信号 ABI 上下文只能通过架构 `machine_context()`/`set_machine_context()` 做字段复制，
禁止把 `TrapContext` 裸指针 cast 成 `MachineContext`。错误路径进入 noreturn 退出前必须
先释放当前函数额外持有的 task `Arc`。

### 3.12 B47 signal frame 投递锁序

自定义 handler 的 frame 投递固定分为：

```text
task.inner + sighand：取 pending、复制 action、复位 SA_RESETHAND
  -> 释放 sighand
  -> task.inner：快照返回上下文、mask 与 frame 布局
  -> 释放 task.inner
  -> UserPtrMut 写完整 SigInfo + UserContext
  -> task.inner：提交 handler 用户寄存器与 mask
```

用户 frame 写入可能缺页、CoW 或等待 TLB shootdown，因此该段不得持有 `task.inner`、
`sighand` 或其他普通内核锁。写成功前不发布 handler PC；写失败时直接退出，不需要回滚
半提交的 live trap context。

当前任务仍由本 CPU current 槽唯一执行，只有 owner 会写 live trap frame。远端信号只
追加 pending；exec、group-exit 和 affinity 请求在 owner 的安全点生效。因此该锁外
写入不需要新增 trap generation 或投递状态机。

### 3.13 B48 signal syscall 用户访存锁序

`sigaction()` 的 disposition 是进程共享状态，固定顺序为：

```text
UserPtr 读取可选新 action
  -> sighand：快照旧 action、提交新 action
  -> 解锁
  -> UserPtrMut 写回旧 action
```

`sigprocmask()` 和 `sigaltstack()` 修改当前线程状态，固定顺序为：

```text
UserPtr 读取可选新值
  -> task.inner：快照旧值、校验并提交新值
  -> 解锁
  -> UserPtrMut 写回旧值
```

任一 `UserPtr`/`UserPtrMut` 访问都可能缺页、触发 CoW 或等待 TLB shootdown，不能位于
`sighand` 或 `task.inner` 临界区内。共享 action 的快照和替换必须位于同一个
`sighand` 临界区；线程 mask/altstack 则由 current owner 与 `task.inner` 共同保证
一致性，不新增事务对象或状态机。

这不是可回滚事务：输入失败或校验失败发生在提交前；提交成功后的旧值 copyout 若返回
`EFAULT`，已提交状态保持不变。输入、输出指针别名时必须保持“先完整读、后写旧值”的
顺序。

## 4. 永久禁止的组合

- 两个不同 CPU 的 runqueue 锁同时持有；
- task.inner 与任意 runqueue 锁嵌套；
- 普通锁跨 `__switch`、schedule、yield、block、IPI ack 或 shootdown ack；
- MM/PTE 锁内等待远端 TLB ack；
- 发 IPI 时仍持有目标 CPU 可能在 handler 后续路径获取的锁；
- hard IRQ/IPI 中分配内存、进入文件系统/网络栈或执行任务切换；
- 以“当前只有单核”作为 `unsafe impl Send/Sync`、裸指针或 `static mut` 的安全证明。

## 5. 每批锁变更审查记录

涉及锁的 SMP 批次必须在修改前申请和修改后报告中列出：

- 新增或改变的锁、拥有者、IRQ 可达性和是否允许睡眠；
- 完整获取/释放路径，以及和本文哪条部分序对应；
- 是否可能在本地中断关闭、preempt 禁止或 panic 上下文进入；
- 错误、超时、重复 wake/IPI 和回滚路径；
- 双架构 focused test，以及 lockdep 尚未实现时使用的断言和计数器。

如果实际调用链需要本文未定义的嵌套关系，必须先更新设计并人工确认，不能在代码中局部
“先加一把锁”绕过。
