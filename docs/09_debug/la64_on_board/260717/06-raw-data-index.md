---
title: "Python 性能原始数据与复核索引"
category: debug
status: current
author: MangoCore Team
last_update: 2026-07-18
tags: [performance, raw-data, jsonl, serial, csv, manifest, reproducibility]
related_docs:
  - "docs/09_debug/la64_on_board/260717/01-python-performance-baseline.md"
  - "docs/09_debug/la64_on_board/260717/05-strict-align-first-experiment.md"
  - "docs/09_debug/la64_on_board/260717/08-persist-strict-python-default.md"
  - "docs/09_debug/la64_on_board/260717/09-aligned-pillow-and-smolagent-closure.md"
  - "docs/09_debug/la64_on_board/260717/11-smolagents-toolkit-dependency-closure.md"
---

# Python 性能原始数据与复核索引

## 1. 归档目标

性能数据最初位于被 `.gitignore` 排除的 `target/`。为避免清理构建目录后只剩结论而
失去证据，本批次把文本层完整复制到 [raw-data/](raw-data/README.md)：manifest、
`records.jsonl`、串口 raw、CSV/Markdown reports 和构建验证日志。前五个实验目录共
220 个文本文件，另有 10 个 strict runtime build 文件和 2 个归档说明/哈希文件；
本轮再增加 44 个 anonymous-unmap/runtime 文本文件（约 756 KiB）。
P4 默认运行时固化另归档 44 个文件：`records.jsonl` 含 34 条 record，另有 38 份 raw、
4 份报告以及 manifest/records 两个索引文件。
Aligned Pillow/SmolAgent 闭包另归档 64 个文件、约 5.7 MiB：48 条 record、51 份 raw、
11 份 analyzer 报告及 manifest/records；大 runtime archive 仍只登记 hash。
OpenAI 可选后端补充归档 8 个文件：4 条 record、6 份 raw 以及 manifest/records；其中
raw 数包含两份因 512-byte 主机门禁未形成 record 的长命令失败日志。
SmolAgents 内置工具闭包另归档 48 个文件、约 4.8 MiB：15 条实板 record、18 份 raw、
11 份 analyzer 报告，以及 17 份 manifest/current/构建/双架构验证文件。

没有复制 uImage、ELF 和 81,627,728–87,057,368 B runtime archives；这些二进制仍可由原路径或构建
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
| `20260717T-anon-unmap-quant` | 22 | 24 | 14 | strict 入口复核、精确 unmap 扫描、六项真实 Python 占比 |
| `20260717T-p4-strict-python-default` | 26 | 30 | 3 | P4 发布、默认入口、chroot、72/72、SmolAgent fail-exposed |
| `20260717T-aligned-pillow` | 48 | 51 | 11 | aligned Pillow/MarkupSafe/PyYAML、P4 原子发布、默认 SmolAgent、72/72 |
| `20260718T-openai-dependency-audit` | 4 | 6 | 0 | OpenAI/Pydantic 版本闭包、P4 pure wheel、默认 OpenAIModel 构造、pip tag 残留 |
| `20260718T-smolagents-toolkit-closure` | 15 | 18 | 11 | ddgs/markdownify 传递闭包、P4 发布、默认三工具构造和 native 来源门禁 |
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

## 9. 匿名释放量化目录

路径：[`raw-data/20260717T-anon-unmap-quant/`](raw-data/20260717T-anon-unmap-quant/)

| 文件 | 用途 |
|------|------|
| `manifest.json` | HEAD/dirty/build-input、QEMU 与实板 image 身份 |
| `records.jsonl` | P4 前检、10 个合成样本、6 个 Python workload、runtime/QEMU smoke |
| `reports/anon_unmap_synthetic.csv` | 五档预热/正式 close、ticks 和精确扫描不变量 |
| `reports/anon_unmap_python.csv` | 六项 body 的 calls/pages/scans/ticks/占比/最大时延 |
| `reports/anon_unmap_quantification.md` | 可由 analyzer 重生成的聚合表 |
| `raw/qemu_*_anon_diag_smoke-*.log` | 双架构计数节点和 reset/freeze 门禁 |
| `raw/anon_mmap_release_*.log` | 合成探针原始串口 |
| `raw/cpython_bench_*.log` | strict Python 目标端事件和 counter snapshot |
| `verification-final/qemu-*-final.log` | 最终错误路径计数修正后的双架构 QEMU smoke |
| `runtime-current.json`、`runtime-installed.stamp.json` | strict artifact 选择结果与标准 tools 缓存安装身份 |

最初两条过长前检命令在宿主 512-byte 门禁处被拒绝，raw 保留但没有 workload record；
后续短前检通过。`board_strict_runtime_smoke` 未设置 `CPYTHON_ROOT`，启动的是兼容默认
runtime，不作为 P4 strict 身份证据；以 `board_strict_runtime_manifest_smoke` 和六项
benchmark environment 为准。

## 10. P4 默认运行时目录

路径：[`raw-data/20260717T-p4-strict-python-default/`](raw-data/20260717T-p4-strict-python-default/)

| 文件 | 用途 |
|------|------|
| `manifest.json` | 最终 b7/a420 artifact、a5e uImage、启动串口、初始/分阶段源码指纹 |
| `records.jsonl` | 34 条部署、默认入口、隔离、chroot、功能和失败暴露记录 |
| `raw/pyctl_run-1-7b19d952.log` | b7 77.8 MiB 解包、94 ELF integrity、smoke 和最终 current 发布 |
| `raw/board_boot_history-through-a5e60c0d.log` | U-Boot bytes/CRC/iminfo、冷启动、launcher、chroot PASS |
| `raw/p4_python_default_verify_a5e_final-1-b4dc9ddd.log` | 最终镜像默认解释器/site/pip/self-exec 门禁 |
| `raw/p4_strict_functional_72_final-1-9479d11e.log` | P4 ext4 上 L3-L9，judge 输入为 72/72 |
| `raw/p4_python_default_verify_b7_final-1-89f02744.log` | b7 最终解释器/site/pip/self-exec 门禁 |
| `raw/p4_strict_functional_72_b7_final-1-016cffe7.log` | b7 P4 ext4 L3-L9，项目 judge 72/72 |
| `raw/p4_smolagent_command_a5e_final-1-9cf47dfe.log` | strict `.real` 入口后的唯一 PIL 缺口 |
| `reports/repackage-idempotence.txt` | a420/b7 逐成员比较、重复打包 hash 与最终 b7 上板边界 |

中间 `p4_tools_isolation_final-1-bbd14a27.log` 因宿主 512-byte 门禁被拒，不是有效板端
样本；`p4_shell_probe-1-3612740b.log` 为 0 字节/rc124，也不作证据。失败样本继续保留以说明
为何增加 PT_INTERP、环境清理、`.real` console 选择和 direct-copy 发布。

## 11. Aligned Pillow 与 SmolAgent 闭包目录

路径：[`raw-data/20260717T-aligned-pillow/`](raw-data/20260717T-aligned-pillow/)

| 文件 | 用途 |
|------|------|
| `manifest.json` | 初始源码指纹和 071/e14/43d 三个递进制品身份 |
| `records.jsonl` | 48 条构建、部署、失败清理、门禁、实板功能和双架构 build 记录 |
| `raw/aligned_python_pyyaml_build-1-d59a52ab.log` | 首次 PyYAML 构建因 native attempt/宿主 wheel tag 被拒绝 |
| `raw/aligned_python_pyyaml_build-2-5fb08f54.log` | 强制 pure Python 后的完整 build、QEMU smoke 和 43d 打包 |
| `raw/pyctl_run-1-2e2cdd96.log` | `/tmp` 接收 archive、P4 解包、100 ELF integrity、四组 smoke 和 current 发布 |
| `raw/persist_default_gate-2-c3c0b0a4.log` | 默认 interpreter/site/self-exec/pip/Pillow/SmolAgent 最终 PASS |
| `raw/pillow_ext4_smoke-2-4eed2fcb.log` | P4 ext4 PNG/JPEG 写入、fsync、重开、hash 和模块路径 |
| `raw/smolagent_agentimage-1-a4ef3edf.log` | SmolAgent `AgentImage` 实际调用 Pillow 的 PASS |
| `raw/cpython_l3_l9_final-1-f58ddacf.log` | 最终 43d runtime 的 L3-L9，项目 judge `72/72` |
| `reports/summary.csv` | 所有成功测试的 wall 汇总 |
| `reports/failures.csv` | 缺依赖、ENOSPC、构建门禁和首次容器权限等失败样本 |

该目录保留依赖闭包的递进发现过程：071 首先补 Pillow，e14 再补 MarkupSafe，最终 43d
加入纯 Python PyYAML。只有 43d 的最终门禁可作为“默认 SmolAgent 已通过”的证据。

## 12. OpenAI 可选后端补充目录

路径：[`raw-data/20260718T-openai-dependency-audit/`](raw-data/20260718T-openai-dependency-audit/)

| 文件 | 用途 |
|------|------|
| `manifest.json` | HEAD、dirty/build-input 指纹、2K1000LA production 身份 |
| `records.jsonl` | 启动前超时、版本闭包、默认 OpenAIModel 构造和 pip check 共 4 条记录 |
| `raw/dependency_versions-1-b19f2483.log` | OpenAI/Pydantic/HTTPX/typing-extensions/SmolAgent 精确版本与安装位置 |
| `raw/default_openai_smolagent_smoke-1-0d1314b0.log` | 默认入口无网络构造 `OpenAIModel`，`pydantic.compiled=False`，退出 0 |
| `raw/pip_check_after_pydantic-1-40ea49fa.log` | 缺 Pydantic 已消失；仅剩 Pillow/MarkupSafe 平台 tag，退出 1 |

`openai_metadata` 的 rc124 发生在板卡尚未进入 Shell 时；两份更长的 OpenAI smoke raw
在宿主 512-byte 串口命令门禁处被拒绝，没有形成 record。它们保留用于解释执行过程，
但不作为板端功能失败。成功构造样本 wall 37.074 s，只含本地 import/client setup，不含
真实 API 网络时间。

## 13. SmolAgents 内置工具依赖闭包目录

路径：[`raw-data/20260718T-smolagents-toolkit-closure/`](raw-data/20260718T-smolagents-toolkit-closure/)

| 文件 | 用途 |
|------|------|
| `manifest.json` | 本次 2K1000LA production run 的源码、dirty/build-input 与平台身份 |
| `records.jsonl` | 15 条部署/默认环境/工具构造/路径审计记录，保留 4 条预期非零诊断样本 |
| `raw/pyctl_run-1-c0855d82.log` | 首候选因 user-site click 版本遮蔽被单层 smoke 拒绝并回滚 |
| `raw/pyctl_run-1-2d7a0db5.log` | 最终 28f artifact 的 113 ELF、exact/effective smoke 与 current 发布 |
| `raw/smolagents_toolkit_effective-1-c759e58a.log` | 默认 normal-site 下的版本和离线构造门禁 |
| `raw/t-1-b2e57f5c.log` | 以真实脚本文件执行三项 `TOOL_MAPPING` 构造的成功记录 |
| `raw/native-1-950def0f.log` | user site 无 native `.so`，primp 从 current aligned release 加载 |
| `verification/strict-runtime-manifest.json` | schema 4、113 ELF、包版本/wheel hash/strict flags 的完整 manifest |
| `verification/smolagents-toolkit-build-resume*.log` | libxml/lxml/primp/BoringSSL 交叉构建的失败与渐进闭环过程 |
| `verification/*kernel-build-smolagents-closure.log` | RV64、LA64 严格串行最终内核构建输出 |

四条非零 record 不能简单读成“最终 11/15 通过”：首候选部署由发布门禁安全拒绝，旧
启动镜像没有新 `/rescue/verify-persist-python`，`python -c` 触发 python-dotenv 的
`<string>` 路径假设，另一次 `cwd` 检查确认 prompt 中的 `/persist/apk-root` 已不存在。
后续相同目的的真实文件探针和默认命令均通过。两条超过 512 字节的命令被宿主 harness
发送前拒绝，没有 record；raw 保留但不作板端证据。

## 14. 二进制制品身份

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
| anonymous-unmap diagnostic uImage | 16,539,304 B | `5e3f6cd4...24e5` |
| P4 default strict runtime | 81,628,064 B | `a420d79d...61a9` |
| P4 default final uImage | 16,769,280 B | `a5e60c0d...5da1` |
| post-board reproducible host package | 81,630,652 B | `b7f36138...244c` |
| aligned Pillow/SmolAgent final runtime | 82,412,900 B | `43d7bb2e...579e` |
| Pydantic 1.10.26 pure wheel | 166,975 B | `c43ad70d...d917` |
| SmolAgents toolkit final runtime | 87,057,368 B | `28f61fb...e75e` |

完整 64 位哈希见 `ARTIFACTS.sha256`，缩写只用于表格可读性。

## 15. 重分析方法

若原始 `target/perf-runs/<run>` 仍存在，可重新运行：

```text
python3 scripts/kernel_perf.py analyze \
  --run-dir target/perf-runs/<run-id>
```

匿名释放专项表：

```text
python3 scripts/analyze_anon_unmap.py \
  --run-dir target/perf-runs/20260717T-anon-unmap-quant
```

复核 CPython L3-L9：

```text
python3 judge/judge_cpython-isolated.py \
  < docs/09_debug/la64_on_board/260717/raw-data/20260717T042020Z-cpython-strict-align/raw/strict_functional_l3_l9-1-bec4b123.log
```

复核最终 aligned Pillow/SmolAgent runtime：

```text
python3 scripts/kernel_perf.py analyze \
  --run-dir target/perf-runs/20260717T-aligned-pillow

python3 judge/judge_cpython-isolated.py \
  < docs/09_debug/la64_on_board/260717/raw-data/20260717T-aligned-pillow/raw/cpython_l3_l9_final-1-f58ddacf.log
```

复核 SmolAgents 三项内置工具闭包：

```text
python3 scripts/kernel_perf.py analyze \
  --run-dir target/perf-runs/20260718T050500Z-smolagents-toolkit-closure
```

报告复核顺序应为：

1. `manifest.json` 确认 build/platform/storage/suite；
2. `records.jsonl` 确认 rc、wall、sample 和 raw 文件；
3. raw 中确认 begin/end/rc marker 完整；
4. reports 只作聚合，不覆盖 raw；
5. 跨 run 比较前检查 workload、warmup、storage、suite 和内核哈希；
6. 任一身份不同则降低比较等级，不补造下降百分比。

## 16. 未归档为“原始数据”的内容

- 81,627,728–87,057,368 B runtime archives、uImage 和 ELF：体积大，已记录 hash；
- P4 失败 staging 目录：只在实板保留，不属于 canonical runtime；
- 尚未执行的 PMU、30 分钟稳定性、SmolAgent 固定本地端点/真实 API：没有数据，不能在
  报告中补全；默认 SmolAgent、AgentImage 和三项工具离线构造已通过，但不等价于真实
  搜索、网页下载或模型请求；
- develop 分支新 ext4 结果：尚未产生，未来必须新建独立 run，不能覆盖本批次。
