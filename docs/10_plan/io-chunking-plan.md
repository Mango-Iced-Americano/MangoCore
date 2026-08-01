# I/O Chunking — 消除用户驱动的大连续堆分配

> **状态：✅ 已实现**（2026-06-11）
> Oracle 审查 DragonOS 对比后修正方案，三路并行 deep agent 实施完成。
> 双架构编译通过，内核启动验证通过。具体实现可能与以下原始计划有差异（如 IO_CHUNK_SIZE 修正为 heap/128）。

## 问题背景

**现象 1**：LTP openat02 测试中，`write` 系统调用触发 heap OOM。

```
=== HEAP ALLOCATION FAILED (FATAL) ===
triggered by syscall: write
layout: size=16777216, align=8
KERNEL_HEAP_SIZE: 33554432 bytes
```

**现象 2**：`mkfs.ext4 /dev/vdb2` 4MiB journal 写入被截断为 2MiB → "short write" 错误。

**根因**：`write` 路径中 `write_from_user` → `UserBufferReader::read_to_vec` 一次性分配 `count` 字节的连续内核堆缓冲区。用户传入 16MB count，但 32MB buddy heap 不存在 16MB 连续空闲块。同时 `MAX_SYSCALL_BUFFER_SIZE = 2MiB` 在 syscall 入口处静默截断，导致 4MiB 写被限为 2MiB。

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

- `sendto`/`recvfrom` 的用户地址路径（原计划暂缓，B58 已删除
  `trans_ref`/`trans_refmut` 并改为内核快照）
- `Arc<FrameTracker>` + `Arc<PageEntry>` slab 分配器（单独优化）

## 验证清单

- [ ] `rg "MAX_SYSCALL_BUFFER_SIZE|MAX_MSG_IO_SIZE" os/src` 无残留
- [ ] `make rv64-kernel-build-only` ✅
- [ ] `make la64-kernel-build-only` ✅
- [ ] QEMU rv64 smoke ✅
- [ ] QEMU la64 smoke ✅
- [ ] LTP 定向测试（read01/write01/readv01/writev01）
- [ ] 16MB write reproducer 不触发 buddy OOM

---

## 实现记录（2026-06-11）

### Oracle 审查 DragonOS 对比分析

DragonOS 没有完整的 I/O 分块方案：chunk 了 `read`/`pread`/`pwrite`/`readv`(非socket) 和 `sendfile`/`copy_file_range`，但 **`write`**/**`writev`**/**`pwritev`**/**`sendmsg`**/**`recvmsg`** 仍然全量分配连续内核缓冲。Mango 的方案更全面。

### 方案修正（Oracle 建议）

| 项 | 原始计划 | 修正后 | 理由 |
|----|----------|--------|------|
| `IO_CHUNK_SIZE` 上限 | 2 MiB (heap/16) | **256 KiB** (heap/128) | 2MiB 仍受碎片影响 |
| 用户可见上限 | 无 | **`MAX_RW_COUNT`** = `i32::MAX & !0xFFF` | Linux 兼容 |
| EFAULT 语义 | 未明确 | **分 chunk 检查可访问性，已传部分返回进度** | DragonOS `pread_pwrite_common` 模式 |

### 实现范围

| 文件 | 改动量 | 关键变更 |
|------|--------|----------|
| `hal/mod.rs` | +2 常量 | `IO_CHUNK_SIZE`, `MAX_RW_COUNT` |
| `mm/uaccess.rs` | +4 方法 +1 fn | `accessible_len_at`, `reader_buffer_at`, `writer_buffer_at`, `build_user_buffer_at`, `user_accessible_len` |
| `mm/mod.rs` | +1 导出 | `user_accessible_len` |
| `syscall/fs.rs` | ~500 行变更 | 16 个 I/O 路径全部重写（read/write/pread/pwrite/readv/writev/preadv/pwritev 分块 + sendfile/copy_file_range cap/错误语义修复） |
| `net/syscall/sendmsg.rs` | 完全重写 | stream chunk / datagram 单包 |
| `net/syscall/recvmsg.rs` | 完全重写 | recv_cap 限 IO_CHUNK_SIZE |

### 编译验证
- `make rv64-kernel-build-only` ✅
- `make la64-kernel-build-only` ✅
- 零新增编译错误

### 残留问题
- `mkfs.ext4 4MiB write` 需专用测试镜像验证（不在当前环境）
- `linkat01 case 22`（mkfifo→ext4 create_with_data）已 defer
