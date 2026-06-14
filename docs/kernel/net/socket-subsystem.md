# Socket 子系统: UDP, RAW, Unix, Netlink

## UDP Socket

> 文件: `os/src/net/socket/inet/datagram/udp.rs` (694 lines)

### 结构

```rust
pub struct UdpSocket {
    socket_handler: RouteSocketHandle,   // smoltcp socket 的路由令牌
    bound: Mutex<BoundInner>,            // 绑定元数据
    inner: Mutex<UdpSocketInner>,        // rx_queue + endpoint 缓存
    recv_waiters: EventWaitQueue,        // epoll 读等待
    send_waiters: EventWaitQueue,        // epoll 写等待
}

pub struct UdpSocketInner {
    remote_endpoint: Option<IpEndpoint>,  // connect 后缓存
    local_endpoint: Option<IpEndpoint>,
    rx_queue: VecDeque<(Vec<u8>, IpEndpoint)>,  // 接收队列
    recvbuf_size: usize, sendbuf_size: usize,
    reuse_addr: bool, multicast_group_joined: bool,
    // ...
}
```

### 本地投递 (Local Delivery)

```rust
fn try_deliver_local(&self, remote: IpEndpoint, data: &[u8]) -> Result<Option<isize>> {
    if !is_local_udp_destination(remote.addr) { return Ok(None); }
    let peer = find_local_udp_recipient(remote, src)?;
    peer.rx_queue.push_back((data.to_vec(), src));  // 直接入队, 不走 smoltcp
    peer.recv_waiters.notify_events_all(EPOLLIN | EPOLLRDNORM);
    Ok(Some(data.len()))
}
```

目标 IP 为本地地址时, 完全绕过 smoltcp 协议栈。在 `try_send()` 和 `try_sendmsg()` 中作为 smoltcp 发送之前的首步检查。

### dispatch_udp_packets

```rust
pub fn dispatch_udp_packets(sockets: &mut SocketSet);
```

在 poll 后调用, 从 smoltcp `SocketSet` 中抽取 UDP 数据包:
1. 遍历所有 smoltcp UDP socket
2. `udp_sock.can_recv()` → `udp_sock.recv()` → 获取 `(data, UdpMetadata)`
3. 匹配最合适的 OS UDP socket (按 remote addr + port 匹配)
4. 入队到目标 socket 的 `rx_queue`

Phase 4 后接受 `&mut SocketSet` (per-DeviceStack dispatch), 而非 `&mut NetInterfaceInner`。

### 未来: UDP Wildcard Per-Interface

```
当前: 一个 UDP socket → 一个 smoltcp socket → 一个 SocketSet
未来: bind(0.0.0.0) → 每个活跃 iface 一个 smoltcp UDP socket
      sendto() → route_output(remote) → 选对应的 iface socket
      recvfrom() → 扫描所有 iface sockets → 返回最早可读 datagram
```

---

## RAW Socket

> 文件: `os/src/net/socket/inet/raw/raw.rs` (281 lines)

```rust
pub struct RawSocket {
    socket_handler: RouteSocketHandle,
    inner: Mutex<RawSocketInner>,
    recv_waiters: EventWaitQueue,
}
```

- 支持 `IPPROTO_RAW` (手动构造 IP 头)
- `try_send()` / `try_recv()` 通过 smoltcp raw socket
- `bind`, `listen`, `connect`, `accept` 返回 `EOPNOTSUPP`
- 全局注册到 `RAW_SOCKETS: Mutex<Vec<(RouteSocketHandle, Weak<RawSocket>)>>`

---

## Unix Socket

> 文件: `os/src/net/socket/unix/` (7 files)

### Endpoint 类型

```rust
pub enum UnixEndpoint {
    Unnamed,                    // socketpair 创建
    Path(Arc<String>),          // 文件系统路径 (如 /tmp/mysock)
    Abstract(Arc<Vec<u8>>),     // Linux abstract namespace (@name)
}

pub static PATH_TABLE: Mutex<BTreeMap<String, Weak<UnixStreamSocket>>>;
```

### Stream Socket

- `UnixStreamSocket` 带 inner 状态机 (Init/Listening/Connected/Closed)
- 阻塞 accept: WaitQueue + 连接队列
- 非阻塞 I/O: `try_send` / `try_recv` + ring_buffer

### Datagram Socket

- `UnixDatagramSocket`: sendto/recvfrom + ring_buffer
- 无连接: 每次 sendto 指定目标路径

### 环形缓冲区 (ring_buffer.rs)

```rust
pub struct RingBuffer {
    buf: Mutex<VecDeque<u8>>,
    capacity: usize,
    read_waiters: WaitQueue,
    write_waiters: WaitQueue,
}
```

---

## Netlink

> 文件: `os/src/net/socket/netlink/` (3 files)

### NetlinkSocket

```rust
pub struct NetlinkSocket {
    inner: Mutex<NetlinkInner>,
    recv_waiters: EventWaitQueue,
}
```

### NETLINK_ROUTE

**支持的操作**:
- `RTM_GETLINK` — 接口列表 dump (ifinfomsg)
- `RTM_GETADDR` — 地址列表 dump (ifaddrmsg)
- `RTM_GETROUTE` — 路由表 dump (rtmsg)
- `NLMSG_DONE` — 多段响应结束标志
- `NLMSG_ERROR` — 不支持操作返回 NLMSG_ERROR

**不支持**:
- `RTM_NEWLINK` / `RTM_DELLINK`
- `RTM_NEWADDR` / `RTM_DELADDR`
- `RTM_NEWROUTE` / `RTM_DELROUTE`

所有不支持操作返回 `NLMSG_ERROR` (errno=EOPNOTSUPP), 永不 panic。

### 消息格式

```rust
// nlmsghdr (16 bytes) + 属性
pub struct nlmsghdr {
    nlmsg_len: u32, nlmsg_type: u16, nlmsg_flags: u16,
    nlmsg_seq: u32, nlmsg_pid: u32,
}

// rtattr (4 bytes) + data
pub struct rtattr {
    rta_len: u16, rta_type: u16,
}
```

---

## 共享组件

### BoundInner (bound.rs)

```rust
pub struct BoundInner {
    pub socket_handle: Option<RouteSocketHandle>,  // Lazy bind 时可为 None
    pub ifindex: u32,
    pub bound_addr: Option<IpAddress>,
    pub bound_port: u16,
}
```

### PortManager (port.rs)

```rust
pub static TCP_PORTS: Mutex<BTreeMap<u16, PortBinding>>;
pub static UDP_PORTS: Mutex<BTreeMap<u16, Vec<UdpPortBinding>>>;

impl PortManager {
    pub fn alloc_ephemeral_port() -> u16;  // 32768..60999
    pub fn bind_port(...);
    pub fn check_bind_conflict(...);
}
```

### Address 工具 (address.rs)

```rust
pub struct SocketAddrv4 { sin_family, sin_port, sin_addr, sin_zero }
pub struct SocketAddrv6 { sin6_family, sin6_port, sin6_flowinfo, sin6_addr, sin6_scope_id }
pub fn listen_endpoint(addr: &SocketAddrv4, len: usize) -> Result<IpListenEndpoint>;
pub fn fill_with_endpoint(endpoint: &SocketAddrv4, ep: &IpEndpoint);
```
