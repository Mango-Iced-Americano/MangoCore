---
title: "系统调用 ABI 与分发 (Syscall ABI and Dispatch)"
category: syscall
status: stable
author: MangoCore Team
last_update: 2026-06-29
tags: [syscall, abi, trap, errno, uaccess]
---

# 系统调用 ABI 与分发

## 1. 概述

MangoCore 的 syscall 入口由架构 trap 后端收集寄存器，再统一调用：

```rust
pub fn syscall(syscall_id: usize, args: [usize; 6]) -> isize
```

该函数位于 `os/src/syscall/mod.rs`。它负责 syscall 名称映射、trace/perf 记录、seccomp 检查、扁平 `match` 分发、未知 syscall 日志和 `ENOSYS` 返回。具体业务语义分散在 `syscall/fs.rs`、`syscall/process/*`、`net/syscall/*` 和部分 `fs/*` 文件中。

`syscall()` 开头统一记录 perf、trace、当前 syscall id、入口日志和 seccomp：

```rust
pub fn syscall(syscall_id: usize, args: [usize; 6]) -> isize {
    crate::task::perf::record_syscall_enter(syscall_id);
    let _syscall_start = crate::task::perf::perf_time_now();
    crate::trace_event!(syscall_id, args[0], args[1], args[2], args[3], args[4], args[5]);
    crate::task::set_current_syscall_id(Some(syscall_id));
    let syscall_info_log_enabled = matches!(option_env!("LOG"), Some("info" | "debug" | "trace"));
    let mut show_info = true;
    if syscall_info_log_enabled
        && ![
            SYSCALL_YIELD,
            SYSCALL_WRITE,
            SYSCALL_GETDENTS64,
            SYSCALL_READV,
            SYSCALL_WRITEV,
            SYSCALL_PSELECT6,
            SYSCALL_SIGACTION,
            SYSCALL_SIGPROCMASK,
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
        /* flat dispatch table */
    };
    ret
}
```

上面的 `match` 以注释表示完整分发表位置；完整注册表位于 `os/src/syscall/mod.rs` 的同一函数内。seccomp 在业务分发前执行，`KillThread/KillProcess` 直接进入退出路径，不会返回普通 syscall 分支。

## 2. ABI

rv64 和 la64 的 trap 后端都按相同 ABI 进入分发层：

| 寄存器 | 含义 |
|--------|------|
| `a7` | syscall id |
| `a0` | 参数 0 / 返回值 |
| `a1` | 参数 1 |
| `a2` | 参数 2 |
| `a3` | 参数 3 |
| `a4` | 参数 4 |
| `a5` | 参数 5 |

trap 后端在调用 `syscall()` 前把用户 PC 前进 4 字节：

| 架构 | PC 前进方式 |
|------|-------------|
| rv64 | `cx.gp.pc += 4` |
| la64 | `ERA::read().next_ins().write()` + `cx.gp.pc += 4` |

返回值通常写回 `a0`。`rt_sigreturn` 的 syscall id 是 139，两套 trap 后端都不把普通返回值覆盖到 `a0`，因为该 syscall 恢复的是完整用户 trap context。

## 3. 分发函数结构

`syscall()` 的控制流：

```
record_syscall_enter(syscall_id)
perf_time_now()
trace_event!(syscall_id, args...)
set_current_syscall_id(Some(syscall_id))
optional info log
seccomp check
  Allow
  KillThread  -> exit_current_and_run_next(signum)
  KillProcess -> exit_group_and_run_next(signum)
match syscall_id
  SYSCALL_READ   -> sys_read(args[0], args[1], args[2])
  SYSCALL_CLONE  -> sys_clone(raw clone ABI converted by cfg branch)
  SYSCALL_SOCKET -> sys_socket(args[0] as u32, args[1] as u32, args[2] as u32)
  _ -> ENOSYS
optional return log
record_syscall_cost_ticks()
record_getppid_cost()       [syscall_id == 173]
record_syscall(syscall_id, ret)
return ret
```

该路径没有动态注册表，也没有多层 syscall table。所有已注册 syscall 都在一个 `match syscall_id` 中显式列出。

## 4. 名称映射

`syscall_name(id)` 与分发表在同一文件。它用于日志、trace tag 解码、perf/诊断输出。

名称映射和分发注册不是同义关系：

| 情况 | 例子 |
|------|------|
| 名称映射和分发都存在 | `read`, `write`, `mmap`, `clone`, `socket` |
| 名称映射存在但分发表未注册 | `ls`, `clear` |
| 非标准且注册 | `shutdown`(501), `ext4_counters`(503), `open`(506), `get_time`(1690) |

未知 id 的名称为 `"unknown"`。

## 5. 日志策略

`syscall_info_log_enabled` 在 `LOG=info|debug|trace` 时为真。入口日志排除一组高频 syscall：

| 排除项 | 原因 |
|--------|------|
| `yield` | 调度热路径 |
| `write` | 输出高频 |
| `getdents64` | 目录遍历高频 |
| `readv`, `writev` | 向量 I/O 高频 |
| `pselect6` | 等待循环高频 |
| `sigaction`, `sigprocmask` | libc 启动和信号路径高频 |
| `clock_gettime` | 时间查询高频 |

返回日志使用 `Errno::try_from(ret)` 判断负 errno；非 errno 值按十六进制输出。

## 6. Seccomp

`syscall()` 在进入 `match` 前检查：

```rust
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
```

| action | 行为 |
|--------|------|
| `Allow` | 继续进入 syscall 分发 |
| `KillThread(signal)` | 记录失败检查，当前线程以对应信号退出 |
| `KillProcess(signal)` | 记录失败检查，整个进程组退出 |

seccomp 检查耗时由 `task::perf` 中的 seccomp 计数记录。

## 7. 架构差异：`clone`

`SYSCALL_CLONE` 在分发表中使用架构条件编译：

| 架构 | 原始 ABI | 传入 `sys_clone()` |
|------|----------|--------------------|
| 非 la64 | `flags, stack, parent_tidptr, tls, child_tidptr` | `flags, stack, ptid, tls, ctid` |
| la64 | `flags, stack, parent_tidptr, child_tidptr, tls` | `flags, stack, ptid, tls, ctid` |

la64 分支显式交换 `args[3]` 和 `args[4]` 的解释，以匹配该架构 raw clone ABI：

```rust
// LoongArch raw clone ABI 为 flags, stack, ptid, ctid, tls。
SYSCALL_CLONE => sys_clone(
    args[0] as u32,
    args[1] as *const u8,
    args[2] as *mut u32,
    args[4],
    args[3] as *mut u32,
)
```

`clone3` 通过 `sys_clone3(args[0] as *const u8, args[1])` 进入结构体参数路径，不复用 raw clone 参数布局。

## 8. 返回值与 errno

### 8.1 errno 常量

`os/src/syscall/errno.rs` 中 errno 常量本身是负值：

| errno | 值 |
|-------|----|
| `EPERM` | `-1` |
| `EBADF` | `-9` |
| `EAGAIN` | `-11` |
| `EFAULT` | `-14` |
| `EINVAL` | `-22` |
| `ENOSYS` | `-38` |
| `ENOPROTOOPT` | `-92` |
| `EAFNOSUPPORT` | `-97` |

`ENOSYS` 的注释明确说明：它用于不存在的 syscall；已经存在的 syscall 不应把内部“不支持的参数/功能”伪装成 `ENOSYS`。

### 8.2 层级约定

| 层 | 成功 | 失败 |
|----|------|------|
| syscall handler | `isize >= 0` | 负 errno |
| `Socket::try_recv()/try_send()` | `Ok(isize)` | `Err(SyscallErr)` |
| `File::read()/write()` | `Result<usize, SyscallErr>` 或旧式 `usize` errno 编码路径 | 由 syscall 层转成负 errno |

`syscall()` 不重新包装 errno；领域 handler 返回什么，最终就写回用户态 `a0`。

## 9. 用户内存访问

用户指针访问统一通过 `mm/uaccess.rs`：

| API/类型 | 行为 |
|----------|------|
| `UserPtr<T>` | 只读用户对象；NULL 返回 `EFAULT` |
| `UserPtrMut<T>` | 可读写用户对象；写入用 `copy_to_user` |
| `UserSlice<T>` | 数组读写；字节长度超过 8 MiB 返回 `EFAULT` |
| `UserCString` | NUL 结尾字符串读取 |
| `UserBufferReader` | 用户 buffer 读入内核；优先单页 fast path |
| `UserBufferWriter` | 内核数据写入用户 buffer |
| `UserIoVec` | 读取 iovec，`MAX_IOVEC_COUNT = 1024` |
| `translated_byte_buffer` | 将用户 buffer 按页切成内核可访问片段 |
| `copy_from_user` / `copy_to_user` | 对用户地址进行 fault-in 后复制 |

`MAX_BUFFER_SIZE = 8 MiB`，用于限制单次用户 buffer 翻译，避免内核因用户传入巨大长度而 OOM。

### 9.1 地址范围检查

`check_user_range(ptr, len)` 只做整数溢出和 `USER_VA_END` 范围检查。真正的权限检查发生在 `fault_in_user_va()` 和页表访问权限验证阶段。

### 9.2 token 限制

`current_user_vm(token)` 要求传入 token 等于当前任务用户页表 token。跨进程 VM 访问不能直接复用普通 uaccess token，需要走 `process_vm_readv/writev` 的远程进程路径。

### 9.3 iovec

`UserIoVec::read_user_iovecs()`：

| 检查 | 失败 errno |
|------|------------|
| `iovcnt > 1024` | `EINVAL` |
| `Vec::try_reserve(iovcnt)` 失败 | `ENOMEM` |
| iovec 总长度溢出或超过 `isize::MAX` | `EINVAL` |

构造 reader/writer buffer 时，iovec 会按 `total_cap` 截断。

## 10. 未知 syscall

未匹配 id 进入 `_` 分支：

```rust
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
```

分支中保留了向当前任务注入 `SIGSYS` 的注释代码，但当前运行路径不发送该信号。

## 11. 关键边界

| 边界 | 说明 |
|------|------|
| syscall 参数最多 6 个 | 由 trap ABI 和 `args: [usize; 6]` 固定 |
| `rt_sigreturn` 返回值特殊 | trap 后端不覆盖 `a0` |
| `SYSCALL_SOCK_SHUTDOWN = 210` | socket `shutdown(2)`，进入 `net/syscall/shutdown.rs` |
| `SYSCALL_SHUTDOWN = 501` | MangoCore 系统关机入口，进入 `sys_shutdown()` |
| `open = 506` | 非标准兼容入口，包装为 `sys_openat(AT_FDCWD, path, flags, 0o777)` |
| syscall 名称表可能包含未注册分支 | 以 `match syscall_id` 是否有分支为准 |

## 12. 调试入口

| 症状 | 检查点 |
|------|--------|
| 返回 `ENOSYS` | `syscall_id.rs` 常量和 `syscall/mod.rs` match 分支 |
| 参数错位 | trap 后端 ABI 收集和 `clone` 架构条件分支 |
| 用户指针 `EFAULT` | `mm/uaccess.rs` 的 range、token、fault-in、跨页检查 |
| seccomp 杀线程/进程 | `seccomp_action_for_syscall()` 和 task seccomp 状态 |
| 日志过多/过少 | `LOG` 环境变量和高频 syscall blacklist |

### 12.1 新增或核对 syscall 的阅读顺序

核对一个 syscall 是否真正可用时，按下面顺序看代码：

1. `syscall/syscall_id.rs` 是否定义编号常量。
2. `syscall/mod.rs::syscall_name()` 是否能把编号映射到名称。
3. `syscall/mod.rs::syscall()` 的 `match syscall_id` 是否有分支。
4. 分支是否把 `args[0..5]` 按 ABI 正确转换为指针、整数或 flag。
5. 领域 `sys_xxx()` 是否进行用户指针读取、fd 查表、权限检查和错误码转换。
6. 阻塞路径是否释放业务锁后等待，并把 signal interruption 映射成 `ERESTART` 或领域 errno。

只有前 3 步同时满足，编号才会进入实现。只存在名称映射但没有 match 分支的编号，运行时仍返回 `ENOSYS`。

### 12.2 参数转换的常见模式

`syscall()` 的 match 分支是 ABI 和 Rust 类型之间的边界，常见写法有三类：

| 用户 ABI | 分支转换 | 后续处理 |
|----------|----------|----------|
| 用户地址 | `args[n] as *const u8`、`*mut T` 或 `usize` | 领域函数通过 uaccess 读取，不能直接解引用。 |
| flag/mode | `args[n] as u32` | 领域函数转成 bitflags，未知 bit 返回对应 errno。 |
| fd/pid/size | `usize` 或 `isize` | 领域函数查 fd table、registry 或校验范围。 |

这种转换集中在分发层，使领域函数可以保持接近 Linux syscall 原型；但安全性不在 `as *const` 这一行完成，而是在后续的 `copy_from_user`、`translated_str`、`UserIoVec`、fd table 查找和权限分支中完成。

## 13. 源文件索引

| 路径 | 内容 |
|------|------|
| `os/src/hal/arch/riscv/trap/mod.rs` | rv64 syscall trap 接入 |
| `os/src/hal/arch/loongarch64/trap/mod.rs` | la64 syscall trap 接入 |
| `os/src/syscall/mod.rs` | syscall 名称、seccomp、分发、日志、perf |
| `os/src/syscall/syscall_id.rs` | syscall 编号 |
| `os/src/syscall/errno.rs` | errno 常量和 `Errno` 枚举 |
| `os/src/mm/uaccess.rs` | 用户指针、buffer、iovec、fault-in |
| `os/src/syscall/process/ids.rs` | seccomp action 判定、prctl/seccomp 分支 |
| `os/src/task/task.rs` | task seccomp 状态 |
