---
title: "文件系统初始化与根文件系统设置"
module: "fs/init"
category: fs
status: draft
owner: MangoCore Team
last_updated: 2026-06-29
code_paths:
  - "os/src/fs/mod.rs"
  - "os/src/fs/ext4_backend.rs"
  - "os/src/fs/filesystem.rs"
  - "os/src/fs/initramfs.rs"
entry_points:
  - "VFS_ROOT"
  - "fs::initramfs_init"
  - "force_ramfs"
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

根文件系统初始化是内核启动的关键阶段。它在内存管理初始化之后执行，负责探测块设备、识别文件系统类型、创建 VFS 根并挂载默认伪文件系统。无论底层是 ext4、FAT32 还是 ramfs，最终的根文件系统都被包装为统一的 `MountFS` 实例，供上层系统和用户进程通过 VFS 接口访问。

整个初始化逻辑集中在 `os/src/fs/mod.rs` 的 `VFS_ROOT` lazy_static 和 `mount_common_filesystems` 函数中，由 `rust_main` 在合适的时机触发。

### Ext4 后端选择

`os/src/fs/ext4_backend.rs` 是所有 ext4 打开路径的唯一 facade。Cargo 必须且只能启用 `ext4_lwext4_backend`、`ext4_legacy_backend`、`ext4_another_backend` 之一；默认选择 lwext4。`VFS_ROOT`、`mount_block_fs` 与 `sys_mount` 都调用该 facade，因此构建不会在运行时回退或切换 ext4 后端。`EXT4_BACKEND=lwext4|legacy|another` 由 `os/make/ext4_backend.mk` 映射到对应 Cargo feature。`another` 当前通过 `ext4_another` bridge 以只读方式挂载：只接受 `load_read_only_checked` 验证的干净介质，常规文件读取复用 Mango PageCache，所有写入与元数据变更返回 `EROFS`。

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
  |     |     |-- [mount_common_filesystems()]
  |     |     |     |-- /dev (devfs)
  |     |     |     |-- /proc (procfs)
  |     |     |     |-- /sys (sysfs)
  |     |     |     |-- /tmp (tmpfs)
  |     |     |     |-- /dev/shm (tmpfs)
  |     |     |     |-- /mnt, /run, /var/tmp (目录)
  |     |
  |     |-- drivers::init_net_device()
  |     |-- net::config::init()
  |     |-- fs::mount_boot_block_devices()  探测块设备并挂载
  |
  |-- [initramfs 特性未启用: legacy 路径]
  |     |-- drivers::init_net_device()
  |     |-- net::config::init()
  |     |-- [VFS_ROOT 被延迟到首次访问时初始化]
  |           |-- pre_mount() / detect_fs()  块设备探测
  |           |-- ext4/fat32 或 fallback ramfs
  |           |-- mount_common_filesystems()
  |
  |-- task::add_initproc()            加载 init 进程
  |-- task::run_tasks()               进入调度
```

### 2.1 两种启动模式

**initramfs 模式**（默认，由 `Cargo.toml` 中 `default = ["initramfs"]` 控制）：

1. 创建空 `RamFS` 作为根文件系统。
2. 通过 `initramfs::unpack_embedded()` 解包编译时通过 `.incbin` 嵌入内核的 newc cpio 归档，将 init 程序、busybox 等注入 RamFS。
3. 挂载 devfs、procfs、sysfs、tmpfs 等伪文件系统。
4. 调用 `mount_boot_block_devices()` 探测 virtio 块设备，注册 `/dev/vda` 和 `/dev/vdb`，解析 MBR 分区，将 x0 挂载到 `/sdcard`、x1 挂载到 `/tools`。
5. 块设备故障只打印 warning，不 panic。

**Legacy 模式**（`initramfs` 特性未启用）：

1. 块设备已就绪后首次访问 `VFS_ROOT` 时触发初始化。
2. `pre_mount()` 调用 `detect_fs()` 读取块 0 的引导扇区，识别文件系统类型。
3. 根据 `FS_Type` 打开 ext4 或 FAT32 文件系统，包装为 `MountFS`。
4. 如果均未识别（`FS_Type::Null`），fallback 到 ramfs。
5. 挂载 devfs、procfs、sysfs、tmpfs。

### 2.2 force_ramfs 调试开关

```rust
static FORCE_RAMFS: AtomicBool = AtomicBool::new(false);

pub fn force_ramfs() {
    FORCE_RAMFS.store(true, Ordering::Relaxed);
    crate::drivers::block::disable_block_device();
}
```

调用 `force_ramfs()` 可在块设备初始化之前跳过物理设备探测，直接使用 ramfs 启动。这个路径用于 VFS 层调试或 legacy block root 模式。它同时禁用块设备子系统，确保后续代码不会误访问硬件。

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

初始化流程：new RamFS → MountFS 包装 → 解包 cpio → 挂载伪文件系统。块设备在此阶段不参与，后续通过 `mount_boot_block_devices()` 以子挂载形式接入。

### 3.2 Legacy 模式

```rust
#[cfg(not(feature = "initramfs"))]
lazy_static! {
    pub static ref VFS_ROOT: Arc<MountFS> = {
        let fs_type = if FORCE_RAMFS.load(Ordering::Relaxed) {
            FS_Type::Null
        } else {
            pre_mount()
        };
        let mfs = match fs_type {
            FS_Type::Fat32 => { /* open FAT32 -> MountFS */ }
            FS_Type::Ext4  => { /* open ext4 -> MountFS */ }
            FS_Type::Null  => { /* new RamFS -> MountFS */ }
        };
        mount_common_filesystems(&mfs);
        mfs
    };
}
```

初始化流程：可选择跳过块设备检测 → `pre_mount()` → `detect_fs()` → 打开具体文件系统 → MountFS 包装 → 挂载伪文件系统。

`detect_fs()` 读取块 0 的一个完整块（BLOCK_SIZE）：首先检查偏移 510 处 MBR 签名 `0x55AA`，若无 MBR 则检查偏移 1024 + 56 = 1080 处 ext4 超级块魔数 `0xEF53`。

## 4. 默认挂载点

`mount_common_filesystems()` 在根文件系统就绪后统一挂载以下伪文件系统和目录：

| 挂载点 | 文件系统类型 | 说明 |
|--------|-------------|------|
| `/dev` | devfs | 设备文件系统，注册 tty、null、zero、urandom、full、random、console、ptmx、pts、rtc、cpu_dma_latency、misc/rtc |
| `/dev/shm` | tmpfs | 共享内存，容量 16MB，权限 01777（sticky bit） |
| `/proc` | procfs | 进程信息文件系统，权限 0555，禁用 dentry cache |
| `/sys` | sysfs | 内核对象文件系统，权限 0555，禁用 dentry cache |
| `/tmp` | tmpfs | 临时文件系统，无大小限制，权限 01777 |
| `/mnt` | (目录) | 通用挂载点，权限 0755 |
| `/run` | (目录) | 运行时文件，权限 0755 |
| `/var/tmp` | (目录) | 临时文件备选，权限 01777 |

devfs 使用 `MountFS` 子挂载注入，`/dev/shm` 的 tmpfs 作为 devfs 的子挂载注册。procfs 和 sysfs 禁用 dentry cache，因为它们的内容动态生成。

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
| force_ramfs 回退 | `force_ramfs` | — | 调试 | pass |

## 8. 关键设计点

- **VFS_ROOT 初始化顺序**：必须发生在 `mm::init()` 之后（需要堆分配器），且在 `task::add_initproc()` 之前（init 进程需要根文件系统）。
- **initramfs 中块设备延迟探测**：块设备探测需要连续物理页 DMA，必须在内存分配压力低时进行。initramfs 路径将块设备探测（`mount_boot_block_devices`）推迟到网络初始化之后、preload payload 安装之前。
- **不可递归触发 VFS_ROOT**：initramfs 解包期间（`unpack_newc`）严禁调用 `vfs_root()`，必须使用传入的 `root` 参数，否则引发递归 lazy_static 初始化死锁。
- **块设备故障不 panic**：无论是根文件系统未识别还是 tools 盘缺失，均 fallback 到 ramfs 或打印 warning 继续执行。这对调试和 CI 环境至关重要。
- **MountFS 包装统一入口**：无论底层是磁盘文件系统（ext4/FAT32）还是伪文件系统（ramfs），全部包装为 `MountFS`，使路径解析、子挂载管理、挂载传播通过统一的 MountFS 层处理。
