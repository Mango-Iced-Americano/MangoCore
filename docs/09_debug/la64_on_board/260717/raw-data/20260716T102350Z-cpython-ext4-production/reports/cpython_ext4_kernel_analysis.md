# MangoCore 2K1000LA Python/ext4 性能分析

日期：2026-07-16  
正式运行：`20260716T102350Z-cpython-ext4-production`  
定向诊断：`20260716T-cpython-deepdiag`  
相邻构建 A/B：`20260716T-perf-diag-structural-ab-run`

## 1. 结论摘要

本轮在 2K1000LA 实板、production 内核和 ext4 路径上完成 18/18 项 CPython benchmark，全部正常退出。正式计时区累计 1,928.806 秒。结果表明 Python 慢并非单一的解释器算力问题，而是至少三条彼此独立的内核路径叠加：

1. **已确认：实板不支持硬件非对齐访存，而当前 CPython/扩展代码生成了大量非对齐指令。** 内核把每条指令变成一次完整陷阱，并按字节调用用户访存。`bm_string` 的定向样本中，373,371 次陷阱在 Rust handler 内耗时 7.830 秒，占该样本 system time 的 91.0%；`bm_float` 中 3,000,039 次陷阱耗时 46.806 秒，占 system time 的 95.2%。这直接解释了大量“纯 Python/对象操作却有很高 sys”的现象。
2. **已确认：显式释放居民匿名映射的被测路径为 O(N²)；对完整 Python workload 的实际占比尚未测定。** 1/4/16/32/64 MiB 映射关闭分别耗时 2.494/18.798/239.029/961.312/3,893.434 ms；16 MiB 以上稳定在约 14.3–14.5 ns/page²。代码对每个 resident page 都从 `active` 向量头到尾执行 `retain`。正常 exec/进程退出主要走 `clear_no_hole()`，所以不能把该 microbenchmark 直接外推为 Python 退出主因。
3. **已确认：ext4 小文件生命周期存在很高的线性固定税，unlink 同步写回又被 SATA flush 放大。** production `bm_fileio` 的 5,000 次 create/write/read/unlink 占 46.449/48.677 秒（95.4%）。100 个小文件为 0.892 秒，单位成本 8.924 ms；5000 个为 9.290 ms/文件，操作量放大 50 倍、耗时放大 52.05 倍，当前没有 O(N²) 证据。缩小诊断中，SATA 读/写/flush 仅解释约 0.188/0.815 秒 system time（23.1%）。

因此，截图中 36.5 秒的单命令体验不能直接归因于 LLM API：production 实板上 `python -S` 热启动为 1.159 秒、正常 site 启动为 1.630 秒、固定 import 集合热运行仍需 6.769 秒；完整 benchmark runner 在进入 workload 前也可产生约 34.7 秒的运行时准备成本。真实 API 的公网和服务端排队仍必须单独测量。

本轮只增加诊断和测试能力，没有实现任何性能优化。

## 2. 测试身份与边界

- 源码：HEAD `934c4af9f9c84d38b4dff9c7c2a58bccc83f6ee9`，dirty diff SHA-256 `e5e835b9b1676857efff6ea2d336608efa53493511fcabae67b9e65f4585ad4e`；manifest 另记录 build input 与 untracked source 指纹。
- production 镜像：`kernel-2k1000-persist-shell.ui`，16,756,992 B，SHA-256 `bf1668b9bdbd1068914ac1a683ef58c821c6b03af1016f69771a9a2c25ba63c0`。
- benchmark ZIP：73,688 B，SHA-256 `5059b61e4b241f35ef2f46a859df1848dd056e3a08d2127f7b2bd340a9abdb4e`；解包 suite SHA-256 `6a4c6a1896cbbe1ae55be8fe1149c679bacdbf4a05759b7db3280593c10e0ce1`。
- Python：3.14.5，GCC 15.2.0，LoongArch64。
- 正式路径：suite `/persist/pyperf/s`、临时和 pycache `/persist/pyperf/w`，均位于 `/persist` ext4；解释器和库来自只读 `/tools` ext4。FAT32 `/scratch` 未参与正式 workload。
- 采样：依用户要求，在前三项三次复测差异很小后，正式矩阵使用一次预热加一次测量。故正式表是当前镜像的绝对基线，不能从单样本计算 CV，也不能声称与其他提交有显著差异。
- production 数字用于排名；`perf_diag` 数字只用于归因。诊断计数以 100 MHz stable counter 换算时间。
- `perf_diag` 的运行时计数开关税低于 5%，但相邻构建 A/B 证明加入诊断 feature 会改变内核代码布局，并选择性改变高频陷阱 workload 的用户态耗时。因此诊断计数只用于确认机制和次数，不把其绝对 wall/user 时间外推给 production。

## 3. 18 项 production 实板基线

| 排名 | benchmark | elapsed (s) | user (s) | sys (s) | sys/elapsed | 主要性质 |
|---:|---|---:|---:|---:|---:|---|
| 1 | bm_regex | 421.447 | 122.496 | 298.329 | 70.79% | regex/bytes，system time 异常 |
| 2 | bm_json_loads | 255.947 | 213.572 | 41.921 | 16.38% | JSON 解码，用户态算力为主 |
| 3 | bm_thread | 249.020 | 120.927 | 125.818 | 50.53% | queue/lock/condition |
| 4 | bm_chaos | 199.541 | 96.790 | 102.450 | 51.34% | 对象和控制流 |
| 5 | bm_float | 150.033 | 100.007 | 49.807 | 33.20% | 浮点对象循环，非对齐陷阱已确认 |
| 6 | bm_fork | 124.358 | 1.085 | 1.231 | 0.99%* | 65 个 child；父进程 rusage 不含 child |
| 7 | bm_spectral_norm | 99.805 | 79.153 | 20.510 | 20.55% | 数值计算 |
| 8 | bm_sort | 83.750 | 50.116 | 33.495 | 39.99% | 列表/比较/移动 |
| 9 | bm_bytesio | 69.741 | 36.508 | 33.128 | 47.50% | bytes/BytesIO |
| 10 | bm_dict | 61.230 | 34.520 | 26.612 | 43.46% | dict 操作 |
| 11 | bm_list | 54.585 | 30.053 | 24.441 | 44.78% | list 操作 |
| 12 | bm_fileio | 48.677 | 14.389 | 34.200 | 70.26% | ext4 小文件元数据为主 |
| 13 | bm_decimal | 39.153 | 28.732 | 10.341 | 26.41% | Decimal |
| 14 | bm_richards | 33.197 | 18.059 | 15.086 | 45.44% | 调度器模拟/对象图 |
| 15 | bm_string | 15.670 | 7.151 | 8.489 | 54.17% | 字符串写操作，非对齐陷阱已确认 |
| 16 | bm_nbody | 8.708 | 8.671 | 0.024 | 0.28% | 负对照：稳定用户态计算 |
| 17 | bm_hash | 7.849 | 5.782 | 2.053 | 26.16% | 哈希 |
| 18 | bm_pidigits | 6.093 | 4.873 | 1.211 | 19.87% | 大整数 |

`bm_regex` 与 `bm_dict` 的串口 JSON 行各有字符丢失，但同一日志中的完整 summary、elapsed_ns、user/sys 字段、PASS/rc=0 均保留；正式 CSV 将这些字段标记为 reconstructed，没有修改原始日志。其余 16 项由 analyzer 直接解析。

### 3.1 分阶段结果

- `bm_thread`：`queue_100000` 140.892 秒，`uncontended_locks` 70.895 秒，`event_condition` 33.010 秒，创建/回收线程合计约 4.19 秒。queue 的 put/get 在主线程串行执行，锁也无竞争，因此现有结果不能先归因于线程创建、调度器或 futex 等待；需要分阶段非对齐/futex 计数。当前 `getrusage(RUSAGE_SELF)` 实际只返回当前 TCB 的 rusage，多线程 user/sys 也不是全进程精确分解。
- `bm_fork`：spawn 40 次 76.436 秒，pipe 子进程 20 次 38.400 秒，wait/status/env 9.521 秒，共产生 65 个完整 Python child，平均约 1.91 秒/child。正常 Python 启动 1.630 秒已构成约 85% 的单 child 耗时下限；表中 1.085/1.231 秒只是 parent rusage，不能据此认为内核或 child CPU 开销低。
- `bm_fileio`：5,000 个小文件 46.449 秒；10 MiB 顺序写+fsync 1.664 秒（约 6.0 MiB/s）；50×64 KiB 普通写+fsync 0.225 秒（约 13.9 MiB/s）；重新打开读取分别 0.209/0.067 秒；seek/truncate/fsync 0.029 秒。名称中的 `direct` 只表示 `os.open/os.write`，没有使用 `O_DIRECT`。

## 4. 已确认瓶颈一：LoongArch 非对齐陷阱风暴

### 4.1 实板证据

| 定向 workload | elapsed | sys | 非对齐次数 | handler ticks | handler 时间 | handler/sys |
|---|---:|---:|---:|---:|---:|---:|
| bm_string body | 11.457 s | 8.603 s | 373,371 | 783,003,473 | 7.830 s | 91.0% |
| bm_float body | 71.117 s | 49.175 s | 3,000,039 | 4,680,636,915 | 46.806 s | 95.2% |
| bm_nbody body | 8.539 s | 0.026 s | 39 | 62,820 | 0.000628 s | 2.4% |

`bm_nbody` 是重要负对照：相同 Python、ext4、内核与采集流程下几乎没有非对齐异常，时钟也由 `sleep 1` 的宿主 1.039 秒观测排除了倍频错误。

相邻 production/`perf_diag stats_on=0` 构建使用同一 HEAD、相同非诊断 feature、相同 initramfs 文件内容、同一 Python/suite 哈希和同一 `/persist` ext4 路径，实板结果为：

| workload | adjacent production | diag stats off | 差异 | production user/sys | diag user/sys |
|---|---:|---:|---:|---:|---:|
| bm_nbody | 8.652 s | 8.547 s | -1.21% | 8.613/0.025 s | 8.511/0.024 s |
| bm_string | 15.834 s | 11.306 s | -28.60% | 7.244/8.562 s | 2.813/8.469 s |
| bm_float | 149.893 s | 72.492 s | -51.64% | 99.553/50.116 s | 22.191/50.187 s |

系统时间保持不变而用户时间大幅变化，且只有陷阱密集型 workload 受影响。内核 `.text`、trap handler 和 uaccess/page-fault 函数地址在两构建间明显移动；因此代码/缓存布局敏感是高概率解释，但没有 PMU cache-miss 数据，不能把具体的 L1/L2 冲突模式标为已确认。该结构偏差不削弱“300 万次陷阱及 handler 占 system time 95%”的机制证据，但禁止把诊断版 72 秒当作 production 的性能数字。

`bm_float` 的 600,000 次循环产生 1,800,008 次 4-byte load 和 1,200,003 次 8-byte load，精确接近每轮 3+2 次；对 sqrt/sin/cos/log/pow/exp 的隔离测试没有增加这种模式，根因位于通用 Python 数值对象/字节码生成路径，不是 libm 函数本身。

`bm_string` 产生 115,263/102,327/138,010 次 2/4/8-byte store。按 store 宽度展开为 1,743,914 个写字节，同窗口 `tlb_page=1,761,177`，二者比例为 99.02%。这证明每个非对齐 store 被拆成逐字节 `copy_to_user`，而每个 byte store 又沿 private mapping 写 fault/COW 权限恢复路径触发单页 TLB invalidate。

### 4.2 代码链

1. `os/src/hal/arch/loongarch64/trap/mod.rs:321` 接住 `AddressNotAligned`；先从用户 PC 取指并解码。
2. load/store 在 `:348-359` 按 2/4/8 字节循环，每一字节调用一次 `copy_to_user`/`copy_from_user`。
3. 每次用户访存通过 `os/src/mm/uaccess.rs:588-604` 取得当前 VM、加锁并走 `fault_in_user_va`；不是一次 trap 内只翻译一次页。
4. `os/src/mm/page_fault.rs:136-149` 把已映射 private store 统一分类为 COW。即使页面唯一拥有，`os/src/mm/vma.rs:473-478` 仍调用 `set_user_flags`。
5. LA 页表在 `os/src/hal/arch/loongarch64/laflex.rs:514-518` 每次改 flags 都 invalidate 单页 TLB。
6. 每次陷阱还在 `os/src/hal/arch/loongarch64/trap/trap.S:34-66` 保存全部 GP、FPR，并在启用时保存 32 个 LSX 向量；这些汇编入口/出口开销不在 Rust handler ticks 内，因此以上归因是下界。

实板 `CPUCFG1=0x03e2727e` 的 UAL bit 为 0；当前 CPython 是 GCC 15.2.0 的默认 LoongArch 代码生成，构建参数未见 strict-align。QEMU 的 UAL 能力为 1，所以 QEMU 功能测试无法暴露这个实板性能问题。

### 4.3 正确性风险

当前 handler 对未知 op 和用户复制错误使用 panic/unwrap；浮点非对齐路径写 `cx.fp`，而启用 LSX 时恢复完整向量上下文可能覆盖其低半部。本轮 workload 的分类计数均为整数 load/store，未触发该分支，但这是独立的正确性风险，不应与性能结论混为一谈。

## 5. 已确认瓶颈二：匿名映射释放 O(N²)

| 映射大小 | resident pages | close/munmap elapsed | sys | ns/page² |
|---:|---:|---:|---:|---:|
| 1 MiB | 256 | 2.494 ms | 4.308 ms* | 38.05 |
| 4 MiB | 1,024 | 18.798 ms | 20.601 ms* | 17.93 |
| 16 MiB | 4,096 | 239.029 ms | 241.337 ms* | 14.25 |
| 32 MiB | 8,192 | 961.312 ms | 964.130 ms* | 14.32 |
| 64 MiB | 16,384 | 3,893.434 ms | 3,897.378 ms* | 14.50 |

`*` rusage 的采样边界包含少量计时/调用开销，故小尺寸 sys 可略大于 inner elapsed；增长曲线不受影响。frame_free 约等于 pages+2，`tlb_page` 约等于 pages+33，排除了页表刷新次数自身的二次增长。

代码上，`Vma::unmap` 在 `os/src/mm/vma.rs:386-399` 枚举每个 resident page，并逐页调用 `remove_in_memory`；后者在启用 OOM handler 时于 `os/src/mm/frame_store.rs:327-333` 对整个 `active` 向量执行 `retain`。总扫描元素数为 N+(N-1)+...+1，故为 O(N²)。

这条路径只在显式 `Vma::unmap` 定向 microbenchmark 中闭环。正常 exec/进程退出主要使用 `clear_no_hole()`，本轮也没有在 18 项 workload 中采集大规模显式 munmap 的调用规模和累计耗时。因此“算法复杂度缺陷存在”为已确认，“它对当前 Python 总耗时的贡献”为证据不足。

## 6. 已确认瓶颈三：ext4 小文件与同步写回

production `bm_fileio` 的元数据阶段占 95.4%。为避免诊断版运行过久，定向样本缩为 100 个小文件、256 KiB 两种写入：

- workload 1.036332 秒，user 0.224439 秒，sys 0.814722 秒；元数据阶段 0.892372 秒。
- SATA：24 read / 98,304 B / 0.004208 秒；132 write / 1,081,344 B / 0.013981 秒；128 flush / 0.169691 秒。
- PageCache：103 write calls/166 pages；105 writeback calls/241 pages/0.165328 秒。
- heap：40,714 alloc 0.007796 秒，40,622 dealloc 0.016271 秒；page fault 513 次/0.017892 秒。
- 非对齐 handler 4,458 次/0.062707 秒，只解释约 7.8% sys，说明 fileio 的主因与 Python 对象负载不同。

SATA read+write+flush 共约 0.188 秒，占 sys 23.1%；其余约 76.9% 是 VFS/ext4/PageCache/锁/分配/复制的 CPU 路径。100 与 5000 文件的单位成本只相差 4.10%，证明本场景是稳定线性固定税，不是随文件数增长的 O(N²)。源码支持这一解释：

- `Ext4OSInode::unlink` 在 `os/src/fs/ext4/ext4fs.rs:1929-1951` 删除目录项前调用 `flush_inode_pagecache_if_dirty()`；普通 close 不同步 PageCache。
- PageCache backend `os/src/fs/page_cache.rs:2181-2247` 以物理连续 run 写出。100 个 60 B 文件各有独立 PageCache，形成 100 个 singleton writeback，无法跨文件合并。
- SATA `write_block` 在 `os/src/drivers/block/sata_blk.rs:73-102` 每次写后无条件 `controller.flush()`。缩放样本的 241 个脏页精确拆成 105 次 PageCache writeback；再加 23 个 metadata block，正好对应 128 次 `write_block`/flush。
- ext4 `supports_user_buffer_io` 在尚未创建 PageCache 时返回 false（`os/src/fs/ext4/ext4fs.rs:1171-1175`），首个 I/O 会走 `os/src/fs/vfs/file.rs:1302-1316` 的 kbuf 分配和复制 fallback。
- create miss 会重复父路径解析和不存在叶子扫描；unlink 在 MountFS 与 ext4 层再次重复查找。写路径在 ext4 更新时间戳后，`File::touch_modified` 又读取/回写 metadata。这些重复软件工作由代码确认，但各自时间占比尚未分离。

以上只描述现状和根因证据，不包含优化方案。

## 7. 其他高概率热点与计量限制

### 7.1 同步/调度

`bm_thread` 的 queue、uncontended lock、condition 三阶段合计 244.8 秒，且 production 表面 sys 占 50.5%。不过这三段主要在主线程串行执行或无竞争，实际线程创建/工作只有约 4.19 秒，不能据此把问题列为调度/futex 瓶颈。Python runtime 非对齐陷阱是高概率来源，但尚缺逐阶段计数；`RUSAGE_SELF` 与 `RUSAGE_THREAD` 在 `os/src/syscall/process/time.rs:1223-1237` 也都只读取当前 TCB。

### 7.2 fork/exec

65 个完整 Python child 的 phase wall 124.358 秒是真实体验指标，约 1.91 秒/child；正常 Python 启动的 1.630 秒是约 85% 的已测下限。当前 exec 会逐 4 KiB `pread` Python 主 ELF 与解释器 ELF，并明确绕过 PageCache 帧复用，是已确认的重复路径；但 `subprocess.run()` 是否走 vfork/posix_spawn、CoW 占比和 child rusage 尚未捕获，不能把剩余时间细分。

### 7.3 启动/import 与截图

| 场景 | warmup | measured |
|---|---:|---:|
| `python -S -c pass` | 1.177 s | 1.159 s |
| normal site `python -c pass` | 1.667 s | 1.630 s |
| 固定标准库 import 集合 | 7.877 s | 6.769 s |
| `import smolagents` | 49.347 s | 8.296 s |

SmolAgent 两次都在加载 `/persist` ext4 上的 Python site-packages 后因 `ModuleNotFoundError: PIL` 退出。为保持板上环境不变，本轮没有安装 Pillow，也没有调用真实 API；因此这两项只证明冷/热 import 成本和当前依赖缺失，不能作为成功对话或 API latency 基线。

完整 nbody 解释器进程的旧诊断窗口出现 618,412 次非对齐陷阱、handler 约 9.536 秒，而只包住 nbody body 时只有 39 次。这进一步证明解释器启动、import 和 suite 准备是截图体验中的重要组成，不应只看最终 8.7 秒算法体。

## 8. ext4 最终核验

- 测前、测试中和测后均确认 `/persist on /persist type ext4 (rw,relatime)`；`/tools` 和 `/tools/tests` 为 ext4 ro。
- 全部正式 suite、pycache、临时目录、fileio 数据均在 `/persist` ext4；没有把 FAT32 `/scratch` 的旧结果作为最终标准。
- 矩阵结束后 benchmark ZIP SHA-256 仍为 `5059...db4e`，并经历诊断版重启后再次核验；workdir 只剩 `.`/`..`，说明 wrapper 完成清理。
- 末尾 `sync` wall 0.062156 秒，未观察到残留大写回队列；整个矩阵和定向诊断中没有 panic、hang、ext4 error 或内容校验失败。

边界：P4 在运行时挂载使用，不能安全执行 offline `e2fsck`，本轮也没有做断电恢复/电源故障注入。当前证据只支持“在线 workload、sync、重启后哈希持久化均正常”，不支持宣称 ext4 已通过离线一致性或崩溃恢复验证。当前 ext4 实现也没有 journal/recovery，不能把上述测试外推为断电安全。

## 9. 探针开销

| workload | stats off | profile on | 相对开销 |
|---|---:|---:|---:|
| bm_string body | 11.328026 s | core 11.457334 s | +1.14% |
| scaled ext4 fileio | 1.023133 s | core 1.029012 s | +0.57% |
| scaled ext4 fileio | 1.023133 s | memory_io 1.036332 s | +1.29% |
| bm_float body（相邻诊断构建） | 72.492407 s | core 72.080175 s | -0.57% |

同一诊断构建内部的运行时开关差异都低于 5%，所以计数器本身的执行税可接受。结构门禁则失败：相邻 `perf_diag stats_on=0` 对 string/float 分别偏离 production -28.60%/-51.64%。因此只保留事件次数、handler 时间与调用链的机制归因；正式排名和绝对耗时必须使用 production。

## 10. 瓶颈排序与下一步验证

| 优先级 | 结论 | 等级 | 影响 | 下一步仅验证项 |
|---:|---|---|---|---|
| P0 | CPython 代码生成与实板 UAL 能力不匹配，触发逐字节内核模拟 | 已确认 | string/float/regex/对象 workload 的高 sys；启动/import | 对同一 CPython 做指令地址直方图和 ELF/编译参数审计；严格对齐构建仅作 A/B 验证，不在本轮实施 |
| P0 | private store 模拟触发逐字节 COW 权限恢复和 TLB invalidate | 已确认 | store 型字符串/bytes 路径进一步放大 | 统计 unique-page fast path、uaccess VM lock 和 per-byte fault-in 的分项耗时 |
| P0 | resident anonymous unmap 的 active.retain 为 O(N²) | 复杂度已确认，Python 影响未测 | 显式大 resident munmap | 优化前先在 Python workload 中采集显式 munmap 的 VMA 大小、次数和累计耗时 |
| 暂缓 | ext4 小文件 unlink 同步写回与 SATA 每写 flush 叠加 | 已确认 | fileio 元数据 95.4%；包缓存/pycache/大量 module 文件 | 等 develop 分支新驱动落地后，用同一 `/persist` ext4 workload 复测，不在本轮继续 |
| P2 | thread queue/lock/condition 极慢 | 已确认现象，根因未定 | Python 同步原语 | 先分阶段采集非对齐陷阱，再判断是否需要 futex/调度专项 |
| P1 | startup/import 固定成本高 | 已确认现象，根因部分确认 | 单命令体验 | 对解释器启动按 loader、open/stat/read/mmap、pyc、非对齐和退出释放分窗 |
| P2 | fork/exec 每 child 固定成本高 | 已确认现象，根因未细分 | subprocess/agent tools | child rusage + clone/CoW/exec/load/wait 分段计数 |

## 11. 数据索引

- `manifest.json`：源码、Docker、镜像和 suite 指纹。
- `records.jsonl`：每个宿主采集窗口的结构化记录。
- `raw/`：不可变串口原始日志。
- `reports/formal_benchmarks.csv`：18 项 production 正式表，含数据质量标签。
- `reports/cpython_bench_summary.csv`、`cpython_bench_samples.csv`、`cpython_bench_phases.csv`：analyzer 自动输出。
- 深入诊断原始记录：`target/perf-runs/20260716T-cpython-deepdiag/`。
- 相邻 production/diag 结构 A/B、镜像/ELF/initramfs 身份与原始串口日志：`target/perf-runs/20260716T-perf-diag-structural-ab/`、`target/perf-runs/20260716T-perf-diag-structural-ab-run/`。
- 最终诊断代码在 Docker 中严格串行通过 rv64/la64 构建，两架构 QEMU 均完成 profile/reset、stats 冻结和 runtime counter smoke；内核及 fixture 哈希见 deepdiag 的 `verification/README.md`。QEMU 只作功能门禁，不参与性能排名。

正式结论复现时必须使用 manifest 中的 image/suite 哈希，并把 production 与 perf_diag 两套运行分开；不要用 analyzer 的外层 `wall_seconds` 代替 workload 自报的 `elapsed_seconds`，前者包含解释器启动、import、预热和串口控制开销。

归档状态：2026-07-16 按用户要求停止继续采样。CPUCFG cache 几何探针未成功部署，未将 cache conflict 由“高概率”升级为“已确认”；ext4 分阶段和 Python 显式 munmap 影响量化留待对应优化前补测。
