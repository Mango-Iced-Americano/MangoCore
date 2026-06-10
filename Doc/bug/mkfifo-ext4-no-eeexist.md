# mkfifo → ext4 目录条目未持久化导致 linkat EEXIST 漏检

**发现日期**: 2026-06-10

**严重程度**: 中（影响 LTP linkat01/symlink01 等依赖 FIFO setup 的测试）

**状态**: Pending（需独立 follow-up）

## 现象

LTP linkat01 case 22: `linkat(olddirfd, "old", newdirfd, "new/existing_link", 0)` 期望返回 EEXIST，实际返回 0（成功创建硬链接）。同时 cleanup 阶段报错：

```
tst_rmdir: rmobj(/tmp/LTP_xxx) failed: Cannot open directory .../olddir/fifo; errno=20: ENOTDIR
```

## 根因

1. **LTP setup 用 `mkfifo()` 创建测试文件**：`mkfifo("new/existing_link", 0777)` 和 `mkfifo("olddir/fifo", 0777)`
2. **Mango 的 `sys_mknodat` → `create_with_data` 链路有缺陷**：
   - `sys_mknodat` 将 FIFO 类型和权限传入 `parent.create_with_data(&leaf, FileType::Pipe, perm, 0)`
   - `create_with_data` 在 ext4 上走到**默认实现**（`vfs/index_node.rs:163`），该实现丢弃 `_data` 参数直接调用 `create(name, file_type, mode)`
   - ext4 的 `create` 调用了 `Ext4FileSystem::create(parent, name, mode, 0, 0)` 但可能存在返回值处理或 inode 持久化不完整的问题
3. **结果**：FIFO 在运行时的目录项要么未被写入 ext4 磁盘目录，要么在后续测试用例中被覆盖/移除，导致 case 22 执行时 `list_dirents()` / `dir_find_entry()` / `parent_dir.find()` 均找不到 "existing_link"

## 尝试的修复（均失败，确认条目不在磁盘）

| 尝试 | 方法 | 结果 |
|------|------|------|
| 1 | `vfs_lookup(start, newpath, follow_final=true)` | 悬空 symlink 被跟随→ENOENT |
| 2 | `parent_dir.find(&leaf)` | ENOENT（条目不在 ext4 目录） |
| 3 | `list_dirents()` 遍历 | ENOENT（同上） |
| 4 | ext4 `dir_find_entry` 防御层 | ENOENT（同上） |

## 已应用的防御措施

- **VFS 层**：`sys_linkat` 中的 `list_dirents()` 遍历检查（`os/src/syscall/fs.rs`，约为 line 5258）
- **ext4 层**：`Ext4OSInode::link` 中添加 `dir_find_entry` 查重，命中返回 `SyscallErr::EEXIST`（`os/src/fs/ext4/ext4fs.rs`，约为 line 1212）

这两层防御在 ext4 目录条目正确持久的场景下有效。当前失效是因为底层条目本身不存在。

## 解决方向

1. **修复 ext4 的 `create_with_data` 实现**：为 `Ext4OSInode` 添加 `create_with_data` 重写，正确处理 FIFO/CHR/BLK/SOCK 的创建
2. **修复 `sys_mknodat` 中的 MountFS 链路**：确保 `create_with_data` 经过 MountFS 后正确调用到 ext4 的 `create`
3. **验证**：修复后 linkat01 的 cleanup 不应再报 `ENOTDIR on olddir/fifo`，case 22 应返回 EEXIST

## 影响范围

- LTP linkat01 case 22 (TFAIL)
- LTP symlink01 中可能的 FIFO 依赖 case
- 所有依赖 `mkfifo()` 创建持久 FIFO 的 LTP 测试

## 相关文件

- `os/src/syscall/fs.rs` — `sys_mknodat` (line ~2450)
- `os/src/fs/vfs/index_node.rs` — 默认 `create_with_data` (line 163)
- `os/src/fs/ext4/ext4fs.rs` — `Ext4OSInode::create` (line 1024), `Ext4OSInode::link` (line 1207)
- `os/src/fs/vfs/mount.rs` — `MountFSInode::create` (line 505)
