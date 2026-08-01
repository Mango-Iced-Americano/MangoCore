---
date: 2026-08-01
timezone: Asia/Shanghai
phase: smp-b61
status: pass
---

# B61 SysV 消息唯一摘取证据摘要

## 目标与结论

旧普通 `msgrcv` 把一个逻辑领取拆成两个 `MSG_REGISTRY` 临界区：第一段按 index 复制消息和
serial，用户 copy 成功后第二段再按 serial 删除。两个 CPU 可以在删除前都复制同一条消息，
随后一个删除成功、另一个静默找不到 serial，但两个 syscall 都返回成功。

B61 把普通分支的 `VecDeque::remove(idx)` 设为唯一线性化点。消息在 registry 锁内 move 给
一个调用者，队列统计与 sender wake 在同一临界区完成；锁释放后才写用户 buffer。
`MSG_COPY` 仍只复制内核快照。实现未增加新状态、结构或生产测试代码。

## 并发交错与修复

旧交错：

```text
CPU0: 锁内观察消息 N，clone 后解锁
CPU1: 锁内观察消息 N，clone 后解锁
CPU0: copy_to_user，重锁删除 N，返回成功
CPU1: copy_to_user，重锁未找到 N，仍返回成功
```

新交错：

```text
CPU0: MSG_REGISTRY 锁内选择并 remove(idx)，取得 Message 所有权
CPU1: 只能在 CPU0 解锁后重新选择；原消息已经不在队列
CPU0: 锁外 copy_to_user
```

普通分支同时更新 `cbytes`、`lrpid`、`rtime` 并调用 waiter wake。返回的 `Message.data` 已经
是 owned `Vec`，因此删除了 `Message.serial`、`MsgQueue.next_serial` 和
`remove_received_message()`；不再保留容易让维护者误解为提交协议的事后删除链。

`MSG_COPY` 不能 move 消息，所以在同一锁内复制稳定快照后返回；队列内容和统计不变。

## Linux 与 LTP 对照

Linux `do_msgrcv()` 的普通路径在对象锁内执行 `list_del` 和队列统计更新，释放锁后才调用
用户 copy handler，随后 `free_msg(msg)`。因此用户 copy 返回 `EFAULT` 时消息已经消费，
不会回滚。MangoCore 保持同样的所有权交接，但没有机械照搬 Linux 的 RCU/IDR 生命周期：

- [Linux `ipc/msg.c`](https://github.com/torvalds/linux/blob/master/ipc/msg.c)

采用的 LTP focused case 为：

```text
msgrcv01 msgrcv02 msgrcv03 msgrcv05 msgrcv06 msgrcv07 msgrcv08
msgsnd01 msgsnd02 msgsnd05 msgsnd06
```

官方源码表明：`msgrcv01` 检查消费后 `cbytes/qnum/lrpid/rtime`，`msgrcv02` 包含 EFAULT，
`msgrcv07` 检查 `MSG_COPY` 后消息仍在队列；这些 case 没有构造两个 receiver 同时争抢
一条消息：

- [LTP `msgrcv01.c`](https://github.com/linux-test-project/ltp/blob/master/testcases/kernel/syscalls/msgrcv/msgrcv01.c)
- [LTP `msgrcv02.c`](https://github.com/linux-test-project/ltp/blob/master/testcases/kernel/syscalls/msgrcv/msgrcv02.c)
- [LTP `msgrcv07.c`](https://github.com/linux-test-project/ltp/blob/master/testcases/kernel/syscalls/msgrcv/msgrcv07.c)

## AI 协作与人工裁决

- `smp-b61-msgrcv-claim-design` 在 clean HEAD 上只读确认旧竞态、serial 引用闭环和最小修改
  范围；没有修改工作树；
- 该报告误称 Linux 在锁内执行实际 `free_msg()`。GPT 直接复核官方 `ipc/msg.c`，纠正为
  “锁内摘链和统计更新，锁外用户 copy 与 free”，但保留其关于线性化点的有效结论；
- DeepSeek 建议以 enum 区分普通摘取和 `MSG_COPY` 快照，未采纳：调用者对二者都只需锁外
  写同一份 owned 数据，新增 enum 不承载不同调用方行为，反而增加分支和命名负担；
- `smp-b61-msgrcv-compile` 和 `smp-b61-msgrcv-final` 均通过受限 Docker recipe 串行运行，
  DeepSeek 只读源码与汇总日志；所有 `cc-codex/` 任务和原始日志仅保存在本地忽略目录；
- 最终报告把 focused 简写为每架构 11/11。GPT 读取原始 marker 后按 libc 维度纠正为每架构
  musl 11/11 + glibc 11/11，即 22/22，双架构总计 44/44。

## 最终验证

受测 HEAD 为 `146799075e2756eb94d138b0c1440c4093463b0d`，运行时 child 的 tracked diff
SHA-256 均为：

```text
dc16c7676673e312fc5bf70defd60312806bce26d89d39e09db4303d51a624d8
```

| 架构 | 配置 | 结果 | 用时 | 说明 |
|------|------|------|------|------|
| RV64 | `CORE_NUM=8`, suite focused | musl 11/11 + glibc 11/11 | 298.135s | 22/22，exit 0 |
| LA64 | `CORE_NUM=8`, suite focused | musl 11/11 + glibc 11/11 | 316.288s | 22/22，exit 0 |
| RV64 | `CORE_NUM=8 mask=0x003` | 312/314 | 347.833s | 仅两套既有 `busybox kill 10` |
| LA64 | `CORE_NUM=8 mask=0x003` | 308/314 | 367.487s | 两套既有 `test_brk` 1/3 与 `kill 10` |

两次独立 build-only 也在同一代码指纹下串行通过：RV64 约 127s、LA64 约 134s。四个运行时
child 均 `online_mask=0xff`、`mutation_detected=false`，无 required marker 缺失、forbidden
marker、panic、fatal 或 timeout；初赛失败集合与 B60 精确一致。

测试启动前曾有另一个任务的 RV64 单核 QEMU 正处于自身 600 秒超时尾段；B61 的目标 8 核
QEMU 在该进程退出后才启动。原始日志证明目标启动拓扑、case marker 和最终退出均完整，
没有把先前进程输出混入当前 child。

## 未运行与边界

- 两个 receiver 在不同 CPU 同时争抢唯一消息：NOT RUN；现有 LTP 不含该动态场景，当前由
  registry 锁内 move 的静态所有权证明覆盖；
- `msgstress01`：NOT RUN；其默认长测主要为每队列一 sender/一 receiver，不直接命中本竞态，
  没有为增加测试数量而机械追加；
- `make lint`：NOT RUN；双架构最终 recipe 已各自包含内核编译；
- B61 不证明 SysV IPC ID 删除复用无 ABA，也不证明 WaitQueue 与 `IPC_RMID` 的所有并发交错；
- FS/Net/Driver 全面共享状态审计不在当前负责人范围内。
