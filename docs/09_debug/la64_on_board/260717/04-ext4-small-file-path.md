---
title: "2K1000LA ext4 小文件生命周期高固定税复盘"
category: debug
status: confirmed-deferred
author: MangoCore Team
last_update: 2026-07-17
tags: [ext4, vfs, page-cache, sata, flush, python, fileio, performance]
code_paths:
  - "os/src/fs/ext4/ext4fs.rs"
  - "os/src/fs/page_cache.rs"
  - "os/src/drivers/block/sata_blk.rs"
  - "os/src/fs/vfs/file.rs"
  - "user/tools/cpython/bench/bm_fileio.py"
related_docs:
  - "docs/09_debug/la64_on_board/260710/18-ext4-lazy-init-and-block-group-accounting.md"
  - "docs/09_debug/la64_on_board/260710/18a-ext4-metadata-cache-and-inode-snapshot.md"
  - "docs/09_debug/la64_on_board/260717/01-python-performance-baseline.md"
  - "docs/03_fs/ext4.md"
---

# 2K1000LA ext4 小文件生命周期高固定税复盘

## 0. 一句话结论

production `bm_fileio` 的 5,000 次 create/write/read/unlink 生命周期占
`46.449/48.677 s`，平均 `9.290 ms/file`。100 文件定向实验平均 `8.924 ms/file`；
操作量扩大 50 倍时耗时扩大 52.05 倍，说明当前是很高的线性固定税，不是匿名页问题
那样的 O(N²)。

缩小实验中 PageCache writeback 和 ext4 metadata 写精确闭合为 128 次 SATA flush，但
SATA read/write/flush 总时间只占 sys 的 23.06%；约 76.94% 仍位于 VFS、ext4、
PageCache、路径查找、分配和复制等软件路径。本项按协作决定等待 develop 分支新驱动，
当前不优化。

## 1. production workload 的阶段拆分

`bm_fileio` 在 P4 `/persist` ext4 上执行四类阶段，正式总时间为 `48.676927 s`：

| 阶段 | 参数 | 时间 | 占总 elapsed |
|------|------|-----:|-------------:|
| 小文件 metadata 生命周期 | 5,000 files，create/write/read/unlink | 46.449049 s | 95.42% |
| 顺序写 + fsync | 10 MiB | 1.664358 s | 3.42% |
| direct write + fsync | runner 定义路径 | 0.225182 s | 0.46% |
| 顺序热读 | 10 MiB | 0.209308 s | 0.43% |
| direct 热读 | runner 定义路径 | 0.066814 s | 0.14% |
| seek/truncate/fsync | 固定操作 | 0.028681 s | 0.06% |

总进程 rusage 为 user `14.389204 s`、sys `34.200066 s`，sys 占 `70.26%`。大文件热读
只需几十到两百毫秒，说明 48.7 s 不能简单归因于 SSD 顺序吞吐；主要成本跟随大量
小文件生命周期。

## 2. 100/5,000 文件缩放排除 O(N²)

诊断版将 workload 缩小到 100 个小文件和 256 KiB 数据：

| 指标 | 100 文件 | 5,000 文件 production | 比例 |
|------|---------:|----------------------:|-----:|
| metadata time | 0.892372 s | 46.449049 s | 52.05× |
| 文件数 | 100 | 5,000 | 50× |
| 单文件成本 | 8.924 ms | 9.290 ms | +4.10% |

若是 O(N²)，文件数 50 倍会接近 2,500 倍耗时；实测约 52 倍，单位成本只增加 4.1%。
结论应写成“固定税很高且线性累积”，不能写成“目录操作平方退化”。

## 3. PageCache、ext4 metadata 与 SATA flush 闭合

100 文件 memory profile 的关键 counter：

| 层 | 事件 | 次数/字节 | ticks 对应时间 |
|----|------|-----------|---------------:|
| SATA | read | 24 req / 98,304 B | 0.004208 s |
| SATA | write | 132 req / 1,081,344 B | 0.013981 s |
| SATA | flush | 128 | 0.169691 s |
| PageCache | write | 103 calls / 166 pages | 计入软件路径 |
| PageCache | writeback | 105 calls / 241 pages | 0.165328 s |
| process | sys | — | 0.814722 s |

数据量关系：

```text
241 dirty/writeback pages × 4096 B = 987,136 B
SATA written bytes                     = 1,081,344 B
差值                                   = 94,208 B
94,208 / 4096                          = 23 metadata blocks

105 PageCache writebacks + 23 metadata blocks = 128 flushes
```

这解释了 128 次 flush 的来源：100 个 60 B 左右的小文件没有被批量合并成少量 writeback，
unlink 前同步写回形成大量 singleton transaction；ext4 元数据又贡献 23 个块。

SATA read/write/flush 合计约 `0.187880 s`，占 `0.814722 s` sys 的 `23.06%`。剩余约
`0.626842 s`、即 `76.94%` 发生在控制器计时之外。把问题全部称为“SSD 慢”会忽略
主要软件固定税。

## 4. 源码热点链

### 4.1 unlink 强制同步脏 PageCache

`Ext4OSInode::unlink` 在删除前调用
`flush_inode_pagecache_if_dirty()->writeback_all()`。对只写几十字节、随后立即 unlink 的
文件，这使每个文件各自形成小 writeback，无法在更多文件间批量聚合。

### 4.2 每次块写后控制器 flush

`SataBlock::write_block` 当前每次块写完成后无条件执行 controller flush。上层 105 次
PageCache writeback 和 23 次 metadata block write 因此直接变成 128 次硬件 flush；
flush ticks 0.169691 s 与 PageCache writeback ticks 0.165328 s 高度一致。

### 4.3 仍未拆开的软件固定税

源码审计还发现这些候选，但没有独立 counter，不能分配精确百分比：

- create miss 重复解析父路径和不存在的叶子；
- MountFS 与 ext4 unlink 重复 lookup；
- ext4 更新时间戳后，VFS `File::touch_modified` 再做 metadata 操作；
- 首次 inode I/O 的 fallback 和 inode/PageCache 状态同步；
- kernel buffer 到用户 buffer 的分段复制；
- 目录项分配、块位图和 inode 元数据锁竞争/串行化。

heap alloc/dealloc 分别约 40,714/40,622 次，但计时只有 0.007796/0.016271 s；page fault
513 次约 0.017892 s。它们存在，但不足以解释 0.815 s sys 主体。

## 5. 非对齐 trap 不是 fileio 剩余瓶颈

旧 100 文件诊断 body 有 4,458 次非对齐 trap、6,270,720 handler ticks，约 0.063 s，
只解释 sys 的约 7.8%。在 strict-aligned 正式矩阵中，5,000 文件 body 已经 0 trap，
`bm_fileio` 仍需 `43.127 s`，sys `34.418 s`，metadata `41.170 s`。

因此 ext4 小文件路径与非对齐问题独立存在。不能把 strict 后残留的 43 s 解释成 handler
没有完全消失，也不能把旧 fileio 4,458 次与 strict 5,000 文件直接计算 trap 下降百分比，
因为 workload 规模不同。

## 6. ext4 最终运行和检查边界

本轮所有正式 workload 最终在 P4 ext4，未使用 FAT32 成绩替代。矩阵后完成：

- `/persist` 仍为 ext4 rw；
- benchmark bundle SHA-256 未变化；
- workdir 清空；
- 末尾 `sync` 约 0.062 s；
- 复位后 canonical bundle 哈希仍一致；
- 日志无 panic、hang、ext4 error 或内容校验失败。

但 P4 当时在线挂载并参与测试，没有执行 offline `e2fsck -fn`、断电恢复或 fault
injection。当前实现没有完整 journal/recovery 语义。因此“在线 workload 正确且 sync
后跨重启持久”成立，“断电安全、fsck clean、元数据崩溃一致性”不在本轮证据内。

## 7. 为什么当前暂停

队友正在 develop 分支迁移/替换 ext4 实现。当前分支继续微调旧路径容易形成无法迁移的
局部优化，也会让新旧驱动对照输入变化。后续新实现到位后应直接复用相同输入：

- 5,000 个小文件 metadata；
- 100 文件 memory/core profile；
- 相同 P4 ext4 分区、suite hash 和 file sizes；
- PageCache writeback、metadata block、SATA flush 和 sys 拆分；
- 在线测试后再加离线 e2fsck 和故障恢复检查。

## 8. 原始证据

- production 正式样本：[`cpython_bench_fileio-1-db88d12b.log`](raw-data/20260716T102350Z-cpython-ext4-production/raw/cpython_bench_fileio-1-db88d12b.log)
- production 阶段表：[`cpython_bench_phases.csv`](raw-data/20260716T102350Z-cpython-ext4-production/reports/cpython_bench_phases.csv)
- 100 文件 core：[`ext4_fileio_scaled_core-1-88d95eef.log`](raw-data/20260716T-cpython-deepdiag/raw/ext4_fileio_scaled_core-1-88d95eef.log)
- 100 文件 memory：[`ext4_fileio_scaled_memory-1-cbbb3d8d.log`](raw-data/20260716T-cpython-deepdiag/raw/ext4_fileio_scaled_memory-1-cbbb3d8d.log)
- 计数器表：[`counter_deltas.csv`](raw-data/20260716T-cpython-deepdiag/reports/counter_deltas.csv)
- strict fileio：[`cpython_bench_fileio-1-04d12d92.log`](raw-data/20260717T042020Z-cpython-strict-align/raw/cpython_bench_fileio-1-04d12d92.log)
