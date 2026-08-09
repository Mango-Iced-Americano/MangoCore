---
title: "FAT32 持久化、显式目录事务与 canonical inode 身份"
category: debug
status: resolved-with-known-limits
author: MangoCore Team
last_update: 2026-07-15
tags: [postmortem, fat32, page-cache, inode, persistence, rename, drop, 2k1000la]
code_paths:
  - "os/src/fs/fat32/bitmap.rs"
  - "os/src/fs/fat32/efs.rs"
  - "os/src/fs/fat32/fat_inode.rs"
  - "os/src/fs/page_cache.rs"
  - "os/src/fs/mod.rs"
  - "user/tools/cpython/L7_filesystem.py"
related_docs:
  - "docs/09_debug/la64_on_board/260710/development-log.md"
  - "docs/03_fs/fat32.md"
  - "docs/03_fs/2k1000-full-test-disk.md"
  - "docs/08_testing/cpython-isolated.md"
entry_points:
  - "EasyFileSystem::canonicalize_inode"
  - "FatInode::sync_parent_dir_entry"
  - "FatInode::rename"
  - "FatInode::detach_deleted"
  - "FatPageCacheBackend"
---

# FAT32 持久化、显式目录事务与 canonical inode 身份

## 1. 摘要

P2 FAT32 从“能挂载、能读”推进到“可作为实板 scratch”时，连续暴露了三类一致性
问题：

1. `BlockSizeAdapter` 已把设备 block id 定义为 BPB sector，FAT 表代码却再次除以
   全局 `BLOCK_SZ/512`，导致访问错误扇区；
2. create/unlink/rmdir 和文件首簇/大小更新依赖 inode `Drop` 或延迟 PageCache 写回，
   同一启动内的另一个 inode/PageCache 可能看见新对象，但重建 FS 或 U-Boot 看不到；
3. 同一 FAT 目录项可被多次构造成不同 `FatInode`，各自持有 PageCache。rename 覆盖
   已有目标时，命名空间已经指向源簇，新路径却仍可能从旧目标缓存读出旧内容；
   `Weak<FatInode>` 后端还会在 owner 进入 `Drop` 后无法 upgrade，丢失最后一次写回。

最终方案不是增加 `sync` 次数，而是明确两个不变量：

- **磁盘事务边界明确**：成功返回 create/unlink/rmdir/rename 前，相关父目录页和 FAT
  元数据已经写回；`Drop` 不再承担父目录命名空间提交；
- **内存对象身份唯一**：有首簇的对象以 `Cluster(first_cluster)` 为键，尚无簇的空
  文件以 `{parent_cluster, dir_entry_offset}` 为临时键；首次分簇、rename、unlink 时
  原子重键或 detach，保证同一磁盘对象共享一份 `FileContent` 和 PageCache。

| 属性 | 结论 |
|------|------|
| 严重性 | Critical / P0；可能静默丢文件、复活目录项或读到被覆盖目标旧内容 |
| 首个持久化修复 | `5bb715c0`，2026-07-11 |
| 目录事务修复 | `8f7d8da6`，2026-07-11 |
| 首版原生 rename | `3ce82f0a`，2026-07-12 |
| canonical inode/覆盖 rename | `6b628240`，2026-07-14 |
| 决定性证据 | fresh FS instance、U-Boot 复查、无 fsync rename 压力与 open-target 语义 |

## 2. 证据口径

本文使用三类证据：

- **磁盘证据**：重新创建 `EasyFileSystem`、U-Boot `fatls`、板端复位后读取；
- **对象证据**：首簇、目录项 offset、inode/PageCache 是否为同一实例；
- **行为证据**：syscall 成功后立即重开、删除后重开、覆盖 rename 后旧 fd 与新路径
  分别读到什么。

本文 FAT32 问题的取证/功能基线为 `2031fd5909355994f768f845b2935e4509290a07`；之后
当前 HEAD 的前进未改变这里分析的 FAT32 持久化与 inode identity 代码。本文
不会用“同一内核启动中 lookup 成功”替代持久化证明。

> **现状注记（2026-08-08）**：VFS Phase 5 重构后挂载路径不再套
> `BlockSizeAdapter`，`EasyFileSystem::open` 直接保存原始 BLOCK_SZ(4096) 设备；
> FAT 数据路径（`page_cache.rs::FatPageCacheBackend`）与 FAT 表路径
> （`bitmap.rs::sector_to_parent`）各自做扇区→块换算。本节描述的是 2026-07
> 批次引入适配器时的历史设计与“双换算”bug，用于理解其成因；当前寻址契约以
> 源码注释为准。

## 3. 第一阶段：FAT sector 被重复按平台块换算

### 3.1 旧代码为什么在适配器存在时反而出错

挂载路径已经执行：

```text
raw PartitionBlockDevice
  -> BlockSizeAdapter(BPB_BytsPerSec)
  -> EasyFileSystem
```

因此 FAT 内部看到的 `block_id=N` 已经表示“第 N 个 BPB sector”。旧 `bitmap.rs` 又
执行：

```rust
const SECTORS_PER_BLOCK: usize = BLOCK_SZ / 512;
block_id  = sec_num / SECTORS_PER_BLOCK;
sector_off = (sec_num % SECTORS_PER_BLOCK) * 512;
```

这相当于把同一个单位适配两次。

以 2K1000 的 `BLOCK_SZ=2048`、FAT sector 100 为例：

```text
适配器期望输入: block_id 100 -> byte offset 100 * BPB_BytsPerSec
旧 FAT 再换算: block_id 25, in-block 0
结果:          访问第 25 个 BPB sector，而不是第 100 个
```

这不是“512B 设备性能差”的问题，而是 block id 的契约已经改变，调用方仍按旧契约
解释。

### 3.2 修复后的 FAT 表寻址

FAT 表项 `cluster * 4` 只在 BPB 单位内计算：

```text
fat_byte_offset      = cluster * 4
fat_sector_offset    = fat_byte_offset / BPB_BytsPerSec
entry_offset         = fat_byte_offset % BPB_BytsPerSec
device sector        = reserved_sectors
                       + fat_index * sectors_per_fat
                       + fat_sector_offset
```

不再引用全局 `BLOCK_SZ`。同时实现并校验：

- `BPB_NumFATs`；
- `BPB_ExtFlags` 的 mirror enable 与 active FAT index；
- 镜像启用时向所有 FAT copy 写同一低 28 位值；
- 保留 FAT32 entry 高 4 位；
- 数据簇号范围是 `2..cluster_count+2`；
- FAT entry capacity 足以容纳声明的数据簇。

这一修复关闭的是“元数据写到了错误 sector”，但尚未保证目录项何时持久化。

## 4. 第二阶段：同启动可见不等于磁盘已提交

### 4.1 第一轮文件探针的反常结果

内核为 P2 建立隔离可写视图，执行：

```text
mkdir MANGO_RW_PROBE
create PAYLOAD.BIN
write 6144 bytes
flush/drop/reopen filesystem
reopen file and compare
unlink file
rmdir directory
reopen filesystem again and verify cleanup
```

第一轮修复后，fresh FS instance 已能重开并比对 6144 字节 payload，但清理阶段：

```text
rmdir -> ENOTEMPTY
```

这条结果比“写入失败”更有定位价值：文件数据、首簇和大小至少一度能从盘重建，
但 unlink 对父目录页的删除标记仍未在 rmdir 检查前形成统一可见状态。

### 4.2 为什么依赖 `Drop` 是错误的事务模型

Rust `Drop` 只说明最后一个强引用结束，不能说明：

- syscall 成功返回时已经 Drop；
- 所有别名 inode 同时结束；
- PageCache 先于/后于父目录 inode Drop；
- stale inode 不再持有旧目录项快照；
- 复位或 panic 前一定有机会执行析构。

若 create 的父目录写回留给 Drop，会出现：

```text
mkdir 返回 0
  -> 当前 root inode 的 PageCache 看见目录项
  -> lookup/open 在同一缓存上成功
  -> 另一个 root_inode() 拥有独立 PageCache
  -> 物理复位或 U-Boot 只读磁盘
  -> 根目录仍为空
```

更危险的是 stale inode 在稍后 Drop 时，把旧父目录页写回，覆盖已经成功提交的新
目录项。析构顺序因此不仅会“漏写”，还可能“晚到并覆盖新状态”。

### 4.3 U-Boot `fatls` 提供跨实现旁证，但不能单独排除错 offset

首轮用户态 scratch 测试曾出现：

```text
unlink = 0
rmdir  = -ENOENT
```

复位后 U-Boot 对 P2 执行 `fatls`，结果为：

```text
0 file(s), 0 dir(s)
```

U-Boot 不共享 MangoCore 的 inode/PageCache，因此这个结果排除了“只是在原内核缓存
中看见对象”的假阳性。根目录完全为空也**更支持**“父目录页没有提交”的解释。

但 `fatls` 只列出从 FAT 根目录链可达的目录项：若旧代码把目录页写到了错误 sector
或分区内其他 offset，U-Boot 同样可能显示 0 file/0 dir。该轮没有保存全分区写前/写后
sector diff，也没有扫描不可达区域，因此它不能独立关闭“写到其他 offset”假设。
最终判断来自代码修正、fresh FS 重开、显式父目录写回与 U-Boot 旁证的组合，而不是
把一条空目录输出当作 offset 正确性的充分证明。

### 4.4 显式事务边界

修复后：

- `fat_do_create()` 在返回前写回父目录；
- 创建目录时先建立并提交 `.`、`..` 与结束标记，再发布父目录项；
- write/resize 在首簇或大小改变后调用 `sync_parent_dir_entry()`；
- unlink/rmdir 在成功返回前写回删除标记所在目录页；
- rename 在源/目标目录项完成后显式写回；
- stale inode `Drop` 不再修改父目录命名空间。

`Drop` 仍可负责自身脏数据的最后写回和已经 detach 对象的簇回收，但不再是 namespace
事务的 commit 点。

## 5. 第三阶段：FAT rename 不能复用 `link + unlink`

### 5.1 默认 VFS 实现与 FAT 模型冲突

通用 VFS 可以把 rename 表达为：

```text
link(old_inode, new_name)
unlink(old_name)
```

但 FAT 目录项直接拥有“首簇 + 大小 + 属性”，没有 Unix inode link count，也不支持
硬链接。busybox `mv` 首次在 `/scratch/work` 运行时，只有 rename/rmdir 失败，其他
touch/write/cp/mkdir 均成功，说明基础写路径已工作，剩余边界是命名空间操作。

### 5.2 首版同目录原生 rename

`3ce82f0a` 实现同目录、目标不存在的 rename：

1. 读取源短目录项；
2. 保留首簇、文件大小、属性和时间；
3. 为新名称生成短名与 VFAT 长名项；
4. 创建新目录项；
5. 删除旧目录项；
6. 写回父目录；
7. 删除旧项失败时回滚新项。

它不复制文件数据，也不分配/释放源簇。修复后 musl/glibc busybox 的 `mv` 与后续
`rmdir` 均成功。

此阶段的边界是：只支持同目录、目标不存在；跨目录与覆盖目标明确返回错误。后续
CPython 才逼出覆盖 rename 的 inode 身份问题。

## 6. 第四阶段：覆盖 rename 后命名空间正确，内容却仍旧

### 6.1 决定性失败输出

CPython L7 在真实 FAT scratch 上反复执行同名覆盖、且不在每轮 `fsync` 后，稳定
得到：

```text
RENAME_FAIL 0 b'S00' b'D00' b'tar' 104464 104456 104464
```

该输出同时记录：

- rename 返回 0；
- 源内容为 `S00`；
- 旧目标内容为 `D00`；
- 新目标路径读取仍为旧内容；
- 目录项/首簇诊断表明新路径元数据已经切向源对象。

所以问题不是“rename syscall 没执行”，而是：

> 命名空间映射已经更新，但打开新名字时命中了另一份旧 inode/PageCache 身份。

### 6.2 重复 inode 如何产生静默旧读

旧 `from_fat_ent()` 每次 lookup 都可能新建 `FatInode`：

```text
target name -> FatInode T1 -> PageCache CT1 (old target data)
target name -> FatInode T2 -> PageCache CT2
source name -> FatInode S1 -> PageCache CS1 (new source data)
```

rename 把目标目录项的首簇改成源首簇后，如果后续 lookup 仍返回 T1 或新建一个与
旧 cache 关联的对象，就会出现“stat/目录项都对，read 内容错”的分裂状态。

文件系统磁盘身份和内存对象身份没有一一对应，是根因，而不是少调用一次 cache
invalidate。单纯清某个名字的 dentry 无法证明另一份 `FatInode` 不存在。

### 6.3 `Weak<FatInode>` PageCache 后端的生命周期陷阱

旧 FAT PageCache backend 间接保存 `Weak<FatInode>`，需要 inode 才能读取簇链。
当最后一个强引用进入 `FatInode::drop()` 时：

```text
strong_count becomes zero
  -> Drop runs
  -> try final PageCache writeback
  -> backend.weak_inode.upgrade() == None
  -> cannot map file offset to cluster
  -> final dirty data cannot be written correctly
```

这是 Rust `Arc` 生命周期的必然结果：进入最后一个 owner 的 Drop 时，weak upgrade
不会重新取得该对象。把“最终写回需要的数据”只放在 owner 内部，形成了生命周期
循环依赖。

## 7. canonical inode 设计

### 7.1 稳定键

FAT 没有独立 inode number，因此使用磁盘对象天然身份：

```rust
enum FatInodeKey {
    Cluster(u32),
    EmptyDirEntry {
        parent_cluster: u32,
        offset: u32,
    },
}
```

- 已分配文件和目录：首簇在对象存续期间是稳定身份；
- 空文件：`first_cluster=0`，只能临时用父目录首簇 + 短目录项 offset；
- root/目录必须有簇，可直接使用 Cluster key。

`EasyFileSystem::inode_objects` 是 `BTreeMap<FatInodeKey, Weak<FatInode>>`。lookup 先
尝试 upgrade 已有对象，只有不存在/已死亡时才注册 candidate。表中只存 Weak，
不会延长已删除文件生命周期。

### 7.2 必须重键的状态变化

稳定键不是永远不变。以下操作必须在同一语义事务内移除旧键并注册新键：

| 操作 | 旧键 | 新键 |
|------|------|------|
| 空文件首次写入 | `EmptyDirEntry(parent, off)` | `Cluster(first_cluster)` |
| truncate 到零并释放全部簇 | `Cluster(first_cluster)` | `EmptyDirEntry(parent, off)` |
| 目标不存在 rename | 原目录项键 | 新目录项位置或同一 Cluster key |
| 覆盖空目标 | 源临时键 + 目标临时键 | 先 detach 目标，再迁移源到目标位置 |
| unlink | 当前键 | 无：从 namespace canonical 表移除 |

若首次分簇不重键，后续按 Cluster lookup 仍会新建第二个 inode；若 unlink 不移除，
同一个目录项 offset 被新文件复用时会错误复活旧 inode。

### 7.3 PageCache 后端共享最小状态

`FatPageCacheBackend` 改为持有：

```text
Arc<RwLock<FileContent>>
```

其中包含簇链、大小等映射所需最小状态。inode 与 backend 共享 `FileContent`，但 backend
不强持有 inode，因此：

- 不形成 `inode -> PageCache -> backend -> inode` 强引用环；
- 最后一个 inode 进入 Drop 时，backend 仍能读取簇链完成写回；
- canonical inode 保证同一对象只有一份活动 PageCache。

这是“共享必要后端状态”，不是“把整个 inode 改成永久全局对象”。

## 8. 覆盖 rename 的对象与磁盘事务

### 8.1 目标存在的普通文件覆盖

当前同目录普通文件覆盖遵循：

1. 若 `RENAME_NOREPLACE`，目标存在立即 `EEXIST`；
2. 校验源/目标类型；目录覆盖仍显式 `ENOSYS`；
3. 保留目标短名，使既有 LFN checksum 有效；
4. 把目标目录项的首簇、大小、属性、时间替换为源元数据；
5. 删除旧源目录项；失败则恢复原目标目录项；
6. 显式写回父目录；
7. 源 inode/PageCache 迁移到目标目录项身份；
8. 旧目标从 namespace detach，但其簇保留到最后一个打开引用关闭。

### 8.2 为什么不能立刻释放旧目标簇

POSIX rename-over-existing 要求：

```text
fd = open("target")
rename("source", "target")
read(fd)          -> 仍读旧 target
open("target")   -> 读 source
close(fd)         -> 此后才可回收旧 target storage
```

若更新目录项后立即释放旧目标簇，旧 fd 会读到被重新分配的数据；若旧目标 PageCache
稍后写回，还可能覆盖新文件。`detach_deleted()` 将“从命名空间移除”和“释放数据簇”
拆开，簇回收延迟到旧 inode 最后引用结束。

## 9. 验证链

### 9.1 每一步为何有辨别力

| 验证 | 结果 | 证明 |
|------|------|------|
| fresh FS 重开 6144B | 内容一致 | 不只命中原 inode/PageCache |
| 第一次清理 `ENOTEMPTY` | 失败 | 暴露 unlink 目录页延迟提交 |
| 补显式写回后再次重开 | 文件/目录均不存在 | 删除已持久化 |
| U-Boot `fatls` 为空 | 0 file/0 dir | 排除同一内核缓存假命中；支持父目录未落盘解释，但不单独排除错 offset |
| `[scratch-smoke] PASS` | write/fsync/truncate/reopen/read/unlink/rmdir | 用户态完整 CRUD |
| busybox `mv` | musl/glibc 均 success | FAT 原生 rename 基本路径 |
| QEMU CPython L7 | rv64/la64 均 `72/72` 总门禁 | 双架构回归 |
| 实板 50 轮无 fsync rename | `RENAME_NOSYNC_PASS 50` | 重复目录项复用/caching |
| 空文件覆盖 | `RENAME_EMPTY_PASS` | 临时 EmptyDirEntry key 重键 |
| 旧目标保持 open | `RENAME_OPEN_TARGET_PASS` | detach 与延迟簇释放 |
| 综合专项 | `FAT_RENAME_DIAG_PASS` | rename 内容与生命周期闭环 |

实板最终还执行目录清理与全局 `sync`，输出 `FAT_DIAG_CLEAN` 和
`BOARD_FINAL_SYNC_OK`，避免专项测试只在内存中结束。

### 9.2 为什么不能只看 syscall 返回 0

原始 `RENAME_FAIL` 已证明 rename 返回 0 时内容仍可错误。任何命名空间测试至少要
验证：

- 旧名字不存在；
- 新名字存在；
- 新名字内容来自源；
- 旧目标打开 fd 仍保持旧内容；
- fresh FS/reboot 后结论不变；
- cleanup 后对象没有复活。

## 10. 已知边界

当前已提交实现支持同目录文件/目录改名，以及普通文件覆盖普通文件；
`RENAME_NOREPLACE` 可拒绝覆盖。仍明确限制：

- 跨目录移动返回 `ENOSYS`，因为需要双目录锁顺序与 `..` 更新；
- 目录覆盖返回 `ENOSYS`，尚缺目标空目录校验和目录簇事务回收；
- FAT 不支持硬链接、Unix 权限与精细时间戳；
- canonical key 依赖首簇或目录项位置，所有修改这两者的新代码都必须同步重键；
- 显式 writeback 提供运行时一致性，不等同于断电事务或日志恢复。

## 11. 可复用调试方法

### 11.1 强制跨缓存边界验证持久性

```text
operation
  -> sync relevant object
  -> drop all known inode/FS handles
  -> construct fresh FS instance
  -> lookup/read
  -> optional bootloader or physical reset read
```

只 close/reopen fd 可能仍命中同一 inode；只重新 lookup 可能仍命中同一 dentry；只有
重建 FS 或换读取者，才能证明磁盘状态。

### 11.2 同时记录“名字身份”和“数据身份”

rename 问题应同时打印：

```text
parent first cluster
dir entry offset
file first cluster
inode object identity
PageCache identity
source/target bytes
```

名字对而内容错时，优先检查 canonical inode/PageCache；名字错而内容在孤立簇上，
优先检查目录事务。

### 11.3 析构不是提交协议

所有必须在 syscall 成功时成立的磁盘条件，都要有显式 `sync_*` 调用和失败返回；
`Drop` 只能做资源兜底。审计时搜索“父目录更新只在 Drop 中发生”应视为高风险。

## 12. 最终因果链

```text
FAT block id 已由 BlockSizeAdapter 表达为 BPB sector
  -> FAT 内部再次按 BLOCK_SZ/512 换算
  -> FAT entry 访问错误 sector

修正 sector 后，目录 mutation 仍依赖 Drop/延迟 PageCache
  -> 同启动旧 cache 看似成功
  -> fresh FS / U-Boot 看不到，或 stale Drop 覆盖新状态

显式目录写回后，同一磁盘对象仍能创建多个 FatInode/PageCache
  -> rename 目录项指向源簇
  -> 新路径却读旧目标 cache

canonical inode + 变更时重键
  + backend 共享 FileContent
  + target detach/延迟簇释放
  -> 命名空间、数据内容、打开 fd 生命周期和磁盘持久性重新一致
```

这条链路说明 FAT 写入验收必须从“命令成功”一直追到“另一读取者、另一 FS 实例和
下一次启动看到同一状态”。只有这一层证据成立，P2 才能从一次性写探针升级为可信
的用户态 scratch。
