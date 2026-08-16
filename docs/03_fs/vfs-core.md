---
title: "VFS 核心类型"
module: "fs/vfs"
category: fs
status: draft
owner: "MangoCore Team"
last_updated: "2026-08-11"
code_paths:
  - "os/src/fs/vfs/file.rs"
  - "os/src/fs/vfs/index_node.rs"
  - "os/src/fs/vfs/file_system.rs"
  - "os/src/fs/vfs/mount.rs"
entry_points:
  - "File"
  - "IndexNode trait"
  - "FileSystem trait"
  - "FdTable"
  - "SuperBlock"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "open*"
    - "read*"
    - "write*"
    - "close*"
    - "dup*"
    - "fcntl*"
  oscomp:
    - "basic"
    - "busybox"
    - "lua"
    - "libctest"
related_docs:
  - "docs/03_fs/architecture.md"
  - "docs/03_fs/page-cache.md"
  - "docs/03_fs/init-and-rootfs.md"
---

## 概述

VFS 核心类型层位于系统调用分发与具体文件系统实现之间，由四个主要抽象组成：`File` 管理每个打开文件描述符的可变状态（偏移量、标志、访问模式），`FdTable` 管理进程级文件描述符表，`IndexNode` trait 定义 inode 级别操作的接口契约，`FileSystem` trait 定义文件系统生命周期的接口契约。文档涵盖这些类型的设计细节和关键行为，不涉及 MountFS 挂载层、PageCache 以及 ext4 / tmpfs 等具体文件系统实现。

## 核心类型

### File

`File` 是文件描述符层的核心结构体，每个 `open` / `socket` / `pipe` 调用创建一个实例。它封装一个 `Arc<dyn IndexNode>` 并管理每个打开描述符独立的可变状态。

```rust
pub struct File {
    pub inode: Arc<dyn IndexNode>,
    offset: AtomicUsize,
    flags: AtomicU32,
    mode: FileMode,
    file_type: FileType,
    private_data: Mutex<FilePrivateData>,
    open_file_id: usize,
    posix_lock_key: (usize, usize),
    created_by_open: bool,
    owner: Mutex<FileOwner>,
    file_rw_hint: Mutex<u64>,
    lease: Mutex<Option<i16>>,
}
```

`offset` 和 `flags` 使用原子类型，因为 `Arc<File>` 在 dup 后的 fd 之间共享。POSIX 要求 dup 后的 fd 共享同一个打开文件描述，包括偏移量和状态标志。原子操作保证了跨线程 / 跨进程并发的正确性而无需持锁。

`mode` 在构造时从 flags 推导一次，之后不可变更。它编码了文件访问能力（FMODE_READ / FMODE_WRITE / FMODE_PATH）、流式语义（FMODE_STREAM）、以及特殊设备优化（FMODE_DEV_NULL / FMODE_DEV_ZERO）。`flags` 由 `AtomicU32` 存储，访问模式位（O_RDONLY / O_WRONLY / O_RDWR）通过 `fcntl(F_SETFL)` 禁止修改，仅允许变更状态标志（O_NONBLOCK / O_APPEND / O_ASYNC / O_DIRECT / O_DSYNC / O_NOATIME）。

**构造路径**：`File::new()` 从 inode 和 flags 创建文件，内部调用 `inode.metadata()` 获取文件类型，识别特殊设备（/dev/null、/dev/zero），识别管道 / 套接字设置 FMODE_STREAM，然后调用 `inode.open()` 通知底层 FS。`File::new_with_metadata()` 提供给已持有 Metadata 的调用方复用避免二次查询。`File::new_without_open()` 用于 socket / pipe 等非 `open` 系统调用创建的 fd，跳过 `inode.open()` 调用。

**读写语义**：
- `read(buf)`：检查 readable 权限，获取当前 offset（流式文件取 0），调用 `inode.read_at()`，实际读取后退还更新 offset。
- `write(buf)`：检查 writable 权限。普通模式取当前 offset 写入；O_APPEND 模式下从文件末尾写入（每次重新调用 `inode.metadata()` 获取最新大小）。流式文件始终使用 offset 0。
- 成功写入后调用 `IndexNode::touch_modified()`。默认实现维持原有 metadata 更新；`another_ext4` 覆盖该钩子，将 mtime/ctime 写入 inode lifetime 缓存，避免每次 `write()` 的同步 inode 块读改写。缓存时间戳立即对 `metadata()` 可见，并在 `fsync`/`syncfs`/`sync` 的持久化边界合并提交。
- `pread(offset, buf)` 和 `pwrite(offset, buf)`：不更新 offset，O_PATH 文件返回 EBADF，流式文件返回 ESPIPE。
- `read_user` 和 `write_user`：直连 UserBuffer 版本，优先调用 `inode.read_at_user()` / `inode.write_at_user()` 实现零拷贝，fallback 到内核缓冲区的 kbuf 路径。

**lseek 行为**：
- SeekSet：直接设置 offset 指定值。
- SeekCurrent：当前 offset 加上偏移量。
- SeekEnd：文件末尾加上偏移量，需要调用 `inode.metadata()` 获取文件大小。
- 负偏移返回 EINVAL，流式文件返回 ESPIPE。

**目录流语义**：`getdents64()` 在 offset 为 0 时通过一次 `list_dirents()` 建立
`(name, inode_id, file_type)` 完整快照，后续批次直接从该快照编码 `linux_dirent64`。
这既保证并发目录修改不会破坏游标，也避免为每个名称再次执行 `find()` 与
`metadata()`；rewind 到 offset 0 时才重建快照。

**flags 管理**：`set_flags()` 只允许修改状态标志掩码（O_APPEND / O_NONBLOCK / O_DSYNC / O_DIRECT / O_NOATIME / O_ASYNC），访问模式位保持不变。`set_nonblock()` 是对 O_NONBLOCK 位的快捷设置。

### FdTable

`FdTable` 是每个进程持有的文件描述符表，管理 fd 的分配与释放。

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

**fd 分配算法**：`alloc_fd()` 从 0 开始线性扫描数组，返回第一个空闲位置。这一设计遵循 Linux 语义：每次 `open` / `socket` / `accept` 应返回当前进程最低编号的可用 fd。没有空闲槽位时按翻倍策略扩容，上限为 `SYSTEM_FD_LIMIT`。

**特殊分配**：
- `alloc_fd_from(min_fd)`：用于 `fcntl(F_DUPFD)`，分配不小于 min_fd 的空闲 fd。
- `alloc_fd_at(fd)`：用于 `dup2`，在指定位置分配，自动扩容到所需容量。

**容量管理**：初始容量 32，翻倍扩容上限 SYSTEM_FD_LIMIT。缩容时只截断到最高已用索引 + 1，不无谓释放内存。`soft_limit` 是 rlimit 软限制，`hard_limit` 是硬限制，可通过 `set_soft_limit()` 调节。

**fork 克隆**：`try_clone()` 创建 FdTable 的副本，每个 fd 共享同一个 `Arc<File>`（符合 POSIX fork 语义）。新 FdTable 分配独立的 `lock_owner_id`，确保子进程的 POSIX 锁不干扰父进程。所有 Vec 扩容调用 `try_reserve` 以 OOM 安全方式返回 ENOMEM。

**close_on_exec**：`cloexec` 数组记录每个 fd 的 FD_CLOEXEC 标志。`close_cloexec()` 在 exec 时关闭所有标记为 cloexec 的 fd。
释放 fd 时 `drop_fd()` 会调用 `release_posix_for_owner()` 清理该打开文件描述关联的 POSIX 锁。`close_range()` 批量关闭一段连续 fd。

### IndexNode trait

`IndexNode` 是 VFS 层最核心的 trait，所有 inode 实现必须实现它。设计上采用"默认返回 ENOSYS"模式，具体文件系统按需覆盖。

```rust
pub trait IndexNode: Any + Send + Sync + Debug {
    fn read_at(&self, offset: usize, len: usize, buf: &mut [u8],
               data: MutexGuard<FilePrivateData>) -> Result<usize, SyscallErr>;
    fn write_at(&self, offset: usize, len: usize, buf: &[u8],
                data: MutexGuard<FilePrivateData>) -> Result<usize, SyscallErr>;
    fn read_at_user(&self, offset: usize, len: usize,
                    dst: &mut UserBuffer) -> Result<usize, SyscallErr>;
    fn write_at_user(&self, offset: usize, len: usize,
                     src: &UserBuffer) -> Result<usize, SyscallErr>;
    fn read_direct(&self, offset: usize, len: usize, buf: &mut [u8],
                   data: MutexGuard<FilePrivateData>) -> Result<usize, SyscallErr>;
    fn write_direct(&self, offset: usize, len: usize, buf: &[u8],
                    data: MutexGuard<FilePrivateData>) -> Result<usize, SyscallErr>;
    fn read_sync(&self, offset: usize, buf: &mut [u8]) -> Result<usize, SyscallErr>;
    fn write_sync(&self, offset: usize, buf: &[u8]) -> Result<usize, SyscallErr>;
    fn is_discard_write(&self) -> bool;
    fn discard_write_at(&self, offset: usize, len: usize,
                        data: MutexGuard<FilePrivateData>) -> Result<usize, SyscallErr>;
    fn supports_user_buffer_io(&self) -> bool;

    fn open(&self, data: MutexGuard<FilePrivateData>,
            flags: &FileFlags) -> Result<(), SyscallErr>;
    fn close(&self, data: MutexGuard<FilePrivateData>) -> Result<(), SyscallErr>;

    fn find(&self, name: &str) -> Result<Arc<dyn IndexNode>, SyscallErr>;
    fn list(&self) -> Result<Vec<String>, SyscallErr>;
    fn list_dirents(&self) -> Result<Vec<(String, InodeId, FileType)>, SyscallErr>;

    fn create(&self, name: &str, file_type: FileType,
              mode: InodeMode) -> Result<Arc<dyn IndexNode>, SyscallErr>;
    fn create_with_data(&self, name: &str, file_type: FileType,
                        mode: InodeMode, data: usize)
        -> Result<Arc<dyn IndexNode>, SyscallErr>;
    fn create_with_attrs(&self, name: &str, file_type: FileType,
                         attrs: CreateAttrs) -> Result<Arc<dyn IndexNode>, SyscallErr>;
    fn create_with_data_and_attrs(&self, name: &str, file_type: FileType,
                                  attrs: CreateAttrs, data: usize)
        -> Result<Arc<dyn IndexNode>, SyscallErr>;
    fn symlink(&self, name: &str, target: &str)
        -> Result<Arc<dyn IndexNode>, SyscallErr>;
    fn symlink_with_attrs(&self, name: &str, target: &str,
                          attrs: CreateAttrs) -> Result<Arc<dyn IndexNode>, SyscallErr>;
    fn link(&self, name: &str, other: &Arc<dyn IndexNode>) -> Result<(), SyscallErr>;
    fn rename(&self, old_name: &str, new_parent: &Arc<dyn IndexNode>,
              new_name: &str, flags: u32) -> Result<(), SyscallErr>;
    fn unlink(&self, name: &str) -> Result<(), SyscallErr>;
    fn rmdir(&self, name: &str) -> Result<(), SyscallErr>;
    fn mkdir(&self, name: &str, mode: InodeMode) -> Result<Arc<dyn IndexNode>, SyscallErr>;

    fn metadata(&self) -> Result<Metadata, SyscallErr>;
    fn set_metadata(&self, metadata: &Metadata) -> Result<(), SyscallErr>;
    fn get_entry_name(&self, ino: InodeId) -> Result<String, SyscallErr>;
    fn get_entry_name_and_metadata(&self, ino: InodeId)
        -> Result<(String, Metadata), SyscallErr>;

    fn resize(&self, len: usize) -> Result<(), SyscallErr>;
    fn truncate(&self, len: usize) -> Result<(), SyscallErr>;

    fn fs(&self) -> Arc<dyn FileSystem>;
    fn page_cache(&self) -> Option<Arc<PageCache>>;
    fn ensure_page_cache(&self) -> Option<Arc<PageCache>>;

    fn ioctl(&self, cmd: u32, data: usize,
             private_data: MutexGuard<FilePrivateData>) -> Result<usize, SyscallErr>;
    fn poll(&self, private_data: &FilePrivateData) -> Result<usize, SyscallErr>;
    fn is_stream(&self) -> bool;
    fn read_wait_queue(&self) -> Option<&Mutex<WaitQueue>>;
    fn read_event_queue(&self) -> Option<&EventWaitQueue>;
    fn write_wait_queue(&self) -> Option<&Mutex<WaitQueue>>;
    fn write_event_queue(&self) -> Option<&EventWaitQueue>;
    fn fasync_items(&self) -> Option<&FAsyncItems>;
    fn absolute_path(&self) -> Result<String, SyscallErr>;
    fn mount(&self, fs: Arc<dyn FileSystem>, mount_flags: MountFlags)
        -> Result<Arc<MountFS>, SyscallErr>;
    fn umount(&self) -> Result<Arc<MountFS>, SyscallErr>;
    fn as_any_ref(&self) -> &dyn Any;
    fn sync(&self) -> Result<(), SyscallErr>;
    fn datasync(&self) -> Result<(), SyscallErr>;
    fn fadvise(&self, offset: i64, len: i64, advise: i32) -> Result<usize, SyscallErr>;
    fn mknod(&self, filename: &str, mode: InodeMode, dev_t: u64)
        -> Result<Arc<dyn IndexNode>, SyscallErr>;
    fn getxattr(&self, name: &str, buf: &mut [u8]) -> Result<usize, SyscallErr>;
    fn setxattr(&self, name: &str, value: &[u8], flags: u32) -> Result<usize, SyscallErr>;
    fn listxattr(&self, buf: &mut [u8]) -> Result<usize, SyscallErr>;
    fn removexattr(&self, name: &str) -> Result<usize, SyscallErr>;
}
```

方法分类：
- **基本 I/O（8 个）**：read_at / write_at 是主路径，带文件私有数据参数。read_at_user / write_at_user 提供 UserBuffer 直连零拷贝路径，默认返回 ENOSYS 由 File 层 fallback。read_direct / write_direct 绕开 PageCache 用于 O_DIRECT。read_sync / write_sync 供 PageCache 内部使用。
- **生命周期（2 个）**：open / close 在 File 创建和销毁时调用。
- **目录操作**：find / list / list_dirents / create / create_with_data / create_with_attrs / create_with_data_and_attrs / symlink / symlink_with_attrs / link / rename / unlink / rmdir / mkdir。带 attrs 的创建接口允许后端在发布目录项的同一事务中写入最终 uid/gid/mode；默认实现仍可回退为创建后 `set_metadata`，并传播后者的错误。mkdir 默认实现先检查 EEXIST 再委托 create。
- **元数据（4 个）**：metadata、fs、as_any_ref 必须由实现者提供，其余方法有默认 ENOSYS 实现。
- **大小管理（2 个）**：resize / truncate，truncate 默认委托给 resize。
- **FS 引用（3 个）**：fs 返回所属 FileSystem。page_cache / ensure_page_cache 提供 PageCache 访问。
- **其他（13 个）**：ioctl / poll / is_stream / 等待队列 / fasync / absolute_path / mount / umount / as_any_ref / sync / datasync / fadvise / mknod / xattr 系列。

### FileSystem trait 与 SuperBlock

```rust
pub trait FileSystem: Any + Send + Sync + Debug {
    fn identity_key(&self) -> usize;
    fn root_inode(&self) -> Arc<dyn IndexNode>;
    fn info(&self) -> FsInfo;
    fn name(&self) -> &str;
    fn super_block(&self) -> SuperBlock;
    fn statfs(&self, inode: &Arc<dyn IndexNode>) -> Result<SuperBlock, SyscallErr>;
    fn support_readahead(&self) -> bool;
    fn permission_policy(&self) -> FsPermissionPolicy;
    fn sync(&self) -> Result<(), SyscallErr>;
    fn on_umount(&self) -> Result<(), SyscallErr>;
    fn as_any_ref(&self) -> &dyn Any;
}
```

`root_inode` 是路径解析的起点。`info` 返回 FsInfo（块设备 ID / 最大文件名长度 / 特性列表）。`super_block` 和 `statfs` 提供 statfs 系统调用所需信息。

`sync()` 是文件系统实例级持久化入口。`syncfs(2)` 只负责从 fd 解析所属
`FileSystem` 并调用该接口，不在 syscall 层识别具体后端；`sync(2)` 则快照
`BackendLifecycle` registry 后逐实例调用，同样不枚举具体文件系统类型。持久化后端应
覆盖默认 no-op，使用自己的 PageCache registry、journal 和块设备 flush 顺序。FAT32
尚无实例级 PageCache registry，因此在 FAT 后端内部显式保留全局 flush 兼容路径。

`on_umount()` 是可失败的 teardown 事务，默认委托 `sync()`；需要停止 journal、C cache
或注销设备的后端覆盖完整流程。具体后端只有在数据/元数据写回、journal/cache
停止和 C/设备注册表脱钩全部成功后才返回 `Ok(())`。`BackendLifecycle` 使用
`Active -> Dying -> Dead` 状态机；最后一个 MountFS 引用消失后进入 Dying，调度器在不持
registry 锁时调用回调。失败时仍保持 Dying 并重新入队，不能把半卸载后端标成 Dead。
正常关机路径先完成全局 PageCache writeback，再调用所有 backend teardown；任一失败都
阻止“持久化成功”的最终状态，但不会阻止其他独立后端尝试提交。

`identity_key()` 是仅在本次启动期有效的文件系统实例身份，不是用户态 `st_dev`。
默认实现使用具体文件系统对象地址；`MountFS` 必须转发到底层文件系统，使同一 inode
经普通挂载或 bind mount 访问时仍使用同一身份。全局 inode 注册表必须以
`(identity_key, inode_id)` 为键，不能直接使用尚未为所有文件系统实现的 `Metadata.dev_id`，
否则 ramfs、tmpfs 和 ext4 都报告占位值 0 时，相同 inode 号会互相触发 `ETXTBSY` 等状态。
`ftruncate -9` 如何由 reopen 的 `ETXTBSY` 二次遮蔽而来，见
[`18b-cross-filesystem-executable-inode-identity.md`](../09_debug/la64_on_board/260710/18b-cross-filesystem-executable-inode-identity.md)。

`SuperBlock` 结构体直接对标 Linux `struct statfs`：
- f_type（文件系统魔数）、f_bsize（块大小）、f_blocks / f_bfree / f_bavail（块计数）、f_files / f_ffree（inode 计数）、f_namelen（最大文件名长度）、f_fsid（文件系统 ID）、f_frsize（片段大小）、flags（挂载标志）。

FsPermissionPolicy 区分标准 Unix DAC 权限检查和远程文件系统（如 FUSE）的权限委派模式。

## 辅助类型

### FileFlags

bitflags 位域结构，直接映射 Linux `open()` 的 oflag 参数：
- 访问模式（O_RDONLY / O_WRONLY / O_RDWR / O_ACCMODE）：掩码提取只取低 2 位。
- 打开时标志（O_CREAT / O_EXCL / O_NOCTTY / O_TRUNC）：open 时一次性使用，不持久化到 File 实例。特别地 O_TRUNC 在 open 路径由 VFS 处理截断，不进入 File 结构体。
- 文件状态标志（O_APPEND / O_NONBLOCK / O_DSYNC / O_SYNC / O_ASYNC / O_DIRECT / O_NOATIME）：持久化在 `File.flags` 中，可通过 fcntl(F_SETFL) 修改。
- 特殊标志（O_PATH / O_CLOEXEC / O_DIRECTORY / O_NOFOLLOW / O_LARGEFILE / O_TMPFILE）：O_CLOEXEC 在 FdTable 层面单独管理，不在 File 中持久化。

`STATUS_MASK` 常量定义了 fcntl(F_GETFL) 返回的有效状态位：O_APPEND、O_NONBLOCK、O_DSYNC、O_SYNC、O_ASYNC、O_DIRECT、O_LARGEFILE、O_NOATIME。

### FileMode

从 flags 推导的访问模式位域，构造后不可变更：
- FMODE_READ / FMODE_WRITE：读写能力。
- FMODE_LSEEK / FMODE_PREAD / FMODE_PWRITE：默认所有文件均支持 lseek 和 positional I/O。
- FMODE_PATH：O_PATH 文件，绝大多数 I/O 操作返回 EBADF。
- FMODE_STREAM：管道 / 套接字，offset 始终为 0，lseek 返回 ESPIPE。
- FMODE_DEV_NULL / FMODE_DEV_ZERO：特殊设备优化，File 层可直接跳过实际 I/O。

### Metadata

`Metadata` 结构体对标 Linux `struct kstat`，包含 inode 的完整元状态：
- 类型信息：dev_id / inode_id / file_type / mode / flags。
- 文件大小：size（i64）/ blk_size / blocks。
- 时间戳：atime / mtime / ctime。
- 所有权：uid / gid / nlinks。
- 设备文件专用：raw_dev（主次设备号）。

## 关键设计决策

**File 与 IndexNode 的分离**：这是 VFS 设计中最核心的决策。`File` 持有每个打开描述符的可变状态（offset、flags、private_data），而 `IndexNode` 持有 inode 级别的共享状态（数据块、元数据、等待队列）。一个 inode 可能对应多个 File（同一文件被打开多次或 dup），各自维护独立的 offset 和 flags。这种分离使 Inode 级元数据缓存得以共享，同时保证了 per-fd 状态的隔离。

**offset 的原子化**：`offset` 使用 `AtomicUsize` 而非 `Mutex<usize>`，因为 dup 后的 fd 共享同一个 `Arc<File>`，offset 的读写竞争频率高。原子操作在大部分架构上单条指令完成，比 Mutex 的开销低一个数量级。fp read / write 路径使用 SeqCst 排序保证跨线程可见性。

**flags 的原子化**：同样使用 `AtomicU32`，因为 `fcntl(F_SETFL)` 可能在任何线程发起。`fetch_update` 配合闭包避免了 CAS 循环被并发 SETFL 覆盖的问题。访问模式位的不可变性保证了 `writable()` / `readable()` 检查不需要持锁。

**O_NONBLOCK 的处理**：O_NONBLOCK 状态位在 File 的 flags 中设置后，File 层的 read / write 方法不直接感知非阻塞语义。非阻塞的判断由 syscall 层或更上层的 `wait_io` 包装函数执行：检查 `is_nonblock()` 为 true 时，I/O 操作仅尝试一次（调用 `try_xxx` 族方法），不会进入等待队列轮询。

**O_APPEND 的 offset 处理**：每次 append 写操作前，File 层重新调用 `inode.metadata()` 获取文件当前大小作为写入 offset。这一设计确保了即使多个写入者并发，写入始终到达文件末尾。代价是每次 O_APPEND 写多一次 metadata 查询，但对于裸机内核的典型负载来说可接受。

**流式文件语义**：管道和套接字标记为 FMODE_STREAM。File 层在 read / write 时始终使用 offset 0，跳过 offset 更新。lseek 返回 ESPIPE。pread / pwrite 返回 ESPIPE。这一实现来自 Linux 的 `FMODE_STREAM` 设计，避免了流式文件层做无意义的 offset 管理。

**FdTable 的 fd 分配**：`alloc_fd` 始终保持最低可用编号 fd 的语义，确保与 Linux 行为一致。扩容使用翻倍策略（最小 O(n) 均摊）且所有扩容调用 `try_reserve` 保证 OOM 安全。`next_fd` 优化位记录下次扫描起点，避免已分配大量 fd 时从头重复扫描。

**File 的 Drop**：File 析构时依次释放 OFD 锁、注销 write_busy 状态、调用 `inode.close()`。Arc 引用归零时自动触发这一链式清理，确保 inode 级的引用计数正确。

## 测试映射

| 特性 | API | LTP 用例 | OSCOMP 组 | 状态 |
|------|-----|----------|-----------|------|
| 打开/关闭文件 | sys_openat / sys_close | open01..open10, close01..close02 | basic | pass |
| 文件读写 | sys_read / sys_write | read01..read06, write01..write04 | basic | pass |
| 文件偏移 | sys_lseek | lseek01..lseek07 | basic | pass |
| fd 复制 dup / dup2 | sys_dup / sys_dup2 / sys_dup3 | dup01..dup02, dup201..dup202 | basic | pass |
| fcntl 控制 | sys_fcntl | fcntl01..fcntl10 | basic | pass |
| O_NONBLOCK 语义 | 组合测试 | nonblock01 | basic | pass |
| O_APPEND 语义 | 组合测试 | append01 | basic | pass |
| fd 表 fork 继承 | sys_fork / sys_clone | fork01..fork09 | basic | pass |
| close_on_exec | sys_execve | exec01..exec02 | basic | pass |
| pread / pwrite | sys_pread64 / sys_pwrite64 | pread01..pread02 | basic | pass |
| getdents64 | sys_getdents64 | getdents01..getdents02 | basic | pass |

### LTP 跳过清单

| 用例 | 跳过原因 | 跟踪 |
|------|----------|------|
| fcntl36 (F_OFD_SETLKW) | OFD lock wait 未实现 | — |
| dup03 (超过 rlimit) | rlimit 集成需要信号机制 | — |

## 已知问题

1. **O_APPEND 并发写入的性能**
   - 现象：每次追加写都调用 `inode.metadata()` 获取文件大小，高频写入路径有额外开销。
   - 根因：File 层不缓存 inode 大小，O_APPEND 路径必须获取最新大小。
   - 影响：microbenchmark 上 O_APPEND 路径比非 append 路径慢约 5-10%。
   - 修复方向：考虑在 inode 层提供原子化的 file_size 读取接口，减少 metadata 结构体全量拷贝。

2. **FdTable 扩容不可逆转**
   - 现象：大量临时 fd 分配后，FdTable 的 Vec 容量保持在高水位，即使 fd 已被释放。
   - 根因：缩容策略保守，只截断到最高已用索引 +1，对离散的 fd 分配无效果。
   - 影响：短生命周期大量 fd 分配的场景（如遍历大目录）内存不回收。
   - 修复方向：在 fd 离散释放时定期触发 shrink_to_fit。

3. **FMODE_PATH 检查的一致性**
   - 现象：O_PATH 文件的某些 ioctl 操作可能漏掉 EBADF 检查。
   - 根因：部分 ioctl 路径直接在 IndexNode 层实现，跳过了 File 层的 readable / writable 检查。
   - 影响：极低，O_PATH 主要用于 /proc/self/fd 等场景。
   - 修复方向：在 syscall 分发层统一做 O_PATH 过滤。
