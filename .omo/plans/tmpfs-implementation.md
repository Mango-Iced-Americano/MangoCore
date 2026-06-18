# TmpFS 实现方案

## 背景

当前内核使用 `RamFS` 充当 `/tmp` 和 `sys_mount("tmpfs", ...)` 的底层文件系统。
随着 LTP 测试对 tmpfs 语义（statfs、mknod、xattr、mount options）的要求越来越严格，
需要实现一个语义更完整的 tmpfs。

RamFS 死锁已修复（`drop(new_locked)` before rollback，commit pending）。

## 一、现有 RamFS 分析

### 当前架构（`os/src/fs/ramfs/mod.rs`，816 行）

```
RamFS (FileSystem impl):
  root_inode: Arc<LockedRamFSInode>
  self_ref: Weak<RamFS>
  max_pages: usize          ← quota（0 = 不限）
  page_count: Mutex<usize>

LockedRamFSInode(Mutex<RamFSInode>):
  parent, self_ref, children, pages,
  new_page_cache, file_size, metadata, fs
```

### 已实现的 IndexNode 方法
- `open`, `read_at`, `write_at`, `resize` (truncate)
- `find` (lookup), `create`, `link`, `unlink`, `rmdir`, `rename`
- `get_entry_name` (readdir), `poll`
- `metadata`, `set_metadata`
- `fs`, `page_cache`, `as_any_ref`

### 当前用作 tmpfs 的地方
1. `/tmp` 挂载：`os/src/fs/mod.rs:163-187` → 使用 `RamFS::new_with_quota(0)`
2. `sys_mount("tmpfs", ...)`：`os/src/syscall/fs.rs:3472` → 使用 `RamFS::new_with_quota(4096)`

## 二、TmpFS 需要补充的语义

| 功能 | RamFS 现状 | TmpFS 需要 |
|------|-----------|-----------|
| **Magic** | `0x8584_58f6` (ramfs) | `0x0102_1994` (TMPFS_MAGIC) |
| **statfs** | 静态 SuperBlock | 动态：`f_bfree`, `f_bavail` 基于剩余页数；`f_ffree` 跟踪 |
| **挂载选项** | 无 | `size=`, `nr_inodes=`, `mode=` 解析 |
| **时间戳** | `TimeSpec::new()` (0) | 使用 wall-clock 时间 |
| **文件名长度** | 64 | 255 (Linux tmpfs) |
| **块大小** | 512 | PAGE_SIZE (4096) |
| **mknod** | 未实现 | 支持设备节点 |
| **xattr** | 未实现 (ENOSYS) | trusted.* 等 |
| **st_blocks** | 未正确计算 | `blocks = (size + 511) / 512` |
| **inode 回收** | 无 (单调递增) | 至少提供 per-fs 计数器，让 statfs 有意义 |

## 二点五、DragonOS TmpFS 参考（来自 GitHub: DragonOS-Community/DragonOS）

关键发现：
1. **数据存储统一到 PageCache**：DragonOS tmpfs 没有 `BTreeMap<FrameTracker>`，数据全部走 `PageCache`。`TmpfsPageCacheBackend::read_page`/`write_page` 是空操作——数据在 PageCache 的 Page 中，后端不需要持久化。
2. **两阶段读写**：持 page_cache 锁收集页引用到 Vec → 释放锁 → 逐页 prefault 用户缓冲区 + 拷贝。防止 read/write 过程中用户页缺页导致的死锁。
3. **容量限制用 AtomicU64**：`current_size: AtomicU64` + CAS 循环，`size_limit: RwSem<Option<u64>>`。比 Mutex 更轻量。
4. **rename 按 inode_id 排序锁顺序**：防止 AB-BA 死锁。
5. **inode 创建时立即建 PageCache**：`PageCache::new()` + `set_unevictable(true)` + `set_shmem(true)`。
6. **mmap 缺页走 pagecache_fault_zero**：`FileSystem::fault()` 方法直接从 PageCache 取页建映射。
7. **Mount 选项解析**：支持 `size=`, `mode=` 以及 k/M/G 后缀。

## 三、实现方案

### Phase 1：创建 `os/src/fs/tmpfs/mod.rs`（核心模块，~900 行）

**Step 1.1：数据结构**

```rust
const TMPFS_MAGIC: u64 = 0x0102_1994;
const TMPFS_MAX_NAMELEN: usize = 255;
const TMPFS_BLOCK_SIZE: u64 = 512;
const TMPFS_FRSIZE: u64 = 4096;

/// TmpFS 挂载实例（DragonOS 风格）
pub struct TmpFS {
    root_inode: Arc<LockedTmpFSInode>,
    self_ref: Mutex<Weak<TmpFS>>,
    /// 容量限制（字节），None = 不限制
    size_limit: Mutex<Option<u64>>,  // 简化：DragonOS 用 RwSem，我们用 Mutex
    /// 当前已分配容量（字节），Atomic 以支持无锁读取
    current_size: AtomicU64,
}

/// 带锁的 TmpFS inode 包装器
pub struct LockedTmpFSInode(pub Mutex<TmpFSInode>);

/// TmpFS inode 内部数据
pub struct TmpFSInode {
    parent: Weak<LockedTmpFSInode>,
    self_ref: Weak<LockedTmpFSInode>,
    children: BTreeMap<String, Arc<LockedTmpFSInode>>,
    /// ★ 文件数据走 PageCache（不再有 BTreeMap<FrameTracker>）
    page_cache: Option<Arc<NewPageCache>>,
    file_size: usize,
    metadata: Metadata,
    fs: Weak<TmpFS>,
}
```

**Step 1.2：FileSystem trait 实现**

```rust
impl FileSystem for TmpFS {
    fn root_inode(&self) -> Arc<dyn IndexNode> { self.root_inode.clone() }
    fn info(&self) -> FsInfo { /* 与 RamFS 一致 */ }
    fn name(&self) -> &str { "tmpfs" }
    fn super_block(&self) -> SuperBlock { /* TMPFS_MAGIC */ }
    fn statfs(&self, _inode: &Arc<dyn IndexNode>) -> Result<SuperBlock, SyscallErr> {
        // ★ 动态计算（DragonOS 风格）
        let current = self.current_size.load(Ordering::Acquire) as u64;
        let total = self.size_limit.lock().unwrap_or(u64::MAX);
        // 转为块数（512 字节块，st_blocks 语义）
        let current_blocks = (current + TMPFS_BLOCK_SIZE - 1) / TMPFS_BLOCK_SIZE;
        let total_blocks = if total == u64::MAX { u64::MAX } else { total / TMPFS_BLOCK_SIZE };
        let free = total_blocks.saturating_sub(current_blocks);
        Ok(SuperBlock {
            f_type: TMPFS_MAGIC,
            f_bsize: TMPFS_FRSIZE,
            f_blocks: total_blocks,
            f_bfree: free,
            f_bavail: free,
            f_files: u64::MAX,
            f_ffree: u64::MAX,
            f_namelen: TMPFS_MAX_NAMELEN as u64,
            f_frsize: TMPFS_FRSIZE,
            ..SuperBlock::default()
        })
    }
    fn as_any_ref(&self) -> &dyn Any { self }
}
```

**Step 1.3：挂载选项解析**

```rust
impl TmpFS {
    pub fn new() -> Arc<Self> { Self::new_with_options(0) }

    /// max_pages = 0 表示不限制
    pub fn new_with_options(max_pages: usize) -> Arc<Self> {
        // 与 RamFS:new_inner 基本相同，差异：
        // 1. 记录 mount_time
        // 2. inode 计数器从 1 开始（root inode）
        // 3. 设置 TMPFS_MAGIC
        // ...
    }
}
```

**Step 1.4：IndexNode trait 实现**

从 RamFS 直接移植，修改点：
- 类型名 `RamFS` → `TmpFS`，`RamFSInode` → `TmpFSInode`
- `get_entry_name` 中 name 长度检查：64 → 255
- `rename` 使用**已修复的死锁避免模式**
- `truncate` 中 st_blocks 更新：`blocks = (size + 511) / 512`
- `metadata()` 返回时更新 atime/mtime/ctime 为 wall-clock
- 新增 `mknod` 支持（设备节点 inode，存储 rdev 到 metadata.raw_dev）
- `page_cache()` 后端经 `TmpFSPageCacheBackend` 桥接

### Phase 2：集成到 VFS 层

**文件：`os/src/fs/mod.rs`**
- 添加 `pub mod tmpfs;`
- 第 170 行：`RamFS::new_with_quota(0)` → `TmpFS::new_with_options(0)`
- 第 176 行：`tmpfs` 变量名保持不变（已经是 tmpfs 了）

**文件：`os/src/syscall/fs.rs`**
- 第 3472 行：`RamFS::new_with_quota(4096)` → `TmpFS::new_with_options(4096)`

### Phase 3：保留 RamFS 作为后备

RamFS 作为轻量级内存文件系统仍然有用（调试、`force_ramfs` 模式）。
保留 `os/src/fs/ramfs/mod.rs` 不变，只将其从 /tmp 用途中移除。

### Phase 4：验证

1. `make rv64-kernel-build-only && make la64-kernel-build-only` — 双架构编译
2. `cd os && make rv64-run` — QEMU 启动 + basic 测试
3. `make -C os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt CONF_FILE=../os_test.conf` + QEMU 测试
4. 验证 rename 不再死锁（同一目录 + 跨目录 EEXIST 场景）

## 四、设计决策

### Q1：RamFS 是否需要删除？
**决定**：保留。作为轻量级后备和 VFS 调试工具。只需从 /tmp 和 sys_mount("tmpfs") 路径中移除。

### Q2：数据存储方式？BTreeMap<FrameTracker> vs PageCache-only？
**决定**：采用 DragonOS 的 PageCache-only 方案。
- 不再维护 `BTreeMap<usize, Arc<FrameTracker>>`，所有文件数据通过 `PageCache` 的 `get_or_create_entry` → `populate_page_zero` 管理
- `TmpfsPageCacheBackend::read_page` 返回空（数据已在 Page 中），`write_page` 返回 `Ok(buf.len())`
- `read_at`/`write_at` 使用两阶段模式（先收集页引用，再释放 page_cache 锁做用户拷贝），防止持锁过程中用户页缺页导致死锁
- `page_cache()` 方法不再懒初始化——inode 创建时（File/SymLink）立即建 PageCache 并设 `unevictable`
- 优势：mmap 缺页走统一的 `pagecache_fault_zero` 路径，不需要两套数据流
- 风险：我们需要确保自己的 `PageCache`（`os/src/fs/page_cache.rs`）有 `populate_page_zero`、`commit_overwrite`、`unevictable` 等机制

### Q7：是否复用 RamFS 目录管理代码？
**决定**：复用 BTreeMap 目录结构（`children: BTreeMap<String, Arc<LockedTmpFSInode>>`）。
`find`, `create`, `link`, `unlink`, `rmdir` 从 RamFS 移植（仅类型名替换）。
`rename` 附加：按 inode_id 排序锁顺序（DragonOS 方案）。

### Q8：读写路径锁策略？
**决定**：采用两阶段模式（DragonOS 方案）：
1. Phase 1：持 inode/page_cache 锁，`commit_overwrite` 收集页 Arc 到 Vec
2. Phase 2：释放所有锁，逐页 prefault 用户缓冲区 + `page.read()`/`page.write()` 拷贝数据
这避免了持锁过程中用户页缺页 → 触发的二次内存分配 → 潜在的锁冲突。

### Q3：是否需要 mknod 支持？
**决定**：Phase 1 实现基本设备节点（metadata.raw_dev 存储 rdev），但暂不连接真实设备驱动。设备节点创建返回 inode，open 时返回 ENODEV。

### Q4：inode 回收策略？
**决定**：先保持单调用计数器（per-fs `inode_count`），不做回收。Linux tmpfs 也默认无 inode 限制（`nr_inodes=0` 表示不限）。未来可添加 `nr_inodes` 挂载选项。

### Q5：statfs 不限制时的返回值？
**决定**：`f_blocks = u64::MAX`，`f_bfree = u64::MAX`，`f_files = u64::MAX`，`f_ffree = u64::MAX`。与 Linux tmpfs 行为一致。

### Q6：st_blocks 计算？
**决定**：`(file_size + 511) / 512`，存储在 `metadata.blocks` 中。
所有修改 `file_size` 的地方同步更新 `blocks`。

## 五、风险与缓解

| 风险 | 等级 | 缓解 |
|------|------|------|
| 大规模复制引入维护负担 | 中 | 保持 RamFS+TmpFS 同步注释；长期可泛型化 |
| PageCache-only 方案需确认现有 PageCache 能力 | 中 | 先验证 `commit_overwrite`、`populate_page_zero`、`unevictable` 已就绪 |
| 动态 statfs 触发新 LTP 失败 | 低 | 先仅实现基础 statfs；后续按 LTP 失败精确调整 |
| mknod 语义不完整 | 低 | 按需补充；当前无 LTP mknod 用例阻塞 |
| rename 死锁在新代码中重现 | 低 | TmpFS rename 使用 inode_id 排序锁 + 已修复回滚模式 |
| 两阶段读写引入时序窗口 | 低 | DragonOS 已验证：commit_overwrite 确保页在收集阶段已分配 |

## 六、文件清单

| 操作 | 文件 |
|------|------|
| **新建** | `os/src/fs/tmpfs/mod.rs` (~900 行) |
| **修改** | `os/src/fs/mod.rs` (+2 行：`pub mod tmpfs` + 切换挂载) |
| **修改** | `os/src/syscall/fs.rs` (+1 行：切换 sys_mount 文件系统) |
| **不变** | `os/src/fs/ramfs/mod.rs`（保留作为后备） |
