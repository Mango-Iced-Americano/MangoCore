---
title: "进程生命周期与身份 syscall"
category: syscall
status: stable
author: MangoCore Team
last_update: 2026-07-31
tags: [syscall, process, clone, exec, ids]
---

# 进程生命周期与身份 syscall

## 1. 概述

进程相关 syscall 分布在：

| 文件 | 范围 |
|------|------|
| `syscall/process/clone.rs` | `clone`, `clone3`, `unshare`, `setns` |
| `syscall/process/exec.rs` | `execve`, `execveat` |
| `syscall/process/lifecycle.rs` | `exit`, `exit_group`, `wait4`, `waitid`, robust list |
| `syscall/process/ids.rs` | UID/GID、PID、进程组、session、capability、prctl、rlimit、sched |
| `syscall/process/misc.rs` | 少量 misc 入口通过 process 模块导出 |

这些 syscall 直接操作 `TaskControlBlock`、`ProcessControlBlock`、地址空间、fd table、namespace、信号和调度状态。

## 2. clone

### 2.1 raw clone 分发表

`syscall/mod.rs` 对 raw `clone` 做架构 ABI 适配：

| 架构 | 用户 ABI | 传给 `sys_clone()` |
|------|----------|--------------------|
| 非 la64 | `flags, stack, ptid, tls, ctid` | 原顺序 |
| la64 | `flags, stack, ptid, ctid, tls` | 交换为 `ptid, tls, ctid` |

`sys_clone()` 额外检查：

| 条件 | errno |
|------|-------|
| `CLONE_PIDFD` 与 `CLONE_PARENT_SETTID` 同时设置 | `EINVAL` |

`CLONE_PIDFD` 使用 `ptid` 指针作为 pidfd 写回地址。

### 2.2 `sys_clone_inner`

核心流程：

```
unallocated_frames() < 32 -> ENOMEM
解析低 8 位 exit_signal
CloneFlags::from_bits(flags & !0xff)
校验 flag 依赖
parent.sys_clone(flags, stack, tls, exit_signal)
la64 分配 ASID
CLONE_PARENT_SETTID 写 parent tid pointer
CLONE_CHILD_SETTID 写 child 用户地址
CLONE_CHILD_CLEARTID 设置 clear_child_tid
CLONE_PIDFD 创建 pidfd 并写回父用户地址
ProcessManager::publish_clone_child()
ProcessManager::schedule_clone_child()
返回 child tid
```

flag 依赖：

| 条件 | errno |
|------|-------|
| `CLONE_SIGHAND` 缺少 `CLONE_VM` | `EINVAL` |
| `CLONE_THREAD` 缺少 `CLONE_SIGHAND` | `EINVAL` |
| `CLONE_VFORK` 与 `CLONE_THREAD` 同时设置 | `EINVAL` |
| `CLONE_NEWNS` 与 `CLONE_FS` 同时设置 | `EINVAL` |
| `CLONE_NEWUTS/NEWNET/NEWNS/NEWIPC` 且父进程 euid 非 0 | `EPERM` |

写用户 tid/pidfd 失败时，会调用 `cleanup_unpublished_clone()` 清理未发布子任务；pidfd 已分配但写回失败时会关闭父进程中新建 fd。

### 2.3 clone3

`clone3` 读取 `CloneArgs`：

| 检查 | errno |
|------|-------|
| size 小于 `CloneArgsV0` | `EINVAL` |
| size 大于 `PAGE_SIZE` | `E2BIG` |
| uargs 为 NULL | `EFAULT` |
| size 大于支持结构体且 extra bytes 非 0 | `E2BIG` |
| flags 高 32 位非 0 | `EINVAL` |
| exit_signal > 0xff | `EINVAL` |
| `CLONE_PIDFD` 但 pidfd 指针不可写 | `EFAULT` |
| stack 和 stack_size 只有一个为 0 | `EINVAL` |
| stack + stack_size 溢出 | `EINVAL` |

`clone3` 把 `exit_signal` OR 进 flags 低 8 位，然后复用 `sys_clone_inner()`。

`sys_clone()` 和 `sys_clone3()` 在 syscall 层的包装如下：

```rust
pub fn sys_clone(
    flags: u32,
    stack: *const u8,
    ptid: *mut u32,
    tls: usize,
    ctid: *mut u32,
) -> isize {
    if flags & CloneFlags::CLONE_PIDFD.bits() != 0
        && flags & CloneFlags::CLONE_PARENT_SETTID.bits() != 0
    {
        return EINVAL;
    }
    let pidfd_ptr = if flags & CloneFlags::CLONE_PIDFD.bits() != 0 {
        Some(ptid)
    } else {
        None
    };
    sys_clone_inner(flags, stack, ptid, tls, ctid, pidfd_ptr)
}

pub fn sys_clone3(uargs: *const u8, size: usize) -> isize {
    let token = current_user_token();
    let args = match read_clone3_args(uargs, size, token) {
        Ok(args) => args,
        Err(errno) => return errno,
    };

    if args.flags >> 32 != 0 {
        return EINVAL;
    }

    let mut flags = args.flags as u32;
    if args.exit_signal > 0xff {
        return EINVAL;
    }
    if flags & CloneFlags::CLONE_PIDFD.bits() != 0
        && translated_byte_buffer(
            token,
            args.pidfd as *const u8,
            core::mem::size_of::<u32>(),
            UserAccess::Write,
        )
        .is_err()
    {
        return EFAULT;
    }
    if (args.stack == 0) != (args.stack_size == 0) {
        return EINVAL;
    }
    flags |= args.exit_signal as u32;

    let stack = if args.stack == 0 {
        core::ptr::null()
    } else {
        match (args.stack as usize).checked_add(args.stack_size as usize) {
            Some(sp) => sp as *const u8,
            None => return EINVAL,
        }
    };

    let pidfd_ptr = if flags & CloneFlags::CLONE_PIDFD.bits() != 0 {
        Some(args.pidfd as *mut u32)
    } else {
        None
    };

    sys_clone_inner(
        flags,
        stack,
        args.parent_tid as *mut u32,
        args.tls as usize,
        args.child_tid as *mut u32,
        pidfd_ptr,
    )
}
```

传统 clone 的 `ptid` 在 `CLONE_PIDFD` 场景下被复用为 pidfd 写回指针；clone3 的 `pidfd` 是结构体字段，syscall 层会先校验它可写。

## 3. unshare 和 setns

### 3.1 unshare

支持 flags：

| flag | 行为 |
|------|------|
| `CLONE_FILES` | `process.unshare_files()` |
| `CLONE_FS` | `process.unshare_fs()` |
| `CLONE_NEWUTS` | `process.unshare_uts()` |
| `CLONE_NEWNET` | 要求 live thread count 为 1，`process.unshare_net()` |
| `CLONE_NEWNS` | 要求 live thread count 为 1，`process.set_mnt(MountNamespace::new())` |
| `CLONE_NEWIPC` | 要求 live thread count 为 1，`process.set_ipc(IpcNamespace::new())` |

不在支持集合中的 flag 返回 `EINVAL`。创建 UTS/NET/MNT/IPC namespace 要求 euid 为 0，否则 `EPERM`。

### 3.2 setns

`sys_setns(fd, nstype)` 支持：

| nstype | inode 类型 | 行为 |
|--------|------------|------|
| 0 或 `CLONE_NEWNET` | `ProcNsNetInode` | euid 0 后切换 net namespace |
| 0 或 `CLONE_NEWNS` | `ProcNsMntInode` | euid 0 后切换 mount namespace |
| 0 或 `CLONE_NEWIPC` | `ProcNsIpcInode` | euid 0 后切换 IPC namespace |

未知 nstype 返回 `EINVAL`；fd 不存在返回 `EBADF`；权限不足返回 `EPERM`；fd 不是支持的 ns inode 返回 `EINVAL`。

## 4. execve / execveat

### 4.1 打开和校验可执行文件

`exec.rs` 的校验：

| 检查 | errno |
|------|-------|
| 路径过长或 component 过长 | `ENAMETOOLONG` |
| 目标不是普通文件 | 目录为 `EISDIR`，其他为 `EACCES` |
| 当前 fsuid/fsgid/groups 没有执行权限 | `EACCES` |
| inode 正被写打开 | `ETXTBSY` |
| `AT_SYMLINK_NOFOLLOW` 命中符号链接 | `ELOOP` |
| ELF magic 不匹配且非 shebang | `ENOEXEC` |

root (`fsuid == 0`) 仍要求任意 execute 位存在。

### 4.2 argv/envp

`read_exec_vectors()` 从用户空间读取 argv/envp：

| 行为 | 说明 |
|------|------|
| argv NULL 或空 | 自动添加空字符串 argv[0] |
| 每个字符串 | 使用 `UserCString` 读取 |
| 总字节 | 不能超过 `USER_STACK_INIT_SIZE / 2` |
| Vec 扩容失败 | `ENOMEM` |
| 栈布局估算超过 `USER_STACK_INIT_SIZE - PAGE_SIZE` | `E2BIG` |

辅助 `validate_exec_stack_usage()` 会把字符串、AT_RANDOM、auxv、argv/envp 指针数组和 argc 都计入用户栈容量。

### 4.3 shebang

`parse_shebang()` 读取前 128 字节：

```
#! interpreter [arg]
```

若 interpreter 是有效 ELF，则构造：

```
argv = [interpreter, optional_arg, script_path, original argv[1..]]
```

若 interpreter 打不开或不是有效 ELF，尝试 shell fallback：`/bin/sh`，再 `/bin/bash`。fallback 成功时会把脚本路径插入 argv。

### 4.4 execveat

`execveat` 支持 flags：

| flag | 行为 |
|------|------|
| `AT_SYMLINK_NOFOLLOW` | 最后一段不跟随符号链接 |
| `AT_EMPTY_PATH` | pathname 为空时从 dirfd 指向文件执行 |

未知 flags 返回 `EINVAL`。空路径但没有 `AT_EMPTY_PATH` 返回 `ENOENT`；空路径且 dirfd 为 `AT_FDCWD` 也返回 `ENOENT`。

exec 成功后：

```
task.load_elf(...)
process.mark_execed()
process.set_exe_path(abs_path)
process.complete_vfork()
```

`load_elf()` 内部完成地址空间替换、CLOEXEC fd 关闭、信号/futex 状态重置和同进程其他线程清理。

`sys_execve()` 本身只负责读取路径和参数，随后进入 `exec_opened_file()`：

```rust
pub fn sys_execve(pathname: *const u8, argv: *const *const u8, envp: *const *const u8) -> isize {
    let task = current_task().unwrap();
    let token = current_user_token();
    let fs_ref = task.process.fs();
    let path = match UserCString::new(pathname).read(token) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    if let Err(errno) = validate_exec_path_len(&path) {
        return errno;
    }
    let (argv_vec, envp_vec) = match read_exec_vectors(token, argv, envp) {
        Ok(v) => v,
        Err(errno) => return errno,
    };
    let (working_inode, working_path) = {
        let lock = fs_ref.lock();
        (lock.working_inode.clone(), lock.working_path.clone())
    };
    let cwd_inode: Arc<dyn vfs::IndexNode> = working_inode.inode.clone();
    let abs_path = make_abs_exec_path(&path, &working_path);

    match open_exec(&cwd_inode, &path) {
        Ok(file) => exec_opened_file(&cwd_inode, &path, abs_path, file, argv_vec, envp_vec),
        Err(errno) => errno,
    }
}
```

因此 path/argv/envp 的用户内存错误在替换地址空间前返回；ELF/shebang 构造阶段由 `exec_opened_file()` 和 `load_elf()` 继续处理。

## 5. exit 和 wait

### 5.1 exit

| syscall | 行为 |
|---------|------|
| `exit(code)` | `exit_current_and_run_next((code & 0xff) << 8)` |
| `exit_group(code)` | `exit_group_and_run_next((code & 0xff) << 8)` |

退出状态按传统 wait status 格式左移 8 位。

### 5.2 wait4

`sys_wait4(pid, status, option, ru)`：

| pid | 语义 |
|-----|------|
| `pid > 0` | 等待指定 pid |
| `pid == -1` | 等待任意子进程 |
| `pid == 0` | 等待同进程组子进程 |
| `pid < -1` | 等待 pgid 为 `|pid|` 的子进程 |

`pid == i32::MIN` 返回 `ESRCH`。option 必须由 `WaitOption` 支持的位组成，否则 `EINVAL`。status 非 NULL 时写回用户地址。

`sys_wait4()` 的源码包装如下：

```rust
pub fn sys_wait4(pid: isize, status: *mut u32, option: u32, _ru: *mut Rusage) -> isize {
    if pid == i32::MIN as isize {
        return ESRCH;
    }
    let option = match WaitOption::from_bits(option) {
        Some(option) => option,
        None => return EINVAL,
    };
    let task = current_task().unwrap();
    let token = current_user_token();
    let process = task.process.clone();
    match ProcessManager::wait_child(
        &process,
        pid,
        option.contains(WaitOption::WNOHANG),
        true,
        option.contains(WaitOption::WSTOPPED),
        option.contains(WaitOption::WCONTINUED),
        option.contains(WaitOption::WNOWAIT),
    ) {
        Ok(Some(child)) => {
            if !status.is_null() {
                if let Err(errno) = UserPtrMut::new(status).write(token, &child.status) {
                    return errno;
                }
            }
            child.pid as isize
        }
        Ok(None) => SUCCESS,
        Err(errno) => errno,
    }
}
```

### 5.3 waitid

`waitid` 要求 options 至少包含 `WEXITED | WSTOPPED | WCONTINUED` 之一，否则 `EINVAL`。`WSTOPPED/WCONTINUED` 会被解析接受；完整 stopped/continued 子进程状态上报尚未接入，主路径仍以 exited/zombie child 为准。支持 idtype：

| idtype | 语义 |
|--------|------|
| `P_ALL` | 任意子进程 |
| `P_PID` | 指定 pid |
| `P_PGID` | 指定进程组；id=0 使用当前进程组 |
| `P_PIDFD` | 从 pidfd file 取得目标 pid |

pidfd 为 nonblock 且目标未 zombie 时返回 `EAGAIN`。`WNOWAIT` 会保留子进程等待状态。

`waitid_siginfo()` 根据 wait status 生成 `SIGCHLD` siginfo，覆盖 exited、killed、dumped、stopped、continued。

## 6. robust list 与 clear child tid

| syscall/flag | 行为 |
|--------------|------|
| `set_tid_address(tidptr)` | 设置当前任务 `clear_child_tid`，返回 tid |
| `CLONE_CHILD_CLEARTID` | clone 时设置子任务 `clear_child_tid` |
| `set_robust_list(head, len)` | len 必须等于 `RobustList::HEAD_SIZE` |
| `get_robust_list(pid, head_ptr, len_ptr)` | pid=0 查询当前任务；非 0 路径按目标任务权限处理 |

退出路径会在 clear child tid 地址写 0 并唤醒对应 futex。

## 7. UID/GID 与进程组

### 7.1 身份查询

`getpid/getppid/getuid/geteuid/getgid/getegid/gettid` 读取调度器发布的当前任务/身份 hint 或任务字段，避免不必要的重锁。

### 7.2 setuid/setgid 系列

`ids.rs` 维护 real/effective/saved/fs uid/gid 以及 groups。相关 syscall 包括：

| 类别 | syscall |
|------|---------|
| UID | `setuid`, `setreuid`, `setresuid`, `getresuid`, `setfsuid` |
| GID | `setgid`, `setregid`, `setresgid`, `getresgid`, `setfsgid` |
| groups | `getgroups`, `setgroups` |

`NGROUPS_MAX = 65536`，并保留 `LEGACY_NGROUPS_MAX = 32` 常量。

### 7.3 进程组和 session

| syscall | 行为 |
|---------|------|
| `setpgid` | 修改目标进程进程组，涉及父子关系、exec 状态和 session 检查 |
| `getpgid` | 查询进程组 |
| `setsid` | 创建新 session |
| `getsid` | 查询 session |

当前任务查询直接读取 PCB 的 PGID/SID 权威原子 hint，不再维护调度器影子缓存。

## 8. capability、prctl、rlimit、sched

### 8.1 capability

`ids.rs` 支持 capability 版本：

| 版本常量 |
|----------|
| `LINUX_CAPABILITY_VERSION_1 = 0x19980330` |
| `LINUX_CAPABILITY_VERSION_2 = 0x20071026` |
| `LINUX_CAPABILITY_VERSION_3 = 0x20080522` |

`CAP_LAST_CAP = 40`，`CAP_FULL_SET` 覆盖 0..40。

### 8.2 prctl

支持的 prctl 选项包括 pdeathsig、dumpable、keepcaps、task name、seccomp、cap bounding set、securebits、timer slack、child subreaper、no_new_privs、THP disable、ambient capability、speculation ctrl 等。具体分支位于 `sys_prctl()`。

### 8.3 rlimit

`getrlimit/setrlimit/prlimit` 读写当前或目标进程资源限制。`RLIMIT_NOFILE_MAX = 1024 * 1024`。

### 8.4 scheduler ABI

注册的调度 syscall：

| syscall |
|---------|
| `sched_setparam`, `sched_setscheduler`, `sched_getscheduler`, `sched_getparam` |
| `sched_setaffinity`, `sched_getaffinity` |
| `sched_get_priority_max`, `sched_get_priority_min`, `sched_rr_get_interval` |
| `sched_setattr`, `sched_getattr` |

调度器已使用 Per-CPU RunQueue，但普通生产任务暂时仍为 CPU0-only。B32 的 raw
`sched_getaffinity()` 已按 TID 返回 TCB 的真实 `cpus_allowed`，成功值为复制字节数。B34 的
`sched_setaffinity()` 已支持 current 线程改 mask 与必要自迁移；B35 又支持非 current 的稳定
Blocked 线程在 registry 锁内改 mask，并由后续 wake 按新 mask 选点；B36 再支持稳定
Queued 线程在 owner runqueue 内更新 mask，必要时经短暂 `Migrating` 搬到合法 CPU。B37
让新任务与 wake 按 affinity、在线状态、局部性和近似负载选择 owner；B38 再让远程
Running 线程通过请求—安全点—完成协议真正交接 owner，Blocking 短窗口则等待稳定后重试。
普通任务默认 mask 仍是 bit0，已离开可管理容器的任务仍可能返回 `EOPNOTSUPP`，因此这还不是
完整 Linux affinity 语义。

## 9. 错误码边界

| 场景 | errno |
|------|-------|
| clone 物理页少于 32 | `ENOMEM` |
| clone flag 依赖不满足 | `EINVAL` |
| namespace 创建权限不足 | `EPERM` |
| unshare unsupported flags | `EINVAL` |
| unshare NEWNET/NEWNS/NEWIPC 且多线程 | `EINVAL` |
| setns fd 不存在 | `EBADF` |
| exec 路径过长 | `ENAMETOOLONG` |
| exec 非普通文件 | `EISDIR` 或 `EACCES` |
| exec 无执行权限 | `EACCES` |
| exec inode 正被写打开 | `ETXTBSY` |
| exec argv/envp 超栈容量 | `E2BIG` |
| wait4 pid 为 `i32::MIN` | `ESRCH` |
| waitid options 不含等待类型 | `EINVAL` |
| robust list len 错误 | `EINVAL` |

这些 errno 的先后顺序直接影响 LTP 兼容性。进程类 syscall 通常先做结构性校验，再做权限和对象查找：clone 先检查 flag 依赖，namespace 再检查 euid；exec 先把路径和 argv/envp 从用户态安全读入，再检查文件类型、权限、busy 状态和 ELF/shebang；wait 先校验 options，再决定是否扫描 children、是否进入 `child_exit_wait`。

读生命周期 syscall 时可以按三层定位：`syscall/process/*.rs` 负责 ABI 参数和 errno 优先级；`task/task.rs` 负责 TCB 创建、exec 装载和线程级退出；`task/process.rs` 负责 PCB 资源、children、wait 可见状态和最终回收。不要把这三层的职责混在一个函数里找。

## 10. 测试映射

| 功能 | 代表测试 |
|------|----------|
| clone/clone3 | LTP `clone*`, `clone3*` |
| execve/execveat | LTP `execve*`, `execveat*`, shebang 测试 |
| exit/wait | LTP `wait*`, `waitid*`, shell 子进程测试 |
| pid/uid/gid | LTP `getpid*`, `setuid*`, `setgid*`, `getgroups*` |
| process group/session | LTP `setpgid*`, `setsid*`, `getsid*` |
| prctl/cap/rlimit | LTP `prctl*`, `capget*`, `getrlimit*`, `prlimit*` |
| scheduler ABI | LTP `sched_*` |
| namespace | `unshare*`, `setns*` 中 net/mnt/ipc 相关用例 |

## 11. 源文件索引

| 路径 | 内容 |
|------|------|
| `os/src/syscall/process/clone.rs` | clone、clone3、unshare、setns |
| `os/src/syscall/process/exec.rs` | execve、execveat、shebang、argv/envp |
| `os/src/syscall/process/lifecycle.rs` | exit、wait、robust list |
| `os/src/syscall/process/ids.rs` | UID/GID、capability、prctl、rlimit、sched |
| `os/src/task/task.rs` | TaskControlBlock |
| `os/src/task/process.rs` | ProcessControlBlock |
| `os/src/task/manager.rs` | wait child、进程管理 |
| `os/src/fs/pidfd.rs` | pidfd 文件对象 |
