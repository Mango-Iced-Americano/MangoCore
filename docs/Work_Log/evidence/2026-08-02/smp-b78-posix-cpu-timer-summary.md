# B78 POSIX CPU-time timer 证据摘要

## 结论

B78 将 `CLOCK_PROCESS_CPUTIME_ID` 和 `CLOCK_THREAD_CPUTIME_ID` 的 POSIX timer 从
wall-time heap 分离，改为由真实 CPU 累计在安全点驱动。process timer 读取 PCB
线程组总量，thread timer 用创建者 `Weak<TCB>` 固定对象身份；表锁内只唯一领取
到期事件，信号队列和调度器唤醒全部在锁外执行。冻结生产代码已通过双架构
`CORE_NUM=8` 构建和双 libc POSIX timer focused 回归。

## 规范与源码对照

- POSIX/Linux `timer_create()`：process CPU clock 统计进程全部线程的 user+system
  时间，thread CPU clock 只统计调用线程；`SIGEV_SIGNAL` 是进程定向的通知。
- Linux 6.6 `kernel/time/posix-cpu-timers.c` 把 CPU timer 绑定到 task/process CPU clock，
  而非 monotonic/realtime deadline。
- 本内核采用安全点抢占，所以在 trap return 和 schedule-out 扫描 CPU timer；
  这是对当前抢占模型的明确适配，不宣称任意内核指令点都可立即投递。

参考：

- <https://man7.org/linux/man-pages/man2/timer_create.2.html>
- <https://man7.org/linux/man-pages/man3/clock_gettime.3.html>
- <https://github.com/torvalds/linux/blob/v6.6/kernel/time/posix-cpu-timers.c>
- <https://pubs.opengroup.org/onlinepubs/9699919799/functions/timer_create.html>

## 实现边界与不变量

1. wall timer 保留 `wall_deadline`，CPU timer 使用 `cpu_deadline_us`；两个时钟域不共用
   heap deadline，sleep 或阻塞时间不能推进 CPU timer。
2. process clock 由 PCB 持有；thread clock 保存 `Weak<TCB>` 而不是可复用 TID，防止
   线程退出后的 ID ABA。
3. 先在 timer 表锁外采样 CPU 时间，再在表锁内比较 deadline。多个 CPU 扫描
   同一 PCB 时，锁内的 clear/advance 保证一次到期只被一个 CPU 领取。
4. 并发记账使锁外样本偏旧时，最多延迟到下一安全点；它不会提前触发，
   也不会绕过表锁制造重复领取。
5. 表锁内只把最多 32 个完整值事件放入固定栈批次；释放表锁后才进入
   可能扩容的 signal pending queue 和 runqueue。
6. `posix_cpu_timers_active` 只是 Release/Acquire fast hint；slot 和 deadline 始终是
   权威状态，arm/disarm/delete/clear/scan 均在同一 owner 锁下同步 hint。
7. 已到期但尚未在安全点领取的 CPU timer，`timer_gettime()` 返回 1ns，
   不伪装成已 disarm。

## Docker / DeepSeek 验证

冻结基线：`HEAD=0b830c2afaae004e429e9a8d54d6609e95ece916`，生产代码 diff
SHA-256 为 `61a0b7e8d6d0d8204457effc135d1365de1ae641d14523fd4160384bf462ee45`。
四项 accepted child job 的 source-before/source-after 相同，没有代理修改源码。

| 项目 | 结果 | 耗时 | 摘要 |
|------|------|------|------|
| RV64 kernel build | PASS, exit 0 | 133.408s | Docker，`CORE_NUM=8` |
| LA64 kernel build | PASS, exit 0 | 137.190s | Docker，严格位于 RV64 之后 |
| RV64 POSIX timer focused | PASS, exit 0 | 76.162s | `online_mask=0xff`；musl/glibc 各 2/2 |
| LA64 POSIX timer focused | PASS, exit 0 | 84.158s | `online_mask=0xff`；musl/glibc 各 2/2 |

focused 配置为 `mask=0x800`、`timer_settime01,timer_settime02`。每套 libc 中，
`timer_settime01` 32/32，其中 process/thread CPU clock 的 relative、old-value、
periodic 和 absolute 全部 TPASS；`timer_settime02` 48/48。两架构均无
panic、fatal trap、timeout 或缺少结束 marker。

原始 stdout SHA-256：

- RV64 build：`662d6932067be75001f1c291e4538bd800a876819b30a3924b47d897e2ae96d7`
- LA64 build：`a4a563e94ac8c8d2ddd01445264983a86a17afb4fa9ca49b977b02bc2758bdd8`
- RV64 focused：`0b2b7eb8b9333afd6591a931be37cfb2a2fd29ab18c08a7c1541bfe38e7c8b75`
- LA64 focused：`f38850459a33a98f123d4323e0296f2f55cc33658d14bcc242d70ed61c3e39e3`

首轮 B78 构建在 RV64 编译阶段暴露 Rust 2018 数组 `.into_iter()` 产生引用的
`E0308`，尚未进入 QEMU。实现改为显式 `.iter().copied()` 后重新冻结指纹，
再由 r2 从头串行执行四项验收。该失败没有被隐藏或记为内核动态失败。

DeepSeek 报告对“旧样本”的一句描述误写成会更早触发；人工按单调计数和
比较方向复核后未采纳：旧样本只会延迟本轮领取，不会提前越过 deadline。

## 明确未覆盖

以下场景均为 `NOT RUN`，不得从普通 LTP busy-loop 或 8 核启动外推：

- 不同 sibling 安全点并发扫描同一 process timer 的精确唯一领取；
- thread timer 创建者退出后的 get/set 与旧对象身份；
- 显式 sleep 期间 CPU clock 不推进；
- periodic timer 的 per-timer pending/overrun 精确身份；
- block/yield 恰好发生在最后一段 CPU 计时尾数的 schedule-hook 交错。

本批只改 Task/timer/signal 生命周期与安全点，没有重复运行初赛
`mask=0x003` 全组；FS/Net/Driver 完整并发审查仍由对应负责人后续执行。
