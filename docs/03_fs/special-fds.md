---
title: "特殊文件描述符：eventfd / timerfd / pidfd / signalfd"
module: "fs/special-fds"
category: fs
status: draft
owner: "MangoCore Team"
last_updated: "2026-07-29"
code_paths:
  - "os/src/fs/eventfd.rs"
  - "os/src/fs/timerfd.rs"
  - "os/src/fs/pidfd.rs"
  - "os/src/syscall/process/signal.rs"
entry_points:
  - "sys_eventfd2"
  - "sys_timerfd_create"
  - "sys_timerfd_settime"
  - "sys_timerfd_gettime"
  - "sys_pidfd_open"
  - "sys_signalfd4"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "eventfd*"
    - "timerfd*"
    - "pidfd*"
    - "signalfd*"
  oscomp:
    - "basic"
related_docs:
  - "docs/03_fs/architecture.md"
  - "docs/03_fs/vfs-core.md"
  - "docs/03_fs/README.md"
---

## 概述

eventfd、timerfd、pidfd 和 signalfd 是四种通过标准文件描述符接口暴露内核事件通知能力的特殊 fd 类型。它们复用 VFS 的 `File` / `IndexNode` 抽象层，因此天然支持 epoll、poll/select 等 I/O 多路复用机制。这些 fd 由对应 syscall 创建后放入进程的 `FdTable`，不关联任何持久化存储。

这四个 fd 的部分共性特征：

- 实现 `IndexNode` trait，通过 `read_at()` / `write_at()` 操作
- 支持 `*_NONBLOCK` 和 `*_CLOEXEC` 标志
- 集成 VFS 框架，通过 `File` / `FdTable` 管理

eventfd、timerfd 与 pidfd 通过 `read_wait_queue()` 或 `read_event_queue()` 接入 poll/epoll。signalfd 的通知队列挂在共享 `sighand`；阻塞 `read(2)` 先克隆其队列 `Arc` 再进入 `WaitQueue`，使 fork 后的子进程可安全等待自己的通知点。

## eventfd -- 事件通知 fd

### 用途

eventfd 提供一个内核维护的 64 位计数器，用于线程间或进程间事件通知。典型场景是 epoll 配合 eventfd 实现异步事件唤醒。

### 创建与标志位

```rust
pub fn sys_eventfd2(initval: u32, flags: u32) -> isize
```

`sys_eventfd`（无 flags 版本，固定 initval=0）和 `sys_eventfd2` 均通过 `EventFd` 结构体实现。

| 标志 | 值 | 说明 |
|------|-----|------|
| `EFD_SEMAPHORE` | `0x1` | 信号量模式（见下文） |
| `EFD_NONBLOCK` | `0o4000` | 非阻塞 I/O |
| `EFD_CLOEXEC` | `0o2000000` | execve 时关闭 |

### 读语义

`read(fd, buf, 8)` 要求缓冲区不小于 8 字节。读操作返回写入的 8 字节值：

- **普通模式**（无 `EFD_SEMAPHORE`）：返回当前计数器的值，然后将计数器置零。如果计数器为 0，返回 `EAGAIN`（非阻塞）或阻塞等待。
- **信号量模式**（`EFD_SEMAPHORE`）：返回 1，计数器减 1。此模式适合多消费者场景，每个消费者一次消费一个计数单位。

读成功后通知写等待队列（`notify_writable`）。

### 写语义

`write(fd, buf, 8)` 将 8 字节值加到内部计数器上。约束：

- 写入值不能为 `u64::MAX`（`EINVAL`）
- 计数器上限为 `u64::MAX - 1`，超出此范围返回 `EAGAIN`
- 计数器添加操作成功后会通知读等待队列（`notify_readable`）

### 内部数据结构

```rust
struct EventFd {
    inner: Mutex<EventFdInner>,  // 计数器
    semaphore: bool,              // 是否信号量模式
    read_wait: EventWaitQueue,    // 读阻塞队列
    write_wait: EventWaitQueue,   // 写阻塞队列
    metadata: Metadata,
}
```

`EventWaitQueue` 是 epoll 与 fd 阻塞之间的桥接抽象，负责在条件满足时通过 `notify_events_all` 唤醒 epoll 等待者。

### poll 行为

- `EPOLLIN | EPOLLRDNORM`：当计数器 `> 0` 时可读
- `EPOLLOUT | EPOLLWRNORM`：当计数器 `< EVENTFD_COUNTER_MAX` 时可写

### 实现位置

实现在 `os/src/fs/eventfd.rs`。

## timerfd -- 定时器 fd

### 用途

timerfd 将 POSIX 定时器事件通过 fd 接口暴露，允许用户态用 `read()` 或 epoll 监听定时器到期事件。已实现五种 clock ID：

| clock_id | 说明 |
|----------|------|
| `CLOCK_REALTIME` | 系统实时时钟 |
| `CLOCK_MONOTONIC` | 单调递增时钟 |
| `CLOCK_BOOTTIME` | 单调递增（含休眠时间） |
| `CLOCK_REALTIME_ALARM` | 实时时钟（允许唤醒系统） |
| `CLOCK_BOOTTIME_ALARM` | 单调递增（允许唤醒系统） |

### 创建与设置

```rust
pub fn sys_timerfd_create(clock_id: usize, flags: u32) -> isize
pub fn sys_timerfd_settime(fd: usize, flags: u32, new_value: *const TimerFdSpec,
                           old_value: *mut TimerFdSpec) -> isize
pub fn sys_timerfd_gettime(fd: usize, curr_value_ptr: *mut TimerFdSpec) -> isize
```

- `sys_timerfd_create`：创建 timerfd。flags 支持 `TFD_CLOEXEC | TFD_NONBLOCK`。
- `sys_timerfd_settime`：设置定时器参数。`TimerFdSpec` 包含 `it_interval`（周期性间隔）和 `it_value`（首次到期时间）。`it_value` 为 0 表示 disarm。`TFD_TIMER_ABSTIME` 标志指定 `it_value` 为绝对时间。`TFD_TIMER_CANCEL_ON_SET` 标记在实时时钟设置时是否取消。
- `sys_timerfd_gettime`：返回当前剩余时间和间隔配置。

### 读语义

`read(fd, buf, 8)` 读出一个 `u64` 值，表示自上次 `read()`（或设置）以来定时器到期的次数。如果尚未到期，返回 `EAGAIN`。写操作被禁用（返回 `EINVAL`）。

### 内核定时器集成

timerfd 使用 `TIMERFD_REGISTRY` 全局弱引用注册表管理所有活跃 timerfd，通过内核定时器队列（`add_kernel_timer(TimerAction::TimerFdSweep)`）批量扫描到期货期。

核心机制：

1. `register_timerfd()` 在创建时将 timerfd 的 `Weak` 引用加入注册表
2. `rearm_timerfd_sweep()` 扫描注册表找到最早的到期时间，调度内核定时器
3. `wake_expired_timerfds()` 被内核定时器回调触发，遍历注册表、更新每个 timerfd 的到期计数
4. `handle_realtime_clock_was_set()` 处理实时时钟调整，重新计算 CLOCK_REALTIME timerfd 的单调截止时间
5. 每 64 次 sweep 执行一次惰性清理（`registry.retain` 移除已 drop 的引用）

### 到期计数计算

```rust
fn update_locked(inner: &mut TimerFdState, now: TimeSpec)
```

对于周期性定时器，根据 `interval` 和经过的时间计算共触发了多少次到期，一次性累加到 `expirations` 中。下一次到期时间 = 上次截止时间 + (count * interval)。非周期定时器到期后自动 disarm。

### poll 行为

`poll()` 先调用 `update_locked` 刷新到期状态，然后检查 `expirations > 0`；可读时返回 `EPOLLIN | EPOLLRDNORM`。

### 实现位置

实现在 `os/src/fs/timerfd.rs`。

## pidfd -- 进程 fd

### 用途

pidfd 提供一种不竞态（race-free）的进程引用方式。通过 `pidfd_open` 获得 fd，之后用 `pidfd_send_signal` 发送信号，或用 `pidfd_getfd` 获取目标进程的 fd。pidfd 避免了传统 `kill(pid)` 中 PID 重用导致的 TOCTOU 问题。

### 核心结构

```rust
struct PidFd {
    target_pid: usize,
    target: Weak<ProcessControlBlock>,
    state: Arc<PidFdState>,
    metadata: Metadata,
}

struct PidFdState {
    exited: AtomicBool,
    waiters: EventWaitQueue,
}
```

同一目标进程的所有 pidfd 共享一个 `Arc<PidFdState>`；PCB 只保存该状态的 `Weak`，不会阻止状态随最后一个 pidfd 释放。pidfd 自身持有强引用，因此目标 PCB 被 reap 后，已打开 pidfd 仍保留退出可读状态；而依赖目标 PCB 的操作在 PID 已释放后返回 `ESRCH`。

### 创建与使用

```rust
pub fn sys_pidfd_open(pid: usize, flags: usize) -> isize
pub fn sys_pidfd_send_signal(fd: usize, sig: usize, info: usize, flags: usize) -> isize
pub fn sys_pidfd_getfd(fd: usize, target_fd: usize, flags: usize) -> isize
```

- `pidfd_open` 系统调用查找 PID 对应的 `ProcessControlBlock` 并创建 `PidFd` 实例
- `new_pidfd_file_with_flags` 允许指定 `FileFlags`（pidfd_open 当前接受 `O_NONBLOCK`）
- `pidfd_send_signal`：通过 fd 向目标进程发送信号，免去 PID 查询和权限检查的竞态窗口
- `pidfd_getfd`：获取目标进程指定 fd 的副本
- `target_pid()`：返回当前有效的目标 PID；进程已释放则返回 `ESRCH`

### 读写语义

`read()` 和 `write()` 均返回 `EINVAL`。pidfd 不支持数据读写，仅作为进程引用的容器。

### poll 行为

`PidFd::poll()` 在目标进程已退出时返回 `EPOLLIN | EPOLLRDNORM`，否则返回 0。`read_wait_queue()` 和 `read_event_queue()` 都引用共享状态的 `EventWaitQueue`，因此 poll、ppoll/select 与 epoll 都能阻塞等待退出。

最后一个线程使 PCB 转为 `Zombie` 后、PID 可能被回收前，退出路径先以 release store 设置 `PidFdState::exited`，再通过 `notify_events_all(EPOLLIN | EPOLLRDNORM)` 唤醒全部 pidfd 等待者。若在 zombie 期间新开 pidfd，创建路径会从 PCB 生命周期状态初始化 `exited`，因而立即可读，不会错过已经发生的退出事件。

### 实现位置

实现在 `os/src/fs/pidfd.rs`。

## signalfd -- 信号 fd

### 用途

signalfd 允许进程通过 fd 接口接收信号，替代传统的 signal handler 回调方式。特别适合与 epoll 配合使用的事件驱动模型。

### 创建

```rust
pub fn sys_signalfd4(fd: usize, mask: usize, sigsetsize: usize, flags: usize) -> isize
```

- `fd == -1`：创建新的 signalfd，指定 `mask` 为要接收的信号集合
- `fd >= 0`：修改已有 signalfd 的信号掩码（复用已有 fd）
- `flags`：支持 `SFD_NONBLOCK` 和 `SFD_CLOEXEC`

`mask` 参数是一个用户空间指针，指向 `sigset_t`（当前实现为 64 位位掩码）。

### 内部结构

```rust
struct SignalFd {
    mask: Mutex<Signals>,
    metadata: Metadata,
}
```

`Signals` 是一个 `bitflags` 值，表示当前 signalfd 接收的信号集合。掩码可以通过第二次 `sys_signalfd4` 调用（带已有 fd）修改。

### 读语义

`read(fd, buf, len)` 返回一个或多个 `SignalfdSiginfo` 结构体：

```rust
struct SignalfdSiginfo {
    ssi_signo: u32,   // 信号编号
    ssi_errno: i32,   // 错误号
    ssi_code: i32,    // 信号来源 code
    ssi_pid: u32,     // 发送进程 PID
    ssi_uid: u32,     // 发送进程 UID
    ssi_fd: i32,      // 关联的 fd（SIGIO）
    ssi_tid: u32,     // 发送线程 TID
    ssi_band: u32,    // 带外数据标记
    ssi_overrun: u32, // timer overrun 计数
    ssi_status: i32,  // 退出/停止状态
    ssi_int: i32,     // sigval 整数
    ssi_ptr: u64,     // sigval 指针
    ssi_utime: u64,   // 用户态 CPU 时间
    ssi_stime: u64,   // 内核态 CPU 时间
    ssi_addr: u64,    // 错误地址
}
```

实现与 Linux `struct signalfd_siginfo` 兼容（128 字节），采用 `#[repr(C)]` 布局。

读操作从进程的待处理信号队列中取出与掩码匹配的信号，填充 `SignalfdSiginfo` 并返回。`read_at()` 在无匹配信号时仍返回 `EAGAIN`；常规阻塞 `read()` 则通过共享队列等待至信号投递后重试。写操作被禁用（返回 `EINVAL`）。

信号取出通过 `take_pending_signal_matching()` 实现，先搜索线程级 `sigpending`，再搜索进程级共享信号队列。

### poll 行为

`poll()` 检查当前线程和进程是否有匹配 signalfd 掩码的待处理信号。可读时返回 `EPOLLIN | EPOLLRDNORM`。

### 实现位置

`SignalFd` 和 `sys_signalfd4` 实现在 `os/src/syscall/process/signal.rs`。与信号系统的集成点包括 `take_pending_signal_matching()`（读）、`has_pending_signal_matching()`（poll）和来自共享 `sighand` 的 `EventWaitQueue`（阻塞 read / epoll 唤醒）。

## epoll 集成

所有四种特殊 fd 都实现了 `read_wait_queue()`（或 `read_event_queue()`），因此可以注册到 epoll 实例：

| fd 类型 | 可读条件 | epoll 事件 |
|---------|----------|------------|
| eventfd | counter > 0 | EPOLLIN / EPOLLRDNORM |
| timerfd | expirations > 0 | EPOLLIN / EPOLLRDNORM |
| signalfd | 有在掩码范围内的待处理信号 | EPOLLIN / EPOLLRDNORM |
| pidfd | 进程退出且可 wait（待实现） | EPOLLIN（规划中） |

事件发生时对应 fd 的 `EventWaitQueue` 会调用 `notify_events_all` 唤醒所有在 epoll 上等待的线程。具体 epoll 使用方式参见 `docs/03_fs/README.md` 中关于 eventpoll 的部分。

## 测试映射

| LTP 测试集范围 | 覆盖情况 |
|----------------|----------|
| `eventfd*` | eventfd 基本功能、eventfd2、semaphore 模式、阻塞/非阻塞 |
| `timerfd*` | timerfd_create、timerfd_settime 相对/绝对时间、周期性定时器 |
| `pidfd*` | pidfd_open、pidfd_send_signal、pidfd_getfd |
| `signalfd*` | signalfd 创建、掩码修改、信号读取、非阻塞模式 |

OSComp 基础测试（mask=0x001）包含以上四种 fd 的冒烟测试。

## 已知问题

1. **pidfd poll 未实现**：当前 `PidFd` 的 `poll()` 返回 0，意味着无法通过 epoll 监听进程退出事件。`pidfd_poll` 需要检查目标进程是否已变成僵尸态（`PIDFD_RELEASED` 或子进程 waitable）。
2. **timerfd 注册表清理是全扫描**：`wake_expired_timerfds` 每次遍历所有注册的 timerfd。大量 timerfd（数千个）时性能可能退化。当前每 64 次 sweep 清理一次已 drop 的引用。
3. **signalfd 实时信号排队**：LTP `signalfd*` 测试中实时信号（SIGRTMIN+）的多信号排队需要通过 `SigQueued` 机制实现，当前部分实时信号语义尚未完全覆盖。
4. **eventfd 阻塞竞争**：在竞争条件下（多个线程同时读同一个 eventfd），有符号量模式可能多个消费者各取走 1，但无符号量模式只有一个成功读取。
