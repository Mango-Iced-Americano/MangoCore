# SMP B24 RV64 页级 RFENCE 证据摘要

## 1. 范围

- 分支：`smp`
- 基线 HEAD：`83c7c6c620ab1adb7d6b1d63ef2ee625beecd157`
- 状态：`pass` — 双架构 build、初稿 focused 与非平凡逻辑 CPU 子集复测均通过
- 本地 `cc-codex/` 任务、模型输出和原始日志被 Git 忽略，不进入 GitHub。

## 2. 生产调用链

```text
MmuGather::seal
  -> TlbFlush::execute
  -> synchronize_user_tlb(targets, page)
       Page + RV64 RFENCE available
         -> logical CPU mask -> physical hart mask
         -> SBI REMOTE_SFENCE_VMA(start, PAGE_SIZE)
       Full / LA64 / RFENCE unavailable
         -> USER_TLB_SYNC request -> full local invalidate -> ack
  -> TlbContext::acknowledge
  -> release retired frames
```

没有新增 MM 提交对象或共享 range slot。RFENCE 路径由 OpenSBI 自己同步；software IPI
fallback 仍只传递幂等 reason 和 sequence，因此不同 MM 的请求合并仍由全量失效覆盖。

## 3. 规范与上游依据

- RISC-V SBI RFENCE：EID `0x52464e43`、Remote SFENCE.VMA FID `1`，参数为
  `hart_mask, hart_mask_base, start_addr, size`；`start=size=0` 表示全量，本批只传单页范围。
- Linux RISC-V 在无远端目标时本地失效，有远端且 RFENCE 可用时走 SBI，否则用同步 IPI；
  MangoCore 采用相同后端选择，但未引入当前不需要的 ASID/range threshold 层。
- DragonOS 同样让 `MmuGather` 保留范围和待释放页面，把 shootdown 放在独立 TLB 层；
  MangoCore 保留已经收敛的 `record_change -> seal -> execute` 命名。

## 4. 初稿验证与人工裁决

DeepSeek job `smp-b24-rfence-validation-20260729` 自主选择并完成：

| 子任务 | 结果 | 用时/关键事实 |
|---|---|---|
| RV64 normal build | PASS | exit 0，源码指纹不变 |
| LA64 normal build | PASS | exit 0，源码指纹不变 |
| RV64 8 核 SMP focused | PASS 17/17 | RFENCE enabled，boot hart=4，页级/退休用例通过 |
| LA64 8 核 SMP focused | PASS 17/17 | 页级 IPI fallback、退休用例通过 |

模型把 boot hart=4 与全 8 核页级测试合并解读为“已动态证明逆映射”，该结论证据不足：
全量逻辑/物理 mask 都是 `0xff`。人工裁决保留四项真实 PASS，但把最终测试目标改为逻辑
CPU0/1 子集；修改后的结果必须单独冻结，不能沿用上表。

## 5. 最终冻结验证

- DeepSeek job：`smp-b24-rfence-final-20260729`

| 子任务 | 结果 | 用时 | 关键事实 |
|---|---|---:|---|
| `agent-9bb38a3cde12-r01-rv64-ktest` | PASS 17/17 | 135.685 s | boot hart=5，RFENCE enabled，逻辑 `0b11` 映射物理 `0b100001`，页级用例未增加 software request |
| `agent-9bb38a3cde12-r02-la64-ktest` | PASS 17/17 | 143.818 s | 页级用例增加 request/ack，使用全量 IPI fallback |

两项均为 `CORE_NUM=8 KTEST=smp KREPEAT=1`，`configured=8`、`online_mask=0xff`；进程
exit 0，无 timeout、panic、forbidden marker 或缺失完成标记。测试前后均为：

- HEAD：`83c7c6c620ab1adb7d6b1d63ef2ee625beecd157`
- status SHA-256：`c5bf7c292b9c0a071c8883f6dbdaf27630535caaba5beea57e7cb14e11203874`
- tracked diff SHA-256：`65fcf24b8130dab0eee3aa3812dca10e2efee6223ac329ef274225f60561f309`
- untracked content SHA-256：`8c8b130afb1cd00a8ddabea8df6181ff065c5a9edc56aa8659d808513db35d5e`
- `mutation_detected=false`

两架构的页级后端断言与双页 `Full` 退休窗口均通过。最终测试后只把上述真实结果写入
Work Log/evidence，未再修改生产源码或测试逻辑。

## 6. 未覆盖范围

- 连续多页 range 与 RFENCE all 策略；
- LoongArch MM-owned ASID、精确 `invtlb` 和固定 shootdown slot；
- 用户 victim 无 trap 的 stale-translation 硬件窗口；
- 普通用户任务跨 CPU、cached CPU detach 与高频 CoW/mprotect 性能数据。
