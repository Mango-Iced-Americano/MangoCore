---
title: "UDP / RAW / Unix / Netlink / Packet Socket 实现"
category: net
status: stable
author: MangoCore Team
last_update: 2026-06-14
tags: [net, udp, raw, unix, netlink, packet]
---

## 概述

本文档覆盖 Mango 内核中 TCP 以外的所有 socket 类型。每种 socket 类型对应一个协议族，在内核中有独立的实现文件或目录。所有 socket 类型均实现 `crate::net::Socket` trait，通过 `Socket::alloc()` 工厂方法分发。

```
Socket trait
  ├── UdpSocket         (SOCK_DGRAM,  AF_INET/AF_INET6)
  ├── RawSocket         (SOCK_RAW,    AF_INET/AF_INET6)
  ├── UnixStreamSocket  (SOCK_STREAM, AF_UNIX)
  ├── UnixDatagramSocket(SOCK_DGRAM,  AF_UNIX)
  ├── NetlinkSocket     (SOCK_RAW|SOCK_DGRAM, AF_NETLINK)
  └── PacketSocket      (SOCK_RAW|SOCK_DGRAM, AF_PACKET)
```

## UDP Socket

**文件**: `os/src/net/socket/inet/datagram/udp.rs` (~745 行)
**类型**: `SOCK_DGRAM`, `IPPROTO_UDP`
**Socket trait**: 通过 `impl Socket for UdpSocket` 实现

### UdpSocket 结构

```rust
pub struct UdpSocket {
    inner: Mutex<UdpSocketInner>,
    socket_handler: RouteSocketHandle,
    bound: Mutex<BoundInner>,
    bound_ifindex: Mutex<Option<u32>>,
    recv_waiters: EventWaitQueue,
    send_waiters: EventWaitQueue,
    pub ip_version: IpVersion,
}
```

- `socket_handler`: smoltcp 路由 socket 句柄，由 `NET_INTERFACE.add_routed_socket()` 分配
- `bound`: `BoundInner` 封装绑定的 socket 句柄、ifindex、地址和端口
- `bound_ifindex`: `SO_BINDTODEVICE` 绑定的接口索引
- `inner`: 核心内部状态

```rust
struct UdpSocketInner {
    remote_endpoint: Option<IpEndpoint>,
    local_endpoint: Option<IpListenEndpoint>,
    rx_queue: VecDeque<(Vec<u8>, IpEndpoint)>,
    last_recv_addr: Option<IpEndpoint>,
    msg_more_buf: Vec<u8>,
    recvbuf_size: usize,
    sendbuf_size: usize,
    reuse_addr: bool,
    multicast_group_joined: bool,
    ipv6_checksum_offset: Option<u32>,
}
```

- `rx_queue`: 接收队列，每个条目包含数据 payload 和源地址。数据通过 `dispatch_udp_packets` 或本地环路放入
- `msg_more_buf`: `MSG_MORE` 缓冲累积数据，发送时合并
- `last_recv_addr`: 最近一次接收的源地址（用于 `recvfrom` 返回对端地址）

### bind

```rust
fn bind(&self, endpoint: &Endpoint) -> SyscallRet
```

端口冲突检测通过 `PortManager::check_bind_conflict()` 完成：

1. 若端口为 0，调用 `PortManager::alloc_ephemeral_port()` 分配临时端口
2. 设置 `inner.local_endpoint`
3. 通过 `NET_INTERFACE.udp_routed_socket()` 调用 smoltcp 的 `socket.bind()`
4. 通过 `self.bound.lock().bind()` 记录绑定状态

**Ephemeral 端口范围**: `local_port_range()` 返回 `(32768, 60999)`，匹配 Linux 默认范围。UDP 初始化默认 `NEXT_EPHEMERAL_PORT` 为 49152，但 `alloc_ephemeral_port()` 实际使用动态范围。

**冲突检测**:
- `UDP_PORTS` 表维护所有已绑定的 UDP socket 弱引用
- `check_udp_conflict()` 检查端口+地址冲突
- `SO_REUSEADDR` 允许双方都启用时绕过冲突
- 已 connect 到远程的 UDP socket 不阻止同端口其他 bind

### connect

```rust
fn connect(&self, endpoint: &Endpoint) -> SyscallRet
```

设置 `inner.remote_endpoint`。若未 bind，自动分配临时端口和源 IP（通过 `lookup_source_ip`）。INADDR_ANY 映射为本地回环地址。已 connect 的 UDP socket 可通过 `try_send()` 直接发送（无需目的地址）。

### try_sendmsg

```rust
fn try_sendmsg(&self, buf, dest, flags) -> Result<isize, SyscallErr>
```

实现 sendto/sendmsg 语义：

1. **EMSGSIZE 检查**: UDP 最大负载 65535 - 20(IP头) - 8(UDP头) = 65507 字节
2. **MSG_MORE 处理**: 若设定了 `MSG_MORE`，数据缓冲到 `msg_more_buf`；非 `MSG_MORE` 时合并缓冲区
3. **本地环路优先**: `try_deliver_local()` 检查目标是否为本地地址
   - `is_local_udp_destination()`: 判断地址是否为回环或本地接口地址
   - `find_local_udp_recipient()`: 按 scoring 匹配最合适的本地接收 socket
     - addr 精确匹配 = 2 分, addr 通配(unspecified) = 1 分
     - peer 精确匹配 = 2 分, peer 未指定 = 1 分
   - 本地投递时直接将数据 push 到目标 socket 的 rx_queue，旁路 smoltcp
4. **路由检查**: `route_check()` 验证目标地址是否可达
5. **smoltcp 发送**: 通过 `NET_INTERFACE.udp_routed_socket()` 调用 `socket.send_slice()`

### try_recvmsg

```rust
fn try_recvmsg(&self, buf) -> Result<(isize, Option<Endpoint>), SyscallErr>
```

从 `inner.rx_queue` 弹出首个数据包并拷贝到用户缓冲区，返回字节数和源地址。队列为空返回 `EAGAIN`。

### dispatch_udp_packets

```rust
pub fn dispatch_udp_packets(sockets: &mut SocketSet)
```

在 smoltcp poll 之后调用，从 smoltcp SocketSet 的 UDP socket 中抽取数据包并分发到 OS 层 `UdpSocket`：

1. 遍历 smoltcp 的所有 socket，对每个 UDP socket 做 `downcast_mut`
2. 抽干缓冲区：`while udp_sock.can_recv()` 循环调用 `udp_sock.recv()`
3. `find_best_match()` 寻找最匹配的 OS UdpSocket：
   - 精确匹配(已 connect 到同地址): 3 分
   - 通配匹配(未 connect): 1 分
   - 已 connect 到其他地址: 0 分(不匹配)
4. 将数据推入目标 socket 的 rx_queue，通知接收等待队列

### Drop

```rust
impl Drop for UdpSocket
```

将 `socket_handler` 加入 `UDP_SOCKETS_TO_REMOVE`，由 `NET_INTERFACE` 的统一清理逻辑移除。

### Socket 选项

| 选项 | 方法 | 说明 |
|------|------|------|
| SO_REUSEADDR | `set_reuse_addr()` | 允许同一地址多 socket 绑定 |
| SO_BROADCAST | (隐式支持) | smoltcp 默认支持广播发送 |
| SO_BINDTODEVICE | `set_bind_to_device()` | 绑定 socket 到指定接口 |
| SO_RCVBUF | `set_recv_buf_size()` | 设置接收缓冲区大小(队列字节数限制) |
| SO_SNDBUF | `set_send_buf_size()` | 设置发送缓冲区大小 |
| IPV6_CHECKSUM | `set_ipv6_checksum()` | IPv6 UDP 校验和偏移 |
| IP_MULTICAST_JOIN | `join_multicast_group()` | 加入多播组 |
| IP_MULTICAST_LEAVE | `leave_multicast_group()` | 离开多播组 |

### 全局跟踪

```rust
pub static UDP_SOCKETS: Mutex<Vec<Weak<UdpSocket>>>;
pub static UDP_SOCKETS_TO_REMOVE: Mutex<Vec<RouteSocketHandle>>;
```

- `UDP_SOCKETS`: 所有存活 UdpSocket 的弱引用，用于 `dispatch_udp_packets` 分发
- `UDP_SOCKETS_TO_REMOVE`: 待移除的 smoltcp socket 句柄队列

### 实例化

```rust
pub fn new(ver: IpVersion) -> Self
```

创建 smoltcp UDP socket 并注册到 `NET_INTERFACE`。默认 1024 个 packet metadata 槽位，缓冲区大小 `MAX_BUFFER_SIZE`。

---

## RAW Socket

**文件**: `os/src/net/socket/inet/raw/raw.rs` (~624 行)
**类型**: `SOCK_RAW`
**协议**: 任意 `IPPROTO_*` 值（ICMP, IGMP, TCP, UDP, RAW 等）

### RawSocket 结构

```rust
pub struct RawSocket {
    inner: Mutex<RawSocketInner>,
    socket_handlers: Vec<RouteSocketHandle>,
    recv_waiters: EventWaitQueue,
    send_waiters: EventWaitQueue,
}
```

- `socket_handlers`: 每个网络栈一个 smoltcp raw socket 句柄。索引 0 为主 handler，后续为 lo、veth 等
- `inner`: 核心内部状态

```rust
struct RawSocketInner {
    local_endpoint: Option<IpListenEndpoint>,
    remote_endpoint: Option<IpEndpoint>,
    ip_version: IpVersion,
    ip_protocol: IpProtocol,
    recvbuf_size: usize,
    sendbuf_size: usize,
    bound_ifindex: Option<u32>,
    ipv6_checksum_offset: Option<u32>,
    icmp6_filter: [u32; 8],  // 256-bit bitmap
}
```

- `icmp6_filter`: 256-bit 位图，匹配 Linux ICMP6_FILTER 语义。bit=1 表示阻塞该类型 ICMPv6 消息

### 不支持的操作

| 操作 | 返回值 |
|------|--------|
| `bind` | 仅记录 local_endpoint(地址)，不做真实绑定 |
| `listen` | `EOPNOTSUPP` |
| `accept` | `EOPNOTSUPP` |

`connect` 可以设置 `remote_endpoint`，用于已连接 RAW socket 的 send 路径。

### try_sendmsg / send_to

两个模式：

**已连接模式** (有 remote_endpoint):
- 调用 `send_to()` 自动构造 IP 头
- IPv4: 构造 20 字节 IP 头，填充 version、header_len、total_len、protocol(TOS)、hop_limit、src/dst addr、校验和
- IPv6: 构造 40 字节 IP 头，填充 version、payload_len、next_header、hop_limit、src/dst addr
- 源 IP 通过目标接口的第一个地址确定，或通过 `lookup_source_ip()` 回退
- 接口选择: 优先 `SO_BINDTODEVICE`，否则路由查找

**未连接模式** (无 remote_endpoint):
- 不构造 IP 头，直接发送 raw 字节（IP_HDRINCL 语义）
- 用户自己在 payload 中包含 IP 头

### try_recvmsg / try_recv

```rust
fn try_recv(&self, buf) -> Result<isize, SyscallErr>
```

从所有 `socket_handlers` 依次尝试接收：

1. 对每个 handler 调用 `socket.recv_slice()`
2. IPv6 处理: 剥离 40 字节 IP 头（`buf.copy_within(40..nbytes, 0)`），payload_len = nbytes - 40
3. IPv4 处理: 保留完整 IP 头，通过 `Ipv4Packet::new_unchecked` 解析源地址
4. ICMP6 过滤: 检查 ICMPv6 type 在 `icmp6_filter` 中是否被阻塞，被阻塞则跳过
5. 更新 `remote_endpoint` 为最后一个收到的源地址

### Socket 选项

| 选项 | 方法 | 说明 |
|------|------|------|
| IP_HDRINCL | (隐式) | 未连接模式自动启用 |
| IPV6_CHECKSUM | `set_ipv6_checksum()` | 设置 IPv6 伪头校验和插入偏移(必须偶数) |
| ICMP6_FILTER | `set_icmp6_filter()` | 设置 256-bit ICMPv6 type 过滤 |
| SO_BINDTODEVICE | `set_bind_to_device()` | 绑定到指定接口 |

### 实例化

```rust
pub fn new(protocol: u32, ip_version: IpVersion) -> Self
```

为每个网络栈(lo, veth, eth0)创建独立的 smoltcp raw socket。每个 socket 128 个 metadata 槽位，默认 `MAX_BUFFER_SIZE`。

### 全局跟踪

```rust
pub static RAW_SOCKETS: Mutex<Vec<(RouteSocketHandle, Weak<RawSocket>)>>;
```

Drop 时自动清理所有 handler 的注册。

### IPv6 伪头校验和

`ipv6_pseudo_header_checksum()` 函数实现 RFC 2460 §8.1 的伪头校验和计算，用于 `IPV6_CHECKSUM` socket 选项。

---

## Unix Socket

**文件**: `os/src/net/socket/unix/` (7 个文件)
**类型**: `AF_UNIX` (AF_LOCAL), `SOCK_STREAM` / `SOCK_DGRAM` / `SOCK_SEQPACKET`

```
unix/
├── mod.rs           # 核心类型和工厂函数 (243 行)
├── stream/
│   ├── mod.rs       # UnixStreamSocket 实现 (565 行)
│   └── inner.rs     # 状态机: Init/Connected/Listener (190 行)
├── datagram/
│   └── mod.rs       # UnixDatagramSocket 实现 (490 行)
├── ns/
│   └── mod.rs       # UnixAbstractTable 抽象命名空间 (84 行)
└── ring_buffer.rs   # 通用环形缓冲区 (146 行)
```

### UnixEndpoint

```rust
pub enum UnixEndpoint {
    Path(String),               // 文件系统路径, 如 /tmp/socket.sock
    Abstract(Vec<u8>),          // 抽象命名空间, name 不含前导 NUL
    Unnamed,                    // 匿名 socket
}
```

```rust
pub enum UnixEndpointBound {
    Path(String),
    Abstract(Vec<u8>),
    Unnamed,
}
```

### PATH_TABLE

```rust
pub static PATH_TABLE: Mutex<BTreeMap<String, Weak<dyn Socket>>>
```

全局文件系统路径绑定表。`bind()` 时注册，unlink 或 drop 时自动清理。

### UnixAbstractTable

```rust
pub static ABSTRACT_TABLE: UnixAbstractTable
```

```rust
pub struct UnixAbstractTable {
    sockets: Mutex<BTreeMap<Arc<[u8]>, Weak<dyn Socket>>>,
}
```

- `UNIX_PATH_MAX = 108`
- `create_abstract_name_bytes()`: 注册指定名称的抽象 socket
- `alloc_ephemeral_abstract_name()`: 自动分配临时抽象名称（基于自增 ID）
- `lookup_abstract_name_bytes()`: 按名称查找
- `remove_abstract_name_bytes()`: 注销

### UnixStreamSocket

```rust
pub struct UnixStreamSocket {
    pub inner: Mutex<Inner>,
    is_nonblock: AtomicBool,
    recv_buf_size: AtomicUsize,
    send_buf_size: AtomicUsize,
    pub recv_waiters: EventWaitQueue,
    pub send_waiters: EventWaitQueue,
    pub connect_waiters: EventWaitQueue,
    pub accept_waiters: EventWaitQueue,
}
```

#### 状态机

```rust
pub enum Inner {
    Init(Init),
    Connected(Connected),
    Listener(Listener),
}
```

| 状态 | 说明 |
|------|------|
| `Init` | 刚创建或已 bind，尚未 connect 或 listen |
| `Connected` | 已通过 `connect`/`accept` 建立连接，使用双向 RingBuffer |
| `Listener` | 已通过 `listen` 进入监听状态 |

#### Connected 状态

```rust
pub struct Connected {
    pub addr: Option<UnixEndpointBound>,
    pub peer_addr: Option<UnixEndpointBound>,
    pub peer_creds: Option<(u32, u32, u32)>,  // (pid, uid, gid)
    pub peer_rx: Arc<Mutex<RingBuffer<u8>>>,
    pub rx: Arc<Mutex<RingBuffer<u8>>>,
}
```

双向通信使用两个独立的 `RingBuffer<u8>`:

- `peer_rx`: 写入此缓冲区 → 对端可读取（本端发送）
- `rx`: 从此缓冲区读取 → 对端写入的数据（本端接收）

默认缓冲区大小: `UNIX_STREAM_DEFAULT_BUF_SIZE = 64 * 1024` (64KB)

#### socketpair

`Connected::new_pair(buf_size)` 创建一对互连的 Connected:

```
side_a.peer_rx == side_b.rx
side_a.rx == side_b.peer_rx
```

工厂函数 `make_unix_socket_pair()` 在 `unix/mod.rs` 中，支持 `SOCK_STREAM` 和 `SOCK_DGRAM` 的 socketpair。

#### 监听与接受

Listener 持有等待连接的 `backlog` 队列。`accept` 从 backlog 取出已完成三次握手的 Connected 状态对端。

### UnixDatagramSocket

```rust
struct Inner {
    local_addr: Option<UnixEndpointBound>,
    peer_addr: Option<UnixEndpointBound>,
    recv_queue: VecDeque<DatagramMessage>,
    recv_queue_capacity: usize,
}
```

```rust
struct DatagramMessage {
    data: Vec<u8>,
    src_addr: Option<UnixEndpointBound>,
}
```

**独立 BindTable**:

```rust
static ref BIND_TABLE: BindTable
```

- `path_table`: `BTreeMap<String, Weak<UnixDatagramSocket>>`
- `abstract_table`: `AbstractTable`（包含 `BTreeMap<Arc<[u8]>, Weak<UnixDatagramSocket>>` 和自增 ID）

Datagram 消息传递: 发送时查找目标地址的 socket，直接推入其 `recv_queue`。

### RingBuffer

```rust
pub struct RingBuffer<T> {
    deque: VecDeque<T>,
    capacity: usize,
    recv_shutdown: AtomicBool,
    send_shutdown: AtomicBool,
}
```

通用环形缓冲区，基于 `VecDeque<T>`，支持原子 shutdown 标志。全局跟踪：
- `rb_alive()`: 活跃 RingBuffer 数量
- `rb_bytes()`: 总容量

### fill_with_endpoint

```rust
pub fn fill_with_endpoint(ep: &UnixEndpoint, addr: usize, addrlen: usize) -> SyscallRet
```

将 `UnixEndpoint` 写入用户空间 `sockaddr_un` 缓冲区:

- NULL 指针检查 → `EFAULT`
- addrlen 对齐检查（4 字节）→ `EFAULT`
- 容量小于 2 → `EINVAL`
- Path 变体: 拷贝路径字节，末尾补 NUL
- Abstract 变体: 写入前导 NUL + 抽象名称
- Unnamed 变体: 只写入 2 字节 `sa_family`

### 对等凭证

`SO_PEERCRED` 支持: `Connected` 状态维护 `peer_creds: Option<(pid, uid, gid)>`，在连接建立时通过 `current_task()` 获取。

---

## Netlink Socket

**文件**: `os/src/net/socket/netlink/` (4 个文件)
**类型**: `AF_NETLINK`, `SOCK_RAW` 或 `SOCK_DGRAM`

```
netlink/
├── mod.rs      # NetlinkSocket 结构 + Socket trait 实现 (181 行)
├── netlink.rs  # 协议常量和构建函数 (288 行)
├── segment.rs  # 消息段解析/序列化 (387 行)
└── route/
    ├── mod.rs  # 路由消息分发 (547 行)
    ├── link.rs # 链路消息处理
    ├── addr.rs # 地址消息处理
    └── route.rs# 路由消息处理
```

### NetlinkSocket

```rust
pub struct NetlinkSocket {
    pub protocol: u32,
    pub recv_queue: spin::Mutex<VecDeque<Vec<u8>>>,
    pub recv_wait: Mutex<WaitQueue>,
    local_portid: Mutex<u32>,
}
```

- `protocol`: netlink 协议类型（当前仅支持 `NETLINK_ROUTE`）
- `recv_queue`: 消息接收队列
- `local_portid`: 由 `bind()` 分配的自增端口 ID

#### 队列限制

- `MAX_NETLINK_QUEUE_LEN = 1024`（最大消息数）
- `MAX_NETLINK_QUEUE_BYTES = 256 * 1024`（最大总字节数）
- `push_recv()` 检查两个限制，队列满返回 false

#### bind

```rust
fn bind(&self, ep: &Endpoint) -> SyscallRet
```

接收 `Endpoint::Netlink(0)`，分配自增 `local_portid`（`NEXT_NETLINK_PORTID` 全局原子计数器）。

#### try_sendmsg

```rust
fn try_sendmsg(&self, buf, dest, flags) -> Result<isize, SyscallErr>
```

解析 netlink 消息流：

1. 遍历缓冲区中的 nlmsghdr（最小 16 字节）
2. 对每个消息调用 `route::handle_netlink_msg()`
3. 消息间按 `nlmsg_align()` 跳转到下一条
4. 全部处理完成后返回消耗的总字节数

#### 不支持的操作

| 操作 | 返回值 |
|------|--------|
| listen | `EOPNOTSUPP` |
| connect | `EOPNOTSUPP` |
| accept | `EOPNOTSUPP` |

### Netlink 协议常量 (netlink.rs)

**nlmsghdr 字段**:
- `nlmsg_align(len)`: 按 `NLMSG_ALIGNTO` (4) 向上对齐

**消息类型**:
| 常量 | 值 | 说明 |
|------|-----|------|
| NLMSG_NOOP | 1 | 空操作 |
| NLMSG_ERROR | 2 | 错误响应 |
| NLMSG_DONE | 3 | 多部分消息结束 |
| NLMSG_OVERRUN | 4 | 数据丢失 |
| NLMSG_MIN_TYPE | 0x10 | 应用自定义类型起始 |

**NLM_F 标志**:
| 常量 | 值 | 说明 |
|------|-----|------|
| NLM_F_REQUEST | 0x01 | 请求标志 |
| NLM_F_MULTI | 0x02 | 多部分消息 |
| NLM_F_ACK | 0x04 | 需要确认 |
| NLM_F_DUMP | 0x300 | 转储标志(ROOT\|MATCH) |

**RTM 类型** (NETLINK_ROUTE):

| 类型 | 常量 | 值 |
|------|------|-----|
| 链路 | RTM_NEWLINK | 16 |
| 链路 | RTM_DELLINK | 17 |
| 链路 | RTM_GETLINK | 18 |
| 链路 | RTM_SETLINK | 19 |
| 地址 | RTM_NEWADDR | 20 |
| 地址 | RTM_DELADDR | 21 |
| 地址 | RTM_GETADDR | 22 |
| 路由 | RTM_NEWROUTE | 24 |
| 路由 | RTM_DELROUTE | 25 |
| 路由 | RTM_GETROUTE | 26 |
| 邻居 | RTM_NEWNEIGH | 28 |
| 邻居 | RTM_DELNEIGH | 29 |
| 邻居 | RTM_GETNEIGH | 30 |
| 规则 | RTM_NEWRULE | 32 |
| 规则 | RTM_DELRULE | 33 |
| 规则 | RTM_GETRULE | 34 |

### 消息段类型 (segment.rs)

```rust
// C 结构体对应
pub struct CMsgSegHdr { len: u32, type_: u16, flags: u16, seq: u32, pid: u32 }
pub struct CAttrHeader { len: u16, type_: u16 }
```

**SegmentCommon<Body, Attr>**: 泛型消息段，自动解析和序列化
- `read_from_buf(buf)`: 从字节流解析 header + body + attributes
- `to_bytes()`: 序列化为字节流（含 alignment padding）

**身体类型**:
| 类型 | C 结构 | 大小 | 用途 |
|------|--------|------|------|
| CIfinfoMsg | ifinfomsg | 16 字节 | 链路信息 |
| CIfaddrMsg | ifaddrmsg | 8 字节 | 地址信息 |
| CRtMsg | rtmsg | 12 字节 | 路由信息 |
| ErrorSegmentBody | nlmsgerr | 20 字节 | 错误响应 |
| DoneSegmentBody | - | 4 字节 | 多部分结束 |

**RouteNlSegment 枚举**: 所有 RTM 消息类型的 discriminated union。

### 路由消息处理 (route/mod.rs)

`handle_netlink_msg()` 分发逻辑：

1. 解析 nlmsghdr 获取 type、flags、seq、pid
2. 验证 `NLM_F_REQUEST` 标志
3. Dump 请求 (`NLM_F_DUMP | NLM_F_ROOT`): 调用批量 handler
4. 单对象请求: 调用对应 handler

**支持的操作**:

**Dump 操作**:
| 操作 | handler | 响应格式 |
|------|---------|----------|
| RTM_GETLINK | `handle_getlink` | 每个接口一个 RTM_NEWLINK + NLMSG_DONE |
| RTM_GETADDR | `handle_getaddr` | 每个地址一个 RTM_NEWADDR + NLMSG_DONE |
| RTM_GETROUTE | `handle_getroute` | 每条路由一个 RTM_NEWROUTE + NLMSG_DONE |
| RTM_GETNEIGH | `handle_getneigh` | 每个邻居一个 RTM_NEWNEIGH + NLMSG_DONE |

**修改操作**:
| 操作 | handler |
|------|---------|
| RTM_NEWLINK | `link::handle_newlink` (veth 对创建) |
| RTM_DELLINK | `link::handle_dellink` (veth 对删除) |
| RTM_SETLINK | `link::handle_setlink` (接口状态设置) |
| RTM_NEWADDR | `addr::handle_newaddr` (IP 地址添加) |
| RTM_DELADDR | `addr::handle_deladdr` (IP 地址删除) |
| RTM_NEWNEIGH | `handle_newneigh` (邻居记录) |
| RTM_DELNEIGH | `handle_delneigh` (邻居删除) |

**RTM_GETLINK dump (handle_getlink)**:
- 遍历 `net_core::current_netns().device_list`
- 每个接口构造 `ifinfomsg` (16 字节) + 属性:
  - IFLA_IFNAME: 接口名
  - IFLA_MTU: MTU 值
  - IFLA_ADDRESS: MAC 地址(非 loopback)
- 以 `NLM_F_MULTI` 标志发送
- 末尾发送 `NLMSG_DONE` 结束

**RTM_GETADDR dump (handle_getaddr)**:
- 遍历所有接口的 IP 地址
- 每个地址构造 `ifaddrmsg` (8 字节) + 属性:
  - IFA_ADDRESS, IFA_LOCAL, IFA_LABEL
- 支持 IPv4 (AF_INET=2) 和 IPv6 (AF_INET6=10)

**RTM_GETROUTE dump (handle_getroute)**:
- 遍历路由表条目
- 构造 `rtmsg` (12 字节) + 属性:
  - RTA_DST: 目标地址(prefix_len>0)
  - RTA_GATEWAY: 下一跳
  - RTA_OIF: 出接口索引

**不支持的操作**:
- RTM_NEWROUTE (路由添加): 返回 `EOPNOTSUPP`
- RTM_DELROUTE (路由删除): 返回 `EOPNOTSUPP`

**属性工具函数**:
- `rta_data(rta_type, payload)`: 构造 RTA 属性（4 字节 header + payload + alignment）
- `finish_response(segments, was_dump)`: 添加 NLM_F_MULTI 标志和 NLMSG_DONE 结束标记

### 响应格式

所有响应通过 `sock.push_recv()` 推入 `NetlinkSocket.recv_queue`:

- 成功 dump: 多个 `NLM_F_MULTI` 消息 + 一个 `NLMSG_DONE`（error_code=0）
- 错误: 一个 `NLMSG_ERROR`（error_code = 负 errno, 包含原始请求头）

---

## Packet Socket

**文件**: `os/src/net/socket/packet.rs` (~320 行)
**类型**: `AF_PACKET`, `SOCK_RAW` 或 `SOCK_DGRAM`

### PacketSocket

```rust
pub struct PacketSocket {
    pub inner: Mutex<PacketSocketInner>,
    recv_waiters: EventWaitQueue,
    send_waiters: EventWaitQueue,
}
```

```rust
pub struct PacketSocketInner {
    pub bound_ifindex: u32,
    pub bound_protocol: u16,
    pub rx_queue: VecDeque<Vec<u8>>,
    pub recvbuf_size: usize,
    pub sendbuf_size: usize,
}
```

- `bound_ifindex`: 绑定的接口索引，0 表示匹配所有接口
- `bound_protocol`: 协议过滤，`ETH_P_ALL (0x0003)` 匹配所有以太网协议

### 协议常量

```rust
pub const ETH_P_ALL: u16   = 0x0003;
pub const ETH_P_ARP: u16   = 0x0806;
pub const ETH_P_IP: u16    = 0x0800;
```

### bind

```rust
fn bind(&self, endpoint: &Endpoint) -> SyscallRet
```

接收 `Endpoint::Packet(ep)`，设置 `bound_ifindex` 和 `bound_protocol`。`Endpoint::Unspecified` 可绑定到任意接口。

### 不支持的操作

| 操作 | 返回值 |
|------|--------|
| listen | `EOPNOTSUPP` |
| connect | `EOPNOTSUPP` |
| accept | `EOPNOTSUPP` |

### try_send

```rust
fn try_send(&self, buf, flags) -> Result<isize, SyscallErr>
```

通过设备层的 `transmit()` 直接发送原始以太网帧：

1. 根据 `bound_ifindex` 找到目标接口
2. 调用 `device.transmit(timestamp)` 获取发送 token
3. `tx_token.consume()` 将帧数据写入设备发送缓冲区

`try_sendmsg` 额外支持通过 `Endpoint::Packet` 的 ifindex 覆盖临时发送接口。

### try_recv

```rust
fn try_recv(&self, buf) -> Result<isize, SyscallErr>
```

从 `inner.rx_queue` 弹出原始以太网帧。

### deliver_frame_to_packet_sockets

```rust
pub fn deliver_frame_to_packet_sockets(frame: &[u8], ifindex: u32)
```

将原始以太网帧分发到匹配的 PacketSocket:

1. 从帧头提取 ethertype (`frame[12..14]`)
2. 遍历 `PACKET_SOCKETS` 全局列表，找到匹配的 socket:
   - `bound_ifindex == 0`（任意接口）或 `== ifindex`
   - `bound_protocol == ETH_P_ALL` 或 `== ethertype`
3. 将完整帧推入匹配 socket 的 `rx_queue`
4. 通知 `recv_waiters`

### deliver_frames_from_veth_queue

```rust
pub fn deliver_frames_from_veth_queue(ifindex: u32, rx_queue: &VecDeque<Vec<u8>>)
```

在 poll 循环中调用，早于 smoltcp 消费帧。将 veth 的接收队列中的帧逐个调用 `deliver_frame_to_packet_sockets` 分发。

### 全局跟踪

```rust
pub static PACKET_SOCKETS: Mutex<Vec<Weak<PacketSocket>>>;
```

`Drop` 时自动清理已失效的弱引用。

### SO_BINDTODEVICE

`set_bind_to_device()` 支持通过接口名动态绑定/解绑。

---

## 跨 Socket 类型的比较

| 特性 | UDP | RAW | Unix Stream | Unix Dgram | Netlink | Packet |
|------|-----|-----|-------------|------------|---------|--------|
| 协议族 | AF_INET/AF_INET6 | AF_INET/AF_INET6 | AF_UNIX | AF_UNIX | AF_NETLINK | AF_PACKET |
| socket 类型 | Datagram | Raw | Stream | Datagram | Raw | Raw |
| bind | 端口绑定 | 记录地址 | 路径/抽象名 | 路径/抽象名 | 分配 portid | 接口+协议 |
| connect | 设置远端 | 设置远端 | 建立连接 | 设置对端 | EOPNOTSUPP | EOPNOTSUPP |
| listen | EOPNOTSUPP | EOPNOTSUPP | 支持 | EOPNOTSUPP | EOPNOTSUPP | EOPNOTSUPP |
| accept | EOPNOTSUPP | EOPNOTSUPP | 支持 | EOPNOTSUPP | EOPNOTSUPP | EOPNOTSUPP |
| 数据单元 | 报文(保持边界) | 原始 IP 包 | 字节流 | 报文(保持边界) | nlmsg 消息 | 原始帧 |
| 环路优化 | try_deliver_local | 无 | RingBuffer 直连 | BindTable 直连 | 队列传递 | 帧分发 |
| smoltcp 依赖 | 是 | 是 | 否 | 否 | 否 | 否(直连设备) |
