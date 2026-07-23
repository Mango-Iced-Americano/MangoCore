# 2K1000LA CPython strict-aligned 实板对照报告

## 结论

将 CPython 3.14.5 及其原生依赖闭包统一按 `-march=loongarch64 -mabi=lp64d -mstrict-align` 重编译后，2K1000LA 实板上 18/18 个正式 benchmark 全部通过；每项均执行一次预热和一次正式样本。只包住正式 workload body 的 18 个诊断窗口中：

- `user_unaligned_traps` 全部为 `0`；
- 2/4/8-byte load/store 分类全部为 `0`；
- 浮点 load/store 分类全部为 `0`；
- unaligned handler total/max ticks 全部为 `0`。

最强匹配对照是 `bm_float`：同一份归档诊断内核、同一 suite、同一 workload/runner、同为 P4 ext4，旧 runtime 为 `3,000,039` 次 trap 和 `4,767,941,219` handler ticks，严格对齐版为 `0/0`，trap 下降 `100%`。`bm_string` 的 workload 与 runner 文件哈希也完全一致，旧 `373,371` 次、严格版 `0`，下降 `100%`。

因此，本轮可以确认：此前 Python body 中的非对齐异常风暴来自用户态 CPython/原生库生成的非对齐访问；完整 strict-aligned 重编译能够在已覆盖的 18 项 workload body 中消除这条内核模拟路径。该结论不依赖 PC Top-N，也没有修改内核模拟器。

## 实验边界与身份

| 项目 | 值 |
|---|---|
| 实板 | 2K1000LA，串口 `/dev/cu.wchusbserial120` |
| 源码 | `HEAD 934c4af9f9c84d38b4dff9c7c2a58bccc83f6ee9` + manifest 中 dirty/build-input 指纹 |
| 内核 | 归档 `perf_diag` uImage，SHA-256 `53e43b0430d07df8c307e7d9bd61524b538e07f132133f11c4c5b4637c288e68` |
| unaligned handler 源码 | SHA-256 `1c513e5df2499097354e46932ff70b0101061b8c5219cc35a84f617c9e3b5471`，重编译前后未变 |
| benchmark suite | revision `c50669c2b59a7d6d979fb12aea42c1b508ed3765`，suite SHA-256 `6a4c6a1896cbbe1ae55be8fe1149c679bacdbf4a05759b7db3280593c10e0ce1` |
| pyperformance 参考 | revision `216cbeb5f828b8ee5864f9bb52f3563d2d1a4846` |
| strict runtime | `/persist/pyperf/r/s-abbc714ce59f` |
| runtime 压缩包 | 81,627,728 B，SHA-256 `abbc714ce59f105fe1ebaab00cc053cea3d09161a6e51c2431112b6beeaff56a` |
| runtime manifest | SHA-256 `2be976aabcf2a3f964447a3cbaca818f588683e6d3540bb40b6c5ad3eabe447c` |
| 板端正式结果 | `/persist/pyperf/o/sa-20260717a`，18 个 JSONL |
| 板端 work/tmp | `/persist/pyperf/w-sa-20260717a` |
| 功能门禁目录 | `/persist/pyperf/f-20260717a` |
| 文件系统 | runtime、suite、work、tmp、pycache 和结果全部位于 P4 `/persist` ext4 rw |

P3 `/tools` 未写入。旧对照数据直接复用 2026-07-16 留档，没有重新跑控制组。没有采集 PC Top-N，没有修改 `os/src/hal/arch/loongarch64/trap/mod.rs` 的模拟逻辑，也没有实施内核性能优化。

## strict runtime 构建审计

严格标志应用于 musl 1.2.6、zlib、bzip2、xz、libffi、expat、mpdecimal、OpenSSL、ncurses、readline、SQLite 和 CPython 3.14.5 的原生闭包；使用 GCC 15.2.0 crosstool-NG musl 工具链。最终 runtime manifest 审计了 94 个 ELF，CPython 保留 PGO 与 LTO。

PGO 训练通过 QEMU 启动上游 `--pgo` 任务，运行 43/43 个测试文件、9,846 个测试，产生 309 个 `.gcda`、共 2,318,060 B。训练命令本身不是功能全绿：13 个测试文件失败、1 个环境变化，主要位于交叉 QEMU 执行边界；最终编译也对未被训练覆盖的可选/测试扩展发出 `-Wmissing-profile`，所以 PGO 覆盖不是 94 个 ELF 的全覆盖。构建策略只在 profile 数据实际存在时继续，并使用 `-fprofile-use -fprofile-correction`。因此这里声明的是“PGO 数据已采集并使用”，不是“上游 CPython test 全通过”。MangoCore 上的功能正确性由后述独立 72 项门禁判定。

规范化压缩包包含 8,809 个成员。它与首次生成的 8,810-member 包相比只省略了合成根成员 `./`，路径归一化后的逐成员 metadata/content 清单一致，runtime manifest 哈希也一致。

## 功能与构建门禁

- strict runtime 离线 LoongArch QEMU import/thread/sysconfig smoke：通过；
- Docker 内严格串行 `rv64-kernel-build-only`：退出 0；
- Docker 内严格串行 `la64-kernel-build-only`：退出 0；
- 实板 runtime smoke：通过；
- 实板 CPython L3-L9：项目自带 `judge_cpython-isolated.py` 对原始串口日志判定为 `72/72`，组退出码 0；覆盖文件、启动、语言、stdlib、signal、ext4 文件系统、线程、subprocess、DNS、HTTP 和 HTTPS；
- 正式 benchmark：18/18 通过，无 benchmark panic、hang 或内容校验失败；
- 末尾 postflight：`/persist` 为 ext4 rw，P4 大小 4 GiB，18 份板端 JSONL 存在，runtime manifest 哈希一致，`sync` 退出 0。

## 18 项严格版正式结果

以下都是实板 P4 ext4、一次预热后的一次正式 body 样本。`fork` 的 user/sys 是父进程 rusage，不包含 child。

| benchmark | elapsed | user | sys | sys/elapsed | unaligned traps |
|---|---:|---:|---:|---:|---:|
| bytesio | 5.826 s | 4.467 s | 1.350 s | 23.17% | 0 |
| chaos | 14.592 s | 14.513 s | 0.050 s | 0.34% | 0 |
| decimal | 15.291 s | 14.607 s | 0.648 s | 4.24% | 0 |
| dict | 6.246 s | 4.953 s | 1.280 s | 20.50% | 0 |
| fileio | 43.127 s | 8.608 s | 34.418 s | 79.81% | 0 |
| float | 10.358 s | 10.316 s | 0.027 s | 0.26% | 0 |
| fork | 38.013 s | 0.554 s | 0.780 s | parent only | 0 |
| hash | 3.416 s | 3.357 s | 0.056 s | 1.64% | 0 |
| json_loads | 80.795 s | 80.288 s | 0.297 s | 0.37% | 0 |
| list | 21.122 s | 12.969 s | 8.105 s | 38.37% | 0 |
| nbody | 7.595 s | 7.563 s | 0.021 s | 0.27% | 0 |
| pidigits | 4.670 s | 3.658 s | 1.003 s | 21.49% | 0 |
| regex | 4.538 s | 4.291 s | 0.240 s | 5.29% | 0 |
| richards | 2.521 s | 2.510 s | 0.011 s | 0.43% | 0 |
| sort | 9.567 s | 8.591 s | 0.954 s | 9.97% | 0 |
| spectral_norm | 8.881 s | 8.840 s | 0.026 s | 0.30% | 0 |
| string | 1.354 s | 0.909 s | 0.447 s | 33.01% | 0 |
| thread | 25.557 s | 23.755 s | 0.857 s | 3.35% | 0 |
| **累计** | **303.470 s** | **214.751 s** | **50.572 s** | — | **0** |

## 旧 trap 基线与下降趋势

| workload | 旧 trap | strict trap | 下降 | 可比性 | 解释 |
|---|---:|---:|---:|---|---|
| float | 3,000,039 | 0 | 100% | 强匹配 | 同内核、同 suite、同 runner、同 ext4；handler ticks `4,767,941,219 → 0` |
| string | 373,371 | 0 | 100% | 近匹配 | workload/runner 文件哈希一致、同 ext4；旧 suite 容器总哈希不同；ticks `783,003,473 → 0` |
| nbody | 39 | 0 | 100% | 边界负对照 | workload 文件一致，但旧数据位于 FAT32；原本只有 39 次，不是热路径风暴 |
| fileio | 4,458 | 0 | 不报告 | 不配对 | 旧 `diag-short` 只有 100 文件/256 KiB，strict 是 5,000 文件/10 MiB |

其余 14 项没有旧侧“同一 workload body”的 trap 计数，只能报告 strict post-only=`0`，不能声称配对下降百分比。

`bm_float` 的匹配诊断样本还显示：elapsed `72.080 → 10.358 s`，sys `50.070 → 0.027 s`，同时 trap 与 handler ticks 归零。虽然旧样本没有预热、每侧都只有一个正式样本，且用户态 runtime 被完整重建，但“约 50 秒 system time 随 300 万次模拟陷阱一同消失”与先前 handler/sys=95.22% 的归因闭合，是本轮最可信的时间侧证据。

## 耗时趋势的正确口径

旧 production 18 项累计 `1,928.806 s`，strict 诊断构建累计 `303.470 s`，表面为 `6.36x`、elapsed 下降 `84.27%`。这个数字只能作为辅助趋势，不能写成隔离的 strict-align 性能收益，原因是：

1. 旧矩阵是 production 内核，新矩阵为 `perf_diag stats_on=1`；此前已经证明诊断 feature 会改变代码布局；
2. 新侧是 CPython 与全部原生依赖的全量重编译，不能只把差异归于单个 CPython 编译单元；
3. 每项只有一个正式样本，按用户要求没有重跑旧控制组；
4. `fork` 的 parent rusage 不含 child，不能用 user+sys 解释全部 elapsed。

正式结论因此只使用 trap 事件：18 个 strict body 全为 0；其中 float/string 有旧侧匹配证据，均下降 100%。耗时表保留在 `strict_align_timing_trend.csv`，供后续 production-to-production 复测时参考。

## ext4 仍然独立存在

严格对齐后 `bm_fileio` 仍为 `43.127 s`，其中 sys `34.418 s`、占 elapsed `79.81%`；5,000 个小文件 metadata 阶段单独为 `41.170 s`。相对旧 production `48.677 s` 只下降约 11.4%，远小于 float/string/regex 等计算型路径的趋势，而且 strict 窗口已经没有非对齐 trap。

这进一步支持“ext4 小文件固定税是独立问题”，但本轮按约定不优化、不继续拆分；等待队友 develop 分支的新实现后，用同一 P4 ext4 workload 复测。

## 部署异常与数据质量

`failures.csv` 中 4 条失败都发生在 benchmark 前，不是 18 项测试失败：

1. 首次用 P3 旧 Python `tarfile` 解压 81.6 MB runtime，900 秒超时且板端前台不可中断，用户物理复位；
2. 随后的超时审计与串口恢复各 11 秒超时，是同一失联现场的记录；
3. 首版 BusyBox tar 包含显式根成员 `./`，当前 MangoCore ext4/VFS 拒绝重新创建该目录，退出 1。

最终处理仅修改测试部署工具：宿主先校验成员路径与 SHA，板端用 BusyBox 原生 tar/xz 解压；压缩包省略合成根成员并保留其余成员内容/metadata。规范化包在 P4 解压、smoke、原子发布后完成 72/72 与 18/18。P4 还留有首次超时产生的隐藏 staging 目录 `.s-dbdb27d10477.staging`，为避免再次触发慢递归删除，本轮未清理；它不在 canonical runtime 路径，也未参与测试。

## 后续验收建议

- 将 `-mstrict-align` 固化到 LoongArch CPython runtime、musl、所有原生依赖和后续第三方 C/C++ extension/wheel 的构建策略；缺一个原生扩展就可能重新引入 trap。
- 下一次只做 production-to-production 相邻 A/B，仍使用 18 项相同 suite 和 P4 ext4；先看 trap 是否继续为 0，再报告正式性能收益。
- 对启动/import/第三方包单独开 body 窗口；本轮的 0 只覆盖 18 个正式 workload body，不能外推为任意 Python 程序永远不会触发非对齐访问。
- 内核模拟器仍保留作兼容/兜底；本轮不改它。匿名页释放 O(N²) 也完全未修改，后续按既定顺序单独优化和验收。

## 数据位置

- 本轮 manifest：`target/perf-runs/20260717T042020Z-cpython-strict-align/manifest.json`
- 原始记录：`target/perf-runs/20260717T042020Z-cpython-strict-align/records.jsonl`
- 原始串口：`target/perf-runs/20260717T042020Z-cpython-strict-align/raw/`
- 正式样本：`reports/cpython_bench_samples.csv`
- 计数器差值：`reports/counter_deltas.csv`
- trap 对照：`reports/strict_align_trap_comparison.csv`
- 耗时趋势：`reports/strict_align_timing_trend.csv`
- 旧 production：`target/perf-runs/20260716T102350Z-cpython-ext4-production/`
- 旧深入诊断：`target/perf-runs/20260716T-cpython-deepdiag/`
- 旧相邻 float A/B：`target/perf-runs/20260716T-perf-diag-structural-ab-run/`
