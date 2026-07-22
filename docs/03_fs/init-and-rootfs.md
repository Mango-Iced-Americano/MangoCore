---
title: "文件系统初始化与根文件系统设置"
module: "fs/init"
category: fs
status: draft
owner: MangoCore Team
last_updated: 2026-07-22
code_paths:
  - "os/src/fs/mod.rs"
  - "os/src/fs/filesystem.rs"
  - "os/src/fs/initramfs.rs"
entry_points:
  - "VFS_ROOT"
  - "fs::initramfs_init"
  - "detect_fs"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "mount01"
    - "mount02"
    - "umount01"
  oscomp:
    - "basic"
related_docs:
  - "docs/03_fs/architecture.md"
  - "docs/03_fs/vfs-core.md"
  - "docs/03_fs/page-cache.md"
---

# 文件系统初始化与根文件系统设置

## 1. 概述

根文件系统初始化是内核启动的关键阶段。它在内存管理初始化之后执行，负责创建 VFS 根、解包 initramfs、提供 PID1 所需的最小 devfs/tty bootstrap，并发现和注册块设备。常规伪文件系统和磁盘挂载由 `/sbin/init` 通过 `mount` syscall 执行。

内核初始化逻辑集中在 `os/src/fs/mod.rs` 的 `VFS_ROOT` lazy_static 和 `prepare_kernel_bootstrap_filesystem` 函数中；PID1 挂载策略集中在 `user/src/bin/initd.rs`。

## 2. 启动流程

```
rust_main()
  |
  |-- mm::init()                       物理内存和页表就绪
  |-- timer_subsystem_init()           定时器子系统初始化
  |
  |-- [initramfs 特性启用]
  |     |-- fs::initramfs_init()       触发 VFS_ROOT lazy_static
  |     |     |-- [new RamFS]
  |     |     |-- [unpack_embedded()]  解包内嵌 cpio 归档
   |     |     |-- [prepare_kernel_bootstrap_filesystem()]
   |     |     |     |-- 创建 PID1 挂载点目录
   |     |     |     |-- /dev (devfs，含 /dev/tty 与 /dev/shm cover 目录)
  |     |
  |     |-- drivers::init_net_device()
  |     |-- net::config::init()
   |     |-- fs::register_boot_block_devices()  探测块设备并注册 /dev/vd*
  |
   |-- task::add_initproc()            加载 init 进程（fd 0/1/2 使用 /dev/tty）
   |-- task::run_tasks()               进入调度
   |     |-- /sbin/init: 挂载 proc/sys/run/tmp/dev/shm
   |     |-- /sbin/init: normal 模式挂载 x0→/sdcard、x1→/tools
```

### 2.1 两种启动模式

**initramfs 模式**（默认，由 `Cargo.toml` 中 `default = ["initramfs"]` 控制）：

1. 创建空 `RamFS` 作为根文件系统。
2. 通过 `initramfs::unpack_embedded()` 解包编译时通过 `.incbin` 嵌入内核的 newc cpio 归档，将 init 程序、busybox 等注入 RamFS。
3. 仅挂载 devfs，并注册 `/dev/tty` 以建立 PID1 的 fd 0/1/2；其余挂载点只是目录。
4. 调用 `register_boot_block_devices()`：`boot_block` 子模块探测 virtio 块设备、注册 `/dev/vda`、`/dev/vdb` 及 MBR 分区节点，不打开或挂载其文件系统。
5. `/sbin/init` 挂载 procfs、sysfs 和 tmpfs，并在非 regression 模式下将 x0 挂载到 `/sdcard`、将 x1（优先 `/dev/vdb1`，回退 `/dev/vdb`）挂载到 `/tools`。
5. 块设备故障只打印 warning，不 panic。

## 3. VFS_ROOT lazy_static

`VFS_ROOT` 是 `Arc<MountFS>` 类型的全局根文件系统引用，通过 `lazy_static` 实现单次初始化。

### 3.1 initramfs 模式

```rust
#[cfg(feature = "initramfs")]
lazy_static! {
    pub static ref VFS_ROOT: Arc<MountFS> = {
        let ramfs = RamFS::new();
        let mfs = MountFS::new(ramfs, MountFlags::empty());
        initramfs::unpack_embedded(&mfs).ok();
        mount_common_filesystems(&mfs);
        mfs
    };
}
```

初始化流程：new RamFS → MountFS 包装 → 解包 cpio → 创建挂载点 → 挂载 devfs。块设备在此阶段不参与，后续仅由 `register_boot_block_devices()` 注册为设备节点。

`detect_fs()` 读取块 0 的一个完整块（BLOCK_SIZE）：首先检查偏移 510 处 MBR 签名 `0x55AA`，若无 MBR 则检查偏移 1024 + 56 = 1080 处 ext4 超级块魔数 `0xEF53`。

## 4. 默认挂载点

内核 `prepare_kernel_bootstrap_filesystem()` 只创建以下挂载点并挂载 devfs；PID1 随后使用 mount syscall 覆盖对应目录：

| 挂载点 | 文件系统类型 | 说明 |
|--------|-------------|------|
| `/dev` | devfs | 设备文件系统，注册 tty、null、zero、urandom、full、random、console、ptmx、pts、rtc、cpu_dma_latency、misc/rtc |
| `/dev/shm` | PID1 tmpfs | 共享内存；内核只提供 devfs cover 目录 |
| `/proc` | PID1 procfs | 进程信息文件系统，动态内容禁用 dentry cache |
| `/sys` | PID1 sysfs | 内核对象文件系统，动态内容禁用 dentry cache |
| `/tmp` | PID1 tmpfs | 临时文件系统 |
| `/mnt` | (目录) | 通用挂载点，权限 0755 |
| `/run` | PID1 tmpfs | 运行时文件 |
| `/var/tmp` | (目录) | 临时文件备选，权限 01777 |

devfs 使用 `MountFS` 子挂载注入，并在 PID1 创建前注册 `/dev/tty`。procfs 和 sysfs 由 `sys_mount` 创建，且因内容动态生成而禁用 dentry cache。

### 4.1 PID1 tty 与后续挂载职责

内核在创建 PID1 前完成 devfs 挂载并注册 `/dev/tty`，因此 `TaskControlBlock::new()` 可以为 fd 0、1、2 打开最小控制台 bootstrap。ktest 的独立内核任务没有用户态 fd，显式跳过这一步。除此 `/dev/tty` bootstrap 外，所有正常启动挂载策略均由 PID1 接管。

## 5. Initramfs 解包

当 `feature = "initramfs"` 启用时，构建脚本通过链接器脚本的 `.incbin` 指令将 newc cpio 归档嵌入内核的 `.data` 段。链接器符号 `sinitramfs` 和 `einitramfs` 标记归档的起止地址。

`unpack_embedded()` 执行：
1. 通过 `embedded_archive()` 获取归档切片。
2. 调用 `unpack_newc()` 逐条解析 newc header。
3. 支持的文件类型：常规文件（写数据）、目录（mkdir）、符号链接。
4. 字符/块设备、fifo、socket 静默跳过（由 devfs 管理）。
5. 路径安全校验：拒绝包含 `..` 的路径，去除前导 `./` 和 `/`。
6. 自动创建中间目录。

解包统计输出示例：
```
[initramfs] unpacked: files=42 dirs=12 symlinks=3 bytes=1048576
```

## 6. vfs_root() 辅助函数

```rust
pub fn vfs_root() -> Arc<MountFS> {
    VFS_ROOT.clone()
}
```

提供对根文件系统的共享引用，供路径解析、挂载操作、procfs 等模块使用。`mount_block_fs()` 接受 `parent_mfs` 参数，可在 VFS_ROOT 下任意位置创建子挂载。

## 7. Test Mapping

| 特性 | 入口 | LTP 用例 | OSCOMP 分组 | 状态 |
|------|------|----------|-------------|------|
| 根文件系统挂载 | `VFS_ROOT` | `mount01`, `mount02` | basic | pass |
| 卸载 | `umount` syscall | `umount01` | basic | pass |
| /proc 挂载 | procfs | `proc01` | basic | pass |
| /dev/null | devfs | `null01` | basic | pass |
| /tmp 可写 | tmpfs | `tmpfs01` | basic | pass |
| ext4 检测 | `detect_fs` | — | basic | pass |
| FAT32 检测 | `detect_fs` | — | basic | pass |
| initramfs 启动 | `initramfs_init` | — | basic | pass |

## 8. 关键设计点

- **VFS_ROOT 初始化顺序**：必须发生在 `mm::init()` 之后（需要堆分配器），且在 `task::add_initproc()` 之前（init 进程需要根文件系统）。
- **initramfs 中块设备延迟探测**：块设备探测需要连续物理页 DMA；initramfs 路径在网络初始化后只发现/注册（`register_boot_block_devices`），不得在内核中选择或挂载 x0/x1。
- **不可递归触发 VFS_ROOT**：initramfs 解包期间（`unpack_newc`）严禁调用 `vfs_root()`，必须使用传入的 `root` 参数，否则引发递归 lazy_static 初始化死锁。
- **块设备故障不 panic**：无论是根文件系统未识别还是 tools 盘缺失，均 fallback 到 ramfs 或打印 warning 继续执行。这对调试和 CI 环境至关重要。
- **MountFS 包装统一入口**：无论底层是磁盘文件系统（ext4/FAT32）还是伪文件系统（ramfs），全部包装为 `MountFS`，使路径解析、子挂载管理、挂载传播通过统一的 MountFS 层处理。
