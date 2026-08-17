---
title: "L5 — 官方集成测试"
category: testing
status: stable
author: MangoCore Team
last_update: 2026-08-11
tags: [testing, l5, ltp, lmbench, iperf, integration, os_test]
---

# L5 — 官方集成测试

L5 是官方集成测试：运行 LTP / lmbench / iperf / libc-test / 比赛测例，是最终验收和性能趋势观察的关卡，通过 `os_test.conf` 的 mask 控制范围。

## 设计

L5 测的是**系统兼容性、性能和比赛表现**——完整用户态测试套件在真实 rootfs 上运行。它是测试体系的最终验收层，也是性能趋势观察的场所。

在 L0-L5 体系中，L5 承担**端到端兼容性**的验证：L4 都过但 L5 挂，通常指向边界语义、特殊文件、procfs/devfs、权限、资源限制或脚本假设。L5 发现 bug 后按「Bug 下沉流程」逐层下沉：先写 L4 regression → 如涉及内核机制下沉为 L3 → 如根因在纯逻辑提取 L1 用例。

## 原理

L5 依赖 `os_test.conf` 的 `mask` 字段注入（12-bit 控制测试组）。通过 `conf-inject` 将配置注入 rootfs，QEMU 启动后按 mask 选择要运行的测试组。

### 测试组

| 位 | 掩码 | 测试组 | 用途 |
|----|------|--------|------|
| 0 | `0x001` | basic | 冒烟 |
| 1 | `0x002` | busybox | 基础命令 |
| 2 | `0x004` | lua | 脚本解释器 |
| 3 | `0x008` | libctest | C 库测试 |
| 4 | `0x010` | iozone | 文件 I/O 性能 |
| 5 | `0x020` | unixbench | 系统基准 |
| 6 | `0x040` | iperf | 网络吞吐 |
| 7 | `0x080` | libcbench | C 库基准 |
| 8 | `0x100` | lmbench | 微基准 |
| 9 | `0x200` | netperf | 网络性能 |
| 10 | `0x400` | cyclictest | 实时延迟 |
| 11 | `0x800` | LTP | Linux 兼容性 |

常用 mask：`0x001` (basic)、`0x003` (basic+busybox)、`0x800` (LTP)、`0xFFF` (全量)。

## 如何启动运行

所有命令在 **Docker 容器内**执行：

```bash
# 注入测试配置
make -C os conf-inject CONF_ARCH=rv64 CONF_FILE=../os_test.conf

# QEMU 运行
cd os && make rv64-run

# 全量自动化
python3 scripts/run_full_test.py
```

准备测试镜像：

```bash
make testsuits-download
xz -dkc fs-img-dir/sdcard-rv.img.xz > sdcard-rv.img
xz -dkc fs-img-dir/sdcard-la.img.xz > sdcard-la.img
```

### SMP 8 核初赛非回归门禁

SMP 中改变普通用户任务执行路径的 T3 节点，以及 Phase/合并候选，必须在 Docker 内严格串行执行 RV64、LA64 的 normal `CORE_NUM=8` + `mask=0x003`。四组 START/END、脚本 `exit_code=0`、`online_mask=0xff`、无 panic/timeout/source drift 是硬条件；judge 还必须识别 314 个计分点，且得分和精确失败集合相对人工接受基线不退化。

当前 raw 参考为 RV64 312/314、LA64 305/314；semantic 最低分为 RV64 312/314、LA64 308/314。两者差异只来自执行规范中对官方 `test_pipe` 多 write 输出交错的严格块级归一化，raw judge 分数必须原样报告。不能只比较总分：同分但失败项换位也视为未通过；更好结果需稳定证据和人工确认后才向上 ratchet，任何失败都不能反向降低基线。纯文档/注释可复用同一代码快照的新鲜结果，局部 helper 按风险使用 focused test。完整触发条件、归一化前提、允许失败集合和证据边界见 [SMP Agent 执行规范](../10_plan/smp-agent-execution-spec.md#82-双架构-8-核初赛非回归门禁)。

### Bug 下沉流程

L5 发现 bug 后：先尝试写 L4 regression → 如涉及内核机制，进一步下沉为 L3 → 如根因在纯逻辑，提取 L1 用例。
