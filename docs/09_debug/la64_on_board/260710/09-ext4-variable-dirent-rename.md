---
title: "ext4 rename 历史干预、目录首记录 framing 与 checksum 根因"
category: debug
status: resolved-with-known-limits
author: MangoCore Team
last_update: 2026-07-15
tags: [postmortem, ext4, rename, dirent, rec-len, checksum, apk, mixed-evidence]
code_paths:
  - "os/src/fs/ext4/direntry.rs"
  - "os/src/fs/ext4/ext4fs.rs"
  - "os/src/fs/ext4/test.rs"
  - "user/src/bin/initproc.rs"
related_docs:
  - "docs/09_debug/la64_on_board/260710/development-log.md"
  - "docs/03_fs/ext4.md"
  - "docs/08_testing/apk-isolated.md"
  - ".agents/skills/mango-workflow/references/harness-patterns.md"
entry_points:
  - "Ext4OSInode::rename"
  - "Ext4FileSystem::try_insert_to_existing_block"
  - "Ext4FileSystem::dir_remove_entry"
  - "remove_dir_entry_record"
  - "Ext4FileSystem::dir_set_csum"
---

# ext4 rename 历史干预、目录首记录 framing 与 checksum 根因

## 1. 摘要

P4 `persist-shell` 曾在 `cp temporary && mv -f temporary final` 后出现 rename 返回 0、
源名和目标名却都不可见；APK 提交 `wcurl`、Python 文件时也出现过 `ENOENT`。提交
`b62828cf` 将同目录 rename 从“先发布新名、再删源名”改为“先删源、再删覆盖目标、
最后发布新名”，并补充回滚；空目标和覆盖目标专项随后均 PASS。

但 2026-07-15 对 ext4 不定长目录项重新做区间算术后，原先用于解释这一结果的
“删除普通旧项会让前驱 `rec_len` 跨过刚插入的新项”并不成立。对非块首记录，add
把旧记录的 `R` 切为 `S + (R-S)`；remove 看到的旧记录长度已经是 `S`，只把前驱
扩大 `S`。扩大后的终点恰好等于新项起点，不会跨过新项。

这意味着必须把结论拆成两层：

- **2026-07-14 已提交阶段事实**：`b62828cf` 是有专项 PASS 支持的顺序干预和回滚
  增强，但其原“slack 必然吞项”解释已被算术反证，不能再写成唯一根因；
- **2026-07-15 当前 HEAD 事实**：继续下钻发现两个可以直接从代码证明的低层缺陷：
  `dir_remove_entry()` 删除块内 offset 0 的首记录时，把自身误作前驱并令自身
  `rec_len` 加倍；目录块 checksum 又错误使用块首目录项 inode，而不是所属目录
  inode。当前提交通过 `remove_dir_entry_record()`、T6/T7 和显式目录 inode 修正。

该修复随整批 ext4 提交完成双架构 63/63、关机后 `e2fsck -fn`、P4 fixture fsck、
QEMU 回归和实板 P4 写入/iozone 回归；这些结果证明当前 HEAD 整体状态显著收敛，但该批次
还包含 bitmap、计数、inode snapshot、metadata cache 等修复，不能把全部集成结果
单变量归功于首记录 framing 或 checksum。

| 属性 | 结论 |
|------|------|
| 严重性 | Critical / P0；目录 framing 损坏可使名称消失并污染后续元数据 |
| 已提交干预 | `b62828cf`，2026-07-14：rename 顺序、回滚、同 inode no-op |
| 历史基线 | `2031fd5909355994f768f845b2935e4509290a07`，仍含首记录/checksum 缺陷 |
| 当前修复 HEAD | `b6c5c973aec727539df32592841e5bb06aefa45d`，`fix(fs): stabilize ext4 persistent writes`，2026-07-15 |
| 被否定口径 | 普通非首记录的 split/merge 会必然跨过新项 |
| framing 根因 | offset 0 时 `prev_offset==offset==0`，旧删除逻辑令自身 `rec_len` 加倍 |
| checksum 根因 | CRC seed 使用块首 child inode，而不是 parent directory inode |
| 精确历史归因 | mixed；缺少故障当时的原始目录块 offset/rec_len dump，不能唯一回填 |

## 2. 证据状态与阅读规则

本文刻意同时记录历史基线与当前 HEAD，避免把较晚根因倒灌成 `b62828cf` 当时已经证明
的结论。

### 2.1 历史已提交阶段：可由 commit 重建

```text
b62828cf  fix(board): stabilize persistent Python and ext4 rename
2031fd59  framing/checksum 修复前的历史基线
```

截至 `2031fd59` 已经存在：

- 同目录 rename 的 source-first 顺序；
- 覆盖目标的延迟 link-count finalize；
- add/remove 失败后的 best-effort rollback；
- source/target 指向同一 inode 时 POSIX no-op；
- `EXT4_RENAME_ABSENT_PASS`、`EXT4_RENAME_OVERWRITE_PASS` 对应的归档结果。

这些都是已落地事实。它们证明该干预在当时两个 focused 布局中有效，不证明旧因果
解释必然正确，也不证明所有多块目录布局均已覆盖。

### 2.2 当前 HEAD：framing/checksum 已提交

当前分支顶部是 `b6c5c973 fix(fs): stabilize ext4 persistent writes` 集成提交，
父提交为 `2031fd59`。该 HEAD 中的 `direntry.rs/ext4fs.rs/test.rs` 包含：

- `remove_dir_entry_record()`；
- T6：块首记录删除必须保留原 `rec_len`；
- T7：非首记录删除必须只并入直接前驱；
- checksum 显式传入 parent directory inode；
- 同批次的 ext4 bitmap、计数、缓存和 inode snapshot 修复。

因此 framing/checksum 已是当前 HEAD 能力；`2031fd59` 只作为修复前历史基线，不能再
称为当前已提交状态。

### 2.3 证据强度

- **直接代码证明**：offset 0 自合并、checksum inode 取错均可逐行推导；
- **算术反证**：普通非首记录不会按旧解释跨过新项；
- **行为证据**：rename 专项、63/63、fsck、QEMU 与实板结果；
- **历史一致性**：APK `ENOENT`、`mv EIO` 与目录损坏相符，但早期还有其他 ext4 根因；
- **缺口**：没有保存原始失败目录块的完整 `block/offset/inode/rec_len/name` 快照。

## 3. 发现与认知修正时间线

### 3.1 历史压力症状

APK 安装 Python 依赖时曾出现：

```text
failed to commit usr/bin/python3: No such file or directory
failed to commit usr/lib/python3.14/socket.py: No such file or directory
failed to commit usr/lib/python3.14/ssl.py: No such file or directory
```

后续 `apk add curl` 在提交 `usr/bin/wcurl` 时也出现过 `ENOENT`。这些 install/commit
路径通常写临时文件后 rename 到最终名字，因此 rename 是合理调查入口。

但当时 ext4 还同时存在 lazy bitmap、块组计数、metadata cache 失效和 inode snapshot
等问题。历史日志只能证明“小文件元数据链损坏”，不能单凭一个 errno 指定唯一根因。

### 3.2 2026-07-14：focused rename 与顺序干预

P4 应用根的 wrapper 发布被缩小为：

```text
cp source temporary
mv -f temporary final
lookup temporary
lookup final
```

曾观察到 rename 返回成功、两个名字随后都不可见。`b62828cf` 改变同目录操作顺序并
补充 rollback，随后归档：

```text
EXT4_RENAME_ABSENT_PASS
EXT4_RENAME_OVERWRITE_PASS
```

这是有效的行为干预证据。当时给出的机制解释是 add/remove 对同一 slack 的切分与
合并发生重叠；这一解释随后进入 Work Log 和注释。

### 3.3 2026-07-15：复算推翻“普通记录必吞新项”

对每个 offset 和长度逐项代数化后发现：只要被删项不是块首记录，旧解释的区间终点
不成立。这个反证迫使调试从 rename 高层顺序重新下钻到 `dir_find_in_block()` 与
`dir_remove_entry()` 的 framing 细节。

### 3.4 继续下钻：发现 offset 0 与 checksum 两个直接缺陷

代码审计随后确认：

1. 块内第一条记录没有前驱，但查找结果仍以 `prev_offset=0` 表示；
2. 删除逻辑无条件把 `prev_offset+4` 当作前驱 `rec_len` 字段；
3. 当目标本身位于 offset 0 时，该地址就是目标自身的 `rec_len`；
4. 旧逻辑因此写入 `R+R`，再把目标 inode 清零；
5. 目录 checksum 还从块首记录取 inode，非首目录块上这个 inode 并不是目录 inode。

这两项不依赖历史症状解释，单看代码即可证明违反 ext4 framing/checksum 约束。

## 4. ext4 目录项原理

### 4.1 `rec_len` 同时承担记录长度与空闲空间

目录块以变长记录串联：

```text
next_offset = current_offset + current.rec_len
used_len    = align4(8 + name_len)
```

一个已使用记录可以让 `rec_len > used_len`，尾部差值就是可复用 slack：

```text
[ inode | rec_len | name_len | type | name ][       slack       ]
<-------------------------- rec_len ---------------------------->
<----------- used_len ----------->
```

目录遍历只沿 `rec_len` 前进。字节仍留在块中不代表目录项可达；反过来，任何错误的
`rec_len` 都可能让遍历跳入记录内部、跨过有效项或越过 checksum tail。

### 4.2 add 的 split

对 offset `O` 上一条原长度为 `R`、实际使用长度为 `S` 的记录，若 slack 足够，
`try_insert_to_existing_block()` 生成：

```text
old: offset O      rec_len S
new: offset O + S  rec_len R - S
```

这是原地切分，不是从独立 free list 分配。

### 4.3 remove 的两种合法情形

Linux/ext4 framing 需要区分：

- **非首记录**：把被删记录整个 span 合并进直接前驱；
- **块首记录**：没有前驱，必须保留该记录原 `rec_len`，只把它标为空闲。

块首记录不等于整个目录的 `.`。多块目录中，每个后续数据块的 offset 0 都是块首
记录，通常是普通 child；APK 大量小文件正会进入这种布局。

## 5. 关键算术反证：普通记录不会按旧解释吞掉新项

### 5.1 设定

设：

```text
P = old 的直接前驱起点
L = 前驱 rec_len，因此 P + L = O
O = old 起点
R = add 前 old.rec_len
S = align4(8 + old.name_len)
```

add 把 old slack 切给 new：

```text
old = [O, O + S)
new = [O + S, O + R)
```

### 5.2 随后删除 old

删除代码此时读取的 `old.entry_len` 已经是 `S`，不是 add 前的 `R`。合法的非首记录
合并后：

```text
new predecessor length = L + S
new predecessor end    = P + L + S
                       = O + S
                       = new start
```

终点恰好落在 new 起点，既不重叠也不跨过 new。

### 5.3 覆盖目标同理

如果 temporary source 被插入 target slack，删除普通非首 target 也只把 target 当前
`used_len` 合并给其前驱，终点仍落在 source 起点。于是：

```text
"source 位于 target slack"
    !=
"remove target 必然把 source 一并隐藏"
```

### 5.4 反证的结论

这不是说 rename 顺序无关，也不是说 focused 现象不存在；它只否定了一个过强命题：

> 对普通非首记录，split 后再 merge 会必然跨过新项。

因此 `b62828cf` 可以被描述为顺序干预、回滚增强和经过专项验证的行为修复，不能再用
上述命题证明它关闭了唯一底层根因。

## 6. 已提交根因修复一：块首记录自合并

### 6.1 `prev_offset=0` 的歧义

旧 `dir_find_in_block()` 初始化：

```text
offset         = 0
prev_de_offset = 0
```

若第一条记录匹配，返回：

```text
result.offset      = 0
result.prev_offset = 0
```

这里的 `prev_offset=0` 不是“前驱真的在 0”，而是“没有前驱”被错误编码成同一个值。

### 6.2 旧删除代码如何把自身长度加倍

旧 `dir_remove_entry()` 无条件执行：

```text
pde_rec_len_addr = prev_offset + 4
current_len      = load_u16(pde_rec_len_addr)
new_len          = current_len + deleted.entry_len
store_u16(pde_rec_len_addr, new_len)
clear deleted.inode
```

目标位于 offset 0、长度为 `R` 时：

```text
prev_offset + 4 = 4              # 实际是目标自身 rec_len 字段
current_len     = R
deleted.len     = R
stored len      = 2R
```

随后 inode 被清零，目录扫描把它视为空闲记录，却按 `2R` 前进。若下一项从 `R` 开始，
扫描将直接跨过它；若 `2R` 越过有效区或 tail，framing 本身已经损坏。这一机制能够产生
“目标被删、相邻源/新项也不可见”，且不需要错误的普通记录 slack 推导。

### 6.3 为什么大目录更容易触发

首个目录块通常从 `.`、`..` 开始，普通文件不在 offset 0。目录扩展到第二、第三块后，
每个新块的第一条普通记录都会位于 offset 0。APK 安装制造大量短生命周期临时文件，
更容易删除非初始块的首记录并破坏相邻项。

但历史故障当时没有保留目标所在 block/offset，因此本文只说该缺陷与症状高度吻合，
不把某一次 2026-07-14 失败事后断言为“已证明目标恰在 offset 0”。

## 7. 已提交根因修复二：目录 checksum 使用了错误 inode

### 7.1 ext4 checksum 的身份对象

目录块 checksum 的 seed 需要：

```text
filesystem UUID
parent directory inode number
parent directory inode generation
directory block bytes excluding checksum tail
```

关键对象是“拥有该块的目录 inode”，不是块中第一条 child 记录的 inode。

### 7.2 历史基线的错误来源

`2031fd59` 的 `dir_set_csum()` 读取块首 `Ext4DirEntry`，再由
`Ext4DirEntry::ext4_dir_get_csum()` 使用 `self.inode`：

```text
first block: first entry is "." -> inode 恰好等于 parent，偶然正确
later block: first entry is child -> inode 是任意 child，错误
after deleting offset 0: first inode becomes 0 -> seed 进一步错误
```

这解释了为什么小目录可能长期不暴露，而 APK 大目录的新增/删除更容易在离线 fsck
中出现目录 checksum/metadata 异常。

### 7.3 当前修复

当前 HEAD 将 API 改为显式传入：

```text
dir_set_csum(block, parent.inode_num, parent.inode.generation())
```

CRC helper 不再从 block payload 推断 owner identity。这是比“首项通常是 `.`”更稳固的
类型边界：调用者必须提供所属目录身份。

## 8. 已提交 framing 修复与 T6/T7

### 8.1 `remove_dir_entry_record()`

新 helper 在修改前验证：

- `entry_len >= fixed header` 且 4 字节对齐；
- `offset + entry_len <= data_end`；
- 非首记录必须满足 `prev_offset < offset`；
- `prev_offset + prev_len == offset`；
- 合并后不能越过有效数据区，也不能溢出 `u16`。

offset 0 分支只清 inode/name/type 等内容，保留原 `rec_len`；非首分支才把 span 并入
直接前驱。这样“是否存在前驱”由 `offset==0` 显式表达，不再依赖含糊 sentinel。

### 8.2 T6：块首 framing

T6 构造 4096B 块，第一条记录位于 offset 0、`rec_len=64`。删除后断言：

```text
rec_len remains 64
inode/body cleared
byte at offset 64 unchanged
```

它直接防止“自合并为 128、跨过下一项”。

### 8.3 T7：非首合并

T7 构造：

```text
predecessor offset=0  rec_len=16
deleted     offset=16 rec_len=32
```

删除后断言前驱长度为 48、被删 span 清零，证明修首记录时没有破坏普通记录合并语义。

T6/T7 已存在于当前 HEAD 源码并随双架构构建通过；仓库没有单独归档包含这两个测试名的
运行输出，因此不能把“源码已定义、编译已通过”夸写成“独立 T6/T7 运行日志已保存”。

## 9. `b62828cf` 应该如何评价

### 9.1 已确定的价值

该提交确实改善了高层事务：

- 先保存 source/target inode snapshot；
- source/target 同 inode时 no-op；
- 删除或发布失败时尝试逆向恢复；
- 被覆盖目标的 link-count/cache finalize 延后到发布成功；
- 跨目录“新名已发布、旧名删除失败”时尝试撤销新名。

这些行为即使在低层 framing 正确后仍有价值。

### 9.2 它没有证明什么

两个 focused PASS 只能证明测试所生成的目录布局通过，不能证明：

- 目标或源覆盖过每个目录块的 offset 0；
- 普通 split/merge 会跨越新项；
- 历史 APK 的所有 `ENOENT` 都来自同一原因；
- 低层 checksum 已正确；
- rename 具备掉电原子性。

因此本复盘将 `b62828cf` 定义为“有行为证据的顺序干预”，而不是以错误算术支撑的
唯一根因提交。

## 10. 根因矩阵

| 假设/缺陷 | 证据 | 当前结论 |
|-----------|------|----------|
| 普通非首 old split 后删除会跨过 new | 前驱终点复算为 `O+S == new_start` | 反证，不成立 |
| 普通非首 target 删除必吞 source | 同一复算适用 | 反证，不成立 |
| 块首删除把自身当作前驱 | `2031fd59` 中 `offset=prev_offset=0`，写回 `R+R` | 直接代码证明 |
| 非首删除仍需并入前驱 | ext4 framing 语义 + T7 | 成立 |
| checksum 可取块首 entry inode | 非初始块首项是任意 child | 反证 |
| checksum 应取 parent directory inode | 当前 HEAD 显式参数 + ext4 identity 语义 | 直接代码证明 |
| `b62828cf` 干预有效 | 空目标/覆盖目标专项 PASS | 对已测布局成立 |
| `b62828cf` 关闭唯一根因 | 算术被推翻，offset0/checksum 直到 2026-07-15 提交才修复 | 不成立 |
| 历史 APK errno 可唯一归因 | 同期存在 bitmap/cache/count 等问题 | 不成立 |

## 11. 验证证据分层

### 11.1 历史 `b62828cf` 阶段

§3.2 已列出空目标与覆盖目标两个归档标签。用例检查 syscall、源名消失、目标名存在、
内容和清理，而不只看返回 0。完整 CPython
P3 + P4 还完成 pip/six/idna 持久复用，说明顺序干预后的常见应用布局可工作。

`b62828cf` 当时的新实板 uImage 已生成，但 Work Log 明确记录尚未执行该轮 focused
实板验收；这部分直接证据来自 QEMU。后续实板 P4 工作负载只能算间接覆盖。

### 11.2 2026-07-15 当前 HEAD 整批回归

当前 `b6c5c973` ext4 批次的 Work Log 记录：

- LA64/RV64 严格串行强制构建通过；
- 两架构在全新 256MiB ext4 fixture、chroot 真实根上均 `63/63`；
- 两个测试镜像关机后 `e2fsck -fn` 五阶段退出 0；
- Python APK 小文件压力对应 P4 fixture 离线 fsck clean；
- 双架构 `basic + iozone + libctest` 均完成；
- 2K1000LA P4 `PASS mode=reuse`；
- 实板完成 16MiB write/sync/copy/cmp/truncate/reopen/delete 与 16MiB iozone。

这些门禁覆盖了“运行时看似成功、关机后磁盘损坏”和“只在 QEMU 成功”的两类假阳性。
但该提交同时修改多项 ext4 元数据逻辑，验证结论应写成：

> 当前 HEAD 整体通过回归，且 framing/checksum 修复有直接代码依据。

不能反向写成：

> 63/63 或实板 iozone 单独证明历史 rename 故障只由 offset 0 引起。

### 11.3 尚缺的单变量证据

最强的后续实验应保留一个旧代码 fixture，确定构造：

1. 多块目录；
2. 目标位于非初始目录块 offset 0；
3. 相邻记录携带可识别名称；
4. 旧 remove 后 dump `rec_len=2R` 与相邻项失联；
5. 只替换 `remove_dir_entry_record()` 后同一 fixture 通过；
6. 分别以外部 e2fsck 验证旧/新 checksum。

当前 HEAD 的 T6/T7 已覆盖核心字节算法，但仓库未保存上述完整 before/after 行为日志。

## 12. 为什么历史症状看起来不稳定

是否命中块首记录取决于：

- 目录是否已扩展为多块；
- 每块记录顺序和名称对齐长度；
- create/unlink 后的 slack 分布；
- APK 临时名与最终名长度；
- 本次删除的 source/target 是否恰在 offset 0；
- checksum feature 是否开启、错误何时被读取或由 fsck 检出。

因此干净小目录可能长期正常，持续安装/删除的目录才暴露。第二次 `apk fix` 也可能因
布局已改变而通过。这是历史布局依赖，不必诉诸并发竞态。

## 13. 可复用调试方法

### 13.1 不定长记录必须先做区间代数

对每次 mutation 记录：

```text
block id
offset
previous offset
inode
rec_len before/after
used_len
name
```

先算端点，再讲“覆盖”“吞项”。本次旧解释之所以持续，是因为只画了 slack 示意图，
没有把 add 后 old 的 `rec_len` 已从 `R` 变成 `S` 代入 remove。

### 13.2 sentinel 必须与合法值可区分

`prev_offset=0` 同时表示“前驱在 offset 0”和“没有前驱”是危险编码。修复选择以
`offset==0` 分支；另一种设计可使用 `Option<usize>`，但不能继续让 0 承担双重语义。

### 13.3 checksum owner 必须由调用关系传递

不要从 payload 第一条记录猜所属对象。目录 inode、generation 等 identity 应从
parent inode 显式传入 checksum helper，并用外部 fsck 作为 oracle。

### 13.4 成功 oracle 必须跨缓存和重启

```text
rename == 0
AND old lookup == ENOENT
AND new lookup succeeds
AND new content == source content
AND sync/remount/reboot preserves result
AND offline e2fsck clean
```

任何只看 syscall 返回值或同一 PageCache lookup 的测试都不足以证明磁盘目录正确。

## 14. 已知边界

1. framing/checksum 已提交到当前 `b6c5c973`；`2031fd59` 与 `b62828cf` 不包含
   该修复。
2. ext4 无 journal replay；运行期 rollback 不等于掉电原子 rename。
3. `b62828cf` rollback 使用普通目录操作，二次 ENOSPC/I/O 失败时仍可能恢复失败。
4. 当前提交的系统回归是多修复组合，不是 framing/checksum 的单变量 fault-injection。
5. 历史失败缺少原始目录块 dump，因此精确 incident attribution 保持 mixed。
6. APK 早期连锁 `ENOENT` 还涉及 lazy bitmap、计数、cache ownership 等独立根因。

## 15. 最终因果链

```text
2026-07-14 focused rename failure
  -> b62828cf 调整顺序、补 rollback
  -> absent/overwrite 专项 PASS
  -> 证明干预对已测布局有效
  -X-> 不能证明普通 slack merge 必然吞项

重新做区间算术
  -> add 后 old.rec_len = S
  -> remove 后 predecessor end = O + S = new start
  -> 原普通记录唯一根因被反证

继续审计块首记录
  -> offset=0 与 prev_offset=0 重合
  -> 旧 remove 把自身 rec_len 从 R 写成 2R
  -> 目录扫描可跨过相邻记录

同时审计 checksum
  -> 非初始块首项是 child，不是 parent directory
  -> 旧 CRC 使用错误 inode identity

当前 HEAD b6c5c973
  -> offset0 保留 rec_len，非首项严格合并直接前驱
  -> checksum 显式使用 parent inode
  -> T6/T7 + 63/63 + offline fsck + QEMU/实板整批回归
  -> 修复已提交并完成整批回归
```

本次最重要的调试结论不是“换一个更像根因的故事”，而是保留算术反证：行为修复通过
不等于机制解释已经证明。对 on-disk 不定长记录，必须同时闭合字节区间、owner identity、
外部 fsck 与跨重启行为证据。
