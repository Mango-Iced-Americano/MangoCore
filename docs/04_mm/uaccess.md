---
title: "用户地址访问与 UserBuffer"
category: mm
status: stable
author: MangoCore Team
last_update: 2026-08-01
tags: [mm, uaccess, user-pointer, iovec]
---

# 用户地址访问与 UserBuffer

## 1. 源码位置

用户地址访问封装位于 `os/src/mm/uaccess.rs`。它为 syscall 层提供安全的用户指针读取、写入、字符串读取和 iovec 翻译。

| 源码 | 作用 |
|------|------|
| `os/src/mm/uaccess.rs` | 用户指针封装、buffer/iovec 翻译、copy_from/to_user |
| `os/src/mm/address_space.rs` | `fault_in_user_va()` 和后置权限校验 |
| `os/src/mm/page_fault.rs` | fault-in 触发后的缺页动作分类 |
| `os/src/syscall/fs.rs` | read/write/readv/writev 对用户 buffer 的使用 |
| `os/src/syscall/process/mm.rs` | process_vm_readv/writev、mmap 相关用户结构 |

主要导出：

| 类型/函数 | 用途 |
|-----------|------|
| `UserPtr<T>` | 只读用户对象指针 |
| `UserPtrMut<T>` | 可写用户对象指针 |
| `UserSlice<T>` | 用户数组 |
| `UserCString` | 用户 C 字符串 |
| `UserBufferReader` | 从用户 buffer 读取 |
| `UserBufferWriter` | 向用户 buffer 写入 |
| `UserIoVec` | 读取并管理用户 iovec |
| `translated_byte_buffer()` | 跨页用户 buffer 翻译 |
| `translated_str()` | 读取 NUL 结尾字符串 |
| `copy_from_user/copy_to_user` | 对象/数组拷贝基础函数 |
| `user_accessible_len()` | 非 faulting 可访问长度探测 |

## 2. 核心约束

`uaccess` 的核心安全约束有三条：

1. 地址范围必须在 `USER_VA_END` 以下。
2. faulting 用户访问只允许当前任务的 user token。
3. 翻译过程会触发缺页，并在成功后检查 PTE 权限。

`current_user_vm(token)` 明确检查：

```rust
if crate::task::current_user_token() != token {
    return Err(EFAULT);
}
```

因此，faulting uaccess 不支持对任意非当前进程地址空间执行。

## 3. 长度限制

文件内定义：

```rust
const MAX_BUFFER_SIZE: usize = 1024 * 1024 * 8;
const MAX_IOVEC_COUNT: usize = 1024;
```

含义：

| 限制 | 错误 |
|------|------|
| 单次用户 buffer 翻译超过 8 MiB | `EFAULT` |
| iovec 数量超过 1024 | `EINVAL` |
| iovec 总长度溢出或超过 `isize::MAX` | `EINVAL` |

这些限制用于防止 syscall 通过巨大用户 buffer 诱导内核分配大量 `Vec<&mut [u8]>` 元数据。

## 4. 非 faulting 探测

`user_accessible_len(token, ptr, len, access)` 只检查已有 PTE：

```text
user_accessible_len()
  ├── checked ptr + len
  ├── uaccess_user_range_ok()
  ├── token 必须属于当前任务
  ├── PageTableImpl::from_token(token)
  └── 逐页 user_access_ok()
```

它不会：

| 不做的事 |
|----------|
| 分配匿名页 |
| 触发文件 mmap 读页 |
| 触发 COW |
| 触发 MAP_GROWSDOWN |
| 修改页表 |

该接口适合非阻塞 I/O 或分段 I/O 中预估当前可访问长度。

### 4.1 已映射页的 fault-in 快路径

`AddressSpace::fault_in_user_va()` 先以 `mapped_user_va()` 检查当前 PTE。对 Load 要求 `U+R`，对 Store 要求 `U+W`，并且物理地址必须仍在可分配 RAM 范围内；命中时直接返回物理地址，不进入缺页处理或输出 post-fault warning。

未映射页、权限不满足页、lazy/file-backed 缺页、COW/shared-write 和 grow-down 场景全部继续走原有慢路径。快路径不修改 PTE，因此不会替代或省略需要 PTE 更新的 TLB 刷新。

## 5. faulting 翻译

`translate_user_va_checked_with_vm()` 是 faulting 翻译核心：

```rust
match access {
    UserAccess::Read => fault_in_user_va_with_vm(vm, va, FaultAccess::Load),
    UserAccess::Write => fault_in_user_va_with_vm(vm, va, FaultAccess::Store),
    UserAccess::ReadWrite => {
        fault_in_user_va_with_vm(vm, va, FaultAccess::Load)?;
        fault_in_user_va_with_vm(vm, va, FaultAccess::Store)
    }
}
```

这意味着：

| UserAccess | 缺页动作 |
|------------|----------|
| `Read` | 可触发 lazy alloc、文件读缺页、执行读权限检查 |
| `Write` | 可触发 COW、shared write、匿名写缺页 |
| `ReadWrite` | 先读后写，确保同一对象两种权限均可用 |

## 6. 跨页 buffer 翻译

`translate_user_buffer_checked()` 将用户 buffer 切成多个物理页切片：

```text
translate_user_buffer_checked(token, ptr, len, access)
  ├── len <= MAX_BUFFER_SIZE
  ├── len == 0 -> empty Vec
  ├── ptr 非 null
  ├── check_user_range()
  ├── current_user_vm(token)
  └── while start < end:
        ├── translate_user_va_checked_with_vm()
        ├── 计算当前页可覆盖范围
        └── push ppn.get_bytes_array()[offset..end]
```

返回值是 `Vec<&'static mut [u8]>`。这些切片指向已经 fault-in 并通过权限检查的物理页。

这里返回 `'static` 切片并不表示用户页永久有效，而是内核通过物理页直接映射获得了可访问内存视图；调用者必须把它当作一次 syscall 内部的临时借用使用。uaccess 的安全边界依赖当前单核内核和调用路径不跨越会释放/重映射这些用户页的操作。长时间保存这些切片、把它们放入异步结构，都会破坏这个约定。

跨页翻译还有一个重要语义：它会逐页 fault-in。前半部分已经成功翻译、后半部分遇到坏地址时，helper 返回错误；具体 syscall 需要决定是否允许部分完成。文件读写路径通过先探测关键地址和按 chunk 复制，尽量避免已经消费文件数据后才发现用户 buffer 后半段不可写。

## 7. 单页 fast path

`UserBufferReader::new()` 和 `UserBufferWriter::new()` 优先尝试单页 fast path：

```rust
translate_single_page_user_bytes(token, ptr, len, UserAccess::Read)
```

如果 buffer 完全落在同一页且权限满足，就构造 `UserBuffer::single(slice)`；否则退回跨页 `translated_byte_buffer()`。

这减少了常见小 read/write 的 `Vec` 分配。

## 8. UserPtr 和 UserPtrMut

`UserPtr<T>`：

| 方法 | 行为 |
|------|------|
| `is_null()` | 判断空指针 |
| `addr()` | 返回地址 |
| `read(token)` | 空指针返回 `EFAULT`，否则读对象 |
| `read_optional(token)` | 空指针返回 `Ok(None)` |

`UserPtrMut<T>` 额外支持：

| 方法 | 行为 |
|------|------|
| `write(token, value)` | 空指针返回 `EFAULT`，否则写对象 |
| `write_optional(token, value)` | value 为 None 时允许不写 |

这些类型用于 syscall 参数中结构体、长度、返回值指针等固定大小对象。

## 9. translated_ref 系列

`translated_ref<T>()`、`translated_refmut<T>()`、`translated_ref_write<T>()` 要求对象不能跨页：

```rust
if va.floor() != last.floor() {
    return Err(EFAULT);
}
```

原因是它们返回单个 Rust 引用，不能自然表示跨页对象。跨页数据必须使用 byte buffer 或 array copy 接口。

权限对应：

| 函数 | UserAccess |
|------|------------|
| `translated_ref` | `Read` |
| `translated_refmut` | `ReadWrite` |
| `translated_ref_write` | `Write` |

## 10. translated_str

`translated_str(token, ptr)` 从用户空间读取 NUL 结尾字符串：

1. 从 `ptr` 开始逐页 fault-in。
2. 每页扫描 `0` 字节。
3. ASCII 快路径批量追加。
4. 非 ASCII 按字节转 `char` 追加。
5. 长度达到 `MAX_BUFFER_SIZE` 返回 `EFAULT`。
6. 地址递增溢出返回 `EFAULT`。

该函数用于路径名、exec 参数、环境变量、socket 选项中字符串等。

## 11. UserIoVec

`UserIoVec::read_user_iovecs(token, iov, iovcnt, total_cap)`：

```text
read_user_iovecs()
  ├── iovcnt <= MAX_IOVEC_COUNT
  ├── Vec<IOVec>::try_reserve(iovcnt)
  ├── copy_from_user_array()
  ├── 计算 total_len
  └── 保存 token/iovecs/total_len/total_cap
```

后续可构造：

| 方法 | 用途 |
|------|------|
| `reader_buffer()` | 将所有 iovec 翻译为读 buffer |
| `writer_buffer()` | 将所有 iovec 翻译为写 buffer |
| `reader_buffer_at(offset, len)` | 从逻辑偏移构造读 buffer |
| `writer_buffer_at(offset, len)` | 从逻辑偏移构造写 buffer |
| `accessible_len_at(offset, len, access)` | 非 faulting 探测指定逻辑范围 |

`total_cap` 用于限制本次实际构造的 buffer 长度，例如 readv/writev 与 socket I/O 的分段处理。

## 12. UserBuffer

`UserBuffer` 支持 Empty、Single 和 Multi 三种内部表示。它提供：

| 方法 | 语义 |
|------|------|
| `len()` | 逻辑总长度 |
| `read(dst)` | 从用户 buffer 读到内核 dst |
| `write(src)` | 从内核 src 写到用户 buffer |
| `write_cursor().write_from(src)` | 顺序写入连续产出的多个内核 chunk |
| iterator | 逐片段遍历 |

跨页时，逻辑连续 buffer 被拆成多段物理页切片；read/write 会按顺序复制。`UserBufferWriteCursor` 保存当前 segment index 与 segment 内偏移，适用于 PageCache 按文件页升序产出数据的场景；一个请求只前进遍历每个目标 segment 一次。它不替换随机访问的 `write_at(offset, src)`，后者仍服务于需要显式逻辑偏移的调用方。

`UserBufferWriter::new_writable_prefix()` 用一次当前 VM lock 构造连续的既有可写前缀。read/pread 的每个 chunk 先消费该前缀；第一个不可访问页才触发 Store fault-in。这样有效前缀保留 POSIX partial-read 结果，同时避免 `writable_len_for_read()` 与完整 Writer 初始化的重复遍历。

## 13. 与 syscall 层的配合

典型使用方式：

| syscall 场景 | uaccess 接口 |
|--------------|--------------|
| `read(fd, buf, count)` | `UserBufferWriter` 或 `translated_byte_buffer(..., Write)` |
| `write(fd, buf, count)` | `UserBufferReader` 或 `translated_byte_buffer(..., Read)` |
| `openat(path)` | `UserCString::read()` |
| `statx(buf)` | `UserPtrMut<T>::write()` |
| `readv/writev` | `UserIoVec` |
| `nanosleep(rem)` | `UserPtrMut<T>::write_optional()` |

syscall 层不应直接把用户地址 cast 成内核引用。

## 14. 错误码边界

| 场景 | 错误 |
|------|------|
| 用户对象指针为 null | `EFAULT` |
| optional 指针为 null 且无写入值 | 成功 |
| buffer 超过 8 MiB | `EFAULT` |
| iovcnt 超过 1024 | `EINVAL` |
| iovec 总长溢出 | `EINVAL` |
| token 不是当前任务 token | `EFAULT` |
| 对象跨页但使用 `translated_ref*` | `EFAULT` |
| fault-in 后权限不满足 | `EFAULT` |

## 15. 调试核对点

| 现象 | 检查 |
|------|------|
| read 写用户 buffer 返回 EFAULT | 传入 access 是否应为 `Write` |
| write 读取用户 buffer 触发 COW | 用户 buffer 所在页是否私有可写且被错误当成 write access |
| readv/writev 处理过长 iovec | `MAX_IOVEC_COUNT` 与 `total_cap` |
| 路径读取卡住或越界 | `translated_str` 的 NUL 和 8 MiB 上限 |
| 非阻塞路径意外分配页 | 是否错误使用 faulting 翻译而非 `user_accessible_len()` |
