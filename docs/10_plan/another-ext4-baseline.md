# Phase 0/1：another_ext4 基线与验证协议

**状态：协议冻结；Phase 1 source/build isolation 已通过 remediation-2，当前数值仍仅为历史参考，baseline runner 仍为 NO-GO**
**日期：2026-07-19**
**关联计划：[another-ext4-migration.md](another-ext4-migration.md)**

## 1. 基线目的与证据等级

本文件定义 another_ext4 接入前的可复现实验协议。基线必须能区分后端语义、PageCache 状态、BlockDevice 传输和 QEMU 环境噪声，不能只比较一个总分。所有未来实现都必须先通过同一协议，再决定是否允许进入 VFS 或 mount。

`testresult/archive_20260709_164909` 仅作参考，不是当前基线证据。它包含 `summary.txt`、`output-rv64.txt` 和 `output-la64.txt`，记录时间为 2026-07-09 16:49:09。该归档缺少当前 commit hash、Docker container hash 或 container ID、镜像 hash、测试盘 hash、重复运行信息和本协议要求的 BlockDevice/PageCache counters。因此不能据此声称当前版本无回归，也不能把其中的数值当作 another_ext4 结果。

归档中的 sysfs lwext4 counters 读取返回 ENOENT，故没有可用的 lwext4 计数器快照。历史输出也不具备本次新后端的计数器证据。

## 2. 历史归档中可复核的内容

### 2.1 原始 QEMU 命令

RV64：

```text
qemu-system-riscv64 -machine virt -kernel kernel-rv -m 1024 -nographic -smp 1 -bios default -drive file=sdcard-rv.img,if=none,format=raw,id=x0 -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 -drive file=disk.img,if=none,format=raw,id=x1 -device virtio-blk-device,drive=x1,bus=virtio-mmio-bus.1 -no-reboot -rtc base=utc -device virtio-net-device,netdev=net,bus=virtio-mmio-bus.7 -netdev user,id=net
```

LA64：

```text
qemu-system-loongarch64 -kernel kernel-la -m 1G -nographic -smp 1 -drive file=sdcard-la.img,if=none,format=raw,id=x0 -device virtio-blk-pci,drive=x0 -drive file=disk-la.img,if=none,format=raw,id=x1 -device virtio-blk-pci,drive=x1 -no-reboot -device virtio-net-pci,netdev=net0 -netdev user,id=net0 -rtc base=utc
```

归档摘要还记录 QEMU timeout 为 7200s，RV64 和 LA64 的 QEMU rc 均为 0。这里仅描述归档文件中的记录，不表示本次执行过这些命令。

### 2.2 FS-test 片段

两个架构的串口输出都记录了 `=== FS Test Suite ===` 和 73 项测试。可见片段包括：

```text
PASS: create+write /tmp/fs_test/file OK (18 bytes)
PASS: read /tmp/fs_test/file OK
FAIL: symlinkat returned -38
FAIL: readlink target='' (expected '/tmp/fs_test/file')
PASS: ftruncate hole zero-filled (16 bytes at offset 20)
PASS: large file 64KB write+read OK
```

RV64 还记录了 `getdents_1000_8k entries=1000 calls=4`、`getdents_1000_64k entries=1000 calls=1` 和 `repeated_lookup existing_ok=100`。这些片段可帮助选择未来 conformance case，但没有提供 another_ext4 对照，也不能单独解释失败原因。

### 2.3 历史 lmbench 片段

归档摘要的 JSON 中可见以下结果，单位沿用归档标签。以下只是历史观测，不是当前承诺的 baseline：

| 架构和 libc | 项目 | result | archive baseline |
|---|---|---:|---:|
| RV64 musl | Simple syscall，微秒 | 34.62 | 9.25013 |
| RV64 musl | Simple read，微秒 | 78.4247 | 16.8192 |
| RV64 musl | Simple open/close，微秒 | 195.4444 | 501.2352 |
| RV64 musl | Pipe bandwidth，MB/sec | 32.71 | 127.244 |
| LA64 musl | Simple syscall，微秒 | 5.1976 | 9.25013 |
| LA64 musl | Simple read，微秒 | 48.8661 | 16.8192 |
| LA64 musl | Simple open/close，微秒 | 62.0319 | 501.2352 |
| LA64 musl | Pipe bandwidth，MB/sec | 32.63 | 127.244 |

### 2.4 历史 IOZone 片段

归档摘要中可见的 RV64 musl 项目包括：

| 项目 | result，KB/sec | archive baseline，KB/sec |
|---|---:|---:|
| write/read 4 initial writers | 2278.55 | 3524.04 |
| write/read 4 readers | 2283.37 | 13135.64 |
| random-read 4 random readers | 1596.72 | 11082.03 |
| fwrite/fread 4 freaders | 1514.40 | 7123.18 |
| pwrite/pread 4 pread readers | 2368.65 | 14183.61 |

归档中还包含 RV64 glibc 的相邻项目，例如 `write/read 4 initial writers = 2281.63 KB/sec`、`write/read 4 readers = 2291.31 KB/sec`。本节不把这些值合成为“无回归”结论，也不推断不同 libc 或架构之间可以直接比较。

## 3. 新基线强制 manifest

每次可验收的基线都必须在 `docs/Work_Log/evidence/YYYY-MM-DD/` 下归档。每个运行目录至少包含：

```text
git-hash.txt
container-id.txt
config.txt
qemu-output.log
qemu-head-tail.txt
command-and-status.txt
manifest.txt
metrics.csv
counters.csv
```

`manifest.txt` 必须包含：

* MangoCore commit 的完整 SHA 和 dirty 状态。
* Docker image digest、container ID、宿主机工作目录到容器的 mount 映射。
* kernel、initramfs、sdcard、tools disk 的文件大小和 SHA256。
* QEMU、OpenSBI、Rust nightly、编译器、测试 runner 和 libc 版本。
* 架构、CPU 数、内存、机器类型、块设备模型、网络模型、时钟设置和完整 QEMU 命令。
* backend 选择、启动或显式 mount 路径、another_ext4 fork URL、branch 和精确 40 位 pinned commit。
* `os_test.conf` 全文或 checksum、mask、测试顺序、过滤项、超时和环境变量。
* 计数器开关、采样时点、采样开销对照和分析工具版本。
* 开始时间、结束时间、退出状态、失败重试说明和证据生成时间。

任何字段缺失都只能标为参考运行，不能进入性能门禁。

## 4. 样本、重复与可比环境

### 4.1 固定样本

每个后端和每个架构至少执行以下样本集：

1. FS semantics：文件、目录、权限、rename、link、unlink、truncate、hole、extent 边界、inode 复用和 recovery。
2. PageCache：cold read、warm read、跨页 read、顺序 read、随机 read、readahead、eviction、dirty writeback 和 redirty retry。
3. Sync：fsync、syncfs、unmount、重新挂载和设备 flush 故障注入。
4. lmbench：至少保留 Simple syscall、read、write、stat、fstat、open/close、pipe、fork、pagefault、file bandwidth、mmap 和 context switch。
5. IOZone：initial writer、rewriter、reader、re-reader、random reader/writer、reverse reader、stride reader、fwrite/fread、pwrite/pread 和 pwritev/preadv。

### 4.2 重复次数

正式性能样本每个组合至少重复 5 次，另加 1 次预热运行。报告中保留每次原始结果、median、p10、p90 和离散度。若运行失败，保留失败日志并说明是否重跑，不能只保留最好的一次。语义测试必须至少执行一次冷启动和一次 warm cache 运行，故障注入 case 必须重复到能确认错误不是偶发日志噪声。

### 4.3 环境控制

RV64 和 LA64 必须在 Docker 内串行执行，先 RV64，再 LA64，不得并行切换 nightly。固定 QEMU 单核、内存、磁盘镜像、块设备、时钟、网络、日志级别、测试盘内容、测试配置和 runner 顺序。比较时不得混用不同 commit 的 kernel、initramfs、sdcard 用户态和 `os_test.conf`。任何构建或测试都必须在 Docker 中完成，证据归档必须写入 `docs/Work_Log/evidence/YYYY-MM-DD/`，不能用根目录 `testresults/` 代替。

## 5. 指标与低开销 counters

### 5.1 用户可见指标

* FS-test 每个 case 的结果、errno、字节数、操作次数和总耗时。
* lmbench 每个项目的原始延迟或带宽，不只报告 composite score。
* IOZone 每个 workload 的 KB/sec、文件大小、记录大小、读写模式和 libc。
* QEMU 启动时间、测试总时间、超时、退出状态和 panic。
* 内存峰值、PageCache 页面数量、Dirty 页面、Writeback backlog 和 reclaim 事件。

### 5.2 内核 counters

counter 设计必须默认关闭，启用时使用低开销的单调计数器，并提供无探针对照。最小集合为：

| 层 | 最小 counters |
|---|---|
| BlockDevice/adapter | read/write/flush 次数，批次页数和字节数，短读/短写，设备错误，重试，部分完成，提交等待时间 |
| PageCache | hit、miss、Loading、UpToDate、Dirty、Writeback、Redirty、retry、readahead、eviction、generation mismatch |
| metadata | inode metadata dirty，transaction begin/commit，JBD2 checkpoint，orphan add/remove，recovery replay |
| 同步路径 | fsync、syncfs、unmount 的各阶段耗时和失败阶段 |

每个高频 counter 必须有 probe tax 样本。不得把未拆分的剩余 cycles 直接归因于 another_ext4。计数器快照要能按 workload 窗口计算增量，避免只看跨运行累计值。

## 6. 比较矩阵与决策门

每个架构至少包含下列行：

| 对照 | 启动后端 | 显式 mount 后端 | 目的 |
|---|---|---|---|
| A | lwext4 | lwext4 | 当前可用后端的同路径参考 |
| B | 旧 ext4 | 旧 ext4 | 记录当前显式 mount 分歧 |
| C | lwext4 | another_ext4，显式测试开关 | 评估候选后端，不改变默认 |
| D | another_ext4，仅在后续阶段允许 | another_ext4 | 验证完成后的候选完整路径 |

C 和 D 在 Phase 0 不执行，因为运行时激活尚未获准。A/B conformance 先比较语义，再比较性能。任何数据内容不一致、错误码不一致、flush 顺序不一致或 inode 复用交叉写回，都直接阻断后端接入。

性能门限固定为：相对同架构、同 workload、同 libc 的对应基线，关键指标退化超过 5% 必须调查，达到或超过 10% 默认阻断。只有记录根因、影响范围、回滚方式、书面批准和后续修复计划后，才可接受超过 10% 的例外。性能改善不等于语义通过。

## 7. 未来执行顺序与证据检查

1. Phase 1 已完成 source provenance、subtree sync、UPSTREAM.md 和精确 Mango commit pin：`dragonos` 到 `sync` 的 lineage split 为 `571b85084fade21f5c26726a78e71356210c4f86`，远端为 `git@github.com:Mango-Iced-Americano/another_ext4.git`，分支为 `mango`，最终 pin 为 `6887c41ef212b483a6841c87cb4d4b025b8d2c1b`。证据目录为 `docs/Work_Log/evidence/2026-07-19/another-ext4-phase1-build/`。
2. Remediation-2 已在 Docker 中按 RV64 后 LA64 串行通过 isolation gate 和全部四次最终编译，四次均 exit 0：`rv64-default`、`rv64-feature-on`、`la64-default`、`la64-feature-on`。命令和结果见 `remediation-2-result-status.txt`，对应完整日志为 `remediation-2-rv64-default.log`、`remediation-2-rv64-feature-on.log`、`remediation-2-la64-default.log`、`remediation-2-la64-feature-on.log`。这只是 compile isolation 证据，不是 QEMU、runtime validation、语义或性能证据。
3. 后续才运行语义和故障注入，再运行 lmbench 与 IOZone。每个运行检查 manifest 字段、时间戳新鲜性和完整日志。
4. 父级验收者必须核对证据目录包含 git hash、container 映射、config、完整 QEMU 输出、首尾片段、命令状态、metrics 和 counters。缺字段的结果不得标为通过。
5. 汇总按架构、libc、后端和 workload 分组，保留原始样本和统计摘要，最后才进入第九阶段 performance decision。

本文件不报告新的 QEMU、运行时激活、语义、性能回归或无回归结果；Phase 1 没有 another_ext4 运行时实现或路由变化。早期 gate 和编译失败日志仍保留在同一证据目录，并由 `remediation-artifact-verification.txt` 标记为 retained diagnostics，最终 remediation-2 结果不删除这些历史诊断。`scripts/run_lwext4_baseline.sh` 仍是 Oracle 判定的 NO-GO 草案，未获准作为 baseline 结果或性能结论的来源。
