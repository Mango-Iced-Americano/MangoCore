---
title: "VFS 分层架构"
module: "fs"
category: fs
status: draft
owner: "MangoCore Team"
last_updated: "2026-06-29"
code_paths:
  - "os/src/fs/vfs/"
entry_points:
  - "File"
  - "IndexNode"
  - "FileSystem"
  - "MountFS"
  - "FdTable"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "open*"
    - "read*"
    - "write*"
    - "stat*"
    - "mount*"
    - "unlink*"
    - "rename*"
  oscomp:
    - "basic"
    - "busybox"
    - "lua"
    - "libctest"
related_docs:
  - "docs/03_fs/page-cache.md"
  - "docs/03_fs/init-and-rootfs.md"
  - "docs/03_fs/vfs-core.md"
---

## 概述

VFS（Virtual File System）是 MangoCore 的文件系统抽象层，位于系统调用与具体文件系统实现之间。它定义了一套统一的接口，使得 ext4、FAT32、tmpfs、ramfs、procfs、devfs 等不同文件系统可以透过同一套 API 被用户态程序访问。设计参考 DragonOS 的 VFS/MountFS 架构，行为语义以 Linux 6.6 为基准。

VFS 层的核心目标是隔离具体文件系统的差异，提供路径解析、权限检查、挂载管理和文件描述符管理的统一框架。

## 分层架构

```text
syscall 层 (sys_openat, sys_read, sys_write, ...)
    |
    v
File (文件描述符层: offset, flags, mode, read, write, lseek)
    |
    v
IndexNode trait (inode 操作: read_at, write_at, find, create, unlink, ...)
    |
    v
MountFS / MountFSInode (挂载层: 跨 FS 路径解析, 挂载点管理, dentry cache)
    |
    v
FileSystem trait (具体 FS: root_inode, info, name, super_block)
    |
    v
PageCache (页缓存: 状态机, 脏页追踪, 回写, 预读)
    |
    v
BlockDevice trait (块设备: read_block, write_block)
```

每一层只与相邻层交互，上层通过 trait 或结构体方法调用下层，下层不依赖上层。这种单向依赖关系保证了模块化的可替换性。

## 核心抽象

### FileSystem trait

`FileSystem` 是具体文件系统的最高抽象。每个文件系统实现（ext4、tmpfs、devfs 等）都必须实现此 trait。

```rust
pub trait FileSystem: Any + Send + Sync + Debug {
    fn root_inode(&self) -> Arc<dyn IndexNode>;
    fn info(&self) -> FsInfo;
    fn name(&self) -> &str;
    fn super_block(&self) -> SuperBlock;
    fn statfs(&self, inode: &Arc<dyn IndexNode>) -> Result<SuperBlock, SyscallErr>;
    fn support_readahead(&self) -> bool;
    fn permission_policy(&self) -> FsPermissionPolicy;
}
```

关键方法：
- `root_inode()`: 返回文件系统的根 inode，是路径解析的起点。
- `info()`: 返回 `FsInfo`，包含块设备 ID、文件名最大长度、支持的特性列表。
- `super_block()`: 返回 `SuperBlock`，提供 `statfs` 所需的块大小、总块数、空闲块数等信息。
- `support_readahead()`: 指示该文件系统是否支持 PageCache 预读。

### IndexNode trait

`IndexNode` 是 inode 级别操作的抽象。它定义了所有文件系统在目录项和文件数据层面必须提供的接口。默认方法返回 `ENOSYS`，具体实现按需覆盖。

```rust
pub trait IndexNode: Any + Send + Sync + Debug {
    // 基本 I/O
    fn read_at(&self, offset: usize, len: usize, buf: &mut [u8],
               data: MutexGuard<FilePrivateData>) -> Result<usize, SyscallErr>;
    fn write_at(&self, offset: usize, len: usize, buf: &[u8],
                data: MutexGuard<FilePrivateData>) -> Result<usize, SyscallErr>;

    // 文件生命周期
    fn open(&self, data: MutexGuard<FilePrivateData>, flags: &FileFlags) -> Result<(), SyscallErr>;
    fn close(&self, data: MutexGuard<FilePrivateData>) -> Result<(), SyscallErr>;

    // 目录操作
    fn find(&self, name: &str) -> Result<Arc<dyn IndexNode>, SyscallErr>;
    fn create(&self, name: &str, file_type: FileType, mode: InodeMode)
              -> Result<Arc<dyn IndexNode>, SyscallErr>;
    fn link(&self, name: &str, other: &Arc<dyn IndexNode>) -> Result<(), SyscallErr>;
    fn unlink(&self, name: &str) -> Result<(), SyscallErr>;
    fn mkdir(&self, name: &str, mode: InodeMode) -> Result<Arc<dyn IndexNode>, SyscallErr>;
    fn rmdir(&self, name: &str) -> Result<(), SyscallErr>;
    fn rename(&self, old_name: &str, new_parent: &Arc<dyn IndexNode>,
              new_name: &str, flags: u32) -> Result<(), SyscallErr>;

    // 元数据
    fn metadata(&self) -> Result<Metadata, SyscallErr>;
    fn set_metadata(&self, metadata: &Metadata) -> Result<(), SyscallErr>;

    // 大小管理
    fn resize(&self, len: usize) -> Result<(), SyscallErr>;
    fn truncate(&self, len: usize) -> Result<(), SyscallErr>;

    // 文件系统引用
    fn fs(&self) -> Arc<dyn FileSystem>;
    fn page_cache(&self) -> Option<Arc<PageCache>>;
}
```

关键设计原则：
- `read_at` / `write_at` 接收 `offset` 参数，自身不维护偏移量。偏移量管理在 `File` 层。
- `find` 只在当前目录下查找，不递归。跨挂载点的遍历由 `MountFSInode` 处理。
- `metadata`、`fs`、`as_any_ref` 必须由实现者提供，其余方法都有默认的 `ENOSYS` 实现。
- `open` / `close` 用于生命周期管理，块设备文件系统可在此触发设备初始化或释放。
- `page_cache` 返回该 inode 关联的 PageCache，供 `File` 层进行零拷贝 UserBuffer I/O。

### File 结构体

`File` 是文件描述符层的核心结构体，封装一个 `IndexNode` 并管理每个打开文件描述的可变状态。

```rust
pub struct File {
    pub inode: Arc<dyn IndexNode>,
    offset: AtomicUsize,      // 文件偏移量
    flags: AtomicU32,         // 打开标志 (O_RDONLY, O_NONBLOCK, O_APPEND, ...)
    mode: FileMode,           // 访问模式 (FMODE_READ, FMODE_WRITE, FMODE_STREAM, ...)
    file_type: FileType,      // 文件类型
    private_data: Mutex<FilePrivateData>,  // FS 特定私有数据 (readahead, memfd seals, ...)
    open_file_id: usize,      // 全局唯一打开文件 ID
    posix_lock_key: (usize, usize),  // POSIX 锁键 (dev_id, inode_id)
}
```

`File` 与 `IndexNode` 的职责分离：
- `File` 管理 per-fd 可变状态：offset、flags、mode、private data。
- `IndexNode` 管理 per-inode 共享状态：数据块、元数据、等待队列。
- `File::read()` 调用 `IndexNode::read_at()` 后更新 offset 字段。
- `Arc<File>` 被 dup 后的 fd 共享，POSIX 要求 status flags（如 O_NONBLOCK、O_APPEND）在 dup 后保持一致。

`File::new()` 的创建过程：
1. 从 flags 推导 mode（FMODE_READ / FMODE_WRITE / FMODE_PATH 等）。
2. 调用 `inode.metadata()` 获取文件类型和设备类型。
3. 识别特殊设备（/dev/null、/dev/zero）设置 FMODE_DEV_NULL / FMODE_DEV_ZERO。
4. 对管道和套接字设置 FMODE_STREAM。
5. 为可读的普通文件初始化 readahead 状态。
6. 调用 `inode.open()` 让底层 FS 执行打开时的初始化。

## MountFS 挂载层

`MountFS` 和 `MountFSInode` 构成 VFS 的挂载抽象层，实现跨文件系统边界的路径解析。

### MountFS

`MountFS` 包装一个具体 `FileSystem`，添加挂载点管理和 dentry 缓存：

```rust
pub struct MountFS {
    inner_filesystem: Arc<dyn FileSystem>,
    root_inner_inode: Option<Arc<dyn IndexNode>>,
    mountpoints: Mutex<BTreeMap<InodeId, Arc<MountFS>>>,  // 子挂载点表
    self_mountpoint: Mutex<Option<Arc<MountFSInode>>>,     // 自身被挂载到的父 inode
    mount_flags: Mutex<MountFlags>,                        // RDONLY, NOSUID, NODEV, ...
    propagation: MountPropagation,                         // 挂载传播状态
    dentry_cache: Mutex<DentryCache>,                      // 目录项缓存
    dentry_gen: AtomicU64,                                 // 目录版本号
    no_dentry_cache: AtomicBool,                           // 动态 FS 跳过 cache
}
```

每个挂载的文件系统都对应一个 `MountFS` 实例。`mountpoints` 表将子挂载点的 inode ID 映射到子 `MountFS`，实现挂载树。

### MountFSInode

`MountFSInode` 包装 `Arc<dyn IndexNode>`，实现 `IndexNode` trait。大部分操作直接委托给 `inner_inode`。关键增强在 `find()` 方法：

1. 检查当前 inode 是否为挂载点根，若是则跨越到挂在它下面的子文件系统的根。
2. 在 `find("..")` 时跨越挂载边界返回父文件系统的对应目录。
3. 查询 dentry cache 避免重复的磁盘 I/O。
4. 对目录修改操作（create、unlink、rmdir、rename）递增 `dentry_gen` 使缓存失效。

路径解析示例（"/mnt/ext4/file"）：
1. 根 `MountFSInode.find("mnt")` → `inner_inode.find("mnt")` 返回 mnt 的 inode。
2. 检查 `mountpoints` 表：mnt 是挂载点，返回 ext4 子 `MountFS` 的根 `MountFSInode`。
3. 子根 `MountFSInode.find("file")` → 委托给 ext4 的 `find()`，返回目标 inode。

### 挂载传播

`MountFS` 支持 Linux 语义的挂载传播（shared / private / slave / unbindable）。`propagation` 模块管理 peer group 和 slave group，在 `mount_subtree` 和 `umount` 时自动将挂载事件复制到所有 peer 挂载的相同位置。

## FD 表集成

每个进程有一个 `FdTable`，管理该进程的所有文件描述符：

```rust
pub struct FdTable {
    fds: Vec<Option<Arc<File>>>,
    cloexec: Vec<bool>,
    next_fd: usize,
    soft_limit: usize,
    hard_limit: usize,
    lock_owner_id: usize,
}
```

关键操作：
- `alloc_fd()`: 分配最低编号的可用 fd。从 0 开始线性扫描空闲位置，没有空闲时自动扩容（翻倍策略），上限为 `SYSTEM_FD_LIMIT`。
- `alloc_fd_from(min_fd)`: 分配不小于 `min_fd` 的空闲 fd（用于 `F_DUPFD`）。
- `alloc_fd_at(fd)`: 在指定位置分配 fd（用于 `dup2`）。
- `drop_fd(fd)`: 释放 fd，触发 POSIX 锁清理。
- `close_cloexec()`: exec 时关闭所有设置了 `FD_CLOEXEC` 的 fd。

Syscall 层通过 `FdTable` 提供的 `get_file(fd)` 获取 `Arc<File>`，然后调用 `File` 的方法进行读写操作。

## PageCache 关系

PageCache 位于 VFS 层与块设备之间，为普通文件系统（ext4、FAT32、tmpfs）提供页粒度的缓存。ramfs 主要将数据存储在物理页中，但为了支持 mmap/filemap 集成，对共享缺页路径也暴露了懒加载 PageCache。

`IndexNode` trait 提供 `page_cache()` 和 `ensure_page_cache()` 方法，供 `File` 层在读写时获取该 inode 的 PageCache 实例：

- `page_cache()`: 只读查询，无则返回 None。
- `ensure_page_cache()`: 按需创建（如果 FS 支持），默认委托给 `page_cache()`。

对于支持 PageCache 的文件系统，`read_at` 和 `write_at` 的实现内部会穿透 PageCache。`read_at_user` / `write_at_user` 提供零拷贝路径，直接从 UserBuffer 到 PageCache 页，省去内核缓冲区的中转开销。

PageCache 的内部状态机（Loading → UpToDate ↔ Dirty → Writeback）、LRU 回收策略和预读逻辑不属于本文档范围，详见单独文档。

## 系统调用到磁盘的数据流

以 `sys_read(fd, buf, count)` 为例展示完整调用链：

```text
sys_read(fd, buf, count)
  -> current_process().fd_table.get_file(fd)           // 获取 Arc<File>
    -> File::read(&mut buf)                             // 检查权限，获取 offset
      -> IndexNode::read_at(offset, len, buf, data)     // 委托给具体实现
        -> [PageCache::read_at()]                        // 穿透 PageCache（如果存在）
          -> [ext4/tmpfs/... 具体 FS 实现]               // 解析 extent，读取数据
            -> [BlockDevice::read_block()]               // 块设备 I/O
```

所有 I/O 操作最终落到 `BlockDevice` trait 上。`File` 层不直接感知具体文件系统或块设备，所有差异由 `IndexNode` 的实现抹平。
