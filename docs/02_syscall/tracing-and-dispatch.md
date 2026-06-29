---
title: "分发观测与调试入口"
category: syscall
status: stable
author: MangoCore Team
last_update: 2026-06-29
tags: [syscall, trace, debug, perf]
---

# 分发观测与调试入口

## 1. 概述

syscall 分发层同时承担观测职责：日志、trace、perf、当前 syscall id 诊断和未知 syscall 输出。相关入口位于：

| 文件 | 内容 |
|------|------|
| `os/src/syscall/mod.rs` | syscall entry/return 日志、trace_event、perf 记录 |
| `os/src/trace.rs` | trace ring buffer、tag 解码、Ctrl+T dump |
| `os/src/task/perf.rs` | syscall cost、seccomp、trap、TLB、调度等统计 |
| `os/src/task/processor.rs` | 当前 syscall id 和当前任务快捷缓存 |

## 2. syscall entry 观测流程

`syscall()` 开头执行：

```
record_syscall_enter(syscall_id)
_syscall_start = perf_time_now()
trace_event!(syscall_id, args[0]..args[5])
set_current_syscall_id(Some(syscall_id))
syscall_info_log_enabled = LOG in {info, debug, trace}
```

这些动作发生在 seccomp 检查前，因此被 seccomp kill 的 syscall 也会留下 entry 记录。

## 3. 当前 syscall id

`task/processor.rs` 中：

```rust
static CURRENT_SYSCALL_ID: AtomicUsize = AtomicUsize::new(0);
```

`set_current_syscall_id(Some(id))` 在 `heap_trace` 或 `perf_stats` feature 下把 `id + 1` 写入原子变量；0 表示无记录。`current_syscall_name()` 用它在 OOM/panic 诊断中显示当前 syscall 名称。

## 4. 日志开关

`LOG=info|debug|trace` 时 syscall 分发可输出 entry/return。entry 日志跳过高频 syscall：

| 高频 syscall |
|--------------|
| `yield` |
| `write` |
| `getdents64` |
| `readv`, `writev` |
| `pselect6` |
| `sigaction`, `sigprocmask` |
| `clock_gettime` |

entry 日志格式：

```
[syscall] name(id) args: [A0, A1, A2, A3, A4, A5]
```

return 日志先尝试 `Errno::try_from(ret)`。若成功，打印 errno 名称；否则按普通数值打印。

## 5. trace_event

syscall entry 调用：

```rust
crate::trace_event!(syscall_id, args[0], args[1], args[2], args[3], args[4], args[5]);
```

`trace.rs` 中 `TraceEntry` 包含：

| 字段 | 含义 |
|------|------|
| `timestamp` | 微秒时间戳 |
| `tag` | syscall id 或自定义事件 id |
| `arg1..arg6` | 六个 payload |

`TRACE_RET_MASK = 0x8000_0000_0000_0000` 用于标记 syscall return 事件。tag 解码优先调用 `syscall_name(tag as usize)`；若不是已知 syscall，再匹配网络/调试自定义 tag。

## 6. Ctrl+T dump

调度循环轮询 console 字符：

| 架构 | 轮询频率 |
|------|----------|
| rv64 | 每 64 个 schedule tick |
| 非 rv64 | 每轮调度循环 |

读到 `MAGIC_KEY = 0x14` 时，`trace::check_magic_key(ch, "schedule")` 触发 trace dump 和 shutdown。普通字符进入 `trace::stash_char()`，供 TTY read 使用。

## 7. perf 记录

`syscall()` 返回前执行：

```
_syscall_ticks = perf_time_now() - _syscall_start
record_syscall_cost_ticks(_syscall_ticks)
if syscall_id == 173 {
    record_getppid_cost(_syscall_ticks)
}
record_syscall(syscall_id, ret)
```

`task/perf.rs` 中相关计数包括：

| 计数 | 含义 |
|------|------|
| `SYSCALL_TOTAL` | syscall 总数 |
| `SYSCALL_GETPPID_TOTAL` | getppid 次数 |
| `SYSCALL_COST_MAX_TICKS` | syscall cost 最大值 |
| `SYSCALL_COST_TICKS_TOTAL` | syscall cost 总和 |
| `ECALL_TRAP_COST_TICKS_TOTAL/MAX` | ecall trap 成本 |
| `SECCOMP_CHECK_CALLS` | seccomp 检查次数 |

这些计数受 `perf_stats` feature 和 `STATS_ON` 开关影响。

## 8. seccomp 观测

seccomp 检查周围记录：

```
record_seccomp_check_call()
seccomp_action_for_syscall(syscall_id)
record_seccomp_check(start, allowed)
```

如果 action 为 kill，线程或进程会退出，不进入后续 match 分支。

## 9. 未知 syscall 调试

未知 syscall 输出两类信息：

| 输出 | 内容 |
|------|------|
| console | syscall 名称、编号、完整 args 数组 |
| error log | 名称、编号、每个参数十六进制 |

默认分支内对 id 242 保留 `trace_event!(0xB042, ...)`，但 242 已注册为 `accept4`，正常 accept4 不会落入默认分支。

## 10. 调试路径表

| 目标 | 入口 |
|------|------|
| syscall 是否注册 | `syscall/mod.rs::syscall()` 的 `match` |
| syscall 编号 | `syscall/syscall_id.rs` |
| syscall 名称 | `syscall/mod.rs::syscall_name()` |
| errno 定义 | `syscall/errno.rs` |
| 用户指针 | `mm/uaccess.rs` |
| 当前任务/当前 syscall | `task/processor.rs` |
| trace dump | `trace.rs` 和调度循环 console poll |
| perf 计数 | `task/perf.rs` |

一条 syscall 的观测链路是：trap 后端收集寄存器，`syscall()` 记录 trace event 和 perf enter，领域函数执行，返回后记录结果和耗时。若日志里 syscall 名称正确但参数错误，优先看 trap ABI；若参数正确但返回 `ENOSYS`，看 match 分支；若返回 errno 与预期不同，进入领域函数看校验顺序；若系统卡住，结合 `current_syscall_id` 和 perf/trace 判断是否停在阻塞路径。

`LOG=info` 适合看低频 syscall 的入口参数，高频调用被 blacklist 降噪；trace ring 更适合看事件序列；perf 计数适合定位“调用次数异常”或“某阶段耗时异常”。三者不是替代关系，而是从不同粒度观察同一条分发路径。

## 11. 测试映射

| 观测点 | 验证方式 |
|--------|----------|
| entry 日志 | `cd os && make rv64-run LOG=info` |
| trace dump | QEMU 控制台发送 Ctrl+T |
| unknown syscall | 用户态调用未注册编号，检查 `ENOSYS` 和日志 |
| seccomp kill | seccomp LTP/自测 |
| syscall cost | 打开 `perf_stats` 后读取 sysfs stats |

## 12. 源文件索引

| 路径 | 内容 |
|------|------|
| `os/src/syscall/mod.rs` | 分发观测主体 |
| `os/src/trace.rs` | trace ring buffer 和 dump |
| `os/src/task/perf.rs` | syscall/seccomp/trap 统计 |
| `os/src/task/processor.rs` | 当前 syscall id 和调度循环 |
| `os/src/console.rs` | 日志输出 |
