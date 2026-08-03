# B96 TCB zombie 唯一 owner 证据摘要

## 范围与结论

本节点把线程终态收敛为唯一生产路径：

```text
Running(owner_cpu) -> Zombie -> owner_cpu.local_zombies -> idle stack drop
```

`TaskControlBlock::mark_zombie()` 现在只接受本 CPU 的 `Running(cpu_id())`；阻塞 sibling
必须先被唤醒，queued sibling 必须先被 fetch，未发布的 New task 直接 drop。
`New/Queued/Blocking/Blocked/Migrating` 和错误 CPU 的 Running 直接 fail-stop。

同时删除 interruptible registry 中从不可达 zombie 路径派生出的按项摘取、按 PID 摘取、
计数和 CPU0 每 64 tick 扫描。按 PID 回收只扫描 Per-CPU `local_zombies`；原
`stale_zombie` profile 阶段改名为 `taskq_stats`，保留 runqueue/nice 诊断但不再获取全局
`TaskManager` 锁。

## 调用链与生命周期证明

- TCB `mark_zombie()` 只有 `exit_thread_resources()` 和 ktest trampoline 两个调用点，
  两者都由 `current_task()` 取得自身并在 Running 状态执行。
- group-exit、fatal signal 和 exec sibling stop 都先经 wake/RESCHEDULE 让目标重新取得
  Running owner，再由目标自己在安全点退出；OOM 也只投递 SIGKILL。
- clone 发布失败保持 New 并直接释放 Arc，不调用 `mark_zombie()`。
- auto-reap 的 `finish_exit()` 发生在最后 current 切回 idle 之前，因此按 PID 清理只能提前
  摘取已入 local queue 的 sibling；最后 current 仍由随后 `finish_switch_out()` 入队并回收。
- Zombie 发布早于 context switch，真正的最后调度 Arc 只在 idle stack 上进入本地队列，
  不会在仍使用自身 kernel stack 时析构。

## DeepSeek 协作与 GPT 裁决

DeepSeek max 只读调用图审查未发现阻塞项，确认 New/Blocked 入口和 interruptible zombie
扫描均不可达。GPT 未照搬其建议示例中的 `Running(_)` CAS：若 expected 取实际旧状态，
错误 owner 仍可能 CAS 成功；最终实现显式匹配 `Running(cpu_id())`，把 CPU 所有权也纳入门禁。

验证经受限 Docker gateway 严格串行运行：

| 架构 | recipe | 结果 | 耗时 | 关键证据 |
|---|---|---:|---:|---|
| RV64 | `rv64-ktest`, `CORE_NUM=8 KTEST=smp KREPEAT=1` | 34/34 PASS | 141.221 s | owner/zombie/group-exit/exec/STOP PASS |
| LA64 | `la64-ktest`, `CORE_NUM=8 KTEST=smp KREPEAT=1` | 34/34 PASS | 141.395 s | owner/zombie/group-exit/exec/STOP PASS |

两项 runner 均 `process_exit_code=0`、`timed_out=false`、`mutation_detected=false`，
`online_mask=0xff`，无 panic、fatal trap 或 forbidden marker。

## 冻结信息与边界

- baseline HEAD: `8660a413bc87cc7d47e1ef2a79346a800c6a80ee`
- tracked diff SHA-256: `27319a36256b4604808db4d6fe37ec023943e573647955e8db9cb82ad1a0d15b`
- RV64 child: `agent-98dd82f30b4e-r01-rv64-ktest`
- LA64 child: `agent-98dd82f30b4e-r02-la64-ktest`

本节点没有修改 wait/futex/signal、FS、Net 或 Driver，也不解除普通用户任务的 CPU0
默认 affinity。`interruptible_zombie_max` 的 sysfs 兼容字段由共享子系统负责人后续统一处理。
