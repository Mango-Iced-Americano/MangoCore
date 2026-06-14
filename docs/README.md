---
title: "MangoCore 文档索引"
category: overview
status: stable
author: MangoCore Team
last_update: 2026-06-14
tags: [docs, index, overview]
---

# MangoCore 文档索引

## 概述

本文档是 MangoCore 项目所有文档的入口。它描述了目录结构，为不同读者提供推荐阅读路径，并收录每个现有模块文档及其状态。

文档遵循 10 类别布局（`00_overview` 到 `09_debug`），另含工作日志、Bug 事后分析、LTP 测试计划和架构设计等遗留目录。

## 目录结构

| 目录 | 主题领域 | 说明 |
|-----------|-----------|-------------|
| `00_overview/` | 项目概述 | 项目高层描述、目标和范围 |
| `01_architecture/` | 架构 | 系统架构、启动流程、HAL 设计 |
| `02_syscall/` | 系统调用参考 | 系统调用表、ABI、分发结构 |
| `03_fs/` | 文件系统 | VFS 层、ext4、FAT32、tmpfs、ramfs、procfs、devfs、PageCache |
| `04_mm/` | 内存管理 | 物理分配器、SV39 页表、VMA、mmap、CoW、OOM |
| `05_process/` | 进程与任务 | 调度、信号、futex、IPC、线程 |
| `06_net/` | 网络 | smoltcp 集成、TCP/UDP/RAW/Unix 套接字、设备适配器 |
| `07_driver/` | 驱动 | Virtio 块/网卡、设备 trait、HAL 后端 |
| `08_testing/` | 测试 | 测试配置、QEMU 设置、CI 工作流 |
| `09_debug/` | 调试 | GDB 设置、日志、常见调试技巧、Bug 事后分析（7 篇） |
| `_templates/` | 模板 | 新模块文档的标准文档模板 |
| `kernel/` | 遗留子系统文档 | 旧版模块文档（待迁移到 00-09 布局） |
| `ltp/` | LTP 测试计划 | LTP 测试策略、各子系统状态、工作流 |
| `plan/` | 架构计划 | 设计方案和迁移计划 |
| `Work_Log.md` | 开发日志 | 所有重要变更的时间顺序记录 |

## 阅读指南

### 新开发者

从 `00_overview` 了解项目背景，接着看 `01_architecture` 理解系统设计，然后阅读项目根目录 `README.md` 的**快速开始**部分。

**顺序：** `00_overview` → `01_architecture` → 快速开始（根目录 README）

### 竞赛评委

审阅项目概览和功能范围，然后跳转到 `08_testing` 了解测试方法和功能矩阵。

**顺序：** `00_overview` → `08_testing` → 功能矩阵（见 08_testing）

### 子系统开发者

聚焦相关子系统文档，配合 `02_syscall` 了解系统调用 ABI 参考，以及 `08_testing` 了解该子系统的测试配置。

**顺序：** 子系统文档 → `02_syscall` → `08_testing`

### 调试人员

从 `09_debug` 开始，了解 GDB 设置、日志配置和常见调试工作流。

**顺序：** `09_debug`

## 模块文档

| 目录 | 文档 | 模块 | 状态 | 最后更新 |
|-----------|----------|--------|--------|-------------|
| `06_net/` | `README.md` | 网络概览 | 稳定 | 2026-06-14 |
| `06_net/` | `architecture.md` | 网络架构 | 稳定 | 2026-06-14 |
| `06_net/` | `socket-trait-and-fd.md` | Socket trait 与 fd | 稳定 | 2026-06-14 |
| `06_net/` | `syscall-layer.md` | 网络系统调用层 | 稳定 | 2026-06-14 |
| `06_net/` | `smoltcp-device-routing.md` | 适配器与路由 | 稳定 | 2026-06-14 |
| `06_net/` | `tcp.md` | TCP 实现 | 稳定 | 2026-06-14 |
| `06_net/` | `udp-raw-unix-netlink-packet.md` | UDP、RAW、Unix、Netlink、Packet | 稳定 | 2026-06-14 |
| `06_net/` | `test-map.md` | 网络测试映射 | 草稿 | 2026-06-14 |
| `06_net/` | `debugging.md` | 网络调试 | 草稿 | 2026-06-14 |

## 模板与规范

| 文件 | 说明 |
|------|-------------|
| `docs/_templates/module.md` | 新模块文档的标准模板。包含 YAML 前置元数据、架构图、API 参考、测试映射和已知问题等章节。 |

## 旧文档

以下目录包含项目早期阶段的文档，正在逐步迁移到 00-09 分类布局中。

| 目录 | 内容 |
|-----------|----------|
| `docs/kernel/fs/` | `ext4-cache-design.md` — ext4 的 PageCache 设计 |
| `docs/kernel/net/` | `README.md`、`architecture.md`（迁移前版本）、`device-layer.md`、`multi-iface-routing.md`、`roadmap.md`、`socket-subsystem.md`、`syscalls.md`、`tcp-state-machine.md` |
| `docs/kernel/` | `futex.md`、`Nanosleep.md`、`tgkill.md`、`信号.md` — 子系统深入分析 |
| `docs/ltp/` | LTP 测试策略（`LTP_BOTTOM_UP_GUIDE.md`）、各子系统计划和状态（`ltp_fs_plan.md`、`ltp_fs_status.md`、`ltp_mount_plan.md`、`ltp_mount_status.md`、`ltp_net_plan.md`、`ltp_net_status.md`）、工作流（`ltp_workflow.md`）、完整堆追踪报告及 MM 重构指南 |
| `docs/09_debug/` | 7 个已修复 Bug 的事后分析，包括 ext4 rename、mount bind 泄露、virtio 块对齐和内核栈溢出 |
| `docs/plan/` | 架构提案：NET_PLAN、UDP 改进计划、VFS 迁移计划、I/O 分块计划 |
| `docs/Work_Log.md` | 每日开发日志，涵盖所有重要内核变更及编译和 QEMU 验证结果 |
