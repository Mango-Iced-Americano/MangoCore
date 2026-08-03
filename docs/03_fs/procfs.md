---
title: "procfs 伪文件系统"
module: "fs/procfs"
category: fs
status: draft
owner: "MangoCore Team"
last_updated: "2026-06-29"
code_paths:
  - "os/src/fs/procfs/"
entry_points:
  - "LockedProcInode"
arch:
  rv64: supported
  la64: supported
tests:
  oscomp:
    - "basic"
  ltp:
    - "proc01"
    - "proc02"
related_docs:
  - "docs/03_fs/architecture.md"
  - "docs/03_fs/vfs-core.md"
  - "docs/06_net/device-stack-and-poll.md"
---

## 概述

procfs 是挂载在 `/proc` 下的伪文件系统，提供内核内部数据结构的用户态视图。所有文件内容在读取时动态生成，不占用持久存储。

MangoCore 的 procfs 实现仿照 DragonOS `kernel/src/filesystem/procfs/` 设计，适配本项目的 VFS 架构。核心数据结构是 `LockedProcInode`（`Mutex<ProcInodeData>`），每个 inode 携带一组函数指针，在 `read_at()` 调用时按需生成内容。

## 动态内容生成模型

procfs 的读写模型基于三个函数指针类型，定义在 `mod.rs`：

| 类型 | 签名 | 用途 |
|------|------|------|
| `ProcContentFn` | `fn(extra_data, offset, len, buf) -> Result<usize, SyscallErr>` | 按偏移和长度直接写入缓冲区，每次 read 都重新调用 |
| `ProcTextFn` | `fn(extra_data) -> Result<String, SyscallErr>` | 生成完整文本，缓存到 `FilePrivateData::ProcText`，后续 read 从缓存读取 |
| `ProcWriteFn` | `fn(extra_data, offset, buf) -> Result<usize, SyscallErr>` | 写入回调，仅 writable 文件注册 |

`extra_data` 字段携带上下文（如 PID），由 inode 构造时注入。`ProcTextFn` 用于内容不频繁变化的文件（如 `/proc/version`），生成一次后缓存，避免重复格式化开销。

目录结构通过 `ProcInodeData.children: BTreeMap<String, Arc<dyn IndexNode>>` 管理静态子项。对于动态子项（如 PID 目录、网络接口目录），通过 `find_hook` 和 `list_hook` 两个回调实现：

```rust
pub type FindHookFn = fn(inode: &LockedProcInode, name: &str) -> Option<Arc<dyn IndexNode>>;
pub type ListHookFn = fn(inode: &LockedProcInode) -> Vec<String>;
```

`find()` 先在 `children` BTreeMap 中查找，miss 后调用 `find_hook`。`list()` 先返回 "."、".." 和 children 键列表，再追加 `list_hook` 结果。这种设计将静态与动态内容分离，避免为每个可能的 PID 预分配 inode。

## 目录结构

### /proc 根级文件

| 文件 | 生成方式 | 说明 |
|------|----------|------|
| `cmdline` | ProcContentFn | 内核启动参数，返回 `"BOOT_IMAGE=kernel\n"` |
| `version` | ProcTextFn（缓存） | 内核版本号 + 架构标识 |
| `cpuinfo` | ProcTextFn（缓存） | CPU 信息，含架构、ISA、MMU 类型 |
| `meminfo` | ProcContentFn | 物理内存总量/空闲/可用、内核堆统计、Committed_AS |
| `stat` | ProcContentFn | CPU 统计（简化版）、btime、进程数、运行/阻塞数 |
| `uptime` | ProcContentFn | 系统运行时间 |
| `filesystems` | ProcTextFn（缓存） | 注册的文件系统列表（proc、devfs、ramfs、tmpfs、ext4、vfat） |
| `mounts` | ProcContentFn | 当前挂载点列表（委托给 mounts 模块） |
| `config` | ProcTextFn（缓存） | 内核编译配置（当前为占位） |
| `self` | 动态符号链接 | 指向当前进程 PID 的软链接，每次读取时通过 `current_task()` 动态解析 |

### /proc/[pid]/ 进程目录

每个运行中的进程通过 `pid_find_hook` 动态创建 PID 目录。目录结构如下：

| 文件 | 生成方式 | 说明 |
|------|----------|------|
| `status` | ProcContentFn | 进程状态、PID/PPID/TGID、UID/GID 四元组、FD 上限、VmRSS/VmHWM/VmLck、信号掩码/挂起、capability 集、线程数 |
| `stat` | ProcContentFn | 进程统计，兼容 procps 工具解析格式 |
| `comm` | ProcContentFn | 进程命令名 |
| `cmdline` | ProcContentFn | 进程命令行参数（以 null 分隔） |
| `maps` | ProcContentFn | 用户地址空间 VMA 映射，委托给 `MemorySet::proc_maps_content()` |
| `smaps` | ProcCursorFn（有界游标） | 逐 VMA 内存统计，per-open 游标按段流式输出，不缓存完整快照 |
| `mounts` | ProcContentFn | 进程视角的挂载列表 |
| `mountinfo` | ProcContentFn | 挂载详细信息（挂载 ID、父 ID、主次设备号等） |
| `io` | ProcContentFn | I/O 统计（读写字节数，当前为简化版） |
| `pagemap` | ProcContentFn | 虚拟页到物理帧的映射信息 |
| `exe` | 动态符号链接 | 指向可执行文件的路径 |
| `fd/` | 目录 + 钩子 | 文件描述符子目录，每个 fd 为指向实际文件的符号链接 |
| `task/` | 目录 + 钩子 | 线程子目录，按 TID 枚举 |
| `ns/` | 目录 | 命名空间目录，包含 `net`、`mnt`、`ipc` 子项 |

`fd/` 和 `task/` 目录也使用 `set_hooks()` 注册动态查找和列表钩子，避免为所有 fd 或线程预建 inode。

### /proc/net/ 网络统计

| 文件 | 说明 |
|------|------|
| `tcp` / `tcp6` | TCP 套接字列表（sl、本地地址、远程地址、状态、队列深度、uid、inode） |
| `udp` / `udp6` | UDP 套接字列表 |
| `raw` / `raw6` | RAW 套接字列表 |
| `unix` | Unix 域套接字列表 |
| `arp` | ARP 缓存表 |
| `route` | 路由表 |
| `dev` | 网络接口统计（收发包计数、错误、丢弃） |
| `if_inet6` | IPv6 接口地址列表 |
| `igmp` / `igmp6` | IGMP/MLD 组成员信息 |
| `snmp` / `snmp6` | SNMP 协议统计占位 |
| `netstat` | 网络扩展统计占位 |

网络统计文件持有 `NET_INTERFACE` 的锁遍历 smoltcp socket 集合，生成 `/proc/net/tcp` 格式的文本行。输出格式适配 `netstat`、`ss` 等工具的解析逻辑。

### /proc/sys/ 内核参数

`/proc/sys/kernel/` 下包含：

| 文件 | 读写 | 说明 |
|------|------|------|
| `pid_max` | 只读 | PID 上限 |
| `threads-max` | 只读 | 线程数上限 |
| `ns_last_pid` | 读写 | 命名空间 PID 分配器 |
| `core_pattern` | 读写 | core dump 模式 |
| `tainted` | 只读 | 内核污染标记 |
| `osrelease` | 只读 | 内核发布版本 |
| `shmmax` / `shmall` / `shmmni` | 只读 | SysV 共享内存参数 |
| `msgmax` / `msgmnb` / `msgmni` / `msg_next_id` | 读写 | SysV 消息队列参数 |
| `sem` | 读写 | SysV 信号量参数 |

`/proc/sys/fs/` 下包含 pipe 和 mqueue 相关参数。`/proc/sys/vm/` 下包含 overcommit、max_map_count、min_free_kbytes、panic_on_oom 等内存调优参数。`/proc/sys/net/` 下包含 ip_forward、ipv6 conf disable_ipv6 等网络参数。

接口名通过 `set_hooks()` 动态解析：`/proc/sys/net/ipv6/conf/<iface>/` 下的目录由 `ipv6_conf_find_hook` 根据当前 netns 中的网络设备名称动态创建。

### 其他节点

`/proc/sysvipc/shm`、`/proc/sysvipc/msg`、`/proc/sysvipc/sem` 分别输出 System V IPC 对象列表。`/proc/loadavg` 当前未实现。

## 缓存策略

procfs 区分三种内容生成模式：

**有界游标（ProcCursorFn）**：用于可能非常大的逐段生成文件（如 `/proc/[pid]/smaps`）。文件通过 `add_cursor_file()` 构造，`read_at()` 从 `FilePrivateData::ProcSmapsCursor` 惰性创建 per-open 游标（`vfs::SmapsCursor`），每次 read(2) 只生成并缓存**一个 VMA 段**（紧凑约 256 B / 完整约 1 KiB）。顺序读整体 O(N) 且有界内核堆内存，避免了为数千个 VMA 构建多 MiB 快照 String 导致的堆 OOM；offset 回退时游标重置后从头生成，乱序 pread 语义与 Linux seq_file 一致。

**动态生成（ProcContentFn）**：每次 `read_at()` 都调用生成函数重新计算内容。适用于需要反映实时状态的文件：`/proc/meminfo`、`/proc/stat`、`/proc/uptime`、`/proc/mounts`、`/proc/version`、`/proc/cpuinfo`、`/proc/[pid]/status`、`/proc/[pid]/maps`、`/proc/net/*`。

**符号链接**：普通符号链接的 target 存储在 `symlink_target` 字段中。`/proc/self` 使用 `new_dynamic_symlink_wired()` 构造，由 `content_fn` 在每次读取时调用 `current_task().pid()` 生成。

**目录动态性**：`find_hook` / `list_hook` 在每次目录查找和列表操作时触发。PID 目录在进程退出后仍然可以通过 `create_dead_ns_dir()` 访问其命名空间信息（net、mnt、ipc），进程已消失但 PID 目录 inode 未被 dentry cache 淘汰时仍可返回命名空间内容。

## IndexNode 实现要点

`LockedProcInode` 实现 `IndexNode` trait 时注意以下设计约束：

- **read_at**：在锁外提取 `content_fn`/`cursor_fn`/`extra_data` 后调用生成函数，避免内容生成函数持有 procfs 内部锁导致死锁。`cursor_fn` 的文件（如 smaps）走 per-open 有界游标路径，`content_fn` 的文件每次都生成。
- **write_at**：仅当 `writable = true` 且注册了 `write_fn` 时才允许写入。写入后更新 mtime/ctime。
- **find/list**：`find()` 在短锁内提取静态子项和钩子引用，释放锁后再调用钩子。`list()` 类似，先收集静态键再追加动态键。
- **set_metadata**：只允许更新时间戳，拒绝 mode/uid/gid 变更（符合 Linux 的 procfs 只读语义）。
- **get_entry_name**：通过父节点反向查找 inode 对应的名称，在锁外遍历子项以避免锁嵌套。

## FS 注册

procfs 在 `mount_common_filesystems()` 流程中挂载到 `/proc`，通过 `MountFS` 禁用 dentry cache（`flags.no_dentry_cache = true`）。注册入口在 `files/mod.rs::register_all()`，依次添加根级文件、`/proc/sys/` 层级树、`/proc/net/` 文件组和 `/proc/sysvipc/` 文件。最后调用 `pid::setup_pid_hooks(root)` 为 `/proc` 根目录注入 PID 动态查找/列表钩子。

超级块魔数为 `0x9fa0`（PROC_SUPER_MAGIC），最大文件名长度 255 字节，符号链接 target 最大 64 字节。

## 测试映射

| 测试 | 范围 | 状态 |
|------|------|------|
| LTP proc01 | 基本 /proc 文件读取（version、cpuinfo、meminfo、stat、uptime、cmdline 等） | 通过 |
| LTP proc02 | /proc/self 解析 | 通过 |
| LTP proc03-* | /proc/[pid]/status、maps、fd | 待验证 |
| OSComp basic | /proc/version 等基础文件读取 | 通过 |
| busybox ps | 通过 /proc/[pid]/status 获取进程信息 | 通过 |
| busybox top | 通过 /proc/stat 和 /proc/[pid]/stat 获取统计 | 通过 |
| netstat -an | 读取 /proc/net/tcp、udp、unix 等 | 通过 |
| ip neighbor / route | 读取 /proc/net/arp、route | 通过 |

## 已知问题

1. **`/proc/loadavg` 未实现**。当前缺失 `/proc/loadavg` 节点，一些依赖负载统计的工具（如 `uptime`、某些 busybox 变体）可能无法获取信息。需在 `register_all()` 中新增对应文件。
2. **`/proc/stat` CPU 时间简化**。`/proc/stat` 的 cpu 行全为 0（user/nice/system/idle 等字段），仅 btime、processes、procs_running、procs_blocked 字段反映实际值。不影响 LTP 基础测试，但性能分析工具可能得到不准确的 CPU 使用率。
3. **`/proc/meminfo` 部分字段占位**。Buffers、Cached、SwapTotal、SwapFree、Dirty、Writeback、Shmem 等字段固定为 0。当前内核未实现 swap 和块缓存统计。
4. **`/proc/[pid]/io` 简化版**。I/O 统计仅计数读写的字节数，未区分 block I/O 和字符设备 I/O。不影响进程管理工具，但依赖于精细化 I/O 监控的场景可能受限。
5. **`/proc/[pid]/smaps` 为有界流式输出**。per-open 游标每次 read(2) 只生成一个 VMA 段，内核堆内存有界；但 VMA 列表在两个 read(2) 之间变化时，后续内容反映最新状态（与 Linux seq_file 的读取时快照行为一致）。
6. **`/proc/sys/` 可写文件的持久性**。写入 `/proc/sys/` 下参数的修改仅影响运行时内核状态，重启后丢失。当前内核未实现 sysctl 配置持久化。
7. **动态 PID 目录生命周期**。PID 目录由 `find_hook` 动态创建，没有明确的回收机制。进程退出后，PID 目录 inode 通过 `create_dead_ns_dir()` 退化为仅包含命名空间信息的 stub。长生命周期进程中频繁的 PID 分配和回收可能导致 inode 残留。
8. **锁顺序**。`LockedProcInode` 使用内部 `Mutex`，生成回调运行时锁已释放。但 `ProcContentFn` 内部如果访问其他 procfs inode（如 `/proc/mounts` 遍历挂载点），仍需注意全局锁顺序，避免锁反转。
