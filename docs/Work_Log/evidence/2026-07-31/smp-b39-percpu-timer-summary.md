# SMP B39 Per-CPU 调度 tick 证据摘要

## 结论

- 状态：`pass`
- 被测基线 HEAD：`e5e115d1`（完整 commit：
  `e5e115d1fc0c4cf338d86e91b01a3bd3f2db039e`）
- 被测生产源码 diff SHA-256（`git diff -- os/src`）：
  `3d3670bfc12e1702d0256dd9d12c23666a9a74ea40fc257901e36b10af2431e6`
- 最终冻结复检记录的完整 tracked diff SHA-256：
  `eec8bfde6f0b626296b7002bb83eb6b079b7f12e597e77d248c16d7e43bafbd6`
- 测试前后源码指纹一致；DeepSeek/runner 未修改 tracked source。
- 双架构 8 核 kernel build 通过，SMP focused 均为 25/25，初赛失败集合未扩大。

## 实现证据

- 每个 `PerCpu` 独占一个 100 Hz 绝对调度 deadline；删除了跨 CPU 竞争的全局
  `NEXT_SCHED_TICK_NS`。
- 所有 CPU 的 hard timer IRQ 都只发布 per-CPU deferred 标志。真正推进 quantum、
  设置 `need_resched` 和切换任务仍只发生在任务安全点，不从中断帧直接 context switch。
- CPU0 是全局 timer queue、timeout、timerfd 和网络后台 poll 的唯一执行者，并按
  “本地调度 tick、全局最早 deadline 两者的较小值”编程 one-shot timer。
- AP timer 只推进本地 quantum。AP 插入更早的全局 timer 时，先释放 timer queue 锁，再向
  CPU0 发送独立 `TIMER_REPROGRAM` reason；IPI handler 只发布原子请求。
- 两架构都先初始化 deadline、编程本地 one-shot timer，再开放 timer interrupt。
  LoongArch 修改 `ECFG` 时保留已经开放的 IPI line，timer 初值向上对齐到硬件要求的 4 倍数。
- LA64 stable-counter 频率由 CPU0 Release store 到 `AtomicUsize`，AP 以 Acquire load
  读取；本路径不再并发读取 `static mut CLOCK_FREQ`。
- AP 在进入本地调度循环前执行同一个 `timer_cpu_init()`；idle 安全点也消费 deferred timer，
  因此本 CPU 尚无 current 时不会遗留 timer 请求。
- AP 可原子累加调度/timer 计数，但格式化性能快照会读取 FS/net 全局状态并打印 console；
  `print_snapshot` 和 timer/scheduler 周期触发因此限制为 CPU0-only。

## 官方规范依据

- RISC-V SBI TIME extension 的 `set_timer(stime_value)` 接受绝对时间，并在写入未来时间后清除
  pending timer interrupt；MangoCore 保留绝对 deadline，不使用“当前时间 + 反复累计误差”的
  相对编程。
- LoongArch stable counter 是每核稳定计数源；timer 是每核设备并使用中断 line 11。
  `TCFG.InitVal` 低两位固定为零，`TICLR` 采用写 1 清除。因此代码先写合法 InitVal，再开放
  timer line，并保持 IPI line 不被覆盖。

## Docker 构建

首轮冻结验证的六个 child 都在编译期失败，原因是新增 HAL API 只在两个架构模块导出，
`hal::arch` 中间层漏做 re-export；该批没有启动 QEMU，不计作通过证据。补齐两条中间层
导出后执行冻结复检：

验证任务：`smp-b39-build-recheck`

| 顺序 | child | 配置 | 用时 | 结果 |
|---:|---|---|---:|---|
| 1 | `agent-762d790e5035-r01-la64-kernel-build` | LA64, `CORE_NUM=8` kernel build | 130.9 s | exit 0 |
| 2 | `agent-762d790e5035-r02-rv64-kernel-build` | RV64, `CORE_NUM=8` kernel build | 131.4 s | exit 0 |

两项严格串行，均无 forbidden marker 或源码 mutation。

## 双架构 8 核 focused QEMU

冻结验证任务：`smp-b39-focused-validation`

| 顺序 | child | 配置 | 用时 | 结果 |
|---:|---|---|---:|---|
| 1 | `agent-1c4eae38b7b7-r01-rv64-ktest` | RV64, `CORE_NUM=8 KTEST=smp KREPEAT=1` | 136.2 s | 25/25 |
| 2 | `agent-1c4eae38b7b7-r02-la64-ktest` | LA64, `CORE_NUM=8 KTEST=smp KREPEAT=1` | 135.8 s | 25/25 |

两架构 raw TAP 都包含：

- `online_mask=0xff`；
- 第 8 项 `smp::user_timer_preempts_on_secondary_cpu ... ok`；
- 第 25 项 terminal STOP；
- `25 passed, 0 failed, 25 total`。

第 8 项在 CPU1 先运行一段没有 syscall、yield 或内存访问的用户死循环，再把同 CPU helper
排在其后。远程 enqueue IPI 在进入用户态前已经消费；helper 只有在 CPU1 本地 timer 令用户
任务回到安全点并被重新排队后才能运行。helper 同时确认用户任务处于 `Queued(1)`，再发送
SIGKILL 完成可回收的测试收尾。因此该用例不是仅检查 timer 计数，也不会由测试清理路径制造
假通过。

## 初赛 basic + busybox

冻结验证任务：`smp-b39-preliminary-validation`

| 架构 | child | 用时 | 结果 | 接受失败集合 |
|---|---|---:|---:|---|
| RV64 | `agent-3e96f1c18a54-r01-rv64-preliminary` | 333.6 s | 312/314 | 两套 busybox `kill 10` |
| LA64 | `agent-3e96f1c18a54-r02-la64-preliminary` | 343.7 s | 308/314 | 两套 basic `test_brk` 各 1/3；两套 busybox `kill 10` |

两项 child 均 exit 0、四组 START/END 完整、无 panic、timeout 或源码 mutation。
fork/clone/exec、sleep、times 与 gettimeofday 没有新增失败，结果等于 B38 人工接受基线。

## DeepSeek 协作与人工裁决

- 设计审查任务运行时 tracked diff 仍在变化，包装器按协议 fail-closed；该任务 stdout 只作为
  反例线索，不作为冻结审查通过证据。
- DeepSeek 提醒发送方必须同时处理 timer 与 reprogram 两种 pending、AP 不得执行全局
  callback、AP idle 也要消费 deferred timer，三项经源码复核后均纳入实现。
- DeepSeek 建议复用 `RESCHEDULE` reason 传递重编程请求。人工拒绝：纯 timer queue deadline
  变化并不要求切换 current，复用会把时间设备控制请求伪装成调度请求。独立 reason 与独立
  原子 pending 使 handler 和安全点职责更清楚。
- 首轮编译 RED 被采纳并修复；只有中间 HAL re-export 发生变化。修复后重新冻结源码，构建、
  focused 和初赛三组任务均保持指纹不变。
- `CLOCK_FREQ` 原子化后的最小复检通过双架构 build 与 LA64 25/25；DeepSeek 随后建议提交。
  人工没有直接接受，而是继续审计 AP deferred 调用链，发现并收口了上述性能快照入口。

最终人工审计完成后又冻结精确源码，执行任务 `smp-b39-final-freeze`：

| 顺序 | child | 配置 | 用时 | 结果 |
|---:|---|---|---:|---|
| 1 | `agent-fc6aea30843f-r01-rv64-kernel-build` | RV64, `CORE_NUM=8` kernel build | 128.301 s | exit 0 |
| 2 | `agent-fc6aea30843f-r02-la64-kernel-build` | LA64, `CORE_NUM=8` kernel build | 132.396 s | exit 0 |
| 3 | `agent-fc6aea30843f-r03-la64-ktest` | LA64, `CORE_NUM=8 KTEST=smp KREPEAT=1` | 135.687 s | 25/25 |

三项的 source before/after 均为上述完整 tracked diff 指纹，`mutation_detected=false`。
LA64 日志再次包含 `online_mask=0xff`、第 8 项真实 AP timer 抢占 PASS、第 25 项 STOP PASS
和最终 KTEST PASS。该复检专门覆盖人工最后加入的 `CLOCK_FREQ` 原子发布与 CPU0-only
性能快照边界；源码未再变化。随后只补写本证据中的结果和文档日期，不改被测生产源码。

## 未覆盖边界

- 动态测试证明的是用户态本地 timer 抢占；它不代表任意内核指令位置已经可抢占。
- 长 syscall 中可以接收 timer IRQ，但 deferred 调度仍等到既有任务安全点，不会在 syscall
  中央直接 context switch。
- AP 不执行全局 callback；文件系统 reclaim、console input 等剩余 housekeeping owner 仍需
  在共享子系统阶段继续审计。
- 普通用户任务默认 affinity 仍为 bit0，本工作包不代表默认全核调度已开放。

## 环境

- 容器：`mangocore-smp-integration-20260725-os-dev-1`
- image ID：
  `sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- repo digest：
  `zhouzhouyi/os-contest@sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`
- image created：`2026-05-10T08:46:16`
- QEMU：RV64/LA64 均为 10.0.2

DeepSeek prompt、manifest、stdout/stderr 与原始 Docker/QEMU 日志位于忽略的本地
`cc-codex/`，不上传 GitHub。本摘要只归档可公开复核的配置、指纹、结果和人工裁决。
