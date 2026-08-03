# develop Batch 5 signalfd 动态 sighand 等待域证据

## 变更身份

- 基线 HEAD：`a2737f301a70d9bed331f9ca6195adaaf8161d4d`
- 分支：`codex/smp-develop-integration`
- 测试源码 tracked diff SHA-256：
  `cd104a89c85c6ee248c48d5200aabd041fce45561de7de317742244544168202`
- 验收状态：`pass`

## 静态证据

- Linux 6.6 `fs/signalfd.c`：read/poll 订阅 `current->sighand->signalfd_wqh`，signal enqueue
  和 mask update 唤醒 sighand queue。
- signalfd(2)：fork 后 child 可读取自己的 pending；fork 前加入 epoll 的 signalfd registration
  不会通知 child 自己的信号，child 应重新注册。
- MangoCore 普通 fork 通过 `Sighand::from_existing()` 复制 action、创建新事件队列；
  `CLONE_SIGHAND` 复用同一个 `Arc<Mutex<Sighand>>`。
- `File::read_wait_queue()` 与 `read_event_queue()` 按 `ReadWaitSource` 动态取得 current sighand；
  普通 inode 路径保持原有 inode-owned queue。
- private/shared pending producer 均在 owner 锁外调用 `notify_signalfd()`，未新增双锁嵌套。

## DeepSeek 只读审查

- Job：`develop-batch5-signalfd-final-review-r2-20260803`
- 结论：动态 owner、fork/CLONE_SIGHAND、丢失唤醒、重复消费、VFS 路径和 raw pointer
  lifetime 未发现 P0 阻塞项。
- GPT 复核：审查提出的无条件通知与 Linux `signalfd_notify()` 一致；mask 是共享 open-file
  状态，不因 exec 清零；fork 前 epoll registration 的限制由 Linux man page 明确记录。

## Docker 最终回归

父 Job：`develop-batch5-signalfd-final-regression-r2-20260803`，DeepSeek effort=max，双架构
严格串行，`CORE_NUM=8`。

| 架构 | child job | 耗时 | 退出码 | online | L4 | 变异 |
|------|-----------|------|--------|--------|----|------|
| RV64 | `agent-97bb77bbafed-r01-rv64-regression-8core` | 143.427s | 0 | `0xff` | 7/7 PASS | false |
| LA64 | `agent-97bb77bbafed-r02-la64-regression-8core` | 142.620s | 0 | `0xff` | 7/7 PASS | false |

两架构均满足：

- `blocking detail: count=128 signo=10`，延迟超过 100ms，sender 正常回收；
- inherited fd：ready/send/result/reap 全成功，child 收到 SIGUSR1；
- `ok 6 signalfd`、`ok 7 clone_vm_second_slot`；
- `[L4 REGRESSION RESULT: PASS]`；
- 无 forbidden marker、required marker 缺失、panic、fatal trap 或 timeout；
- source before/after fingerprint 完全一致。

## 根因回环

早期 RV64 诊断中，signalfd 已读到正确 signo/result byte，但 wait/reap 返回异常；LA64 同源码
正常。内核 dispatch 的 wait4 会读取第 4 个 rusage 参数，用户库却调用三参数 wrapper：LA64
桥接汇编恰好补零，RV64 inline asm 留下未约束 a3。改为四参数调用并显式传 `rusage=0` 后，
RV64 signalfd 与 clone probe 同时转绿，闭合 RED→root cause→GREEN。

## 验收边界

- 本批验证阻塞 read 与普通 fork 继承 fd；未新增专门的 CLONE_SIGHAND 双线程、epoll mask
  更新并发或信号风暴性能测试。
- 同一生产内核的双架构 normal build 和 8 核 `mask=0x003` 已在本批早期通过；最终回归前的
  后续生产改动仅为用户态 wait4 wrapper，因此没有机械重复初赛矩阵。
