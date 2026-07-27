---
title: "文件映射缺页与 PageCache 交互"
category: mm
status: stable
author: MangoCore Team
last_update: 2026-07-27
tags: [mm, mmap, filemap, page-cache, tlb-batch]
---

# 文件映射缺页与 PageCache 交互

## 1. 源码位置

文件 mmap 的缺页处理位于 `os/src/mm/filemap.rs`，由 `os/src/mm/page_fault.rs` 调用。

| 源码 | 函数/对象 | 场景 |
|------|-----------|------|
| `os/src/mm/filemap.rs` | `filemap_read_fault()` | 文件映射首次读或执行 |
| `os/src/mm/filemap.rs` | `filemap_private_fault()` | 文件私有映射首次写 |
| `os/src/mm/filemap.rs` | `filemap_shared_write_fault()` | 文件共享映射首次写 |
| `os/src/mm/filemap.rs` | `check_within_file()` | 检查 fault 偏移是否在文件大小 round-up 范围内 |
| `os/src/mm/filemap.rs` | `zero_tail()` | 清零最后一页 EOF 之后的字节 |
| `os/src/mm/filemap.rs` | `verify_filemap_fault()` | 校验 VMA resident frame 与 PTE 一致 |
| `os/src/mm/page_fault.rs` | `FaultAction::FileBacked*` | 将缺页分类派发到 filemap |
| `os/src/fs/` | `PageCache` / inode page cache 接口 | 提供文件页缓存 frame |

文件 mmap 的 page cache 对象来自 VFS inode：

```rust
let pc = inode.ensure_page_cache()
    .ok_or(MemoryError::BackingStoreFailure)?;
```

PageCache 本身属于文件系统层，MM 层只通过 inode 的 page cache 接口获取读页或写页 frame。

## 2. 调用关系

```
do_page_fault()
  └── page_fault::handle_page_fault()
        ├── FileBackedRead
        │     └── filemap_read_fault()
        ├── FileBackedWrite
        │     └── filemap_private_fault()
        └── FileBackedSharedWrite
              └── filemap_shared_write_fault()
```

分类依据来自 `Vma`：

| 判断 | 来源 |
|------|------|
| 是否文件映射 | `area.vm_kind() == VmAreaKind::FileBacked` |
| private/shared | `area.vm_mapping()` |
| fault 类型 | `FaultAccess::Load/Store/Execute` |

## 3. 文件偏移计算

文件映射 VMA 保存起始文件偏移：

```rust
pub map_file_offset: usize
```

缺页时，`area.vm_file_offset(ctx.vpn)` 根据 fault VPN 与 VMA 起始 VPN 的差计算实际文件偏移。

```
file_offset =
  area.map_file_offset
  + (ctx.vpn - area.vm_start()) * PAGE_SIZE
```

VMA 分裂时，`Vma::into_two()` 会为第二段调整 `map_file_offset`。因此 `filemap.rs` 不需要知道 VMA 曾经是否被 `mprotect/munmap/madvise` 分裂。

## 4. EOF 边界

`check_within_file(inode, file_offset)` 获取 inode metadata 中的 size，并使用 `round_up_page(file_size)` 作为最大可 fault 范围：

```rust
if file_offset >= round_up_page(file_size) {
    return Err(MemoryError::BeyondEOF);
}
```

含义：

| fault 位置 | 行为 |
|------------|------|
| 小于文件大小 | 读取真实文件内容 |
| 在最后一页 EOF 之后但仍在 round-up 页内 | 允许映射，尾部填零 |
| 大于等于 round-up 文件大小 | `BeyondEOF` |

`zero_tail(file_size, file_offset, buf)` 会把最后一页文件有效内容之后的部分清零。

## 5. PageCache 错误映射

PageCache 接口返回 `SyscallErr`，MM 层转换为 `MemoryError`：

```rust
fn map_pc_error(e: SyscallErr) -> MemoryError {
    match e {
        SyscallErr::ENOMEM => MemoryError::OutOfMemory,
        SyscallErr::EIO => MemoryError::BackingStoreFailure,
        _ => MemoryError::BackingStoreFailure,
    }
}
```

这使缺页路径能统一交给 `memory_error_to_errno()` 或 trap 信号处理，而不在 filemap 内直接返回 syscall errno。

## 6. 读缺页路径

`filemap_read_fault()` 用于读或执行：

```text
filemap_read_fault(area, page_table, ctx)
  ├── inode = area.vm_file()
  ├── file_offset = area.vm_file_offset(ctx.vpn)
  ├── file_size = check_within_file()
  ├── pc = inode.ensure_page_cache()
  ├── cache_frame = pc.frame_for_read(page_index)
  ├── zero_tail(file_size, file_offset, cache_frame)
  ├── map_perm = area.vm_perm()，若含 W 则去掉 W
  ├── area.inner.alloc_in_memory(ctx.vpn, cache_frame)
  ├── UserMapper::map_user_page(ctx.vpn, cache_ppn, map_perm)
  └── verify_filemap_fault()
```

读缺页不复制文件页。VMA resident frame 指向 page cache frame。

## 7. 为什么读缺页要清 W

如果 VMA 权限包含 W，读缺页仍会映射为只读：

```rust
let map_perm = if area.vm_perm().contains(MapPermission::W) {
    area.vm_perm().difference(MapPermission::W)
} else {
    area.vm_perm()
};
```

原因分两类：

| 映射 | 后续 store 需要的行为 |
|------|-----------------------|
| `MAP_PRIVATE` | 触发 COW，复制出私有页 |
| `MAP_SHARED` | 触发 shared write fault，先标脏 page cache |

如果读缺页直接保留 W，后续写会绕过缺页处理，private/shared 语义都会被破坏。

## 8. private 写缺页

`filemap_private_fault()` 用于文件私有映射首次写：

```text
filemap_private_fault()
  ├── 定位 inode 和 file_offset
  ├── 检查 EOF
  ├── pc.frame_for_read(page_index)
  ├── area.map_one_zeroed_unchecked()
  ├── 从 cache_frame 复制整页到 private frame
  ├── zero_tail()
  └── verify_filemap_fault()
```

该路径会分配新的匿名物理页。写入 private mmap 不会污染 page cache，也不会写回原文件。

## 9. shared 写缺页

`filemap_shared_write_fault()` 用于文件共享映射首次写：

```text
filemap_shared_write_fault()
  ├── 定位 inode 和 file_offset
  ├── 检查 EOF
  ├── pc.frame_for_write(page_index)
  ├── area.inner.alloc_in_memory(ctx.vpn, cache_frame)
  ├── UserMapper::new(batch).map_user_page(..., area.vm_perm())
  └── verify_filemap_fault()
```

`frame_for_write()` 是关键差异。它不仅取得 page cache frame，还把该页放入可写/dirty 路径，使文件系统后续能回写或处理脏页。

## 10. restore_shared_write

还有一种 shared 写缺页不是“首次映射”，而是“读缺页已映射为只读后首次 store”。该路径在 `page_fault.rs::restore_shared_write()`：

1. 如果是 file-backed shared，先调用 `pc.frame_for_write(page_index)`。
2. 通过 `UserMapper::new(batch).set_user_flags(ctx.vpn, area.vm_perm())` 恢复 VMA 的 W 权限。
3. 翻译 PPN 并返回 fault 物理地址。

这与 `filemap_shared_write_fault()` 的区别是：前者 PTE 已存在，后者 PTE 不存在。

## 11. verify_filemap_fault

所有 filemap fault 成功后都会调用 `verify_filemap_fault()`：

```rust
let mapped_ppn = UserMapper::new(batch)
    .translate(ctx.vpn)
    .ok_or(MemoryError::NotMapped)?;
if mapped_ppn != expected_ppn {
    return Err(MemoryError::BackingStoreFailure);
}
```

它检查两个事实：

1. `VmPageStore` 中必须有 resident frame。
2. PTE 指向的 PPN 必须等于预期 frame。

该校验可以及时发现“VMA 元数据和页表不一致”的问题。

## 12. 与 mincore 的关系

`VmaSet::mincore_range()` 判断文件页 resident 时，不只看 PTE：

```rust
page_table.is_mapped(cursor) || file_backed_page_resident(area, cursor)
```

`file_backed_page_resident()` 会查询 inode page cache 是否包含对应 page index。这样文件页即使尚未安装到当前进程 PTE，只要 page cache 已有该页，mincore 也能返回 resident。

## 13. 与 PageCache 状态机的边界

文件系统文档描述 PageCache 的 Loading、UpToDate、Dirty、Writeback 等状态。MM 层只依赖两个抽象操作：

| PageCache 操作 | MM 语义 |
|----------------|---------|
| `frame_for_read(index)` | 获取可读缓存页，必要时从文件加载 |
| `frame_for_write(index)` | 获取可写缓存页，并进入 dirty/write 路径 |

MM 层不直接操作块设备，也不决定脏页何时回写。

## 14. 错误与信号边界

`filemap.rs` 返回 `MemoryError`。上层可能有两种处理：

| 上层 | 结果 |
|------|------|
| trap 缺页 | 转换为用户信号或终止流程 |
| uaccess fault-in | 转换为 syscall errno |

`BeyondEOF` 是文件 mmap 的特殊错误：它表示 VMA 范围内的 fault 超过文件 round-up 大小。具体是否表现为 `SIGBUS` 取决于 trap 处理层。

file-backed mmap 的关键是区分“文件页缓存”和“进程私有页”。`MAP_SHARED` 读写最终都围绕 inode PageCache，写 fault 必须把 PageCache 页置 dirty；`MAP_PRIVATE` 读可以复用 PageCache 内容，但写 fault 要复制到进程私有 frame，后续写入不应污染文件缓存。最后一页跨 EOF 时还要清零尾部，避免把旧页内容暴露给用户。

调试文件映射时先确认 fault 类型：读/执行 fault 应进入 `filemap_read_fault()`；shared 写 fault 应进入 `filemap_shared_write_fault()`；private 写 fault 应进入 private frame 构造路径。若 shared mmap 写后文件没有变化，看 dirty/writeback；若 private mmap 写回了文件，看是否错误调用了 `frame_for_write()`。

## 15. 调试核对点

| 现象 | 检查 |
|------|------|
| mmap 文件最后一页出现旧数据 | `zero_tail()` 是否对 cache/private frame 执行 |
| MAP_PRIVATE 写回了文件 | private 写缺页是否错误使用 `frame_for_write()` |
| MAP_SHARED 写不回文件 | store fault 是否经过 `frame_for_write()` |
| mincore 返回 0 但 page cache 已有页 | `file_backed_page_resident()` 的 offset/page_index 计算 |
| 文件 VMA 分裂后读错位置 | `Vma::into_two()` 是否正确调整 `map_file_offset` |
