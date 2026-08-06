# B93 Per-CPU TLB shootdown 生产诊断证据摘要

## 变更范围

- 基线：`48f45a07`
- 生产代码：`os/src/smp.rs`、`os/src/panic_diag.rs`
- 文档：页表/TLB 架构、8 核 SMP 实施计划、Work Log 与 AI 使用披露
- 未修改：`MmuGather`、`TlbFlush`、页表写入、ASID 分配、HAL TLB 指令、trap handler、
  FS、Net、Driver

## 统计口径

每轮真正含远端 CPU 的同步只进入一个互斥类型：

1. `kernel_full`：kernel-global 全量同步；
2. `user_full`：调用方原本要求全用户失效；
3. `range_fw`：架构固件接受并执行精准区间，当前主要是 RV64 SBI RFENCE；
4. `range_ipi`：固定槽携带 ASID/VPN 区间并通过软件 IPI/ack 完成；
5. `range_fallback`：精准请求因发起 CPU 固定槽被占用而退化为全用户同步。

辅助值：

- `range_pages` 只累计真正进入精准后端的区间长度之和，不乘目标 CPU 数；
- `remote_targets` 累计尝试覆盖的远端 CPU 数，固件/同步失败时仍记录计划 fanout；
- `sync_ticks_total/max` 从选择远端后端前计到成功或错误返回，使用架构 raw timer ticks；
- `failures` 只统计最终同步错误；doorbell 单点失败由 B92 IPI 诊断单独统计。

本地-only、无 live target 和参数校验失败均不算 shootdown。全部计数使用 Relaxed，
不参与 request/ack、generation、ASID 或 frame 退休同步。

## 所有权与中断边界

- `MmuGather` 仍在 VM 锁内冻结范围与退休 frame，`TlbFlush` 仍是唯一持有者。
- 诊断记录位于同步函数返回前，但不借用、移动或释放 frame；错误返回后上层仍执行原有
  `leak_retired_frames + panic`。
- slot timeout 仍故意不释放槽，避免迟到 doorbell/ack 错配；该行为不是诊断补丁引入。
- `handle_ipi()`、`UserTlbRangeSlot::service()` 和 HAL 失效函数均未修改；hard IRQ 中没有
  新增计时、锁、分配、打印或 TLB 指令。

## DeepSeek 审查与 GPT 裁决

- clean-tree 设计审查和补丁审查均完成；未发现 P0 或协议级 P1。
- 采纳：发起侧归属、Relaxed、实际后端分类、raw ticks、panic 快照。
- GPT 补充：精准页数和远端 fanout 都是性能解释的必要维度，不按模型建议省略。
- GPT 补充：RFENCE 虽在 SBI 内部执行，但调用前后 raw time 可观察端到端耗时。
- 拒绝：per-target handler 字段、ASID rollover 重复字段、trace buffer、直方图和专用
  生产测试 hook；现有 request/ack 与 B92 reason 计数足够交叉定位。
- 维护性修正：range 页数只校验/计算一次；panic 类型与成本拆为两行。

## 冻结验证

采纳 job：`smp-b93-tlb-diagnostics-validation`

| Child job | Recipe | 结果 | 耗时 |
|---|---|---:|---:|
| `agent-d811302755f5-r01-rv64-kernel-build` | RV64 normal build | PASS | 135.8 s |
| `agent-d811302755f5-r02-la64-kernel-build` | LA64 normal build | PASS | 147.9 s |
| `agent-d811302755f5-r03-rv64-ktest` | RV64 `CORE_NUM=8 KTEST=smp` | 34/34 PASS | 138.4 s |
| `agent-d811302755f5-r04-la64-ktest` | LA64 `CORE_NUM=8 KTEST=smp` | 34/34 PASS | 148.4 s |

四项均满足：

- `process_exit_code=0`
- `mutation_detected=false`
- `timed_out=false`
- `forbidden_markers_found=[]`
- 无 panic、stale TLB、active-MM/generation/slot 残留或 frame 提前释放标记

focused #20—#28 覆盖全用户同步、双架构 ASID、精准区间、真实 CoW/remap/mprotect、
并发 PTE writer、ack 前 frame 保留、membarrier 和 kernel stack shootdown。测试中预期的
RV64 `StorePageFault` 与 LA64 `PageModifyFault` 均发生在通过的真实权限/CoW 场景，不是
内核失败。

## 明确边界

- 不新增专用计数 ktest 或生产 hook；计数记录点由静态审查，协议非回归由真实 focused
  路径证明。
- 不运行 preliminary `mask=0x003`：本节点不改变用户 ABI 或共享 I/O，双架构 TLB focused
  是更直接的最小充分门禁。
- 普通用户 affinity 仍保持 CPU0-only，等待队友完成 FS/Net/Driver 共享子系统后再解除。
