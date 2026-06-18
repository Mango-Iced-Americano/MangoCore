# MangoCore FS Hot Path Optimization — 完整执行计划

## 背景

MangoCore 内核（`#![no_std]` Rust, riscv64/loongarch64 双架构）的 ext4 文件系统存在以下性能瓶颈：

1. **getdents64 效率低**：固定大小 `Dirent`（147B），8192 buffer 只能返回 ~55 个 entry
2. **stat/access/open 触发不必要的 full open**：`fstatat`/`statx` 非 NOFOLLOW 路径通过 `open_file_at()` 创建完整 `vfs::File`
3. **无 metadata block cache**：所有元数据修改立即同步写盘，导致严重 write amplification
4. **无 dentry cache / inode cache**：repeated lookup 每次都重新扫描
5. **symlink 创建无 dirty batching**：每个 symlink 触发 ~6 次 metadata block write

## Phase 0：基线确认

### 文件
- `user/src/bin/fs_test.rs` (1929 行, 51 tests)
- `os/src/fs/ext4/counters.rs` (270 行, 45 fields)
- `os/src/fs/vfs/file.rs` (937 行)
- `os/src/fs/vfs/index_node.rs` (359 行)
- `os/src/fs/mod.rs` (489 行, VFS_ROOT + vfs_lookup)
- `os/src/syscall/fs.rs` (2287 行)
- `os/src/fs/ext4/ext4fs.rs` (1654 行)
- `os/src/fs/ext4/direntry.rs`
- `os/src/fs/ext4/ext4_inode.rs`
- `os/src/fs/dirent.rs` (50 行)

### 步骤
1. 编译双架构：`make rv64-kernel-build-only` + `make la64-kernel-build-only`
2. 运行 fs_test：确保 51/51 全部通过
3. 开启 profile_audit：修改 `run_profile_audit()` 调用 `profile_audit_real()`
4. 保存 baseline counter dump

---

## Phase 1：新增测试 + Counter 扩展

### 1.1 fs_test.rs 新增 5 个 perf tests

**插入点**：
- 函数定义：`test_stress_truncate()` 结束后（行 1412），E 组注释前（行 1414）
- main() 调用：`[49/51]` 后（行 1697），E 组注释前（行 1699）
- 总测试数：51 → 56，fork 测试重新编号为 `[55/56]` `[56/56]`

**新增测试**：
1. `test_perf_getdents_1000` — 1000 文件目录 getdents 扫描（8KB + 64KB buffer）
2. `test_perf_stat_like_1000` — 1000 文件目录 fstatat path lookup
3. `test_perf_repeated_lookup_cache` — 重复 lookup 100 次（existing + negative）
4. `test_perf_symlink_batch_200` — 200 个 fast symlink 正确性 + 性能
5. `test_perf_open_access_large_dir` — 1000 文件目录 open/access path lookup

### 1.2 新增 no_std 路径构造 helper

```rust
use alloc::format;
use alloc::string::String;

fn make_path(prefix: &str, name: &str) -> String;
fn make_file_path(prefix: &str, idx: usize) -> String;
fn make_link_path(prefix: &str, idx: usize) -> String;
```

如果 `alloc::format` 不可用，使用栈 buffer helper（128 字节）。

### 1.3 新增 getdents parser helper

```rust
fn count_dir_entries(buf: &[u8], n: isize, expected_prefix: Option<&str>) -> (usize, bool);
```

支持 Linux-like `linux_dirent64` 变长格式解析。

### 1.4 扩展 ext4 counter 系统

**新增 AtomicU64 字段**（在 `os/src/fs/ext4/counters.rs`）：

```
block_read_count          metadata_block_cache_hit    dentry_lookup_count
block_write_count         metadata_block_cache_miss   dentry_cache_hit
metadata_block_read_count metadata_dirty_block_count  dentry_cache_miss
metadata_block_write_count metadata_flush_immediate    negative_dentry_hit
inode_load_count          dir_lookup_count            negative_dentry_insert
inode_cache_hit           dir_full_scan_count         getdents_call_count
inode_cache_miss          dir_full_scan_entries       getdents_returned_entries
inode_dirty_count         cache_all_subfile_count     getdents_returned_bytes
inode_flush_count         cache_all_subfile_entries   getdents_invalid_reclen
symlink_slow_count        cache_all_subfile_inodes
```

**修改位置**：
1. 声明：`counters.rs` 中对应 section 下
2. `reset_counters()` 的 `all` 数组
3. `dump_scenario()` 的 `println!` 行（`[ext4_counters][<label>] <key>=<value>` 格式）

### 1.5 添加 sys_faccessat2 和 sys_statx wrapper

在 `user/src/syscall.rs` 中新增：
- `pub const SYSCALL_FACCESSAT2: usize = 439;`
- `pub fn sys_faccessat2(dirfd, path, mode, flags) -> isize;`
- `pub const SYSCALL_STATX: usize = 291;`
- `pub fn sys_statx(dirfd, path, flags, mask, buf) -> isize;`

### 验证
- 双架构编译通过
- 新增 5 个测试能运行（先允许性能阈值 WARN）
- Counter dump 能输出所有新字段
- 原有 51 tests 全部通过

---

## Phase 2：VFS lookup 轻量化

### 2.1 创建 lightweight lookup_path API

在 `os/src/fs/mod.rs` 中新增：

```rust
/// Lightweight path lookup — 不创建 File, 不修改 fd offset, 不处理 O_CREAT/O_TRUNC
pub fn lookup_path(
    start: &Arc<dyn IndexNode>,
    path: &str,
    options: LookupOptions,
) -> Result<LookupResult, isize>;
```

`LookupOptions` 包含：
- `follow_final_symlink: bool`
- `allow_empty_path: bool`

`LookupResult` 包含：
- `inode: Arc<dyn IndexNode>`
- `metadata: Metadata` (已获取，避免二次调用)
- `file_type: FileType`

### 2.2 改造 fstatat/statx/faccessat2/readlinkat

- `sys_fstatat`：non-NOFOLLOW 路径改用 `lookup_path` 替代 `open_file_at()`
- `sys_statx`：non-NOFOLLOW 路径改用 `lookup_path`
- `sys_faccessat2`：改用 `lookup_path`，不再调用 `open_file_at()`
- `sys_readlinkat`：已用 `vfs_lookup(start, path, false)`，保持不变

### 2.3 确保 check_open_permission 不被绕过

轻量 lookup 也需要基本的访问权限检查（如权限模型未实现可默认 allow）。

### 验证
- 所有原有 fstatat/readlink/symlink 测试通过
- `test_perf_stat_like_1000` 不触发 `cache_all_subfile`
- `test_perf_open_access_large_dir` 不触发全目录扫描

---

## Phase 3：getdents64 重构（变长 dirent64 + list_dirents trait）

### 3.1 新增 IndexNode::list_dirents trait method

在 `os/src/fs/vfs/index_node.rs` 中新增默认实现：

```rust
/// 遍历目录项，对每个 entry 调用回调（传入 name, inode_id, file_type）。
/// ext4 可 override 直接使用 dir_get_entries 里的 inode/type，
/// 避免默认实现中的 O(n) find() 调用。
fn list_dirents(&self, mut f: impl FnMut(&str, InodeId, FileType)) -> Result<(), SyscallErr> {
    // 默认 fallback：list() + find() + metadata()
    for name in self.list()? {
        if let Ok(child) = self.find(&name) {
            if let Ok(meta) = child.metadata() {
                f(&name, meta.inode_id, meta.file_type);
            }
        }
    }
    Ok(())
}
```

### 3.2 ext4 override list_dirents

在 `os/src/fs/ext4/ext4fs.rs` 中 override：

```rust
fn list_dirents(&self, mut f: impl FnMut(&str, InodeId, FileType)) -> Result<(), SyscallErr> {
    let entries = self.ext4fs.dir_get_entries(inode_num)?;
    for entry in &entries {
        let ft = dir_entry_type_to_vfs(entry.file_type);
        f(&entry.get_name(), entry.inode as InodeId, ft);
    }
    Ok(())
}
```

### 3.3 内核侧：变长 dirent64 打包

在 `os/src/fs/vfs/file.rs` 中实现 `get_dirent64()`：

```rust
pub fn get_dirent64(&self, buf: &mut [u8]) -> Result<usize, isize>;
```

每 record 格式（Linux `linux_dirent64`）：
```
d_ino:   u64 (8 bytes, LE)
d_off:   i64 (8 bytes, LE)
d_reclen: u16 (2 bytes, LE) = 对齐到 8 字节的总长度
d_type:  u8  (1 byte) — Linux 语义：从 record 末尾读取 (pos + d_reclen - 1)
d_name:  [u8] — null-terminated, \0 填充对齐
```

- 不分配 Vec，直接逐 record 写入用户 buffer
- buffer 放不下下一个完整 record 时停止
- 用 `self.offset` 跟踪目录字节偏移量
- 到目录末尾返回 0

### 3.4 修改 sys_getdents64

`os/src/syscall/fs.rs` 第 868 行：不再分配 `Vec<Dirent>`，改为调用 `file.get_dirent64(user_buf_writer)`。

### 3.5 更新 fs_test parser helper

- 新建 `count_dir_entries()` helper（已在 Phase 1.3）
- 更新 `test_getdents64()` 和 `test_stress_getdents()` 使用新 helper
- Parser 从 `pos + d_reclen - 1` 读取 `d_type`（Linux 语义）

### 验证

| 指标 | 目标 |
|------|------|
| getdents_1000_8k calls | ≤ 8 |
| getdents_1000_64k calls | ≤ 2 |
| getdents_returned_entries | == 1000 |
| getdents_invalid_reclen | == 0 |

---

## Phase 4：Dentry Cache + Inode Cache 增强

### 4.1 Dentry Cache（per-directory，带版本号失效）

在 `os/src/fs/ext4/layout.rs` 的 `Ext4OSInode` 中扩展：

```rust
// 已有: children: Mutex<BTreeMap<String, Arc<dyn IndexNode>>>

// 新增:
negative_dentry: Mutex<BTreeMap<String, ()>>,  // negative cache
dir_version: AtomicU64,                       // 目录修改版本号
negative_versions: Mutex<BTreeMap<String, u64>>, // negative entry 的创建版本
```

**Dentry cache 一致性维护**：
- `find()` 命中 → dentry_cache_hit++
- `find()` 未命中 → dentry_cache_miss++ → 查磁盘 → 插入 positive dentry
- negative lookup → 插入 negative dentry + 记录版本号
- negative hit 时校验版本：`current_version == entry_version`，不一致则失效
- `create()` / `symlink()` / `mkdir()` → `dir_version++`，清除同名 negative，插 positive
- `unlink()` / `rmdir()` → `dir_version++`，清除 positive，可选插 negative
- **跨目录 rename**：源目录删 old key + `dir_version++`，目标目录插 new key + `dir_version++`
- `rmdir()` 场景下至少保证不返回已删除节点

### 4.2 Inode Cache（扩展现有，不新建第二套缓存）

**不新建 `inode_cache.rs`**。扩展现有的 `Ext4FileSystem.inode_cache: BTreeMap<u32, Arc<Mutex<CachedExt4Inode>>>`（`ext4fs.rs:56`）。

增强现有缓存：
- `lookup(ino)` → 命中返回，未命中从磁盘加载（与现有逻辑一致）
- `mark_dirty(ino)` → 标记脏（已有 `CachedExt4Inode` dirty 字段）
- `flush_inode(ino)` → **改为写入 MetaBlockCache** 的对应 block+offset，不直接 `write_block()`
- `invalidate(ino)` → unlink/rename 时从 cache 移除

### 4.3 VFS 层 inode_objects 与 ext4 层 inode_cache 的关系

- VFS 层 `inode_objects: BTreeMap<u32, Weak<dyn IndexNode>>`：管理 IndexNode Arc 生命周期，防止重复创建
- ext4 层 `inode_cache: BTreeMap<u32, Arc<Mutex<CachedExt4Inode>>>`：缓存磁盘 inode 数据
- **失效同步**：`unlink` 时同时从两层移除（ext4 层主动 invalidate，VFS 层随 Arc 自然释放）
- **避免内存泄漏**：ext4 层不用强 Arc 引用 IndexNode，仅缓存 `CachedExt4Inode`（磁盘数据）

### 验证
- `test_perf_repeated_lookup_cache`：dentry_cache_hit ≥ 90, negative_dentry_hit ≥ 90
- `test_perf_stat_like_1000_second`：dentry_cache_hit ≥ 3, inode_cache_hit ≥ 3

---

## Phase 5：Metadata Block Cache（带写顺序 + RMW 合并）

### 5.1 MetaBlockCache（以 block 为唯一合并点）

新增 `os/src/fs/ext4/meta_cache.rs`：

```rust
pub struct MetaBlockCache {
    blocks: Mutex<BTreeMap<usize, CachedBlock>>,
}

struct CachedBlock {
    data: Vec<u8>,
    dirty: bool,
    block_id: usize,
}
```

**核心接口**（RMW-safe）：
```rust
/// 获取 block 的 mutable 引用，用于 read-modify-write。
/// 多个调用者修改同一 block 的不同 offset 时不会互相覆盖。
fn with_block_mut(&self, block_id: usize, f: impl FnOnce(&mut [u8]));

/// 读取 block 数据（优先从 cache，miss 则从设备加载）
fn read_block_cached(&self, block_id: usize) -> Vec<u8>;

/// 立即刷单个 block
fn flush_block(&self, block_id: usize);
```

### 5.2 插入点 + 写顺序约束

所有直接 `block_device.write_block()` 调用替换为 `meta_cache.with_block_mut(block_id, |buf| ...)`。

**Flush 顺序**（`flush_all_dirty()` 必须遵守）：
1. **data blocks** — 数据块必须先于引用它们的元数据
2. **inode table blocks** — inode 必须先于目录项
3. **directory blocks** — 目录项更新
4. **bitmap blocks** (inode bitmap → block bitmap) — 位图更新
5. **group descriptor blocks** — 块组描述符
6. **superblock** (block 0) — 最后写 superblock

### 5.3 读路径

- `Block::load_offset()` → 先查 MetaBlockCache，命中直接返回，未命中从设备读取并**缓存**
- **重要**：只替换 write path 不够，读路径也必须先查 cache，否则会读到未 flush 的旧磁盘内容
- `Block::load_offset()` 需要能访问 MetaBlockCache → 通过 `Ext4FileSystem` 引用传入

### 5.4 同步 debug mode

- `SYNC_METADATA_MODE` 下：`mark_dirty` → 立即 `flush_block`
- 正常模式：`mark_dirty` 仅标记 dirty，由 `flush_all_dirty()` 统一写回
- Flush 触发时机：`sync()`/`fsync()`/`unmount()`/cache 容量阈值

### 5.5 Eviction 策略（防止 OOM）

- 缓存容量上限（如 256 个 block = 1MB）
- 超过上限时 evict clean blocks（LRU）
- 不 evict dirty blocks（必须先 flush）
- 容量统计导出到 counter

### 验证
- 原有 51 tests 全部通过（正确性不受影响）
- `perf_symlink_batch_200` metadata_block_write_count 显著低于同步模式
- `audit_busybox_like_300_symlinks` block_write_count 显著降低

---

## Phase 6：Symlink Write Batching + Busybox 幂等

### 6.1 Symlink 创建接入 metadata batching

在 `create_fast_symlink()`（`file.rs:306`）中：
- 同一 inode table block 多个 inode 修改 → 合并为一次 write
- 同一 dir block 多次插入 → 合并为一次 write
- 批量完成后统一 flush

### 6.2 Busybox applet install 幂等

修改 `user/src/bin/initproc.rs` 的 `prepare_symlink()`：
```rust
// 改为:
let install_cmd = "busybox mkdir -p /bin; [ -f /bin/sh ] || busybox --install -s /bin\0";
```

或使用 marker 文件 `/bin/.busybox_installed`。

### 6.3 开启 profile_audit

修改 `run_profile_audit()` 调用 `profile_audit_real()` 以便保存 baseline。

### 验证
- `test_perf_symlink_batch_200` 正确性：200 symlinks + readlink + fstatat NOFOLLOW + open/read
- `profile_busybox_like_300_symlinks` counter dump 显示 write count 优化

---

## Phase 7：最终集成验证

### 7.1 双架构编译
```bash
# 在 Docker 容器内
cd os && make rv64-kernel-build-only
cd os && make la64-kernel-build-only
```

### 7.2 fs_test 运行（原有 51 tests + 新增 5 tests = 56 tests）
```bash
# fs_test 已预置在默认镜像中
# 使用 kernel-dev_kernel_run 运行
# 期望输出：=== FS Test: 56/56 passed ===
```

### 7.3 Profile audit 运行
```bash
# 修改 run_profile_audit() 调用 profile_audit_real()
# 编译后运行 fs_test
# 期望输出中包含所有 audit 子场景的 counter dump
```

### 7.4 优化前后对比表

| 指标 | 目标值 | 测量方式 |
|------|--------|---------|
| getdents_1000_8k calls | ≤ 8 | counter dump: `getdents_call_count` |
| getdents_1000_64k calls | ≤ 2 | counter dump: `getdents_call_count` |
| stat_like_1000_first cache_all_subfile_count | == 0 | counter dump |
| stat_like_1000_first cache_all_subfile_entries | == 0 | counter dump |
| stat_like_1000_second dentry_cache_hit | ≥ 3 | counter dump |
| repeated_lookup_existing dentry_cache_hit | ≥ 90 | counter dump |
| repeated_lookup_negative negative_dentry_hit | ≥ 90 | counter dump |
| symlink_batch_200_create symlink_fast_count | ≥ 200 | counter dump |
| symlink_batch_200_create metadata_flush_immediate_count | < symlink_create_count | counter dump |
| audit_busybox_like_300 block_write_count | < 600 | counter dump |
| open_large_dir_first cache_all_subfile_count | == 0 | counter dump |
| open_large_dir_negative negative_dentry_hit | ≥ 90 | counter dump |

---

## Part 11：禁止事项

1. ❌ 不要删除或跳过原有 51 个测试
2. ❌ 不要为了性能跳过 ext4 必要 metadata 更新
3. ❌ 不要让 stat/access/readlink 走 full `open()` 
4. ❌ 不要让 lookup 一个文件时加载整个目录所有 inode
5. ❌ 不要把 `cache_all_subfile()` 用作单文件 lookup 默认路径
6. ❌ 不要把 metadata block 直接混进 regular file page cache
7. ❌ 不要只改 initproc 或 fs_test 来掩盖内核问题
8. ❌ 不要只打印"优化完成"，必须用 `sys_ext4_counters` dump 给出指标
9. ❌ 不要破坏 symlink 语义：dangling/ELOOP/chain/unlink/NOLINK/NOFOLLOW

---

## 实施顺序

```
Phase 0 (基线) → Phase 1 (测试+counter) → Oracle 审查
→ Phase 2 (VFS lookup) → Oracle 审查
→ Phase 3 (getdents64) → Oracle 审查
→ Phase 4 (dentry/inode cache) → Oracle 审查
→ Phase 5 (metadata block cache) → Oracle 审查
→ Phase 6 (symlink batching) → Oracle 审查
→ Phase 7 (最终验证)
```
