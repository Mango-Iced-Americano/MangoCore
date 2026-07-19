# lwext4 + lwext4_rust 上游修复记录

**状态：进行中** | **分支：board-develop-combined** | **最新更新：2026-07-19**（创建于 2026-07-06）

本文档记录在 MangoCore 集成 lwext4 + lwext4_rust 过程中发现的 vendored 代码缺陷，用于后续向上游提 PR。

---

## 修复 1：`unsafe extern` blocks 兼容性

**文件**：`dependency/lwext4_rust/src/bindings.rs`（bindgen 自动生成）

**问题**：bindings.rs 使用 `unsafe extern "C" { ... }` 语法（218 处），该语法在 Rust < 1.82 中不可用。MangoCore la64 使用 nightly-2024-05-01，不支持此语法。

**症状**：`error: extern block cannot be declared unsafe` × 218

**修复**：
- 添加 `#![feature(unsafe_extern_blocks)]` 到 `lib.rs`
- 在 `blockdev.rs` 中将 `pub unsafe extern "C" fn` 改为 `pub extern "C" fn`，函数体包 `unsafe { }`

**上游 PR 建议**：根据 MSRV 选择方案 — 若需支持旧版 Rust，移除 `unsafe` 关键字并用 `unsafe { }` 包装函数体；若只需新版，文档注明 MSRV ≥ 1.82。

---

## 修复 2：`file_seek()` EOF clamp 导致 pwrite 写错位置

**文件**：`dependency/lwext4_rust/src/file.rs:172-179`

**问题**：
```rust
pub fn file_seek(&mut self, offset: i64, seek_type: u32) -> Result<usize, i32> {
    let mut offset = offset;
    let size = self.file_size() as i64;
    if offset > size {
        warn!("Seek beyond the end of the file");
        offset = size;  // BUG: pwrite beyond EOF writes to EOF instead
    }
    // ...
}
```
POSIX 允许 `lseek(fd, offset, SEEK_SET)` 到文件末尾之后（创建稀疏 hole），但此代码将 offset clamp 到文件大小。导致 `pwrite(fd, data, len, offset > size)` 实际写入 offset=size，数据错位。

**修复**：移除 EOF clamp，直接调用 `ext4_fseek()`。C 库应正确处理超 EOF seek。

**上游 PR 建议**：直接提交此修复，附 POSIX 引用和 QEMU 复现步骤。

---

## 修复 3：`lwext4_dir_entries()` 忽略 `ext4_dir_open()` 返回值

**文件**：`dependency/lwext4_rust/src/file.rs:391-435`

**问题**：
```rust
pub fn lwext4_dir_entries(&self) -> Result<...> {
    unsafe {
        ext4_dir_open(&mut d, c_path);  // 未检查返回值
        // ... ext4_dir_entry_next() 在未初始化的 ext4_dir 上操作
    }
}
```
若 `ext4_dir_open()` 失败（如 ENOENT），后续 `ext4_dir_entry_next()` 会在未初始化/零填充的 `ext4_dir` 结构体上操作，C 侧可能空指针解引用。

**修复**：检查 `ext4_dir_open()` 返回值，失败时返回错误。

**上游 PR 建议**：直接提交。

---

## 修复 4：多实例挂载 — 设备名/挂载点硬编码

**文件**：`dependency/lwext4_rust/src/blockdev.rs:73-86`

**问题**：
```rust
let c_name = CString::new("ext4_fs")...      // 设备名硬编码
let c_mountpoint = CString::new("/")...      // 挂载点硬编码
```
1. **设备名碰撞**：`ext4_device_register()` 要求设备名全局唯一。第二个 `Ext4BlockWrapper` 创建时返回 `EEXIST`。
2. **挂载点碰撞**：`ext4_mount(name, "/", ...)` 的挂载点 `"/"` 被多个实例共享。lwext4 C 库遇到重复挂载点直接返回 `EOK`，不实际挂载第二个盘。

**症状**：MangoCore 启动日志显示 `tools disk mounted at tools`，但 `/tools/bin` 实际为空（ENOENT），因为 lwext4 未真正挂载 tools 盘。

**修复**（MangoCore 侧）：
- 添加 `new_with_names(dev, dev_name, mount_point)` 构造函数
- `open_ext4rs()` 使用原子计数器生成唯一设备名 `"ext4_{id}"`，mount point 保持 `"/"`
- **注**：曾尝试使用唯一 mount point（`"/ext4_{id}"`），但需要全路径前缀翻译，已在 commit `bcd27725→回退` 后恢复为简单方案（唯一 dev_name + 统一 `"/"` mount point）

**上游 PR 建议**：建议 `Ext4BlockWrapper::new()` 接受可选 name/mount_point 参数，或提供 builder pattern。同时文档说明多实例用法。

---

## 修复 5：`Drop` 双重卸载 panic

**文件**：`dependency/lwext4_rust/src/blockdev.rs:381-387`

**问题**：
```rust
impl<K: KernelDevOp> Drop for Ext4BlockWrapper<K> {
    fn drop(&mut self) {
        self.lwext4_umount().unwrap();  // 若已手动 umount 则 panic
        // ...
    }
}
```
若 `Ext4FileSystem::on_umount()` 显式调用 `lwext4_umount()`，后续 `Drop` 再次调用会导致 panic。

**修复**：`.unwrap()` → `.ok()`，静默忽略重复卸载。

**上游 PR 建议**：添加 `mounted: bool` 字段，`Drop` 中检查后再卸载；或文档说明 `Drop` 会尝试卸载但失败不 panic。

---

## 修复 6：`strlen` 未定义（no_std 环境）

**文件**：`dependency/lwext4_rust/c/ulibc.c`

**问题**：Rust 的 `alloc::ffi::CStr` 和 `CString` 在 no_std 环境下需要 `strlen`，但 ulibc.c 未提供。

**修复**：添加 `__attribute__((weak)) size_t strlen(const char *s)` stub。

**上游 PR 建议**：ulibc.c 应包含完整的 no_std 字符串函数集合。

---

## 修复 7：`CStr::from_ptr` 类型随 Rust 版本变化

**文件**：MangoCore `os/src/fs/ext4_lwext4/layout.rs`（非 vendored，但值得记录）

**问题**：`CStr::from_ptr` 在不同 Rust nightly 版本中接受 `*const i8`（旧）或 `*const u8`（新）。使用 `core::ffi::c_char` 作为中间类型解决。

**修复**：`data as *const core::ffi::c_char`

---

## 修复 8：cmake 构建目录缓存污染（la64）

**文件**：MangoCore `os/make/la64.mk`、`os/make/rv64.mk`（非 vendored）

**问题**：rv64 和 la64 共用 `build_musl-generic` cmake 构建目录。rv64 编译后 cmake 缓存了 `riscv64-linux-gnu-gcc`，la64 编译时复用缓存导致生成 RISC-V 机器码的 loongarch64 .a 文件。

**修复**：分离构建目录 `build_lwext4-rv64` / `build_lwext4-la64`。

---

## 修复 10：稀疏文件 ftruncate 扩展 + ext4_fread 空洞读取

**文件**：`dependency/lwext4_rust/c/lwext4/src/ext4.c`

**问题**：
对 lwext4 的 C 库而言，`ext4_ftruncate()` 和 `ext4_fread()` 在稀疏文件语义上有两个独立但相关的缺陷：

1. **ftruncate 扩展不更新 inode 大小**（`ext4_ftruncate_no_lock` 第 1617-1621 行）：
   当 `file->fsize <= size`（文件扩展，未发生实际截断）时，原代码直接 `r = EOK; goto Finish;`，既不更新 `file->fsize`，也不通过 `ext4_inode_set_size()` 写回 inode 元数据。结果：
   - `ext4_fseek(file, 12288, SEEK_SET)` 检查 `offset > file->fsize` 失败，返回 `EINVAL`
   - 即使通过其他方式写入 page3（block 3 = 12 KB），`fsync` 后 inode 的 `i_size` 仍停留在 4096

2. **ext4_fread 空洞读取缺陷**：
   lwext4 通过 `ext4_fs_get_inode_dblk_idx(ref, iblock, &fblock, true)` 支持 "unwritten block" 语义（参数 `support_unwritten=true` 使 `fblock == 0` 时函数返回 `EOK` 而非报错）。然而在 `ext4_fread` 中，只有**首块非对齐分支**（第 1751-1760 行）检查了 `fblock != 0` 并填入零。其余两条数据路径均未处理空洞：
   - **对齐整块批量读取**（第 1774-1806 行）：当内层 while 循环收集的批次起始块 `fblock_start == 0` 时，仍调用 `ext4_blocks_get_direct(bdev, buf, 0, count)`，企图从物理块 0 读取数据
   - **末尾非完整块**（第 1808-1823 行）：未检查 `fblock` 是否为 0，直接计算 `off = 0 * block_size = 0` 调用 `ext4_block_readbytes`

**症状**（在 MangoCore 上观察）：
- 真实 QEMU 场景：cold reopen 后读空洞区域**偶然**正确（块设备缓存旧零值），但 `fsync` 后写 page3 返回 `EIO` — 根因是 ftruncate 扩展未更新 inode 大小，`ext4_fseek(12288)` 拒绝跳转到空洞区域
- `ext4_fread` 在遇到 `fblock == 0` 的批次时，行为未定义（取决于 block 0 的内容）

**修复**：

### fix 10a：`ext4_ftruncate_no_lock` — 稀疏扩展更新元数据

```c
/* 原代码 */
if (file->fsize <= size) {
    r = EOK;
    goto Finish;
}

/* 修复后 */
if (file->fsize <= size) {
    /* 稀疏扩展：仅更新 inode 大小元数据，不分配块 */
    file->fsize = size;
    ext4_inode_set_size(ref.inode, size);
    ref.dirty = true;
    /* ftruncate 扩展时保持 fpos 不变（截断时需 clamp） */
    r = EOK;
    goto Finish;
}
```

关键设计决策：
- **不预分配 block**：ftruncate 扩展不应强制分配数据块（POSIX 不要求、ext4 extent 语义允许 hole）。块分配由后续 `ext4_fwrite()` 按需触发
- **标记 inode 脏**：`ref.dirty = true` 确保 `ext4_fs_put_inode_ref()` 将更新后 `i_size` 写回磁盘
- **保持 fpos**：扩展时 `file->fpos` 不变；截断时（原有逻辑）若 `fpos > size` 则 clamp 到 `size`

### fix 10b：`ext4_fread` 对齐整块循环 — 空洞填零

```c
/* 原代码 */
r = ext4_blocks_get_direct(file->mp->fs.bdev, u8_buf,
                           fblock_start, fblock_count);

/* 修复后 */
if (fblock_start == 0) {
    /* 空洞：未映射块 → 填零 */
    memset(u8_buf, 0, block_size * fblock_count);
} else {
    r = ext4_blocks_get_direct(file->mp->fs.bdev, u8_buf,
                               fblock_start, fblock_count);
    if (r != EOK)
        goto Finish;
}
```

`fblock_start == 0` 条件同时覆盖两种场景：
- 批次首个块即为空洞（`fblock_start` 被设为 0）
- 连续空洞块被收集为一批（`fblock_count > 1`）

### fix 10c：`ext4_fread` 末尾非完整块 — 空洞填零

```c
/* 原代码 */
off = fblock * block_size;
r = ext4_block_readbytes(file->mp->fs.bdev, off, u8_buf, size);

/* 修复后 */
if (fblock != 0) {
    uint64_t off = fblock * block_size;
    r = ext4_block_readbytes(file->mp->fs.bdev, off, u8_buf, size);
    if (r != EOK)
        goto Finish;
} else {
    /* 空洞：填零 */
    memset(u8_buf, 0, size);
}
```

**兼容性**：
- 三个修复均仅修改扩展路径或错误路径，不影响正常（已分配块）读写路径
- fix 10a 不触发块分配，不会改变磁盘布局或导致空间耗尽
- fix 10b/10c 的 memset 路径仅在 `ext4_fs_get_inode_dblk_idx` 返回 `fblock == 0` 时触发，与上游 lwext4 的 `support_unwritten` 语义一致
- 已确认与修复 1-9 中的路径前缀翻译方案（Approach A）无冲突：本修复在 C 库内部，不受 mount point 命名影响

**上游位置**：
- 上游仓库：[gkostka/lwext4](https://github.com/gkostka/lwext4)
- 目标文件：`src/ext4.c`
- 函数：`ext4_ftruncate_no_lock()`（第 1604 行）、`ext4_fread()`（第 1672 行）

**上游提交建议**：3 个独立 commit：
1. `ext4_ftruncate_no_lock: update inode size on sparse extension` — 最小化变更，不涉及读路径
2. `ext4_fread: fill holes with zeros in aligned full-block path` — 读路径空洞修复
3. `ext4_fread: fill holes with zeros in trailing partial-block path` — 读路径空洞修复（第二个分支）

**待验证**：
- [ ] `make rv64-kernel-build-only` + `make la64-kernel-build-only`
- [ ] QEMU 下 cold reopen 后 `pread(fd, buf, 4096, 12288)` 返回零而非 EIO/EINVAL
- [ ] `ftruncate(fd, 16384)` 后 `fstat` 确认 `st_size == 16384`（无块分配）
- [ ] 非空洞（正常写入区域）读写不受影响
- [ ] lwext4 自带测试套件 `make test` 不新增 FAIL

---

## 待确认的上游问题

- [ ] lwext4_rust 的 `file.rs` 中 `flags_to_cstring()` 仅映射了部分 open flags（0, 2, 0x241, 0x242, 0x442），缺少 `O_APPEND` 单独映射等
- [ ] `Ext4File` 的 path-based API 无法表达 "open by inode" 语义，导致硬链接、open-unlink 场景有正确性风险
- [ ] bindings.rs 缺少 `ext4_chown`、`ext4_utime` 的传递性（需检查 C 库是否支持）

---

## 修复 9：多实例挂载 — 路径前缀翻译方案（替代撤回的修复 4）

**文件**：`os/src/fs/ext4_lwext4/ext4fs.rs`、`layout.rs`、`page_cache.rs`

**背景**：修复 4 尝试用唯一 mount point（`/ext4_{id}`）但因子模块调用点多、路径翻译复杂而回退（`bcd27725`），改回了「唯一 dev_name + 统一 `"/"` mount point」的简单方案。然而该方案在根目录 `/sdcard` 和 `/tools` 均为 ext4 时失效：lwext4 C 库发现 `"/"` 已被占用，第二次 `ext4_mount()` 直接返回 EOK 而不初始化第二个块设备。所有后续路径操作经 `ext4_get_mount(path)` 都落在第一个 ext4 实例上，导致 `/tools` 显示 `/sdcard` 的内容。

**根因分析**（`dependency/lwext4_rust/c/lwext4/src/ext4.c`）：

```c
// ext4_mount(): 重复 mount point → 直接返回 EOK
if (mp->name exists) return EOK;   // 第 392 行

// ext4_get_mount(): 按 mount point 前缀匹配，返回第一个
// 两个实例的 mount point 都是 "/"，所以永远返回第一个
```

**方案**：Approach A — 唯一内部 lwext4 mount point + Rust 适配层路径前缀翻译。

### 设计

给每个 `Ext4FileSystem` 实例分配唯一内部 mount point：

```rust
static NEXT_FS_ID: AtomicUsize = AtomicUsize::new(1);

struct Ext4FileSystem {
    // ... 现有字段 ...
    lw_dev_name: String,      // "e1"
    lw_mount_point: String,   // "/e1/"
}
```

`open_ext4rs()` 中使用 `new_with_names(dev, "e1", "/e1/")`，**不再传 `"/"`**。

### 路径翻译层

所有传给 lwext4 C API 的路径必须加前缀：

```rust
impl Ext4FileSystem {
    /// VFS 路径 → lwext4 内部路径
    fn lw_path(&self, vfs_path: &str) -> Result<String, SyscallErr> {
        if vfs_path == "/" {
            return Ok(self.lw_mount_point.clone());  // → "/e1/"
        }
        // "/bin/sh" → "/e1/bin/sh"
        Ok(format!("{}{}", self.lw_mount_point, &vfs_path[1..]))
    }

    fn lw_c_path(&self, vfs_path: &str) -> Result<CString, SyscallErr> {
        CString::new(self.lw_path(vfs_path)?).map_err(|_| SyscallErr::EINVAL)
    }
}
```

### 涉及修改的调用点

| 文件 | 方法 | 改动 |
|------|------|------|
| `ext4fs.rs` | `open_ext4rs()` | mount point 从 `"/"` → `"/e{N}/"` |
| `ext4fs.rs` | `super_block()` | `ext4_mount_point_stats()` 传入 `self.lw_mount_point` |
| `layout.rs` | `probe_inode_meta()` | `ext4_raw_inode_fill()` 路径加前缀 |
| `layout.rs` | `probe_type()` | `file_mode_get()` 路径加前缀 |
| `layout.rs` | `create()` / `mkdir()` / `unlink()` / `rmdir()` | 子路径加前缀 |
| `layout.rs` | `rename()` | 新旧路径均加前缀；跨 ext4 实例返回 `EXDEV` |
| `layout.rs` | `symlink()` | link 路径加前缀，target **不加**（target 是 VFS 语义） |
| `layout.rs` | `link()` / `mknod()` / `truncate()` | 路径加前缀 |
| `layout.rs` | `set_metadata()` / getxattr/setxattr 等 | 路径加前缀 |
| `layout.rs` | `logical_size_or_refresh()` / `read_at` (symlink) | 路径加前缀 |
| `layout.rs` | `list()` / `list_dirents()` | 目录路径加前缀；`lwext4_dir_entries()` 返回 basename，无需去前缀 |
| `page_cache.rs` | `LwExt4PageCacheBackend` | 存储 `lw_path` 字段，构造时翻译一次，I/O 使用翻译后路径 |

### 关键边界处理

1. **根目录**：`lw_path("/")` → `"/e1/"`，必须保留末尾 `/`，lwext4 按前缀匹配
2. **符号链接**：只给 symlink inode 路径加前缀，target 内容不加（target 是用户数据的 VFS 路径）
3. **跨 ext4 rename/link**：检查 `Arc::ptr_eq(&self.fs, &other.fs)`，不匹配返回 `EXDEV`
4. **readdir 去前缀**：不需要！lwext4 的 `ext4_generic_open2()` 内部已做 `path += strlen(mp->name)`，返回的是 basename
5. **文件句柄**：`file_open()` 用翻译后路径打开后，后续 read/write/seek/close 无需再翻译（file_desc 内已记录 mount point）

### 为何不用其他方案

| 方案 | 问题 |
|------|------|
| B: dev_name 路由 | 需改 C API 签名，破坏上游兼容性 |
| C: 同 mount point 按设备区分 | `ext4_mount_point_stats` / `ext4_umount` 等 API 只接受 mount_point，需大改 C 侧 |
| D: 传 block device 指针 | FFI 改动最大，与 lwext4 基于路径的设计冲突 |

Approach A 是 lwext4 的设计意图：内部唯一 mount point + 适配层路径前缀。

### 工作量

Medium，1-2 天。每个调用点改动量小，但涉及约 20 个方法需逐一适配。`page_cache.rs` 改动最简单（构造时翻译一次）。

### 验证

- [x] `make rv64-kernel-build-only` + `make la64-kernel-build-only`
- [x] QEMU 启动后 `ls /sdcard` 与 `ls /tools` 显示**不同**内容
- [x] `/tools` 中可正常读写文件
- [x] 基本文件操作 (open/read/write/unlink/rename) 在两个 ext4 实例上均正常

### 实现完成 (2026-07-12)

Implemented in commit implementing multi-ext4-instance mount isolation:
- `os/src/fs/ext4_lwext4/ext4fs.rs` — Added `lw_mount_point` field, `lw_path()` helper, unique mount points in `open_ext4rs()`, fixed `super_block()`, `get_inode_id()`, `probe_type()`, `probe_inode_meta()` to use translated paths
- `os/src/fs/ext4_lwext4/layout.rs` — All ~20 methods adapted: every `Ext4File::new()`, `file_open()`, `file_mode_get()`, `check_inode_exist()`, `dir_mk()`, `dir_rm()`, `file_rename()`, `dir_mv()`, `file_truncate()`, `ext4_readlink()`, `ext4_fsymlink()`, `ext4_flink()`, `ext4_mknod()`, `ext4_*xattr()`, and `ext4_owner_set()` call site now passes paths through `lw_path()` translation. Added cross-fs checks (`Arc::ptr_eq`) for `rename()` and `link()`. Symlink targets NOT translated (user-data, VFS-semantic).
- `os/src/fs/ext4_lwext4/page_cache.rs` — Added `lw_path` field, pre-computed at construction time; all `Ext4File` operations use translated path
- rv64 compile: zero errors from ext4_lwext4 files ✅
- la64: toolchain corrupted (pre-existing env issue, not code-related)

---

## 修复 11：目录项 file type 不能覆盖真实 inode mode

**文件**：`dependency/lwext4_rust/c/lwext4/src/ext4.c`

**问题**：启用 `INCOMPAT_FILETYPE` 后，`ext4_generic_open2()` 把目录项 file type 当作
路径类型真值；`EXT4_DE_UNKNOWN` 会被映射为 regular，更隐蔽的是旧卷还可能出现
“dentry=regular、inode=symlink”这类非 unknown 冲突。此时 `lstat`/`test -L` 与
`readlink` 可得出矛盾结果，迁移器无法安全删除旧链接。

**修复**：找到每个路径分量后，复用遍历/open 本来就必须加载的 child inode，并通过
`ext4_inode_type()` 得到权威类型；中间分量必须为目录，目标若指定具体类型则必须与
inode mode 一致。目录项 file type 只保留为提示，不再参与语义判定。健康路径没有额外
inode get/put，错误或损坏目录项路径反而更早 fail closed。

**上游 PR 建议**：提交最小 C 修复，并同时增加两类 fixture：FILETYPE 已启用但单个
dentry type 为 unknown，以及 dentry concrete type 与 inode mode 冲突。覆盖中间目录、
最终 symlink/readlink 和引用释放失败路径。

## 修复 12：`ext4_fsymlink()` 覆盖已存在的同类型 symlink

**文件**：`os/src/fs/ext4_lwext4/layout.rs`（适配层修复）

**问题**：底层 `ext4_fsymlink()` 经 open 路径可重写已存在的同类型 symlink，而 POSIX
symlink create 必须是排他的，目标存在应返回 `EEXIST`。这还会掩盖旧卷中的重复目录项。

**修复**：在 Rust 适配层的 lwext4 全局锁内先 probe 目标，存在立即返回 `EEXIST`，不存在
才调用 C API，使检查与创建相对于本内核所有 lwext4 操作原子。RV64/LA64 新回归均 9/9。

**上游 PR 建议**：C API 本身应提供 create-exclusive 语义并用目录锁保证原子性；在此之前
保留适配层门禁，不能依赖调用方先 unlink。
