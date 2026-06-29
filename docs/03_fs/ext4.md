---
title: "Ext4 文件系统"
module: "fs/ext4"
category: fs
status: draft
owner: "MangoCore Team"
last_updated: "2026-06-29"
code_paths:
  - "os/src/fs/ext4/"
entry_points:
  - "Ext4FileSystem"
  - "Ext4Inode"
  - "Ext4OSInode"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "open*"
    - "read*"
    - "write*"
    - "stat*"
    - "rename*"
    - "link*"
    - "unlink*"
    - "symlink*"
    - "mkdir*"
    - "rmdir*"
    - "chmod*"
    - "chown*"
    - "truncate*"
    - "fsync*"
    - "getdents*"
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
  - "docs/ltp/ltp_fs_plan.md"
---

## 概述

Ext4 是 MangoCore 的主力持久化文件系统。它直接运行在块设备之上（virtio-blk），实现了 Linux ext4 格式的核心子集，包括 extent 树、稀疏文件、符号链接、硬链接、目录项缓存和元数据块缓存。所有 I/O 路径在 VFS 层通过 PageCache 缓存，写操作在 PageCache 回写时同步到块设备。

ext4 是项目中最复杂的文件系统模块。代码位于 `os/src/fs/ext4/`。

## Ext4FileSystem

`Ext4FileSystem` 是 ext4 实例的核心结构体，持有块设备引用、超级块、元数据缓存、inode 缓存、PageCache 注册表和目录查找缓存。系统支持多个 ext4 实例共存（如 `/sdcard` 和 `/tools`），所有实例通过 `EXT4_REGISTRY` 全局表注册。

### 块设备后端

块设备通过 `BlockDevice` trait 抽象。riscv64 使用 `virtio_blk`，loongarch64 使用 `virtio_blk_pci`。所有块 I/O 操作（`read_block`/`write_block`）通过该 trait 转发。元数据路径使用 `MetaBlockCache` 减少读盘次数；数据路径通过 `PageCache` 缓存。

### 超级块

`Ext4Superblock` 是对应 ext4 on-disk superblock 的 `#[repr(C)]` 结构体。`rust_main()` 初始化阶段在块偏移 1024 处读取超级块，验证魔数 `0xEF53`，提取块大小、每组块数、每组 inode 数等关键参数。支持 `rev_level` 为 0（经典）和 1（动态）两种格式，兼容 `EXT4_FEATURE_INCOMPAT_64BIT`、`EXT4_FEATURE_INCOMPAT_FLEX_BG`、`EXT4_FEATURE_INCOMPAT_EXTENTS` 等特性标志。

当前挂载路径仅检测 ext4 魔数 `0xEF53`，不强制执行特性兼容性检查。`rev_level` 支持 0（经典）和 1（动态）两种格式。

### 目录查找缓存

`Ext4DirectoryLookupCache` 基于 LRU 淘汰策略加速 `find()` 操作。每个目录维护一个 `name -> ino` 的 BTreeMap，通过版本号（`bump_version`）检测目录变更。默认最多 128 个目录缓存，每个目录最多 1024 个条目。`rename`、`unlink`、`rmdir`、`create` 等操作自动使关联缓存失效。

## Inode 管理

### Ext4Inode

`Ext4Inode` 是对应 on-disk inode 的 `#[repr(C)]` 结构体，包含 mode/uid/size/时间戳/链接计数/块指针/扩展属性等字段。通过 `get_inode_ref()` 从磁盘读取，通过 `write_back_inode()` 写回。64 位大小由 `size` 和 `size_hi` 拼接而成。

`CachedExt4Inode` 是带位置的缓存封装，记录 inode 在 inode table 中的块号和块内偏移，支持原地写回。`inode_cache`（`BTreeMap<u32, Arc<Mutex<CachedExt4Inode>>>`）减少 inode table 重复读盘。

### Extent 树

extent 是 ext4 的默认数据块映射方式（`EXT4_INODE_FLAG_EXTENTS`）。`Ext4Extent` 每条记录包含起始逻辑块号、块数和起始物理块号。`Ext4ExtentHeader` 位于树节点开头，包含魔数（`0xF30A`）、有效条目数、最大条目数和树深度。

`get_pblock_idx()` 负责将逻辑块号翻译为物理块号：先遍历根节点（内嵌在 inode `block[15]`），深度 > 0 时递归下降到索引节点，在叶节点中对 extent 数组执行二分查找。查找调用者必须验证返回的 extent 覆盖范围——`binsearch_extent` 不保证完全覆盖。

### 稀疏文件

稀疏文件（hole）是 ext4 对 Linux 的关键兼容特性。当 `get_pblock_idx()` 找不到对应 extent 时返回 `Err`，调用者按全零处理。`read_at` 路径对 hole 填零；`write_at` 路径调用 `insert_inode_pblk_from()` 分配新块并插入 extent 树。

## 目录项操作

`Ext4DirEntry` 是 on-disk 目录项结构，包含 inode 号、entry_len、name_len、文件类型和文件名。目录项以不定长链表形式存储在目录文件的数据块中，`entry_len` 指向下一个条目。

`dir_find_entry()` 线性扫描目录所有数据块，匹配文件名。获取到 `Ext4DirSearchResult` 后调用者可修改或删除条目。`write_entry()` / `write_de_to_blk()` 负责新的目录项写入。

核心目录操作：

| 操作 | 方法 | 说明 |
|------|------|------|
| 查找 | `find()` | 通过 `dir_find_entry` 扫描 + `dir_lookup_cache` 加速 |
| 创建 | `create()` | 分配 inode → 初始化 inode → 写入目录项 |
| 链接 | `link()` | 增加 links_count → 添加目录项 |
| 解除链接 | `unlink()` | 删除目录项 → 减少 links_count → links_count=0 时释放数据块和 inode |
| 重命名 | `rename()` | 跨目录重命名，处理旧目录项删除和新目录项写入 |
| 符号链接 | `symlink()` | 短链接（≤60B）存储在 inode block[0..14]；长链接写入数据块 |

## 文件 I/O

### 读取路径

`Ext4OSInode::read_at()` 是 VFS 层的入口：

1. 检查文件类型（目录返回 EISDIR）
2. 尝试从 `cached_file_size` 获取文件大小
3. 符号链接优先尝试 `cached_symlink_target`
4. 普通文件通过 `get_new_page_cache()` 获取 PageCache，调用 `pc.read()`
5. PageCache 不存在时回退到 `Ext4FileSystem::read_at()` 直接块设备读取
6. 直接读取路径：逐逻辑块调用 `get_pblock_idx()` 获取物理块号，对 hole 填零

`read_at_user()` 类似但直接通过 PageCache 的 `read_user()` 写入用户缓冲区，跳过内核暂存。

### 写入路径

`Ext4OSInode::write_at()` 的核心流程：

1. 调用 `ensure_blocks_allocated()` 确保写入范围内的所有逻辑块都有对应的物理块（nodelalloc 策略，不延迟分配）
2. 更新 inode size、mtime、ctime，并 push 到 `inode_cache` 标记脏
3. 通过 PageCache 的 `write()` 写入数据，传入 `old_size` 避免超出原 EOF 的 page 触发不必要的后端读

`ensure_blocks_allocated()` 对每个逻辑块调用 `get_pblock_idx()`，如果发现 hole 则调用 `insert_inode_pblk_from()` 分配物理块并更新 extent 树。

### PageCache 集成

PageCache 在 ext4 中的状态机：`Loading -> UpToDate -> Dirty -> Writeback`。写操作标记脏页，脏页总量超过高水位时触发 LRU 回收。`flush_metadata_cache()` 同时写回脏 inode 和脏元数据块。

## 块分配

### 块分配器

`balloc` 模块在块组级别管理空闲块位图。`balloc_alloc_block()` 查找空闲块、设置位图、更新块组计数器。`balloc_alloc_contiguous_blocks()` 尝试分配连续块（最大 64 块，对应 256KB）。

### Inode 分配器

`ialloc` 模块管理 inode 位图。`ialloc_alloc_inode()` 依次遍历块组，找到有空闲 inode 的块组，在位图中分配，更新块组空闲计数。`ialloc_free_inode()` 释放 inode 位。

### 块组描述符

`Ext4BlockGroup` 缓存每个块组的描述符信息（块位图块号、inode 位图块号、inode table 块号、空闲块/inode 计数）。支持 csum 校验（`set_block_group_checksum`/`set_block_group_ialloc_bitmap_csum`）。

## 元数据缓存

`MetaBlockCache` 是通用的元数据块缓存（LRU 淘汰，默认容量可配）。所有超级块读、块组描述符读、inode table 读、目录块读写都经过此缓存。`with_block_mut()` 支持原地修改并标记脏块，`flush_all_dirty()` 集中写回脏块。元数据脏写支持 defer mode（`meta_batch_active`），在批量创建 symlink 或文件时合并超级块和块组描述符的更新以减少磁盘写入。

## 已知缺失

### 日志（Journal）

**ext4 日志未实现。** 这是 ext4 实现的最大功能缺口。标准 Linux ext4 通过 `EXT4_FEATURE_COMPAT_HAS_JOURNAL` 使用 jbd2（Journaling Block Device 2）提供崩溃安全的元数据事务。当前实现直接修改 on-disk 元数据，写入顺序不保证原子性。如果系统在写入过程中崩溃，可能产生不一致的文件系统。mkfs.ext4 创建的日志 inode（ino=8）在挂载时被忽略。

### 其他缺失

- 延迟分配（delalloc）：当前采用 nodelalloc 策略，写入立即分配物理块
- 在线调整大小和在线碎片整理
- EA（扩展属性）系统调用的完整支持
- HTREE 目录索引（大目录线性扫描性能受限）
- 纳秒时间戳精度（当前使用秒级时间戳）
- i_version / NFS 支持

## 测试映射

### LTP 覆盖

ext4 通过 LTP 文件系统测试用例验证。关键覆盖领域：

| 测试范围 | 代表性用例 | 状态 |
|----------|-----------|------|
| 文件创建与打开 | `open*`, `creat*`, `close*` | 通过 |
| 读写 | `read*`, `write*`, `pread*`, `pwrite*` | 通过 |
| 目录操作 | `mkdir*`, `rmdir*`, `getdents*` | 通过 |
| 链接与重命名 | `link*`, `symlink*`, `rename*`, `unlink*` | 通过 |
| 元数据 | `stat*`, `chmod*`, `chown*`, `truncate*` | 通过 |
| 同步 | `fsync*`, `sync*` | 通过 |
| 扩展操作 | `fallocate*`, `ftruncate*` | 通过 |

### OSComp 覆盖

- `basic`: ext4 根文件系统基本启动与文件操作
- `busybox`: shell 脚本在 ext4 上的文件创建、读取、管道操作
- `lua`: Lua 脚本解释器从 ext4 加载脚本
- `libctest`: libc 标准文件操作测试
- `iozone`: ext4 上的持续 I/O 性能基准测试

## 已知问题

- 日志缺失意味着系统崩溃后文件系统可能需要 `fsck` 修复。不要在没有正常关机的情况下信任 ext4 分区的完整性
- 大目录（超过 1024 个条目）的线性扫描性能差（无 HTREE 支持），但目录查找缓存可以缓解热点目录的重复查询
- 稀疏文件的 `seek` 操作需要调用者注意 `get_pblock_idx` 返回 `Err` 时按 hole 处理
- 使用 `meta_batch_active` 时如果系统崩溃，批量更新可能部分生效
- ext4 `rename` 的跨目录原子性无法保证（缺少日志支持）
