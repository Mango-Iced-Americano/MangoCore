---
title: "TaskControlBlock 线程级执行实体"
category: process
status: stable
author: MangoCore Team
last_update: 2026-07-31
tags: [process, task, tcb, thread, smp]
---

# TaskControlBlock 线程级执行实体

## 1. 源码位置

`TaskControlBlock` 定义在 `os/src/task/task.rs`，由 `os/src/task/mod.rs` 导出。它是调度器直接运行的实体，对应用户可见的线程 ID，即 `gettid()`。

```
TaskControlBlock
  ├── 不可变或原子字段：tid、process、kstack、user_res_slot、sched_state、hint
  └── inner: Mutex<TaskControlBlockInner>
        ├── trap context / task context
        ├── signal mask / pending / alternate stack
        ├── sched ABI state
        ├── rlimit / credentials / capability
        ├── robust futex / clear_child_tid
        ├── per-thread rseq registration
        └── timers / rusage / seccomp
```

TCB 不保存进程的 fd table、cwd、VM、children、sighand 或 futex 表本体；这些属于 `ProcessControlBlock`，TCB 通过 `process: Arc<ProcessControlBlock>` 访问。

## 2. TCB 外层字段

`TaskControlBlock` 外层字段定义在 `os/src/task/task.rs:102`。这些字段大多在任务创建后不直接替换，或者使用原子变量提供热路径 hint。

```rust
pub struct TaskControlBlock {
    pub tid: Arc<TidHandle>,
    pub user_res_slot: usize,
    pub process: Arc<ProcessControlBlock>,
    pub kstack: KernelStack,
    pub ustack_base: usize,
    pub user_stack_allocated: AtomicBool,
    pub(crate) thread_live_counted: AtomicBool,
    seccomp_counted: AtomicBool,
    uid_hint/euid_hint/suid_hint/gid_hint/egid_hint/sgid_hint,
    pub exit_signal: Signals,
    _thread_quota: Option<TaskQuotaGuard>,
    inner: Mutex<TaskControlBlockInner>,
    sched_state: AtomicUsize,
    pub wait_io_timer_pending: AtomicBool,
    pub wait_timer_generation: AtomicUsize,
    pub sched_nice_hint: AtomicI32,
}
```

| 字段 | 语义 |
|------|------|
| `tid: Arc<TidHandle>` | 用户可见线程 ID |
| `user_res_slot` | 当前地址空间内 trap context / 默认用户栈槽位 |
| `process` | 所属 `ProcessControlBlock` |
| `kstack` | 内核栈 |
| `ustack_base` | 默认用户栈底，或 clone 指定 child stack |
| `user_stack_allocated` | 当前线程是否拥有内核管理的默认用户栈区域 |
| `thread_live_counted` | 是否已经计入进程 live thread 计数 |
| `seccomp_counted` | 是否计入全局 active seccomp task 数 |
| `uid/euid/suid/gid/egid/sgid_hint` | 当前身份热路径缓存 |
| `exit_signal` | 非 `CLONE_THREAD` child 退出时投递给父进程的信号 |
| `_thread_quota` | `CLONE_THREAD` 线程级 quota guard |
| `sched_state` | 原子调度状态与 CPU owner，是调度所有权的唯一真值 |
| `wait_io_timer_pending` | I/O fallback timer 去重标记（已注册 deadline 唤醒 timer 的去重标记） |
| `wait_timer_generation` | wait timeout generation，过滤旧 timer |
| `wait_io_fallback_active_generation` | fallback wait 活跃 generation |
| `sched_nice_hint` | runqueue fast path 判断 nice 是否为 0 |
| `sched_vruntime_hint` | runqueue 锁内公平选择使用的 vruntime 原子快照 |

这些字段里，`sched_nice_hint` 和当前任务身份 hint 直接影响 syscall/调度热路径，避免频繁持有 TCB inner 锁。
LA64 ASID 不属于线程字段：它由 `AddressSpace` 的 `TlbContext` 持有，同一 MM 的线程
共享一个 versioned ASID，并在用户返回时与页表根一起激活。

无 deadline 的等待不注册 timer：Waiter/Waker 的 one-shot 通知握手在显式唤醒、信号或条件满足时完成状态转换，因此支持无限期等待而不需要定时器兜底。

## 3. TCB inner 字段分组

`TaskControlBlockInner` 定义在 `task.rs:154`，使用 `spin::Mutex` 保护。字段可以按职责分组：

| 分组 | 字段 |
|------|------|
| 信号 | `sigmask`, `sigmask_to_restore`, `sigpending`, `signal_wait_mask`, `signal_stack` |
| 上下文 | `trap_cx_ppn`, `task_cx` |
| 调度兼容 | `sched_policy`, `sched_priority`, `sched_reset_on_fork`, `sched_nice`, `sched_vruntime`, `sched_runtime`, `sched_deadline`, `sched_period` |
| I/O 优先级 | `ioprio_class`, `ioprio_prio` |
| rlimit | `rtprio`, `nice`, `sigpending`, `stack`, `memlock`, `fsize`, `nproc`, `cpu`, `core` |
| 进程属性兼容 | `personality`, `pdeath_signal`, `dumpable`, `task_comm`, `timer_slack` |
| ptrace/seccomp | `ptrace_traceme`, `seccomp_mode`, `seccomp_filter` |
| credentials/capability | uid/gid/fsuid/fsgid/groups/cap sets/securebits/ambient |
| futex 退出协作 | `clear_child_tid`, `robust_list` |
| 计时 | `rusage`, `clock` |
| OOM | `pending_oom_kill` |

其中一部分字段用于 syscall 回读、权限分支或 fork 继承规则；真实参与调度、权限检查和 ptrace 行为的路径在对应章节单独列出。
PRIVATE_EXPEDITED membarrier 注册不再属于 TCB inner；B44 将它放入共享
`AddressSpace`，使同 MM 线程共享而 fork/exec 新 MM 不继承。

### 3.1 inner 字段读写路径

| 字段组 | 主要读写位置 | 用户可见影响 |
|--------|--------------|--------------|
| `sigmask/sigpending/signal_stack` | `task/signal/*`, `syscall/process/signal.rs` | `rt_sigprocmask`, `sigaltstack`, signal delivery, signalfd/sigtimedwait。 |
| `trap_cx_ppn/task_cx` | `task/task.rs`, `task/processor.rs`, HAL trap return | 上下文切换、返回用户态。 |
| 外层 `sched_state` | `task/task.rs`, `task/manager.rs`, `task/mod.rs` | 原子调度状态、队列归属和当前 CPU owner。 |
| `sched_*` | `task/run_queue.rs`, `syscall/process/ids.rs` | runqueue 选择、`sched_get*`/`sched_set*` 回读。 |
| `rlimit` 字段 | `syscall/process/ids.rs`, `syscall/fs.rs`, `syscall/process/time.rs` | 文件大小、CPU 时间、memlock、nice/rtprio 等限制。 |
| `clear_child_tid/robust_list` | `task/task.rs::exit_thread_resources()`, `syscall/process/lifecycle.rs` | pthread join/futex robust list 退出协作。 |
| `rusage/clock` | trap enter/leave、schedule in/out、time syscall | 线程 CPU 记账；legacy itimer 由 PCB 汇总账户驱动。 |
| `pending_oom_kill` | MM 分配失败和 trap return 安全点 | OOM 后在可安全返回点杀任务。 |

读 TCB 代码时要关注锁边界：`task.inner` 保护线程可变状态，但很多 syscall 需要在释放 inner 锁后访问 fd、VM 或 WaitQueue。跨等待点持有 inner 锁会阻塞 signal、exit 和调度状态更新。

## 4. TaskStatus

任务状态枚举：

```rust
pub enum TaskStatus {
    New,
    Queued(usize),
    Running(usize),
    Blocking(usize),
    Blocked,
    Migrating,
    Zombie,
}
```

状态含义：

| 状态 | 所在位置 |
|------|----------|
| `New` | 已构造但尚未发布到运行队列 |
| `Queued(cpu)` | CPU `cpu` 的 runqueue 所有；B15 阶段 `cpu` 恒为 CPU0 |
| `Running(cpu)` | CPU `cpu` 的 current slot 所有；B15 阶段 `cpu` 恒为 CPU0 |
| `Blocking(cpu)` | 已登记到 interruptible registry，但仍由 CPU `cpu` 执行 |
| `Blocked` | 已切离 CPU，在 interruptible registry 中等待唤醒 |
| `Migrating` | queued task 已离开源队列、尚未进入目标队列的单-owner 窗口 |
| `Zombie` | 已执行线程级退出，等待切栈后 drop 或进程级 wait 回收 |

`sched_state` 把状态 tag 与 CPU owner 编码在一个 `AtomicUsize` 中，并通过
Acquire 读取、AcqRel CAS 迁移。`task.inner` 不再保存第二份状态。文档不能把
`ProcessState::Zombie` 和 `TaskStatus::Zombie` 混为一谈。

### 4.1 TaskStatus 状态图

```
New --publish--> Queued(cpu) --fetch--> Running(cpu)
Queued(cpu) --move--> Migrating --publish--> Queued(other_cpu)
Running(cpu) --yield + switch complete--> Queued(cpu)
Running(cpu) --begin sleep--> Blocking(cpu)
Blocking(cpu) --early wake--> Running(cpu)
Blocking(cpu) --switch complete--> Blocked
Blocked --wake--> Queued(cpu)
Running(cpu) --exit--> Zombie --switch complete--> zombie queue
```

发布、fetch 和 yield 后重入队由 owner CPU 的 RunQueue 在一个锁域内同时提交状态 CAS
与容器变更；阻塞登记仍由 `TaskManager` registry 管理。idle 在切栈后提交
`Blocking -> Blocked`；早到 wake 只恢复 `Blocking -> Running`，晚到 wake 按
`TASK_MANAGER -> CPU0 RunQueue` 执行 `Blocked -> Queued(CPU0)`。重复 wake 因 CAS
失败而不会重复入队。退出只能由任务自己在 owner CPU 的安全点提交
`Running(cpu) -> Zombie`；阻塞 sibling 先被唤醒，排队 sibling 先被 fetch。
`New/Queued/Blocking/Blocked/Migrating -> Zombie` 都是所有权错误并 fail-stop。

## 5. initproc 创建

`TaskControlBlock::new(elf)` 只用于初始进程。

主要流程：

1. 将 `/init` 或 `/initproc` ELF 映射到内核空间。
2. `AddressSpaceInner::from_elf()` 创建用户地址空间数据，再包装成共享 `AddressSpace`。
3. 从 `KERNEL_SPACE` 删除临时 ELF 映射。
4. 创建 `RecycleAllocator` 作为用户资源槽位分配器。
5. `tid_alloc()` 分配 tid，pid/pgid/sid 初始都取该 tid。
6. `kstack_alloc()` 分配内核栈。
7. `alloc_user_res_with_trap_ppn(slot, true)` 分配默认用户栈和 trap context。
8. `create_elf_tables()` 写入 init 的 argv/envp/auxv。
9. 打开 `/dev/tty` 三次作为 fd 0、1、2。
10. cwd 设置为 `/`。
11. 构造 `ProcessControlBlock`。
12. 构造 TCB，原子状态为 `New`。
13. 注册进程和任务，并由 `publish_task()` 发布为 `Queued(CPU0)`。
14. 初始化 trap context，入口为 ELF entry，用户栈为 init stack。

initproc 的默认环境变量包括 `PATH=/:/bin:/sbin:/usr/bin:/tools/bin`、`PWD=/`、`HOME=/root`。

## 6. clone 创建 TCB

普通 clone 调用 `TaskControlBlock::sys_clone()`。它在父任务 inner 锁下完成大量状态复制：

| 资源 | `CLONE_*` 影响 |
|------|----------------|
| VM | `CLONE_VM` 共享，否则 `AddressSpaceInner::from_existing_user()` 后包装新 `AddressSpace` |
| user_res_slot_allocator | 共享 VM 时共享，否则 clone allocator |
| ProcessControlBlock | `CLONE_THREAD` 共享进程，否则创建新 PCB |
| files | `CLONE_FILES` 共享，否则 clone fd table |
| fs | `CLONE_FS` 共享，否则 clone cwd/root/umask 状态 |
| sighand | `CLONE_SIGHAND` 共享，否则复制 action 表 |
| futex table | 共享 VM 时共享，否则新建 private futex table |
| namespaces | `CLONE_NEWUTS/NEWNET/NEWNS/NEWIPC` 创建对应对象 |

共享 VM 的线程会分配独立 trap context 槽位；非共享 VM 的 fork 子进程沿用同一个 slot 号，因为 slot 是地址空间内布局索引，不是全局唯一线程号。

读 `TaskControlBlock::sys_clone()` 时要把“复制 TCB 字段”和“共享/复制 PCB 资源”分开看。TCB 自身一定是新的调度实体：有新的 TID、新的内核栈、新的 `TaskContext`，并且最终要进入 ready queue。PCB 是否新建由 `CLONE_THREAD` 决定；地址空间是否共享由 `CLONE_VM` 决定；fd/fs/sighand 是否共享分别由对应 flag 决定。因此同一个 clone 入口可以覆盖 fork、pthread create 和 vfork 等不同语义。

child 的 trap context 是 clone 返回值差异的关键：父线程从 syscall 返回 child tid，子线程恢复到用户态时返回 0。实现通过复制父 trap context 后改写 child 的返回寄存器完成，而不是让 child 重新进入 syscall 分发。

## 7. trap context 与 task context

TCB 同时持有两个上下文：

| 上下文 | 位置 | 作用 |
|--------|------|------|
| `TrapContext` | 用户地址空间中的 trap context 页，PPN 保存在 `trap_cx_ppn` | 保存用户寄存器、内核页表 token、kernel_sp、trap_handler |
| `TaskContext` | `TaskControlBlockInner::task_cx` | 内核调度切换上下文 |

新任务的 `task_cx` 使用 `TaskContext::goto_trap_return(kstack_top)`，使任务第一次被调度时进入 trap return 路径回到用户态。

clone 子任务的 `TrapContext`：

1. 共享 VM 时，先从父 trap context 复制。
2. 指定 child stack 时改写 `sp`。
3. `CLONE_SETTLS` 时设置 `tp`。
4. 子任务返回值 `a0 = 0`。
5. `kernel_sp` 改为子内核栈顶。

父进程返回值由 syscall 层返回 child tid。

`TaskControlBlockInner::trap_context_mut(&mut self)` 是 Rust 层唯一的 trap context
可变访问入口。它返回的引用只能活在当前 `task.inner` guard 内；禁止恢复曾经存在的
`&'static mut TrapContext` 或从临时 guard 返回引用的 current-task helper。需要在
用户访存、PTE 更新、IPI ack 等等待边界两侧访问 trap frame 时，应先在锁内按值快照所需
字段，解锁完成操作，再重新加锁校验并提交。

信号 ABI 只保存用户通用寄存器和浮点寄存器。双架构统一通过
`TrapContext::machine_context()` 按值取得这部分状态，并通过
`set_machine_context()` 恢复；禁止依赖 `TrapContext`/`MachineContext` 的前缀布局做
裸指针强转。这样恢复信号 frame 时不会覆盖 `kernel_sp`、内核页表 token、trap handler
或 CPU-local 指针。

## 8. 调度时间统计

`TaskControlBlockInner` 在 trap 和 schedule 处维护 CPU 时间：

| 方法 | 时机 |
|------|------|
| `update_process_times_enter_trap()` | 从用户态进入内核态 |
| `update_process_times_leave_trap()` | 从内核态返回用户态 |
| `update_process_times_schedule_out()` | 内核态主动让出 CPU |
| `update_process_times_schedule_in()` | 任务被调度运行 |

用户时间 `ru_utime` 在进入 trap 时按上次用户态时间差累加；系统时间 `ru_stime` 在离开 trap 或 schedule out 时累加。

`sched_vruntime` 根据 nice 权重更新：

```text
vruntime += runtime_us * 1024 / nice_weight
```

这只用于 runqueue 中非零 nice 任务的选择，不是完整 CFS 实现。

## 9. itimer 与 POSIX timer

legacy interval timer 也不再属于 TCB。PCB 的 `IntervalTimerTable` 是线程组唯一 owner：
thread clone 共享，普通 fork 新建空表，exec 保留，最后线程退出清空。REAL 使用 monotonic
heap deadline；VIRTUAL/PROF 分别读取 PCB 的线程组 user 与 user+system CPU 累计，并在
trap-return、schedule-out 安全点唯一领取到期。三类信号都进入进程共享 pending，不绑定
调用 `setitimer()` 的线程。

POSIX timer 不属于 TCB。PCB 的独立 `PosixTimerTable` 是线程组共享 owner：thread clone
共享，fork 新建空表，exec 和最后线程退出清空。创建线程退出不会单独删除 timer；到期信号
进入进程共享 pending，由任一未屏蔽的 sibling 接收。wall-time action 只持 PCB `Weak`，
不会为保留 timer 而延长 zombie 生命周期。

`CLOCK_THREAD_CPUTIME_ID` 是唯一仍引用某个 TCB 的 POSIX timer，但只保存创建者
`Weak<TaskControlBlock>`：它读取该线程累计 user+system 时间，不拥有线程，也不会被 TID
复用误命中。`CLOCK_PROCESS_CPUTIME_ID` 直接读取 PCB 的线程组累计。两者都不进入 wall-time
kernel timer heap，而在 trap return 与 schedule-out 安全点由 PCB 表锁唯一领取到期。

## 10. 线程级退出资源

`exit_thread_resources(exit_code)` 只处理线程级资源：

1. 状态改为 `Zombie`。
2. 结算 schedule out 时间。
3. 取出并清空 `clear_child_tid`。
4. 重置 robust list。
5. 从进程 live thread 计数移除。
6. 如果 `clear_child_tid != 0`，向用户地址写 0。
7. 唤醒 private futex 和可能的 shared futex key。
8. 尝试缓存 trap context slot。
9. 释放当前线程用户栈/trap context 映射，或保留 trap context 页。

父子关系、进程 zombie、fd table 关闭、VM 释放不在该函数中完成。

## 11. clear_child_tid 和 robust list

`clear_child_tid` 来自 `set_tid_address` 或 `CLONE_CHILD_CLEARTID`。退出时：

```text
write 0 to clear_child_tid
  ├── process.futex().wake(clear_child_tid, 1)
  └── 如果所在 VMA 使用 shared key:
        ├── wake old physical key
        └── 若 fault 后 key 变化，wake new physical key
```

写用户地址通过当前进程 VM 的 `fault_in_user_va(Store)` 完成，支持跨页 4 字节写。

## 12. Drop 行为

`Drop for TaskControlBlock`：

1. 取消 seccomp active 计数。
2. 从 task registry 注销 tid。
3. 从所属进程线程表移除。
4. 释放 `user_res_slot_allocator` 中的 slot。

当前任务退出时不能在自己的内核栈上释放最后一个 `Arc<TaskControlBlock>`，所以 `exit_current_and_run_next()` 先加入 zombie queue，切回 idle 后由调度循环 drop。

## 13. 关键约束

1. `TaskControlBlockInner` 锁不能跨等待点持有。
2. 线程级退出和进程级退出必须分层处理。
3. `user_res_slot` 是地址空间内槽位，fork 后独立地址空间可以复用同一 slot。
4. `CLONE_THREAD` 的 quota 放在线程 TCB，非线程 clone 的 quota 放在 PCB。
5. `current_task()` 返回克隆的 `Arc`；进入不返回的 context switch 前必须显式释放。
6. ABI 保存字段要按实际读写路径描述，不把保存字段扩写成独立子系统。

## 14. 调试核对点

| 现象 | 检查 |
|------|------|
| clone 子进程返回值不是 0 | 子 trap context `a0` 是否改写 |
| 线程退出后 futex wait 卡住 | `clear_child_tid` 写 0 和 futex wake 路径 |
| exec 后旧线程仍运行 | `load_elf()` 是否杀掉同线程组其他线程并从队列移除 |
| wait 观察不到进程 zombie | 是否只有线程 zombie，进程 live thread count 是否为 0 |
| la64 线程切换后地址空间串线 | 同一 PCB 的 `AddressSpace` 是否返回同一个 ASID；rollover 是否先完成全 CPU flush/ack |
