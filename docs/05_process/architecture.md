---
title: "进程与任务架构详解 (Process and Task Architecture)"
category: process
status: stable
author: MangoCore Team
last_update: 2026-07-27
tags: [process, task, scheduler, signal, futex, ipc]
---

# 进程与任务架构详解

## 1. 概述

MangoCore 的执行模型分为线程级 `TaskControlBlock` 和进程级 `ProcessControlBlock`。TCB 是调度实体，持有内核栈、trap context、线程信号状态、调度字段和退出清理信息；PCB 是资源容器，持有地址空间、fd table、文件系统状态、namespace、sighand、futex 表、子进程关系和进程生命周期状态。

调度器是单核实现，核心位于 `task/manager.rs` 和 `task/processor.rs`。系统调用层通过 `syscall/process/*` 进入 clone、exec、exit/wait、signal、futex、IPC、time、ids、rlimit 和 sched 兼容路径。

## 2. 设计目标

| 目标 | 实现方式 |
|------|----------|
| 线程/进程分层 | TCB 管调度，PCB 管资源和进程关系 |
| Linux clone 语义 | `CLONE_VM/FILES/FS/SIGHAND/THREAD` 控制资源共享 |
| 单核可抢占调度 | timer interrupt + ready/interruptible/zombie 队列 |
| 阻塞原语统一 | `WaitQueue` 支撑 futex、epoll、eventfd、socket、timer |
| 进程生命周期可 wait | PCB 维护 children、exit_code、stopped/continued 状态和 wait queue |
| signal 交付 | trap return 前 `do_signal()` 构造/恢复用户信号帧 |
| futex shared/private | private 表按进程，shared 表按物理地址 key |
| IPC namespace | PCB 持有 IPC namespace，clone/unshare/setns 可切换 |

## 3. 架构

### 3.1 层次

```
+-------------------------------------------------------------------+
| syscall/process/*                                                 |
| clone exec lifecycle signal futex ipc ids time bpf keyring misc   |
+-------------------------------------------------------------------+
| ProcessControlBlock                                               |
| pid threads vm files fs uts net mnt ipc sighand futex children    |
+-------------------------------------------------------------------+
| TaskControlBlock                                                  |
| tid kstack trap_cx signal mask status sched clear_child_tid       |
+-------------------------------------------------------------------+
| TaskManager + Processor                                           |
| ready_queue | interruptible_queue | zombie_queue | idle context   |
+-------------------------------------------------------------------+
| HAL switch / trap / timer                                         |
+-------------------------------------------------------------------+
```

### 3.2 源文件地图

| 文件 | 职责 |
|------|------|
| `task/task.rs` | TCB、TCB inner、任务创建、clone 构造、exec 装载、线程退出 |
| `task/process.rs` | PCB、ProcessInner、资源共享、finish_exit、父子关系 |
| `task/manager.rs` | TaskManager、WaitQueue、KernelTimerQueue |
| `task/completion.rs` | Completion 单次通知原语 |
| `task/processor.rs` | Processor、当前任务、`run_tasks()`、`schedule()` |
| `task/mod.rs` | task 子系统导出、suspend/block/exit 辅助 |
| `task/signal/` | signal action、delivery、frame、pending、wait |
| `task/threads.rs` | futex 表和 wait/wake/requeue |
| `syscall/process/clone.rs` | clone、clone3、unshare、setns |
| `syscall/process/exec.rs` | execve、execveat |
| `syscall/process/lifecycle.rs` | exit、wait、robust list |
| `syscall/process/signal.rs` | signal、pidfd、signalfd、kcmp |
| `syscall/process/futex.rs` | futex syscall |
| `syscall/process/ipc.rs` | SysV IPC、POSIX MQ |
| `syscall/process/time.rs` | time、timer、rusage |
| `syscall/process/ids.rs` | ids、rlimit、sched、prctl、capability |

## 4. 关键数据结构

### 4.1 TaskControlBlock

| 字段 | 说明 |
|------|------|
| `tid` | 用户可见线程 ID |
| `user_res_slot` | trap context / 默认用户栈资源槽位 |
| `process` | 所属 PCB |
| `kstack` | 内核栈 |
| `ustack_base` | 用户栈基址 |
| `exit_signal` | 非线程 clone 的退出信号 |
| `sched_nice_hint` | ready queue 快路径 hint |
| `sched_state` | 原子调度状态与 CPU owner，调度所有权唯一真值 |
| `asid` | la64 ASID；rv64 保持 0 |
| `inner` | TCB 可变状态 |

`TaskControlBlockInner`：

| 类别 | 代表字段 |
|------|----------|
| signal | `sigmask`, `sigpending`, `signal_wait_mask`, `signal_stack` |
| context | `trap_cx_ppn`, `task_cx` |
| sched | `sched_policy`, `sched_priority`, `sched_nice`, `sched_vruntime` |
| rlimit | stack、memlock、fsize、nproc、cpu、core 等 |
| prctl/personality | `personality`, `pdeath_signal`, `dumpable`, `task_comm` |
| futex exit | `clear_child_tid`, `robust_list` |
| time | rusage、clock、POSIX timer |
| OOM | `pending_oom_kill` |

### 4.2 TaskStatus

```
New --publish--> Queued(cpu) --fetch--> Running(cpu)
Running(cpu) --yield + switch complete--> Queued(cpu)
Running(cpu) --begin sleep--> Blocking(cpu)
Blocking(cpu) --early wake--> Running(cpu)
Blocking(cpu) --switch complete--> Blocked
Blocked --wake--> Queued(cpu)
Running(cpu) --exit--> Zombie --switch complete--> zombie queue
New / Blocked --external cleanup--> Zombie
```

| 状态 | 含义 |
|------|------|
| `New` | 已构造但尚未发布 |
| `Queued(cpu)` | 由 CPU `cpu` 的 runqueue 拥有 |
| `Running(cpu)` | 由 CPU `cpu` 的 current slot 拥有 |
| `Blocking(cpu)` | 已登记阻塞意图但尚未切离 CPU；早到 wake 恢复为 `Running(cpu)` |
| `Blocked` | 已切离 CPU并留在 interruptible registry |
| `Zombie` | 线程退出，等待回收 |

状态存放在 TCB 外层 `AtomicUsize`，状态 tag 与 CPU owner 一次 CAS 更新；
`task.inner` 不保存第二份状态。B15 时队列仍为全局容器，owner 固定为 CPU0。

### 4.3 ProcessControlBlock

| 字段 | 说明 |
|------|------|
| `pid` | 用户可见 PID |
| `leader_tid` | 线程组 leader TID |
| `threads` | 线程列表 |
| `live_threads` | 活跃线程计数 |
| `trap_context_cache` | 可复用用户资源槽位 |
| `child_exit_wait` | 父进程等待子进程退出的 WaitQueue |
| `vfork_parent`, `vfork_done` | vfork 父线程和 Completion |
| `adopted_by_init` | 是否被 init 收养 |
| `inner` | 进程可变资源 |

`ProcessInner`：

| 类别 | 字段 |
|------|------|
| executable | `exe`, `exec_key`, `exe_path` |
| files/fs | `files`, `fs` |
| namespace | `uts`, `net`, `mnt`, `ipc` |
| memory | `vm` |
| signal/futex | `sighand`, `futex` |
| resource slots | `user_res_slot_allocator` |
| relations | `parent`, `children`, `pgid`, `sid` |
| lifecycle | `state`, `exit_code`, stopped/continued/ptrace 字段 |

### 4.4 ProcessState

| 状态 | 含义 |
|------|------|
| `Running` | 进程处于运行生命周期 |
| `Stopped` | signal/ptrace 停止 |
| `Zombie` | 进程退出完成，等待 wait 或自动回收 |

### 4.5 TaskManager

| 队列 | 类型 | 说明 |
|------|------|------|
| `ready_queue` | `VecDeque<Arc<TaskControlBlock>>` | 可运行任务 |
| `interruptible_queue` | `VecDeque<Arc<TaskControlBlock>>` | 可中断睡眠任务 |
| `zombie_queue` | `VecDeque<Arc<TaskControlBlock>>` | 延迟回收任务 |
| `ready_nonzero_nice_count` | 计数 | ready queue 是否需要公平扫描 |

### 4.6 WaitQueue

`WaitQueue` 内部保存 `Weak<TaskControlBlock>`，避免等待队列强持有任务。

| 方法 | 说明 |
|------|------|
| `wake_all()` | 唤醒所有有效等待者 |
| `wake_at_most(limit)` | 最多唤醒指定数量 |
| `wake_one()` | 唤醒一个 |
| `wait_event_interruptible()` | 可被信号打断的条件等待 |
| `wait_event_timeout()` | 带 timeout 的条件等待 |
| locked variants | 条件检查与外部锁配合，避免 lost wakeup |

## 5. 执行流程

### 5.1 init 进程创建

```
task::add_initproc()
    INITPROC lazy_static
        try /init
        fallback /initproc
    TaskControlBlock::new(elf)
        AddressSpace::from_elf()
        allocate pid/tid/pgid
        allocate user resources and stack
        build argv/envp
        open /dev/tty as fd 0/1/2
        cwd = /
        register process/task
```

### 5.2 调度主循环

```
run_tasks()
    do_wake_expired()
    NET_INTERFACE.try_poll() periodically
    fs::reclaim::maybe_reclaim_fs_caches()
    drain zombie queue
    cleanup stale zombies
    compact shared futex
    fetch ready task
        set Running
        schedule_in accounting
        publish current hints
        __switch(idle, task)
    idle if no ready task
```

`pop_next_ready()`：

| 条件 | 行为 |
|------|------|
| 无非零 nice 任务 | FIFO `pop_front()` |
| 有非零 nice 任务 | 扫描 ready queue，按 `(sched_vruntime, sched_nice, tid)` 选择 |

### 5.3 让出和阻塞

| 函数 | 流程 |
|------|------|
| `suspend_current_and_run_next()` | 当前任务设为 Ready，加入 ready queue，切换 idle |
| `block_current_and_run_next()` | 当前任务设为 Interruptible，加入 interruptible queue，切换 idle |
| checked/locked block | 条件重检后阻塞，防止 lost wakeup |
| `schedule()` | 当前任务上下文切换到 idle |

### 5.4 clone

```
sys_clone/sys_clone3
    validate flags
    check namespace privilege
    choose VM:
        CLONE_VM -> share
        else -> AddressSpace::from_existing_user()
    choose resources:
        CLONE_FILES -> share files
        CLONE_FS -> share fs
        CLONE_SIGHAND -> share sighand
        CLONE_THREAD -> same PCB
        else -> new PCB
    allocate TID/user slot/kstack/trap context
    write parent/child TID if requested
    allocate pidfd if requested
    publish child
    schedule child or wait vfork completion
```

关键校验：

| 条件 | errno |
|------|-------|
| `CLONE_SIGHAND` 缺少 `CLONE_VM` | `EINVAL` |
| `CLONE_THREAD` 缺少 `CLONE_SIGHAND` | `EINVAL` |
| `CLONE_VFORK` 与 `CLONE_THREAD` 同时设置 | `EINVAL` |
| `CLONE_NEWNS` 与 `CLONE_FS` 同时设置 | `EINVAL` |
| namespace 操作 euid 非 0 | `EPERM` |
| `CLONE_PIDFD` 与 `CLONE_PARENT_SETTID` 同时设置 | `EINVAL` |

### 5.5 exec

```
sys_execve()
    read pathname/argv/envp
    open_exec()
    exec_opened_file()
sys_execveat()
    read dirfd/pathname/argv/envp/flags
    open_exec_with_follow() 或 reopen_exec_fd()
    exec_opened_file()
exec_opened_file()
    check file type/access/ETXTBSY
    parse shebang
    validate stack usage
TaskControlBlock::load_elf()
    build new AddressSpace
    map ELF/interpreter/heap/user stack
    terminate sibling threads
    close CLOEXEC fds
    reset sighand/futex
    clear thread exit state
    complete vfork parent
```

`execveat` 支持 `AT_SYMLINK_NOFOLLOW` 和 `AT_EMPTY_PATH`。

### 5.6 exit

```
sys_exit
    do_exit(current, code)
        exit_thread_resources()
        if live_thread_count == 0:
            release process resources
            finish_exit()
        exit_current_and_run_next()
```

`exit_group` 会请求组退出，移除其他线程队列项，释放其他线程资源后退出当前线程。

`exit_thread_resources()`：

| 项 | 行为 |
|----|------|
| 状态 | 设置 `Zombie` |
| live count | 从进程 live thread 计数移除 |
| clear_child_tid | 写 0，唤醒 futex |
| robust_list | 清理 |
| user slot | 缓存或释放 trap context slot |

### 5.7 finish_exit

| 阶段 | 行为 |
|------|------|
| vfork | complete vfork parent |
| rusage | 汇总 maxrss 等统计 |
| state | 标记 PCB zombie |
| auto-reap | 根据父进程 SIGCHLD/收养状态决定 |
| exec key | 注销 executable busy key |
| children | reparent 给 subreaper 或 init |
| wait | 唤醒父进程 `child_exit_wait` |
| signal | 向父进程发送退出信号 |
| resources | 释放 VM、关闭 fd |

### 5.8 wait

```
wait4/waitid
    validate id/options
    scan children
    if exited/stopped/continued:
        fill status or siginfo
        consume unless WNOWAIT
    else if WNOHANG:
        return 0
    else:
        sleep on child_exit_wait
```

`waitid` 支持 `P_PIDFD`。

### 5.9 signal

```
signal syscall or trap-generated signal
    enqueue pending
    wake interruptible target if applicable
trap_return()
    do_signal()
        choose signal
        build signal frame
        modify trap context to handler
handler returns
    rt_sigreturn
        restore mask and machine context
```

`SIGKILL` 和 `SIGSTOP` 不可屏蔽。发送权限由 `can_signal_process()` 判断。

### 5.10 futex

```
sys_futex()
    validate uaddr
    parse cmd/options
    select key:
        private -> virtual address
        shared VMA -> physical address
    Wait/Wake/Requeue/CmpRequeue/WaitBitset/WakeBitset
```

private futex 表在 PCB 中；shared futex 表是全局 `PROCESS_SHARED_FUTEX`。调度主循环周期性压缩 shared futex 表。

### 5.11 IPC 和 MQ

| 类别 | syscall |
|------|---------|
| SysV shm | `shmget`, `shmat`, `shmdt`, `shmctl` |
| SysV sem | `semget`, `semctl`, `semtimedop`, `semop` |
| SysV msg | `msgget`, `msgsnd`, `msgrcv`, `msgctl` |
| POSIX MQ | `mq_open`, `mq_unlink`, `mq_timedsend`, `mq_timedreceive`, `mq_notify`, `mq_getsetattr` |

`ProcessInner::ipc` 保存 IPC namespace。`CLONE_NEWIPC`、`unshare(CLONE_NEWIPC)` 和 `setns` 可创建或切换 IPC namespace 对象。

### 5.12 TCB 与 PCB 为什么分开

`TaskControlBlock` 和 `ProcessControlBlock` 的分层是阅读进程代码的关键。可以按“谁被调度”和“谁持有资源”区分：

| 问题 | 所属对象 | 原因 |
|------|----------|------|
| 当前运行到哪个 trap context | TCB | 每个线程有独立寄存器上下文和内核栈。 |
| 当前线程是否 Ready/Running/Interruptible/Zombie | TCB | 调度器调度的是线程。 |
| `gettid()` 返回什么 | TCB | TID 是线程级 ID。 |
| fd table、cwd/root、umask | PCB | 同一进程内线程可以通过 `CLONE_FILES/CLONE_FS` 共享这些资源。 |
| 地址空间 | PCB | `CLONE_VM` 决定线程共享 VM，fork 子进程复制或 CoW 继承 VM。 |
| children、parent、exit_code | PCB | wait 语义是进程关系，不是单个线程关系。 |
| signal action 表 | PCB | `CLONE_SIGHAND` 控制共享；线程仍有自己的 mask 和 pending。 |
| futex private table | PCB | private futex key 按进程地址空间解释。 |

因此，看到 `current_task()` 之后通常要判断代码接下来操作的是线程状态还是进程资源。比如 `sys_gettid()` 只读当前 TCB 的 tid；`sys_getpid()` 读当前 TCB 所属 PCB 的 pid；`sys_read()` 通过 PCB 的 files 查 fd；`sys_sigprocmask()` 修改 TCB 的 mask；`sys_sigaction()` 修改 PCB 的 sighand。

### 5.13 `run_tasks()` 循环源码解析

调度循环每轮都做“后台推进 + 选择下一个任务 + 上下文切换”：

```
loop {
    poll console input;
    do_wake_expired();
    NET_INTERFACE.try_poll() periodically;
    fs::reclaim::maybe_reclaim_fs_caches();
    drain zombie queue;
    cleanup stale zombies;
    compact shared futex;
    if let Some(task) = fetch_task() {
        mark Running;
        schedule_in accounting;
        __switch(idle, task);
    } else {
        idle poll/spin;
    }
}
```

几个实现细节会影响其他子系统：

| 细节 | 影响 |
|------|------|
| 控制台轮询在 rv64 上降频 | SBI getchar 成本高，调度循环按 `RV64_CONSOLE_POLL_INTERVAL` 控制频率。 |
| `do_wake_expired()` 保留 legacy sweep | 一些早期等待路径仍依赖调度循环扫描 timeout。 |
| 网络 poll 周期执行 | socket 阻塞路径之外，网络状态机也能被后台推进。 |
| PageCache reclaim 在调度循环调用 | 没有独立写回线程时，调度循环提供合作式回收入口。 |
| zombie queue 在 idle 后 drain | 退出任务切走后再 drop，避免释放仍在使用的内核栈。 |
| `fetch_task()` 才真正选择用户任务 | 维护动作做完后才进入 ready queue 选择。 |

这也是为什么 `run_tasks()` 是进程、网络、文件系统和 timer 的共同枢纽，不只是一个队列 pop 函数。

### 5.14 `finish_exit()` 的进程级退出

线程退出先进入 task 层；当最后一个 live thread 退出时，PCB 的 `finish_exit()` 完成进程级收尾。源码顺序可以概括为：

```
complete_vfork()
统计 rusage / resident maxrss
mark_zombie(exit_code, rusage)
读取 parent 和 SIGCHLD auto-reap 策略
注销 exec_key
取出 children 并交给 child reaper
唤醒 parent.child_exit_wait
必要时向 parent 投递 exit_signal
释放 zombie VM 页和 close-on-exit 文件
```

逐项解释：

| 步骤 | 作用 |
|------|------|
| `complete_vfork()` | 如果父线程因 `CLONE_VFORK` 等待，子进程 exec 或 exit 时必须释放父线程。 |
| `mark_zombie()` | 将进程状态改为 Zombie，并保存 wait 可见的 exit code/rusage。 |
| `sigchld_requests_auto_reap()` | 父进程设置忽略 SIGCHLD 或 SA_NOCLDWAIT 时，部分子进程可自动回收。 |
| `take_children()` | 当前进程的子进程需要被最近 child reaper 收养，避免孤儿失去 wait 归属。 |
| `child_exit_wait.wake_all()` | 正在 `wait4/waitid` 中睡眠的父进程通过这个队列被唤醒。 |
| `exit_signal` | 非线程 clone 的退出信号通常是 SIGCHLD；线程组内线程退出不走同一进程 wait 语义。 |
| `release_for_zombie()` | 当 VM 引用只剩进程/zombie 路径时释放用户页，降低 zombie 占用。 |
| `close_files_on_exit()` | 进程退出关闭 fd table 中需要关闭的文件对象。 |

这条路径把 exit 和 wait 连在一起：exit 不是直接删除 PCB，而是把 PCB 变成 parent 可观察的 zombie；wait 成功消费后才释放 pid、解除父子关系并完成最终回收。

### 5.15 从 shell 执行命令到返回 wait

把 clone、exec、调度和 wait 连起来，可以得到一条完整用户可见路径：

```
shell fork/clone
  -> sys_clone_inner()
  -> TaskControlBlock::sys_clone()
  -> publish_clone_child()
  -> schedule_clone_child()

child execve
  -> sys_execve()
  -> TaskControlBlock::load_elf()
  -> AddressSpace::from_elf()
  -> 替换 VM / fd CLOEXEC / signal 状态

child exit
  -> sys_exit_group() 或 sys_exit()
  -> do_exit()
  -> ProcessControlBlock::finish_exit()

parent wait
  -> sys_wait4()/sys_waitid()
  -> 扫描 children 或睡眠在 child_exit_wait
  -> 读取 exit_code/rusage
```

读源码时，可以先从 syscall 文件找到参数校验，再进入 task/process 对象：

| 阶段 | 入口 | 深入对象 |
|------|------|----------|
| clone 参数 | `syscall/process/clone.rs` | `TaskControlBlock::sys_clone()` |
| exec 参数 | `syscall/process/exec.rs` | `TaskControlBlock::load_elf()`、`AddressSpace::from_elf()` |
| exit syscall | `syscall/process/lifecycle.rs` | `do_exit()`、`finish_exit()` |
| wait syscall | `syscall/process/lifecycle.rs` | PCB children、`child_exit_wait` |

这条路径也是 LTP、busybox shell、pthread 和 libc 进程测试共同覆盖的主线。

## 6. 接口与 API

### 6.1 clone/namespace API

| API | 说明 |
|-----|------|
| `sys_clone()` | raw clone 入口 |
| `sys_clone3()` | clone3 结构体入口 |
| `ProcessManager::publish_clone_child()` | 发布父子关系 |
| `ProcessManager::schedule_clone_child()` | 加入调度或处理 vfork |
| `sys_unshare()` | 拆分当前进程资源 |
| `sys_setns()` | 切换 namespace |

### 6.2 exec API

| API | 说明 |
|-----|------|
| `sys_execve()` | 路径 exec |
| `sys_execveat()` | fd/dirfd exec |
| `TaskControlBlock::load_elf()` | 替换进程执行映像 |
| `AddressSpace::from_elf()` | 构造 ELF 地址空间 |

### 6.3 wait/exit API

| API | 说明 |
|-----|------|
| `sys_exit()` | 当前线程退出 |
| `sys_exit_group()` | 线程组退出 |
| `do_exit()` | 线程/进程退出主路径 |
| `ProcessControlBlock::finish_exit()` | 进程级退出完成 |
| `sys_wait4()` | wait4 |
| `sys_waitid()` | waitid |

### 6.4 signal/pidfd API

| API | 说明 |
|-----|------|
| `sys_kill/tkill/tgkill` | 发送信号 |
| `sys_sigaction` | handler 表 |
| `sys_sigprocmask` | mask |
| `sys_sigreturn` | 恢复上下文 |
| `sys_signalfd4` | signalfd |
| `sys_pidfd_open/send_signal/getfd` | pidfd |
| `sys_kcmp` | 进程对象比较 |

### 6.5 time/sched/rlimit API

| API | 说明 |
|-----|------|
| `sys_nanosleep`, `sys_clock_nanosleep` | 睡眠 |
| `sys_clock_*`, `sys_timer_*` | clock/POSIX timer |
| `sys_getrusage`, `sys_times` | 统计 |
| `sys_sched_*` | 调度 ABI |
| `sys_getrlimit`, `sys_setrlimit`, `sys_prlimit` | rlimit |
| `sys_prctl`, `sys_capget`, `sys_capset` | prctl/capability |

## 7. 测试映射

| 功能 | 测试来源 |
|------|----------|
| init 进程和 exec | busybox、shell、LTP exec |
| clone/fork/thread | pthread、LTP clone、libctest |
| wait/exit | LTP wait、shell job 子进程 |
| signal | LTP signal、tgkill、sigaltstack、signalfd |
| futex | pthread mutex/cond、LTP futex、libcbench |
| scheduler | busybox 并发、unixbench、cyclictest |
| time/timer | nanosleep、timerfd、clock_gettime、cyclictest |
| IPC | SysV IPC、POSIX MQ LTP |
| rlimit/sched ABI | LTP sched、rlimit、prctl |

## 8. 已知边界

| 边界 | 说明 |
|------|------|
| 单核调度 | affinity/sched ABI 字段可保存和回读，真实运行仍由单核 ready queue 驱动 |
| namespace | net/mnt/ipc namespace 对象可切换，隔离能力以对应 namespace 实现为准 |
| signal | trap return 前统一交付；`rt_sigreturn` 由 trap 后端特殊处理 |
| futex PI/WakeOp | `FutexCmd` 枚举存在，未接入的命令分支返回 `EINVAL` |
| core dump | rlimit core 字段保留 ABI 状态，不生成 core 文件 |
| vfork | completion 由 exec 成功或 exit 路径释放父线程 |

## 9. 源文件索引

| 路径 | 内容 |
|------|------|
| `os/src/task/task.rs` | TCB、clone、exec、退出资源 |
| `os/src/task/process.rs` | PCB、finish_exit、父子关系 |
| `os/src/task/manager.rs` | TaskManager、WaitQueue、Completion、timer |
| `os/src/task/processor.rs` | run_tasks、schedule、current task |
| `os/src/task/signal/` | signal 核心 |
| `os/src/task/threads.rs` | futex 表 |
| `os/src/syscall/process/clone.rs` | clone/unshare/setns |
| `os/src/syscall/process/exec.rs` | execve/execveat |
| `os/src/syscall/process/lifecycle.rs` | exit/wait/robust list |
| `os/src/syscall/process/signal.rs` | signal/pidfd/signalfd/kcmp |
| `os/src/syscall/process/futex.rs` | futex syscall |
| `os/src/syscall/process/ipc.rs` | IPC/MQ |
| `os/src/syscall/process/time.rs` | time/timer |
| `os/src/syscall/process/ids.rs` | ids/sched/rlimit/prctl/cap |
