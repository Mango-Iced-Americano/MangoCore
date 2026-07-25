# SMP-P2-B10 确定性验证摘要

## 验证对象

- 基线提交：`9f4cc7f0 feat(smp): add dual-architecture IPI ping path`
- 验证时的内核源码差异 SHA-256：
  `daa553e07985d55639544757c99c7ec44ac4564fa622118bf7eaa105bb5adc77`
- 环境：项目 Docker 开发容器，仓库绑定到 `/app`
- 拓扑：`CORE_NUM=4`
- 测试集：`KTEST=smp`，`KREPEAT=1`

验证前后的已跟踪源码差异指纹一致。`make lint` 生成了未跟踪的用户工具
stub；这些文件已移出工作树，没有进入源码差异或本摘要。
验证后只校正了一处广播批次边界的中文注释，当前两份内核源码差异 SHA-256
为 `2e05cbad283973336850ceb2924e67174d509c5dbda38be6b5634c2f220ed98c`；
该改动不进入编译产物，因此没有重复执行构建或 QEMU。

## 串行执行

```text
cd /app && make kernel ARCH=rv64 PROFILE=normal CORE_NUM=4
cd /app && make kernel ARCH=la64 PROFILE=normal CORE_NUM=4
cd /app && make lint
cd /app && make ktest ARCH=rv64 PROFILE=normal CORE_NUM=4 KTEST=smp KREPEAT=1
cd /app && make ktest ARCH=la64 PROFILE=normal CORE_NUM=4 KTEST=smp KREPEAT=1
```

双架构构建和 QEMU 严格串行，没有并行切换共享工具链。

## 结果

| 门禁 | 退出码 | 关键结果 | 结论 |
|---|---:|---|---|
| RV64 kernel | 0 | 根 Make facade 完成内核构建 | PASS |
| LA64 kernel | 0 | 根 Make facade 完成内核构建 | PASS |
| lint | 2 | warning baseline/parser 差异 | FAIL |
| RV64 SMP ktest | 0 | `boot_hw_id=1`、`online_mask=0xf`、5/5 | PASS |
| LA64 SMP ktest | 0 | `boot_hw_id=0`、`online_mask=0xf`、5/5 | PASS |

两个架构的 focused QEMU 均通过：

```text
ok 1 smp::configured_cpus_are_online
ok 2 smp::legacy_scheduler_stays_on_boot_cpu
ok 3 smp::secondary_cpus_enter_idle_context
ok 4 smp::bsp_to_ap_ipi_ping
ok 5 smp::bsp_broadcasts_ipi_to_all_aps
# results: 5 passed, 0 failed, 5 total
[KTEST RESULT: PASS]
```

lint 原始输出中 `src/smp.rs` 的 warning 位于基线提交已经存在的
`let secondary_entry = _start as usize`，不属于 B10 diff。当前解析器无法
为缺少重复 `#[warn(...)]` note 的后续 warning 恢复 lint code，因而把多项
记录为 `unknown`；本次没有修改 lint 脚本或重新采集 baseline。

本证据只验收通用 reason 发布和 BSP→三个 AP 的广播/独立 ack，不外推为
AP→BSP、交叉发送、并发 reason、STOP、timer、调度或 TLB shootdown 已完成。
