---
title: "系统调用层详解 (Syscall Layer)"
category: syscall
status: stable
author: MangoCore Team
last_update: 2026-06-29
tags: [syscall, abi, dispatch, errno, seccomp]
---

# 系统调用层详解

## 1. 概述

MangoCore 的 syscall 层由架构 trap 后端、`syscall/mod.rs` 扁平分发器、领域 `sys_*` 函数和用户内存访问辅助共同组成。rv64 与 la64 后端统一使用 `a7` 传递 syscall id，`a0..a5` 传递六个参数，返回值写回 `a0`。

`syscall/mod.rs::syscall()` 是所有系统调用的架构无关入口。该函数负责名称映射、日志、seccomp、match 分发、返回值记录和未知 syscall 处理；具体业务语义由 `syscall/fs.rs`、`syscall/process/*`、`net/syscall/*` 和少量 `fs/*` fd 对象实现。

## 2. 设计目标

| 目标 | 实现方式 |
|------|----------|
| 架构 ABI 统一 | trap 后端把寄存器整理成 `syscall(id, [usize; 6])` |
| 分发路径可读 | `syscall/mod.rs` 使用扁平 `match syscall_id`，不做动态注册 |
| errno 语义清晰 | syscall handler 返回 `isize`，错误为负 errno |
| 用户指针安全 | 参数读取通过 `mm/uaccess.rs` 和地址空间 fault-in |
| 可观测 | syscall 名称、耗时、返回值、未知 syscall 参数均有记录入口 |
| 兼容边界显式 | 未进入分发表的编号返回 `ENOSYS`；已进入分发表的兼容 stub 返回具体 errno |

## 3. 架构

### 3.1 调用层次

```
+---------------------------------------------------------------+
| user mode                                                     |
| a7 = syscall id, a0..a5 = args                                |
+---------------------------------------------------------------+
                              |
                              v
+---------------------------------------------------------------+
| arch trap backend                                             |
| rv64: hal/arch/riscv/trap/mod.rs                              |
| la64: hal/arch/loongarch64/trap/mod.rs                        |
+---------------------------------------------------------------+
                              |
                              v
+---------------------------------------------------------------+
| syscall::syscall(syscall_id, args)                            |
| name | log | seccomp | match | perf | unknown fallback         |
+---------------------------------------------------------------+
          |              |              |              |
          v              v              v              v
   fs syscalls   process/mm syscalls   net syscalls   misc
```

### 3.2 源文件地图

| 文件 | 职责 |
|------|------|
| `os/src/syscall/mod.rs` | 名称映射、seccomp、分发、日志、perf、`getrandom` |
| `os/src/syscall/syscall_id.rs` | syscall 编号常量 |
| `os/src/syscall/errno.rs` | 负 errno 常量和转换 |
| `os/src/syscall/utils.rs` | 旧阻塞 I/O 辅助函数 |
| `os/src/syscall/fs.rs` | 文件、fd、挂载、stat、xattr、同步等 syscall |
| `os/src/syscall/process/clone.rs` | clone、clone3、unshare、setns |
| `os/src/syscall/process/exec.rs` | execve、execveat |
| `os/src/syscall/process/lifecycle.rs` | exit、wait、robust list |
| `os/src/syscall/process/mm.rs` | mmap、brk、mprotect、mlock、process_vm |
| `os/src/syscall/process/signal.rs` | signal、pidfd、signalfd、kcmp |
| `os/src/syscall/process/time.rs` | clock、timer、sleep、rusage |
| `os/src/syscall/process/ids.rs` | UID/GID、pgid/session、rlimit、sched、prctl、cap |
| `os/src/syscall/process/ipc.rs` | SysV IPC、POSIX MQ |
| `os/src/syscall/process/futex.rs` | futex、futex_waitv |
| `os/src/syscall/process/bpf.rs` | bpf |
| `os/src/syscall/process/keyring.rs` | add_key、request_key、keyctl |
| `os/src/net/syscall/` | socket syscall |

## 4. 关键数据结构

### 4.1 syscall id

`syscall_id.rs` 定义编号常量。编号常量不是接口可用性的唯一依据；可调用接口以 `syscall/mod.rs` 的 match 分支为准。

非标准编号：

| 编号 | 名称 | 分发状态 |
|------|------|----------|
| 500 | `ls` | 仅名称映射 |
| 501 | `shutdown` | 系统关机 syscall |
| 502 | `clear` | 仅名称映射 |
| 503 | `ext4_counters` | ext4 诊断 syscall |
| 506 | `open` | 包装为 `openat(AT_FDCWD, ...)` |
| 1690 | `get_time` | 非标准时间入口 |

### 4.2 errno

`errno.rs` 中的 errno 常量本身是负值。syscall handler 成功返回非负 `isize`，失败直接返回负 errno。

| 例子 | 值 |
|------|----|
| `SUCCESS` | 0 |
| `EINVAL` | -22 |
| `ENOSYS` | -38 |
| `EAGAIN` | -11 |
| `ERESTART` | 内部可重启错误 |

### 4.3 用户内存访问

| API | 语义 |
|-----|------|
| `translated_ref` | 翻译一个只读用户对象，不允许跨页 |
| `translated_refmut` | 翻译一个可写用户对象，不允许跨页 |
| `translated_str` | 读取 NUL 结尾字符串，扫描上限 8 MiB |
| `translated_byte_buffer` | 将用户 buffer 切成内核可访问片段 |
| `UserPtr` / `UserPtrMut` | 用户指针包装 |
| `UserIoVec` | iovec 读取，数量上限 1024 |
| `fault_in_user_va` | 缺页并校验页表权限 |

`check_user_range()` 只做范围和溢出检查；是否可读写由 fault-in 和页表权限决定。

### 4.4 seccomp action

syscall 分发前检查 seccomp：

| action | 行为 |
|--------|------|
| `Allow` | 继续进入 match 分发 |
| `KillThread` | 当前线程以 `SIGSYS` 退出 |
| `KillProcess` | 线程组以 `SIGSYS` 退出 |

## 5. 执行流程

### 5.1 trap 到 syscall

```
user
  a7 = id
  a0..a5 = args
        |
        v
trap backend
  pc += 4
  origin a0 saved
        |
        v
syscall(id, args)
        |
        v
ret -> a0
```

`rt_sigreturn` 是例外：id 为 139 时，trap 后端不使用普通 syscall 返回值覆盖 `a0`。

### 5.2 `syscall()` 内部流程

`syscall()` 定义在 `os/src/syscall/mod.rs:302`。源码按公共记录、seccomp、分发、返回记录四段组织：

```
syscall_name(id)
record current syscall id
optional log
seccomp_action_for_syscall()
match id
    -> sys_xxx(...)
    -> ENOSYS fallback
record cost ticks
record syscall(id, ret)
return ret
```

未知 syscall 分支会打印 syscall 名称、编号和 6 个参数，返回 `ENOSYS`。向任务注入 `SIGSYS` 的代码块保留为注释，运行路径未启用。

关键源码位置：

| 段 | 源码位置 | 说明 |
|----|----------|------|
| perf/trace/current syscall | `mod.rs:303-307` | 记录入口、trace 参数和当前 syscall id。 |
| info 日志 blacklist | `mod.rs:308-340` | `LOG=info/debug/trace` 时输出低频 syscall 参数。 |
| seccomp | `mod.rs:341-359` | `Allow/KillThread/KillProcess` 在 match 前生效。 |
| 扁平 match | `mod.rs:360-886` | 所有已注册 syscall 的唯一可调用入口。 |
| unknown fallback | `mod.rs:887-920` | 打印编号和参数，返回 `ENOSYS`。 |
| 返回记录 | `mod.rs:923-945` | 输出 errno/返回值日志，记录耗时和 syscall 结果。 |

这也说明 `syscall_name()` 和 `syscall_id.rs` 不是可调用性的最终依据；只有 `mod.rs:360-886` 的 match 分支会进入领域实现。

#### 5.2.1 `syscall()` 入口公共段

`syscall()` 的前半段把观测、当前 syscall id、日志和 seccomp 检查放在分发表之前：

```rust
pub fn syscall(syscall_id: usize, args: [usize; 6]) -> isize {
    crate::task::perf::record_syscall_enter(syscall_id);
    let _syscall_start = crate::task::perf::perf_time_now();
    crate::trace_event!(syscall_id, args[0], args[1], args[2], args[3], args[4], args[5]);
    // 记录当前系统调用 ID，供 OOM 诊断使用
    crate::task::set_current_syscall_id(Some(syscall_id));
    let syscall_info_log_enabled = matches!(option_env!("LOG"), Some("info" | "debug" | "trace"));
    let mut show_info = true;
    if syscall_info_log_enabled
        && ![
            //black list
            SYSCALL_YIELD,
            // SYSCALL_READ,
            SYSCALL_WRITE,
            SYSCALL_GETDENTS64,
            SYSCALL_READV,
            SYSCALL_WRITEV,
            SYSCALL_PSELECT6,
            SYSCALL_SIGACTION,
            SYSCALL_SIGPROCMASK,
            // SYSCALL_WAIT4,
            // SYSCALL_GETPPID,
            SYSCALL_CLOCK_GETTIME,
        ]
        .contains(&syscall_id)
    {
        show_info = false;
        log::info!(
            "[syscall] {}({}) args: [{:X}, {:X}, {:X}, {:X}, {:X}, {:X}]",
            syscall_name(syscall_id),
            syscall_id,
            args[0],
            args[1],
            args[2],
            args[3],
            args[4],
            args[5],
        );
    }
    if crate::task::any_seccomp_enabled() {
        let _start = crate::task::perf::perf_time_now();
        crate::task::perf::record_seccomp_check_call();
        match seccomp_action_for_syscall(syscall_id) {
            SeccompSyscallAction::Allow => {
                crate::task::perf::record_seccomp_check(_start, true);
            }
            SeccompSyscallAction::KillThread(signal) => {
                crate::task::perf::record_seccomp_check(_start, false);
                let signum = signal.to_signum().unwrap() as u32;
                exit_current_and_run_next(signum);
            }
            SeccompSyscallAction::KillProcess(signal) => {
                crate::task::perf::record_seccomp_check(_start, false);
                let signum = signal.to_signum().unwrap() as u32;
                exit_group_and_run_next(signum);
            }
        }
    }
    let ret = match syscall_id {
```

这段代码说明 syscall 进入领域函数前已经完成三类公共行为：记录 trace/perf，保存当前 syscall id，执行 seccomp。`exit_current_and_run_next()` 和 `exit_group_and_run_next()` 不返回到领域函数，因此 seccomp kill 不会继续执行 match 分支。

#### 5.2.2 分发表的文件/fd 开头段

match 开头首先注册 xattr、路径、fd 和事件 fd 类 syscall：

```rust
        SYSCALL_SETXATTR => sys_setxattr(
            args[0] as *const u8, args[1] as *const u8, args[2] as *const u8, args[3], args[4] as u32,
        ),
        SYSCALL_LSETXATTR => sys_lsetxattr(
            args[0] as *const u8, args[1] as *const u8, args[2] as *const u8, args[3], args[4] as u32,
        ),
        SYSCALL_FSETXATTR => sys_fsetxattr(
            args[0], args[1] as *const u8, args[2] as *const u8, args[3], args[4] as u32,
        ),
        SYSCALL_GETXATTR => sys_getxattr(
            args[0] as *const u8, args[1] as *const u8, args[2] as *mut u8, args[3],
        ),
        SYSCALL_LGETXATTR => sys_lgetxattr(
            args[0] as *const u8, args[1] as *const u8, args[2] as *mut u8, args[3],
        ),
        SYSCALL_FGETXATTR => sys_fgetxattr(
            args[0], args[1] as *const u8, args[2] as *mut u8, args[3],
        ),
        SYSCALL_LISTXATTR => sys_listxattr(args[0] as *const u8, args[1] as *mut u8, args[2]),
        SYSCALL_LLISTXATTR => sys_llistxattr(args[0] as *const u8, args[1] as *mut u8, args[2]),
        SYSCALL_FLISTXATTR => sys_flistxattr(args[0], args[1] as *mut u8, args[2]),
        SYSCALL_REMOVEXATTR => sys_removexattr(args[0] as *const u8, args[1] as *const u8),
        SYSCALL_LREMOVEXATTR => sys_lremovexattr(args[0] as *const u8, args[1] as *const u8),
        SYSCALL_FREMOVEXATTR => sys_fremovexattr(args[0], args[1] as *const u8),
        SYSCALL_GETCWD => sys_getcwd(args[0], args[1]),
        SYSCALL_DUP => sys_dup(args[0]),
        SYSCALL_DUP3 => sys_dup3(args[0], args[1], args[2] as u32),
        SYSCALL_EVENTFD2 => sys_eventfd2(args[0] as u32, args[1] as u32),
        SYSCALL_EPOLL_CREATE1 => sys_epoll_create1(args[0]),
        SYSCALL_EPOLL_CTL => sys_epoll_ctl(
            args[0],
            args[1],
            args[2],
            args[3] as *const crate::fs::eventpoll::EpollUserEvent,
        ),
        SYSCALL_EPOLL_PWAIT => sys_epoll_pwait(
            args[0],
            args[1] as *mut crate::fs::eventpoll::EpollUserEvent,
            args[2] as isize,
            args[3] as isize,
            args[4] as *const crate::task::signal::Signals,
        ),
        SYSCALL_EPOLL_PWAIT2 => sys_epoll_pwait2(
            args[0],
            args[1] as *mut crate::fs::eventpoll::EpollUserEvent,
            args[2] as isize,
            args[3] as *const TimeSpec,
            args[4] as *const crate::task::signal::Signals,
        ),
```

这一段体现分发表的类型转换职责：用户态 ABI 参数一律以 `usize` 进入，分发器在调用领域函数前把路径指针、事件结构指针、flags、fd 等转换为对应 Rust 类型。

#### 5.2.3 网络分支、默认分支和返回记录

网络分支集中在 match 后段，下面这一段是 `SYSCALL_SOCKET` 到 `SYSCALL_EXT4_COUNTERS` 的连续注册区间：

```rust
        SYSCALL_SOCKET => sys_socket(args[0] as u32, args[1] as u32, args[2] as u32),
        SYSCALL_SOCKETPAIR => sys_socketpair(
            args[0] as u32,
            args[1] as u32,
            args[2] as u32,
            args[3] as usize,
        ),
        SYSCALL_BIND => sys_bind(args[0] as u32, args[1] as usize, args[2] as u32),
        SYSCALL_LISTEN => sys_listen(args[0] as u32, args[1] as u32),
        SYSCALL_ACCEPT => sys_accept(args[0] as u32, args[1] as usize, args[2] as usize),
        SYSCALL_ACCEPT4 => sys_accept4(
            args[0] as u32,
            args[1] as usize,
            args[2] as usize,
            args[3] as u32,
        ),
        SYSCALL_CONNECT => sys_connect(args[0] as u32, args[1] as usize, args[2] as u32),
        SYSCALL_GETSOCKNAME => sys_getsockname(args[0] as u32, args[1] as usize, args[2] as usize),
        SYSCALL_GETPEERNAME => sys_getpeername(args[0] as u32, args[1] as usize, args[2] as usize),
        SYSCALL_SENDTO => sys_sendto(
            args[0] as u32,
            args[1] as usize,
            args[2],
            args[3] as u32,
            args[4] as usize,
            args[5] as u32,
        ),
        SYSCALL_RECVFROM => sys_recvfrom(
            args[0] as u32,
            args[1] as usize,
            args[2] as u32,
            args[3] as u32,
            args[4] as usize,
            args[5] as usize,
        ),
        SYSCALL_SETSOCKOPT => sys_setsockopt(
            args[0] as u32,
            args[1] as u32,
            args[2] as u32,
            args[3] as usize,
            args[4] as u32,
        ),
        SYSCALL_GETSOCKOPT => sys_getsockopt(
            args[0] as u32,
            args[1] as u32,
            args[2] as u32,
            args[3] as usize,
            args[4] as usize,
        ),
        SYSCALL_SOCK_SHUTDOWN => sys_sock_shutdown(args[0] as u32, args[1] as u32),
        SYSCALL_SENDMSG => sys_sendmsg(args[0] as u32, args[1], args[2] as u32),
        SYSCALL_RECVMSG => sys_recvmsg(args[0] as u32, args[1], args[2] as u32),
        SYSCALL_GETRANDOM => sys_getrandom(args[0] as usize, args[1] as usize, args[2] as u32),
        SYSCALL_MEMFD_CREATE => sys_memfd_create(args[0] as *const u8, args[1] as u32),
        SYSCALL_BPF => sys_bpf(args[0] as u32, args[1], args[2]),
        SYSCALL_DELETE_MODULE => sys_delete_module(args[0] as *const u8, args[1] as u32),
        SYSCALL_SHUTDOWN => sys_shutdown(),
        SYSCALL_EXT4_COUNTERS => crate::fs::ext4::counters::sys_ext4_counters(args[0], args[1], args[2]),
```

`SYSCALL_EXT4_COUNTERS` 之后源码继续注册 sched、mempolicy、fadvise、madvise 等分支；未匹配编号最终落入默认分支并进入统一返回记录：

```rust
        _ => {
            if syscall_id == 242 {
                crate::trace_event!(
                    0xB042,
                    args[0] as u64,
                    args[1] as u64,
                    args[2] as u64,
                    args[3] as u64,
                    0,
                    0
                );
            }
            println!(
                "[syscall] Unsupported syscall: {} ({}), calling over arguments: {:?}",
                syscall_name(syscall_id),
                syscall_id,
                args
            );
            error!(
                "Unsupported syscall:{} ({}), calling over arguments:",
                syscall_name(syscall_id),
                syscall_id
            );
            for i in 0..args.len() {
                error!("args[{}]: {:X}", i, args[i]);
            }
            /*
            crate::task::current_task()
                .unwrap()
                .acquire_inner_lock()
                .add_signal(crate::task::Signals::SIGSYS);
            */
            errno::ENOSYS
        }
    };

    if syscall_info_log_enabled && show_info {
        match Errno::try_from(ret) {
            Ok(errno) => info!(
                "[syscall] {}({}) -> {:?}",
                syscall_name(syscall_id),
                syscall_id,
                errno
            ),
            Err(val) => info!(
                "[syscall] {}({}) -> {:X}",
                syscall_name(syscall_id),
                syscall_id,
                val.number
            ),
        }
    }
    let _syscall_ticks = crate::task::perf::perf_time_now() - _syscall_start;
    crate::task::perf::record_syscall_cost_ticks(_syscall_ticks);
    if syscall_id == 173 {
        crate::task::perf::record_getppid_cost(_syscall_ticks);
    }
    crate::task::perf::record_syscall(syscall_id, ret);
    ret
}
```

`SYSCALL_SHUTDOWN` 和 `SYSCALL_SOCK_SHUTDOWN` 在网络注册片段中分离：前者是系统关机，后者是 socket half-close。默认分支返回 `ENOSYS`，并保留对 242 号的 trace 诊断；由于 242 已注册为 `accept4`，正常 `accept4` 不会进入默认分支。

### 5.3 文件 syscall 分组

| 范围 | syscall |
|------|---------|
| xattr | `setxattr`、`getxattr`、`listxattr`、`removexattr` 及 l/f 变体 |
| 路径和目录 | `getcwd`、`mkdirat`、`mknodat`、`unlinkat`、`linkat`、`symlinkat`、`readlinkat` |
| fd | `openat`、`open`、`close`、`close_range`、`dup`、`dup3`、`fcntl`、`ioctl` |
| 读写 | `read`、`write`、`readv`、`writev`、`pread*`、`pwrite*`、`lseek` |
| 数据搬运 | `sendfile`、`copy_file_range`、`splice`、`vmsplice` |
| stat | `fstatat`、`fstat`、`statx`、`statfs`、`fstatfs` |
| 同步和空间 | `sync`、`syncfs`、`fsync`、`fdatasync`、`truncate`、`ftruncate`、`fallocate` |
| mount | `mount`、`umount2` |
| 事件 fd | `epoll_*`、`eventfd2`、`timerfd_*`、`signalfd4` |

### 5.4 进程 syscall 分组

| 范围 | syscall |
|------|---------|
| clone | `clone`、`clone3`、`unshare`、`setns` |
| exec | `execve`、`execveat` |
| exit/wait | `exit`、`exit_group`、`wait4`、`waitid` |
| ids | `getpid`、`getppid`、`gettid`、UID/GID、pgid/session、groups |
| signal | `kill`、`tkill`、`tgkill`、`rt_sig*`、`sigaltstack`、`rt_sigreturn` |
| pidfd/kcmp | `pidfd_open`、`pidfd_send_signal`、`pidfd_getfd`、`kcmp` |
| futex | `futex`、`futex_waitv` |
| rlimit/sched | `getrlimit`、`setrlimit`、`prlimit`、`sched_*` |
| time | `nanosleep`、`clock_*`、`timer_*`、`gettimeofday`、`times`、`getrusage` |
| IPC | SysV msg/sem/shm、POSIX MQ |

### 5.5 MM syscall 分组

| syscall | 关键语义 |
|---------|----------|
| `brk`, `sbrk` | heap 在 `[heap_bottom, heap_bottom + USER_HEAP_SIZE]` 内移动 |
| `mmap` | fd 优先校验；支持 anonymous/file、shared/private、fixed/noreplace |
| `munmap` | 分裂 VMA 并释放范围 |
| `mprotect` | 校验 VMA 权限、shared write seal、`may_write` |
| `mremap` | 调整映射范围 |
| `mincore` | PTE 或 page cache resident 判断 |
| `madvise` | DONTNEED/FREE/DONTFORK/WIPEONFORK 等 |
| `mlock*` | locked pages 和 rlimit 相关路径 |
| `process_vm_readv/writev` | 跨进程 VM 读写 |
| `remap_file_pages` | 校验 prot/flags/range 后返回 `EINVAL` |

### 5.6 网络 syscall 分组

网络分发表将 198-212、242 号 syscall 交给 `net/syscall/*`：

| syscall | 文件 |
|---------|------|
| `socket` | `socket.rs` |
| `bind` | `bind.rs` |
| `listen` | `listen.rs` |
| `accept`, `accept4` | `accept.rs` |
| `connect` | `connect.rs` |
| `sendto`, `sendmsg` | `sendto.rs`, `sendmsg.rs` |
| `recvfrom`, `recvmsg` | `recvfrom.rs`, `recvmsg.rs` |
| `getsockopt`, `setsockopt` | `getsockopt.rs`, `setsockopt.rs` |
| `getsockname`, `getpeername` | `getsockname.rs`, `getpeername.rs` |
| `shutdown`, `socketpair` | `shutdown.rs`, `socketpair.rs` |

`SYSCALL_SOCK_SHUTDOWN = 210` 是 socket shutdown；`SYSCALL_SHUTDOWN = 501` 是系统关机入口。

## 6. 重点 syscall 流程

### 6.1 `sys_mmap`

```
parse prot
parse flags
if non-anonymous:
    lookup fd
    check readable
    check shared writable permission
    check memfd seals
    /dev/zero -> anonymous
    non-regular -> EACCES
do_mmap()
    choose address
    handle fixed/noreplace
    create VMA
    prealloc anonymous shared writable frames if needed
    insert VMA
return start address
```

错误优先级：

| 条件 | errno |
|------|-------|
| 非匿名坏 fd | `EBADF` |
| `MAP_SHARED_VALIDATE` 未知 flag | `EOPNOTSUPP` |
| 文件不可读或 shared writable 文件不可写 | `EACCES` |
| fixed_noreplace 覆盖已有 VMA | `EEXIST` |
| anonymous shared eager 分配过大 | `ENOMEM` |

### 6.2 `sys_clone`

```
dispatch layer adapts raw ABI
sys_clone()
    reject CLONE_PIDFD + CLONE_PARENT_SETTID
    sys_clone_inner()
        validate flag dependencies
        check namespace privilege
        create/share VM
        create/share files/fs/sighand/futex
        allocate TID/PID/user slot/kstack
        write parent/child tid if requested
        allocate pidfd if requested
        publish child
        schedule child or vfork wait
```

la64 raw clone ABI 为 `flags, stack, ptid, ctid, tls`；非 la64 路径按 `flags, stack, ptid, tls, ctid` 适配。

### 6.3 `sys_execve`

```
read path
open executable
check metadata and ETXTBSY key
parse shebang if needed
read argv/envp
validate stack usage
TaskControlBlock::load_elf()
    build new AddressSpace
    map ELF/interpreter/heap/stack/auxv
    terminate sibling threads
    close CLOEXEC fds
    reset sighand/futex
    complete vfork
```

### 6.4 `sys_futex`

```
validate uaddr != null and 4-byte aligned
cmd = futex_op & 0x7f
option = PRIVATE/CLOCK_REALTIME bits
key:
    private -> virtual address
    shared mapping -> physical address
match cmd:
    Wait / WaitBitset
    Wake / WakeBitset
    Requeue / CmpRequeue
    Invalid or unsupported -> EINVAL
```

`WakeOp` 和 PI 类命令未进入实现分支，返回 `EINVAL`。

### 6.5 `sys_wait4` / `sys_waitid`

```
validate options
select child set
if observable state exists:
    fill status/siginfo
    consume unless WNOWAIT
else if WNOHANG:
    return 0
else:
    wait on child_exit_wait
```

`waitid` 支持 `P_PIDFD` 路径。

### 6.6 `sys_read` / `sys_write`

文件读写 syscall 是理解 fd、用户指针和阻塞语义的最短路径。`sys_read(fd, buf, count)` 的源码顺序如下：

```rust
let count = count.min(crate::hal::MAX_RW_COUNT);
let task = current_task().unwrap();
let file = {
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    fd_table.get_file(fd)
};
检查 readable;
let token = task.get_user_token();
按设备特例、nonblock、WaitQueue、普通文件分支读取;
```

逐步解释：

| 步骤 | 语义 |
|------|------|
| 限制 `MAX_RW_COUNT` | 避免一次 syscall 读写超过 HAL 规定的最大安全块。 |
| `current_task()` | syscall 总是在当前线程上下文中执行，fd table 来自该线程所属 PCB。 |
| 查 fd table 时 clone `Arc` | 查表只在锁内完成，实际 I/O 在锁外执行，避免跨等待点持有 fd table 锁。 |
| `readable()/writable()` | fd 权限错误直接返回 `EBADF`，不会继续访问用户 buffer。 |
| `/dev/null`、`/dev/zero` | 设备特例在 VFS 对象层前快速处理。 |
| `is_nonblock()` | 非阻塞 fd 只尝试一次；遇到 EAGAIN 直接返回。 |
| `inode.read_wait_queue()` | 有等待队列的对象用 `WaitQueue::wait_until_interruptible()` 反复尝试，信号到达返回 `ERESTART`。 |
| 普通文件 | 普通文件没有 read/write wait queue，直接进入 PageCache 路径。 |

`sys_write()` 与 `sys_read()` 结构相同，但多了 `RLIMIT_FSIZE` 检查：它会通过当前 task inner 的 `fsize_limit_cur` 和文件写入起点限制本次写入长度，超限时按实现返回 errno 或裁剪长度。

### 6.7 `syscall()` 主函数代码解读

`syscall::syscall()` 的职责不是实现业务，而是保证每个 syscall 都经过同一组公共步骤：

```
record_syscall_enter(id)
perf_time_now()
trace_event!(id, args...)
set_current_syscall_id(Some(id))
可选 info 日志
seccomp_action_for_syscall(id)
match syscall_id
record cost / result
return ret
```

这个顺序有几个直接影响：

| 位置 | 影响 |
|------|------|
| `set_current_syscall_id(Some(id))` 在分发前 | OOM 和 panic 诊断可以知道当前线程卡在哪个 syscall。 |
| seccomp 在 match 前 | 被 seccomp 杀死的 syscall 不会进入领域实现。 |
| `match syscall_id` 扁平展开 | 新增 syscall 必须同时更新编号、名称映射和 match 分支；只加编号不会让接口可调用。 |
| 返回值统一为 `isize` | 领域函数可以直接返回负 errno，trap 后端无需再包装错误类型。 |
| 未匹配编号返回 `ENOSYS` | 与已注册但参数不支持的 `EINVAL/EOPNOTSUPP/ENOPROTOOPT` 等错误区分。 |

阅读 syscall 层时可以把 `syscall/mod.rs` 当作目录页：先找到 match 分支，再跳到领域文件；不要在分发器里寻找文件系统、网络或进程语义。

### 6.8 用户指针为什么不能直接解引用

用户传入的 `buf`、`pathname`、`iovec` 都是用户虚拟地址。内核不能把它们当作普通 Rust 指针直接读写，原因包括：

| 风险 | 处理方式 |
|------|----------|
| 地址不在用户范围 | `check_user_range()` 和 `fault_in_user_va()` 返回 `EFAULT`。 |
| 地址跨页 | `translated_byte_buffer()` 和 iovec 路径把 buffer 切成片段。 |
| PTE 尚未建立 | uaccess 触发 `AddressSpace::fault_in_user_va()`，按访问类型补页。 |
| 权限不满足 | fault 后再次用 `user_access_ok()` 验证 R/W/X 权限。 |
| 字符串无 NUL | `translated_str()` 有扫描上限，超过限制返回错误而不是无限扫描。 |

因此，syscall 代码中看到 `*const u8` 或 `usize` 用户地址时，应继续追到 `uaccess.rs` 或具体 helper，而不是把它理解为内核地址。

## 7. 阻塞、重启与等待队列

### 7.1 WaitQueue 优先路径

新的阻塞路径使用 `WaitQueue`：

| API | 语义 |
|-----|------|
| `wait_event_interruptible` | 条件等待，可被信号打断 |
| `wait_event_timeout` | 条件等待，带 timeout |
| `wait_event_interruptible_timeout` | 可中断 + timeout |
| locked variants | 在持有业务锁的条件路径中避免 lost wakeup |

`WaitResult::Interrupted` 通常映射为 `-ERESTART`；`TimedOut` 通常映射为 `-EAGAIN`。

### 7.2 旧 wait_io

`syscall/utils.rs` 中的 `wait_io_core()`、`wait_io()` 等函数仍存在，但注释标为 deprecated，新路径优先使用 WaitQueue。

### 7.3 信号打断

阻塞 syscall 在睡眠前后需要检查 actionable signal。信号到达后，等待路径返回 `ERESTART` 或对应 errno，trap 返回前的 signal delivery 再决定用户态表现。

## 8. 错误码和兼容边界

| 场景 | 返回 |
|------|------|
| 未匹配 syscall id | `ENOSYS` |
| futex 空地址或非 4 字节对齐 | `EINVAL` |
| `clone` flag 依赖关系错误 | `EINVAL` |
| namespace 权限不足 | `EPERM` |
| `mmap` 非匿名坏 fd | `EBADF` |
| `mmap` 文件不可读 | `EACCES` |
| `MAP_SHARED_VALIDATE` 未知 bit | `EOPNOTSUPP` |
| 用户结构体跨页 | `EFAULT` |
| 用户 buffer 超过可访问范围 | `EFAULT` 或领域 errno |

已注册但仅提供兼容语义的 syscall 不使用 `ENOSYS` 表示“存在但参数/能力不支持”，而返回具体 errno。

## 9. 测试映射

| 领域 | 用例来源 |
|------|----------|
| 基础 syscall | basic、busybox |
| 文件/fd | LTP fs、libctest、iozone |
| MM | LTP mmap/brk/mprotect/mincore/process_vm |
| 进程 | LTP clone/exec/wait/ids |
| signal | LTP signal、tgkill、signalfd |
| futex | libcbench、LTP futex、pthread 相关测试 |
| time/timer | nanosleep、clock、timerfd、cyclictest |
| IPC | SysV IPC、POSIX MQ LTP |
| network | iperf、netperf、socket LTP；详细映射见 `docs/06_net/test-map.md` |

## 10. 源文件索引

| 路径 | 内容 |
|------|------|
| `os/src/hal/arch/riscv/trap/mod.rs` | rv64 syscall trap |
| `os/src/hal/arch/loongarch64/trap/mod.rs` | la64 syscall trap |
| `os/src/syscall/mod.rs` | syscall 分发主入口 |
| `os/src/syscall/syscall_id.rs` | 编号常量 |
| `os/src/syscall/errno.rs` | errno |
| `os/src/mm/uaccess.rs` | 用户指针 |
| `os/src/syscall/fs.rs` | FS syscall |
| `os/src/syscall/process/` | 进程、MM、signal、time、IPC、futex、ids |
| `os/src/net/syscall/` | 网络 syscall |
| `os/src/syscall/process/ids.rs` | seccomp action 判定、prctl/seccomp 分支 |
| `os/src/task/task.rs` | task 内的 seccomp mode/filter 状态与 active 计数 |
| `os/src/task/perf.rs` | syscall perf 记录 |
