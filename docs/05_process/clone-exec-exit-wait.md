---
title: "clone、exec、exit、wait 生命周期总路径"
category: process
status: stable
author: MangoCore Team
last_update: 2026-07-27
tags: [process, lifecycle, clone, exec, wait]
---

# clone、exec、exit、wait 生命周期总路径

## 1. 源码位置

| 源码 | 作用 |
|------|------|
| `os/src/syscall/process/clone.rs` | clone/clone3/unshare/setns syscall 层 |
| `os/src/syscall/process/exec.rs` | execve/execveat、shebang、argv/envp |
| `os/src/syscall/process/lifecycle.rs` | exit/exit_group/wait4/waitid/robust list |
| `os/src/task/task.rs` | `TaskControlBlock::sys_clone()`、`load_elf()`、线程退出 |
| `os/src/task/process.rs` | PCB 资源、children、`finish_exit()` |
| `os/src/task/process_manager.rs` | pid registry、process lookup、wait 选择 |
| `os/src/task/manager.rs` | ready/zombie queue、调度发布和回收 |

## 2. 总览

进程生命周期由 syscall 层、TCB、PCB、调度器和 MM 共同完成：

```
clone/fork
  -> 创建 TCB/PCB/VM
  -> 发布 child
  -> 调度 child

exec
  -> 校验文件/argv/envp
  -> 构造新 AddressSpace
  -> 替换当前进程 VM
  -> 清理同线程组其他线程和 CLOEXEC/futex/sighand

exit
  -> 线程级资源退出
  -> 最后线程触发进程级 finish_exit
  -> 进入 zombie

wait
  -> 父进程观察/回收 child zombie
  -> 释放 pid/quota/process registry
```

## 3. clone 的跨层路径

```
sys_clone/sys_clone3
  └── sys_clone_inner()
        ├── flag/权限/内存余量校验
        ├── parent.sys_clone()
        │     ├── VM share/copy
        │     ├── PCB share/create
        │     ├── TCB create
        │     └── registry register
        ├── 写 parent_tid/child_tid/pidfd
        ├── publish_clone_child()
        └── schedule_clone_child()
```

关键分界：

| 阶段 | 可回滚 |
|------|--------|
| child 构造完成但未发布 | 可以 cleanup_unpublished_clone |
| parent_tid/child_tid/pidfd 写入 | 失败时回滚 |
| publish 到 children 后 | child 成为 waitable |
| schedule 后 | child 可运行 |

clone 发布边界由 `sys_clone_inner()` 最后的三步体现：

```rust
if let Err(errno) = ProcessManager::publish_clone_child(&parent, child.clone(), flags) {
    if let Some(pidfd) = allocated_pidfd {
        drop_parent_fd(&parent, pidfd);
    }
    child.cleanup_unpublished_clone(flags.contains(CloneFlags::CLONE_VM));
    return errno;
}
ProcessManager::schedule_clone_child(&parent, child, flags);
new_tid as isize
```

在 `publish_clone_child()` 成功前，失败路径仍可清理 pidfd 和共享 VM 中的 user resource；成功后 child 已经进入父进程 children 或作为线程共享同一 PCB。

## 4. fork 与线程 clone 差异

| 项 | fork/非 `CLONE_THREAD` | `CLONE_THREAD` |
|----|------------------------|----------------|
| PID | 新 pid | 共享进程 pid，tid 不同 |
| PCB | 新 PCB | 共享 PCB |
| children | 作为 waitable child 发布 | 不发布为 child |
| quota | PCB 持有 process quota | TCB 持有 thread quota |
| wait4 | 可由 parent wait | 不作为独立进程 wait |
| exit_signal | 可投递给 parent | 线程退出不作为进程 child exit |

`CLONE_VM` 可以用于非线程 clone，例如 vfork；它决定 VM 是否共享，但不等同于 `CLONE_THREAD`。

## 5. exec 的跨层路径

```
sys_execve()
  ├── UserCString 读取 pathname
  ├── read_exec_vectors()
  ├── open_exec()
  └── exec_opened_file()

sys_execveat()
  ├── 读取 dirfd/pathname/flags
  ├── read_exec_vectors()
  ├── open_exec_with_follow() 或 reopen_exec_fd()
  └── exec_opened_file()
        ├── ELF/shebang/shell fallback
        ├── validate_exec_stack_usage()
        ├── task.load_elf()
        └── mark_execed/set_exe_path/complete_vfork
```

`load_elf()` 内部：

1. 构造新 `AddressSpace`。
2. 准备用户 heap、栈、auxv。
3. 设置新 trap context。
4. 杀掉同线程组其他线程。
5. 如果旧 VM 共享，清理旧 VM 中当前线程资源。
6. 替换 exe、关闭 CLOEXEC fd、替换 VM、重置信号处理、清 futex。

exec 的提交点集中在 `TaskControlBlock::load_elf()` 末尾：

```rust
self.process.replace_exe(elf);
{
    let files_ref = self.process.files();
    let mut fd_table = files_ref.lock();
    crate::syscall::fs::close_cloexec_and_release_fcntl_locks(self.pid(), &mut fd_table);
}
self.process.replace_vm(memory_set);
self.process.sighand().lock().reset();
self.process.futex().lock().clear();
Ok(())
```

这些操作发生前，ELF 映射、用户栈、auxv 和 trap context 都已经在临时 `memory_set` 中构造完成。

## 6. exec 与 vfork

`CLONE_VFORK` child exec 成功后：

```rust
task.process.complete_vfork();
```

如果 child exit，则 `finish_exit()` 开头也会 complete。父线程等待的是 PCB 中的 `Completion`，不是 WaitQueue。

## 7. exit 的跨层路径

```
sys_exit(code)
  └── exit_current_and_run_next(encoded)
        ├── current_task()（Processor.current 保留 owner）
        ├── do_exit(task, encoded)
        │     ├── task.exit_thread_resources()
        │     └── if live_thread_count == 0:
        │           ├── release fcntl locks
        │           ├── shm_detach_process()
        │           └── process.finish_exit()
        ├── drop 本地 current Arc
        ├── schedule(idle)
        └── idle: finish_switch_out() -> zombie queue
```

`exit_group()` 先把同进程其他线程全部做线程级退出，再处理当前线程和进程级退出。

最后线程判断和进程级退出触发点在 `do_exit()`：

```rust
fn do_exit(task: &Arc<TaskControlBlock>, exit_code: u32) {
    if task.exit_thread_resources(exit_code) {
        if task.process.live_thread_count() == 0 {
            crate::syscall::fs::release_fcntl_locks_for_pid(task.pid());
            crate::syscall::shm_detach_process(task.pid());
            task.process.finish_exit(task.as_ref(), exit_code);
        }
    }
}
```

因此普通线程退出不会关闭 fd、释放 VM 或唤醒父进程 wait；只有 live thread count 归零时才进入 PCB 的 `finish_exit()`。

## 8. 线程级与进程级资源表

| 资源 | 线程级退出 | 进程级退出 |
|------|------------|------------|
| task status | Zombie | 不涉及 |
| clear_child_tid | 清零并 futex wake | 不涉及 |
| robust list | reset | 不涉及 |
| user stack/trap context | 释放或缓存 | VM 整体可能释放 |
| fd table | 不关闭 | close all fd |
| fcntl locks | 最后线程释放 pid locks | close fd 时释放 file locks |
| SysV shm attachment | 最后线程 detach process | 是 |
| children | 不处理 | 收养 |
| parent wait queue | 不唤醒 | 唤醒 |
| pid/quota | 不释放 | wait/auto-reap 释放 |

## 9. wait 的跨层路径

```
sys_wait4/sys_waitid
  ├── 解析 pid/idtype/options
  ├── ProcessManager::wait_child()
  │     ├── 扫描 current process children
  │     ├── 匹配 pid/pgid/pidfd
  │     ├── 检查 exited/stopped/continued
  │     ├── WNOWAIT 决定是否消费
  │     └── 无 ready child 时等待 child_exit_wait
  └── 写 status 或 siginfo
```

`child_exit_wait` 是 PCB 上的 WaitQueue。子进程 `finish_exit()` 或 stopped/continued 状态变化会唤醒它。

wait4 的 syscall 包装如下：

```rust
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
```

`WNOWAIT` 通过参数传给 `wait_child()`，因此状态消费和 pid/quota 释放不在 syscall 包装层完成。

## 10. rusage 聚合

TCB 记录当前线程 `Rusage`。进程退出时：

1. 取 exit task 的 rusage。
2. 更新 resident maxrss。
3. 保存到 PCB inner `rusage`。
4. parent wait 回收时，把 child rusage 累加到 parent `child_rusage`。

当前 `Rusage` 中主要实现 CPU 时间和 maxrss；其他字段保留为 0。

## 11. wait 状态编码

| 状态 | 编码 |
|------|------|
| 正常 exit code N | `(N & 0xff) << 8` |
| stopped | `(signum << 8) | 0x7f` |
| continued | `0xffff` |
| signal killed | 低 7 位为 signal，core dump 位按状态 |

`waitid` 会把这些编码转换为 `SigInfo` 的 `CLD_*` code。

## 12. registry 与 pid 生命周期

clone 成功会：

| 情况 | registry |
|------|----------|
| 新 PCB | `registry::register_process()` |
| 所有 TCB | `registry::register_task()` |

退出和回收：

| 阶段 | 行为 |
|------|------|
| 线程 drop | unregister task |
| auto-reap / wait | unregister process，release pid |
| PCB drop | unregister process 兜底 |

pid handle 保证 wait 前 zombie pid 不被复用。

## 13. 错误边界汇总

| 场景 | 错误 |
|------|------|
| clone 低物理内存 | `ENOMEM` |
| clone flag 依赖不满足 | `EINVAL` |
| namespace clone/unshare/setns 非 root | `EPERM` |
| exec 文件不可执行 | `EACCES` |
| exec 文件正被可写打开 | `ETXTBSY` |
| exec argv/envp 过大 | `E2BIG` |
| wait pid 无 child | 由 `ProcessManager::wait_child()` 返回相应 errno |
| pidfd waitid nonblock target running | `EAGAIN` |

这四个 syscall 阶段组成一条闭环：clone 发布 child，exec 替换 child 映像，exit 把 child 变成 wait 可见状态，wait 消费状态并释放 pid/quota。任一阶段的顺序错误都会在下一阶段暴露：child 未 publish 会导致 wait 不到；exec 失败后过早替换 VM 会破坏原进程；exit 直接释放 PID 会导致 wait 前复用；wait 忘记消费状态会导致 zombie 堆积。

读这条闭环时，先看 syscall 参数层，再看 task/process 对象层。参数层决定 errno，task/process 层决定对象生命周期。尤其是 vfork、pidfd、CLOEXEC、SIGCHLD auto-reap 都跨越多个阶段，不能只在单个 syscall 文件中判断完整语义。

## 14. 调试核对点

| 现象 | 检查 |
|------|------|
| child 创建后父 wait 不到 | 是否 publish 到 parent children |
| clone 失败泄露资源 | 失败点是否在 publish 前并 cleanup |
| exec 后父 vfork 不醒 | complete_vfork 成功路径 |
| exit 后 TCB 没 drop | zombie queue drain |
| wait 后 pid 未释放 | wait/auto-reap 是否调用 release_pid/release_process_quota_once |
