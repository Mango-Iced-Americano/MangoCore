---
title: "BuildStorm Linux/QEMU 对照诊断方案"
category: testing
status: planned
author: MangoCore Team
last_update: 2026-08-11
tags: [testing, buildstorm, linux, qemu, performance, diagnostics]
---

# BuildStorm Linux/QEMU 对照诊断方案

## 1. 目标与范围

本实验用于回答：在相同 RV64 QEMU、CPU、内存、磁盘镜像、Rust 工具链和
BuildStorm 工作量下，MangoCore 相对 Linux 的额外时间究竟来自：

- Cargo/rustc DAG 和单 crate 用户态计算；
- timer 抢占、context switch、迁核或 runqueue 等待；
- page fault、MM 激活和 TLB 路径；
- PageCache、ext4、journal 或小粒度块请求；
- QEMU vCPU 调度、宿主机竞争或磁盘等待。

时间紧迫，本轮只运行 **一次 RV64 Linux/QEMU 低开销诊断 BuildStorm**。不再增加
无采样的干净性能基线，不做重复轮次，也不在完整 BuildStorm 上使用
`perf record`、`strace`、`trace-cmd` 或 eBPF 全量追踪。

诊断轮应尽量运行到 `BUILDSTORM_COMPILE ok=true`；若环境或时间不允许完整结束，
至少运行到 `core` crate 退出并覆盖随后 codegen/LTO 并行阶段。现有 MangoCore
full-diag 作为对照基线。

## 2. 测试合同

### 2.1 QEMU 和资源

Linux 与 MangoCore 必须使用相同的：

- QEMU 可执行文件及版本；
- `virt` machine、8 vCPU、8 GiB 内存；
- `-accel tcg,thread=multi`；
- CPU affinity、NUMA 和宿主机资源范围；
- VirtIO-MMIO 块设备模型；
- drive cache、aio、discard 和 detect-zeroes 参数；
- OpenSBI/firmware 层级，或者明确记录 Linux 启动路径中的差异。

运行期间不得并行启动其他 QEMU、内核编译或重磁盘任务。启动前记录宿主机
load、PSI、可用内存和磁盘空间。

### 2.2 BuildStorm 输入

推荐 Linux 从 initramfs 或独立只读 root 启动，把官方 RV `pub` 镜像的独立
overlay 作为 BuildStorm 数据盘挂载并 chroot。这样 Linux 启动过程不会提前修改
BuildStorm 文件系统。

chroot 中准备：

```text
/proc
/sys
/dev
/tmp
```

必须保持一致：

- 官方 BuildStorm 脚本；
- `/work/tgoskits` 源码及 `Cargo.lock`；
- Rust nightly、Cargo 和 RISC-V target；
- `uname -m=riscv64`；
- `target/riscv64gc-unknown-linux-musl` 的清理规则；
- 编译命令、环境变量、产物检查和成功 marker。

本轮虽然不是干净性能基线，仍应从 golden 创建新的独立 overlay。删除 target
失败必须 fail-closed，不能带残留进入计时阶段。

## 3. 必须归档的身份信息

运行前保存：

```text
manifest.txt
qemu-version.txt
qemu-command.txt
linux-kernel-config.txt
kernel-cmdline.txt
input-sha256.txt
guest-environment.txt
```

至少包含：

- QEMU、Linux kernel、OpenSBI 的版本/hash；
- kernel config 中 SMP、SCHEDSTATS、TASKSTATS、PSI、PERF_EVENTS、EXT4/JBD2 状态；
- 官方镜像、overlay backing、BuildStorm 脚本、Cargo.lock 和源码 hash；
- `rustc -Vv`、`cargo -V`、`uname -a`、`lscpu`；
- `/proc/cmdline`、`mount`、`lsblk` 和 `/sys/block/vda/queue/*`；
- 完整 QEMU 命令和 CPU affinity；
- 测试开始时的宿主机 load、PSI、内存与磁盘状态。

同时记录 guest `/proc/uptime` 和宿主机 monotonic 时间，避免把 QEMU 暂停或宿主机
争用误算成 guest 内核时间。

## 4. 唯一诊断轮的采样层级

### 4.1 宿主机：每 1 秒

宿主机采样是跨 MangoCore/Linux 最稳定的统一口径。记录：

- QEMU 总 user/system CPU、RSS；
- 每个 vCPU 线程的 CPU time、运行 CPU、context switch 和 migration；
- QEMU cgroup throttling；
- 块设备 read/write bytes、IOPS、平均请求大小、await 和 util；
- CPU、I/O、memory PSI；
- 宿主机 load、iowait、steal 和可用内存。

可使用：

```bash
pidstat -h -u -r -w -t -p "$QEMU_PID" 1
iostat -x -y 1
vmstat -w 1
```

并读取：

```text
/proc/$QEMU_PID/stat
/proc/$QEMU_PID/status
/proc/$QEMU_PID/task/*/stat
/proc/$QEMU_PID/task/*/schedstat
/proc/pressure/{cpu,io,memory}
```

可复用并改造现有 `host-arceos-rv64/collect_host_metrics.py` 的 CSV 口径。

### 4.2 Linux guest：每 1 秒轻量汇总

记录：

- `/proc/stat` 的 per-CPU user/system/idle/iowait；
- BuildStorm 进程树中的 runnable、D-state 和活跃 rustc 数；
- `/proc/loadavg`；
- `/proc/pressure/{cpu,io,memory}`；
- `/proc/diskstats` 和 `/sys/block/vda/stat`。

这一层不遍历所有线程的重型文件，只提供并行波形和 I/O 时间线。

### 4.3 Linux guest：每 5 秒任务快照

发现 BuildStorm 根 PID 后，递归枚举其全部后代和线程，读取：

```text
/proc/PID/task/TID/stat
/proc/PID/task/TID/status
/proc/PID/task/TID/schedstat
/proc/PID/task/TID/sched
/proc/PID/task/TID/wchan
/proc/PID/task/TID/syscall
/proc/PID/cmdline
/proc/PID/io
```

输出字段：

```text
timestamp, pid, tid, ppid, state, cpu, comm, exe, crate
user_ticks, system_ticks, run_ns, rq_wait_ns, timeslices
voluntary_cs, involuntary_cs, migrations
minor_faults, major_faults, read_bytes, write_bytes
wchan, syscall_id
```

从 rustc argv 的 `--crate-name` 提取 crate。该表与 MangoCore 的
`user_us/kernel_us/blocked_us/runnable_wait_us/current_cpu/syscall_id` 对齐。

### 4.4 Linux guest：每 30 秒重型快照

保存：

```text
/proc/vmstat
/proc/meminfo
/proc/slabinfo
/proc/interrupts
/proc/softirqs
/proc/diskstats
/proc/pressure/*
/proc/fs/jbd2/*/info
/sys/block/vda/stat
/sys/block/vda/queue/*
```

重点计算：

- minor/major fault、anon/file page、pgscan/pgsteal；
- timer interrupt、IPI 和 TLB shootdown；
- block request、sector、平均请求大小和完成延迟；
- ext4/JBD2 transaction 与 commit；
- reclaim、writeback 和 dirty page 变化。

## 5. `core` crate 专项数据

`rustc --crate-name core` 是当前 MangoCore 最长串行段。Linux 诊断轮必须记录：

- exec、首次采样、opt/LTO worker 出现和进程退出时间；
- 完整 argv 和必要环境变量；
- leader、ctrl-c、主工作线程及全部 `opt_cgu.*`、`lto_cgu.*` TID；
- 每线程 user/system、runqueue wait、阻塞、context switch、migration；
- minor/major fault、read/write bytes、wchan 和 syscall；
- 同期 QEMU vCPU CPU time 和宿主磁盘增量。

若唯一完整诊断轮仍不能区分用户计算与内核税，才对捕获的原样 `core` 命令做一次
单独重放，并可使用软件 perf event：

```bash
perf stat \
  -e task-clock,context-switches,cpu-migrations,page-faults,minor-faults,major-faults \
  -o core-perf-stat.txt \
  -- <exact-rustc-command>
```

QEMU TCG 下不默认采用 `cycles`、`instructions` 或虚拟 PMU 数据。rustc
`-Z self-profile`、`-Z time-passes=json` 和 `strace -f -c` 也只允许用于这一可选
单 crate 重放，不能加入主 BuildStorm。

## 6. 输出目录

建议使用单独、未跟踪的结果目录：

```text
build/buildstorm-linux-qemu-diag-YYYYMMDD/
├── manifest.txt
├── qemu-version.txt
├── qemu-command.txt
├── linux-kernel-config.txt
├── kernel-cmdline.txt
├── input-sha256.txt
├── guest-environment.txt
├── build.log
├── markers.log
├── exit.status
├── artifact.txt
├── guest-system-1s.csv
├── guest-tasks-5s.csv
├── guest-heavy-30s.log
├── guest-disk-1s.csv
├── host-qemu-1s.csv
├── host-vcpu-1s.csv
├── host-iostat-1s.csv
├── host-pressure-1s.csv
└── core-lifecycle.csv
```

日志、镜像和 overlay 不提交 Git。文档只记录输入 hash、执行命令、结果路径和最终
归因结论。

## 7. 结果计算

### 7.1 全局指标

```text
guest_avg_busy_cores = guest_total_task_cpu / compile_wall
host_qemu_avg_cores  = qemu_host_cpu / host_wall
avg_read_size        = block_read_bytes / block_read_requests
avg_write_size       = block_write_bytes / block_write_requests
```

比较 MangoCore 与 Linux 的：

- `BUILDSTORM_BEGIN -> BUILDSTORM_COMPILE` 墙钟；
- 总 user/system CPU 与平均忙核；
- QEMU host CPU-seconds；
- runnable/rq-wait、context switch 和 migration；
- fault、block request、journal commit 和 I/O wait；
- 各主要 crate，尤其 `core` 的阶段墙钟和 CPU 构成。

### 7.2 判定规则

| 对照结果 | 结论 |
|---|---|
| 两边 `core` 都是单热线程，用户态墙钟接近 | Cargo/rustc/TCG 固有关键路径 |
| MangoCore nonvoluntary switch、MM activate 或 migration 显著更多 | timer 无竞争抢占或迁核放大 |
| MangoCore kernel time、minor fault 或 TLB 工作显著更高 | MM、page fault 或 trap 路径 |
| Linux 平均块请求更大、IOPS 更少且 crate 更快 | Mango PageCache/readahead/请求合并不足 |
| Mango runnable wait 高且其他 CPU 空闲 | 调度放置或唤醒延迟 |
| guest 用户时间相近但 Mango 的 QEMU host CPU-seconds 更高 | trap、TLB、SBI 或设备路径放大 |
| 两边平均忙核都约 3.2 | 低并行度主要来自 Cargo DAG，不再修改 placement |

## 8. 执行顺序与停止条件

```text
核对 QEMU、镜像、工具链和源码身份
  -> 启动宿主机 1 秒 collector
  -> 启动 Linux guest collector
  -> 运行官方 BuildStorm
  -> 优先等待完整 COMPILE marker
  -> 停止 collector，保存产物与状态
  -> 与 MangoCore full-diag 对齐分析
  -> 只有结论仍不充分时才重放 core
```

以下情况立即判为无效并停止：

- 输入 hash、target 或工具链不一致；
- target 清理失败；
- QEMU 参数、CPU/内存或 VirtIO 模型不一致；
- 宿主机出现其他 QEMU、CPU throttling 或持续重 I/O；
- collector 文件缺失、时间戳不连续或 marker 无法对齐；
- 编译失败、产物不足 500000 bytes 或架构错误。

本轮不追加干净基线。诊断数据只能用于机制归因；若需要正式 Linux/MangoCore
墙钟结论，应在时间允许时另行执行无采样、重复且顺序随机化的正式 A/B。
