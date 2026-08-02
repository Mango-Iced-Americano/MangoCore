---
title: "futex 与线程退出协作"
category: process
status: stable
author: MangoCore Team
last_update: 2026-08-02
tags: [process, futex, smp, requeue]
---

# futex 与线程退出协作

## 1. 源码与职责

| 文件 | 内容 |
|------|------|
| `os/src/syscall/process/futex.rs` | syscall 参数校验、private/shared key 选择、waitv 解析 |
| `os/src/task/threads.rs` | `FutexTable`、wait/wake/requeue、timeout 与 waitv |
| `os/src/task/task.rs` | `clear_child_tid` 退出唤醒 |
| `os/src/mm/address_space.rs` | 判断 VMA 是否需要 shared key、地址翻译 |

futex 不再复用通用 `WaitQueue`。通用队列只记录任务，无法表示一次等待在
`FUTEX_REQUEUE` 后的当前位置，也无法区分同一任务的多个 `futex_waitv` 注册项。

## 2. 支持范围

| 命令 | 状态 |
|------|------|
| `FUTEX_WAIT` / `FUTEX_WAKE` | 支持 |
| `FUTEX_WAIT_BITSET` / `FUTEX_WAKE_BITSET` | 支持；wake 暂未按 bitset 筛选 |
| `FUTEX_REQUEUE` / `FUTEX_CMP_REQUEUE` | 支持 |
| `futex_waitv` | 支持 32-bit futex 子集，最多 128 项 |
| PI futex、`FUTEX_WAKE_OP`、`FUTEX_FD` | 未支持 |

`FUTEX_PRIVATE_FLAG` 和 `FUTEX_CLOCK_REALTIME` 在 syscall 层解析。普通 wait 的 timeout
是相对时间；wait-bitset 和 waitv 使用绝对 deadline。

## 3. key 与等待表

```rust
enum FutexKey {
    Private(usize),
    Shared(usize),
}
```

| 类型 | 当前 key | 等待表 |
|------|----------|--------|
| private | 当前进程内的用户虚拟地址 | `ProcessControlBlock::futex()` |
| shared | 物理页号与页内偏移组合 | 全局 `PROCESS_SHARED_FUTEX` |

清除 `FUTEX_PRIVATE_FLAG` 不会无条件选择全局表。内核先调用
`AddressSpace::futex_uses_shared_key()`；只有真实 `MAP_SHARED` VMA 才翻译为 shared key，
其余映射仍使用当前进程的 private 表。

shared 表旁的 `PROCESS_SHARED_FUTEX_MAYBE_NONEMPTY` 只是调度循环的快速提示。
`compact_shared_futex()` 每 64 次调用清理失效 waiter 和空 key；它不参与正确性裁决。

> 当前 shared key 仍是 raw PPN + offset。物理页释放后复用可能造成 ABA；B65 将把它替换
> 为由 backing 生命周期约束的稳定身份。本节只描述 B64 已实现的 requeue 正确性。

## 4. 专用数据模型

```rust
pub struct FutexTable {
    queues: BTreeMap<usize, FutexQueue>,
}

struct FutexQueue {
    waiters: VecDeque<Arc<FutexWaiter>>,
}

struct FutexWaiter {
    task: Weak<TaskControlBlock>,
    key: AtomicUsize,
    woken: AtomicBool,
}
```

三个层次各自只有一个职责：

- `FutexTable`：在一把外层锁下串行化注册、wake、requeue 和撤销。
- `FutexQueue`：维护某个 key 的 FIFO 成员关系。
- `FutexWaiter`：表示一次稳定的等待注册，而不是一个任务。

队列持有 `Arc<FutexWaiter>`，syscall 栈也持有同一个 `Arc`；撤销使用
`Arc::ptr_eq()` 精确匹配。waiter 到 TCB 使用 `Weak`，因此不会形成引用环，也不会仅因
等待队列残留而延长任务寿命。

`futex_waitv` 的每个数组元素都有独立 waiter。即使同一个 TCB 用同一 key 注册多次，wake
也只消费实际命中的注册项，清理不会误删兄弟项。

## 5. WAIT 的注册协议

wait 必须同时避免“值已变化却睡下”和“wake 发生后仍返回 timeout”。当前顺序为：

```text
快速检查用户 word
  -> 分配 FutexWaiter
  -> 获取 FutexTable 锁
  -> 检查 deadline / word / signal
  -> enqueue(waiter)
  -> 最后一次检查用户 word
  -> 释放锁并阻塞
```

入队后的最后一次 word 检查覆盖检查与发布之间的窗口。注册完成后不再读取原 futex word：
waiter 可能已被 requeue 到另一个 key，此时原地址的值不再能证明本次等待是否被唤醒。

恢复后，等待方在同一 table 锁下按以下优先级裁决：

1. `waiter.woken == true`：正常返回 0；
2. deadline 到期：按 waiter 当前 key 精确撤销并返回 `ETIMEDOUT`；
3. 有可处理信号：精确撤销并返回 `EINTR`；
4. 否则重新进入阻塞。

短 timeout 的单线程自旋和尾部 spin guard 仍保留，用来控制 QEMU 下的定时误差；它们不
绕过上述 waiter 身份协议。尾部自旋同样取得 table 锁后裁决 wake/timeout 的胜者。

## 6. WAKE 的发布顺序

`wake_at_most(limit)` 在 table 锁内扫描调用时已有的队列长度：

```text
从队首取 waiter
  -> 丢弃失效 Weak / 非法 New、Zombie 项
  -> waiter.woken = true (Release)
  -> 使旧 timer generation 失效
  -> 若任务已 Blocking/Blocked，调用 wake_interruptible()
```

先发布 `woken`，再让任务 runnable。等待 CPU 用 Acquire 读取，所以不会出现任务已经运行、
却仍把真实 wake 误判成 timeout 或 signal 的窗口。

尚未达到 wake 数量的 waiter 被推回原队列；只扫描原长度，因此不在自旋锁内创建临时
`Vec`，也保持未消费项的 FIFO 顺序。`Queued/Running` 仍可能是“入队后、正式阻塞前”的
合法窗口，此时只标记真实 wake，不重复发布任务。

锁顺序固定为：

```text
FutexTable -> TASK_MANAGER -> 单个 RunQueue
```

阻塞 helper 会在切换前释放 `FutexTable` guard；任何路径都不得跨 context switch 持锁。

## 7. REQUEUE 的成员关系

`FUTEX_REQUEUE` 先从 source 唤醒最多 `val` 项，再移动最多 `val2` 项。source 与 target
属于同一张表，所以整个操作只持一把 table 锁，不需要双队列锁顺序。

每个被移动 waiter 的顺序固定为：

```text
从 source 弹出
  -> waiter.key = target (Release)
  -> 发布到 target 队列
```

目标队列可见前先更新 current key。这样 timeout、signal 和 waitv 清理总能找到 waiter 的
真实位置。不能再通过“是否仍在最初 source 队列”推断正常 wake：requeue 和 wake 都会让
source membership 消失，但二者的返回语义完全不同。

`FUTEX_CMP_REQUEUE` 还要求 `*uaddr == val3`，否则返回 `EAGAIN`。两个 key 必须同为 private
或同为 shared，混合类型返回 `EINVAL`。

## 8. futex_waitv

syscall 层先把用户数组解析成内核拥有的 `FutexWaitSpec`：

```rust
pub struct FutexWaitSpec {
    pub futex_word: UserPtr<u32>,
    pub futex_key: usize,
    pub val: u32,
}
```

所有条目在同一张 table 锁下注册。任一 waiter 被 wake 后，内核按数组顺序撤销全部注册，
并返回最后一个已经被唤醒的下标。使用“最后一个”是当前 Linux
`futex_unqueue_multiple()` 的明确语义；并发唤醒多个 key 时不能擅自改成第一个。

timeout 返回 `ETIMEDOUT`，信号返回 `EINTR`，任一初始值不匹配返回 `EAGAIN`。private 与
shared 条目不能混在同一次 waitv 中。

## 9. clear_child_tid

线程退出时：

1. 向 `clear_child_tid` 用户地址写 0；
2. 唤醒进程 private key；
3. 若地址属于 shared VMA，同时唤醒全局 shared key；
4. 若 fault 前后物理 key 改变，同时尝试旧 key 与新 key。

这条路径供 pthread join 使用，与 syscall wake 共用 `FutexTable::wake()`，不再经过通用
`WaitQueue`。

## 10. 错误与不变量

| 条件 | 返回 |
|------|------|
| wait 时 word 不等于期望值 | `EAGAIN` |
| 用户地址不可读 | 对应 uaccess errno |
| 可处理信号 | `EINTR` |
| deadline 到期 | `ETIMEDOUT` |
| 正常 wake | 0；waitv 返回数组下标 |

必须保持以下不变量：

1. 每次注册最多属于一个 futex queue。
2. requeue 在目标发布前更新 waiter 当前 key。
3. wake 在任务 runnable 前发布 waiter 的 `woken`。
4. timeout/signal 按当前 key 和准确 Arc 身份撤销。
5. 注册后的恢复路径不重读最初 futex word。
6. table 锁是 wait/wake/requeue/cleanup 的唯一线性化点。

## 11. 已验证与剩余边界

B64 的双架构 8 核 focused LTP 每架构执行 musl、glibc 各 13 次：20 PASS、6 SKIP。
六个 SKIP 是 `futex_waitv01/02/03` 在两套 libc 下因内核报告 Linux 5.10、用例要求 5.16
而跳过，不代表 waitv 动态通过。

以下场景仍是 **NOT RUN**：

- requeue 后 timeout / signal 的精确竞态；
- waitv 与 requeue 组合、多个 key 同时 wake；
- shared raw PPN key 的释放复用 ABA；
- table 自旋锁内 faultable 用户读取的锁序与时延。

设计对照：Linux 当前的
[`futex_q`](https://github.com/torvalds/linux/blob/master/kernel/futex/futex.h)、
[`futex_requeue`](https://github.com/torvalds/linux/blob/master/kernel/futex/requeue.c) 和
[`futex_unqueue_multiple`](https://github.com/torvalds/linux/blob/master/kernel/futex/waitwake.c)。

## 12. 调试核对点

| 现象 | 优先检查 |
|------|----------|
| requeue 后错误超时或目标队列残留 | waiter current key、`Arc::ptr_eq` 清理 |
| wake 后仍返回 `EINTR/ETIMEDOUT` | `woken` 是否先于 runnable 发布 |
| waitv 返回错误下标 | 每项独立 waiter、最后 woken index |
| shared futex 跨进程不醒 | VMA 是否 `MAP_SHARED`、shared key 是否一致 |
| pthread join 卡住 | clear_child_tid 写 0 与 private/shared wake |
