---
title: "MangoCore SMP 锁序与中断上下文约束"
category: architecture
status: proposed
owner: MangoCore Team
last_updated: 2026-08-05
tags: [smp, locking, irq, preemption, scheduler, tlb]
related_docs:
  - "docs/10_plan/smp-8core-implementation.md"
  - "docs/10_plan/smp-agent-execution-spec.md"
  - "docs/01_architecture/boot-and-trap.md"
  - "docs/05_process/scheduler.md"
---

# MangoCore SMP 锁序与中断上下文约束

本文定义 SMP 改造期间的目标锁契约。`status: proposed` 表示这些规则是实施门禁，
不表示当前单核代码已经满足。每个引入或改变锁关系的批次都必须同步本文，并用实际调用链验证。

## 1. 基础原语前置条件

在 AP 允许处理中断或多个 CPU 进入共享内核路径前，先提供统一的 `IrqSaveSpinLock`：

- guard 创建时保存本 CPU 原中断状态并关闭本地中断；
- guard 销毁时恢复它保存的状态，不得无条件开中断；
- 同一 CPU 上嵌套 guard 必须严格按 LIFO 销毁；
- 跨 CPU 互斥由锁本身保证，关本地中断只解决本 CPU 的 IRQ 重入；
- 是否同时增加 `preempt_depth` 由实现模型统一决定，不能把 `irq_depth` 当成正确性的替代品；
- guard 不得跨 context switch、yield、睡眠或远端 ack 等待点。

B39 已在不取得任何普通锁的 hard-IRQ fast path 上开放 AP 本地 timer；deferred AP 分支只
推进 CPU-local tick。`IrqSaveSpinLock` 的完整门禁仍约束 console、设备和其它 IRQ 可达共享
对象，不能因 timer 已开放而外推这些子系统已经多核安全。

## 2. 上下文能力

| 上下文 | 普通锁 | irq-save 锁 | 分配/睡眠 | 等待远端 ack |
|---|---|---|---|---|
| boot BSP/AP park | 仅已初始化对象 | 可以 | 发布堆后才可以 | 仅有界启动握手 |
| hard IRQ / IPI | 禁止 | 仅最小、已证明的叶子锁 | 禁止 | 禁止 |
| 调度/idle 栈 | 可以 | 可以 | 不得在持锁时睡眠 | 释放普通锁后可以 |
| 普通任务/系统调用 | 可以 | 可以 | 释放自旋锁后可以 | 释放 MM/PTE 锁后可以 |
| panic/STOP | 禁止等待普通锁 | 只用 try/raw fallback | 禁止 | 仅有界原子 ack |

IPI handler 只能访问 per-CPU mailbox、固定 shootdown slot 和无锁诊断计数；不得获取
runqueue、task.inner、MM/PTE、timer、VFS、网络或设备业务锁。

## 3. 部分序而非虚假的总序

MangoCore 不采用“给所有锁编号后允许任意嵌套”的总序。以下路径必须拆成
“锁内改变状态—释放—执行下一阶段”，从结构上消除双锁依赖：

1. **唤醒目标态**：统一入口在当前调度锁内裁决
   `Blocking(cpu) -> Running(cpu)` 或 `Blocked -> Queued(cpu)`；拆分 per-CPU
   runqueue 后只锁一个目标队列，释放 runqueue 后才发 IPI。
2. **调度**：task.inner 与 runqueue 不得嵌套；状态转移 API 是唯一调度状态真值来源。
3. **迁移/偷取**：任何时刻只持有一个 runqueue 锁；从 victim 取出后释放，再按 CAS
   结果进入目标队列，失败则回滚到合法状态。
4. **页表失效**：MM/PTE 锁内修改并记录 `MmuGather`，释放锁后本地 flush、发送
   shootdown、等待 ack；ack 完成后才能释放 frame/页表页/ASID。
5. **timer 重编程**：timer queue 锁内更新最早 deadline，释放锁后向 CPU0 发送
   `TIMER_REPROGRAM`；IPI handler 只保留原子请求，不能读取 timer queue。
6. **console**：B55 后正常顺序固定为
   `local IRQ-off -> OUTPUT_LOCK -> LA64 UART Mutex`（RV64 无 UART Mutex）。
   `OUTPUT_LOCK` 是业务层叶子锁；格式化参数应在进入 `print()` 前求值，持有时不得获取
   task/MM/VFS 等业务锁。panic 先 Release 发布单向状态，等待者 Acquire 观察后放弃
   `OUTPUT_LOCK`，直接走不取得两把 Rust 锁的原始 UART/SBI fallback。
7. **lwext4**：跨实例全局锁位于 C 调用外层，保护区内只允许同步块 I/O，禁止
   yield、任务事件等待或调用会反向获取 VFS 高层锁的路径。
8. **线程组退出**：首次发布允许 `thread_group -> 单个 RunQueue` 的短嵌套；
   group-exit 快照释放 `thread_group` 后才取得 task/registry 锁、唤醒或发送 IPI。
   不存在 RunQueue 反向获取 thread-group 锁的路径。

### FS/Net 已实现锁域

FS、网络与调度之间不建立可任意嵌套的全局总序；跨域操作统一拆成
“锁内快照或提交—解锁—进入下一域”。当前实现约束如下：

| 子系统 | 正向锁序 | 约束 |
|---|---|---|
| native ext4 namespace | `rename_gate -> parent dir_gate -> victim dir_gate -> inode_txn` | 跨目录先取全局 rename 门；同类对象按 inode ID 排序，锁后重验目录项 |
| 文件 I/O | `inode_txn -> PageCache.op_gate -> entries/inner -> PageEntry.data` | 用户访问在全部 inode/PageCache 锁外；`PageEntry.data` 不反向取得元数据锁 |
| lwext4 | `PageCache -> LWEXT4_GLOBAL -> fs.lw -> inode state` | C 全局门不可重入，不跨 fault、调度、IPI/TLB ack 或输出锁 |
| 网络控制面 | `PortRegistry/NetDirectory -> socket lifecycle -> one DeviceStack` | N0 只做短提交；一个线程同时最多持有一个设备栈锁 |
| 网络通知 | 释放 DeviceStack 后再进入 EventPoll/WaitQueue | IRQ 只置原子 pending/deferred 标志，CPU0 worker 才执行 smoltcp |

`RouteSocketHandle` 单调且不复用。访问者在目录中确认 route 状态并升级目标栈的
`Weak`，释放目录锁后取得单个栈锁，再用 route ID 与 protocol 重验本地 binding。
端口绑定则以每 netns 的 `reserve -> socket.bind -> commit/abort` 为线性化协议；
registry 锁不跨 socket 或 DeviceStack 操作。

### 3.1 B15 历史过渡约束

B15 尚未拆分 per-CPU runqueue 时，ready/interruptible 容器曾由单一
`TASK_MANAGER` 保护。该实现只用于说明状态机的演进背景，已由 B18 的 3.3 节取代，
不得再作为新增调用路径的锁序依据。

WaitQueue 的 `wake_*` 以 `WaitQueue -> TASK_MANAGER -> 单个 RunQueue` 的单向顺序
调用；反向获取不存在，且该路径不获取 `task.inner`。B20 已把 IPI 通知与容器迁移
分段：锁内只完成 registry/runqueue 交接，释放全部调度锁后才敲远程 doorbell。

B15 新增的 publish、wake、block 和 switch-out 状态迁移都不在 `TASK_MANAGER`
内获取 `task.inner`。当时登记的 nice-aware 锁内读取技术债已在 B18 通过原子
nice/vruntime hint 消除。

### 3.2 B17 Per-CPU current 约束

B17 已把全局 `PROCESSOR` 拆为每个 `PerCpu` 独占的 `CpuTaskState`。本 CPU
processor 锁只保护 current `Arc` 和 idle context；hard IRQ/IPI 不获取该锁，
panic 诊断只能使用 `try_lock()`。

- `current_task()` 在锁内只克隆 `Arc`，返回前释放锁；
- dispatch 先单独取得 `task.inner` 中的 context 指针，再获取 processor 锁发布
  current，禁止形成 `processor -> task.inner`；
- processor 锁必须在 `__switch` 前释放，不能跨 context switch；
- current 槽只能在已回到所属 CPU 的 idle 栈后清空；
- `schedule()`、退出和架构 `noreturn` 路径前必须释放本地 current `Arc`，因为旧
  Rust 栈帧不会被展开。

B17 本身没有引入 runqueue 双锁、远程 enqueue 或任务迁移；其后的 B18 已拆出
per-CPU RunQueue，但仍保持生产任务 owner 为 CPU0。

#### B56 panic 诊断边界

panic 中“读取统计”同样可能隐藏阻塞锁。B56 将 allocator 诊断收敛为：

- heap mutex 只由 `try_heap_stats()` 尝试一次；失败后只读原子 charge/peak；
- frame allocator rwlock 只由 `try_unallocated_frames()` 尝试一次；失败打印 locked；
- processor、task.inner 与 active-MM 槽只允许 `try_lock()`，runqueue 不取锁而读取计数 hint；
- per-CPU IPI/timer/TLB/barrier 字段只作 best-effort 快照，不参与 owner 或资源释放判定。

因此 panic 调用链不得新增普通 `.lock()`/`.read()`，也不得为了补齐 IRQ/preempt 等尚未
建立的不变量而增加仅服务诊断的热路径状态。

#### B57 固定大小 uaccess 边界

固定对象和数组 copy 会 fault，并取得当前地址空间的 VM 写锁。B57 固定以下顺序：

```text
释放 fd table / task.inner / file-private 等普通锁
  -> AddressSpace VM lock
       -> fault-in + PTE 权限后验检查
       -> direct-map raw pointer copy
  -> 释放 VM lock
  -> 如有 PTE 修改，在锁外执行 TLB flush / 远端 ack
```

- 用户 PA 与 raw pointer 只能在同一个 `AddressSpace::write` closure 内取得并使用；
- 不得从 fixed-size helper 返回 `&'static T`、`&'static mut T` 或保存物理页切片；
- 跨页 copy 每页独立持锁，允许部分完成，不能要求 VM 锁跨越整个大数组；
- `sys_ioctl()` 先从 fd table 克隆 file `Arc`，再释放 table；块设备 ioctl 也在 uaccess 前
  释放不参与操作的 file-private guard。

该约束在 B59 已扩展到 `UserBuffer` 和 iovec，但 B57 只迁移了当时删除 API 的 ioctl 调用点。
SysV IPC 等既有 fixed-copy 调用链是否跨 registry 锁仍须在后续 task/process 审计中逐项处理；
不能把 helper 内部映射安全外推为所有调用方锁序均已合格。

#### B58 用户页视图绕过路径收口

B58 不再让字符串和 sockaddr 解析器消费锁外物理页 slice：

- `translated_str()` 每页在 VM 锁内复制到 4 KiB 内核 scratch，释放锁后才扫描 NUL
  并扩容 `String`；堆分配器不进入 VM 临界区；
- `bind/connect/sendto` 通过 `read_sockaddr()` 得到最多 512 字节的内核所有
  `Vec<u8>`，`Endpoint` 只解析这份快照；
- `trans_ref!`/`trans_refmut!` 以及未使用的 raw-pointer sockaddr 解析入口已删除；
- `fault_in_user_range()` 只用于在 clone3/getrandom/mincore 的外部副作用前提前检查。
  预检查后另一 CPU 仍可改变映射，因此真正读写必须再走 `copy_from/to_user`；
- 阻塞 recv 在等待期间只保存内核 buffer，唤醒后再 copy-to-user，不得跨
  WaitQueue 保存用户页视图。

旧 `UserBuffer` 在构造时 fault-in、随后锁外访问物理页的剩余边界已由 B59 取代。

#### B59 VA-backed UserBuffer 边界

B59 删除 `translated_byte_buffer()`、`translate_user_buffer_checked()` 和物理页 slice 表示。
`UserBuffer` 只保存 token、访问方向与连续/scatter 用户 VA 区间，实际传输固定按以下顺序：

```text
锁外预 fault（只服务 ABI 排序）
  -> 每页取得 AddressSpace VM 锁
       -> resolve 当前 PTE，必要时 fault-in
       -> 权限后验检查 + direct-map raw copy
  -> 释放 VM 锁
  -> 下一页或锁外 TLB flush/ack
```

- FS/网络等普通路径必须先释放 inode/socket/file-private 锁，再进入 faultable copy；
- tmpfs 写入先在锁外复制到内核 bounce buffer，再取得 inode 锁；
- TCP 接收先在 socket 锁内写入内核 buffer，释放锁后才 copy-to-user；
- 流式接口首字节失败返回 errno，已有进度返回完成前缀；固定结构使用 exact helper；
- pipe ring 当前使用 `spin::Mutex`，不能在其内进入 fault handler。它只允许调用
  crate-private nofault helper：构造时已在锁外 fault-in，锁内只解析已有 PTE；并发
  `munmap/mprotect` 导致映射变化时立即部分完成或 `EFAULT`，不得持锁等待；
- `fault_in_user_va()` 必须先解析已经满足权限的 PTE，避免正常 copy 重复进入
  CoW/SharedWrite 并提交无效 TLB 刷新。

### signalfd pending 与通知域

signalfd 的 pending 队列是权威状态，`Sighand::signalfd_events` 只负责通知等待者重查。生产者
固定采用以下单向顺序：

```text
task.inner 或 process.signal 锁内提交 pending
  -> 释放 pending owner 锁
  -> 短暂取得 sighand，克隆 EventWaitQueue Arc
  -> EventWaitQueue::notify_events_all
```

禁止持有 `task.inner` 或 `process.signal` 进入 `notify_signalfd()`；也禁止让 EventWaitQueue
回调反向获取 pending owner 锁。普通 fork 创建新 sighand 通知域，`CLONE_SIGHAND` 才共享，
共享的 signalfd File 不能缓存某个进程的队列地址。

B59 只适配完成重构所必需的 FS/Net 调用点，不代表这些共享子系统已通过完整 SMP 并发审计。
Driver 未在本批改动；其余 FS/Net/Driver 审计由对应负责人后续完成。

#### B60 IPC registry 与 faultable uaccess

SysV semaphore 和 POSIX mqueue 的全局表锁保护对象身份与共享状态，不能覆盖用户页访问。
需要先验证对象、再读取用户数据的写命令固定采用：

```text
registry 锁内验证对象、权限并快照固定长度
  -> 释放 registry 锁
  -> 分配内核缓冲并 copy_from/to_user
  -> registry 锁内重验对象、权限与固定长度
  -> 一次提交共享状态
```

- `semctl(GETALL)` 在锁内生成内核快照，释放锁后才写用户数组；
- `semctl(SETVAL/SETALL)` 保留“坏 semid/权限优先于用户指针错误”的首轮校验，copy 后再
  重验；semaphore ID 单调且耗尽后返回 `ENOSPC`，不通过饱和计数复用 ID；
- `GETALL/SETALL` 操作整个集合，按 Linux ABI 忽略 `semnum`；
- `mq_open(O_CREAT)` 仅在首轮确认名称不存在后锁外读取 attr；第二次锁以名称表为
  线性化点，重新处理 `O_EXCL`、容量和同名并发创建；
- `Arc<MqQueue>` 固定对象生命周期，权限检查在名称表解锁后单独取得 queue inner；创建
  回滚只允许删除 `Arc::ptr_eq` 的本次对象，不能误删 unlink 后重建的同名队列。

B60 的结论只覆盖 `syscall/process/ipc.rs` 的 registry/queue 与用户访问锁序；不覆盖
FS/Net/Driver，也不宣称所有 IPC 阻塞领取协议已完成多核语义审计。

#### B61 SysV 消息的唯一摘取

普通 `msgrcv` 不能把“选择消息”和“删除消息”拆成两个 `MSG_REGISTRY` 临界区。旧实现允许
两个 CPU 在第一段同时复制同一条消息，随后只有一个删除成功，但两个 syscall 都向用户返回
成功。B61 固定为：

```text
MSG_REGISTRY 锁内选择消息
  -> 普通接收：VecDeque::remove(idx)，同步更新 cbytes/lrpid/rtime 并唤醒等待者
  -> MSG_COPY：复制内核快照但不修改队列
  -> 释放 MSG_REGISTRY
  -> copy_to_user
```

普通分支的 `remove(idx)` 是领取线性化点，返回的 `Vec<u8>` 已由当前 CPU 独占，不再需要
消息 serial 或第二次删除。锁外用户 copy 失败时消息仍保持已消费状态，这与 Linux 的
`msgrcv` 所有权交接一致。该规则只证明单条消息不会被重复领取；IPC ID 复用、WaitQueue
对象删除竞态和精确的双 receiver 动态压力仍需后续独立审计。

#### B62 SysV message queue 对象身份

阻塞的 `msgsnd/msgrcv` 会释放 `MSG_REGISTRY` 并等待，因此数值 `msqid` 必须在整个等待
区间保持身份稳定。若 `IPC_RMID` 后立即把最小空洞分给新队列，旧 waiter 醒来可能把新对象
误认为原对象。B62 的发布协议固定为：

```text
MSG_REGISTRY 锁内选择 requested ID 或自动 cursor
  -> 确认该 ID 从未发布且当前未占用
  -> 为 published_ids 预留容量并登记历史
  -> 插入 queues，完成对象发布

IPC_RMID：从 queues 删除 -> wake_all；删除路径不分配
```

自动 cursor 必须跳过显式 requested ID 留下的稀疏历史，整数耗尽返回 `ENOSPC`；请求值
无效、已占用或曾发布时只回退自动分配，不能重新使用旧身份。`was_removed(id)` 由“已经
发布且当前不存在”定义，因此旧 waiter 只能观察 `EIDRM`。v1 采用线性 `Vec` 历史换取简单
的可失败预留；若以后改为 index+generation allocator，仍必须保持“先登记身份、后发布
对象”和“删除不分配”两个不变量。

#### B63 SysV semaphore 与 shared-memory 对象身份

semaphore 不需要复制 message queue 的发布历史。`sem_wait_condition()` 是私有 helper，
唯一调用者已经在同一把 `SEM_REGISTRY` 锁下证明 `semid` 存在且操作需要阻塞；ID 又只按
`checked_add()` 单调前进。因此后续锁内查找失败只能是 `IPC_RMID`，可直接返回 `EIDRM`：

```text
首次查找缺失 -> EINVAL
首次查找存在且必须等待 -> 注册 WaitQueue
等待期间再次查找缺失 -> IPC_RMID -> EIDRM
```

删除路径只移除 set 并 `wake_all`，不得为 tombstone 临时分配内存。否则 OOM 会让删除历史
记录失败，使旧 waiter 错误返回 `EINVAL`。

shared-memory 没有同类 waiter，但 `shmat` 会跨越 registry 解锁建立 VMA，再重锁按 `shmid`
登记 attachment。`ShmRegistry` 因此也必须以 `Option<i32> + checked_add()` 保证 ID 不回绕；
耗尽返回 `ENOSPC`，不能用饱和加法重复返回最后一个 ID 并覆盖活段。VMA clone 的 frame
`Arc` 负责物理页寿命，attachment 重验失败后的 `munmap`/TLB shootdown 必须位于 registry
解锁后。

#### B64 futex waiter 身份与 requeue 线性化

通用 `WaitQueue` 只保存任务，无法表达一个注册项被 requeue 后的当前位置。futex 因此使用
专用的 `FutexTable -> FutexQueue -> Arc<FutexWaiter>`；同一个 Arc 同时由 syscall 和队列
持有，撤销必须用 Arc 身份精确匹配，不能只匹配 TCB。

`FutexTable` 外层锁是 enqueue、wake、requeue、timeout/signal cleanup 的唯一线性化点：

```text
requeue: source.remove -> waiter.key = target -> target.publish
wake:    queue.remove -> waiter.woken = true -> wake_interruptible(task)
cleanup: waiter.key -> exact Arc remove
```

- target 队列可见前必须先更新 waiter 的 current key；
- 任务 runnable 前必须先发布 `woken`，等待方以 Acquire 读取；
- source membership 消失不能表示正常 wake，因为 requeue 也会产生相同现象；
- waitv 每个数组项拥有独立 waiter，多个项被唤醒时返回 Linux 定义的最后一个下标；
- 注册后的恢复路径不能重读最初 futex word 判定 wake，requeue 后该 word 已不是权威状态；
- 锁顺序固定为 `FutexTable -> TASK_MANAGER -> 单个 RunQueue`，反向获取禁止；
- `block_current_and_run_next_with_lock_checked()` 只在提交阻塞状态时接管并释放 table guard，
  任何路径都不得跨 context switch 持有该锁。

B64 的 requeue 身份证明不覆盖 shared backing 生命周期，也不覆盖锁内用户访问；前者已由
B65 收口，后者已由 B66 的 locked nofault 注册协议收口。

#### B65 shared futex backing 身份与 pin

shared futex 不能用 raw PPN 充当长期 key。旧页被释放后，分配器可能把同一 PPN 交给无关
新页，使新进程错误命中旧等待队列。B65 改用共享映射实际持有的 `Arc<FrameTracker>` 对象
身份与页内偏移：

```text
持 AddressSpace VM 锁
  -> 确认 VMA 为 MAP_SHARED
  -> clone resident backing Arc
  -> 校验 backing.ppn == PTE.ppn
释放 VM 锁
  -> 获取 FutexTable 锁
  -> 以 (Arc identity, page offset) 查找或发布队列
```

- `FutexQueue` 为每个非空 shared key 保留一份 backing pin；队列为空并从 map 删除后才 drop；
- requeue 先建立/验证目标 pin，再更新 waiter current key，最后把 waiter 发布到目标队列；
- waiter 的 backing identity 与 word offset 分存两个原子字段，但它们从不构成无锁二元组；
  `queue_key_locked()`/`move_to_locked()` 的调用者必须持有同一 `FutexTable` 外层锁；
- `clear_child_tid` 在 VM 锁内取得 fault 前后稳定 key，退出 VM 临界区后才依次执行 wake；
- `AddressSpace` 与 `FutexTable` 没有嵌套锁序；`Arc` 是跨阶段的稳定所有权载体。

该协议排除了 raw PPN 复用造成的 false-positive。B67 又删除匿名页回收中绕过引用计数的
`force_swap_out()`：deep clean 统一尊重 backing pin，`SharedPage` 候选放回队尾，pin 解除后
仍可回收。文件 truncate/page-cache invalidate 仍是独立的跨 FS backing 生命周期边界。

#### B66 futex 锁内 nofault 注册

futex 的最后一次值比较必须和 waiter 发布共用 table 锁，才能关闭 lost-wake 窗口；但
`UserPtr::read()` 可能取得 VM 锁、处理缺页和分配内存，不能发生在自旋锁内。B66 固定为：

```text
锁外 faultable 读取并比较 word
  -> 锁外解析当前 key
  -> 锁外分配 waiter、克隆 VM Arc
  -> FutexTable 锁
       -> AddressSpace VM try_lock
       -> shared backing + PTE/权限 + u32 nofault 复查
       -> 同一 table 临界区 enqueue
  -> 解锁并阻塞
```

- `FutexTable -> AddressSpace` 是**条件式非阻塞边**，不是普通嵌套锁顺序：只允许一次
  `try_lock`，失败立即释放 table，再从 syscall 外层重新 fault-in 和解析 key；
- VM `Arc` 在 table 锁外从 PCB clone，table 临界区不获取 `process.inner`；
- locked nofault 检查不能只比较 PPN。shared 条目同时要求当前 backing 与预解析 backing
  `Arc::ptr_eq`，并再次验证 VMA resident frame、PTE 和读权限；
- `Retry` 只允许在任何 waiter 发布前返回，不是用户可见 errno；普通 WAIT 的相对 timeout
  只转换一次绝对 deadline，重试不得重新计时；
- waitv 描述符只快照一次，但每次 Retry 都重读全部 word、重算全部 key；waiter 数组必须在
  table 锁外分配，全组 nofault 比较成功后才在同一临界区发布；
- 不允许入队后再做第三次用户读取。wake 若先完成状态写，锁内比较会观察到新值；wait 若先
  比较并发布，wake 取得同一 table 锁后会找到 waiter。

这条例外只解决“最后比较 + enqueue”的原子性与 faultable-uaccess 锁序。nofault 比较之后
发生的并发 unmap/remap、文件 truncate backing 替换，以及 VM 锁长期繁忙时的重试公平性
仍是独立边界，不能写成 B66/B67 已动态证明。

#### B68 futex compare 与 requeue 原子性

`FUTEX_CMP_REQUEUE` 不仅要稳定两个 key，还必须让 source word 的比较与队列修改构成同一
线性化操作。若在 table 锁外比较，另一 CPU 可在比较后改写 source，而旧请求仍会错误地
wake/move waiter。B68 复用 B66 的条件式锁边：

```text
锁外 fault-in source/target，并解析两个 key
  -> FutexTable 锁
       -> AddressSpace VM try_lock
       -> shared 两端复核 backing + PTE；private CMP 复核 source PTE
       -> nofault 读取并比较 source word
       -> 同一 table 临界区 wake/requeue
  -> 解锁
```

- `FutexTable -> AddressSpace` 仍只允许 `try_lock()`；锁忙或映射变化必须在队列尚未修改时
  返回内部 `Retry`，释放 table 后完整重算两端 key；
- 普通 private REQUEUE 不读取 source，其 key 是当前 MM 内的 VA，因此不新增 source PTE
  前置条件；CMP private 必须读取 source，不能跳过 nofault 复查；
- shared 的 source/target 都要复核 backing 对象身份和 PTE，不能只比较 PPN；
- compare 成功后不得释放 table 再调用 requeue，也不得在部分 wake/move 后返回 `Retry`；
- 该锁协议提供静态线性化证明；focused LTP 不会精确制造多核 compare/write/requeue 交错，
  因而专项动态竞态仍标记为 NOT RUN。

### 3.3 B18 Per-CPU RunQueue 约束

B18 删除全局 runnable 容器。每个 `CpuTaskState` 独占一个 `RunQueue`，其锁只保护
该 CPU 的 `Queued(cpu)` 成员关系和 nice 快速路径计数；`nr_running` 只是排队任务数的
无锁近似值，不包含 current，也不替代锁内成员关系。

- publish、fetch、yield 后重新入队只获取一个 owner runqueue；
- nice-aware 选择只读取 TCB 的原子 nice/vruntime hint，不在 runqueue 锁内获取
  `task.inner`；
- Blocked 唤醒先持有 `TASK_MANAGER` 从 interruptible registry 移除任务，再按
  `TASK_MANAGER -> 单个目标 RunQueue` 提交 `Blocked -> Queued(cpu)`；
- 批量移除也采用同一方向，并逐个定位 owner；任何时刻不得同时持有两个 runqueue；
- 从 runqueue 撤回的 `Arc` 必须先释放队列锁，再执行 drop；
- B18 当时仍固定目标为 CPU0；B37 已用 affinity-aware 通用放置取代该历史限制，
  但远程 enqueue 的唯一 owner 仍由 runqueue 锁和任务状态确立。

### 3.4 B19 AP 调度与内核栈发布约束

B19 只为 focused ktest 的 kernel-only 任务开放显式目标 CPU，不改变普通任务的 CPU0
策略。其跨核发布顺序固定为：

1. CPU0 在 `KERNEL_SPACE` 锁内建立动态 kernel stack 映射并释放锁；
2. 不持有 MM/PTE/runqueue 锁发送 `KERNEL_TLB_SYNC`，等待目标本地 flush ack；
3. ack 完成后只锁目标的一个 runqueue，提交 `New -> Queued(cpu)` 并释放锁；
4. 最后发送 `RESCHEDULE` doorbell，IPI handler 只置位；AP idle 或运行中用户任务的
   trap-return 安全点随后消费，不在 hard IRQ 内 fetch。

AP 安装页表根时可以短暂取得 `KERNEL_SPACE` 锁；此时 CPU0 只在 scheduler-ready
屏障等待且不持锁。AP dispatch 前只锁自己的 runqueue；`dispatch_task()` 先后取得
`task.inner` 和本地 processor，但两把锁不嵌套，也不跨 `__switch`。任务切回 idle
后先释放 processor 锁，再把 Zombie 加入 owner CPU 的 `local_zombies`，因此不需要
获取全局 `TASK_MANAGER`。

这个批次没有两个 runqueue 的同时持有、迁移或 work stealing。AP 任务入口也不得
访问尚未审计的 console、FS、NET、设备和用户 MM；这些能力约束不能用锁本身替代。

### 3.5 B20 远程 blocked wake 约束

B20 不新增调度状态。`last_cpu` 只记录最近一次成功 fetch 的 CPU；B31 又用
`cpus_allowed` 约束哪些 CPU 可以取得 owner。任务真正阻塞后，统一 wake 入口按以下
顺序重新发布：

1. 持有 `TASK_MANAGER`，确认状态为 `Blocked` 并从 interruptible registry 移除；
2. 计算 `cpus_allowed & online & scheduler & !stopped`，对候选 CPU 的
   `nr_running + current_present` 做无锁负载估算；`last_cpu` 合法且不高于最小负载 `+1`
   时优先保留局部性，否则选最小负载，同负载选最低 CPU 编号；
3. 在 `TASK_MANAGER -> 一个目标 RunQueue` 锁序下提交 `Blocked -> Queued(target)`；
4. 释放目标 RunQueue，再释放 `TASK_MANAGER`；批量路径只保留目标 CPU bitmask；
5. 外层排除本 CPU 后发送 `RESCHEDULE`，IPI handler 只置 per-CPU 原子提示；目标在 AP
   idle 或用户 trap-return 安全点消费。

`Blocking(cpu)` 的提前 wake 仍只恢复 `Running(cpu)`，不入 runqueue、不发 IPI；idle
侧随后把它重新排入本地队列。批量 wake 每次调用 `enqueue_woken()` 都在函数返回前
释放该目标队列，因此循环不会同时持有两个 runqueue。当前该远程能力只对受控
kernel-only AP 任务完成验证。初始 affinity 已作为入队硬约束，B34 的本地 current 写侧
不持 task.inner/runqueue 锁完成目标选择和内核栈同步，发布 mask/target 后立即进入既有安全点。

B35 复用同一 `TASK_MANAGER` 锁串行化稳定 Blocked 线程的 affinity 与 wake：写侧必须在锁内
同时确认精确 `Blocked` 状态和同一 TCB 指针仍在 registry，随后 Release 发布 mask；wake 取得
同一锁后以 Acquire 读取并选择目标。只检查状态不够，因为 wake 会在同一锁域把任务移出
registry 并发布新 owner。退出不会直接执行 `Blocked -> Zombie`：group-exit、exec 和 fatal
signal 必须先唤醒目标，再由目标在自己的 `Running(cpu)` 安全点退出。该路径不获取 runqueue，
也不搬 owner；远程 Running/Blocking 修改在 B35 当时尚未实现，后续由 B38 的请求槽协议闭合。

### 3.6 B36 稳定 Queued affinity 搬队约束

B36 不为 queued 搬队同时锁定源/目标 runqueue。顺序固定为：

1. 不持调度锁选择合法目标，并完成目标 kernel-stack TLB 同步；
2. 只锁 source，复核 `Queued(source)` 和精确 TCB 成员关系，提交
   `Queued(source) -> Migrating` 后摘除节点、释放 source；
3. `Migrating` 的同步调用方 Release 发布新 mask；
4. 只锁 target，提交 `Migrating -> Queued(target)` 并插入节点、释放 target；
5. 所有队列锁释放后才发送 RESCHEDULE。

`Migrating` 后禁止获取 `TASK_MANAGER`、等待 IPI/TLB ack 或进入析构；因此退出清理
即使在 `TASK_MANAGER` 内短暂等待搬队完成，也不存在反向依赖。nice 更新读到
旧 owner 时，必须先在旧队列锁内校准派生计数，再按最新状态重新定位。`Queued(cpu)` 状态下
若同一 TCB 不在该 owner 队列，且该队列锁仍由检查方持有，应 fail-stop；不能把真实容器损坏
误判为迁移，因为迁移回该 CPU 同样必须先取得这把锁。

### 3.6.1 B37 affinity-aware 通用放置约束

B37 把新任务发布、Blocked wake 和 current 自迁移的目标选择收敛到同一函数。
选择器只依赖 per-CPU 原子提示，不依赖 processor 锁：

- `nr_running` 只近似表示已排队数，`current_present` 只近似表示 current 槽非空；
- 两个值只影响放置质量，不证明任务 owner，也不取代目标 runqueue 锁内的
  affinity/状态复核；
- `current_present` 在 current 槽安装后 Release 置位，在 idle 栈取回 current 后
  Release 清位；读侧用 Acquire 取样；
- 选择器不获取 `TASK_MANAGER`、processor 或 runqueue 锁，不等待 IPI/TLB ack，
  因此可在既有 `TASK_MANAGER -> 单个 RunQueue` 顺序中使用；
- BSP 在 scheduler-ready mask 发布前创建 init/ktest runner 时显式放到 CPU0，
  这是启动时序例外，不是普通任务的隐式回退。

### 3.6.2 B38 远程 Running/Blocking affinity 锁序

B38 不增加调度状态，而是在 TCB 中增加受锁的单个
`remote_affinity_request` 槽。该槽不是 owner 容器；任务切回 idle 前仍保持
`Running(source)`，切栈后才直接交给 `Queued(target)`。固定锁序为：

```text
task.inner -> remote_affinity_request -> 单个 RunQueue
TASK_MANAGER -> remote_affinity_request -> 单个 RunQueue
```

具体约束：

- `exit_thread_resources()` 可在持有 `task.inner` 时调用 `mark_zombie()`，因此
  `task.inner -> remote_affinity_request` 是显式锁序；不存在请求槽反向获取
  `task.inner` 的路径；
- `begin_interruptible_sleep()` 由外层持有 `TASK_MANAGER`，再获取请求槽，
  在同一临界区完成 `Running -> Blocking` 和旧请求 Retry；
- 源 idle 的 `finish_switch_out()` 持有请求槽，再由
  `requeue_after_switch()` 短暂取得一个 target runqueue；直到
  `Running(source) -> Queued(target)` 完成后才发布 Applied；
- runqueue 入口不获取请求槽，因此没有 `RunQueue -> remote_affinity_request`
  的反向路径；
- target kernel-stack TLB 同步必须在获取请求槽前完成；IPI 发送、
  请求方协作式 yield 和 context switch 也都发生在解锁后；
- `RemoteAffinityRequest::complete()` 只做单次 CAS 和 Release 发布，不获取
  WaitQueue/`TASK_MANAGER`，所以可在上述短临界区中调用；请求方用 Acquire
  读取 Applied/Retry。

远程写侧看到 `Blocking` 时不取上述任何锁，而是协作式让出 CPU，
等待状态稳定为 Running、Blocked 或 Zombie 后重试。两个并发写侧会通过
单槽串行化，但 B38 focused 只动态验证单请求方；多写侧压力仍是后续门禁。

### 3.6.3 B39 Per-CPU tick 与全局 timer owner

每个 `PerCpu` 独占 `sched_tick_deadline_ns`，只有所属 CPU 在关中断安全点推进。硬件
timer 到期时只静默本地 one-shot、置 `timer_pending` 并返回；hard IRQ 不取得
`KERNEL_TIMER_QUEUE`、runqueue、task 或网络锁。

CPU0 是全局 kernel timer queue 的唯一执行者：

1. 任意 CPU 在 queue 锁内插入动作并计算它是否成为最早 deadline；
2. 释放 queue 锁后，CPU0 可直接重编程本地硬件；AP 则先 Release 置
   `timer_reprogram_requested`，再发送 `TIMER_REPROGRAM`；
3. CPU0 安全点 Acquire 消费 timer/reprogram 标志，短暂取得 queue 锁弹出到期项后立即解锁；
4. callback、timeout/timerfd 和网络 poll 均在锁外执行，最后按最新 queue deadline 与
   CPU0 本地 tick 的较小值重编程；
5. AP deferred 分支不取得全局 timer queue，只推进本地 tick 并重编程自己的 one-shot。

性能计数器可以由 AP 原子累加，但格式化快照会读取 FS/net 全局诊断状态并输出 console，
因此 `print_snapshot` 及 timer/scheduler 周期快照在共享子系统完成 SMP 审计前只允许 CPU0
执行。不能因计数器本身是 atomic，就把整个诊断调用链视为 IRQ-safe 或 SMP-safe。

直接发布 reprogram 标志是为了覆盖 CPU0 以 IRQ-off 状态轮询 idle 的窗口；IPI doorbell
用于尽快打断用户/内核执行。多个请求可以合并，因为 queue 保存权威绝对 deadline，安全点
每次都重新读取最早项。该协议不提供任意内核点抢占：长 syscall 中到达的 timer/IPI 仍等到
既有任务安全点才执行 callback 或切换。

### 3.6.4 B40 group-exit 门禁与 stop ack

首次发布固定采用：

```text
远端内核栈同步
  -> thread_group
       -> 一个目标 RunQueue：成员登记 + New -> Queued(cpu)
  -> 解锁
  -> RESCHEDULE IPI
```

group-exit 固定采用：

```text
thread_group：发布退出码 + 克隆 live 成员 Arc
  -> 解锁
  -> 逐任务短持 task.inner 投递 SIGKILL
  -> TASK_MANAGER/单个 RunQueue 唤醒 Blocked
  -> 解锁后聚合发送 RESCHEDULE
```

`sleep_interruptible()` 的登记后复查只读原子 group-exit/exec 快照，不取得
thread-group 锁，因此不会形成 `thread_group <-> TASK_MANAGER` 环。退出线程在没有上述锁时完成 user-memory/TLB
清理，最后以 AcqRel live-thread 递减发布 ack；观察到 1→0 的唯一线程才执行 PCB/MM
收尾。任何 ack、IPI 或 context switch 等待点都不持有 thread-group、task.inner、
TASK_MANAGER 或 runqueue 锁。

### 3.6.5 B41 exec 会话与 Completion

exec 的临时门禁固定采用：

```text
构造未发布的新 AddressSpace
  -> thread_group：安装 ExecSession + 克隆 live sibling Arc
  -> 解锁
  -> 逐 sibling 投递 SIGKILL/wake/RESCHEDULE
  -> 释放快照 Arc
  -> Completion 等待 sibling 清理资源并离开 current 槽（不持任何内核锁）
  -> owner 独占旧 MM 后安装新映像
  -> thread_group：清除 ExecSession 并重新开放 clone
```

关键约束：

- `publish_thread()` 只在持有 `thread_group` 时同时检查永久 group exit 和临时 exec，
  因此成员登记、`New -> Queued` 与关门操作具有单一线性化顺序；
- `remove_thread()` 在用户资源撤销和 TLB flush/ack 完成后才以 AcqRel 递减 live
  count。该 ack 只证明线程不再使用用户资源，不证明它已离开自身内核栈和 CPU current 槽；
- idle 收尾先撤销 current 槽，再由 `publish_exit_inactive()` 递减
  `ExecState.pending_inactive`。计数到零时只在 `thread_group` 内克隆
  Completion，解锁后才 `complete()`；
- exec owner 必须同时观察到 `live_threads == 1` 和 `pending_inactive == 0`。
  前者保护 MM/资源生命期，后者保证非 leader exec 不会在旧 leader 仍使用
  内核栈时交换 TID；
- exec owner 的等待、IPI/TLB ack 和 context switch 都不持有 `thread_group`、
  `TASK_MANAGER`、task.inner 或 runqueue；
- WaitQueue 协议在提交 Blocking 前后都复查生命周期停止请求，先摘除 waiter 再返回
  `Interrupted`；调用层释放 syscall 栈上的 `Arc` 后才进入安全点；
- 任何 noreturn 退出调用都必须在 context switch 前显式释放当前内核栈上的
  TCB `Arc`；调度切换不会展开已废弃的 Rust 栈帧；
- vfork child 已经 publish 后，父线程被生命周期请求中止只能返回 `StopCaller`，
  不能调用 unpublished cleanup。
- `reset_exec_resources()` 只在 live count 为 1 后运行。它在 `process.inner` 内读取
  fd table/sighand 的共享状态和快照，释放锁后再复制、关闭 CLOEXEC 与重置信号；
  重新取得锁只用于安装最终对象；
- 被替换的 futex table 必须先移出 `ProcessInner`，释放 `process.inner` 后再析构。
  `FutexTable` 析构会释放 waiter、Weak 和容器存储，禁止让这条 allocator/drop 链在进程锁内执行。

永久 group exit 可以在 exec owner 等待期间发布。安全点优先消费永久退出码；owner
醒来后放弃新映像并清除临时会话，但永久发布门仍保持关闭。

### 3.6.6 B43 exec 身份接管

非 leader exec 的身份更新固定采用：

```text
exec.finish()：释放 thread_group 锁并重新开放 clone
  -> task registry：校验 owner/旧 leader，交换 TidHandle，重键 weak entry
  -> 解锁
  -> 析构旧 leader 临时 Arc 和被替换的 TidHandle
  -> Per-CPU current TID
  -> OOM active tracker
  -> 释放 owner 的额外 thread quota
```

关键约束：

- live count 已在安装新映像前收缩为 1，且所有 sibling 均已发布 inactive ack，
  因此 `exec.finish()` 后不再存在可与身份接管并发
  发布的同 PCB sibling；身份交换不需要嵌套 `thread_group` 和 task registry；
- registry 锁内可以短持单个 TCB 的 `tid_handle` 锁，但不得析构 TCB、`TidHandle`，
  也不得取得 processor、`TASK_MANAGER` 或 runqueue 锁；
- `TaskControlBlock::Drop` 只在“TID 键仍指向当前 TCB”时删除 registry 项。旧 leader
  迟到析构时即使数值已经交换，也不能删除新 leader 的 PID 项；
- processor current hint 与 OOM tracker 是 TID 派生索引，只能在 registry 事务完成
  后更新；这段路径不包含 context switch、IPI ack 或其它等待点。

### 3.7 B21 内核栈退休与 shootdown 锁序

TCB 最后一个 `Arc` 可能在 `wait`/进程锁保护区内消失，因此 `KernelStack::drop` 不能
直接取得页表锁或等待远端 CPU。缓存未满时它只把仍保持映射的 slot 放回
`KSTACK_CACHE`；缓存溢出时只短暂取得固定容量 `KSTACK_RETIRE_QUEUE` 并登记 slot，
两把锁不嵌套。

CPU0 idle 调度循环在尚未取得 processor、runqueue 或子系统锁时按以下顺序回收：

1. 取得退休队列锁弹出一个 slot，并立即释放队列锁；
2. 在 `KERNEL_SPACE` 锁内摘下 mapping、清除 PTE，但继续持有其中的 frame；
3. 释放 `KERNEL_SPACE` 后发送 shootdown，并在不持普通锁时等待 ack；
4. ack 完成后释放 frame；最后单独取得 slot allocator 锁归还 ID。

等待窗口临时开中断只用于让本 CPU 响应并发 IPI；hard timer 仍遵守 deferred 协议，不能
在 MM 层直接执行 timer callback。当前退休队列由 CPU0 生命周期路径消费；未来若允许 AP
并发完成普通进程回收，需要重新审查容量、所有者和批处理策略。

### 3.8 B22/B23/B51/B52/B53 用户 MM 驻留与 shootdown 锁序

B22 的 trap-return 激活登记、B23 的 PTE 修改侧和 B51 的切离登记由同一个
`AddressSpace` 串行化：

1. 激活侧在 VM 锁内先把 CPU 加入 `active_cpus`，再读取 generation；落后时完成
   本地全用户失效并更新 observed，最后重查 generation；
2. 修改侧在同一 VM 锁内通过 `UserMapper` 修改 PTE，由 `MmuGather` 记录失效范围和
   退休 frame；`seal()` 推进 generation、校验 active CPU mask 快照并生成 `TlbFlush`；
3. 修改侧释放 VM 锁后，`TlbFlush::execute()` 才执行本地失效、发送 IPI/RFENCE、
   等待远端 ack；B52 的固定 slot 只携带 ASID、起始 VPN 和不超过 64 的页数，handler
   扫描固定 8 个槽且不获取普通锁。B53 为生产 range 请求再携带同步借用期内有效的
   MM context/generation，handler 固定按“精准失效 → observed → ack”发布；跨度更大时
   仍走 `USER_TLB_SYNC` 全刷并由发送方在同步返回后记账；
4. 任务已经切回 idle 栈后，切离侧在改变 current/runqueue owner 前执行完整屏障，
   再在 VM 锁内清除本 CPU active bit；
5. 全部目标 ack 后才 drop retired frame。错误路径也必须保留这一顺序，不能退回
   “清 PTE 后立即释放”。

`read()` 只向闭包提供不可变引用；`write()/try_write()` 在锁内调用
`MmuGather::seal()` 取得 `TlbFlush`，再由块作用域析构 guard。这个接口不暴露可变
guard，是“先解锁再等 ack”的类型级门禁，不依赖每个调用点人工记住 `drop()`。

B86 把同一边界继续下推到架构页表实现：物理页的原始 PTE 视图分为只读
`get_pte_array()` 和可写 `get_pte_array_mut()`，两者均为 crate-private `unsafe`；只读 walk
不得再先制造 `&mut PTE` 后降级为共享引用，写 walk 则必须持有 `&mut PageTable`。这里
`unsafe` 只证明物理页类型和存活期，`&mut PageTable` 与外层 VM 锁才共同证明独占修改权。
因此禁止恢复任何 `&PageTable -> &mut PTE` 的辅助函数，即使当前调用点“碰巧持锁”。

禁止在 VM 锁内等待 user-TLB ack。目标 CPU 可能已经关闭本地 IRQ并在 page fault 中等待
同一 VM 锁；发起者若持锁等它处理 IPI，会形成 `VM lock -> ack -> target VM lock` 环。
等待者临时开放 IRQ只能解决“两个无锁等待者互相成为 IPI 目标”，不能修复持普通锁等待。

`active_cpus` 与 `generation` 是不同 Atomic；各自的 Acquire/Release 不自动组成完整的
join/leave-vs-update 顺序。fixed slot 中 observed-before-ack 只消除 handler 返回时的重复
补刷，不替代这把锁的 enter/leave-vs-update 线性化。当前正确性来自共同 VM 锁，不来自对跨原子传递的猜测。
若 writer 在 leave 前取快照，它会包含该 CPU 并等待 ack；若 leave 先完成，writer
不再发送 IPI，但仍推进 generation，CPU 下次 enter 时必须补刷。若未来要把激活、切离
或目标快照改成 lockless，必须重新证明这两种次序，不能只增强某一个 Atomic 的内存序。

### 3.9 B44 membarrier 锁序

PRIVATE_EXPEDITED 的注册状态属于 `AddressSpace`。目标选择和 CPU enter/leave 沿用
B22/B23/B51/B52/B53 的 VM 锁，而远端同步固定发生在解锁后：

```text
lock VM -> snapshot active CPU mask -> unlock VM
        -> pre full fence -> publish request -> IPI/fence/ack -> post full fence
```

快照先于新 CPU 激活时，新 CPU 在同一 VM 锁之后执行 enter full fence；激活先于快照时，
该 CPU 已进入 mask 并收到 IPI。CPU 若先完成 leave，切离 full fence 已提供有序点，因而
无需继续留在目标集合。IPI handler 只读取本 CPU request、执行 fence 并 Release 发布 ack，
不分配、不取普通锁。等待复用通用 `IpiWaitIrqGuard`，调用方不得持有 VM、runqueue、
task.inner 或其它普通锁。

### 3.10 B45 trap context 借用边界

trap context 页由对应 TCB 拥有，Rust 可变访问只能通过
`TaskControlBlockInner::trap_context_mut(&mut self)` 完成。返回引用的生命周期绑定到
`task.inner` guard；禁止把直映区指针包装成 `'static mut`，也禁止 current-task helper
从临时 guard 中返回引用。

B87 删除了 `PhysAddr::{get_ref,get_mut,get_bytes_ref,get_bytes_mut}` 和
`PhysPageNum::get_mut`：这些安全函数无法证明任意物理内存的类型、存活期或独占权，却能
返回可逃逸的 `'static` 引用。trap context 的 raw pointer 解引用改为只存在于上述 TCB
owner 方法，unsafe 注释分别证明 frame 存活、页首对齐和 `&mut self` 独占。整页 byte view
仍涉及 MM/PageCache/FS 的共享所有权，必须作为独立审计处理，不能借本节点顺带修改。

LoongArch 用户未对齐访存固定分为：

```text
task.inner：快照 PC/store 源寄存器
  -> 解锁
  -> 用户指令和数据 copyin/copyout
  -> task.inner：校验 PC、提交 load 结果并推进 PC
```

用户访存可能缺页并进入 MM/TLB 同步，不能跨越它持有 `task.inner`。trap return 最后把
用户 trap context 地址交给汇编是明确的 owner 边界：Rust guard 已释放，当前任务仍由本
CPU current 槽独占，汇编立即恢复并离开内核。

### 3.11 B46 sigreturn 恢复锁序

`sys_sigreturn()` 固定分为三段：

```text
task.inner：快照用户 SP
  -> 解锁
  -> UserPtr 读取 sigmask / machine context / 架构扩展
  -> task.inner：一次提交用户寄存器与 sigmask
```

当前线程在 syscall 内仍是 live trap frame 的唯一执行 owner。远端信号只追加 pending；
exec、group-exit 和 affinity 请求由 owner 在返回安全点消费，不会越过锁改写 trap
frame。因此锁外 user read 不要求再增加 trap generation 或第二套状态机。全部读取成功
后才提交，畸形 frame 不会留下部分恢复状态。

信号 ABI 上下文只能通过架构 `machine_context()`/`set_machine_context()` 做字段复制，
禁止把 `TrapContext` 裸指针 cast 成 `MachineContext`。错误路径进入 noreturn 退出前必须
先释放当前函数额外持有的 task `Arc`。

### 3.12 B47 signal frame 投递锁序

自定义 handler 的 frame 投递固定分为：

```text
task.inner + sighand：取 pending、复制 action、复位 SA_RESETHAND
  -> 释放 sighand
  -> task.inner：快照返回上下文、mask 与 frame 布局
  -> 释放 task.inner
  -> UserPtrMut 写完整 SigInfo + UserContext
  -> task.inner：提交 handler 用户寄存器与 mask
```

用户 frame 写入可能缺页、CoW 或等待 TLB shootdown，因此该段不得持有 `task.inner`、
`sighand` 或其他普通内核锁。写成功前不发布 handler PC；写失败时直接退出，不需要回滚
半提交的 live trap context。

当前任务仍由本 CPU current 槽唯一执行，只有 owner 会写 live trap frame。远端信号只
追加 pending；exec、group-exit 和 affinity 请求在 owner 的安全点生效。因此该锁外
写入不需要新增 trap generation 或投递状态机。

### 3.13 B48 signal syscall 用户访存锁序

`sigaction()` 的 disposition 是进程共享状态，固定顺序为：

```text
UserPtr 读取可选新 action
  -> sighand：快照旧 action、提交新 action
  -> 解锁
  -> UserPtrMut 写回旧 action
```

`sigprocmask()` 和 `sigaltstack()` 修改当前线程状态，固定顺序为：

```text
UserPtr 读取可选新值
  -> task.inner：快照旧值、校验并提交新值
  -> 解锁
  -> UserPtrMut 写回旧值
```

任一 `UserPtr`/`UserPtrMut` 访问都可能缺页、触发 CoW 或等待 TLB shootdown，不能位于
`sighand` 或 `task.inner` 临界区内。共享 action 的快照和替换必须位于同一个
`sighand` 临界区；线程 mask/altstack 则由 current owner 与 `task.inner` 共同保证
一致性，不新增事务对象或状态机。

这不是可回滚事务：输入失败或校验失败发生在提交前；提交成功后的旧值 copyout 若返回
`EFAULT`，已提交状态保持不变。输入、输出指针别名时必须保持“先完整读、后写旧值”的
顺序。

### 3.14 B69 task reply 的快照与提交顺序

`get_robust_list()` 只在目标 `task.inner` 内复制两个标量，随后按 Linux ABI 先向用户写
24 字节的 `robust_list_head` 长度，再写 head 地址。目标 TCB 由锁外持有的 `Arc` 固定生命
周期，两个 copyout 都不得跨越 `task.inner`。

同时带“新配置”和“旧值输出”的 timer syscall 使用固定顺序：

```text
UserPtr 读取并校验完整新值
  -> task.inner：快照旧值并一次提交新状态
  -> 解锁
  -> 注册锁外 KernelTimer（若需要）
  -> UserPtrMut 写回旧值
```

`setitimer()` 查询 timer 的 remaining 时只修改栈上快照，不能为了输出而改写
PCB 内保存的 deadline。旧值 copyout 若
返回 `EFAULT`，新配置仍保持生效，不得重锁回滚并覆盖另一 CPU 已观察到的状态。

### 3.15 B70 sigtimedwait 的领取与回复边界

`WaitQueue` 条件闭包会在无锁快速路径执行，也会在完成 waiter 登记后于等待队列锁外再次
执行。登记后的早到 wake 由 `WaitEntry` token 保存，不再要求用队列锁包住第二次检查。
`sigtimedwait()` 的条件闭包仍只允许在 signal owner 锁内领取一条 pending signal，再把完整
`PendingSignal` 移交给 syscall 栈：

```text
task.inner 或 process signal lock：唯一 dequeue
  -> syscall 栈持有 PendingSignal
  -> 完全退出 WaitQueue，清除 signal_wait_mask
  -> UserPtrMut 写回 SigInfo
```

用户地址写入可能缺页、触发 CoW 或等待 TLB shootdown，不能位于 `task.inner` 或进程 signal
lock 内，也应放在整个等待协议退出之后。copyout 返回 `EFAULT` 时信号已经消费，这与 Linux 6.6 先 dequeue、后
`copy_siginfo_to_user()` 的顺序一致；不得为“回滚”把信号重新入队。

### 3.16 B71 sigtimedwait 的睡眠登记窗口

WaitQueue 的第二次条件检查结束后，当前任务尚未完成 `Running -> Blocking` 登记。若远端 CPU
恰在这个窗口发布 waited signal，发送方看到任务仍是 `Running`，不会替接收方保存一次“未来
wake”；因此接收方不能只依赖发送方唤醒。pending signal 本身才是持久事件，固定协议为：

```text
发布 signal_wait_mask
  -> WaitQueue 条件在 owner 锁内尝试 dequeue
  -> 调度器登记 Blocking
  -> 最终睡眠谓词检查 waited pending 或普通 actionable signal
  -> 不满足睡眠条件时撤销 waiter
  -> 任意非 Ready 返回都在 owner 锁内再 dequeue 一次
  -> 清除 signal_wait_mask，再决定 signal / EINTR / EAGAIN
```

`has_waited_signal()` 只观察 `signal_wait_mask` 与线程/进程 pending 集合，不领取信号；线程队列
与进程共享队列分别在各自 owner 锁下读取，禁止嵌套。它只接入目前唯一发布非空 wait mask 的
非 locked WaitQueue 路径，不改变通用 condition 的调用次数或调度状态机。

普通 ignored-signal 清理同样必须排除 `signal_wait_mask`：disposition 为 ignore 并不代表
`sigtimedwait` 放弃领取。最后一次 dequeue 同时处理 `Interrupted` 和 `TimedOut`，让在 timeout
边界已经 pending 的 waited signal 优先于 `EINTR`/`EAGAIN`；领取动作仍只发生在 signal owner
锁内，因此不会重复消费。

### 3.17 B72 prlimit 的成对提交与用户回复边界

`prlimit()` 同时承担“设置新值”和“返回旧值”。新值必须先完整 copyin；随后在资源当前的
owner 锁内完成旧 soft/hard pair 快照、hard-limit 提权复核和新 pair 提交，最后释放锁再
copyout 旧值：

```text
copyin new limit
  -> owner lock：snapshot previous + validate hard raise + commit pair
  -> unlock
  -> copyout previous limit
```

NOFILE 当前由 fd table 持有，两个 setter 必须位于同一次 `files.lock()` 内；其余已实现限制
暂由 `task.inner` 持有。任何合法读者都必须使用同一 owner 锁，因此不能观察到一半来自旧提交、
一半来自新提交的 pair。日志和 uaccess 都不得跨越 owner 锁；old pointer 返回 `EFAULT` 时新值
已经发布，不能回滚覆盖并发更新。

这只是提交协议收口，不代表 owner 已经符合 Linux 的线程组语义。进程级 rlimit owner、组级
CPU accounting，以及 `CLONE_FILES` 跨进程共享时 NOFILE 与 fd table 生命周期的分离仍须后续
节点处理。

### 3.18 B73 进程级 rlimit owner

除 CPU 和 NOFILE 外，已实现的 rlimit 统一由 `ProcessControlBlock::rlimits` 持有。这样
`CLONE_THREAD` 自然共享同一个 owner，普通 fork 在父进程 rlimit 锁内复制完整快照后创建独立
owner，exec 因复用 PCB 而保留限制：

```text
thread clone -> clone PCB Arc -> share rlimits
fork        -> rlimits lock -> copy ProcessLimits -> unlock -> construct child PCB
exec        -> reuse PCB -> preserve rlimits
consumer    -> rlimits lock -> copy one soft limit -> unlock -> task / VM / FS lock
```

`prlimit()` 仍在一次 owner 临界区内完成旧 pair 快照、权限复核与新 pair 提交；普通消费者只
复制所需标量，不能把 rlimit guard 带进 `task.inner`、VM、signal queue 或文件操作。因此本批
没有新增嵌套锁边，只改变共享对象的归属。

CPU 暂时仍由 TCB 持有，因为将字段移入 PCB 而不同时实现线程组运行时间累加，会制造错误的
组限额语义；NOFILE 暂时仍由 fd table 持有，因为它必须先与 `CLONE_FILES` 的跨进程共享生命
周期解耦。这两个例外必须在独立节点中处理，不能把 B73 外推成全部 rlimit 已完成。

### 3.19 B74 线程组 CPU 限额的热路径与安全点

`RLIMIT_CPU` 现在和其它进程限制一样由 PCB 的 rlimit owner 持有，但运行时间不能在每次
trap 进出时获取这个 mutex。每个 TCB 先在 `task.inner` 下累计最多 1ms 的本地尾数，离开
trap、切出或退出时领取增量；调用方释放 `task.inner` 后，才以原子加法冲刷到 PCB：

```text
task.inner：结算 user/system 时间 + 领取本地批次
  -> unlock task.inner
  -> PCB 原子 runtime 累加 + 比较已发布阈值
  -> 到期时只发布 expiry_pending
  -> 用户返回安全点领取 pending
  -> rlimits lock：hard/soft 判定 + soft 推进 1 秒 + 发布下一阈值
  -> unlock rlimits
  -> signal lock：加入进程共享 SIGKILL/SIGXCPU
```

热路径不获取 rlimit/signal 锁，也不直接投递信号；慢路径只在 trap frame 完整且业务锁已释放的
用户返回安全点执行。hard limit 优先于 soft limit；soft 命中后按 Linux 语义增加一秒，使持续
超限的进程每秒再次收到 `SIGXCPU`。线程 clone 共享 PCB 计数，普通 fork 复制限制但建立从零
开始的新计数，exec 复用并保留原 PCB。

并发修改限额时，`rearm_cpu_limit()` 只能把到期标志从 false 发布为 true，不能无条件清零；
否则会覆盖另一 CPU 在运行时间越线后刚发布的事件。安全点更新下一阈值后还要复查累计值，闭合
“处理旧阈值期间另一 CPU 已跨过新阈值”的窗口。由于本地批量策略，动态触发最多可能滞后约
1ms × live-thread 数；退出和 schedule-out 会强制冲刷尾数。这个上界和精确多线程越限交错
尚未由专项用例动态证明，不能把普通 8 核运行外推为该证明。

### 3.20 B75 线程组 CPU 时间查询与退出快照

线程组 CPU 时间使用三个 PCB 原子量：user/system 是 ABI 可见分项，total 是
`RLIMIT_CPU` 唯一的阈值判定源。total 不是由两次分项读取临时相加得到，否则并发 flush 会让
一次限额判定跨越两个不同快照。TCB 仍在自己的 `task.inner` 内结算本地尾数，但只能在释放锁后
发布到 PCB：

```text
task.inner：结算并领取 (user_us, system_us)
  -> unlock task.inner
  -> PCB user/system 原子加法
  -> PCB total 原子加法 + RLIMIT_CPU 阈值判定
```

查询当前进程时，调用线程先用同一路径强制发布已经结算的本地尾数，再读取 PCB 分项；其它正在
运行的 sibling 允许处于至多一个批次的近似窗口，这与无全局停核的 SMP 资源查询语义一致。
线程退出则必须在 `live_threads.fetch_sub(AcqRel)` 前强制冲刷；最后一个线程通过该 release
chain 观察所有 sibling 的先前冲刷，并把 PCB 线程组快照保存到 zombie。父进程后续累加不再
依赖“最后退出线程”的私有 `Rusage`。

这里没有新增 mutex 锁边：`task.inner` guard 在 PCB 原子操作前释放，进程查询也只在完成当前
TCB 冲刷后访问 PCB。精确的跨 CPU 同时查询仍是近似快照，不能为追求瞬时一致而停核或同时获取
多个 TCB 锁。

### 3.21 B76 wait 事件快照与锁外回复

一次 wait 可见事件必须在 child 仍由父进程列表持有时生成完整值快照，不能只返回 PID 后再查
registry。zombie 的 PID 可能紧接着释放；重新查询既可能失败，也可能命中后来复用的对象：

```text
parent process.inner
  -> child process.inner：领取 status，快照 state / exit-rusage / child-rusage
  -> child inner unlock：按 live/zombie owner 组成 RUSAGE_BOTH
  -> 同一快照写入 WaitChildResult，并在 reap 时累加 parent child_rusage
  -> PID / registry / quota 回收
  -> parent inner unlock
  -> syscall copyout
```

`parent process.inner -> child process.inner` 是既有 children 扫描和 reap 顺序，本批没有增加反向
边。`WaitChildResult` 只保存 Copy 值，不把 child 引用或 guard 带到用户访问。`WNOWAIT` 生成同样
的快照但不移除 child、不累加 parent；WNOHANG 无事件时不写 rusage。

`wait4` 在 reap 后按 status→rusage 写回，raw `waitid` 按 rusage→siginfo 写回。任一 copyout
触发缺页、CoW、TLB shootdown 或 EFAULT 时，所有 parent/child/WaitQueue 锁都已释放；Linux
也不会因 EFAULT 把已经消费的 stop/continue 或 zombie 事件重新发布。

### 3.22 B77-B80 进程级 timer owner、CPU clock 与事件交付

POSIX timer 表由 PCB 的独立 mutex 保护，不再嵌入任一 TCB。允许的局部锁边只有：

```text
KERNEL_TIMER_QUEUE -> PosixTimerTable     # compact 只读 stale action 身份
PosixTimerTable -> task.inner             # CPU timer set/get 只读目标线程计时
```

这两条都没有反向边：`timer_settime()`、周期 callback 和 realtime rearm 都必须先释放 timer 表，
再调用 `add_kernel_timer()`。B78 进一步删除 `PosixTimerTable -> process.signal`：wall/CPU callback
都只在表锁内验证身份、推进 deadline 并把完整事件写入固定栈批次；释放表锁后才进入可能扩容的
signal queue、扫描 sibling 或唤醒 runqueue。

CPU timer 安全点先在锁外采样当前 TCB/PCB 的单调 CPU 累计，再取得 timer 表锁唯一领取到期。
采样与表锁不组成嵌套边；并发记账最多让本轮采样偏旧、把投递延迟到下一个安全点，不会提前
触发或重复领取。`timer_settime/gettime` 若需采样 thread clock，当前只存在
`PosixTimerTable -> task.inner` 的单向读取边；任务记账路径释放 `task.inner` 后才访问 PCB 状态，
不得新增反向嵌套。

`timer_create()` 在表锁内把 slot 置为 `Reserved`，锁外写回用户 timer ID，之后再发布
`Active`；任何用户 copyin/copyout 都不得跨 timer 表锁。delete、exec、最后线程退出与回调
使用同一 owner 锁决定先后：回调若先取得锁，则信号已在线性化点生成；清理若先取得锁，旧
action 会因 slot/`arm_seq`/deadline 不匹配而失效。

B80 为每个 timer 对象增加全表单调的 `instance_seq`，并把 pending 身份定义为
`timer ID + instance_seq`。`arm_seq` 只拒绝旧 heap 装载，`instance_seq` 只拒绝删除重建后的
旧 signal 事件，不能混为一个序号。同一 timer 最多一个 pending 事件；不同 timer 即使使用
相同 signal number，也不得被非实时 signal 的普通合并规则折叠。

事件交付和清理固定使用两阶段锁序：

```text
process.signal：dequeue 精确事件并更新 pending hint
  -> unlock
PosixTimerTable：核对 instance_seq，固化 SigInfo/last_overrun

PosixTimerTable：clear/delete 收集精确事件身份
  -> unlock
process.signal：删除对应队列项并更新 pending hint
```

两条路径都不同时持有 signal lock 和 timer owner。`timer_settime()` 若遇到旧事件仍 pending，
保留该队列项但使其旧设置失效；只有新设置再次到期后，交付该项才允许更新
`timer_getoverrun()` 的最近交付值。

B79 的 legacy `IntervalTimerTable` 遵循同一“owner 锁内提交、锁外投递”规则，但不与
`PosixTimerTable` 嵌套：

```text
IntervalTimerTable unlock
  -> KERNEL_TIMER_QUEUE              # REAL 注册/重装
  -> process.signal -> scheduler     # REAL/VIRTUAL/PROF 到期投递
```

REAL callback 在表锁内核对 PCB generation/deadline 并推进周期；VIRTUAL/PROF 安全点先在
锁外采样 PCB CPU 累计，再在表锁内唯一领取。`task.inner` 只负责把当前线程记账尾数取出，
释放后才原子冲刷 PCB；不能形成 `task.inner -> IntervalTimerTable` 的嵌套边。

### 3.23 B81 shared signal hint 与权威队列同临界区发布

`shared_pending_hint` 只是 process signal queue 的无锁快照，不拥有 signal。所有 queue writer
必须遵守：

```text
process.signal lock
  -> enqueue / dequeue / remove exact timer event
  -> 重新计算完整 pending bits
  -> Release store shared_pending_hint
process.signal unlock
```

若 store 位于 unlock 之后，旧消费者可先算出 0 并暂停，新生产者完整入队并写入非零，旧消费者
随后再写回 0；mutex 只串行了 queue mutation，没有串行这两个 store。把 store 放回临界区后，
mutex 决定 writer 的唯一全序，最后离开临界区的 writer 一定发布对应的最新队列快照。

无锁读端用 Acquire load。Release/Acquire 表达 hint 的发布与消费，但不能替代上述 writer
互斥；单纯把锁外 Relaxed 改成 Release 仍然保留旧值覆盖窗口。读到 hint 后需要领取对象时，
仍必须取得对应 signal owner 锁重新检查权威队列。

`take_shared_signal()`/`take_shared_matching()` 只在 signal 临界区内 dequeue 和更新 hint；随后
先解锁，再进入 POSIX timer owner 执行 discard/finalize，因此 B80 的 signal/timer 无嵌套锁序
保持不变。

### 3.24 B89 单页帧领取与锁外清零

`FRAME_ALLOCATOR` 写锁只保护 fresh region 游标、recycled 栈和 owner bit。普通
`frame_alloc()` 的固定顺序为：

```text
FRAME_ALLOCATOR write lock
  -> reserve_one(): 唯一领取 PPN 并生成 FrameReservation
FRAME_ALLOCATOR unlock
  -> FrameReservation::into_tracker(): 按需清零 4 KiB
  -> Arc::new(FrameTracker)
```

返回 reservation 后必须先结束包含 `write()` 临时 guard 的语句，才能消费或
drop reservation；否则异常回滚会通过 `frame_dealloc()` 重入同一把锁。当前
OOM/非 OOM 调用点都使用独立 `let reservation = ...;` 语句表达该边界。

reservation 用 `Option::take()` 完成 PPN 所有权移交；消费后它的 `Drop` 是 no-op，
未消费则将 PPN 归还。recycled 页始终在锁外重新清零；只有启用
`zero_init` 且首次领取的 fresh 页可依赖 BSP 预清零而跳过。连续帧与
unsafe uninit 路径未经过此 reservation，不应把两类所有权协议混用。

## 4. 永久禁止的组合

- 两个不同 CPU 的 runqueue 锁同时持有；
- task.inner 与任意 runqueue 锁嵌套；
- 普通锁跨 `__switch`、schedule、yield、block、IPI ack 或 shootdown ack；
- MM/PTE 锁内等待远端 TLB ack；
- 发 IPI 时仍持有目标 CPU 可能在 handler 后续路径获取的锁；
- hard IRQ/IPI 中分配内存、进入文件系统/网络栈或执行任务切换；
- 以“当前只有单核”作为 `unsafe impl Send/Sync`、裸指针或 `static mut` 的安全证明。

## 5. 每批锁变更审查记录

涉及锁的 SMP 批次必须在修改前申请和修改后报告中列出：

- 新增或改变的锁、拥有者、IRQ 可达性和是否允许睡眠；
- 完整获取/释放路径，以及和本文哪条部分序对应；
- 是否可能在本地中断关闭、preempt 禁止或 panic 上下文进入；
- 错误、超时、重复 wake/IPI 和回滚路径；
- 双架构 focused test，以及 lockdep 尚未实现时使用的断言和计数器。

如果实际调用链需要本文未定义的嵌套关系，必须先更新设计并人工确认，不能在代码中局部
“先加一把锁”绕过。
