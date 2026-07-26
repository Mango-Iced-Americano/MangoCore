# SMP-P2-B14 证据摘要

- 基线 commit：`1a4801f2`
- 验证时 tracked source diff SHA-256：
  `ccb1f9bcc001cb8a6cef9c3ebe0d100c418dc05d57c7e5b7cce3f677cd684088`
- 配置：QEMU `CORE_NUM=4`，focused `KTEST=smp KREPEAT=2`

## 实现边界

- 只在双架构 user-syscall closure 内开放 timer/IPI；入口和退出均
  为 IRQ-off，不在持有 `task.inner` 时开窗口。
- `schedule()` 显式保存并关闭 `SIE/IE`，idle 接管 IRQ-off CPU；
  原任务恢复后才恢复它自己的中断快照。
- timer/IPI hard path 仍只发布原子状态，不持普通锁、不调度；
  panic 在任何诊断前立即关中断。
- LA64 trap return 中的 CPU-local/PRMD 更新收敛到 `task.inner` 锁内。

## 验证结果

| 命令 | 退出码 | 结果 | 用时 |
|---|---:|---:|---:|
| `make kernel ARCH=rv64 PROFILE=normal CORE_NUM=4` | 0 | PASS | 121.131s |
| `make kernel ARCH=la64 PROFILE=normal CORE_NUM=4` | 0 | PASS | 126.951s |
| `make ktest ARCH=rv64 PROFILE=normal CORE_NUM=4 KTEST=smp KREPEAT=2` | 0 | 17/17 PASS | 129.253s |
| `make ktest ARCH=la64 PROFILE=normal CORE_NUM=4 KTEST=smp KREPEAT=2` | 0 | 17/17 PASS | 127.186s |

双架构均观察到 `online_mask=0xf` 和 TAP `1..17`。8 个普通用例
重复两轮，`smp::syscall_irq_window_survives_schedule` 在第 8/16 项
各 PASS 一次；`smp::secondary_cpus_stop_and_ack` 只在第 17 项执行。
最终均为 17 passed、0 failed、`[KTEST RESULT: PASS]`，QEMU 正常退出。

四次 recipe 的 source-before/source-after 指纹一致，无 mutation、panic、
timeout 或 forbidden marker。测试后只更新本摘要、Work Log 和架构文档，
按 workflow 新鲜度规则不重复执行构建/QEMU。
`schedule()` 的并发状态注释在验证后被详细化，不改变任何可执行语句；
因此本文仍保留实际被测源码的 `ccb1f9bc...` 指纹，不将注释后指纹
伪装为 QEMU 验证输入。

## 证据边界

focused ktest 直接执行生产 `with_local_interrupts_enabled()`、真实
`schedule()` 和 AP→BSP IPI，但它运行在 kernel task，没有从用户态发起
真实 syscall。双架构 trap 分支接线本轮通过两个 normal kernel build
验证；用户态 basic/regression 属于 Phase 2 阶段门禁，本摘要不将
17/17 focused PASS 扩大解读为全用户态回归。
