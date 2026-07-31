---
title: "任务、进程与调度器协作路径"
category: process
status: stable
author: MangoCore Team
last_update: 2026-07-28
tags: [process, task, scheduler, integration]
---

# 任务、进程与调度器协作路径

## 1. 源码位置

| 源码 | 作用 |
|------|------|
| `os/src/task/task.rs` | TCB 字段、线程状态、trap context、clone/exec/exit |
| `os/src/task/process.rs` | PCB 字段、进程资源、children、finish_exit |
| `os/src/task/run_queue.rs` | Per-CPU runnable 队列和 owner 状态交接 |
| `os/src/task/manager.rs` | interruptible/zombie/timer registry、WaitQueue |
| `os/src/task/processor.rs` | `run_tasks()`、Per-CPU current/idle 状态 |
| `os/src/task/process_manager.rs` | process registry、pid lookup、wait helper |
| `os/src/hal/arch/*/switch.*` | `__switch` 上下文切换 |

## 2. 三个核心对象

```
ProcessControlBlock
  ├── 进程资源：VM / fd table / fs / namespace / sighand / futex
  ├── 进程关系：parent / children / pgid / sid
  └── 生命周期：Running / Stopped / Zombie

TaskControlBlock
  ├── 调度资源：kstack / task_cx / trap_cx / TaskStatus
  ├── 线程状态：sigmask / pending / rusage / timers / rlimit
  └── 指向所属 ProcessControlBlock

Per-CPU RunQueue + Processor
  ├── Queued(cpu) runnable / current Arc / idle context
  └── __switch idle <-> task

Global TaskManager
  └── interruptible / zombie / timer registry
```

这三个对象的职责必须分开理解：PCB 不直接被调度，TCB 不直接持有 fd table，TaskManager 不拥有进程资源。

三者在源码中的入口分别是 `ProcessControlBlock`、`TaskControlBlock` 和调度器状态。`TaskControlBlock` 外层字段直接表明“线程是调度实体，进程是资源容器”：

```rust
pub struct TaskControlBlock {
    pub tid: TidHandle,
    pub user_res_slot: usize,
    pub process: Arc<ProcessControlBlock>,
    pub kstack: KernelStack,
    pub ustack_base: usize,
    pub user_stack_allocated: AtomicBool,
    pub thread_live_counted: AtomicBool,
    pub seccomp_counted: AtomicBool,
    pub uid_hint: AtomicUsize,
    pub euid_hint: AtomicUsize,
    pub suid_hint: AtomicUsize,
    pub gid_hint: AtomicUsize,
    pub egid_hint: AtomicUsize,
    pub sgid_hint: AtomicUsize,
    pub exit_signal: Signals,
    inner: Mutex<TaskControlBlockInner>,
}
```

调度层把 runnable owner 与全局等待 registry 分开：

```rust
pub struct TaskManager {
    interruptible_queue: VecDeque<Arc<TaskControlBlock>>,
    zombie_queue: VecDeque<Arc<TaskControlBlock>>,
}

pub struct CpuTaskState {
    processor: Mutex<Processor>,
    run_queue: Mutex<RunQueue>,
    nr_running: AtomicUsize,
}
```

当前所有生产任务仍发布到 CPU0；该结构尚不等于 AP 调度和任务迁移已经完成。

## 3. 创建路径中的协作

clone 创建 child 时：

1. syscall 层校验 flag 和用户指针。
2. TCB 层决定复制或共享 VM。
3. PCB 层决定复制或共享进程资源。
4. registry 注册 task/process。
5. syscall 层写 parent_tid/child_tid/pidfd。
6. PCB 层把 child 发布为 waitable child。
7. `publish_task()` 把 `New` child 发布到 CPU0 runqueue。

发布前失败可回滚；发布后 child 成为系统可见进程或线程。

## 4. 运行路径中的协作

```
RunQueue::fetch(cpu)
  └── Processor::run_tasks()
        ├── CAS Queued(CPU0) -> Running(CPU0)
        ├── 写本 CPU 的 PID/TID 快照
        ├── local processor.current = Some(task)
        └── __switch(idle, task)
```

用户态进入 trap 后，架构 trap handler 通过 `current_task()` 克隆本 CPU current
槽中的 TCB `Arc`，再通过 `task.process` 访问进程资源。

## 5. syscall 当前任务访问

每个 `PerCpu` 的 `CpuTaskState` 只保存：

| 字段 | 用途 |
|------|------|
| `current_pid/current_tid` | current 槽有效期间不变的无锁快照 |
| `current_syscall_id` | heap/perf 诊断构建中的本 CPU syscall 标识 |
| `processor.current` | 当前 TCB 的权威 `Arc` owner |

`current_task()` 在本 CPU processor 锁内克隆 `Arc`，随后释放锁；不存在全局
current 裸指针或伪造 `'static` 引用。父 PID、身份、进程组和页表 token 直接读取
TCB/PCB 权威 hint，避免 setter 漏刷影子缓存。返回的 `Arc` 不能跨
`schedule()` 或 `asm!(noreturn)` 保存，否则旧栈帧不会析构它。

## 6. 主动让出 CPU

`suspend_current_and_run_next()`：

| 步骤 | 对象 |
|------|------|
| 保留 `Processor.current` | 任务真实切离 CPU 前不能撤销 current owner |
| 结算 schedule out 时间 | TCB inner |
| `schedule(task_cx_ptr)` | HAL `__switch` |
| `finish_switch_out()` | idle 栈上清空 current，再 CAS `Running(cpu) -> Queued(cpu)` 并入队 |

该路径用于 yield 或时间片触发后的普通让出。

## 7. 阻塞路径

阻塞等待通过 WaitQueue 模板：

1. 当前任务加入业务 WaitQueue。
2. TaskManager 锁内完成 `Running(cpu) -> Blocking(cpu)` CAS。
3. 同一临界区加入 interruptible registry。
4. 释放业务锁。
5. `schedule()` 切回 idle。
6. idle 在真实切栈后提交 `Blocking(cpu) -> Blocked`。
7. 早到唤醒执行 `Blocking(cpu) -> Running(cpu)`，晚到唤醒执行
   `Blocked -> Queued(CPU0)`；两者都由同一个 TaskManager 入口裁决，晚到唤醒按
   `TASK_MANAGER -> CPU0 RunQueue` 的固定锁序完成容器交接。

带锁版本必须先入队、复查条件、再释放锁，避免丢失唤醒。

真正切换前的阻塞入口如下：

```rust
pub(crate) fn block_current_and_run_next_with_lock_checked<T>(
    lock: MutexGuard<'_, T>,
    should_block: impl FnOnce(&Arc<TaskControlBlock>) -> bool,
) {
    let task = current_task().unwrap();

    let task_cx_ptr = {
        let mut inner = task.acquire_inner_lock();
        inner.update_process_times_schedule_out();
        &mut inner.task_cx as *mut TaskContext
    };

    sleep_interruptible(task.clone());
    if !should_block(&task) {
        let _ = wake_interruptible(task.clone());
    }
    drop(lock);
    schedule(task_cx_ptr);
}
```

这个函数先把 task 原子迁移为 `Blocking(cpu)` 并加入 interruptible registry，
再复查是否仍应阻塞，然后释放业务锁并切换。复查失败时 wake 只取消阻塞，
不会把仍使用当前内核栈的任务提前加入 runqueue。
WaitQueue 自身只登记 `Weak<TaskControlBlock>`，不会提前写调度状态。

## 8. 定时器与等待

等待超时通过 `KernelTimerQueue` 与 TCB generation 配合：

| 元素 | 所属 |
|------|------|
| `TimerAction::WakeTask` | KernelTimerQueue |
| `wait_timer_generation` | TCB |
| `wait_io_timer_pending` | TCB |
| `wait_io_fallback_active_generation` | TCB |

timer 到期只负责把任务唤醒到 owner runqueue；真正 syscall 返回值由等待模板根据 `WaitResult` 转换。

## 9. 信号唤醒

信号可能存在于：

| 位置 | 来源 |
|------|------|
| TCB `sigpending` | `tkill/tgkill` 或硬件异常转同步信号 |
| PCB `shared_pending` | `kill(pid)`, `killpg`, pidfd_send_signal |

WaitQueue 可中断等待在睡前和醒后检查 `has_actionable_signal()`。如果有可处理信号，返回 `WaitResult::Interrupted`。

调度器 Ctrl+C 路径使用 `send_signal_to_interruptible(SIGINT)` 向 interruptible queue 中非 initproc 任务投递信号。

## 10. exec 路径中的协作

exec 成功后：

| 对象 | 更新 |
|------|------|
| TCB | 新 trap context、清 clear_child_tid、重置 robust list、禁用 alt signal stack |
| PCB | replace exe、replace VM、mark execed、set exe path |
| ExecSession | 临时关闭 clone，等待 sibling 在 owner CPU 清理并发布 live ack |
| fd table | 关闭 CLOEXEC fd |
| sighand/futex | reset/clear |
| Completion | complete vfork |

当前任务继续运行，其他线程在各自任务安全点完成线程级退出。exec owner 不远程摘除
runqueue 节点或释放 TCB；只有 live count 降为 1 后才安装新 MM。

## 11. exit 路径中的协作

线程退出：

| 对象 | 行为 |
|------|------|
| TCB | 状态 Zombie、释放线程资源 |
| PCB | live thread count 减一 |
| MM | 释放或缓存该线程用户资源 |
| Futex | clear_child_tid wake |

最后线程退出：

| 对象 | 行为 |
|------|------|
| PCB | mark process Zombie、收养 children、关闭 fd、释放 VM |
| TaskManager | 当前 TCB 入 zombie queue |
| parent PCB | child_exit_wait wake |
| registry | wait/auto-reap 时注销 |

## 12. wait 路径中的协作

父进程 wait：

1. syscall 层解析 pid/idtype/options。
2. ProcessManager 扫描 parent PCB children。
3. 找到 exited/stopped/continued child。
4. 若 `WNOWAIT` 为 false，消费状态并可能 detach child。
5. 更新 parent child_rusage。
6. 释放 child pid/quota/registry。
7. 用户写 status 或 siginfo。

无可观察 child 且非 `WNOHANG` 时，父任务睡在 `parent.child_exit_wait`。

wait 的 syscall 层只负责选项解析和用户写回，child 查找与回收集中在 `ProcessManager::wait_child()`：

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

这段包装解释了文档前面的分层：syscall 参数层决定 pid/options/status 指针，process manager 决定 child 状态和生命周期消费。

## 13. 状态一致性约束

| 约束 | 原因 |
|------|------|
| TCB zombie 不等于 PCB zombie | 多线程进程可能还有 live thread |
| child 发布前不可调度 | 用户指针写入失败需要回滚 |
| 当前 TCB 最后 Arc 不在当前栈 drop | 需要先切回 idle |
| WaitQueue 不持强引用 | 避免等待队列阻止 TCB 回收 |
| current `Arc` 不跨不返回的切换边界 | context switch 不会展开旧 Rust 栈帧 |
| 持锁阻塞必须使用 checked path | 防丢唤醒 |

这组约束的核心是生命周期分层。TCB 可以先 zombie 并被调度器延迟 drop，PCB 可以继续作为 wait 可见 zombie 存在，PID handle 可以继续防止 pid 复用。调试进程问题时不要只看一个状态位：线程是否还 live、进程是否 zombie、父进程是否已 wait、pid 是否已释放，是四个不同问题。

调度器相关 bug 还要检查 Per-CPU current 槽。PID/TID 快照必须和槽位同步设置、
清理；身份与页表 token 应读取权威对象。跨不返回的调度边界保存 current `Arc`
不会产生悬空引用，但会永久泄漏该强引用。

## 14. 调试核对点

| 现象 | 检查 |
|------|------|
| syscall 看到错误当前进程 | CPU-local 指针、current 槽和 PID/TID 快照 |
| 任务睡眠后无法唤醒 | WaitQueue 入队顺序与 wake path |
| exit 后 wait 不回收 | PCB children、ProcessState、child_exit_wait |
| 多线程 exec 后资源泄露/死锁 | ExecSession 门禁、live count ack 与等待点生命周期退出 |
| vfork 互锁 | Completion complete 和父等待路径 |
