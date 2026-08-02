# B69 task reply 锁外用户访存证据摘要

## 1. 结论

状态：`pass`

`get_robust_list()`、`setitimer()` 和 `timer_settime()` 不再持有 `task.inner` 进入
faultable copyout。带新旧值的 timer syscall 在 owner 锁内同时完成旧值快照与新状态提交，
之后才注册外部 timer 并写回旧值；copyout `EFAULT` 不回滚已经提交的配置。

## 2. 协议与语义

```text
完整 copyin / 参数校验
  -> task.inner：快照旧值 + 一次提交新状态
  -> 解锁
  -> 注册 KernelTimer（如需要）
  -> copyout 旧值
```

- robust-list 查询持有目标 `Arc`，锁内只复制 `head/len`，锁外先写 24 字节长度再写 head；
- real itimer 的 remaining 在栈上按 `deadline - now` 计算，不修改 owner 内保存值；
- POSIX timer 的 generation、deadline、立即到期信号在同一任务锁内发布；
- old pointer 无效时返回 `EFAULT`，新配置仍生效，避免重锁回滚覆盖并发观察。

## 3. 官方对照

- Linux 6.6 `kernel/futex/syscalls.c`：`get_robust_list()` 先写结构体长度，再写 head；
- Linux 6.6 `kernel/time/itimer.c`：锁内取得旧配置并安装新配置，锁外 copyout；
- Linux 6.6 `kernel/time/posix-timers.c`：timer 锁内修改，随后向用户写旧 spec。

## 4. AI 协作与人工裁决

| Job | 结果 | 采纳边界 |
|---|---|---|
| `smp-b69-task-uaccess-final-review-r1` | PASS | 最终两文件只读审查，无源码 mutation |
| `smp-b69-task-uaccess-validation-r1` | 主动中止 | 双架构 build 有效；gate 误纳入已知排除 `timer_settime03`，不计 PASS |
| `smp-b69-task-uaccess-validation-r2` | REVIEWED/PASS | 只补跑两架构 frozen focused+初赛 gate |

DeepSeek 最终汇总中把 gate 描述成重新全量编译，并写成“copyout 在锁释放前完成”；两项均与
脚本/源码不符，未采纳。实际 gate 只重建用户工具盘并复用 r1 的冻结 kernel，copyout 明确
发生在释放 `task.inner` 之后。

## 5. 冻结源码

```text
base HEAD: 639ad0f930bd8f287b7106172c06211c63cf75c8
tracked diff SHA-256: fbb52d180110216096fb0820a73f2f346a029f96718ff055b190863ee7f8a074
f3bea9493e41d9cd4383fa523ca8fdf16791452857d098f3b0438748bcbf8285  lifecycle.rs
8cae5dc5dba8167d82fc4b5ff543522825dc51c2cd0cc9a7e9a07aacfbfde4c0  time.rs
```

四项有效 child 的 source-before/source-after 均一致，`mutation_detected=false`。

## 6. Docker 验证

| Child | Recipe | CORE_NUM | 结果 | 时长 |
|---|---|---:|---|---:|
| `agent-a56730cb1881-r01-rv64-kernel-build` | RV64 kernel build | 8 | PASS, exit 0 | 128.902 s |
| `agent-a56730cb1881-r02-la64-kernel-build` | LA64 kernel build | 8 | PASS, exit 0 | 128.370 s |
| `agent-854972a64dfd-r01-rv64-task-uaccess-gate` | RV64 focused + preliminary | 8 | PASS, exit 0 | 293.020 s |
| `agent-854972a64dfd-r02-la64-task-uaccess-gate` | LA64 focused + preliminary | 8 | PASS, exit 0 | 302.157 s |

focused LTP 在每个架构的 musl/glibc 中均执行 6 个语义 case，全部 PASS：
`get_robust_list01`、`set_robust_list01`、`setitimer01/02`、`timer_settime01/02`。
`timer_settime03` 为仓库长期排除的 overrun 专项，本批明确 **NOT RUN**。

| 架构 | online mask | 初赛结果 | 既有失败 |
|---|---|---:|---|
| RV64 | `0xff` | raw/semantic 312/314 | musl/glibc `busybox kill 10` |
| LA64 | `0xff` | raw/semantic 308/314 | 两套 `test_brk` 1/3、两套 `busybox kill 10` |

原始 child 日志与模型报告只保存在本地 ignored `cc-codex/runtime/jobs/`，不提交、不上传。

## 7. 证据边界

LTP 覆盖 ABI 成功/错误路径以及普通 timer 交付，初赛证明常规用户路径未退化。它们没有专门
制造两个 CPU 同时修改同一 TCB timer、copyout fault 与并发配置更新的精确交错；这些动态
竞态记为 **NOT RUN**。本批并发正确性依据 owner 锁内快照/提交、锁外 uaccess 的源码证明。
