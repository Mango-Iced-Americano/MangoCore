---
title: "2K1000LA 实板工作报告时间线"
category: debug
status: current
author: MangoCore Team
last_update: 2026-07-17
tags: [loongarch64, 2k1000la, board, timeline, evidence, performance]
related_docs:
  - "docs/09_debug/la64_on_board/260710/README.md"
  - "docs/09_debug/la64_on_board/260717/README.md"
  - "docs/09_debug/README.md"
---

# 2K1000LA 实板工作报告时间线

## 1. 目录用途

本目录按工作批次日期保存 2K1000LA 实板调试、性能实验和问题复盘。日期目录采用
`YYMMDD`，表示这一批连续工作的开始日期，而不表示目录中每个实验都只发生在当天。
每个批次必须同时保留结论、失败路径、证据边界和可复核的原始数据入口。

2026-07-17 重组前，`la64_on_board/` 下共有 32 个 Markdown 文件。它们已完整迁入
`260710/`，文件内容、未提交的 `development-log.md` 工作区差异和 102 个同目录相对
链接均被保留；父目录只承担跨批次导航，不再混放具体问题复盘。

## 2. 批次导航

| 批次 | 覆盖范围 | 主要产物 | 状态 |
|------|----------|----------|------|
| [260710](260710/README.md) | 2026-07-10 起的首次实板 bring-up 总账 | uImage/VALEN/TLB、2 GiB DRAM、AHCI/P1-P4、GMAC/DHCP/HTTPS、CSPRNG、CPython/APK、ext4/ABI；32 篇原始文档 | 历史基线，持续可审计 |
| [260717](260717/README.md) | 2026-07-17 起的 Python 性能专项 | production 18 项基线、非对齐 trap、匿名页释放 O(N²) 及真实 Python 占比、ext4 小文件、strict runtime 固化和文本原始数据 | 当前批次 |

## 3. 阅读顺序

首次了解实板整体移植时，从 [260710/development-log.md](260710/development-log.md)
进入；它按 34 个提交和阶段 A-H 组织完整时间线。分析 Python 性能时，从
[260717/README.md](260717/README.md) 进入，再按“基线 → 单问题根因 → 首轮实验 →
原始数据”顺序阅读。

文档中的状态词保持严格区分：

| 状态 | 含义 |
|------|------|
| production 实板数据 | 无性能诊断 feature 的 2K1000LA 正式计时，可用于当前绝对性能基线 |
| perf_diag 实板数据 | 用于事件计数和路径归因；只有同一构建内 `stats_on=0/1` 可衡量运行时探针税 |
| QEMU 数据 | 用于功能、双架构和计数器自检，不替代实板性能结论 |
| 已确认 | 实板现象、可复现步骤、源码链和计数/短追踪同时闭合 |
| 高概率 | 有多项一致证据，但仍缺 PMU、精确阶段计时或隔离变量 |
| 证据不足 | 只能记录现象或假设，不写成根因和收益 |

## 4. 归档约束

- 历史批次只修正失效路径和明显事实错误，不重写当时的证据口径。
- 大型 uImage、ELF、runtime 压缩包不复制进文档树；记录文件名、大小和 SHA-256。
- manifest、JSONL、CSV、串口日志和验证日志应进入对应批次的 `raw-data/`，使
  `target/` 被清理后仍可审计文本证据。
- 不把不同内核布局、不同 suite、不同存储介质或不同 workload 规模直接计算成正式
  加速比；这种数字只能标为辅助趋势。
- 所有 Python 正式性能结论最终落到 2K1000LA 和 ext4；FAT32/QEMU 只作边界或负对照。
