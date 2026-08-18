---
title: "MangoCore 文档索引"
category: overview
status: draft
owner: MangoCore Team
last_updated: 2026-07-25
tags: [docs, index, overview]
---

# MangoCore 文档索引

## 概述

本文档是 MangoCore 项目所有文档的入口。它描述了目录结构，为不同读者提供推荐阅读路径，并收录每个现有模块文档及其状态。

文档遵循编号布局（`00_overview` 到 `10_plan`），另含工作日志、Bug 事后分析、LTP 测试计划和架构设计等遗留目录。

## 评审材料（竞赛评委优先阅读）

| 文档 | 说明 |
|------|------|
| [Technical-Report-MangoCore.md](00_overview/Technical-Report-MangoCore.md) | **技术报告** — 项目完整技术综述：架构设计、模块实现、工程实践与竞赛历程 |
| [Engineering-Casebook.md](00_overview/Engineering-Casebook.md) | **工程案例手册** — 以 Q&A 形式记录各模块的设计权衡、调试过程与 Regression 案例分析 |
| [AI-Usage-Report.md](00_overview/AI-Usage-Report.md) | **AI 工具使用情况报告** — 工具清单、使用场景、证据与合规声明 |
| [toolchain-and-build-guide.md](00_overview/toolchain-and-build-guide.md) | **工具链与工程入口手册** — Docker、Rustup、Cargo、Make、QEMU、测试镜像、日志与故障排查 |

## 目录结构

| 目录 / 文件 | 主题领域 | 说明 |
|-----------|-----------|-------------|
| `00_overview/Technical-Report-MangoCore.md` | 评审材料 | 竞赛技术报告：项目综述与工程实践 |
| `00_overview/Engineering-Casebook.md` | 评审材料 | 竞赛工程案例手册：Q&A 与调试案例 |
| `00_overview/AI-Usage-Report.md` | 评审材料 | AI 工具使用情况报告：工具清单、使用场景、证据与合规声明 |
| `00_overview/` | 项目概述 | 项目高层描述、目标和范围 |
| `01_architecture/` | 架构 | 系统架构、BSP/AP 启动流程、trap 与 HAL 设计 |
| `02_syscall/` | 系统调用参考 | 系统调用表、ABI、分发结构 |
| `03_fs/` | 文件系统 | VFS 层、ext4、FAT32、tmpfs、ramfs、procfs、devfs、PageCache |
| `04_mm/` | 内存管理 | 物理分配器、SV39 页表、VMA、mmap、CoW、OOM |
| `05_process/` | 进程与任务 | 调度、信号、futex、IPC、线程 |
| `06_net/` | 网络 | smoltcp 集成、TCP/UDP/RAW/Unix 套接字、设备适配器 |
| `07_driver/` | 驱动 | Virtio 块/网卡、设备 trait、HAL 后端 |
| `diagrams/` | 架构图 | 子系统架构图、流程图、关系图等图片资源 |
| `08_testing/` | 测试 | 隔离 CPython/APK 运行时、QEMU 与实板测试门禁 |
| [`09_debug/`](09_debug/README.md) | 调试 | GDB 设置、日志、常见调试技巧、Bug 事后分析（多篇） |
| `_templates/` | 模板 | 新模块文档的标准文档模板 |
| `kernel/` | 遗留子系统文档 | 旧版模块文档（待迁移到 00-09 布局） |
| `ltp/` | LTP 测试计划 | LTP 测试策略、各子系统状态、工作流 |
| `10_plan/` | 架构计划 | SMP、跨子系统设计方案和迁移计划 |
| `Work_Log.md` | 开发日志 | 所有重要变更的时间顺序记录 |

## 阅读指南

### 新开发者

从 `00_overview` 了解项目背景，接着看 `01_architecture` 理解系统设计，然后阅读项目根目录 `README.md` 的**快速开始**部分。

**顺序：** `00_overview` → `01_architecture` → 快速开始（根目录 README）

### 竞赛评委

先阅读**评审材料**中的两份文档了解项目全貌和深度案例，再按需查阅子系统模块文档。

**顺序：** [Technical-Report-MangoCore.md](00_overview/Technical-Report-MangoCore.md)（技术报告全貌）→ [Engineering-Casebook.md](00_overview/Engineering-Casebook.md)（工程案例深度）→ [AI-Usage-Report.md](00_overview/AI-Usage-Report.md)（AI 使用披露）→ 按需读 `03_fs/`、`06_net/` 等子系统文档

### 子系统开发者

聚焦相关子系统文档，配合 `02_syscall` 了解系统调用 ABI 参考，以及 `08_testing` 了解该子系统的测试配置。

**顺序：** 子系统文档 → `02_syscall` → `08_testing`

### 调试人员

从 `09_debug` 开始，了解 GDB 设置、日志配置和常见调试工作流。

**顺序：** [09_debug/README.md](09_debug/README.md)

## 模块文档

| 目录 | 文档索引 | 状态 | 说明 |
|-----------|----------|--------|------|
| `01_architecture/` | [README.md](01_architecture/README.md) | 草稿 | 架构文档已对齐 CPIO→PID1→runner、镜像角色、BSP/AP 启动边界与当前 Make facade |
| `02_syscall/` | [README.md](02_syscall/README.md) | 稳定 | 系统调用文档含 12 篇文档，覆盖 ABI、分发、syscall 表、文件/fd/event、进程、MM、signal/time/IPC、网络索引和错误码 |
| `03_fs/` | [README.md](03_fs/README.md) | 草稿 | 文件系统子系统含 14 篇文档，涵盖 VFS、PageCache、ext4、FAT32、tmpfs、procfs 等 |
| `04_mm/` | [README.md](04_mm/README.md) | 稳定 | 内存管理文档含 14 篇文档，覆盖 frame allocator、页表/TLB、AddressSpace/VMA、mmap/brk、fault/uaccess、CoW、filemap 和 OOM |
| `05_process/` | [README.md](05_process/README.md) | 稳定 | 进程文档含 17 篇文档，覆盖 TCB/PCB、调度、WaitQueue、clone/namespace、exec、exit/wait、signal、futex、IPC 和 rlimit |
| `06_net/` | [README.md](06_net/README.md) | 草稿 | 网络子系统重构后含 21 篇文档，涵盖 socket 类型、设备层、路由、DHCP、调试等 |
| `09_debug/` | [README.md](09_debug/README.md) | 持续更新 | `la64_on_board/` 按工作批次归档：`260710/` 保存 32 篇 bring-up 总账/专题，`260717/` 保存 Python 性能报告与原始数据 |
| `10_plan/` | [SMP 实施方案](10_plan/smp-8core-implementation.md)、[Agent 执行规范](10_plan/smp-agent-execution-spec.md) | 提案/规范 | RV64 最高 8 核、LA64 最高 12 核的 SMP 设计、风险分级工作包和自适应验证门禁 |

### 测试与实板使用

| 文档 | 说明 |
|---|---|
| [la64_on_board/](09_debug/la64_on_board/README.md) | 2K1000LA 按日期组织的工作报告时间线；包含首次 bring-up 总账、故障深挖和 Python 性能专项 |
| [mangocore-python-guide.md](08_testing/mangocore-python-guide.md) | 2K1000LA 上使用 MangoCore 特制 CPython、首次引导 pip、持久安装和故障排查 |
| [cpython-isolated.md](08_testing/cpython-isolated.md) | CPython L3-L9 隔离测试设计、ABI 覆盖和 QEMU/实板门禁 |
| [apk-isolated.md](08_testing/apk-isolated.md) | APK 隔离环境、P4 持久应用根和 `persist-shell` 设计 |

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
| `docs/ltp/` | LTP 测试策略（`LTP_BOTTOM_UP_GUIDE.md`）、各子系统计划和状态（`ltp_fs_plan.md`、`ltp_fs_status.md`、`ltp_mount_plan.md`、`ltp_mount_status.md`、`ltp_net_plan.md`、`ltp_net_status.md`）、deferred 失败清单（`ltp_net_deferred.md`）、工作流（`ltp_workflow.md`）、完整堆追踪报告及 MM 重构指南 |
| `docs/09_debug/` | 多个已修复 Bug 的事后分析及 `la64_on_board/` 实板专题，包括 LA64 用户栈 ABI、ext4 rename、mount bind 泄露、virtio 块对齐和内核栈溢出 |
| `docs/plan/` | 架构提案：NET_PLAN、UDP 改进计划、VFS 迁移计划、I/O 分块计划 |
| `docs/Work_Log.md` | 每日开发日志，涵盖所有重要内核变更及编译和 QEMU 验证结果 |
