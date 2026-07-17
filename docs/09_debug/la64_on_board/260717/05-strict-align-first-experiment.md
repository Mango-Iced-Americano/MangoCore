---
title: "Python 第一次优化实验：完整 strict-aligned runtime"
category: debug
status: completed-with-timing-caveat
author: MangoCore Team
last_update: 2026-07-17
tags: [python, cpython, strict-align, loongarch64, pgo, lto, deployment, ext4, board]
code_paths:
  - "scripts/build_cpython_runtime_la64_strict.sh"
  - "scripts/deploy_cpython_runtime.py"
  - "scripts/run_cpython_bench_matrix.py"
  - "user/tools/cpython/run_strict_benchmark.sh"
  - "user/tools/cpython/run_strict_functional.sh"
  - "user/tools/cpython/strict_runtime_smoke.sh"
  - "os/src/hal/arch/loongarch64/trap/mod.rs"
related_docs:
  - "docs/09_debug/la64_on_board/260717/02-unaligned-trap-root-cause.md"
  - "docs/09_debug/la64_on_board/260717/04-ext4-small-file-path.md"
  - "docs/09_debug/la64_on_board/260717/06-raw-data-index.md"
---

# Python 第一次优化实验：完整 strict-aligned runtime

## 0. 最终结论

第一次处理没有修改内核非对齐模拟器，也没有采集 PC Top-N；按决定直接把 CPython
3.14.5、musl 和全部原生依赖用
`-march=loongarch64 -mabi=lp64d -mstrict-align` 重编译，并复用 2026-07-16 的旧
对照数据。

实板结果为：

- runtime smoke、CPython L3-L9 `72/72`、18/18 benchmark 全部通过；
- 18 个正式 workload body 的非对齐总数、handler ticks、2/4/8-byte load/store 和
  浮点 load/store 全部为 0；
- 强匹配 `bm_float` 从 3,000,039 次 trap 降到 0，handler ticks 从
  4,767,941,219 降到 0；sys 从 50.070 s 降到 0.027 s；
- 近匹配 `bm_string` 从 373,371 次 trap 降到 0；
- `bm_fileio` 仍为 43.127 s、sys 79.81%，说明 ext4 小文件瓶颈独立存在。

旧 production 总时间 1,928.806 s、新 strict diagnostic 总时间 303.470 s，表面为
6.36 倍，但这不是隔离的正式性能 A/B：两侧内核 build mode 不同，新侧重编了完整
依赖闭包且每项只有一个样本。正式结论是“已覆盖 body 的非对齐 trap 被消除”，不是
“production Python 已确认提速 6.36 倍”。

## 1. 实验决策与不变量

用户明确指定：

1. 直接按 strict-aligned 重编所有用户态原生组件；
2. 不测 PC Top-N；
3. 内核模拟器先不修改；
4. 旧控制组直接使用已归档结果，不重新跑；
5. ext4 未来迁移到新实现，本轮主要记录 trap 数和下降趋势；
6. 最终测试必须在实板 ext4 上运行。

为了证明实验没有偷偷改变内核 handler，归档了两个身份：

| 对象 | SHA-256 |
|------|---------|
| 归档 diagnostic uImage | `53e43b0430d07df8c307e7d9bd61524b538e07f132133f11c4c5b4637c288e68` |
| `trap/mod.rs` 源码 | `1c513e5df2499097354e46932ff70b0101061b8c5219cc35a84f617c9e3b5471` |

源码 SHA 在实验前后相同。本次事件变化来自用户态代码生成，不是 handler 被加速或
禁用统计。

## 2. 为什么必须重编完整原生闭包

只重编 `python` 可执行文件不能保证解释器进程不再执行未对齐指令。动态 loader、musl、
`libpython`、stdlib C extension、OpenSSL、libffi、SQLite 等都在同一地址空间执行；
其中任何一个旧 ELF 都可能重新引入 trap。

构建脚本固定并验证以下闭包：

| 组件 | 版本/来源 |
|------|-----------|
| Linux headers | Alpine `7.1.3-r0` |
| musl | 1.2.6 + Alpine LoongArch/安全补丁 |
| zlib | 1.3.2 |
| bzip2 | 1.0.8 |
| xz | 5.8.3 |
| libffi | 3.5.2 |
| expat | 2.8.2 |
| mpdecimal | 4.0.1 |
| OpenSSL | 3.5.7 |
| ncurses | 6.6-20260516 |
| readline | 8.3 + patch 001–003 |
| SQLite | 3.53.3 |
| CPython | 3.14.5 |
| compiler | crosstool-NG 1.28.0 / GCC 15.2.0 / musl target |

公共 C/C++ flags 为：

```text
-Os -fstack-clash-protection -Wformat -Werror=format-security -fno-plt
-march=loongarch64 -mabi=lp64d -mstrict-align
```

脚本启动时用 GCC 编译 strict flag probe，并检查 `libgcov.a` 中存在完整 profiler
runtime；避免在耗时构建末尾才发现工具链只能编译、不能链接 PGO。

## 3. CPython PGO/LTO 构建与审计

CPython 保留 `--enable-optimizations` 和 `--with-lto`，而不是为了 strict-align 退化为
普通 `-O0/-O2` 构建。构建脚本还处理了两项交叉编译细节：

- CPython 的 PGO profile 命令不自动经过 `HOSTRUNNER`，脚本对 recipe 做精确验证后
  才注入 QEMU user runner；recipe 不匹配则拒绝继续；
- GCC 15.2 的并行 LTO link 可能耗尽容器资源，最终 PGO/LTO 阶段使用受控并发，AR/
  RANLIB 也使用 `gcc-ar/gcc-ranlib` 保留 plugin metadata。

训练记录：

| 指标 | 值 |
|------|---:|
| 上游测试文件 | 43/43 被运行 |
| 测试数量 | 9,846 |
| `.gcda` 文件 | 309 |
| `.gcda` 总大小 | 2,318,060 B |
| failed files | 13 |
| env_changed | 1 |

最终 config/sysconfig 同时验证 `-mstrict-align`、`-fprofile-use`、
`--enable-optimizations` 和 `--with-lto`。但 13 个测试文件失败、1 个环境变化，部分
可选/测试扩展还有 `-Wmissing-profile`。正确表述是“成功采集并使用 PGO，且最终运行时
保留 LTO”；不能表述为“上游测试全绿”或“94 个 ELF 每个函数都有完整 profile”。

## 4. 运行时制品身份

最终制品：

| 项目 | 值 |
|------|---|
| 文件 | `cpython-la64-strict-3.14.5-abbc714ce59f.tar.xz` |
| 大小 | 81,627,728 B |
| SHA-256 | `abbc714ce59f105fe1ebaab00cc053cea3d09161a6e51c2431112b6beeaff56a` |
| runtime manifest SHA-256 | `2be976aabcf2a3f964447a3cbaca818f588683e6d3540bb40b6c5ad3eabe447c` |
| ELF 数量 | 94 |
| board canonical | `/persist/pyperf/r/s-abbc714ce59f` |
| board results | `/persist/pyperf/o/sa-20260717a`，18 JSONL |
| functional work | `/persist/pyperf/f-20260717a` |
| benchmark work | `/persist/pyperf/w-sa-20260717a` |

manifest 对每个 ELF 记录相对路径、SHA-256、SONAME 和 NEEDED 依赖，可检查 loader、
`libpython`、原生 `.so` 是否来自同一闭包。runtime、suite、work、tmp、pycache 和结果
全部位于 P4 `/persist` ext4；P3 `/tools` 没有写入或覆盖。

## 5. 部署过程与两次失败

### 5.1 首包与旧 Python `tarfile` 超时

第一版制品为：

```text
cpython-la64-strict-3.14.5-dbdb27d10477.tar.xz
size   81,598,372 B
sha256 dbdb27d10477d91ce9e1c1d9ff98241a6b2e9810800c1d72b847c0f8ce2853dd
```

初版 deployer 用 P3 旧 CPython 的 `tarfile.extractall()` 在 P4 解压。宿主 capture 900 s
超时返回 124，但板端前台解压仍未结束，Ctrl-C/审计命令无法取得 shell。随后 11 s 的
审计和 11 s 的串口恢复超时只是同一失联现场，不是三个独立 runtime 错误。用户物理
复位后才重新取得控制面。

这次失败没有写 P3，也没有发布 canonical runtime；只留下隐藏 staging
`/persist/pyperf/r/.s-dbdb27d10477.staging`。为避免慢递归删除再次占用串口，本轮保留
该目录，它不在 canonical 路径且未参与任何测试。

### 5.2 BusyBox tar 拒绝显式根成员

改用板端 BusyBox 原生 `tar/xz` 后，首版 archive 仍含显式根成员 `./`。当前
MangoCore VFS/ext4 在已存在 staging 根中重建 `./` 时返回失败，解包退出 1。

最终重新打包只省略这个合成根成员：原包 8,810 members，新包 8,809 members；对
其余成员的 path、type、mode、size、link target 和 content hash 做归一化比较，没有
差异。因此这是 archive 兼容性规范化，不是删除 runtime 文件。

### 5.3 最终安全发布流程

最终 deployer 的阶段门禁为：

```text
宿主检查 archive path/type，拒绝绝对路径、.. 和危险链接
  -> P4 mount、大小、rw 和 canonical parent 身份检查
  -> 下载到 .part
  -> 板端 SHA-256 校验
  -> BusyBox tar/xz 解到隐藏 staging
  -> staging strict-runtime smoke
  -> sync
  -> 原子 rename 为 s-abbc714ce59f
  -> sync
  -> 删除 .part
```

只有 smoke 成功后的 staging 才能成为 canonical。`failures.csv` 中四行全部发生在
benchmark 前，不能从 18/18 通过率中扣除，也不能为了“报告好看”删掉。

## 6. 功能门禁

| 门禁 | 结果 | 证据 |
|------|------|------|
| host/QEMU runtime smoke | PASS | import 原生扩展、sysconfig flags、thread create/join |
| rv64 kernel build | PASS | 严格串行构建日志已归档 |
| la64 kernel build | PASS | 严格串行构建日志已归档 |
| 实板 runtime smoke | PASS | canonical 前 staging smoke + 发布后 smoke |
| CPython L3-L9 | `72/72` | 项目 judge，组 rc=0 |
| 18 项 benchmark | `18/18` | 一次预热 + 一次正式 |

项目 judge 命令为：

```text
python3 judge/judge_cpython-isolated.py \
  < raw/strict_functional_l3_l9-1-bec4b123.log
```

输出 `{"all":72,"pass":72}`。PGO 上游训练的失败不能替代这套 MangoCore 实板正确性
门禁；两者目的不同，均在报告中保留。

## 7. strict 实板 18 项正式结果

每项完成一次预热；预热后 reset counter，只包正式 workload body：

| benchmark | elapsed/s | user/s | sys/s | sys/elapsed | unaligned traps |
|-----------|----------:|-------:|------:|------------:|----------------:|
| bytesio | 5.826148 | 4.467131 | 1.349930 | 23.17% | 0 |
| chaos | 14.591695 | 14.513306 | 0.050051 | 0.34% | 0 |
| decimal | 15.291264 | 14.606998 | 0.648359 | 4.24% | 0 |
| dict | 6.246292 | 4.953064 | 1.280471 | 20.50% | 0 |
| fileio | 43.127077 | 8.607959 | 34.418366 | 79.81% | 0 |
| float | 10.358258 | 10.316053 | 0.027063 | 0.26% | 0 |
| fork | 38.012917 | 0.554104 | 0.780300 | parent only | 0 |
| hash | 3.416400 | 3.357461 | 0.056183 | 1.64% | 0 |
| json_loads | 80.795074 | 80.288421 | 0.297004 | 0.37% | 0 |
| list | 21.122467 | 12.969275 | 8.105259 | 38.37% | 0 |
| nbody | 7.595484 | 7.563465 | 0.020751 | 0.27% | 0 |
| pidigits | 4.669502 | 3.658342 | 1.003495 | 21.49% | 0 |
| regex | 4.537687 | 4.290953 | 0.239877 | 5.29% | 0 |
| richards | 2.521149 | 2.509645 | 0.010856 | 0.43% | 0 |
| sort | 9.567168 | 8.590621 | 0.953681 | 9.97% | 0 |
| spectral_norm | 8.880512 | 8.840344 | 0.026346 | 0.30% | 0 |
| string | 1.354068 | 0.908570 | 0.446945 | 33.01% | 0 |
| thread | 25.557253 | 23.755384 | 0.857149 | 3.35% | 0 |
| **累计** | **303.470416** | **214.751096** | **50.572086** | — | **0** |

18 个窗口不仅 total=0；handler ticks、load2/4/8、store2/4/8 和 float load/store 也
全部为 0。不存在“总数解析为 0，但分类计数仍有残留”的矛盾。

## 8. 旧 trap 对照与证据等级

| workload | 旧 trap | strict trap | handler ticks | trap 下降 | 可比性 |
|----------|---------:|------------:|--------------:|----------:|--------|
| float | 3,000,039 | 0 | 4,767,941,219 → 0 | 100% | 强匹配：同内核、suite、runner、ext4 |
| string | 373,371 | 0 | 783,003,473 → 0 | 100% | 近匹配：workload/runner hash 同，旧 suite 容器总 hash 不同 |
| nbody | 39 | 0 | 62,820 → 0 | 100% | 边界负对照：旧侧 FAT32，不能做热路径时间 A/B |
| fileio | 4,458 | 0 | 6,270,720 → 0 | 不报告 | 不配对：旧 100 文件/256 KiB，新 5,000/10 MiB |

其余 14 项没有旧侧同一 body 的 trap counter，只能报告 strict post-only=0，不能编造
“下降 100%”。

最强的时间侧证据是 float 匹配诊断：

| 指标 | 旧 | strict |
|------|---:|-------:|
| elapsed | 72.080175 s | 10.358258 s |
| user | 21.895004 s | 10.316053 s |
| sys | 50.070068 s | 0.027063 s |
| trap | 3,000,039 | 0 |
| handler ticks | 4,767,941,219 | 0 |

旧侧约 50 s sys 随 trap/handler 一起消失，与旧 handler/sys=95.22% 的归因闭合。由于
旧样本没有预热、用户态闭包被完整重建且各侧单样本，该表仍是机制证据，不升级为
正式 production 性能收益。

## 9. 时间趋势为何只能辅助使用

旧 production 18 项累计 `1,928.805693 s`，strict `perf_diag stats_on=1` 累计
`303.470416 s`，表面 `6.36×`、下降 `84.27%`。限制有四项：

1. 旧侧 production，新侧 diagnostic；已知 feature 会改变 ELF 布局；
2. 新侧重编 CPython、musl 和全部原生依赖，不能只归因某个 CPython 编译单元；
3. 每项只有一个正式样本，旧控制组按决定没有重跑；
4. `fork` parent rusage 不包含 children，wall/user/sys 不能直接相加比较。

因此 `strict_align_timing_trend.csv` 保留每项表面趋势，供未来相邻 production A/B
选热点，但本次对外结论只使用 trap 事件。

## 10. ext4 反证与剩余边界

strict `bm_fileio` 仍为 `43.127 s`，其中 sys `34.418 s`、metadata 5,000 文件
`41.170 s`。与旧 production `48.677 s` 相差远小于 float/string/regex 的趋势，并且
strict body 已 0 trap。这支持 ext4 是独立瓶颈；处理边界见
[04-ext4-small-file-path.md](04-ext4-small-file-path.md)。

本次 0 trap 只覆盖 18 个 workload body。解释器启动、第三方 package import、未来
wheel、显式 packed pointer dereference 和手写汇编仍可能产生非对齐访问。后续如果把
strict build 固化为生产 runtime，必须要求所有本地 C/C++ extension 继承相同编译策略，
并保留内核模拟器作为兼容兜底。

## 11. 原始证据

- [完整 strict 对照报告](raw-data/20260717T042020Z-cpython-strict-align/reports/strict_align_comparison.md)
- [18 项正式样本](raw-data/20260717T042020Z-cpython-strict-align/reports/cpython_bench_samples.csv)
- [18 项 counter delta](raw-data/20260717T042020Z-cpython-strict-align/reports/counter_deltas.csv)
- [trap 配对表](raw-data/20260717T042020Z-cpython-strict-align/reports/strict_align_trap_comparison.csv)
- [辅助时间趋势](raw-data/20260717T042020Z-cpython-strict-align/reports/strict_align_timing_trend.csv)
- [部署失败表](raw-data/20260717T042020Z-cpython-strict-align/reports/failures.csv)
- [L3-L9 原始串口](raw-data/20260717T042020Z-cpython-strict-align/raw/strict_functional_l3_l9-1-bec4b123.log)
- [runtime manifest](raw-data/strict-runtime-build/strict-runtime-manifest.json)
- [完整构建日志](raw-data/strict-runtime-build/build-full-gcc15.log)
- [恢复构建日志](raw-data/strict-runtime-build/build-full-gcc15-resume.log)
- [rv64 内核构建日志](raw-data/strict-runtime-build/verify-rv64-kernel.log)
- [la64 内核构建日志](raw-data/strict-runtime-build/verify-la64-kernel.log)

## 12. 2026-07-17 标准运行时入口固化

第一次实验最初只证明 `/persist/pyperf/r/s-abbc714ce59f` 可用，标准 tools 镜像仍可能
通过 `fetch_cpython_runtime.py` 取得未保证 strict-align 的通用 runtime。后续已将该
风险关闭：

- `os/Makefile tools-cpython-la` 固定调用 strict builder 和 verified installer；
- 根目录增加 `cpython-la64-runtime-build/verify/install` 一键 Docker 目标；
- builder 从通过完整校验的 artifact 生成 `current.json`，不以旧 TFTP manifest 或目录
  是否存在作为 runtime 身份；
- installer 校验 archive/sidecar/manifest/94 ELF，拒绝路径逃逸和特殊成员，在同目录
  staging 写 stamp 后原子替换；
- 标准缓存 `user/tools/loongarch64/tests/cpython` 已实际安装，stamp 中 artifact SHA、
  manifest SHA 和 policy 与本文件第 4 节一致。

实板仍使用 P4 canonical，未写 P3。P4 `strict_runtime_smoke.sh` 再次完成 manifest、
原生扩展和线程门禁。完整固化过程和匿名释放后续量化见
[07-strict-runtime-and-anon-unmap-quantification.md](07-strict-runtime-and-anon-unmap-quantification.md)。
