---
title: "2K1000LA Python 性能专项（2026-07-17 批次）"
category: debug
status: current
author: MangoCore Team
last_update: 2026-07-17
tags: [loongarch64, 2k1000la, python, cpython, performance, ext4, strict-align]
code_paths:
  - "user/tools/cpython/bench/"
  - "scripts/run_cpython_bench_matrix.py"
  - "scripts/kernel_perf.py"
  - "scripts/build_cpython_runtime_la64_strict.sh"
  - "scripts/deploy_cpython_runtime.py"
  - "os/src/hal/arch/loongarch64/trap/mod.rs"
  - "os/src/mm/vma.rs"
  - "os/src/mm/frame_store.rs"
related_docs:
  - "docs/09_debug/la64_on_board/260717/01-python-performance-baseline.md"
  - "docs/09_debug/la64_on_board/260717/02-unaligned-trap-root-cause.md"
  - "docs/09_debug/la64_on_board/260717/03-anonymous-unmap-quadratic.md"
  - "docs/09_debug/la64_on_board/260717/04-ext4-small-file-path.md"
  - "docs/09_debug/la64_on_board/260717/05-strict-align-first-experiment.md"
  - "docs/09_debug/la64_on_board/260717/06-raw-data-index.md"
  - "docs/09_debug/la64_on_board/260717/07-strict-runtime-and-anon-unmap-quantification.md"
---

# 2K1000LA Python 性能专项（2026-07-17 批次）

## 1. 批次目标与完成状态

本批次承接 2026-07-16 的全面 Python 性能扫描。第一阶段不优化内核，只在 production
实板建立 18 项 workload 的绝对基线，再用 `perf_diag` 解释高 system time。随后按
决定只进行第一次用户态实验：将 CPython、musl 和全部原生依赖用
`-mstrict-align` 重编译，内核非对齐模拟器保持不变，旧控制组不重跑。

截至归档时已经完成：

- production 实板、P4 ext4 上 18/18 benchmark，正式 body 累计 `1,928.806 s`；
- 启动、site、固定 imports、fork/thread/fileio 阶段拆分；
- 非对齐 trap、匿名页显式释放和 ext4 小文件三条问题链；
- strict-aligned 完整 runtime 构建、QEMU smoke、双架构内核编译、实板 runtime smoke；
- CPython L3-L9 实板 `72/72` 和 strict 18/18 benchmark；
- strict 正式 body 的所有非对齐计数为 0；`bm_float`、`bm_string` 有旧侧匹配证据；
- 五个实验目录的可审计文本数据复制到本目录 `raw-data/`。
- strict runtime 标准 Make/安装入口固化，匿名页释放 15 项计数器和实板影响量化完成。

当前没有修改内核非对齐模拟器，没有修复匿名页释放 O(N²)，也没有优化 ext4。ext4
等待队友 develop 分支的新实现后复测。

## 2. 一眼看懂

| 项目 | 结论 | 证据等级 |
|------|------|----------|
| production Python 基线 | 18/18 通过，累计 1,928.806 s；最慢为 regex 421.447 s | production 实板 |
| 问题 1：非对齐访问 | `bm_float` 3,000,039 次 trap，handler 解释 95.2% sys；逐字节 uaccess 又放大 COW/TLB | 已确认 |
| 问题 2：匿名页释放 | 64 MiB resident mapping 关闭 3.890 s、扫描 134,225,920 步；list/dict body 占比 11.29%/9.69% | 复杂度与真实影响已确认 |
| 问题 3：ext4 小文件 | 5,000 个文件生命周期 46.449 s；高线性固定税，非 O(N²) | 已确认，当前分支暂停 |
| 第一次实验 | strict-aligned runtime 的 18 个 benchmark body 非对齐计数全部为 0 | 实板已确认 |
| 功能门禁 | L3-L9 `72/72`，18/18 benchmark 通过 | 实板已确认 |
| 时间收益口径 | 1,928.806 → 303.470 s 只作辅助趋势，不是 production-to-production 隔离 A/B | 不作为正式收益 |

## 3. 文档导航

| 文档 | 内容 | 推荐用途 |
|------|------|----------|
| [01-python-performance-baseline.md](01-python-performance-baseline.md) | 环境、采样方法、18 项完整表、启动/import、fork/thread 和测试停止点 | 汇报总体性能画像 |
| [02-unaligned-trap-root-cause.md](02-unaligned-trap-root-cause.md) | CPUCFG、trap decoder、逐字节 uaccess、COW/TLB 放大和诊断布局偏差 | 解释问题 1 |
| [03-anonymous-unmap-quadratic.md](03-anonymous-unmap-quadratic.md) | resident mapping 曲线、源码复杂度推导、影响边界和后续计数方案 | 解释问题 2 |
| [04-ext4-small-file-path.md](04-ext4-small-file-path.md) | 5,000/100 文件缩放、PageCache/SATA flush 闭合、在线 ext4 检查边界 | 解释问题 3 |
| [05-strict-align-first-experiment.md](05-strict-align-first-experiment.md) | 构建闭包、PGO/LTO、部署失败、72/72、18 项结果、trap 对照和时间口径 | 第一次优化留档 |
| [06-raw-data-index.md](06-raw-data-index.md) | 原始目录、文件 schema、二进制哈希、重分析命令和数据质量说明 | 审计与复现 |
| [07-strict-runtime-and-anon-unmap-quantification.md](07-strict-runtime-and-anon-unmap-quantification.md) | 标准 strict runtime 入口、安全安装器、15 个 VMA 计数器、实板精确扫描与六项 Python 占比 | 第二阶段量化留档 |

## 4. 证据分层

本批次同时使用三类样本，结论不能跨层偷换：

```text
production 2K1000LA + P4 ext4
    -> 当前绝对性能与排名

perf_diag 2K1000LA + stats_on=0/1
    -> trap/pagecache/SATA/TLB 等事件与路径归因

strict userspace + 归档 perf_diag 内核
    -> 验证重新编译后事件是否消失；耗时只作辅助趋势
```

`perf_diag stats_on=0/1` 同构建内开关税低于 1.3%，但 production 与 diagnostic ELF 的
`.text` 大小和函数地址不同；`bm_float` 的 user time 对布局非常敏感。因此诊断数据能
解释“系统态在做什么”，不能替换 production 正式排名。

## 5. 原始数据入口

所有可审计文本数据位于 [raw-data/](raw-data/README.md)，包括五组 run 的 manifest、
`records.jsonl`、原始串口日志、CSV/Markdown 派生报告和构建验证日志。二进制产物没有
重复提交，身份集中记录在 `raw-data/ARTIFACTS.sha256`。

测试中曾出现四条 benchmark 前失败：旧 P3 Python 解包超时、同一失联现场的审计和
串口恢复超时，以及首版 tar 显式根成员 `./` 被 VFS 拒绝。它们全部保留，不能计入
18 项 benchmark failure，也不能从记录中删除。

## 6. 当前停止点

- `-mstrict-align` 已证明能消除所覆盖 body 的非对齐 trap，但尚未做相邻 production
  内核上的正式耗时 A/B。
- strict 的 0 只覆盖这 18 个 workload body，不能外推到启动、import、任意第三方
  wheel 或含手写汇编的扩展。
- O(N²) 路径尚未修改；真实 Python 的 calls/requested/resident/active/scans/ticks 已补齐，
  下一步可以直接实施并验收批量删除/索引结构方案。
- ext4 仅完成在线 rw、sync、重启后哈希和 workload 正确性检查；没有 offline e2fsck、
  断电恢复或 fault injection，不宣称 journal/断电安全。
- 30 分钟混合稳定性、PMU cache miss、成功 SmolAgent 本地端点和真实 API 仍未完成。
