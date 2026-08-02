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
    Shared(SharedFutexKey),
}
```

| 类型 | 当前 key | 等待表 |
|------|----------|--------|
| private | 当前进程内的用户虚拟地址 | `ProcessControlBlock::futex()` |
| shared | backing `Arc` 对象身份与页内偏移 | 全局 `PROCESS_SHARED_FUTEX` |

清除 `FUTEX_PRIVATE_FLAG` 不会无条件选择全局表。内核先调用
`AddressSpace::futex_shared_backing()`；只有真实 `MAP_SHARED` VMA 才取得 resident
`Arc<FrameTracker>`，其余映射仍使用当前进程的 private 表。VMA backing 与 PTE 映射在同一
VM 锁临界区内交叉验证；两者不一致时返回 `EFAULT`，不能把 waiter 发布到任意一侧。

`SharedFutexKey` 在 syscall/退出路径上传递拥有所有权的 `Arc`。进入等待表后，内部
`QueueKey` 只保存 `(Arc::as_ptr(), page_offset)` 供 `BTreeMap` 排序查找；对应
`FutexQueue::backing_pin` 为每个非空 shared key 保留一份 `Arc`。因此旧页被解除映射后，
它的分配对象不会在旧队列仍存在时析构，也就不会因 PPN 被新页复用而错误唤醒无关任务。
空队列被删除时 pin 随队列一起释放，不按 waiter 数量重复持有。

shared 表旁的 `PROCESS_SHARED_FUTEX_MAYBE_NONEMPTY` 只是调度循环的快速提示。
`compact_shared_futex()` 每 64 次调用清理失效 waiter 和空 key；它不参与正确性裁决。

## 4. 专用数据模型

```rust
pub struct FutexTable {
    queues: BTreeMap<QueueKey, FutexQueue>,
}

struct FutexQueue {
    waiters: VecDeque<Arc<FutexWaiter>>,
    backing_pin: Option<Arc<FrameTracker>>,
}

struct FutexWaiter {
    task: Weak<TaskControlBlock>,
    current_backing_identity: AtomicUsize,
    current_word_offset: AtomicUsize,
    woken: AtomicBool,
}
```

三个层次各自只有一个职责：

- `FutexTable`：在一把外层锁下串行化注册、wake、requeue 和撤销。
- `FutexQueue`：维护某个 key 的 FIFO 成员关系，并固定 shared backing 生命周期。
- `FutexWaiter`：表示一次稳定的等待注册，而不是一个任务；两个 current-key 原子字段只为
  Rust 跨 CPU 共享提供安全存储，调用者必须持有 table 锁，不能把它们当作无锁原子二元组。

队列持有 `Arc<FutexWaiter>`，syscall 栈也持有同一个 `Arc`；撤销使用
`Arc::ptr_eq()` 精确匹配。waiter 到 TCB 使用 `Weak`，因此不会形成引用环，也不会仅因
等待队列残留而延长任务寿命。

`futex_waitv` 的每个数组元素都有独立 waiter。即使同一个 TCB 用同一 key 注册多次，wake
也只消费实际命中的注册项，清理不会误删兄弟项。

## 5. WAIT 的注册协议

wait 必须同时避免“值已变化却睡下”和“在不可睡眠的 table 锁内触发缺页”。B66 将注册
拆成可重试的两层协议：

```text
锁外 faultable 读取用户 word 并比较 expected
  -> 锁外解析当前 private/shared key
  -> 锁外分配 FutexWaiter，并提前克隆当前 VM
  -> 获取 FutexTable 锁
  -> AddressSpace::try_read() 尝试取得 VM 锁
  -> 校验 shared backing、当前 PTE/读权限，并 nofault 读取 u32
  -> 检查 deadline / signal
  -> enqueue(waiter)
  -> 释放锁并阻塞
```

最后一次值比较与 enqueue 位于同一个 `FutexTable` 临界区：若用户态状态写与 wake 已先
发生，等待方随后会在锁内观察到变化后的 word；若状态写发生在 locked compare 之后，waker
只能在 waiter 发布后取得 table 锁并找到它。因此不再需要入队后的第三次读取。注册完成后
也不能读取原 futex word：waiter 可能已被 requeue 到另一个 key，此时原地址不再是本次注册
的权威状态。

`AddressSpace::try_read()` 是非阻塞锁边：VM 锁忙、PTE/权限变化或 shared backing 身份
变化都只产生内部 `FutexOpOutcome::Retry`。该结果只可能在 waiter 发布前返回；syscall
必须先释放 table 锁，再从锁外 faultable 读取和 key 解析重新开始，不能把 `Retry` 暴露为
用户 errno。普通 WAIT 的相对 timeout 在第一次尝试前转换为固定绝对 deadline，重试不会
延长用户请求的等待时间。

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
  -> 在 table 锁内更新 waiter 的两个 current-key 字段
  -> 发布到 target 队列
```

目标队列在发布第一个 waiter 前已经持有目标 backing pin，并在可见前更新 current key。
这样 timeout、signal 和 waitv 清理总能找到 waiter 的真实位置。不能再通过“是否仍在最初
source 队列”推断正常 wake：requeue 和 wake 都会让 source membership 消失，但二者的
返回语义完全不同。

`FUTEX_CMP_REQUEUE` 还要求 `*uaddr == val3`，否则返回 `EAGAIN`。B68 将比较和队列修改纳入
同一条可重试协议：syscall 先在 table 锁外 fault-in 两端地址并解析 key，随后取得 private
进程表或全局 shared 表的锁，再通过 `AddressSpace::try_read()` 做 nofault 复查。CMP source
word 的硬件宽度读取、比较以及紧随其后的 wake/requeue 共用同一个 table 临界区，因此不再
存在“锁外比较成功、另一 CPU 改值、旧请求仍继续搬队列”的窗口。

shared REQUEUE 必须在锁内重新核对 source/target 的 backing 身份、PTE 和读权限；映射改变或
VM 锁忙时，只能在任何 waiter 尚未移动前返回内部 `Retry`，释放 table 后重新 fault-in 并
重算两个 key。普通 private REQUEUE 不读取 source word，其 key 仍是当前 MM 内的 VA，所以
不额外要求 source 已有 PTE；CMP private 因为必须读取 source，仍要做锁内 PTE 复查。target
保留原有的可读映射校验。两个 key 必须同为 private 或同为 shared，混合类型返回 `EINVAL`。

## 8. futex_waitv

syscall 层先把用户数组解析成内核拥有的 `FutexWaitSpec`：

```rust
pub struct FutexWaitSpec {
    futex_word: UserPtr<u32>,
    queue_key: QueueKey,
    shared_backing: Option<Arc<FrameTracker>>,
    val: u32,
}
```

syscall 只能通过 `FutexWaitSpec::private()` 或 `::shared()` 构造条目。shared 构造器在注册前
保留 backing，table 入队时把它交给对应队列的单份 pin；这样 waitv 不会只携带可复用的整数
身份。

用户描述符数组只在 syscall 入口快照一次，避免内部重试改变本次请求的地址、期望值或 flags。
每次 `Retry` 都在锁外重新读取全部 futex word、重算全部当前 key，然后在一次
`AddressSpace::try_read()` 与同一张 table 锁下完成全组 nofault 比较和 waiter 发布。
waiter 数组在取得 table 锁前分配；`BTreeMap` 队列节点的既有锁内容量变化是单独的
allocator/锁序审计债务，不能据此声称整个 table 临界区绝不分配。任一 waiter 被 wake 后，
内核按数组顺序撤销全部注册，并返回最后一个已经被唤醒的下标。使用“最后一个”是当前 Linux
`futex_unqueue_multiple()` 的明确语义；并发唤醒多个 key 时不能擅自改成第一个。

timeout 返回 `ETIMEDOUT`，信号返回 `EINTR`，任一初始值不匹配返回 `EAGAIN`。private 与
shared 条目不能混在同一次 waitv 中。

## 9. clear_child_tid

线程退出时：

1. 向 `clear_child_tid` 用户地址写 0；
2. 唤醒进程 private key；
3. 若地址属于 shared VMA，同时唤醒全局 shared key；
4. 若 fault 前后的 backing 身份改变，同时尝试旧 key 与新 key。

这条路径供 pthread join 使用，与 syscall wake 共用 `FutexTable::wake()`，不再经过通用
`WaitQueue`。地址分类与 backing clone 在 VM 锁内完成；实际 private/shared wake 在 VM 锁
释放后执行，因此不存在 `AddressSpace -> FutexTable` 的嵌套持锁。

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
7. 每个非空 shared queue 必须持有与 `QueueKey` 对应的 backing pin，删除空队列才可释放。
8. faultable 用户访问、首次 key 解析、waiter 与临时数组分配不得持有 futex table 锁；
   `BTreeMap` 队列节点的既有容量变化仍需单独收口，不能误记为已经满足“全程无分配”。
9. table 锁内只能通过 VM `try_lock` 做 nofault 复查；失败必须释放 table 后从外层重试，
   禁止在 `FutexTable -> AddressSpace` 边上等待。
10. 值比较和 waiter 发布必须处于同一个 table 临界区，不得恢复入队后的第三次用户读取。
11. CMP_REQUEUE 的 source 比较与 wake/requeue 必须处于同一个 table 临界区；`Retry` 只能在
    队列尚未修改时返回，不能对已经部分移动的请求重放。

## 11. 已验证与剩余边界

B68 的双架构 8 核 focused LTP 每架构执行 musl、glibc 各 13 次：20 PASS、6 SKIP。
六个 SKIP 是 `futex_waitv01/02/03` 在两套 libc 下因内核报告 Linux 5.10、用例要求 5.16
而跳过，不代表 waitv 动态通过。两套 libc 的 `futex_cmp_requeue02` 均通过；双架构
`CORE_NUM=8` kernel build 同样通过。8 核 `mask=0x003` 初赛账本保持 RV64 312/314、
LA64 308/314，失败集合未扩大。

以下场景仍是 **NOT RUN**：

- requeue 后 timeout / signal 的精确竞态；
- waitv 与 requeue 组合、多个 key 同时 wake；
- 旧 shared 队列存活期间物理页反复释放复用的专项动态竞态；B65 已用 backing pin 静态排除
  raw PPN 的错误命中，但 focused LTP 不能替代该竞态压力证明；
- B67 已删除匿名页回收中绕过 backing 引用的 `force_swap_out()`；deep clean 与 shallow
  clean 现在都尊重 queue pin，并在 pin 解除后保留该页的后续回收机会。双架构 MM ktest
  覆盖“被 pin 时保持 identity、解除后重新压缩并 fault-in”；
- 文件 truncate/page-cache invalidate 仍可能替换 file-backed shared futex 的 backing，
  该跨 FS 边界尚未收口；
- 普通 swap/zram/page-cache 回收会因 pin 临时延后，单页生命周期已验证，但长时间、多 waiter
  内存压力量化仍未执行；
- 精确的“WAIT 最后比较与并发 wake”以及“CMP_REQUEUE 比较与并发写/requeue”动态竞态尚未
  用专门 ktest 放大；`futex_cmp_requeue02` 主要覆盖错误路径，不等价于多 waiter 并发交错。
  当前结论来自锁协议静态证明和 focused LTP 功能回归；
- VM 锁长期繁忙时的连续 `Retry` 尚未做 livelock/性能压力；
- nofault 比较完成后若用户线程并发 unmap/remap，该行为属于比较线性化点之后的映射变更；
  仍需结合用户映射生命周期规则做专项动态验证，不能把一次 PTE 校验外推成永久 pin。

设计对照：Linux 当前的
[`futex_q`](https://github.com/torvalds/linux/blob/master/kernel/futex/futex.h)、
[`futex_wait_setup()`](https://github.com/torvalds/linux/blob/master/kernel/futex/waitwake.c)、
[`futex_requeue`](https://github.com/torvalds/linux/blob/master/kernel/futex/requeue.c) 和
[`futex_unqueue_multiple`](https://github.com/torvalds/linux/blob/master/kernel/futex/waitwake.c)。

## 12. 调试核对点

| 现象 | 优先检查 |
|------|----------|
| requeue 后错误超时或目标队列残留 | waiter current key、`Arc::ptr_eq` 清理 |
| wake 后仍返回 `EINTR/ETIMEDOUT` | `woken` 是否先于 runnable 发布 |
| waitv 返回错误下标 | 每项独立 waiter、最后 woken index |
| shared futex 跨进程不醒 | VMA 是否 `MAP_SHARED`、两边是否共享同一 backing `Arc` 与页内偏移 |
| pthread join 卡住 | clear_child_tid 写 0 与 private/shared wake |
