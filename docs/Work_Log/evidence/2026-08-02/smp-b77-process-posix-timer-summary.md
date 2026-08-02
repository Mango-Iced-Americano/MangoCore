# B77 进程级 POSIX timer 证据摘要

## 结论

B77 把 POSIX timer 的唯一 owner 从创建线程 TCB 迁入 PCB，并闭合 SMP 下 sibling 共享、
timerid 发布窗口、delete/recreate slot ABA、exec/exit 清理和进程级信号投递的静态协议。
冻结生产代码在 RV64、LA64 上均完成 `CORE_NUM=8` 构建与双 libc focused 回归。

## 规范与源码对照

- POSIX/Linux `timer_create()`：timer 是进程级对象，timer ID 在进程内唯一，`SIGEV_SIGNAL`
  面向进程；显式 `sigev_value` 进入 `SI_TIMER`。
- Linux `fork(2)`：子进程不继承 POSIX timer；Linux `execve(2)`：成功 exec 删除 POSIX timer。
- Linux 6.6 `kernel/time/posix-timers.c` 与 `signal_struct::posix_timers`：对象由线程组共享
  signal state 持有，而不是由创建线程的 task-private pending 持有。

参考：

- <https://man7.org/linux/man-pages/man2/timer_create.2.html>
- <https://man7.org/linux/man-pages/man2/fork.2.html>
- <https://man7.org/linux/man-pages/man2/execve.2.html>
- <https://github.com/torvalds/linux/blob/v6.6/kernel/time/posix-timers.c>

## 并发不变量

1. PCB 的 `PosixTimerTable` 是唯一 owner；thread clone 共享，fork 空表，exec 与最后线程退出清空。
2. `timer_create()` 使用 `Vacant -> Reserved -> Active`，用户 copyout 不跨表锁且不暴露半对象。
3. 每次 arm 从全表取得唯一 `arm_seq`；action 同时匹配 PCB、timer ID、arm sequence、deadline。
4. callback 在表锁内提交 timer 状态并生成 shared pending，锁外完成 sibling 唤醒和周期重装。
5. `KernelTimerQueue::compact()` 允许 `queue -> timer table` 只读边；所有注册路径禁止反向嵌套。
6. SIGEV_NONE 不生成信号；SIGEV_SIGNAL 的 `sigev_value` 或默认 timer ID 进入 `SI_TIMER` siginfo。

## Docker / DeepSeek 验证

冻结基线：`HEAD=ae9c5165318777add052db800b1bbb167b848704`，生产代码 diff SHA-256：
`d9fc03efb3d6a3c09b96816e1aeb0e54655c2be488d0e4f93afeeffeb3531c64`。全部 accepted
子任务均为 `mutation_detected=false`，运行前后源码指纹一致。

| 项目 | 结果 | 耗时 | 摘要 |
|------|------|------|------|
| RV64 kernel build | PASS, exit 0 | 129.763s | Docker，`CORE_NUM=8` |
| LA64 kernel build | PASS, exit 0 | 130.269s | Docker，串行位于 RV64 之后 |
| RV64 focused | PASS, exit 0 | 72.550s | `online_mask=0xff`；musl 2/2、glibc 2/2 |
| LA64 focused | PASS, exit 0 | 86.143s | `online_mask=0xff`；musl 2/2、glibc 2/2 |

focused 配置为 `mask=0x800`、`ltp_include=timer_settime01,timer_settime02`、两套 libc。
每套 libc 中 `timer_settime01` 为 32 个断言全过，`timer_settime02` 为 48 个断言全过；
两架构均无 panic、fatal trap、timeout、LTP FAIL 或 runner mutation。

首轮 RV64 focused 在进入 QEMU 前以 exit 126 失败，stderr 为脚本 `Permission denied`。
根因是本地 ignored recipe 直接执行没有 `+x` 的新脚本；改为 `bash script` 后仅补跑缺失的
RV64 gate 并通过，没有把测试框架故障归为内核失败，也没有重复已通过的三项任务。

原始日志 SHA-256：

- RV64 build：`662d6932067be75001f1c291e4538bd800a876819b30a3924b47d897e2ae96d7`
- LA64 build：`a4a563e94ac8c8d2ddd01445264983a86a17afb4fa9ca49b977b02bc2758bdd8`
- RV64 focused：`878f66ae362700e498812b829173d5d228037930ba2b5c13cc79fba60dc52774`
- LA64 focused：`fd38ec52fed1734d7190898a62f33186e7c0b94980af723ac1d482981cec34c1`

## 明确未覆盖

以下内容均为 `NOT RUN`，不得由普通 8 核启动或 `timer_settime01/02` 外推：

- sibling 创建/设置/查询/删除同一 timer 的精确交错；
- 创建线程退出而进程存活时 timer 继续工作；
- fork 空表、exec 删除与 callback 并发；
- delete/recreate 同 ID 对旧 callback 的动态 ABA 注入；
- 两个相同 signal timer 的 per-timer pending/overrun 身份；
- `CLOCK_PROCESS_CPUTIME_ID`/`CLOCK_THREAD_CPUTIME_ID` 按 CPU 消耗而非 wall time 到期；
- realtime absolute timer 与 `clock_settime()` 并发重定位。

本批只触及 Task/timer/signal 生命周期，不重复执行初赛 `mask=0x003` 全组；现有 focused
覆盖直接 ABI 面，剩余高风险语义已拆成后续独立节点。
