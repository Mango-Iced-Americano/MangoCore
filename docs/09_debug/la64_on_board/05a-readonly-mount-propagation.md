---
title: "只读挂载经 bind、recursive bind 与传播副本丢失 RDONLY"
category: debug
status: resolved
author: MangoCore Team
last_update: 2026-07-15
tags: [postmortem, vfs, mount, bind, propagation, readonly, erofs, ext4]
code_paths:
  - "os/src/fs/vfs/mount.rs"
  - "os/src/fs/vfs/propagation.rs"
  - "os/src/syscall/fs.rs"
  - "os/src/drivers/block/partition.rs"
related_docs:
  - "docs/09_debug/la64_on_board/development-log.md"
  - "docs/09_debug/la64_on_board/05-block-size-translation.md"
  - "docs/03_fs/init-and-rootfs.md"
entry_points:
  - "MountFlags::persistent"
  - "MountFSInode::ensure_mount_writable"
  - "do_bind_mount"
  - "apply_rbind_snapshot"
  - "propagate_to_mount"
---

# 只读挂载经 bind、recursive bind 与传播副本丢失 RDONLY

## 1. 摘要

2K1000LA 将 P1 `/sdcard` 与 P3 `/tools` 只读挂载，再把 `/sdcard/musl`、
`/sdcard/glibc` 等子树 bind 到全局路径。旧 `do_bind_mount()` 用本次 syscall 的
`MS_BIND/MS_REC` 参数构造新 `MountFS`，没有继承源挂载 `RDONLY`；recursive bind
和 shared/slave 传播路径更直接使用空 flags。

结果是原挂载点写入正确返回 `EROFS`，bind 视图却被 VFS 认为可写。`mkdir`、create
或 link 没在 `MountFSInode` 边界停止，而是进入 ext4 block allocator，最后以
`No free blocks`、`ENOSYS` 等无关错误失败。底层 `ReadOnlyBlockDevice` 虽能阻止真实
SSD 写命令，却无法修复 errno、内存元数据和命名空间语义。

根因是混淆了两类 flags：

```text
operation control: BIND / REC / REMOUNT
persistent state:  RDONLY / NODEV / NOEXEC / NOSUID / ...
```

修复新增 `MountFlags::persistent()` 过滤操作位；普通 bind 从 source MountFS 克隆长期
属性，rbind snapshot 的每个 child 与传播产生的 peer/slave 副本都执行同一规则。
修复后 `/glibc/lib` 的 `mkdir` 在 VFS 入口明确返回 `EROFS`，不再出现 ext4 allocator
日志。

| 属性 | 结论 |
|------|------|
| 严重性 | Critical / P0；只读策略可被路径别名绕过 |
| 修复提交 | `296a67a2`，2026-07-11 |
| 直接根因 | clone mount 使用 syscall 操作位/空 flags，而非源挂载长期属性 |
| 错误失败层 | ext4 allocator 或底层 block write barrier |
| 正确失败层 | `MountFSInode::ensure_mount_writable()`，返回 `EROFS` |
| 实板证据 | `/glibc/lib` 创建明确 EROFS，无 `[balloc]`/ENOSYS/panic |

## 2. 证据范围

本问题的取证/功能基线为 `2031fd5909355994f768f845b2935e4509290a07`，关键变化由
`296a67a2` diff 锁定；之后当前 HEAD 的前进未改变这里分析的 bind flags 继承代码。

本文只讨论 VFS 挂载属性传播。MBR/平台/FS 三种块大小的适配是独立根因，见
`05-block-size-translation.md`。

## 3. 触发现象

### 3.1 启动布局

```text
P1 ext4 -> /sdcard         RDONLY
  /sdcard/musl  -> /musl   bind
  /sdcard/glibc -> /glibc  bind

P3 ext4 -> /tools          RDONLY
```

initproc 会在全局运行时路径尝试补目录/链接。对 bind 后的 `/glibc` 执行 `mkdir` 时，
旧实现没有立即得到：

```text
Read-only file system
```

而是进入 ext4 分配路径并打印：

```text
[balloc] No free blocks available
Function not implemented
```

这些输出误导调试去检查 ext4 空闲块、inode bitmap 或 ENOSYS 实现；但同一操作的
策略前提是“不允许写”，根本不该到 allocator。

### 3.2 最小反常对照

```text
mkdir /sdcard/xxx  -> EROFS
mkdir /glibc/xxx   -> enters ext4 allocator
```

两条路径最终指向同一个底层只读 ext4。唯一关键差异是后一条经过 bind MountFS，
因此故障边界应在 mount clone，而不是 ext4 或 SSD。

## 4. flags 的底层语义

### 4.1 操作控制位

这些位描述 `mount(2)` 当前调用要执行的动作：

```text
BIND      创建 bind view
REC       递归处理子挂载
REMOUNT   修改现有挂载属性
```

操作完成后，新挂载不应该“长期处于 BIND 状态”来决定读写权限。把它们保存在实例上
也会污染后续 propagation、remount 和 `/proc/mounts` 语义。

### 4.2 持久属性位

这些位必须随 mount instance 保留：

```text
RDONLY
NOSUID
NODEV
NOEXEC
SYNCHRONOUS
DIRSYNC
NOATIME
NODIRATIME
...
```

bind 的语义是为同一挂载子树增加一个路径视图，不是重新探测底层 FS 并凭调用参数
创建默认可写实例。因此 bind 初始属性来自 source MountFS。

### 4.3 `persistent()`

```rust
pub fn persistent(self) -> Self {
    self & !(REMOUNT | BIND | REC)
}
```

实现使用 bit mask/from_bits_truncate，但语义就是：过滤这三个操作位，保留长期属性。

## 5. 旧代码的三条泄漏路径

### 5.1 普通 bind

旧 `do_bind_mount()`：

```rust
let mnt_flags = vfs::MountFlags::from_bits_truncate(mountflags.bits());
```

调用通常只带 `MS_BIND`，所以 `mnt_flags` 不含 source 的 RDONLY。新 MountFS 与源指向
同一 inner filesystem，却有不同写权限状态。

### 5.2 recursive bind

`MS_BIND|MS_REC` 先快照 source 子挂载，再在 target 树重建。旧
`apply_rbind_snapshot()` 为每个 child 传 `MountFlags::empty()`。即使顶层 bind 修好，
嵌套 mount 仍会在递归副本中变可写。

### 5.3 shared/slave propagation

挂载事件传播到 peer/slave 时，`propagate_to_mount()` 也曾使用 empty flags：

```text
source new child RDONLY
  -> propagate clone
  -> target clone flags empty
```

这条路径最隐蔽，因为直接 bind 测试可通过，只有 shared/slave topology 下才丢策略。

三条路径共同说明：修一个 syscall 分支不够，必须审计所有创建 `MountFS` 副本的入口。

## 6. 为什么底层只读包装器不能替代 VFS

2K1000 验收路径有三层保护：

| 层 | 责任 | 失败结果 |
|----|------|----------|
| MountFS `RDONLY` | Linux/POSIX 可见语义 | mutation 立即 `EROFS` |
| 只读 block device node | 防止用户直接写原盘 | node write/open 被拒绝 |
| `ReadOnlyBlockDevice` | 阻断 FS 内部漏出的 writeback | 不发真实块写命令 |

若第一层丢失：

```text
mkdir
 -> ext4 alloc inode/block
 -> update memory caches/counters
 -> dirty metadata
 -> block wrapper drops/blocks eventual write
```

此时即使盘上没变化，也可能：

- syscall 返回 ENOSPC/EIO/ENOSYS 而不是 EROFS；
- 同一启动的 lookup 看见未持久化“幽灵对象”；
- 内存 bitmap/counter 已改变；
- rollback 路径因底层写失败产生二次错误。

物理安全和 API 正确性是两项独立验收。

## 7. 修复实现

### 7.1 普通 bind

```rust
let mnt_flags = source_mount_fs.mount_flags().persistent();
```

取值源改为 source instance，而不是 syscall 参数。`MS_BIND` 只负责路由进入 bind
分支，不决定新实例的 RDONLY。

### 7.2 recursive bind

对 snapshot 中每个 child：

```rust
entry.child_mfs.mount_flags().persistent()
```

递归副本逐层继承各自 source child 的属性；不能只把顶层 flags 一次性下传，因为
子挂载可能有不同的 RDONLY/NODEV/NOEXEC。

### 7.3 propagation

peer/slave 副本创建的三处路径统一：

```rust
new_child.mount_flags().persistent()
```

包括 root mount event、fallback direct constructor，以及目标已存在时 descend 的分支。

### 7.4 mutation 统一门禁

`MountFSInode::ensure_mount_writable()` 检查当前 wrapper 所属 MountFS：

```rust
if mount_flags.contains(RDONLY) {
    return Err(EROFS);
}
```

create/link/unlink/rename/write 等 mutation 在转发到 inner inode 之前调用。只要 clone
属性正确，底层 ext4/FAT 无需知道路径是原挂载还是 bind view。

## 8. 根因证明

| 候选原因 | 证据 | 结论 |
|----------|------|------|
| ext4 真无空闲块 | 同一底层原挂载直接 EROFS，根本不进 allocator | 排除 |
| `/glibc` 指向另一文件系统 | bind 指向 `/sdcard/glibc` 同一 inner FS | 排除 |
| 底层只读 wrapper 失效 | 最终写仍被挡，但 errno/路径错误 | 不是根因 |
| syscall flags 应决定新属性 | `MS_BIND` 本身不携带 RDONLY | 排除 |
| source flags 在 clone 时丢失 | diff 直接显示 old 使用 request/empty flags | 成立 |
| 普通 bind 单点修复足够 | rbind/propagation 另有 empty flags | 排除 |
| 全部 clone 使用 persistent source flags | 四类 mutation 与实板通过 | 闭环 |

## 9. 验证矩阵

### 9.1 RV64 QEMU

对正式测试盘的 `/sdcard` remount 只读，再 bind 到 `/tmp/robind`：

- `mkdir` 返回 `Read-only file system`；
- 文件 create 返回 `Read-only file system`；
- hard link 返回 `Read-only file system`；
- 删除既有文件返回 `Read-only file system`；
- 四类操作均未留下新对象或删除旧对象；
- 原 `/os_test.conf` 仍存在。

这同时验证 errno 与命名空间不变，不只检查“盘没写”。

### 9.2 2K1000LA 实板

```text
/dev/sda1 -> /sdcard RDONLY
/sdcard/glibc -> /glibc bind
mkdir /glibc/lib -> Read-only file system
```

修复后不再出现：

```text
[balloc] No free blocks available
Function not implemented
panic
```

实板 uImage/TFTP CRC 通过，证明运行的是对应修复镜像而非旧内核。

### 9.3 recursive/propagation 的证据边界

提交 diff 明确修复了 rbind snapshot 与 propagation 三个 clone 分支；Work Log 的
行为样本重点记录普通 bind。后续回归应保留构造 shared/slave topology 的专门测试，
不能用普通 bind 的通过替代所有传播组合。

## 10. 为什么这个 bug 容易误诊

1. 原挂载点正常 EROFS，只有别名路径异常；
2. 底层写屏障使 SSD 没被破坏，表面像“安全但 errno 不漂亮”；
3. ext4 allocator 先打印错误，把注意力引向块组/bitmap；
4. bind、rbind、propagation 各有独立 constructor，修一个后仍可能局部复发；
5. `MS_RDONLY` 和 VFS `RDONLY` 名字相近，容易忽略 flags 的来源和生命周期。

## 11. 可复用审计方法

搜索所有创建 mount clone 的位置：

```text
MountFS::new
MountFS::new_with_root
mount_subtree_inner
propagate_to_mount
apply_rbind_snapshot
```

逐个回答：

- 这是全新挂载还是 source clone？
- flags 来自 syscall 参数、source instance 还是 empty？
- 是否过滤 operation bits？
- 每个 child 是否保留自己的长期属性？
- mutation 是否在 wrapper 转发前检查？

测试至少覆盖：

```text
original RDONLY mount
plain bind
recursive bind with nested mounts
shared peer propagation
slave propagation
create/write/link/rename/unlink
```

## 12. 已知边界

1. `persistent()` 当前显式过滤 `REMOUNT/BIND/REC`；新增其他纯操作位时必须同步更新。
2. bind 后通过合法 remount 修改 flags 是另一个语义路径，不能被“永远继承 source”
   误伤。
3. 本修复保证挂载属性，不替代 block node 权限或 `ReadOnlyBlockDevice`。
4. propagation 的复杂拓扑需要持续 focused test；普通启动布局覆盖不到所有组合。
5. `RDONLY` 不等于数据完整性：只读读取仍需文件系统校验与底层 I/O 正确。

## 13. 最终因果链

```text
source MountFS has RDONLY
  -> bind clone uses request flags (MS_BIND only)
  -> rbind/propagation clone uses empty flags
  -> cloned MountFS loses RDONLY
  -> MountFSInode allows mutation through
  -> ext4 allocator runs and reports misleading errors
  -> block barrier only stops final media write

source.mount_flags().persistent()
  applied to plain bind + each rbind child + every propagated clone
  -> operation bits removed, long-term attributes retained
  -> mutation stops at VFS boundary with EROFS
  -> namespace and ext4 allocator remain untouched
```
