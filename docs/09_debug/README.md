---
title: "调试与故障复盘索引"
category: debug
status: stable
author: MangoCore Team
last_update: 2026-07-15
tags: [debugging, postmortem, performance, qemu, loongarch64, riscv64]
---

# 调试与故障复盘

## 定位

本目录保存具体故障的证据链、反汇编、对照实验和修复复盘。稳定的子系统设计与
长期接口约束应写入对应的 `01_architecture` 到 `08_testing` 文档；带有时间线、
诊断插桩和失败假设排除过程的内容归入本目录。

## 故障复盘

| 文档 | 状态 | 主题 |
|------|------|------|
| [`la64_on_board/`](la64_on_board/README.md) | 持续更新 | 2K1000LA 组会入口、34 提交总账、29 篇编号专题与 1 篇 hole-read ABI 深挖 |
| [`la64_on_board/bug-hole-read-mismatch.md`](la64_on_board/bug-hole-read-mismatch.md) | 已修复，保留专用遥测边界 | LA64 数据正确但切片比较失败；用户栈 16 字节 ABI 对齐、LLVM `ori` 地址折叠和完整 exec/signal 链路 |
| `bug-la64-kernel-stack-overflow.md` | 已修复 | LA64 内核栈溢出、guard page 与静默堆损坏 |
| `bug-fallback-timer-lmbench-hang.md` | 已分析 | fallback timer 与 lmbench hang |
| `ext4-rename-name-panic.md` | 已分析 | ext4 rename/name 处理 panic |
| `mkfifo-ext4-no-eeexist.md` | 已分析 | mkfifo/ext4 错误码和重复创建语义 |
| `virtio-blk-unaligned-panic.md` | 已分析 | VirtIO block 非对齐访问 panic |
| `mount-bind-leak-analysis.md` | 已分析 | bind mount 生命周期与资源泄漏 |

## 调试计划与实验

| 文档 | 内容 |
|------|------|
| `mount-bind-fix-plan.md` | mount bind 修复计划 |
| `buddy-allocator-scan-drift.md` | buddy allocator 扫描漂移和性能定位 |
| `timer-timekeeping-contrast-experiment-20260618.md` | timer/timekeeping 对照实验 |
| `perf_diag.md` | 通用性能诊断记录与指标 |

## 阅读方法

故障复盘优先按以下顺序阅读：

```text
症状与最小复现
-> 已确认事实
-> 排除的假设
-> 反汇编/日志/地址证据
-> 根因
-> 修复设计
-> 回归门禁
-> 可复用调试原则
```

不要仅根据测试名称确定子系统。例如 `fs_test` 中的数据比较失败，可能发生在 VFS、
用户拷贝、编译器优化或用户入口 ABI，而不一定发生在文件系统。
