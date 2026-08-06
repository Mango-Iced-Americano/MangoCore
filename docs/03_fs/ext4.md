---
title: "Ext4 文件系统"
module: "fs/ext4"
category: fs
status: draft
owner: "MangoCore Team"
last_updated: "2026-08-05"
code_paths:
  - "os/src/fs/ext4/"
  - "os/src/fs/filesystem.rs"
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
  - "docs/10_plan/ext4-lwext4-migration-audit-20260718.md"
  - "docs/03_fs/architecture.md"
  - "docs/03_fs/vfs-core.md"
  - "docs/03_fs/page-cache.md"
  - "docs/03_fs/init-and-rootfs.md"
  - "docs/ltp/ltp_fs_plan.md"
---

## 概述

> 本文主体描述仍保留的 legacy 纯 Rust `os/src/fs/ext4/`，不代表当前
> `ext4_lwext4` 融合候选的实现细节或生产状态。新旧差异、onboard 修正覆盖、性能判断、
> 双架构专项证据和 journal/orphan blocker 见
> [`ext4-lwext4-migration-audit-20260718.md`](../10_plan/ext4-lwext4-migration-audit-20260718.md)。

Ext4 是 MangoCore 的主力持久化文件系统。它直接运行在块设备之上（virtio-blk），实现了 Linux ext4 格式的核心子集，包括 extent 树、稀疏文件、符号链接、硬链接、目录项缓存和元数据块缓存。所有 I/O 路径在 VFS 层通过 PageCache 缓存，写操作在 PageCache 回写时同步到块设备。

ext4 是项目中最复杂的文件系统模块。代码位于 `os/src/fs/ext4/`。当前 SMP
契约覆盖目录 gate、per-FS `rename_gate` 与 `inode_txn -> PageCache::op_gate`；
这不等同于 MountFS、fd table 和所有 VFS 深路径均已完成并行性能重构。

## Ext4FileSystem

`Ext4FileSystem` 是 ext4 实例的核心结构体，持有块设备引用、超级块、元数据缓存、inode 缓存、PageCache 注册表和目录查找缓存。系统支持多个 ext4 实例共存（如 `/sdcard` 和 `/tools`），所有实例通过 `EXT4_REGISTRY` 全局表注册。

### 块设备后端

块设备通过 `BlockDevice` trait 抽象。riscv64 使用 `virtio_blk`，loongarch64 使用 `virtio_blk_pci`。所有块 I/O 操作（`read_block`/`write_block`）通过该 trait 转发。元数据路径使用 `MetaBlockCache` 减少读盘次数；数据路径通过 `PageCache` 缓存。

### 超级块

`Ext4Superblock` 是对应 ext4 on-disk superblock 的 `#[repr(C)]` 结构体。`rust_main()` 初始化阶段在块偏移 1024 处读取超级块，验证魔数 `0xEF53`，提取块大小、每组块数、每组 inode 数等关键参数。支持 `rev_level` 为 0（经典）和 1（动态）两种格式，兼容 `EXT4_FEATURE_INCOMPAT_64BIT`、`EXT4_FEATURE_INCOMPAT_FLEX_BG`、`EXT4_FEATURE_INCOMPAT_EXTENTS` 等特性标志。

通用文件系统发现路径只检测 ext4 魔数 `0xEF53`，不把卷标或 UUID 当作类型判断条件。
但策略控制的 2K1000 P4 可写挂载会额外读取主超级块的 UUID、16 字节卷标、compat 和
incompat feature 字段；只有固定 `MANGO_STATE` 身份且没有 `HAS_JOURNAL`/`RECOVER`
位的文件系统才允许挂载为 `/persist`。这是写权限策略，不改变通用 ext4 类型探测。

### 目录查找缓存

`Ext4DirectoryLookupCache` 基于 LRU 淘汰策略加速 `find()` 操作。每个目录维护一个 `name -> ino` 的 BTreeMap，通过版本号（`bump_version`）检测目录变更。默认最多 128 个目录缓存，每个目录最多 1024 个条目。`rename`、`unlink`、`rmdir`、`create` 等操作自动使关联缓存失效。

SMP 下 `Ext4FileSystem` 按 inode number 复用 canonical `dir_gate`，因此同一 inode
的多个 `Ext4OSInode` wrapper 仍共享一把锁。`find`/readdir 取 parent read gate，
创建、链接、删除和 rmdir 取 parent write gate；children、negative dentry 和 lookup
cache 只能在该 gate 后访问。跨目录 rename 先取 per-FS `rename_gate`，再按祖先优先、
否则 inode ID 升序的规则获取相关目录 gate，并在锁内重新解析源与目标。被覆盖 inode
的最终回收放在 namespace gate 释放之后，避免回收路径反向进入目录锁。

目录修改还必须保持 VFS inode 快照与底层 inode table 一致。`create`、`symlink`、
`link` 和 `rename` 由低层辅助函数写回父 inode 后，`Ext4OSInode` 会重新读取父 inode，
避免下一次目录操作用旧的 size/extent/link count 覆盖刚写入的状态。创建入口在分配 inode
前直接扫描磁盘目录项并拒绝重复名称，不能只依赖可能过期的 dentry cache。

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

目录项的 file type 是枚举值而不是可组合 bit flags；inode mode 必须先提取精确的
`S_IFMT` 类型再转换。删除块内第一条记录时没有前驱可以吸收空间，因此只清零 inode
和记录内容并保留原 `rec_len`；删除其他记录才把长度合并到紧邻前驱。启用 metadata
checksum 时，目录块 CRC 使用目录自身的 inode 号和 generation，不能从块内第一条
目录项推断父 inode，因为该记录可能已经被删除。

`dir_find_entry()` 线性扫描目录所有数据块，匹配文件名。获取到 `Ext4DirSearchResult` 后调用者可修改或删除条目。`write_entry()` / `write_de_to_blk()` 负责新的目录项写入。

核心目录操作：

| 操作 | 方法 | 说明 |
|------|------|------|
| 查找 | `find()` | 通过 `dir_find_entry` 扫描 + `dir_lookup_cache` 加速 |
| 创建 | `create()` | 分配 inode → 初始化 inode → 写入目录项 |
| 链接 | `link()` | 增加 links_count → 添加目录项 |
| 解除链接 | `unlink()` | 删除目录项 → 减少 links_count → links_count=0 时释放数据块和 inode |
| 重命名 | `rename()` | 处理同目录/跨目录及覆盖目标，失败时回滚已发布的目录项 |
| 符号链接 | `symlink()` | 短链接（≤60B）存储在 inode block[0..14]；长链接写入数据块 |

同目录 rename 采用“先移除源名称、再处理覆盖目标、最后发布目标名称”的保守事务顺序；
发布失败时恢复源名称和原覆盖目标，覆盖目标的链接计数与目录缓存只在成功后更新。这个顺序
及回滚改善运行期失败语义，但它本身不是某个磁盘布局根因的唯一证明；由于尚无 journal，
也不提供掉电原子性。

可变长目录项必须按块内 `rec_len` 算术维护 framing。删除块首记录时没有前驱，必须保留
原 `rec_len` 并清空 inode/body；若把 `prev_offset=0` 当作真实前驱，会读取并累加自身长度，
把记录跨度从 `R` 错写为 `2R`，使扫描跳过下一项。删除非块首记录才可把长度并入紧邻前驱。
启用 metadata checksum 时，CRC 的身份输入必须是目录自身 inode 和 generation，不能取块首
目录项的 inode。完整追溯与历史证据边界见
[`09_debug/la64_on_board/260710/09-ext4-variable-dirent-rename.md`](../09_debug/la64_on_board/260710/09-ext4-variable-dirent-rename.md)。

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

带 `EXT4_BG_BLOCK_UNINIT` 的块组采用 lazy bitmap 初始化。首次分配时内核按该块组
实际块数重建位图，标记 super/GDT、块位图、inode 位图、inode table 以及尾部无效位，
随后清除 UNINIT、重算 bitmap checksum，并校正块组与超级块空闲计数。bigalloc 和
异常的 group 0 UNINIT 当前明确返回 `EIO`，不会把全零位图当作可用空间。

释放跨块组范围时按每个块组实际边界拆分，只对原来置位的 bit 增加空闲计数，从而避免
重复释放造成 summary counter 漂移。超级块更新总是从当前 metadata cache/batch 快照
继续累加，不能从挂载时的只读副本重新计算，否则一次批量操作中的前序更新会被覆盖。
完整故障算术、八类代码缺陷和验证边界见
[`18-ext4-lazy-init-and-block-group-accounting.md`](../09_debug/la64_on_board/260710/18-ext4-lazy-init-and-block-group-accounting.md)。

### Inode 分配器

`ialloc` 模块管理 inode 位图。`ialloc_alloc_inode()` 依次遍历块组，找到有空闲 inode 的块组，在位图中分配，更新块组空闲计数。`ialloc_free_inode()` 释放 inode 位。

带 `EXT4_BG_INODE_UNINIT` 的非零块组在首次分配时初始化 inode bitmap，并将超出该组
有效 inode 数的尾部 bit 置为已用。每个新分配的 inode slot 在发布 bitmap bit 前清零，
同时维护 `itable_unused`、目录数、块组和超级块计数。释放路径先写入非零 deletion time，
并拒绝对已经清零的 bitmap bit 重复计数。

### 块组描述符

`Ext4BlockGroup` 缓存每个块组的描述符信息（块位图块号、inode 位图块号、inode table 块号、空闲块/inode 计数）。支持 csum 校验（`set_block_group_checksum`/`set_block_group_ialloc_bitmap_csum`）。

## 元数据缓存

`MetaBlockCache` 是通用的元数据块缓存（LRU 淘汰，默认容量可配）。所有超级块读、块组描述符读、inode table 读、目录块读写都经过此缓存。`with_block_mut()` 支持原地修改并标记脏块，`flush_all_dirty()` 集中写回脏块。元数据脏写支持 defer mode（`meta_batch_active`），在批量创建 symlink 或文件时合并超级块和块组描述符的更新以减少磁盘写入。

数据块或 extent/目录元数据块被释放后，`MetaBlockCache::invalidate_range()` 会立即丢弃
对应物理块。释放后的脏缓存不能跨越块所有权转移继续存在，否则延迟 flush 可能覆盖
同一物理块的新文件内容。
父 inode 快照、延迟回收与 cache owner 的完整证据链见
[`18a-ext4-metadata-cache-and-inode-snapshot.md`](../09_debug/la64_on_board/260710/18a-ext4-metadata-cache-and-inode-snapshot.md)。

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

2026-07-15 的持久化回归使用全新 ext4 fixture，将 `fs_test` chroot 到被测文件系统，
rv64/la64 均为 63/63；每轮关机后宿主 `e2fsck -fn` 均通过。两架构还分别完成
`basic + iozone + libctest` 的 musl/glibc 组合。2K1000LA P4 `/persist` 上完成 16 MiB
写入/同步/复制校验/截断/删除探针，以及 16 MiB、64 KiB record 的 iozone
write/rewrite/read/reread/random read/random write，均返回 0。

## 已知问题

- 日志缺失意味着系统崩溃后文件系统可能需要 `fsck` 修复。不要在没有正常关机的情况下信任 ext4 分区的完整性
- 大目录（超过 1024 个条目）的线性扫描性能差（无 HTREE 支持），但目录查找缓存可以缓解热点目录的重复查询
- 稀疏文件的 `seek` 操作需要调用者注意 `get_pblock_idx` 返回 `Err` 时按 hole 处理
- 使用 `meta_batch_active` 时如果系统崩溃，批量更新可能部分生效
- ext4 `rename` 的跨目录原子性无法保证（缺少日志支持）
- rename 回归除 LTP 外覆盖同目录空目标、覆盖已有目标、源/目标同 inode no-op，以及
  APK/Python 包装器的 `temporary file -> final file` 安装模式
