# ext4 Metadata / Inode 缓存优化设计文档

> **参考蓝本**: DragonOS `kernel/src/filesystem/ext4/` (inode.rs / filesystem.rs / page_cache.rs)
> **执行日期**: 2026-05-17
> **状态**: Phase 0 — 调研 + 对照 + 规划

---

## 0. DragonOS 对照调研（修正版）

### 0.1 调研范围

| DragonOS 文件 | 关键内容 |
|--------------|---------|
| `kernel/src/filesystem/ext4/filesystem.rs` | `Ext4FileSystem` 持有 `another_ext4::Ext4`，root inode 创建，VFS→another_ext4 边界 |
| `kernel/src/filesystem/ext4/inode.rs` | `Ext4Inode` / `LockedExt4Inode`，`children` cache，`cached_file_size`，`metadata_dirty`，`page_cache` |
| `kernel/src/filesystem/page_cache.rs` | `PageCache` / `InnerPageCache` / `PageCacheBackend` / `AsyncPageCacheBackend`，dirty_pages，writeback |

### 0.2 缓存分类对照表（修正版）

| 缓存类别 | DragonOS | 当前内核 (MangoCore) | 借鉴/增强 |
|---------|----------|---------------------|----------|
| **普通文件数据缓存** | `PageCache` + `AsyncPageCacheBackend` → `IndexNode::read_sync/write_sync` → `another_ext4::Ext4` | `PageCache` + `Ext4PageCacheBackend` → `Ext4FileSystem::read_at/write_at` | ✅ 已对齐。`PageCache` 是普通文件数据唯一 dirty owner |
| **VFS inode global object cache** | **无。** 不维护全局 `ino → Arc<Inode>` 映射。child inode 由 parent.children 持有 | **无。** 每次 `find()` / `lookup` 都 `new_vfs()` | 🔶 **MangoCore 增强。** 需处理 hardlink/rename/parent/dname 语义 |
| **目录 children cache** | `Ext4Inode.children: BTreeMap<DName, Arc<LockedExt4Inode>>`，`find()` 先查 | **无。** 每次 `find()` → `dir_find_entry` → `get_inode_ref` 磁盘读 | ✅ **直接借鉴。** 最优先实现项 |
| **cached_file_size** | `Ext4Inode.cached_file_size: Option<u64>` | **无。** 每次 `metadata()` → `get_inode_ref` 读磁盘 | ✅ **直接借鉴** |
| **metadata_dirty (per-inode)** | `Ext4Inode.metadata_dirty: bool`，`close()` 延迟写回 | **无。** 每次修改立即 `write_back_inode` | ✅ **直接借鉴**。第一版仅管 per-inode metadata |
| **cached_symlink_target** | **无显式字段。** `read_sync()` → `another_ext4::readlink()` 由底层处理 | **无。** 每次 `read_at()` → `get_inode_ref` 磁盘读 → `block_as_bytes()` | 🔶 **MangoCore 增强。** fast symlink target ∈ inode metadata，不走 PageCache |
| **底层 ext4 inode table cache** | `another_ext4` 内部机制未确认（不假设有或没有） | **无。** `get_inode_ref()` 每次 `Block::load_offset` → `read_block` | 🔶 **Phase 5 评估** |
| **通用 metadata block cache** | **无** | **无** | 🔶 **Phase 5 评估** |
| **noatime 优化** | ✅ `read_at()` 不更新 atime | ❌ `write_at()` 更新 mtime/ctime 后立即写回 | ✅ **直接借鉴** |

### 0.3 关键架构差异

| 维度 | DragonOS | 当前内核 (MangoCore) |
|------|---------|---------------------|
| **底层 ext4 实现** | `kdepends::another_ext4` 外部 crate（完整语义） | 自己实现所有 ext4 逻辑（extent/bitmap/balloc/ialloc/direntry） |
| **Ext4FileSystem 结构** | `fs: another_ext4::Ext4` + `raw_dev` + `root_inode` | `block_device` + `superblock` + `block_size` + `page_caches` + `__self_ref` |
| **VFS inode 结构** | `LockedExt4Inode(Mutex<Ext4Inode>)`：VFS 层对象 | `Ext4OSInode`：持有 `Arc<Mutex<Ext4InodeRef>>`（磁盘快照） |
| **inode 定位方式** | `inner_inode_num` → 委托 `another_ext4` | `Ext4InodeRef` (inode_num + Ext4Inode 副本) |
| **children 持有方式** | `BTreeMap<DName, Arc<LockedExt4Inode>>` 强引用 | 无缓存 |
| **PageCache 初始化时机** | inode 创建时立即初始化 | 懒初始化 (`get_new_page_cache()`) |

### 0.4 实现优先级

```
P0 (本阶段必须):
  ✅ children cache        — 消除同目录 repeated lookup 的目录扫描
  ✅ per-inode metadata     — cached_file_size + metadata_dirty + cached_symlink_target
  ✅ noatime-like read      — 读操作不更新 atime 到磁盘

P1 (Phase 4 后评估):
  🔶 global inode object cache — 见 0.5 预备分析
  🔶 get_inode_cached/modify_inode_cached — 缓存底层 inode table 读

P2 (Phase 5 评估):
  🔶 metadata block cache  — 仅当统计证明需要
```

### 0.5 global inode object cache 预备分析

**DragonOS 没有全局 inode object cache 的原因**：
- `parent.children` 持有 `Arc<LockedExt4Inode>` 强引用，子 inode 生命周期绑在父目录上
- 不需要额外全局注册表来查找 inode object

**MangoCore 为什么可以考虑加**：
- 当前内核的 `find()` 每次创建新 `Arc<Ext4OSInode>`，开销大
- 但直接加 `BTreeMap<u32, Weak<dyn IndexNode>>` 需处理：

| 场景 | 问题 | 处理要求 |
|------|------|---------|
| hardlink | 同一 inode 有两个不同 parent | 不能绑定唯一父路径。inode object 不应存储 "parent" 指针（除非正确处理多父） |
| rename (跨目录) | 旧 parent 的 children 移除，新 parent 的 children 加入 | 缓存一致性 |
| rename (覆盖) | 被覆盖的旧 inode 应失效 | 从 cache 移除或标记 stale |
| unlink | links_count 减 1，可能未到 0 | inode 不能从 global cache 移除，除非 links_count == 0 |
| 内存泄漏 | Weak 升级失败需清理 | 周期清理或惰性清理 |

**结论**：先实现 children cache（P0），再评估是否需要 global inode object cache（P1）。二者不冲突——children cache 解决 lookup 加速，global cache 解决同一 ino 多次访问的 VFS 对象复用。

---

## 1. 缓存边界定义（最终态）

| 层 | 管理者 | 数据类型 | 备注 |
|----|--------|---------|------|
| 普通文件数据 | `PageCache` | data blocks (4KB pages) | PageCache 是唯一 dirty owner |
| VFS inode object | `Ext4FileSystem.inode_objects` (Phase 1) | `Weak<dyn IndexNode>` | 可选，Phase 1 实现，P1 评估 |
| 目录项 lookup 加速 | `Ext4OSInode.children` (Phase 2) | `BTreeMap<String, Weak<dyn IndexNode>>` | create/unlink/rename 必须维护一致性 |
| per-inode metadata | `Ext4OSInode` cached 字段 (Phase 3) | `cached_file_size`, `cached_symlink_target`, `metadata_dirty` | fast symlink target 属于 metadata |
| 底层 ext4 inode snapshot | `get_inode_cached` / `CachedExt4Inode` (Phase 4) | `Ext4Inode` + dirty flag | 兼容 `get_inode_ref` 旧签名 |
| 普通文件 data block | 不进入 metadata cache | — | — |

---

## 2. Counter 框架

### 2.1 定义

```rust
// os/src/fs/ext4/counters.rs (新增)

use core::sync::atomic::{AtomicU64, Ordering};

// ── VFS inode object cache ──
pub static INODE_OBJ_CACHE_HIT: AtomicU64 = AtomicU64::new(0);
pub static INODE_OBJ_CACHE_MISS: AtomicU64 = AtomicU64::new(0);
pub static INODE_OBJ_INSERT: AtomicU64 = AtomicU64::new(0);
pub static INODE_OBJ_REMOVE: AtomicU64 = AtomicU64::new(0);
pub static INODE_OBJ_INVALIDATE: AtomicU64 = AtomicU64::new(0);

// ── children cache ──
pub static DIR_CHILDREN_CACHE_HIT: AtomicU64 = AtomicU64::new(0);
pub static DIR_CHILDREN_CACHE_MISS: AtomicU64 = AtomicU64::new(0);
pub static DIR_CHILDREN_INSERT: AtomicU64 = AtomicU64::new(0);
pub static DIR_CHILDREN_REMOVE: AtomicU64 = AtomicU64::new(0);
pub static DIR_CHILDREN_INVALIDATE: AtomicU64 = AtomicU64::new(0);

// ── per-inode metadata cache ──
pub static INODE_META_CACHE_HIT: AtomicU64 = AtomicU64::new(0);
pub static INODE_META_CACHE_MISS: AtomicU64 = AtomicU64::new(0);
pub static SYMLINK_TARGET_CACHE_HIT: AtomicU64 = AtomicU64::new(0);
pub static SYMLINK_TARGET_CACHE_MISS: AtomicU64 = AtomicU64::new(0);
pub static METADATA_DIRTY_MARK: AtomicU64 = AtomicU64::new(0);
pub static METADATA_FLUSH_COUNT: AtomicU64 = AtomicU64::new(0);
pub static METADATA_FLUSH_ERROR: AtomicU64 = AtomicU64::new(0);

// ── 底层 ext4 block I/O (Phase 5 评估用) ──
pub static INODE_TABLE_READ: AtomicU64 = AtomicU64::new(0);
pub static INODE_TABLE_WRITE: AtomicU64 = AtomicU64::new(0);
pub static INODE_BITMAP_READ: AtomicU64 = AtomicU64::new(0);
pub static INODE_BITMAP_WRITE: AtomicU64 = AtomicU64::new(0);
pub static BLOCK_BITMAP_READ: AtomicU64 = AtomicU64::new(0);
pub static BLOCK_BITMAP_WRITE: AtomicU64 = AtomicU64::new(0);
pub static DIR_BLOCK_READ: AtomicU64 = AtomicU64::new(0);
pub static DIR_BLOCK_WRITE: AtomicU64 = AtomicU64::new(0);
pub static GROUP_DESC_READ: AtomicU64 = AtomicU64::new(0);
pub static GROUP_DESC_WRITE: AtomicU64 = AtomicU64::new(0);
pub static SUPERBLOCK_READ: AtomicU64 = AtomicU64::new(0);
pub static SUPERBLOCK_WRITE: AtomicU64 = AtomicU64::new(0);

// ── 辅助函数 ──
pub fn dump_cache_stats() {
    log::info!("=== ext4 Cache Statistics ===");
    log::info!("[inode_obj] hit={} miss={} insert={} remove={} invalidate={}",
        INODE_OBJ_CACHE_HIT.load(Ordering::Relaxed),
        INODE_OBJ_CACHE_MISS.load(Ordering::Relaxed),
        INODE_OBJ_INSERT.load(Ordering::Relaxed),
        INODE_OBJ_REMOVE.load(Ordering::Relaxed),
        INODE_OBJ_INVALIDATE.load(Ordering::Relaxed),
    );
    log::info!("[children]   hit={} miss={} insert={} remove={} invalidate={}",
        DIR_CHILDREN_CACHE_HIT.load(Ordering::Relaxed),
        DIR_CHILDREN_CACHE_MISS.load(Ordering::Relaxed),
        DIR_CHILDREN_INSERT.load(Ordering::Relaxed),
        DIR_CHILDREN_REMOVE.load(Ordering::Relaxed),
        DIR_CHILDREN_INVALIDATE.load(Ordering::Relaxed),
    );
    log::info!("[meta]       hit={} miss={} sym_hit={} sym_miss={} dirty={} flush={} flush_err={}",
        INODE_META_CACHE_HIT.load(Ordering::Relaxed),
        INODE_META_CACHE_MISS.load(Ordering::Relaxed),
        SYMLINK_TARGET_CACHE_HIT.load(Ordering::Relaxed),
        SYMLINK_TARGET_CACHE_MISS.load(Ordering::Relaxed),
        METADATA_DIRTY_MARK.load(Ordering::Relaxed),
        METADATA_FLUSH_COUNT.load(Ordering::Relaxed),
        METADATA_FLUSH_ERROR.load(Ordering::Relaxed),
    );
    log::info!("[block_io]   ino_tbl_r={} w={} ino_bmp_r={} w={} blk_bmp_r={} w={} dir_r={} w={} gd_r={} w={} sb_r={} w={}",
        INODE_TABLE_READ.load(Ordering::Relaxed),
        INODE_TABLE_WRITE.load(Ordering::Relaxed),
        INODE_BITMAP_READ.load(Ordering::Relaxed),
        INODE_BITMAP_WRITE.load(Ordering::Relaxed),
        BLOCK_BITMAP_READ.load(Ordering::Relaxed),
        BLOCK_BITMAP_WRITE.load(Ordering::Relaxed),
        DIR_BLOCK_READ.load(Ordering::Relaxed),
        DIR_BLOCK_WRITE.load(Ordering::Relaxed),
        GROUP_DESC_READ.load(Ordering::Relaxed),
        GROUP_DESC_WRITE.load(Ordering::Relaxed),
        SUPERBLOCK_READ.load(Ordering::Relaxed),
        SUPERBLOCK_WRITE.load(Ordering::Relaxed),
    );
}
```

### 2.2 插桩位置

在 `get_inode_ref`、`write_back_inode`、`Block::load_offset`、`BlockDevice::read_block/write_block` 等关键位置插入 counter inc。

---

## 3. 实施计划

| Phase | 内容 | 涉及文件 | 风险 |
|-------|------|---------|------|
| **0** | 本文档 + counter 框架 | `docs/ext4-cache-design.md`, `os/src/fs/ext4/counters.rs` | 无 |
| **1** | filesystem-level inode object cache | `os/src/fs/ext4/ext4fs.rs` (Ext4FileSystem), `os/src/fs/ext4/layout.rs` (Ext4OSInode) | hardlink/rename 语义 |
| **2** | directory children cache | `os/src/fs/ext4/layout.rs` (Ext4OSInode), `os/src/fs/ext4/ext4fs.rs` (find/create/unlink/rename) | 锁顺序、缓存一致性 |
| **3** | per-inode metadata cache | `os/src/fs/ext4/layout.rs`, `os/src/fs/ext4/ext4fs.rs` (read_at/write_at/symlink/truncate) | PageCache 边界 |
| **4** | cached inode 改造 | `os/src/fs/ext4/ext4_inode.rs` (CachedExt4Inode), 所有调用点 | API 兼容性 |
| **5** | 热点路径迁移 + 测试 | 各文件 | 回归风险 |
| **6** | IO counter 统计报告 | `os/src/fs/ext4/counters.rs` | — |

---

## 4. 设计原则（逐条对应禁止项）

1. ✅ 不引入新的通用 BlockDevice dirty cache
2. ✅ 不改变 BlockDevice::write_block 语义
3. ✅ 不实现 journal / transaction / crash replay
4. ✅ 不实现复杂批量提交框架
5. ✅ 普通文件 data block 不放 metadata cache
6. ✅ PageCache 是普通文件数据唯一 dirty owner
7. ✅ 不做大规模无关重构
8. ✅ 维护 inode checksum / metadata checksum / mode / size / nlink / time
9. ✅ 理解设计后结合当前内核接口迁移（不直接照抄）

---

## 5. 参考

- DragonOS `kernel/src/filesystem/ext4/inode.rs` — Ext4Inode / LockedExt4Inode 结构
- DragonOS `kernel/src/filesystem/ext4/filesystem.rs` — Ext4FileSystem 与 another_ext4 边界
- DragonOS `kernel/src/filesystem/page_cache.rs` — PageCache / PageCacheBackend 设计
- 当前内核 `os/src/fs/ext4/ext4fs.rs` — Ext4FileSystem + IndexNode impl
- 当前内核 `os/src/fs/ext4/layout.rs` — Ext4OSInode 结构
- 当前内核 `os/src/fs/ext4/ext4_inode.rs` — Ext4Inode / get_inode_ref / write_back_inode
- 当前内核 `os/src/fs/page_cache.rs` — PageCache 实现
- 当前内核 `os/src/fs/vfs/index_node.rs` — IndexNode trait
