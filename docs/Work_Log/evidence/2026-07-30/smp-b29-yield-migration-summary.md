# SMP B29 显式 yield 用户迁移证据摘要

## 结论

状态：`pass`。

B29 在不增加新 `TaskStatus`、不同时锁两个 runqueue 的前提下，完成了一个真实用户 TCB
从 CPU0 经 `sched_yield` 安全点迁移到 CPU1 的闭环。双架构最终 8 核 SMP focused 均为
21/21 PASS；同一生产 diff 的双架构 `mask=0x003` 初赛门禁也均 PASS。

本证据不代表普通用户任务默认全核调度、通用 affinity、timer 抢占、work stealing 或
FS/net/driver 多核安全已经完成。

## 被测源码

- worktree：`/home/lzm/projects/MangoCore-smp-integration-20260725`
- branch：`smp`
- HEAD：`5f6c316f3f6323d487c1b91f910aa85f99bb1537`
- 双架构 focused 被测功能源码 diff SHA-256：
  `06c79ee1f886fc04b73f01af8c164a36da1fa506dc3e22a77ea9bcbdf6c63579`
- focused 通过后只修正当前能力边界注释；当前源码 diff SHA-256：
  `442864756c16e6f0663c31d3ce1d893fda67b28e90d9f03802de4b62bd727439`
- 功能修改：
  - `os/src/task/task.rs`
  - `os/src/task/run_queue.rs`
  - `os/src/task/manager.rs`
  - `os/src/kernel_tests/smp.rs`
- 注释同步：
  - `os/src/task/task.rs`
  - `os/src/task/mod.rs`
  - `os/src/task/processor.rs`
  - `os/src/task/manager.rs`
  - `os/src/smp.rs`

后一个哈希相对被测哈希只包含注释修正，不改变机器码，因而没有机械重复 QEMU。

## 生产不变量

1. `migration_target` 只是一项一次性请求，真实 owner 仍是 `sched_state`。
2. 请求只允许 New 任务独占持有者或本地 current Running 任务设置。
3. 目标 kernel stack TLB 同步完成后，才 Release 发布迁移目标。
4. 源 current 在 idle 栈上清空后，才执行 `Running(source) -> Queued(target)`。
5. 转换和目标入队位于同一个目标 runqueue 锁域；一次只持一把队列锁。
6. `RESCHEDULE` IPI 发生在目标队列锁释放后。
7. Blocking/Zombie 不继承未消费的 yield 请求。
8. CPU1 返回用户态时继续由既有 trap-return 刷新 CPU-local 指针、激活 MM/ASID 并追赶
   generation；本节点没有复制第二套 trap 或 MM 路径。

## 首轮 RED

| 架构 | child job | 耗时 | 结果 |
|---|---|---:|---|
| RV64 | `agent-7d860f325f93-r01-rv64-ktest` | 137.832s | 前 19 项 PASS；第 20 项 TLB ack timeout |
| LA64 | `agent-7d860f325f93-r02-la64-ktest` | 139.202s | 与 RV64 同构 |

共同错误：

```text
user TLB shootdown failed: mm=291 generation=2 targets=0x3
Timeout { cpu_id: 0, expected: 1, observed: 0, send_error: None }
```

`synchronize_user_tlb()` 的等待集合排除了发起 CPU，因此 missing CPU0 反证发起者是 CPU1。
这说明迁移已经执行；CPU1 退出时对已在 CPU0/CPU1 激活的 MM 发起失效，而 CPU0 runner
关中断自旋，无法 ack。修正是让等待循环进入既有受控中断窗口，而不是更改 TLB 协议。

## 最终 focused

| 架构 | child job | 耗时 | exit | online | TAP | 新用例 |
|---|---|---:|---:|---:|---:|---|
| RV64 | `agent-cb48c95fc981-r01-rv64-ktest` | 136.205s | 0 | `0xff` | 21/21 | PASS |
| LA64 | `agent-cb48c95fc981-r02-la64-ktest` | 135.708s | 0 | `0xff` | 21/21 | PASS |

两份日志都明确包含：

```text
ok 20 smp::user_task_migrates_on_yield
[KTEST RESULT: PASS]
```

没有 panic、fatal trap、TLB timeout、owner invariant、missing marker 或 source mutation。

## 初赛非回归

| 架构 | child job | 耗时 | exit | judge | 硬条件 |
|---|---|---:|---:|---:|---|
| RV64 | `agent-7d860f325f93-r03-rv64-preliminary` | 334.726s | 0 | 312/314 | PASS |
| LA64 | `agent-7d860f325f93-r04-la64-preliminary` | 343.227s | 0 | 308/314 | PASS |

两架构均为 `mask=0x003`、`online_mask=0xff`，四个 basic/busybox END 完整，无 forbidden
marker；失败集合未相对 B28 接受基线扩大或换位。最终修改仅位于 ktest 等待窗口，因此没有
机械重复 preliminary。

## DeepSeek 与人工裁决

- 冻结 patch 的只读审查无阻断发现；采纳内存序收紧与文档说明，拒绝跳过目标内核栈 TLB
  同步的性能建议。
- 首轮测试报告把 panic 误判为构造期。GPT/Codex 依据“missing CPU 是远端目标”与
  `targets=0x3` 修正为 CPU1 退出等待 CPU0；最终 PASS 证明该因果链。
- 一个最终复验 job 因模型未执行预授权命令而被 wrapper fail-closed；后续 job 实际完成
  两架构。模型文本不是 PASS 证据，child result、TAP marker、退出码和源码 hash 才是。

原始 prompt、模型输出、stdout/stderr 和 runner manifest 位于本地忽略的 `cc-codex/`，
按协作约束不进入 GitHub。
