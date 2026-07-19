---
title: "居民匿名映射显式释放 O(N²) 复盘"
category: debug
status: confirmed-and-quantified
author: MangoCore Team
last_update: 2026-07-17
tags: [memory, mmap, munmap, vma, frame-store, complexity, python, performance]
code_paths:
  - "os/src/mm/vma.rs"
  - "os/src/mm/frame_store.rs"
  - "user/tools/cpython/bench/diag_mmap_release.py"
related_docs:
  - "docs/09_debug/la64_on_board/260717/01-python-performance-baseline.md"
  - "docs/09_debug/la64_on_board/260717/07-strict-runtime-and-anon-unmap-quantification.md"
  - "docs/03_fs/page-cache.md"
  - "docs/09_debug/perf_diag.md"
---

# 居民匿名映射显式释放 O(N²) 复盘

## 0. 结论与证据边界

`Vma::unmap` 释放 resident anonymous pages 时，外层按页枚举，内层
`FrameStore::remove_in_memory` 每次又对整个 `active` vector 执行 `retain`。N 个页面的
扫描量为 `N + (N-1) + ... + 1`，即 O(N²)。2K1000LA 实板的 1/4/16/32/64 MiB 曲线
与代码复杂度一致，64 MiB 关闭需要 `3.893 s`。

已确认的是“显式 VMA unmap 的这一条路径存在平方复杂度”。2026-07-17 追加的实板
`memory_io` 窗口进一步在内核内精确记录每次 `retain` 之前的 active 长度，1/4/16/32/
64 MiB 主映射的扫描步数分别严格等于 `N(N+1)/2`。对六个 strict-aligned Python
正式 workload 的同一计数显示：`bm_list`、`bm_dict`、`bm_bytesio` 分别有 11.29%、
9.69%、4.30% 的 body 时间落在该 `Vma::unmap` 路径；`json_loads` 则近似为 0。

因此“真实 Python 影响未知”已经关闭，但仍不能写成“每个 Python 进程退出都有 O(N²)”：
影响取决于 workload 是否在正式 body 内显式释放 resident anonymous VMA，常规 exec/exit
仍主要走 `clear_no_hole()`。

## 1. 实验设计

### 1.1 为什么必须让页面 resident

只创建匿名 mapping 而不触页，unmap 主要删除 VMA 元数据，无法覆盖 frame store 的
逐页删除。`diag_mmap_release.py` 因此执行：

1. 创建指定大小的匿名可写 mapping；
2. 以页粒度写入，使每个页面实际 resident；
3. 在所有触页完成后开始计时；
4. 只计 `close()/munmap`；
5. 同时读取 page/frame/TLB counter，确认测到的是释放而不是前置缺页。

测试尺寸按 4 KiB 页换算为 256、1,024、4,096、8,192、16,384 页，既覆盖小映射，也
让大尺寸的二次项远大于固定计时开销。

### 1.2 实板结果

| resident mapping | pages | close/munmap | ns/page | ns/page² |
|-----------------:|------:|-------------:|--------:|---------:|
| 1 MiB | 256 | 2.493880 ms | 9,741.72 | 38.05 |
| 4 MiB | 1,024 | 18.798230 ms | 18,357.65 | 17.93 |
| 16 MiB | 4,096 | 239.028990 ms | 58,356.69 | 14.25 |
| 32 MiB | 8,192 | 961.311790 ms | 117,347.63 | 14.32 |
| 64 MiB | 16,384 | 3,893.434490 ms | 237,636.38 | 14.50 |

若复杂度是 O(N)，`ns/page` 应在大尺寸趋于稳定；实际它从约 58 µs/page 增到
238 µs/page。相反，16 MiB 以上 `ns/page²` 稳定在 `14.25–14.50`，与二次模型相符。
小尺寸的 38.05/17.93 ns/page² 较高，是固定 trap、计时和 VMA 成本在分母较小时的
表现，不否定大尺寸渐近趋势。

### 1.3 尺寸倍增关系

| 区间 | 页数倍数 | 时间倍数 | O(N²) 预测 |
|------|---------:|---------:|------------:|
| 4 → 16 MiB | 4× | 12.72× | 16× |
| 16 → 32 MiB | 2× | 4.02× | 4× |
| 32 → 64 MiB | 2× | 4.05× | 4× |

大尺寸连续两次翻倍都约四倍耗时，是最直观的实板复杂度证据。

## 2. 源码复杂度推导

`Vma::unmap` 对待释放范围中的 resident VPN 逐项处理，逻辑等价于：

```text
for vpn in resident_pages_to_remove:
    frame_store.remove_in_memory(vpn)
```

`remove_in_memory` 不是 O(1) 索引删除，而是：

```text
active.retain(|entry| entry.vpn != target_vpn)
```

假设 active 初始有 N 项，且本次全部删除：

```text
第 1 次比较 N 项
第 2 次比较 N-1 项
...
第 N 次比较 1 项

总比较次数 = N(N+1)/2
```

64 MiB 即 16,384 页，理论 retain predicate 调用约
`16,384 × 16,385 / 2 = 134,225,920` 次。frame free、页表 unmap 和 TLB invalidate
本身通常只随页数线性增长；它们无法解释 `ns/page²` 稳定，而 vector 全表扫描可以。

## 3. 真实 Python workload 的实板影响

本轮新增的 15 个 `memory_io` 计数器只在 `stats_on=1` 时记录，Python runner 在预热后
执行 `stats_on=0 → reset → stats_on=1 → 正式 workload → stats_on=0`。runtime、suite、
work、pycache 和结果全部位于 P4 `/persist` ext4。内核时钟为 100 MHz，表中
`anonymous unmap` 为 `anon_unmap_ticks_total / 100,000,000`：

| benchmark | body/s | unmap/s | body 占比 | sys 占比 | calls | resident pages | max active pages | retain scan steps | max call |
|-----------|-------:|--------:|----------:|---------:|------:|---------------:|-----------------:|------------------:|---------:|
| list | 21.226191 | 2.396336 | 11.290% | 29.143% | 436 | 110,176 | 3,909 | 70,469,395 | 327.313 ms |
| dict | 6.288156 | 0.609509 | 9.693% | 46.901% | 56 | 22,000 | 5,121 | 19,751,552 | 370.314 ms |
| bytesio | 5.841216 | 0.251081 | 4.298% | 18.236% | 146 | 21,698 | 1,223 | 6,398,530 | 24.344 ms |
| fork | 29.706629 | 0.228320 | 0.769% | n/a | 11,386 | 41,231 | 235 | 1,921,022 | 1.627 ms |
| thread | 25.528802 | 0.007704 | 0.030% | 0.917% | 1,122 | 1,322 | 252 | 33,442 | 1.881 ms |
| json_loads | 84.956221 | 0.000010 | 0.000% | 0.003% | 1 | 2 | 2 | 3 | 0.010 ms |

这组数据说明复杂度缺陷的实际影响由“调用次数”与“单个 VMA 的 resident/active 规模”
共同决定。`bm_fork` 调用最多，但以小映射为主，累计占比不到 1%；`bm_dict` 只有 56
次，却出现一个 active=5,121 页的释放，最长一次达到 370 ms。优化验收不应只比较
calls，应同时比较扫描步数、最大调用时延和 workload 累计占比。

占比是当前诊断构建中的路径归因，不是未来修复后的保证提速：修复会改变 cache/TLB/
frame free 的交互，且 production 与 diagnostic 存在已知布局差异。它足以确定该问题对
`list/dict/bytesio` 有可观影响，并为优化优先级提供实板证据。
`bm_fork` 的 rusage 只覆盖 parent，而内核全局窗口包含 children，故不计算 unmap/sys。

## 4. 为什么这会被视为 Python 性能风险

CPython 使用 arena、匿名 mapping、大 buffer、fork/CoW 和扩展模块分配；其中某些路径
会显式关闭大 mapping。如果这些 mapping 的 resident pages 位于上述 active vector，
单次释放会阻塞当前单核内核数百毫秒到数秒，表现为：

- 大对象释放或 mmap close 偶发长尾；
- benchmark body 结束后迟迟不返回；
- 线程/进程退出阶段出现不可由用户代码解释的 sys；
- 串口 harness 看起来像卡死，但最终仍完成。

前述现象中，前三个真实 workload 已有累计时间与长尾的实板量化；未覆盖的第三方扩展
和其他 Python 应用仍只能视为风险面，不能从六项样本外推具体百分比。

## 5. 不能外推到哪些路径

正常 `execve` 替换地址空间或进程退出时，当前内核主要使用
`clear_no_hole()` 批量清理，而非一定逐 VMA 调用相同 `unmap` 路径。因此以下说法都
超出证据：

- “每个 Python 进程退出都有 O(N²)”；
- “`bm_fork` 主要来自该问题”（本轮实测仅 0.769%）；
- “修复后 18 项总时间会下降某个百分比”；
- “所有匿名页释放都是 O(N²)”。

本问题与非对齐 trap 的口径仍不同：trap 已在真实 float/string body 中解释 91%–95%
sys；O(N²) 则用六个真实 workload 的 `Vma::unmap` 内部时间解释 body 的 0–11.29%。

## 6. 已落地的影响量化接口

本轮已在不逐事件打印、不增加 syscall/用户 ABI 的前提下，为显式 `Vma::unmap` 增加：

| 计数器 | 用途 |
|--------|------|
| calls | Python workload 实际调用次数 |
| requested pages | 用户请求范围，不等于 resident pages |
| resident pages | 决定 active 删除工作量 |
| active length before/max | 验证全表扫描的真实规模 |
| elapsed ticks | 计算对 body sys 的累计占比 |
| resident size buckets | 1–16、17–256、257–4096、4097+ 页，避免高基数日志 |
| caller path | 已区分 range unmap 与 remove-area；更细的 mremap/split 仍可后补 |

此外增加 `retain_scan_steps_total`，不是按页面数推算，而是在每次现有 `retain` 之前累加
当时 `active.len()`。合成探针的五档正式样本均为“理论主映射扫描步数 + 3”；额外 3
来自计时窗内一个 2 页辅助映射，说明计数能同时解释主项和背景项。

## 7. 未来修复方向的验收条件

本轮不实现修复，但后续方案无论采用哈希索引、按 VPN 排序结构、批量 retain 一次删除
还是换 frame store 表示，都必须满足：

- 1–64 MiB resident mapping 的 close 曲线接近 O(N)；
- 大尺寸 `ns/page` 稳定，`ns/page²` 不再稳定；
- frame 引用、CoW、swap/zram/pagecache 状态不丢失；
- 页表修改后对应 LA64/RV64 TLB 刷新正确；
- `mmap/munmap/mremap/fork/exec/exit` 功能测试不回归；
- 双架构编译、QEMU 功能和 2K1000LA 实板最终验证齐全。

## 8. 原始证据

- [1 MiB](raw-data/20260716T-cpython-deepdiag/raw/ext4_mmap_release_1m-1-169b468f.log)
- [4 MiB](raw-data/20260716T-cpython-deepdiag/raw/ext4_mmap_release_4m-1-01e47934.log)
- [16 MiB](raw-data/20260716T-cpython-deepdiag/raw/ext4_mmap_release_16m-1-441bcd08.log)
- [32 MiB](raw-data/20260716T-cpython-deepdiag/raw/ext4_mmap_release_32m-1-75ab60de.log)
- [64 MiB](raw-data/20260716T-cpython-deepdiag/raw/ext4_mmap_release_64m-1-62aa083a.log)
- [结构化 records.jsonl](raw-data/20260716T-cpython-deepdiag/records.jsonl)
- [本轮量化报告](raw-data/20260717T-anon-unmap-quant/reports/anon_unmap_quantification.md)
- [合成探针 CSV](raw-data/20260717T-anon-unmap-quant/reports/anon_unmap_synthetic.csv)
- [真实 Python CSV](raw-data/20260717T-anon-unmap-quant/reports/anon_unmap_python.csv)
- [本轮结构化 records.jsonl](raw-data/20260717T-anon-unmap-quant/records.jsonl)

代码、精确扫描步数和真实 Python 累计时间共同证明复杂度及其选择性影响；下一阶段可以
直接进入数据结构/批量删除方案的实现与对照，不再需要先做 PC Top-N 或重复旧控制组。
