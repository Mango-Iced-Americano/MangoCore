# SMP B34 当前线程运行期 affinity 证据摘要

## 结论

状态：`pass`（current-only）。

B34 让 raw `sched_setaffinity(0/current_tid)` 真实修改当前线程的 `cpus_allowed`。新 mask
仍包含当前 CPU 时只发布 mask；排除当前 CPU 时，syscall 先同步目标 kernel-stack 映射，
再发布 mask 与一次性迁移目标，并在同一安全点切回 idle。既有单目标 runqueue 协议完成
owner 交接后，syscall 才在目标 CPU 恢复并返回用户态。

本节点没有实现远程 TID、Queued/Blocked affinity、默认全核 affinity、CPU hotplug 或
work stealing。非 current TID 明确返回 `EOPNOTSUPP`，不能把本证据描述成完整 Linux
`sched_setaffinity` 已完成。

## 上游依据与阶段边界

Linux 写侧先清空内核 CPU mask，再复制用户提供的低位字节；`cpusetsize` 小于内核 mask
大小时，未提供的字节保持零，因此非零短 mask 合法。完整远程 affinity 则需要
`task_rq_lock()` 稳定 task/rq 归属，并以 `TASK_ON_RQ_MIGRATING` 表达队列间交接。DragonOS
同样把 `cpus_allowed` 与 placement 放在 `pi_lock` 下，并区分
`OnRq::{Queued,Migrating,None}`。

参考：

- Linux [`kernel/sched/syscalls.c`](https://github.com/torvalds/linux/blob/master/kernel/sched/syscalls.c)
- Linux [`kernel/sched/core.c`](https://github.com/torvalds/linux/blob/master/kernel/sched/core.c)
- DragonOS [`sys_sched_setaffinity.rs`](https://github.com/DragonOS-Community/DragonOS/blob/master/kernel/src/smp/syscall/sys_sched_setaffinity.rs)
- DragonOS [`sched_info.rs`](https://github.com/DragonOS-Community/DragonOS/blob/master/kernel/src/process/sched_info.rs)

MangoCore 当前没有等价的 task/rq 串行化层。B34 因而只处理调用中的 current TCB，不新增
`TaskStatus`，不借用 `Blocked` 充当迁移中间态，也不同时持有两个 runqueue。

## 生产调用链

```text
sys_sched_setaffinity(pid, cpusetsize, mask)
  → 校验 signed pid / size / pointer
  → 零填充内核 word，复制 min(cpusetsize, sizeof(usize))
  → requested & configured；空集合返回 EINVAL
  → pid=0 取 current；正数严格按 TID 查找
  → 权限检查；目标不是 current 时返回 EOPNOTSUPP
  → allowed & online & scheduler & !stopped；空集合返回 EINVAL
  → TaskControlBlock::set_current_affinity(allowed)
       → 断言 Running(source) 且就是本地 current
       → 断言没有 pending migration_target
       → source 仍允许：Release 发布 mask，返回 false
       → source 被排除：
            按 nr_running + current 选择最低负载目标
            synchronize_kernel_mapping(target)
            cpus_allowed.store(mask, Release)
            migration_target.compare_exchange(..., Release)
            返回 true
  → drop TCB Arc
  → 必须迁移时 suspend_current_and_run_next()
  → 源 idle Acquire 取 target
  → 只锁目标 runqueue：Running(source) -> Queued(target)
  → 锁外 RESCHEDULE target
  → 目标 fetch：Queued(target) -> Running(target)
  → syscall 返回 SUCCESS
```

`cpus_allowed()` 使用 Acquire 读取。目标选择的队列计数只是无锁近似值；调用点额外读取
current 槽，把“当前正在运行一个任务”计入负载。放置快照可以过时，但不会改变 owner
正确性：所有权仍由 `sched_state` 与唯一 current/runqueue 容器决定。

## Focused 用例的反假通过设计

第 20 项现名为 `smp::user_task_reschedules_and_sets_affinity`：

1. probe 在 CPU0 验证 getcpu=0、affinity=`0b11`；
2. CPU1 helper 向 CPU0 发送真实 RESCHEDULE，B33 安全点把同一 TCB 交给 CPU1；
3. probe 在 CPU1 验证 getcpu=1、affinity=`0b11`；
4. probe 调用 `sched_setaffinity(0, 8, bit0)`；
5. syscall 返回后立即要求 getcpu=0，排除“只改 mask、不迁移”；
6. 再要求 getaffinity 返回 8 且 mask=bit0，排除“迁移但未持久发布”；
7. exit(0) 后核对 `last_cpu=0`、wait/reap、helper/user Zombie 和两个 Weak 释放。

probe 复用既有双架构内联汇编，不新增用户 ELF、生产诊断字段或测试开关。远程 TID、短/长
mask 和错误优先级来自源码审计，当前正向 probe 没有动态覆盖这些边界。

## 首错溯源与人工裁决

第一次 RV64 真实运行在第 20 项 quiescence timeout。用户任务要从 CPU1 回到 CPU0，但
CPU0 正由 ktest runner 的等待循环占用；只开放本地中断只能让 IPI handler 设置
`need_resched`，不会违反安全点模型从任意内核位置强制 context switch。等待器随后接入既有
`run_task_safe_point()`，让 runner 合法让出 CPU。

第二次仍 timeout。一次临时失败快照显示：

```text
task=Zombie helper=Zombie process_zombie=true
current0=true current1=false rq0=0 rq1=0 zombies=0 resched0=2 helper_result=1
```

这证明 setaffinity 自迁移、IPI 消费、退出与 owner 清理均已完成；唯一保持为真的旧条件是
`zombie_queue_count_fast() == 0`。DeepSeek 将其推断为计数器不同步，但 GPT/Codex 继续读取
生产源码，确认 CPU0 `run_tasks()` 每轮会执行 `take_zombie_tasks(64)` 并及时 drain 队列。
因此 zombie 队列非空只是瞬态，不能作为稳定完成条件。最终删除该过时条件与临时快照，
保留 Zombie 状态、CPU1 current/rq 为空和 Weak 生命周期验证。

## DeepSeek 本地协作

- `smp-b34-self-affinity-design-review`：227.305 秒，exit 0，未超时，
  `mutation_detected=false`；确认 current-only 方案无需新增状态/锁。
- 两个中间验证 job 的 Docker child 已按首错停止，但 max-effort 模型返回长时间停滞；
  GPT/Codex 终止外层会话并直接读取完整 child manifest/TAP，不把模型文本当测试证据。
- `smp-b34-affinity-timeout-diagnostic-001`：227.136 秒，完成一次 RV64 诊断 run；模型对
  zombie 计数的结论被生产 drain 源码否决。
- 最终 `smp-b34-self-affinity-validation-004`：1262.013 秒，四个 child 全部 PASS；
  DeepSeek 只读、未修改源码、未提交、未 push。

## 双架构 focused

| 架构 | child job | 耗时 | exit | online | TAP |
|---|---|---:|---:|---:|---:|
| RV64 | `agent-789b92e8de39-r01-rv64-ktest` | 136.684s | 0 | `0xff` | 21/21 |
| LA64 | `agent-789b92e8de39-r02-la64-ktest` | 134.952s | 0 | `0xff` | 21/21 |

两份日志均明确包含：

```text
[smp] minimal boot ready: configured=8 ... online_mask=0xff
ok 20 smp::user_task_reschedules_and_sets_affinity
[KTEST RESULT: PASS]
```

## 初赛非回归

| 架构 | child job | 耗时 | exit | judge | 精确接受失败集合 |
|---|---|---:|---:|---:|---|
| RV64 | `agent-789b92e8de39-r03-rv64-preliminary` | 550.320s | 0 | 312/314 | 两套 `busybox kill 10` 各 0/1 |
| LA64 | `agent-789b92e8de39-r04-la64-preliminary` | 345.145s | 0 | 308/314 | 两套 `test_brk` 各 1/3；两套 `busybox kill 10` 各 0/1 |

四项 child 均未超时、无 forbidden marker、`mutation_detected=false`。最终冻结源码的
source-before/source-after HEAD、Git status 和 tracked diff 指纹完全一致。

## 被测源码与本地边界

- branch：`smp`
- HEAD：`2aa8504ddc595c70384304b6a8c1ddff3bd0d8ce`
- 被测功能源码 diff SHA-256：
  `7f3b601862e5e081792de120ad9fddb37e388324ba3a85fbc8c040f8c0c2b4b9`
- 最终验证 job：`smp-b34-self-affinity-validation-004`

所有 prompt、模型输出和原始 stdout/stderr 只保存在本地忽略的 `cc-codex/`，不上传 GitHub。
最终测试后只同步文档，没有修改已验证功能源码，也没有遗留临时诊断、测试字段、用户 ELF、
`.orig` 或 `.rej`。B34 保持未提交，等待人工批准。
