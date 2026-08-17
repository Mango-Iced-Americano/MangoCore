---
title: "文件系统初始化与根文件系统设置"
module: "fs/init"
category: fs
status: draft
owner: MangoCore Team
last_updated: 2026-08-07
code_paths:
  - "os/src/fs/mod.rs"
  - "os/src/fs/boot_block.rs"
  - "os/src/drivers/block/descriptor.rs"
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
  |-- timer_cpu_init()                 初始化本 CPU 调度 tick
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
   |     |-- fs::mount_boot_block_devices(&BootConfig)
    |     |     |-- 发布驱动描述符对应的启动注册表和 /dev 节点
     |     |     |-- Step 3：root= / mango.root= 选择 /sdcard；未显式选择时回退首原始盘
  |
   |-- task::add_initproc()            加载 init 进程（fd 0/1/2 使用 /dev/tty）
   |-- task::run_tasks()               进入调度
   |     |-- /sbin/init: 挂载 proc/sys/run/tmp/dev/shm
```

### 2.1 两种启动模式

**initramfs 模式**（默认，由 `Cargo.toml` 中 `default = ["initramfs", "ext4_another_backend"]` 控制）：

1. 创建空 `RamFS` 作为根文件系统。
2. 通过 `initramfs::unpack_embedded()` 解包编译时通过 `.incbin` 嵌入内核的 newc cpio 归档，将 init 程序、busybox 等注入 RamFS。
3. 仅挂载 devfs，并注册 `/dev/tty` 以建立 PID1 的 fd 0/1/2；其余挂载点只是目录。
4. 调用 `register_boot_block_devices()`：`boot_block` 子模块探测 virtio 块设备、注册 `/dev/vda`、`/dev/vdb` 及 MBR 分区节点，不打开或挂载其文件系统。`boot_block` 会先验证驱动提供的 `BlockDeviceDescriptor`，再将所有原始块设备及其发现的 MBR 分区名称和主次设备号发布到 DevFS 和启动注册表。`root=` 优先于 `mango.root=`，仅选择挂载到 `/sdcard` 的卷：显式 `root=initramfs` 保留 initramfs，其他非空值按已注册的原始盘或 MBR 分区节点名解析（可带一个 `/dev/` 前缀）；未显式提供 root 选择器时才回退首个原始设备。内核不解析 `mango.tools=`，也不挂载 `/tools`；该挂载由 initramfs 用户态按需执行。磁盘名由驱动声明：virtio 为 `vd*`，SATA 为 `sd*`，MMC 为 `mmcblk*`，未声明的设备才使用 `blk*`。
5. `/sbin/init` 挂载 procfs、sysfs、`/run`、`/dev/shm`，并在非 regression 模式下将 x0 挂载到 `/sdcard`、将 x1（优先 `/dev/vdb1`，回退 `/dev/vdb`）挂载到 `/tools`。`profile=buildstorm` 是例外：只挂载 x0，准备 `/proc`、`/sys`、`/dev`、`/tmp` 后 chroot `/sdcard`，不绑定 tools 盘；`profile=mainline` 是 SATA 板级主线：显式 `root=/dev/sda3` 后校验 `/sdcard` 为完整根，将运行时伪文件系统 bind 进 P3 并 chroot 执行持久根 init。当前实板 P1 仅含官方 `glibc/musl` 测试载荷，不能作为系统根。
6. `/tmp` 优先 bind `/sdcard/tmp`；x0 或 bind 失败时挂载 tmpfs。块设备故障只打印 warning，不 panic。

ext4 动态挂载通过 `fs::ext4_backend` 分派到 `another_ext4`（可持久化写入且要求可靠 flush）；
`ext4_lwext4_backend` 仅在显式 legacy feature 下编译，避免默认路径链接旧 C 后端。

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

初始化流程：new RamFS → MountFS 包装 → 解包 cpio → 创建挂载点 → 挂载 devfs。块设备在此阶段不参与，后续由 `register_boot_block_devices()` 注册为设备节点，并仅按 root 选择器决定是否挂载 `/sdcard`。

`detect_fs()` 读取块 0 的一个完整块（BLOCK_SIZE）：首先检查偏移 510 处 MBR 签名 `0x55AA`，若无 MBR 则检查偏移 1024 + 56 = 1080 处 ext4 超级块魔数 `0xEF53`。

## 4. 默认挂载点

内核 `prepare_kernel_bootstrap_filesystem()` 只创建以下挂载点并挂载 devfs；PID1 随后使用 mount syscall 覆盖对应目录：

| 挂载点 | 文件系统类型 | 说明 |
|--------|-------------|------|
| `/dev` | devfs | 设备文件系统，注册 tty、null、zero、urandom、full、random、console、ptmx、pts、rtc、cpu_dma_latency、misc/rtc |
| `/dev/shm` | PID1 tmpfs | 共享内存；内核只提供 devfs cover 目录 |
| `/proc` | PID1 procfs | 进程信息文件系统，动态内容禁用 dentry cache |
| `/sys` | PID1 sysfs | 内核对象文件系统，动态内容禁用 dentry cache |
| `/tmp` | PID1 bind 或 tmpfs | 优先绑定 `/sdcard/tmp`，失败时 tmpfs |
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
- **initramfs 中块设备延迟探测**：块设备探测需要连续物理页 DMA；initramfs 路径在网络初始化后发布所有原始盘和 MBR 分区（`register_boot_block_devices`），由驱动描述符定义节点；内核仅按 root 选择器或无选择器的首原始盘回退决定 `/sdcard`，不依赖 x0/x1 槽位。
- **不可递归触发 VFS_ROOT**：initramfs 解包期间（`unpack_newc`）严禁调用 `vfs_root()`，必须使用传入的 `root` 参数，否则引发递归 lazy_static 初始化死锁。
- **块设备故障不 panic**：显式 root 选择器解析不到设备、或设备上没有可挂载文件系统时，只打印 warning 并保留 initramfs；不会回退到另一块设备。这对调试和 CI 环境至关重要。
- **主线根切换 fail-closed**：`profile=mainline` 只使用命令行明确指定的 P3，不会在分区间猜测或回退；P3 必须存在可执行的 `/sbin/init`、`/init`、`/initproc`、`/bin/busybox`、`/bin/sh` 或 `/bash` 之一，否则回到 initramfs rescue shell。当前实板以静态 BusyBox 作为 PID1 兜底。
- **MountFS 包装统一入口**：无论底层是磁盘文件系统（ext4/FAT32）还是伪文件系统（ramfs），全部包装为 `MountFS`，使路径解析、子挂载管理、挂载传播通过统一的 MountFS 层处理。
