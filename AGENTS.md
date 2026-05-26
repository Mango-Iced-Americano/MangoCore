# oskernel2026-mango — AI 开发助手指令

## 项目简介

`#![no_std]` 裸机 Rust 内核，支持 **riscv64** 和 **loongarch64** 双架构，通过 OpenSBI 在 QEMU 上运行。

| 属性 | 值 |
|------|-----|
| 语言 | Rust nightly（双工具链：`nightly-2025-01-18` / `nightly-2024-05-01`） |
| 架构 | `riscv64gc-unknown-none-elf`、`loongarch64-unknown-linux-gnu` |
| 功能 | ext4/fat32、smoltcp TCP/UDP/RAW、virtio 块/网卡、SV39 虚拟内存、zram、POSIX syscall |
| 设计蓝本 | [DragonOS](https://github.com/DragonOS-Community/DragonOS)（VFS/Endpoint/MountFS 架构） + Linux 6.6 语义 |
| 约束 | **无 `cargo test`/`cargo clippy`** — 裸机内核，唯一验证 = 编译 + QEMU 集成测试 |

---

## 关键规则（每次操作前必读）

1. **Docker 优先** — 宿主机无交叉编译工具链，所有编译/运行/调试必须在 Docker 容器内：`make docker`
2. **不要并行编译双架构** — rv64 和 la64 使用不同 nightly 工具链，Makefile 会切换 `rustup override`，并行会竞态
3. **永远不要直接编辑 `lang_items.rs`** — 编辑 `lang_items.rs.rv` / `lang_items.rs.la` 变体；`user/src/lang_items.rs` 同理
4. **每次修改必须双架构编译验证** — `make rv64-kernel-build-only` + `make la64-kernel-build-only`
5. **修改核心功能后必须 QEMU 测试** — 不要只靠编译通过
6. **修改 PTE 后必须刷新 TLB** — `sfence.vma`（riscv）/ `invtlb`（la64），这是最常见的 bug 来源
7. **不要跨越等待点持锁** — 锁 → clone Arc → 释放锁 → 执行操作

---

## 编译与测试

### 快速编译（日常迭代）

```bash
# 进入 Docker 容器
make docker

# 仅编译内核（最快，推荐日常开发）
cd os && make rv64-kernel-build-only
cd os && make la64-kernel-build-only

# 完整编译（内核 + 用户态 + 镜像）
cd os && make rv64-only
cd os && make la64-only

# 项目根目录双架构全量编译
make all
```

### 测试镜像准备

```bash
# 下载测试镜像
make testsuits-download
xz -dkc fs-img-dir/sdcard-rv.img.xz > sdcard-rv.img
xz -dkc fs-img-dir/sdcard-la.img.xz > sdcard-la.img
```

### 配置测试范围

`os_test.conf` 的 `mask` 字段用 12-bit 控制测试组（**不要日常跑全量**）：

```
bit0=basic    bit1=busybox   bit2=lua       bit3=libctest
bit4=iozone   bit5=unixbench bit6=iperf     bit7=libcbench
bit8=lmbench  bit9=netperf   bit10=cyclictest bit11=ltp
```

常用 mask：`0x001`（basic）、`0x003`（basic+busybox）、`0xFFF`（全量，仅提交评测用）

修改 `os_test.conf` 后注入镜像：

```bash
make -C os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt    CONF_FILE=../os_test.conf
make -C os conf-inject CONF_ARCH=la64 CONF_BLK_MODE=virt_pci CONF_FILE=../os_test.conf
```

LTP 本地调试可临时用 `ltp_runner=inline` + `ltp_include=read01,write01`（提交前恢复为 `ltp_runner=script`）。

### 运行测试

```bash
cd os && make rv64-run            # 带日志: LOG=info make rv64-run
cd os && make la64-run

# 全量一键测试（编译 + QEMU + 评分 + 存档）
python3 scripts/run_full_test.py

# 双架构并行测试
make docker-test-parallel
```

---

## 架构概览

### 启动流程

```
QEMU → OpenSBI (M-mode) → entry.asm (S-mode) → rust_main():
  console::init() → mm::init() → drivers::init() → fs::init()
  → net::init() → task::init() [加载 initproc ELF] → run_tasks()
```

### 系统调用分发

`syscall/mod.rs` 中扁平 `match`（~100+ 分支），`SYSCALL_XXX` → 处理函数。

| 分组 | 模块 | 主要 syscall |
|------|------|-------------|
| 文件 I/O | `syscall/fs.rs` | read, write, openat, close, lseek, getdents64 |
| 网络 | `net/syscall/*.rs` | socket, bind, connect, sendto, recvfrom, accept |
| 进程 | `syscall/process.rs` | clone, execve, exit, wait4, kill, mmap, brk |
| 信号 | `syscall/process.rs` | sigaction, sigprocmask, sigreturn |
| 时间 | `syscall/fs.rs` | clock_gettime, nanosleep |
| 轮询 | `syscall/fs.rs` | pselect6, ppoll（实现于 `fs/poll.rs`） |

### I/O 阻塞抽象（wait_io / wait_io_core）

两层设计——关键区分：

- **`wait_io_core(f, nonblock)`** — 循环调用 `f()`，遇 EAGAIN 则 yield CPU + 检查信号 + 重试。不调用 `NET_INTERFACE.poll()`。用于通用文件 I/O（管道/tty/普通文件）。
- **`wait_io(f, nonblock)`** — 同上，但每次重试前先调用 `NET_INTERFACE.poll()`。用于 socket 操作（accept/connect/sendto/recvfrom）。

Socket 的 `try_xxx` 方法只做单次非阻塞尝试——不 poll、不 yield、不循环。syscall 层用 `wait_io` 包装实现阻塞语义。

### 网络栈

```
syscall → Socket trait → TcpSocket/UdpSocket/RawSocket/UnixSocket
  → NET_INTERFACE (smoltcp Interface + SocketSet)
  → adapter.rs → virtio_net → QEMU
```

`impl_file_for_socket!` 宏自动从 `Socket` trait 生成 `File` trait（`read` → `try_recv`，`write` → `try_send`）。

**设计规则：**
- `try_xxx` 不 poll / 不 yield / 不循环
- `poll()` 仅由 `wait_io()` 和 `socket_r_ready()`/`socket_w_ready()` 调用
- 非阻塞 socket 路径（recvfrom/sendto 带 MSG_DONTWAIT）必须在 `try_xxx` 前调用 `NET_INTERFACE.try_poll()`，防止 livelock

### 内存管理

- **物理内存**：基于栈的帧分配器，4KB/帧
- **虚拟内存**：SV39 页表，每进程独立 `MemorySet`
- **用户内存访问**：`translated_ref`、`translated_refmut`、`translated_byte_buffer`、`copy_from_user`、`translated_str`
- **关键约束**：
  - MAP_SHARED 页面不参与 Copy-on-Write（fork 时恢复源页表 W 权限，缺页时只恢复 W 不做 CoW，mmap 时预分配物理帧）
  - **修改 PTE 后必须 TLB 刷新**（`unmap`、`block_and_ret_mut`、`set_pte_flags` 等所有 PTE 修改操作）
  - `execve`/`clone` 路径 Vec 扩容必须用 `try_reserve` 并返回 `ENOMEM`，避免堆耗尽 panic
- **OOM 防御**：`alloc()` 三次重试失败后设置 `pending_oom_kill`，由 `trap_return()` 安全点发送 SIGKILL

### 任务/进程

单核、基于定时器中断的抢占式多任务。状态：`Ready` → `Running` → `Interruptible` / `Zombie`。调度：`VecDeque<Arc<TaskControlBlock>>` 轮转。

### 文件系统（VFS 迁移中）

**当前状态：Phase 1-4 ✅ 完成，Phase 5-6 待做。**

已完成（2026-05-13）：ext4、FAT32、所有设备文件（Null/Zero/Urandom/Tty/Pipe）、SocketFile 均原生实现 `IndexNode` + `FileSystem` trait。`OldFileIndexNode` 适配器、`PlaceholderFS`、`vfs_old.rs`、`FilePageCacheBackend` 全部已删除。

**待完成（Phase 5-6）：** `TaskControlBlock` 迁移到新 `vfs::File`/`vfs::FdTable` → 所有 fd 操作 syscall 迁移 → 删除旧 VFS 文件。

**新旧对照：**
```
旧: File trait (职责混乱) + InodeTrait (FAT32耦合) + DirectoryTreeNode
新: vfs::File (fd层) → IndexNode trait (inode层) → FileSystem trait (FS层) → MountFS (挂载层) → PageCache
```

**模块关键文件：**
- `os/src/fs/vfs/mod.rs` — 类型定义 / `os/src/fs/vfs/file.rs` — fd 层 / `os/src/fs/vfs/index_node.rs` — inode 层
- `os/src/fs/vfs/mount.rs` — MountFS 挂载层 / `os/src/fs/vfs/file_system.rs` — FS trait
- `os/src/fs/mod.rs` — `VFS_ROOT` + `vfs_lookup()` / `os/src/fs/page_cache.rs` — 新 PageCache
- `os/src/fs/directory_tree.rs` — 旧 VFS 过渡层（待 Phase 5-6 删除）

详细迁移计划见 `doc/vfs-migration-plan.md`。

---

## 编码规范

### 命名规则

| 模式 | 用途 | 示例 |
|------|------|------|
| `sys_xxx` | syscall 处理函数 | `sys_read`、`sys_sendto` |
| `_xxx` | 内部辅助函数（单次执行，不循环） | `_read`、`_connect` |
| `try_xxx` | 一次非阻塞尝试，返回 `Result` | `try_recv`、`try_send` |
| `socket_xxx` | socket 专用，避免与 `File` 方法名冲突 | `socket_r_ready` |

### 返回值编码

| 层 | 成功 | 错误 |
|----|------|------|
| `File::read()/write()` | `usize`（字节数） | `usize`（`-(errno as isize) as usize`） |
| `Socket::try_recv()/try_send()` | `Ok(isize)` | `Err(SyscallErr::XXX)` |
| syscall 处理器 | `isize`（>= 0） | `isize`（负 errno，如 `-11` = EAGAIN） |

### 死锁预防

- 锁 → clone Arc → 释放锁 → 执行操作（不要跨越等待点持锁）
- 信号检查必须在释放 `task.inner` 锁后调用 `has_actionable_signal()`（内部会获取同一锁）
- `NET_INTERFACE.xxx_socket()` 使用内部 `Mutex`，保持闭包简短

---

## 常见踩坑

### 编译

| 问题 | 修复 |
|------|------|
| `Vec` 重复定义 | 不要同时 `use alloc::vec;` 和 `use alloc::vec::Vec;` |
| `lang_items` 不匹配 | 编辑 `.rv` / `.la` 变体，不编辑 `lang_items.rs` |
| rv64/la64 串行编译失败 | 两个架构必须分开命令行运行，不要 `&&` 串接 |
| `cargo check` 在根目录失败 | 始终在 `os/` 目录用 Makefile 目标 |

### 内存管理

| 问题 | 根因 | 修复 |
|------|------|------|
| MAP_SHARED 父子进程数据不一致 | fork 时 CoW 破坏共享语义 | MAP_SHARED 页面跳过 CoW，fork 时恢复 W 权限 |
| `mlock201` 中 `mlock2(0)` 锁 1 页却显示 8 页 present | 匿名 `MAP_SHARED` mmap 直接安装整段 PTE，`mincore()` 无法区分未触达页 | 匿名 shared 可以预分配共享 frame 保留 fork 语义，但 PTE 要懒安装；首次访问/`mlock` fault-in 再映射现有 frame |
| `mlock05` 找不到 `/proc/self/smaps` 或 `Rss` 多 1 页 | procfs 缺少 smaps，且 `mlock()` 锁定子区间后 VMA 合并导致 smaps 粗粒度统计误计旁边页 | 提供 `/proc/<pid>/smaps` 最小实现，并按 locked 页边界拆分输出段；`Rss`/`Locked` 只统计该段 |
| unmap 后读到 PTE 残留值 | 未刷新 TLB，CPU 仍用旧缓存 | **所有 PTE 修改后 `sfence.vma` / `invtlb`** |
| heap allocator panic | 内核堆耗尽 | `try_reserve` 防御 + OOM killer |
| la64 `futex_cmp_requeue01`/clone 压测在 `clone()` 触发 heap fatal | la64 每个 task kernel stack 是堆上的 `Vec<u8>`；128KB 栈遇到 1000 waiter 会吃满 128MB kernel heap | la64 task stack 保持 64KB，并把 `BOOT_STACK_SIZE` 和 `KERNEL_STACK_SIZE` 分离；boot stack 仍保留 128KB |
| `brk`/`mmap` 返回意外值 | 堆/mmap 区域冲突 | 检查 `program_break` 边界 |
| la64 在大量匿名页 fault 时 `frame_alloc`/`memset` AddressError，常见停在物理 `0xb0000000` | QEMU la64 DTB 的高段 RAM 是 `memory@80000000` + `0x30000000`，旧配置错误地按 1GB 连续 RAM 分配到 `0xc0000000` | la64 `MEMORY_SIZE` 必须匹配 DTB 的 `0x30000000`；不要跨真实 RAM 结尾分配物理页 |
| la64 `getrusage03` 变成 `TCONF: needs at least 512MB MemAvailable` 或 30s timeout | la64 高段 RAM 只有 768MB，256MB 静态 kernel heap 挤占可用页帧；页帧清零被降成 byte-wise `memset`，大批量 fault 很慢 | la64 kernel heap 保持在实际需要范围内；页帧清零用 64-bit store 循环，避免每页 4096 次 byte store |
| `mmap18` MAP_GROWSDOWN 栈增长失败 | 页故障只查已覆盖 VMA，未在 guard page fault 时扩展 grow-down VMA | fault 地址位于 grow-down VMA 下方且不碰撞/不进入 stack_guard_gap 时，先下扩 VMA 起点再走懒分配 |
| `munlockall01`/`mlock203` 读取 `VmLck` 失败或重复锁页计数异常 | `/proc/<pid>/status` 缺 `VmLck`，或 mlock 路径未维护 ABI 可见锁页状态 | 在地址空间维护页级 locked 集合，`mlock/mlockall` 设置、`munlock/munlockall/munmap` 清除，`VmLck` 汇总 locked 用户页 |
| `mmap14` 中 `MAP_LOCKED` 后 `VmLck=0` | `mmap()` 保留了 `MAP_LOCKED` VMA flag，但没有同步更新页级 locked 集合 | `MAP_LOCKED` 建图后必须把映射范围计入 locked 页；避免和未锁匿名 VMA 合并导致统计丢失 |

### 文件系统

| 问题 | 根因 | 修复 |
|------|------|------|
| ext4 sparse file 导致 OOM | `get_pblock_idx` 对 hole 返回垃圾地址 | hole 返回 `Err`，`read_at` 填零，`write_at` 分配新块 |
| ext4 extent 搜索错误 | `binsearch_extent` 不验证覆盖范围 | 调用者必须检查 `lblock` 在 extent 范围内 |

### 网络栈

| 问题 | 根因 | 修复 |
|------|------|------|
| `pselect` 永远挂起 | `socket_r_ready()` 缺少 `NET_INTERFACE.poll()` | poll 后再检查 socket 状态 |
| `connect` 永不返回 | TCP 握手失败，重试循环阻塞 | 使用 `try_connect` + `wait_io` |
| 非阻塞 recv 导致 livelock | 紧循环 EAGAIN 阻止定时器中断 | 非阻塞路径 `try_xxx` 前先 `try_poll()` |
| setsockopt 未知 level | 返回 EOPNOTSUPP(95) | Linux 语义：未知 level/optname 统一返回 ENOPROTOOPT(92) |
| socketpair 非 AF_UNIX | 返回 EAFNOSUPPORT | Linux 语义：返回 EPROTONOSUPPORT(93) |
| getpeername NULL addr | EFAULT 被 ENOTCONN 覆盖 | 必须先验证参数再检查连接状态 |

### 时间/定时器

| 问题 | 根因 | 修复 |
|------|------|------|
| `leapsec01` 报 `adjtimex status ... not set` | `adjtimex/clock_adjtime` 只返回快照，未保存 `ADJ_STATUS` 等可调字段 | 保存 `TimexState`，按 `ADJ_*` 更新并在后续 snapshot 回填 |

### LTP 扫描取舍

| 问题 | 根因 | 修复 |
|------|------|------|
| `unshare01` 全部 ENOSYS | `unshare(2)` 未接入通用 syscall 表 | 最小支持 `CLONE_FILES`/`CLONE_FS` 拷贝，`CLONE_NEWNS` 在当前全局 mount tree 下兼容返回成功 |
| `unshare01.sh` 持续 5 分钟 shell 噪声 | LTP standalone helper 不通过标准 `tst_run` 运行 | broad scan 中跳过 |
| `umip_basic_test` TBROK/TCONF | x86_64-only UMIP 测试 | broad scan 中跳过 |
| `umask01` 大量 mode/return TFAIL | 文件创建 umask 语义未实现，涉及 fs 权限路径 | fs 适配窗口前先跳过，避免和 VFS 工作冲突 |
| `utsname02/03` sethostname ENOSYS | syscall 161 未注册，hostname 固定写死在 `uname` | 用进程共享 `UtsNamespace` 保存 nodename/domainname，`sethostname`/`setdomainname` 更新当前 UTS namespace |
| `utsname04` 非 root `CLONE_NEWUTS` 未拒绝 | `clone` 未检查 UTS namespace 权限 | 非 root 使用 `CLONE_NEWUTS` 返回 `EPERM` |
| `waitid11` SIGKILL 子进程被报告为正常退出 | `waitid` siginfo 总是填 `CLD_EXITED` | 按 wait status 低 7 位区分 `CLD_KILLED/CLD_DUMPED` |
| `waitid10` 先被跳过后 `si_code` 和 core-dump 语义异常 | 缺 `/proc/sys/kernel/core_pattern`，且 `RLIMIT_CORE` 更新不可见、WCOREDUMP 只按信号号硬编码 | 补 core_pattern 最小 sysctl；保存/继承 `RLIMIT_CORE`；只有 core-default 信号且 dumpable、core limit > 0 时设置 WCOREDUMP |
| `waitid07/08`、`waitpid08/13` stopped/continued 用例失败 | SIGSTOP 只让任务睡眠，没有给父进程留下可 wait 的 stop/continue 状态；`waitid` 误要求必须带 `WEXITED` | 进程记录 stopped/continued 事件，`wait4/waitid` 按 `WSTOPPED/WCONTINUED/WNOWAIT` 返回 Linux wait status / `CLD_STOPPED` / `CLD_CONTINUED` |
| `userns*`、`utime*`、`vmsplice*`、`wireguard*`、`zram*` 等后段失败 | user namespace/procfs、fs timestamp、pipe splice、net/module 环境缺失 | broad scan 中按家族窄跳过，后续专项处理 |
| `aio*`、`chdir01`、`dio*`、`data*`、`dccp*`、`dhcp*`、`dctcp*` 等前段噪声 | libaio 用户态环境、外部测试设备、fs direct-io 压测、standalone helper、网络协议矩阵 | broad scan 中按家族/精确项跳过，保留普通核心 syscall 用例 |
| `clone08` musl 失败但 glibc 通过 | musl `clone()` wrapper 对 `CLONE_THREAD/CLONE_CHILD_CLEARTID` 组合直接 `EINVAL`，未进入内核；glibc 路径验证内核线程 clone 可用 | broad scan 中仅跳过 musl `clone08`，保留 glibc |
| `acct*`、`add_key*`、`bpf_*`、`binfmt_misc*`、`broken_ip*`、`chroot*` 等早段扫描噪声 | process accounting/keyring/BPF/binfmt/module、raw network、fs chroot 等当前非核心或 fs/net 环境缺失 | broad scan 中窄跳过这些阻塞项，保留 `brk*`、`capget/capset`、`chdir04` 等已通过核心用例 |
| `clock_gettime03/04`、`cve-*`、`dirtyc0w*`、`dirtypipe`、`crypto_user*`、`dns*`、`doio`、`du01.sh`、`dynamic_debug01.sh` 等中段噪声 | time namespace 配置、clock 性能阈值、procfs/CVE、pipe/fs、crypto netlink、DNS/net、I/O 压测、debugfs 环境缺失 | broad scan 中先跳过，后续如专攻性能/time/fs/procfs/net 再单独恢复 |

### 信号/进程

| 问题 | 根因 | 修复 |
|------|------|------|
| nanosleep 唤醒后死锁 | 持 `task.inner` 锁调 `has_actionable_signal()` | 释放锁后再调 |
| 被屏蔽信号导致 EINTR | 用 `is_empty()` 检查信号 | 用 `sigpending.difference(sigmask)` |
| execve 后 OOM | 新旧内存集同时存在 | `load_elf` 开头 `recycle_data_pages()` |
| execve 映射只读 ELF 段 panic | `map_elf` 把内核临时文件映射 fast path 当成必然成功并 `unwrap()`，失败后还可能留下部分用户映射 | fast path 只作为优化：严格检查页对齐/大小，失败回退 copy load，并保证跨地址空间映射失败时回滚 |
| job-control stop 后父进程 wait 不到状态 | 默认 stop 信号没有记录进程级状态，也没有唤醒父进程 `child_exit_wait` | SIGSTOP/SIGTSTP/SIGTTIN/SIGTTOU 标记 stopped；SIGCONT 标记 continued 并唤醒父进程；wait 消费事件但不回收子进程 |
| `rt_sigaction03` invalid sigsetsize 误成功 | `rt_sigaction` 分发忽略第 4 参数 `sigsetsize` | syscall 层传入并校验 `sigsetsize == sizeof(kernel sigset_t)` |
| `sigaction01` 中 `SA_RESETHAND` 清掉 `SA_SIGINFO` | 信号投递后直接删除 action，handler 内 `sigaction(..., oldact)` 读到默认空 flags | `SA_RESETHAND` 只把 handler 重置为 `SIG_DFL`，保留 flags/mask/restorer 供 oldact 查询 |
| `rt_sigqueueinfo01` pthread checkpoint 超时 | TID 目标信号立即唤醒 futex/checkpoint waiter，导致 LTP 后续 `TST_CHECKPOINT_WAKE` 找不到等待者 | 对非 leader TID 先入线程 pending，不主动信号唤醒；由测试的 futex wake 释放后再处理 pending signal |
| LTP cgroup 脚本触发 `syslog/klogctl` panic 或 `syslog12` 非 root 失败 | `sys_syslog()` 对 READ_CLEAR/CLEAR/console/size action 留了 `todo!()`，用户拷贝 `unwrap()`，且缺少权限检查 | 所有 action 返回稳定 errno/成功值，用户指针错误返回错误码；除 READ_ALL/SIZE_BUFFER 外需 root 或 `CAP_SYS_ADMIN/CAP_SYSLOG` |
| LTP nice05 动态 CPU clock 失败 | glibc 使用负数动态 clock id，且相邻 nice 在单核调度中差异太小 | 解码动态 CPU clock id，并让 `CPUCLOCK_SCHED`/调度统计体现 nice |
| `futex_cmp_requeue01` 1000 waiter 通过 Test 5 后 30s timeout | nice-aware scheduler 每次 `fetch_task()` 都全队列扫描，默认 nice=0 的大量 waiter 变成 O(n²) 调度开销 | ready 队列记录非默认 nice 数量；全 nice=0 时走 FIFO fast path，非默认 nice 存在时才扫描；`wait(-1)` 大量回收用 `swap_remove` |
| glibc pthread cancel 缺库 | 测试镜像缺少架构匹配 `libgcc_s.so.1` | initproc 写入 `/glibc/lib/libgcc_s.so.1` 后再链接到 `/lib` |
| pidfd_send_signal 对 `/proc/<pid>` fd 返回 EBADF | procfs inode 被 MountFSInode 包装，且 `/proc/<pid>` 目录未记录 pid | 解包 MountFSInode 后识别 LockedProcInode，并在 pid 目录 `extra_data` 保存 pid |
| pidfd_getfd 已退出目标返回 EBADF | wait 后 zombie 进程仍可被 registry 查到，但 fd table 已关闭 | 目标进程 zombie 时按 Linux 语义返回 `ESRCH` |
| pidfd_open04 waitid(P_PIDFD) ENOSYS | 缺少 `waitid(95)` 分发和 P_PIDFD 最小语义 | 支持 `P_PIDFD + WEXITED`，非阻塞未退出返回 `EAGAIN`，退出后回收子进程 |
| futex_wait05 短 timeout 超时过长 | 单线程短等待走完整 wait queue/调度路径，QEMU 下固定增加数毫秒 | 仅对单线程、ready 队列为空、短 timeout 使用硬件时钟短轮询；仍保持值不匹配优先返回 `EAGAIN` |
| `getrusage03` 读 `/proc/self/status` TBROK、`RUSAGE_CHILDREN.ru_maxrss=0` 或尾部卡住 | status 缺 `VmSwap`/RSS 字段，`ru_maxrss` 未按 resident high-water 更新，wait 聚合漏掉已回收后代资源；la64 `rt_sigaction(sigsetsize=16)` 被误拒导致 `SIGCHLD=SIG_IGN` 未生效 | status 补 `VmSwap`/RSS，`getrusage`/进程退出更新 `ru_maxrss`，wait 回收时合并子进程自身和后代 `rusage`；`rt_sigaction` 接受 libc 传入的 >=8 字节 sigsetsize，显式忽略 SIGCHLD/`SA_NOCLDWAIT` 时自动摘除子进程并从 registry 移除 |
| process_vm_readv03 多 iovec 数据错误 | 大量同页一字节 iovec 被长期保存成多个 `&mut` 页切片，存在别名风险 | 跨进程 iovec 拷贝逐页 chunk 即时复制，不持久保存页切片 |
| process_vm01 无权限场景返回 EFAULT | 先访问目标进程坏地址，后做 ptrace/credential 权限判断，errno 优先级反了 | 读取/写入远程 VM 前先按 uid/gid/suid/sgid、dumpable 和 `CAP_SYS_PTRACE` 检查访问权限，无权限返回 `EPERM` |
| LTP `msg*` 大片 ENOSYS/errno 失败 | 缺少 SysV message queue syscall 和 Linux IPC 权限/时间字段语义 | 最小实现 `msgget/msgctl/msgsnd/msgrcv`，用 wall-clock 填 `msg_*time`，校验 uid/gid/mode、NULL 用户指针和 `MSG_COPY/MSG_EXCEPT/MSG_NOERROR`；`msgrcv06/msgsnd06` 这类删除唤醒需后续 wait queue 化 |

### QEMU / 测试

| 问题 | 修复 |
|------|------|
| QEMU 启动无显示 | 检查 `console::init()` 是否第一个被调用 |
| `os_test.conf` 修改不生效 | 用 `conf-inject` 重新注入镜像 |
| QEMU 进程残留 | `pkill qemu-system` |
| `sigtimedwait01`/`rt_sigtimedwait01`/`sigwaitinfo01` 卡住整轮 LTP | 当前 signal wait 缺少专用唤醒队列，先由 inline runner 显式 skip，后续专项修 |
| `signal06` 返回 TCONF 32 | LTP 标注 x86_64-only，rv64/la64 下不是有效适配目标，inline runner 显式 skip |
| `pthcli`/`pthserv` 单独运行失败或挂住 | 它们是 LTP 网络 helper/server，不是独立 syscall 用例，inline runner 显式 skip |
| `ptrace*` 大片 ENOSYS/TBROK | 当前内核无 ptrace stop/wait/tracee 状态机，属于结构性子系统 | 先由 inline runner skip，后续专项做 ptrace 模型 |
| LTP `pm_*`/`pkey01`/`profil01`/`pt_test`/部分 `prctl*` TCONF | 依赖 power-management、pkey、profil、perf、procfs/capability 等当前非目标环境 | 先 narrow skip，避免阻塞后续 syscall 扫描 |
| LTP `rename*` 大片 TBROK 或卡住 | 多数依赖外部块设备、目录权限矩阵等 fs 适配面 | 当前 fs/net 协作期先由 inline runner skip |
| LTP helper 复制失败：`cp: command not found` | `/bin/sh` 已存在时 initproc 跳过 `busybox --install -s /bin`，导致 `/bin/cp` 等 applet 缺失 | 每次启动都幂等安装 BusyBox applet，并兜底创建常用命令 symlink |
| LTP `request_key*`/`rmdir*`/`route*` 阻塞后续扫描 | 分别依赖 keyring 子系统、fs 语义和网络路由脚本 | 当前非 fs/net 主线先 narrow skip |
| LTP `rtc*`/`run_cpuctl*`/`run_freezer*`/`run_memctl*`/`runpwtests*` TFAIL/TCONF | 依赖 RTC ioctl、cgroup controller 或 power-management 环境 | 当前非设备/控制器主线先 narrow skip |
| LTP `rwtest` 持续刷 Broken pipe | 文件/管道压力 helper，容易拖慢扫描 | 当前 broad scan 中单点 skip |
| LTP `sched_stress.sh` 长时间运行且刷脚本命令异常 | scheduler stress helper，不适合作为 syscall 适配扫描阻塞点 | 当前 broad scan 中单点 skip，保留普通 `sched_*` 语义测试 |
| LTP `sched_tc0/1/6`、`sem_comm`、`semctl08/09`、`semget05` 阻塞扫描 | 依赖 KERNEL 环境、IPC namespace、semid64 time_high、SEM_STAT_ANY 或 `/proc/sys/kernel/sem` | 当前 syscall 扫描中先 narrow skip，后续 IPC/procfs 专项处理 |
| LTP `sctp*`、`send02`、`sendmsg01`、`sendmmsg*`、`recvmmsg01`、部分 `sendfile*` 阻塞扫描 | SCTP/网络收发或 fs sendfile 边界用例 | 当前 fs/net 协作期先由 inline runner skip |
| LTP `*_16`、`set_mempolicy*`、`set_thread_area01`、`set_ipv4addr`、`sendmsg03`/`sendto03` | 16-bit compat、NUMA policy、架构 TLS 或网络配置环境不支持 | 当前 broad scan 中显式 skip |
| LTP `setsockopt02/04..10`、`setxattr*`、`sgetmask01`、`shell_pipe01.sh`、`shm_comm`、`shm_test` | net/fs、旧 signal ABI、standalone helper 或 System V SHM 长耗时/namespace 兼容问题 | 当前 broad scan 中 narrow skip，后续专项处理 |
| LTP `shm*`、`splice*`、`squashfs01`、`ssetmask01`、`ssh-stress.sh`、`stack_clash`、`starvation` | IPC/pipe/fs/网络压力、旧 signal ABI、procfs/CVE 环境或长耗时 scheduler stress | 当前 broad scan 中 narrow skip，后续专项处理 |
| LTP `stat03*`、`statfs*`、`statvfs*`、`statx*`、`swap*`、`symlink*`、`sync*`、`sysctl*`、`tcp*`、`stream02`、`support_numa` | fs/device/procfs/网络/NUMA 或 stdio helper 范围 | 当前 fs/net 协作期 broad scan 中 narrow skip |
| LTP `tee*`、SCTP `test_*` helper、`testsf_*` | pipe/fs 或网络 helper，不是当前非 fs/net 主线 | 当前 fs/net 协作期 broad scan 中 narrow skip |
| LTP `thp01` 超大 argv 触发内核跳到 `0x6363...` | `execve` 参数栈跨页写入时旧代码只翻译栈顶一页，且缺少过大 argv/env 的 `E2BIG` 预检 | `execve` 先按用户栈容量拒绝过大参数，ELF 启动栈按虚拟地址逐页翻译写入 |
| LTP `thp02/03/04`、`timed_forkbomb` | THP/huge page 环境缺失或 fork 压力长耗时 | 当前 broad scan 中 narrow skip，保留 `thp01` 回归验证 |
| LTP `times03` CPU 时间统计异常 | `times(2)` 把硬件 tick 当作 `clock_t`，且没有累计已 wait 回收子进程 CPU 时间 | 按 Linux `USER_HZ=100` 换算 `clock_t`，wait 回收 zombie 时累加子进程 `rusage`，`getrusage(RUSAGE_CHILDREN)` 同步返回累计值 |
| LTP la64 `waitpid03` 二次 wait 曾返回已回收 child | `TidHandle` 只由 leader `TaskControlBlock` 持有，leader task 释放后 pid/tid 提前回收到全局分配器；zombie `ProcessControlBlock` 仍在父进程 children 中，25 连续 fork 时 la64 更容易复用 pid | 进程 PCB 持有 leader `TidHandle`，把 pid 生命周期延长到 zombie 被 wait 回收 |
| LTP `timens*`、`timerfd*`、`tst_*`、`tpm*`、`trace*`、`truncate03*` 阻塞扫描 | time namespace/timerfd/TPM/tracing/fs truncate edge 或 LTP 内部 helper，当前非 fs/net 主线不适合长卡 | 先 narrow skip 解堵；`timerfd*` 后续作为 fd+timer 子系统专项实现 |
| LTP `uaccess`、`udp*` 阻塞扫描 | `uaccess` 依赖 LTP kernel module，`udp*` 属于网络矩阵 | 当前 broad scan 中 narrow skip，避免和 net 适配冲突 |
| 非阻塞 socket 测试失败 | 检查是否在 `try_xxx` 前调了 `try_poll()` |

### 错误码对齐（Linux 语义）

- 未对齐 addrlen → EFAULT（RISC-V 硬件不报错，需显式检查 `addrlen % 4 != 0`）
- `mmap` 非匿名映射的坏 fd → EBADF 优先于 len/flags 校验；`mmap08` 会在 page size 异常为 0 时仍期待 EBADF
- `msync` 的 `MS_ASYNC|MS_SYNC` → EINVAL；`MS_INVALIDATE` 命中 `MAP_LOCKED` VMA → EBUSY；未映射区间 → ENOMEM
- `prctl` 兼容项不要统一返回 EINVAL：`PR_SET_NAME` 坏用户指针要 EFAULT，`PR_SET_DUMPABLE` 非 0/1 要 EINVAL，`PR_CAPBSET_DROP`/`PR_SET_SECUREBITS` 缺 `CAP_SETPCAP` 要 EPERM，`PR_SET_TIMERSLACK(0)` 要恢复线程默认值
- `process_vm_readv/writev`：`flags != 0` → EINVAL，`liovcnt/riovcnt > 1024` → EINVAL，零 iovec sanity call 要返回 0；跨进程访问要用目标进程 `vm` fault-in，不能直接套当前 token 的 uaccess；远程地址 EFAULT 前要先做权限检查，无权限返回 EPERM
- SysV msg：`msgp == NULL` → EFAULT，`mtype <= 0` 或 `msgsz > MSGMAX` → EINVAL，无权限按读写位返回 EACCES；空队列配 `IPC_NOWAIT/MSG_COPY` 返回 ENOMSG
- setsockopt 未知 level → ENOPROTOOPT(92)，不是 EOPNOTSUPP(95)
- socketpair 非 AF_UNIX → EPROTONOSUPPORT(93)，不是 EAFNOSUPPORT(97)
- `Socket::alloc` 未知 domain → EAFNOSUPPORT(97)，不是 EINVAL(22)

---

## 新增功能

### 新增 Syscall

```rust
// 1. syscall/syscall_id.rs: pub const SYSCALL_MY_FEATURE: usize = 300;
// 2. 模块中实现: 成功返回 >= 0, 失败返回负 errno
pub fn sys_my_feature(arg1: usize, arg2: usize) -> isize { 0 }
// 3. syscall/mod.rs: 注册到 syscall_name 和 dispatch match 分支
```

### 新增 Socket 类型

```rust
impl Socket for MySocket { /* try_recv, try_send, socket_type → PSOCK::Stream, ... */ }
impl_file_for_socket!(MySocket);  // 自动生成 File trait
// 在 net/mod.rs 的 Socket::alloc() 中接入
```

### 新增块设备/网卡驱动

实现 `drivers/block/mod.rs` 的 `BlockDevice` trait 或 `drivers/net/mod.rs` 的 `NetworkDevice` trait。

### 验证清单

- [ ] `make rv64-kernel-build-only` ✅
- [ ] `make la64-kernel-build-only` ✅
- [ ] QEMU 启动不 panic
- [ ] 相关测试组通过
- [ ] 修改记录写入 `WORK_LOG.md`，可复用经验写入 `EXPERIENCE.md`

---

## 维护本文档

本文档是 AI 助手在该项目的单一事实来源。修改代码后必须同步更新：

- **新的 bug 模式** → 添加到**常见踩坑**
- **代码修改** → 记录到 `WORK_LOG.md`（按日期分区，包含涉及文件 + 验证结果）
- **跨对话经验** → 记录到 `EXPERIENCE.md`（按主题分类，格式：`[现象] → [根因] → [教训]`）
- **架构变更** → 更新本文档对应章节
- **调试技巧** → 参考 `how-to-run.md` 了解 LTP 本地调试和并行测试详情

---

## 交流语言

AI 助手必须使用**中文**与用户交流。代码、注释、commit message 使用英文或中文均可。
