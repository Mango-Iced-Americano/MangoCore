# B70 sigtimedwait 锁外回复证据摘要

## 1. 结论

状态：`pass`

`sigtimedwait()` 的 WaitQueue 条件闭包不再访问用户地址。闭包只从线程私有或进程共享
pending 队列唯一领取 `PendingSignal`，结果由 syscall 栈持有；等待路径退出、wait mask 清理
完成后才向用户写 `SigInfo`。

## 2. 锁序与错误语义

```text
copyin set/timeout
  -> 设置 task.signal_wait_mask
  -> WaitQueue 条件：signal owner 锁内 dequeue
  -> syscall 栈持有 PendingSignal
  -> 退出 WaitQueue，清除 signal_wait_mask
  -> copyout SigInfo
```

- 条件闭包可能在 WaitQueue 锁内运行，因此不得触发用户缺页、CoW 或 TLB shootdown；
- 同一 `Arc<TaskControlBlock>` 跨越整个等待调用，迁移后仍操作原 TCB；
- `ERESTART`、timeout 和 NULL `info` 路径不产生伪造的 siginfo 写入；
- copyout `EFAULT` 不重新入队已领取信号。

Linux 6.6 `kernel/signal.c::do_sigtimedwait()` 在 signal lock 内 dequeue，syscall wrapper 随后
执行 `copy_siginfo_to_user()`；因此写回失败仍消费信号，与本批顺序一致。

## 3. AI 协作与裁决

| Job | 结果 | 独立裁决 |
|---|---|---|
| `smp-b70-sigtimedwait-plan-review-r1` | ACCEPT/NOT RUN | 采纳“栈上持有领取结果、锁外 copyout” |
| `smp-b70-sigtimedwait-final-review-r1` | PASS/NOT RUN | 七项静态不变量通过；另识别登记窗口竞态，拆为后续节点 |
| `smp-b70-sigtimedwait-validation-r1` | 中止 | 两次 Docker build 有效；模型自匹配 `pgrep` 等待被终止 |
| `smp-b70-sigtimedwait-validation-r2` | REVIEWED/FAIL | inline runner 仅缓存目录前 16 KiB，目标用例未枚举，不计功能证据 |
| `smp-b70-sigtimedwait-validation-r3` | REVIEWED/原始用例 PASS | suite runner 双架构各 11 TPASS；wrapper 因旧 judge 兼容行误报 FAIL |

r2 报告根据任务结束后的当前脚本误称当次已使用 suite runner，与当次原始日志不符，未采纳。
r3 正确识别 suite runner 无条件打印 `FAIL LTP CASE ... : 0` 的旧 judge 兼容行；权威结果是紧随
其后的 `LTP CASE RESULT ... : PASS (0)` 和 LTP Summary。本地 ignored recipe 已改为只判定
权威 marker；没有为了把 wrapper 变绿而重复运行完全相同的 QEMU。

## 4. 冻结源码

```text
base HEAD: 25d14969e9128e0ae7281efcb0824bfa5a6bd855
tracked diff SHA-256: e007ddca78f1eb2bc241d80748701d92fddda41c7857e721144ab3551c86204a
0cabbe7de892a68774615381ebdba8aa3ecc3020b4eef9ccb7a673830b9d36f5  os/src/task/signal/wait.rs
```

四项有效 child 的 source-before/source-after 均一致，`mutation_detected=false`。

## 5. Docker 验证

| Child | Recipe | CORE_NUM | 原始结果 | 时长 |
|---|---|---:|---|---:|
| `agent-2575af1bd33e-r01-rv64-kernel-build` | RV64 kernel build | 8 | PASS, exit 0 | 125.130 s |
| `agent-2575af1bd33e-r02-la64-kernel-build` | LA64 kernel build | 8 | PASS, exit 0 | 130.884 s |
| `agent-87c95053d147-r01-rv64-sigtimedwait-gate` | RV64 suite focused | 8 | 11 TPASS, exit 0 | 125.920 s |
| `agent-87c95053d147-r02-la64-sigtimedwait-gate` | LA64 suite focused | 8 | 11 TPASS, exit 0 | 133.515 s |

两架构均打印 `online_mask=0xff`、`LTP CASE RESULT sigtimedwait01 : PASS (0)`，汇总均为
11 passed、0 failed、0 broken、0 skipped。musl `sigtimedwait01` 命中既有 libc exclude，
`rt_sigtimedwait01` 不在 include 集合，两者均为 **NOT RUN**，不是 PASS。

B69 已在相同父 HEAD 完成双架构 8 核 `mask=0x003`，本批只移动一个 signal copyout 锁边，
因此按风险复用该新鲜初赛证据，不重复全量。

## 6. 证据边界

LTP 覆盖普通领取、siginfo、mask 恢复、timeout 与用户地址错误；未精确制造 signal 恰好落在
WaitQueue 第二次条件检查与调度器睡眠登记之间的跨核交错。只读审查已确认该既有窗口需要让
waited signal 参与最终睡眠复查，并在被通用 Interrupted 唤醒后重新 dequeue；作为下一独立
生产节点处理，不把本批锁外 uaccess 的功能证据外推为该竞态已经解决。

原始 child 日志与模型报告保存在本地 ignored `cc-codex/runtime/jobs/`，不提交、不上传。
