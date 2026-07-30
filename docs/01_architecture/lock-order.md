---
title: "MangoCore SMP 锁序与中断上下文约束"
category: architecture
status: proposed
owner: MangoCore Team
last_updated: 2026-07-30
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

`IrqSaveSpinLock` 的双架构实现、嵌套恢复测试和 panic 诊断通过前，不得开放 AP 的普通 timer
中断，也不得让多个 CPU 并发使用 console 等 IRQ 可达共享对象。

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
   `TIMER_REPROGRAM`。
6. **console**：全局 irq-save console 锁是叶子锁；持有时不得获取其他锁。
   panic 路径不等待该锁，直接走原始 UART/SBI fallback。
7. **lwext4**：跨实例全局锁位于 C 调用外层，保护区内只允许同步块 I/O，禁止
   yield、任务事件等待或调用会反向获取 VFS 高层锁的路径。

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
- B18 仍固定目标为 CPU0，因此本节尚不证明远程 enqueue、迁移或 work stealing 正确。

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
2. 计算 `cpus_allowed & online & scheduler & !stopped`，优先选择仍在交集中的
   `last_cpu`，无效时选交集的最低编号 CPU；
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
同一锁后以 Acquire 读取并选择目标。只检查状态不够，因为 exit/exec 摘除 registry 后、标记
Zombie 前存在短暂 Blocked 窗口。该路径不获取 runqueue，也不搬 owner；远程
Running/Blocking 修改仍未实现。

### 3.6 B36 稳定 Queued affinity 搬队约束

B36 不为 queued 搬队同时锁定源/目标 runqueue。顺序固定为：

1. 不持调度锁选择合法目标，并完成目标 kernel-stack TLB 同步；
2. 只锁 source，复核 `Queued(source)` 和精确 TCB 成员关系，提交
   `Queued(source) -> Migrating` 后摘除节点、释放 source；
3. `Migrating` 的同步调用方 Release 发布新 mask；
4. 只锁 target，提交 `Migrating -> Queued(target)` 并插入节点、释放 target；
5. 所有队列锁释放后才发送 RESCHEDULE。

`Migrating` 后禁止获取 `TASK_MANAGER`、等待 IPI/TLB ack 或进入析构；因此持
`TASK_MANAGER` 的 exit/exec remove 即使短暂等待搬队完成，也不存在反向依赖。nice 更新读到
旧 owner 时，必须先在旧队列锁内校准派生计数，再按最新状态重新定位。`Queued(cpu)` 状态下
若同一 TCB 不在该 owner 队列，且该队列锁仍由检查方持有，应 fail-stop；不能把真实容器损坏
误判为迁移，因为迁移回该 CPU 同样必须先取得这把锁。

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

### 3.8 B22/B23 用户 MM 激活与 shootdown 锁序

B22 的 trap-return 激活登记与 B23 的 PTE 修改侧现由同一个
`AddressSpace` 串行化：

1. 激活侧在 VM 锁内先把 CPU 加入单调 `cached_cpus`，再读取 generation；落后时完成
   本地全用户失效并更新 observed，最后重查 generation；
2. 修改侧在同一 VM 锁内通过 `UserMapper` 修改 PTE，由 `MmuGather` 记录失效范围和
   退休 frame；`seal()` 推进 generation、校验 cached CPU mask 快照并生成 `TlbFlush`；
3. 修改侧释放 VM 锁后，`TlbFlush::execute()` 才执行本地失效、发送
   `USER_TLB_SYNC`、等待远端 ack；
4. 全部目标 ack 后才 drop retired frame。错误路径也必须保留这一顺序，不能退回
   “清 PTE 后立即释放”。

`read()` 只向闭包提供不可变引用；`write()/try_write()` 在锁内调用
`MmuGather::seal()` 取得 `TlbFlush`，再由块作用域析构 guard。这个接口不暴露可变
guard，是“先解锁再等 ack”的类型级门禁，不依赖每个调用点人工记住 `drop()`。

禁止在 VM 锁内等待 user-TLB ack。目标 CPU 可能已经关闭本地 IRQ并在 page fault 中等待
同一 VM 锁；发起者若持锁等它处理 IPI，会形成 `VM lock -> ack -> target VM lock` 环。
等待者临时开放 IRQ只能解决“两个无锁等待者互相成为 IPI 目标”，不能修复持普通锁等待。

`cached_cpus` 与 `generation` 是不同 Atomic；各自的 Acquire/Release 不自动组成完整的
join-vs-update 顺序。当前正确性来自共同 VM 锁，不来自对跨原子传递的猜测。若未来要把
激活或目标快照改成 lockless，必须给出两种竞态次序的正式证明和相应 fence/重试协议，
不能只把 `generation.fetch_add` 改成更强内存序就宣称完成。

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
