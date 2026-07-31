---
title: "ProcessControlBlock 进程级资源"
category: process
status: stable
author: MangoCore Team
last_update: 2026-07-31
tags: [process, pcb, fd, namespace, lifecycle]
---

# ProcessControlBlock 进程级资源

## 1. 源码位置

`ProcessControlBlock` 定义在 `os/src/task/process.rs`。它保存进程级资源，对应用户可见的 PID/TGID。一个进程可以包含多个 `TaskControlBlock` 线程。

```
ProcessControlBlock
  ├── pid / leader_tid / pid handle / quota
  ├── thread_group / group_exit_code / live_threads / trap_context_cache
  ├── child_exit_wait / vfork completion
  ├── parent-child relation hints
  ├── inner: Mutex<ProcessInner>
  └── signal: Mutex<ProcessSignalState>
```

TCB 是调度实体，PCB 是资源容器和进程生命周期实体。

## 2. 外层字段

`ProcessControlBlock` 定义在 `os/src/task/process.rs:30`：

```rust
pub struct ProcessControlBlock {
    pub pid: usize,
    pub leader_tid: usize,
    _pid_handle: Arc<TidHandle>,
    process_quota: Mutex<Option<TaskQuotaGuard>>,
    thread_group: Mutex<ThreadGroupState>,
    group_exit_code: AtomicU64,
    live_threads: AtomicUsize,
    trap_context_cache: Mutex<Vec<usize>>,
    pub child_exit_wait: Mutex<WaitQueue>,
    vfork_parent: Mutex<Option<Weak<TaskControlBlock>>>,
    vfork_done: Completion,
    pub adopted_by_init: AtomicBool,
    pgid_hint/sid_hint/parent_pid_hint/user_token_hint,
    inner: Mutex<ProcessInner>,
    signal: Mutex<ProcessSignalState>,
    shared_pending_hint: AtomicU64,
}
```

| 字段 | 说明 |
|------|------|
| `pid` | 用户可见进程 ID |
| `leader_tid` | 线程组主线程 tid |
| `_pid_handle` | 保持 pid/tgid 到 wait 回收前不复用 |
| `process_quota` | 进程级 clone quota guard |
| `thread_group` | 成员弱引用、首次发布、group exit 与临时 exec 会话的共同锁域 |
| `group_exit_code` | 0 表示正常；其余值编码为统一退出码 `+1`，供安全点无锁读取 |
| `live_threads` | 当前计入 live 的线程数 |
| `exec_owner_tid` | active exec owner 的无锁快照；`usize::MAX` 表示无临时会话 |
| `trap_context_cache` | 可复用 trap context slot，限制 256 |
| `child_exit_wait` | 父进程 wait4/waitid 等待队列 |
| `vfork_parent` | `CLONE_VFORK` 父线程弱引用 |
| `vfork_done` | vfork 完成通知 |
| `adopted_by_init` | 是否为 init 收养的孤儿 |
| `pgid/sid/parent/user_token_hint` | syscall 热路径 hint |
| `inner` | 进程资源和生命周期状态 |
| `signal` | 进程级 shared pending |
| `shared_pending_hint` | shared pending 快速位图 |

## 3. ProcessInner

`ProcessInner` 定义在 `process.rs:65`，是 PCB 的主要资源集合：

| 字段 | 语义 |
|------|------|
| `exe` | 当前可执行文件 |
| `exec_key` | 可执行 inode busy key，用于 `ETXTBSY` |
| `exe_path` | `/proc/self/exe` 路径 |
| `files` | fd table |
| `fs` | cwd/root/umask |
| `uts` | hostname/domainname |
| `net` | 网络命名空间 |
| `mnt` | 挂载命名空间 |
| `ipc` | IPC 命名空间 |
| `vm` | 地址空间 |
| `sighand` | 信号处理表 |
| `futex` | private futex table |
| `user_res_slot_allocator` | 同一地址空间内用户资源槽位分配器 |
| `pgid/sid/parent/children` | 进程树和会话/进程组 |
| `has_execed` | 是否成功 exec |
| `child_subreaper` | `PR_SET_CHILD_SUBREAPER` |
| `state` | `ProcessState` |
| `exit_code` | wait 可见退出码 |
| `stopped/continued` | wait stopped/continued 状态 |
| `ptrace_tracer_pid` | 最小 ptrace attach 状态 |
| `rusage/child_rusage` | 进程与已回收子进程 CPU 时间 |
| `sched_*` | leader 调度兼容快照 |

### 3.1 ProcessInner 资源共享关系

| 资源 | clone 影响 | exec 影响 | exit/wait 影响 |
|------|------------|-----------|----------------|
| `files` | `CLONE_FILES` 共享，否则 clone fd table | 共享时先复制，再关闭当前 PCB 副本的 CLOEXEC fd | 进程退出关闭 fd |
| `fs` | `CLONE_FS` 共享，否则复制 cwd/root/umask | 保留 | 退出时随 PCB 释放 |
| `vm` | `CLONE_VM` 共享，否则由 `AddressSpaceInner::from_existing_user()` 构造新 `AddressSpace` | `replace_vm(new)` | zombie 时可 `write(|vm| vm.release_for_zombie())` |
| `sighand` | `CLONE_SIGHAND` 共享，否则复制 | 共享时先复制；清用户 handler，保留 `SIG_IGN` | PCB drop 释放 |
| `futex` | 共享 VM 时共享，否则新建 private table | 换成新 private table，不清空可能被其它 PCB 使用的旧表 | 退出时处理 robust/clear child tid |
| `children/parent` | 非 `CLONE_THREAD` child 发布到父进程 | 保留 | wait/auto-reap 消费 |
| `ipc/net/mnt/uts` | namespace flag 决定共享或新建 | 保留 | PCB 释放时 drop 引用 |

这张表是读 clone/exec/exit 的主线：clone 决定资源从哪里来，exec 决定哪些资源被替换或重置，exit/wait 决定资源何时释放和用户何时可观察退出状态。

`reset_exec_resources()` 只能在线程组通过 `ExecSession` 收缩到一个 live thread 后调用。
它先在 `process.inner` 内判断 fd table/sighand 是否仍跨 PCB 共享并取得快照，再在锁外完成
可能分配内存的复制与 CLOEXEC/signal 重置；提交新对象时只短持 `process.inner`。旧 futex
也在锁外析构，避免 WaitQueue 的任务引用析构链回入进程锁。

## 4. ProcessState

```rust
pub enum ProcessState {
    Running,
    Stopped,
    Zombie,
}
```

| 状态 | 含义 |
|------|------|
| `Running` | 进程仍有活动线程或可运行状态 |
| `Stopped` | signal/ptrace 相关停止，可由 wait 观察 |
| `Zombie` | 进程级退出已完成，等待父进程 wait 或 auto-reap |

`TaskStatus::Zombie` 表示线程退出；`ProcessState::Zombie` 表示进程 live thread 已清零并完成进程级收尾。

### 4.1 ProcessState 状态图

```
Running
  | signal/ptrace stop
  v
Stopped
  | SIGCONT / wait continued 消费
  v
Running
  |
  | last live thread exit
  v
Zombie
  |
  | wait/auto-reap
  v
pid/quota/registry released
```

`Stopped` 和 `continued_pending` 是 wait 可观察状态；`Zombie` 是 exit 可观察状态。`waitid(WNOWAIT)` 可以读取状态但不消费，普通 wait 会消费并更新父进程 child rusage。

## 5. exec inode busy key

PCB 使用两个全局表：

```rust
EXEC_INODE_REFS: BTreeMap<InodeBusyKey, usize>
WRITE_INODE_REFS: BTreeMap<InodeBusyKey, usize>
```

| 表 | 用途 |
|----|------|
| `EXEC_INODE_REFS` | 当前被 exec 的 inode 引用计数 |
| `WRITE_INODE_REFS` | 当前以可写方式打开的 inode 引用计数 |

`check_exec_metadata()` 在 exec 前检查 `is_writable_inode_busy()`，忙时返回 `ETXTBSY`。`replace_exe()` 会更新 exec key 引用计数。

`InodeBusyKey` 使用 `(inode.fs().identity_key(), inode_id)`。这里不能使用
`Metadata.dev_id` 的占位值：不同文件系统可以同时存在相同 inode 号，例如 initramfs
中的正在执行文件与 ext4 上新建文件都可能是 inode 16；若 key 只由 `(0, 16)` 组成，
后者会被误判为正在执行并在普通可写 reopen 时返回 `ETXTBSY`。`MountFS` 对
`identity_key()` 的转发保证 bind mount 不会绕过同一文件的 busy 状态。

## 6. 资源共享与 unshare

PCB 提供资源 getter 和 unshare/setter：

| 资源 | getter | unshare/setter |
|------|--------|----------------|
| files | `files()` | `unshare_files()` |
| fs | `fs()` | `unshare_fs()` |
| uts | `uts()` | `unshare_uts()` |
| net | `net()` | `unshare_net()`, `set_net()` |
| mnt | `mnt()` | `set_mnt()` |
| ipc | `ipc()` | `set_ipc()` |
| vm | `vm()` | `replace_vm()` |
| sighand | `sighand()` | clone 时复制或共享 |
| futex | `futex()` | exec 清空，fork 按 VM 共享情况决定 |

`replace_vm()` 会清空 trap context cache，更新 `user_token_hint`，并刷新当前运行进程的 token hint。

## 7. 线程表和 live count

新线程不再在 TCB 构造期间单独调用 `add_thread()`。`publish_thread()` 在
`thread_group` 锁内完成：

```text
检查 group_exit_code == 0
  -> members 加入 Weak<TCB>
  -> 设置 thread_live_counted
  -> live_threads + 1
  -> 单个 runqueue 提交 New -> Queued(cpu)
```

这把锁只包住首次发布的短临界区；远端内核栈同步发生在取锁前，IPI doorbell
发生在解锁后。group exit 使用同一锁先发布非零退出码、再形成 live 成员快照；
多线程 exec 使用同一锁安装 `ExecSession`、保存 owner 和 Completion。两者都与
late clone 线性化，且 group exit 可以覆盖临时 exec。

`remove_thread()` 在 `exit_thread_resources()` 的最后消费 live token，并在列表过于
稀疏时 compact：

```text
members.len() > live * 4 + 128
  -> retain live weak refs
```

`threads()` 返回当前 live 的强引用列表，并顺便清理失效 weak。`any_live_thread()` 用于信号、wait 唤醒和进程状态判断。

线程表使用 weak 引用是为了避免 PCB 和 TCB 互相强持有造成无法释放：TCB 外层持有 `Arc<ProcessControlBlock>`，如果 PCB 再强持有所有 TCB，线程退出后就无法自然 drop。需要遍历 live 线程时，代码临时升级 weak；升级失败说明线程对象已经释放，可以顺手清理。

`live_threads` 和成员弱引用不是同一件事。前者是退出 ack 计数；最后一个
AcqRel `fetch_sub` 能观察此前 sibling 已完成的用户内存/TLB 清理，并独占
`finish_exit()`。后者是可遍历集合，供 signal、exec、procfs 和 wait 查找任务对象。
调试“进程为什么没有 zombie”时看 live count；调试“信号为什么找不到线程”时看
members weak 是否仍可升级。

active exec 期间，`remove_thread()` 看到 live count 降为 1 时会克隆
`ExecState.siblings_done`，释放线程组锁后才 `complete()`。因此 owner 的确认条件是
权威计数，而不是开始时快照为空；Completion 唤醒也不会反向嵌套 WaitQueue/runqueue
与线程组锁。

## 8. trap context cache

线程退出时，`exit_thread_resources()` 会尝试：

```rust
process.try_cache_trap_context_slot(user_res_slot)
```

缓存条件：

| 条件 | 结果 |
|------|------|
| 进程正在 group exit | 不缓存 |
| 当前存在 exec 会话 | 不缓存 |
| live thread count <= 1 | 不缓存 |
| cache 长度 >= 256 | 不缓存 |
| slot 已在 cache 中 | 不缓存 |
| 其他 | 保存 slot |

共享 VM 的新线程创建时可 `take_cached_trap_context_slot()`，减少重新分配 trap context 页的成本。

## 9. 进程组和会话

PCB 保存 `pgid` 和 `sid`，同时维护原子 hint：

| 方法 | 行为 |
|------|------|
| `setpgid(pgid)` | 更新 inner 和 `pgid_hint` |
| `setsid(sid)` | 设置 sid，同时 pgid=sid |
| `getpgid()` | 读取 hint |
| `getsid()` | 读取 hint |

如果当前进程正在运行，`refresh_current_process_group_hints()` 会同步更新 processor 热路径缓存。

## 10. 进程停止和继续状态

停止和继续事件用于 wait：

| 方法 | 行为 |
|------|------|
| `mark_stopped(signum)` | 进入 Stopped，记录 stop signal，唤醒 parent/tracer |
| `mark_continued()` | 从 Stopped 回 Running，设置 continued_pending |
| `take_stopped_status(nowait)` | 返回 `((signum << 8) | 0x7f)` |
| `take_continued_status(nowait)` | 返回 `0xffff` |

`WNOWAIT` 为 true 时，状态不会被消费。

## 11. 进程级 shared pending

`ProcessSignalState` 只保存进程共享 pending：

| 字段 | 说明 |
|------|------|
| `shared_pending` | `kill(pid)` / `killpg()` 等进程级投递队列 |

线程级 pending 在 TCB inner 中，进程级 pending 在 PCB signal state 中。取信号时先看线程 pending，再看 shared pending。

group-exit 不再混入 signal 锁。`group_exit_code: AtomicU64` 是安全点热路径的权威
快照，`thread_group` 锁只负责把第一次非零发布与成员/runqueue 发布排序。编码采用
`0 = not exiting`、`stored = u32 exit_code + 1`，既保留全部退出码，又避免额外
`group_exiting` 布尔第二真值。

## 12. vfork completion

PCB 保存：

| 字段 | 语义 |
|------|------|
| `vfork_parent` | 正在等待的父线程 |
| `vfork_done` | `Completion` |

`complete_vfork()` 会清空 parent 并完成 completion。exec 成功和 exit 都会调用它；
父线程通过 `wait_vfork_done_killable()` 等待。普通信号不打断等待，线程组生命周期
停止请求会返回 `Interrupted`，由 clone 调用层释放已发布 child 的本地引用后进入安全点。

## 13. 子进程收养

进程退出时，PCB 会把 children 转交给最近的 child reaper：

1. `nearest_child_reaper(parent)` 向上找未 zombie 且 `child_subreaper` 的祖先。
2. 找不到则使用 `INITPROC.process`。
3. zombie child 若转给 init，会累加 rusage、释放 pid、注销 process 并移除 zombie TCB。
4. live child 设置新 parent，并按 reaper 存入 children。

`PR_SET_CHILD_SUBREAPER` 不被 fork/clone 继承，但跨 exec 保留。

## 14. finish_exit()

进程级退出由 `ProcessControlBlock::finish_exit(exit_task, exit_code)` 完成：

```text
complete_vfork()
收集 exit_task rusage + resident maxrss
mark_zombie(exit_code, rusage)
解除 exec busy key
收养 children
根据 parent SIGCHLD action 判断 auto-reap
唤醒 parent child_exit_wait
按 exit_signal 向 parent live thread 投递信号
若 VM 未共享，release_for_zombie()
close_files_on_exit()
```

`close_files_on_exit()` 遍历 fd table，逐个 drop fd，并释放 flock。

## 15. PID 生命周期

PID 不在进程进入 zombie 时立即复用。PCB 持有 `_pid_handle`，直到：

| 路径 | 释放 |
|------|------|
| 父进程 wait 回收 | `release_pid()` |
| auto-reap | `release_pid()` |
| init 收养 zombie orphan | `release_pid()` |
| PCB drop | 注册表兜底注销 |

这保证 zombie 仍可由父进程 wait 观察。

## 16. 调试核对点

| 现象 | 检查 |
|------|------|
| exec 文件被写打开仍成功 exec | `WRITE_INODE_REFS` 和 `is_writable_inode_busy()` |
| wait 不唤醒父进程 | `child_exit_wait.wake_all()`、auto-reap、parent 指针 |
| vfork 父进程永久阻塞 | `complete_vfork()` 是否在 exec/exit 调用 |
| PID 过早复用 | `_pid_handle` 是否在 wait 前释放 |
| 多线程 exec 后旧 VM 残留 | ExecSession、live count ack、外部共享 VM 槽撤销 |
