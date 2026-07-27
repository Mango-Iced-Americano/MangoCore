# SMP-P2.5-B15 验证摘要

- 日期：2026-07-27
- 基线 HEAD：`4dbae121cb35502a660631c0ae545ececcfd5861`
- 可执行源码差异 SHA-256：
  `f5de06284a7f3bab618da6669358ee7257f1506a46093afbc5bb44b1fdbe7ef7`
- Docker 容器：`mangocore-smp-integration-20260725-os-dev-1`
- 工具链：`nightly-2026-05-10`

## 只读审查

- 最终冻结审查：`ACCEPT WITH CHANGES`
- 进程退出码：0
- 超时：否
- 审查期间源码变更：否
- 结论：未发现调度所有权或丢唤醒 Bug；采纳 `#[must_use]` 与 ktest 生命周期注释。
- 已知债务：nice-aware ready 选择仍在全局队列锁内读取 `task.inner`，Phase 3 收口。

## Docker 结果

| 架构/recipe | 参数 | 退出码 | 用时 | 结果 |
|---|---|---:|---:|---|
| RV64 kernel build | `CORE_NUM=4 PROFILE=normal` | 0 | 136.666s | PASS |
| LA64 kernel build | `CORE_NUM=4 PROFILE=normal` | 0 | 138.537s | PASS |
| RV64 SMP ktest | `CORE_NUM=4 KTEST=smp KREPEAT=2` | 0 | 140.055s | 19/19 PASS |
| LA64 SMP ktest | `CORE_NUM=4 KTEST=smp KREPEAT=2` | 0 | 129.526s | 19/19 PASS |
| RV64 WaitQueue ktest | `CORE_NUM=4 KTEST=waitqueue KREPEAT=1` | 0 | 138.692s | 4/4 PASS |

两架构 SMP 日志均包含：

- `online_mask=0xf`
- `smp::scheduler_state_has_unique_owner` 第 9、18 项 PASS
- `smp::secondary_cpus_stop_and_ack` 仅第 19 项执行
- `19 passed, 0 failed`
- `[KTEST RESULT: PASS]`

全部真实 recipe 的 source-before/source-after 一致，无 panic、timeout 或 forbidden
marker。一次网关子任务 ID 超长错误发生在 Docker recipe 提交前，未运行任何内核命令；
本地网关已改用父任务 ID 的短摘要，后续五项真实验证均通过。

## 证据边界

本摘要证明 CPU0 legacy scheduler 与 4 核 SMP 服务循环在双架构下共存，以及本次
Blocking/Blocked、switch-out 和 zombie handoff 的 focused 生产 API 路径。它不证明
AP 调度普通任务、per-CPU runqueue、远程 enqueue、迁移或跨核 MM/TLB 正确性。
