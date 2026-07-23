---
title: "strict-aligned 运行时固化与匿名页释放实板量化"
category: debug
status: completed
author: MangoCore Team
last_update: 2026-07-17
tags: [python, cpython, strict-align, mmap, munmap, vma, complexity, perf-diag, ext4, board]
code_paths:
  - "scripts/build_cpython_runtime_la64_strict.sh"
  - "scripts/install_cpython_runtime_la64_strict.py"
  - "scripts/run_cpython_bench_matrix.py"
  - "scripts/analyze_anon_unmap.py"
  - "os/src/task/perf.rs"
  - "os/src/mm/vma.rs"
  - "os/src/mm/vma_set.rs"
  - "os/src/fs/sysfs/files/diag.rs"
related_docs:
  - "docs/09_debug/la64_on_board/260717/03-anonymous-unmap-quadratic.md"
  - "docs/09_debug/la64_on_board/260717/05-strict-align-first-experiment.md"
  - "docs/09_debug/la64_on_board/260717/06-raw-data-index.md"
  - "docs/09_debug/perf_diag.md"
---

# strict-aligned 运行时固化与匿名页释放实板量化

## 0. 本轮结论

本轮完成两件事，仍未实施匿名页释放优化：

1. 将 LA64 strict-aligned CPython 从一次性实验制品固化为标准 tools/runtime 入口：
   `os/Makefile tools-cpython-la`、根目录 build/verify/install 三个目标都只接受经过完整
   校验的 strict 制品，不再静默回退到通用 Alpine CPython；
2. 在不增加 syscall/公开用户 ABI 的情况下，为当前 `Vma::unmap` 增加精确复杂度和耗时
   计数，在 2K1000LA P4 ext4 上完成五档居民匿名映射和六个真实 strict Python workload
   的量化。

关键结果：

- 1/4/16/32/64 MiB 主映射的实际扫描步数精确等于 `N(N+1)/2`；64 MiB 为
  `134,225,920` 步、单次内核 unmap `3.888306 s`；
- `bm_list`、`bm_dict`、`bm_bytesio` 的正式 body 中，`Vma::unmap` 内累计耗时分别占
  `11.290%`、`9.693%`、`4.298%`；
- `bm_fork` 虽有 11,386 次释放，但都较小，累计仅 `0.769%`；`bm_json_loads` 近似 0；
- 所有量化样本 `anon_unmap_errors_total=0`，无 panic/hang/ext4 错误；
- 当前缺陷已经从“代码复杂度 + 合成曲线”升级为“精确扫描次数 + 真实 Python 时间
  占比”。下一阶段可以开始优化数据结构/批量删除路径，不需要再先补 PC Top-N。

这些占比是 diagnostic build 的路径归因，不是修复后的保证提速，也不替代 production
正式数字。

## 1. strict-aligned 运行时固化

### 1.1 唯一制品策略

固定制品身份不变：

| 项目 | 值 |
|------|----|
| artifact | `cpython-la64-strict-3.14.5-abbc714ce59f.tar.xz` |
| artifact SHA-256 | `abbc714ce59f105fe1ebaab00cc053cea3d09161a6e51c2431112b6beeaff56a` |
| runtime manifest SHA-256 | `2be976aabcf2a3f964447a3cbaca818f588683e6d3540bb40b6c5ad3eabe447c` |
| target | `loongarch64-linux-musl` |
| strict flags | `-march=loongarch64 -mabi=lp64d -mstrict-align` |
| PGO/LTO | `true/true` |
| native ELF closure | 94 |
| policy | `mangocore-la64-strict-align-v1` |

构建脚本现在为最新通过校验的制品写
`target/cpython-strict/artifacts/current.json`。选择 current 之前会重新检查 archive sidecar、
目标 triple、strict flags、PGO/LTO、manifest 和所需入口，不以文件名或 mtime 代替验证。
在宿主 Docker 缺少 QEMU user 时，只允许复用已经完成 `python-target.done` 和
`runtime-package.done` 的缓存；没有完整缓存时立即报错，不会生成部分制品。

### 1.2 安全安装器

新增 `install_cpython_runtime_la64_strict.py`，安装前执行：

- archive SHA 与 `.sha256` sidecar 双重一致性检查；
- 拒绝绝对路径、越出根目录的 `..`、特殊文件、重复成员和逃逸链接；
- 逐个从压缩包读取 94 个 ELF，并与 manifest SHA-256 比对；
- 要求 loader、Python、wrapper 和 strict smoke 等入口齐全；
- 只接受 target/flags/PGO/LTO/ELF count 全部符合 policy 的 manifest。

通过后先解到同目录 staging，写 `.cpython-runtime.stamp`，再用 rename 原子替换目标；
旧目标在新 staging 发布失败时仍可恢复。标准安装已实际执行到
`user/tools/loongarch64/tests/cpython`，stamp 的 artifact/manifest hash 与上表一致。
该目录是宿主生成缓存，不是本轮向实板 P3 写入：实板测试继续复用 P4 canonical
`/persist/pyperf/r/s-abbc714ce59f`，P3 未覆盖。

### 1.3 标准 Make 入口

固化后的入口为：

```text
make cpython-la64-runtime-build
make cpython-la64-runtime-verify
make cpython-la64-runtime-install

cd os && make tools-cpython-la
```

`tools-cpython-la` 已从通用 `fetch_cpython_runtime.py` 切换为 strict builder + verified
installer；rv64 保持原通用入口。根目录目标使用项目固定 Docker image 单次运行，避免
macOS 上 compose 的 `/dev/sdb` blkio 设备映射阻止纯运行时校验。

### 1.4 实板 smoke 与 trap 口径

P4 上执行 `strict_runtime_smoke.sh`，验证 manifest 中的 target、`-mstrict-align`、PGO、
LTO、`kernel_handler_modified=false`，并导入 `_bz2/_ctypes/_decimal/_hashlib/_lzma/
_sqlite3/readline/ssl/threading/zlib` 后创建/回收线程，输出
`strict-runtime-board-smoke-ok`。

两个容易误读的样本原样保留：

- 首个 `board_strict_runtime_smoke` 直接调用 wrapper 却没有设置 `CPYTHON_ROOT`，wrapper
  按兼容默认值启动了 `/tools/tests/cpython`，只证明 wrapper 可用，不证明 P4 strict；
- 随后的 canonical/smoke 外层 `core` 快照分别看到 381/577 次非对齐 trap，但窗口还
  包含未 strict 重编的 BusyBox shell、grep、dirname 和 harness ACK，不能归因给 Python。

strict 的“workload trap=0”仍以之前 18 项 runner 内部的
`预热 → stats_off → reset → stats_on → body → stats_off` 为证据。启动/import 若要精确
拆分，需另做 strict-aligned 的最小父进程/launcher，不能使用外层 shell 总计数补结论。

## 2. 匿名 VMA 计数设计

### 2.1 插桩边界

`Vma::unmap` 只有同时满足以下条件才记录：

- `perf_stats` 编译启用；
- 当前 `stats_on=1` 且 profile 为 `memory_io`；
- VMA 为 anonymous + private，排除 file mapping 与 shared mapping。

`stats_on=0` 时不会逐页读时钟或更新扫描计数；production feature 关闭时 record 函数
编译为 inline no-op。算法本身仍是逐页调用 `remove_in_memory`，本轮没有改动释放顺序、
frame 引用、页表或 TLB 语义。

### 2.2 15 个受限计数器

`/sys/kernel/stats/anon_unmap` 暴露：

| 计数器 | 解释 |
|--------|------|
| `calls_total`、`range_calls`、`area_calls` | 总调用，并区分 range unmap 与 remove-area |
| `requested_pages_total` | 调用范围页数；可能包含未 resident 页 |
| `resident_pages_total` | 实际进入 resident 删除的页数 |
| `active_before_total/max` | 每次调用开始时 frame store active 规模 |
| `retain_scan_steps_total` | 每次现有 `VecDeque::retain` 前累加当时 `active.len()` |
| `ticks_total/max` | `Vma::unmap` 内总耗时和单次最大耗时 |
| `errors_total` | 释放错误数 |
| `pages_le_16/le_256/le_4096/gt_4096` | resident size 分桶 |

`retain_scan_steps_total` 是本轮最关键的机制计数：它记录实际遍历元素数，不以页数事后
估算，因此可以直接检验当前数据结构的复杂度。所有计数只作聚合，不输出逐页串口日志。

## 3. 构建与 QEMU 门禁

当前源码指纹以 run manifest 为准，基准 HEAD 为
`0a738d66d5caf4f816676f7f1b34af8ee00067d1`，同时保存 dirty/build-input/untracked
source hash。Docker 中严格串行完成 rv64、la64 production/perf_diag/board-shell 编译。

双架构 QEMU 均验证：

- `/sys/kernel/stats/anon_unmap` 存在，15 个键初值为 0；
- profile/reset/stats_on 可用；
- `stats_on=0` 时计数冻结；
- runtime profile smoke 到达 DONE，无 FAIL/panic。

QEMU 原始日志位于本轮 raw 目录，只作接口和双架构正确性门禁，不进入 2K1000LA 性能
排名。

实板采样后又把扫描步数更新点从“页表 unmap 前”收紧到“页表 unmap 成功后、实际
retain 前”，正常成功路径不变，只避免未来错误路径虚增一次扫描。最终源码重新完成
rv64/la64 production 和 perf_diag build，最终 QEMU kernel SHA-256 分别为
`cc68000cdd27d18fcae27bdd5f9d21ac29e21c8b4ec825dc592c11df29360cb4`、
`e54dfd325c2c3ff6bc7718b580c0dc3f674429877e5ad9e49d731ee7bc0a4ae4`，两架构 smoke
再次到达 DONE。实板性能表仍绑定下述采样 uImage；所有实板样本 errors=0，因此该位置
收紧不会改变任何已记录扫描数。

实板 uImage：

| 项目 | 值 |
|------|----|
| image | `kernel-2k1000-perf-diag-shell.ui` |
| size | 16,539,304 B |
| SHA-256 | `5e3f6cd464be0d0dc0f72a45f49f95d6d286334779908d6671252708c21624e5` |

TFTP 下载字节数、CRC32、legacy image checksum、LoongArch type/entry 均由一键启动脚本
校验。引导后 SATA scratch smoke 和 P4 application root 准备通过。

## 4. 实板环境和采样边界

实板前检确认：

- `/dev/sda4 /persist ext4 rw`；
- `/dev/sda4` 精确为 4,294,967,296 B；
- `/persist` 不是 symlink；
- strict runtime、suite、work、pycache、tmp 和 result 都在 `/persist/pyperf/...`；
- runtime canonical 为 `/persist/pyperf/r/s-abbc714ce59f`；
- suite SHA-256 为 `6a4c6a1896cbbe1ae55be8fe1149c679bacdbf4a05759b7db3280593c10e0ce1`。

前两个合并前检命令因 serial work line 超过 512 B，被宿主在发送 workload 前拒绝；
拆为短命令后通过。失败日志保留，但它们没有运行测试或写 SSD。P3 全程没有写入。

采样采用用户指定的“一次预热 + 一次正式”。合成探针每档各运行两次；真实 Python
由目标端 runner 先预热一次，再只给正式 body 开计数。外层 harness wall 包含启动、
快照和串口 ACK，报告不把它当 benchmark elapsed。

## 5. 居民匿名映射精确曲线

`diag_mmap_release.py` 创建 private anonymous mapping，逐页写入使其 resident，然后在
关闭映射前 reset/on，在 `mapping.close()` 后立即 off。正式结果：

| size | pages N | close/ms | `Vma::unmap`/ms | max call/ms | observed scans | primary `N(N+1)/2` | extra |
|-----:|--------:|---------:|----------------:|------------:|---------------:|--------------------:|------:|
| 1 MiB | 256 | 2.446 | 1.860 | 1.849 | 32,899 | 32,896 | 3 |
| 4 MiB | 1,024 | 18.336 | 17.657 | 17.646 | 524,803 | 524,800 | 3 |
| 16 MiB | 4,096 | 238.420 | 237.281 | 237.269 | 8,390,659 | 8,390,656 | 3 |
| 32 MiB | 8,192 | 959.497 | 958.383 | 958.368 | 33,558,531 | 33,558,528 | 3 |
| 64 MiB | 16,384 | 3,889.679 | 3,888.325 | 3,888.306 | 134,225,923 | 134,225,920 | 3 |

每档都有 2 次匿名 unmap、resident=`N+2`。主映射贡献精确的 `N(N+1)/2`；另一个 2 页
辅助映射贡献 `2+1=3` 步。该一致性比单纯拟合 wall 曲线更强：它直接证明每删除一页
都会重新扫描剩余 active 队列。

预热与正式 close 时间分别为：

| size | warm/ms | formal/ms | 差异 |
|-----:|--------:|----------:|-----:|
| 1 MiB | 2.413 | 2.446 | +1.38% |
| 4 MiB | 18.337 | 18.336 | -0.01% |
| 16 MiB | 237.865 | 238.420 | +0.23% |
| 32 MiB | 959.496 | 959.497 | +0.00% |
| 64 MiB | 3,889.683 | 3,889.679 | -0.00% |

旧无此精确计数器的 1–64 MiB 结果为 2.494/18.798/239.029/961.312/3,893.434 ms；
新结果分别低 1.92%/2.46%/0.25%/0.19%/0.10%。两组不是专门的同镜像探针税 A/B，
但至少没有看到计数器令 O(N²) 曲线进一步变慢；正式机制结论使用扫描步数而非这组差值。

## 6. 真实 strict Python 影响

| benchmark | body/s | anon unmap/s | 占 body | 占 sys | calls | resident pages | max active | scans | max call |
|-----------|-------:|-------------:|--------:|-------:|------:|---------------:|-----------:|------:|---------:|
| list | 21.226191 | 2.396336 | 11.290% | 29.143% | 436 | 110,176 | 3,909 | 70,469,395 | 327.313 ms |
| dict | 6.288156 | 0.609509 | 9.693% | 46.901% | 56 | 22,000 | 5,121 | 19,751,552 | 370.314 ms |
| bytesio | 5.841216 | 0.251081 | 4.298% | 18.236% | 146 | 21,698 | 1,223 | 6,398,530 | 24.344 ms |
| fork | 29.706629 | 0.228320 | 0.769% | n/a | 11,386 | 41,231 | 235 | 1,921,022 | 1.627 ms |
| thread | 25.528802 | 0.007704 | 0.030% | 0.917% | 1,122 | 1,322 | 252 | 33,442 | 1.881 ms |
| json_loads | 84.956221 | 0.000010 | 0.000% | 0.003% | 1 | 2 | 2 | 3 | 0.010 ms |

### 6.1 list/dict/bytesio

这三项都在 body 内创建并释放较大的 Python 容器或 buffer。`list` 累计扫描 7,047 万
元素，`dict` 虽只有 56 次调用，却出现 active=5,121 页的单次释放，最长 370 ms。
它们确认该缺陷会形成用户可见的长尾，并解释 strict 后仍存在的一部分 system time。

### 6.2 fork/thread

`fork` 的调用数最高，但 resident page 总量分散在大量小 VMA，最大 active 只有 235
页，因此累计只有 0.228 s。`thread` 同理。由此可以否定“calls 多就必然是 O(N²) 主
瓶颈”的简单判断，优化前后都必须保留 size/active 分布。
`bm_fork` sample 的 rusage 是 parent-only，而内核窗口包含 65 个 child，不能用两种范围
不同的数据计算 unmap/sys；表中保持 n/a。

### 6.3 json_loads 负对照

`json_loads` 84.956 s body 内只有一个 2 页映射释放，10 µs，说明计数器不会无差别给
所有 Python workload 归因高开销。该负对照同时支持 target-side stats 边界有效。

## 7. 证据等级与剩余边界

已确认：

- 当前 anonymous private `Vma::unmap` 的实际扫描复杂度为 O(N²)；
- 问题对 list/dict/bytesio 正式 body 有 4.3%–11.3% 的诊断路径占比；
- runtime/suite/work/result 全部落在 P4 ext4，结果不是 tmpfs/FAT32 路径；
- 本轮没有修改非对齐内核模拟器、VMA 删除算法、frame store 表示或 ext4。

不能外推：

- 上述占比不是修复后 production 保证提速；
- 常规 exec/exit 的 `clear_no_hole()` 不能用本计数替代；
- 六项不能覆盖任意第三方 Python package；
- 外层 shell 启动窗口的 381/577 次 trap 不能归因于 strict CPython；
- 当前 ext4 develop 新驱动尚未合入，本轮不评价其未来表现。

## 8. 原始数据与复核

文本原始数据已经从被忽略的 `target/` 复制到
[`raw-data/20260717T-anon-unmap-quant/`](raw-data/20260717T-anon-unmap-quant/)，共 44 个
manifest/records/raw/reports 文件，未复制 QEMU 磁盘和 16 MiB uImage。

重点入口：

- [聚合量化报告](raw-data/20260717T-anon-unmap-quant/reports/anon_unmap_quantification.md)
- [合成映射 CSV](raw-data/20260717T-anon-unmap-quant/reports/anon_unmap_synthetic.csv)
- [真实 Python CSV](raw-data/20260717T-anon-unmap-quant/reports/anon_unmap_python.csv)
- [全部 counter delta](raw-data/20260717T-anon-unmap-quant/reports/counter_deltas.csv)
- [结构化 records](raw-data/20260717T-anon-unmap-quant/records.jsonl)
- [实板 strict manifest smoke](raw-data/20260717T-anon-unmap-quant/raw/board_strict_runtime_manifest_smoke-1-d714c8a7.log)
- [rv64 QEMU smoke](raw-data/20260717T-anon-unmap-quant/raw/qemu_rv64_anon_diag_smoke-1-9b69be92.log)
- [la64 QEMU smoke](raw-data/20260717T-anon-unmap-quant/raw/qemu_la64_anon_diag_smoke-1-c4fcff8f.log)
- [最终 rv64 QEMU smoke](raw-data/20260717T-anon-unmap-quant/verification-final/qemu-rv64-final.log)
- [最终 la64 QEMU smoke](raw-data/20260717T-anon-unmap-quant/verification-final/qemu-la64-final.log)
- [strict current artifact index](raw-data/20260717T-anon-unmap-quant/runtime-current.json)
- [标准 tools runtime 安装 stamp](raw-data/20260717T-anon-unmap-quant/runtime-installed.stamp.json)

可重生成聚合表：

```text
python3 scripts/kernel_perf.py analyze \
  --run-dir target/perf-runs/20260717T-anon-unmap-quant
python3 scripts/analyze_anon_unmap.py \
  --run-dir target/perf-runs/20260717T-anon-unmap-quant
```
