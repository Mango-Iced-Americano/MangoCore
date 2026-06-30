---
title: "MangoCore 文档索引"
category: overview
status: draft
owner: MangoCore Team
last_updated: 2026-06-29
tags: [docs, index, overview]
---

# MangoCore 文档索引

## 概述

本文档是 MangoCore 项目所有文档的入口。它描述了目录结构，为不同读者提供推荐阅读路径，并收录每个现有模块文档及其状态。

文档遵循 10 类别布局（`00_overview` 到 `09_debug`），另含工作日志、Bug 事后分析、LTP 测试计划和架构设计等遗留目录。

## 评审材料（竞赛评委优先阅读）

| 文档 | 说明 |
|------|------|
| [Technical-Report-MangoCore.md](00_overview/Technical-Report-MangoCore.md) | **技术报告** — 项目完整技术综述：架构设计、模块实现、工程实践与竞赛历程 |
| [Engineering-Casebook.md](00_overview/Engineering-Casebook.md) | **工程案例手册** — 以 Q&A 形式记录各模块的设计权衡、调试过程与 Regression 案例分析 |

## 目录结构

| 目录 / 文件 | 主题领域 | 说明 |
|-----------|-----------|-------------|
| `00_overview/Technical-Report-MangoCore.md` | 评审材料 | 竞赛技术报告：项目综述与工程实践 |
| `00_overview/Engineering-Casebook.md` | 评审材料 | 竞赛工程案例手册：Q&A 与调试案例 |
| `00_overview/` | 项目概述 | 项目高层描述、目标和范围 |
| `01_architecture/` | 架构 | 系统架构、启动流程、HAL 设计 |
| `02_syscall/` | 系统调用参考 | 系统调用表、ABI、分发结构 |
| `03_fs/` | 文件系统 | VFS 层、ext4、FAT32、tmpfs、ramfs、procfs、devfs、PageCache |
| `04_mm/` | 内存管理 | 物理分配器、SV39 页表、VMA、mmap、CoW、OOM |
| `05_process/` | 进程与任务 | 调度、信号、futex、IPC、线程 |
| `06_net/` | 网络 | smoltcp 集成、TCP/UDP/RAW/Unix 套接字、设备适配器 |
| `07_driver/` | 驱动 | Virtio 块/网卡、设备 trait、HAL 后端 |
| `diagrams/` | 架构图 | 子系统架构图、流程图、关系图等图片资源 |
| `08_testing/` | 测试 | 待填充 |
| `09_debug/` | 调试 | GDB 设置、日志、常见调试技巧、Bug 事后分析（多篇） |
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

先阅读**评审材料**中的两份文档了解项目全貌和深度案例，再按需查阅子系统模块文档。

**顺序：** [Technical-Report-MangoCore.md](00_overview/Technical-Report-MangoCore.md)（技术报告全貌）→ [Engineering-Casebook.md](00_overview/Engineering-Casebook.md)（工程案例深度）→ 按需读 `03_fs/`、`06_net/` 等子系统文档

### 子系统开发者

聚焦相关子系统文档，配合 `02_syscall` 了解系统调用 ABI 参考，以及 `08_testing` 了解该子系统的测试配置。

**顺序：** 子系统文档 → `02_syscall` → `08_testing`

### 调试人员

从 `09_debug` 开始，了解 GDB 设置、日志配置和常见调试工作流。

**顺序：** `09_debug`

## 模块文档

| 目录 | 文档索引 | 状态 | 说明 |
|-----------|----------|--------|------|
| `01_architecture/` | [README.md](01_architecture/README.md) | 稳定 | 架构文档含 11 篇文档，覆盖总体架构、初始化、trap/syscall 入口、HAL、双架构平台和调试映射 |
| `02_syscall/` | [README.md](02_syscall/README.md) | 稳定 | 系统调用文档含 12 篇文档，覆盖 ABI、分发、syscall 表、文件/fd/event、进程、MM、signal/time/IPC、网络索引和错误码 |
| `03_fs/` | [README.md](03_fs/README.md) | 草稿 | 文件系统子系统含 14 篇文档，涵盖 VFS、PageCache、ext4、FAT32、tmpfs、procfs 等 |
| `04_mm/` | [README.md](04_mm/README.md) | 稳定 | 内存管理文档含 14 篇文档，覆盖 frame allocator、页表/TLB、AddressSpace/VMA、mmap/brk、fault/uaccess、CoW、filemap 和 OOM |
| `05_process/` | [README.md](05_process/README.md) | 稳定 | 进程文档含 17 篇文档，覆盖 TCB/PCB、调度、WaitQueue、clone/namespace、exec、exit/wait、signal、futex、IPC 和 rlimit |
| `06_net/` | [README.md](06_net/README.md) | 草稿 | 网络子系统重构后含 21 篇文档，涵盖 socket 类型、设备层、路由、DHCP、调试等 |

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
| `docs/09_debug/` | 多个已修复 Bug 的事后分析，包括 ext4 rename、mount bind 泄露、virtio 块对齐和内核栈溢出 |
| `docs/plan/` | 架构提案：NET_PLAN、UDP 改进计划、VFS 迁移计划、I/O 分块计划 |
| `docs/Work_Log.md` | 每日开发日志，涵盖所有重要内核变更及编译和 QEMU 验证结果 |
