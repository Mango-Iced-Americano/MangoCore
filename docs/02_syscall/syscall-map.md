---
title: "已注册系统调用表 (Registered Syscall Map)"
category: syscall
status: stable
author: MangoCore Team
last_update: 2026-06-29
tags: [syscall, table, reference]
---

# 已注册系统调用表

## 说明

系统调用表按 `os/src/syscall/mod.rs` 的实际 `match` 分支整理。编号来自 `os/src/syscall/syscall_id.rs`。同一领域内仅列已进入分发的 syscall；网络 syscall 的详细参数语义见 `docs/06_net/syscall-layer.md`。

## 文件系统、fd 与事件

| 编号 | syscall | 入口 |
|------|---------|------|
| 5-16 | `setxattr`, `lsetxattr`, `fsetxattr`, `getxattr`, `lgetxattr`, `fgetxattr`, `listxattr`, `llistxattr`, `flistxattr`, `removexattr`, `lremovexattr`, `fremovexattr` | `syscall/fs.rs` |
| 17 | `getcwd` | `syscall/fs.rs` |
| 19 | `eventfd2` | `fs/eventfd.rs` |
| 20-22, 441 | `epoll_create1`, `epoll_ctl`, `epoll_pwait`, `epoll_pwait2` | `fs/eventpoll.rs` |
| 23-25 | `dup`, `dup3`, `fcntl` | `syscall/fs.rs` |
| 29-32 | `ioctl`, `ioprio_set`, `ioprio_get`, `flock` | `syscall/fs.rs`, process misc |
| 33-40 | `mknodat`, `mkdirat`, `unlinkat`, `symlinkat`, `linkat`, `umount2`, `mount` | `syscall/fs.rs` |
| 43-48 | `statfs`, `fstatfs`, `truncate`, `ftruncate`, `fallocate`, `faccessat` | `syscall/fs.rs` |
| 49-58 | `chdir`, `fchdir`, `chroot`, `fchmod`, `fchmodat`, `fchownat`, `fchown`, `openat`, `close`, `vhangup` | `syscall/fs.rs` |
| 59, 61-76 | `pipe2`, `getdents64`, `lseek`, `read`, `write`, `readv`, `writev`, `pread`, `pwrite`, `preadv`, `pwritev`, `sendfile`, `pselect6`, `ppoll`, `signalfd4`, `vmsplice`, `splice` | `syscall/fs.rs`, signal, event helpers |
| 78-83 | `readlinkat`, `fstatat`, `fstat`, `sync`, `fsync`, `fdatasync` | `syscall/fs.rs` |
| 85-88 | `timerfd_create`, `timerfd_settime`, `timerfd_gettime`, `utimensat` | `fs/timerfd.rs`, `syscall/fs.rs` |
| 276, 291, 306, 436, 439 | `renameat2`, `statx`, `syncfs`, `close_range`, `faccessat2` | `syscall/fs.rs` |
| 285-287 | `copy_file_range`, `preadv2`, `pwritev2` | `syscall/fs.rs` |
| 503, 506 | `ext4_counters`, `open` | ext4 counters, `sys_openat(AT_FDCWD, ...)` |

## 进程生命周期、clone 与 exec

| 编号 | syscall | 入口 |
|------|---------|------|
| 93-97 | `exit`, `exit_group`, `waitid`, `set_tid_address`, `unshare` | `syscall/process/lifecycle.rs`, `clone.rs` |
| 220, 435 | `clone`, `clone3` | `syscall/process/clone.rs` |
| 221, 281 | `execve`, `execveat` | `syscall/process/exec.rs` |
| 260 | `wait4` | `syscall/process/lifecycle.rs` |
| 268 | `setns` | `syscall/process/clone.rs` |
| 99-100 | `set_robust_list`, `get_robust_list` | `syscall/process/lifecycle.rs` |

## Futex、信号、pidfd 与 ptrace

| 编号 | syscall | 入口 |
|------|---------|------|
| 98, 449 | `futex`, `futex_waitv` | `syscall/process/futex.rs` |
| 117 | `ptrace` | `syscall/process/signal.rs` |
| 129-139 | `kill`, `tkill`, `tgkill`, `sigaltstack`, `rt_sigsuspend`, `rt_sigaction`, `rt_sigprocmask`, `rt_sigpending`, `rt_sigtimedwait`, `rt_sigqueueinfo`, `rt_sigreturn` | `syscall/process/signal.rs` |
| 272 | `kcmp` | `syscall/process/signal.rs` |
| 424, 434, 438 | `pidfd_send_signal`, `pidfd_open`, `pidfd_getfd` | `syscall/process/signal.rs`, `fs/pidfd.rs` |
| 74 | `signalfd4` | `syscall/process/signal.rs` |

## 时间、timer 与资源统计

| 编号 | syscall | 入口 |
|------|---------|------|
| 101-103 | `nanosleep`, `getitimer`, `setitimer` | `syscall/process/time.rs` |
| 107-115 | `timer_create`, `timer_gettime`, `timer_getoverrun`, `timer_settime`, `timer_delete`, `clock_settime`, `clock_gettime`, `clock_getres`, `clock_nanosleep` | `syscall/process/time.rs` |
| 153 | `times` | `syscall/process/time.rs` |
| 165, 168-171, 266 | `getrusage`, `getcpu`, `gettimeofday`, `settimeofday`, `adjtimex`, `clock_adjtime` | `syscall/process/time.rs` |
| 1690 | `get_time` | `sys_get_time()` |

## UID/GID、进程组、session 与调度

| 编号 | syscall | 入口 |
|------|---------|------|
| 140-152 | `setpriority`, `getpriority`, `setregid`, `setgid`, `setreuid`, `setuid`, `setresuid`, `getresuid`, `setresgid`, `getresgid`, `setfsuid`, `setfsgid` | `syscall/process/ids.rs` |
| 154-162 | `setpgid`, `getpgid`, `getsid`, `setsid`, `getgroups`, `setgroups`, `uname`, `sethostname`, `setdomainname` | `ids.rs`, `misc.rs` |
| 172-179 | `getpid`, `getppid`, `getuid`, `geteuid`, `getgid`, `getegid`, `gettid`, `sysinfo` | `ids.rs`, `misc.rs` |
| 118-127, 274-275 | `sched_setparam`, `sched_setscheduler`, `sched_getscheduler`, `sched_getparam`, `sched_setaffinity`, `sched_getaffinity`, `sched_yield`, `sched_get_priority_max`, `sched_get_priority_min`, `sched_rr_get_interval`, `sched_setattr`, `sched_getattr` | `syscall/process/misc.rs`, scheduler helpers |
| 163-164, 261 | `getrlimit`, `setrlimit`, `prlimit` | `syscall/process/misc.rs` |

## 内存管理

| 编号 | syscall | 入口 |
|------|---------|------|
| 213-216 | `sbrk`, `brk`, `munmap`, `mremap` | `syscall/process/mm.rs` |
| 222-234 | `mmap`, `fadvise64`, `mprotect`, `msync`, `mlock`, `munlock`, `mlockall`, `munlockall`, `mincore`, `madvise`, `remap_file_pages` | `syscall/process/mm.rs`, `syscall/fs.rs` |
| 236 | `get_mempolicy` | `syscall/process/mm.rs` |
| 259 | `riscv_flush_icache` | `syscall/process/mm.rs` |
| 270-271 | `process_vm_readv`, `process_vm_writev` | `syscall/process/mm.rs` |
| 283-284 | `membarrier`, `mlock2` | `syscall/process/mm.rs` |
| 288-290 | `pkey_mprotect`, `pkey_alloc`, `pkey_free` | `syscall/process/mm.rs` |
| 279 | `memfd_create` | `fs/memfd.rs` |

## IPC、message queue 与 keyring

| 编号 | syscall | 入口 |
|------|---------|------|
| 180-185 | `mq_open`, `mq_unlink`, `mq_timedsend`, `mq_timedreceive`, `mq_notify`, `mq_getsetattr` | `syscall/process/ipc.rs` |
| 186-197 | `msgget`, `msgctl`, `msgrcv`, `msgsnd`, `semget`, `semctl`, `semtimedop`, `semop`, `shmget`, `shmctl`, `shmat`, `shmdt` | `syscall/process/ipc.rs` |
| 217-219 | `add_key`, `request_key`, `keyctl` | `syscall/process/keyring.rs` |

## 网络

| 编号 | syscall | 入口 |
|------|---------|------|
| 198-212 | `socket`, `socketpair`, `bind`, `listen`, `accept`, `connect`, `getsockname`, `getpeername`, `sendto`, `recvfrom`, `setsockopt`, `getsockopt`, `shutdown`, `sendmsg`, `recvmsg` | `net/syscall/*.rs` |
| 242 | `accept4` | `net/syscall/accept.rs` |

`SYSCALL_SOCK_SHUTDOWN = 210` 是 socket 的 `shutdown(2)`；`SYSCALL_SHUTDOWN = 501` 是 MangoCore 的系统关机入口，两者不是同一个 syscall。

## capability、system、random、BPF 和其他杂项

| 编号 | syscall | 入口 |
|------|---------|------|
| 90-92 | `capget`, `capset`, `personality` | `syscall/process/misc.rs` |
| 106 | `delete_module` | `syscall/process/misc.rs` |
| 116, 142 | `syslog`, `reboot` | `syscall/process/misc.rs` |
| 166-167 | `umask`, `prctl` | `syscall/process/misc.rs` |
| 278 | `getrandom` | `syscall/mod.rs::sys_getrandom()` |
| 280 | `bpf` | `syscall/process/bpf.rs` |
| 500-502 | `ls`, `shutdown`, `clear` | 非标准 MangoCore syscall；分发表注册 `shutdown`，`ls/clear` 仅在名称映射中出现 |

## 未知 syscall

未匹配的 syscall id 进入 `_` 分支：

| 行为 | 实现状态 |
|------|----------|
| 日志 | 打印 syscall 名称、编号和 6 个参数 |
| 返回值 | `errno::ENOSYS` |
| 信号 | 不发送 `SIGSYS`；相关代码被注释 |

## 编号来源与注册关系

系统调用是否可用取决于两个文件同时对齐：

| 文件 | 职责 |
|------|------|
| `os/src/syscall/syscall_id.rs` | 定义 `SYSCALL_*` 编号常量 |
| `os/src/syscall/mod.rs::syscall_name()` | 把编号映射到日志/trace 中的名称 |
| `os/src/syscall/mod.rs::syscall()` | 把编号注册到实际 `sys_*` 处理函数 |

三者关系如下：

```
syscall_id.rs
  pub const SYSCALL_READ: usize = 63;
        |
        v
syscall_name()
  SYSCALL_READ => "read"
        |
        v
syscall()
  SYSCALL_READ => sys_read(args[0], args[1], args[2])
```

编号常量存在但 `syscall()` 没有分支时，用户态调用仍进入未知 syscall 分支并返回 `ENOSYS`。名称映射存在只影响日志展示，不代表 syscall 已注册。

## 分发表中的跨模块入口

`syscall/mod.rs` 不是所有 syscall 的实现文件。它通过 `use` 把多个模块的入口收敛到一个 `match`：

| 入口来源 | 代表 syscall | `use` 来源 |
|----------|--------------|------------|
| `syscall/fs.rs` | `openat`, `read`, `write`, `mount`, `statx` | `use fs::*` |
| `syscall/process/*` | `clone`, `execve`, `mmap`, `signal`, `timer`, `ipc` | `use process::*` |
| `net/syscall/*` | `socket`, `bind`, `sendto`, `recvmsg` | `use crate::net::syscall::*` |
| `fs/eventpoll.rs` | `epoll_create1`, `epoll_ctl`, `epoll_pwait*` | 显式 `use crate::fs::eventpoll::{...}` |
| `fs/eventfd.rs` | `eventfd2` | 显式 `use crate::fs::eventfd::sys_eventfd2` |
| `fs/timerfd.rs` | `timerfd_*` | 显式 `use crate::fs::timerfd::{...}` |
| `fs/ext4/counters.rs` | `ext4_counters` | 分支中直接调用 |

这种组织方式使 syscall 编号表集中，但领域语义仍由具体子系统维护。

## 参数转换模式

`syscall()` 中的分支负责把 `usize` 参数转换为目标函数类型。常见模式：

| 参数类型 | 转换方式 | 例子 |
|----------|----------|------|
| fd | `args[n]` 或 `args[n] as u32` | `sys_close(args[0])`, `sys_socket(args[0] as u32, ...)` |
| 用户只读指针 | `args[n] as *const T` | `sys_execve(args[0] as *const u8, ...)` |
| 用户可写指针 | `args[n] as *mut T` | `sys_clock_gettime(args[0], args[1] as *mut TimeSpec)` |
| flags/mode | `args[n] as u32` | `sys_openat(..., args[2] as u32, args[3] as u32)` |
| signed offset | `args[n] as isize` | `sys_lseek(args[0], args[1] as isize, ...)` |
| 结构体 ABI | `args[n] as *const Struct` | `sys_sched_setattr(args[0], args[1] as *const SchedAttr, ...)` |

类型转换本身不验证用户指针；具体 `sys_*` 函数必须通过 `mm/uaccess.rs` 的接口读写用户空间。

## 特殊注册项

### socket shutdown 与系统 shutdown

| 编号 | 名称 | 分支 | 含义 |
|------|------|------|------|
| 210 | `sock_shutdown` / `shutdown(2)` | `SYSCALL_SOCK_SHUTDOWN => sys_sock_shutdown(fd, how)` | socket 半关闭 |
| 501 | `shutdown` | `SYSCALL_SHUTDOWN => sys_shutdown()` | 系统关机 |

网络目录中的 socket shutdown 只对应 210。501 不接受 socket fd 参数。

### `open` 兼容入口

`SYSCALL_OPEN = 506` 的分支：

```rust
SYSCALL_OPEN => sys_openat(
    AT_FDCWD,
    args[0] as *const u8,
    args[1] as u32,
    0o777u32,
)
```

它把旧式 `open(path, flags)` 包装成 `openat(AT_FDCWD, path, flags, mode=0o777)`。真实权限仍会在 `sys_openat()` 和 VFS 创建路径中应用 umask。

### `accept` 与 `accept4`

| 编号 | 分支 |
|------|------|
| 202 | `SYSCALL_ACCEPT => sys_accept(fd, addr, addrlen)` |
| 242 | `SYSCALL_ACCEPT4 => sys_accept4(fd, addr, addrlen, flags)` |

未知分支中保留了 `if syscall_id == 242 { trace_event!(0xB042, ...) }` 的调试逻辑；当前编号 242 已有正式分支，因此正常调用不会落入该 fallback。

### `get_time`

`SYSCALL_GET_TIME = 1690` 是非标准入口，分发到 `sys_get_time()`。标准时间相关入口仍包括 `gettimeofday(169)`、`clock_gettime(113)`、`times(153)` 等。

## 已定义但未作为分支注册的常量/名称

`syscall_name()` 中存在 `ls`、`clear` 名称映射，但 `syscall()` 中没有对应 `SYSCALL_LS`、`SYSCALL_CLEAR` 分支。用户态调用 500 或 502 会按未知 syscall 处理并返回 `ENOSYS`。

编号表中也存在部分兼容常量，其语义由具体分支决定。例如 `SYSCALL_SPLICE = 76` 已在分发表中进入 `sys_splice()`；`SYSCALL_OPEN = 506` 不在 Linux 标准编号范围内。

## 维护检查流程

新增或修改 syscall 时按以下顺序核对：

1. 在 `syscall_id.rs` 中添加或确认 `SYSCALL_*` 编号。
2. 在 `syscall_name()` 中添加日志名称。
3. 在 `syscall()` 的 `match` 中注册实际分支。
4. 确认架构 ABI 是否需要特殊处理；当前只有 raw `clone` 对 la64 有参数重排。
5. 确认用户指针均由 `mm/uaccess.rs` 读取或写入。
6. 失败路径返回具体负 errno；仅未注册 syscall 返回 `ENOSYS`。
7. 更新本表和对应领域专题页。

这套检查流程要按顺序执行，因为 syscall 的“存在性”和“语义正确性”在代码中分属不同层。编号和名称只影响日志与可读性；`match` 分支决定是否进入实现；领域函数才决定 fd、权限、用户指针、flag 和阻塞语义。测试中看到 `ENOSYS`，优先看分发表；看到 `EINVAL/EFAULT/EBADF`，再进入领域函数核对参数校验顺序。

维护表格时不要只按编号排序，还要保留领域归类。评审或调试时常见问题不是“某个数字是多少”，而是“这个接口应该属于 fs、process、mm 还是 net，真实实现在哪个文件”。编号表负责把数字和实现入口连接起来。

## 测试映射

| 功能区域 | 代表测试 |
|----------|----------|
| 编号注册 | 用户态直接发 syscall 编号，确认不是 `ENOSYS` |
| 文件 fd | LTP `open*`, `read*`, `write*`, `fcntl*`, `epoll*` |
| 进程生命周期 | LTP `clone*`, `execve*`, `wait*`, `exit*` |
| MM | LTP `mmap*`, `mprotect*`, `mlock*`, `mincore*` |
| 信号 | LTP `kill*`, `tgkill*`, `rt_sig*`, `signalfd*` |
| IPC | LTP `msg*`, `sem*`, `shm*`, POSIX mq 用例 |
| 网络 | LTP `socket*`, `bind*`, `connect*`, `send*`, `recv*` |
| 非标准入口 | 内核测试程序或 busybox shell 中的 shutdown/ext4 counter 调试 |

## 源文件索引

| 路径 | 内容 |
|------|------|
| `os/src/syscall/syscall_id.rs` | 编号常量 |
| `os/src/syscall/mod.rs` | `syscall_name()` 和分发表 |
| `os/src/syscall/fs.rs` | 文件/fd/挂载/事件相关 syscall |
| `os/src/syscall/process/` | process/mm/signal/time/ipc/ids/misc |
| `os/src/net/syscall/` | 网络 syscall |
| `os/src/fs/eventpoll.rs` | epoll 分支实现 |
| `os/src/fs/eventfd.rs` | eventfd 分支实现 |
| `os/src/fs/timerfd.rs` | timerfd 分支实现 |
| `os/src/fs/ext4/counters.rs` | `ext4_counters` |
