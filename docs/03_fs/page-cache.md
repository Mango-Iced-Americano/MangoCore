---
title: "PageCache 页面缓存"
module: "fs/page_cache"
category: fs
status: current
owner: "MangoCore Team"
last_updated: "2026-08-09"
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

当前 SMP 实现以 `op_gate` 建立操作级边界，以每页 `PageEntry.data` 保护物理页
字节，并以 `PG_LOCKED` 写租约阻止同页写者与 writeback 同时修改。不同页可以并行
推进；直接 UserBuffer 路径只使用已经 fault-in 的 no-fault copy，不在 PageCache
锁内触发用户缺页。

文件 `MAP_SHARED` 使用独立的 `FileVmaRmap` 身份记录。PageCache 只保存它的
`Weak`，walker 升级后持有强引用快照，释放 PageCache 注册表锁再进入目标
`AddressSpace`；VM 锁内用 `Arc::ptr_eq` 重验 VMA 身份后才执行 mkclean 或 truncate
unmap。强引用覆盖整个重验窗口，避免 allocator 地址复用造成 ABA 误命中。

## PageState 状态机

每个缓存页面由 `PageEntry` 管理。状态编码在 `flags: AtomicU32` 中，
`PG_UPTODATE`、`PG_DIRTY`、`PG_WRITEBACK`、`PG_ERROR` 等位由 CAS 更新；
对外的 `PageState` 仍是诊断视图，但内部以一个 `AtomicU32` 保存正交的 `PG_*` 位：锁、最新、脏、写回、错误、引用和写回期间重脏。RV64 可将该字级原子操作映射为原生 AMO。

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

写入通过 CAS 添加 `PG_LOCKED | PG_REFERENCED`，复制完成后以一次原子更新发布 `PG_UPTODATE | PG_DIRTY` 并清除锁；不会再把既有脏页往返转换为 `Loading`。写回只认领 `PG_DIRTY && !PG_LOCKED` 的页，认领时清除脏位并设置 `PG_WRITEBACK | PG_LOCKED`。如果写回期间页面被再次写入，`PG_REDIRTIED` 会使完成路径恢复 `PG_DIRTY`，而不是错误地发布为 UpToDate。

启用 `perf_diag` 的 `memory_io` profile 时，`write_user` 额外将 PageEntries 查找、写 lease、用户缓冲复制和 Dirty 发布的累计周期导出到 `/sys/kernel/stats/blockio`。这些是诊断计数，不参与状态转换或锁定协议。

## PageEntry 与 partial-write 跟踪

`PageEntry` 将每页按 512B segment 划分为 8 个扇区（`VALID_SEG_COUNT = 8`），通过 `valid_mask: AtomicU32` 的低 8 位掩码跟踪哪些 segment 已写入有效数据：

- 页面刚 populate 时，`valid_mask = VALID_ALL`（全部有效）
- 整页覆写时，写路径直接标记所有 segment 有效
- 部分写入时，`mark_valid_and_check_full` 逐步累积 `valid_mask`；目标位已全置位时先以 Relaxed load 返回，不执行冗余的原子 OR
- `ensure_fully_valid` 读取后端数据填充无效 segment（快速路径：已满则直接返回）

这一设计解决了稀疏文件（sparse file）中超出旧 EOF 页面的零填充问题：当写入位置超出旧 EOF 时，`get_or_create_entry` 不触发后端 read_page，而是 `frame_alloc` 零填充页并设置 `valid_mask = VALID_ALL`。新建页若对应一次完整 4KB 覆写，写路径改用 `frame_alloc_uninit()`；该页保持 `Loading`，直至完整拷贝完成并提交，因此不会将未初始化内容暴露给读取者。其余部分覆盖和零填充场景继续使用清零分配。

写入、回写和截断不再使用文件级 `io_gate`。写入者通过每个 `PageEntry` 的 `PG_LOCKED` 位独占目标页；回写以 `PG_DIRTY && !PG_LOCKED` 的 CAS 认领页，竞争者不会重复提交同一页。`truncate(new_size)` 移除完整超出 EOF 的页面；若新 EOF 落在保留页中间，会获取同一页锁，清零 EOF 到页尾后发布脏位，避免与同页写入并发复制。

`PageEntries` 使用 `RwLock<Vec<Option<Arc<PageEntry>>>>` 保存连续的页索引目录。查找、快照、统计和 clock 扫描共享读锁；发布、删除及扩容使用写锁。调用方取得 `Arc<PageEntry>` 后立即释放目录锁，后续页面状态转换、数据复制和后端 I/O 不持有目录锁。该实现避免了原子 radix 目录、永久节点分配和不安全指针发布的复杂生命周期约束。

another_ext4 的 `truncate_inode()` 会在缩容时按 extent 尾部释放新 EOF 之后的数据块和空的 extent-tree 元数据块。bridge 先回写已有脏页、截断缓存并回写保留末页的零尾，再调用该后端 API；因此不会再对将被释放的范围执行逐页零填充。

### PageEntry 内核对象引用

```rust
struct PageEntry {
    page: Arc<FrameTracker>,     // 物理页面
    data: RwLock<()>,            // 只在 with_bytes{,_mut} 闭包内保护页面字节
    flags: AtomicU32,            // PageState + LOCKED/REFERENCED/REDIRTIED (PG_LOCKED/UPTODATE/DIRTY/WRITEBACK/...)
    valid_mask: AtomicU32,       // 512B segment 有效性位图（低 8 位）
    map_count: AtomicUsize,      // 已安装的 file-backed PTE 数
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

默认 `write_pages` / `read_pages` 回退为逐页调用，支持合并 I/O 的后端（如 ext4）可覆盖实现批量读写。`sync_batch_read_pages()` 将每次 backend `read_pages` 调用限制为最多 256 个 4 KiB 页（1 MiB staging）；较大的显式 ELF 预取递归拆批，another_ext4 backend 同时以 `E2BIG` 防御超限调用。当前生产路径分别由 `Ext4PageCacheBackend`、`LwExt4PageCacheBackend`、`AnotherExt4PageCacheBackend`、`FatPageCacheBackend` 以及 tmpfs/ramfs 的内部后端承接；`BlockPageCacheBackend` 是尚未接入具体文件系统的通用块设备实现。

## SMP 锁与 I/O API

`PageCache::op_gate` 的读锁用于普通 read/write 与 writeback，写锁用于 truncate、
invalidate、clean-page eviction 以及 I/O 后的 entry 发布。元数据固定按
`entries -> inner` 获取；两者释放后才能进入单页 `PageEntry.data`，page data
绝不能反向取得 PageCache 元数据锁。

crate 内文件路径使用 `read_kernel`、`write_kernel`、`read_at_user` 和
`write_at_user`。UserBuffer 接口直接在 PageEntry 与预校验用户页之间 no-fault copy，
不再分配整段 bounce buffer。写入先取得所有目标页的 `PG_LOCKED` 租约，再开始复制；
写回持有 data read lock 直到 backend I/O 完成。因此不会写出撕裂页面，也不会把新写入
错误清成 UpToDate。

## 二阶段读写模式

所有读写路径采用两阶段模式，核心约束为**不在持有页面目录锁时执行用户态拷贝**：

### 读路径（read_kernel / read_at_user）

```
Phase 1（按页目录查找）: 收集
  for each page in [start_page, end_page]:
    entry = get_page_for_read(page_index)   // 获取或分配页，从后端加载
    ensure_fully_valid(page_index)          // 填充无效 segment
    copies.push(CopyItem { entry, offset, len })

Phase 2（逐页 data 锁）: 拷贝到 kernel buffer
  for each item in copies:
    entry.with_bytes(|src| copy_to_kernel_bounce(src))
```

`read_at_user` 在 Phase 2 完成并释放全部 PageCache 锁后才把 bounce 写入用户空间。
`read_user()` 的多页分支在 Phase 2 前创建一次 `UserBufferWriteCursor`，按 `ReadCopy` 的页序依次调用 `write_from()`。因此跨页数据和用户 buffer segment 都只单调前进一次；它不为每个源页重新从第一个目标 segment 扫描。单页分支仍使用直接 `write_at(0, ...)` 快路径。

### 写路径（write_kernel / write_at_user）

```
Phase 1（按页目录查找）: 收集
  for each page in [start_page, end_page]:
    entry = get_page_for_write_populate(page_index, old_file_size, full_overwrite)
    lease = try_lock_for_write(entry)       // 单次 CAS，不自旋
    // populate 条件: !full_overwrite && !beyond_eof
    // 页超出 EOF → 跳过后端读取，使用零填充
    copies.push(CopyItem { entry, lease, offset, len, full_overwrite })

Phase 2（逐页 data 写锁）: 拷贝并发布 Dirty
  for each item in copies:
    entry.with_bytes_mut(|dst| copy_from_kernel_bounce(dst))
    mark_valid_and_check_full()
    mark_dirty_after_copy()
```

单页场景有 fast path（跳过 `Vec<CopyItem>` 构造和循环分配）。`write_user()` 直接从已经校验的 `UserBuffer` 拷贝到缓存页，供支持该接口的 regular inode 避免 syscall 临时 `Vec` 中转。整页及以上写入在完成后检查 dirty pressure；小于一页的写入每 16 次检查一次，避免在远低于 32 MiB 背景水位时为每个小写入重复执行全局检查。

租约 CAS 观察到 `Loading`、`Writeback`、另一写者的 `PG_LOCKED` 或 CAS 竞争失败时，
内部结果为 Busy，而不是后端 `EAGAIN`。调用者释放 `op_gate` 和本轮已经取得的租约，
再在共享 WaitQueue 上等待目标页可写；成功提交或失败回滚统一发布状态进度。这样普通
文件写不会因 SMP 短暂竞争向用户态泄漏虚假 `EAGAIN`，同时后端真实返回的 `EAGAIN`
仍按 I/O 错误原样传播。

## 脏页追踪与回写

### 全局计数器

`perf_diag` 的 `memory_io` profile 还会记录 PageCache read/write/writeback 调用、页数、
miss、copy/lookup 与总 ticks，并与 `/sys/kernel/stats/blockio` 的后端请求差值配对。
所有热路径计时都先检查 profile；`stats_on=0` 时既不更新原子计数，也不读取架构时钟。

```rust
static GLOBAL_DIRTY_PAGES: AtomicUsize;    // 脏页总数
static GLOBAL_WRITEBACK_PAGES: AtomicUsize; // 正在写回的页数
```

每页 Dirty/Writeback 状态直接编码在原子 flags 中；全量或范围写回通过 entries 快照筛选
脏页，不再维护第二份 `dirty_pages` 集合。脏页计数在 CAS UpToDate → Dirty 成功时递增，
在 claim writeback 时递减，并在失败恢复或 redirty 时校正。写入提交仅在 clean→dirty 转换时递增全局计数；写回认领时递减，失败或 redirty 时恢复。写回从 `PageEntries` 的有序快照收集带 `PG_DIRTY` 的页。

### 紧急写回水位

| 常量 | 值 | 含义 | 动作 |
|------|-----|------|------|
| DIRTY_BACKGROUND | 8192 | 后台启动线（约 32MB） | 触发 `maybe_background_writeback` |
| DIRTY_THROTTLE | 16384 | 写入者节流线（约 64MB） | 写入者同步帮助写回 |
| DIRTY_BACKGROUND_PAGES | 8192 | 后台水位（32 MiB） | 满足空闲帧比例时请求后台写回 |
| DIRTY_THROTTLE_PAGES | 16384 | 节流水位（64 MiB） | 写入者帮助完成一批写回 |
| DIRTY_EMERGENCY_PAGES | 32768 | 紧急水位（128 MiB） | 物理帧压力下强制合作写回 |
| dirty/free ratio | dirty ≥ free × 3/4 | 紧急物理帧压力线 | `maybe_background_writeback` 批量写回最多 256 页 |

`fsync()`、`fdatasync()`、最后一次 `close()` 和系统关闭仍使用 `writeback_all()` 保证持久化。正常 `write()` 不再因固定脏页阈值进入同步后端 I/O。

### 写回层级

1. **单页写回** `writeback_page`: CAS 认领 dirty/unlocked 页并置 `PG_WRITEBACK|PG_LOCKED`，调 `backend.write_page`，完成后检查 PG_REDIRTIED
2. **批量写回** `writeback_pages_run`: 连续脏页组收集 → 统一 flags CAS → 调 `backend.write_pages`
3. **全量写回** `writeback_all`: 扫描所有脏页，分组为连续 run 依次提交；遇到短暂的 `Loading`、`Writeback` 或后端 `EAGAIN` 时，最多重试 100 次且每次让出调度器，而非忙等自旋
4. **合作写回** `maybe_background_writeback`: 仅在紧急脏页/空闲帧比率达到水位时遍历 registry 并写回
5. **压力检查** `balance_dirty_pages`: 整页写入后、或每 16 次小写入后检查内存压力；未达背景/节流水位时不执行写回

写回失败的处理：无论后端 `write_pages` 失败，还是写回前
`ensure_fully_valid` 补全 partial page 失败，页面都会恢复为 Dirty、重新加入
`PG_DIRTY` 并回退全局计数，等待下次写回重试，不会遗留在 Writeback 或 Locked 状态。
`flush_all_page_caches()` 汇总所有 cache 的写回，记录每个失败并返回首个
`SyscallErr`；`syncfs` 和卸载路径将该错误返回给调用者，`sync(2)`、电源循环
和后台回写则记录错误并继续各自必须完成的后续步骤。

### 瞬时状态等待

同一 `PageCache` 共享一个状态进度 generation 和 `WaitQueue`。遇到
`Loading` 或 `Writeback` 页面时，调用者先记录 generation，释放 `op_gate`，再等待
状态转换事件；发布加载、写回成功或写回失败结果时递增 generation 并唤醒等待者。
“先观察、后入队”由 generation 条件重验封闭，避免丢失唤醒，也避免持有 PageCache
操作门时睡眠。

没有 `current_task` 的 scheduler/early-boot 上下文不能加入任务 WaitQueue。此时等待入口
直接返回瞬时 `EAGAIN`：后台 lifecycle drain 会在后续调度轮次重新执行 teardown，
不会在 idle/scheduler 栈上无限忙等。等待者计数使无竞争的状态发布只递增 generation，
不获取 WaitQueue 锁。

文件系统后端自己的事务 admission 竞争由后端适配层等待其真实进度事件。syscall 和
PageCache 不对后端 `EAGAIN` 做固定次数轮询，防止跨层重试放大 CPU 消耗并掩盖错误
归属。

PageCache 的直接 UserBuffer 接口采用全有或全无的描述符长度契约：当请求长度大于
UserBuffer 可访问长度时，读写都必须在加载页面、获取写 lease 或修改脏页状态之前返回
`EFAULT`，不能静默缩短请求并报告部分成功。后端写回返回 `EAGAIN` 时，PageCache 恢复
Dirty 状态并把错误返回给本次调用者；是否等待事务 admission 事件由后端适配层负责，
PageCache 本身不轮询后端。

## 回收机制

回收由调度器循环的 reclaim hook 驱动（`maybe_reclaim_fs_caches`），每 THROTTLE=64 tick 执行一次。

### Clock/Second-Chance Eviction

`evict_clean_pages_clock` 实现时钟算法回收干净页：

```text
hand 指针循环扫描有序 PageEntries 快照
  ├─ 跳过非 UpToDate 页
  ├─ 跳过引用计数 >1 的页（被 mmap 持有）
  ├─ 跳过引用计数 >1 的 FrameTracker
  ├─ PG_REFERENCED 置位 → 清除标志，给第二次机会
  └─ 否则 → 移除 entry，回收页帧
```

Sweep 扫描上限为 `min(len*2, target*16 + 64)`，防止失控。回收时以目标 `Arc<PageEntry>` 为条件移除页槽，避免删除快照后已被并发替换的条目。

注意：以下回收水位线与脏页紧急水位是两个独立的机制。紧急水位只在脏页占用大量可用帧时触发后端写回；回收水位线触发 LRU/Clock 淘汰干净页。两者互不依赖。

### 干净页回收水位线

| 水位 | 条件 | 批次大小 | 触发 |
|------|------|----------|------|
| 低水位 | cached > LOW_WATER (1024) 或堆压力 >75% | 8 页 | 温和回收 |
| 高水位 | cached > HIGH_WATER (4096) | 64 页 | 积极回收 |
| 紧急 | 堆使用率 >90% | 32 页 | 全 PageCache 强制回收 |

### 回收阶段顺序

```
maybe_reclaim_fs_caches:
   1. maybe_background_writeback()          // 仅紧急内存压力时刷脏页
  2. compact_fifo_registry()               // pipe fifo 清理
  3. EXT4_REGISTRY 弱引用清理
  4. prune_inode_objects_budgeted()        // inode 对象回收
  5. prune_page_caches()                   // 孤儿 PageCache 释放
  6. prune_children_stale_entries()        // children 目录项清理
  7. evict_dir_cache()                     // 目录缓存淘汰
  8. shrink_all_page_caches_clean()        // 干净页回收（取决于水位）
```

### 锁约束

**不可在持有 inode 锁时调用 page cache invalidate**。因为 `invalidate_range` 需要独占 `op_gate` 后获取 `entries` 和 `inner` 锁，如果调用者已经持有 inode 的内部锁，且 PageCache 的 writeback 路径需要 inode 锁（如 ext4 后端写入需要获取 inode 信息），就可能产生死锁。规范做法：在调用任何 PageCache 方法前释放 inode 锁。

PageCache 的元数据锁不依赖调度器；`entries` 使用 `spin::RwLock`，读取路径可并发查找/快照，写者仅在发布、删除或扩容时排他。不可重入：在持有同一 PageCache 的写锁时不得再次锁该实例。

### unevictable 页

tmpfs/shmem 的 PageCache 设 `unevictable = true`，clock eviction 直接跳过。这些页没有持久化后端，回收即数据丢失。

## 内部数据结构

`InnerPageCache` 管理元数据：

```rust
struct InnerPageCache {
    pages: BTreeSet<usize>,      // 所有缓存的页索引
}
```

`PageCache` 主结构：

```rust
pub struct PageCache {
    op_gate: RwLock<()>,
    inner: Mutex<InnerPageCache>,
    backend: Mutex<Option<Arc<dyn PageCacheBackend>>>,
    inode: Mutex<Option<Weak<dyn IndexNode>>>,
    entries: Mutex<Vec<Option<Arc<PageEntry>>>>, // 页索引 → 页条目
    i_mmap: Mutex<BTreeMap<usize, Weak<FileVmaRmap>>>,
    unevictable: AtomicBool,       // true = 不可回收（tmpfs/shmem）
    clock_hand: AtomicUsize,       // clock sweep 光标
    state_wait_generation: AtomicUsize,
    state_waiter_count: AtomicUsize,
    state_waiters: Mutex<WaitQueue>,
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

5. **同页写者的串行开销**
   多个 CPU 写同一页时由 `PageEntry.data` 串行化；这保证字节复制与 Dirty 发布的一致性，
   但不会为同一 4 KiB 页提供并行吞吐。不同页仍可分别取得自己的 data lock。
