# B91 Per-CPU 调度生产诊断证据

## 变更边界

- `CpuTaskState` 新增 `context_switches`、`migrations`、`steals`、`run_queue_peak`。
- 两个真实 `__switch` 方向各计一次 context switch。
- `last_cpu.swap()` 发生在唯一 owner 已取得 `Running(cpu)` 后；首次运行不算迁移，
  queued affinity 搬运不提前计数。
- steal 仅在 `Migrating -> Running(thief)` 成功后计数。
- runqueue peak 沿用 `nr_running` 的“不含 current”口径。
- 所有字段仅用于 best-effort 诊断，不参与调度决策，不新增锁和堆分配。

## DeepSeek 交叉审查

`smp-b91-core-gap-audit` 在当前 task/mm/hal/smp 范围未发现新的 P0/P1 正确性缺口；
确认整页 `'static mut` byte view 需要与 PageCache/FS 共同处理，默认全核 affinity 仍受
共享子系统门禁约束。

`smp-b91-scheduler-diagnostics-review` 对冻结源码 diff 检查 owner、内存序、记录点和命名，
未发现阻塞项；唯一建议是让 peak helper 自行读取长度的风格选择，当前唯一调用方直接传入
同一次 `fetch_add + 1` 的精确结果更清楚，故不采纳。

首次 `smp-b91-scheduler-diagnostics-validation` 错用 read-only profile，四项测试均为
NOT RUN。该报告只作为失败流程证据保留，不参与 PASS 判定。

## Docker 冻结验证

正确的 `smp-b91-scheduler-diagnostics-validation-r2` 使用 `agent-docker-validation`，四项
recipe 均设为 required，严格串行：

| 子任务 | 配方 | 结果 | 耗时 |
|---|---|---:|---:|
| `agent-e1abcfde4ece-r01-rv64-kernel-build` | RV64 normal build | PASS, exit 0 | 137 s |
| `agent-e1abcfde4ece-r02-la64-kernel-build` | LA64 normal build | PASS, exit 0 | 138 s |
| `agent-e1abcfde4ece-r03-rv64-ktest` | RV64, 8 CPU, SMP | PASS, 34/34 | 137 s |
| `agent-e1abcfde4ece-r04-la64-ktest` | LA64, 8 CPU, SMP | PASS, 34/34 | 141 s |

四项均无 forbidden marker，源码前后 HEAD、status 和 tracked diff 指纹一致，
`mutation_detected=false`。两架构在真实远端 PTE 用例中各出现一次预期用户态 fault，随后
对应用例与 suite 均 PASS；不能误判成内核 panic。

## 裁决

B91 可以提交。它完成 Phase 6 调度侧 per-CPU 生产诊断，不改变任务状态机、队列归属、
放置策略或默认 affinity。IPI/TLB 等其它诊断统计保持为后续独立节点。
