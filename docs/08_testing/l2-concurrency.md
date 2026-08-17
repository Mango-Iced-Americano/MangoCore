---
title: "L2 并发测试层（Systematic Concurrency Testing）"
category: testing
status: draft
author: MangoCore Team
last_update: 2026-08-17
tags: [testing, L2, concurrency, waitqueue, property-based, replay, systematic]
---

# L2 并发测试层（Systematic Concurrency Testing）

> **状态说明：** 本文档描述 L2 测试层的**设计意图**。当前正在实现一个 L2 PoC——
> 宿主侧 systematic concurrency testing，针对内核 WaitQueue。文中标注「实现中」的
> 能力表示设计已定、代码尚未全部落地，**不要**据此声称已完成。

## 1. L1 与 L2 的区别

L1 和 L2 都通过 host `cargo test` 运行，但二者验证方式本质不同：

| 维度 | L1（纯逻辑单元测试） | L2（属性 / 并发模型测试） |
|------|----------------------|---------------------------|
| 用例来源 | 人工编写固定 testcase | 自动生成操作序列 / 输入 |
| 期望 | 固定 expected 值 | 性质（invariant / property） |
| 交错控制 | 无（单线程顺序执行） | 控制并发交错（scheduler） |
| 失败处理 | 断言失败即停 | 记录 counterexample 并支持重放 |
| 目标 | 确定性逻辑正确性 | 并发下不变量保持、无丢唤醒等 |

L1 回答「这段纯逻辑在给定输入下是否返回预期结果」；L2 回答「在大量自动生成的
操作序列与并发交错下，某个不变量是否始终成立」。

## 2. 为什么 L1 和 L2 共用 host cargo test

两者都跑在 `libs/mango-kernel-core/` 这个纯逻辑库 crate 上。原因：

- `os/` 内核 crate 是 `#![no_std]` 裸机 crate，且配置了**空的 test_runner**，
  无法在 host 上编译运行 `#[test]`。
- 因此任何需要 host 测试的纯逻辑都必须**下沉**到 `libs/mango-kernel-core/`，
  内核通过 path dependency 引用同一份源码，不维护两份副本。
- L2 的并发核心（如 `wait_queue_core`）同样下沉到该 lib crate，生产代码
  `os/src/task/manager.rs` 组合它，host 测试则直接驱动该核心。

## 3. Property-based Testing 是什么

Property-based testing 自动生成输入 / 操作序列，并对每个生成用例断言某个**性质**
（property）成立，而不是断言某个固定期望值。

本 PoC 采用轻量做法：

- 用**确定性 LCG**（linear congruential generator）生成合法 operation 序列；
- 用**多个 schedule seed** 覆盖不同随机交错；
- **不引入** proptest 等大型框架，保持依赖面小、可确定性重放。

确定性 LCG 的意义在于：给定 seed，生成的序列完全可复现，便于失败后重放。

## 4. Systematic Concurrency Testing 是什么

Systematic concurrency testing（SCT）的核心思想是：**由测试框架控制「下一步执行
哪个 worker / actor」**，而不是让真实 OS scheduler 决定。通过枚举可能的交错
（interleaving），系统化地覆盖并发执行顺序，从而发现只在特定交错下才出现的 bug
（如 lost-wakeup）。

与「跑很多次碰运气」的随机压力测试不同，SCT 是**有界穷举**：在给定边界内枚举
所有（或足够多）交错，能给出「在此边界内未发现反例」的更强结论。

## 5. Scheduler 如何控制 Interleaving

本 PoC 使用一个有界 DFS `Explorer` 来枚举交错：

- **choose enabled actor**：在每一步从当前可执行的 actor 集合中选择一个；
- **run step**：执行该 actor 的一步操作；
- **snapshot-restore**：执行后保存 / 恢复状态，以便回溯探索其它分支。

探索受两个参数约束：

- `max_steps`：单条执行路径的最大步数；
- `max_context_switches`：允许的最大上下文切换次数（见第 11 节 Iterative Context
  Bounding 的设计来源）。

探索结果有三种状态：

| 状态 | 含义 |
|------|------|
| `Counterexample` | 找到违反 invariant 的交错 |
| `ExhaustedWithinBounds` | 在边界内穷举完所有交错，未发现反例 |
| `InconclusiveResourceLimit` | 因资源 / 时间限制提前终止，未穷举完 |

## 6. Counterexample 如何生成

当 invariant violation 发生时，测试框架输出一个可读的 counterexample，包含：

- **schedule**：完整的 actor 执行顺序；
- **operations**：每个 actor 执行的具体操作；
- **failing step**：违反 invariant 的那一步；
- **invariant 名**：被违反的不变量名称；
- **状态**：失败时的系统状态快照。

这份输出既是诊断依据，也是重放的输入（见第 7 节）。

## 7. Replay 如何使用

失败后可通过环境变量重放：

```
MANGO_L2_CASE      # 指定要重放的测试用例
MANGO_L2_SEED      # 指定生成操作序列的 seed
MANGO_L2_SCHEDULE  # 指定要重放的 schedule（从失败输出复制）
```

从失败输出复制命令即可重跑，验证反例是否稳定复现。重放还包含 **divergence 检测**：
如果重放过程中实际执行与记录的 schedule 不一致（例如代码已修改导致行为漂移），
框架会报告 divergence，避免把「已修复」误判为「仍失败」或反之。

## 8. 当前 L2 覆盖范围

> **实现状态：实现中。** 本 PoC 正在落地，尚未全部完成。

当前 L2 聚焦 WaitQueue 的纯共享核心（`wait_queue_core`，下沉到
`libs/mango-kernel-core/`），覆盖：

- **token 状态机**：`WAITING / NOTIFIED / CLOSED` 的状态转换合法性；
- **FIFO 注册 / 领取**：waiter 按 FIFO 顺序注册与领取，无丢失 / 重复；
- **finish 精确删除**：waiter 完成时精确从队列删除，不残留陈旧条目；
- **B71 lost-wakeup 协议反例**：验证「producer-before-register」与「register 后
  wake」两类交错下不丢唤醒——这正是
  [2026-08-02 B71 sigtimedwait 睡眠登记窗口闭合](../Work_Log/2026-08-02.md) 修复的
  竞态类别，L2 把它固化为可系统化枚举的协议反例。

## 9. 当前明确不覆盖什么

L2 第一版**明确不做**：

- **不做** KCSAN / DataCollider / data-race sanitizer（动态数据竞争检测）；
- **不做** 弱内存模型验证（如 C11 / Rust 内存模型下的 relaxed/acquire/release 语义）；
- **不覆盖** TCB 状态机、signal、timeout、runqueue 的**真实行为**——这些依赖
  L3 ktest + 代码审查。

L2 不是形式化验证（formal verification），它只做有界、抽象模型下的系统化检验。

## 10. 与 L3 KTest 的关系

| 维度 | L2 | L3 KTest |
|------|----|----------|
| 运行环境 | host（`libs/mango-kernel-core`） | QEMU 内核态 |
| 验证对象 | 抽象协议 / 状态机 / interleaving-sensitive invariant | 真实 kernel / scheduler / task / interrupt / SMP / HAL |
| 交错控制 | 测试框架控制 | 真实调度器 |
| 结论强度 | 有界抽象模型下无反例 | 真实环境行为正确 |

**L2 PASS ≠ real SMP PASS。** L2 验证的是被内核实际引用的 `WaitQueueCore` 握手与
队列算法在抽象调度模型下的一致性；真实 SMP 下的集成正确性仍需 L3 focused QEMU
验证。L2 找到反例后，可把该反例沉淀为 L3 / L4 regression 用例。

## 11. 参考论文（设计参考）

以下论文是本次 L2 设计的**思想启发**。**MangoCore 未实现其完整算法**，只借鉴了
其中与「小型 host-side protocol tester」匹配的部分。

| 论文 | 一句话要点 | 与本次 L2 的关系 |
|------|-----------|------------------|
| **CHESS** — *Finding and Reproducing Heisenbugs in Concurrent Programs*, OSDI 2008 | 通过 scheduler control 系统化探索交错，并支持 deterministic replay 复现 heisenbug | 本次 scheduler controller / schedule recording / replay 的思想来源 |
| **Iterative Context Bounding** — PLDI 2007 | 用 context bounding 缓解 state-space explosion，优先探索少量 preemptive context switch | `max_context_switches` 参数的设计来源 |
| **Landslide** — *A Systematic Testing Framework for Concurrent Systems*, CMU-CS-12-118 | 面向内核的并发测试框架，systematic execution control，未来可扩展 DPOR | 本次只做小型 host-side protocol tester + DFS bounded exploration，未来可扩展 DPOR |
| **Snowboard** — *Finding and Eliminating Concurrency Bugs through Systematic Inter-thread Communication Analysis*, SOSP 2021 | 并发 bug 依赖 input + interleaving 的组合，系统化分析线程间通信 | 未来 Pipe 测试可联合生成 input sequence + interleaving |
| **MOKERT** — *Model-based Kernel Testing through Counter Example Replay* | 基于模型的测试 + counterexample replay，把 failing schedule 固化为 stable replay | 本次 failing schedule / stable replay 直接参考 |
| **KCSAN / DataCollider** — 动态数据竞争检测 | 通过 compiler instrumentation / hardware watchpoint 动态检测 data race | **第一版禁止实现**：本阶段重点是 state-machine invariant / lost wakeup / ownership / 非法并发转换，不是 data-race sanitizer |

## 12. 后续扩展计划

> 以下均为**规划**，不在本次实现范围内。

### Pipe 模型

- 操作：`Read(n)` / `Write(data)` / `CloseReader` / `CloseWriter`；
- 状态：buffer / reader / writer / EOF / blocking / wakeup；
- 参考 Snowboard：联合生成 input sequence + interleaving，验证数据完整性与阻塞语义。

### Scheduler / RunQueue 模型

- 状态：`TaskId` / `CpuId` / `TaskState` / `RunQueue`；
- 不变量：
  - 一个任务至多在一个 RunQueue；
  - `Running` 有唯一 CPU owner；
  - `Zombie` 不入队；
  - `Reclaimed` 不再运行；
- 对应 MangoCore SMP 任务所有权设计（见 `AGENTS.md` 中 B29–B41 的 owner 交接语义）。

## 13. 注意事项 / 边界声明

> **L2 当前是 testing infrastructure，不是 formal verification；它不能证明内核
> 正确性。** 措辞应表述为：**「在有界、抽象调度模型下系统化检验内核实际引用的
> WaitQueueCore 握手与队列算法，并经 focused QEMU 测试验证与真实调度器的集成」**。

- L2 的「通过」只意味着在给定边界（`max_steps` / `max_context_switches` / seed 集合）
  内未发现反例，不构成正确性证明。
- L2 验证的是抽象模型，真实内核的调度器、中断、SMP、HAL 行为必须由 L3 ktest 覆盖。
- 任何声称「L2 已证明内核无并发 bug」的表述都是**过度声明**，应避免。
