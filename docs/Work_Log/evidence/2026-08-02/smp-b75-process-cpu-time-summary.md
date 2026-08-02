# B75 线程组 CPU 时间查询证据

## 结论

状态：`pass`。

MangoCore 的进程 CPU 时间现由 PCB 统一累计 user/system 分项；`RLIMIT_CPU` 继续使用独立、
单调的 total 作为唯一判定源。process CPU clock、`getrusage(RUSAGE_SELF)` 与 `times()` 不再
只看到调用线程，退出后的 sibling 时间也保留在最终进程快照中。双架构 8 核编译、focused
CPU-time LTP 和初赛基线均未退化。

## 官方语义与设计

对照 Linux 6.6 `kernel/sys.c`、`kernel/exit.c` 与 `kernel/time/posix-cpu-timers.c`：进程级
查询使用线程组累计，最后线程退出后保留 group dead-time，child rusage 再由 wait 路径返回。

- <https://github.com/torvalds/linux/blob/v6.6/kernel/sys.c>
- <https://github.com/torvalds/linux/blob/v6.6/kernel/exit.c>
- <https://github.com/torvalds/linux/blob/v6.6/kernel/time/posix-cpu-timers.c>

TCB 在 `task.inner` 内结算 user/system 尾数并取走本次批次，释放锁后才更新 PCB 原子。分项
用于 ABI 返回；total 在同一次 flush 中单独累加，避免 `RLIMIT_CPU` 从两次独立分项读取拼接
出跨时刻样本。当前进程查询先强制冲刷调用线程尾数；退出线程在 `live_threads` release 发布
前强制冲刷，最后线程经 acquire/release chain 保存完整 PCB 线程组快照。

普通 fork 创建零值进程累计，thread clone/exec 共享或保留原 PCB。thread CPU clock 与
`RUSAGE_THREAD` 仍使用 TCB 私有统计，没有把 process 与 thread ABI 合并。

## 冻结源码

- 基线 commit：`7d8e422a`。
- 最终 tracked/production diff SHA-256：
  `b4c5e5a0b137065c6ca325af7b83f45b9143e0fb960d0192f87d15999629fc8a`。
- RV64 干净复跑记录的 diff 指纹与上述值相同，`mutation_detected=false`。
- `git diff --check`：通过。

## Docker 冻结验证

首轮任务：`smp-b75-process-cpu-time-validation-r1`；最终 RV64 复核任务：
`smp-b75-rv64-cpu-time-r2`。所有内核构建/QEMU 都在项目 Docker 容器内执行，`CORE_NUM=8`。

| Recipe | 结果 | 耗时 | 说明 |
|---|---:|---:|---|
| RV64 kernel build | PASS | 126.132s | 首轮冻结源码 |
| LA64 kernel build | PASS | 137.883s | 首轮冻结源码 |
| RV64 CPU-time focused r2 | PASS | 205.219s | 最终 diff，musl/glibc 各 9/9 |
| LA64 CPU-time focused | PASS | 212.403s | musl/glibc 各 9/9 |
| RV64 `mask=0x003` | PASS | 353.578s | 312/314，失败集合未扩大 |
| LA64 `mask=0x003` | PASS | 404.072s | 308/314，失败集合未扩大 |

focused 集合为 `clock_getres01`、`clock_gettime01/02/04`、`getrusage01/02/04`、
`times01/03`。两架构均观察到 `online_mask=0xff`；`getrusage02` 每套 libc 有预期 TCONF 子项，
但 case-level 结果为 PASS。日志中没有 kernel panic、fatal trap 或 timeout。

初赛原始 judge 结果为 RV64 312/314（仅 busybox `kill 10` 两项）与 LA64 308/314（两套 basic
各 `test_brk` 1/3，加 busybox `kill 10` 两项），均与人工接受基线一致。DeepSeek 对 LA64 和
RV64 libc 归属的概括有误，最终证据以原始分组与 judge 计数为准。

首轮 RV64 focused 不计入验收：配方误含测试 400MB/maxrss 的 `getrusage03`，同时 ignored
runner 在 Bash 仍执行时被修改，导致后半段 `${PIPESTATUS[0]}` 文本损坏。移除无关用例并冻结
全部输入后，r2 在最终代码指纹上通过；该问题归类为 harness 输入未冻结，而非内核回归。

其后尝试删除 `finish_exit()` 看似无用的 TCB 参数，双架构 build 均在函数后半段的
`exit_task.exit_signal()` 处报 `E0425`，证明该参数仍承载 clone exit-signal 语义。该实验改动
随即撤销，没有进入提交；恢复后的生产 diff 与 r2 指纹完全相同，因此不以失败实验替代已有的
最终指纹 QEMU 证据。

## 证据边界

- 两个活跃线程在不同 CPU 上的精确 user/system 合计：`not-run`；当前由 owner 与 flush
  顺序静态证明，实时查询允许批次级近似。
- 查询与 sibling 同时 flush 时分项是否来自同一瞬时点：`not-run`；ABI 不承诺停核快照，
  `RLIMIT_CPU` 则不依赖这两个分项拼接。
- exited sibling 的 process CPU clock 保留、fork-zero 与 exec-preserve 的专项交错：
  `not-run`；生命周期由同一 PCB/新 PCB 构造路径证明。
- `wait4/raw waitid` 将保存的 child rusage 写回用户态：尚未实现，属于 B76。
- POSIX CPU timer 仍使用旧 wall-time 路径；`RLIMIT_NOFILE` 仍依附 fd table，均不属于本节点。
