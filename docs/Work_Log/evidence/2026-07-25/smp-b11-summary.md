# SMP-P2-B11 确定性验证摘要

## 验证对象

- 基线提交：`1baf4932 feat(smp): broadcast IPI reasons to online APs`
- 验证时的内核源码差异 SHA-256：
  `b78d41e003d047bc5076927f8a8c038f72d0bcb3c54ac897d4687a5df643a12b`
- 环境：项目 Docker 开发容器，仓库绑定到 `/app`
- 拓扑：`CORE_NUM=2`
- 测试集：`KTEST=smp`，`KREPEAT=1`

下表四项 PASS 的 source-before/source-after 指纹一致。最初一次 RV64 build
期间发现并修正了 hard IRQ 诊断打印旁路，因此该次退出 0 的产物主动废弃，
最终 RV64 build 在冻结源码上重新执行。lint 生成的未跟踪用户工具 stub 已
可恢复地移出工作树，没有进入源码差异或本摘要。

验证后只按 rustfmt 建议调整四处 import/export/条件换行，当前内核源码差异
SHA-256 为
`66118e70ce5f0aa77249e19d0a85c8857ba1066c187c1ad23cf6672413ce4952`。
机械换行不改变编译产物，因此没有重复构建或 QEMU。changed-file rustfmt
仍会报告同一批文件中本包之外的既有格式差异，本次没有格式化整文件。

## 串行执行

```text
cd /app && make kernel ARCH=rv64 PROFILE=normal CORE_NUM=2
cd /app && make kernel ARCH=la64 PROFILE=normal CORE_NUM=2
cd /app && make ktest ARCH=rv64 PROFILE=normal CORE_NUM=2 KTEST=smp KREPEAT=1
cd /app && make ktest ARCH=la64 PROFILE=normal CORE_NUM=2 KTEST=smp KREPEAT=1
cd /app && make lint
```

双架构构建和 QEMU 严格串行，没有并行切换共享工具链。

## 结果

| 门禁 | 退出码 | 用时 | 关键结果 | 结论 |
|---|---:|---:|---|---|
| RV64 kernel | 0 | 125.040 s | 最终源码指纹稳定 | PASS |
| LA64 kernel | 0 | 129.314 s | 最终源码指纹稳定 | PASS |
| RV64 SMP ktest | 0 | 127.468 s | `online_mask=0x3`、6/6 | PASS |
| LA64 SMP ktest | 0 | 130.109 s | `online_mask=0x3`、6/6 | PASS |
| lint | 2 | 14.230 s | warning baseline/parser 差异 | FAIL |

两个架构的 focused QEMU 均通过：

```text
ok 1 smp::configured_cpus_are_online
ok 2 smp::legacy_scheduler_stays_on_boot_cpu
ok 3 smp::secondary_cpus_enter_idle_context
ok 4 smp::bsp_to_ap_ipi_ping
ok 5 smp::bsp_broadcasts_ipi_to_all_aps
ok 6 smp::kernel_timer_irq_is_deferred
# results: 6 passed, 0 failed, 6 total
[KTEST RESULT: PASS]
```

第六项连续验证两轮真实内核 timer interrupt。每轮 hard IRQ 返回后，
`timer_irq_count` 已增加，但 deferred count 未增加、pending 为 true、
当前 TID 未变化；显式安全点返回后 pending 被清除且 deferred count 恰好
增加一。第二轮成功到达同时证明第一轮已经重新编程 one-shot。

lint 仍把 `src/drivers/rng/mod.rs`、`src/fs/ext4/bitmap.rs` 和
`src/smp.rs` 的既有 warning 解析为未覆盖的 `unknown`，现有 baseline
也不匹配。本次没有修改 lint 工具或采集新 baseline。

本证据只验收 CPU0 timer hard/deferred 分界、双架构硬件静默和安全点
重编程；不外推为 CPU0 已接收 IPI、AP timer、长 syscall 中断窗口、
STOP、RESCHEDULE、调度或 TLB shootdown 已完成。
