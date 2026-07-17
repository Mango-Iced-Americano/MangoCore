---
title: "Python 性能原始数据与复核索引"
category: debug
status: archived
author: MangoCore Team
last_update: 2026-07-17
tags: [performance, raw-data, jsonl, serial, csv, manifest, reproducibility]
related_docs:
  - "docs/09_debug/la64_on_board/260717/01-python-performance-baseline.md"
  - "docs/09_debug/la64_on_board/260717/05-strict-align-first-experiment.md"
---

# Python 性能原始数据与复核索引

## 1. 归档目标

性能数据最初位于被 `.gitignore` 排除的 `target/`。为避免清理构建目录后只剩结论而
失去证据，本批次把文本层完整复制到 [raw-data/](raw-data/README.md)：manifest、
`records.jsonl`、串口 raw、CSV/Markdown reports 和构建验证日志。五个实验目录共
220 个文本文件，另有 10 个 strict runtime build 文件和 2 个归档说明/哈希文件，
总计 232 个文件、约 7.9 MiB。

没有复制 uImage、ELF 和 81.6 MiB runtime archive；这些二进制仍可由原路径或构建
脚本恢复，其文件名、大小、SHA-256 记录于
[`ARTIFACTS.sha256`](raw-data/ARTIFACTS.sha256)。

## 2. 不可变层、结构化层和派生层

```text
manifest.json
  源码、dirty、build input、镜像、平台、suite、命令身份

raw/*.log
  串口原样捕获；解析失败也不修改

records.jsonl
  每次 harness 命令的 test/sample/build/cache/rc/wall/log 路径

reports/*.csv, *.md
  从 records 和 raw 解析得到；允许重生成，但必须保留 data_quality
```

原始 `records.jsonl` 的 `log` 字段保留采集时宿主绝对路径
`/Users/luzimo/dev/MangoCore/target/...`。归档副本没有回写相对路径，以免破坏原记录
hash；阅读时把同名尾部映射到本目录对应 `raw/` 即可。

## 3. 运行目录总览

| run | records | raw logs | reports | 角色 |
|-----|--------:|---------:|--------:|------|
| `20260716T102350Z-cpython-ext4-production` | 37 | 39 | 13 | 正式绝对性能、startup/import、SmolAgent 失败探针 |
| `20260716T-cpython-deepdiag` | 56 | 58 | 11 | trap、mmap release、fileio/PageCache/SATA、探针税 |
| `20260716T-perf-diag-structural-ab-run` | 18 | 22 | 1 | production/diag-off 实板结构 A/B |
| `20260717T042020Z-cpython-strict-align` | 42 | 44 | 14 | strict 部署、72/72、18 项和 trap 对照 |
| `20260716T-perf-diag-structural-ab` | — | — | build logs | 相邻镜像构建身份，不含大二进制 |
| `strict-runtime-build` | — | — | build logs/manifest | 完整依赖闭包、PGO/LTO、双架构编译 |

## 4. production 目录

路径：[`raw-data/20260716T102350Z-cpython-ext4-production/`](raw-data/20260716T102350Z-cpython-ext4-production/)

关键文件：

| 文件 | 用途 |
|------|------|
| `manifest.json` | HEAD、dirty、build input、镜像、suite、平台和命令身份 |
| `records.jsonl` | 37 条执行记录，含 warmup/formal/startup/import/ext4 post-check |
| `reports/formal_benchmarks.csv` | 18 项正式 production 表，包含 reconstructed 标记 |
| `reports/cpython_bench_samples.csv` | 原 analyzer 的逐样本输出 |
| `reports/cpython_bench_phases.csv` | fileio/fork/thread 阶段数据 |
| `reports/cpython_ext4_kernel_analysis.md` | 当时生成的综合分析，SHA-256 `8f34941a...a22e` |
| `reports/failures.csv` | harness 失败记录，不等价于 benchmark 失败数 |

原始日志定位：

- benchmark：`raw/cpython_bench_<name>-*.log`；
- `python -S`：`raw/python_startup_minus_s*`；
- site：`raw/python_startup_site*`；
- imports：`raw/python_import_runner*`；
- SmolAgent：`raw/smolagents_import_*`；
- ext4 末尾校验：`raw/ext4_post_*`。

`regex` 和 `dict` 各有一行 JSON 串口缺字符，但同一 raw 中完整保留 elapsed、user、sys、
summary、PASS 和 rc=0。`formal_benchmarks.csv` 将字段标为
`reconstructed_sample_fields_from_raw_serial_summary_intact`，没有修改 raw。

## 5. deepdiag 目录

路径：[`raw-data/20260716T-cpython-deepdiag/`](raw-data/20260716T-cpython-deepdiag/)

按问题查找：

| 问题 | raw 前缀 | 主要报告 |
|------|----------|----------|
| string/float/nbody trap | `ext4_string_*`、`float_body_*`、`nbody_body_*` | `reports/counter_deltas.csv` |
| 1–64 MiB unmap | `ext4_mmap_release_*` | `records.jsonl` |
| 100-file ext4 | `ext4_fileio_scaled_*` | `counter_deltas.csv` |
| 探针开关税 | `*_off`、`*_core`、`*_memory` | `probe_tax.csv` |
| 双架构 QEMU | `verification/qemu-*` | `verification/README.md` |

`counter_deltas.csv` 是 profile snapshot 的差值，不是逐事件 trace。字段为 0 可能表示
窗口没有事件，也可能表示该 profile 没启用该 counter；复核时必须同时读取记录中的
profile 名和 `stats_on`。

## 6. 结构 A/B 目录

构建验证：[`raw-data/20260716T-perf-diag-structural-ab/`](raw-data/20260716T-perf-diag-structural-ab/)

实板运行：[`raw-data/20260716T-perf-diag-structural-ab-run/`](raw-data/20260716T-perf-diag-structural-ab-run/)

`reports/structural_ab.csv` 只包含 nbody/string/float。它证明 diagnostic feature 即使
`stats_on=0` 也会选择性改变 user time，因此不能把 diagnostic absolute time 当正式
production 性能。对应 uImage/ELF 二进制未复制，哈希在 `ARTIFACTS.sha256`。

## 7. strict 目录

路径：[`raw-data/20260717T042020Z-cpython-strict-align/`](raw-data/20260717T042020Z-cpython-strict-align/)

关键文件：

| 文件 | 用途 |
|------|------|
| `manifest.json` | strict 运行源码、dirty、build input、内核和 runtime 身份 |
| `records.jsonl` | 部署、功能、18 项和控制面失败共 42 条记录 |
| `raw/strict_functional_l3_l9-*.log` | 72 项项目 judge 的输入 |
| `reports/cpython_bench_samples.csv` | strict 18 项正式 elapsed/user/sys |
| `reports/counter_deltas.csv` | 每项 body 的 trap 分类，全部 0 |
| `reports/cpython_bench_phases.csv` | strict fileio/fork/thread 阶段 |
| `reports/failures.csv` | 四条 benchmark 前部署/控制面失败 |
| `reports/strict_align_trap_comparison.csv` | 四个旧侧候选的可比性分级 |
| `reports/strict_align_timing_trend.csv` | 18 项辅助趋势，明确标为 auxiliary |
| `reports/strict_align_comparison.md` | 完整派生报告，SHA-256 `bd302a3f...1706` |

trap 对照 CSV SHA-256 为 `4dc028b2...ff29`，时间趋势 CSV 为
`0ae1674f...f59e`。四条 failures 的 test 名、rc 和 wall 保留原样；其中 900 s 超时后
两条 11 s 记录属于同一板端前台仍在运行的失联现场。

## 8. strict runtime build 目录

路径：[`raw-data/strict-runtime-build/`](raw-data/strict-runtime-build/)

| 文件 | 内容 |
|------|------|
| `build-full-gcc15.log` | 首轮完整构建、PGO 训练和失败明细 |
| `build-full-gcc15-resume.log` | 已验证 profile 后恢复 PGO/LTO 和最终打包 |
| `strict-runtime-manifest.json` | 94 个 ELF 的 hash/SONAME/NEEDED、flags、PGO 统计 |
| `package-final.log` | 首版 dbdb 制品和 smoke |
| `deploy-dry-run.txt` | 初版 Python tarfile 部署命令，解释 900 s 失败来源 |
| `verify-rv64-kernel.log` | rv64 串行内核编译 |
| `verify-la64-kernel.log` | la64 串行内核编译 |
| `*.tar.xz.sha256` | 原始包和规范化最终包的 SHA 文件 |

## 9. 二进制制品身份

| 制品 | 大小 | SHA-256 |
|------|-----:|---------|
| production uImage | 16,756,992 B | `bf1668b9...63c0` |
| deepdiag LA64 kernel | 62,020,224 B | `7bd0dc97...b22` |
| deepdiag RV64 kernel | 77,111,808 B | `a0ad8aad...416` |
| adjacent production uImage | 16,756,992 B | `728b2187...802` |
| adjacent production ELF | 71,555,720 B | `1df241b2...afe` |
| adjacent diagnostic uImage | 16,546,136 B | `53e43b04...e68` |
| adjacent diagnostic ELF | 65,855,664 B | `4e96eb6f...29` |
| strict runtime tar.xz | 81,627,728 B | `abbc714c...56a` |

完整 64 位哈希见 `ARTIFACTS.sha256`，缩写只用于表格可读性。

## 10. 重分析方法

若原始 `target/perf-runs/<run>` 仍存在，可重新运行：

```text
python3 scripts/kernel_perf.py analyze \
  --run-dir target/perf-runs/<run-id>
```

复核 CPython L3-L9：

```text
python3 judge/judge_cpython-isolated.py \
  < docs/09_debug/la64_on_board/260717/raw-data/20260717T042020Z-cpython-strict-align/raw/strict_functional_l3_l9-1-bec4b123.log
```

报告复核顺序应为：

1. `manifest.json` 确认 build/platform/storage/suite；
2. `records.jsonl` 确认 rc、wall、sample 和 raw 文件；
3. raw 中确认 begin/end/rc marker 完整；
4. reports 只作聚合，不覆盖 raw；
5. 跨 run 比较前检查 workload、warmup、storage、suite 和内核哈希；
6. 任一身份不同则降低比较等级，不补造下降百分比。

## 11. 未归档为“原始数据”的内容

- 81.6 MiB runtime、uImage 和 ELF：体积大，已记录 hash；
- P4 失败 staging 目录：只在实板保留，不属于 canonical runtime；
- 尚未执行的 PMU、30 分钟稳定性、成功 SmolAgent/API：没有数据，不能在报告中补全；
- develop 分支新 ext4 结果：尚未产生，未来必须新建独立 run，不能覆盖本批次。
