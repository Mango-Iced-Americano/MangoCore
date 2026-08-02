# B80 POSIX timer 精确 pending 与 overrun 证据

## 结论

状态：`partial`。

B80 完成生产路径上的对象级 pending：每个 POSIX timer 最多有一个排队事件，后续周期到期
累加到该事件的 overrun；不同 timer 即使选择同一个非实时 signal，也不会再被 signal-number
合并。delete/recreate 的 pending ABA 由全表单调 `instance_seq` 隔离，heap stale action 仍由
独立 `arm_seq` 隔离。双架构 8 核构建与基础 POSIX timer LTP 已通过；精确并发交错尚未运行，
因此不把阶段写成 `pass`。

## 设计证据

### 身份分层

| 身份 | 生命期 | 拒绝的问题 |
|---|---|---|
| `instance_seq` | create/publish 到 delete | 旧 pending 命中复用 ID 的新 timer |
| `arm_seq` | 一次 `timer_settime()` 装载 | 旧 heap callback 修改新设置 |
| `PosixTimerEventId` | 一个排队事件 | 同 timer overrun 合并与精确清理 |

`next_instance_seq` 和 `next_arm_seq` 都属于整张 PCB timer 表，exec 清表时不重置，避免旧异步
对象与新映像复用相同 slot 后形成 ABA。

### 锁序

```text
到期：PosixTimerTable 记录事件 -> unlock -> process.signal 入队 -> scheduler wake
交付：process.signal dequeue -> unlock -> PosixTimerTable 固化 overrun
清理：PosixTimerTable 收集事件身份 -> unlock -> process.signal 精确删除
```

任一路径都不同时持有 timer owner 与 signal lock。跨临界区携带的是值类型事件身份，不携带
guard 或内部引用。signal queue 拒绝入队时立即撤销 owner-side pending，避免 timer 永久只累计
overrun 却没有可交付队列项。

### Linux 6.6 对照

- `kernel/time/posix-timers.c`：每个 `k_itimer` 保存唯一预分配 sigqueue；同一 timer 已排队时
  累加 overrun，不重复排队。
- `kernel/signal.c`：signal dequeue 释放 siglock 后再进入 POSIX timer rearm/finalize。
- `include/linux/posix-timers.h`：timer 对象持有 overrun/requeue 与 sigqueue 状态。

对照版本：Linux v6.6。

## 冻结源码

- 基线 HEAD：`833247de7749f864c626eed46aa924997a71acc7`
- tracked diff SHA-256：
  `34313121a86219eea2bfca2a3a465a78d7551a6487b14093a62f23e623458172`
- source status SHA-256：
  `8c2ce79dd17c842b036eea99f0c7ade18183871938e363de7214925072b5fbde`
- 四项 accepted job 均记录 `mutation_detected=false`，before/after 指纹一致。

## Docker 验证

| Job | 配置 | 时间 | 结果 |
|---|---|---:|---|
| `agent-46376733b12b-r01-rv64-kernel-build` | RV64, `CORE_NUM=8` | 129.165s | PASS |
| `agent-46376733b12b-r02-la64-kernel-build` | LA64, `CORE_NUM=8` | 136.298s | PASS |
| `agent-46376733b12b-r03-rv64-posix-timer-gate` | RV64, `CORE_NUM=8` | 78.480s | PASS |
| `agent-46376733b12b-r04-la64-posix-timer-gate` | LA64, `CORE_NUM=8` | 80.501s | PASS |

focused gate 覆盖两套 libc 的 `timer_settime01` 32 项和 `timer_settime02` 48 项，总计 80/80。
所有命令 exit 0，无 panic/fatal/timeout/forbidden marker。

首轮 `smp-b80-build-review-r1` 在 RV64 build 期间检测到源码继续变化，按协议失败；它不属于
accepted evidence。修改冻结后重新执行上述四项，未用失败轮替代最终结果。

## 未覆盖边界

- 两个 timer 使用同一个 signal 时的独立队列项：`NOT RUN`。
- 周期到期、signal dequeue、`timer_getoverrun()` 的精确数值回环：`NOT RUN`。
- delete/recreate 与旧 pending 同时交错的动态 ABA 探针：`NOT RUN`。
- signalfd 的 `ssi_tid/ssi_overrun` 二进制布局专项：`NOT RUN`。

这些边界已有对象序号与锁序静态证明，但不能由 `timer_settime01/02` 的普通功能结果外推。
