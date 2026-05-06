# Loopback + 物理网卡共存：双 Interface + 共享 SocketSet 方案

> 创建日期: 2026-05-06
> 状态: 设计阶段
> 参考: DragonOS `kernel/src/driver/net/loopback.rs`、Mango `NET_PLAN.md`

---

## 1. 背景与问题

### 1.1 当前架构

Mango 网络栈基于 smoltcp 0.10.0（`dependency/smoltcp/`），处于 **loopback-only** 模式：

```
smoltcp::Interface → Loopback device (Medium::Ip)
                    IP: 127.0.0.1/8
```

物理网卡驱动（`drivers/net/virtio_net.rs`）已实现但被注释（`main.rs:124`）。之前有 RoutingDevice（`adapter.rs`）尝试将 Loopback + 物理网卡合并为一个 Device 给 smoltcp。

### 1.2 RoutingDevice 的两个致命缺陷

#### 缺陷 1: ARP 泄漏

RoutingDevice 声明 `Medium::Ethernet`，smoltcp 对所有目标 IP（包括 127.0.0.1）做 ARP 解析：

```
TCP connect(127.0.0.1) → smoltcp 需要 ARP 解析 127.0.0.1 的 MAC
                      → 发送 ARP Request via RoutingDevice
                      → RoutingTxToken 判断 dst_ip=127.x.x.x → 转发到 Loopback
                      → Loopback 是 Medium::Ip，不处理 ARP
                      → ARP 永远无回复 → TCP 连接卡死
```

虽然可以在 `RoutingTxToken::consume()` 中手动注入合成 ARP Reply 来修复，但这是 hack 且易碎。

#### 缺陷 2: 源 IP 选择 —— smoltcp 0.10 结构性死结

关键代码在 `dependency/smoltcp/src/iface/interface/mod.rs:1073-1085`：

```rust
// ⚠️ _dst_addr 参数完全未被使用！
pub(crate) fn get_source_address_ipv4(
    &mut self,
    _dst_addr: Ipv4Address,
) -> Option<Ipv4Address> {
    for cidr in self.ip_addrs.iter() {
        if let IpCidr::Ipv4(cidr) = cidr {
            return Some(cidr.address());  // 永远返回第一个 IPv4！
        }
    }
    None
}
```

smoltcp 0.10 的源地址选择**完全忽略目标地址**，只看 `ip_addrs` 列表顺序。在一个 Interface 上同时配两个 IPv4 必然出错：

| ip_addrs 顺序            | 到 127.0.0.1 的源IP | 到 8.8.8.8 的源IP | 问题           |
| ------------------------ | ------------------- | ----------------- | -------------- |
| `[127.0.0.1, 10.0.2.15]` | 127.0.0.1 ✅         | 127.0.0.1 ❌       | 外部包源IP错误 |
| `[10.0.2.15, 127.0.0.1]` | 10.0.2.15 ❌         | 10.0.2.15 ✅       | 环回包源IP错误 |

> **注**：DragonOS 使用的 smoltcp 版本可能更新，其 `get_source_address_ipv4` 会检查 `cidr.contains_addr(&dst_addr)` 来匹配目标地址，但 Mango vendored 的 0.10.0 不包含此改进。

**结论：RoutingDevice 在同一 Interface 上配两个 IP 的方案在 smoltcp 0.10 上有结构性死结。不修改 smoltcp 源码则无解。**

---

## 2. 方案对比

### 方案 A: RoutingDevice + 补丁 smoltcp（不推荐）

修改 vendored smoltcp 的 `get_source_address_ipv4` 使其检查目标地址属于哪个子网。

**问题**：
- 修改 vendored 依赖 → 升级 smoltcp 时需重新合入补丁
- ARP 泄漏仍需额外 hack
- 一个 Interface 绑定两个 Medium 不同的 Device 本身就不符合 smoltcp 设计假设

### 方案 B: 双 Interface + 分离 SocketSet（DragonOS 方案）

```
lo Interface → lo SocketSet     eth Interface → eth SocketSet
```

每个 socket 创建时绑定到特定 Interface。需要 PortManager 跨 Interface 协调端口分配。Listening on 0.0.0.0 需要两个 SocketSet 各创建 listener。

**问题**：
- 需要 socket-iface 绑定（每个 socket 记录属于哪个 Interface）
- 需要跨 Interface 端口管理（PortManager）
- Listening on 0.0.0.0 需要在两个 SocketSet 各创建一个 listener
- 架构改动大，不适合 Mango 当前阶段

### 方案 C: 双 Interface + 共享 SocketSet（推荐）✅

```
              ┌── 共享 SocketSet ──────────┐
              │  (同一组 TCP/UDP sockets)   │
              └────┬──────────┬────────────┘
                   │          │
        ┌──────────▼──┐  ┌───▼──────────────┐
        │ lo Interface│  │ eth Interface     │
        │ Medium::Ip  │  │ Medium::Ethernet  │
        │ 127.0.0.1/8 │  │ 10.0.2.15/24      │
        │             │  │ routes:           │
        │ routes:     │  │  127.0.0.0/8 via  │
        │  127.0.0.0/8│  │    127.0.0.1 (🚫) │
        │  via 127... │  │  0.0.0.0/0 via    │
        └──────┬──────┘  │    10.0.2.2        │
               │         └──────┬─────────────┘
        ┌──────▼──────┐  ┌──────▼─────────────┐
        │ Loopback    │  │ SmoltcpDeviceAdapter│
        │ device      │  │ (virtio-net)        │
        └─────────────┘  └────────────────────┘
```

---

## 3. 为什么双 Interface + 共享 SocketSet 能解决 RoutingDevice 的问题

### 3.1 源 IP 不再混淆

- **lo Interface** 只有一个 IPv4 (`127.0.0.1`) → `get_source_address_ipv4` 始终返回 127.0.0.1 ✅
- **eth Interface** 只有一个 IPv4 (`10.0.2.15`) → `get_source_address_ipv4` 始终返回 10.0.2.15 ✅
- 不存在同一 Interface 上两个 IP 的源地址歧义

### 3.2 环回包不会泄漏到物理网卡

核心技术手段：在 **eth Interface 上添加 `127.0.0.0/8 via 127.0.0.1` 路由**。

当 `eth.poll()` 处理 `socket_egress` 时，smoltcp 对每个 socket 调用 `egress_permitted()`：

```
egress_permitted(timestamp, has_neighbor)
  └─ has_neighbor(127.0.0.1)   // socket 的目标 IP 是 127.0.0.1
       └─ route(127.0.0.1)
            ├─ in_same_network(127.0.0.1)?  → false  (10.0.2.15/24 不含 127.x)
            └─ routes.lookup(127.0.0.1)
                 → 匹配路由: 127.0.0.0/8 via 127.0.0.1  ← 我们添加的路由!
       └─ routed_addr = 127.0.0.1
       └─ medium = Ethernet
       └─ neighbor_cache.lookup(127.0.0.1)
            → ARP for 127.0.0.1 on physical Ethernet → 永远无回复
            → found() = false
  → egress_permitted = false → socket 被跳过 ✅
```

关键：**Ethernet 上的 ARP 请求 127.0.0.1 永远得不到回复**（这是 RFC 规范行为），所以 `has_neighbor` 始终返回 false。

### 3.3 外部包不会被 lo Interface 处理

eth Interface 有 `0.0.0.0/0 via 10.0.2.2` 默认路由，但 lo 没有。当 lo.poll() 处理 socket_egress 时：

```
has_neighbor(8.8.8.8)
  └─ in_same_network(8.8.8.8)? → false  (127.0.0.1/8 不含 8.8.8.8)
  └─ routes.lookup(8.8.8.8)  → lo 路由表只有 127.0.0.0/8 → 无匹配
  └─ route() = None
  └─ has_neighbor = false
→ egress_permitted = false ✅
```

### 3.4 Poll 顺序：lo 优先于 eth

```
poll_once():
  1. lo.poll()  → 处理 127.x.x.x 的收发（源IP正确 + 走 Loopback）
  2. dispatch_udp_packets()
  3. eth.poll() → 处理外部收发（127.x.x.x 被路由阻断，源IP正确 + 走物理网卡）
  4. dispatch_udp_packets()
```

lo 先 poll 确保 127.x.x.x 的包不会落到 eth 手里。即使 eth 有默认路由 0.0.0.0/0，用添加的 `127.0.0.0/8 via 127.0.0.1` 阻断路由也能保证不会重复发送。

### 3.5 TCP 不会重复发包（关键验证）

TCP socket 在 smoltcp 中通过 `tx_buffer` 管理发送数据。当 `dispatch()` 被调用时：
- smoltcp 从 `tx_buffer` 读取待发送数据，构建 TCP segment
- 数据**不会被移除**直到收到对端的 ACK

所以同一 socket 被两个 Interface 分别 poll 时，如果两个 Interface 都走 `egress_permitted() == true`，TCP segment 会被重复发送。

**但在我们的架构中**：
- lo.poll() → egress_permitted(127.0.0.1) → has_neighbor(127.0.0.1) → Medium::Ip → **true** → socket 被调度 ✅
- eth.poll() → egress_permitted(127.0.0.1) → has_neighbor(127.0.0.1) → 路由到 127.0.0.1 → ARP 永远无回复 → **false** → socket 被跳过 ✅
- eth.poll() → egress_permitted(8.8.8.8) → has_neighbor(8.8.8.8) → 默认路由 → ARP for 10.0.2.2 → **true** → socket 被调度 ✅
- lo.poll() → egress_permitted(8.8.8.8) → has_neighbor(8.8.8.8) → 无路由 → **false** → socket 被跳过 ✅

**每个目标 IP 只有一个 Interface 会处理，不会重复发包。**

---

## 4. 与 DragonOS 的区别

| 维度              | DragonOS                      | 本方案                 |
| ----------------- | ----------------------------- | ---------------------- |
| Interface 数量    | N 个（每个网卡一个）          | 2 个（lo + eth）       |
| SocketSet         | N 个（分离的）                | 1 个（共享的）         |
| socket-iface 绑定 | 创建 socket 时绑定            | 不需要                 |
| Port 管理         | PortManager 跨 Interface 协调 | 单 SocketSet，自动协调 |
| 0.0.0.0 listener  | 每个 SocketSet 各一个         | 只需一个               |
| 代码复杂度        | 高                            | 中等                   |

本方案比 DragonOS 更简洁：共享 SocketSet 让 smoltcp 内部的 socket 调度自然覆盖 TCP/UDP/Raw，不需要 socket-iface 绑定层。

---

## 5. 详细代码设计

### 5.1 `os/src/net/config.rs` — NetInterface（核心改动）

#### 5.1.1 新结构

```rust
// file: os/src/net/config.rs

use crate::drivers::net::{NET_DEVICE, NetDevice};
use crate::net::adapter::SmoltcpDeviceAdapter;
use crate::net::socket::inet::datagram::udp::dispatch_udp_packets;
use crate::net::socket::inet::stream::inner::tcp_state_code;
use crate::net::{TCP_SOCKETS, TCP_SOCKETS_TO_REMOVE, UDP_SOCKETS_TO_REMOVE};
use crate::timer::current_time_duration;
use crate::trace_event;
use alloc::vec;
use alloc::vec::Vec;
use smoltcp::{
    iface::{Config, Interface, Route, SocketHandle, SocketSet},
    phy::{Device, Loopback, Medium},
    socket::{raw, tcp, udp, AnySocket},
    time::Instant,
    wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address, Ipv4Cidr},
};

pub static NET_INTERFACE: NetInterface = NetInterface::new();

pub fn init() {
    NET_INTERFACE.init();
}

pub struct NetInterface {
    inner: Mutex<Option<NetInterfaceInner>>,
}

pub struct NetInterfaceInner {
    // === Loopback（永远存在）===
    pub lo_device: Loopback,
    pub lo_iface: Interface,

    // === Ethernet（仅当物理网卡可用）===
    pub eth_device: Option<SmoltcpDeviceAdapter>,
    pub eth_iface: Option<Interface>,

    // === 共享的 SocketSet ===
    pub sockets: SocketSet<'static>,
}
```

#### 5.1.2 初始化逻辑

```rust
impl NetInterfaceInner {
    /// QEMU user-mode networking 默认参数
    const DEFAULT_LOCAL_IP: [u8; 4] = [10, 0, 2, 15];
    const DEFAULT_SUBNET_PREFIX: u8 = 24;
    const DEFAULT_GATEWAY: [u8; 4] = [10, 0, 2, 2];

    fn new() -> Self {
        let now = Instant::from_millis(current_time_duration().as_millis() as i64);
        let mut sockets = SocketSet::new(vec![]);

        // ══════════════════════════════════════════
        // 1. 创建 Loopback Interface
        // ══════════════════════════════════════════
        let mut lo_device = Loopback::new(Medium::Ip);
        let lo_config = Config::new(HardwareAddress::Ip);
        let mut lo_iface = Interface::new(lo_config, &mut lo_device, now);

        // lo: IP 127.0.0.1/8
        lo_iface.update_ip_addrs(|addrs| {
            addrs.push(IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8)).unwrap();
        });

        // lo: 路由 — 127.0.0.0/8 走本地
        lo_iface.routes_mut().update(|routes| {
            routes.push(Route {
                cidr: IpCidr::new(IpAddress::v4(127, 0, 0, 0), 8),
                via_router: IpAddress::v4(127, 0, 0, 1),
                preferred_until: None,
                expires_at: None,
            }).unwrap();
        });

        // ══════════════════════════════════════════
        // 2. 如果物理网卡可用，创建 Ethernet Interface
        // ══════════════════════════════════════════
        let (eth_device, eth_iface) = if let Some(net_dev) = NET_DEVICE.lock().as_ref() {
            let mac = net_dev.mac_address();
            let mut eth_device = SmoltcpDeviceAdapter::new(net_dev.clone());
            let eth_config = Config::new(HardwareAddress::Ethernet(
                EthernetAddress(mac),
            ));
            let mut eth_iface = Interface::new(eth_config, &mut eth_device, now);

            // eth: IP 10.0.2.15/24
            eth_iface.update_ip_addrs(|addrs| {
                addrs.push(IpCidr::new(
                    IpAddress::v4(
                        Self::DEFAULT_LOCAL_IP[0],
                        Self::DEFAULT_LOCAL_IP[1],
                        Self::DEFAULT_LOCAL_IP[2],
                        Self::DEFAULT_LOCAL_IP[3],
                    ),
                    Self::DEFAULT_SUBNET_PREFIX,
                )).unwrap();
            });

            // eth: 路由
            //   - 127.0.0.0/8 via 127.0.0.1  → 阻断环回包在 eth 上调度
            //   - 0.0.0.0/0 via 10.0.2.2      → 默认网关
            eth_iface.routes_mut().update(|routes| {
                // 阻塞路由：让 has_neighbor(127.0.0.1) 在 Ethernet 上永久返回 false
                routes.push(Route {
                    cidr: IpCidr::new(IpAddress::v4(127, 0, 0, 0), 8),
                    via_router: IpAddress::v4(127, 0, 0, 1),
                    preferred_until: None,
                    expires_at: None,
                }).unwrap();

                // 默认路由
                routes.push(Route {
                    cidr: IpCidr::new(IpAddress::v4(0, 0, 0, 0), 0),
                    via_router: IpAddress::v4(
                        Self::DEFAULT_GATEWAY[0],
                        Self::DEFAULT_GATEWAY[1],
                        Self::DEFAULT_GATEWAY[2],
                        Self::DEFAULT_GATEWAY[3],
                    ),
                    preferred_until: None,
                    expires_at: None,
                }).unwrap();
            });

            // AnyIP: 允许接收路由前缀内的包
            eth_iface.set_any_ip(true);

            (Some(eth_device), Some(eth_iface))
        } else {
            // 无物理网卡 — 纯 loopback 模式
            (None, None)
        };

        NetInterfaceInner {
            lo_device,
            lo_iface,
            eth_device,
            eth_iface,
            sockets,
        }
    }
}
```

#### 5.1.3 Poll 逻辑

```rust
impl NetInterface {
    // ... 其他方法不变 ...

    fn poll_once(&self) -> bool {
        let mut progressed = false;

        self.inner_handler(|inner| {
            let timestamp = Instant::from_millis(
                current_time_duration().as_millis() as i64
            );

            // 1. 清理标记删除的 UDP sockets
            let mut to_remove = UDP_SOCKETS_TO_REMOVE.lock();
            for handle in to_remove.drain(..) {
                inner.sockets.remove(handle);
            }
            drop(to_remove);

            // 2. ★ 先 poll Loopback ★
            let lo_progressed = inner.lo_iface.poll(
                timestamp,
                &mut inner.lo_device,
                &mut inner.sockets,
            );

            // 3. 分发 lo 上的 UDP 包
            dispatch_udp_packets(&inner.sockets);

            // 4. 清理 TCP sockets（lo poll 后）
            Self::reap_tcp_sockets(&mut inner.sockets);

            // 5. ★ 再 poll Ethernet（如果存在）★
            let eth_progressed = if let (Some(eth_iface), Some(eth_device)) =
                (inner.eth_iface.as_mut(), inner.eth_device.as_mut())
            {
                let p = eth_iface.poll(timestamp, eth_device, &mut inner.sockets);

                // 6. 分发 eth 上的 UDP 包
                dispatch_udp_packets(&inner.sockets);

                // 7. 清理 TCP sockets（eth poll 后）
                Self::reap_tcp_sockets(&mut inner.sockets);

                p
            } else {
                false
            };

            progressed = lo_progressed || eth_progressed;
        });

        progressed
    }

    /// 清理已关闭的 TCP sockets
    fn reap_tcp_sockets(sockets: &mut SocketSet<'static>) {
        let mut to_remove = TCP_SOCKETS_TO_REMOVE.lock();
        let ready: Vec<SocketHandle> = to_remove
            .iter()
            .filter(|&&h| {
                let sock = sockets.get::<tcp::Socket>(h);
                sock.state() == tcp::State::Closed
            })
            .copied()
            .collect();

        if !ready.is_empty() {
            log::info!(
                "[NetInterface::reap_tcp_sockets] removing {} TCP sockets",
                ready.len()
            );
        }
        for h in &ready {
            sockets.remove(*h);
        }
        to_remove.retain(|h| !ready.contains(h));
        drop(to_remove);
    }

    /// 根据目标 IP 返回正确的源地址
    /// - 127.x.x.x → 127.0.0.1
    /// - 其他     → 物理网卡 IP（如果有的话），否则 127.0.0.1
    pub fn lookup_source_ip(&self, dst_ip: IpAddress) -> IpAddress {
        match dst_ip {
            IpAddress::Ipv4(addr) => {
                let bytes = addr.as_bytes();
                if bytes[0] == 127 {
                    IpAddress::v4(127, 0, 0, 1)
                } else if let Some(inner) = self.inner.lock().as_ref() {
                    inner
                        .eth_iface
                        .as_ref()
                        .and_then(|iface| iface.ipv4_addr())
                        .map(IpAddress::from)
                        .unwrap_or(IpAddress::v4(127, 0, 0, 1))
                } else {
                    IpAddress::v4(127, 0, 0, 1)
                }
            }
            IpAddress::Ipv6(_) => {
                // IPv6: 暂用 ::1
                IpAddress::v6(0, 0, 0, 0, 0, 0, 0, 1)
            }
        }
    }
}
```

### 5.2 `os/src/net/adapter.rs` — 保留 SmoltcpDeviceAdapter，删除 RoutingDevice

RoutingDevice 不再需要（已被双 Interface 方案替代），但 `SmoltcpDeviceAdapter` 仍然需要用于包装物理网卡为 smoltcp `Device` trait。

```rust
// file: os/src/net/adapter.rs — 简化版

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use crate::drivers::net::NetDevice;
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;

/// 将 Mango NetDevice 适配为 smoltcp phy::Device
pub struct SmoltcpDeviceAdapter {
    pub inner: Arc<dyn NetDevice>,
}

impl SmoltcpDeviceAdapter {
    pub fn new(inner: Arc<dyn NetDevice>) -> Self {
        Self { inner }
    }
}

impl Device for SmoltcpDeviceAdapter {
    type RxToken<'a> = NetRxToken;
    type TxToken<'a> = NetTxToken;

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1500;
        caps.medium = Medium::Ethernet;
        caps
    }

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let mut buf = [0u8; 2048];
        if let Some(len) = self.inner.receive(&mut buf) {
            let rx = NetRxToken { buf: buf[..len].to_vec() };
            let tx = NetTxToken { inner: self.inner.clone() };
            Some((rx, tx))
        } else {
            None
        }
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(NetTxToken { inner: self.inner.clone() })
    }
}

pub struct NetRxToken { buf: Vec<u8> }

impl RxToken for NetRxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where F: FnOnce(&mut [u8]) -> R {
        f(&mut self.buf)
    }
}

pub struct NetTxToken { inner: Arc<dyn NetDevice> }

impl TxToken for NetTxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where F: FnOnce(&mut [u8]) -> R {
        if len == 0 {
            let mut empty = [];
            return f(&mut empty);
        }
        let mut buf = vec![0u8; len];
        let result = f(&mut buf);
        self.inner.transmit(&buf);
        result
    }
}
```

**关键变化**：
- RoutingDevice / RoutingRxToken / RoutingTxToken / `ROUTING_BUF` — **全部删除**
- SmoltcpDeviceAdapter / NetRxToken / NetTxToken — 保留，用于 eth Interface

### 5.3 `os/src/net/mod.rs` — 恢复导出

```rust
// file: os/src/net/mod.rs

pub mod adapter;  // ← 取消注释，SmoltcpDeviceAdapter 仍需要
pub mod config;
mod macros;
pub mod posix;
pub mod socket;
pub mod syscall;

pub use spin::Mutex;

// ... 其余 re-exports 不变 ...
```

### 5.4 `os/src/main.rs` — 恢复网卡初始化

```rust
// file: os/src/main.rs

// ... 
fs::directory_tree::init_fs();
drivers::init_net_device();   // ← 取消注释
net::config::init();
// ...
```

### 5.5 `os/src/net/socket/inet/common/address.rs` — 修复源 IP 选择

当前 `_to_endpoint` / `_endpoint` 函数将 `ANY` 地址硬编码为 `127.0.0.1`。需要在 `bind` / `connect` / `sendto` 时根据目标 IP 选择正确的源地址。

```rust
// file: os/src/net/socket/inet/common/address.rs

use crate::net::config::NET_INTERFACE;

/// 将 IpListenEndpoint 转为 IpEndpoint，根据实际网络配置选择源地址
pub fn _to_endpoint(listen_endpoint: IpListenEndpoint) -> IpEndpoint {
    let addr = match listen_endpoint.addr {
        Some(addr) if addr.is_unspecified() => {
            // ★ 不再硬编码 127.0.0.1 ★
            // 对于 bind/listen，unspecified 意味着绑定所有接口。
            // 但 smoltcp 要求具体地址，所以用 0.0.0.0 代表 "any"。
            // 实际源地址由 Interface 的 get_source_address_ipv4 决定。
            IpAddress::v4(0, 0, 0, 0)
        }
        Some(addr) => addr,
        None => IpAddress::v4(0, 0, 0, 0),
    };
    IpEndpoint::new(addr, listen_endpoint.port)
}

/// connect / sendto 时：目标地址决定源地址
pub fn source_ip_for_dst(dst: IpAddress) -> IpAddress {
    NET_INTERFACE.lookup_source_ip(dst)
}
```

### 5.6 `os/src/net/socket/inet/datagram/udp.rs` — 适配新签名

`dispatch_udp_packets` 当前接受 `&mut NetInterfaceInner`，需要改为接受 `&mut SocketSet<'static>`。由于共享 SocketSet 不需要访问 Interface 内部，这个改动很小：

```rust
// 旧签名
pub fn dispatch_udp_packets(inner: &mut NetInterfaceInner) { ... }

// 新签名 — 只需要 SocketSet
pub fn dispatch_udp_packets(sockets: &mut SocketSet<'static>) { ... }
```

### 5.7 `os/Makefile` — QEMU 网卡参数

在 QEMU 启动参数中添加 virtio-net 设备：

```makefile
# QEMU flags for riscv64 (user-mode networking)
QEMU_NET_FLAGS := -netdev user,id=hostnet0 \
                  -device virtio-net-pci,netdev=hostnet0
```

需要 QEMU 端口转发时加 `hostfwd`：
```makefile
QEMU_NET_FLAGS := -netdev user,id=hostnet0,hostfwd=tcp::12580-:12580 \
                  -device virtio-net-pci,netdev=hostnet0
```

---

## 6. 实施顺序

| Phase | 改动                                                               | 文件                                       | 风险 |
| ----- | ------------------------------------------------------------------ | ------------------------------------------ | ---- |
| **1** | adapter.rs: 删除 RoutingDevice，保留 SmoltcpDeviceAdapter          | `os/src/net/adapter.rs`                    | 低   |
| **2** | mod.rs: 取消注释 `pub mod adapter;`                                | `os/src/net/mod.rs`                        | 低   |
| **3** | config.rs: 改造 NetInterfaceInner（双 Interface + 共享 SocketSet） | `os/src/net/config.rs`                     | 中   |
| **4** | udp.rs: dispatch_udp_packets 签名改为 `&mut SocketSet`             | `os/src/net/socket/inet/datagram/udp.rs`   | 低   |
| **5** | address.rs: 修复源 IP 硬编码                                       | `os/src/net/socket/inet/common/address.rs` | 低   |
| **6** | main.rs: 取消注释 `drivers::init_net_device()`                     | `os/src/main.rs`                           | 低   |
| **7** | Makefile: 添加 QEMU virtio-net 参数                                | `os/Makefile`                              | 低   |
| **8** | 编译验证                                                           | —                                          | —    |
| **9** | QEMU 测试                                                          | —                                          | 中   |

Phase 1-2 可以合并，Phase 3 是核心。

---

## 7. 验证清单

1. `make rv64-kernel-build-only` 通过
2. `make la64-kernel-build-only` 通过
3. **无 NIC 时**：QEMU 不加 `-netdev` 参数，内核启动日志显示 "loopback-only mode"，127.0.0.1 连接正常
4. **有 NIC 时**：QEMU 加 `-netdev user -device virtio-net-pci`，启动日志显示正确设备模式
5. `ping 127.0.0.1` 正常（即使有 NIC，loopback 延迟极低）
6. `ping 10.0.2.2` 正常（QEMU 网关可达）
7. TCP connect 到 127.0.0.1:$port 正常，源 IP = 127.0.0.1
8. TCP connect 到 10.0.2.2:$port 正常，源 IP = 10.0.2.15
9. 无 ARP for 127.x.x.x 出现在物理网卡（tcpdump/Wireshark 验证）
10. 无重复 TCP segment（tcpdump 验证每个目标 IP 的包只出现一次）
11. 已有测试（basic/busybox）通过

---

## 8. 常见问题

### Q: 共享 SocketSet 会不会导致 smoltcp 内部的 socket 被两个 Interface 竞争访问？

不会。`poll_once()` 在持有 `inner` Mutex 的情况下依次 poll lo 和 eth，两个 poll 是串行的。smoltcp 的 `socket_egress` 在 poll 结束时从 tx_buffer 读取数据并分派，第二个 Interface poll 时数据已被第一个 Interface 处理。

### Q: 那 TCP 重复发包怎么办？

如 3.5 节分析的：eth 上添加了 `127.0.0.0/8 via 127.0.0.1` 阻断路由后，`has_neighbor(127.0.0.1)` 在 Ethernet 上永远 false，`egress_permitted` 返回 false。TCP socket 的 127.x.x.x 流量不会被 eth.poll() 分派。

### Q: Listening on 0.0.0.0 时，127.0.0.1 和 10.0.2.15 的连接都能收到吗？

能。smoltcp TCP socket 的 `accepts()` 检查 `listen_endpoint.addr`，如果为 `None`（即绑定 ANY），则接受所有目标 IP 的连接。两个 Interface 的 poll 都会匹配这个 listener。

### Q: 为什么不用 DragonOS 的分离 SocketSet 方案？

DragonOS 的方案更完整但更复杂。分离 SocketSet 需要：
- socket-iface 绑定层（每个 socket 创建时决定属于哪个 Interface）
- PortManager（跨 Interface 协调端口）
- 两套 listener（0.0.0.0 需要在两个 SocketSet 各创建 listener）

对当前 Mango 阶段来说过度设计。共享 SocketSet + 精心配置路由表 + poll 顺序就能正确工作，代码改动也小得多。

### Q: 将来升级 smoltcp 后，是不是可以不用路由表 hack 了？

是的。新版本 smoltcp 的 `get_source_address_ipv4` 会检查 `cidr.contains_addr(&dst_addr)`，这时在同一 Interface 上配两个 IP 也不会混淆源地址。如果将来升级 smoltcp，可以考虑切回 RoutingDevice 的单 Interface 方案。但当前 vendored 0.10.0 不支持。

---

## 9. 附录：关键代码路径参考

### smoltcp `has_neighbor` 调用链

```
iface.poll()
  └─ socket_egress()
       └─ Meta::egress_permitted(timestamp, |ip| inner.has_neighbor(&ip))
            ├─ NeighborState::Active → true
            └─ NeighborState::Waiting { neighbor, silent_until }
                 └─ has_neighbor(neighbor)
                      └─ InterfaceInner::has_neighbor(addr)
                           └─ route(addr) → routed_addr
                                └─ match medium:
                                     Medium::Ethernet → neighbor_cache.lookup(routed_addr).found()
                                     Medium::Ip       → true  ← lo 总是 true!
```

关键点：`Medium::Ip` 的 `has_neighbor` 永远返回 true（不需要 ARP），`Medium::Ethernet` 才需要 neighbor cache。

### smoltcp `get_source_address_ipv4` 调用点

```
dispatch_ip() → ip_repr.set_src_addr(self.get_source_address_ipv4(dst_addr))
```

每次发包时调用，决定源 IP。在 0.10.0 中 `_dst_addr` 被忽略。

### Mango `dispatch_udp_packets` 结构

```rust
pub fn dispatch_udp_packets(inner: &mut NetInterfaceInner) {
    // 遍历 inner.sockets 中的所有 UDP sockets
    // 对每个有数据的 socket，找到 Mango 的 UdpSocket 并分发
}
```

改为接受 `&mut SocketSet<'static>` 后，在 `poll_once()` 中两次调用分别用同一个 `inner.sockets` 即可。

---

## 10. 变更总结

| 项目                 | 变更                                                                               |
| -------------------- | ---------------------------------------------------------------------------------- |
| RoutingDevice        | **删除**                                                                           |
| SmoltcpDeviceAdapter | 保留，用于 eth Interface                                                           |
| NetInterfaceInner    | 从单 `device + iface` 改为 `lo_device + lo_iface + Option<eth_device + eth_iface>` |
| SocketSet            | **共享**一个                                                                       |
| Poll                 | lo 优先，eth 其次，中间 dispatch UDP                                               |
| 源 IP                | 每个 Interface 只有一个 IPv4，自动正确                                             |
| 路由表               | eth 上添加 `127.0.0.0/8 via 127.0.0.1` 阻断路由                                    |
| 编译                 | 恢复 `pub mod adapter`、`drivers::init_net_device()`                               |
