# SMP B35 远程稳定 Blocked affinity 证据摘要

## 结论

状态：`pass`（仅稳定 Blocked）；完整远程 affinity 仍为 `partial`。

B35 允许 raw `sched_setaffinity(remote_tid, ...)` 修改一个已经完全离开 current/runqueue、
且仍登记在 interruptible registry 中的 Blocked 线程。修改不迁移 owner：Blocked 本来就没有
runnable owner，后续 wake 直接根据新 mask 选择目标 CPU。远程 Running、Blocking、Queued、
Zombie 仍返回 `EOPNOTSUPP`，普通任务默认 affinity 仍为 bit0。

## 为什么选择 Blocked 作为独立节点

Linux 的完整 affinity 写侧以 task/rq 锁稳定 placement，并为 queued migration 提供显式中间
状态；DragonOS 同样把允许集和 on-rq 状态置于 per-task 锁保护。MangoCore 当前还没有等价
协议，直接写远程 Running/Queued 的原子 mask 会留下不在允许集内的 current/runqueue owner。

稳定 Blocked 则没有 owner 需要搬运，而且已有 `TASK_MANAGER` 同时保护睡眠 registry 和唯一的
`Blocked -> Queued(cpu)` wake 入口。因此本节点可以交付真实能力而不新增 `TaskStatus`、锁、
IPI reason、迁移容器或双 runqueue 锁序。

参考：

- Linux [`kernel/sched/syscalls.c`](https://github.com/torvalds/linux/blob/master/kernel/sched/syscalls.c)
- Linux [`kernel/sched/core.c`](https://github.com/torvalds/linux/blob/master/kernel/sched/core.c)
- DragonOS [`sys_sched_setaffinity.rs`](https://github.com/DragonOS-Community/DragonOS/blob/master/kernel/src/smp/syscall/sys_sched_setaffinity.rs)
- DragonOS [`sched_info.rs`](https://github.com/DragonOS-Community/DragonOS/blob/master/kernel/src/process/sched_info.rs)

## 生产协议

```text
sys_sched_setaffinity(remote_tid, size, user_mask)
  -> 用户拷贝、configured/runnable mask、严格 TID、权限校验
  -> update_blocked_affinity(task, allowed)
       -> lock TASK_MANAGER
       -> 状态必须精确为 Blocked
       -> registry 必须仍含同一 TCB 指针
       -> set_blocked_affinity(allowed)
            -> 断言 configured/runnable 边界
            -> cpus_allowed.store(allowed, Release)
       -> unlock TASK_MANAGER
  -> SUCCESS

later wake
  -> lock TASK_MANAGER
  -> cpus_allowed.load(Acquire)
  -> 按新允许集选择 CPU
  -> Blocked -> Queued(target)
  -> unlock TASK_MANAGER / RunQueue
  -> 必要时锁外 RESCHEDULE
```

状态和 registry 成员关系必须在同一临界区内共同成立。exit/exec 会先在 `TASK_MANAGER` 锁内
摘除任务，再把短暂的 Blocked 状态改成 Zombie；只看状态会对正在退出的任务错误返回成功。

并发结果由同一锁线性化：

- affinity 写侧先取得锁：Release 发布新 mask，后续 wake 必须按新允许集选点；
- wake 先取得锁：任务已从 registry 移除并变为 Queued，写侧返回 `EOPNOTSUPP`；
- exit/exec 先摘除 registry：即使状态暂时仍为 Blocked，写侧也返回 `EOPNOTSUPP`。

## Focused 反假通过用例

当前第 13 项 `smp::blocked_affinity_redirects_wake` 走完整生产路径：

1. CPU0 把 kernel-only 任务首次发布到 CPU1；
2. 任务经真实 Completion/WaitQueue 阻塞；
3. runner 同时确认状态为 Blocked、CPU1 current 为空、CPU1 runqueue 为空；
4. CPU0 调用生产 `update_blocked_affinity()` 把 mask 改为 bit0，并回读权威字段；
5. `Completion::complete()` 走生产 wake；
6. runner 让出 CPU0，任务必须在 CPU0 恢复并退出；
7. CPU1 current/runqueue 必须继续为空。

若 wake 仍按旧 `last_cpu=1`，任务会在 CPU1 恢复并使步骤 6/7 失败；因此用例不是只验证原子
字段写入。旧第 12 项仍验证“mask 未改变时回 last_cpu”，B34 用户 probe 当前顺延为第 21 项，
第 22 项继续验证 terminal STOP。

focused 没有从用户态直接调用远程 TID syscall；用户拷贝/current 写侧由 B34 probe 覆盖，
B35 新增的严格 TID 分支与权限顺序通过源码审计验收。

## DeepSeek 本地协作与人工裁决

- 与实现并行的 `smp-b35-blocked-affinity-review` 因 GPT/Codex 正在改变 tracked diff，被包装器
  以“visible Git state changed” fail-closed；该报告没有作为最终证据。
- 冻结任务 `smp-b35-blocked-affinity-final-review` 用时 262.726 秒，exit 0、未超时、
  `mutation_detected=false`，未发现 P0。其 `mark_zombie` 前置条件建议属于既有维护项，未混入
  B35；关于 release 构建 assert 的表述不准确，Rust `assert!` 在本项目 release 中仍生效。
- 验证任务 `smp-b35-blocked-affinity-validation` 用时 1037.075 秒，严格串行四个 Docker child。
  DeepSeek 汇总误称 LA64 `test_brk` 未被 mask=0x003 触发；Codex 复核原始日志确认它属于 basic，
  musl/glibc 均实际执行且各为 1/3，因此最终按真实 308/314 失败集合记录。

## 双架构 focused

| 架构 | child job | 耗时 | exit | online | TAP |
|---|---|---:|---:|---:|---:|
| RV64 | `agent-6ed1d245a644-r01-rv64-ktest` | 140.775s | 0 | `0xff` | 22/22 |
| LA64 | `agent-6ed1d245a644-r02-la64-ktest` | 138.915s | 0 | `0xff` | 22/22 |

两份日志均明确包含第 12/13/21/22 项 PASS 与最终 `[KTEST RESULT: PASS]`。

## 初赛非回归

| 架构 | child job | 耗时 | exit | judge | 精确接受失败集合 |
|---|---|---:|---:|---:|---|
| RV64 | `agent-6ed1d245a644-r03-rv64-preliminary` | 346.276s | 0 | 312/314 | 两套 `busybox kill 10` 各 0/1 |
| LA64 | `agent-6ed1d245a644-r04-la64-preliminary` | 356.490s | 0 | 308/314 | 两套 `test_brk` 各 1/3；两套 `busybox kill 10` 各 0/1 |

四个 child 均未超时、无 forbidden marker、`mutation_detected=false`；source-before 与
source-after 的 HEAD、status 和 tracked diff 指纹完全一致。

## 被测源码与边界

- branch：`smp`
- HEAD：`3cad2b4ce7cadfc8b4b7ab9304f4018a7c575e0f`
- 功能 diff SHA-256：
  `dc704963345ab55677d0397acb99e3a4d936603ad8626e6f6137eb0f5b059c0b`
- 最终验证 job：`smp-b35-blocked-affinity-validation`

本地 `cc-codex/` 中的 prompt、模型输出和原始日志继续忽略，不上传 GitHub。验证后只同步文档，
没有修改被测功能源码，也没有遗留临时诊断字段、测试开关、用户 ELF、`.orig` 或 `.rej`。
B35 保持未提交，等待人工审核。
