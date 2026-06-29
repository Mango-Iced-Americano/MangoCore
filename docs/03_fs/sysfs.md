---
title: "sysfs 伪文件系统"
module: "fs/sysfs"
category: fs
status: draft
owner: "MangoCore Team"
last_updated: "2026-06-29"
code_paths:
  - "os/src/fs/sysfs/"
entry_points:
  - "SysInode"
arch:
  rv64: supported
  la64: supported
tests:
  oscomp:
    - "basic"
  ltp:
    - "sysfs"
    - "sysfs01 (planned)"
related_docs:
  - "docs/03_fs/architecture.md"
  - "docs/03_fs/vfs-core.md"
  - "docs/03_fs/procfs.md"
---

## 概述

sysfs 是挂载在 `/sys` 下的伪文件系统，提供内核对象（设备、网络接口、内核诊断）的用户态视图。所有文件内容在读取时动态生成，不占用持久存储。

MangoCore 的 sysfs 实现仿照 procfs 的设计模式，适配本项目的 VFS 架构。核心数据结构是 `SysInode`（`Mutex<SysInodeData>`），每个 inode 携带函数指针，在 `read_at()` 调用时按需生成内容。

## 动态内容生成模型

sysfs 的读写模型基于以下类型，定义在 `mod.rs`：

| 类型 | 签名 | 用途 |
|------|------|------|
| `SysContentFn` | `fn(extra_data, offset, len, buf) -> Result<usize, SyscallErr>` | 按偏移和长度直接写入缓冲区，每次 read 都重新调用 |
| `SysWriteFn` | `fn(extra_data, offset, buf) -> Result<usize, SyscallErr>` | 写入回调，仅可写文件注册 |
| `FindHookFn` | `fn(inode, name) -> Option<Arc<dyn IndexNode>>` | 目录查找 miss 时的动态回调 |
| `ListHookFn` | `fn(inode) -> Vec<String>` | 目录列表时的动态子项名称生成 |

与 procfs 相比，sysfs 没有 `ProcTextFn` 缓存路径。所有动态内容的文件都使用 `SysContentFn` 每次重新生成。对于内容不需要每次变化的文件，sysfs 提供 `owned_content: Option<String>` 字段，写入一次后直接从字符串拷贝，不重复调用生成函数。

目录结构通过 `SysInodeData.children: BTreeMap<String, Arc<dyn IndexNode>>` 管理静态子项。对于动态子项（如网络接口目录），通过 `find_hook` 和 `list_hook` 回调实现，模式与 procfs 完全相同。

## 读写行为

**读（read_at）：** 优先使用 `owned_content`（静态字符串），其次调用 `content_fn` 动态生成。目录返回 EISDIR。两者皆无时返回 ENOSYS。

**写（write_at）：** 仅当 `writable == true` 且注册了 `write_fn` 时允许写入。写入后更新 mtime/ctime。只读文件返回 EPERM。

**resize：** 目录返回 EISDIR。非目录只允许截断到 0 长度，其他返回 EINVAL。

**set_metadata：** 只允许更新时间戳（atime/mtime/ctime），拒绝 mode/uid/gid 变更，符合 sysfs 只读语义。

## 目录结构

sysfs 的节点注册在 `files/mod.rs::register_all()` 中完成，以 `/sys` 根 inode 为入口依次添加子项。

### /sys/class/net/

动态目录，通过 `FindHookFn` / `ListHookFn` 钩子实现。每次 `find()` 或 `list()` 时遍历当前命名空间及已注册的所有网络命名空间的设备列表，动态生成每个网络接口的子目录。

每个接口目录下包含：

| 文件 | 生成方式 | 说明 |
|------|----------|------|
| `address` | owned_content | MAC 地址，`xx:xx:xx:xx:xx:xx` 格式 |
| `mtu` | owned_content | MTU 数值 |

`devices_all_ns()` 辅助函数负责全命名空间设备枚举，使用 BTreeSet 去重避免同一设备出现在多个命名空间中时重复列出。

### /sys/block/

静态目录，当前无子项。预留用于块设备信息展示（如分区名、设备大小），后续可按 `class/net` 模式扩展。

### /sys/kernel/stats/

编译特性 `perf_diag` 启用时注册，存放内核性能计数器，以 `key=value` 格式输出。定义在 `files/diag.rs`。

| 文件 | 可写 | 来源计数器（`crate::task::perf`） |
|------|------|------|
| `features` | 否 | compile-time feature 标记 |
| `stats_on` | 是 | `STATS_ON` 总开关 |
| `reset` | 仅写 | `reset_all_counters()` |
| `taskq` | 否 | 调度队列统计（ready 长度、interruptible 长度、zombie 扫描、enqueue 计数等） |
| `timer` | 否 | 内核定时器统计（ktimer 队列深度、pop、wake、compact、stale 移除） |
| `seccomp` | 否 | seccomp 检查调用次数和耗时 |
| `syscall` | 否 | 系统调用总数、getppid 调用数、syscall 最大耗时、ecall trap 耗时 |
| `ctxsw` | 否 | 上下文切换总数 |
| `reclaim` | 否 | PageCache 回收运行次数、扫描页数、释放页数，Clock 页置换统计 |
| `tlb` | 否 | TLB 刷新统计（full/flush/page/activate/global） |
| `heap` | 否 | 内核堆使用量、最大用量、分配/释放调用次数和耗时 |
| `pagecache` | 否 | PageCache I/O 统计（读/写/回写调用次数、页数、miss/hit 耗时） |
| `blockio` | 否 | 块设备 I/O 请求数和扇区数 |
| `resource` | 否 | 全局资源快照（就绪任务数、空闲帧数、套接字统计、pipe/unix 活跃数、mount/dcache/pagecache 统计） |
| `buddyinfo` | 否 | 伙伴分配器自由块直方图 |
| `zombies` | 否 | zombie 进程统计，按父 PID 分组 |

`stats_on` 和 `reset` 提供运行时控制：写入 `1`/`0` 启用或禁用计数器收集，写入 `reset` 清空所有 P0 计数器。

### /sys/kernel/tracing/

同样在 `perf_diag` 下注册，提供内核追踪环状缓冲区的控制接口。

| 文件 | 可写 | 说明 |
|------|------|------|
| `tracing_on` | 是 | 追踪总开关，`1`/`0` 控制 |
| `trace` | 否 | 格式化环状缓冲区内容转储（最多 512 条） |
| `dropped` | 否 | 因缓冲区满丢弃的事件计数 |
| `buffer_size` | 否 | 环状缓冲区容量（编译时常量 `TRACE_SIZE`） |
| `clear` | 仅写 | 清空环状缓冲区并重置丢弃计数 |
| `trigger` | 仅写 | 触发诊断扫描（`buddy` / `zombie` / `heap`） |

`trigger` 文件中的命令仅标记为接受，实际扫描操作在后续上下文中异步触发。

## 与 procfs 的架构对比

sysfs 与 procfs 共享同一设计范式，差异如下：

| 维度 | procfs | sysfs |
|------|--------|-------|
| 核心 inode | `LockedProcInode` | `SysInode` |
| 内容生产 | `ProcContentFn` / `ProcTextFn`（含缓存） | `SysContentFn` / `owned_content`（无 TextFn 缓存） |
| 写入 | `ProcWriteFn` | `SysWriteFn` |
| 动态子项 | `FindHookFn` / `ListHookFn` | `FindHookFn` / `ListHookFn`（签名一致） |
| 挂载路径 | `/proc` | `/sys` |
| dentry cache | 禁用 | 禁用（通过 `MountFS` 设置 `no_dentry_cache`） |
| 构造模式 | `new_cached_text_file_wired()` 等专用构造 | `add_file()` / `add_file_owned()` / `add_writable_file_with_write()` 等通用构造 |
| 主题域 | 进程、网络统计、内核参数 | 设备、网络接口、内核诊断 |
| 超级块魔数 | `0x9fa0` | `0x62656572` |

两者都使用 `Arc::new_cyclic` 模式安全地初始化 inode 的 `Weak` 自引用和父引用，避免在回调中访问悬空指针。

## FS 注册

sysfs 在 `initramfs_init()` 或 `mount_common_filesystems()` 流程中挂载到 `/sys`。创建流程：

```rust
let sysfs = SysFS::new();
files::register_all(sysfs.root());
let mnt = MountFS::new(sysfs, MountFlags::empty());
mnt.no_dentry_cache.store(true);
mnt.set_mount_path(Some("/sys"));
// mnt 注册到 VFS_ROOT 挂载树
```

sysfs 同时支持运行时按需挂载（如 `sys_mount("sysfs", "/sys", "sysfs", 0, 0)`），在 `syscall/fs.rs` 中分派到 `SysFS::new()` + `register_all()`。

## 测试映射

| 测试 | 范围 | 状态 |
|------|------|------|
| OSComp basic | `/sys` 目录可访问 | 通过 |
| 手动验证 | `/sys/class/net/lo/address` | 通过 |
| 手动验证 | `/sys/class/net/lo/mtu` | 通过 |
| LTP sysfs01 | sysfs 基本文件读取 | 未覆盖（计划中） |
