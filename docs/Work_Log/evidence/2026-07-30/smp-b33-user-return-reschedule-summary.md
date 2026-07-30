# SMP B33 用户返回 RESCHEDULE 安全点证据摘要

## 结论

状态：`pass`。

B33 让运行中的用户任务在双架构 `trap_return()` 安全点消费远端
`RESCHEDULE` IPI。hard-IRQ handler 仍只设置本 CPU 的原子提示；timer 与 IPI 的调度请求
由统一的 `run_task_safe_point()` 合并，并且最多执行一次上下文切换。

本节点没有实现动态 `sched_setaffinity()`、运行期 mask 写入、queued/blocked 迁移、
默认全核 affinity 或任意内核指令位置抢占。AP kernel-only 任务在函数体执行期间仍保持
IRQ-off；本次完成的是“完整用户 trap frame 返回边界”，不是通用内核抢占。

## 上游依据

Linux 在返回用户态的 `exit_to_user_mode_loop()` 中检查 `_TIF_NEED_RESCHED` 并调用
`schedule()`，随后才处理信号等工作；架构调度接口也要求在用户返回或 idle 等明确边界
响应 `need_resched`，而不是从任意 hard IRQ 被打断点直接切换。

参考：

- Linux [`kernel/entry/common.c`](https://github.com/torvalds/linux/blob/master/kernel/entry/common.c)
- Linux [Scheduler Architecture Hints](https://www.kernel.org/doc/html/latest/scheduler/sched-arch.html)

MangoCore 沿用同一原则：IPI handler 只发布意图，完整 trap frame 上的安全点负责调度。

## 生产调用链

```text
远端 CPU
  runqueue/迁移请求已发布
  → mailbox Release
  → IPI doorbell

目标 CPU hard IRQ
  → handle_ipi()
  → need_resched.store(true, Release)
  → 返回被打断现场

目标 CPU trap_return()
  → run_task_safe_point()
     → local_irq_save()
     → run_deferred_timer_work()
     → take_reschedule_request()       // Acquire + 清除提示
     → timer || IPI 时至多 schedule 一次
     → 恢复入口 IRQ 状态
  → do_signal()
  → 激活当前 CPU/MM 并返回用户态
```

`take_reschedule_request()` 同时供 AP idle 路径使用，避免出现两套消费语义。它只操作
本 CPU 原子字段，不取 runqueue/MM/task 锁，也不直接切换任务。新增的
`reschedule_count` 只在提示确实由安全点取走时递增，是 Phase 6 可继续复用的生产诊断计数，
不是测试专用开关。

## 并发与中断边界

- mailbox/reason 和 handler 使用 Release 发布，安全点用 Acquire `swap(false)` 消费；
  调度侧可以观察到门铃之前已经发布的 runnable/迁移状态。
- `run_task_safe_point()` 从 timer 判定、IPI 消费到决定切换全程 IRQ-off。本 CPU handler
  不能插入到“清提示—切换”之间；窗口后到达的 doorbell 保持硬件 pending，留给下一轮处理。
- timer 与 IPI 同时到达时只执行一次 `suspend_current_and_run_next()`，不会重复切换。
- 上下文切换后，任务恢复点继续重新读取当前 CPU 的 PerCpu/current；双架构
  `trap_return()` 随后重写 `tp/$r21` 锚点，不沿用旧 owner。
- 安全点位于 `do_signal()` 之前，与 Linux 当前 `exit_to_user_mode_loop()` 的
  need-resched-before-signal-work 顺序一致。

## Focused 用例的反假通过设计

原 `smp::user_task_migrates_on_yield` 升级为
`smp::user_task_reschedules_from_ipi`。同一用户 TCB 仍从 CPU0 起跑、目标仍为 CPU1，
但探针不再调用显式 `sched_yield`：

1. CPU0 首次 `getcpu` 必须看到 0，且迁移前 affinity 必须为 `0b11`；
2. CPU1 helper 等到 probe 已越过首次 CPU0 `getcpu` 后，调用生产
   `request_reschedule(0)`；
3. probe 反复调用 `getcpu`，只有 CPU0 trap-return 消费 IPI 并切出后才能交给 CPU1；
4. CPU0 的 `reschedule_count` 必须严格增加，排除 timer 或既有 yield 偶然促成迁移；
5. probe 在 CPU1 观察到 1，再次确认 affinity 为 `0b11` 并 exit(0)；
6. runner 核对 `last_cpu == 1`、进程 wait/reap、helper/user zombie 和两个 Weak 均释放。

采样计数基线前先运行一次安全点，清除前序用例可能合并留下的提示；此时 helper 尚未创建、
用户任务尚未发布，因此本轮增量只能来自新的远端请求。若恢复旧实现“handler 置位但
trap-return 不消费”，probe 会永久停在 CPU0 的 `getcpu` 循环并超时，无法假通过。

## DeepSeek 只读审查与人工裁决

- 首次 job `smp-b33-user-return-design-review-001` 无效：Codex 在冻结审查期间继续写入
  功能 diff，wrapper 因可见 Git 状态改变 fail-closed。该结果不作为审查证据，也不能归因
  为只读 worker 修改源码。
- 冻结重试 `smp-b33-user-return-design-review-002` 用时 209.293 秒，exit 0、未超时、
  `mutation_detected=false`，未发现 IRQ 窗口、Release/Acquire、双架构对称性或命名 blocker。
- 审查对应 diff SHA-256 为 `b6374dac...`。其后人工补上“先清旧提示再采样计数基线”，
  最终测试冻结 diff 为 `a40dec50...`；因此最终正确性以四项 Docker/QEMU 结果为准。

人工纠正了三处模型表述：

- `reschedule_count` 是 B33 新增的生产诊断字段，不是旧代码已有字段；
- 旧 pending 可能使计数基线产生假增量，必须在 helper/用户任务发布前先消费；
- Linux 当前返回用户态循环先处理 `_TIF_NEED_RESCHED`，模型写成“先信号后调度”不准确。

## 双架构 focused

| 架构 | child job | 耗时 | exit | online | TAP |
|---|---|---:|---:|---:|---:|
| RV64 | `agent-694366de7164-r01-rv64-ktest` | 134.595s | 0 | `0xff` | 21/21 |
| LA64 | `agent-694366de7164-r02-la64-ktest` | 136.604s | 0 | `0xff` | 21/21 |

两份日志均明确包含：

```text
[smp] minimal boot ready: configured=8 ... online_mask=0xff
ok 20 smp::user_task_reschedules_from_ipi
[KTEST RESULT: PASS]
```

## 初赛非回归

| 架构 | child job | 耗时 | exit | judge | 精确接受失败集合 |
|---|---|---:|---:|---:|---|
| RV64 | `agent-694366de7164-r03-rv64-preliminary` | 333.488s | 0 | 312/314 | 两套 `busybox kill 10` 各 0/1 |
| LA64 | `agent-694366de7164-r04-la64-preliminary` | 351.966s | 0 | 308/314 | 两套 `test_brk` 各 1/3；两套 `busybox kill 10` 各 0/1 |

两架构均为 `CORE_NUM=8`、`mask=0x003`、`online_mask=0xff`，四个
basic/busybox END 与 runner done 完整。四个 child 均 exit 0、未超时、无 forbidden
marker，source-before/source-after HEAD、status 和 tracked diff 指纹一致，
`mutation_detected=false`。

## 被测源码与本地协作边界

- branch：`smp`
- HEAD：`55b43bb79d7814c29d44135b19ae4efd71d471c1`
- 被测功能源码 diff SHA-256：
  `a40dec50f1ec3616528fb91737e757385ca3a92ebb866ed5cd924504e0f01266`
- DeepSeek 冻结验证 job：`smp-b33-user-return-validation-001`
- 总耗时：1050.247 秒

所有 prompt、模型输出和原始 stdout/stderr 只保存在本地忽略的 `cc-codex/`，不纳入
GitHub。测试完成后只新增本文档并同步已有文档，没有修改被测功能源码，也没有遗留
临时用户 ELF、调试开关、`.orig` 或 `.rej`。B33 当前保持未提交，等待人工审查批准。
