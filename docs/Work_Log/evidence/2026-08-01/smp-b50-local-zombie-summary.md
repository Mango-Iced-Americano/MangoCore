# SMP B50 Per-CPU zombie 回收证据

## 1. 结论

状态：`pass`

B50 将退出 TCB 的最后调度 Arc 从全局 `TASK_MANAGER.zombie_queue`
迁到退出 CPU 的 `CpuTaskState.local_zombies`。任务已切回 idle 栈后才
入队，同一 CPU 在下一次 dispatch 之前析构，因此既不释放仍在使用的
内核栈，也不再让 AP 退出竞争全局 TaskManager 锁。

## 2. 所有权与锁协议

```text
Running(cpu) 在自身栈上标记 Zombie
  -> __switch 切回 CPU cpu 的 idle 栈
  -> 释放 processor 锁并清空 current
  -> local_zombies(cpu) 锁内 push Arc / nr_zombies++
  -> 下一轮 idle 在锁内摘取 / nr_zombies--
  -> 锁外 drop Arc
  -> TCB::drop -> KernelStack::drop 只登记映射退休
  -> CPU0 idle 后续执行 kernel-TLB shootdown 并归还 frame/slot
```

- enqueue/take/remove 都在同一 CPU-local queue 锁内同步容器与
  `nr_zombies`；快照偏小时只少取，剩余项下轮 drain，不会双取或丢失。
- 跨 CPU take/reap 依次处理每个队列，不同时持有两把 zombie 锁。
- `TASK_MANAGER` 只摘取 interruptible zombie；释放该锁后才扫描 Per-CPU 队列。
  两类容器是互斥的终态 owner，不存在 interruptible→local 的 zombie 搬运。
- 承接移出 Arc 的 `Vec` 在队列锁外分配/扩容；TCB/PCB/KernelStack 析构也在
  所有容器锁外。`VecDeque::push_back` 可在队列锁内扩容，但当前 allocator
  不会反向取 zombie 锁；未引入一个没有安全溢出协议的任意固定容量。

## 3. 冻结源码与环境

- 分支：`smp`
- 被测 HEAD：`da3046006add83a92552d68bb3ee3a43221bd109`
- 冻结 tracked diff SHA-256：
  `9c7d88145430f9e6435f32b1e2ef428fa1b70aa6b10f23007c496dc6314d0a03`
- DeepSeek 任务：`smp-b50-local-zombie-validation`，包装器状态 `SUCCEEDED`，
  模型结论 `ACCEPT`。
- Docker 容器：
  `a99062375fdbde7b8989f6b9622438229a8609991a3aad86443a5eafcc4acfca`，
  image `zhouzhouyi/os-contest:20260510`，
  `/home/lzm/projects/MangoCore-smp-integration-20260725 -> /app`。
- QEMU：RV64/LA64 均为 10.0.2。
- 四个 child 运行前后 HEAD、status、tracked diff 和 untracked-content 指纹一致，
  `mutation_detected=false`。验证后只增加源码注释与文档，未改变可执行逻辑。

## 4. 双架构 focused

| 架构 | child job | CORE_NUM | 结果 | 用时 |
|------|-----------|----------|------|------|
| RV64 | `agent-4e3cc940e4c1-r01-rv64-ktest` | 8 | 32/32 PASS | 137.232 s |
| LA64 | `agent-4e3cc940e4c1-r02-la64-ktest` | 8 | 32/32 PASS | 136.202 s |

两个 child 均含 normal kernel build，exit 0，`online_mask=0xff`，无 panic、timeout、
fatal/forbidden marker。新增 `zombie_reclaims_on_owner_idle` 在 CPU0 runner 保持
current 的时候让 CPU1 任务退出；测试方 drop 自身 Arc 后，只有 CPU1 本地
idle drain 能使 Weak 消失。旧的“CPU0 代为回收”会在该窗口超时。

`kernel_stack_reclaim_waits_for_shootdown` 不再观察全局 zombie 队列长度，而是等待
CPU1 释放所有 TCB Weak，再要求退休队列产生实际回收，并继续检查全部
AP 的 kernel-TLB ack 增量与第二轮 slot 复用。

## 5. 初赛非回归

| 架构 | child job | CORE_NUM | 得分 | 精确失败集合 | 用时 |
|------|-----------|----------|------|--------------|------|
| RV64 | `agent-4e3cc940e4c1-r03-rv64-preliminary` | 8 | 312/314 | musl/glibc `busybox kill 10` 各 0/1 | 356.109 s |
| LA64 | `agent-4e3cc940e4c1-r04-la64-preliminary` | 8 | 308/314 | musl/glibc `test_brk` 各 1/3；`busybox kill 10` 各 0/1 | 359.093 s |

失败身份与 B49 完全一致，四个 group-end 和 `run_selected_groups done` 均出现；
这一结论来自 child 原始 judge JSON，而不是 DeepSeek 的二次概括。

## 6. DeepSeek 建议的人工裁决

- **接受：** 锁/计数协议、owner CPU idle 栈 Drop、wait/reap 互斥和 focused
  用例的总体 `ACCEPT` 结论与源码/实测一致。
- **纠正：** AP 不调用 `reclaim_retired_kernel_stacks()`；AP 在 Drop 时只登记
  退休，CPU0 idle 才做 shootdown 和最终归还。DeepSeek 还漏报了 LA64 两套
  `test_brk` partial failure，原始得分是 308/314，不是“只有 kill 10”。
- **不接受：** 未引入固定容量 zombie 数组。只规定 64 个槽位而不定义
  溢出后的可等待、可回收交接，会让内存压力下的退出语义失败。

## 7. 已知边界

- STOP/panic 在进入不可返回 idle 前可能留下最后一批本地 Arc；这是有界
  停机泄漏，不影响继续运行时的所有权或内存安全。
- 本节点不开放普通用户任务的默认全核 affinity；FS/net/driver 共享路径
  仍须完成 Phase 5 审计。
- `nr_zombies` 是快路径/诊断提示；释放某个 TCB 的权威依据始终是受锁
  CPU-local 容器中是否真实存在该 Arc。
