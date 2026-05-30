# TCP 状态机详解

> 文件: `os/src/net/socket/inet/stream/inner.rs` (915 lines)
> 设计参考: DragonOS `kernel/src/net/socket/inet/stream/inner.rs`

## 六状态变体

```rust
pub enum Inner {
    Init(Init),              // 初始化 (Unbound / Bound)
    Connecting(Connecting),   // 正在建立连接
    Listening(Listening),    // 监听中
    Established(Established), // 已建立连接
    SelfConnected(SelfConnected), // 自连接 (connect 自身)
    Closed(Closed),          // 已关闭
}
```

## 状态转换图

```
new() → Init::Unbound(Box<tcp::Socket>, IpVersion)
  │
  ├── bind(addr) ──────────────────────────────→ Init::Bound { socket, local }
  │                                                │
  │   connect(remote) ─────────────────────────────┤
  │     → route_output(remote) → 选 ifindex        │
  │     → add_routed_socket(InetProtocol::Tcp,     │
  │        socket) → RouteSocketHandle              │
  │     → tcp_connect(handle, remote, local)        │
  │     → Connecting { handle, local, peer,         │
  │         result: ConnectResult::Pending }         │
  │                                                  │
  │   listen(backlog) ──────────────────────────────┤
  │     → add_routed_socket → handle                │
  │     → backlog 个 smoltcp listen sockets         │
  │     → Listening { handles: Vec<RouteSocketHandle>│
  │         listen_addr }                            │
  │                                                    │
  ├── connect(remote) ────────────────────────────────┤
  │   (Unbound auto-bind ephemeral)                   │
  │   → new tcp::Socket → add_routed_socket →         │
  │     Connecting                                     │
  │
  Connecting ──(update_io_events: is_connected)──→ Established { handle, local, peer }
  │
  Listening ──(accept: smoltcp accept)──→ Established { handle, local, peer }
  │                                      (从 Listening.handles 中 swap 取出)
  │
  ◆ SelfConnected: connect(self_addr:self_port) 时进入
    内部 VecDeque 模拟回环, 不走 smoltcp (尚未接入)
  │
  Established ──(close: smoltcp close)──→ TCP_SOCKETS_TO_REMOVE 队列
  Connecting  ──(close: smoltcp abort)──→ TCP_SOCKETS_TO_REMOVE 队列
  Listening   ──(close: 逐个 close handle)──→ TCP_SOCKETS_TO_REMOVE 队列
```

## Init 子状态

```rust
pub enum Init {
    Unbound(Box<tcp::Socket<'static>>, IpVersion),
    // Phase 6 (Lazy Bind): socket 尚未在 SocketSet 中
    Bound {
        socket: Box<tcp::Socket<'static>>,  // 已绑定地址, 尚未附着到 SocketSet
        local: IpEndpoint,                   // 绑定的本地地址
    },
}
```

**Lazy Bind 语义** (Phase 6):
- `bind()` 保存 boxed socket + local endpoint, 不调用 `SocketSet::add()`
- `connect()` / `listen()` 时按 `route_output()` 选择目标 ifindex, 然后 `add_routed_socket()` 附着
- `Bound` 状态下的 `setsockopt` (nagle, keepalive) 延迟到 connect/listen 时应用
- `close()` 对 `Bound` 状态直接 drop boxed socket (无 SocketSet 清理)

## 其他状态结构

```rust
pub struct Connecting {
    pub handle: RouteSocketHandle,
    pub local: IpEndpoint,
    pub peer: IpEndpoint,
    pub result: AtomicU8,  // ConnectResult::{Pending, Connected, Refused, RefusedConsumed}
    pub send_shutdown: AtomicBool,
    pub failure_reason: Mutex<Option<()>>,
}

pub struct Listening {
    pub handles: Vec<RouteSocketHandle>,  // backlog sockets (至少 1, 最多 8)
    pub listen_addr: IpListenEndpoint,
    pub connect: AtomicUsize,  // 已连接数
}

pub struct Established {
    pub handle: RouteSocketHandle,
    pub local: IpEndpoint,
    pub peer: IpEndpoint,
}

pub struct SelfConnected {  // 尚未接入
    pub handle: RouteSocketHandle,
    pub local: IpEndpoint,
    pub buf: Mutex<VecDeque<u8>>,  // 模拟 loopback buffer
    pub rx_cap: AtomicUsize,
    pub send_shutdown: AtomicBool,
}
```

## 关键辅助函数

```rust
pub(crate) fn with_tcp_mut<R>(
    handle: RouteSocketHandle,
    f: impl FnOnce(&mut tcp::Socket) -> R,
) -> Option<R> {
    NET_INTERFACE.tcp_routed_socket(handle, f)
}

pub(crate) fn with_tcp<R>(
    handle: RouteSocketHandle,
    f: impl FnOnce(&tcp::Socket) -> R,
) -> Option<R> {
    NET_INTERFACE.tcp_routed_socket(handle, |s| f(s))
}
```

协议层通过 `RouteSocketHandle` + 路由 facade 访问 smoltcp TCP socket, 永远不直接操作 `SocketSet`。

## 子文件

| 文件 | 说明 |
|------|------|
| `events.rs` | 各变体 (Connecting/Established/Listening/SelfConnected) 的 `update_io_events()` — 更新 epoll pollee |
| `io.rs` | Established 的 `send_slice()` / `recv_slice()`; SelfConnected 的 I/O |
| `lifecycle.rs` | bind / connect / listen / accept / shutdown / set_nagle / set_keep_alive |
| `tcp_info.rs` | `struct tcp_info` (Linux 兼容), getsockopt(TCP_INFO) |
