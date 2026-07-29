---
title: "exit、exit_group、wait4 与 waitid"
category: process
status: stable
author: MangoCore Team
last_update: 2026-07-29
tags: [process, exit, wait, zombie]
---

# exit、exit_group、wait4 与 waitid

## 1. 源码位置

| 文件 | 内容 |
|------|------|
| `os/src/syscall/process/lifecycle.rs` | `sys_exit`, `sys_exit_group`, `sys_wait4`, `sys_waitid`, robust list syscall |
| `os/src/task/mod.rs` | `exit_current_and_run_next`, `exit_group_and_run_next`, `do_exit` |
| `os/src/task/task.rs` | `exit_thread_resources()` |
| `os/src/task/process.rs` | `finish_exit()`、zombie、children、vfork、auto-reap |
| `os/src/task/process_manager.rs` | wait child 查找与回收 |

## 2. exit code 编码

syscall 层编码：

```rust
sys_exit(code)       -> exit_current_and_run_next((code & 0xff) << 8)
sys_exit_group(code) -> exit_group_and_run_next((code & 0xff) << 8)
```

wait 可见的正常退出状态为低 8 位退出码左移 8 位。

## 3. 线程级退出

`do_exit(task, exit_code)` 先调用：

```rust
task.exit_thread_resources(exit_code)
```

线程级资源清理包括：

1. TCB 状态置 `Zombie`。
2. 结算系统时间。
3. 清理 `clear_child_tid` 和 robust list。
4. 从进程 live thread count 移除。
5. 清零用户 `clear_child_tid` 并唤醒 futex。
6. 释放或缓存 trap context / 默认用户栈映射。

如果进程还有其他 live thread，进程不会进入 `ProcessState::Zombie`。

## 4. 进程级退出

当 `live_thread_count() == 0`：

```rust
release_fcntl_locks_for_pid(pid)
shm_detach_process(pid)
process.finish_exit(task, exit_code)
```

`finish_exit()` 负责：

1. 完成 vfork。
2. 把 resident user bytes 更新到 rusage maxrss。
3. `mark_zombie(exit_code, rusage)`。
4. 解除 exec inode busy key。
5. 收养 children。
6. 判断 auto-reap。
7. 唤醒 parent 的 `child_exit_wait`。
8. 按 child `exit_signal` 向 parent live thread 投递信号。
9. 若 VM 未共享，`release_for_zombie()`。
10. 关闭所有 fd。

线程级退出和进程级退出分开，是为了支持线程组语义。普通线程退出只清理自己的内核栈、trap context、clear_child_tid、robust list 和 live count；只要同一进程还有其他 live thread，PCB 的 fd table、VM、children 和 exit code 都不能进入 zombie。最后一个线程退出时才执行 `finish_exit()`，父进程才能通过 wait 观察到进程退出。

`finish_exit()` 不直接 drop PCB，而是把它变成 wait 可见的 zombie。这样父进程可以读取 exit status 和 rusage；PCB 中也保留 stopped/continued 相关字段，但完整状态上报仍受 wait option 支持范围限制。若父进程忽略 SIGCHLD 或目标是被 init 收养的 auto-reap 子进程，才走自动回收分支。

线程级退出和最后线程判断在 `task/mod.rs::do_exit()` 中完成：

```rust
fn do_exit(task: &TaskControlBlock, exit_code: u32) {
    if task.exit_thread_resources(exit_code) {
        if task.process.live_thread_count() == 0 {
            crate::syscall::fs::release_fcntl_locks_for_pid(task.pid());
            crate::syscall::shm_detach_process(task.pid());
            task.process.finish_exit(task, exit_code);
        }
    }
}

pub fn exit_current_and_run_next(exit_code: u32) -> ! {
    let task = current_task().unwrap();
    do_exit(&task, exit_code);
    drop(task);
    let mut _unused = TaskContext::zero_init();
    schedule(&mut _unused as *mut _);
    panic!("Unreachable");
}
```

`current_task()` 从本 CPU current 槽克隆一个本地 `Arc`。退出路径在完成
`do_exit()` 后、进入不返回的 `schedule()` 前显式 drop 这个 clone；current 槽
仍保留 owner。任务切回 idle 后，idle 才从槽位取出 retained Arc 并转入 zombie queue。

`ProcessControlBlock::finish_exit()` 是最后线程退出后的进程级提交点：

```rust
pub fn finish_exit(&self, exit_task: &TaskControlBlock, exit_code: u32) {
    self.complete_vfork();
    let mut rusage = exit_task.acquire_inner_lock().rusage;
    let resident_kb = self.vm().read(|vm| vm.resident_user_bytes()) / 1024;
    rusage.update_maxrss_kb(resident_kb);
    if !self.mark_zombie(exit_code, rusage) {
        return;
    }
    let parent_process = self.parent();
    let auto_reap = parent_process
        .as_ref()
        .map(|parent| {
            let sighand_ref = parent.sighand();
            let sighand = sighand_ref.lock();
            sigchld_requests_auto_reap(&sighand)
        })
        .unwrap_or(false);
    let old_exec_key = self.inner.lock().exec_key.take();
    if let Some(key) = old_exec_key {
        unregister_exec_key(key);
    }

    let children = self.take_children();
    let child_reaper = Self::nearest_child_reaper(parent_process.clone());
    let adopted_children = if children.is_empty() {
        false
    } else {
        Self::adopt_children_by_reaper(children, child_reaper.clone())
    };

    if let Some(parent_process) = parent_process {
        let auto_reap = self.adopted_by_init.load(Ordering::Relaxed)
            || auto_reap
            || sigchld_requests_auto_reap(&parent_process.sighand().lock());
        if auto_reap {
            parent_process.detach_child(self.pid);
            self.set_parent(None);
            self.release_pid();
            registry::unregister_process(self.pid);
            self.release_process_quota_once();
            crate::task::remove_zombie_tasks_by_pid(self.pid);
            parent_process.child_exit_wait.lock().wake_all();
        } else {
            parent_process.child_exit_wait.lock().wake_all();
            if !exit_task.exit_signal.is_empty() {
                if let Some(parent_task) = parent_process.any_live_thread() {
                    let mut parent_inner = parent_task.acquire_inner_lock();
                    parent_inner.add_signal(exit_task.exit_signal);
                    drop(parent_inner);
                    let _ = wake_interruptible(parent_task);
                }
            }
        }
    } else {
        warn!("[finish_process_exit] parent is None");
    }

    if adopted_children {
        Self::wake_child_waiters(&child_reaper);
    }

    let vm = self.vm();
    if Arc::strong_count(&vm) <= 2 {
        vm.update(|address_space| address_space.release_for_zombie());
    }
    self.close_files_on_exit();
}
```

这段代码的顺序决定 wait 可见性：先 `mark_zombie()` 保存 exit status/rusage，再处理 parent wait queue 和 SIGCHLD，最后释放 VM 数据页并关闭 fd。auto-reap 分支会立即 detach child、释放 pid、注销 process 并移除 zombie TCB。

## 5. exit_current_and_run_next

当前任务仍运行在自己的内核栈上，不能立即 drop 最后一个 TCB 引用。退出函数：

```text
current_task()（Processor.current 保持 owner）
do_exit() -> TaskStatus::Zombie
drop 本地 current Arc
schedule(idle)
idle: clear current -> finish_switch_out() -> zombie queue
```

调度循环之后从 zombie queue 取出并 drop TCB。

## 6. exit_group_and_run_next

`exit_group()`：

1. 取出当前 task。
2. `process.request_group_exit(exit_code)`。
3. 收集同进程其他线程。
4. 从调度队列移除其他线程。
5. 对其他线程调用 `exit_thread_resources(exit_code)`。
6. 当前线程走 `do_exit()`。
7. 当前 task 放入 zombie queue 并切回 idle。

`ProcessSignalState` 中的 `group_exiting/group_exit_code` 让信号路径和线程路径能看到线程组退出状态。

## 7. 子进程收养与 auto-reap

进程退出时 children 交给最近 child reaper：

| 情况 | reaper |
|------|--------|
| 祖先中存在非 zombie 且 `child_subreaper` | 该祖先 |
| 否则 | initproc |

auto-reap 条件包括：

1. 当前进程是被 init 收养的孤儿。
2. parent 的 SIGCHLD action 要求 auto reap。
3. 再次读取 parent sighand 后仍请求 auto reap。

auto-reap 会 detach child、释放 pid、注销 process、释放 process quota、移除 zombie TCB。

## 8. wait4 参数

`sys_wait4(pid, status, option, ru)`：

| pid | 语义 |
|-----|------|
| `pid > 0` | 等待指定 pid child |
| `pid == -1` | 等待任意 child |
| `pid == 0` | 等待同进程组 child |
| `pid < -1` | 等待 pgid 为 `abs(pid)` 的 child |
| `pid == i32::MIN` | `ESRCH` |

支持选项：

| option | 语义 |
|--------|------|
| `WNOHANG` | 无可回收 child 时立即返回 0 |
| `WSTOPPED` | 可观察 stopped child |
| `WEXITED` | waitid 使用，wait4 路径传 true |
| `WCONTINUED` | 可观察 continued child |
| `WNOWAIT` | 观察但不消费 |
| `WNOTHREAD/WALL/WCLONE` | flag 被接受，具体 wait 筛选按现有实现 |

`WSTOPPED`、`WCONTINUED` 和相关 stopped/continued flag 会被解析接受，以兼容 shell 和测试程序传入的 wait options；完整的 stopped/continued 子进程状态上报尚未接入，可消费的主路径仍是 exited/zombie child。

未知 option bit 返回 `EINVAL`。

## 9. wait4 返回

`ProcessManager::wait_child()` 返回：

| 结果 | sys_wait4 行为 |
|------|----------------|
| `Ok(Some(child))` | 写 status，返回 child pid |
| `Ok(None)` | 返回 `SUCCESS`，即 0 |
| `Err(errno)` | 返回 errno |

status 指针非 null 时使用 `UserPtrMut` 写入；写失败返回该 errno。

## 10. waitid

`sys_waitid(idtype, id, infop, options, ru)` 支持：

| idtype | 语义 |
|--------|------|
| `P_ALL = 0` | 任意 child |
| `P_PID = 1` | 指定 pid |
| `P_PGID = 2` | 指定 pgid；id 为 0 表示当前 pgid |
| `P_PIDFD = 3` | id 是 pidfd |

options 必须包含 `WEXITED/WSTOPPED/WCONTINUED` 中至少一个，否则 `EINVAL`。

## 11. P_PIDFD waitid

P_PIDFD 分支：

1. 从 fd table 取文件。
2. `pidfd_file_target_pid()` 解析 pidfd 或 proc pid ns dir。
3. 若 pidfd file 是 nonblock，且目标进程存在但不是 zombie，返回 `EAGAIN`。
4. 调用 `wait_child()`，`nohang = nonblock || WNOHANG`。
5. 成功时向 `infop` 写 `SigInfo`。
6. 无 child 且 nonblock 返回 `EAGAIN`；否则可写空 `SigInfo`。

`sys_wait4()` 是 `ProcessManager::wait_child()` 的直接包装：

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

syscall 层只负责参数位解析和用户 status 写回；是否存在可观察 child、是否消费 zombie、是否聚合 rusage 都由 `ProcessManager::wait_child()` 决定。

## 12. waitid siginfo 编码

`waitid_siginfo(pid, wait_status)`：

| wait_status | code |
|-------------|------|
| `0xffff` | `CLD_CONTINUED` |
| 低 8 位为 `0x7f` | `CLD_STOPPED` |
| 低 7 位有终止信号 | `CLD_KILLED` 或 `CLD_DUMPED` |
| 正常退出 | `CLD_EXITED` |

信号固定为 `SIGCHLD`，sender pid 为 child pid。

## 13. robust list syscall

`sys_set_tid_address(tidptr)` 设置当前线程 `clear_child_tid` 并返回 tid。

`sys_set_robust_list(head, len)` 要求 `len == RobustList::HEAD_SIZE`，否则 `EINVAL`。

`sys_get_robust_list(pid, head_ptr, len_ptr)`：

| pid | 行为 |
|-----|------|
| 0 | 返回当前线程 robust list |
| 非 0 | 查找目标 task |

跨 task 查询需要同 uid/euid/gid/egid，或 euid 为 0，或具备 `CAP_SYS_PTRACE`。不满足返回 `EPERM`。

## 14. 调试核对点

| 现象 | 检查 |
|------|------|
| 线程退出导致整个进程 zombie | live thread count 是否正确 |
| wait4 永久阻塞 | child 是否进入 ProcessState::Zombie，parent wait queue 是否唤醒 |
| pidfd waitid nonblock 不返回 EAGAIN | pidfd file `O_NONBLOCK` |
| zombie 资源未释放 | 父进程是否 wait，auto-reap 条件是否满足 |
| vfork 父进程未唤醒 | `finish_exit()` 开头 `complete_vfork()` |
