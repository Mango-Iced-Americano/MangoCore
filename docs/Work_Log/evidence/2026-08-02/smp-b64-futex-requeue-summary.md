# B64 futex requeue waiter 身份证据

## 1. 结论

B64 修复了通用 `WaitQueue` 无法表达 futex requeue 成员迁移的问题。每次 wait 现在由独立
`Arc<FutexWaiter>` 表示，记录 current key 和真实 wake 状态；timeout、signal、tail-spin
与 waitv 都按当前 key 和准确 Arc 身份清理。

本节点状态：`partial`。实现与双架构 8 核 focused/初赛非回归门禁通过，但精确的
requeue-timeout、requeue-signal、waitv+requeue 动态竞态仍为 NOT RUN；shared key 的 raw
PPN ABA 和 table 锁内 faultable uaccess 留给后续节点。

## 2. 根因与修复

旧实现只在 source/target `WaitQueue` 中搬运 `Weak<TCB>`：

```text
T 在 source 阻塞
  -> requeue 把 Weak<T> 搬到 target
  -> timeout 或 signal 让 T 恢复
  -> T 仍去 source finish_wait()
  -> “source 中不存在”被误判为真实 wake
  -> 错误返回 SUCCESS，target 留下活动 Weak
```

waitv 还会把第一个“不在原队列”的 entry 误报为 wake 下标，且同一 TCB 的多项注册没有
独立身份。

新结构：

```text
FutexTable
  -> key -> FutexQueue
              -> Arc<FutexWaiter> { Weak<TCB>, current key, woken }
```

线性化顺序：

- requeue：从 source 取出 → Release 更新 current key → 发布到 target；
- wake：从队列消费 → Release 发布 `woken` → 必要时 `wake_interruptible()`；
- cleanup：Acquire 读取 current key → `Arc::ptr_eq` 精确删除；
- waitv：每个数组项独立 waiter，按 Linux 当前实现返回最后一个已 wake 下标。

通用 `WaitQueue` 的 futex 专用 `normal_wake_result` 分支随之删除。event/IPC 等普通条件等待
继续使用通用队列，没有向其扩散 futex token 或测试字段。

## 3. 官方语义对照

对照 Linux 主线源码：

- `kernel/futex/futex.h`：一次等待由独立 `futex_q` 表示，包含当前 key/bucket 状态；
- `kernel/futex/requeue.c`：requeue 更新完整注册项而非只移动任务；
- `kernel/futex/waitwake.c`：`futex_unqueue_multiple()` 返回最后一个已经 wake 的数组下标；
- `include/linux/futex.h` 与 `get_futex_key()`：共享 key 绑定 mm/file backing 身份，优先避免
  不相关对象的 false-positive 匹配。

参考链接：

- <https://github.com/torvalds/linux/blob/master/kernel/futex/futex.h>
- <https://github.com/torvalds/linux/blob/master/kernel/futex/requeue.c>
- <https://github.com/torvalds/linux/blob/master/kernel/futex/waitwake.c>
- <https://github.com/torvalds/linux/blob/master/include/linux/futex.h>

MangoCore 没有机械复制 Linux 的哈希桶/rt_mutex 实现，只引入本节点所需的 registration
identity、current location 和 wake attribution。

## 4. 变更范围

| 文件 | 变更 |
|------|------|
| `os/src/task/threads.rs` | 新增专用 table/queue/waiter；统一 wait/wake/requeue/waitv |
| `os/src/syscall/process/futex.rs` | syscall 解析输出改为 `FutexWaitSpec` |
| `os/src/task/{mod,process,task}.rs` | PCB 字段与构造路径使用 `FutexTable` |
| `os/src/task/manager.rs` | 删除无调用者的 futex 专用 WaitQueue wake-result API |
| `docs/`、`AGENTS.md` | 同步成员模型、锁序、验收边界和 AI 使用记录 |

没有增加生产测试字段、临时 IPI、调试模块或新的源码文件。

## 5. Docker 环境

| 项 | 值 |
|----|----|
| 容器 | `mangocore-smp-integration-20260725-os-dev-1` |
| image | `zhouzhouyi/os-contest:20260510` |
| image ID | `sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac` |
| repo digest | `sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d` |
| image created | `2026-05-10T08:46:16Z` |
| QEMU RV64 / LA64 | `10.0.2` / `10.0.2` |
| 基线 HEAD | `fd6dba9ef0c772433ffdd49364f9fe3f847d5f5e` |

所有构建和 QEMU 均由本地 DeepSeek worker 在上述 Docker 中串行执行；`cc-codex/` 任务、
日志和配方均为 ignored 本地材料，不提交或上传。

## 6. 构建门禁

任务 `smp-b64-frozen-build`：

| child | 命令语义 | 结果 |
|-------|----------|------|
| `agent-b7e72f35be89-r01-rv64-kernel-build` | RV64 normal kernel，`CORE_NUM=8` | PASS，exit 0 |
| `agent-b7e72f35be89-r02-la64-kernel-build` | LA64 normal kernel，`CORE_NUM=8` | PASS，exit 0 |

两次构建前后 tracked diff SHA-256 均为
`d60ace8b3411459dfed59bcf7f9ebd7feb56c058d2635e0268c3890ee4a6c32a`，无源码 mutation。
此前 `smp-b64-build-review` 的两个 child 虽 process exit code 为 0，却因尚未收口的 wrapper
marker 规则标成 FAIL，已由冻结任务替代，不计入内核结果。

## 7. focused 与初赛回归

### 7.1 初始完整门禁

任务 `smp-b64-futex-final` 在语义等价的初版专用 waiter 上执行：

| child | 结果 |
|-------|------|
| `agent-6b3040397459-r01-rv64-futex-ltp` | exit 0；musl+glibc 合计 20 PASS、6 SKIP |
| `agent-6b3040397459-r02-la64-futex-ltp` | exit 0；musl+glibc 合计 20 PASS、6 SKIP |
| `agent-6b3040397459-r03-rv64-preliminary` | 312/314；仅两套 `busybox kill 10` |
| `agent-6b3040397459-r04-la64-preliminary` | 308/314；两套 `test_brk` 1/3 与两套 `busybox kill 10` |

四个 child 均为 `CORE_NUM=8`、`online_mask=0xff`，无 panic、fatal trap、runner timeout、
forbidden marker 或源码 mutation。初赛失败集合与 B63 基线一致。

### 7.2 最终结构冻结门禁

删除 1:1 token 包装、把身份直接合并进 `FutexWaiter`，并将 waitv 下标从“第一个”修正为
Linux 当前语义的“最后一个”后，任务 `smp-b64-final-refactor-r2` 重跑双架构 focused：

| child | duration | 调用结果 | 进程结果 |
|-------|----------|----------|----------|
| `agent-0fb412aae1b0-r01-rv64-futex-ltp` | 314.120 s | 20 PASS + 6 SKIP | PASS，exit 0 |
| `agent-0fb412aae1b0-r02-la64-futex-ltp` | 327.707 s | 20 PASS + 6 SKIP | PASS，exit 0 |

每架构实际运行 musl 13 次和 glibc 13 次，共 26 次。10 个用例在两套 libc 下 PASS：
`futex_cmp_requeue02`、`futex_wait01..05`、`futex_wake01..03`、
`futex_wait_bitset01`。`futex_waitv01..03` 在两套 libc 下均 SKIP，原因是 LTP 明确要求
Linux 5.16+，MangoCore 当前 uname 为 5.10。

两次最终运行的 before/after tracked diff SHA-256 均为
`b0569d02d9fde5fdc4ed89903831feea8640b38fcbd90ca621e01c577703a30d`；
HEAD、status、untracked 指纹也完全一致。测试结束后只有 `threads.rs` 的术语注释和文档发生
变化，生产逻辑未变，因此未机械重复初赛长回归。

## 8. DeepSeek 结论裁决

DeepSeek 对根因、current key、wake-before-runnable、Arc 精确清理和无引用环的判断成立；
以下汇总错误由 GPT/Codex 直接依据源码和原始日志纠正：

1. 初版报告称变更“仅为重命名”，实际包含完整 waiter 身份重构；
2. 初版报告称 `wake_interruptible()` 位于 table 锁外，最终实现中外层 table guard 仍持有，
   锁序是 `FutexTable -> TASK_MANAGER -> RunQueue`；
3. 初版接受 waitv 第一个 wake 下标，Linux 当前实现明确保留最后一个，源码已改为
   `rposition()`；
4. 最终报告把每种 libc 的 10 PASS + 3 SKIP 误写成每架构总数；原始日志清楚包含 musl
   和 glibc 两段，正确总数为每架构 20 PASS + 6 SKIP。

任务 `smp-b64-final-refactor` 因 dispatcher 已自动启动后又被手动运行而发生 lease 竞争，
状态为 FAILED，未形成内核测试；重复进程终止后使用新 job ID `...-r2` 完成有效门禁。

## 9. NOT RUN 与后续边界

| 场景 | 状态 | 原因/后续 |
|------|------|-----------|
| requeue 后 timeout | NOT RUN | focused 未构造精确交错；静态顺序已审查 |
| requeue 后 signal | NOT RUN | 同上 |
| waitv + requeue | NOT RUN | waitv LTP 又受 uname 版本门禁 |
| waitv 多 key 同时 wake | NOT RUN | 未有动态竞争用例 |
| `futex_cmp_requeue01` | NOT RUN | 1000+ waiter 超出当前任务/LA64 配额风险 |
| shared raw PPN + offset ABA | NOT RUN | B65 稳定 backing identity |
| table 锁内 faultable uaccess | NOT RUN | 后续独立缩短自旋锁临界区 |

不要把 focused PASS 外推为上述竞态已动态证明，也不要为解锁 waitv LTP 而单独伪造更高
uname；那会同时开启大量新版本 ABI 假设。
