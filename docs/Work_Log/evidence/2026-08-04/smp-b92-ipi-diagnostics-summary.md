# B92 Per-CPU IPI 生产诊断证据摘要

## 变更范围

- 基线：`b5df485a9adb95cc8960c09247f45e5f2a259ef4`
- 生产代码：`os/src/smp.rs`、`os/src/panic_diag.rs`
- 文档：启动/trap 架构、8 核 SMP 实施计划、Work Log 与 AI 使用披露
- 未修改：架构 doorbell HAL、trap 汇编、ack sequence、调度状态机、MM、FS、Net、Driver

## 设计不变量

1. mailbox 仍先以 Release 发布 reason，再触发硬件 doorbell。
2. handler 仍以 Acquire 一次消费 mailbox，再按原协议发布各自 ack。
3. 新计数全部为 Relaxed，只用于事后快照，不建立正确性 happens-before。
4. `published` 按目标 CPU 数累计；`consumed` 按实际取得的 bit 累计。同类 bit 在 handler
   运行前可以合并，因此二者差值不是丢中断证据。
5. doorbell 失败统一在 `send_ipi_mask()` 对每个失败目标计一次；已发布 mailbox 不回滚。
6. hard IRQ 新增工作只有固定上界位扫描和原子加法，无分配、普通锁、打印或调度。
7. 未登记名字的高位不会触发诊断 panic，避免可观测性代码破坏生产 IPI 路径。

## DeepSeek 审查与 GPT 裁决

- 设计和补丁审查均未发现 P0/P1。
- 采纳：发起侧 publication、接收侧 consumption、handler entry 和统一失败记录点。
- 修正：`ipi_send_failures` 是纯诊断，读写统一使用 Relaxed，不保留误导性的
  Release/Acquire。
- 拒绝：另增 `ipi_doorbell_failures`，因为它与既有字段重复。
- 拒绝：以发布/消费总数近似相等作为正确性条件；同类 mailbox 位允许合并。
- 拒绝：为纯诊断改动机械追加 preliminary `mask=0x003`；双架构 8 核 SMP focused 已直接
  覆盖 mailbox、round-trip、STOP、reschedule、TLB 与 membarrier。

## 冻结验证

采纳 job：`smp-b92-ipi-diagnostics-validation-v2`

| Child job | Recipe | 结果 | 耗时 |
|---|---|---:|---:|
| `agent-bbd8521db83d-r01-rv64-kernel-build` | RV64 normal build | PASS | 134.279 s |
| `agent-bbd8521db83d-r02-la64-kernel-build` | LA64 normal build | PASS | 149.400 s |
| `agent-bbd8521db83d-r03-rv64-ktest` | RV64 `CORE_NUM=8 KTEST=smp` | 34/34 PASS | 140.890 s |
| `agent-bbd8521db83d-r04-la64-ktest` | LA64 `CORE_NUM=8 KTEST=smp` | 34/34 PASS | 150.643 s |

四项均满足：

- `process_exit_code=0`
- `mutation_detected=false`
- `timed_out=false`
- `forbidden_markers_found=[]`
- 无 panic、fatal trap、重复 owner、IPI/TLB failure

用户 `StorePageFault` / `PageModifyFault` 是 `remote_user_pte_updates_take_effect` 的预期
权限/CoW 场景，测试本身通过，不属于内核失败。

## 废弃证据与边界

首轮 job `smp-b92-ipi-diagnostics-validation` 的 RV64 build exit 0，但执行期间 GPT 又写入
架构文档，child `agent-566e7811a135-r01-rv64-kernel-build` 因
`mutation_detected=true` 正确标记 FAIL。该轮被主动中止，任何结果均未用于验收。

本节点没有为计数器新增专用 ktest 或生产 hook；动态数值的语义通过真实 IPI/TLB/调度
focused 路径和静态记录点共同证明。普通用户默认 affinity 仍为 CPU0-only，等待队友完成
FS/Net/Driver 共享子系统后再解除。
