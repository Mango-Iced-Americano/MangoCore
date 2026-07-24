---
title: "PageCache 页面缓存"
module: "fs/page_cache"
category: fs
status: draft
owner: "MangoCore Team"
last_updated: "2026-06-29"
code_paths:
  - "os/src/fs/page_cache.rs"
  - "os/src/fs/reclaim.rs"
entry_points:
  - "PageCache"
  - "PageState"
  - "global_dirty_pages"
  - "flush_all_page_caches"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "read*"
    - "write*"
    - "mmap*"
    - "fsync*"
  oscomp:
    - "basic"
    - "busybox"
    - "iozone"
    - "libctest"
related_docs:
  - "docs/03_fs/architecture.md"
  - "docs/03_fs/init-and-rootfs.md"
  - "docs/03_fs/vfs-core.md"
---

## 概述

PageCache 是 MangoCore VFS 层的页面级数据缓存，位于 IndexNode trait 与块设备后端之间。它以 4KB 页为粒度缓存文件数据，通过 PageState 状态机管理脏页追踪、回写和回收。设计参考 DragonOS 的 `kernel/src/filesystem/page_cache.rs`，实现基于 Linux 回写机制的简化模型。

PageCache 不感知具体文件系统格式，通过 `PageCacheBackend` trait 桥接到 ext4、tmpfs、ramfs 等不同后端。

## PageState 状态机

每个缓存页面由 `PageEntry` 管理，其 `state` 字段为 `AtomicU8`，取值如下：

```text
Loading ──→ UpToDate ←──→ Dirty ──→ Writeback ──→ UpToDate
  │            │                            │
  └──── I/O error ──→ Error ←─── I/O error ─┘
```

| 状态 | 含义 | 触发条件 |
|------|------|----------|
| Loading | 正在从后端加载 | 页面刚分配，后端 I/O 未完成 |
| UpToDate | 数据为最新 | 读取完成，或写回成功后无 redirty |
| Dirty | 有未写回的数据修改 | 写入路径通过 CAS UpToDate → Dirty |
| Writeback | 正在写回 I/O | writeback_page / writeback_pages_run CAS Dirty → Writeback |
| Error | I/O 错误 | 后端读写失败 |

状态转换通过 `compare_exchange_state`（原子 CAS）实现，避免并发竞态。关键的红色路径处理：如果写回期间页面被再次写入，`PG_REDIRTIED` 标志被设置，写回完成后状态恢复为 Dirty 而非 UpToDate。

## PageEntry 与 partial-write 跟踪

`PageEntry` 将每页按 512B segment 划分为 8 个扇区（`VALID_SEG_COUNT = 8`），通过 `valid_mask: AtomicU8` 位掩码跟踪哪些 segment 已写入有效数据：

- 页面刚 populate 时，`valid_mask = VALID_ALL`（全部有效）
- 整页覆写时，写路径直接标记所有 segment 有效
- 部分写入时，`mark_valid_and_check_full` 逐步累积 `valid_mask`
- `ensure_fully_valid` 读取后端数据填充无效 segment（快速路径：已满则直接返回）

这一设计解决了稀疏文件（sparse file）中超出旧 EOF 页面的零填充问题：当写入位置超出旧 EOF 时，`get_or_create_entry` 不触发后端 read_page，而是 `frame_alloc` 零填充页并设置 `valid_mask = VALID_ALL`。

### PageEntry 内核对象引用

```rust
struct PageEntry {
    page: Arc<FrameTracker>,     // 物理页面
    state: AtomicU8,             // PageState 编码
    valid_mask: AtomicU8,        // 512B segment 有效性位图
    flags: AtomicU8,             // PG_REFERENCED, PG_REDIRTIED
}
```

## PageCacheBackend trait

```rust
pub trait PageCacheBackend: Send + Sync {
    fn read_page(&self, index: usize, buf: &mut [u8]) -> Result<usize, SyscallErr>;
    fn write_page(&self, index: usize, buf: &[u8]) -> Result<usize, SyscallErr>;
    fn write_pages(&self, start_index: usize, pages: &[&[u8]]) -> Result<usize, SyscallErr>;
    fn read_pages(&self, start_index: usize, pages: &mut [&mut [u8]]) -> Result<usize, SyscallErr>;
    fn npages(&self) -> usize;
}
```

默认 `write_pages` / `read_pages` 回退为逐页调用。支持合并 I/O 的后端（如 ext4）可覆盖实现批量读写。`BlockPageCacheBackend` 是块设备后端的默认实现，将页索引转换为块偏移后通过 `BlockDevice` trait 驱动。

## 二阶段读写模式

所有读写路径采用两阶段模式，核心约束为**不在持有锁时执行用户态拷贝**：

### 读路径（read / read_user）

```
Phase 1（持锁）: 收集
  for each page in [start_page, end_page]:
    entry = get_page_for_read(page_index)   // 获取或分配页，从后端加载
    ensure_fully_valid(page_index)          // 填充无效 segment
    copies.push(CopyItem { entry, offset, len })

Phase 2（无锁）: 拷贝
  for each item in copies:
    copy_from_slice(src, dst)   // 或 UserBuffer::write_at
```

### 写路径（write / write_user）

```
Phase 1（持锁）: 收集
  for each page in [start_page, end_page]:
    entry = get_page_for_write_populate(page_index, old_file_size, full_overwrite)
    // populate 条件: !full_overwrite && !beyond_eof
    // 页超出 EOF → 跳过后端读取，使用零填充
    copies.push(CopyItem { entry, offset, len, full_overwrite })

Phase 2（无锁）: 拷贝
  for each item in copies:
    copy data from user buffer to entry.as_slice_mut()
    mark_valid_and_check_full()
```

单页场景有 fast path（跳过 `Vec<CopyItem>` 构造和循环分配）。写入完成后调用 `balance_dirty_pages()` 触发节流检测。

## 脏页追踪与回写

### 全局计数器

`perf_diag` 的 `memory_io` profile 还会记录 PageCache read/write/writeback 调用、页数、
miss、copy/lookup 与总 ticks，并与 `/sys/kernel/stats/blockio` 的后端请求差值配对。
所有热路径计时都先检查 profile；`stats_on=0` 时既不更新原子计数，也不读取架构时钟。

```rust
static GLOBAL_DIRTY_PAGES: AtomicUsize;    // 脏页总数
static GLOBAL_WRITEBACK_PAGES: AtomicUsize; // 正在写回的页数
```

每个 `PageCache` 实例维护 `inner.dirty_pages: BTreeSet<usize>` 记录其脏页索引。脏页计数在 CAS UpToDate → Dirty 成功时递增，在写回完成后递减。

### 节流阈值

| 常量 | 值 | 含义 | 动作 |
|------|-----|------|------|
| DIRTY_BACKGROUND | 2048 | 后台启动线（约 8MB） | 触发 `maybe_background_writeback` |
| DIRTY_THROTTLE | 4096 | 写入者节流线（约 16MB） | 写入者同步帮助写回 |

### 写回层级

1. **单页写回** `writeback_page`: CAS Dirty → Writeback，调 `backend.write_page`，完成后检查 PG_REDIRTIED
2. **批量写回** `writeback_pages_run`: 连续脏页组收集 → 统一 CAS → 调 `backend.write_pages`
3. **全量写回** `writeback_all`: 扫描所有脏页，分组为连续 run 依次提交
4. **合作写回** `maybe_background_writeback`: 调度器 reclaim hook 每 64 tick 触发一次，遍历 registry 所有 PageCache
5. **写入者节流** `balance_dirty_pages`: 超过 DIRTY_THROTTLE 时，写入者主动写回一批脏页

写回失败的处理：页面恢复为 Dirty 状态，全局计数回退，等待下次写回重试。

## 回收机制

回收由调度器循环的 reclaim hook 驱动（`maybe_reclaim_fs_caches`），每 THROTTLE=64 tick 执行一次。

### Clock/Second-Chance Eviction

`evict_clean_pages_clock` 实现时钟算法回收干净页：

```text
hand 指针循环扫描 entries[]
  ├─ 跳过非 UpToDate 页
  ├─ 跳过引用计数 >1 的页（被 mmap 持有）
  ├─ 跳过引用计数 >1 的 FrameTracker
  ├─ PG_REFERENCED 置位 → 清除标志，给第二次机会
  └─ 否则 → 移除 entry，回收页帧
```

Sweep 扫描上限为 `min(len*2, target*16 + 64)`，防止失控。收回的页在 `inner.pages` 中同步移除，entries 数组末尾的 `None` 槽被截断。

注意：以下回收水位线与脏页节流阈值（DIRTY_BACKGROUND/DIRTY_THROTTLE）是两个独立的机制。脏页阈值触发写回（将脏页写入后端），而回收水位线触发 LRU/Clock 淘汰干净页以释放内存。两者互不依赖。

### 干净页回收水位线

| 水位 | 条件 | 批次大小 | 触发 |
|------|------|----------|------|
| 低水位 | cached > LOW_WATER (1024) 或堆压力 >75% | 8 页 | 温和回收 |
| 高水位 | cached > HIGH_WATER (4096) | 64 页 | 积极回收 |
| 紧急 | 堆使用率 >90% | 32 页 | 全 PageCache 强制回收 |

### 回收阶段顺序

```
maybe_reclaim_fs_caches:
  1. maybe_background_writeback()          // 先刷脏页
  2. compact_fifo_registry()               // pipe fifo 清理
  3. EXT4_REGISTRY 弱引用清理
  4. prune_inode_objects_budgeted()        // inode 对象回收
  5. prune_page_caches()                   // 孤儿 PageCache 释放
  6. prune_children_stale_entries()        // children 目录项清理
  7. evict_dir_cache()                     // 目录缓存淘汰
  8. shrink_all_page_caches_clean()        // 干净页回收（取决于水位）
```

### 锁约束

**不可在持有 inode 锁时调用 page cache invalidate**。因为 `invalidate_range` 需要获取 `entries` 和 `inner` 锁，如果调用者已经持有 inode 的内部锁，且 PageCache 的 writeback 路径需要 inode 锁（如 ext4 后端写入需要获取 inode 信息），就可能产生死锁。规范做法：在调用任何 PageCache 方法前释放 inode 锁。

PageCache 自身的 `inner` 和 `entries` 使用 `spin::Mutex`，不依赖调度器，因此即使在中断上下文中也安全。但不可重入：在持有 PageCache 锁时不得再次锁同一个 PageCache 实例。

### unevictable 页

tmpfs/shmem 的 PageCache 设 `unevictable = true`，clock eviction 直接跳过。这些页没有持久化后端，回收即数据丢失。

## 内部数据结构

`InnerPageCache` 管理元数据：

```rust
struct InnerPageCache {
    pages: BTreeSet<usize>,      // 所有缓存的页索引
    dirty_pages: BTreeSet<usize>, // 脏页索引
}
```

`PageCache` 主结构：

```rust
pub struct PageCache {
    inner: Mutex<InnerPageCache>,
    backend: Mutex<Option<Arc<dyn PageCacheBackend>>>,
    inode: Mutex<Option<Weak<dyn IndexNode>>>,
    entries: Mutex<Vec<Option<Arc<PageEntry>>>>, // 页索引 → 页条目
    unevictable: AtomicBool,       // true = 不可回收（tmpfs/shmem）
    clock_hand: AtomicUsize,       // clock sweep 光标
}
```

`PAGE_CACHE_REGISTRY` 是全局 `Vec<Weak<PageCache>>`，用于遍历所有活跃 PageCache 执行写回和回收。

## Test Mapping

| 特性 | 覆盖范围 | 测试方式 |
|------|----------|----------|
| 基本读写 | read/write 系统调用语义 | LTP read* / write* |
| 脏页写回 | 修改后数据持久化 | fsync/close 后验证 |
| mmap 共享页 | MAP_SHARED 文件映射 | mmap* 测试组 |
| 稀疏文件 | 超出 EOF 写入 + 空洞读取 | OSComp lua / libctest |
| 多页读写 | 跨页边界的缓冲 I/O | iozone / unixbench |
| 回收压力 | 大量文件读取后的回收行为 | libctest / LTP mmapstress |

## Known Issues

1. **异步 I/O 缺失**
   当前实现为同步 I/O，`writeback_page` 在 CAS Writeback 后阻塞等待后端完成。无 IO_URING 或 AIO 支持。PageEntry 在 Writeback 期间不阻塞读者（read path 照常），但写回本身是同步的。

2. **Clock eviction 精细度**
   单 PageCache 级别的 clock sweep 无法感知全局内存压力。在极端内存压力下，可能一个 PageCache 被过度回收而其他 PageCache 仍持有大量缓存页。回收不追踪页的访问频度（仅有单 bit PG_REFERENCED），可能过早淘汰高热度页。

3. **lock-inversion 防护缺失**
   `invalidate_range` 要求调用者确保不持有 inode 锁，但此约束没有编译期检查。违反约束将导致运行时死锁。未来可考虑引入 `debug_assert!` 或锁顺序文档化工具。

4. **partial-write 与读回一致性**
   `ensure_fully_valid` 在部分写入后补读后端数据时，如果后端数据在两次操作间发生变更（外部修改），可能读取到旧数据。当前场景下 MangoCore 是唯一写入者，此问题不存在；但未来支持多核或共享存储时需要处理。

5. **PG_REDIRTIED 与多写者**
   写回期间 `PG_REDIRTIED` 只能标记一次。如果多个写入者在写回期间并发写入，后一个写入者无法感知前一个的 redirty 标志已被消耗。目前单核抢占式调度下此情况不会发生。
