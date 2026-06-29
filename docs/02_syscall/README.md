---
title: "系统调用子系统 (Syscall Subsystem)"
category: syscall
status: stable
author: MangoCore Team
last_update: 2026-06-29
tags: [syscall, abi, errno, dispatch]
---

# 系统调用子系统

## 概述

MangoCore 的系统调用入口由各架构 trap 后端接入，统一进入 `os/src/syscall/mod.rs::syscall()`。该函数完成 syscall 名称映射、日志、seccomp 检查、扁平 `match` 分发、性能统计和未知 syscall 处理；具体语义由 `fs`、`syscall/process/*`、`net/syscall/*` 等模块实现。

已注册 syscall 以 `syscall/mod.rs` 的分发表为准。仅有编号常量但未出现在分发表中的项目，不计入用户态可调用接口范围。

## 依据范围

| 主题 | 主要源码 |
|------|----------|
| syscall 编号 | `os/src/syscall/syscall_id.rs` |
| syscall 名称与分发 | `os/src/syscall/mod.rs` |
| errno | `os/src/syscall/errno.rs` |
| 用户内存参数访问 | `os/src/mm/uaccess.rs` |
| 文件系统 syscall | `os/src/syscall/fs.rs` 及 `os/src/fs/` |
| 进程、MM、信号、时间、IPC | `os/src/syscall/process/` |
| 网络 syscall | `os/src/net/syscall/` |

## 调用层次

```
userspace
    |
    | a7 = syscall id, a0..a5 = args
    v
arch trap handler
    |
    v
syscall::syscall(syscall_id, [usize; 6])
    |
    +-- seccomp action check
    +-- flat match dispatch
    +-- perf/log record
    v
sys_xxx handler
    |
    v
kernel subsystem
```

成功返回值使用非负 `isize`。失败返回负 errno，例如 `EINVAL = -22`、`ENOSYS = -38`。`File` trait 层仍然保留 `usize` 编码错误的接口约定，syscall 处理器负责转换成用户可见的负 errno。

## 文件地图

### syscall 核心

| 文件 | 说明 |
|------|------|
| `os/src/syscall/mod.rs` | 名称映射、日志、seccomp、扁平分发、性能统计、未知 syscall 返回 |
| `os/src/syscall/syscall_id.rs` | syscall 编号常量 |
| `os/src/syscall/errno.rs` | errno 常量和 `Errno` 转换 |
| `os/src/syscall/utils.rs` | 阻塞 I/O 辅助函数；当前注释建议优先使用 `WaitQueue` |

### 文件与事件

| 文件 | 说明 |
|------|------|
| `os/src/syscall/fs.rs` | open/read/write/stat/mount/xattr/fcntl/ioctl 等文件系统入口 |
| `os/src/fs/eventpoll.rs` | epoll fd 实现 |
| `os/src/fs/eventfd.rs` | eventfd 实现 |
| `os/src/fs/timerfd.rs` | timerfd 实现 |
| `os/src/fs/pidfd.rs` | pidfd 文件对象 |
| `os/src/fs/memfd.rs` | memfd 文件对象 |

### 进程域

| 文件 | 说明 |
|------|------|
| `os/src/syscall/process/clone.rs` | clone、clone3、unshare、setns |
| `os/src/syscall/process/exec.rs` | execve、execveat 和 shebang 处理 |
| `os/src/syscall/process/lifecycle.rs` | exit、exit_group、wait4、waitid、robust list |
| `os/src/syscall/process/mm.rs` | brk/sbrk/mmap/mprotect/mlock/mincore/madvise 等 |
| `os/src/syscall/process/signal.rs` | signal、pidfd、signalfd、kcmp |
| `os/src/syscall/process/time.rs` | nanosleep、clock、timer、timeofday、rusage |
| `os/src/syscall/process/ids.rs` | UID/GID、进程组、session、priority |
| `os/src/syscall/process/ipc.rs` | SysV IPC 和 POSIX message queue |
| `os/src/syscall/process/futex.rs` | futex、futex_waitv |
| `os/src/syscall/process/misc.rs` | uname、sysinfo、prctl、capability、sched、bpf 等杂项 |
| `os/src/syscall/process/keyring.rs` | add_key、request_key、keyctl |

### 网络域

| 文件 | 说明 |
|------|------|
| `os/src/net/syscall/*.rs` | socket、bind、connect、listen、accept、send/recv、getsockopt/setsockopt 等 |
| `docs/06_net/syscall-layer.md` | 网络 syscall 的详细文档 |

## 文档索引

| 文档 | 内容 |
|------|------|
| `README.md` | syscall 子系统总览、文件地图 |
| `syscall-layer.md` | 系统调用层详解，覆盖 ABI、分发、领域流程、错误码和测试映射 |
| `dispatch-and-abi.md` | ABI、trap 接入、分发、返回值和用户指针约束 |
| `syscall-map.md` | 当前已注册 syscall 按领域分组索引 |
| `fs-fd-event.md` | 文件、fd、epoll/eventfd/timerfd/memfd/pidfd 入口 |
| `process-lifecycle-and-ids.md` | clone、exec、exit、wait、UID/GID、进程组和调度 ABI |
| `mm-syscalls.md` | brk、mmap、mprotect、mlock、mincore、process_vm 等 |
| `signal-time-ipc.md` | signal、pidfd、时间/timer、SysV IPC、POSIX MQ |
| `network-syscalls.md` | 网络 syscall 到 `docs/06_net/` 的索引 |
| `compatibility-and-errors.md` | errno、未支持路径、seccomp 和兼容边界 |
| `tracing-and-dispatch.md` | syscall 日志、trace、perf 和调试入口 |
| `debugging.md` | syscall 编号追踪、用户指针、errno 优先级、观测入口和测试映射 |
