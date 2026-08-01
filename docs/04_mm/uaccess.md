---
title: "用户地址访问与 UserBuffer"
category: mm
status: stable
author: MangoCore Team
last_update: 2026-08-01
tags: [mm, uaccess, user-pointer, iovec, smp]
---

# 用户地址访问与 UserBuffer

## 1. 设计目标

`os/src/mm/uaccess.rs` 是 syscall 与用户地址空间之间的唯一常规复制边界。B57—B59
完成后，所有生产 uaccess 对象只保存用户虚拟地址或内核所有的数据，不再向调用方返回
PA、direct-map pointer、`&'static T` 或 `&'static mut [u8]`。

核心约束如下：

1. faulting uaccess 只能访问当前任务的 user token；
2. 每个用户页都在当前 `AddressSpace` 锁内解析 PTE、检查权限并立即复制；
3. PA 和 raw pointer 只在该锁的 closure 内短暂存在；
4. 构造 `UserBuffer` 时的预 fault 不会固定映射，实际复制仍会重新校验；
5. VM 锁释放后才执行 `MmuGather` 产生的 TLB flush、远端 ack 或其它等待；
6. 跨页与 scatter I/O 允许部分完成，固定格式对象使用 exact helper。

## 2. 源码与接口

| 源码 | 作用 |
|------|------|
| `os/src/mm/uaccess.rs` | 用户指针、字符串、buffer/iovec 与 copy helper |
| `os/src/mm/address_space.rs` | 已有 PTE 解析、fault-in 和权限后验检查 |
| `os/src/mm/page_fault.rs` | lazy page、CoW、shared-write 等 fault 动作 |

主要导出如下：

| 类型/函数 | 用途 |
|-----------|------|
| `UserPtr<T>` / `UserPtrMut<T>` | 固定对象的只读/可写用户指针 |
| `UserSlice<T>` | 固定类型数组 |
| `UserCString` / `translated_str()` | NUL 结尾字符串的内核快照 |
| `UserBufferReader` / `UserBufferWriter` | 连续用户 buffer 的读写封装 |
| `UserIoVec` | 读取 iovec 描述符并构造 scatter `UserBuffer` |
| `copy_from_user` / `copy_to_user` | 固定对象的 exact copy |
| `fault_in_user_range()` | 外部副作用前的预 fault，不替代实际 copy |
| `user_accessible_len()` | 不触发 fault 的已有映射前缀探测 |

旧 `translated_byte_buffer()`、`translate_user_buffer_checked()`、单页 slice fast path 和
`Vec<&'static mut [u8]>` 已删除。

## 3. 地址、token 与长度

`check_user_range(ptr, len)` 只检查整数溢出和 `[ptr, ptr + len)` 是否位于
`USER_VA_END` 以下，不读取页表。真正的 R/W 权限由实际复制时的 PTE 检查决定。

faulting 入口通过 `current_user_vm(token)` 要求 token 属于当前任务：

```rust
if crate::task::current_user_token() != token {
    return Err(EFAULT);
}
```

因此跨进程访问不能复用普通 uaccess，必须使用有独立权限和目标 VM 生命周期协议的
`process_vm_readv/writev` 路径。

| 限制 | 值 | 失败语义 |
|------|----|----------|
| 单次用户 buffer | 8 MiB | `EFAULT` |
| iovec 数量 | 1024 | `EINVAL` |
| iovec 总长度 | 不超过 `isize::MAX` | `EINVAL` |

8 MiB 上限限制一次 fault/copy 工作量；B59 后连续 buffer 不再为了逐页表示分配 `Vec`。

## 4. PTE resolve-first

`AddressSpaceInner::fault_in_user_va()` 先调用无副作用的内部 resolver：

```text
resolve_user_va_inner(va, access)
  ├── translate_va
  ├── 验证 PA 属于可用 DRAM
  └── 验证 U + R/W/X 权限
```

若已有 PTE 满足权限，函数直接返回 PA；只有未映射或写权限尚未建立时才进入 page-fault
handler。这一点很重要：uaccess 是软件发起的复制，不是硬件真的报告了一次 fault。若每次
copy-to-user 都无条件进入 handler，已映射的 private/shared 页面可能被误判为 CoW 或
SharedWrite，并产生无意义的 PTE 修改和 TLB shootdown。

`resolve_user_va()` 是同一 resolver 的 nofault 包装，只接受当前已有且权限满足的 PTE，
不会分配页面、触发 CoW 或等待缺页 I/O。

## 5. 固定对象与字符串

固定对象、数组和字符串最终都进入逐页 copy：

```text
检查范围并取得当前 AddressSpace Arc
  -> 计算本页 chunk
  -> 取得 VM 写锁
       -> resolve 已有 PTE，必要时 fault-in
       -> 权限与物理范围后验检查
       -> direct-map raw copy
  -> 释放 VM 锁
  -> 在锁外完成可能的 TLB flush/ack
  -> 下一页
```

raw pointer 不会逃逸 closure，因此同一页上的 fork/CoW、`mprotect` 和 `munmap` 只能发生
在该 chunk 复制之前或之后。跨页 copy 不提供事务原子性：后续页失效时，前面的页可能已经
完成。

`translated_str()` 每页复制到 4 KiB 内核 scratch，释放 VM 锁后才扫描 NUL、扩容
`String`。parser 永远只消费内核所有快照，不读取用户物理页 slice。

## 6. VA-backed UserBuffer

`UserBuffer` 只保存：

```text
token
UserRanges
  ├── Contiguous(UserRange { start_va, len })
  └── Scatter(Vec<UserRange>)
logical_len
UserAccess
```

普通 read/write 使用一个 `Contiguous` 区间，不为小 I/O 分配逐页元数据；readv/writev
按非空 iovec 保存一个逻辑 VA 区间。两种表示都不会保存 PA、frame、direct-map pointer
或 Rust slice。

构造过程会预 fault 整个描述区间，用于保持既有 ABI 的“先验证用户输出，再执行外部
副作用”排序，但这不是 pin。另一 CPU 可在构造后立即改变映射，所以每次 `read_into()`、
`write_from()`、`fill_at()` 或 `clear()` 仍逐页重新获取 VM 锁并解析当前 PTE。

## 7. partial 与 exact 语义

流式接口返回 `Result<usize, isize>`：

| 情况 | 返回 |
|------|------|
| 第一个字节前即失败 | `Err(errno)` |
| 已复制一段前缀后，后续页失败 | `Ok(copied_prefix)` |
| 全部完成 | `Ok(requested_len)` |

主要 partial 接口包括：

- `read_into()` / `read_into_at()`；
- `write_from()` / `write_from_at()`；
- `fill_at()` / `clear()`。

文件、pipe、socket 和 PageCache 必须用实际复制字节数推进 offset、有效范围或返回值，
不能把请求长度当成已经完成的长度。

固定格式对象不接受部分成功：

- `UserBufferReader::read_exact()`；
- `UserBufferWriter::write_all()`。

这两个 wrapper 只有在复制长度等于请求长度时才返回成功，否则返回 `EFAULT`。选择 partial
还是 exact 由 syscall ABI 决定，不能在底层统一抹平。

## 8. UserIoVec

`UserIoVec::read_user_iovecs()` 先用 fixed-copy 读取用户 iovec 数组，检查计数、长度溢出和
总长度，然后保留描述符的内核副本。

构造 reader/writer buffer 时：

1. 按 `total_cap` 截断逻辑总长；
2. 每个非空 iovec 形成一个 `UserRange`；
3. 对每个范围做构造期预 fault；
4. 实际 scatter copy 再按逻辑 offset 逐页校验当前映射。

因此 iovec 边界与页边界是两个不同层次：前者定义用户可见的逻辑序列，后者只影响实际
copy 的锁粒度。

## 9. nofault 锁内例外

普通规则是“释放业务锁，再做 faultable uaccess”。pipe 的 ring buffer 当前由
`spin::Mutex` 保护，复制期间必须保持 head/tail 与数据一致，又不能在该自旋锁内进入可能
等待的 fault handler。B59 为此保留两个 crate-private 入口：

- `read_into_at_nofault()`；
- `write_from_at_nofault()`。

pipe 在锁外构造 `UserBuffer` 并预 fault；进入 ring 锁后，nofault copy 只调用
`resolve_user_va()` 检查现有 PTE。若并发 `munmap/mprotect` 改变了映射，复制立即按 partial
规则返回，而不是在自旋锁内 fault、分配或等待。

nofault 不是普通 I/O 的优化开关。新增调用点必须同时证明：

1. 锁外已经完成必要的 fault-in；
2. 锁内状态不能拆成“复制到内核临时 buffer—解锁—用户 copy”；
3. PTE 变化时允许立即失败或部分完成；
4. 入口保持 crate-private，不向任意业务代码扩散。

## 10. 锁序与副作用

常规调用顺序为：

```text
克隆稳定的 file/socket/task owner
  -> 释放 fd table、task.inner、file-private/socket 等普通锁
  -> faultable user copy
  -> 再进入业务操作或发布结果
```

需要同时满足以下规则：

- 不跨 WaitQueue、磁盘 I/O、socket poll 或远端 TLB ack 保存用户页翻译结果；
- 需要解析的变长对象先复制成内核快照；
- stateful 操作若先推进 offset，后续 exact copy 失败时必须回滚，或改为先复制到内核；
- 预 fault 只服务副作用排序，不能作为后续不再校验的理由；
- SysV IPC 等 registry 调用链仍需单独审计是否跨普通锁进入 faultable uaccess。

## 11. 非 faulting 探测

`user_accessible_len()` 只遍历现有 PTE，返回从起点开始连续满足权限的字节数。它不会：

- 分配 lazy page；
- 触发文件 mmap 读页；
- 处理 CoW；
- 扩展 `MAP_GROWSDOWN`；
- 修改 PTE 或刷新 TLB。

该接口适合非阻塞/分段 I/O 的可访问前缀估算，但结果同样是瞬时快照，实际 copy 仍需重验。

## 12. 审查清单

- [ ] 用户地址只由 uaccess helper 访问，没有直接解引用；
- [ ] helper 不返回 PA、direct-map pointer 或用户页 Rust 引用；
- [ ] partial 调用方按实际字节数更新状态；
- [ ] fixed ABI 使用 exact helper；
- [ ] faultable copy 前已释放普通业务锁；
- [ ] 必须锁内复制的路径使用受限 nofault helper，并接受映射变化失败；
- [ ] 预 fault 后的实际 copy 仍重新验证；
- [ ] PTE 修改产生的 flush/ack 位于 VM 锁外。
