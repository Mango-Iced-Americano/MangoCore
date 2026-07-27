---
title: "MangoCore SMP 锁序与中断上下文约束"
category: architecture
status: proposed
owner: MangoCore Team
last_updated: 2026-07-27
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

### 3.1 B15 过渡期约束

B15 尚未拆分 per-CPU runqueue，所有 ready/interruptible 容器仍由单一
`TASK_MANAGER` 保护。当前实现因此在这个锁内同时完成调度状态 CAS 与容器移动，
保证“状态已发布但尚未入队”或“已经出队但仍标为 Queued”不会被其他唤醒路径观察。

WaitQueue 的 `wake_*` 当前以 `WaitQueue -> TASK_MANAGER` 的单向顺序调用；反向获取
不存在，且该路径不获取 `task.inner`。Phase 3 拆分目标 runqueue 时必须把候选任务
收集与远程入队分段，落实上一节的最终部分序，不能照搬这个全局锁过渡实现。

本批新增的 publish、wake、block 和 switch-out 状态迁移都不在 `TASK_MANAGER`
内获取 `task.inner`。但旧 nice-aware `pop_fair_ready()` 仍通过 `sched_pick_key()`
在全局队列锁内读取 `sched_vruntime/sched_nice`；当前没有反向同时持锁路径，因此尚未
形成环，但这是明确登记的既有例外，Phase 3 必须以无锁调度 hint 或出锁快照消除。

## 4. 永久禁止的组合

- 两个不同 CPU 的 runqueue 锁同时持有；
- 新增 task.inner 与任意 runqueue 锁嵌套；上述 legacy nice-aware 例外不得扩散；
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
