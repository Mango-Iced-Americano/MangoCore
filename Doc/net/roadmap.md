# 网络子系统后续规划

> 最后更新: 2026-05-30 | 基于 routing 分支 Phase 1-6 架构

## 已完成的 Phase

| Phase | 状态 | 核心成果 |
|-------|------|---------|
| 1: 路由原语 | ✅ | RouteSocketHandle, RouteDecision, route_output() |
| 2: SocketHandle facade | ✅ | Binding table, add_routed_socket() |
| 3: 协议层脱离 SocketHandle ★ | ✅ | 9 文件, 零 smoltcp SocketHandle 引用 |
| 4: DeviceStack 包装 | ✅ | per-stack poll, dispatch_udp_packets(&mut SocketSet) |
| 5: 拆分 lo + eth0 ★ | ✅ | IfaceDevice enum, 两独立 smoltcp 栈 |
| 6: TCP lazy bind | ✅ | bind 不附着, connect 按路由选栈 |

## 近期规划 (Phase 6 剩余)

### 1. TCP Wildcard Fanout

```
当前: bind(0.0.0.0:8080) → 只创建一个 listen socket (eth0)
目标: bind(0.0.0.0:8080) → 为每个活跃 iface (lo + eth0) 创建 listen socket
```

**实现**:
```rust
// routing.rs
pub fn listener_ifindexes(addr: Option<IpAddress>) -> Vec<u32> {
    // 127.x → [1], specific local addr → [owning_iface], unspecified → [1, 2]
}
```

```rust
// lifecycle.rs listen():
let ifindexes = listener_ifindexes(listen_addr.addr);
let handles = ifindexes.iter().map(|idx| {
    NET_INTERFACE.add_routed_socket_on(InetProtocol::Tcp, new_socket, *idx)
}).collect();
Listening::new(handles, listen_addr)
```

### 2. UDP Wildcard Per-Interface

```
当前: bind(0.0.0.0:8080) → 一个 smoltcp UDP socket
目标: bind(0.0.0.0:8080) → 每个活跃 iface 一个 smoltcp UDP socket
```

**实现**:
```rust
// UdpSocket
socket_handlers: Mutex<Vec<RouteSocketHandle>>,  // 接收端 (per-iface)
tx_handler: Mutex<Option<RouteSocketHandle>>,     // 发送端 (按路由选)

// sendto: route_output(remote) → 选 ifindex → 用对应 iface socket
// recvfrom: 扫描所有 iface sockets → 返回最早可读 datagram
```

### 3. Route-Layer Local Delivery

```
问题: connect(own_eth0_ip) 当前通过 smoltcp 发送, 会在 ARP 阶段卡住
方案: route_output 返回 is_local: true → 直接将包注入目标栈 RX
```

**实现** (参考 Linux RTN_LOCAL → loopback_xmit → netif_rx):
```rust
fn handle_local_delivery(packet: &[u8], dst_ifindex: u32) {
    // 将 IP 包注入目标 DeviceStack 的 smoltcp 处理流程
    // 不经过物理 NIC
}
```

### 4. SelfConnected 接入

```
同 socket 自连接 (connect(self_addr:self_port)):
当前: SelfConnected 结构已定义但未实例化
目标: 在 connect() 中检测 local == remote → SelfConnected
```

### 5. /proc/net/route 优化

```
当前: 每次读取调用 Router::init_default() 临时构建
目标: 全局 Router 实例, /proc/net/route 直接读取
```

---

## 中期规划

### 1. 跨接口 IPv4 Forwarding

```rust
// net/forward.rs
pub trait RouterEnableDevice: Iface {
    fn handle_routable_packet(&self, frame: &EthernetFrame) -> Result<()>;
}
```

流程:
1. 检查 dst IP 是否本地 (is_my_ip) → 本地则交给 smoltcp
2. Router.lookup_route(dst_ip) → RouteDecision
3. ingress iface != egress iface → 合法转发
4. TTL > 1 → 递减 TTL → 转发到 egress

### 2. ARP/Neighbor Cache

smoltcp 的 neighbor cache 不公开 API。需要最小实现:

```rust
pub struct NeighborCache {
    entries: BTreeMap<Ipv4Address, (EthernetAddress, Instant)>,
}
```

用于跨接口转发时解析下一跳 MAC。未知 MAC 时发送 ARP request, 包短暂排队或丢弃。

### 3. 多 NIC 支持

```rust
// drivers/net/mod.rs
pub static NET_DEVICES: Mutex<Vec<Arc<dyn NetDevice>>>;

// config.rs
for nic in NET_DEVICES.lock().drain(..) {
    stacks.push(DeviceStack {
        ifindex: next_ifindex,
        name: format!("eth{}", idx),
        device: IfaceDevice::Eth(SmoltcpDeviceAdapter::new(nic)),
        ...
    });
}
```

### 4. IPv6

smoltcp 内置 IPv6 支持:
- IPv6 地址配置 (SLAAC / static)
- 邻居发现 (ND)
- TCPv6 / UDPv6
- ICMPv6

---

## 远期规划

| 功能 | 参考 | 说明 |
|------|------|------|
| Net Namespace | DragonOS `net_namespace.rs` | 隔离的设备列表 + 路由表 |
| NAT/ConnTrack | DragonOS `routing/nat.rs` | SNAT/DNAT + 连接跟踪 |
| Bridge | DragonOS `bridge.rs` | 虚拟网桥, 连接多接口 |
| veth | DragonOS `veth.rs` | 虚拟网卡对, container 网络 |
| NAPI | DragonOS `napi.rs` | bounded poll + 中断合并 |
| ICMP Socket | smoltcp icmp | ICMP echo reply, 端口不可达 |
| Policy Routing | Linux `ip rule` | 基于 fwmark / source 的路由选择 |
| TCP Congestion Control | smoltcp 上游 | 当前 smoltcp 不支持 |
| Multi-Queue NIC | virtio-net multiqueue | 多队列网卡支持 |
| TUN/TAP | — | 用户态虚拟网卡 |
