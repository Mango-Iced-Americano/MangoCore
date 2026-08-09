---
title: "信号、pidfd 与 signalfd"
category: process
status: stable
author: MangoCore Team
last_update: 2026-07-31
tags: [process, signal, pidfd, signalfd]
---

# 信号、pidfd 与 signalfd

## 1. 源码位置

信号实现分两层：

| 文件 | 内容 |
|------|------|
| `os/src/task/signal/` | 信号 action、pending queue、投递、frame、wait |
| `os/src/syscall/process/signal.rs` | signal/pidfd/signalfd syscall |
| `os/src/task/process.rs` | 进程级 shared pending、线程组退出门禁、stopped/continued |
| `os/src/task/task.rs` | 线程级 sigmask/pending/signal stack |

syscall 层负责参数和权限校验；task/signal 层负责具体投递、取 pending、构造/恢复 signal frame。

## 2. 线程级与进程级 pending

| pending | 位置 | 来源 |
|---------|------|------|
| 线程级 | `TaskControlBlockInner::sigpending` | `tkill/tgkill`、同步异常信号、deferred thread signal |
| 进程级 | `ProcessSignalState::shared_pending` | `kill(pid)`、`killpg`、`pidfd_send_signal` |

`signalfd` 和 `sigtimedwait` 取 pending 时会先看线程队列，再看进程 shared pending。

signalfd 的等待通知域放在 `Sighand` 中，而不是 `SignalFd` inode 中。普通 fork 复制 action
并创建新通知域；`CLONE_SIGHAND` 共享 action 与通知域。共享 open file 只保存 mask，read/poll
每次都按当前任务动态取得通知域，因此 fd 继承不会改变 pending 的所有权。

B40 后 group-exit 状态不再放在 `ProcessSignalState` 内。fatal signal 的默认动作进入
`exit_group_and_run_next()`，由进程级原子退出码通知所有 sibling；私有 SIGKILL 负责
打断睡眠，真正的线程资源清理仍由各 CPU 的任务安全点执行。

`SignalFd` 把这一语义暴露成一个可读的 `IndexNode`。结构体只保存 mask 和 metadata（以及可重绑定的 `Sighand` 事件队列）；pending 数据仍在 TCB/PCB 的信号队列里：

```rust
struct SignalFd {
    mask: Mutex<Signals>,
    metadata: Metadata,
    event_queue: Mutex<Arc<EventWaitQueue>>,
}

impl SignalFd {
    fn new(mask: Signals, event_queue: Arc<EventWaitQueue>) -> Self {
        Self {
            mask: Mutex::new(mask),
            metadata: Metadata::new(
                FileType::File,
                InodeMode::S_IFREG | InodeMode::from_bits_truncate(0o600),
            ),
            event_queue: Mutex::new(event_queue),
        }
    }

    fn set_mask(&self, mask: Signals) {
        *self.mask.lock() = mask;
    }

    fn pending_mask(&self) -> Signals {
        *self.mask.lock()
    }

    fn rebind_event_queue(&self, new_queue: Arc<EventWaitQueue>) {
        *self.event_queue.lock() = new_queue;
    }
}
```

fork/clone 未设置 `CLONE_SIGHAND` 时会创建新的 `Sighand` 与事件队列。fd table 复制完成后，子进程遍历继承的 signalfd 并调用 `rebind_event_queue()`；因此子进程的阻塞 signalfd read 只会由子进程收到的信号唤醒。该路径不改变 VFS 的 `PollWaitQueue`、`EventQueueHandle` 或 `IndexNode` 接口。

线程级 pending 和进程级 pending 的分离对应 Linux 的两个投递目标：`tkill/tgkill` 指向具体线程，所以进入 TCB；`kill(pid)` 指向线程组，所以进入 PCB 的 shared pending，之后由可接收该信号的线程在返回用户态或等待信号时取走。信号 mask 是线程级状态，因此同一个进程里的不同线程可能对 shared pending 信号有不同可接收性。

读 signal 代码时要分清三张表：`sighand` 保存 handler/action，属于进程共享资源；`sigmask` 属于 TCB，决定当前线程屏蔽哪些信号；pending 队列保存已经投递但尚未交付的信号。`rt_sigreturn` 则是从用户信号帧恢复 trap context 和 mask 的返回路径。

### 2.1 fast pending、投递时机与 syscall restart

每个 TCB 另有原子 `signal_pending` 提示位。它是“当前线程或其进程 shared pending 中可能存在未被当前 `sigmask` 屏蔽的信号”的快速提示，不替代 pending 队列或 action 表。线程/进程 pending 的入队和取走、mask/action 修改以及丢弃路径都会重算该位，避免 stale hint 使返回用户态路径无谓扫描队列。

两个架构的 `trap_return()` 都在释放 `task.inner` 后检查该位：为 false 时直接恢复用户态；为 true 时才调用 `do_signal()` 构造 signal frame 或执行默认动作。signal frame 投递仍只发生在返回用户态的安全点，不移入调度器或 waitqueue。

可被信号打断的阻塞 syscall 在内核内部以 typed `RestartKind` 表达 restart 结果，用户态不会收到 Linux 内部的 `-ERESTART*` 数值。`RestartSys` 仅在 handler 使用 `SA_RESTART` 时回退 syscall PC/参数；不满足 restart 条件及明确不可重启的 wait/signal/time/poll 路径都返回 `EINTR`。被忽略的信号不应把可重启调用错误地暴露为 `EINTR`。

向 interruptible waiter 投递信号时，唤醒判断同时考虑线程 `sigmask` 与 sigwait mask；被屏蔽信号仍入队，但不抢先唤醒等待者。

## 3. 信号权限

`can_signal_process(target)`：

| 条件 | 允许 |
|------|------|
| sender pid == target pid | 是 |
| sender euid == 0 | 是 |
| target 没有 live thread | 是 |
| sender uid/euid 匹配 target uid/suid | 是 |
| 其他 | 否 |

不允许时返回 `EPERM`。目标不存在返回 `ESRCH`，信号号无效返回 `EINVAL`。

权限判断源码如下：

```rust
fn can_signal_process(target: &ProcessControlBlock) -> bool {
    let Some(sender) = current_task() else {
        return false;
    };
    if sender.pid() == target.pid {
        return true;
    }
    let sender_uid = current_uid();
    let sender_euid = current_euid();

    if sender_euid == 0 {
        return true;
    }

    let Some(target_task) = target.any_live_thread() else {
        return true;
    };
    let target_inner = target_task.acquire_inner_lock();
    sender_uid == target_inner.uid
        || sender_uid == target_inner.suid
        || sender_euid == target_inner.uid
        || sender_euid == target_inner.suid
}
```

这里没有检查 gid；允许条件只包含同进程、root euid、目标无 live thread、sender uid/euid 匹配目标 uid/suid。

## 4. kill/tkill/tgkill

`sys_kill(pid, sig)`：

| pid | 行为 |
|-----|------|
| `> 0` | 向指定进程投递 |
| `0` | 向当前进程组投递 |
| `-1` | 向所有可投递进程投递 |
| `< -1` | 向指定进程组投递 |

`sys_tkill(tid, sig)` 查找 tid 并投递线程信号。`sys_tgkill(pid, tid, sig)` 要求 tid 属于 pid。

`SIGKILL` 路径会打印诊断日志，包括 sender tid/pid、当前 syscall 和目标。

## 5. pidfd

pidfd 相关 syscall：

| syscall | 行为 |
|---------|------|
| `pidfd_open(pid, flags)` | 为目标进程创建 pidfd 文件 |
| `pidfd_send_signal(pidfd, sig, info, flags)` | 通过 pidfd 投递进程信号 |
| `pidfd_getfd(pidfd, targetfd, flags)` | 复制目标进程 fd 到当前进程 |

`pidfd_open`：

| 条件 | 错误 |
|------|------|
| pid 为 0 或负数 | `EINVAL` |
| flags 除 `O_NONBLOCK` 外非 0 | `EINVAL` |
| 目标进程不存在 | `ESRCH` |

pidfd 文件总是以 CLOEXEC 方式分配 fd。

## 6. pidfd target 解析

`pidfd_file_target_pid(file)` 支持两类 inode：

1. `PidFd` inode。
2. `/proc/[pid]` namespace 目录类 `LockedProcInode`，要求 file type 为 Dir 且 pid 非 0。

如果 proc inode 里保存了 process weak ref，会确认 weak ref 仍指向相同 pid 且 pid 未释放；否则返回 `ESRCH`。

解析函数既支持真实 pidfd，也支持 `/proc/[pid]` 目录 inode：

```rust
pub(super) fn pidfd_file_target_pid(file: &File) -> Result<usize, isize> {
    let inode = MountFSInode::unwrap_inode(&file.inode);
    if let Some(pidfd) = inode.as_any_ref().downcast_ref::<PidFd>() {
        return pidfd.target_pid().map_err(|err| -(err as isize));
    }
    if let Some(proc_inode) = inode.as_any_ref().downcast_ref::<LockedProcInode>() {
        let (file_type, pid, process_ref) = {
            let data = proc_inode.0.lock();
            (
                data.metadata.file_type,
                data.extra_data,
                data.process_ref.clone(),
            )
        };
        if file_type == FileType::Dir && pid != 0 {
            if let Some(process_ref) = process_ref {
                return match process_ref.upgrade() {
                    Some(process) if process.pid == pid && !process.pid_released() => Ok(pid),
                    _ => Err(ESRCH),
                };
            }
            return Ok(pid);
        }
    }
    Err(EBADF)
}
```

因此 `waitid(P_PIDFD)` 和 `pidfd_send_signal()` 能共享同一套 fd 到 pid 的解析逻辑。

## 7. pidfd_send_signal

参数规则：

| 条件 | 错误 |
|------|------|
| flags 非 0 | `EINVAL` |
| sig 无效 | `EINVAL` |
| info 指针非 0 但读取失败 | `EFAULT` |
| info.signo 与 sig 不一致 | `EINVAL` |
| target 不存在 | `ESRCH` |
| 无权限 | `EPERM` |
| 对其他进程发送 kernel-generated siginfo | `EPERM` |

sig 为 0 时只做权限/存在性检查，成功返回 0。

`sys_pidfd_send_signal()` 的主路径如下：

```rust
pub fn sys_pidfd_send_signal(pidfd: usize, sig: usize, info: usize, flags: usize) -> isize {
    if flags != 0 {
        return EINVAL;
    }
    let signal = match Signals::from_signum(sig) {
        Ok(signal) => signal,
        Err(_) => return EINVAL,
    };

    let task = current_task().unwrap();
    let token = current_user_token();
    let queued_siginfo = if info != 0 {
        match UserPtr::<SigInfo>::from_addr(info).read(token) {
            Ok(siginfo) => {
                if siginfo.signo() != sig {
                    return EINVAL;
                }
                Some(siginfo)
            }
            Err(_) => return EFAULT,
        }
    } else {
        None
    };

    let target_pid = match pidfd_target_pid(pidfd) {
        Ok(pid) => pid,
        Err(errno) => return errno,
    };
    let Some(process) = ProcessManager::find_process(target_pid) else {
        return ESRCH;
    };
    if !can_signal_process(&process) {
        return EPERM;
    }
    if signal.is_empty() {
        return SUCCESS;
    }
    match queued_siginfo {
        Some(siginfo) => {
            if target_pid != task.pid() && siginfo.is_kernel_generated() {
                return EPERM;
            }
            send_process_signal_info(&process, signal, siginfo.with_signal_sender(sig, task.pid()));
            SUCCESS
        }
        None => ProcessManager::send_signal_to_process(target_pid, signal),
    }
}
```

这段代码的 errno 顺序是：先校验 flags 和信号号，再读取 siginfo，再解析 pidfd，再检查目标存在和权限。`sig == 0` 在 `Signals::from_signum(0)` 成功得到空信号后只做存在性和权限检查。

## 8. pidfd_getfd

`pidfd_getfd(pidfd, targetfd, flags)`：

1. flags 必须为 0。
2. 解析 pidfd 得到 target pid。
3. 目标进程必须存在且不能是 zombie。
4. 必须通过 `can_signal_process()` 权限检查。
5. 从目标 fd table 获取 `targetfd`。
6. 在当前 fd table 分配新 fd，CLOEXEC 为 true。

它复制的是 `Arc<File>`，因此共享同一底层文件对象。

## 9. signalfd

`SignalFd` 是 devfs 下的 `IndexNode` 实现，内部保存一个信号 mask。

`sys_signalfd4(fd, mask, sigsetsize, flags)`：

| 参数 | 行为 |
|------|------|
| `fd == -1` | 创建新 signalfd |
| `fd >= 0` | 更新已有 signalfd mask |
| flags | 只允许 `SFD_NONBLOCK`、`SFD_CLOEXEC` |
| sigsetsize | 必须至少容纳 u64 |

读取 signalfd：

| 条件 | 结果 |
|------|------|
| len 小于 `SignalfdSiginfo` | `EINVAL` |
| 有 matching pending | 写一个或多个 `SignalfdSiginfo` |
| 无 matching pending | `EAGAIN` |

poll 在有 matching pending 时返回 `EPOLLIN | EPOLLRDNORM`。

阻塞 read、poll 和 epoll 通过 VFS 的 `ReadWaitSource::CurrentSighand` 取得等待队列。信号投递
遵循固定顺序：在 `task.inner` 或 `process.signal` 锁内提交 pending，释放 owner 锁，再调用
`ProcessControlBlock::notify_signalfd()`。等待队列只传递“需要重查”的事件，不复制 pending，
所以多个等待者最终仍由 pending dequeue 唯一决定谁消费信号。

普通 fork 后 child 虽继承同一个 signalfd `File`，但第一次 read/poll 会绑定 child 自己的
sighand 队列。`CLONE_SIGHAND` 线程共享同一队列。Linux 对继承 epoll 注册有一个明确边界：
fork 前建立的 epoll 项仍订阅父 sighand，child 需要重新 `epoll_ctl()` 才能监听自己的信号；
MangoCore 保持同样语义。

`SignalFd::read_at()` 每次读取一个或多个 `SignalfdSiginfo`，没有 matching pending 时直接返回 `EAGAIN`：

```rust
fn read_at(
    &self,
    _offset: usize,
    len: usize,
    buf: &mut [u8],
    _data: MutexGuard<FilePrivateData>,
) -> Result<usize, SyscallErr> {
    let info_size = size_of::<SignalfdSiginfo>();
    if len < info_size || buf.len() < info_size {
        return Err(SyscallErr::EINVAL);
    }

    let count = core::cmp::min(len, buf.len()) / info_size;
    let task = current_task().ok_or(SyscallErr::ESRCH)?;
    let mask = self.pending_mask();
    let mut written = 0usize;
    for slot in 0..count {
        let Some(pending) = take_pending_signal_matching(task, mask) else {
            break;
        };
        let info = SignalfdSiginfo::from_siginfo(pending.siginfo);
        let start = slot * info_size;
        buf[start..start + info_size].copy_from_slice(info.bytes());
        written += info_size;
    }

    if written == 0 {
        Err(SyscallErr::EAGAIN)
    } else {
        Ok(written)
    }
}
```

`poll()` 不消费 pending，只检查 mask 是否命中：

```rust
fn poll(&self, _private_data: &FilePrivateData) -> Result<usize, SyscallErr> {
    let task = current_task().ok_or(SyscallErr::ESRCH)?;
    if has_pending_signal_matching(task, self.pending_mask()) {
        Ok((EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM).bits())
    } else {
        Ok(0)
    }
}
```

## 10. sigaction、sigprocmask 与 sigaltstack

`sys_sigaction(signum, act, oldact, sigsetsize)` 要求 `sigsetsize == size_of::<u64>()`。

`sigaction()` 对 `signum = 0`、`SIGKILL(9)`、`SIGSTOP(19)` 以及 `signum >= 65` 返回 `EINVAL`。其中 `signum = 0` 不作为 handler 查询入口处理；如果只需要权限或存在性检查，应使用 `kill(pid, 0)` 的信号发送语义。

`sys_sigprocmask(how, set, oldset, sigsetsize)` 要求 `sigsetsize >= size_of::<u64>()`。
用户指针固定按低 64 位 mask 读写，不能直接暴露内部 `Signals` 的 Rust 布局；这在
LoongArch 内部 signal bitset 宽于用户 ABI 时尤其重要。

信号 mask 会去掉不可屏蔽信号集合 `Signals::CAN_NOT_BE_MASKED`。

这三个带“新值/旧值”指针的 syscall 统一遵循 read/commit/write：

```text
锁外读取可选新值
  -> 短持 sighand 或 task.inner：快照旧值、校验并提交新值
  -> 解锁
  -> 锁外向用户写回旧值
```

`sigaction` 的 disposition 属于进程共享状态，因此旧 action 的快照和新 action 的替换
必须在同一个 `sighand` 临界区内线性化；`sigprocmask` 和 altstack 属于当前线程，使用
`task.inner`。输入读取失败或新值校验失败时不提交状态；新值已经提交后，旧值 copyout
失败只返回 `EFAULT`，不回滚已发布状态。输入和输出指针相同时也必须先完成输入读取，
否则旧值 copyout 会覆盖尚未读取的新值。

Linux v6.6 的 `rt_sigprocmask` 和 `sigaltstack` wrapper 都只在内部操作成功后 copyout
旧值；因此非法 `how`、非法 altstack flags、当前已在 altstack 或栈空间不足时，返回
对应错误且不写旧值。不要把内部 helper 已经生成旧值误解为 syscall 错误路径仍会
copyout。

## 11. sigpending、sigtimedwait、sigsuspend

| syscall | 行为 |
|---------|------|
| `rt_sigpending` | 返回线程 pending 与进程 shared pending 的并集 |
| `sigtimedwait` | 等待 set 中信号，支持 timeout |
| `rt_sigsuspend` | 临时替换 sigmask 并等待信号 |

等待路径使用 WaitQueue/调度器的可中断睡眠；信号到达时，登记级通知 token 会保存提前 wake，
调度器再按 `Blocking/Blocked` 的实际所有权完成撤销阻塞或重新入队。

## 12. 用户 handler frame 与 sigreturn

### 12.1 handler frame 投递

`do_signal()` 选择自定义 handler 时遵循 snapshot/write/commit：

1. 在 `task.inner -> sighand` 锁序内取出 pending signal、复制 action；若带
   `SA_RESETHAND`，在释放 `sighand` 前把 handler 复位为默认动作。
2. 释放 `sighand`，在 `task.inner` 内按值快照返回寄存器、signal stack、sigmask 和
   `sigmask_to_restore`，并计算 handler 期间使用的新 mask。
3. 释放 `task.inner`，向用户栈写入完整 `SigInfo + UserContext`。
4. 两个用户对象全部写成功后，重新短持 `task.inner`，只提交用户机器寄存器、
   handler 参数、用户 SP/PC/RA 和 signal mask。

用户栈写入可能缺页并进入 MM/TLB 同步，不能跨它持有普通任务锁。写入失败时 live trap
context 和 mask 尚未切换到 handler，当前任务按 `SIGSEGV` 退出；不会出现“PC 已指向
handler，但 frame 只写了一半”的可执行状态。

双架构都使用同一份完整 rt frame，不再为未设置 `SA_SIGINFO` 的 handler 单独拼接
sigmask 和 machine context。`a0` 为信号号，`a1/a2` 始终指向 `SigInfo/UserContext`；
单参数 handler 会按 ABI 自然忽略额外参数。这与 Linux RV64/LA64 的 rt signal frame
入口约定一致，也让投递与 `sigreturn` 始终使用同一种布局。

### 12.2 sigreturn 恢复

`sys_sigreturn()` 从用户 signal frame 恢复：

1. 短持 `task.inner`，只快照当前用户 `sp`。
2. 释放锁后计算 ucontext、sigmask 和 machine context 地址。
3. 通过 `UserPtr::read()` 把 `UserSignalMask`、`MachineContext` 和架构扩展状态全部读入局部值。
4. 全部读取成功后重新短持 `task.inner`，一次提交用户寄存器和 sigmask。
5. LoongArch 先安装完整 LSX，再把 machine context 中标量 FPR 的低 64-bit lane
   合并进去。FPR 与 LSX 物理别名，用户 handler 对标量上下文的修改具有优先级。
6. 返回恢复后的 `a0`。

frame 地址溢出、sigmask、machine context 或 LSX context 读取失败时，当前任务以 `SIGSEGV` 退出。`signal_frame_layout()` 使用 `max(align_of::<UserContext>(), USER_STACK_ABI_ALIGN)` 对齐 ucontext，并将传给用户 handler 的 `sp` 按 16 字节对齐；这样既满足 LoongArch LSX context 的自然对齐，也保持 rv64/la64 用户函数入口 ABI。

恢复路径遵循 snapshot/read/commit：

```text
task.inner {
    sp = trap_context.sp
}

restored_sigmask   = UserPtr(sigmask_addr).read(token)
restored_mcontext  = UserPtr(mcontext_addr).read(token)
restored_extension = UserPtr(extension_addr).read(token)  # LA64

task.inner {
    trap_context.set_machine_context(restored_mcontext)
    restore restored_extension
    sigmask = restored_sigmask
}
```

上例只表达锁边界；实际代码对每次地址加法和每个用户读取分别检查。用户 frame
读取可能缺页并进入 MM/TLB 路径，因此不能跨它持有 `task.inner`。当前线程在 syscall
期间仍是 live trap frame 的唯一执行 owner；远端信号只追加 pending，exec、group-exit
和 affinity 请求要到 owner 的返回安全点才生效，所以锁外读取不会与远端 trap-frame
写者竞争。

`TrapContext::machine_context()` 和 `set_machine_context()` 通过字段复制转换信号 ABI
上下文，不依赖 `TrapContext` 与 `MachineContext` 恰好拥有相同内存前缀，也不会把
`kernel_sp`、页表 token 或 CPU-local 指针暴露给用户。全部用户读取成功后才进入一次
提交临界区，错误路径不会留下半恢复的寄存器或 sigmask。所有地址计算都使用
`checked_add()`；退出 helper 会先释放当前函数持有的额外 task `Arc`，再进入不展开
syscall 栈的 noreturn 退出路径。

trap 返回汇编在 LSX 已启用时只恢复完整向量快照，不会随后再用 `FLD.D` 覆盖其低 lane；
未启用 LSX 时才走纯标量 FPR 恢复路径。B47 后，投递和恢复两侧都不再跨 faultable
uaccess 持有 `task.inner`；`SA_SIGINFO`、altstack、`SA_NODEFER`、`SA_RESETHAND` 和各类
错误分支仍需分别区分源码审查与动态覆盖，不能仅凭普通 SIGCHLD 往返声称全部运行过。

## 13. stopped/continued 与 wait

进程级停止和继续状态保存在 PCB：

| 方法 | wait 可见状态 |
|------|---------------|
| `mark_stopped(signum)` | `((signum << 8) | 0x7f)` |
| `mark_continued()` | `0xffff` |

PCB 保留 stopped/continued 状态编码；wait option 解析接受 `WSTOPPED/WCONTINUED/WNOWAIT`，但完整 stopped/continued 子进程状态上报以 `exit-wait.md` 中的限制说明为准。

## 14. 调试核对点

| 现象 | 检查 |
|------|------|
| kill 返回 EPERM | uid/euid/suid 匹配和 euid 0 |
| signalfd 一直 EAGAIN | mask 是否匹配 pending，信号是否被其他路径取走 |
| pidfd 指向已回收进程 | pid handle 是否释放，proc inode weak ref 是否失效 |
| sigreturn 后寄存器异常 | signal frame 地址计算和 MachineContext 拷贝 |
| waitid stopped/continued 不出现 | PCB stopped_reported/continued_pending |
