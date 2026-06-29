# MangoCore — 双架构裸机 Rust 内核

## 项目概述

MangoCore 是一个 `#![no_std]` 裸机 Rust 内核，支持 **riscv64** 和 **loongarch64** 双架构，通过 OpenSBI 在 QEMU 上运行。项目面向操作系统内核竞赛开发，实现了约 218 个 Linux 兼容的系统调用，涵盖进程管理、虚拟内存、文件系统、网络、进程间通信和事件通知。架构设计参考了 DragonOS 的 VFS/MountFS 设计模式，行为语义以 Linux 6.6 为基准。

## 评审材料（竞赛评委入口）

项目为竞赛评审准备了两份深度文档，建议优先阅读：

| 文档 | 说明 |
|------|------|
| [📄 Technical-Report-MangoCore.md](docs/Technical-Report-MangoCore.md) | **技术报告** — 架构设计、模块实现、工程实践与竞赛历程 |
| [📄 Engineering-Casebook.md](docs/Engineering-Casebook.md) | **工程案例手册** — 以 Q&A 形式记录各模块设计权衡与调试过程 |

## 系统架构

```
QEMU
  |
  v
OpenSBI (M-mode)
  |
  v
entry.asm (S-mode)
  |
  v
rust_main()
  |-- console::init()
  |-- mm::init()
  |-- drivers::init()
  |-- fs::init()
  |-- net::init()
  |-- task::init()  [loads initproc ELF]
  |-- run_tasks()
```

内核从 QEMU 穿过 OpenSBI 固件（机器态）进入内核入口点（监管者态），然后自底向上初始化各子系统：控制台、内存管理、设备驱动、文件系统、网络，最后是任务调度器，负责加载并运行初始进程。

## 功能矩阵

| 类别 | 功能 | 状态 | 测试覆盖 |
|------|------|------|----------|
| 进程 | clone/fork/execve/wait4 | 完成 | LTP clone*, fork*, wait* |
| 内存 | SV39/MAP_SHARED/CoW/zRAM | 完成 | LTP mmap*, munmap*, mprotect* |
| 文件系统 | ext4/fat32/tmpfs/ramfs/procfs/devfs | 完成 | LTP open*, read*, write*, stat* |
| 网络 | TCP/UDP/RAW/Unix/Netlink/Packet | 完成 | LTP socket*, iperf, netperf |
| 进程间通信 | futex/SysV msg/sem/shm | 完成 | LTP futex*, sem*, shm* |
| 事件 | epoll/eventfd/signalfd/pidfd | 完成 | LTP epoll*, eventfd* |
| 定时器 | POSIX timer/nanosleep/clock | 完成 | LTP timer*, nanosleep* |
| 架构 | riscv64 + loongarch64 HAL | 完成 | 双架构编译通过 |
| 驱动 | virtio 块/网卡, PCI | 完成 | QEMU 启动 |
| 系统调用 | ~218 个 | 完成 | OSComp basic/busybox/lua/libctest |

## 快速开始

所有编译均在 Docker 容器内执行。这是唯一受支持的构建环境。

```bash
# 进入容器
make docker

# 仅编译内核（快速，适合迭代开发）
cd os && make rv64-kernel-build-only
cd os && make la64-kernel-build-only

# 构建完整镜像并在 QEMU 中运行
cd os && make rv64-only   # 构建 rv64 内核 + 用户态 + 镜像
cd os && make la64-only   # 构建 la64 内核 + 用户态 + 镜像
make rv64-run             # 在 QEMU 中运行 rv64（从项目根目录）
make la64-run             # 在 QEMU 中运行 la64
```

**注意：** 双架构使用不同的 nightly 工具链。禁止并行构建或放在同一命令行执行 — 请始终分开运行 `rv64-*` 和 `la64-*` 目标。

## 测试

测试选择由 `os_test.conf` 中的 `mask` 字段控制（12 位掩码）：

| 掩码 | 测试组 |
|------|--------|
| `0x001` | basic |
| `0x003` | basic + busybox |
| `0x800` | LTP |
| `0xFFF` | 全量（仅用于最终评测） |

```bash
# 快速冒烟测试
cd os && make rv64-run

# 全自动测试套件（双架构）
python3 scripts/run_full_test.py

# 注入自定义测试配置
make -C os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt CONF_FILE=../os_test.conf
```

测试镜像从 oscomp-testsuits-for-oskernel 仓库独立分发。

## 项目结构

```
os/            内核源码（#![no_std] 裸机）
user/          用户态程序和 C 库
docs/          文档（结构化，按测试映射）
scripts/       构建、测试和分析脚本
Makefile       顶层构建编排
```

内核源码树采用模块化结构：HAL（硬件抽象层）隔离 riscv64 和 loongarch64 的架构相关代码；VFS（虚拟文件系统）提供受 DragonOS 启发的分层设计；系统调用分发器将约 218 个系统调用路由到领域特定的处理函数。

## 文档

- `docs/Technical-Report-MangoCore.md` — **评审技术报告**（项目全貌与工程实践）
- `docs/Engineering-Casebook.md` — **评审工程案例手册**（Q&A 与调试案例）
- `docs/README.md` — 文档索引和导航
- `docs/08_testing/` — LTP 和 OSComp 测试映射
- `docs/Work_Log.md` — 开发日志和变更记录

## 开发规则

- **Docker 优先：** 所有编译在容器内执行，不依赖宿主机工具链。
- **禁止并行双架构编译：** riscv64 和 loongarch64 工具链冲突，必须串行执行。
- **永远不要直接编辑 `lang_items.rs`：** 编辑 `lang_items.rs.rv` 或 `lang_items.rs.la` 变体。`user/src/lang_items.rs` 同样适用。
- **每次 PTE 修改后必须刷新 TLB：** 使用 `sfence.vma`（riscv64）或 `invtlb`（loongarch64）。这是最常见的内存 bug 来源。
- **不要在等待点持有锁：** 克隆 Arc，释放锁，再执行操作。
- **修复根因：** 永远不要采用临时绕过方案。
- **更新 `docs/Work_Log.md`：** 每次代码修改后必须按 mango-workflow 格式更新。
- **双架构编译验证：** 每次变更必须同时通过 `make rv64-kernel-build-only` 和 `make la64-kernel-build-only`。
- **返回值约定：** 系统调用处理函数成功返回 `>= 0`，失败返回负 errno（例如 `-11` 表示 EAGAIN）。

## 参考资料

- [DragonOS](https://github.com/DragonOS-Community/DragonOS) — VFS/MountFS 架构设计参考
- Linux 6.6 — 系统调用语义和行为参考
- [smoltcp](https://github.com/smoltcp-rs/smoltcp) — TCP/IP 协议栈
- [virtio-drivers](https://github.com/rcore-os/virtio-drivers) — Virtio 设备驱动
