---
title: "TCP 协议实现"
category: net
status: current
owner: MangoCore Team
last_updated: 2026-08-04
tags: [net, tcp, smoltcp, state-machine]
---

# TCP 协议实现

## 概述

TCP 子系统基于 smoltcp 协议栈实现，使用 6 状态 `Inner` 枚举管理 TCP 状态机。各状态变体封装自己的数据（smoltcp handle、缓存 endpoint、连接结果等），通过 `Inner` 枚举统一 match 分发操作。

设计对标 DragonOS `net/socket/inet/stream/` 架构，兼顾 Linux TCP 语义兼容性。

TCP 的 N1 lifecycle state 只能向下进入目标 N2 DeviceStack；N2 中完成 smoltcp 操作和 route/binding 重验后才更新 pollee 或唤醒 EventWaitQueue/epoll。接收数据先进入内核所有 buffer，释放 socket/DeviceStack 后再 copyout；等待条件只检查 readiness 并 kick poll worker，不在 WaitQueue 闭包内 poll。该协议未使同一 SocketSet 的多 socket 数据路径并行。

## 源文件地图

| 文件 | 职责 |
|------|------|
| `stream/mod.rs` | TcpSocket 结构体定义、Socket trait 实现、注册/唤醒辅助 |
| `stream/inner.rs` | Inner 6 状态枚举及各变体 struct（Init/Connecting/Listening/Established/SelfConnected/Closed） |
| `stream/lifecycle.rs` | bind / connect / listen / accept / shutdown 生命周期操作 |
| `stream/io.rs` | try_send / try_recv / recv_to_user / try_recv_peek I/O 操作 |
| `stream/events.rs` | Inner::update_io_events 统一分发 |
| `stream/tcp_info.rs` | Linux struct tcp_info 定义（用于 getsockopt TCP_INFO） |

## TcpSocket 结构体

定义在 `stream/mod.rs:44`，提供 TCP socket 的完整 Socket trait 实现：

```rust
pub struct TcpSocket {
    pub inner: Mutex<Inner>,                    // TCP 状态机
    pub pollee: AtomicUsize,                    // 缓存 EPOLL 事件掩码
    pub read_shutdown: AtomicBool,              // 读半关闭标记
    pub write_shutdown: AtomicBool,             // 写半关闭标记
    pub reuse_addr: AtomicBool,                 // SO_REUSEADDR
    multicast_group_joined: AtomicBool,          // 多播组加入标记
    pub bound: Mutex<BoundInner>,               // 绑定元数据（handle, ifindex, addr, port）
    pub bound_ifindex: Mutex<Option<u32>>,       // SO_BINDTODEVICE 接口索引
    pub recv_waiters: EventWaitQueue,           // 读等待队列
    pub send_waiters: EventWaitQueue,           // 写等待队列
    pub connect_waiters: EventWaitQueue,         // 连接等待队列
    pub accept_waiters: EventWaitQueue,          // 接受等待队列
    pub ip_version: IpVersion,                  // IPv4 / IPv6
}
```

关键字段说明：

- **inner**: 状态机枚举，所有状态转换和操作由此分发
- **pollee**: 缓存 epoll 事件位的原子变量。`update_io_events()` 根据 smoltcp socket 真实状态刷新此值，`socket_r_ready()`/`socket_w_ready()` 直接读取
- **read_shutdown / write_shutdown**: 记录 `shutdown(SHUT_RD/SHUT_WR)` 后的半关闭状态
- **bound**: 绑定元数据，记录 RouteSocketHandle、ifindex、地址端口，用于端口释放和事件回调
- **recv/send/connect/accept waiters**: 四种 EventWaitQueue，分别对应不同阻塞语义的唤醒

## Inner 六状态枚举

定义在 `stream/inner.rs:805`：

```rust
pub enum Inner {
    Init(Init),
    Connecting(Connecting),
    Listening(Listening),
    Established(Established),
    SelfConnected(SelfConnected),
    Closed(Closed),
}
```

其中各状态的中文含义：Init 表示初始状态，Connecting 表示连接中，Listening 表示监听中，Established 表示已建立连接，SelfConnected 表示自连接（连接自身的地址端口对），Closed 表示已关闭。

### 状态图

```
                    ┌──────────────────────────────────────┐
                    │                                      │
                    v                                      │
    Init::Unbound ──> Init::Bound                          │
         |               |                                 │
         |   bind()      |  connect() / listen()           │
         v               v                                 │
    ┌────┴───────────────┴───────────┐                     │
    │                                │                     │
    v                                v                     │
 Connecting                     Listening                  │
    |                                |                     │
    |  SYN_SENT -> Established       |  accept()           │
    v                                v                     │
 Established ────────────────────────                      │
    |                                                      │
    |  self-connect (same addr:port)                      │
    v                                                      │
 SelfConnected                                             │
    |                                                      │
    |  close() / shutdown()                               │
    v                                                      │
 Closed ──────────────────────────────────────────────────┘
```

### 变体说明

**Init** (Unbound / Bound):
- `Unbound(Box<tcp::Socket>, IpVersion)` — socket 已创建但未加入 smoltcp SocketSet，未分配本地端口
- `Bound { socket, local, pending_error }` — 已通过 bind() 绑定本地 endpoint，socket 仍然持有但尚未添加到 NET_INTERFACE（延迟绑定）

**Connecting**（连接中）: 正在建立 TCP 连接（SYN_SENT / SYN_RCVD）。包含 `handle: RouteSocketHandle`、`local: IpEndpoint`、`remote: IpEndpoint`、`result: Mutex<ConnectResult>`、`was_established: AtomicBool`。

**Listening**（监听中）: 监听状态。包含 `handles: Vec<RouteSocketHandle>`（多个 listen socket 实现 backlog）、`connect: AtomicUsize`、`listen_addr: IpListenEndpoint`。

**Established**（已连接）: 已建立连接。包含 `handle: RouteSocketHandle`、`local: IpEndpoint`、`peer: IpEndpoint`。

**SelfConnected**（自连接）: Linux 兼容的自连接（connect 到自身相同的 addr:port）。数据走内部 `VecDeque<u8>` 回环，不经过网络栈。

**Closed**（已关闭）: 已关闭，不再持有任何 smoltcp handle。仅保留 `ver: IpVersion` 用于构造全零 endpoint。

## 生命周期转换

生命周期方法定义在 `stream/lifecycle.rs`（`impl Inner`）：

| 方法 | 源文件 | 输入状态 | 输出状态 | 说明 |
|------|--------|----------|----------|------|
| `bind()` | lifecycle.rs:28 | Init::Unbound | Init::Bound | 绑定本地 endpoint，分配端口 |
| `connect()` | lifecycle.rs:56 | Init (Unbound/Bound) | Connecting | 路由查找加 SYN 发送；Unbound 先分配临时端口 |
| `listen()` | lifecycle.rs:142 | Init (Unbound/Bound) | Listening | 创建 backlog 数量的 listen socket |
| `accept()` | lifecycle.rs:257 | Listening | (Established, peer_endpoint) | 从 listen handles 中取出已连接 socket |
| `shutdown()` | lifecycle.rs:276 | Established / SelfConnected | 不变 | 半关闭或全关闭 |

Socket trait 方法实现于 `stream/mod.rs:181`：

```rust
impl Socket for TcpSocket {
    fn bind()      // mod.rs:182 — 组合 bound_inner 记录 + Inner::bind()
    fn listen()    // mod.rs:227 — 组合 bound 记录 + Inner::listen()
    fn connect()   // mod.rs:253 — 组合 bound 记录 + Inner::connect() + 首次 poll
    fn accept()    // mod.rs:367 — 组合 Inner::accept() + SocketFile 创建
    fn shutdown()  // mod.rs:494 — 组合 read_shutdown/write_shutdown 标记
}
```

### 延迟绑定

`bind()` 仅将 socket 状态置为 `Init::Bound` 并记录本地地址端口，**不**将 smoltcp socket 加入 `NET_INTERFACE` 的 SocketSet。实际的 `add_routed_socket_on()` 延迟到 `connect()` 或 `listen()` 时才触发。

```rust
// lifecycle.rs:29 (bind)
Init::Unbound(socket, ver) => {
    // ...allocate port, construct local endpoint...
    Ok(Inner::Init(Init::Bound { socket, local, pending_error: None }))
}
```

connect 时（lifecycle.rs:98）：

```rust
let handle = NET_INTERFACE
    .add_routed_socket_on(InetProtocol::Tcp, socket, ifindex)
    .ok_or_else(|| { /* create new socket for Bound recovery */ })?;
```

listen 时（lifecycle.rs:188）：

```rust
let handle = NET_INTERFACE
    .add_routed_socket_on(InetProtocol::Tcp, socket, listen_ifindex)
    .ok_or_else(|| { /* create new socket for Bound recovery */ })?;
```

操作失败时通过创建全新的 smoltcp socket 和 `Init::Bound` 恢复原状态，保证状态机一致性。

## I/O 路径

I/O 方法定义在 `stream/io.rs`（`impl Inner`）：

### try_send

```rust
pub fn try_send(&self, buf: &[u8]) -> Result<isize, SyscallErr> {
    match self {
        Inner::Established(e) => e.send_slice(buf).map(|n| n as isize),
        Inner::SelfConnected(sc) => sc.send_slice(buf).map(|n| n as isize),
        Inner::Init(_) | Inner::Closed(_) => Err(SyscallErr::EPIPE),
        Inner::Connecting(_) => Err(SyscallErr::EAGAIN),
        Inner::Listening(_) => Err(SyscallErr::EINVAL),
    }
}
```

Established 状态下通过 `with_tcp_mut` 模式访问 smoltcp socket：

```rust
fn send_slice(&self, buf: &[u8]) -> Result<usize, SyscallErr> {
    with_tcp_mut(self.handle, |socket| {
        if socket.can_send() {
            socket.send_slice(buf).map_err(|_| SyscallErr::ECONNABORTED)
        } else {
            match socket.state() {
                tcp::State::Closed => Err(SyscallErr::ECONNRESET),
                tcp::State::TimeWait | tcp::State::Closing | tcp::State::LastAck => Err(SyscallErr::EPIPE),
                _ => Err(SyscallErr::EAGAIN),
            }
        }
    }).unwrap_or(Err(SyscallErr::EAGAIN))
}
```

### try_recv

```rust
pub fn try_recv(&self, buf: &mut [u8]) -> Result<isize, SyscallErr> {
    match self {
        Inner::Established(e) => {
            with_tcp_mut(e.handle, |socket| {
                if socket.can_recv() {
                    return socket.recv_slice(buf).map(|n| n as isize).map_err(|_| SyscallErr::ENOTCONN);
                }
                let state = socket.state();
                if state == tcp::State::CloseWait
                    || state == tcp::State::Closing
                    || state == tcp::State::LastAck
                    || state == tcp::State::TimeWait
                { return Ok(0); }  // EOF
                if state == tcp::State::Closed { return Err(SyscallErr::ECONNRESET); }
                if !socket.may_recv() { Ok(0) } else { Err(SyscallErr::EAGAIN) }
            }).unwrap_or(Err(SyscallErr::EAGAIN))
        }
        Inner::SelfConnected(sc) => sc.recv_into(buf, false).map(|n| n as isize),
        Inner::Closed(_) => Ok(0),
        Inner::Connecting(_) => Err(SyscallErr::EAGAIN),
        _ => Err(SyscallErr::EINVAL),
    }
}
```

### with_tcp_mut 模式

定义在 `stream/inner.rs:127`，是访问 smoltcp tcp::Socket 的统一入口：

```rust
pub(crate) fn with_tcp_mut<R>(
    handle: RouteSocketHandle,
    f: impl FnOnce(&mut tcp::Socket) -> R,
) -> Option<R> {
    NET_INTERFACE.tcp_routed_socket(handle, f)
}
```

形如 `NET_INTERFACE.tcp_routed_socket(rh, |sock| sock.send_slice(buf))` 的闭包模式贯穿所有 I/O 路径。`tcp_routed_socket` 返回 `Option`，handle 无效时返回 `None`。

## 就绪判断

```rust
fn socket_r_ready(&self) -> bool {       // mod.rs:591
    self.update_io_events();
    self.pollee.load(Ordering::Acquire) & EPollEvent::EPOLLIN.bits() != 0
}

fn socket_w_ready(&self) -> bool {       // mod.rs:600
    self.update_io_events();
    self.pollee.load(Ordering::Acquire) & EPollEvent::EPOLLOUT.bits() != 0
}
```

- `socket_r_ready()`: 先在 `update_io_events()` 中刷新 pollee 事件掩码，再检查 `EPOLLIN` 位。Established 状态下 `can_recv()` 或对端 FIN 后设置此位。
- `socket_w_ready()`: 类似逻辑检查 `EPOLLOUT`。Established 状态下 `can_send()` 为 true 时设置。

就绪检查前必须调用 `update_io_events()`，确保 pollee 缓存的位与 smoltcp socket 真实状态一致。

## 事件更新

`Inner::update_io_events()` 定义在 `stream/events.rs:14`，按变体类型分发：

```rust
pub fn update_io_events(&self, pollee: &AtomicUsize) {
    match self {
        Inner::Init(_) => {}                     // 无操作
        Inner::Connecting(c) => c.update_io_events(pollee),
        Inner::Listening(l)  => l.update_io_events(pollee),
        Inner::Established(e) => e.update_io_events(pollee),
        Inner::SelfConnected(sc) => sc.update_io_events(pollee),
        Inner::Closed(_) => {},                  // 无操作
    }
}
```

各变体的 update_io_events 实现于 `stream/inner.rs`：

- **Connecting**: 检查 smoltcp state，更新 `ConnectResult`。收到 Established/CloseWait 设 EPOLLOUT|EPOLLWRNORM；收到 Refused 设 EPOLLHUP|EPOLLERR；连接过程中所有事件清零。
- **Listening**: 遍历 handles 找 `is_active()` 的 socket，找到设 EPOLLIN|EPOLLRDNORM，未找到清除。
- **Established**: 根据 state 设置或清除 EPOLLOUT、EPOLLIN、EPOLLHUP、EPOLLRDHUP、EPOLLERR。can_send 控制 EPOLLOUT，can_recv 或 fin_received 控制 EPOLLIN。
- **SelfConnected**: 检查内部 VecDeque 队列长度和写端关闭标记，设置或清除 EPOLLIN/EPOLLOUT。

## 套接字选项

| 选项 | 方法 | 源文件 | 实现 |
|------|------|--------|------|
| TCP_NODELAY | `set_nagle_enabled()` | lifecycle.rs:313 | 转发 smoltcp `set_nagle_enabled()` |
| TCP_KEEPALIVE | `set_keep_alive()` | lifecycle.rs:340 | 转发 smoltcp `set_keep_alive(timeout)`（7200s） |
| TCP_INFO | `tcp_info()` | — | 返回 TcpInfo 结构体，tcpi_state 由 `tcp_state_code()` 产出 |
| SO_REUSEADDR | `set_reuse_addr()` | mod.rs:524 | AtomicBool 标记，bind 时检查冲突 |
| SO_BINDTODEVICE | `set_bind_to_device()` | mod.rs:529 | 设置 `bound_ifindex` 覆盖路由 ifindex |
| SO_RCVBUF | `set_recv_buf_size()` | mod.rs:447 | Init 状态可调整，Established 不可调 |
| SO_SNDBUF | `set_send_buf_size()` | mod.rs:468 | Init 状态可调整，Established 不可调 |

## 等待队列

TcpSocket 维护四种 `EventWaitQueue`，分别对应不同的阻塞语义：

```rust
pub recv_waiters: EventWaitQueue,      // 等待数据可读 (recvfrom/read)
pub send_waiters: EventWaitQueue,      // 等待可写 (sendto/write)
pub connect_waiters: EventWaitQueue,   // 等待连接完成 (connect)
pub accept_waiters: EventWaitQueue,    // 等待新连接 (accept)
```

应用层通过 Socket trait 暴露的 `recv_wait_queue()`、`send_wait_queue()`、`connect_wait_queue()`、`accept_wait_queue()` 获取对应的 `Mutex<WaitQueue>`，供 pselect/epoll 等阻塞机制挂入。

## 唤醒机制

`wake_tcp_waiters()` 定义在 `net/socket/mod.rs:854`，每次 `NET_INTERFACE.poll()` 后调用：

```rust
pub fn wake_tcp_waiters() {
    // 1. 升级 Weak<TcpSocket> 引用，收集存活 socket
    let mut live_sockets: Vec<Arc<TcpSocket>> = Vec::new();
    for (i, weak_socket) in TCP_SOCKETS.lock().iter().enumerate() {
        if let Some(socket) = weak_socket.upgrade() {
            live_sockets.push(socket);
        } else {
            remove_indices.push(i);   // 标记已释放的 Weak 引用
        }
    }
    // 2. 逐个唤醒
    for socket in &live_sockets {
        socket.wake_if_ready();
    }
    // 3. 清理死引用
    for &i in remove_indices.iter().rev() {
        TCP_SOCKETS.lock().remove(i);
    }
}
```

`TcpSocket::wake_if_ready()` 定义在 `stream/mod.rs:118`，按条件唤醒：

1. 先调用 `update_io_events()` 刷新 pollee
2. 检查 pollee 中事件位：
   - `EPOLLIN` — 唤醒 accept_waiters
   - `EPOLLOUT|EPOLLERR|EPOLLHUP` — 唤醒 connect_waiters
   - `EPOLLIN|EPOLLRDHUP` — 唤醒 recv_waiters（最多一个）
   - `EPOLLOUT` 或 write_shutdown — 唤醒 send_waiters（最多一个）

`wake_wait_queues()`（mod.rs:103）：无差别唤醒所有队列（用于 shutdown/close/Drop 时的紧急通知）。

## TcpInfo 结构体

`stream/tcp_info.rs` 定义 Linux `struct tcp_info`（用于 `getsockopt(TCP_INFO, ...)`）：

```rust
#[repr(C)]
pub struct TcpInfo {
    tcpi_state: u8,          // TCP 状态码
    tcpi_ca_state: u8,
    tcpi_retransmits: u8,
    tcpi_probes: u8,
    tcpi_backoff: u8,
    tcpi_options: u8,
    tcpi_snd_wscale: u8,
    tcpi_rcv_wscale: u8,
    tcpi_rto: u32,           // 重传超时 (ms)
    tcpi_ato: u32,
    tcpi_snd_mss: u32,       // 发送 MSS
    tcpi_rcv_mss: u32,       // 接收 MSS
    // ...（完整 Linux tcp_info 结构，71 个字段）
}
```

构造时仅填充 `tcpi_state` 和 `tcpi_{snd,rcv}_mss`，其余字段置零。`tcpi_state` 通过 `Inner::tcp_state_code()` 获取。

## TCP 状态码常量

用于 `tcp_state()` 查询和 `TcpInfo.tcpi_state` 的 Linux 兼容状态码：

| 常量名 | 值 | 说明 |
|--------|-----|------|
| TCP_ESTABLISHED | 1 | 连接已建立 |
| TCP_SYN_SENT | 2 | 主动连接（SYN 已发出） |
| TCP_SYN_RECV | 3 | 被动连接（SYN 已收到） |
| TCP_FIN_WAIT1 | 4 | 主动关闭（FIN 已发出） |
| TCP_FIN_WAIT2 | 5 | 主动关闭（等待对端 FIN） |
| TCP_TIME_WAIT | 6 | 等待旧报文消失 |
| TCP_CLOSE | 7 | 关闭 |
| TCP_CLOSE_WAIT | 8 | 被动关闭（对端 FIN 已收） |
| TCP_LAST_ACK | 9 | 被动关闭（FIN 已发，等待 ACK） |
| TCP_LISTEN | 10 | 监听 |
| TCP_CLOSING | 11 | 同时关闭 |

`Inner::tcp_state_code()`（inner.rs:906）的映射：

```
Inner::Init(_)         → 7  (TCP_CLOSE)
Inner::Connecting(_)   → 2  (TCP_SYN_SENT)
Inner::Listening(_)    → 10 (TCP_LISTEN)
Inner::Established(e)  → smoltcp state 映射：Established→1, CloseWait→8,
                         FinWait1→4, FinWait2→5, Closing→11, LastAck→9, 其他→7
Inner::SelfConnected(_)→ 1  (TCP_ESTABLISHED)
Inner::Closed(_)       → 7  (TCP_CLOSE)
```

trace_event 使用的 smoltcp tcp::State → u64 映射见 inner.rs:109（独立于上述 TcpStateCode 常量，仅用于调试）。
