# B79 进程级 legacy interval timer 证据摘要

## 目标与根因

旧实现把三类 `setitimer/getitimer` 状态放在 `TaskControlBlockInner`，并从 RISC-V trap、
WaitQueue、futex 等路径分别扣减 remaining。这既让同一线程组的 sibling 看到不同 timer，
也使 LoongArch 与 RISC-V 的刷新点不对称。B79 将权威状态迁入 PCB，并以绝对 deadline
统一表达三种时钟域。

## 官方语义对照

- Linux 6.6 `kernel/time/itimer.c` 把 timer 放在 `signal_struct`：REAL 使用 monotonic
  hrtimer，VIRTUAL 使用线程组 user CPU，PROF 使用线程组 user+system CPU。
- Linux 6.6 `kernel/fork.c::copy_signal()` 对 `CLONE_THREAD` 复用 `signal_struct`，普通 fork
  分配并初始化新对象，因此不继承 interval timer。
- exec 不清 legacy interval timer；最后线程退出才取消。`setitimer(new=NULL)` 采用停表的
  历史兼容语义，old-value copyout 失败不撤销已经提交的新状态。

## 实现协议

1. `IntervalTimerTable` 由一个 PCB mutex 保护，包含 REAL/VIRTUAL/PROF 三个绝对 deadline。
2. `set/get` 先冲刷 current 的 CPU 记账尾数，表锁内快照/提交，锁外注册 heap 和 copyout。
3. REAL action 携带 `Weak<PCB> + generation + deadline`；callback 锁内唯一领取并按旧
   deadline 批量追赶周期，锁外投递 `SIGALRM` 和重装。
4. VIRTUAL/PROF 以 Release/Acquire active hint 跳过空表；安全点锁外采样 PCB 累计，表锁内
   清 one-shot 或推进 periodic，锁外投递 `SIGVTALRM/SIGPROF`。
5. thread clone 共享 PCB；普通 fork 构造空表；exec 不清；最后线程退出推进 generation 并清表。

## 锁序与生命周期证明

```text
task.inner：领取并清空 current 的 CPU 记账尾数
  -> unlock
  -> PCB 原子累计
  -> IntervalTimerTable：读取/提交/唯一领取
  -> unlock
  -> process.signal / scheduler / KERNEL_TIMER_QUEUE
```

不存在 `IntervalTimerTable -> task.inner`、`IntervalTimerTable -> process.signal` 或
`IntervalTimerTable -> KERNEL_TIMER_QUEUE` 的持锁嵌套。heap compact 只会单向读取
`KERNEL_TIMER_QUEUE -> IntervalTimerTable`，注册/重装必须先释放表锁。

## 冻结验证

基线 HEAD：`169229c3c74c2da8a4787f935e5d0d5c1a5962f7`

| 门禁 | 架构 | 结果 | 耗时 |
|------|------|------|------|
| `CORE_NUM=8` kernel build | RV64 | PASS，exit 0 | 125.158s |
| `CORE_NUM=8` kernel build | LA64 | PASS，exit 0 | 133.627s |
| focused timer LTP + `mask=0x003` | RV64 | PASS，exit 0 | 406.791s |
| focused timer LTP + `mask=0x003` | LA64 | PASS，exit 0 | 438.430s |

所有 job 均报告 `online_mask=0xff`、无 panic/fatal marker、required marker 齐全，且前后：

- `status_sha256 = 20263120373e9d4841c2f1f3482ca00d2ba9949b35b1443407f5d317e1434050`
- `tracked_diff_sha256 = 3a2858cd39f0a25368a0e89eb6bfe5e8a2bd4d8e1db0b85ba566f309f77edf4d`
- `untracked_content_sha256 = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`

focused 在 musl/glibc 各执行 `setitimer01/02`，三类 timer 的装载、old value、信号到期和
EINVAL/EFAULT 边界均通过。初赛失败集合保持 RV64 312/314、LA64 308/314；两架构均只有
既有 basic/busybox 基线项，不因本批扩大。

## 覆盖边界

- 多 sibling 在不同 CPU 上恰好同时跨越同一 VIRTUAL/PROF deadline：NOT RUN；表锁保证
  唯一领取，但普通 LTP 不精确注入该交错。
- fork/exec/exit 与 callback 的纳秒级交错：NOT RUN；由 PCB owner、generation 与生命周期
  顺序做静态证明。
- B80 才处理 POSIX timer 的 per-timer pending/overrun 身份；它与本批 legacy timer 不混用。
