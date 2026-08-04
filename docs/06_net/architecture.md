---
title: "网络子系统架构设计"
module: "net"
category: net
status: draft
owner: MangoCore Team
last_updated: 2026-08-04
code_paths:
  - "os/src/net/mod.rs"
  - "os/src/net/config.rs"
  - "os/src/net/routing.rs"
  - "os/src/net/adapter.rs"
entry_points:
  - "rust_main() -> net::config::init()"
  - "NET_INTERFACE"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "socket01"
    - "connect01"
  oscomp:
    - "basic"
    - "busybox"
related_docs:
  - "docs/06_net/device-stack-and-poll.md"
  - "docs/06_net/device-adapter.md"
  - "docs/06_net/routing.md"
  - "docs/06_net/socket-trait-and-fd.md"
  - "docs/06_net/syscall-layer.md"
---

# 网络子系统架构设计

## 1. 概述

网络子系统是 Mango 内核中负责 TCP/IP 协议栈、路由、数据包收发以及网络设备管理的核心模块。它基于 smoltcp 库（Rust 生态中的嵌入式 TCP/IP 协议栈），在 `#![no_std]` 环境中提供 POSIX socket API（AF_INET / AF_INET6 / AF_UNIX / AF_NETLINK / AF_PACKET）的实现。

整个子系统位于 `os/src/net/` 目录下，共约 13 个模块：

| 模块 | 职责 | 参考文档 |
|------|------|---------|
| `socket/` | 所有 Socket 类型的 trait 定义和具体实现（TCP/UDP/RAW/Unix/Netlink/Packet） | — |
| `config.rs` | NetInterface 全局单例，per-device smoltcp 栈管理，poll 循环 | [device-stack-and-poll.md](device-stack-and-poll.md) |
| `routing.rs` | 路由表、RouteSocketHandle、route_output 查路由 | [routing.md](routing.md) |
| `adapter.rs` | smoltcp Device trait 适配层（IfaceDevice 枚举 + SmoltcpDeviceAdapter） | [device-adapter.md](device-adapter.md) |
| `net_core.rs` | 设备注册中心，DHCP 状态，netns 辅助函数 | [net-core-iface.md](net-core-iface.md), [dhcp.md](dhcp.md) |
| `iface.rs` | Iface trait 定义（统一的网络接口抽象） | [net-core-iface.md](net-core-iface.md) |
| `syscall/` | syscall 分发层（socket/bind/connect/sendto/recvfrom 等） | — |
| `neighbour.rs` | ARP/NDP 邻居表 | [neighbour.md](neighbour.md) |
| `ioctl.rs` | SIOCGIFxxx ioctl 分发 | [net-core-iface.md](net-core-iface.md) |
| `posix.rs` | POSIX socket 参数解析 | — |

> 各模块的详细实现说明已从原 `smoltcp-device-routing.md` 拆分为 6 篇专题文档，见上表参考文档列。如需深入阅读，可直接跳转对应文档。

---

## 2. 设计目标

1. **POSIX 兼容**：提供 `socket`、`bind`、`connect`、`sendto`、`recvfrom`、`epoll` 等标准接口，运行 unixbench、iperf、libcbench 等测试套件。
2. **双架构支持**：同一套代码在 riscv64 和 loongarch64 上均可运行，HAL 层隔离架构差异。
3. **可扩展设备模型**：支持 lo（环回）、eth0（virtio-net）、veth（虚拟以太网对）等多种设备类型，每种设备拥有独立的 smoltcp 栈。
4. **响应式轮询**：单核环境下通过计时器中断驱动轮询，非阻塞 I/O 路径确保系统不被网络卡死。
5. **分离的数据平面与控制平面**：Socket handle 与 RouteSocketHandle 双层抽象，将用户态 socket fd 与底层 smoltcp socket 解耦。

### SMP 已完成边界

网络适配已完成 `NetNamespace::ports` 的 reserve→bind→commit/abort、N0 route directory 的 route-ID 重验、单 N2 `DeviceStack` 和 generation poll worker。端口 registry、route directory 与 DeviceStack 不嵌套；socket/epoll 通知与用户 copy 都在业务锁外。这不承诺单个 smoltcp `SocketSet` 的 per-socket 并行，也不代表 eventpoll/fd-table、netlink 或 RAW edge cases 已完成跨子系统 SMP 审计。

---

## 3. 架构

### 3.1 三层架构总览

```
┌──────────────────────────────────────────────────────────────────────┐
│  Protocol Layer — Socket trait + socket/*                           │
│                                                                      │
│  TcpSocket    UdpSocket    RawSocket    UnixSocket    PacketSocket   │
│  NetlinkSocket                                                        │
│                                                                      │
│  每个 Socket 类型实现 Socket trait，封装 POSIX 语义：                   │
│  bind / connect / listen / accept / send_to / try_send / try_recv     │
└──────────────────────────┬───────────────────────────────────────────┘
                            │ RouteSocketHandle 间接
                            ▼
┌──────────────────────────────────────────────────────────────────────┐
│  Routing Layer — routing.rs + NetInterfaceInner                     │
│                                                                      │
│  核心职责：                                                            │
│  - route_output(dst) → RouteDecision {ifindex, source, next_hop}     │
│  - RouteSocketHandle → SocketBinding {ifindex, handle, proto}        │
│  - poll 循环：驱动所有 DeviceStack 的 iface.poll()                    │
│  - 清理/重建 socket binding（TCP Closed 去除、UDP rebind 等）          │
└──────────────────────────┬───────────────────────────────────────────┘
                            │ DeviceStack 数组（per-device smoltcp 栈）
                            ▼
┌──────────────────────────────────────────────────────────────────────┐
│  Device Layer — adapter.rs + iface.rs + drivers/                    │
│                                                                      │
│  DeviceStack {nic: Arc<dyn Iface>, device: IfaceDevice,             │
│              iface: Interface, sockets: SocketSet}                   │
│                                                                      │
│  IfaceDevice 枚举：Lo(Loopback) | Eth(SmoltcpDeviceAdapter)          │
│                 | Veth(VethDriver)                                   │
│                                                                      │
│  底层驱动：VirtIONetWrapper → NetDevice trait → SmoltcpDeviceAdapter  │
└──────────────────────────────────────────────────────────────────────┘
```

### 3.2 核心抽象：RouteSocketHandle

`RouteSocketHandle` 是整个三层架构的粘合剂。它将用户态 socket 操作间接转发到正确的 smoltcp socket，而不暴露底层的 `SocketHandle` 和 `ifindex`。

```rust
/// 路由层 socket 句柄（对用户 Socket 透明）
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RouteSocketHandle(pub(crate) usize);

/// 从 RouteSocketHandle 到 smoltcp SocketHandle 的映射
#[derive(Clone, Copy, Debug)]
pub(crate) struct SocketBinding {
    pub ifindex: u32,              // 所属 DeviceStack
    pub handle: SocketHandle,       // smoltcp 内部 handle
    pub proto: InetProtocol,        // Tcp / Udp / Raw
}

/// 每个 NET_INTERFACE 内部维护一个 BTreeMap:
/// bindings: BTreeMap<RouteSocketHandle, SocketBinding>
```

当 `TcpSocket::try_send(buf)` 被调用时：

1. `TcpSocket` 持有 `socket_handler: RouteSocketHandle`。
2. 调用 `NET_INTERFACE.tcp_routed_socket(rh, |tcp_sock| ...)`。
3. 内部查找 `bindings[rh]` 得到 `SocketBinding{ifindex, handle, Tcp}`。
4. 通过 `stack_mut(ifindex)` 定位 DeviceStack，再通过 `sockets.get_mut::<tcp::Socket>(handle)` 获取真实 socket。
5. 闭包操作真实的 smoltcp tcp::Socket。

这个抽象实现了两层解耦：
- 用户态的 `Arc<TcpSocket>` 不依赖 DeviceStack 的布局。
- 跨 DeviceStack 迁移（如 UDP rebind）只需更新 `SocketBinding`，不改变用户态对象。

### 3.3 每设备独立 smoltcp 栈

每个网络设备拥有自己独立的 smoltcp `Interface` 和 `SocketSet`，由 `DeviceStack` 封装：

```
NetInterface
  └── inner: Mutex<NetInterfaceInner>
        ├── stacks: Vec<DeviceStack>          ← 多个设备栈
        ├── bindings: BTreeMap<...>           ← 路由映射
        └── next_socket_id: usize

DeviceStack (index 0: lo, ifindex=1)
  ├── nic: Arc<dyn Iface>                     ← loopback 元数据
  ├── device: IfaceDevice::Lo(Loopback)       ← smoltcp Device
  ├── iface: smoltcp::Interface               ← IP 层状态
  └── sockets: SocketSet                      ← 该设备的 socket 集合

DeviceStack (index 1: eth0, ifindex=2)
  ├── nic: Arc<dyn Iface>                     ← eth0 元数据
  ├── device: IfaceDevice::Eth(SmoltcpDeviceAdapter)
  ├── iface: smoltcp::Interface
  └── sockets: SocketSet
```

为什么采用 per-device 而非共享一个 Interface？

- **环回与物理隔离**：lo 的 Loopback 设备直接提供 `Medium::Ip`，不需要 ARP；eth0 的 `SmoltcpDeviceAdapter` 基于 VirtIO 硬件，需要 `Medium::Ethernet`。
- **独立 IP 地址和路由**：每个 Interface 维护自己的 IP 地址列表、ARP 缓存、路由表。
- **故障隔离**：一个设备栈中的 socket 异常关闭不会影响其他设备栈。

---

## 4. 关键数据结构

### 4.1 NetInterface

```rust
pub static NET_INTERFACE: NetInterface = NetInterface::new();

pub struct NetInterface<'a> {
    inner: Mutex<Option<NetInterfaceInner<'a>>>,
}
```

全局单例，是网络子系统的入口点。`inner` 在 `NetInterface::init()` 调用前为 `None`，确保在 init 之前任何网络操作都返回空。

### 4.2 NetInterfaceInner

```rust
pub struct NetInterfaceInner<'a> {
    pub stacks: Vec<DeviceStack<'a>>,
    pub bindings: BTreeMap<RouteSocketHandle, SocketBinding>,
    pub next_socket_id: usize,
}
```

- `stacks[0]`：环回设备 lo（ifindex=1）。
- `stacks[1]`：以太网设备 eth0（ifindex=2）。
- `bindings`：RouteSocketHandle 到 SocketBinding 的映射。支持 O(log n) 查找。
- `next_socket_id`：单调递增的 RouteSocketHandle 计数器。

### 4.3 DeviceStack

```rust
pub struct DeviceStack<'a> {
    pub nic: Arc<dyn Iface>,
    pub device: IfaceDevice,
    pub iface: Interface,
    pub sockets: SocketSet<'a>,
}
```

每个 DeviceStack 代表一个完整的网络设备栈。

- `nic`：元数据接口（名称、MAC、IP 地址、flags）。通过 `Iface` trait 访问。
- `device`：smoltcp 硬件抽象层。`IfaceDevice` 枚举包装了 `Loopback`、`SmoltcpDeviceAdapter` 和 `VethDriver` 三种设备类型。
- `iface`：smoltcp 的 `Interface`——IP 层状态机，负责 IP 分片、ARP、路由查找。
- `sockets`：挂载在该设备上的所有 smoltcp socket（TcpSocket、UdpSocket、RawSocket、Dhcpv4Socket 等）。

### 4.4 RouteSocketHandle

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RouteSocketHandle(pub(crate) usize);

#[derive(Clone, Copy, Debug)]
pub(crate) struct SocketBinding {
    pub ifindex: u32,
    pub handle: SocketHandle,
    pub proto: InetProtocol,
}
```

RouteSocketHandle 是递增整数，作为键用于 `BTreeMap` 查找。SocketBinding 记录该 handle 在哪个 DeviceStack 的哪个位置。

### 4.5 RouteDecision

```rust
#[derive(Clone, Debug)]
pub struct RouteDecision {
    pub ifindex: u32,
    pub source: IpAddress,
    pub next_hop: Option<IpAddress>,
    pub is_local: bool,
}
```

`route_output(dest_ip)` 返回的决策结果：
- `ifindex`：数据应走哪个设备。
- `source`：该设备的源 IP 地址。
- `next_hop`：网关 IP（直连路由为 None）。
- `is_local`：目标是否为本机地址（环回或本地 IP）。

---

## 5. 执行流程

### 5.1 启动流程

```
QEMU → OpenSBI (M-mode)
  → entry.asm (S-mode)
    → rust_main()
      → console::init()
      → mm::init()
      → drivers::init_net_device()         [1]
      → net::config::init()                [2]
      → task::add_initproc()
      → task::run_tasks()
```

**步骤详解：**

**[1] drivers::init_net_device()**

```rust
// os/src/drivers/net/mod.rs
pub fn init_net_device() {
    if let Some(net_dev) = virtio_net::VirtIONetWrapper::new() {
        *NET_DEVICE.lock() = Some(Arc::new(net_dev));
    }
}
```

创建 VirtIO 网络设备，包装为 `VirtIONetWrapper`，存储到全局 `NET_DEVICE` 静态变量。如果 QEMU 未提供 virtio-net 设备，则 `NET_DEVICE` 为 `None`。

**[2] net::config::init()**

```rust
// os/src/net/config.rs
pub fn init() {
    let has_nic = NET_DEVICE.lock().is_some();
    net_core::init();         // 注册 lo 和 eth0 到 netns 设备列表
    NET_INTERFACE.init();     // 创建 DeviceStack，DHCP 探测
}
```

`net_core::init()` 执行：
1. 创建 ifindex=1 的环回设备 `lo`（IP: 127.0.0.1/8, ::1/128）。
2. 若 `NET_DEVICE` 存在，创建 ifindex=2 的以太网设备 `eth0`（MAC 来自硬件，无静态 IP）。

`NET_INTERFACE.init()` 执行 `NetInterfaceInner::new()`：

1. **构建 lo 的 DeviceStack**：
   - 使用 `IfaceDevice::Lo(Loopback::new(Medium::Ip))`。
   - smoltcp 配置为 `HardwareAddress::Ip`。
   - 添加 IP：127.0.0.1/8 和 ::1/128。

2. **构建 eth0 的 DeviceStack**：
   - 使用 `SmoltcpDeviceAdapter::new(NET_DEVICE.take())`。
   - smoltcp 配置为 `HardwareAddress::Ethernet(mac)`。

3. **DHCP 探测**（eth0 有硬件时）：

```rust
// DHCP 探测伪代码
let mut dhcp_socket = dhcpv4::Socket::new();
dhcp_socket.set_retry_config(/* 2s discover, 1s request, 3 retries */);
let dhcp_handle = eth_sockets.add(dhcp_socket);

loop {
    eth_iface.poll(timestamp, &mut eth_device, &mut eth_sockets);
    match dhcp_socket.poll() {
        Some(Event::Configured(cfg)) => {
            set_eth0_ipv4(cfg.address);
            set_default_gateway(cfg.router);
            break;
        }
        _ => {}
    }
    if timeout(5s) { break; }
}
eth_sockets.remove(dhcp_handle);
```

4. **注入 DHCP 结果**：将从 netns 收集到的 IP 地址和默认网关写入 eth0 的 smoltcp `Interface`。

### 5.2 TCP 发送

以 `sys_sendto(sockfd, buf, len, flags, dest_addr, addrlen)` 为例，目标地址为远程主机（非本机）。

```
sys_sendto
  │
  ├─ 通过 fd 表查找 SocketFile → SocketFile.inner: Arc<dyn Socket>
  │
  ├─ 调用 Socket::try_send(buf, flags)
  │    └─ TcpSocket::try_send(buf, _flags)
  │         ├─ NET_INTERFACE.try_poll()      [1] 轻量轮询，推进发送
  │         ├─ 检查 write_shutdown
  │         ├─ 如果处于 Connecting，先 try_connect
  │         └─ inner.try_send(buf)
  │              └─ Established::send_slice(buf)
  │                   └─ with_tcp_mut(handle, |socket| {
  │                        socket.send_slice(buf)   [2] 写入 smoltcp 发送缓冲区
  │                      })
  │
  └─ [完整的路径还包括 poll 循环触发实际发送]

[1] NET_INTERFACE.try_poll()
  → inner.try_lock() 防止重入
  → poll_once()
    → 对每个 DeviceStack:
      → iface.poll(timestamp, device, sockets)   [3] smoltcp 驱动发送
        → device.transmit() → SmoltcpDeviceAdapter::transmit()
          → NetTxToken::consume(len, |buf| { ... })
            → self.inner.transmit(&buf)            [4] VirtIONetWrapper::transmit()

[2] socket.send_slice(buf):
  → 将用户数据拷贝到 smoltcp tcp::Socket 的内部发送缓冲区
  → TCP 分段 / 窗口管理由 smoltcp 内部处理

[3] iface.poll():
  → smoltcp 从 tcp::Socket 发送缓冲区读取数据
  → 构造 IP 头 + TCP 头
  → 获取 device 的 TxToken
  → TxToken::consume() → 写以太网帧到硬件

[4] VirtIONetWrapper::transmit():
  → 将完整以太网帧写入 VirtIO 设备的可用环 (available ring)
  → QEMU 侧接收并在虚拟网络中传输
```

整体链路的持有关系：

```
Task (user space)
  │ file descriptor: SocketFile
  │   └── Arc<TcpSocket>
  │         └── RouteSocketHandle(id=42)
  │               └── NetInterfaceInner.bindings[RH(42)]
  │                     └── SocketBinding {ifindex=2, handle=H5, Tcp}
  │                           └── DeviceStack[1].sockets.get(H5): &mut tcp::Socket
```

### 5.3 UDP 本地投递

UDP 套接字在向本地地址（127.0.0.1 或本机 IP）发送时，绕过 smoltcp 协议栈，直接在内核内部完成数据交付。这是性能优化的关键路径。

```
syscall_sendto(sockfd, buf, len, flags, dest_addr, addrlen)
  │
  └─ UdpSocket::try_sendmsg(buf, dest, flags)
       │
       ├─ 解析目标地址 → remote: IpEndpoint
       │
       ├─ try_deliver_local(remote, data)             [1]
       │    │
       │    ├─ is_local_udp_destination(remote.addr)?  [2]
       │    │    ├─ 127.x.x.x? → true
       │    │    ├─ 本机 IP?   → true
       │    │    └─ 其他       → false → return Ok(None)
       │    │
       │    ├─ 计算 source endpoint
       │    │
       │    ├─ find_local_udp_recipient(remote, src)   [3]
       │    │    └─ 遍历 UDP_SOCKETS 全局列表
       │    │         ├─ 匹配 local_endpoint.port == remote.port
       │    │         ├─ 匹配 remote_endpoint（已 connect 的 socket）== src
       │    │         └─ 得分最高的 → 返回 Arc<UdpSocket>
       │    │
       │    ├─ peer.inner.rx_queue.push_back((data, src))
       │    └─ peer.recv_waiters.notify_events_all(EPOLLIN)
       │
       └─ 未本地交付 → 走 smoltcp 正常发送路径              [4]
            ├─ route_check(remote.addr) → ENETUNREACH?
            ├─ rebind_routed_udp(目标 ifindex)
            └─ udp_routed_socket → socket.send_slice(...)

[1] try_deliver_local 是 UdpSocket 上的方法，在 try_send 和 try_sendmsg
    的 smoltcp 路径之前调用。

[2] is_local_udp_destination 判定：
    - 127.0.0.1/8
    - ::1
    - 当前 netns 中任一设备配置的 IP 地址

[3] find_local_udp_recipient 使用评分制选择最匹配的接收 socket：
    - addr_score: 本地地址精确匹配=2，未指定=1，其他=0
    - peer_score: 远端端点精确匹配=2，未指定=1，不匹配=0
    - 总分最高者胜出

[4] 非本地 UDP 走标准 smoltcp 发送路径：
    udp_routed_socket(rh, |socket| socket.send_slice(data, meta))
```

为什么要有本地交付旁路？UDP 环回如果走 smoltcp 完整路径（Loopback device 到套接字再到应用），需要经历完整的以太网/IP/UDP 封装和解封装。内核内本地交付直接将数据从发送方的 rx_queue 放入接收方的 rx_queue，避免了协议栈开销，同时避免了死锁——因为 smoltcp 的 `poll()` 可能在同一个线程持有锁。

### 5.4 Poll 循环

网络轮询由 `NetInterface::poll()` 或 `try_poll()` 触发。`poll()` 在有锁时阻塞等待，`try_poll()` 在锁被持有时不等待直接返回。

**驱动时机**：
- 每次 socket 读写系统调用时会调用 `NET_INTERFACE.try_poll()`。
- 系统空转时由定时器中断触发轮询。

**poll_once 详细步骤（4 阶段）**：

```
poll_once()
  │
  ├── [阶段 0] 收集待移除的 socket
  │     ├─ UDP_SOCKETS_TO_REMOVE  → (rh, ifindex) 映射
  │     └─ TCP_SOCKETS_TO_REMOVE  → (rh, ifindex) 映射
  │
  ├── [阶段 1-4] 逐 DeviceStack 处理
  │
  │   for each stack in stacks:
  │   │
  │   ├── [1] 清理 UDP socket
  │   │     for rh in udp_removes:
  │   │       if rh.ifindex == stack.nic.nic_id:
  │   │         stack.sockets.remove(rh.handle)
  │   │         bindings.remove(rh)
  │   │
  │   ├── [1.5] Veth 帧分发（仅在 veth 设备上）
  │   │     deliver_frames_from_veth_queue(nic_id, rx_queue)
  │   │
  │   ├── [2] smoltcp 协议栈推进
  │   │     stack.iface.poll(timestamp, device, sockets)
  │   │     → 处理入站帧、ARP 解析、TCP 重传、UDP 发送等
  │   │
  │   ├── [3] 清理 TCP socket（必须确认 Closed 状态）
  │   │     for rh in tcp_removes:
  │   │       if socket.state() == Closed:
  │   │         stack.sockets.remove(rh.handle)
  │   │         bindings.remove(rh)
  │   │       else:
  │   │         TCP_SOCKETS_TO_REMOVE.push(rh)  // 下次再试
  │   │
  │   └── [4] UDP 数据分发
  │         dispatch_udp_packets(stack.sockets)
  │         → 从 smoltcp udp::Socket recv() 数据
  │         → 通过 find_best_match 匹配 OS UdpSocket
  │         → 推入 OS UdpSocket.rx_queue
  │         → 唤醒接收等待队列
  │
  └── [阶段 5] 全局唤醒
        ├─ wake_tcp_waiters(): 遍历 TCP_SOCKETS，唤醒就绪 socket
        └─ wake_raw_waiters(): 遍历 RAW_SOCKETS，唤醒可读 socket
```

**关键设计要点**：

1. **惰性清理**：TCP socket 的清理必须等待 smoltcp 状态机进入 `Closed`，确保四次挥手完成。未完成的 socket 重新放回 `TCP_SOCKETS_TO_REMOVE` 等待下次轮询。

2. **UDP 双缓冲机制**：OS 层的 `UdpSocket` 有自己的 `rx_queue: VecDeque<(Vec<u8>, IpEndpoint)>`。`dispatch_udp_packets` 将 smoltcp udp::Socket 的数据批量抽干到 OS 层队列中，之后 OS 路径不再需要 smoltcp 锁。

3. **避免重入**：`try_poll()` 使用 `try_lock()` 尝试获取 `inner` 锁，如果锁已被持有（如 syscall 路径中嵌套调用），则跳过本次轮询。这防止了单线程环境下的死锁。

4. **Veth 帧预分发**：在 smoltcp `poll()` 之前，veth 设备的 RX 队列中的原始帧被提前递交给 packet socket（AF_PACKET），确保类 wireshark 的 socket 不会错过任何帧。

---

## 6. 接口与 API

### 6.1 NetInterface 公开方法

| 方法 | 说明 |
|------|------|
| `init()` | 初始化 NetInterfaceInner，创建 lo 和 eth0 的 DeviceStack |
| `add_socket(ifindex, socket)` | 在指定 DeviceStack 添加 smoltcp socket |
| `add_routed_socket(proto, socket)` | 创建路由 socket（自动绑定到默认设备） |
| `add_routed_socket_on(proto, socket, ifindex)` | 在指定设备上创建路由 socket |
| `tcp_routed_socket(rh, f)` | 通过 RouteSocketHandle 访问 tcp::Socket |
| `udp_routed_socket(rh, f)` | 通过 RouteSocketHandle 访问 udp::Socket |
| `raw_routed_socket(rh, f)` | 通过 RouteSocketHandle 访问 raw::Socket |
| `tcp_connect(rh, remote, local)` | 发起 TCP 连接 |
| `remove_routed(rh)` | 移除路由 socket（从 SocketSet 和 bindings 中移除） |
| `rebind_routed_udp(rh, new_ifindex)` | 将 UDP socket 迁移到另一个设备栈 |
| `rebind_routed_raw(rh, new_ifindex, ...)` | 将 RAW socket 迁移到另一个设备栈 |
| `tcp_socket(handler, ifindex, f)` | 直接通过 SocketHandle 访问 tcp::Socket |
| `udp_socket(handler, ifindex, f)` | 直接通过 SocketHandle 访问 udp::Socket |
| `raw_socket(handler, ifindex, f)` | 直接通过 SocketHandle 访问 raw::Socket |
| `inner_handler(f)` | 通用闭包访问 NetInterfaceInner |
| `poll()` | 阻塞式轮询（等待锁） |
| `try_poll()` | 非阻塞轮询（锁被持有时跳过） |
| `poll_until_quiescent()` | 反复 try_poll 直到无数据 |
| `remove(handler, ifindex)` | 从指定 DeviceStack 移除 socket |
| `add_veth_stack(nic, device)` | 注册 veth DeviceStack |
| `remove_veth_stack(nic_id)` | 移除 veth DeviceStack |
| `add_ip_to_stack(ifindex, cidr)` | 同步 IP 到 smoltcp Interface |
| `remove_ip_from_stack(ifindex, cidr)` | 从 smoltcp Interface 移除 IP |
| `stack_ifindexes()` | 列出所有注册的 DeviceStack 的 ifindex |
| `socket_stats()` | 返回 (tcp_count, udp_count, raw_count, pending_remove) |

### 6.2 Iface trait 方法

```rust
pub trait Iface: Send + Sync + fmt::Debug {
    fn nic_id(&self) -> usize;
    fn iface_name(&self) -> String;
    fn set_iface_name(&self, name: &str);
    fn flags(&self) -> u32;
    fn set_flags(&self, flags: u32);
    fn mtu(&self) -> usize;
    fn set_mtu(&self, mtu: usize);
    fn ip_addrs(&self) -> Vec<IpCidr>;
    fn add_ip_addr(&self, addr: IpCidr);
    fn del_ip_addr(&self, addr: IpCidr);
    fn mac(&self) -> [u8; 6];
    fn kind(&self) -> DeviceKind;
    fn peer_ifindex(&self) -> Option<usize>;
    fn common(&self) -> &IfaceCommon;
    fn as_smoltcp_device(&self) -> &dyn SmoltcpDeviceAccess;
}
```

`Iface` 是网络设备的统一接口。`NetDeviceEntry` 是其主要实现，包装了 lo 和 eth0 的元数据（名称、MAC、IP、MTU、flags、操作状态）。

### 6.3 邻居管理接口

ARP/NDP 邻居表位于 `os/src/net/neighbour.rs`，在 poll 循环中通过 NetRxToken 的 `consume` 方法触发：

```rust
// NetRxToken::consume() 中
let ifindex = *CURRENT_POLL_IFINDEX.lock();
try_capture_arp_reply(&self.buf, ifindex);
```

`CURRENT_POLL_IFINDEX` 在每个 DeviceStack 轮询前设置，确保 ARP 响应帧关联到正确的接口。

---

## 7. 测试映射

| 功能域 | 测试 | 说明 |
|--------|------|------|
| TCP 收发 | libcbench TCP 吞吐测试 | `os_test.conf mask=0x080` (libcbench) |
| UDP 收发 | libcbench UDP 延迟测试 | `os_test.conf mask=0x080` (libcbench) |
| 环回 | basic ping 127.0.0.1 | `os_test.conf mask=0x001` |
| 并发连接 | unixbench 网络相关测试 | `os_test.conf mask=0x020` |
| DHCP | busybox udhcpc | 内核内 DHCP 在启动阶段完成 |
| ARP | busybox ping 同网段 IP | 依赖 neighbour.rs |
| epoll 加网络 | libcbench epoll 测试 | 全测试集 |
| 多设备 | veth 对测试 | `add_veth_stack` 和 `remove_veth_stack` |
| 路由 | route_output 加 route_check | QEMU 集成验证 (通过 ping/iperf 间接覆盖) |
| Socket 迁移 | rebind_routed_udp 跨设备栈 | QEMU 集成验证 (跨接口 UDP 通信) |

---

## 8. 已知问题

1. **单核限制**：当前系统为单核，`NET_INTERFACE.inner` 使用 `spin::Mutex`。多核环境下需要改为更细粒度的锁或无锁结构。

2. **TCP 清理延迟**：`TCP_SOCKETS_TO_REMOVE` 循环重试可能导致极少数情况下 socket 销毁延迟 1 到 2 个 poll 周期。这通常不会影响功能，但在大量短连接场景（连接数每秒超过 1000）下可能积压。

3. **UDP 接收缓冲区竞争**：`dispatch_udp_packets` 和 `try_deliver_local` 各自操作 OS `UdpSocket.rx_queue`，但两者不同时执行（dispatch 在 poll 中，try_deliver_local 在 syscall 路径中），目前是安全的。未来添加多核支持时需要互斥。

4. **DHCP 超时无回退**：DHCP 探测超时（5 秒）后 eth0 无 IP 地址，系统继续运行但网络不可用。没有后续重试或无状态地址自动配置机制。

5. **ARP 表老化机制缺失**：`NEIGHBOUR_TABLE` 是全局持久表（`BTreeMap`），条目不会自动过期。删除依赖 netlink `RTM_DELNEIGH` 或手动干预，缺少 Linux 内核的周期性 NUD 超时回收机制。高 ARP 压力场景下条目可能持续膨胀。

---

## 9. 参考资料

- [smoltcp 文档](https://docs.rs/smoltcp/) - Rust 嵌入式 TCP/IP 协议栈 API
- [DragonOS 网络架构](https://github.com/DragonOS-Community/DragonOS) - VFS/MountFS 加网络子系统架构参考
- [Linux 网络栈详解](https://www.linuxfoundation.org/) - 三层模型（socket 到 routing 到 device）
- [POSIX socket API](https://pubs.opengroup.org/onlinepubs/9699919799/) - sys/socket.h 标准
- [VirtIO 网络规范](https://docs.oasis-open.org/virtio/virtio/v1.2/) - virtio-net 设备驱动接口
- `AGENTS.md` - 项目整体架构指南，见网络栈和系统调用章节
- `docs/06_net/` - 网络子系统各模块的详细文档
