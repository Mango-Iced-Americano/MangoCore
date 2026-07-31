# SMP B49 空闲核 work stealing 证据

## 1. 结论

状态：`pass`

B49 让 CPU0/AP 在本地 fetch 失败后尝试取得一个远端 affinity-compatible 任务。交接
复用现有 `Migrating` 状态，不增加新状态机、不同时持有两把 runqueue 锁，也不在队列锁内
等待 kernel-TLB 同步。普通用户任务仍默认固定 CPU0。

## 2. 实现不变量

```text
victim 锁内克隆候选
  -> 解锁并同步 thief 的 kernel mapping
  -> victim 锁内复核成员、Queued(owner)、affinity、migration target
  -> Queued(victim) -> Migrating，摘队并解锁
  -> Migrating -> Running(thief)
```

- 同一 victim 的本地 fetch、queued affinity 和多个 thief 由同一 runqueue 锁串行化。
- TLB 等待期间任务仍由 victim 队列拥有；二次复核失败直接放弃，不会丢失任务。
- 高负载 victim 全是 pinned 任务时，本轮排除该 victim，继续寻找其它队列。
- pending migration target 的任务在候选和提交两个位置都被拒绝，不能抢跑显式迁移。
- 每轮至多取得一个任务；Running 不计入 `nr_running`，只从 victim 排队数减一。

## 3. 冻结源码与协作裁决

- 分支：`smp`
- 被测 HEAD：`f146a14be93af979ef4badd2eb3883c13a906059`
- 最终冻结 tracked diff SHA-256：
  `6e9895ec1f28e873f67b2d2425e1ca550930db52f321b12dc2e8ef5c01a9f390`
- DeepSeek 最终任务：`smp-b49-work-steal-final`，结论 `ACCEPT_WITH_BOUNDARIES`。
- 首轮任务在运行中发生源码安全修正，被包装器以
  `validation worker changed visible Git state` 标为 FAILED；其父任务判决不计入通过证据。
- Codex 不采纳“下一步直接开放默认全核 affinity”的建议；Phase 5 共享子系统门禁仍有效。

## 4. 最终双架构 focused

| 架构 | child job | CORE_NUM | 结果 | 用时 |
|------|-----------|----------|------|------|
| RV64 | `agent-b7f9d644eeb0-r01-rv64-ktest` | 8 | 31/31 PASS | 138.969 s |
| LA64 | `agent-b7f9d644eeb0-r02-la64-ktest` | 8 | 31/31 PASS | 134.095 s |

两个 child 均包含 normal kernel build，进程退出码为 0，`online_mask=0xff`；新增
`idle_cpu_steals_one_task` 均通过，且无 panic、timeout、fatal/forbidden marker。
运行前后 HEAD、status、tracked diff 与 untracked-content 指纹一致，
`mutation_detected=false`。

## 5. 初赛非回归

生产 steal 逻辑冻结后，首轮四个独立 child 得到：

| 架构 | CORE_NUM | 得分 | 精确失败集合 |
|------|----------|------|--------------|
| RV64 | 8 | 312/314 | musl/glibc `busybox kill 10` 各 0/1 |
| LA64 | 8 | 308/314 | musl/glibc `test_brk` 各 1/3；`busybox kill 10` 各 0/1 |

随后只把 focused subject 从“直接用宽 mask 发布”改成“bit0 稳定排队后扩 mask”，并删除
相应多余测试 helper；`run_queue::steal()`、CPU0/AP 调度入口和普通用户路径均未变化。
因此该初赛结果作为生产路径非回归证据，最终确定性测试构造由上节双架构 focused 覆盖。

## 6. 已知边界

- focused 动态覆盖一个 thief/一个 victim；多 thief 竞争由源码锁协议支持，但未做压力门禁。
- 二次复核失败时本轮返回，可能产生短暂空转；不影响任务唯一所有权。
- `nr_running` 是放置提示而非精确公平保证。
- 默认用户 affinity 仍是 bit0；本证据不宣称 FS/net/driver 已支持任意 CPU 并发。
