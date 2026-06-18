# Task 6 — sync-like syscall audit

## 结论

当前内核只接通了 `fsync(82)`、`umount2(39)` 和 `msync(227)` 三个相关 syscall；`fsync` 和 `umount2` 都是假成功/不完整实现，`msync` 只做参数校验后返回成功。未发现 `fdatasync`、`sync`、`syncfs`、`sync_file_range` 的 syscall ID 或 dispatch。

## syscall 状态表

| syscall | ID | name mapping | dispatch | handler | 当前行为 | 目标任务 |
|---|---:|---|---|---|---|---|
| `umount2` | 39 | `syscall/mod.rs:31` | `syscall/mod.rs:191` | `syscall/fs.rs:1425-1440` | 参数解析后打印 fake implementation 并返回 SUCCESS | T8：接真实 `MountFS::umount()` + `on_umount()` |
| `fsync` | 82 | `syscall/mod.rs:57` | `syscall/mod.rs:243` | `syscall/fs.rs:1205-1213` | 只检查 fd 存在，成功直接返回 SUCCESS，不调用 `File::fsync()` | T7：fd → File::fsync() → IndexNode/FileSystem writeback |
| `msync` | 227 | `syscall/mod.rs:121` | `syscall/mod.rs:372` | `syscall/fs.rs:1851-1871` | 只检查地址/flags/VM buffer，然后 SUCCESS；无实际 mmap writeback | 后续可作为非本轮目标或接 PageCache/VMA 后再实现 |
| `fdatasync` | 未定义 | 无 | 无 | 无 | 未实现 | T7：如测试需要，新增 ID/dispatch 并调用 `File::fdatasync()` |
| `sync` | 未定义 | 无 | 无 | 无 | 未实现 | T7：新增全局 flush，调用 `flush_all_page_caches()` 和 mounted FS `sync_fs()` |
| `syncfs` | 未定义 | 无 | 无 | 无 | 未实现 | 非必须；如实现，应按 fd 找 filesystem 后 `sync_fs()` |
| `sync_file_range` | 未定义 | 无 | 无 | 无 | 未实现 | 非必须；如实现，应走 PageCache range writeback |

## errno / 目标语义

- `fsync(valid_fd)`：调用 `File::fsync()`，成功返回 0；写回失败返回负 errno。
- `fsync(invalid_fd)`：保持当前 fd table 错误路径，返回 `-EBADF` 等对应 fd table 错误。
- `fdatasync(valid_fd)`：调用 `File::fdatasync()`；本内核短期可与 `fsync` 等价或只刷数据。
- `sync()`：Linux 语义通常不向调用者报告单个 I/O 错误；本内核应至少调用全局 PageCache/FileSystem flush，失败时记录日志，返回策略由 T7 定。
- `umount2(valid_target, flags)`：走 VFS lookup/mount table，成功时调用 `MountFS::umount()`，从而触发 `on_umount()`。
- `umount2(invalid_target)`：返回负 errno，不能假成功。
- `umount2(busy_target)`：返回 `-EBUSY`。

## 风险

1. `fsync` 当前假成功会让用户态以为数据落盘，但 DirtyBlockDevice 可能仍只在内存 dirty map 中。
2. `umount2` 当前 fake success 会绕过 `MountFS::umount()`，即使 `on_umount()` 已经具备 sync hook 也不会被调用。
3. `msync` 当前对 mmap-backed writeback 不做实际工作；如果后续 MAP_SHARED 文件页进入 PageCache，需要补 VMA/PageCache writeback。

## T7/T8 直接建议

- T7 修改 `sys_fsync(fd)`：获取 fd 后调用 `file.fsync()`，错误转负 errno。
- T7 如新增 `fdatasync`/`sync`，必须同步更新 `syscall_id.rs`、`syscall_name()` 和 dispatch match。
- T8 修改 `sys_umount2`：解析 target 后走 VFS mount 查找/umount；不要只返回 SUCCESS。
- T7/T8 均不得识别 ext4 类型。
