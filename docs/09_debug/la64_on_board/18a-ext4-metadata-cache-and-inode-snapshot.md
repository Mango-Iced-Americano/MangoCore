---
title: "Ext4 metadata cache 所有权、inode 快照与延迟回收复盘"
category: debug
status: resolved-with-known-limits
author: MangoCore Team
last_update: 2026-07-15
tags: [postmortem, ext4, metadata-cache, inode, unlink, rmdir, page-cache, apk]
code_paths:
  - "os/src/fs/ext4/ext4fs.rs"
  - "os/src/fs/ext4/meta_cache.rs"
  - "os/src/fs/ext4/layout.rs"
  - "os/src/fs/ext4/balloc.rs"
  - "os/src/fs/ext4/test.rs"
related_docs:
  - "docs/09_debug/la64_on_board/18-ext4-lazy-init-and-block-group-accounting.md"
  - "docs/09_debug/la64_on_board/09-ext4-variable-dirent-rename.md"
  - "docs/03_fs/ext4.md"
  - "docs/Work_Log.md"
entry_points:
  - "Ext4OSInode::refresh_inode_snapshot"
  - "Ext4OSInode::finalize_removed_inode"
  - "Ext4FileSystem::cleanup_inode_caches_on_unlink"
  - "MetaBlockCache::invalidate_range"
  - "Ext4OSInode::drop"
---

# Ext4 metadata cache 所有权、inode 快照与延迟回收复盘

## 1. 一句话结论

Ext4 持久写故障不只发生在“磁盘位图写错”这一层。MangoCore 同时持有 on-disk
metadata block cache、cached inode、VFS `Ext4OSInode` 快照、PageCache 和弱引用对象表。
提交前，低层目录操作写回了新的父 inode，但上层 VFS 对象仍保留旧快照；下一次操作
可能把旧 size/extent/link count 写回。另一方面，已经释放并转交新所有者的物理块仍可
留在 metadata cache，旧 dirty entry 以后 flush 时能够覆盖新文件。unlink/rmdir 还存在
链接数传播与“有活引用时何时回收”的分叉不一致。

`b6c5c973` 通过刷新父 inode 快照、统一删除终结路径、按链接数和活对象延迟回收、释放
物理块时立即失效 metadata cache，恢复了“最新元数据只有一份权威状态”和“所有权转交
必须切断旧缓存写权限”两条核心不变量。代码级缺陷是确定的；修复后整批回归和 fsck
clean 也确定。但历史 APK 单次故障没有逐块 flush 时间线，不能唯一判定哪一次旧缓存
写回是首个破坏点。

## 2. 问题卡

| 属性 | 结论 |
|------|------|
| 上层现象 | APK 原子提交期间路径成批消失、重复目录项、类型错误，偶发操作当次仍返回成功 |
| 故障类型 | 缓存身份/快照一致性、物理块所有权转交、unlink/rmdir 生命周期 |
| 严重性 | Critical：旧 dirty metadata 可覆盖已经重新分配的块 |
| 代码级根因 | 父 inode 旧快照、释放块未 invalidate、删除终结路径不统一、rmdir links/reclaim 不闭合 |
| 修复提交 | `b6c5c973aec727539df32592841e5bb06aefa45d` |
| 专项代码覆盖 | `test_rmdir_link_counts_and_reclaim`、`test_freed_metadata_block_is_invalidated` |
| 执行证据边界 | 仓库没有保存上述内部测试函数的独立运行输出，不能把“存在”写成“已执行 PASS” |
| 系统级闭环 | 双架构全新 ext4 `63/63`、离线 fsck clean、APK fixture clean、实板 P4/iozone PASS |

## 3. 五层状态模型

一次 ext4 目录/文件操作可能同时触达五层：

```text
VFS Ext4OSInode snapshot
  -> inode_cache: CachedExt4Inode
  -> metadata block cache: inode table / directory / extent / bitmap block
  -> PageCache: ordinary file data pages
  -> block device
```

旁边还有三个注册表：

```text
inode_objects: ino -> Weak<IndexNode>
children / negative_dentry: name lookup acceleration
page_caches: ino -> Weak<NewPageCache>
```

缓存不是问题本身。问题在于每层的所有权和失效协议不明确时，“同一个 inode 的旧副本”
与“同一个物理块的旧所有者”可以在未来重新写回。

## 4. 两个必须区分的身份

### 4.1 逻辑对象身份：`inode number`

inode cache、VFS 对象和 PageCache 以 inode 号关联。inode 被 unlink 但仍有 fd/mmap 引用时，
逻辑对象仍然存在；此时不能因为目录名称消失就立刻回收数据块或把同号 inode 发给新文件。

### 4.2 物理块所有权：`physical block number`

metadata cache 以物理块号关联。目录块或 extent-tree block 释放后，allocator 可以把相同
物理块给普通文件。即使 cache entry 是“干净”的，也不能继续代表新所有者；若 entry 是
dirty，后果更直接：

```text
old directory block P cached dirty
  -> directory deleted, P bitmap bit cleared
  -> allocator gives P to file B
  -> B writes new data
  -> old cache later flushes directory bytes to P
  -> B is silently overwritten
```

因此“bitmap 已正确清位”不等于所有权转交完成；旧缓存必须同步失效。

## 5. 根因一：低层写回后父 inode 快照仍旧

### 5.1 旧路径为什么会覆盖新状态

`Ext4OSInode` 内部保存一个 `Ext4InodeRef` 快照。低层 `dir_add_entry()` 等 helper 常用
新读取的 inode ref 扩展目录、更新 size/extent，再写进 inode cache。提交前，上层 `self`
中的快照没有随之刷新。

后续操作若再次把 `self.inode.lock()` 当作权威 parent 传入低层，就可能形成：

```text
cache authoritative parent: size=S2, extent=E2, links=L2
VFS stale parent snapshot:  size=S1, extent=E1, links=L1

next operation mutates stale snapshot
  -> write_back_inode(stale + delta)
  -> S2/E2/L2 中上一操作的变化被覆盖
```

这类错误特别适合在 APK 中出现：同一大目录连续 create/symlink/link/rename，单步调用都
可能返回 0，但第二步把第一步刚扩展的目录 inode 覆盖掉。

### 5.2 修复点

`refresh_inode_snapshot()` 以 inode number 从 `inode_cache` 获取新快照，并覆盖 VFS 对象
中的 inode 内容。`b6c5c973` 在以下低层操作成功后调用它：

- `create()` / `create_with_attrs()`；
- `symlink()`；
- `link()`；
- 同目录 `rename()`；
- 跨目录 `rename()` 的 old parent 与 new parent。

刷新发生在低层已成功写回之后，不把缓存失效当作事务回滚替代品。

## 6. 根因二：释放块仍保留旧 metadata cache entry

### 6.1 提交前的缺口

`balloc_free_blocks()` 清 bitmap、增加 free count，却没有清除 `meta_block_cache` 中覆盖该
物理范围的 entry。目录块和 extent-tree block 都复用该 cache；因此后续重新分配同一块
时，旧 entry 仍可能命中或延迟 flush。

### 6.2 修复协议

修复后只有实际发生 1→0 的释放范围才执行：

```rust
self.meta_block_cache.invalidate_range(current_block as usize, span);
```

然后才提交 bitmap 和计数变化。`MetaBlockCache::invalidate_range()` 按半开区间
`[start, start + count)` 删除所有 entry。

这里的语义不是“让缓存更快更新”，而是撤销旧所有者的写权限。新所有者下次读取同一
block id 时必须从设备或新写入重新建立内容。

### 6.3 专项测试代码能证明什么

`test_freed_metadata_block_is_invalidated()` 构造 block 23：

```text
cache dirty [0xA5; 64]
  -> invalidate block 23
  -> 新 owner 的 loader 返回 [0x5A; 64]
  -> 断言读取到的全部是 0x5A
```

该函数精确覆盖缓存所有权转交。但仓库日志中没有它的独立运行输出，当前只能写成
“提交包含定向测试代码”，不能写成“定向测试已执行 PASS”。系统级证据见 §11。

## 7. 根因三：unlink/rmdir 的链接数与活对象生命周期没有统一终点

### 7.1 unlink 名称消失不等于 inode 可立即回收

Linux 语义允许：

```text
open(file) -> unlink(file) -> fd continues I/O -> close(fd) -> final reclaim
```

若 links 归零但仍有活 `Ext4OSInode`，数据和 inode slot 必须保留到最后一个 Arc 释放。
若没有活对象，继续等待 Drop 又会泄漏永远不可达的 inode。因此必须同时判断：

```text
links_count == 0 ?
live VFS object exists ?
```

### 7.2 `finalize_removed_inode()` 统一分叉

修复后的 helper 执行：

1. 写回新的 links count；
2. 若有活 `Ext4OSInode`，把 links count 同步到该对象快照；
3. links 非零：结束，不清理缓存；
4. links 为零：清理软 cache、从全局 page-cache registry 注销；
5. 若仍有 live object：保留 inode/data，由最后 Drop 回收；
6. 若无 live object：立即 truncate、释放 inode、移除 object/inode cache。

rename 覆盖目标、unlink 和 rmdir 都走同一终结路径，避免三处实现逐渐分叉。

### 7.3 rmdir 的两条链接

空目录创建后通常有：

```text
parent/name -> child
child/.     -> child
child/..    -> parent
```

rmdir 会删除 parent 的命名引用和 child 自身的 `.`，所以 child links 必须从 2 归零；同时
parent 因 `..` 消失而减一。旧路径只依赖低层 unlink 的一次减计数，可能让不可达目录以
links=1 残留，进而无法回收。

`b6c5c973` 显式：

- parent links `saturating_sub(1)` 并写回；
- child links 设置为 0；
- 再交给统一终结路径决定立即或延迟回收。

`test_rmdir_link_counts_and_reclaim()` 的代码断言 parent links 恢复、child links=0、持有
child Arc 时 free inode 尚未恢复、drop 后恢复。与 T10 一样，没有保存单独执行日志。

## 8. 为什么 unlink 后不能粗暴清空所有缓存

`cleanup_inode_caches_on_unlink()` 有一个看似反直觉的边界：不重置 `cached_file_size`，
也不直接销毁仍被 live object 持有的 PageCache。

原因是 mmap/fd 可以在名称删除后继续访问文件。若此时 metadata() 回退读取已经释放或
清零的磁盘 inode，可能得到 `size=0`，随后缺页路径将合法映射误判为 BeyondEOF 并发
`SIGBUS`。

因此需要区分：

- **从全局 registry 注销**：防止新同号 inode 取得旧 PageCache；
- **live object 自持 PageCache**：保留旧文件的打开后删除语义；
- **最后 Drop**：writeback、truncate、free inode、清对象表和 inode cache。

这不是“有缓存就全清”可以正确解决的问题。

## 9. Drop 路径的防御性检查

`Ext4OSInode::drop()` 看到本地快照 links=0 时，不立即相信它，而是从 inode cache 重读
权威快照：若 cache 显示 links>0，则记录 stale-snapshot warning 并跳过 free。

这是防止双重/错误释放的最后一道保险，不替代正常路径的 `refresh_inode_snapshot()`。
正常修复负责让快照一致；Drop 检查负责在遗漏发生时 fail-safe。

## 10. 调试追溯与替代假设

### 10.1 追溯顺序

```text
APK 连锁 ENOENT / 路径重复
  -> 离线 fsck 证明 on-disk metadata 已损坏
  -> 审计目录 framing/checksum（独立文档 09）
  -> 审计 bitmap/group counters（独立文档 18）
  -> 继续追问：正确释放的块是否仍可被旧 cache 写回？
  -> 找到 free 未 invalidate metadata cache
  -> 追问：低层 inode cache 更新后，上层 parent snapshot 是否同步？
  -> 找到连续目录操作可用旧快照覆盖新状态
  -> 统一 unlink/rmdir/rename-overwrite 的最终回收路径
```

### 10.2 替代假设矩阵

| 假设 | 证据 | 状态 |
|------|------|------|
| 纯目录项 `rec_len` 错误解释全部损坏 | 释放块 cache 和父快照缺陷不依赖 `rec_len` | 排除“单一根因”；目录 framing 仍是独立真缺陷 |
| 只要 bitmap free 正确，块就安全 | dirty cache 可在所有权转交后写回 | 明确否定 |
| unlink 时立即清所有 cache 最安全 | open-unlinked fd/mmap 仍需旧 size/data | 明确否定 |
| 只是 negative dentry 过期 | 关机后 fsck 仍见磁盘错误 | 排除纯查找缓存问题 |
| 实板 AHCI 写缓存乱序 | 相同代码在两架构 QEMU fixture 也接受 fsck 门禁 | 不是代码缺陷成立的必要条件；未证明 AHCI 所有边界 |
| 某一次旧 metadata flush 是历史 APK 的第一处破坏 | 没有保存 block-id/flush 时间序列 | 未证实，保持未知 |

## 11. 修复为何有效

修复把缓存一致性转换成两个明确协议。

### 11.1 逻辑 inode 协议

```text
低层修改 inode cache
  -> 刷新持久 VFS parent snapshot
  -> links=0 时同步所有 live object
  -> live refs 存在则延迟物理回收
  -> last Drop 再回收
```

### 11.2 物理块协议

```text
bitmap 1->0 确认释放
  -> invalidate 旧 metadata cache entry
  -> 更新 bitmap/checksum/counters
  -> 后续 allocator 可把 block 交给新 owner
```

前者阻止旧 inode 快照覆盖新目录状态，后者阻止旧物理块内容跨所有者写回。两条链不能
互相替代。

## 12. 验证矩阵

| 层级 | 证据 | 结果 | 边界 |
|------|------|------|------|
| 代码 diff | `b6c5c973^..b6c5c973` 的 `ext4fs.rs` / `meta_cache.rs` / `balloc.rs` | 可定位修复 | 证明代码变化，不是运行结果 |
| 定向测试源码 | T9 rmdir reclaim；T10 freed metadata reuse | 覆盖逻辑存在 | 无独立运行日志，不标 PASS |
| LA64 fixture | `logs/ext4-fs-test-la64-fixed-20260715.log` | `63/63`, rc=0 | 用户态整批语义；不单独隔离 T9/T10 |
| RV64 fixture | `logs/ext4-fs-test-rv64-fixed-20260715.log` | `63/63`, rc=0 | 双架构回归 |
| 生命周期用例 | 两份 fixed 日志中的 unlink-open、reclaim、truncate tests | PASS | 覆盖 PageCache 生命周期的用户态路径 |
| 离线检查 | `logs/fsck-ext4-fs-test-{la64,rv64}-fixed-20260715.log` | 五阶段 clean | 关机后独立检查 on-disk metadata |
| APK fixture | `logs/ext4-apk-fsck-fixed-20260715.log` | clean | 目标小文件压力的整批修复后结果 |
| 实板 P4 | `logs/ext4-apk-board-final-20260715.log` | copy/cmp/truncate/delete/iozone PASS | 2K1000LA+AHCI 集成，不是缓存单变量实验 |

fixed `fs_test` 日志末尾明确包括：

```text
[60/63] lifecycle: unlink while open
[61/63] reclaim: clean page cache shrink
[62/63] reclaim: dirty page no-loss
[63/63] truncate: invalidates pagecache
=== FS Test: 63/63 passed ===
```

它们为生命周期提供行为证据；仍不能替代“释放 metadata block 23”这种内部定向测试的
独立执行记录。

## 13. 已知边界

- 没有 preserved log 证明 `test_rmdir_link_counts_and_reclaim()` 和
  `test_freed_metadata_block_is_invalidated()` 在本轮实际被 runner 调用；这里只引用测试
  源码覆盖，不虚构 PASS。
- 没有对父 snapshot、block invalidation、rmdir reclaim 分别做单变量回退 A/B；历史 APK
  incident 的贡献无法拆分。
- 没有保存历史损坏时“物理块 P 被释放、重分配、旧 cache flush”的 block-id 时间线，
  因而系统级损坏与某次具体延迟 flush 之间是机制一致，不是单次现场重演。
- inode/page/meta cache 仍有各自容量和回收策略；本轮只闭合所有权与生命周期，不是完整
  cache model checker。
- 没有 journal，掉电时 rename/unlink 的跨块原子性仍不保证。
- 并发压力主要来自单核任务切换；尚未做多核并发目录修改验证。

## 14. 闭合证据链

```text
用户态操作短期成功，但离线 fsck 看到真实磁盘损坏
  -> 不能停在 dentry/page cache 表象
  -> 代码确认 parent inode 同时存在“新 inode_cache + 旧 VFS snapshot”
  -> 代码确认 freed physical block 的旧 metadata cache entry 未失效
  -> 代码确认 unlink/rmdir/rename-overwrite 的 links/live-object 分叉不统一
  -> b6c5c973 刷新 snapshot、统一 finalize、按 live refs 延迟回收、free 时 invalidate
  -> 双架构全新 fixture 63/63
  -> 关机后两份 fsck clean
  -> APK fixture clean + 实板 P4/iozone 整批通过
```

组会中最重要的一句是：**缓存一致性的本质不是“什么时候 flush”，而是“旧副本在对象或
物理块换主人以后是否还拥有写回权”。**

## 15. 复核命令

```bash
git diff b6c5c973^ b6c5c973 -- \
  os/src/fs/ext4/ext4fs.rs \
  os/src/fs/ext4/meta_cache.rs \
  os/src/fs/ext4/balloc.rs \
  os/src/fs/ext4/test.rs

rg -n "refresh_inode_snapshot|finalize_removed_inode|invalidate_range|cleanup_inode_caches" \
  os/src/fs/ext4

rg -n "unlink while open|dirty page no-loss|63/63" \
  logs/ext4-fs-test-{la64,rv64}-fixed-20260715.log
```
