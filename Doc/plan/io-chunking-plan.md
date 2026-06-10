# I/O Chunking — 消除用户驱动的大连续堆分配

## 问题背景

**现象**：LTP openat02 测试中，`write` 系统调用触发 heap OOM。

```
=== HEAP ALLOCATION FAILED (FATAL) ===
triggered by syscall: write
layout: size=16777216, align=8
KERNEL_HEAP_SIZE: 33554432 bytes
```

**根因**：`write` 路径中 `write_from_user` → `UserBufferReader::read_to_vec` 一次性分配 `count` 字节的连续内核堆缓冲区。用户传入 16MB count，但 32MB buddy heap 不存在 16MB 连续空闲块（~17MB 总空闲但碎片化，最大连续仅 8MB）。

## 碎片化分析

**不是泄漏问题**。heap trace 显示 live heap 仅 ~15MB，alloc/free 基本平衡：

| Rank | 大小 | 分配/释放 | 来源 |
|------|------|-----------|------|
| #10 | 24B | 407K/403K | `Arc<FrameTracker>` in `frame_alloc()` |
| #11 | 32B | 403K/399K | `Arc<PageEntry>` in `PageCache::get_or_create_entry()` |

高 churn 来自 LTP 大量 execve 加载 ELF，触发页面缓存频繁换入换出。

**真正的问题是设计缺陷**：I/O 路径依赖单一连续大分配，而 buddy allocator 碎片化不可避免。Linux 内核通过 `iov_iter` + 页级 I/O 避免此问题。

## 修改设计

### 核心思路

1. **I/O chunk size 从 heap size 动态计算**，不再硬编码
2. **所有 I/O 路径改为单 bounce buffer 循环**，每个 chunk ≤ IO_CHUNK_SIZE

### IO_CHUNK_SIZE 计算

在 `hal/mod.rs`：
```rust
pub const IO_CHUNK_SIZE: usize = {
    let heap = KERNEL_HEAP_SIZE;
    let raw = heap / 16;          // 最多用 1/16 heap 做 I/O 缓冲
    if raw < 64K { 64K }
    else if raw > 2M { 2M }
    else { raw }
};
```

32MB heap → 2MB；4MB heap → 256KB。

### 涉及文件

| 文件 | 改动 |
|------|------|
| `hal/mod.rs` | 新增 `IO_CHUNK_SIZE` const |
| `mm/uaccess.rs` | `UserIoVec` 新增 `reader_buffer_at(offset, len)` / `writer_buffer_at(offset, len)` |
| `syscall/fs.rs` | 删除 `MAX_SYSCALL_BUFFER_SIZE`；chunk 化 `write_from_user`、`pwrite_from_user`、`read_into_user`、`pread_into_user`、`sys_readv`、`sys_writev`、`sys_preadv`、`sys_pwritev`、`sys_sendfile`、`sys_copy_file_range` |
| `net/syscall/sendmsg.rs` | 删除 64MB cap；stream 做 chunked send，datagram 单包 cap 为 `IO_CHUNK_SIZE` |
| `net/syscall/recvmsg.rs` | 删除 64MB cap；单次 recv buffer cap 为 `IO_CHUNK_SIZE`，**不做 multi-chunk**（保持 datagram 原子性） |

### 关键设计决策

- **一个 bounce buffer，复用到底**：每个 I/O 路径只分配一次 `Vec<u8>`（chunk_size），在循环中重复使用
- **recvmsg 不做 chunk**：UDP 多次 recv 会消费多个包；未来 TCP `MSG_PEEK` 会重复读同一段数据
- **sendmsg 仅 stream chunk**：datagram/raw 必须保持消息边界
- **部分写返回进度**：Linux 语义：已写部分数据后遇到错误 → 返回已写字节数
- **fd table 锁不跨 chunk**：改为 clone-and-drop 形式
- **外部 syscall API 不变**：用户空间看不到 chunk 行为

### 暂不在此范围内

- `sendto`/`recvfrom` 的 `trans_ref`/`trans_refmut` 路径
- `Arc<FrameTracker>` + `Arc<PageEntry>` slab 分配器（单独优化）

## 验证清单

- [ ] `rg "MAX_SYSCALL_BUFFER_SIZE|MAX_MSG_IO_SIZE" os/src` 无残留
- [ ] `make rv64-kernel-build-only` ✅
- [ ] `make la64-kernel-build-only` ✅
- [ ] QEMU rv64 smoke ✅
- [ ] QEMU la64 smoke ✅
- [ ] LTP 定向测试（read01/write01/readv01/writev01）
- [ ] 16MB write reproducer 不触发 buddy OOM
