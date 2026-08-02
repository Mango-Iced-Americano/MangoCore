---
title: "信号、时间与 IPC syscall"
category: syscall
status: stable
author: MangoCore Team
last_update: 2026-08-02
tags: [syscall, signal, time, ipc]
---

# 信号、时间与 IPC syscall

## 1. 概述

信号、时间和 IPC syscall 都通过 `syscall/mod.rs` 注册，但实现分布在不同模块：

| 文件 | 范围 |
|------|------|
| `syscall/process/signal.rs` | signal、pidfd、signalfd、kcmp、ptrace 相关入口 |
| `task/signal/` | 信号 pending、action、delivery、frame、wait |
| `syscall/process/time.rs` | nanosleep、itimer、POSIX timer、clock、gettimeofday、rusage |
| `fs/timerfd.rs` | timerfd fd |
| `syscall/process/ipc.rs` | SysV shm/sem/msg 和 POSIX message queue |

三类 syscall 都高度依赖用户指针读写和等待队列：信号等待、timer 超时、IPC 阻塞收发都会在 task 层睡眠并被信号打断。

## 2. signal 权限与公共结构

`signal.rs` 中 `can_signal_process()` 定义发送权限：

| 条件 | 允许 |
|------|------|
| 发送者 pid 等于目标 pid | 是 |
| 发送者 euid 为 0 | 是 |
| 目标无 live thread | 是 |
| 发送者 uid/euid 等于目标 uid/suid | 是 |

否则不允许发送信号。目标身份来自目标任一 live thread 的 `TaskControlBlockInner`。

`rt_sigreturn` 由 syscall 分支进入 `sys_sigreturn()`，但 trap 后端不会把普通返回值覆盖到 `a0`，因为该路径恢复 signal frame 中保存的用户上下文。

## 3. signalfd

### 3.1 文件对象

`signal.rs` 定义 `SignalFd`：

```rust
struct SignalFd {
    mask: Mutex<Signals>,
    metadata: Metadata,
}
```

`SignalfdSiginfo` 是 128 字节风格的用户可见结构，字段包括 signo、errno、code、pid、uid、value、syscall、arch 等。

### 3.2 read 语义

`SignalFd::read_at()`：

| 检查/行为 | 结果 |
|-----------|------|
| len 或 buf 小于 `size_of::<SignalfdSiginfo>()` | `EINVAL` |
| 每个 slot 调用 `take_pending_signal_matching(task, mask)` | 取出匹配 pending signal |
| 没有写入任何 siginfo | `EAGAIN` |
| 成功 | 返回写入字节数，单位为 siginfo 大小 |

### 3.3 syscall

`sys_signalfd4(fd, mask, sigsetsize, flags)` 支持 `SFD_NONBLOCK` 和 `SFD_CLOEXEC`。flags 含其他位返回 `EINVAL`。fd 为已有 signalfd 时更新 mask；fd 为新建时创建文件对象并分配 fd。

读取 `SI_TIMER` 时，`signalfd_siginfo.ssi_tid` 返回 timer ID，`ssi_overrun` 返回本次实际领取
时固化的 overrun；timer 的联合字段不再同时伪装成 sender pid/uid，因此这两项固定为 0。

源码主路径如下：

```rust
pub fn sys_signalfd4(fd: usize, mask: usize, sigsetsize: usize, flags: usize) -> isize {
    if flags & !SFD_VALID_FLAGS != 0 {
        return EINVAL;
    }

    let (token, files_ref) = {
        let task = current_task().unwrap();
        (current_user_token(), task.process.files())
    };
    let sigmask = match read_signalfd_mask(token, mask, sigsetsize) {
        Ok(mask) => mask,
        Err(errno) => return errno,
    };

    let fd_signed = fd as isize;
    if fd_signed == -1 {
        let mut file_flags = FileFlags::O_RDWR;
        if flags & SFD_NONBLOCK != 0 {
            file_flags.insert(FileFlags::O_NONBLOCK);
        }
        if flags & SFD_CLOEXEC != 0 {
            file_flags.insert(FileFlags::O_CLOEXEC);
        }

        let inode = Arc::new(SignalFd::new(sigmask)) as Arc<dyn IndexNode>;
        let file = match File::new(inode, file_flags) {
            Ok(file) => file,
            Err(err) => return -(err as isize),
        };
        let mut fd_table = files_ref.lock();
        return match fd_table.alloc_fd(file, flags & SFD_CLOEXEC != 0) {
            Ok(new_fd) => new_fd as isize,
            Err(err) => -(err as isize),
        };
    }

    if fd_signed < 0 {
        return EBADF;
    }
    /* fd >= 0 时继续校验目标 fd 是否为 SignalFd，并更新 mask。 */
}
```

新建路径设置 file flags 和 fd table 的 close-on-exec 位；更新路径要求 fd 指向已有 `SignalFd` inode。

## 4. kill/tkill/tgkill 与 queued signal

| syscall | 入口 | 语义 |
|---------|------|------|
| `kill(pid, sig)` | `sys_kill` | pid 正数/0/负数/广播语义 |
| `tkill(tid, sig)` | `sys_tkill` | 按 tid 发送 |
| `tgkill(pid, tid, sig)` | `sys_tgkill` | pid+tid 双重定位 |
| `rt_sigqueueinfo(pid, sig, info)` | `sys_rt_sigqueueinfo` | 从用户 siginfo 读取 sender/value |

无效 signal number、权限不足、目标不存在会按对应分支返回 `EINVAL`、`EPERM` 或 `ESRCH`。实际 pending 队列和投递由 `task/signal/` 子模块处理。

进程 shared pending 另有 `shared_pending_hint` 供高频路径无锁判断，但 hint 不是第二份状态：
signal queue 仍是权威 owner。所有 enqueue/dequeue/精确 timer 清理都必须在同一个 process
signal 临界区内更新队列并以 Release 发布最新位图，读端以 Acquire 取得。不能先解锁再写
hint；否则旧消费者可能在新生产者之后写回过期的空位图，使队列非空而 fast path 长期跳过它。

## 5. pidfd 与 kcmp

### 5.1 pidfd target

`pidfd_file_target_pid(file)` 支持两类 inode：

| inode | 行为 |
|-------|------|
| `PidFd` | 调 `target_pid()`，pid released 时返回错误 |
| `LockedProcInode` 且 file_type 为目录、pid 非 0 | 从 proc inode extra data/process_ref 推导 pid |

其他 fd 返回 `EBADF`。

### 5.2 syscall

| syscall | 行为 |
|---------|------|
| `pidfd_open(pid, flags)` | 创建 pidfd file；flags 由实现校验 |
| `pidfd_getfd(pidfd, targetfd, flags)` | 取得目标进程 fd 的复制 |
| `pidfd_send_signal(pidfd, sig, info, flags)` | 通过 pidfd 向目标进程发信号 |
| `kcmp(pid1, pid2, type, idx1, idx2)` | 比较 file/vm/files/fs/sighand/io/sysvsem 等对象 |

`waitid(P_PIDFD, ...)` 不在 signal.rs 中实现，但会调用 `pidfd_file_target_pid()` 取得目标 pid。

## 6. sigaction、sigmask 和等待

| syscall | 入口 | 说明 |
|---------|------|------|
| `rt_sigaction` | `sys_sigaction` | 读取/写入信号 action |
| `rt_sigprocmask` | `sys_sigprocmask` | 修改线程 signal mask |
| `rt_sigpending` | `sys_rt_sigpending` | 写出 pending set |
| `rt_sigtimedwait` | `sys_sigtimedwait` | 等待指定信号集合，可带 timeout |
| `rt_sigsuspend` | `sys_rt_sigsuspend` | 临时替换 mask 并睡眠 |
| `sigaltstack` | `sys_sigaltstack` | 设置或读取备用信号栈 |

这些入口都使用 `UserPtr`/`UserPtrMut` 或 `copy_from_user` 访问用户结构。

`rt_sigtimedwait` 先把用户 `set/timeout` 复制到内核栈，再设置当前 TCB 的
`signal_wait_mask`。条件检查从线程私有或进程共享 pending 队列中唯一领取一条
`PendingSignal`，但不在条件闭包内写用户地址；等待路径退出并清除 wait mask 后，才把
`SigInfo` 写回用户态。这样即使条件检查发生在 WaitQueue 锁内，缺页、CoW 和远端 TLB
shootdown 也不会跨越等待队列锁。若 `info` 无效，返回 `EFAULT`，已领取信号仍保持消费。

为关闭“第二次条件检查完成、任务尚未登记为 Blocking”的丢唤醒窗口，调度器在登记睡眠意图
后还会检查 waited pending；发送方在窗口内即使看到任务仍为 Running，接收方也不会随后睡下。
WaitQueue 以 Interrupted 或 TimedOut 返回时，syscall 在清除 wait mask 前再领取一次：若目标
signal 已经 pending，则返回该 signal；否则才分别返回 `EINTR` 或 `EAGAIN`。通用 ignored-signal
清理必须跳过 wait mask 中的 signal，避免在这个窗口抢先消费它。

## 7. nanosleep 与 itimer

### 7.1 nanosleep

`sys_nanosleep(req, rem)`：

```
UserPtr(req).read()
is_valid_timespec(req)
sleep_relative_interruptible(req)
  Ok -> 0
  Interrupted -> rem 非 NULL 时写 remaining，返回 EINTR
```

无效 timespec 返回 `EINVAL`；用户指针错误返回 uaccess errno。

`sys_nanosleep()` 源码如下：

```rust
pub fn sys_nanosleep(req: *const TimeSpec, rem: *mut TimeSpec) -> isize {
    let token = current_user_token();
    let req = match UserPtr::new(req).read(token) {
        Ok(req) => req,
        Err(errno) => return errno,
    };
    if !is_valid_timespec(req) {
        return EINVAL;
    }

    match sleep_relative_interruptible(req) {
        Ok(()) => SUCCESS,
        Err(interrupted) => {
            if !rem.is_null() {
                if let Err(errno) = UserPtrMut::new(rem).write(token, &interrupted.remaining) {
                    return errno;
                }
            }
            EINTR
        }
    }
}
```

### 7.2 setitimer/getitimer

`setitimer(which, new, old)` 支持 `which <= 2`。三个 timer 由线程组共享的 PCB 表持有：

| 行为 | 说明 |
|------|------|
| old 非 NULL | 写回旧 timer；real timer 的 remaining 按 deadline - now 计算 |
| new 为 NULL | 按 Linux 历史兼容语义停表 |
| new.it_value 为 0 | 清除 real timer deadline |
| new.it_value 非 0 | 设置 deadline；REAL 注册 `TimerAction::IntervalTimerSignal` |

REAL action 保存 PCB `Weak` 和 generation，避免旧 heap 节点触发新一代 timer；其时钟是
monotonic elapsed time，不受 `settimeofday/clock_settime` 的墙钟跳变影响。VIRTUAL/PROF
分别读取线程组 user CPU 与 user+system CPU 累计，并在 trap-return、schedule-out 安全点
领取到期。

内核先完整读取新值并冲刷本线程 CPU 记账尾数，再在同一个 `IntervalTimerTable` 临界区
快照旧 timer 并提交新配置；锁外完成 `KernelTimer` 注册后才写回旧值。old copyout 返回
`EFAULT` 时不回滚已经生效的新配置。remaining 只按绝对 deadline 与当前时钟采样计算，
不再由 syscall、trap 和等待路径分别扣减。

## 8. POSIX timer

### 8.1 timer_create

`sys_timer_create(clock_id, sevp, timerid)`：

| 检查 | errno |
|------|-------|
| timerid NULL | `EFAULT` |
| clock id 不支持 | `EINVAL` |
| `sigev_notify` 非 `SIGEV_SIGNAL`/`SIGEV_NONE` | `EINVAL` |
| signal number 无效 | `EINVAL` |
| timer 槽达到上限 | `EAGAIN` |
| `Vec::try_reserve` 失败 | `ENOMEM` |

timer 表由 PCB 独占并由同一线程组共享，普通 fork 得到空表，exec 和最后线程退出时
清空。`sevp` 为 NULL 时默认投递 `SIGALRM`，默认 `si_value` 是 timer ID；显式
`sigev_value` 会原样进入 `SI_TIMER` 的 `SigInfo`。

创建采用 `Vacant -> Reserved -> Active` 三态发布：先在表锁内保留 ID，释放锁后写回
用户 `timerid`，写回成功才重新持锁发布对象；copyout 失败则撤销预留。并发线程既不会
抢到同一 ID，也不能查询到半初始化 timer，且用户访存不会发生在 timer 表锁内。

### 8.2 set/get/delete

| syscall | 行为 |
|---------|------|
| `timer_settime` | flags 只能包含 `TIMER_ABSTIME`；new_value 不能为 NULL；校验 interval/value timespec |
| `timer_gettime` | 写出当前 timer spec |
| `timer_getoverrun` | 返回最近一次已交付 timer 事件的 overrun |
| `timer_delete` | 删除 timer slot |

`timer_settime()` 同样采用“copyin → 锁内快照并提交 → 锁外注册 → copyout”的顺序；旧值
写回失败不撤销已经发布的新配置或立即到期信号。

墙钟类 timer 通过全局 `KernelTimerQueue` 驱动，action 只持 PCB `Weak`、timer ID 和
`arm_seq`。回调必须同时匹配 ID、装载序号和 deadline 才能提交到期结果，因此 rearm、exec
和退出遗留的旧 heap 节点不能修改当前设置。timer 对象另有 `instance_seq`，用于阻止
delete/recreate 后旧 pending 事件命中复用相同 ID 的新对象。

回调在 timer 表锁内更新状态并构造至多一个精确 timer 事件，释放表锁后才进入进程 shared
pending、唤醒目标线程或重装周期节点。同一 timer 在事件尚未领取时重复到期只累计 overrun；
不同 timer 即使使用同一个非实时 signal，也保留各自队列项。signal dequeue 后先释放 signal
锁，再回到 timer owner 固化本次 `SigInfo` 和 `timer_getoverrun()` 快照，不形成双锁嵌套。

`CLOCK_PROCESS_CPUTIME_ID`/`CLOCK_THREAD_CPUTIME_ID` 不进入 wall-time heap：前者按 PCB
线程组累计推进，后者按创建线程的稳定 TCB 身份推进；trap-return 和 schedule-out 安全点
在 timer 表锁内唯一领取到期，再锁外投递上述同一类精确事件。

## 9. clock 与 timeval

| syscall | 入口 |
|---------|------|
| `clock_gettime` | `sys_clock_gettime` |
| `clock_settime` | `sys_clock_settime` |
| `clock_getres` | `sys_clock_getres` |
| `clock_nanosleep` | `sys_clock_nanosleep` |
| `gettimeofday` | `sys_gettimeofday` |
| `settimeofday` | `sys_settimeofday` |
| `adjtimex` | `sys_adjtimex` |
| `clock_adjtime` | `sys_clock_adjtime` |
| `times` | `sys_times` |
| `getrusage` | `sys_getrusage` |
| `getcpu` | `sys_getcpu` |

`time.rs` 维护 `TIMEX_STATE`，用于 adjtimex/clock_adjtime 的状态。

## 10. timerfd

timerfd syscall 不在 `process/time.rs` 中实现，而是注册到 `fs/timerfd.rs`：

| syscall | 入口 |
|---------|------|
| `timerfd_create` | `sys_timerfd_create(clock_id, flags)` |
| `timerfd_settime` | `sys_timerfd_settime(fd, flags, new, old)` |
| `timerfd_gettime` | `sys_timerfd_gettime(fd, curr)` |

timerfd 是 VFS `File` 对象，可被 poll/epoll 观察。

## 11. SysV IPC

`syscall/process/ipc.rs` 覆盖三类 SysV IPC：

| 类别 | syscall |
|------|---------|
| shared memory | `shmget`, `shmat`, `shmdt`, `shmctl` |
| semaphore | `semget`, `semctl`, `semtimedop`, `semop` |
| message queue | `msgget`, `msgsnd`, `msgrcv`, `msgctl` |

进程持有 IPC namespace。`clone/unshare/setns` 可创建或切换 IPC namespace；SysV IPC 对象表和限制值由该 namespace 维护。

`semctl(GETALL/SETVAL/SETALL)` 不在 `SEM_REGISTRY` 锁内访问用户页。GETALL 先在锁内
快照数值，再锁外写回；SETVAL/SETALL 先验证 semid、权限和集合长度，锁外解析参数或读取
数组，再锁内重验并提交。GETALL/SETALL 按 Linux ABI 忽略 `semnum`。这一顺序既避免缺页
或 TLB shootdown 时占住 IPC 全局锁，也保留既有的参数错误优先级。

普通 `msgrcv` 在一个 `MSG_REGISTRY` 临界区内完成消息选择和 `VecDeque::remove`，并同时
更新队列字节数、最近接收 PID/时间以及 sender wake；随后才在锁外写用户 buffer。因此并发
receiver 不能领取同一条消息。`MSG_COPY` 不执行摘取，只在锁内复制稳定的内核快照。普通
接收的用户 copy 若失败，消息不会放回队列；这与 Linux 先取得消息所有权、再执行用户 copy
的语义一致。

message queue 的 `/proc/sys/kernel/msg_next_id` 是一次性 requested ID，不是自动分配器的
cursor。自动 ID 另用单调 cursor，并跳过所有已发布历史；requested ID 即使已经删除也不能
再次发布。发布前先为历史预留容量并登记，之后才把队列插入 registry；`IPC_RMID` 只删除和
唤醒，不创建 tombstone。这样跨 WaitQueue 等待的旧 `msqid` 不会发生数值 ABA：醒来时对象
不存在返回 `EIDRM`，不会误操作删除后创建的同号队列。

semaphore 使用更小的证明：初次查找失败返回 `EINVAL`；只有同一 registry 锁下已确认对象
存在且需要阻塞后，等待条件才有资格把后续缺失解释为 `IPC_RMID` 并返回 `EIDRM`。其 ID
同样单调不复用，所以不维护可能在删除路径分配失败的 tombstone。shared-memory ID 也改为
checked 单调游标；耗尽返回 `ENOSPC`，避免 `shmat` 的“锁外建 VMA、锁内登记 attachment”
两阶段路径在 ID 回绕后命中新段。message queue 仍单独维护 requested ID 发布历史。

## 12. POSIX message queue

POSIX MQ 入口：

| syscall | 入口 |
|---------|------|
| `mq_open` | `sys_mq_open(name, oflag, mode, attr)` |
| `mq_unlink` | `sys_mq_unlink(name)` |
| `mq_timedsend` | `sys_mq_timedsend(mqdes, msg_ptr, msg_len, prio, timeout)` |
| `mq_timedreceive` | `sys_mq_timedreceive(mqdes, msg_ptr, msg_len, prio, timeout)` |
| `mq_getsetattr` | `sys_mq_getsetattr(mqdes, newattr, oldattr)` |
| `mq_notify` | `sys_mq_notify(mqdes, sevp)` |

`syscall/mod.rs` 还导出 POSIX MQ 和 SysV IPC 限制的 getter/setter，用于 proc/sysctl 风格状态。

`mq_open(O_CREAT)` 采用两阶段名称表查找：已有队列直接克隆 `Arc` 且不读取 attr；确需
创建时在 `MQ_REGISTRY` 锁外复制 attr，再重新加锁原子处理同名创建和 `O_EXCL`。名称表锁
释放后才检查 queue inner 权限，因此不会形成 `MQ_REGISTRY -> MqQueue.inner -> VM` 链。

## 13. 错误码边界

| 场景 | errno |
|------|-------|
| signal 权限不足 | `EPERM` |
| signal 目标不存在 | `ESRCH` |
| signalfd read buffer 小于 siginfo | `EINVAL` |
| signalfd 无匹配 pending signal | `EAGAIN` |
| pidfd fd 类型不对 | `EBADF` |
| nanosleep timespec 无效 | `EINVAL` |
| nanosleep 被信号打断 | `EINTR`，可写 rem |
| timer_create timerid NULL | `EFAULT` |
| timer_create 不支持 clock/notify/signal | `EINVAL` |
| POSIX timer 数量达到上限 | `EAGAIN` |
| timer_settime new_value NULL | `EINVAL` |
| SysV IPC id/key/权限错误 | `ipc.rs` 对应分支返回具体 errno |

signal/time/IPC 的共同特点是对象状态不一定在 syscall 返回时立即消失。signal 可能只是进入 pending 队列，真正构造用户 signal frame 在 trap return 前发生；timer_create/settime 注册的是 kernel timer action，到期后才投递信号；SysV IPC 对象存放在当前 IPC namespace 的 registry 中，进程退出时还要处理 shm attachment。

调试这类 syscall 时要沿“注册状态 -> 唤醒/投递 -> 用户可见结果”三段看。`kill` 返回成功只表示信号进入目标 pending；`nanosleep` 阻塞需要 timer queue 到期或信号打断；`msgsnd/msgrcv` 可能通过 WaitQueue 睡眠，队列删除或消息到达都会改变等待结果。

## 14. 测试映射

| 功能 | 代表测试 |
|------|----------|
| signal | LTP `kill*`, `tkill*`, `tgkill*`, `rt_sig*`, `sigaltstack*` |
| signalfd/pidfd | LTP `signalfd*`, `pidfd_*`, `waitid*` |
| nanosleep/clock | LTP `nanosleep*`, `clock_*`, `gettimeofday*` |
| POSIX timer/timerfd | LTP `timer_*`, `timerfd_*` |
| SysV IPC | LTP `msg*`, `sem*`, `shm*` |
| POSIX MQ | LTP `mq_*` |

## 15. 源文件索引

| 路径 | 内容 |
|------|------|
| `os/src/syscall/process/signal.rs` | signal、pidfd、signalfd、kcmp |
| `os/src/task/signal/` | 信号动作、pending、delivery、frame、wait |
| `os/src/syscall/process/time.rs` | time、itimer、POSIX timer、rusage |
| `os/src/fs/timerfd.rs` | timerfd |
| `os/src/syscall/process/ipc.rs` | SysV IPC 和 POSIX MQ |
| `os/src/task/manager.rs` | kernel timer queue、timeout 和 timer interrupt handler |
