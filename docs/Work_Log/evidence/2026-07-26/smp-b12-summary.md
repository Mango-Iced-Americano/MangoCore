# SMP-P2-B12 确定性验证摘要

## 验证对象

- 基线提交：`eba0b1dc feat(smp): defer timer work to safe points`
- 验证时 tracked source 差异 SHA-256：
  `571b5d7b3439dc26613a7be69cc206560edbb81b05945b53d2f8c0735dd5fe25`
- 环境：项目 Docker 开发容器，仓库绑定到 `/app`
- 拓扑：`CORE_NUM=4`
- 测试集：`KTEST=smp`，`KREPEAT=1`

四项命令执行前后的源码指纹一致；验证过程没有修改 tracked source，也
没有超时、panic 或被禁止的宿主机编译。文档在验证完成后补写，不影响
已验证的内核源码和产物。

## 串行执行

```text
cd /app && make kernel ARCH=rv64 PROFILE=normal CORE_NUM=4
cd /app && make kernel ARCH=la64 PROFILE=normal CORE_NUM=4
cd /app && make ktest ARCH=rv64 PROFILE=normal CORE_NUM=4 KTEST=smp KREPEAT=1
cd /app && make ktest ARCH=la64 PROFILE=normal CORE_NUM=4 KTEST=smp KREPEAT=1
```

双架构构建和 QEMU 严格串行，没有并行切换共享工具链。本工作包没有运行
已知存在 baseline/parser 差异的 `make lint`，避免与 IPI 功能证据混淆。

## 结果

| 门禁 | 退出码 | 用时 | 关键结果 | 结论 |
|---|---:|---:|---|---|
| RV64 kernel | 0 | 124.445 s | tracked source 指纹稳定 | PASS |
| LA64 kernel | 0 | 130.783 s | tracked source 指纹稳定 | PASS |
| RV64 SMP ktest | 0 | 128.256 s | `boot_hw_id=3`、`online_mask=0xf`、7/7 | PASS |
| LA64 SMP ktest | 0 | 131.622 s | `boot_hw_id=0`、`online_mask=0xf`、7/7 | PASS |

两个架构的 focused QEMU 均通过：

```text
ok 1 smp::configured_cpus_are_online
ok 2 smp::legacy_scheduler_stays_on_boot_cpu
ok 3 smp::secondary_cpus_enter_idle_context
ok 4 smp::bsp_to_ap_ipi_ping
ok 5 smp::bsp_broadcasts_ipi_to_all_aps
ok 6 smp::kernel_timer_irq_is_deferred
ok 7 smp::ap_to_bsp_ipi_round_trip
# results: 7 passed, 0 failed, 7 total
[KTEST RESULT: PASS]
```

第七项由 CPU0 在受控 IRQ 窗口发起。每个架构的 CPU1、CPU2、CPU3 各
顺序完成 64 轮 request/reply；每轮先观察对应 ack 再复用 reason bit，
共覆盖 192 次真实 AP→BSP doorbell。AP hard IRQ 只发布 deferred reply，
回复从 AP idle stack 发送；CPU0 的用户/内核 trap 共用同一无锁 fast path。

本证据只验收 CPU0 本地 IPI line、AP→BSP 请求/回复、reason 复用顺序和
AP idle check→wait 协议；不外推为 STOP、普通长 syscall 中断窗口、
RESCHEDULE、调度、TLB shootdown 或共享子系统多核安全已完成。
