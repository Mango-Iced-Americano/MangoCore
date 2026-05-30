# 网络子系统架构文档 (Network Subsystem Architecture)

> 最后更新: 2026-05-30 | 分支: `routing` | 作者: Sisyphus

## 目录

1. [总览](#1-总览)
2. [架构分层](#2-架构分层)
3. [启动流程](#3-启动流程)
4. [数据流](#4-数据流)
5. [内核网络栈 (Core Stack)](#5-内核网络栈-core-stack)
6. [Socket 子系统](#6-socket-子系统)
7. [系统调用层](#7-系统调用层)
8. [后续规划](#8-后续规划)

---

## 1. 总览

### 1.1 设计参考

本网络子系统参考 [DragonOS](https://github.com/DragonOS-Community/DragonOS) 的 VFS/MountFS 架构和 Linux 6.6 语义设计，底层使用 [smoltcp](https://github.com/smoltcp-rs/smoltcp) 作为 TCP/IP 协议栈。

### 1.2 核心特性

| 特性 | 状态 | 说明 |
|------|------|------|
| TCP/IPv4 | ✅ 完整 | 基于 smoltcp, 支持 server/client |
| UDP/IPv4 | ✅ 完整 | 单播/广播/多播, DNS, loopback |
| RAW Socket | ✅ 完整 | IPPROTO_RAW |
| Unix Socket | ✅ 完整 | SOCK_STREAM + SOCK_DGRAM, abstract namespace |
| Netlink | ✅ 部分 | NETLINK_ROUTE (RTM_GETLINK/GETADDR/GETROUTE) |
| DHCP | ✅ 完整 | 启动时同步探测, 动态 IP 分配 |
| 多接口架构 | ✅ 基础 | 每设备独立 smoltcp 栈, RouteSocketHandle 路由层 |
| /proc/net | ✅ 完整 | /proc/net/dev, route, tcp, udp |
| SIOCGIF* ioctl | ✅ 完整 | ifreq/ifconf 读查询 |
| epoll/eventfd | ✅ 完整 | 网络 socket 的 epoll 支持 |
| 跨接口转发 | ⬜ 规划中 | RouterEnableDevice trait |
| NAT/ConnTrack | ⬜ 远期 | SNAT/DNAT |

### 1.3 文件总览 (50 个源文件)

```
os/src/net/
├── mod.rs                     # 模块根: 全局静态变量重导出
├── adapter.rs                 # 设备适配层: SmoltcpDeviceAdapter, IfaceDevice, NullNetDevice
├── config.rs                  # NetInterface: 多栈管理, DHCP, poll, 路由 facade
├── net_core.rs                # 接口元数据: IFACES 注册表, IP 地址管理
├── routing.rs                 # 路由表: Router, RouteTable, RouteSocketHandle
├── ioctl.rs                   # SIOCGIF*: ifreq/ifconf 结构
├── posix.rs                   # PosixArgsSocketType 解析器
├── macros.rs                  # 废弃 (原 impl_file_for_socket!)
├── socket/                    # Socket 子系统
│   ├── mod.rs                 # Socket trait, SocketFile, 全局 socket 注册表
│   ├── common/                # (空)
│   ├── inet/                  # AF_INET 协议族
│   │   ├── mod.rs             # 重导出
│   │   ├── common/            # 共享: BoundInner, PortManager, address 工具
│   │   ├── stream/            # TCP: 6 状态状态机
│   │   ├── datagram/          # UDP
│   │   └── raw/               # RAW socket
│   ├── unix/                  # AF_UNIX 协议族
│   │   ├── mod.rs             # UnixEndpoint, PATH_TABLE
│   │   ├── stream/            # Unix stream socket (+ inner)
│   │   ├── datagram/          # Unix datagram socket
│   │   ├── ns/                # Abstract namespace table
│   │   └── ring_buffer.rs     # 环形缓冲区
│   └── netlink/               # AF_NETLINK
│       ├── mod.rs             # Netlink 协议族注册
│       ├── netlink.rs         # NetlinkSocket 实现
│       └── route.rs           # NETLINK_ROUTE handler
└── syscall/                   # 网络系统调用
    ├── mod.rs                 # dispatch 分发
    ├── socket.rs / bind.rs / connect.rs / listen.rs / accept.rs
    ├── sendto.rs / recvfrom.rs / sendmsg.rs / recvmsg.rs
    ├── getsockname.rs / getpeername.rs
    ├── getsockopt.rs / setsockopt.rs
    ├── shutdown.rs / socketpair.rs
    └── common.rs              # MsgFlags, socket 选项常量
```

---

## 2. 架构分层

### 2.1 三层设计

```
┌──────────────────────────────────────────────┐
│ PROTOCOL LAYER (协议层)                       │
│ os/src/net/socket/                           │
│                                              │
│ • Socket trait + SocketFile (fd 层)          │
│ • TCP 状态机 (6 状态: Init/Connecting/       │
│   Listening/Established/SelfConnected/Closed)│
│ • UDP rx_queue + try_deliver_local           │
│ • RAW socket send/recv                       │
│ • Unix socket (stream + datagram)            │
│ • PortManager (TCP/UDP 端口分配 + 冲突检测)   │
│ • epoll 事件等待队列                          │
│                                              │
│ 只持有: RouteSocketHandle (不透明令牌)        │
│ 不持有: SocketHandle, Interface, Device      │
├──────────────────────────────────────────────┤
│ ROUTING LAYER (路由层)                        │
│ os/src/net/config.rs + routing.rs            │
│                                              │
│ • RouteSocketHandle → {ifindex, SocketHandle}│
│   映射 (binding table)                        │
│ • DeviceStack 管理 (每设备独立 smoltcp 栈)     │
│ • Router + RouteTable (最长前缀匹配)          │
│ • DHCP 探测 (启动时同步)                      │
│ • poll 编排 (遍历所有栈)                      │
│ • source address 选择                         │
│ • local route 判断                            │
├──────────────────────────────────────────────┤
│ DEVICE LAYER (设备层)                         │
│ os/src/net/adapter.rs + drivers/net/         │
│                                              │
│ • IfaceDevice enum (Lo | Eth)                │
│ • SmoltcpDeviceAdapter (phy NIC 封装)         │
│ • NullNetDevice (无 NIC 降级)                │
│ • Loopback (smoltcp 内置回环)                 │
│ • NetDevice trait (驱动程序接口)              │
│ • VirtIO net driver                          │
└──────────────────────────────────────────────┘
```

### 2.2 核心抽象: RouteSocketHandle

```rust
// routing.rs
pub struct RouteSocketHandle(pub(crate) usize);

// 内部映射 (路由层私有)
struct SocketBinding {
    pub ifindex: u32,        // 所属接口 (1=lo, 2=eth0)
    pub handle: SocketHandle, // smoltcp 内部句柄 (只在所属 SocketSet 内有效)
    pub proto: InetProtocol,  // Tcp / Udp / Raw
}
```

协议层代码**永远不**导入 `smoltcp::iface::SocketHandle`，所有 socket 操作通过路由 facade:

```rust
NET_INTERFACE.tcp_routed_socket(route_handle, |sock| ...)
NET_INTERFACE.tcp_connect(route_handle, remote, local)
NET_INTERFACE.add_routed_socket(InetProtocol::Tcp, smoltcp_socket)
```

### 2.3 每设备独立 smoltcp 栈

```
DeviceStack (ifindex=1, name="lo")
├── device: IfaceDevice::Lo(Loopback)
├── iface:  smoltcp Interface (Medium::Ip, 127.0.0.1/8)
└── sockets: smoltcp SocketSet

DeviceStack (ifindex=2, name="eth0")
├── device: IfaceDevice::Eth(SmoltcpDeviceAdapter)
├── iface:  smoltcp Interface (Medium::Ethernet, DHCP IP)
└── sockets: smoltcp SocketSet
```

每个 `DeviceStack` 拥有独立的 smoltcp `Interface` + `SocketSet`。poll 时遍历所有栈分别推进。

---

## 3. 启动流程

```
QEMU → OpenSBI → entry.asm → rust_main()
  → console::init()
  → mm::init()
  → drivers::init()
    → init_net_device()
      → VirtIONetWrapper::new() → NET_DEVICE = Some(Arc<dyn NetDevice>)
  → fs::init()
  → net::init()    ← config.rs::init()
    → net_core::init()
      → 注册 lo (ifindex=1, 127.0.0.1/8)
      → 注册 eth0 (ifindex=2, DHCP 后填充 IP)
    → NetInterfaceInner::new()
      → 栈 1 (lo):   Loopback + smoltcp Interface + SocketSet
      → 栈 2 (eth0): take NET_DEVICE → SmoltcpDeviceAdapter
        → DHCP 同步探测 (5s deadline)
          → smoltcp dhcpv4::Socket poll
          → 成功 → net_core::set_eth0_ipv4() + set_default_gateway()
          → 超时 → 继续无 IP 启动
        → 创建 smoltcp Interface + SocketSet
  → task::init() → 加载 initproc
  → run_tasks()
```

---

## 4. 数据流

### 4.1 TCP 发送

```
sys_sendto(sockfd, buf, len, ...)
  → get_socket!(sockfd) → Arc<dyn Socket>
  → socket.try_send(&buf, flags)
    → TcpSocket::try_send()
      → NET_INTERFACE.try_poll()
      → Inner::try_send(buf)
        → with_tcp_mut(route_handle, |sock| sock.send_slice(buf))
          → NET_INTERFACE.tcp_routed_socket(route_handle, |sock| ...)
            → 查 binding table → 找到 ifindex
            → stack_mut(ifindex) → DeviceStack
            → stack.sockets.get_mut::<tcp::Socket>(handle)
            → sock.send_slice(buf)
              → smoltcp 内部排队
  → NET_INTERFACE.try_poll()
    → poll_once()
      → 遍历所有 DeviceStack
        → stack.iface.poll(timestamp, &mut stack.device, &mut stack.sockets)
          → smoltcp 发送 TCP 段到 Device
```

### 4.2 UDP 本地投递

```
sys_sendto(sockfd, buf, len, MSG_DONTWAIT, dest, addrlen)
  → UdpSocket::try_sendmsg(&buf, remote, flags)
    → NET_INTERFACE.try_poll()
    → try_deliver_local(remote, data)     ← 检查目标是否本地地址
      → is_local_udp_destination(remote.addr)
        → 检查 IFACES 中是否有匹配的 IP
      → 找到本地 peer socket
        → peer.rx_queue.push_back((data, src))   ← 直接入队，不走 smoltcp
        → 唤醒 peer recv_waiters
    → 如果 try_deliver_local 返回 None (非本地)
      → udp_routed_socket(handle, |sock| sock.send_slice(...))
        → smoltcp 正常发送
```

### 4.3 Poll 循环

```
定时器 ISR / 调度器 idle / 系统调用路径
  → NET_INTERFACE.try_poll()
    → poll_once():
      1. 收集 UDP/TCP 待清理 handle (按 ifindex 分组)
      2. 遍历每个 DeviceStack:
         a. UDP socket 清理 (从 SocketSet remove)
         b. smoltcp poll: iface.poll(timestamp, device, sockets)
         c. TCP socket 清理 (检查 Closed/TimeWait 状态)
         d. dispatch_udp_packets(sockets) — 从 smoltcp 分发到 OS rx_queue
      3. 更新所有 TCP socket 的 IO 事件 (update_io_events)
      4. 唤醒 TCP/RAW 等待队列
```

---

## 5. 内核网络栈 (Core Stack)

### 5.1 adapter.rs — 设备适配层

**文件**: `os/src/net/adapter.rs` (300+ lines)

| 类型 | 说明 |
|------|------|
| `IfaceDevice` | 单设备枚举: `Lo(Loopback)` \| `Eth(SmoltcpDeviceAdapter)`, 实现 smoltcp `Device` trait |
| `SmoltcpDeviceAdapter` | 将 `Arc<dyn NetDevice>` 封装为 smoltcp `Device` |
| `NullNetDevice` | 无 NIC 时的空设备, transmit no-op, receive 永远 None |
| `RoutingDevice` | (已废弃) 原 lo+eth 软件交换机, 被 IfaceDevice 取代 |

**IfaceDevice 设计**: smoltcp 的 `Device` trait 有 GAT (Generic Associated Types: `RxToken<'a>`, `TxToken<'a>`), 不能装箱为 trait object。用 enum 包装两个具体 Device 类型, 通过 delegating RxToken/TxToken 实现。

### 5.2 config.rs — NetInterface 多栈管理

**文件**: `os/src/net/config.rs` (530+ lines)

核心全局变量: `NET_INTERFACE: NetInterface` (包含 `Mutex<Option<NetInterfaceInner>>`)

**关键结构**:

```rust
pub struct DeviceStack<'a> {
    pub ifindex: u32,           // 接口编号 (1=lo, 2=eth0)
    pub name: &'static str,     // 接口名
    pub device: IfaceDevice,    // 物理/虚拟设备
    pub iface: Interface,       // smoltcp 协议栈接口
    pub sockets: SocketSet<'a>, // smoltcp socket 集合
}

pub struct NetInterfaceInner<'a> {
    pub stacks: Vec<DeviceStack<'a>>,
    pub bindings: BTreeMap<RouteSocketHandle, SocketBinding>,
    pub next_socket_id: usize,
}
```

**主要 API**:

| 方法 | 说明 |
|------|------|
| `init()` | 调用 `net_core::init()` + `NET_INTERFACE.init()` |
| `poll()` / `try_poll()` | 阻塞/非阻塞 poll |
| `add_routed_socket(proto, socket)` | 添加 socket 到默认栈, 返回 RouteSocketHandle |
| `tcp_routed_socket(rh, f)` | 通过 RouteSocketHandle 访问 TCP socket |
| `udp_routed_socket(rh, f)` | 通过 RouteSocketHandle 访问 UDP socket |
| `raw_routed_socket(rh, f)` | 通过 RouteSocketHandle 访问 RAW socket |
| `tcp_connect(rh, remote, local)` | TCP connect (需要 Interface::context()) |
| `remove_routed(rh)` | 从 SocketSet 移除 + 清理 binding |
| `lookup_source_ip(dest)` | 源地址选择 → 委托 `routing::route_output()` |
| `route_check(dest)` | 路由可达性检查 → 委托 `routing::route_output()` |

### 5.3 routing.rs — 路由表

**文件**: `os/src/net/routing.rs` (300+ lines)

```rust
pub struct Router { pub(crate) table: RouteTable }

impl Router {
    pub fn lookup_route(&self, dest: Ipv4Address) -> Option<&RouteEntry>;  // 最长前缀匹配
    pub fn init_default() -> Self;  // 从 net_core 动态构建 (lo 127/8 + DHCP eth0 + default gw)
}

pub fn route_output(dest: IpAddress) -> Result<RouteDecision, SyscallErr>;
// 统一路由层 API: 先检查 local addr → 再查 FIB → 返回 ifindex + source + next_hop
```

**RouteDecision**:

```rust
pub struct RouteDecision {
    pub ifindex: u32,        // 出接口
    pub source: IpAddress,   // 源地址
    pub next_hop: Option<IpAddress>,  // 下一跳
    pub is_local: bool,      // 目标是否本机地址
}
```

### 5.4 net_core.rs — 接口元数据

**文件**: `os/src/net/net_core.rs` (170+ lines)

```rust
pub static IFACES: Mutex<Vec<DeviceEntry>>;     // 全局接口注册表
pub static ETH0_CIDR: Mutex<Option<IpCidr>>;     // DHCP 分配的 IP
pub static DEFAULT_GW: Mutex<Option<Ipv4Address>>; // DHCP 网关

pub struct DeviceEntry {
    pub ifindex: u32, pub name: &'static str,
    pub flags: u32, pub mtu: u32,
    pub hwaddr: [u8; 6], pub ip_addrs: Vec<IpCidr>,
}
```

注册: lo (ifindex=1, 127.0.0.1/8), eth0 (ifindex=2, 由 DHCP 填充 IP)。

---

## 6. Socket 子系统

### 6.1 Socket trait + SocketFile

**文件**: `os/src/net/socket/mod.rs` (750+ lines)

```rust
pub trait Socket: Send + Sync {
    fn try_recv(&self, buf: &mut [u8], flags: MsgFlags) -> (GeneralRet<usize>, IpEndpoint);
    fn try_send(&self, buf: &[u8], flags: MsgFlags) -> GeneralRet<isize>;
    fn try_sendmsg(&self, buf: &[u8], remote: IpEndpoint, flags: MsgFlags) -> GeneralRet<isize>;
    fn r_ready(&self) -> bool;
    fn w_ready(&self) -> bool;
    fn socket_type(&self) -> PosixArgsSocketType;
    fn metadata(&self) -> SocketMetadata;
    fn setsockopt(&self, level: usize, opt: usize, val: &[u8]) -> GeneralRet<usize>;
    fn getsockopt(&self, level: usize, opt: usize) -> Result<Vec<u8>, SyscallErr>;
    // ...
}

/// IndexNode 包装, 将 Socket 暴露为文件描述符
pub struct SocketFile { pub inner: Arc<dyn Socket> }

/// 全局注册表
pub static UDP_SOCKETS: Mutex<Vec<Weak<UdpSocket>>>;
pub static TCP_SOCKETS: Mutex<Vec<Weak<TcpSocket>>>;
pub static RAW_SOCKETS: Mutex<Vec<(RouteSocketHandle, Weak<RawSocket>)>>;
```

### 6.2 TCP 状态机

**文件**: `os/src/net/socket/inet/stream/inner.rs` (915 lines)

6 状态变体:

```
                    new()
                      │
                      ▼
    ┌─────────── Init ───────────┐
    │ Unbound (Box<tcp::Socket>) │──bind()──► Bound { socket, local }
    │ Bound { socket, local }    │──connect()──► Connecting
    └────────────────────────────┘──listen()──► Listening
                                                  │
    Connecting ──(SYN+ACK)──► Established        │
    Listening ──(accept)──► Established           │
                                                  │
    SelfConnected ←(connect self)                 │
                                                  │
    Established ──(close)──► Closed               │
    Connecting ──(close)──► Closed                │
```

**Lazy Bind (Phase 6)**: `bind()` 不再立即 `SocketSet::add()`。`Bound` 状态保存 `Box<tcp::Socket>`。`connect()` 时按 `route_output()` 选目标 ifindex, `listen()` 时选 eth0。失败时恢复 `Bound` 状态 (创建新 smoltcp socket)。

**关键方法**:

```rust
pub(crate) fn with_tcp_mut(handle: RouteSocketHandle, f: impl FnOnce(&mut tcp::Socket) -> R) -> Option<R>;
// 通过路由层 facade 访问 smoltcp TCP socket
```

**子文件**:
- `events.rs` — 各变体的 `update_io_events` (epoll 事件更新)
- `io.rs` — Established/SelfConnected 的 `send_slice` / `recv_slice`
- `lifecycle.rs` — bind / connect / listen / accept / shutdown
- `tcp_info.rs` — Linux `struct tcp_info` (getsockopt TCP_INFO)

### 6.3 UDP

**文件**: `os/src/net/socket/inet/datagram/udp.rs` (694 lines)

```rust
pub struct UdpSocket {
    socket_handler: RouteSocketHandle,  // smoltcp socket 的路由令牌
    bound: Mutex<BoundInner>,           // 绑定信息 (ifindex, addr, port)
    inner: Mutex<UdpSocketInner>,       // rx_queue, endpoint 缓存
    recv_waiters: EventWaitQueue,
    send_waiters: EventWaitQueue,
}
```

**本地投递 (Local Delivery)**: `try_deliver_local()` 在 smoltcp 发送之前, 检查目标 IP 是否为本地地址。如果是, 直接将数据插入 peer socket 的 `rx_queue`, 完全绕过 smoltcp 协议栈。

**dispatch_udp_packets**: 在 poll 后从 smoltcp SocketSet 抽取 UDP 数据包, 匹配到 OS UDP socket 的 rx_queue。现在接受 `&mut SocketSet` 参数, 每个 DeviceStack 独立 dispatch。

### 6.4 RAW Socket

**文件**: `os/src/net/socket/inet/raw/raw.rs` (281 lines)

```rust
pub struct RawSocket {
    socket_handler: RouteSocketHandle,
    inner: Mutex<RawSocketInner>,
    recv_waiters: EventWaitQueue,
}
```

未实现 bind/listen/connect/accept (返回 EOPNOTSUPP)。

### 6.5 Unix Socket

**文件**: `os/src/net/socket/unix/` (7 files)

```rust
pub enum UnixEndpoint {
    Unnamed,                          // socketpair 创建的无名 socket
    Path(Arc<String>),                // 文件系统路径绑定
    Abstract(Arc<Vec<u8>>),           // Linux abstract namespace (@name)
}

pub static PATH_TABLE: Mutex<BTreeMap<String, Weak<UnixStreamSocket>>>;
```

SOCK_STREAM 和 SOCK_DGRAM 都支持。Stream socket 使用 `UnixStreamSocket` (带 inner 状态机), Datagram 使用 `ring_buffer` 传输。

### 6.6 Netlink

**文件**: `os/src/net/socket/netlink/` (3 files)

```rust
pub struct NetlinkSocket { /* ... */ }
```

NETLINK_ROUTE 支持: RTM_GETLINK / RTM_GETADDR / RTM_GETROUTE 的 dump 响应, NLMSG_DONE, NLMSG_ERROR。不支持 RTM_NEWROUTE / RTM_DELROUTE。

### 6.7 共享组件

**bound.rs**: `BoundInner` 记录 socket 的绑定元数据:
```rust
pub struct BoundInner {
    pub socket_handle: Option<RouteSocketHandle>,  // Lazy bind 时可为 None
    pub ifindex: u32, pub bound_addr: Option<IpAddress>, pub bound_port: u16,
}
```

**port.rs**: `PortManager` 管理 TCP/UDP 端口分配和冲突检测:
```rust
pub static TCP_PORTS: Mutex<BTreeMap<u16, PortBinding>>;
pub static UDP_PORTS: Mutex<BTreeMap<u16, Vec<UdpPortBinding>>>;
```

ephemeral 端口范围: 32768-60999。

**address.rs**: `SocketAddrv4/v6`, `listen_endpoint()`, `fill_with_endpoint()` 工具函数。

---

## 7. 系统调用层

**文件**: `os/src/net/syscall/mod.rs`

扁平 `match` 分发, 约 17 个系统调用:

| 系统调用 | 文件 | 说明 |
|---------|------|------|
| socket | socket.rs | `Socket::alloc(domain, type, protocol)` |
| bind | bind.rs | 端口绑定 + `is_local_bind_addr` 校验 |
| connect | connect.rs | TCP/UDP connect |
| listen | listen.rs | TCP listen |
| accept/accept4 | accept.rs | TCP accept |
| sendto | sendto.rs | UDP send |
| recvfrom | recvfrom.rs | UDP recv |
| sendmsg | sendmsg.rs | scatter-gather send |
| recvmsg | recvmsg.rs | scatter-gather recv |
| getsockname | getsockname.rs | 获取本地地址 |
| getpeername | getpeername.rs | 获取对端地址 |
| getsockopt | getsockopt.rs | 获取 socket 选项 |
| setsockopt | setsockopt.rs | 设置 socket 选项 |
| shutdown | shutdown.rs | SHUT_RD / SHUT_WR / SHUT_RDWR |
| socketpair | socketpair.rs | AF_UNIX socketpair |

**返回值约定**: 处理函数成功返回 `>= 0`, 失败返回负 errno (如 `-11` = EAGAIN)。

---

## 8. 后续规划

### 8.1 近期 (Phase 6 剩余)

| 功能 | 说明 |
|------|------|
| TCP wildcard fanout | `bind(0.0.0.0)` 时在每个活跃 iface 上创建 listen socket |
| UDP wildcard per-iface | wildcard bind 时每 iface 一个 smoltcp UDP socket |
| Route-layer local delivery | `connect(own_eth0_ip)` 不经过 smoltcp ARP, 直接注入目标栈 RX |
| SelfConnected 接入 | 同 socket self-connect (local == remote addr:port) |

### 8.2 中期

| 功能 | 说明 |
|------|------|
| 跨接口 IPv4 forwarding | `RouterEnableDevice::handle_routable_packet()` — TTL 递减, 路由查找 |
| ARP/Neighbor cache | 最小实现, 用于跨接口转发时的 MAC 解析 |
| 多 NIC 支持 | `NET_DEVICES: Vec<Arc<dyn NetDevice>>`, 动态注册 |
| IPv6 | smoltcp IPv6 + 邻居发现 |
| ICMP socket | ICMPv4 echo reply 等 |

### 8.3 远期

| 功能 | 说明 |
|------|------|
| Net namespace | 参考 DragonOS `NetNamespace` 设计 |
| NAT/ConnTrack | SNAT/DNAT + 连接跟踪表 |
| Bridge | 虚拟网桥 (参考 DragonOS bridge.rs) |
| NAPI | bounded poll + 中断合并 |
| Policy routing | 基于 fwmark / source 的路由 |
| TCP congestion control | smoltcp 当前不支持, 需上游 |

---

## 附录

### A. 全局静态变量

| 变量 | 位置 | 类型 | 说明 |
|------|------|------|------|
| `NET_INTERFACE` | config.rs | `NetInterface` | 单例多栈管理器 |
| `NET_DEVICE` | drivers/net/mod.rs | `Mutex<Option<Arc<dyn NetDevice>>>` | 物理 NIC |
| `IFACES` | net_core.rs | `Mutex<Vec<DeviceEntry>>` | 接口元数据 |
| `ETH0_CIDR` | net_core.rs | `Mutex<Option<IpCidr>>` | DHCP IP |
| `DEFAULT_GW` | net_core.rs | `Mutex<Option<Ipv4Address>>` | DHCP 网关 |
| `TCP_PORTS` | inet/common/port.rs | `Mutex<BTreeMap<u16, PortBinding>>` | TCP 端口表 |
| `UDP_PORTS` | inet/common/port.rs | `Mutex<BTreeMap<u16, Vec<UdpPortBinding>>>` | UDP 端口表 |
| `TCP_SOCKETS` | socket/mod.rs | `Mutex<Vec<Weak<TcpSocket>>>` | 全局 TCP socket 列表 |
| `UDP_SOCKETS` | socket/mod.rs | `Mutex<Vec<Weak<UdpSocket>>>` | 全局 UDP socket 列表 |
| `RAW_SOCKETS` | socket/mod.rs | `Mutex<Vec<(RouteSocketHandle, Weak<RawSocket>)>>` | 全局 RAW socket 列表 |

### B. 跨文件关键路径

| 操作 | 调用链 |
|------|--------|
| TCP 发送 | `sys_sendto` → `TcpSocket::try_send` → `with_tcp_mut` → `NET_INTERFACE.tcp_routed_socket` → binding lookup → `stack.sockets.get_mut::<tcp::Socket>` |
| TCP 连接 | `sys_connect` → `Inner::connect` → `route_output` (选 ifindex) → `add_routed_socket` (附着) → `tcp_connect` → smoltcp `connect(context)` |
| UDP 本地投递 | `sys_sendto` → `UdpSocket::try_sendmsg` → `try_deliver_local` → 查 IFACES → peer.rx_queue.push |
| DHCP | `NetInterfaceInner::new()` → 创建 dhcpv4 Socket → poll loop → `net_core::set_eth0_ipv4` |
| Poll | timer ISR → `try_poll` → `poll_once` → 遍历 stacks → `iface.poll` → `dispatch_udp_packets` → wake waiters |
