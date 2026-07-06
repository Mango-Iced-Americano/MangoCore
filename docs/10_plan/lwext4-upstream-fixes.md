# lwext4 + lwext4_rust 上游修复记录

**状态：进行中** | **分支：refactor/ext4** | **日期：2026-07-06**

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
- `open_ext4rs()` 使用原子计数器生成唯一设备名和挂载点
- VFS 适配器在所有 lwext4 API 调用前自动添加挂载点前缀

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

## 待确认的上游问题

- [ ] lwext4_rust 的 `file.rs` 中 `flags_to_cstring()` 仅映射了部分 open flags（0, 2, 0x241, 0x242, 0x442），缺少 `O_APPEND` 单独映射等
- [ ] `Ext4File` 的 path-based API 无法表达 "open by inode" 语义，导致硬链接、open-unlink 场景有正确性风险
- [ ] bindings.rs 缺少 `ext4_chown`、`ext4_utime` 的传递性（需检查 C 库是否支持）
