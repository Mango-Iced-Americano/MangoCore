---
title: "2K1000LA Python 性能检查归档（2026-07-16）"
category: debug
status: checkpoint
author: MangoCore Team
last_update: 2026-07-17
tags: [performance, python, cpython, loongarch64, ext4, board]
code_paths:
  - "user/tools/cpython/bench/"
  - "user/tools/cpython/cpython_benchmark.sh"
  - "scripts/run_cpython_bench_matrix.py"
  - "scripts/kernel_perf.py"
related_docs:
  - "docs/09_debug/la64_on_board/260717/02-unaligned-trap-root-cause.md"
  - "docs/09_debug/la64_on_board/260717/03-anonymous-unmap-quadratic.md"
  - "docs/09_debug/la64_on_board/260717/04-ext4-small-file-path.md"
  - "docs/09_debug/la64_on_board/260717/05-strict-align-first-experiment.md"
  - "docs/09_debug/la64_on_board/260717/06-raw-data-index.md"
---

# 2K1000LA Python 性能检查归档（2026-07-16）

本页是 `HEAD 934c4af9` 在 2026-07-16 的 production 性能停止点归档。它只描述优化前
基线、诊断和根因排序；2026-07-17 的 strict-aligned 第一次实验独立记录在
[05-strict-align-first-experiment.md](05-strict-align-first-experiment.md)，避免把基线和
后测混成同一构建。

## 1. 最终口径

- 最终性能数字来自 2K1000LA 实板 production 镜像，不使用 QEMU 或 `perf_diag` 的绝对耗时替代。
- 18 项 workload 的 suite、临时目录、pycache 和 fileio 数据均位于 P4 `/persist` ext4；Python runtime 位于 P3 `/tools` ext4 ro。
- production 镜像 SHA-256：`bf1668b9bdbd1068914ac1a683ef58c821c6b03af1016f69771a9a2c25ba63c0`。
- benchmark ZIP SHA-256：`5059b61e4b241f35ef2f46a859df1848dd056e3a08d2127f7b2bd340a9abdb4e`；suite SHA-256：`6a4c6a1896cbbe1ae55be8fe1149c679bacdbf4a05759b7db3280593c10e0ce1`。
- Python 3.14.5，GCC 15.2.0，LoongArch64。
- 按用户的缩短策略，正式矩阵为一次预热加一次正式样本；此前三样本项目的变异很小，但单样本项目不声称具有跨提交显著性。

### 1.1 源码、测试集和构建身份

| 项目 | 身份 |
|------|------|
| Git HEAD | `934c4af9f9c84d38b4dff9c7c2a58bccc83f6ee9` |
| dirty diff SHA-256 | `e5e835b9b1676857efff6ea2d336608efa53493511fcabae67b9e65f4585ad4e` |
| build input SHA-256 | `94a8ed6236cc836e3792d23ced6d517d8c7949690053e7b38dee62b6f0cc9eaf` |
| benchmark source revision | `c50669c2b59a7d6d979fb12aea42c1b508ed3765` |
| pyperformance reference revision | `216cbeb5f828b8ee5864f9bb52f3563d2d1a4846` |
| benchmark ZIP | 73,688 B，SHA-256 `5059b61e4b241f35ef2f46a859df1848dd056e3a08d2127f7b2bd340a9abdb4e` |
| unpacked suite | SHA-256 `6a4c6a1896cbbe1ae55be8fe1149c679bacdbf4a05759b7db3280593c10e0ce1` |

测试集来自队友仓库后没有原样直接跑。纳入前完成了四类适配：固定随机输入和结果校验；
让每个样本拥有独立状态；将 file I/O、fork/exec、thread 拆成阶段指标；修复原
`pidigits` 工作量错误和 `richards` 首轮断链后空转。18 项由 harness 逐项启动独立
Python 进程，每项都有 begin/end/rc marker，前后 counter snapshot 只包 workload
body，不把解释器启动、suite import 和结果序列化混进热点计数。

### 1.2 采样策略为何从五次缩短到一次

最初计划采用一次预热加五次正式采样。宿主完整 90 样本矩阵的各项 CV 均低于 3.3%；
实板先完成的 `bytesio/chaos/decimal/dict` 三样本 CV 分别约
`0.38%/0.23%/0.24%/0.11%`。用户要求压缩板卡占用后，剩余矩阵统一为一次预热加
一次正式样本。

这足以形成当前 HEAD 的绝对画像和数量级根因，但不支持跨提交的统计显著性声明。
后续若要报告正式优化收益，应对候选热点恢复至少三次正式样本，或在相邻
production-to-production 构建上进行配对采样。

### 1.3 文件系统和计时边界

最终 suite、work、tmp、pycache、fileio payload 和结果均在 P4 `/persist` ext4；旧
runtime 位于 P3 `/tools` ext4 只读。每个 benchmark 的 `elapsed/user/sys` 来自目标端
同一进程 rusage 和单调时钟。`bm_fork` 的 rusage 只包含父进程，不能用
`user + sys` 解释 65 个 child 的 wall time；这种字段边界在 CSV 中保留。

## 2. production 实板基线

18/18 项通过，正式 workload body 累计 `1,928.806 s`：

| 排名 | benchmark | elapsed | user | sys | sys/elapsed |
|---:|---|---:|---:|---:|---:|
| 1 | regex | 421.447 s | 122.496 s | 298.329 s | 70.79% |
| 2 | json_loads | 255.947 s | 213.572 s | 41.921 s | 16.38% |
| 3 | thread | 249.020 s | 120.927 s | 125.818 s | 50.53% |
| 4 | chaos | 199.541 s | 96.790 s | 102.450 s | 51.34% |
| 5 | float | 150.033 s | 100.007 s | 49.807 s | 33.20% |
| 6 | fork | 124.358 s | 1.085 s | 1.231 s | parent only |
| 7 | spectral_norm | 99.805 s | 79.153 s | 20.510 s | 20.55% |
| 8 | sort | 83.750 s | 50.116 s | 33.495 s | 39.99% |
| 9 | bytesio | 69.741 s | 36.508 s | 33.128 s | 47.50% |
| 10 | dict | 61.230 s | 34.520 s | 26.612 s | 43.46% |
| 11 | list | 54.585 s | 30.053 s | 24.441 s | 44.78% |
| 12 | fileio | 48.677 s | 14.389 s | 34.200 s | 70.26% |
| 13 | decimal | 39.153 s | 28.732 s | 10.341 s | 26.41% |
| 14 | richards | 33.197 s | 18.059 s | 15.086 s | 45.44% |
| 15 | string | 15.670 s | 7.151 s | 8.489 s | 54.17% |
| 16 | nbody | 8.708 s | 8.671 s | 0.024 s | 0.28% |
| 17 | hash | 7.849 s | 5.782 s | 2.053 s | 26.16% |
| 18 | pidigits | 6.093 s | 4.873 s | 1.211 s | 19.87% |

`regex` 和 `dict` 的一行串口 JSON 有丢字符，但 elapsed/user/sys、summary、PASS 和 rc=0 均保留；正式 CSV 标记为 reconstructed，原始日志未修改。

## 3. 问题一：非对齐访问陷阱风暴

### 3.1 结论

已确认 2K1000LA 实板 `CPUCFG1=0x03e2727e` 的 UAL bit 为 0，而当前 CPython/扩展会生成大量非对齐整数 load/store。每条指令进入完整用户陷阱，Rust handler 又把 2/4/8-byte 访问拆成逐字节 uaccess。

| 定向 body | elapsed | sys | trap 数 | handler 时间 | handler/sys |
|---|---:|---:|---:|---:|---:|
| string | 11.457 s | 8.603 s | 373,371 | 7.830 s | 91.0% |
| float | 71.117 s | 49.175 s | 3,000,039 | 46.806 s | 95.2% |
| nbody 负对照 | 8.539 s | 0.026 s | 39 | 0.000628 s | 2.4% |

相邻构建的 float `stats_on=1` 又得到 3,000,039 次 trap、47.679 s handler、50.070 s sys，即 handler 占 sys `95.22%`。float 的计数精确接近每轮 3 次 4-byte load 加 2 次 8-byte load；不是 libm 单个函数造成，而是通用 Python 数值对象/字节码路径。

string 的 store 按宽度展开为 1,743,914 个 byte writes，同窗口单页 TLB invalidate 为 1,761,177，匹配 99.02%。说明逐字节 `copy_to_user` 又触发 private mapping 写权限/COW 检查和单页 TLB invalidate。

主要代码链：

1. `os/src/hal/arch/loongarch64/trap/mod.rs:321,348-359`：接住异常、取指、解码、逐字节模拟。
2. `os/src/mm/uaccess.rs:588-604`：每一字节重新取得当前 VM、加锁并 fault-in。
3. `os/src/mm/page_fault.rs:136-149` 与 `os/src/mm/vma.rs:473-478`：private store 进入 COW/权限恢复。
4. `os/src/hal/arch/loongarch64/laflex.rs:514-518`：每次 PTE flags 修改做单页 TLB invalidate。
5. `os/src/hal/arch/loongarch64/trap/trap.S:34-66`：陷阱入口/出口保存恢复 GP、FPR 和启用时的 LSX；这部分不在 Rust handler ticks 中，所以上述时间仍是下界。

正确性风险另行记录：未知 op 和部分用户复制错误会 panic/unwrap；浮点非对齐与 LSX/FPR 恢复存在覆盖风险。本轮计数都是整数 load/store，没有触发浮点分支。

### 3.2 诊断构建结构偏差

相邻 production 与 `perf_diag stats_on=0` 使用同一 HEAD、相同正常 feature、相同 initramfs 内容、相同 suite/runtime 和同一 ext4 路径：

| workload | production | diag off | 差异 | 关键现象 |
|---|---:|---:|---:|---|
| nbody | 8.652 s | 8.547 s | -1.21% | user/sys 都稳定 |
| string | 15.834 s | 11.306 s | -28.60% | sys 8.562/8.469 s 基本不变，user 大降 |
| float | 149.893 s | 72.492 s | -51.64% | sys 50.116/50.187 s 基本不变，user 大降 |

诊断构建 `.text` 小约 218 KiB，trap handler、uaccess、page-fault 等函数地址明显移动。当前高概率解释是数百万次用户/内核切换对代码/缓存布局高度敏感；没有 PMU cache-miss 数据，不能声称已确认具体 L1/L2 冲突。

同一诊断构建内部 float `stats_on=0/1` 为 72.492/72.080 s，差 -0.57%；string 和 fileio 计数开关差异也都低于 1.3%。因此“计数器运行时税低”成立，但“诊断构建与 production 结构等价”不成立。诊断数据只用于机制、事件数量和 handler 时间，绝对性能排名只用 production。

## 4. 问题二：居民匿名映射显式释放 O(N²)

| resident mapping | pages | close/munmap | ns/page² |
|---:|---:|---:|---:|
| 1 MiB | 256 | 2.494 ms | 38.05 |
| 4 MiB | 1,024 | 18.798 ms | 17.93 |
| 16 MiB | 4,096 | 239.029 ms | 14.25 |
| 32 MiB | 8,192 | 961.312 ms | 14.32 |
| 64 MiB | 16,384 | 3,893.434 ms | 14.50 |

16 MiB 以上稳定在约 14.3–14.5 ns/page²。`Vma::unmap` 在 `os/src/mm/vma.rs:386-399` 枚举 resident pages，每页调用 `remove_in_memory`；`os/src/mm/frame_store.rs:327-333` 的实现对整个 `active` 向量执行 `retain`。扫描总量为 `N+(N-1)+...+1`，O(N²) 已由代码和实板曲线双重确认。

证据边界很重要：正常 exec/进程退出主要走 `clear_no_hole()`，本轮没有证明 18 项 Python workload 大量走这条显式 `Vma::unmap` 路径。因此：

- “复杂度缺陷存在”是已确认；
- “实际影响当前 Python 总耗时多少”是证据不足；
- 优化前需要在真实 Python workload 中记录显式 munmap 的 VMA 大小、resident pages、次数和累计时间。

## 5. 问题三：ext4 小文件生命周期

production fileio 为 48.677 s，其中 5000 个 create/write/read/unlink 为 46.449 s，占 95.42%，平均 9.290 ms/文件。诊断缩放的 100 个文件为 0.892 s，平均 8.924 ms/文件；操作量 50 倍、耗时 52.05 倍，单位成本只增加 4.10%。当前是很高的线性固定税，不是 O(N²)。

缩放诊断闭合数据：

- PageCache：241 个脏页，105 次 writeback；
- SATA：132 write requests、128 flush，写 1,081,344 B；
- 241×4096=987,136 B 数据页，剩余 94,208 B 恰为 23 个 metadata blocks；
- 105 次 PageCache writeback + 23 个 metadata block = 128 次 `write_block`/flush；
- SATA read/write/flush 合计 0.188 s，只占 0.815 s sys 的 23.06%，约 76.94% sys 位于 VFS/ext4/PageCache/路径查找/分配/复制等软件路径。

关键路径是 unlink，不是普通 close：`Ext4OSInode::unlink` 在删除前强制 `flush_inode_pagecache_if_dirty()->writeback_all()`；100 个 60 B 小文件形成 100 个独立 singleton writeback。`SataBlock::write_block` 每次写后无条件 controller flush，无法跨文件合并。

代码还确认 create miss 重复父路径/不存在叶子扫描，MountFS/ext4 unlink 重复查找，ext4 更新时间戳后 `File::touch_modified` 再做 metadata 读写等固定税，但这些子项尚未独立计时。

本项按用户要求暂停：develop 分支正在更换驱动，等新驱动完成后再用同一 `/persist` ext4 workload 复测，不在当前分支继续优化或扩展测试。

## 6. 其他重要现象

### Python 启动/import

- `python -S -c pass`：1.159 s；
- normal site：1.630 s，site 增量约 0.471 s；
- 固定 import 集合：6.769 s，额外 import 主体约 5.139 s；
- 固定 import 的预热/正式为 7.877/6.769 s，相差 14.1%，已标为不稳定，未继续补样本；
- `import smolagents` 冷/热为 49.347/8.296 s，但都因现有环境缺 `PIL` 退出，没有调用真实 API，不能当端到端 LLM latency。

完整 nbody 解释器进程旧诊断窗口有 618,412 次非对齐陷阱、约 9.536 s handler，而只包 workload body 时仅 39 次。这说明启动/import/suite 准备本身也大量触发非对齐路径。

### fork/exec

`bm_fork` 的 124.358 s 是 65 个完整 Python child，平均约 1.91 s/child；正常 Python 启动 1.630 s 已构成约 85% 的下限。当前 exec 每次逐 4 KiB `pread` Python 主 ELF 和解释器 ELF，并明确绕过 PageCache frame reuse。parent rusage 不含 child，`wait4` 也忽略 rusage 参数；未捕获 clone/vfork flags，不能继续分摊 CoW、loader、exec 和退出成本。

### thread

thread 249.020 s 中，线程创建/工作/daemon 合计仅约 4.19 s；主线程串行 queue、无竞争 lock、event/condition 合计 244.8 s，占 98.3%。因此不能先归因于建线程、调度或 futex 等待。高 sys 很可能仍由 Python 同步原语内部的非对齐访问产生，但没有逐阶段 trap/futex 计数，保持“根因未定”。

## 7. ext4 与稳定性边界

- 所有正式 workload 最终均在 ext4 上；没有把 FAT32 `/scratch` 结果作为最终数字。
- 正式矩阵、定向诊断均无 panic、hang、ext4 error 或内容校验失败；正式末尾 `sync` 0.062 s。
- P4 在线挂载，未执行 offline e2fsck、断电恢复或 fault injection；当前实现没有 journal/recovery，不能外推为断电安全。
- 相邻 A/B 复位后 canonical benchmark 包仍在 P4，但短路径副本 `/persist/pyperf/s` 未保留；已从同一 ext4 canonical 包重建、`sync` 并逐文件核对哈希后才运行诊断 A/B。该事实保留为持久化边界，不宣称全部临时别名均跨复位持久。

## 8. 已完成验证

- production 18/18 benchmark 实板完成；
- rv64/la64 `perf_diag` 严格串行编译成功；
- 两架构 QEMU 诊断 smoke 通过 profile/reset、stats freeze 和 runtime counters；
- 所有正式性能结论落到 2K1000LA 实板；
- 相邻 production/diag 镜像、ELF、Cargo feature 和 initramfs 内容完成身份审计；
- 计数器开关税与诊断构建结构偏差分开测量。

没有继续完成：CPUCFG cache geometry 小探针部署、PMU cache-miss、Python 显式 munmap 实际占比、ext4 四阶段隔离、30 分钟混合稳定性、成功 SmolAgent/真实 API。本次停止时均明确标为未完成，不用推断补齐。

## 9. 后续顺序

按当前决定，后续先处理：

1. 非对齐陷阱路径；
2. 匿名页显式释放 O(N²)，但在改动前先补真实 Python 影响量化；
3. ext4 等 develop 新驱动完成后复测，不在当前分支先动。

优化验收必须继续使用 production 实板数字；`perf_diag` 只负责解释，并且每次诊断 feature/layout 改动后都重新做相邻结构 A/B。

## 10. 原始数据

- production：[`raw-data/20260716T102350Z-cpython-ext4-production/`](raw-data/20260716T102350Z-cpython-ext4-production/)
- 深入诊断：[`raw-data/20260716T-cpython-deepdiag/`](raw-data/20260716T-cpython-deepdiag/)
- 相邻镜像/ELF 构建验证：[`raw-data/20260716T-perf-diag-structural-ab/`](raw-data/20260716T-perf-diag-structural-ab/)
- 相邻 A/B 串口记录：[`raw-data/20260716T-perf-diag-structural-ab-run/`](raw-data/20260716T-perf-diag-structural-ab-run/)
- 综合报告：[`cpython_ext4_kernel_analysis.md`](raw-data/20260716T102350Z-cpython-ext4-production/reports/cpython_ext4_kernel_analysis.md)
- 正式 CSV：[`formal_benchmarks.csv`](raw-data/20260716T102350Z-cpython-ext4-production/reports/formal_benchmarks.csv)

原始串口日志和 `records.jsonl` 保持不改；派生报告必须注明 production/diag、ext4 路径和 image/suite 哈希。字段、文件数量和重分析命令见 [06-raw-data-index.md](06-raw-data-index.md)。

## 11. 与 2026-07-17 第一次实验的边界

本基线不回填 strict 结果。后续用户态重编译、部署失败、功能门禁、18 项 strict 表和
trap 下降趋势均见 [05-strict-align-first-experiment.md](05-strict-align-first-experiment.md)。
旧 production 对照按用户要求直接复用本页数据，没有重新跑一遍。
