---
date: 2026-08-01
timezone: Asia/Shanghai
phase: smp-b60
status: pass
---

# B60 IPC registry 锁外用户访问证据摘要

## 目标与结论

B60 处理 `os/src/syscall/process/ipc.rs` 中确定存在的锁序问题：`semctl(GETALL/SETVAL/
SETALL)` 与 `mq_open(O_CREAT)` 原先可能持 IPC 全局表锁进入 faultable uaccess。缺页、CoW
或映射同步会进入 MM/TLB 锁域，多核下不能把这些动作包在 IPC registry 锁内。

最终实现让读侧先形成内核快照、写侧锁外读取用户参数并在提交前重验；双架构 8 核定向
LTP 合计 36/36 PASS，初赛回归保持 RV64 312/314、LA64 308/314，失败集合未扩大。

## 设计裁决

### 1. semaphore 两阶段校验

- `GETALL` 首轮验证集合与读权限；分配后重锁取得一致的内核值快照，最后锁外写用户数组；
- `SETVAL` 先验证 `semid/semnum/owner`，锁外解析 LA64 兼容参数，再重锁重验并一次提交；
- `SETALL` 先快照集合长度与 owner 权限，锁外读取并检查整个数组，再重锁验证长度、权限并
  一次更新；
- `GETALL/SETALL` 按 ABI 忽略 `semnum`，不再被通用单元素分支错误拒绝；
- semaphore ID 由饱和计数改为单调 `Option<i32>`。`i32` 耗尽后 `semget` 返回 `ENOSPC`，
  绝不重新发布相同 ID，从而让两阶段重验具备明确的对象身份保证。

Linux 同样在持锁阶段取得/验证对象，在用户数组 copy 周围释放对象锁并于写侧重新验证；
`semctl(2)` 也明确 GETALL/SETALL 不使用 `semnum`：

- [Linux `ipc/sem.c`](https://github.com/torvalds/linux/blob/master/ipc/sem.c)
- [Linux `semctl(2)`](https://man7.org/linux/man-pages/man2/semctl.2.html)

### 2. `mq_open` 名称发布线性化

已有队列时只克隆 `Arc`，不访问 `attr`。名称不存在且要求创建时，先释放 `MQ_REGISTRY`
再读取、验证用户 attr 并构造候选对象；第二次名称表锁才是发布线性化点：

- 若另一 CPU 已创建同名队列，重新执行 `O_CREAT|O_EXCL` 判定；
- 若名称仍空，重新检查容量并插入候选对象；
- 权限检查只取得稳定 `Arc<MqQueue>` 的 inner 锁，不与名称表锁嵌套；
- fd 分配失败时仅在 `Arc::ptr_eq` 仍指向本次候选对象时回滚，避免并发 unlink + 同名重建
  后删除别人的新对象。

该发布点与 Linux/POSIX 对“检查存在并创建必须原子”的语义一致：

- [Linux `ipc/mqueue.c`](https://github.com/torvalds/linux/blob/master/ipc/mqueue.c)
- [Linux `mq_open(2)`](https://man7.org/linux/man-pages/man2/mq_open.2.html)

## AI 协作与人工裁决

- 首次只读审查在 GPT 继续改源码时触发指纹变化，按协议失效；
- `smp-b60-ipc-uaccess-review-r2` 在冻结 diff 上复核 21 个 IPC uaccess 点，确认目标 copy
  已离开 SEM/MSG/MQ registry 与 `MqQueue.inner` 锁，并认可“不复用 ID”足以排除当前 ABA；
- DeepSeek 建议顺带处理 `mq_timedreceive` copy 失败回滚，未采纳：Linux 普通破坏性 receive
  是先领取消息再 copy，用户 copy 失败仍消费消息，不能把它与 B60 锁序问题混为一谈；
- 首轮 `smp-b60-ipc-final-validation` 的 inline recipe 双架构均零 case。DeepSeek 判断衍生
  镜像缺少二进制；GPT 使用只读 `debugfs` 确认实际镜像中的目标目录、二进制和
  `runtest/syscalls` 条目都存在，因此撤销该根因，只保留“inline 路径未执行 case”的事实；
- 本地忽略的 recipe 改为 suite manifest，并以 9 个 `RUN LTP CASE` 和逐 case FAIL marker
  fail-closed；`smp-b60-ipc-ltp-suite-r2` 随后得到真实结果。

所有 `cc-codex/` task、manifest 与原始日志只保存在本地忽略目录，不上传 GitHub。

## 最终验证

受测 HEAD 为 `105367eedbd8c8314b5f736cab2c331af09b3dde`，tracked diff SHA-256：

```text
4592341ec65443d033e06c35f81e6f2ad741e2c163af2d137bbeabacef9e5499
```

| 架构 | 配置 | 结果 | 用时 | 说明 |
|------|------|------|------|------|
| RV64 | `CORE_NUM=8`, suite focused | musl 9/9 + glibc 9/9 | 285.551s | exit 0 |
| LA64 | `CORE_NUM=8`, suite focused | musl 9/9 + glibc 9/9 | 305.668s | exit 0 |
| RV64 | `CORE_NUM=8 mask=0x003` | 312/314 | 340.008s | 与 B59 一致 |
| LA64 | `CORE_NUM=8 mask=0x003` | 308/314 | 359.759s | 与 B59 一致 |

focused 的 9 个目标均为 `PASS (0)`：

```text
mq_open01
semctl01 semctl02 semctl03 semctl04
semctl05 semctl06 semctl07 semctl09
```

全部最终 child 均为 `mutation_detected=false`、`online_mask=0xff`，没有 forbidden marker、
panic、fatal 或 timeout。RV64 初赛仅两套既有 `busybox kill 10`；LA64 仅两套既有
`test_brk` 1/3 与两套 `busybox kill 10`。

## 未运行与边界

- 两个 CPU 同时触发 semaphore user fault、同名 `mq_open(O_EXCL)` 的专用 fault 注入：
  NOT RUN；当前由锁序静态证明、两阶段重验和双架构 ABI 回归覆盖；
- `make lint`：NOT RUN；
- 首轮 inline 定向 recipe：零 case，明确 NOT RUN，不能作为 PASS 证据；
- B60 只证明目标 IPC 用户访问不在 registry/queue 锁内，不证明 SysV 消息领取、阻塞等待
  与 wake 协议已经 SMP 安全；
- FS/Net/Driver 全面共享状态审计不在当前负责人范围内。
