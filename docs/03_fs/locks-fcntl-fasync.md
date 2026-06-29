---
title: "文件锁、fcntl 与 fasync"
module: "fs/locks"
category: "fs"
status: "draft"
owner: "MangoCore Team"
last_updated: "2026-06-29"
code_paths:
  - "os/src/fs/vfs/posix_lock.rs"
  - "os/src/fs/vfs/fcntl.rs"
  - "os/src/fs/vfs/fasync.rs"
  - "os/src/syscall/flock.rs"
  - "os/src/syscall/fs.rs"
entry_points:
  - "LockRecord"
  - "PosixLockManager"
  - "FcntlCommand"
  - "FAsyncItems"
  - "sys_flock"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "flock*"
    - "fcntl*"
  oscomp:
    - "basic"
    - "busybox"
related_docs:
  - "docs/03_fs/architecture.md"
  - "docs/03_fs/vfs-core.md"
---

# 文件锁、fcntl 与 fasync

## 概述

MangoCore 支持 Linux 兼容的文件锁和文件控制机制，包含三个层次：POSIX 记录锁（fcntl F_SETLK/F_SETLKW/F_GETLK）、BSD 风格 flock 咨询锁，以及基于 fasync 的异步 I/O 通知（SIGIO）。所有锁机制均工作在 VFS 层之上，对底层文件系统透明。

## POSIX 记录锁

`os/src/fs/vfs/posix_lock.rs` 实现完整的 POSIX 记录锁语义，支持两种锁所有者类型：

| 类型 | 语义 | 生命周期 |
|------|------|---------|
| `LockOwner::Posix` | 进程关联锁（fork 后不继承） | fd 关闭时由 `release_posix_for_owner()` 释放 |
| `LockOwner::Ofd` | 打开文件描述锁（open file description） | 最后一个 File 引用释放时由 `release_ofd_for_file()` 释放 |

### 架构设计

全局 `PosixLockManager` 采用 **53 分片**（shard）结构，以 `(dev_id, inode_id)` 作为 `LockKey` 散列到对应分片。每个分片包含一个 `BTreeMap<LockKey, Arc<PosixLockEntry>>`，其中 `PosixLockEntry` 持有：

- `state: Mutex<EntryState>` — 有序的 `Vec<LockRecord>` 记录列表，记录按 `start` 排序且不重叠（通过 `coalesce()` 合并）
- `waitq: Mutex<WaitQueue>` — F_SETLKW 阻塞等待队列

锁冲突规则遵循 POSIX 标准：**写锁排斥所有锁，读锁只排斥写锁**。同一所有者持有的锁从不自相冲突。

### F_SETLKW 与死锁检测

阻塞路径在 `posix_lock_set()` 中循环尝试获取锁，通过 `WaitQueue::wait_event_interruptible()` 阻塞。每次唤醒后：

1. 尝试 `apply_lock()`，成功则返回
2. 找到当前阻塞者（`blocker_id`）
3. 更新全局等待图（`wait_graph: EdgeMap`），记录 `waiter_id → {blocker_id: count}`
4. DFS 检测从 blocker 出发能否到达 waiter，若检测到环则返回 `EDEADLK`

OFD 锁所有者 ID 通过或上 `(1 << 62)` 与 POSIX 所有者 ID 隔离。

### 关键函数

| 函数 | 对应 fcntl cmd | 行为 |
|------|---------------|------|
| `posix_lock_get()` | F_GETLK / F_OFD_GETLK | 查询第一个冲突锁，写回 `PosixFlock` |
| `posix_lock_set()` | F_SETLK / F_OFD_SETLK | 非阻塞获取/释放锁，失败返回 EAGAIN |
| `posix_lock_set()` blocking=true | F_SETLKW / F_OFD_SETLKW | 阻塞等待，带死锁检测 |
| `release_posix_for_owner()` | close(fd) | 释放进程关联的 POSIX 锁并唤醒等待者 |
| `release_ofd_for_file()` | File::drop | 释放 OFD 锁并唤醒等待者 |

`resolve_range()` 解析 `PosixFlock` 中的 `l_whence/l_start/l_len` 为 `(start, end)` 闭区间，支持 SEEK_SET、SEEK_CUR 和 SEEK_END，`l_len=0` 表示锁到文件尾（`i64::MAX`）。

### 使用示例

```c
struct flock fl = {0};
fl.l_type = F_WRLCK;
fl.l_whence = SEEK_SET;
fl.l_start = 0;
fl.l_len = 100;
fcntl(fd, F_SETLKW, &fl);   // 阻塞获取写锁
// ... 临界区 ...
fl.l_type = F_UNLCK;
fcntl(fd, F_SETLK, &fl);    // 释放锁
```

## BSD flock

`os/src/syscall/flock.rs` 提供 BSD 风格 `flock()` 系统调用，使用全局 `FLOCK_TABLE: BTreeMap<(dev_id, inode_id), ()>` 记录锁状态。

| 操作 | 行为 |
|------|------|
| LOCK_SH / LOCK_EX | 文件未被锁定时获取独占锁，已被锁时返回 EAGAIN |
| LOCK_UN | 无条件释放锁 |
| LOCK_NB | 非阻塞标记，与 LOCK_SH/LOCK_EX 配合使用 |

当前实现为简化版本：共享锁（LOCK_SH）和排他锁（LOCK_EX）都使用同一个全局标记，不支持多个 LOCK_SH 同时持有。阻塞模式（不带 LOCK_NB）暂退化为 EAGAIN。未来需引入 WaitQueue 和读锁引用计数。

## fasync 异步 I/O 通知

`os/src/fs/vfs/fasync.rs` 实现 `FAsyncItems` 机制，用于 SIGIO 信号通知。

### 数据结构

每个 inode 可挂载一个 `FAsyncItems` 实例，维护一个 `Vec<FAsyncItem>` 列表。每个 `FAsyncItem` 记录 `(Weak<File>, fd)` 对。

### 注册与注销

在 `fcntl(F_SETFL)` 中 `O_ASYNC` 位变化时调用 `set_file_fasync()`：

| 函数 | 行为 |
|------|------|
| `add(file, fd)` | 注册 fd 到 inode 的 fasync 列表 |
| `remove(fd)` | 从 inode 的 fasync 列表中移除 |
| `send_sigio(signum_override)` | 向所有注册的文件所有者发送信号 |

### 信号交付

`send_sigio()` 遍历 items 列表，对仍保持 `O_ASYNC` 标记的文件执行：

1. 读取 `FileOwnerSnapshot`（含 target 和 signum）
2. 确定信号号：优先使用 `signum_override`，否则用文件 owner 的 `signum`（通过 F_SETSIG 设置），默认 SIGIO(29)
3. 根据 `FileOwnerTarget` 种类发送信号：
   - `Pid(pid)` → `send_process_signal()`
   - `Pgrp(pgid)` → `ProcessManager::send_signal_to_group()`
   - `Tid(tid)` → `send_thread_signal()`

发送信号时不持锁（先 snapshot 后发送），避免锁序问题。

pipe 驱动等流式文件在数据可读时调用 `fasync_items.send_sigio(None)` 触发通知。

## fcntl 命令

`os/src/fs/vfs/fcntl.rs` 定义 `FcntlCommand` 枚举（`#[repr(u32)]`，`TryFromPrimitive`），`os/src/syscall/fs.rs` 中 `sys_fcntl()` 分发处理。支持的命令：

| 命令 | 行为 |
|------|------|
| F_DUPFD / F_DUPFD_CLOEXEC | 复制 fd，分配 >= arg 的最小空闲 fd |
| F_GETFD | 读取 close-on-exec 标志 |
| F_SETFD | 设置 close-on-exec 标志（仅 FD_CLOEXEC 有效） |
| F_GETFL | 返回文件状态标志（access mode + status flags） |
| F_SETFL | 设置文件状态标志（O_ASYNC/O_NONBLOCK 等，保留 access mode） |
| F_GETOWN / F_SETOWN | 获取/设置文件所有者 PID 或进程组 |
| F_GETOWN_EX / F_SETOWN_EX | 扩展版本，支持 TID/PID/PGRP 三种类型 |
| F_GETSIG / F_SETSIG | 获取/设置 fasync 信号号（默认 SIGIO） |
| F_SETLEASE / F_GETLEASE | 文件租约（file lease），仅支持 F_RDLCK 和 F_UNLCK |
| F_ADD_SEALS / F_GET_SEALS | memfd seal 操作 |
| F_SETPIPE_SZ / F_GETPIPE_SZ | 管道容量控制 |
| F_GET_RW_HINT / F_SET_RW_HINT | 文件读写提示（扩展属性） |
| F_CREATED_QUERY | 查询文件是否由 `open()` 创建 |

### 文件租约（Lease）

`File` 结构体包含 `lease: Mutex<Option<i16>>` 字段。F_SETLEASE 当前仅支持：

- `F_RDLCK` — 获取读租约，需文件可读且没有其他进程以写模式打开
- `F_UNLCK` — 释放租约
- `F_WRLCK` — 写租约暂不支持，返回 EAGAIN

## 测试映射

| 测试组 | 覆盖范围 | 状态 |
|--------|---------|------|
| LTP flock* | BSD flock 基本功能 | 基础路径通过 |
| LTP fcntl* | POSIX 记录锁和 fcntl 命令 | 基础路径通过 |
| LTP fcntl{01,02,03,04}... | 死锁检测、OFD 锁语义 | 部分通过 |

## 已知问题

1. **BSD flock 阻塞模式** — 不带 LOCK_NB 时未实现 WaitQueue 等待，返回 EAGAIN
2. **BSD flock 共享锁** — LOCK_SH 和 LOCK_EX 使用相同全局标记，不支持多个读锁并发持有
3. **写租约** — F_SETLEASE(F_WRLCK) 未实现，返回 EAGAIN
4. **死锁检测** — OFD 锁所有者 ID 通过位标记（bit 62）避免冲突，理论上在极端情况下可能误判
5. **shard 锁粒度** — 单个 shard 的锁粒度为 Mutex，高竞争场景下可能成为瓶颈
