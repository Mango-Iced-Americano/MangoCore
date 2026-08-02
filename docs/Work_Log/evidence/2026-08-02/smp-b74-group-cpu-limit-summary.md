# B74 线程组 CPU 限额证据

## 结论

状态：`pass`。

`RLIMIT_CPU` 已从线程私有 TCB 迁入 PCB，并使用线程组累计时间触发进程共享
`SIGXCPU`/`SIGKILL`。热路径只执行本地批量结算和 PCB 原子操作；rlimit mutex 与 signal
queue 只在用户返回安全点访问。双架构编译、现有 CPU-limit 行为和初赛基线均未退化。

## 官方语义与设计

Linux 6.6 的 CPU timer 使用线程组 `CPUCLOCK_PROF` 样本检查 `RLIMIT_CPU`，先检查 hard
limit；soft 命中后将当前 soft limit 增加一秒，使超限进程随后每秒再次收到 `SIGXCPU`：

- <https://github.com/torvalds/linux/blob/v6.6/kernel/time/posix-cpu-timers.c>
- <https://github.com/torvalds/linux/blob/v6.6/include/linux/sched/signal.h>

MangoCore 的等价路径分为两层：TCB 在锁内记录最多 1ms 本地尾数，释放 `task.inner` 后原子
冲刷 PCB；越线只发布 `expiry_pending`。用户返回安全点领取事件，在 PCB rlimit 锁内判定并
发布下一阈值，解锁后加入进程共享 signal queue。退出和 schedule-out 强制冲刷尾数。

两个并发窗口采用以下约束关闭：

1. `rearm_cpu_limit()` 重设阈值后只允许发布 pending，禁止无条件清零，避免覆盖另一 CPU
   刚发布的越线事件。
2. 安全点处理完旧阈值并发布下一阈值后再次读取累计值，覆盖处理期间另一 CPU 已跨过新阈值
   的窗口。

线程 clone 共享 PCB；普通 fork 复制限制但构造新的零值累计；exec 复用 PCB，因此保留限制、
累计和 soft 推进状态。

## 冻结源码

- 基线 commit：`90b3b7c75190cd9ac6e0578cb196c8cc3f33bcf0`
- 生产代码 diff SHA-256：
  `6dd9a621d12c2b6307393394a4c66ba64c2f71c837433dd88722cf53d4e30589`
- `os/src/task/process.rs`：
  `eadebb130beb4d95e40e4b5ab5f99a9d5d4a78c1e09a945bccd11408c4888563`
- `os/src/task/task.rs`：
  `faec5d4729be8de9b6359647d96301ad1ddd9477dd7a244c6f713ab09039877a`
- `git diff --check`：通过。

DeepSeek 的单独冻结 diff 审查进程退出成功但输出为空，按规则不计作 PASS；随后只读验证任务
完整读取 diff、执行六项 recipe 并给出证据边界。GPT/Codex 另外手工证明 account/rearm、
account/threshold update 和双 safe-point 领取的交错，不接受仅凭模型结论验收。

## Docker 冻结验证

本地任务：`smp-b74-cpu-rlimit-validation-r1`。六项 recipe 严格串行、全部
`CORE_NUM=8`；每项 source before/after 指纹一致，`mutation_detected=false`。

| Recipe | 结果 | 耗时 |
|---|---:|---:|
| `rv64-kernel-build` | PASS | 127.817s |
| `la64-kernel-build` | PASS | 132.538s |
| `rv64-prlimit-gate` | PASS | 157.282s |
| `la64-prlimit-gate` | PASS | 161.781s |
| `rv64-preliminary` | PASS | 401.754s |
| `la64-preliminary` | PASS | 351.301s |

两种 focused 日志均打印 `online_mask=0xff`，musl/glibc 各运行
`getrlimit01..03` 与 `setrlimit01..06`，每套 9/9、0 fail/skip。两架构的
`setrlimit06` 都打印 “Got SIGXCPU then SIGKILL after reaching both limit”。

初赛 `mask=0x003` 的独立 judge 结果为：

| 架构 | basic musl/glibc | busybox musl/glibc | 总分 | 基线判定 |
|---|---:|---:|---:|---|
| RV64 | 102/102、102/102 | 54/55、54/55 | 312/314 | 未扩大失败集合 |
| LA64 | 100/102、100/102 | 54/55、54/55 | 308/314 | 未扩大失败集合 |

原始日志没有 kernel panic、fatal trap、timeout 或 runner failure。DeepSeek 报告中“除 kill
外全部通过”的概括不适用于 LA64；最终证据以上表和原始 judge 为准。

## 证据边界

- 两个线程在不同 CPU 上合计越过进程 soft/hard limit：`not-run`。现有 `setrlimit06`
  主要是单线程忙循环。
- 1ms × live-thread 的最大触发滞后：`not-run`；当前只由常量、强制冲刷路径和源码证明约束。
- 多线程同时进入安全点时进程共享信号的去重/分发交错：`not-run`。
- 降低 CPU limit 与另一 CPU 同时记账的精确窗口：`not-run`；当前由 monotonic pending 和
  阈值发布后复查证明。
- `getrusage(RUSAGE_SELF)`、`times()`、process CPU clock 的 user/system 组级查询仍未完成，
  不属于本节点验收声明。
- `RLIMIT_NOFILE` 仍依附 fd table，等待与 `CLONE_FILES` 跨进程生命周期解耦。

## 协作与效率记录

本轮执行顺序在 RV64/LA64 间多次切换，触发了不必要的共享工具链重编译。结果有效，但后续
冻结验证应按“同一架构 build→focused→preliminary，再切换另一架构”分组；能自行构建的 QEMU
recipe 可同时承担编译门禁，避免为机械满足清单重复构建。
