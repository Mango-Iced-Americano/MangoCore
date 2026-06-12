# ext4 rename panic: name.len() = 4097 (max 255)

## 现象

LTP `rename10` 测试时内核 panic：

```
[kernel] panicked at 'range end index 4097 out of range for slice of length 255',
  src/fs/ext4/direntry.rs:207:18
--- SYSCTX ---
syscall: renameat2
```

## 根因分析

`Ext4DirEntry::write_entry()` 在 `direntry.rs:207` 执行：

```rust
self.name[..name.len()].copy_from_slice(name.as_bytes());
```

其中 `name.len() = 4097`，但 `self.name` 是 `[u8; 255]`（ext4 文件名最大 255 字节）。

`4097 = PAGE_SIZE + 1`，不是随机值，疑似目录块解析或 C string 终止符问题。

## 调用链

```
renameat2
  → ext4 rename (ext4fs.rs:1160)
    → dir_add_entry (direntry.rs:519)
      → try_insert_to_existing_block (direntry.rs:621)
        → write_entry(name=???, ...)  ← name 被污染
          → panic!
```

## 状态

- **严重度**：High（内核 panic）
- **优先级**：Medium（不影响基本功能，`rename10` 属于 LTP 压力测试）
- **关联**：无（预置 ext4 bug，与 I/O chunking 无关）
- **首次发现**：2026-06-12
- **状态**：Open
