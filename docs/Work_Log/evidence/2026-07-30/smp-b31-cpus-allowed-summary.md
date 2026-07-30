# SMP B31 `cpus_allowed` 调度约束证据摘要

## 结论

状态：`pass`。

B31 为每个 TCB 建立内核权威的 `cpus_allowed` 位图，并把它接入三条会创建
`Queued(cpu)` 的路径：首次发布、yield 切出后换 owner 和 Blocked 唤醒。任务在取得
某个 CPU 的 runqueue/current 所有权前，目标 CPU 必须属于该位图。

该节点只实现“初始 mask 不变”的内核模型：普通任务默认仅 CPU0，定向 ktest 任务
仅允许指定 CPU，受控用户探针显式允许 CPU0/CPU1。本节点没有实现运行期
`sched_setaffinity()`、queued task 搬队、强制迁移、负载均衡或普通用户任务全核解封。

## 被测源码

- worktree：`/home/lzm/projects/MangoCore-smp-integration-20260725`
- branch：`smp`
- HEAD：`5c885a3979af51aa4cdeb8f0e1df402e41099ee1`
- 四次 Docker/QEMU 门禁使用的 tracked diff SHA-256：
  `7ba987d7ae126e4047072d2bca70911f23c4dc286bbcf5a77b1fd8e5a8a17755`
- 测试后只收紧了生产注释对“独占”的描述，最终功能源码 diff SHA-256：
  `c44cfd403d9e27e7cf6765d34755345f072574364febc8143501cbbabcf5d71f`

四个 child run 的 source-before/source-after HEAD、tracked diff 和 untracked content 指纹完全一致，
包装器均报告 `mutation_detected=false`。测试后的改动只是中文注释，不改变类型、
内存序、状态转移或生成的运行逻辑，因此没有机械重跑 QEMU。

## 数据模型与生命周期

1. `TaskControlBlock::new()` 把普通任务初始化为 CPU0-only，保持当前共享子系统尚未
   完成 AP 审计时的安全边界。
2. `new_ktest_independent()` 保留 CPU0 安全默认，唯一对外创建路径
   `spawn_ktest_task_on()` 在注册和发布前将 mask 收紧为指定 CPU。构造器可见性收紧为
   `pub(crate)`，避免新调用者绕过这条初始化路径。
3. `sys_clone()` 在子 TCB 发布前复制父线程 mask；`exec` 复用原 TCB，因而自然保留
   mask。这与 Linux 的 per-thread affinity 继承语义一致。
4. `set_initial_cpus_allowed()` 拒绝空集、未配置 CPU 位以及非 `New` 任务。它要求
   调用者独占 mask 写入权与首次发布权；`New` 检查只是误用防御，不是一把锁。

TCB 可以在仍为 `New` 时已登记弱引用，因此“创建者独占整个 TCB”不是准确证明。
真正的约束是当前只有创建路径可写 mask，且它不与首次发布并发；原子字段
避免数据竞争，随后的 runqueue 锁和 `New -> Queued` AcqRel 交接完成调度路径可见性。

## CPU placement 硬约束

| 所有权变化 | 唯一入口 | 约束 |
|---|---|---|
| `New -> Queued(cpu)` | `run_queue::publish()` | 取目标 runqueue 锁前验证 `cpu` 在 mask 内 |
| `Running(source) -> Queued(target)` | `run_queue::requeue_after_switch()` | 源 current 已在 idle 栈清空后，交接目标 owner 前再次验证 mask |
| `Blocked -> Queued(target)` | `run_queue::enqueue_woken()` | 唤醒目标必须来自 allowed/online/scheduler/non-stopped 交集 |

`select_wake_cpu()` 先在该交集中优先选择 `last_cpu`；如果局部性提示已失效，选择
交集中最低编号 CPU，不再无条件回退 CPU0。选择在取目标 runqueue 锁前完成，
保持 `TASK_MANAGER -> 单个 RunQueue` 的既有锁序，未新增锁或双 runqueue 持有。

## 只读审查与人工裁决

- job：`smp-b31-affinity-review-001`
- 耗时：197.369 秒
- exit：0
- mutation：false
- 结论：未发现 blocker；三个 TCB 构造路径、三条 placement 路径和既有锁序闭合。
- 采纳：把 `new_ktest_independent()` 收紧为 `pub(crate)`。该改动只减少接口可见性，
  随后与最终功能 diff 一起通过四项门禁。

GPT/Codex 没有接受汇总中“五层验证没有任何盲区”的绝对表述：现有正向测试
能覆盖三条生产路径，但不能替代对所有违规目标的负向穷举；核心结论同时依赖
fail-stop 源码检查、构造路径审计和动态结果。

## 双架构 focused

| 架构 | child job | 耗时 | exit | online | TAP | 关键路径 |
|---|---|---:|---:|---:|---:|---|
| RV64 | `agent-a159bf305164-r01-rv64-ktest` | 134.085s | 0 | `0xff` | 21/21 | 11/12/20 PASS |
| LA64 | `agent-a159bf305164-r02-la64-ktest` | 138.702s | 0 | `0xff` | 21/21 | 11/12/20 PASS |

两份 TAP 均明确包含：

```text
ok 11 smp::remote_kernel_tasks_run_on_target_cpus
ok 12 smp::blocked_kernel_tasks_wake_on_last_cpu
ok 20 smp::user_task_migrates_on_yield
[KTEST RESULT: PASS]
```

这三项分别走过定向首次发布、AP 任务阻塞后唤醒和用户任务 yield 迁移路径。

## 初赛非回归

| 架构 | child job | 耗时 | exit | judge | 精确接受失败集合 |
|---|---|---:|---:|---:|---|
| RV64 | `agent-a159bf305164-r03-rv64-preliminary` | 329.731s | 0 | 312/314 | musl/glibc `busybox kill 10` 各 0/1 |
| LA64 | `agent-a159bf305164-r04-la64-preliminary` | 339.917s | 0 | 308/314 | musl/glibc `test_brk` 各 1/3；`busybox kill 10` 各 0/1 |

两架构均为 `configured=8`、`online_mask=0xff`、`mask=0x003`，四个 basic/busybox END
完整。RV64 失 2 分、LA64 失 6 分，失败身份与已有接受基线完全一致；没有使用
模型汇总替代原始 judge JSON。

## 清理与证据边界

- 没有新增 `TaskStatus`、普通锁、`unsafe`、IPI reason、调试计数器或独立用户 ELF。
- 没有遗留 68 字节用户工具桩、临时源码或新增的 `.orig/.rej`。仓库中两个
  vendor `Cargo.toml.orig` 均是已跟踪的既有文件，与本次测试无关。
- 原始 prompt、DeepSeek 输出、stdout/stderr 和 runner manifest 仅保存在本地忽略的
  `cc-codex/runtime/jobs/`，不纳入 GitHub。
- 初赛只证明 CPU0-only 普通用户路径没有退化；focused 证明当前受控的指定 CPU
  路径符合 mask。两者都不能外推为运行期 affinity 或共享子系统已完成全核审计。
