# SMP B20 远程 blocked wake 验证摘要

## 受测边界

- 基线 HEAD：`3a5bb9ead418fda07d542b957772cf5f868afaa3`（B19）。
- B20 为该 HEAD 上的未提交 tracked diff；本地 `cc-codex` 请求、模型输出和原始日志不上传。
- Docker 容器：`mangocore-smp-integration-20260725-os-dev-1`。
- 四项验证使用相同源码指纹：
  - `status_sha256=c277d7876159c1247b83302b28104c53a93fdc88ba193f559e715aaeed6a71ab`
  - `tracked_diff_sha256=b71943a1c1f8907a693cf416ce3840a8c37a31958e0f57ae3b14f4842a1935eb`
- 每项 before/after 一致，`mutation_detected=false`。

## 实现语义

B20 不新增任务状态。成功 fetch 后记录不参与 owner 判定的 `last_cpu`；真正 `Blocked`
的任务优先回到仍 online、scheduler-entered 且未 STOP 的最近 CPU。状态 CAS、registry
移除和 runqueue 插入在 `TASK_MANAGER -> 单个 RunQueue` 下提交；单个/批量 wake 只返回
目标或聚合 mask，全部调度锁释放后才发送 `RESCHEDULE`。

focused 用例让 CPU1..7 的任务等待同一个 Completion。CPU0 先确认所有任务均已切离
current、状态为 `Blocked` 且目标 runqueue 为空，再一次 `complete()`；恢复后每个任务
必须仍在原 CPU 以 `Running(cpu)` 被 current 唯一拥有，并最终进入 Zombie。

## Docker 串行验证

| Job | 配置 | 结果 | 用时 |
|---|---|---|---:|
| `smp-b20-rv64-build-r1-20260728` | RV64 normal，`CORE_NUM=8` | PASS，exit 0 | 125.712 s |
| `smp-b20-la64-build-r1-20260728` | LA64 normal，`CORE_NUM=8` | PASS，exit 0 | 137.188 s |
| `smp-b20-rv64-ktest-r1-20260728` | RV64，`CORE_NUM=8 KTEST=smp KREPEAT=2` | 25/25 PASS | 135.598 s |
| `smp-b20-la64-ktest-r1-20260728` | LA64，`CORE_NUM=8 KTEST=smp KREPEAT=2` | 25/25 PASS | 136.400 s |

两架构的新用例均在编号 12 和 24 通过，terminal STOP 为编号 25 并通过；online mask
均为 `0xff`。无 panic、timeout、forbidden marker 或 required marker 缺失。

## DeepSeek 协作裁决

前置只读设计 job 与 GPT/Codex 实现并行，因 worktree 指纹变化被包装器正确拒绝为正式
证据；报告内容仅作为建议，由 GPT/Codex 逐项复核。机械验证改由 DeepSeek 按自然语言
驱动 allowlist Docker runner，四个 child job 严格串行并独立读取 result/log 后汇总。

采纳显式 Release/Acquire `last_cpu` 和排除 STOP CPU；拒绝把 `&mut WaitQueue` 当作外围锁
已释放的证据，也未为观察瞬时 Queued 增加生产 test-only 字段。

最终完整 diff 的冻结只读审查 `smp-b20-final-review-20260728` 为
`ACCEPT_WITH_BOUNDARIES`、无 P0，耗时 299.778 秒，源码 before/after 指纹一致且
`mutation_detected=false`。报告确认 guard 析构点、batch 单 runqueue、Blocking 竞态和
真实 Completion 调用链；仅把 STOP 与 wake 的竞态登记为未来 CPU hotplug 边界。

## 尚未证明

- 普通用户任务跨 CPU、affinity、migration、steal 或负载目标选择；
- 用户 MM active mask/range shootdown、LoongArch MM-owned ASID；
- AP 使用过的动态 kernel stack 的全局 unmap shootdown、延迟 frame/VA 复用；
- FS、NET、驱动、console 与用户 syscall 在 AP 上并发执行；
- CPU 热插拔、故障下线和非协作式 stop。
