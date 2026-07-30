# SMP B38 远程 Running/Blocking affinity 证据摘要

## 结论

- 状态：`pass`
- 被测基线 HEAD：`4da0579a90f15dbf2a2a914b246e3c011d31ba00`
- 被测功能 diff SHA-256：
  `4875482b6e06f089eb1c3060a6c20259c902a66a6e44c19ca746c6b42c44b465`
- 测试前后源码指纹一致；DeepSeek/runner 未修改 tracked source。
- 双架构 8 核 kernel build 通过，SMP focused 均为 24/24，初赛失败集合未扩大。

## 实现证据

- TCB 只增加一个 `remote_affinity_request` 会合槽，没有增加 `TaskStatus`。
- 请求固定保存 mask、target 与 `Pending/Applied/Retry`；调度 owner 仍只由
  `sched_state + current/runqueue` 表达。
- 远程 Running 请求排除 owner 时，先锁外同步目标内核栈，再锁内复核并发布请求，锁外发送
  RESCHEDULE。调用者协作式 yield，直到 owner 报告 Applied 或 Retry。
- owner 在 idle 栈上的 `finish_switch_out()` 持请求槽，并只取得一个目标 RunQueue，完成
  `Running(source) -> Queued(target)` 后才标记 Applied。
- `begin_interruptible_sleep()` 与 `mark_zombie()` 会在各自既有锁域内取消未消费请求，使调用者
  重试稳定状态；不在 IPI handler 中切换任务或获取普通锁。

## Docker 构建与 focused QEMU

冻结验证任务：`smp-b38-running-affinity-validation-002`

| 顺序 | child | 配置 | 用时 | 结果 |
|---:|---|---|---:|---|
| 1 | `agent-ed183d6956d9-r01-rv64-kernel-build` | RV64, `CORE_NUM=8` kernel build | 135.158 s | exit 0 |
| 2 | `agent-ed183d6956d9-r02-la64-kernel-build` | LA64, `CORE_NUM=8` kernel build | 137.229 s | exit 0 |
| 3 | `agent-ed183d6956d9-r03-rv64-ktest` | RV64, `CORE_NUM=8 KTEST=smp KREPEAT=1` | 135.585 s | 24/24 |
| 4 | `agent-ed183d6956d9-r04-la64-ktest` | LA64, `CORE_NUM=8 KTEST=smp KREPEAT=1` | 138.235 s | 24/24 |

两架构 raw TAP 均包含 `1..24`、第 15 项
`smp::running_affinity_waits_for_owner_handoff`、第 24 项 terminal STOP 和最终
`24 passed, 0 failed`。RV64 启动 hart 为 3、LA64 BSP 为 CPU0；两者 configured 均为 8，
online mask 均为 `0xff`。运行日志无 panic、timeout 或 forbidden marker。

首轮任务 `smp-b38-running-affinity-validation-001` 在 RV64 build 暴露 sibling 私有类型导入
错误；该轮是有效 RED，不计为通过。修正为 `manager.rs` 直接从 sibling `task` 模块导入后，
才冻结 diff 并执行上述完整串行矩阵。

## 初赛 basic + busybox

冻结验证任务：`smp-b38-preliminary-validation`

| 架构 | child | 用时 | 结果 | 接受失败集合 |
|---|---|---:|---:|---|
| RV64 | `agent-069c98e34d79-r01-rv64-preliminary` | 331.651 s | 312/314 | 两套 busybox `kill 10` |
| LA64 | `agent-069c98e34d79-r02-la64-preliminary` | 347.327 s | 308/314 | 两套 basic `test_brk` 各 1/3；两套 busybox `kill 10` |

两项 child 均 exit 0、四组 START/END 完整、无源码 mutation。fork/clone/exec 与其余 busybox
项目没有新增失败，结果等于人工接受基线。

## 人工裁决与未覆盖边界

- DeepSeek 最终报告称请求槽在取得目标 runqueue 前已经释放；源码实际是 owner 持请求槽跨过
  单个目标 runqueue 临界区。本文按源码修正，锁序文档同步记录
  `remote_affinity_request -> single RunQueue`。
- DeepSeek 一处 TAP 汇总低报数量；原始日志明确为双架构 24/24，按 raw TAP 裁决。
- 动态用例覆盖单请求者、Running owner、mask 包含/排除 owner、迁移后唯一恢复与清理。
- 并发两个远程写者，以及确定性命中 `Running -> Blocking` 的状态交界没有做动态压力测试；
  当前只由单请求槽、Retry 协议和锁序审计约束，不能把 24/24 描述为穷举并发证明。
- 普通用户任务默认 affinity 仍为 bit0，本工作包不代表默认全核调度已开放。

## 本地协作边界

DeepSeek prompt、manifest、stdout/stderr 与原始 Docker/QEMU 日志位于忽略的本地
`cc-codex/`，不上传 GitHub。本摘要只归档可公开复核的配置、指纹、结果和人工裁决。
