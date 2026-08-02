---
title: "时间、调度 ABI、rlimit 与 prctl"
category: process
status: stable
author: MangoCore Team
last_update: 2026-08-03
tags: [process, time, sched, rlimit, prctl]
---

# 时间、调度 ABI、rlimit 与 prctl

## 1. 源码范围

| 文件 | 内容 |
|------|------|
| `os/src/syscall/process/time.rs` | nanosleep、itimer、POSIX timer、clock/timex |
| `os/src/syscall/process/ids.rs` | get/set id、sched、rlimit、prctl、capability、ptrace、process_vm |
| `os/src/task/task.rs` | 线程 rusage、ProcClock 与调度字段 |
| `os/src/task/process.rs` | 线程组 CPU 账户、legacy/POSIX timer owner |
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

计时状态按共享域拆分。TCB inner 只保留线程 CPU 记账与采样锚点：

```rust
pub clear_child_tid: usize,
pub robust_list: RobustList,
pub rusage: Rusage,
pub clock: ProcClock,
pub pending_oom_kill: bool,
```

PCB 的 `cpu_account` 汇总所有线程的 user/system 时间；`interval_timers` 和
`posix_timers` 分别保护 legacy interval timer 与 POSIX timer。三者都不能塞回某个
“创建线程”的 TCB，否则 sibling 无法观察同一线程组时钟，创建线程退出还会错误删除
整个进程的 timer。

`ProcClock` 保存进入用户态/内核态的时间戳，CPU 时间统计函数据此累加 `Rusage`：

```rust
#[repr(C)]
pub struct ProcClock {
    last_enter_u_mode: TimeVal,
    last_enter_s_mode: TimeVal,
}

impl ProcClock {
    pub fn new() -> Self {
        let now = TimeVal::now();
        Self {
            last_enter_u_mode: now,
            last_enter_s_mode: now,
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

三个 timer 都属于 PCB 的 `IntervalTimerTable`：thread clone 共享，普通 fork 创建空表，
exec 保留，最后线程退出清空。表内保存所属时钟域的绝对 deadline，而不是在各 TCB 中递减
remaining，因此 sibling 在不同 CPU 上消耗 CPU 时间时不会覆盖彼此状态。

- `ITIMER_REAL` 使用 monotonic elapsed time 和 `TimerAction::IntervalTimerSignal`；action
  携带 PCB `Weak` 与 generation，旧节点不能命中新装载。墙钟调整不改变 REAL timer。
- `ITIMER_VIRTUAL` 读取线程组 user CPU 累计；`ITIMER_PROF` 读取线程组 user+system 累计。
  trap-return 与 schedule-out 安全点在表锁内唯一领取到期，锁外投递进程共享信号。
- `getitimer()` 以当前时钟采样计算 remaining；active 但已经到期、尚待安全点领取时返回
  最小非零值。`setitimer(new=NULL)` 按 Linux 历史兼容语义停表。

系统调用先冲刷当前线程的 CPU 记账尾数，再在一个 timer 表临界区快照旧值并提交新值；
锁外注册 REAL heap 节点和 copyout 旧值。old copyout 的 `EFAULT` 不回滚已发布配置。

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
| `CLOCK_REALTIME`/`CLOCK_MONOTONIC` | wall-time 到期路径支持 |
| `CLOCK_BOOTTIME` 等部分 clock | 根据 `valid_posix_timer_clock()` |
| process/thread CPU clock | 按线程组累计或创建线程累计的真实 CPU 消耗到期 |

每进程最多 32 个 POSIX timer。`timer_create()`：

| 条件 | 错误 |
|------|------|
| timerid null | `EFAULT` |
| clock_id 无效 | `EINVAL` |
| `sigev_notify` 非 `SIGEV_SIGNAL/SIGEV_NONE` | `EINVAL` |
| timer slot 满 | `EAGAIN` |
| Vec 扩容失败 | `ENOMEM` |

`timer_settime()` 只接受 `TIMER_ABSTIME` 作为 flag；`new_value` null 返回 `EINVAL`。

`PosixTimerTable` 是 PCB 的唯一 owner：线程 clone 共享该表，普通 fork 创建空表，exec 和
最后线程退出清空。表内 slot 使用 `Vacant/Reserved/Active` 三态，避免 `timer_create()`
在用户 copyout 窗口暴露半初始化对象。每次 arm 从全表分配唯一 `arm_seq`；删除后复用相同
ID 时，旧 kernel timer action 也因序号不匹配而失效。

## 7. POSIX timer 到期

wall clock 到期由 `TimerAction::PosixTimerSignal` 处理：

1. 升级 PCB `Weak`，校验 timer ID、`arm_seq` 和 deadline。
2. 一次性 timer 清空 value/deadline。
3. 周期 timer 计算 missed overruns。
4. 对 realtime absolute timer 维护 `realtime_abs_deadline`。
5. 在 timer 表锁内构造带 `sigev_value` 的 `SI_TIMER` 值事件。
6. 释放 owner 锁后加入进程共享 pending，再选择一个可接收 sibling 并唤醒。
7. 周期 timer 重新加入 kernel timer queue。

`KernelTimerQueue::compact()` 也检查同一组 owner/arm/deadline 条件，可提前清理 rearm、
delete、exec 和退出产生的 stale 节点。compact 在队列锁下只读 timer 表，因此装载路径必须
先释放表锁再调用 `add_kernel_timer()`，禁止形成反向锁边。CPU clock timer 不能使用
wall-time heap 驱动。

CPU clock timer 使用另一条路径：

1. `CLOCK_PROCESS_CPUTIME_ID` 采样 PCB 的线程组 user+system 累计；
   `CLOCK_THREAD_CPUTIME_ID` 通过创建者 `Weak<TCB>` 采样该线程累计。
2. trap return 和 schedule-out 安全点先在表锁外采样，再在表锁内比较 `cpu_deadline_us`；
   多个 CPU 同时扫描同一 PCB 时，只有持锁者能清除 one-shot 或推进 periodic deadline。
3. 到期事件写入最多 32 项的固定栈批次，释放表锁后才进入 signal queue 和调度器。
4. `posix_cpu_timers_active` 只是 Release/Acquire fast hint；slot/deadline 始终是权威状态。
5. 微秒记账会把非零纳秒 duration 上取整；已过期但尚未被安全点领取时，gettime 返回 1ns。

当前内核采用安全点抢占，所以长期不经过 trap return、block/yield/exit 的内核循环可能延迟
CPU timer 投递；并发 CPU 记账也只会让锁外样本偏旧、延迟到下一安全点，不会提前或重复触发。

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

B34 的 `sched_setaffinity()` 已支持 current 线程改 mask 与必要自迁移；B35 支持远程稳定
Blocked 线程改 mask。后者只有在目标仍以同一 TCB 指针登记于 interruptible registry 时才
成功，mask 更新和 wake 由同一 `TASK_MANAGER` 锁串行化。B36 支持稳定 Queued 线程：owner
仍合法时只更新 mask，排除 owner 时以 `Migrating` 在两把不重叠持有的 runqueue 锁之间交接。
远程 Running/Blocking 仍没有运行期停止协议，普通生产任务因此继续固定 CPU0。现有 hermetic
用户探针覆盖 current 路径；B35/B36 focused 分别覆盖生产 Blocked→wake 重定向与 Queued 搬队，
但尚未从用户态端到端覆盖远程 TID。

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
| legacy itimer 重复或丢失 | PCB timer kind、generation 与 clock domain |
| POSIX timer overrun 错误 | periodic deadline 与 pending signal |
| sched_getattr 回读不符 | TCB/PCB sched_state 同步 |
| prctl 字段跨 exec/fork 异常 | 哪些字段 clone 复制、哪些 exec 保留或 reset |
