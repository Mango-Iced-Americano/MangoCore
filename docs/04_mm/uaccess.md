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
| `copy_from_user/copy_to_user` | 在 VM 映射同步下完成对象/数组拷贝 |
| `fault_in_user_range()` | 外部副作用前的用户区间预 fault，不替代真正 copy |
| `user_accessible_len()` | 非 faulting 可访问长度探测 |

## 2. 核心约束

`uaccess` 的核心安全约束有四条：

1. 地址范围必须在 `USER_VA_END` 以下。
2. faulting 用户访问只允许当前任务的 user token。
3. 翻译过程会触发缺页，并在成功后检查 PTE 权限。
4. 标量、数组和字符串 copy 的权限检查、物理地址取得和实际访问
   必须处于同一次 VM 锁持有期。

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

这里的 `'static mut` 是尚待消除的历史接口，不是多核安全保证。翻译 helper 返回后已经释放
VM 锁，另一 CPU 可以执行 fork/CoW、`mprotect` 或 `munmap`，使切片指向旧页或不再满足原
权限；Rust 引用本身还携带独占性假设。因此调用方即使只在一次 syscall 内短期使用，也不能
据此证明与并发地址空间修改安全。B57 只收口固定大小 copy；这些 buffer 接口属于 B58 范围。

跨页翻译还有一个重要语义：它会逐页 fault-in。前半部分已经成功翻译、后半部分遇到坏地址时，helper 返回错误；具体 syscall 需要决定是否允许部分完成。文件读写路径通过先探测关键地址和按 chunk 复制，尽量避免已经消费文件数据后才发现用户 buffer 后半段不可写。

## 7. 单页 fast path

`UserBufferReader::new()` 和 `UserBufferWriter::new()` 优先尝试单页 fast path：

```rust
translate_single_page_user_bytes(token, ptr, len, UserAccess::Read)
```

如果 buffer 完全落在同一页且权限满足，就构造 `UserBuffer::single(slice)`；否则退回跨页 `translated_byte_buffer()`。

这减少了常见小 read/write 的 `Vec` 分配。

该 fast path 同样返回物理页切片，尚不具备 B57 固定大小 copy 的 VM 同步保证。不能把“单页”
误解为“不会被并发重映射”。

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

## 9. 固定大小 copy 的 SMP 边界

B57 删除了 `translated_ref<T>()`、`translated_refmut<T>()` 和
`translated_ref_write<T>()`。旧接口先翻译用户 VA、释放 VM 锁，再返回可逃逸的 Rust 引用；
“对象不跨页”只能解决表示问题，不能阻止另一 CPU 在引用使用前修改映射。

现在 `copy_from_user()`、`copy_to_user()` 及其 array 版本统一经过 `copy_user_bytes()`：

```text
检查用户范围并取得当前 AddressSpace Arc
  -> 按用户页计算 chunk
  -> AddressSpace::write（取得 VM 锁）
       -> fault_in_user_va（缺页 + 权限后验检查）
       -> 取得 direct-map raw pointer
       -> 在锁内立即复制当前 chunk
  -> 解锁后执行本轮 MmuGather/TLB flush
  -> 处理下一页
```

这样同一页上的 fork/CoW、`mprotect`、`munmap` 与 copy 形成明确先后关系。raw pointer 不会
跨 closure 泄漏，也不伪造 `&'static mut T` 的永久独占性。

跨页 copy 不承诺事务原子性：另一 CPU 可以在两个 chunk 之间修改后续页；若后续 fault 或
权限检查失败，helper 返回 `EFAULT`，前面页的字节可能已经完成复制。调用方不得把
`Result::Err` 理解为“一个字节都没动”。

## 10. translated_str 的 SMP 边界

`translated_str(token, ptr)` 从用户空间读取 NUL 结尾字符串：

1. 从 `ptr` 开始逐页进入 `copy_user_bytes()`。
2. 在 VM 锁内 fault-in、做权限后验并复制到 4 KiB 内核 scratch。
3. 释放 VM 锁后扫描 NUL、执行 ASCII 快路径或按字节追加，不把堆分配带入锁内。
4. 长度达到 `MAX_BUFFER_SIZE`、地址递增溢出或后续页失效时返回 `EFAULT`。

这条路径不再返回或消费锁外物理页 slice。每页 scratch 会在下一页前覆盖，
字符串结果始终由内核 `String` 所有。

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
| `read_at/write_at` | 从逻辑偏移开始拷贝 |
| `clear/fill_at` | 清零或填充逻辑区间 |

跨页时，逻辑连续 buffer 被拆成多段物理页切片；read/write 会按顺序复制。
旧 Index/IndexMut/IntoIterator 实现没有生产调用方，B58 已删除，避免继续扩大可逃逸视图的 API 面。

这仍是未完成的 SMP 边界：`UserBuffer` 构造完成后 VM 锁已释放，所以 FS/网络层的
后续 read/write 仍可与并发 CoW、mprotect 或 munmap 竞争。B58 只把其他原始绕过路径
收回这一个核心；下一节点需改为 VA-backed 区间，并让实际 read/write 逐页在 VM 锁内完成。

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

syscall 层不应直接把用户地址 cast 成内核引用。进入 faultable copy 前还应释放 fd table、
task inner、file-private 等普通锁；`ioctl` 的固定对象路径已经遵循这一边界。该要求是调用方
锁序约束，不由 `copy_user_bytes()` 自动保证；SysV IPC 等既有路径仍需在后续共享子系统审计
中逐项核对。

## 14. 错误码边界

| 场景 | 错误 |
|------|------|
| 用户对象指针为 null | `EFAULT` |
| optional 指针为 null 且无写入值 | 成功 |
| buffer 超过 8 MiB | `EFAULT` |
| iovcnt 超过 1024 | `EINVAL` |
| iovec 总长溢出 | `EINVAL` |
| token 不是当前任务 token | `EFAULT` |
| fault-in 后权限不满足 | `EFAULT` |
| 跨页 copy 的后续页失效 | `EFAULT`，此前 chunk 可能已经完成 |

## 15. 调试核对点

| 现象 | 检查 |
|------|------|
| read 写用户 buffer 返回 EFAULT | 传入 access 是否应为 `Write` |
| write 读取用户 buffer 触发 COW | 用户 buffer 所在页是否私有可写且被错误当成 write access |
| readv/writev 处理过长 iovec | `MAX_IOVEC_COUNT` 与 `total_cap` |
| 路径读取卡住或越界 | `translated_str` 的 NUL 和 8 MiB 上限 |
| 非阻塞路径意外分配页 | 是否错误使用 faulting 翻译而非 `user_accessible_len()` |
| 多核 fork/unmap 与标量 copy 竞态 | 检查实际复制是否仍位于同一次 `AddressSpace::write` closure |
