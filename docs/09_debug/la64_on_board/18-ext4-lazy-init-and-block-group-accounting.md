---
title: "Ext4 lazy-init、块组字段宽度与累计计数复盘"
category: debug
status: resolved-with-known-limits
author: MangoCore Team
last_update: 2026-07-15
tags: [postmortem, ext4, allocator, bitmap, block-group, inode, apk, fsck]
code_paths:
  - "os/src/fs/ext4/balloc.rs"
  - "os/src/fs/ext4/block_group.rs"
  - "os/src/fs/ext4/ialloc.rs"
  - "os/src/fs/ext4/superblock.rs"
  - "os/src/fs/ext4/ext4fs.rs"
related_docs:
  - "docs/09_debug/la64_on_board/09-ext4-variable-dirent-rename.md"
  - "docs/09_debug/la64_on_board/18a-ext4-metadata-cache-and-inode-snapshot.md"
  - "docs/03_fs/ext4.md"
  - "docs/Work_Log.md"
entry_points:
  - "Ext4FileSystem::load_block_bitmap_for_allocation"
  - "Ext4FileSystem::balloc_free_blocks"
  - "Ext4FileSystem::load_inode_bitmap_for_allocation"
  - "Ext4FileSystem::ialloc_alloc_inode"
  - "Ext4FileSystem::current_superblock"
---

# Ext4 lazy-init、块组字段宽度与累计计数复盘

## 1. 一句话结论

2026-07-15 的 APK 小文件压力暴露了 ext4 持久写链中的一组独立分配器缺陷：旧实现
把带 `EXT4_BG_BLOCK_UNINIT` / `EXT4_BG_INODE_UNINIT` 的 lazy bitmap 当成已初始化
位图直接分配；若干块组描述符高半字段使用了错误位移或错误目标字段；块与 inode
释放没有按真实 bitmap 状态和块组边界计数；连续操作还可能从挂载时的旧 superblock
副本重新起算。上述错误都能由提交前后代码直接证明，它们共同具备制造“位图、块组
描述符、superblock 三者互相矛盾”的能力。

提交 `b6c5c973` 修复了这些确定的代码缺陷，并由全新 fixture、双架构 `fs_test 63/63`、
离线 `e2fsck -fn`、QEMU 组回归和实板 P4 压力完成系统级闭环。但历史 APK 那一次
`failed to commit ... ENOENT` 没有保留故障时每个 bitmap 和 group descriptor 的原始
快照，也没有对每个修复做逐项回退 A/B，因此不能声称某一个分配器缺陷单独解释了全部
历史损坏，更不能给各缺陷分摊百分比。

## 2. 问题卡

| 属性 | 结论 |
|------|------|
| 现象入口 | APK 大量创建、替换文件后连锁 `failed to commit ... No such file or directory` |
| 硬损坏证据 | 修复前 fixture 的离线 fsck 报目录类型错误、重复名称及 filesystem still has errors |
| 影响面 | 使用现代 mkfs lazy-init 的 ext4；大容量/64-byte descriptor；跨组释放和高频 inode 复用 |
| 严重性 | Critical：可能把元数据块分给普通文件，或让汇总计数永久漂移 |
| 确定根因 | 8 类可由旧代码直接证明的分配/计数缺陷，见 §5 |
| 修复提交 | `b6c5c973aec727539df32592841e5bb06aefa45d` |
| 修复后硬门禁 | 双架构全新 ext4 fixture `63/63`，关机后 fsck 五阶段 clean |
| 证据边界 | 没有保存历史单次 APK 失败前后的 bitmap dump；没有逐缺陷单变量运行日志 |

## 3. 为什么 lazy bitmap 不能“读出来就用”

### 3.1 块组里同时存在三种真值

ext4 空闲空间状态至少有三层表示：

1. block/inode bitmap：每一个对象是否已占用；
2. block group descriptor：该组的 free blocks/free inodes、used dirs、flags；
3. superblock：整个文件系统的 free blocks/free inodes 汇总。

正常分配一次块，三者应满足：

```text
bitmap[bit] : 0 -> 1
group.free_blocks : G -> G - 1
super.free_blocks : S -> S - 1
```

释放则反向变化，但只有 bitmap 原来为 1 时才允许增加汇总计数。只改任一层，短期内
路径查找可能仍成功，后续分配或 fsck 才会看到矛盾。

### 3.2 `*_UNINIT` 的真正含义

现代 `mke2fs` 可以在 group descriptor 中设置：

- `EXT4_BG_BLOCK_UNINIT`：该组 block bitmap 尚未生成最终内容；
- `EXT4_BG_INODE_UNINIT`：该组 inode bitmap 尚未生成最终内容；
- `EXT4_BG_INODE_ZEROED`：inode table 是否已完成清零，这是另一条状态轴。

这不是“bitmap 块恰好全零，所以所有 bit 都空闲”。lazy-init 允许格式化阶段推迟真实
位图初始化；首次使用者必须根据块组布局构造位图、设置 checksum、清除 flag，并同步
descriptor。若直接在磁盘字节上找第一个 0，可能把以下对象当作普通数据块：

```text
backup superblock / group descriptor table
block bitmap
inode bitmap
inode table
最后一个不完整块组之外的无效 bit
```

这解释了为什么单文件 read/write 冒烟不足以覆盖：group 0 往往已初始化，只有压力增长
到后续 lazy group 才跨过触发边界。

### 3.3 首次初始化的正确构造

`load_block_bitmap_for_allocation()` 在 `b6c5c973` 中建立如下顺序：

```text
读取 group descriptor
  -> 若无 BLOCK_UNINIT，返回缓存位图
  -> 若是 group 0 或 bigalloc，返回 EIO（当前不支持，禁止猜）
  -> 位图清零
  -> 标记本组 super/GDT 领头元数据
  -> 显式标记 block bitmap、inode bitmap、完整 inode table
  -> 把 valid_blocks 之后的尾部 bit 标为已用
  -> 由实际置位数重算 initialized_free
  -> 清 BLOCK_UNINIT、重算 bitmap checksum
  -> 校正 group 与 superblock 计数并写入当前 metadata 状态
```

尾组有效块数不是固定 `blocks_per_group`，而是：

```text
group_first = first_data_block + bgid * blocks_per_group
valid = min(blocks_per_group, blocks_count - group_first)
```

inode bitmap 初始化同理：只在非零组处理 `INODE_UNINIT`，先清零，再把该组有效 inode
之后的尾部 bit 置 1，清 flag 后才允许分配。

## 4. 16/32/64 位字段为什么会静默截断

### 4.1 块组描述符不是所有字段都同宽

32-byte 基础 descriptor 中，计数字段常是低 16 位；64-byte descriptor 才提供对应的
高 16 位。块地址则是低 32 位加高 32 位。正确拼接分别是：

```text
32-bit count = lo16 | (hi16 << 16)
64-bit block = lo32 | (hi32 << 32)
```

旧代码却把 `itable_unused_hi`、`used_dirs_count_hi`、`free_inodes_count_hi` 先左移 32
再截成 `u32`。任何 `hi << 32` 截回 32 位都等于丢弃高半字段。更严重的是旧
`set_used_dirs_count()` 写入的是 `itable_unused_{lo,hi}`，即更新“已用目录数”时篡改了
“inode table 未使用数”。

这里解释的是 ext4 磁盘格式，不表示当前驱动已兼容两种 descriptor 尺寸。当前
`load()`、metadata/disk sync 都按 64 字节 `Ext4BlockGroup` 读写，相关 getter 也没有
用 superblock 的 `desc_size` 屏蔽 32 字节布局中不存在的高字段。因此本轮只支持并验证
64 字节 descriptor；把 32 字节镜像交给当前实现，静态上就可能跨入相邻 descriptor
读写，不能降格描述成“只是还没做交叉实测”。

### 4.2 inode table 和总块数必须保留 64 位

旧 `get_inode_table_blk_num()` 返回 `u32`：

```rust
((hi as u64) << 32) as u32 | lo
```

高 32 位在返回前已被截掉。旧 `Ext4Superblock::blocks_count()` 也有同样问题。即使当前
P4 容量未超过 2^32 个块，这仍是确定的格式解析错误；修复不是为当前 4 GiB 镜像做
特判，而是让 API 类型与 on-disk 字段一致。

### 4.3 这些字段错误是独立缺陷

字段拼接错误与 lazy bitmap 错误可以同时存在，但逻辑上互不依赖：

- 即使所有 bitmap 都已初始化，高字段截断仍会寻址错误；
- 即使所有地址都在低 32 位，`UNINIT` 仍要求首次初始化；
- `set_used_dirs_count()` 写错字段在小文件系统也会破坏 descriptor。

因此修复记录必须把它们分别列出，不能统称为“bitmap bug”。

## 5. 提交前可直接证明的缺陷清单

以下结论来自 `git diff b6c5c973^ b6c5c973 -- os/src/fs/ext4/...`，不是从最终 PASS
倒推出的猜测。

| 编号 | 旧行为 | 违反的不变量 | `b6c5c973` 修复 |
|------|--------|--------------|------------------|
| A1 | block allocator 直接读取 `BLOCK_UNINIT` 位图并找 0 bit | 未初始化字节不能代表所有权 | 首次按布局重建、清 flag、重算 checksum |
| A2 | inode allocator 直接读取 `INODE_UNINIT` 位图 | 可能发布未初始化/越界 inode slot | 初始化有效范围与尾部 bit，再分配 |
| A3 | 16-bit high fields 左移 32 后截成 `u32` | descriptor 计数高半必须左移 16 | `hi16 << 16` |
| A4 | `set_used_dirs_count()` 写 `itable_unused` 字段 | 一个计数不得覆盖另一个字段 | 写 `used_dirs_count_{lo,hi}` |
| A5 | inode table block、superblock block count 返回 `u32` | 物理块号/总块数必须保留 high32 | 返回并传播 `u64` |
| A6 | free range 旧循环的组边界、bitmap 修改和计数依据不一致 | 每组只清本组 bit；重复 free 不得增计数 | 按 `span` 分组，只统计原来置 1 的 bit |
| A7 | 分配/释放常从 `self.superblock` 挂载副本起算 | 一批 N 次变化必须累计为 N，而不是反复覆盖 | `current_superblock()` 读取 metadata cache 或 batch pending 快照 |
| A8 | 新 inode 发布前未清旧 slot | 重用 inode 不得继承旧 mode/links/extent/xattr | 在设置 bitmap bit 前清零完整 inode slot |

这里列为 A1--A8，而不是一个“大根因”，因为每一项都有不同触发条件和损坏形态。

## 6. 跨组释放与重复释放的算术

### 6.1 为什么不能只算起始块组

假设每组 32768 块，释放范围从组尾 `32760` 开始，长度 20：

```text
group N   : index 32760..32767，8 blocks
group N+1 : index 0..11，12 blocks
```

修复后循环每轮计算：

```text
span = min(remaining, blocks_in_this_group - idx_in_group)
```

然后前进 `current_block += span`。每个 descriptor、bitmap checksum 和 free count 都只
接收本组真实变化量。

### 6.2 为什么 `count` 不等于 `freed_count`

若范围中有 3 个 bit 已经为 0，重复调用 free 不能再次增加 3：

```text
requested span = 20
bitmap 1 -> 0 transitions = 17
group/super increment = 17
inode i_blocks decrement = 17 * (block_size / 512)
```

`b6c5c973` 逐 bit 检查，只对 1→0 计数；整段没有任何变化时记录 duplicate-free warning。
这同时阻止 superblock、group descriptor 和 inode `i_blocks` 三处向不同方向漂移。

## 7. 新 inode 为什么必须在“发布前”清零

inode bitmap 的置位相当于把 slot 发布给其他路径。若先置 bit、后清零，或者完全不清零，
重用 slot 可能带着上一个文件残留的：

- 文件类型和权限；
- link count、size、blocks count；
- extent root；
- deletion time、generation 或扩展属性指针。

因此修复顺序是：

```text
find clear bit
  -> 定位 inode_table_block + slot offset
  -> 清零 inode_size 字节
  -> 设置 bitmap bit
  -> 更新 bitmap checksum、group/super counters
  -> 上层初始化新 inode 并写回
```

`zero_allocated_inode_slot()` 还检查 slot 不跨 metadata block；异常布局返回 `EIO`，不在
无法证明安全时继续分配。

## 8. 调试追溯

### 8.1 从用户态 ENOENT 转向离线磁盘证据

起点是 APK 在大量小文件提交阶段连续报告路径消失。单看 `ENOENT` 容易误判为 mkdir、
rename 或缓存查找问题。本轮把压力后的镜像关机并执行宿主 `e2fsck -fn`，修复前日志
`logs/ext4-apk-fsck-20260715.log` 出现：

```text
Pass 1: Checking inodes, blocks, and sizes
Pass 2: Checking directory structure
Entry ... has an incorrect filetype ...
Duplicate entry ... found.
...
WARNING: Filesystem still has errors
```

这一步证明故障越过了 VFS 内存视图，已经落到 on-disk metadata；但它仍不能指出第一处
破坏发生在哪个 allocator 调用。

### 8.2 用“磁盘不变量”拆分代码审计

随后不是继续在损坏镜像上试命令，而是逐层核对：

1. bitmap flag 是否允许直接读；
2. 元数据保留区是否可能被当作空闲；
3. descriptor 高低字段是否按真实宽度拼接；
4. group 与 super summary 是否从当前值累计；
5. free 是否以真实 1→0 数量更新；
6. inode 重用是否先清 slot；
7. metadata cache 在物理块释放时是否移交所有权。

第 7 项属于另一条缓存所有权链，详见
[`18a-ext4-metadata-cache-and-inode-snapshot.md`](18a-ext4-metadata-cache-and-inode-snapshot.md)。
目录项 framing/checksum 则单列在
[`09-ext4-variable-dirent-rename.md`](09-ext4-variable-dirent-rename.md)。

### 8.3 修复后换全新 fixture，而不是修补旧盘

修复验证使用全新 256 MiB ext4 fixture，并通过 chroot 强制 `/tmp*` 路径真正落在被测
ext4 上。LA64 和 RV64 各跑 `fs_test 63/63`；关机后分别对镜像执行 `e2fsck -fn`，五阶段
完成且退出 0。原 APK 压力的修复后 P4 fixture 也得到：

```text
MANGO_STATE: 2427/262144 files (0.1% non-contiguous),
             39033/1048576 blocks
```

对应 `logs/ext4-apk-fsck-fixed-20260715.log`，没有修复前的 error warning。

## 9. 替代假设及其状态

| 假设 | 证据 | 结论 |
|------|------|------|
| AHCI 把正确数据写错位置 | 同批修复在 rv64/LA64 QEMU 全新 virtio fixture 同样受测；fsck clean | 不是这些代码缺陷的必要条件；不能仅凭此宣布 AHCI 永无问题 |
| 只是 APK 自身重复创建名称 | 修复前 fsck 已见真实重复/类型损坏；代码又存在确定的 allocator 不变量违反 | APK 是压力触发器，不足以解释内核代码错误 |
| 只有目录 rename 有问题 | allocator、字段宽度、累计计数缺陷不依赖 rename | 排除“单一 rename 根因” |
| 只是内存 dentry cache 过期 | 关机后的宿主 fsck 仍见损坏 | 排除纯内存显示问题 |
| lazy bitmap 在当前镜像没有实际触发 | 没有保存逐组 flag/首次分配遥测 | 无法证明历史单次贡献；代码缺陷本身确定存在 |
| 高 32 位截断导致这次 4 GiB P4 故障 | 当前容量通常无需 high32 | 这是必须修的通用缺陷，但不能归为本次单次主触发 |

## 10. 修复为何有效

修复不是在 APK 路径重试，也不是遇到 `ENOENT` 后补建目录，而是恢复四个不变量：

1. **所有权不变量**：只有初始化完成且 bitmap=0 的有效数据块/inode 才能分配；
2. **宽度不变量**：on-disk high/low 字段按规范宽度拼接，不在 API 边界截断；
3. **累计不变量**：后一次更新从前一次的当前快照继续，不从挂载副本重新开始；
4. **重用不变量**：释放只计算真实状态转移，新 inode 发布前不携带旧对象内容。

由此，bitmap、group descriptor、superblock 和 inode `i_blocks` 的变化重新指向同一事实。
离线 fsck 正是在独立实现中重算这些关系，因此它比“同一内核能重新读到自己写的数据”
更强。

## 11. 验证矩阵

| 层级 | 证据 | 结果 | 能证明什么 |
|------|------|------|------------|
| 代码审计 | `b6c5c973^..b6c5c973` 的五个 ext4 文件 diff | PASS | A1--A8 的旧代码和修复均可定位 |
| 编译 | Docker 中 LA64、RV64 kernel build 严格串行 | PASS | 两架构编译闭合 |
| 全新 fixture | `logs/ext4-fs-test-la64-fixed-20260715.log` | `63/63`, rc=0 | LA64 用户态文件语义整批回归 |
| 全新 fixture | `logs/ext4-fs-test-rv64-fixed-20260715.log` | `63/63`, rc=0 | RV64 用户态文件语义整批回归 |
| 独立磁盘检查 | `logs/fsck-ext4-fs-test-{la64,rv64}-fixed-20260715.log` | 五阶段 clean | 关机后的 on-disk 一致性 |
| APK 压力 | `logs/ext4-apk-fsck-fixed-20260715.log` | clean, 2427 inodes | 修复后目标小文件工作负载没有再留下 fsck 错误 |
| QEMU 集成 | `logs/fs-regression-{la64,rv64}-final-20260715.log` | basic/iozone/libctest rc=0 | 超出自制 fs_test 的集成回归 |
| 实板 | `logs/ext4-apk-board-final-20260715.log` | P4 16 MiB 探针、truncate、iozone PASS | 2K1000LA+AHCI 平台上的最终整批行为 |

## 12. 已知边界

- 没有为 A1--A8 每项保存“只回退这一项”的单变量镜像与 fsck 日志；系统级 PASS 证明
  整批修复后的状态，不证明每项对历史 APK incident 的独立贡献。
- 没有保存修复前故障现场的 block/inode bitmap、group descriptor 和首次损坏块号，
  因此不能重放“第一处写坏”时间线。
- `bigalloc` 明确不支持；遇到 lazy block bitmap 时返回 `EIO`，不是已完成适配。
- group 0 出现 `BLOCK_UNINIT` / `INODE_UNINIT` 被当作异常拒绝，尚无损坏恢复策略。
- 64 位字段拼接已修正，但本轮没有 >16 TiB 级 ext4 实盘验证；当前 P4 不覆盖 high32
  非零的真实寻址。
- 当前 loader、getter 和 sync 路径只安全支持本轮已验证的 64-byte group descriptor；
  32-byte 布局可能跨读写相邻 descriptor，属于静态可见的未支持格式，而非单纯缺少
  交叉实测。后续必须按 `desc_size` 使用正确 stride、屏蔽高字段并补 32/64-byte 矩阵，
  或在挂载时显式拒绝 32-byte 镜像。
- `os/src/fs/ext4/test.rs` 有内部测试代码，但本轮没有保存 allocator 各子项的独立输出；
  不能把“测试函数存在”写成“该函数已执行 PASS”。
- ext4 当前没有 journal；这里恢复的是运行期元数据一致性，不等于获得掉电事务原子性。

## 13. 闭合证据链

```text
APK 大量小文件后连锁 ENOENT
  -> 关机后宿主 fsck 仍报目录/引用一致性错误
  -> 证明不是纯 VFS 缓存显示问题
  -> 按 bitmap/group/super/inode 四层不变量审计
  -> 旧代码直接确认 lazy bitmap、字段宽度、累计计数、跨组/重复 free、slot 重用缺陷
  -> b6c5c973 从根上恢复初始化、寻址、累计和重用顺序
  -> 全新 fixture 双架构 63/63
  -> 两个镜像关机后独立 fsck clean
  -> APK fixture fsck clean + QEMU 组回归 + 实板 P4 探针/iozone
```

最后一条边界同样重要：这条链证明“修复后的整批实现闭合”，不证明历史那一次 APK
损坏由其中某一项单独造成。组会中应把“代码级确定缺陷”和“历史 incident 贡献未知”
同时说清楚。

## 14. 复核命令

```bash
git show --stat b6c5c973
git diff b6c5c973^ b6c5c973 -- \
  os/src/fs/ext4/balloc.rs \
  os/src/fs/ext4/block_group.rs \
  os/src/fs/ext4/ialloc.rs \
  os/src/fs/ext4/superblock.rs \
  os/src/fs/ext4/ext4fs.rs

rg -n "BLOCK_UNINIT|INODE_UNINIT|current_superblock|zero_allocated_inode_slot" \
  os/src/fs/ext4

rg -n "63/63|MANGO_STATE|Pass 5" \
  logs/ext4-fs-test-*-fixed-20260715.log \
  logs/ext4-apk-fsck-fixed-20260715.log \
  logs/fsck-ext4-fs-test-*-fixed-20260715.log
```
