# SMP-P2-B09 确定性验证摘要

## 验证对象

- 基线提交：`634c530a feat(smp): move APs onto per-cpu idle stacks`
- 验证时的内核源码差异 SHA-256：
  `edeb7aa59646f6d5fffbbc0e804b4c2650d0dd3fe07d07eb45c61eb870816955`
- 环境：项目 Docker 开发容器，仓库绑定到 `/app`
- 拓扑：`CORE_NUM=2`
- 测试集：`KTEST=smp`，`KREPEAT=1`

验证前后的 `os/src` 差异指纹完全相同，测试过程没有修改进入容器的内核
源码。验证后只澄清了 LoongArch vector 所有权和 SMP 阶段边界注释，当前
差异指纹为
`769e8a90b1a26e2fa1edbb4cf566a795403db4616c66e78bde46d516cc17f6af`；
这些改动不进入编译产物，因此没有重复执行 QEMU。

## 串行执行

```text
cd /app/os &&
make rv64-ktest CORE_NUM=2 KTEST=smp KREPEAT=1

cd /app/os &&
make la64-ktest CORE_NUM=2 KTEST=smp KREPEAT=1
```

两个架构严格串行，未并行切换工具链。每个 ktest 入口都包含对应架构的实际
编译、链接和 QEMU 启动，因此没有再机械重复独立 build-only。

## 结果

| 架构 | 退出码 | 用时 | online mask | TAP | 结论 |
|---|---:|---:|---:|---:|---|
| RV64 | 0 | 129.886 s | `0x3` | 4/4 | PASS |
| LA64 | 0 | 133.619 s | `0x3` | 4/4 | PASS |

两个架构均通过：

```text
ok 1 smp::configured_cpus_are_online
ok 2 smp::legacy_scheduler_stays_on_boot_cpu
ok 3 smp::secondary_cpus_enter_idle_context
ok 4 smp::bsp_to_ap_ipi_ping
# results: 4 passed, 0 failed, 4 total
[KTEST RESULT: PASS]
```

测试过程没有 timeout、panic、缺失必需结束标记或源码变异。该证据只验收
BSP→AP 单播 PING 的 mailbox/doorbell/trap/ack 闭环，不外推为广播、
并发 reason、STOP、timer、调度或 TLB shootdown 已完成。
