---
title: "文件系统子系统"
module: fs
category: fs
status: draft
owner: MangoCore Team
last_updated: "2026-07-23"
code_paths:
  - "os/src/fs/"
entry_points:
  - "VFS_ROOT"
  - "initramfs_init"
  - "prepare_kernel_bootstrap_filesystem"
  - "register_boot_block_devices"
  - "vfs_lookup"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "open*"
    - "read*"
    - "write*"
    - "stat*"
    - "mkdir*"
    - "rmdir*"
    - "rename*"
    - "chmod*"
    - "mount*"
    - "umount*"
  oscomp:
    - "basic"
    - "busybox"
    - "lua"
    - "libctest"
    - "iozone"
related_docs:
  - "docs/03_fs/architecture.md"
  - "docs/03_fs/vfs-core.md"
  - "docs/03_fs/page-cache.md"
  - "docs/03_fs/init-and-rootfs.md"
  - "docs/02_syscall/README.md"
  - "docs/04_mm/README.md"
  - "docs/ltp/ltp_fs_plan.md"
---

## 概述

文件系统子系统是 MangoCore 中最核心、代码量最大的模块。它提供了一套完整的 VFS 抽象层，支持多种具体文件系统类型，并通过 PageCache、MountFS 和 syscall 接口与内核其他部分交互。

系统从 `rust_main()` 启动：`VFS_ROOT`（lazy_static 单例）解包 initramfs CPIO，并只挂载供 PID1 fd 0/1/2 使用的 devfs `/dev`。内核随后发现、注册块设备；`/sbin/init` 挂载 `/proc`、`/sys`、`/run`、`/dev/shm`、x0 `/sdcard` 与 x1 P1 `/tools`，并优先将 `/sdcard/tmp` bind 到 `/tmp`，失败时回退 tmpfs。

## 架构

FS 子系统采用层次化 VFS 设计，自顶向下依次为：

```
  +-------------------------------------------------------------------+
  |                        syscall 层                                  |
  |  sys_read / sys_write / sys_openat / sys_stat / sys_mount ...     |
  +-------------------------------------------------------------------+
  |                     fd/File 层 (FdTable)                           |
  |    File trait: read() / write() / ioctl() / poll() / mmap()       |
  +-------------------------------------------------------------------+
  |                   IndexNode 层 (VFS 核心抽象)                       |
  |    read_at() / write_at() / find() / create() / link() / unlink() |
  |    metadata() / resize() / page_cache() / poll() / ioctl()        |
  +-------------------------------------------------------------------+
  |                  FileSystem 层 (FS 类型分发)                        |
  |    root_inode() / info() / statfs() / on_umount()                 |
  +--------+--------+--------+--------+--------+--------+--------+---+
  |  ext4  | fat32  | tmpfs  | ramfs  | procfs | devfs  | sysfs  |
  +--------+--------+--------+--------+--------+--------+--------+---+
  +-------------------------------------------------------------------+
|                  PageCache 层 (缓存 + 回写)                         |
|    Loading -> UpToDate <-> Dirty -> Writeback                     |
|    正常 write 延迟回写；仅 fsync/close 或紧急内存压力批量 256 页    |
  +-------------------------------------------------------------------+
  +-------------------------------------------------------------------+
  |                  BlockDevice 层 (驱动抽象)                          |
  |    read_block() / write_block() / size_bytes()                    |
  |    virtio_blk (rv64) / virtio_blk_pci (la64)                     |
  +-------------------------------------------------------------------+
```

### 核心抽象

**File 结构体 (os/src/fs/vfs/file.rs):** fd 层与 VFS 的接口。每个打开的 fd 对应一个 `Arc<File>` 实例，持有 inode 引用和打开标志。提供 `read()`、`write()`、`ioctl()`、`poll()`、`mmap()` 等方法。通过 `FdTable` 管理每个进程的文件描述符空间。

**IndexNode trait (os/src/fs/vfs/index_node.rs):** VFS 的核心抽象，对标 Linux inode。所有具体文件系统 inode 都实现此 trait。方法包括 `read_at()`、`write_at()`、`touch_modified()`、`find()`、`create()`、`link()`、`unlink()`、`rename()`、`symlink()`、`metadata()`、`resize()`、`page_cache()`、`poll()`、`ioctl()`。每个 inode 有全局唯一的 `InodeId`。

**FileSystem trait (os/src/fs/vfs/file_system.rs):** 具体文件系统的抽象接口。提供 `root_inode()`、`info()`、`name()`、`super_block()`、`statfs()` 等方法。

**MountFS (os/src/fs/vfs/mount.rs):** 包装层，处理跨文件系统边界的路径解析和挂载传播。每个 MountFS 持有 `BTreeMap<InodeId, Arc<MountFS>>` 挂载点表，在 `find()` 时检查子挂载点并将操作委托到对应 FS。支持 bind mount、recursive bind mount、mount propagation（shared / private / slave）。

**PageCache (os/src/fs/page_cache.rs):** 通用缓存层，状态机为 Loading → UpToDate ↔ Dirty → Writeback。单个 packed `PG_*` 原子位负责每页写入、回写和截断之间的序列化；正常 write 仅发布每页脏位，后台写回从 32 MiB 水位开始按空闲帧比例协作执行，`fsync`/最后一次 `close` 负责持久化，`reclaim.rs` 周期性探测并回收。

**another_ext4 时间戳缓存：** regular-file 写成功后只在 `InodeLifetime` 更新 mtime/ctime 原子缓存，不读取或修改 ext4 inode 块。`metadata()` 在缓存 dirty 时返回新时间戳；`fsync`、`syncfs` 与全局 `sync` 在数据回写后将两个时间戳合并为一次 `setattr`，仅在最终设备 flush 成功后清除 dirty 标记。

### 特殊文件描述符

FD 抽象不限于磁盘文件。以下特殊 fd 也通过 `File` trait 集成到同一框架：

| 类型 | 文件 | 说明 |
|------|------|------|
| eventfd | `os/src/fs/eventfd.rs` | 事件计数 fd，用于线程间通知 |
| pidfd | `os/src/fs/pidfd.rs` | 进程 fd，支持 open / send_signal / getfd |
| timerfd | `os/src/fs/timerfd.rs` | 定时器 fd，基于 POSIX timer 语义 |
| signalfd | 在 signal 模块中 | fd 方式接收信号 |
| epoll | `os/src/fs/eventpoll.rs` | 可扩展 I/O 事件通知机制 |
| poll | `os/src/fs/poll.rs` | `poll()` / `ppoll()` / `select()` 实现 |
| pipe | `os/src/fs/dev/pipe.rs` | 匿名管道 |
| pty | `os/src/fs/dev/pty.rs` | 伪终端 master/slave |

## FS 类型矩阵

| FS 类型 | 模块路径 | inode trait | 存储后端 | 持久化 | 大小限制 | 状态 |
|---------|----------|-------------|----------|--------|----------|------|
| ext4 | `os/src/fs/ext4/` | Ext4Inode | BlockDevice | 是 | 无 | stable |
| FAT32 | `os/src/fs/fat32/` | FatInode | BlockDevice | 是 | 无 | stable |
| tmpfs | `os/src/fs/tmpfs/` | LockedTmpFSInode | 内存 | 否 | 无（默认）/ 可配 | stable |
| ramfs | `os/src/fs/ramfs/` | LockedRamFSInode | 物理页 | 否 | 无 | stable |
| procfs | `os/src/fs/procfs/` | LockedProcInode | 动态生成 | 否 | 无 | stable |
| devfs | `os/src/fs/dev/` | DevFSInode / LockedDevFSInode | 动态注册 | 否 | 无 | stable |
| sysfs | `os/src/fs/sysfs/` | SysInode | 动态生成 | 否 | 无 | stable |
| initramfs | `os/src/fs/initramfs.rs` | MountFS 委托 | 嵌入式 cpio | 否 | 内存容量 | stable |

### FS 类型说明

**ext4:** 主力文件系统，支持 extent 树、稀疏文件、符号链接、硬链接。通过 `Ext4FileSystem` 包装，在块设备上实现 `FileSystem` trait。LTP 测试覆盖 open / read / write / rename / link / unlink / chmod 等主要操作。

**FAT32:** 引导分区和 EFI 分区支持。通过 `EasyFileSystem` 提供简单接口，注意目录项大小写不敏感等 FAT 特有语义。

**tmpfs:** 无大小限制的内存文件系统（可配上限），用于 `/tmp` 和 `/dev/shm`。支持 sticky bit、权限检查和目录层级。

**ramfs:** 物理页支持的内存文件系统，供 initramfs bootstrap 使用，不参与块设备回写。

**procfs:** `/proc` 伪文件系统。动态生成进程信息（`/proc/[pid]/status`、`maps`、`fd` 等）。支持缓存文本文件（`/proc/version`、`/proc/cpuinfo`）和动态符号链接。

**devfs:** 设备文件系统管理 `/dev/` 下的设备节点。支持 `add_dev()` / `add_dir()` 按需注册，pipe / pty / null / zero / urandom / random / full / tty / rtc 静态注册，`/dev/vda` / `/dev/vdb` 及 MBR 分区节点动态注册。

**sysfs:** `/sys` 伪文件系统，提供内核对象信息。架构与 procfs 类似，注册点位于 `sysfs/files.rs`。

**initramfs:** 生产启动根。在内存中创建 RamFS，解包内嵌 `newc` 格式 cpio 归档，创建挂载点并挂载最小 devfs；最后延迟探测块设备并注册 `/dev/vd*`。常规伪文件系统和磁盘挂载由 PID1 执行。

## FS 子模块索引

| 模块 | 路径 | 职责 |
|------|------|------|
| vfs | `os/src/fs/vfs/` | VFS 抽象层：IndexNode / FileSystem / File / MountFS / dentry_cache / propagation / posix_lock / fasync / fcntl |
| page_cache | `os/src/fs/page_cache.rs` | PageCache 缓存层 + `reclaim.rs` 后台回写 |
| ext4 | `os/src/fs/ext4/` | ext4 文件系统实现（extent 树、块分配、目录项） |
| fat32 | `os/src/fs/fat32/` | FAT32 文件系统实现 |
| tmpfs | `os/src/fs/tmpfs/` | 临时内存文件系统 |
| ramfs | `os/src/fs/ramfs/` | 物理页内存文件系统 |
| procfs | `os/src/fs/procfs/` | /proc 伪文件系统 |
| dev | `os/src/fs/dev/` | 设备文件系统（null / zero / urandom / pipe / pty / rtc / block） |
| sysfs | `os/src/fs/sysfs/` | /sys 伪文件系统 |
| eventpoll | `os/src/fs/eventpoll.rs` | epoll_create / epoll_ctl / epoll_wait |
| poll | `os/src/fs/poll.rs` | sys_poll / sys_ppoll / sys_select |
| eventfd | `os/src/fs/eventfd.rs` | eventfd 非阻塞通知 fd |
| timerfd | `os/src/fs/timerfd.rs` | timerfd_create / timerfd_settime |
| pidfd | `os/src/fs/pidfd.rs` | pidfd_open / pidfd_send_signal |
| initramfs | `os/src/fs/initramfs.rs` | initramfs cpio 解包 |
| filesystem | `os/src/fs/filesystem.rs` | FS 类型检测（detect_fs / pre_mount） |
| layout | `os/src/fs/layout.rs` | stat / statx 数据结构 |
| iov | `os/src/fs/iov.rs` | readv / writev iovec 支持 |
| dirent | `os/src/fs/dirent.rs` | getdents64 目录项结构 |
| reclaim | `os/src/fs/reclaim.rs` | PageCache 后台回收 |
| swap | `os/src/fs/swap.rs` | 交换支持（feature = "swap"） |

## 阅读顺序

对于希望理解 FS 子系统的开发者，建议按以下顺序阅读：

1. **`vfs/file_system.rs`** — FileSystem trait 定义，理解 FS 抽象边界
2. **`vfs/index_node.rs`** — IndexNode trait 定义，VFS 核心接口
3. **`vfs/file.rs`** — File trait 和 FdTable，fd 层到 inode 的桥接
4. **`vfs/mount.rs`** — MountFS 和 MountFSInode，挂载管理和跨 FS 路径解析
5. **`mod.rs`** — VFS_ROOT 初始化和 mount_common_filesystems，整体初始化流程
6. **`filesystem.rs`** — detect_fs 和 FS_Type，块设备 FS 检测逻辑
7. **`page_cache/mod.rs`** — PageCache 状态机和 LRU 回收
8. **`ext4/` / `fat32/` / `tmpfs/` / `ramfs/`** — 具体 FS 实现（按需求选择）
9. **`eventpoll.rs`** — epoll 实现，理解 I/O 事件通知机制
10. **`dev/` / `procfs/` / `sysfs/`** — 伪文件系统实现

## 相关文档

- `docs/02_syscall/README.md` — 文件 I/O syscall 参考
- `docs/04_mm/README.md` — 与 mmap / page cache 的交互
- `docs/07_driver/README.md` — BlockDevice 驱动层
- `docs/kernel/fs/ext4-cache-design.md` — ext4 PageCache 设计旧文档
- `docs/ltp/ltp_fs_plan.md` — LTP 文件系统测试计划
- `docs/ltp/ltp_fs_status.md` — LTP 文件系统当前通过状态
- `docs/ltp/ltp_mount_plan.md` — LTP mount 测试计划
- `docs/ltp/ltp_mount_status.md` — LTP mount 当前通过状态
