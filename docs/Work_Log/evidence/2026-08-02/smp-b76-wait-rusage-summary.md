# B76 wait 子进程资源快照证据

## 结论

状态：`pass`。

`wait4` 与 raw `waitid` 已能返回 child 的 RUSAGE_BOTH。一次 wait 事件在 child 仍由父列表固定
时生成 PID/status/rusage 完整值快照；zombie 的 syscall 回复与 parent `child_rusage` 累计复用
同一值，随后才回收 PID/registry/quota。双架构 8 核构建与 wait 生命周期 focused LTP 均通过。

## 官方语义与设计

Linux 6.6 `kernel/exit.c` 的 wait 路径在状态事件确定后取得 `RUSAGE_BOTH`；`wait4` 先写 status
再写 rusage，raw `waitid` 先写 rusage 再写 siginfo。copyout 失败不会撤销已经完成的 reap：

- <https://github.com/torvalds/linux/blob/v6.6/kernel/exit.c>
- <https://man7.org/linux/man-pages/man2/wait4.2.html>
- <https://man7.org/linux/man-pages/man2/wait.2.html>

MangoCore 的 `WaitChildResult` 直接拥有三项 Copy 值。zombie 使用最后线程保存的稳定进程快照，
stop/continue 使用仍在增长的 PCB CPU 账户；两者都加入 child 已回收后代的累计。`WNOWAIT`
只观察同一快照，不移除 child、不累加 parent。所有用户指针都由 syscall 层在 wait 返回后访问，
不会把缺页、CoW 或 TLB shootdown 带进 parent/child/WaitQueue 锁。

## 冻结源码

- 基线 commit：`a2eb28b7b721b5d6450f8b77242d77fce6058851`。
- 最终生产 diff SHA-256：
  `0e891b7a1f9809a7d37bd3225a079b550851080a1d4311b3f5c5255ab5b3abb4`。
- 四项接受任务 before/after 指纹一致，`mutation_detected=false`。
- `git diff --check`：通过。

## Docker 冻结验证

接受任务：`smp-b76-wait-rusage-validation-r2`，全部 `CORE_NUM=8`，按同架构 build→focused
后再切换架构串行执行。

| Recipe | 结果 | 耗时 | 直接证据 |
|---|---:|---:|---|
| `rv64-kernel-build` | PASS | 129.879s | exit 0 |
| `rv64-wait-rusage-gate` | PASS | 170.424s | musl 12/12 + glibc 12/12 |
| `la64-kernel-build` | PASS | 133.678s | exit 0 |
| `la64-wait-rusage-gate` | PASS | 176.344s | musl 12/12 + glibc 12/12 |

focused 集合为 `wait401`、`wait403`、`waitid01..09`、`waitid11`。两架构均打印
`online_mask=0xff`，每套 libc 12 PASS、0 FAIL/SKIP；日志没有 kernel panic、fatal trap、
timeout 或 TBROK。上游用例范围来自 LTP 官方目录：

- <https://github.com/linux-test-project/ltp/tree/master/testcases/kernel/syscalls/wait4>
- <https://github.com/linux-test-project/ltp/tree/master/testcases/kernel/syscalls/waitid>

首个验证 job 曾因误给已有手写 `Debug` 的 `Rusage` 增加 derive 而编译失败；该实验修改在 r2
前完全撤销。r2 的四个 child job 均冻结于上面的最终生产 diff。

## DeepSeek 结论校准

DeepSeek 的锁外 copyout、唯一 zombie 快照和 WNOWAIT 判断与源码一致，但三项措辞需纠正：

1. 每架构是 musl 12/12 加 glibc 12/12，共 24 个 case-level PASS，不是总计 12。
2. PCB user/system 原子计数使用 Relaxed；Acquire load 只保证单个原子读取安全，不构成它所称
   的跨字段 Release 配对或瞬时一致快照。
3. EFAULT 后事件已消费；再次等待该唯一 child 应得到 `ECHILD`，不能再次返回同一 child。

验收以原始日志与源码内存序为准，不以模型摘要替代。

## 证据边界

- `wait4` 返回非零 user/system rusage 内容：`not-run`；`wait401` 不检查字段值。
- raw `waitid` 第五参数：`not-run`；libc 四参数 wrapper 不传该指针。
- rusage copyout EFAULT 后再次 wait 得到 `ECHILD`：`not-run`；当前由 Linux 顺序和源码证明。
- 用户返回快照与 parent `RUSAGE_CHILDREN` 精确相等：`not-run`；两者复用同一局部值的静态证明
  已完成，但没有专用用户探针。
- 初赛 `mask=0x003`：`not-run`；本批只改 wait 事件携带值，双架构逐事件 focused 已直接覆盖
  修改路径，没有为机械门禁重复整套初赛。
