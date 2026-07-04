# 网络模块改进计划

> 本文档以 DragonOS 网络栈为蓝本，梳理 Mango 内核网络模块的现状、差距、改进方案与实施路线。
> 内容整合了三次迭代的分析成果，按「现状 → 目标 → 差距分析 → 实施方案」组织。

---

## 目录

- [一、阻塞/唤醒机制深度分析与改进方案](#一阻塞唤醒机制深度分析与改进方案)
- [二、当前架构总览](#二当前架构总览)
- [三、目标架构参考（DragonOS）](#三目标架构参考dragonos)
- [四、功能差距与缺陷修复路线图](#四功能差距与缺陷修复路线图)
- [五、实施顺序](#五实施顺序)
- [六、DragonOS 参考文件速查](#六dragonos-参考文件速查)
- [七、注意事项](#七注意事项)

---

## 一、阻塞/唤醒机制深度分析与改进方案

> 这是当前 Mango 网络模块最核心的架构问题。本部分分析三层阻塞、盲唤醒、lost-wakeup、超时缺失等问题的根因，并给出分阶段改造方案。

### 3.1 三层阻塞现状

目前 `os/src/syscall/utils.rs` 有三套阻塞等待函数：

| 层级    | 函数             | 机制                                              | 问题                                                 |
| ------- | ---------------- | ------------------------------------------------- | ---------------------------------------------------- |
| Layer 1 | `wait_io_core`   | `suspend_current_and_run_next()` yield 轮询       | 忙等待，不放队列，协议栈不推进，浪费 CPU             |
| Layer 2 | `wait_io`        | 每次循环前 poll + 内部调用 `wait_io_core`         | 本质仍是 yield 循环，已被 `wait_socket_io` 取代      |
| Layer 3 | `wait_socket_io` | WaitQueue + `block_current_and_run_next()` 真阻塞 | 目前最正确，但仍有盲唤醒、无超时、lost-wakeup 等问题 |

**核心矛盾：** `wait_io_core` 注释已承认"本质是轮询，等其他地方都修好应该弃用"，但 pipe/tty 等仍在使用。

### 3.2 盲唤醒（Blind Wake-All）

每次 `NET_INTERFACE.poll()` 末尾：

```rust
dispatch_udp_packets(inner);       // UDP: 每包精准 wake_at_most(1) ✅
crate::net::wake_tcp_waiters();    // TCP: 无差别唤醒 ALL ❌
crate::net::wake_raw_waiters();    // RAW: 逐个检查 can_recv() 后唤醒 ⚠️
```

`wake_tcp_waiters()` 遍历所有 TCP socket 调用 `wake_wait_queues()`：

```rust
// os/src/net/tcp.rs
pub fn wake_wait_queues(&self) {
    self.recv_waiters.lock().wake_all();     // 即使无数据也唤醒
    self.send_waiters.lock().wake_all();     // 即使窗口未空闲也唤醒
    self.connect_waiters.lock().wake_all();  // 即使连接未完成也唤醒
    self.accept_waiters.lock().wake_all();   // 即使无新连接也唤醒
}
```

**后果：** 8 个 TCP socket 的 recv 等待者全被唤醒，但只有 1-2 个有数据——其余醒来→EAGAIN→重新睡眠→下次 poll 再被唤醒，反复震荡。

### 3.3 WaitQueue 不支持 timeout

当前 `WaitQueue` 只有 `wake_all()` 和 `wake_at_most(n)`，没有超时等待方法。`wait_socket_io` 中的 `block_current_and_run_next()` **永久阻塞**直到被显式唤醒，不支持 `SO_RCVTIMEO`/`SO_SNDTIMEO`。

### 3.4 Lost-Wakeup 竞态

```rust
// 当前代码
Err(SyscallErr::EAGAIN) => {
    // 1. 检查失败
    // 2. 加入 wait_queue（先入队）
    wq.lock().add_task(Arc::downgrade(&task));
    // 3. 阻塞
    block_current_and_run_next();
}
```

步骤 2 和 3 之间有窗口：数据恰好在 1 失败后、2 入队前到达 → `wake_at_most(1)` 时队列为空 → 任务入队后永久阻塞。

**正确做法：先入队 waker，再检查条件。**

### 3.5 DragonOS 精准条件唤醒方案

**唤醒流程：**

```
Iface::poll()
  ├─ smoltcp 收发包 → SocketSet 状态变化
  └─ 遍历 iface 绑定的所有 InetSocket:
      sock.notify()
        ├─ check_io_event(): can_recv→EPOLLIN / can_send→EPOLLOUT / Closed→EPOLLHUP
        └─ wait_queue.wakeup(ProcessState::Blocked)
```

**关键差异对比：**

| 维度        | Mango               | DragonOS                           |
| ----------- | ------------------- | ---------------------------------- |
| 唤醒范围    | 全局所有 TCP socket | 仅当前 iface 绑定的 socket         |
| 唤醒条件    | 无条件 wake_all()   | 先 check_io_event() 再决定         |
| 唤醒粒度    | wake_all()          | wakeup()（按进程状态过滤）         |
| 超时        | 不支持              | wait_until_timeout() 完整支持      |
| Lost wakeup | 无保护              | Waker 状态机 + 先入队后检查        |
| 信号中断    | 基本支持            | 完整支持（Interruptible/Killable） |

**Waker 状态机：**

```
idle → prepare_sleep() → Sleeping ──wake()──► Notified ──consume_notification()──► idle
                              │                                          ▲
                              └──timeout/cancel──► Closed ────────────────┘
```

核心：每次检查条件**之前**将 waker 入队，条件满足则出队返回，不满足则睡眠。唤醒者调用 `wake()` 将状态从 Sleeping → Notified。

### 3.6 分阶段改造方案

#### Phase A：TCP 盲唤醒 → 条件唤醒（P0，1-3 天）

**改动要点：**
1. 删除全局 `wake_tcp_waiters()`，改为在每个 TCP socket 的 `wake_wait_queues()` 内部先检查 `can_recv()`/`can_send()` 再决定是否 `wake_at_most(1)`
2. `_poll()` 末尾遍历 TCP_SOCKETS 逐个调用条件唤醒
3. RAW socket 的条件唤醒已实现，保持不变

**预期效果：** 消除"全部唤醒、多数重新睡眠"的震荡；只有有数据可读的 socket 才唤醒 recv 等待者。

#### Phase B：WaitQueue 增加 timeout 支持（P0，1-2 天）

**改动要点：**
1. `WaitQueue` 增加 `wait_timeout(timeout: TimeSpec) -> bool` 方法
2. `wait_socket_io` 增加 `timeout: Option<TimeSpec>` 参数
3. syscall 层从 socket 读取 `SO_RCVTIMEO`/`SO_SNDTIMEO` 传入

**依赖：** Mango 已有 `TimeoutWaitQueue` 和 `wait_with_timeout()` 基础设施，需整合。

#### Phase C：Waker 状态机 + Lost-Wakeup 保护（P2，3-5 天）

**改动要点：**
1. 引入 `Waker` 结构体（Idle/Sleeping/Notified/Closed 状态机）
2. `WaitQueue::wait_until_impl()` 实现"先入队 waker，再检查条件"的顺序
3. 参考 DragonOS `kernel/src/libs/wait_queue.rs`（~1027 行）

#### Phase D：统一阻塞接口，废弃 wait_io_core（P1，1-2 天）

**改动要点：**
1. pipe/tty 添加 `WaitQueue` 字段，写入端关闭/新数据到达时 `wake_all()`
2. 废弃 `wait_io_core` 和 `wait_io`（加 `#[deprecated]`）
3. 所有 I/O syscall 统一使用 `wait_socket_io`（重命名为 `wait_io`）

#### Phase E：poll_until_quiescent（P2）

**现状问题：** `NET_INTERFACE.poll()` 只调用一次 smoltcp poll，TCP 握手、loopback、shutdown 等需要多轮。

**DragonOS 方案：** 循环 `iface.poll()` 最多 128 轮直到返回 false，yield 避免饿死。

**短期（P0）：** 在 `connect`/`accept`/`shutdown` 路径手动循环 8 次 poll。

---

## 二、当前架构总览

### 1.1 模块结构

```
os/src/net/
├── mod.rs          # Socket trait、SocketTable (BTreeMap<Fd, Arc<dyn Socket>>)、alloc()
├── config.rs       # NET_INTERFACE 单例（smoltcp Interface + SocketSet）、poll()、init()
├── adapter.rs      # RoutingDevice (eth + loopback 路由)、SmoltcpDeviceAdapter
├── tcp.rs          # TcpSocket (VecDeque<SocketHandle> backlog)
├── udp.rs          # UdpSocket (rx_queue: VecDeque 分发)
├── raw.rs          # RawSocket (手动 IP 头封装)
├── unix.rs         # UnixSocket（基于管道）
├── address.rs      # SocketAddrv4、endpoint 解析
└── macros.rs       # impl_file_for_socket! 宏
```

### 1.2 核心特征

| 特性               | 状态                                                                                  |
| ------------------ | ------------------------------------------------------------------------------------- |
| Socket 抽象        | 单一 `Socket` trait（继承 `File`），方法较少                                          |
| TCP 状态机         | 简单：listener 用 `VecDeque<SocketHandle>` 做 backlog                                 |
| UDP 分发           | `dispatch_udp_packets()` → `UdpSocket.rx_queue`                                       |
| 阻塞 I/O           | 三层：`wait_io_core`（忙轮询）、`wait_io`（poll+轮询）、`wait_socket_io`（WaitQueue） |
| Poll               | 集中式 `NET_INTERFACE.poll()`，每次 syscall 前调用                                    |
| 端口管理           | 无，仅 `SocketTable::can_bind()` 做冲突检查                                           |
| epoll              | 不支持                                                                                |
| socket 选项        | 极有限（SO_REUSEADDR、SO_SNDBUF、SO_RCVBUF 等少量）                                   |
| shutdown           | 仅 SHUT_WR（TCP），UDP 无                                                             |
| recvmsg/sendmsg    | 不支持                                                                                |
| MSG_PEEK           | 不支持                                                                                |
| connect/disconnect | UDP connect 后不可 disconnect                                                         |
| 多网卡             | 单网卡（RoutingDevice 仅 eth+lo）                                                     |
| 网络命名空间       | 无                                                                                    |

---

## 三、目标架构参考（DragonOS）

### 2.1 模块结构

```
kernel/src/net/
├── mod.rs              # generate_iface_id()
├── net_core.rs         # net_init()、DHCP
├── posix.rs            # PMSG、PSOL、SockAddr、MsgHdr
├── neighbor.rs         # ARP 邻居表
├── routing/            # 路由表、NAT
├── tcp_close_defer.rs  # TCP 延迟关闭
├── tcp_listener_backlog.rs  # backlog 管理
├── syscall/            # 每个 syscall 单独文件
└── socket/
    ├── base.rs         # Socket trait（完整定义）
    ├── endpoint.rs     # Endpoint 枚举
    ├── family.rs       # AddressFamily
    ├── inode.rs        # SocketInode
    ├── common/         # EPollItems、ShutdownBit
    ├── inet/
    │   ├── common/     # BoundInner、port 管理
    │   ├── stream/     # TCP
    │   │   ├── inner.rs       # Inner 状态机 (Init/Connecting/Listening/Established/SelfConnected/Closed)
    │   │   ├── stream_core.rs # TcpSocket 结构、选项
    │   │   ├── io.rs          # recv/send 实现（支持 PEEK、TRUNC、WAITALL）
    │   │   ├── lifecycle.rs   # bind/listen/connect/accept/shutdown/close
    │   │   ├── shutdown.rs    # SHUT_RD 半关闭
    │   │   ├── option.rs      # TCP socket 选项
    │   │   ├── poll_util.rs   # poll_iface_until_quiescent
    │   │   ├── info.rs        # TCP_INFO
    │   │   └── events.rs      # epoll 事件更新
    │   ├── datagram/   # UDP
    │   │   ├── inner.rs       # UnboundUdp / BoundUdp
    │   │   ├── mod.rs         # UdpSocket（~1994行）、recvmsg/sendmsg、CMSG
    │   │   ├── option.rs      # UDP socket 选项
    │   │   └── udp_bindings.rs # UDP 端口绑定管理
    │   ├── raw/        # Raw socket
    │   └── syscall.rs  # INET 通用 syscall
    ├── unix/           # Unix domain socket
    ├── packet/         # Packet socket (AF_PACKET)
    └── netlink/        # Netlink socket
```

### 2.2 核心特征

| 特性                           | 状态                                                                                              |
| ------------------------------ | ------------------------------------------------------------------------------------------------- |
| Socket 抽象                    | 完整 `Socket` trait + `PollableInode`，方法覆盖 POSIX 全套                                        |
| TCP 状态机                     | 六态枚举 `Inner`，状态转换严格，支持 SelfConnected                                                |
| UDP 分发                       | 直接使用 smoltcp socket（不经过 rx_queue 中转），支持 connected 模式过滤                          |
| 阻塞 I/O                       | `WaitQueue::wait_event_io_interruptible_timeout()` 支持超时+信号中断                              |
| Poll                           | `poll_iface_until_quiescent()` 批量轮询直到静止，支持信号检查                                     |
| 端口管理                       | `PortManager`：端口绑定/释放、ephemeral port 分配、SO_REUSEADDR/SO_REUSEPORT                      |
| epoll                          | 完整支持（EPollEventType、pollee 原子变量、事件通知）                                             |
| socket 选项                    | 全面（TCP_NODELAY、TCP_CORK、TCP_QUICKACK、SO_KEEPALIVE、SO_LINGER、SO_RCVTIMEO、SO_SNDTIMEO 等） |
| shutdown                       | 完整 SHUT_RD/SHUT_WR/SHUT_RDWR（TCP 和 UDP 均支持）                                               |
| recvmsg/sendmsg                | 完整支持（iovec + cmsg + MSG_ERRQUEUE）                                                           |
| MSG_PEEK/MSG_TRUNC/MSG_WAITALL | 支持                                                                                              |
| connect/disconnect             | UDP 支持 AF_UNSPEC disconnect                                                                     |
| 多网卡                         | 多接口 + 路由表                                                                                   |
| 网络命名空间                   | NetNamespace 隔离                                                                                 |

---

## 四、功能差距与缺陷修复路线图

### 优先级说明

- 🔴 **P0**：LTP 测例阻塞项，必须立即实现
- 🟠 **P1**：LTP 测例会涉及，高优先级
- 🟡 **P2**：完善网络栈，提升兼容性
- 🟢 **P3**：锦上添花，后续迭代

---

### 4.1 P0 — LTP 阻塞项（必须立即实现）

#### P0-1: recvmsg / sendmsg 系统调用

**现状：** Mango 完全没有 `recvmsg` 和 `sendmsg`。LTP 的 `recvmsg01`、`sendmsg01` 等无法运行。

**实现要点：**
- 定义 `MsgHdr` 和 `IoVec` 结构体（`os/src/net/posix.rs`）
- `Socket` trait 增加 `recv_msg()` / `send_msg()` 方法，默认返回 ENOSYS
- syscall 入口 `sys_recvmsg(212)` / `sys_sendmsg(211)`：读取 MsgHdr → 校验 flags → 读取 iovec → 调用 socket 方法 → 写回 msg_name/msg_flags/msg_controllen
- TCP：recv_msg 调用 `try_recv()`，忽略 msg_name；send_msg 调用 `try_send()`
- UDP：recv_msg 返回 `(data, src_addr, orig_len)`；send_msg 解析 msg_name 为目标地址
- cmsg 暂不实现，设为空即可
- 涉及文件：`syscall/net.rs`、`syscall/syscall_id.rs`、`syscall/mod.rs`、`net/mod.rs`、`net/tcp.rs`、`net/udp.rs`

#### P0-2: MSG_PEEK 支持

**现状：** `try_recv` 直接消费数据，无 peek 能力。

**实现要点：**
- 修改 `Socket::try_recv` 签名：`fn try_recv(&self, buf: &mut [u8], flags: MsgFlags) -> Result<isize, SyscallErr>`
- UDP：`can_recv()` 后用 `peek()` 或 `recv()` 取决于 MSG_PEEK
- TCP：smoltcp TCP socket 有 peek 相关 API
- MSG_PEEK 时不消费数据，下次 recv 应返回相同数据

#### P0-3: MSG_WAITALL 支持（TCP）

**现状：** `TcpSocket::try_recv` 每次只读一段。

**实现要点：**
- 在 `sys_recvfrom` 的 `wait_socket_io` 闭包内做循环
- 只在 `flags.contains(MSG_WAITALL)` 且 socket 是 SOCK_STREAM 时才循环
- 逻辑：累加读取直到 `total == len` 或 `n == 0`（EOF）或 `n == EAGAIN && total > 0`（返回部分）

#### P0-4: MSG_TRUNC 支持

**现状：** UDP 数据报被截断时不通知用户。

**实现要点：**
- 修改 `UdpSocket::try_recv` 返回 `Result<(isize, usize), SyscallErr>`（第二个值为原始 datagram 长度）
- `recvfrom` 当 MSG_TRUNC 时返回原始 datagram 长度
- `recvmsg` 在 `msg_flags` 中设置 `MSG_TRUNC`

#### P0-5: MSG_DONTWAIT / MSG_NOSIGNAL / MSG_MORE

**现状：** `MsgFlags::validate_for_send` 只做基本校验，DONTWAIT 未传递到阻塞逻辑。

**实现要点：**
- `validate_for_send` 返回 `(dontwait, nosignal, more)` 三元组
- syscall 层将 `dontwait` 与 fd 的 `O_NONBLOCK` 做 OR 合并
- MSG_MORE 在 TcpSocket 记录 `pending_more: AtomicBool`（完整实现归入 P2 TCP_CORK）

#### P0-6: dup() 在 TCP/UDP/Unix socket 上 panic（代码缺陷修复）

**现状：** `deep_clone_socket()` 在 TCP/UDP/Unix 中都是 `todo!()`，`dup()`/`dup2()` 直接 panic。

**修复：** 为每种 socket 实现 `deep_clone_socket()`，clone 内部字段。

#### P0-7: UDP sendto 使用 wait_io + 每次 bind/connect（代码缺陷修复）

**现状：** UDP sendto 用 `wait_io`（busy-yield）而非 `wait_socket_io`，且每次 sendto 都重新 bind+connect。

**修复：**
1. UDP send 路径改为 `wait_socket_io`
2. 只在首次 sendto 时 auto-bind（本地端口=0时），后续复用
3. 用 `send_to()` 替代 `connect() + try_send()`（UDP 无连接语义）

---

### 4.2 P1 — LTP 高频涉及项

#### P1-1: shutdown 完善（SHUT_RD / SHUT_RDWR）

**现状：** 只有 SHUT_WR（TCP 调 `socket.close()`，UDP 空操作）。

**实现要点：**
- 定义 `ShutdownBit` bitflags（SHUT_RD=1, SHUT_WR=2, SHUT_RDWR=3）
- TCP SHUT_RD：丢弃接收队列，后续 recv 返回 0 (EOF)；SHUT_WR：发送 FIN
- UDP shutdown：记录 bits + 唤醒等待者
- `try_recv` 中检查 SHUT_RD 标记，返回 EOF

#### P1-2: socket 选项完善

| 选项                    | 现状 | 实现要点                                                              |
| ----------------------- | ---- | --------------------------------------------------------------------- |
| SO_ERROR                | ❌    | `so_error: AtomicI32`，connect 失败/出错时设置，getsockopt 返回并清零 |
| SO_TYPE                 | ❌    | 返回 `socket_type().bits() & SOCK_TYPE_MASK`                          |
| SO_DOMAIN               | ❌    | 返回地址族（AF_INET/AF_UNIX）                                         |
| SO_PROTOCOL             | ❌    | 返回协议类型（IPPROTO_TCP/UDP）                                       |
| SO_ACCEPTCONN           | ❌    | listener 返回 1，否则 0                                               |
| SO_LINGER               | ❌    | `struct linger { l_onoff, l_linger }`，close 时检查                   |
| SO_RCVTIMEO/SO_SNDTIMEO | ❌    | 依赖 Phase B（WaitQueue timeout）                                     |
| SO_REUSEADDR            | ✅    | —                                                                     |
| SO_SNDBUF/SO_RCVBUF     | ✅    | —                                                                     |
| SO_KEEPALIVE            | ✅    | —                                                                     |
| TCP_NODELAY             | ✅    | —                                                                     |
| TCP_MAXSEG              | ✅    | —                                                                     |
| TCP_INFO                | ✅    | —                                                                     |
| SO_OOBINLINE            | ❌    | 简单，返回 0 即可                                                     |
| SO_BROADCAST            | ❌    | 简单，允许设置即可                                                    |
| SO_REUSEPORT            | ❌    | 中等，需 PortManager 配合                                             |
| TCP_QUICKACK            | ❌    | 简单，仅记录标志                                                      |

#### P1-3: SO_RCVTIMEO / SO_SNDTIMEO

**实现前提：** Phase B（WaitQueue timeout）完成。

**实现要点：** socket 存储 `recv_timeout`/`send_timeout` 字段，`wait_socket_io` 中传入超时参数。

#### P1-4: UDP disconnect (connect AF_UNSPEC)

**现状：** `UdpSocket::connect` 设置 remote 后无法清除。

**实现要点：** `connect(fd, {AF_UNSPEC, 0})` → 清除 `remote_endpoint`；`connect(fd, {AF_INET, addr, 0})` 同理。

#### P1-5: getsockname / getpeername 完善

**现状：** 依赖 smoltcp 直接读取，TIME_WAIT/CLOSED 后失效。

**实现要点：** 在 `TcpSocketInner` 中缓存 `cached_peer: Option<IpEndpoint>`，连接建立时保存，getpeername 优先返回缓存。

#### P1-6: Pipe O_NONBLOCK 检查 + WaitQueue 化（代码缺陷修复）

**现状：** pipe 阻塞不检查 `O_NONBLOCK`，信号检查被注释掉，使用 busy-wait 而非 WaitQueue。

**修复：** 检查 `self.nonblock` 返回 EAGAIN；添加 `WaitQueue` 到 Pipe；恢复信号检查。pipe 是 UnixSocket 的底层传输，影响所有 Unix domain socket 操作。

#### P1-7: ppoll 使用 WaitQueue 替代 busy-yield（代码缺陷修复）

**现状：** `ppoll`/`pselect6` 使用 `suspend_current_and_run_next()` 忙等待，不调用 `NET_INTERFACE.poll()`。

**修复：** 集成 WaitQueue 实现事件驱动等待；每次循环前统一调用 `NET_INTERFACE.poll()`。

#### P1-8: RawSocket / UnixSocket todo!() 补全（代码缺陷修复）

**RawSocket** 有 12 个 `todo!()`（bind、listen、connect、accept 等），任何调用都会 panic。至少实现返回 ENOTSUP 的桩函数。

**UnixSocket** 所有 `File` trait 方法都是 `todo!()`，没有 `recv_wait_queue()`/`send_wait_queue()`。至少实现返回 ENOSYS 的桩函数。

#### P1-9: sendfile to socket 修复（代码缺陷修复）

**现状：** `sys_sendfile` 在 socket 返回 EAGAIN 时误判为写入 0 字节而 break。

**修复：** 识别 write 返回值中的 EAGAIN 编码，正确重试。

---

### 4.3 P2 — 架构改进

#### P2-1: TCP 状态机重构

**现状：** 一个 `is_listener: AtomicBool` + `VecDeque<SocketHandle>` 管理所有状态，方法中充满 `if is_listener` 判断。

**分步实施：**
- **Step 1（P0 内）：** 提取 `ensure_not_listener()`/`ensure_listener()` 辅助方法，减少重复判断
- **Step 2（P2）：** 引入 `TcpInner` 枚举：`Init | Connecting | Listening | Established | SelfConnected | Closed`，每个状态有专有方法
- **Step 3（P2 后期）：** 类型安全的状态转换，参考 DragonOS `Init::connect() -> Result<Connecting, ...>` 模式

**参考：** DragonOS `stream/inner.rs`（~1243 行）、`stream/stream_core.rs`

#### P2-2: UDP 去掉 rx_queue 中间层

**现状：** 数据拷贝两次：smoltcp buf → `UdpSocket.rx_queue` (VecDeque) → 用户 buf。

**改动要点：**
- 删除 `rx_queue`，`try_recv` 直接从 smoltcp socket 读取
- Connected 模式：peek 检查源地址，匹配则 recv，不匹配则丢弃继续
- 删除或简化 `dispatch_udp_packets`（只保留唤醒功能）

#### P2-3: 端口管理（PortManager）

**现状：** `SocketTable::can_bind()` O(n) 遍历检查冲突。

**改动要点：**
- `PortManager`：`BTreeMap<u16, Vec<PortBinding>>` 管理 TCP/UDP 端口
- 功能：`bind_port()`、`bind_ephemeral_port()`（49152-65535）、`unbind_port()`、冲突检测
- 每个 Iface 有自己的 PortManager

#### P2-4: TCP Self-Connect 支持

**场景：** `connect(127.0.0.1:自身绑定端口)` 内部回环。

**实现：** 检测 `local == remote` 时用内部 `VecDeque<u8>` 回环，不经过 smoltcp socket。

**参考：** DragonOS `stream/inner.rs` 的 `SelfConnected`

#### P2-5: epoll 支持

**核心：** `epoll_create1` → `epoll_ctl(ADD/MOD/DEL)` → `epoll_wait`（阻塞等待事件）。

**参考：** DragonOS `stream/events.rs`、`filesystem/epoll/`

#### P2-6: TCP_CORK / MSG_MORE

**实现：** `TcpSocketInner` 添加 `cork: AtomicBool` + `cork_buf`，cork 启用时暂存数据，禁用时 flush。

#### P2-7: 其他架构隐患修复

| 缺陷                           | 描述                                                                                       | 修复                                   |
| ------------------------------ | ------------------------------------------------------------------------------------------ | -------------------------------------- |
| 锁顺序不一致                   | `sys_sendto` 先 socket_table 再 files；`sys_accept` 相反                                   | 统一锁顺序为"先 files 后 socket_table" |
| 全局 Weak 条目累积             | `TCP_SOCKETS`/`UDP_SOCKETS`/`RAW_SOCKETS` 的死 Weak 不清理                                 | `Drop` 中主动从全局表移除              |
| NET_INTERFACE 锁持有时间长     | `_poll()` 的 `inner_handler` 闭包内调用 `dispatch_udp_packets`（可能获取 TASK_MANAGER 锁） | 缩短闭包范围，避免锁内再取其他锁       |
| get_socket! 锁粒度过粗         | 每次获取 socket 引用都要取锁                                                               | 使用 `Arc::clone` 尽早释放锁           |
| Socket::alloc() 不处理 AF_UNIX | `AF_UNIX => todo!()`                                                                       | 实现或返回 ENOTSUP                     |

---

### 4.4 P3 — 长期完善

| 项目                      | 说明                                                     |
| ------------------------- | -------------------------------------------------------- |
| 多网卡 / 路由表           | 多接口 + `routing/` 模块 + NAT。比赛内核场景下单网卡足够 |
| 网络命名空间              | `NetNamespace` 隔离网络栈                                |
| Netlink socket            | `socket/netlink/` 完整实现                               |
| Packet socket (AF_PACKET) | 底层包收发，若 LTP 需要则提级                            |
| IPv6 支持                 | 当前仅 IPv4（硬编码 `LOCAL_IP = 10.0.2.15`）             |
| TCP 拥塞控制算法选择      | 当前使用 smoltcp 默认                                    |

---

## 五、实施顺序

```
Phase 0 (阻塞/唤醒紧急修复 + 严重缺陷): 3-7 天
├── Phase A: TCP 盲唤醒 → 条件唤醒 (1-3天) ⚡ 无依赖，收益最大
├── Phase B: WaitQueue timeout 支持 (1-2天) → 解锁 SO_RCVTIMEO
├── P0-6: dup() panic 修复 (TCP/UDP/Unix)
├── P0-7: UDP sendto 改用 wait_socket_io + 去掉每次 bind/connect
└── P1-6: Pipe O_NONBLOCK 检查 + WaitQueue 化

Phase 1 (LTP 阻塞功能): 1-2 周
├── P0-1: recvmsg / sendmsg
├── P0-2: MSG_PEEK (依赖 try_recv 签名修改)
├── P0-3: MSG_WAITALL (TCP)
├── P0-4: MSG_TRUNC
├── P0-5: MSG_DONTWAIT / MSG_NOSIGNAL / MSG_MORE
├── P1-7: ppoll WaitQueue 化
├── P1-8: RawSocket / UnixSocket todo!() 补全
├── P1-9: sendfile to socket 修复
└── Phase D: 统一阻塞接口，废弃 wait_io_core

Phase 2 (LTP 高频项): 1-2 周
├── P1-1: shutdown SHUT_RD 支持
├── P1-2: socket 选项 (SO_ERROR, SO_TYPE, SO_LINGER 等)
├── P1-3: SO_RCVTIMEO / SO_SNDTIMEO (依赖 Phase B)
├── P1-4: UDP disconnect
├── P1-5: getsockname / getpeername 完善
└── P2 TCP 状态机 Step 1 (代码整理)

Phase 3 (架构改进): 2-4 周
├── Phase E: poll_until_quiescent
├── P2-1: TCP 状态机重构 (Step 2-3)
├── P2-2: UDP 去掉 rx_queue 中间层
├── P2-3: 端口管理 PortManager
├── Phase C: Waker 状态机 + Lost-Wakeup 保护
├── P2-4: TCP Self-Connect
├── P2-5: epoll 支持
├── P2-6: TCP_CORK / MSG_MORE
├── P2-7: 架构隐患修复 (锁顺序、Weak 累积等)

Phase 4 (长期完善): 时间待定
└── 多网卡、命名空间、IPv6、Netlink、AF_PACKET 等
```

**依赖关系：**
- Phase A 无外部依赖，可立即开始
- Phase B 无外部依赖
- P1-3 依赖 Phase B
- P0-2 (MSG_PEEK) 需要修改 `try_recv` 签名，影响 TCP/UDP/Raw 三个实现
- P2-2 (UDP 去 rx_queue) 依赖 P0-2 (需要 peeking 做 connected 过滤)
- Phase C 建议在 Phase A/B 稳定后再做

---

## 六、DragonOS 参考文件速查

| 需求                           | 文件                                   | 行数参考 |
| ------------------------------ | -------------------------------------- | -------- |
| WaitQueue + Waker 状态机       | `libs/wait_queue.rs`                   | ~1027    |
| Poll 工具                      | `socket/inet/stream/poll_util.rs`      | ~44      |
| 网卡 poll + bind_socket/notify | `driver/net/mod.rs`                    | ~681     |
| Socket trait 完整定义          | `socket/base.rs`                       | ~243     |
| TCP 状态机（Inner 枚举）       | `socket/inet/stream/inner.rs`          | ~1243    |
| TCP 结构 + 选项                | `socket/inet/stream/stream_core.rs`    | ~511     |
| TCP 数据收发                   | `socket/inet/stream/io.rs`             | ~839     |
| TCP 生命周期                   | `socket/inet/stream/lifecycle.rs`      | ~774     |
| TCP shutdown                   | `socket/inet/stream/shutdown.rs`       | —        |
| TCP 选项                       | `socket/inet/stream/option.rs`         | —        |
| UDP 完整实现                   | `socket/inet/datagram/mod.rs`          | ~1994    |
| UDP 内部                       | `socket/inet/datagram/inner.rs`        | ~692     |
| UDP 端口绑定                   | `socket/inet/datagram/udp_bindings.rs` | —        |
| POSIX 类型 (MsgHdr/SockAddr)   | `posix.rs`                             | —        |
| 通用绑定/端口管理              | `socket/inet/common/`                  | —        |

---

## 七、注意事项

1. **双架构兼容**：所有改动必须同时支持 rv64 和 la64 编译通过。
2. **smoltcp 版本**：Mango 使用的 smoltcp 为 vendor 目录下的版本，API 可能与 DragonOS 不同，封装时需适配。
3. **WaitQueue 差异**：DragonOS 的 WaitQueue 支持 `wait_event_io_interruptible_timeout`，Mango 的 WaitQueue 可能需要增强。
4. **no_std 限制**：Mango 是 `#![no_std]` 裸机内核，DragonOS 某些依赖（如完整 std）不能直接照搬。
5. **QEMU 验证**：每个 Phase 完成后必须在 QEMU 中运行 LTP 网络相关测试组验证。
6. **锁顺序统一**：始终遵循"先 files 锁，后 socket_table 锁"的顺序，避免死锁隐患。