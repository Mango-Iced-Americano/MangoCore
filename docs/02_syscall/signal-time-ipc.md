---
title: "信号、时间与 IPC syscall"
category: syscall
status: stable
author: MangoCore Team
last_update: 2026-06-29
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

源码主路径如下：

```rust
pub fn sys_signalfd4(fd: usize, mask: usize, sigsetsize: usize, flags: usize) -> isize {
    if flags & !SFD_VALID_FLAGS != 0 {
        return EINVAL;
    }

    let (token, files_ref) = {
        let task = current_task_ref().unwrap();
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

`setitimer(which, new, old)` 支持 `which <= 2`。`which == 0` 的 real timer 使用 `KernelTimer`：

| 行为 | 说明 |
|------|------|
| old 非 NULL | 写回旧 timer；real timer 的 remaining 按 deadline - now 计算 |
| new 为 NULL | 只读取旧值 |
| new.it_value 为 0 | 清除 real timer deadline |
| new.it_value 非 0 | 设置 deadline，注册 `TimerAction::SendSignal(SIGALRM)` |

代码维护 `real_timer_generation`，避免旧 timer 触发新一代 timer 的 SIGALRM。

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

`sevp` 为 NULL 时默认 `SIGALRM`。写回 timerid 失败时，会回滚刚分配的 timer slot。

`timer_create()` 源码如下：

```rust
pub fn sys_timer_create(
    clock_id: usize,
    sevp: *const SigeventHeader,
    timerid: *mut i32,
) -> isize {
    if timerid.is_null() {
        return EFAULT;
    }
    if !valid_posix_timer_clock(clock_id) {
        return EINVAL;
    }

    let token = current_user_token();
    let signal = if sevp.is_null() {
        Signals::SIGALRM
    } else {
        let event = match UserPtr::new(sevp).read(token) {
            Ok(event) => event,
            Err(errno) => return errno,
        };
        match event.sigev_notify {
            SIGEV_SIGNAL => match Signals::from_signum(event.sigev_signo as usize) {
                Ok(signal) => signal,
                Err(_) => return EINVAL,
            },
            SIGEV_NONE => Signals::empty(),
            _ => return EINVAL,
        }
    };

    let task = current_task_ref().unwrap();
    let id = {
        let mut inner = task.acquire_inner_lock();
        if let Some((id, slot)) = inner
            .posix_timers
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| slot.is_none())
        {
            *slot = Some(PosixTimer::new(clock_id, signal));
            id
        } else {
            if inner.posix_timers.len() >= MAX_POSIX_TIMERS {
                return EAGAIN;
            }
            if inner.posix_timers.try_reserve(1).is_err() {
                return ENOMEM;
            }
            inner
                .posix_timers
                .push(Some(PosixTimer::new(clock_id, signal)));
            inner.posix_timers.len() - 1
        }
    };

    match UserPtrMut::new(timerid).write(token, &(id as i32)) {
        Ok(()) => SUCCESS,
        Err(errno) => {
            let mut inner = task.acquire_inner_lock();
            if let Some(slot) = inner.posix_timers.get_mut(id) {
                *slot = None;
            }
            errno
        }
    }
}
```

### 8.2 set/get/delete

| syscall | 行为 |
|---------|------|
| `timer_settime` | flags 只能包含 `TIMER_ABSTIME`；new_value 不能为 NULL；校验 interval/value timespec |
| `timer_gettime` | 写出当前 timer spec |
| `timer_getoverrun` | 返回 overrun 计数 |
| `timer_delete` | 删除 timer slot |

POSIX timer 投递信号使用 task timer 队列。

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
