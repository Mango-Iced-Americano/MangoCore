# 260717 Python 性能原始数据

本目录是 `target/perf-runs/` 和 `target/cpython-strict/` 中文本证据的持久归档。复制过程
不修改原始日志内容；原始记录中的绝对宿主路径、当时的 dirty status 和失败状态均按
采集时保存。

## 目录

| 目录 | 内容 |
|------|------|
| `20260716T102350Z-cpython-ext4-production/` | production manifest、33 条记录、18 项串口日志、启动/import/SmolAgent 探针和正式 CSV |
| `20260716T-cpython-deepdiag/` | 非对齐、mmap 释放、ext4/PageCache/SATA、探针税、双架构 QEMU 验证 |
| `20260716T-perf-diag-structural-ab/` | 相邻 production/diag 的构建验证日志；大 ELF/uImage 未复制 |
| `20260716T-perf-diag-structural-ab-run/` | 相邻实板 A/B 的 manifest、records、串口日志和 `structural_ab.csv` |
| `20260717T042020Z-cpython-strict-align/` | strict 部署、功能、18 项 benchmark、counter delta、失败记录和对照报告 |
| `20260717T-anon-unmap-quant/` | strict 标准入口复核、匿名 VMA 精确扫描、五档合成和六项 Python 实板占比 |
| `strict-runtime-build/` | 完整 runtime 构建日志、runtime manifest、双架构内核编译日志和包 SHA 文件 |

## 数据保真规则

- `raw/` 是串口捕获，不因解析失败而回写。
- `records.jsonl` 是 harness 结构化记录；其 `log` 字段仍指向采集时的 `target/` 绝对路径。
- `reports/` 是从原始记录生成的派生表。`regex`、`dict` 的正式 JSON 行串口缺字，报告
  依据同一日志中完整的 summary/elapsed/user/sys/PASS 重建，并明确标记 reconstructed。
- `failures.csv` 同时保存控制面/部署失败；不能仅凭该文件行数推导 benchmark pass 率。
- 二进制不在此目录。`ARTIFACTS.sha256` 保存其大小与 SHA-256，可对仍在 `target/` 的
  原文件重新校验。

完整字段和重分析方法见上级 [06-raw-data-index.md](../06-raw-data-index.md)。
