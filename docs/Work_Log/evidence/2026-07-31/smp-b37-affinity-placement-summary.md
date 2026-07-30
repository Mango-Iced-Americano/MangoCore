# SMP B37 affinity-aware 初始放置证据摘要

- 日期：2026-07-31
- 状态：`pass`
- 被测 HEAD：`85526b978994f4ab31191290e7b914fec69e52a6`
- 功能 diff SHA-256：`af6397f9af924b2994a1cb59d8e7ce480b3cd9bb55d9c1f2e96ecf1a2bcd26f3`
- Docker：`mangocore-smp-integration-20260725-os-dev-1`
- 配置：RV64/LA64，`CORE_NUM=8`，focused 为 `KTEST=smp KREPEAT=1`

## 结果

| Child job | Recipe | 用时 | 结果 |
|---|---|---:|---|
| `agent-f3009647d44a-r01-rv64-kernel-build` | RV64 kernel build | 133.645s | PASS |
| `agent-f3009647d44a-r02-la64-kernel-build` | LA64 kernel build | 134.758s | PASS |
| `agent-f3009647d44a-r03-rv64-ktest` | RV64 SMP focused | 141.275s | 23/23 PASS |
| `agent-f3009647d44a-r04-la64-ktest` | LA64 SMP focused | 138.218s | 23/23 PASS |
| `agent-81714705800a-r01-rv64-preliminary` | RV64 basic+busybox | 335.002s | 312/314 |
| `agent-81714705800a-r02-la64-preliminary` | LA64 basic+busybox | 346.247s | 308/314 |

RV64 focused 启动记录为 `configured=8`、`online_mask=0xff`，本轮 OpenSBI boot hart 为 3；
LA64 为 boot CPU 0、`online_mask=0xff`。双架构第 11 项
`remote_kernel_tasks_run_on_target_cpus` 证明单 bit mask 经通用 `publish_task()` 仍精确到达
AP，第 2 项证明启动期 runner 留在 CPU0，第 23 项 STOP 正常结束。

初赛失败集合与既有基线相同：RV64 只有两套 busybox `kill 10`；LA64 只有两套
`test_brk` 各 1/3 和两套 busybox `kill 10`。所有 child 的 process exit code 为 0，
`forbidden_markers_found=[]`、`mutation_detected=false`，源码前后指纹一致。

DeepSeek 的原始 prompt、stdout、analysis、manifest 与完整 QEMU 日志只保存在本地忽略的
`cc-codex/runtime/jobs/`，不上传 GitHub。本摘要只归档人工核对后的稳定事实。
