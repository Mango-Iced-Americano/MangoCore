# SMP B36 远程 Queued affinity 证据摘要

## 结论

状态：`pass`（stable-Queued-only）。

B36 支持非 current、稳定 `Queued(owner)` 线程修改 affinity。新 mask 保留 owner 时不搬队；
排除 owner 时通过 `Queued(source) -> Migrating -> Queued(target)` 完成唯一 owner 交接。
远程 Running/Blocking 仍为 `EOPNOTSUPP`，普通任务默认 affinity 仍为 bit0。

## 冻结源码

- HEAD：`992e27ee5fd7ba7f65299e60bbc0e18c6501b469`
- source diff SHA-256：`bbd3efd94a1a3f313b06a9d5666df6bca858aea4bb7f64c1f1b7c341dd8e96c0`
- status SHA-256：`ececec76ea5300211f35ee17274ff8f62cac65fb90de8f853515ef582283af05`
- 四项验证的 source-before/source-after 一致，`mutation_detected=false`

## 所有权协议

1. 无锁选择目标并完成目标 kernel-stack TLB 同步。
2. source runqueue 锁内复核状态与成员，提交 `Queued(source) -> Migrating` 并摘除。
3. 释放 source 后，由迁移调用方 Release 发布新 mask。
4. target runqueue 锁内提交 `Migrating -> Queued(target)` 并插入。
5. 释放 target 后才发送 RESCHEDULE。

全程最多持有一把 runqueue；`Migrating` 后不获取 `TASK_MANAGER`、不等待 IPI/TLB ack。
exit/exec remove 可在既有 `TASK_MANAGER -> 单个 RunQueue` 锁序下等待迁移完成，不形成反向依赖。

## DeepSeek 只读审查

| Job | 耗时 | 退出码 | mutation | 用途 |
|---|---:|---:|---|---|
| `smp-b36-queued-affinity-design-review` | 357.611s | 0 | false | 最小状态与锁协议设计 |
| `smp-b36-queued-affinity-final-review` | 384.293s | 0 | false | 完整状态/队列交错审查 |
| `smp-b36-queued-affinity-fix-review` | 366.459s | 0 | false | nice 与退出收尾复审 |

人工采纳 nice 派生计数竞态：更新方读到旧 owner 时，先重算旧队列，再按最新状态重定位。
人工拒绝“释放旧 rq 后双迁移回原 CPU 导致误 panic”的反例，因为实现是在释放旧 rq 之前
读取状态；迁回该 CPU 必须先取得仍被检查方持有的同一锁。

## Docker/QEMU 验证

父任务：`smp-b36-queued-affinity-validation`，总耗时 1011.889s，exit 0。

| Child job | 架构/场景 | 耗时 | 结果 |
|---|---|---:|---|
| `agent-bd87573498b9-r01-rv64-ktest` | RV64，8 核 SMP focused | 134.220s | 23/23 PASS |
| `agent-bd87573498b9-r02-rv64-preliminary` | RV64，8 核 mask=0x003 | 339.372s | 312/314 |
| `agent-bd87573498b9-r03-la64-ktest` | LA64，8 核 SMP focused | 135.588s | 23/23 PASS |
| `agent-bd87573498b9-r04-la64-preliminary` | LA64，8 核 mask=0x003 | 345.572s | 308/314 |

Focused 两架构均确认：

- configured=8、online_mask=0xff；
- 第 12 项 blocked wake、第 13 项 blocked affinity 均 PASS；
- 新第 14 项 `smp::queued_affinity_moves_between_runqueues` PASS；
- 第 22 项用户 affinity probe、第 23 项 terminal STOP 均 PASS；
- 无 panic、fatal trap、timeout 或 forbidden marker。

初赛失败集合未扩大：

- RV64：仅 musl/glibc 两套 busybox `kill 10`；
- LA64：musl/glibc 两套 `test_brk` 各 1/3，及两套 busybox `kill 10`。

DeepSeek prompt、manifest、stdout/stderr 和完整 QEMU 日志只保存在本地忽略的 `cc-codex/`，
不上传 GitHub。
