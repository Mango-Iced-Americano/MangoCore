---
title: "MangoCore SMP 锁序与中断上下文约束"
category: architecture
status: proposed
owner: MangoCore Team
last_updated: 2026-07-28
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
4. **页表失效**：MM/PTE 锁内修改并记录 `TlbBatch`，释放锁后本地 flush、发送
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

WaitQueue 的 `wake_*` 当前以 `WaitQueue -> TASK_MANAGER` 的单向顺序调用；反向获取
不存在，且该路径不获取 `task.inner`。Phase 3 拆分目标 runqueue 时必须把候选任务
收集与远程入队分段，落实上一节的最终部分序，不能照搬这个全局锁过渡实现。

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
4. 最后发送 `RESCHEDULE` doorbell，IPI handler 只置位，AP idle 安全点 fetch。

AP 安装页表根时可以短暂取得 `KERNEL_SPACE` 锁；此时 CPU0 只在 scheduler-ready
屏障等待且不持锁。AP dispatch 前只锁自己的 runqueue；`dispatch_task()` 先后取得
`task.inner` 和本地 processor，但两把锁不嵌套，也不跨 `__switch`。任务切回 idle
后先释放 processor 锁，再把 Zombie 加入受锁的全局 `TASK_MANAGER`，因此没有
`processor -> TASK_MANAGER` 嵌套。

这个批次没有两个 runqueue 的同时持有、迁移或 work stealing。AP 任务入口也不得
访问尚未审计的 console、FS、NET、设备和用户 MM；这些能力约束不能用锁本身替代。

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
