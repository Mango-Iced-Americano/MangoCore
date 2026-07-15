---
title: "只读 CPython 运行时的重复解析瓶颈与 P4 持久 pyc"
category: debug
status: resolved-with-known-limits
author: MangoCore Team
last_update: 2026-07-15
tags: [postmortem, performance, cpython, pyc, import, readonly, p4, cache]
code_paths:
  - "user/tools/cpython/python3-wrapper.sh"
  - "os/build_initramfs.sh"
  - "user/src/bin/initproc.rs"
related_docs:
  - "docs/09_debug/la64_on_board/development-log.md"
  - "docs/09_debug/la64_on_board/08-ahci-command-amplification.md"
  - "docs/08_testing/cpython-isolated.md"
  - "docs/08_testing/mangocore-python-guide.md"
entry_points:
  - "python3-wrapper.sh"
  - "PYTHONPYCACHEPREFIX"
  - "PYTHONDONTWRITEBYTECODE"
---

# 只读 CPython 运行时的重复解析瓶颈与 P4 持久 pyc

## 1. 摘要

2K1000LA 上 `import json,ssl,hashlib,pathlib` 基线中位数为 18.322 s。最初直觉是
SSD/AHCI 太慢，但两个对照推翻了这一判断：AHCI 从 512B/命令改为 64KiB/命令后只
降到 17.993 s（-1.8%）；把同一 CPython 树复制到 tmpfs，重导入仍约 17.45 s。

根因是 P3 运行时只读，旧 wrapper 又设置 `PYTHONDONTWRITEBYTECODE=1`。PageCache
可以缓存 `.py` 源码字节，却不能替 CPython 保存解析/编译结果。每启动一个新进程，
同一批 stdlib 模块都要重新 tokenize、parse、compile，再执行模块顶层代码。把源码
搬到 tmpfs 只消除了磁盘等待，没有消除这些 CPU 工作。

修复保留只读 P3，不复制或修改标准库；wrapper 设置
`PYTHONDONTWRITEBYTECODE=0`，并用 `PYTHONPYCACHEPREFIX` 把 pyc 重定向到 P4
`/persist/python/pycache`。首次填充生成 33 个 pyc，约 19.095 s；后续同一导入稳定
约 4.495 s，相对 18.322 s 降低 75.5%。物理 RESET 前执行 `sync`，复位后在运行
任何 Python 命令之前仍确认 33 个 pyc，证明不是内存 PageCache 假命中。

| 属性 | 结论 |
|------|------|
| 严重性 | Medium / P2；功能正确但每个 Python 进程重复支付大额 CPU 成本 |
| 修复提交 | `f133ba44`，2026-07-14 |
| 表面现象 | SSD 冷读慢、Python import 约 18 s |
| 直接根因 | 只读 runtime + 禁止写 bytecode，跨进程无编译缓存 |
| 关键反证 | tmpfs 重导入仍约 17.45 s |
| 最终方案 | 只读 P3 source + P4 外置持久 `PYTHONPYCACHEPREFIX` |
| 稳态结果 | 18.322 -> 4.495 s，-75.5% |

## 2. 证据口径：不要伪造 `real/user/sys`

仓库的 Work Log 和驱动文档保存了同一 shell `time` 口径的 wall time、中位数与
部分归因结论，但没有归档每次运行完整的 `real/user/sys` 原始三元组。因此精确证据
只能写成：

| 场景 | `real`/wall 已归档 | `user` | `sys` |
|------|--------------------:|--------|-------|
| 基线重导入 | 18.322 s | 原始精确值未归档 | 原始精确值未归档 |
| 64KiB AHCI、无 pyc | 17.993 s | 未归档 | 未归档 |
| tmpfs runtime、无 pyc | ~17.45 s | 未归档 | 未归档 |
| P4 pyc 首次填充 | ~19.095 s | 未归档 | 未归档 |
| P4 pyc 稳定命中 | ~4.495 s | 未归档 | 未归档 |

Work Log 记录“约 11.6 s 用户态解析/编译”为当时 profiling 的层级归因，但原始
`time` 三元组不在仓库，本文不把 11.6 s 包装成可逐行复核的精确 `user` 样本。

结论依赖的是三组 A/B 的方向与幅度，而不是补造缺失数字。

## 3. CPython import 的成本分层

对没有有效 pyc 的纯 Python 模块，简化流程为：

```text
open/read module.py
  -> decode/tokenize
  -> parse AST
  -> compile code object
  -> execute module top-level
  -> optionally serialize code object as pyc
```

其中只有第一步直接受 SSD 吞吐影响。PageCache 命中可以让 `read(module.py)` 不再打盘，
但后四步仍在每个新 CPython 进程执行。

有效 pyc 路径则是：

```text
stat source + validate cache
  -> read marshalled code object from pyc
  -> execute module top-level
```

它跳过 tokenize/parse/compile，但不会跳过模块执行。因而 pyc 能显著降低 import，
却不应被描述为“Python 完全不再执行初始化”。

## 4. 旧策略为何阻止缓存

运行时位于只读 P3：

```text
/tools/tests/cpython/usr/lib/python3.14/*.py
```

旧 wrapper 显式：

```sh
PYTHONDONTWRITEBYTECODE=1
```

即使 CPython 想在标准库旁创建 `__pycache__`，只读挂载也会拒绝；环境变量又让它
根本不尝试写 pyc。该组合保证系统盘不被修改，但也保证每个进程永远是“第一次
导入”。

这不是 P3 只读策略本身的错误。正确解法是分离：

```text
immutable source/runtime  -> P3
derived bytecode cache    -> P4
```

而不是放宽 P3 为可写，或把整个 768 MiB runtime 每次复制到 RAM。

## 5. 调试追溯

### 5.1 基线：Python 慢，但 BusyBox 不慢

同板基线：

```text
BusyBox true                         ~0.034 s
python3 -S -c pass                   ~1.925 s
python3 -c pass                      ~2.385 s
import json,ssl,hashlib,pathlib      18.322 s median
```

BusyBox 启动很快，说明通用 exec/scheduler 不是 18 s 的主要来源。Python 的最小启动
已明显更重，而多模块导入把差距放大。

### 5.2 AHCI 合并只改善 1.8%

64 KiB DMA 命令合并把 5.48 MiB `libpython` 冷读从 13.5 提升到 18.6 MB/s，但：

```text
import: 18.322 -> 17.993 s
```

这证明命令放大是真实 I/O 问题，却不是 18 s import 的主瓶颈。驱动优化与 Python
优化必须分别报告。

### 5.3 tmpfs 对照仍约 17.45 s

把同一 CPython runtime 树复制到 tmpfs 后：

```text
minimal startup ~1.520 s
re-import       ~17.45 s
```

若主因是 SSD 读取，tmpfs 应带来数量级下降；实际只小幅改善。这一反证将主要成本
定位到源码读出后的用户态处理。

tmpfs 不能消除 parse/compile，也不能跨物理复位保留 pyc。它是很有辨别力的诊断
对照，却不是最终持久方案。

### 5.4 开启外置 pyc 后数量级变化

首次运行需要读 `.py`、编译并写 cache，因此 19.095 s 不比基线快；它生成 33 个
pyc。之后同一模块集：

```text
stable import median ~4.495 s
```

下降幅度远大于 AHCI 和 tmpfs 对照，符合“重复 parse/compile 是主瓶颈”的机制预测。

## 6. 修复设计

### 6.1 `PYTHONPYCACHEPREFIX` 分离 source 与 cache

wrapper 的优先级：

```text
1. /persist/python/pycache   # P4 ext4，跨复位
2. /scratch/python/pycache   # P2 fallback，可写但非首选持久语义
3. /tmp/python/pycache       # tmpfs，仅当前启动
```

若外部已设置 `PYTHONPYCACHEPREFIX`，wrapper 不覆盖。应用根/chroot 将 P4
`/persist/python` bind 到 `/var/cache/mango-python`，门禁可显式设置：

```text
PYTHONPYCACHEPREFIX=/var/cache/mango-python/pycache
```

CPython 会按源文件路径在 prefix 下构造不会碰只读 P3 的 cache tree。

### 6.2 允许写 bytecode

```sh
PYTHONDONTWRITEBYTECODE="${PYTHONDONTWRITEBYTECODE:-0}"
```

默认允许写；调用者仍可显式设置为 1 做无缓存对照。这一点对于性能回归很重要：
基线必须能稳定关闭 cache，否则 A/B 会混入上一次运行留下的 pyc。

### 6.3 cache 失效由 CPython 负责

外置 cache 不等于永久信任旧 code object。CPython 正常 pyc validation 根据 source
元数据判断是否有效；P3 runtime 更新后，失配 cache 会重建。

因此不要用自定义“文件存在就直接加载”逻辑，也不要为追求命中跳过解释器自带校验。

### 6.4 wrapper 放入 initramfs

全局 `/usr/bin/python3` 优先链接：

```text
/rescue/python3-wrapper
```

P3 里的 wrapper 只作兼容 fallback。这样修改 cache、loader 或环境策略只需更新
uImage，不需重写 768 MiB P3；真正的解释器、stdlib 和私有 DSO 仍保持 P3 隔离。

## 7. 性能分层结果

### 7.1 import 主路径

| 阶段 | import wall | 相对基线 | 主要变化 |
|------|------------:|---------:|----------|
| 512B AHCI + no pyc | 18.322 s | baseline | 每进程读 `.py` + compile |
| 64KiB AHCI + no pyc | 17.993 s | -1.8% | 只减少磁盘命令 |
| tmpfs runtime + no pyc | ~17.45 s | -4.8% | 基本消除 SSD，仍 compile |
| P4 pyc first fill | ~19.095 s | +4.2% | compile + 写 33 个 pyc |
| P4 pyc warm | ~4.495 s | -75.5% | 跳过 parse/compile |

首次填充与稳定命中必须分开报告。把 19.095 s 与 4.495 s 混成一个平均数，会掩盖
cache 的冷/热语义。

### 7.2 最小启动

| 场景 | `python3 -S -c pass` | `python3 -c pass` |
|------|-----------------------:|------------------:|
| 原始基线 | 1.925 s | 2.385 s |
| 64KiB AHCI，无持久 pyc | 1.714 s | 2.175 s |
| P4 pyc 命中 | 1.159 s | 1.607 s |

`-S` 仍需要解释器启动、编码等基础 import，因此也能从 pyc 获益；普通启动还加载
site 相关模块，绝对时间更高。

## 8. 跨复位持久性证明

仅在同一启动重复运行无法排除：

- pyc 仍在 PageCache、尚未落盘；
- 第二次其实读的是进程/文件系统内存热状态；
- wrapper 回退到 tmpfs prefix。

最终门禁顺序：

1. 清理并重新填充 cache；
2. 确认恰有 33 个 pyc；
3. 执行两次 `sync`；
4. 物理 RESET；
5. 在执行任何 Python 命令之前，先由 shell 统计仍有 33 个 pyc；
6. 确认 `/usr/bin/python3 -> /rescue/python3-wrapper`；
7. 首次 `python3 -S -c pass` 为 1.433 s。

步骤 5 是关键：若先启动 Python 再数文件，解释器可能现场重新生成 cache，无法证明
跨复位复用。

## 9. 根因证明

| 候选原因 | 对照 | 结论 |
|----------|------|------|
| AHCI 512B 命令是全部原因 | 合并后 import 仅 -1.8% | 排除为主因 |
| SSD 带宽是全部原因 | tmpfs 仍约 17.45 s | 排除为主因 |
| 通用 exec/scheduler 极慢 | BusyBox true ~0.034 s | 排除 |
| 只读 P3 必须改成可写 | 外置 prefix 在保持 P3 RO 时生效 | 排除 |
| 重复 parse/compile 是主因 | warm pyc 约 4.495 s | 成立 |
| 4.495 s 只是同启动内存命中 | RESET 前 sync、后先数 33 pyc | 排除 |

## 10. 正确性与安全边界

1. pyc 是派生缓存，不是唯一数据；删除后应能从 P3 source 重建。
2. P4 是首选，因为 ext4 提供更合适的元数据语义；P2 FAT32 仅 fallback。
3. 物理复位前必须 `sync` 才能要求刚生成 cache 保留。未同步脏页突然复位后丢失，
   符合当前文件系统语义。
4. P3 更新后必须让 CPython 正常校验并重建 stale pyc，不能只按文件名命中。
5. tmpfs fallback 只优化同一启动内重复进程，不能作为跨复位性能承诺。
6. pyc 优化不解决 C extension 装载、动态链接、模块顶层执行或 Python 算法性能。
7. 精确 `real/user/sys` 原始样本缺失，今后性能脚本必须归档逐轮原始输出。

## 11. 后续性能测量模板

每个样本至少记录：

```text
image commit / uImage SHA
runtime source path
PYTHONPYCACHEPREFIX
PYTHONDONTWRITEBYTECODE
pyc count before run
pyc count after run
PageCache cold/warm condition
real
user
sys
exit status
```

实验组至少包括：

- no-pyc + SSD source；
- no-pyc + tmpfs source；
- cold-fill P4 pyc；
- warm P4 pyc；
- physical-reset warm P4 pyc。

## 12. 可复用结论

对任何只读解释型语言运行时：

```text
immutable runtime/source
  + per-process parse/compile
  + cache cannot be written beside source
  = every process pays cold compilation cost
```

不要把只读系统盘改成可写；优先寻找语言提供的外置 cache prefix，将派生 cache 放在
独立可写、可清理、可做版本校验的位置。先用 tmpfs 对照区分 I/O 与 CPU，再决定是否
需要持久 cache。

## 13. 最终因果链

```text
P3 standard library read-only
  + PYTHONDONTWRITEBYTECODE=1
  -> no pyc can be retained
  -> each new Python process reparses/recompiles stdlib
  -> import median 18.322 s

64KiB AHCI command batching
  -> import only 17.993 s

move source to tmpfs
  -> import still ~17.45 s
  -> disk I/O is not dominant

PYTHONPYCACHEPREFIX -> P4
  + allow bytecode writes
  + normal source validation
  + sync + physical-reset verification
  -> 33 pyc persist
  -> stable import ~4.495 s (-75.5%)
```
