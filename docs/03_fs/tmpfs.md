---
title: "tmpfs 与 ramfs — 内存文件系统"
module: "fs/tmpfs+ramfs"
category: fs
status: draft
owner: MangoCore Team
last_updated: 2026-06-29
code_paths:
  - "os/src/fs/tmpfs/"
  - "os/src/fs/ramfs/"
entry_points:
  - "LockedTmpFSInode"
  - "LockedRamFSInode"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "open*"
    - "read*"
    - "write*"
    - "stat*"
    - "link*"
    - "rename*"
    - "unlink*"
    - "mkdir*"
    - "rmdir*"
    - "chmod*"
    - "getdents*"
    - "tmpfs01"
  oscomp:
    - "basic"
    - "busybox"
    - "lua"
related_docs:
  - "docs/03_fs/architecture.md"
  - "docs/03_fs/vfs-core.md"
  - "docs/03_fs/init-and-rootfs.md"
  - "docs/03_fs/page-cache.md"
---

# tmpfs 与 ramfs — 内存文件系统

## 1. 概述

MangoCore 提供两种纯内存文件系统：`tmpfs` 和 `ramfs`。它们都不依赖块设备，所有数据驻留在内存中，断电即失。两者的设计目标不同，分别服务于不同的使用场景。

| 属性 | tmpfs | ramfs |
|------|-------|-------|
| 存储后端 | PageCache（不可回收页） | 物理帧 FrameTracker |
| 数据备份 | 仅内存，无回写目标 | 仅内存，无回写目标 |
| 大小限制 | 支持（`max_bytes` 挂载选项） | 可选页数配额 |
| statfs 动态计算 | 是，基于配额或可用内存 | 否，返回固定值 |
| 符号链接 | 支持，通过 PageCache 存储 | 支持，通过物理页存储 |
| 扩展属性 | 支持（user.*） | 支持（user.*） |
| 典型用途 | `/tmp`、`/dev/shm` | 根文件系统、initramfs、故障回退 |
| 实现复杂度 | 中（PageCache 集成） | 低（直接物理页操作） |

## 2. tmpfs — PageCache 后端的内存文件系统

`tmpfs` 以 PageCache 作为唯一数据存储后端。每个文件或符号链接 inode 在创建时初始化一个 `PageCache` 实例并标记为不可回收（unevictable），确保数据不会被内核页面回收机制逐出。

### 2.1 核心数据结构

```
TmpFS (文件系统实例)
  |-- root_inode: Arc<LockedTmpFSInode>
  |-- size_limit: Option<u64>
  |-- current_size: AtomicU64

LockedTmpFSInode (inode 包装器)
  |-- Mutex<TmpFSInode>

TmpFSInode (inode 内部数据)
  |-- parent: Weak<LockedTmpFSInode>
  |-- self_ref: Weak<LockedTmpFSInode>
  |-- children: BTreeMap<String, Arc<LockedTmpFSInode>>
  |-- page_cache: Option<Arc<PageCache>>
  |-- xattrs: BTreeMap<String, Vec<u8>>
  |-- file_size: usize
  |-- metadata: Metadata
  |-- fs: Weak<TmpFS>
```

目录结构使用 `BTreeMap` 组织，键为文件名，值为子 inode 的 `Arc` 引用。父子关系通过 `Weak` 引用避免循环引用导致的内存泄漏。

### 2.2 PageCache 后端

`TmpfsPageCacheBackend` 实现了 `PageCacheBackend` trait。由于 tmpfs 数据仅存在于内存，`read_page` 返回全零页（无持久化数据），`write_page` 为空操作。`npages` 返回 0 表示无后端页数限制，PageCache 持有的页不可回收。

### 2.3 大小配额管理

`TmpFS` 实例记录一个可选的 `size_limit`（字节）和一个原子化的 `current_size`。`check_space` 在每次写入扩文件大小时检查配额；`add_size` 在写入和截断时维护 `current_size` 的增减。无限制时，`super_block` 基于当前可用物理内存动态计算总容量。

### 2.4 重命名与祖先检测

`rename` 操作采用 inode_id 锁排序策略避免死锁。当源和目标父目录不同时，按 inode_id 从小到大加锁。`is_ancestor_of` 在跨目录移入目录时检测目录循环，沿父链向上遍历，一次只锁一个 inode，无死锁风险。

## 3. ramfs — 物理页直接管理

`ramfs` 使用 `BTreeMap<usize, Arc<FrameTracker>>` 存储文件数据，每个物理页通过 `FrameTracker` 直接管理。参考 DragonOS 的 ramfs 实现设计，用于 VFS 层调试和不依赖块设备的启动场景。

### 3.1 核心数据结构

```
RamFS (文件系统实例)
  |-- root_inode: Arc<LockedRamFSInode>
  |-- max_pages: usize
  |-- page_count: Mutex<usize>

LockedRamFSInode (inode 包装器)
  |-- Mutex<RamFSInode>

RamFSInode (inode 内部数据)
  |-- parent: Weak<LockedRamFSInode>
  |-- self_ref: Weak<LockedRamFSInode>
  |-- children: BTreeMap<String, Arc<LockedRamFSInode>>
  |-- pages: BTreeMap<usize, Arc<FrameTracker>>
  |-- new_page_cache: Mutex<Option<Arc<PageCache>>>
  |-- xattrs: BTreeMap<String, Vec<u8>>
  |-- file_size: usize
  |-- metadata: Metadata
  |-- fs: Weak<RamFS>
```

`pages` 字段将文件内的页索引直接映射到物理帧。读取时，如果页索引存在则将物理页内容拷贝到用户缓冲区；不存在则视为空洞，返回零。

### 3.2 写时按需分配

写入操作在遇到空洞时从帧分配器分配物理页，先检查 `max_pages` 配额。分配失败时回滚配额计数并返回 `ENOMEM`。写入超过文件末尾时自动更新 `file_size`。

### 3.3 与 PageCache 的桥接

ramfs 在 `page_cache()` 方法中提供可选的 PageCache 桥接。文件首次请求 PageCache 时创建 `RamFsPageCacheBackend` 实例，该后端将 `read_page`/`write_page` 委托到 ramfs 的物理页映射。这一桥接使 ramfs 文件可以通过 PageCache 的 `read_user`/`write_user` 路径进行零拷贝 I/O。

### 3.4 缩容页回收

`resize` 在缩容时释放超出新文件大小的物理页，并清零最后一页的尾部避免数据泄漏。`RamFSInode` 被 unlink 时也会归还所有已分配的物理页给帧分配器。

## 4. tmpfs 与 ramfs 的关键差异

### 存储模型

tmpfs 的所有数据访问通过 `PageCache` 进行，PageCache 内部管理页面的状态转换。ramfs 直接操作 `FrameTracker`，使用裸指针进行物理内存拷贝。tmpfs 的 PageCache 后端 `read_page`/`write_page` 仅为接口占位，真正的数据完全在 PageCache 内部维护。

### 大小限制

tmpfs 支持字节级的 `size_limit`，`super_block` 动态计算 `f_blocks`/`f_bfree` 反映实际可用容量。ramfs 的配额以页为单位，`super_block` 返回固定值，不反映实时使用情况。

### 性能特征

ramfs 的读写路径直接操作物理页，单次 `read_at` 中拷贝字节数精确，无 PageCache 状态机开销。tmpfs 依赖 PageCache 的页内偏移计算和后备缓冲区管理，在大文件顺序读写场景下通过 PageCache 的预读机制可能获益。

### 数据生命周期

两者数据都仅存在于内存。ramfs 在 `resize`/`unlink` 时即时释放 `FrameTracker` 归还物理内存。tmpfs 依赖 PageCache 的 truncate 机制释放页面，但 unevictable 标记确保保留页不被内核回收。

## 5. 在系统中的使用

两者通过 VFS 初始化流程集成到系统中：

- **ramfs** 作为 initramfs 的内存根文件系统。
- **tmpfs** 通过 `mount_common_filesystems()` 挂载到 `/tmp`（无大小限制，权限 01777）和 `/dev/shm`（16MB 大小限制，权限 01777）。`/dev/shm` 的 tmpfs 作为 devfs 的子挂载注册到挂载树。


## 6. Test Mapping

| 特性 | 入口 | LTP 用例 | OSCOMP 组 | 状态 |
|------|------|----------|-----------|------|
| tmpfs 文件创建 | `create` | `open01` | basic | pass |
| tmpfs 读写 | `read_at`/`write_at` | `read01`, `write01` | basic | pass |
| tmpfs 目录操作 | `mkdir`/`rmdir` | `mkdir01`, `rmdir01` | basic | pass |
| tmpfs 链接操作 | `link`/`unlink`/`rename` | `link01`, `rename01` | basic | pass |
| tmpfs 元数据 | `metadata`/`set_metadata` | `chmod01`, `stat01` | basic | pass |
| tmpfs xattr | `getxattr`/`setxattr` | `setxattr01` | basic | pass |
| tmpfs 大小限制 | `check_space` | `tmpfs01` | basic | pass |
| ramfs 根文件系统 | `VFS_ROOT` | `mount01` | basic | pass |
| ramfs initramfs 解包 | `unpack_embedded` | — | basic | pass |

## 7. 已知问题

1. **tmpfs 大小限制的精度**
   - `current_size` 在进程崩溃时可能出现偏差（部分已计入的写入最终未提交）。当前实现未做崩溃恢复补偿，偏差通常较小（数个 page 级别）。
   - 影响：配额接近上限时，轻微偏差可能导致 `ENOSPC` 误报或漏报。

2. **ramfs 无 statfs 动态信息**
   - `RamFS::super_block` 返回固定值，`f_blocks`/`f_bfree` 不反映真实内存使用。`df` 等工具在 ramfs 上的输出不够准确。
   - 影响：低优先级，ramfs 主要用作根文件系统 fallback，用户很少对其执行容量查询。

3. **ramfs PageCache 桥接的锁竞争**
   - `RamFsPageCacheBackend::read_page`/`write_page` 在持有 inode 锁的状态下操作物理页。高并发读写同一文件时，锁争用可能成为瓶颈。
   - 影响：仅在 ramfs 上运行高 I/O 负载时可见。

4. **目录结构无并发保护增强**
   - `children: BTreeMap` 受 `Mutex<TmpFSInode>`/`Mutex<RamFSInode>` 保护，但遍历子目录树（如 `rmdir -rf` 递归删除）期间锁持有时间较长。
   - 影响：大目录操作可能短暂阻塞同文件系统上的其他操作。

5. **Link count 一致性**
   - unlink 目录时 `nlinks` 递减逻辑在内核 panic 场景下可能不一致导致泄漏。当前在正常退出路径上验证正确。
