---
title: "兼容边界、错误码与未支持路径"
category: syscall
status: stable
author: MangoCore Team
last_update: 2026-06-29
tags: [syscall, errno, compatibility]
---

# 兼容边界、错误码与未支持路径

## 1. 概述

MangoCore syscall 层采用 Linux 风格的负 errno 返回值。兼容边界由三部分共同决定：

| 层 | 作用 |
|----|------|
| trap ABI | 固定 `a7/a0..a5` 参数和 `a0` 返回值 |
| `syscall/mod.rs` | 判断编号是否注册、执行 seccomp、分发到具体 `sys_*` |
| 领域 syscall | 按 Linux 语义决定参数校验顺序和 errno 优先级 |

`ENOSYS` 只表示 syscall 编号没有进入分发表。已经注册的 syscall 即使某个功能分支暂不支持，也应返回具体 errno，例如 `EINVAL`、`EOPNOTSUPP`、`ENOPROTOOPT`、`EACCES`。

## 2. 返回值编码

| 层级 | 成功 | 失败 |
|------|------|------|
| syscall handler | `isize >= 0` | 负 errno |
| `Socket::try_*` | `Ok(isize)` | `Err(SyscallErr)`，由 syscall 层转负 errno |
| VFS `File` 新接口 | `Result<usize, SyscallErr>` | `Err(SyscallErr)` |
| 部分旧式 fd 路径 | `usize` 字节数 | 编码后的负 errno，再由 syscall 包装 |

`errno.rs` 中常量已经是负值：

```rust
pub const EINVAL: isize = -22;
pub const ENOSYS: isize = -38;
pub const ENOPROTOOPT: isize = -92;
```

领域 handler 不需要再取负号；从 `SyscallErr` 转换时常见写法是 `-(e as isize)`。

## 3. 未知 syscall

未知编号进入 `syscall()` 的 `_` 分支：

| 行为 | 实现 |
|------|------|
| 控制台输出 | `println!("[syscall] Unsupported syscall: ...")` |
| error log | 输出名称、编号和每个参数 |
| 返回值 | `errno::ENOSYS` |
| 信号 | 保留 `SIGSYS` 注释块，运行路径未启用 |

名称表返回 `"unknown"` 不影响返回值；即使 `syscall_name()` 有名称，只要 `match syscall_id` 没有分支，仍返回 `ENOSYS`。

## 4. 已命名但未注册的入口

| 编号 | 名称 | 当前分发表状态 |
|------|------|----------------|
| 500 | `ls` | 名称映射存在，未注册分支 |
| 502 | `clear` | 名称映射存在，未注册分支 |

已注册非标准入口：

| 编号 | 名称 | 行为 |
|------|------|------|
| 501 | `shutdown` | 系统关机 |
| 503 | `ext4_counters` | ext4 计数器诊断 |
| 506 | `open` | 包装 `openat(AT_FDCWD, path, flags, 0o777)` |
| 1690 | `get_time` | 非标准时间入口 |

## 5. seccomp 边界

seccomp 检查发生在 `match syscall_id` 前：

| action | 行为 |
|--------|------|
| `Allow` | 继续分发 |
| `KillThread(signal)` | 当前线程按信号退出 |
| `KillProcess(signal)` | 线程组按信号退出 |

被 seccomp kill 的 syscall 不会进入领域 handler，因此不会返回领域 errno。

## 6. 用户内存错误

`mm/uaccess.rs` 中的常见错误来源：

| 场景 | errno |
|------|-------|
| 必需用户指针为 NULL | `EFAULT` |
| 用户范围溢出或超过 `USER_VA_END` | `EFAULT` |
| 单次 buffer 翻译超过 8 MiB | `EFAULT` |
| iovec 数量超过 1024 | `EINVAL` |
| iovec 总长度溢出或超过 `isize::MAX` | `EINVAL` |
| 当前 token 与传入 token 不一致 | `EFAULT` |
| fault-in 后仍无权限/映射 | MM 层 errno，常见 `EFAULT` |

`check_user_range()` 只做范围和溢出检查；它不保证页已经映射，也不保证访问权限。

## 7. 典型 errno 优先级

### 7.1 mmap

| 场景 | errno |
|------|-------|
| 非匿名 mmap 坏 fd | `EBADF`，优先于 len/prot/flags |
| len 为 0 | `EINVAL` |
| prot 未知位 | `EINVAL` |
| `MAP_SHARED_VALIDATE` 带未知 flag | `EOPNOTSUPP` |
| offset 非页对齐 | `EINVAL` |
| 文件不可读 | `EACCES` |
| shared writable 但文件不可写 | `EACCES` |
| memfd seal 阻止 writable shared | `EPERM` |
| 非 regular 文件且非 `/dev/zero` | `EACCES` |

### 7.2 clone/namespace

| 场景 | errno |
|------|-------|
| `CLONE_PIDFD` 与 `CLONE_PARENT_SETTID` 同时设置 | `EINVAL` |
| `CLONE_SIGHAND` 缺少 `CLONE_VM` | `EINVAL` |
| `CLONE_THREAD` 缺少 `CLONE_SIGHAND` | `EINVAL` |
| `CLONE_VFORK` 与 `CLONE_THREAD` 同时设置 | `EINVAL` |
| `CLONE_NEWNS` 与 `CLONE_FS` 同时设置 | `EINVAL` |
| 新 namespace 相关 flag 且 euid 非 0 | `EPERM` |
| unshare NEWNET/NEWNS/NEWIPC 且 live thread count 非 1 | `EINVAL` |

### 7.3 网络

| 场景 | errno |
|------|-------|
| 未知 socket domain | `EAFNOSUPPORT` |
| socketpair 非 AF_UNIX | `EPROTONOSUPPORT` |
| 未知 setsockopt level/optname | `ENOPROTOOPT` |
| getpeername 用户地址坏 | `EFAULT` 优先于 `ENOTCONN` |
| UDP payload 过大 | `EMSGSIZE` |

### 7.4 文件/fd

| 场景 | errno |
|------|-------|
| fd 不存在 | `EBADF` |
| fd 不可读/不可写 | `EBADF` |
| open 路径过长 | `ENAMETOOLONG` |
| `O_CREAT|O_DIRECTORY` 命中目录写打开语义 | `EINVAL` |
| 写打开目录 | `EISDIR` |
| 非目录配合 `O_DIRECTORY` | `ENOTDIR` |
| memfd seal 阻止 truncate | `EPERM` |

### 7.5 wait/time/signal

| 场景 | errno |
|------|-------|
| wait4 pid 为 `i32::MIN` | `ESRCH` |
| waitid options 不含等待类型 | `EINVAL` |
| nanosleep timespec 无效 | `EINVAL` |
| nanosleep 被信号打断 | `EINTR` |
| signalfd 无 pending signal | `EAGAIN` |
| pidfd fd 类型不对 | `EBADF` |

## 8. 可重启与阻塞

阻塞路径常见返回：

| 返回 | 含义 |
|------|------|
| `EAGAIN` | 非阻塞路径当前不能完成，或 timeout |
| `ERESTART` | wait queue 被信号打断，可由上层/信号路径决定重启 |
| `EINTR` | 如 nanosleep 被信号打断并写出剩余时间 |

文件 I/O 的 WaitQueue 路径中，`WaitResult::Interrupted` 返回 `ERESTART`；`sys_nanosleep()` 被打断时返回 `EINTR`。

判断 errno 时先问两个问题：这个 syscall 是否进入了分发表，以及失败发生在参数校验的哪一层。未进入分发表才是 `ENOSYS`；进入分支后，即使功能只覆盖子集，也应返回该接口语义下的具体 errno。参数层常见优先级是 fd/对象是否存在、用户指针是否可访问、flag/prot/command 是否支持、权限是否满足、运行时状态是否允许。

阻塞相关 errno 还要看 fd 是否 nonblock。非阻塞路径通常把“当前不能完成”报告为 `EAGAIN/EWOULDBLOCK` 或连接状态相关 errno；可阻塞路径会入 WaitQueue，信号打断时返回 `ERESTART` 或领域定义的 `EINTR`，timeout 则按接口语义返回 `EAGAIN`、0 或剩余时间。

## 9. 调试检查表

| 症状 | 检查 |
|------|------|
| 返回 `ENOSYS` | `syscall/mod.rs` 是否有 `match` 分支 |
| errno 与 Linux 测例不一致 | 参数校验顺序，尤其 fd/用户指针/flag 优先级 |
| 用户地址 panic | 是否绕过 `mm/uaccess.rs` 直接解引用 |
| 阻塞 syscall 不醒 | WaitQueue 或 timer 是否注册，信号打断是否释放锁 |
| socket option 测试失败 | 未知 opt 是否返回 `ENOPROTOOPT` |
| mmap 测试失败 | 非匿名坏 fd 是否优先 `EBADF`，shared writable 权限是否按顺序检查 |

## 10. 源文件索引

| 路径 | 内容 |
|------|------|
| `os/src/syscall/mod.rs` | 分发、seccomp、未知 syscall |
| `os/src/syscall/errno.rs` | errno 常量 |
| `os/src/syscall/syscall_id.rs` | 编号常量 |
| `os/src/mm/uaccess.rs` | 用户内存访问 |
| `os/src/syscall/process/mm.rs` | mmap/brk/mprotect/mremap 错误优先级 |
| `os/src/syscall/process/clone.rs` | clone/namespace 错误优先级 |
| `os/src/syscall/fs.rs` | 文件/fd 错误优先级 |
| `os/src/net/syscall/` | 网络 syscall 错误优先级 |
