# oskernel2026-mango — AI 开发助手指令

## 项目简介

`#![no_std]` 裸机 Rust 内核，支持 **riscv64** 和 **loongarch64** 双架构，通过 OpenSBI 在 QEMU 上运行。

| 属性 | 值 |
|------|-----|
| 语言 | Rust nightly（双工具链：`nightly-2025-01-18` / `nightly-2024-05-01`） |
| 架构 | `riscv64gc-unknown-none-elf`、`loongarch64-unknown-linux-gnu` |
| syscall | 约 218 个（新增时同步更新本节） |
| 功能 | ext4/fat32/tmpfs/ramfs/procfs、smoltcp TCP/UDP/RAW/Unix、virtio 块/网卡、SV39 虚拟内存、SysV IPC、epoll/eventfd/signalfd/pidfd、POSIX timer |
| 设计参考 | [DragonOS](https://github.com/DragonOS-Community/DragonOS)（VFS/MountFS 架构）+ Linux 6.6 语义 |
| 约束 | **无 `cargo test`/`cargo clippy`** — 裸机内核，唯一验证 = 编译 + QEMU 集成测试 |

---

## 不可违反的规则

1. **Docker 优先** — 所有编译/运行/调试在 Docker 容器内：`make docker`
2. **不要并行编译双架构** — rv64 和 la64 使用不同 nightly 工具链，Makefile 会切换 `rustup override`，并行会竞态。必须分开命令行执行
3. **永远不要直接编辑 `lang_items.rs`** — 编辑 `lang_items.rs.rv` / `lang_items.rs.la` 变体；`user/src/lang_items.rs` 同理
4. **每次修改必须双架构编译验证** — `make rv64-kernel-build-only` + `make la64-kernel-build-only`
5. **修改核心功能后必须 QEMU 测试** — 不要只靠编译通过
6. **修改 PTE 后必须刷新 TLB** — `sfence.vma`（riscv）/ `invtlb`（la64），这是最常见 bug 来源
7. **不要跨越等待点持锁** — 锁 → clone Arc → 释放锁 → 执行操作
8. **不要 workaround** — 从根因解决问题，不做临时绕过
9. **代码修改后必须调用 `skill(name="mango-worklog")`** — 自动更新 `docs/Work_Log.md`（见 §工作日志与知识维护）

---

## 编译与测试

### 日常编译

```bash
make docker                                    # 进入容器
cd os && make rv64-kernel-build-only           # rv64 快速编译
cd os && make la64-kernel-build-only           # la64 快速编译
cd os && make rv64-only                        # rv64 完整（含用户态+镜像）
cd os && make la64-only                        # la64 完整
make all                                       # 根目录双架构全量
```

### 测试镜像

```bash
make testsuits-download
xz -dkc fs-img-dir/sdcard-rv.img.xz > sdcard-rv.img
xz -dkc fs-img-dir/sdcard-la.img.xz > sdcard-la.img
```

### 测试配置

`os_test.conf` 的 `mask` 字段用 12-bit 控制测试组（**不要日常跑全量**）：

```
bit0=basic  bit1=busybox  bit2=lua  bit3=libctest  bit4=iozone  bit5=unixbench
bit6=iperf  bit7=libcbench bit8=lmbench bit9=netperf bit10=cyclictest bit11=ltp
```

常用 mask：`0x001`（basic）、`0x003`（basic+busybox）、`0xFFF`（全量，仅提交评测用）

配置注入镜像：
```bash
make -C os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt    CONF_FILE=../os_test.conf
make -C os conf-inject CONF_ARCH=la64 CONF_BLK_MODE=virt_pci CONF_FILE=../os_test.conf
```

LTP 本地调试：`ltp_runner=inline` + `ltp_include=read01,write01`（提交前恢复为 `suite`）

### 运行测试

```bash
cd os && make rv64-run            # LOG=info 查看 syscall 追踪
cd os && make la64-run
python3 scripts/run_full_test.py  # 全量一键
make docker-test-parallel         # 双架构并行
```

---

## 架构地图

### 启动流程

```
QEMU → OpenSBI (M-mode) → entry.asm (S-mode) → rust_main():
  console::init() → mm::init() → drivers::init() → fs::init()
  → net::init() → task::init() [加载 initproc ELF] → run_tasks()
```

### 系统调用

`syscall/mod.rs` 中扁平 `match` 分发。子模块按领域拆分：

| 模块 | 文件 | 范围 |
|------|------|------|
| 文件 I/O | `syscall/fs.rs` | read/write/openat/close/lseek/getdents64/stat/fcntl |
| 进程 | `syscall/process/{mod,mm,ids,signal,time,ipc,lifecycle,exec}.rs` | clone/execve/exit/wait4/mmap/brk/signal/timer/IPC |
| 网络 | `net/syscall/*.rs` | socket/bind/connect/sendto/recvfrom/accept |
| FD/事件 | `fs/eventpoll.rs`、`fs/eventfd.rs`、`fs/pidfd.rs` | epoll/eventfd/pidfd/signalfd |

**返回值约定**：syscall 处理函数成功返回 `>= 0`，失败返回负 errno（如 `-11` = EAGAIN）。

### 内存管理

- **物理内存**：栈式帧分配器，4KB/帧；`frame_store.rs` 跟踪帧状态用于 swap/zram
- **虚拟内存**：SV39 页表，每进程独立 `MemorySet`；`VmaSet` 管理 VMA；`filemap.rs` 处理 mmap 文件缺页
- **用户内存访问**：`translated_ref/refmut/byte_buffer`、`copy_from_user`、`translated_str`
- **关键约束**：MAP_SHARED 页面不参与 CoW；修改 PTE 后必须 TLB 刷新；`execve`/`clone` 路径用 `try_reserve` 防 OOM
- **OOM 防御**：`alloc()` 三次重试失败后设 `pending_oom_kill`，由 `trap_return()` 安全点发 SIGKILL

### 任务/进程

单核、基于定时器中断的抢占式多任务。调度：`VecDeque<Arc<TaskControlBlock>>` 轮转，默认 nice=0 走 FIFO fast path。

- **TaskControlBlock** — 线程级（调度实体、内核栈、trap context）
- **ProcessControlBlock** — 进程级（地址空间、fd table、信号、PID）
- **信号**：`task/signal/` 子模块（action/delivery/frame/pending/wait）
- **WaitQueue** — 支持 futex、epoll、eventfd 等阻塞原语
- **Completion** — 单次通知原语

### 文件系统（VFS）

分层设计：`File`（fd 层）→ `IndexNode`（inode 层）→ `FileSystem`（FS 层）→ `MountFS`（挂载层）→ `PageCache`

| FS 类型 | 模块 | 说明 |
|---------|------|------|
| ext4 | `fs/ext4/` | 主力文件系统，含 extent 树、稀疏文件 |
| FAT32 | `fs/fat32/` | 引导/EFI 分区支持 |
| tmpfs | `fs/tmpfs/` | 无大小限制的临时内存 FS |
| ramfs | `fs/ramfs/` | 物理页支持的内存 FS（`/dev/shm` 等） |
| procfs | `fs/procfs/` | `/proc` 伪文件系统（含 `/proc/[pid]/status/maps/fd`） |
| devfs | `fs/dev/` | 设备文件（null/zero/urandom/tty/pipe/pty/rtc） |

**PageCache**：状态机（Loading→UpToDate↔Dirty→Writeback），LRU 回收（高水位 64MB，批量 64 页）。`reclaim.rs` 周期性后台回收。

**MountFS**：包装层，处理跨 FS 边界 lookup 和挂载传播（shared/private/slave）。

### 网络栈

```
syscall → Socket trait → TcpSocket/UdpSocket/RawSocket/UnixSocket
  → NET_INTERFACE (smoltcp Interface + SocketSet)
  → adapter.rs → virtio_net → QEMU
```

**I/O 阻塞抽象**：
- `try_xxx` — 单次非阻塞尝试，不 poll/yield/循环
- `wait_io` — socket 操作阻塞包装（每次重试前 poll）
- `wait_io_core` — 通用文件 I/O 阻塞包装（不 poll）
- 非阻塞路径（MSG_DONTWAIT）在 `try_xxx` 前必须 `try_poll()` 防 livelock

### IPC / 同步

| 机制 | 文件 | 说明 |
|------|------|------|
| SysV IPC | `syscall/process/ipc.rs` | msgget/semget/shmget 全套（msg/sem/shm） |
| futex | `task/futex.rs` | 快速用户空间互斥 |
| epoll | `fs/eventpoll.rs` | 可扩展 I/O 事件通知 |
| eventfd | `fs/eventfd.rs` | 事件计数 fd |
| signalfd | `syscall/process/signal.rs` | fd 方式接收信号 |
| pidfd | `fs/pidfd.rs` | 进程 fd（open/send_signal/getfd） |

### HAL

`hal/` 目录提供硬件抽象层，将架构相关代码（陷阱处理、页表操作、TLB 管理、控制寄存器）从架构无关代码中分离。支持多平台（rv64: QEMU/K210/fu740、la64: QEMU/2k1000）。

---

## 编码规范

### 命名规则

| 模式 | 用途 | 示例 |
|------|------|------|
| `sys_xxx` | syscall 处理函数 | `sys_read`、`sys_sendto` |
| `_xxx` | 内部辅助函数（单次执行） | `_read`、`_connect` |
| `try_xxx` | 一次非阻塞尝试，返回 `Result` | `try_recv`、`try_send` |
| `socket_xxx` | socket 专用，避免与 `File` 冲突 | `socket_r_ready` |

### 返回值编码

| 层 | 成功 | 错误 |
|----|------|------|
| `File::read()/write()` | `usize`（字节数） | `usize`（`-(errno as isize) as usize`） |
| `Socket::try_recv()/try_send()` | `Ok(isize)` | `Err(SyscallErr::XXX)` |
| syscall 处理器 | `isize`（>= 0） | `isize`（负 errno） |

### 死锁预防

- 锁 → clone Arc → 释放锁 → 执行操作
- 信号检查必须在释放 `task.inner` 锁后调用 `has_actionable_signal()`
- `NET_INTERFACE.xxx_socket()` 闭包内保持简短

---

## 关键易错点

### 内存

- **TLB 刷新**：所有 PTE 修改操作（`unmap`/`block_and_ret_mut`/`set_pte_flags`）后必须 `sfence.vma`/`invtlb`
- **MAP_SHARED**：不参与 CoW，fork 时恢复 W 权限，缺页只恢复 W
- **OOM**：`execve`/`clone` 路径 Vec 扩容必须 `try_reserve` 返回 `ENOMEM`

### 网络

- Socket 就绪检查前必须 `NET_INTERFACE.poll()`（`socket_r_ready`/`socket_w_ready`）
- 非阻塞路径 `try_xxx` 前必须 `NET_INTERFACE.try_poll()` 防 livelock
- `impl_file_for_socket!` 宏自动生成 `File` trait（`read` → `try_recv`，`write` → `try_send`）

### 文件系统

- ext4 hole（稀疏文件）→ `get_pblock_idx` 返回 `Err`，`read_at` 填零，`write_at` 分配新块
- ext4 `binsearch_extent` 不验证覆盖范围 → 调用者必须检查 lblock 在 extent 范围内
- PageCache invalidate 时不能持有 inode 锁（`TicketMutex` 不可重入）

### 错误码（Linux 语义）

- setsockopt 未知 level/optname → `ENOPROTOOPT(92)`，不是 `EOPNOTSUPP(95)`
- socketpair 非 AF_UNIX → `EPROTONOSUPPORT(93)`，不是 `EAFNOSUPPORT(97)`
- `Socket::alloc` 未知 domain → `EAFNOSUPPORT(97)`，不是 `EINVAL(22)`
- getpeername NULL addr → 先验证参数返回 `EFAULT`，再检查连接状态
- mmap 非匿名映射坏 fd → `EBADF` 优先于其他校验
- 跨进程 VM 访问 → 先做权限检查（`EPERM`），再访问远程地址（`EFAULT`）
- RISC-V 未对齐 addrlen → 需显式检查 `addrlen % 4 != 0`，硬件不报错

### 编译

- `cargo check` 必须从 `os/` 目录用 Makefile 目标，不能在根目录
- `Vec` 重复定义 → 检查是否同时 `use alloc::vec;` 和 `use alloc::vec::Vec;`
- lang_items 不匹配 → 编辑 `.rv`/`.la` 变体，不编辑 `lang_items.rs`

---

## 新增功能

### 新增 Syscall

```rust
// 1. syscall/syscall_id.rs: 添加 pub const SYSCALL_MY_FEATURE: usize = NNN;
// 2. 对应模块中实现: pub fn sys_my_feature(...) -> isize { ... }
// 3. syscall/mod.rs: 注册到 dispatch match 分支
```

### 新增 Socket 类型

```rust
impl Socket for MySocket { /* try_recv, try_send, socket_type */ }
impl_file_for_socket!(MySocket);
// 在 net/mod.rs 的 Socket::alloc() 中接入
```

### 新增块设备/网卡驱动

实现 `drivers/block/mod.rs` 的 `BlockDevice` trait 或 `drivers/net/mod.rs` 的 `NetworkDevice` trait。

### 验证清单

- [ ] `make rv64-kernel-build-only` ✅
- [ ] `make la64-kernel-build-only` ✅
- [ ] QEMU 启动不 panic
- [ ] 相关测试组通过
- [ ] 更新 `docs/Work_Log.md`（按 mango-worklog Skill 格式）

---

## 工作日志与知识维护

### 前置参考（调试前先读）

调试性能退化、非确定性 bug、或遇到可疑模式时，先读取以下已沉淀的经验：

- **性能退化调试工作流** → `.agents/skills/mango-worklog/references/harness-patterns.md`（§渐进性能退化调试方法论）
- **常见调试模式和技巧** → `.agents/skills/mango-worklog/references/debugging-patterns.md`

这些文件记录了之前跨多个对话验证过的调试策略和修复模式，可以帮助快速定位问题类型并避免重复试错。

### 自动 Worklog

每次代码修改完成后，**必须调用 `skill(name="mango-worklog")`** 加载工作日志指令并执行。该 Skill 会读取当前对话上下文中的修改内容，自动按格式更新 `docs/Work_Log.md`，并判断是否需要沉淀经验到 `references/`。不要等待用户提示——这是强制性规则。

格式：日期戳条目 → 涉及文件 → 验证结果 → 备注。

### 经验沉淀

发现**可能跨对话复用**的 bug 模式或调试技巧时，追加到对应 reference 文件：

- Bug 根因 → 修复模式 → `.agents/skills/mango-worklog/references/harness-patterns.md`
- 调试技巧 → `.agents/skills/mango-worklog/references/debugging-patterns.md`

注意：已经在本文档「关键易错点」中覆盖的内容无需重复沉淀。

### 架构变更

修改核心架构（模块拆分、trait 变更、新子系统）时，必须同步更新本文档对应章节。

### LTP 测试详情

具体的 LTP 测试跳过策略、逐用例适配记录见 `Doc/Work_Log.md`。本文档只保留通用级别的规则和易错点。

---

## 交流语言

AI 助手使用**中文**与用户交流。代码、注释、commit message 使用英文或中文均可。

## 参考资源

- 设计蓝本：[DragonOS](https://github.com/DragonOS-Community/DragonOS)
- 详细测试策略：`Doc/Work_Log.md`、`Doc/LTP_BOTTOM_UP_GUIDE.md`
- VFS 迁移历史：`Doc/vfs-migration-plan.md`
- 调试技巧：`how-to-run.md`
