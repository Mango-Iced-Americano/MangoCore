---
title: "poll/select 与 epoll 实现"
module: "fs/poll+epoll"
category: fs
status: draft
owner: "MangoCore Team"
last_updated: "2026-06-29"
code_paths:
  - "os/src/fs/poll.rs"
  - "os/src/fs/eventpoll.rs"
  - "os/src/fs/vfs/event.rs"
entry_points:
  - "sys_ppoll"
  - "sys_pselect6"
  - "sys_epoll_create1"
  - "sys_epoll_ctl"
  - "sys_epoll_pwait"
  - "EventWaitQueue"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "epoll*"
    - "poll*"
    - "select*"
    - "pselect*"
  oscomp:
    - "basic"
    - "busybox"
    - "lua"
related_docs:
  - "docs/03_fs/architecture.md"
  - "docs/03_fs/vfs-core.md"
  - "docs/03_fs/special-fds.md"
---

## 概述

MangoCore 提供两套 I/O 事件通知机制：传统的 poll/select（同步轮询）和可扩展的 epoll（事件驱动）。两者共享底层的 `poll_events()` 接口和 `EventWaitQueue` 通知基础设施，但设计哲学和性能特征差异显著。

## poll/select 实现

`os/src/fs/poll.rs` 实现了 `poll()`、`ppoll()`、`select()` 和 `pselect()` 四个系统调用。核心流程是扫描 -> 条件等待 -> 返回就绪集合。

### 数据结构

**PollFd** 是 32 位风格的 fd 事件结构，包含 `fd`、`events`（请求事件）和 `revents`（返回事件）。事件类型通过 `PollEvent` bitflags 定义，与 Linux `poll.h` 一致，包括 POLLIN/POLLOUT/POLLPRI 等请求事件和 POLLERR/POLLHUP/POLLNVAL 等隐含事件。

**FdSet** 是一个 1024 位（16 x u64）的 bitmap，用于 `select()`/`pselect()`。限制最大 nfds 为 1024（`poll()` 为 4096）。

### 扫描流程

一次完整的 poll 扫描分为三个阶段：

1. **预扫描（collect_wait=true）**：调用 `scan_ppoll()` 或 `scan_pselect()`，对每个 fd 执行 `file.poll_events()` 获取就绪事件。如果 fd 未就绪且 `collect_wait=true`，收集其 `PollWaitQueue` 用于后续阻塞等待。`timeout=0` 的首扫先等待一张内部网络 poll ticket，避免只读取 CPU0 worker 尚未消费的旧网络状态；这不消耗用户 timeout，下一测试批次会用 ktest 覆盖该边界。
2. **等待**：如果预扫描没有找到就绪 fd，调用 `poll_wait()` 在收集到的 wait_queue 上阻塞。`poll_wait` 使用 `WaitQueue::wait_on_queues_interruptible_timeout` 实现可中断、带超时的多队列等待。条件闭包在每次被唤醒时重新扫描。
3. **结果写回**：将 `revents` 写回用户空间。

短超时优化：当超时小于 50ms 且就绪任务数为 0 时，`poll_wait` 回退到自旋等待（`try_short_empty_poll`），避免调度器切换开销。

### ppoll/pselect 信号掩码

`apply_temporary_sigmask()` 在等待期间临时替换进程的 sigmask，等待结束后通过 `restore_sigmask()` 恢复。这是 ppoll/pselect 与普通 poll/select 的核心区别，允许用户在等待期间原子性地解除特定信号的屏蔽。

### select 与 poll 的差异

select 使用 bitmap 传递 fd 集合，每次调用都需要从用户态拷贝三组 bitmap（read/write/exception）。poll 使用数组传递 PollFd 结构，fd 数量上限更高（4096 vs 1024）。内部实现上，select 的扫描逻辑（`scan_pselect`）与 poll 的 `scan_ppoll` 结构相似，都通过 `poll_events()` 获取就绪状态。

## epoll 实现

`os/src/fs/eventpoll.rs` 实现了 `epoll_create1()`、`epoll_ctl()` 和 `epoll_pwait()`（含 `epoll_pwait2()`）。epoll 通过事件驱动机制避免每次调用时遍历全部监视的 fd。

### 核心数据结构

**EventPoll** 是整个 epoll 实例的核心：

- `inner`（`Mutex<EventPollInner>`）：持有 `items: BTreeMap<usize, EPollItem>`（fd -> 监视项映射）和 `ready_list: VecDeque<ReadyEvent>`（就绪事件队列）。
- `wait_queue`：用于阻塞等待 epoll_wait 的调用者。
- `id`：全局递增的 epoll 实例 ID，用于事件监听器注册和嵌套检测。

**EPollItem** 存储每个被监视 fd 的状态：
- `file`: 被监视文件的 Arc 引用
- `events`: 用户注册的感兴趣事件（含 EPOLLET/EPOLLONESHOT 控制位）
- `data`: 用户传入的 data 字段（在就绪事件中原样返回）
- `enabled`: 是否启用（oneshot 投递后设为 false）
- `last_ready`: 上次观察到的就绪事件（用于 edge-triggered 新旧位检测）
- `event_queues`: 从文件收集的 EventQueueHandle 列表

**EPollScan** 是每次扫描的临时结果，包含就绪事件列表和需要等待的 wait_queue。

### epoll_create1

`sys_epoll_create1(flags)` 创建一个新的 `EventPollFile` inode（实现 `IndexNode` trait），通过 `File::new` 包装后分配到进程的 `FdTable`。`flags` 目前仅支持 `O_CLOEXEC`，任何其他标志位返回 `EINVAL`。

### epoll_ctl

`sys_epoll_ctl(epfd, op, fd, event)` 支持三种操作：

- **EPOLL_CTL_ADD**：添加 fd 到监视集合。做嵌套 epoll 检测（`check_nested_epoll`），收集文件的 event_queue，注册 `EventPoll` 自身作为 `EventListener`，然后立即执行一次 `record_observed_event` 捕获初始就绪状态。重复添加返回 `EEXIST`。
- **EPOLL_CTL_MOD**：修改已注册 fd 的感兴趣事件和 data。重置 `last_ready`，更新 event_queue 注册，重新扫描当前状态。
- **EPOLL_CTL_DEL**：移除 fd。取消注册 event_queue，从 ready_list 清理。

ADD 操作会校验文件的 `FileMode::FMODE_STREAM` 标志，非 stream 文件且不是 epoll 实例时返回 `EPERM`。嵌套 epoll 深度限制为 4 层，超限返回 `EINVAL`。

### 扫描与就绪语义

`EventPoll::scan(collect_wait)` 是 epoll 的扫描核心：

1. 调用 `reset_level_ready_list()`：对 level-triggered 的 fd，清除 `last_ready` 并从 ready_list 移除（后续重新检测）；对 edge-triggered 的 fd，保留 ready_list 条目。
2. 遍历所有 enabled 的 items，调用 `file.poll_events()` 获取当前就绪事件。
3. 对每个满足条件的 fd 调用 `record_observed_event()`。

**record_observed_event** 决定是否向 ready_list 添加条目：

- 如果返回事件为空，清除 `last_ready` 并移除 ready_list 中的对应条目。
- 对于 **edge-triggered**（EPOLLET）：只在新事件位出现时（`returned & !last_ready` 非空）才添加，避免重复唤醒。
- 对于 **level-triggered**：每次扫描都重新评估，只要从 fd 返回的就绪事件与感兴趣事件有交集，就添加到 ready_list。

**take_ready(maxevents)** 从 ready_list 队首取出最多 maxevents 个就绪事件，跳过 disabled 的条目（oneshot 禁用后不再投递）。

### 等待机制

`EventPoll::wait(maxevents, timeout)` 工作流程：

1. 扫描（`self.scan(true)` 收集 wait_queue）。
2. 尝试取 ready（`self.take_ready`），如果有就绪事件，执行 `disable_oneshot` 并返回。
3. 无就绪时，在文件 wait_queue + epoll 自身 wait_queue 上调用 `WaitQueue::wait_on_queues_interruptible_timeout`。条件闭包在每次被唤醒时重新扫描。
4. 醒来后重新扫描并取 ready。

timeout 语义：`-1` 无限等待，`0` 立即返回（非阻塞），正值表示毫秒超时。`epoll_pwait2` 使用 `TimeSpec` 结构支持纳秒精度。

### 事件回调机制

当被监视文件的 `EventWaitQueue.notify_events_all()` 被触发时（例如 socket 收到数据），它会调用所有注册的 `EventListener::on_event()`。`EventPoll` 实现了 `EventListener` trait，在 `on_event` 中调用 `record_observed_event` 记录就绪事件并 `wake_all` 唤醒等待的 epoll_wait 调用者。

## EPollEvent 事件位

`os/src/fs/vfs/event.rs` 定义了统一的 `EPollEvent` bitflags，值对齐 Linux `include/uapi/asm-generic/poll.h`。整个内核（VFS、net、设备驱动）统一使用此类型。

控制位（不传递给 `file.poll_events()`）：

| 位 | 名称 | 支持状态 |
|---|------|---------|
| `EPOLLET` (1<<31) | Edge-triggered | 支持 |
| `EPOLLONESHOT` (1<<30) | 单次触发 | 支持 |
| `EPOLLEXCLUSIVE` (1<<28) | 独占唤醒 | 不支持（返回 EINVAL） |
| `EPOLLWAKEUP` (1<<29) | 唤醒锁定 | 不支持（返回 EINVAL） |

隐含事件（始终被监视，无需在 events 中设置）：EPOLLERR、EPOLLHUP。

## EventWaitQueue 机制

`EventWaitQueue` 是连接 fd 就绪状态变化与 epoll/poll 等待者的桥梁。每个支持 poll 的 inode 维护一个 EventWaitQueue。

**EventListener trait**：`on_event(&self, key: usize, events: EPollEvent)`。EventPoll 实现此 trait，注册为文件的监听器。

**通知流程**：

```
文件状态变化（如 socket 收到数据）
  -> sock.poll_update(events) / EventWaitQueue.notify_events_all(events)
    -> notify_listeners(events)
      -> 遍历已注册的 EventListener
      -> EventPoll::on_event(fd, events)
        -> record_observed_event()
        -> wait_queue.wake_all()
```

`EventPoll::unregister_event_queues` 在 EPOLL_CTL_DEL 或 EventPoll drop 时清理监听器注册，防止悬空引用。

## poll/select 与 epoll 对比

| 维度 | poll/select | epoll |
|------|-------------|-------|
| fd 集合传递 | 每次调用从用户态拷贝所有 fd | epoll_ctl 注册，wait 只返回就绪事件 |
| 扫描开销 | O(N)，每次调用扫描全部 fd | O(1) 获取 ready_list，后台 event-driven |
| fd 数量限制 | poll: 4096, select: 1024 | 无硬限制（受内存约束） |
| 触发模式 | level-triggered 仅 | level-triggered + edge-triggered |
| oneshot 支持 | 无 | 支持 |
| 信号掩码 | ppoll/pselect 支持临时替换 | epoll_pwait 支持临时替换 |
| 嵌套使用 | 不支持监视 epoll fd | 支持（深度限制 4 层） |
| 适用场景 | 少量 fd，一次性查询 | 大量 fd，持续事件循环 |

## 已知问题

1. **EPOLLEXCLUSIVE / EPOLLWAKEUP 未实现**：Linux 高负载场景下的优化特性，当前返回 EINVAL。
2. **select 的 exception fd 集合**：`pselect_except_events` 定义为 EPOLLPRI | EPOLLRDBAND | EPOLLERR，与 Linux 严格语义存在细微差异。
3. **ET 模式下连续就绪检测**：EPOLLET 依赖 `last_ready` 的新旧位比较，若文件在两次 scan 之间维持相同就绪状态且未产生新事件位，可能丢失唤醒。当前实现认为这是 ET 语义的正确行为。
4. **多线程 epoll 竞争**：单实例多线程同时调用 epoll_wait 时，ready_list 和 disable_oneshot 的互斥访问正确，但 `scan` 过程中释放和重取锁可能导致线程间就绪事件可见性延迟。
5. **非流式文件的 epoll 支持**：EPOLL_CTL_ADD 对非 `FMODE_STREAM` 且非 epoll 实例的文件返回 EPERM。这排除了对某些特殊文件（如 procfs 动态文件）的 epoll 监视。
