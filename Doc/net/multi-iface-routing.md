# 多接口路由架构

> 文件: `os/src/net/routing.rs` (300+ lines), `os/src/net/config.rs` (530+ lines)
> 设计参考: DragonOS `kernel/src/net/routing/`, Linux `net/ipv4/route.c`

## 架构演进

```
Phase 1-2:  单 smoltcp Interface + 单 SocketSet + RoutingDevice 软件交换机
Phase 3:    协议层引入 RouteSocketHandle, 脱离原始 SocketHandle
Phase 4:    DeviceStack 包装 (单栈)
Phase 5:    拆分 lo 和 eth0 为独立 DeviceStack
Phase 6:    TCP lazy bind (bind 不附着, connect 按路由选栈)
```

## 核心类型

### RouteSocketHandle

```rust
// routing.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RouteSocketHandle(pub(crate) usize);
```

不透明令牌。协议层持有, 路由层内部映射到 `{ifindex, SocketHandle}`。协议层代码**永不**导入 `smoltcp::iface::SocketHandle`。

### SocketBinding (路由层内部)

```rust
pub(crate) struct SocketBinding {
    pub ifindex: u32,        // 所属设备栈 (1=lo, 2=eth0)
    pub handle: SocketHandle, // smoltcp 内部句柄
    pub proto: InetProtocol,  // Tcp / Udp / Raw
}
```

### DeviceStack

```rust
// config.rs
pub struct DeviceStack<'a> {
    pub ifindex: u32,
    pub name: &'static str,
    pub device: IfaceDevice,
    pub iface: Interface,       // smoltcp Interface (per-device)
    pub sockets: SocketSet<'a>, // smoltcp SocketSet (per-device)
}
```

### Router + RouteTable

```rust
// routing.rs
pub struct Router { pub(crate) table: RouteTable }

pub struct RouteTable { pub entries: Vec<RouteEntry> }

pub struct RouteEntry {
    pub destination: IpCidr,
    pub next_hop: Option<IpAddress>,
    pub ifindex: u32, pub metric: u32, pub route_type: RouteType,
}
```

### route_output() — 统一路由 API

```rust
pub fn route_output(dest: IpAddress) -> Result<RouteDecision, SyscallErr>;
```

处理逻辑:
1. 检查 `dest` 是否为本地地址 (`IFACES` 中查找) → `is_local: true`
2. 检查 `127.x.x.x` → ifindex=1 (loopback)
3. 查 FIB (最长前缀匹配) → 返回 RouteDecision
4. 无路由 → `Err(ENETUNREACH)`

## 数据流: TCP connect 如何选栈

```
sys_connect(sockfd, &remote_addr)
  → socket.bind()                 // Bound { socket: Box<tcp::Socket>, local }
  → Inner::connect(remote)
    → route_output(remote.addr)   // 返回 RouteDecision { ifindex, source, ... }
    → NET_INTERFACE.add_routed_socket(InetProtocol::Tcp, socket)
      → ifindex = route.ifindex   // eth0=2 或 lo=1
      → stack = stack_mut(ifindex)
      → handle = stack.sockets.add(socket)
      → bindings.insert(RouteSocketHandle(id), SocketBinding { ifindex, handle, Tcp })
    → NET_INTERFACE.tcp_connect(route_handle, remote, local)
      → stack = stack_mut(binding.ifindex)
      → socket = stack.sockets.get_mut::<tcp::Socket>(binding.handle)
      → socket.connect(stack.iface.context(), remote, local)
```

## Poll 编排

```
poll_once():
  1. 收集 UDP/TCP_SOCKETS_TO_REMOVE 队列 (按 ifindex 分组)
  2. 遍历每个 DeviceStack:
     a. UDP 清理: 仅处理属于本栈的 handle
     b. smoltcp poll: stack.iface.poll(timestamp, &mut stack.device, &mut stack.sockets)
     c. TCP 清理: 检查 Closed 状态, 仅处理属于本栈的 handle
     d. dispatch_udp_packets(&mut stack.sockets)
  3. update_io_events (所有 TCP socket)
  4. wake_tcp_waiters + wake_raw_waiters
```

## DHCP 探测

```rust
// 在 NetInterfaceInner::new() 中, 仅对 eth0 栈执行
let mut dhcp_socket = dhcpv4::Socket::new();
dhcp_socket.set_retry_config(RetryConfig {
    discover_timeout: Duration::from_secs(2),
    initial_request_timeout: Duration::from_secs(1),
    request_retries: 3,
    ...
});
let dhcp_handle = eth_sockets.add(dhcp_socket);

loop {
    eth_iface.poll(timestamp, &mut eth_device, &mut eth_sockets);
    let event = eth_sockets.get_mut::<dhcpv4::Socket>(dhcp_handle).poll();
    match event {
        Some(Configured(cfg)) => {
            net_core::set_eth0_ipv4(IpCidr::Ipv4(cfg.address));
            net_core::set_default_gateway(cfg.router);
            break;
        }
        ...
    }
    if timestamp >= deadline { break; }  // 5s 超时
}
eth_sockets.remove(dhcp_handle);
```

## 关键设计决策

| 决策 | 原因 |
|------|------|
| RouteSocketHandle 而非 SocketKey | 协议层不应感知 ifindex, 由路由层内部管理映射 |
| IfaceDevice enum (非 trait object) | smoltcp Device trait 有 GAT, 不能装箱 |
| 全局 port table (非 per-device) | Linux 语义: 同 netns 内按 (addr, port) 冲突 |
| TCP lazy bind | bind(0.0.0.0) 时不知后续 connect 目标, connect 时才选栈 |
| DHCP 仅 eth0 | lo 固定 127.0.0.1/8, 无需 DHCP |
