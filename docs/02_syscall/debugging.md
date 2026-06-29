---
title: "系统调用调试与测试映射"
category: syscall
status: stable
author: MangoCore Team
last_update: 2026-06-29
tags: [syscall, debug, errno, test]
---

# 系统调用调试与测试映射

## 1. 从编号追到实现

定位 syscall 首先确认编号是否真的进入分发表：

```
syscall_id.rs
  -> syscall_name()
  -> syscall() match
  -> sys_xxx(...)
  -> fs / task / mm / net / ipc object
```

| 现象 | 首查位置 | 说明 |
|------|----------|------|
| 返回 `ENOSYS` | `syscall/mod.rs::syscall()` match | 只有未匹配编号才走未知 syscall 分支。 |
| 日志名称正确但仍 `ENOSYS` | `syscall_name()` 与 match 是否同时更新 | 名称映射不代表接口可调用。 |
| 参数明显错位 | 架构 trap 后端 | 检查 `a7` 和 `a0..a5` 收集；raw `clone` 还要看架构参数顺序。 |
| 返回 errno 不符合 LTP | 领域 `sys_xxx()` | 核对 fd、用户指针、flag、权限、状态的校验顺序。 |
| syscall 卡住 | WaitQueue、timer、socket poll | 看是否释放锁后等待，以及 signal/timeout 是否能唤醒。 |

## 2. `syscall()` 公共路径

`syscall::syscall()` 固定执行：

1. `record_syscall_enter(syscall_id)`。
2. `trace_event!(syscall_id, args...)`。
3. `set_current_syscall_id(Some(syscall_id))`。
4. 可选 info 日志。
5. seccomp 检查。
6. `match syscall_id` 调用领域函数。
7. 记录耗时和返回值。

如果 seccomp 返回 kill action，领域 syscall 不会执行。若 OOM 或 panic 诊断需要知道当前 syscall，依赖第 3 步记录的 current syscall id。

## 3. 用户指针调试

用户指针问题不要看 Rust 指针本身，要看 uaccess：

| 参数类型 | 常用 helper | 典型错误 |
|----------|-------------|----------|
| 字符串 | `translated_str()` | 无 NUL、超过 8 MiB、地址不可读 |
| 结构体读 | `copy_from_user()` | 跨页/权限不足/未映射 |
| 结构体写 | `copy_to_user()` | 输出地址不可写 |
| buffer | `translated_byte_buffer()`、`UserBufferReader/Writer` | 跨页后半段 fault、权限不匹配 |
| iovec | `UserIoVec` | iovcnt 超 1024、单项地址坏 |

用户地址范围检查只说明地址在用户空间范围内；真正可读写要经过 `fault_in_user_va()` 和 PTE 权限复核。

## 4. errno 优先级

| 领域 | 常见优先级 |
|------|------------|
| fd/file | fd 是否存在 -> fd 权限 -> 用户 buffer -> 文件对象状态 |
| mmap | 非匿名坏 fd -> len/prot/flags -> 文件权限/seal -> VMA range |
| clone | 低内存 -> flag 依赖 -> namespace 权限 -> 用户 tid/pidfd 写回 |
| signal | signal 编号 -> target 是否存在 -> 权限 -> pending/交付 |
| socket | fd 是否 socket -> sockaddr 用户指针 -> domain/type/protocol -> socket 状态 |
| IPC | 用户结构 -> key/id -> 权限 -> NOWAIT/阻塞状态 |

已注册但只支持子集的 syscall 应返回具体 errno；`ENOSYS` 只表示没有进入分发表。

## 5. 观测入口

| 入口 | 用途 |
|------|------|
| `LOG=info` | 低频 syscall 参数日志 |
| `trace_event!` | syscall 事件序列 |
| `task/perf.rs` | syscall 次数、耗时、seccomp 检查 |
| `current_syscall_id` | OOM/panic 时定位当前 syscall |
| 未知 syscall log | 打印编号和 6 个参数 |

## 6. 测试映射

| 范围 | 测试 |
|------|------|
| 编号和分发 | 直接发 syscall 编号，确认不是 `ENOSYS` |
| fd/read/write | LTP `read*`, `write*`, `open*`, `fcntl*` |
| epoll/eventfd/timerfd | LTP `epoll*`, `eventfd*`, `timerfd*` |
| mmap/brk/mprotect | LTP `mmap*`, `brk*`, `mprotect*`, `mincore*` |
| clone/exec/wait | LTP `clone*`, `execve*`, `wait*` |
| signal/pidfd | LTP `kill*`, `tgkill*`, `signalfd*`, `pidfd_*` |
| futex | pthread、libcbench、LTP futex |
| IPC | LTP SysV msg/sem/shm、POSIX MQ |
| socket | LTP socket、iperf、netperf |
