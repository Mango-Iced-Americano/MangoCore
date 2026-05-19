# VFS 迁移计划：Phase 3–5

## 当前状态

### 已完成（Phase 1–2）

- ✅ RamFS: `Vec<u8>` → 物理页式存储 (`BTreeMap<usize, Arc<FrameTracker>>`)
- ✅ DevFS: 删除 7 个设备文件旧 `impl File for` 死代码（~1200 行）
- ✅ 测试套件: 51 项 LTP 风格测试全通过
- ✅ Oracle 审查修复: rmdir ENOTEMPTY, truncate TOCTOU, urandom read_at
- ✅ Bug 修复: lseek pipe→ESPIPE, getdents 循环读取

### 新旧 VFS 并存现状

```
旧 VFS (待删):
  InodeTrait (inode.rs)         ← FAT32(FatInode) + EXT4(Ext4Inode) 均有 impl
  VFS trait (directory_tree.rs)  ← EasyFileSystem + Ext4FileSystem 均有 impl
  VFSFileContent / VFSDirEnt    ← FAT32 FatInode 仅标记 impl
  File trait (file_trait.rs)     ← FatOSInode + Ext4OSInode + SocketFile 实现
  DirectoryTreeNode              ← dirnode_ptr 字段 (FatOSInode + Ext4OSInode)
  FILE_SYSTEM / GLOBAL_BLOCK_SIZE ← ext4 全家 + swap.rs + fs/mod.rs

新 VFS (保留+扩展):
  IndexNode trait (vfs/index_node.rs)  ← FatInode + Ext4OSInode + RamFS + DevFS
  FileSystem trait (vfs/file_system.rs) ← EasyFileSystem + Ext4FileSystem
  MountFS (vfs/mount.rs)               ← VFS_ROOT 使用
  PageCache (page_cache.rs)            ← 仅 FAT32 使用 (FatPageCacheBackend)
```

### 关键差距

| 文件系统 | 新 PageCache | page_cache() 暴露 | 旧 PageCache (cache.rs) |
|---------|-------------|-------------------|------------------------|
| **FAT32** | ✅ 内部使用 FatPageCacheBackend | ❌ IndexNode::page_cache() 返回 None | ⚠️ file_cache_mgr 字段仍残留 |
| **EXT4** | ❌ 完全未使用 | ❌ 返回 None | ✅ BlockCacheManager (通过旧 File trait 使用) |
| **RamFS** | ❌ 不需要 | ❌ 不需要 | ❌ 不需要 |
| **DevFS** | ❌ 不需要 | ❌ 不需要 | ❌ 不需要 |

---

## Phase 3: FAT32 清理

### 目标
删除 FAT32 的旧 VFS 依赖，同时保持新 VFS 完整，并暴露 PageCache。

### 操作步骤

#### 3.1 提取 IndexNode 依赖的方法
将 `InodeTrait` 块中 IndexNode impl 仍需使用的方法移到 `impl FatInode` 独立块：

| 方法 | 行号 | 被谁使用 |
|------|------|---------|
| `find_local_lock` | InodeTrait 块内 | IndexNode::find (L1794) |
| `ls_lock` | InodeTrait 块内 | IndexNode::list (L1841) |
| `dir_iter` | InodeTrait 块内 | find_local_lock 内部 |
| `create_dir_ent` / `delete_dir_ent` / `set_dir_ent` / `get_dir_ent` | InodeTrait 块内 | create/unlink |
| `fill_empty_dir` | InodeTrait 块内 | create 内部 |
| `set_hint` | InodeTrait 块内 | FatInode::new (L213) |
| `gen_short_name_slice` / `gen_name_slice` / `gen_long_name_slice` | InodeTrait 块内 | 内部使用, IndexNode::create 间接 |

**操作**: 将这些方法从 `impl InodeTrait for FatInode` (L964) 中移出，放入新的 `impl FatInode` 块。

#### 3.2 删除 fat_osinode.rs
- 整个文件 (484 行) → 删除
- `impl File for FatOSInode` → 被 `impl IndexNode for FatInode` 替代
- `FatOSInode.inner: Arc<dyn InodeTrait>` → 不再需要
- `FatOSInode.dirnode_ptr` → 目录操作改用 IndexNode::find/create/unlink

#### 3.3 删除 InodeTrait impl
- 删除 `fat_inode.rs:964-1676` 整个 `impl InodeTrait for FatInode` 块
- 删除 `use crate::fs::inode::InodeTrait` 导入 (L9)

#### 3.4 删除旧 VFS trait 标记
- `fat_inode.rs:40`: 删除 `impl VFSFileContent for FileContent {}`
- `layout.rs:242`: 删除 `impl VFSDirEnt for FATDirEnt {}`
- `efs.rs:143-153`: 删除 `impl VFS for EasyFileSystem`

#### 3.5 清理旧 PageCache 残留
- 删除 `file_cache_mgr: PageCacheManager` 字段 (L70)
- 删除 `PageCacheManager::new()` 构造 (L213)
- 删除 `get_single_cache` / `get_all_cache` / `oom` 方法
- 删除 `clear_at_block_cache_lock` / `get_neighboring_sec` 方法
- 删除 `read_at_block_cache_*` / `write_at_block_cache_lock` 方法 (已改用 new_page_cache)

#### 3.6 暴露 PageCache
- `FatInode` 添加 `IndexNode::page_cache()` 重写，返回 `get_new_page_cache()`
- 使 `File::map_to_kernel_space()` 能正确使用 FAT32 的 PageCache 帧

#### 3.7 适配 dir_iter.rs
- 删除 `use crate::fs::inode::InodeTrait` (L6)
- 改为直接使用 `FatInode` 原生方法

#### 3.8 清理 imports
- `fat_inode.rs`: 删除 `BlockCacheManager, Cache, PageCache, PageCacheManager`, `InodeTrait`, `VFSFileContent, VFS`
- `efs.rs`: 删除 `use crate::fs::directory_tree::VFS`
- `layout.rs`: 删除 `use crate::fs::directory_tree::VFSDirEnt`
- `mod.rs`: 删除 `pub mod fat_osinode`, `pub use fat_osinode::FatOSInode`

### 验证
```bash
make rv64-kernel-build-only && make la64-kernel-build-only
# QEMU 运行 basic+busybox 测试组
make docker -> cd os && make rv64-run
```

---

## Phase 4: EXT4 迁移

### 目标
移除 dirnode_ptr，集成新 PageCache，删除旧式依赖。

### 操作步骤

#### 4.1 创建 Ext4PageCacheBackend
参照 `FatPageCacheBackend` (page_cache.rs:618) 创建：

```rust
pub struct Ext4PageCacheBackend {
    fs: Arc<Ext4FileSystem>,
    inode_num: u32,
    self_ref: ...,
}

impl PageCacheBackend for Ext4PageCacheBackend {
    fn read_page(&self, page_idx: usize, frame: &mut [u8]) {
        // 读取一个 4KB 页 (PAGE_SIZE/BLOCK_SIZE 个块)
        for blk in 0..blocks_per_page {
            let lblock = page_idx * blocks_per_page + blk;
            let pblock = self.fs.get_pblock_idx(self.inode_num, lblock)?;
            self.fs.block_device.read_block(pblock, &mut frame[blk*BLOCK_SIZE..]);
        }
    }
    fn write_page(&self, page_idx: usize, frame: &[u8]) {
        // 类似写入
    }
    fn npages(&self) -> usize {
        // (inode_size + PAGE_SIZE - 1) / PAGE_SIZE
    }
}
```

#### 4.2 在 Ext4OSInode 中集成 PageCache
- 添加字段: `new_page_cache: Mutex<Option<Arc<PageCache>>>`
- 添加方法: `get_new_page_cache()` (懒初始化)
- 重写 `IndexNode::page_cache()` 返回 `get_new_page_cache()`
- 在 `Drop` 中调用 `writeback_all()`

#### 4.3 修改 IndexNode::read_at/write_at 使用 PageCache
当前 (ext4fs.rs:426-453):
```rust
fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<usize> {
    self.ext4fs.read_at(self.inode.inode_num, offset, buf)  // 直接块设备读
}
```

改为:
```rust
fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<usize> {
    let pc = self.get_new_page_cache();
    pc.read(offset, buf)  // 通过 PageCache
}
```

#### 4.4 移除 dirnode_ptr
- 删除 `Ext4OSInode.dirnode_ptr: Arc<Mutex<Weak<DirectoryTreeNode>>>` 字段 (layout.rs:70)
- 删除 `info_dirtree_node()` / `get_dirtree_node()` 方法 (layout.rs:454-461)
- 删除 `deep_clone()` 中的 `dirnode_ptr` 克隆 (layout.rs:147)
- `unlink()` 中: 删除 `dir_node_weak` 路径，保留 `lookup_parent_and_name()` (已有回退逻辑，direntry.rs:777)
- `Drop` 中: 删除 `self.get_dirtree_node()` 相关逻辑 (layout.rs:119)

#### 4.5 删除 InodeTrait impl
- 删除 `ext4_inode.rs:549-679` `impl InodeTrait for Ext4Inode` (大部分为 todo!())
- 删除 `ext4_inode.rs:615-665` 中 block_cache 相关 stub 方法

#### 4.6 删除旧 VFS trait 实现
- `ext4fs.rs:354-364`: 删除 `impl VFS for Ext4FileSystem`
- 保留 `ext4fs.rs:651` `impl FileSystem for Ext4FileSystem` (新)
- 删除 `ext4_inode.rs:549` `impl InodeTrait for Ext4Inode`

#### 4.7 清理旧的 BlockCacheManager 依赖
- `Ext4FileSystem.cache_mgr` 字段: 如果旧 File trait 删除后无用户，则删除
- 保留 `block_device` 引用给新 PageCache backend 使用

### 验证
```bash
make rv64-kernel-build-only && make la64-kernel-build-only
make docker -> cd os && make rv64-run
```

---

## Phase 5: 删除旧 VFS 代码

### 目标
删除 directory_tree.rs、file_trait.rs、inode.rs 中的 InodeTrait 及所有引用。

### 前置条件
- Phase 3, 4 完成 → 所有 InodeTrait impl 已删除
- 所有 `<dyn File>` 引用已替换为 `<dyn IndexNode>`
- 所有 `dirnode_ptr` 已删除
- FILE_SYSTEM / GLOBAL_BLOCK_SIZE 已替换

### 操作步骤

#### 5.1 替换 GLOBAL_BLOCK_SIZE (EXT4 全局)
每个 ext4 模块通过 Ext4FileSystem::block_size() 获取块大小：

```rust
// 旧: let zero_block = [0u8; *crate::fs::directory_tree::GLOBAL_BLOCK_SIZE];
// 新: 通过 self.block_device.block_size() 或参数传递
```

受影响的文件 (15 处):
- ext4_inode.rs:523,539
- block_group.rs:54,236,376,389,413,460,490
- extent.rs:200,235,597,738
- file.rs:106,434,460
- direntry.rs:282
- balloc.rs, superblock.rs, ialloc.rs (import only)

#### 5.2 替换 VFS_ROOT 初始化中的 FILE_SYSTEM
`fs/mod.rs:59,65` 当前通过 `Arc::downcast` 获取具体 FS 实例。
替换为直接引用已创建的 FS 实例。

#### 5.3 替换 swap.rs 的 alloc_blocks
`swap.rs:38` 当前调用 `FILE_SYSTEM.alloc_blocks(blocks)`。
需要直接对 EasyFileSystem 调用，或创建新的分配接口。

#### 5.4 替换 mm/map_area.rs 的 Arc<dyn File>
`mm/map_area.rs:487,509,518` 使用 `Option<Arc<dyn File>>`。
替换为 `Option<Arc<dyn IndexNode>>`，使用 `map_to_kernel_space()` (已有的 vfs::File 方法)。

#### 5.5 替换 OOM/Shrink 回收机制
`mm/frame_allocator.rs:182` — `fs::directory_tree::oom()`
`mm/heap_allocator.rs:56` — `fs::directory_tree::shrink()`

替代方案:
- 使用全局 LRU 列表 (类似 Linux 的 shrinker 机制)
- 或暂时用空函数替代 (no-op)，OOM killer 已有 `pending_oom_kill` 机制

#### 5.6 替换 stats.rs 的 directory_node_count
`utils/stats.rs:6,34` — 删除或替换为新 VFS 统计。

#### 5.7 删除 main.rs 的 init_fs()
`os/src/main.rs:125` — 新 VFS 根已在 `VFS_ROOT` lazy_static 中初始化，此行可删除。

#### 5.8 最终删除文件
- 删除 `os/src/fs/directory_tree.rs` (1134 行)
- 删除 `os/src/fs/file_trait.rs` (76 行)
- 删除 `os/src/fs/inode.rs` 中 `InodeTrait` 的定义 + impl_downcast (L16-126)
- 从 `os/src/fs/mod.rs` 删除 `pub mod directory_tree`, `pub mod file_trait`, `mod inode`
- 从 `os/src/net/socket/mod.rs` 删除 `DirectoryTreeNode` / `File` trait 引用和旧方法体

#### 5.9 清理 fs/mod.rs 的导出
- 保留: `pub use self::fat32::DiskInodeType` (或移到新位置)
- 删除: 任何 directory_tree / file_trait / inode 的 pub mod/use

### 验证
```bash
make rv64-kernel-build-only && make la64-kernel-build-only
make docker -> cd os && make rv64-run
# 验证 51 项 fs_test 全部通过
```

---

## 验证清单

每个 Phase 完成后:
- [ ] `make rv64-kernel-build-only` ✅
- [ ] `make la64-kernel-build-only` ✅
- [ ] QEMU 启动不 panic
- [ ] 51 项 fs_test 全部通过
- [ ] basic + busybox 测试组通过 (mask=0x003)
- [ ] 修改写入 WORK_LOG.md
