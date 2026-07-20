# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目概述

MangoCore 是一个 **#![no_std] Rust 内核**，支持 **riscv64** 和 **loongarch64** 双架构，通过 OpenSBI 在 QEMU 上运行。这是一个竞赛操作系统，支持 ext4/FAT32 文件系统、smoltcp 网络协议栈、virtio 块/网卡驱动、SV39 虚拟内存、zram 交换、以及 100+ 个 Linux 兼容系统调用。

**关键约束：** 没有 `cargo test` 或 `cargo clippy`——这是裸机内核。唯一可用的验证是编译 + QEMU 集成测试。

详细架构指南见 `AGENTS.md`。重构变更记录见 `WORK_LOG.md`。踩坑经验见 `EXPERIENCE.md`。

**设计蓝本：本项目以 [DragonOS](https://github.com/DragonOS-Community/DragonOS) 为参考蓝本。** VFS 架构、socket 抽象、Endpoint 设计、挂载层等核心模块均参照 DragonOS 的实现。写代码时务必先查阅 DragonOS 对应模块的设计思路和 API 签名，不要凭空造轮子。

## 构建与测试命令

所有操作在 Docker 容器内执行（宿主机缺少交叉编译工具链）。先进入容器：

```bash
make docker          # 启动容器并进入 bash
```

根目录评测入口 `make all` 会派生 HOME 对应的 `RUSTUP_HOME` 和 `CARGO_HOME`，并在需要时自动执行 setup 和 preflight。全新容器首次运行可能使用网络。直接执行 OS、用户态或架构目标前，先运行只读的 `make toolchain-preflight`，这些入口不会自动 provisioning。手动流程仍可运行 `make toolchain-setup`，固定工具链仍为 `nightly-2026-05-10`。

### 构建

```bash
# 仅编译内核（最快，日常迭代用）
cd os && make rv64-kernel-build-only
cd os && make la64-kernel-build-only

# 完整编译（内核 + 用户态 + 文件系统镜像）
cd os && make rv64-only
cd os && make la64-only

# 项目根目录双架构编译
make all
```

**注意：** rv64 和 la64 共用根目录固定的 `nightly-2026-05-10`，LA64 target 仍为 `loongarch64-unknown-linux-gnu`。**不要并行编译两个架构**，因为两条路径共享架构生成状态，必须串行执行。根目录 `make all` 仅在需要时 provisioning，直接构建路径只做 preflight。

**注意：** 永远不要直接编辑 `os/src/lang_items.rs` 或 `user/src/lang_items.rs`——编辑对应的 `.rv` / `.la` 变体文件。编译期由 `target_arch` 直接选择正确变体。

### 运行测试

```bash
# 先修改 os_test.conf 中的 mask，然后注入镜像
make -C os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt CONF_FILE=../os_test.conf

# 用当前配置运行 QEMU
cd os && make rv64-run
cd os && make la64-run

# 带详细日志
cd os && LOG=info make rv64-run
cd os && LOG=debug make rv64-run
```

测试组 mask（12-bit，配置在 `os_test.conf`）：
```
bit0=basic  bit1=busybox  bit2=lua    bit3=libctest
bit4=iozone bit5=unixbench bit6=iperf bit7=libcbench
bit8=lmbench bit9=netperf bit10=cyclictest bit11=ltp
```

常用 mask：`0x001`（仅 basic）、`0x003`（basic+busybox）、`0xFFF`（全量，提交评测用）。

### 全量测试

```bash
# 一键：编译双架构内核 + 解压镜像 + 串行 QEMU + 评分 + 留档
python3 scripts/run_full_test.py
```

该脚本会依次完成：`make all` → 解压 sdcard 镜像 → 串行 rv64/la64 QEMU（各 10 分钟超时）→ `judge/run_parse.py` 评分 → 终端汇总 + 存档至 `testresult/archive_{timestamp}/`。

## 架构总览

### 双架构硬件抽象层

`os/src/hal/` 定义 HAL trait（`boot`、`mmio`、`timer`、`context_switch`），两个实现在 `hal/arch/riscv/` 和 `hal/arch/loongarch64/`。架构相关代码（页表、陷阱处理、上下文切换汇编、链接脚本）隔离在这两个目录中。

### 启动流程

```
QEMU → OpenSBI (M-mode) → entry.asm (S-mode)
  → rust_main():
      console::init() → mm::init() → drivers::init()
      → fs::init() → net::init() → task::init() → run_tasks()
```

`task::init()` 从根文件系统加载 `initproc` ELF。`initproc` 读取 `/os_test.conf` 并执行测试二进制，通过退出码报告通过/失败。

### 系统调用分发

`syscall/mod.rs` 中一个扁平的 `match`（约 100+ 分支）将 `SYSCALL_XXX` 常量映射到处理函数：

| 分组 | 模块 | 主要 syscall |
|------|------|-------------|
| 文件 I/O | `syscall/fs.rs` | read, write, openat, close, lseek, getdents64 |
| 网络 | `syscall/net.rs` | socket, bind, connect, sendto, recvfrom, accept |
| 进程 | `syscall/process.rs` | clone, execve, exit, wait4, kill, mmap |
| 信号 | `syscall/process.rs` | sigaction, sigprocmask, sigreturn |
| 时间 | `syscall/fs.rs` | clock_gettime, nanosleep |
| 轮询 | `syscall/fs.rs` | pselect6, ppoll（实现于 `fs/poll.rs`） |

返回值约定：成功返回 `isize >= 0`，失败返回负 errno（如 `-11` 表示 `EAGAIN`）。

### I/O 阻塞抽象

两层设计：
- **`wait_io_core(f, nonblock)`** — 循环调用 `f()`，遇 EAGAIN 则 yield CPU、检查信号、重试。用于通用文件 I/O（管道、tty、普通文件）。
- **`wait_io(f, nonblock)`** — 同上，但每次重试前先调用 `NET_INTERFACE.poll()`。用于 socket 操作（accept、connect、sendto、recvfrom）。

Socket 的 `try_xxx` 方法只做单次非阻塞尝试——不 poll、不 yield、不循环。syscall 层用 `wait_io` 包装实现阻塞语义。

### 网络栈

```
用户程序 → syscall/net.rs → Socket trait (net/mod.rs)
  → TcpSocket / UdpSocket / RawSocket (smoltcp 封装)
  → NET_INTERFACE (smoltcp Interface + SocketSet)
  → net/adapter.rs → virtio_net → QEMU
```

`impl_file_for_socket!` 宏自动从 `Socket` trait 实现生成 `File` trait 实现（`read` → `try_recv`，`write` → `try_send`）。

### 内存管理

- **物理内存：** 基于栈的帧分配器，每帧 4KB
- **虚拟内存：** SV39 页表（RISC-V），每个进程独立 `MemorySet`
- **用户内存访问：** `mm/page_table.rs` 中五个 `translated_*` 函数处理用户指针翻译，支持跨页缓冲
- **写时复制：** fork 使用 CoW；MAP_SHARED 页面跳过 CoW，直接共享物理帧
- **TLB：** 所有 PTE 修改后必须执行 `sfence.vma`（riscv）/ `invtlb`（la64）——缺少 TLB 刷新是反复出现的 bug 根源

### 文件系统（当前重构中）

文件系统正在从旧设计迁移到受 DragonOS 启发的分层 VFS 架构。

**旧架构（大部分 syscall 仍在使用）：**
- `File` trait（fd 和 inode 职责混在一起）
- `DirectoryTreeNode` 做路径解析
- `InodeTrait`（与 FAT32 耦合）

**新架构（`os/src/fs/vfs/`，逐步迁移中）：**
```
syscall 层
    ↓
File 结构体（fd 层：offset、flags、mode）
    ↓
IndexNode trait（inode 操作：read_at、write_at、find、create、link、unlink 等）
    ↓
MountFS / MountFSInode（挂载层：跨 FS 路径解析、子挂载点管理）
    ↓
FileSystem trait（具体 FS：root_inode、info、name、super_block）
    ↓
PageCache（状态机：Loading→UpToDate↔Dirty→Writeback→UpToDate）
```

关键新 VFS 文件：
- `os/src/fs/vfs/mod.rs` — 核心类型定义
- `os/src/fs/vfs/mount.rs` — `MountFS`、`MountFSInode`、`MountList` 全局挂载管理
- `os/src/fs/vfs/adapters.rs` — `OldFileIndexNode` 适配器包装旧 `File` trait
- `os/src/fs/vfs/placeholder.rs` — `PlaceholderFS` 桥接旧 FS 到新 `FileSystem` trait
- `os/src/fs/page_cache.rs` — 新 PageCache，含脏页追踪
- `os/src/fs/vfs_old.rs` — 由旧 `vfs.rs` 重命名而来，保持向后兼容
- `os/src/fs/mod.rs` — `vfs_lookup()`、`vfs_lookup_parent()` 等基于新 VFS 层；`VFS_ROOT` 懒静态桥接新旧世界

当前迁移状态（Phase 1-3 完成，Phase 4-6 待做）：
- ✅ Phase 1-3：VFS 核心抽象、MountFS、PageCache、旧 VFS 重命名
- ❌ Phase 4-6：ext4/fat32 适配新 trait、syscall 层迁移、QEMU 验证

## 命名规范

| 模式 | 用途 | 示例 |
|------|------|------|
| `sys_xxx` | syscall 处理函数 | `sys_read`、`sys_sendto` |
| `_xxx` | 内部辅助函数（单次执行，不循环） | `_read`、`_connect` |
| `try_xxx` | 一次非阻塞尝试，返回 `Result` | `try_recv`、`try_send` |
| `socket_xxx` | socket 专用，避免与 `File` 方法名冲突 | `socket_r_ready` |

## 代码修改检查清单

每次内核修改后：
1. `cd os && make rv64-kernel-build-only` — 编译通过
2. 注入测试配置并在 QEMU 中运行相关测试组
3. 如果涉及架构相关代码，另一个架构也要编译
4. 修改记录写入 `WORK_LOG.md`；可复用经验写入 `EXPERIENCE.md`

## 常见踩坑

- **持锁跨越等待点导致死锁** — `task.files.lock()` 或 `task.socket_table.lock()` 跨越 `suspend_current_and_run_next()` 会死锁。正确模式：加锁 → clone Arc → 释放锁 → 执行操作。
- **PTE 修改后必须刷 TLB** — `unmap`、`block_and_ret_mut`、`set_pte_flags` 都需要 `sfence.vma`（riscv）/ `invtlb`（la64）。
- **信号检查必须用 `sigpending.difference(sigmask)`** 而非 `sigpending.is_empty()`——被屏蔽的信号不应导致 `EINTR`。
- **不要在持有 `task.inner` 锁时调用 `has_actionable_signal()`**——内部会获取同一把锁，导致死锁。参考 `pselect`/`wait_io_core` 的模式。
- **ext4 `get_pblock_idx` 对 hole 返回 `Err`**——sparse file 的 hole 是合法状态，读取应返回零填充数据，写入应分配新块。
- **永远不要直接编辑 `lang_items.rs`**——编辑 `lang_items.rs.rv` / `lang_items.rs.la`。
- **rv64 和 la64 编译必须分开运行**——两条路径共享架构生成状态，必须串行；正常编译不会切换 `rustup override`。

## 交流语言

**必须用中文与用户交流。** 代码和注释可使用英文或中文，但面向用户的对话输出始终用中文。

## 代码审查

编辑代码后，定期调用 `kernel-code-reviewer` subagent 对变更进行审查。该 agent 会检查代码质量、潜在 bug、架构一致性和安全风险。

---

每次任务执行完成后，输出 **Happy Coding!** 标记。
