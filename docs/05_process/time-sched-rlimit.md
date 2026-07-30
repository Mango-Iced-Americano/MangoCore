---
title: "时间、调度 ABI、rlimit 与 prctl"
category: process
status: stable
author: MangoCore Team
last_update: 2026-07-30
tags: [process, time, sched, rlimit, prctl]
---

# 时间、调度 ABI、rlimit 与 prctl

## 1. 源码范围

| 文件 | 内容 |
|------|------|
| `os/src/syscall/process/time.rs` | nanosleep、itimer、POSIX timer、clock/timex |
| `os/src/syscall/process/ids.rs` | get/set id、sched、rlimit、prctl、capability、ptrace、process_vm |
| `os/src/task/task.rs` | rusage、ProcClock、timer state、rlimit 字段 |
| `os/src/task/manager.rs` | KernelTimerQueue |
| `os/src/task/sleep.rs` | sleep helper |

本页把参与真实调度/计时的状态、syscall 回读字段和权限检查字段分开描述。

## 2. CPU 时间统计

TCB inner 中：

| 字段 | 说明 |
|------|------|
| `rusage.ru_utime` | 用户态 CPU 时间 |
| `rusage.ru_stime` | 内核态 CPU 时间 |
| `clock.last_enter_u_mode` | 上次进入用户态时间 |
| `clock.last_enter_s_mode` | 上次进入内核态时间 |

更新点：

| 方法 | 时机 |
|------|------|
| `update_process_times_enter_trap()` | 用户态进入内核态 |
| `update_process_times_leave_trap()` | 内核态返回用户态 |
| `update_process_times_schedule_out()` | 任务在内核态让出 CPU |
| `update_process_times_schedule_in()` | 任务被调度运行 |

`ru_maxrss` 在进程退出时根据 resident user bytes 更新；其他 rusage 字段多数保持 0。

计时状态实际保存在 TCB inner 的 `rusage`、`clock`、itimer 和 POSIX timer 字段中：

```rust
pub clear_child_tid: usize,
pub robust_list: RobustList,
pub rusage: Rusage,
pub clock: ProcClock,
pub timer: [ITimerVal; 3],
pub real_timer_deadline: Option<TimeSpec>,
pub real_timer_generation: usize,
pub posix_timers: Vec<Option<PosixTimer>>,
pub pending_oom_kill: bool,
```

`ProcClock` 保存进入用户态/内核态的时间戳，CPU 时间统计函数据此累加 `Rusage`：

```rust
#[repr(C)]
pub struct ProcClock {
    last_enter_u_mode: TimeVal,
    last_enter_s_mode: TimeVal,
    pub last_real_timer_update: TimeVal,
}

impl ProcClock {
    pub fn new() -> Self {
        let now = TimeVal::now();
        Self {
            last_enter_u_mode: now,
            last_enter_s_mode: now,
            last_real_timer_update: now,
        }
    }
}
```

## 3. RLIMIT_CPU

`enforce_cpu_rlimit()` 使用 `ru_utime + ru_stime`：

| 条件 | 行为 |
|------|------|
| 超过 hard limit | 加 `SIGKILL` |
| 超过 soft limit 且尚未发送 | 加 `SIGXCPU` 并置 `cpu_limit_sigxcpu_sent` |
| soft/hard 都为 unlimited | 不处理 |

limit 单位为秒，内部转换为微秒。

## 4. setitimer/getitimer

支持 `which = 0..2`：

| which | timer | 信号 |
|-------|-------|------|
| 0 | `ITIMER_REAL` | `SIGALRM` |
| 1 | `ITIMER_VIRTUAL` | `SIGVTALRM` |
| 2 | `ITIMER_PROF` | `SIGPROF` |

REAL timer 使用 `KernelTimerQueue::TimerAction::SendSignal`，并用 `real_timer_generation` 过滤旧 timer。VIRTUAL/PROF 在 CPU 时间统计更新时递减。

`getitimer(ITIMER_REAL)` 会根据 `real_timer_deadline - now` 计算剩余时间。

## 5. nanosleep

`sys_nanosleep(req, rem)`：

1. 读取 `req`。
2. 校验 `tv_nsec < NSEC_PER_SEC` 且 `tv_sec` 合法。
3. 调用 `sleep_relative_interruptible(req)`。
4. 成功返回 0。
5. 被信号中断时，如果 `rem` 非 null，写入剩余时间，然后返回 `EINTR`。

sleep 使用任务等待与 kernel timer，不忙等。

源码路径如下：

```rust
pub fn sys_nanosleep(req: *const TimeSpec, rem: *mut TimeSpec) -> isize {
    let token = current_user_token();
    let req = match UserPtr::new(req).read(token) {
        Ok(req) => req,
        Err(errno) => return errno,
    };
    if !is_valid_timespec(req) {
        return EINVAL;
    }

    match sleep_relative_interruptible(req) {
        Ok(()) => SUCCESS,
        Err(interrupted) => {
            if !rem.is_null() {
                if let Err(errno) = UserPtrMut::new(rem).write(token, &interrupted.remaining) {
                    return errno;
                }
            }
            EINTR
        }
    }
}
```

因此 `rem` 只在被信号中断时写入；`req` 本身非法时不会进入 sleep。

## 6. POSIX timer

支持 clock：

| clock | 支持情况 |
|-------|----------|
| `CLOCK_REALTIME` | 支持 |
| `CLOCK_MONOTONIC` | 支持 |
| `CLOCK_BOOTTIME` 等部分 clock | 根据 `valid_posix_timer_clock()` |

每任务最多 `MAX_POSIX_TIMERS = 32`。`timer_create()`：

| 条件 | 错误 |
|------|------|
| timerid null | `EFAULT` |
| clock_id 无效 | `EINVAL` |
| `sigev_notify` 非 `SIGEV_SIGNAL/SIGEV_NONE` | `EINVAL` |
| timer slot 满 | `EAGAIN` |
| Vec 扩容失败 | `ENOMEM` |

`timer_settime()` 只接受 `TIMER_ABSTIME` 作为 flag；`new_value` null 返回 `EINVAL`。

POSIX timer 对象直接挂在创建线程的 TCB 上：

```rust
#[derive(Clone, Copy, Debug)]
pub struct PosixTimer {
    pub clock_id: usize,
    pub signal: Signals,
    pub interval: TimeSpec,
    pub value: TimeSpec,
    pub deadline: Option<TimeSpec>,
    pub realtime_abs_deadline: Option<TimeSpec>,
    pub generation: usize,
    overrun: usize,
}

impl PosixTimer {
    const OVERRUN_MAX: usize = i32::MAX as usize;

    pub fn new(clock_id: usize, signal: Signals) -> Self {
        Self {
            clock_id,
            signal,
            interval: TimeSpec::new(),
            value: TimeSpec::new(),
            deadline: None,
            realtime_abs_deadline: None,
            generation: 0,
            overrun: 0,
        }
    }
}
```

`timer_create()` 负责选择空 slot 或追加 slot，并在写回 timerid 失败时撤销刚创建的 timer：

```rust
pub fn sys_timer_create(
    clock_id: usize,
    sevp: *const SigeventHeader,
    timerid: *mut i32,
) -> isize {
    if timerid.is_null() {
        return EFAULT;
    }
    if !valid_posix_timer_clock(clock_id) {
        return EINVAL;
    }

    let token = current_user_token();
    let signal = if sevp.is_null() {
        Signals::SIGALRM
    } else {
        let event = match UserPtr::new(sevp).read(token) {
            Ok(event) => event,
            Err(errno) => return errno,
        };
        match event.sigev_notify {
            SIGEV_SIGNAL => match Signals::from_signum(event.sigev_signo as usize) {
                Ok(signal) => signal,
                Err(_) => return EINVAL,
            },
            SIGEV_NONE => Signals::empty(),
            _ => return EINVAL,
        }
    };

    let task = current_task().unwrap();
    let id = {
        let mut inner = task.acquire_inner_lock();
        if let Some((id, slot)) = inner
            .posix_timers
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| slot.is_none())
        {
            *slot = Some(PosixTimer::new(clock_id, signal));
            id
        } else {
            if inner.posix_timers.len() >= MAX_POSIX_TIMERS {
                return EAGAIN;
            }
            if inner.posix_timers.try_reserve(1).is_err() {
                return ENOMEM;
            }
            inner
                .posix_timers
                .push(Some(PosixTimer::new(clock_id, signal)));
            inner.posix_timers.len() - 1
        }
    };

    match UserPtrMut::new(timerid).write(token, &(id as i32)) {
        Ok(()) => SUCCESS,
        Err(errno) => {
            let mut inner = task.acquire_inner_lock();
            if let Some(slot) = inner.posix_timers.get_mut(id) {
                *slot = None;
            }
            errno
        }
    }
}
```

## 7. POSIX timer 到期

到期由 `TimerAction::PosixTimerSignal` 处理：

1. 校验 timer id、generation、deadline。
2. 一次性 timer 清空 value/deadline。
3. 周期 timer 计算 missed overruns。
4. 对 realtime absolute timer 维护 `realtime_abs_deadline`。
5. 如果 signal 非空，向线程 pending 队列投递 `SI_TIMER`。
6. 如果信号可唤醒 Interruptible 任务，转 Ready 并唤醒。
7. 周期 timer 重新加入 kernel timer queue。

到期处理在 `KernelTimerQueue::run_timer()` 中根据 `TimerAction` 分支执行。`WakeTask` 分支体现了 generation 过滤和 fallback timer 重挂逻辑：

```rust
pub fn run_timer(timer: KernelTimer, now: TimeSpec) -> bool {
    match timer.action {
        TimerAction::WakeTask { task, generation, fallback_ms } => {
            let Some(task) = task.upgrade() else { return false };
            task.wait_io_timer_pending
                .store(false, AtomicOrdering::Relaxed);

            if let Some(ms) = fallback_ms {
                let active = task
                    .wait_io_fallback_active_generation
                    .load(AtomicOrdering::Acquire);
                if active == 0 {
                    return false;
                }
                if active != generation {
                    let current =
                        task.wait_timer_generation.load(AtomicOrdering::Relaxed);
                    if active == current {
                        if !task.wait_io_timer_pending.swap(true, AtomicOrdering::Relaxed) {
                            let new_gen = task
                                .wait_timer_generation
                                .fetch_add(1, AtomicOrdering::Relaxed)
                                + 1;
                            add_kernel_timer(
                                TimerAction::WakeTask {
                                    task: Arc::downgrade(&task),
                                    generation: new_gen,
                                    fallback_ms: Some(ms),
                                },
                                TimeSpec::now() + TimeSpec::from_ms(ms),
                            );
                            task.wait_io_fallback_active_generation
                                .store(new_gen, AtomicOrdering::Release);
                        }
                    }
                    return false;
                }
            }

            if task.wait_timer_generation.load(AtomicOrdering::Relaxed) != generation {
                crate::task::perf::record_ktimer_stale_waketask();
                return false;
            }

            let should_wake = matches!(
                task.task_status(),
                super::TaskStatus::Blocking(_) | super::TaskStatus::Blocked
            );
            if should_wake && wake_interruptible(task) {
                crate::task::perf::record_ktimer_real_wake();
                return true;
            }
            false
        }
```

这段代码只展示 `WakeTask` 分支；`SendSignal`、`PosixTimerSignal` 和 `TimerFdSweep` 在同一个 match 中处理。

## 8. realtime clock 调整

time.rs 中 `TimexState` 保存 `adjtimex` 可读写字段：

| 字段 | 说明 |
|------|------|
| `offset/freq/maxerror/esterror/status/constant/tick/tai` | Linux timex 结构对应字段 |

`CAP_SYS_TIME` 用于设置类操作权限。`wake_realtime_abstime_sleepers_after_clock_set()` 用于 realtime 绝对睡眠在时钟跳变后重新唤醒/调整。

## 9. 调度 ABI 状态

TCB inner 保存：

| 字段 | 说明 |
|------|------|
| `sched_policy` | POSIX 调度策略回读 |
| `sched_priority` | 优先级回读 |
| `sched_reset_on_fork` | fork 时重置调度状态 |
| `sched_nice` | nice 值，参与简化 runqueue 选择 |
| `sched_runtime/deadline/period` | sched_attr 保存字段 |

当前 runnable 容器已拆为 Per-CPU RunQueue。每个 TCB 已持有 per-thread
`cpus_allowed`，但生产任务的初始 mask 仍只有 CPU0；
FIFO/RR/DEADLINE 等字段用于 syscall 语义、fork reset 和测试回读。

### 9.1 当前 CPU 查询与 affinity 边界

`getcpu()` 已返回调用瞬间的连续逻辑 CPU 编号。该编号来自 `smp::cpu_id()`，用于索引
PerCpu 和 scheduler mask，不等同于 RISC-V hart ID 或 LoongArch CoreID。syscall 在写用户
指针前只采样一次 CPU；`cpu`/`node` 为 NULL 时分别忽略，第三个 tcache 参数忽略。当前没有
NUMA，node 固定返回 0。

B31 已建立内核权威的 `cpus_allowed`：普通任务初始为 bit0，clone 继承父线程
mask，exec 保留原 TCB 的 mask；定向 ktest 任务和受控迁移探针可在首次发布前设置
更窄或 CPU0/CPU1 允许集。首次发布、yield requeue 与 blocked wake 都拒绝越过该 mask。

B32 已让 raw `sched_getaffinity()` 读取这份真实 mask。`pid=0` 查询调用线程，正数只按
TID 查找；当前 8 核 mask 占一个 `usize`，`cpusetsize` 必须至少为 8 且按 8 字节粒度
对齐，成功返回实际复制的 8 字节而不是 libc wrapper 的 0。查询先释放 task registry
锁，再进行可能 fault-in 的用户拷贝，不跨 uaccess 持有调度锁。

`sched_setaffinity()` 仍没有 Running/Queued/Blocked 的运行期迁移协议，普通生产任务
因此继续固定 CPU0。B32 的 hermetic 用户探针在显式 yield 前后同时验证 mask 保持
`0b11` 和 getcpu 返回 `0 -> 1`；它只覆盖单线程 leader，严格 TID 查找还依赖生产源码
审计，不能据此宣称完整动态 affinity 已实现。

fork 时，如果父设置 reset 或策略为 FIFO/RR，子任务降回 normal，priority/nice/runtime/deadline/period 清零。

调度 syscall 并不改变真实调度算法；它更新 TCB 中的 ABI 状态，并同步到进程级缓存。`sched_setscheduler()` 主路径如下：

```rust
pub fn sys_sched_setscheduler(pid: usize, policy: usize, param: *const SchedParam) -> isize {
    if signed_pid_invalid(pid) || param.is_null() {
        return EINVAL;
    }
    if !valid_sched_policy(policy) {
        return EINVAL;
    }
    let task = match find_task_for_pid_or_current(pid) {
        Ok(task) => task,
        Err(errno) => return errno,
    };
    let param = match UserPtr::new(param).read(current_user_token()) {
        Ok(param) => param,
        Err(_) => return EFAULT,
    };
    if !valid_sched_priority(policy, param.sched_priority) {
        return EINVAL;
    }
    let base_policy = policy & !SCHED_RESET_ON_FORK;
    let new_reset_on_fork = policy & SCHED_RESET_ON_FORK != 0;
    let (old_policy, old_priority, old_reset_on_fork) = {
        let inner = task.acquire_inner_lock();
        (inner.sched_policy, inner.sched_priority, inner.sched_reset_on_fork)
    };
    if !can_apply_sched_change(
        &task,
        old_policy,
        old_priority,
        old_reset_on_fork,
        base_policy,
        param.sched_priority,
        new_reset_on_fork,
    ) {
        return EPERM;
    }
    let state = {
        let mut inner = task.acquire_inner_lock();
        inner.sched_policy = base_policy;
        inner.sched_priority = param.sched_priority;
        inner.sched_reset_on_fork = new_reset_on_fork;
        inner.sched_runtime = 0;
        inner.sched_deadline = 0;
        inner.sched_period = 0;
        SchedState {
            policy: inner.sched_policy,
            priority: inner.sched_priority,
            reset_on_fork: inner.sched_reset_on_fork,
            nice: inner.sched_nice,
            runtime: inner.sched_runtime,
            deadline: inner.sched_deadline,
            period: inner.sched_period,
        }
    };
    sync_process_sched_state(&task, state);
    SUCCESS
}
```

`sched_setattr()` 额外写入 nice/runtime/deadline/period，并调用 `update_ready_nice()` 调整 owner runqueue 中的 nice hint：

```rust
inner.sched_policy = base_policy;
inner.sched_priority = priority;
inner.sched_reset_on_fork = new_reset_on_fork;
inner.sched_nice = attr.sched_nice;
task.sched_nice_hint
    .store(inner.sched_nice, core::sync::atomic::Ordering::Relaxed);
inner.sched_runtime = attr.sched_runtime;
inner.sched_deadline = attr.sched_deadline;
inner.sched_period = attr.sched_period;
```

## 10. rlimit 字段

TCB inner 保存：

| rlimit | 实际使用 |
|--------|----------|
| `RLIMIT_RTPRIO` | 非 root 设置实时调度权限检查 |
| `RLIMIT_NICE` | nice 权限/回读 |
| `RLIMIT_SIGPENDING` | 实时信号 pending 限额语义 |
| `RLIMIT_STACK` | ABI 可见；用户栈映射仍按固定 slot |
| `RLIMIT_MEMLOCK` | mlock/mlockall 权限和限额 |
| `RLIMIT_FSIZE` | 普通文件写入长度限制 |
| `RLIMIT_NPROC` | 保存 ABI 可见状态 |
| `RLIMIT_CPU` | 软硬限制投递 SIGXCPU/SIGKILL |
| `RLIMIT_CORE` | 不生成 core，但 wait status 可见 core dump 位语义 |

## 11. prctl 状态

`ids.rs` 支持多种 prctl：

| prctl | 字段 |
|-------|------|
| `PR_SET/GET_PDEATHSIG` | `pdeath_signal` |
| `PR_SET/GET_DUMPABLE` | `dumpable` |
| `PR_SET/GET_NAME` | `task_comm[16]` |
| `PR_SET/GET_SECCOMP` | `seccomp_mode`, `seccomp_filter` |
| `PR_SET/GET_TIMERSLACK` | `timer_slack_ns` |
| `PR_SET/GET_CHILD_SUBREAPER` | PCB `child_subreaper` |
| `PR_SET/GET_NO_NEW_PRIVS` | `no_new_privs` |
| `PR_SET/GET_THP_DISABLE` | `thp_disabled` |
| capability bounding/ambient | cap sets / securebits |

这些字段在以下检查点中被读取：seccomp 分发、时间设置权限、ptrace 权限和 capability bit 查询。

## 12. seccomp 子集

seccomp 状态包括：

| 字段 | 说明 |
|------|------|
| `seccomp_mode` | disabled/strict/filter |
| `seccomp_filter` | 从用户复制的 BPF 指令 |
| `ACTIVE_SECCOMP_TASKS` | 全局是否存在 seccomp task 的快速判断 |

filter 指令长度上限 `4096`。syscall dispatch 根据 seccomp 结果允许调用、杀线程或杀进程。

## 13. capability 与身份

身份字段同时存在于 TCB inner 和原子 hint：

| 字段 | 用途 |
|------|------|
| `uid/euid/suid/fsuid` | 权限判断和 getuid |
| `gid/egid/sgid/fsgid` | 权限判断和 getgid |
| `groups` | supplementary groups |
| `cap_effective/permitted/inheritable/bounding/ambient` | capability 集合保存和查询 |

部分权限检查实际使用 euid==0 或特定 cap bit，例如 `CAP_SYS_TIME`、`CAP_SYS_PTRACE`、`CAP_SYS_TTY_CONFIG`。

## 14. ptrace 子集

`sys_ptrace()` 支持：

| request | 行为 |
|---------|------|
| `PTRACE_TRACEME` | 设置当前 task `ptrace_traceme` |
| `PTRACE_CONT` | 对 traceme target 发送 `SIGCONT` |
| `PTRACE_KILL` | 对 traceme target 发送 `SIGKILL` |
| `PTRACE_ATTACH` | euid 0 才可 attach，目标进入 stopped |
| `PTRACE_DETACH` | detach 并 mark continued |
| 其他 | `EIO` |

其他 ptrace 请求返回 `EIO`，读写寄存器/内存类请求没有接入处理分支。

## 15. ioprio 与 personality

`ioprio_get/set`：

| 限制 | 错误 |
|------|------|
| which 非 process | `EINVAL` |
| who 非 0 且不是当前 pid | `ESRCH` |
| class/prio 非法 | `EINVAL` |

ioprio 只保存 ABI 状态，不影响实际 I/O 调度。

`personality()` 读写 `inner.personality`，`0xffff_ffff` 或 `usize::MAX` 表示只读旧值。

时间和调度 ABI 的源码阅读要分清两类字段：一类参与运行时行为，例如 rusage CPU 时间、RLIMIT_CPU、itimer/POSIX timer deadline、nice hint；另一类用于 syscall 设置和回读，例如部分 sched_attr、ioprio、personality、capability 集合。文档中的表格按“实际使用点”描述，判断行为时以对应 syscall 分支和 task inner 字段读写点为准。

timer 类接口的共同路径是：syscall 读取用户 timespec/sigevent，转换成内部 deadline 或 timer slot，注册到 `KernelTimerQueue`，到期后唤醒任务或投递信号。若 timer 不触发，先看时间是否推进，再看 kernel timer queue 是否入队，最后看到期 action 是否唤醒/投递到正确 task。

## 16. 调试核对点

| 现象 | 检查 |
|------|------|
| CPU limit 不触发 | enter/leave trap 是否更新 rusage |
| ITIMER_REAL 重复或丢失 | `real_timer_generation` 与 deadline |
| POSIX timer overrun 错误 | periodic deadline 与 pending signal |
| sched_getattr 回读不符 | TCB/PCB sched_state 同步 |
| prctl 字段跨 exec/fork 异常 | 哪些字段 clone 复制、哪些 exec 保留或 reset |
