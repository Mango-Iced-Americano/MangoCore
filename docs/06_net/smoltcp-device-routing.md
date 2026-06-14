---
title: "smoltcp 适配层、路由层与设备层"
category: net
status: stable
author: MangoCore Team
last_update: 2026-06-14
tags: [net, smoltcp, routing, device, dhcp]
---

## 概述

该文档描述 Mango 内核中 smoltcp 协议栈的集成适配层、设备层抽象、路由子系统、DHCP 初始化流程以及轮询编排机制。整体架构以 `NET_INTERFACE` 全局静态为核心，将 smoltcp `Interface` + `SocketSet` 按设备栈分拆，通过 `IfaceDevice` 枚举统一设备操作，通过 `Router` + `RouteTable` 实现最长前缀匹配路由。

核心文件：

| 文件 | 行数 | 职责 |
|------|------|------|
| `os/src/net/config.rs` | 823 | `NetInterface` 全局静态，`DeviceStack`，DHCP 初始化，轮询编排，socket 管理 API |
| `os/src/net/adapter.rs` | 390 | `IfaceDevice` 枚举，`SmoltcpDeviceAdapter`，`NullNetDevice`，已废弃的 `RoutingDevice` |
| `os/src/net/routing.rs` | 356 | `RouteSocketHandle`，`SocketBinding`，`Router` / `RouteTable`，`route_output` |
| `os/src/net/iface.rs` | 205 | `Iface` trait，`IfaceCommon`，`SmoltcpDeviceAccess` trait |
| `os/src/net/net_core.rs` | 438 | `NetDeviceEntry`（`Iface` 实现），设备注册，DHCP 网关全局变量，`current_netns` |
| `os/src/net/neighbour.rs` | 104 | 全局邻居表 `NEIGHBOUR_TABLE`，ARP 拦截捕获 |
| `os/src/drivers/net/mod.rs` | 31 | `NetDevice` trait，`NET_DEVICE` 全局 |
| `os/src/drivers/net/virtio_net.rs` | 86 | `VirtIONetWrapper`（MMIO + PCI 两种传输层） |
| `os/src/drivers/net/veth.rs` | 329 | Veth 虚拟网卡驱动，`VethInterface`，`veth_pair_new` / `veth_pair_delete` |

---

## 设备层

### IfaceDevice 枚举

`IfaceDevice` 是 smoltcp `phy::Device` trait 的枚举实现，替代了旧版本中 `RoutingDevice` 的多设备软件交换机设计。每个 `DeviceStack` 持有独立的 `IfaceDevice` 实例，无需在 transmit 路径做跨端口路由判决。

```rust
pub enum IfaceDevice {
    Lo(Loopback),
    Eth(SmoltcpDeviceAdapter),
    Veth(VethDriver),
}
```

三种变体分别对应 loopback、物理以太网（virtio）和虚拟以太网（veth）设备。

**为何选择枚举而非 trait object：**

smoltcp 的 `Device` trait 包含关联类型 `RxToken<'a>` 和 `TxToken<'a>`，这两个类型是 GAT（generic associated type）。Rust 目前不支持将带有 GAT 的 trait 用作 trait object（`dyn Device`）。因此无法通过 `Box<dyn Device>` 统一持有不同设备类型。枚举 `IfaceDevice` 通过穷举所有设备变体，将 GAT 派生的 token 类型也枚举化（`IfaceRxToken` / `IfaceTxToken`），从而绕过了 trait object 的限制。

```rust
pub enum IfaceRxToken<'a> {
    Lo(<Loopback as Device>::RxToken<'a>),
    Eth(<SmoltcpDeviceAdapter as Device>::RxToken<'a>),
    Veth(<VethDriver as Device>::RxToken<'a>),
}
```

`Device` trait 的实现通过 `match self` 分发到具体变体：

```rust
impl Device for IfaceDevice {
    type RxToken<'a> = IfaceRxToken<'a>;
    type TxToken<'a> = IfaceTxToken<'a>;

    fn receive(&mut self, timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        match self {
            Self::Lo(lo) => { let (rx, tx) = lo.receive(timestamp)?; Some((IfaceRxToken::Lo(rx), IfaceTxToken::Lo(tx))) }
            Self::Eth(eth) => { ... }
            Self::Veth(veth) => { ... }
        }
    }

    fn transmit(&mut self, timestamp: Instant) -> Option<Self::TxToken<'_>> {
        match self {
            Self::Lo(lo) => lo.transmit(timestamp).map(IfaceTxToken::Lo),
            Self::Eth(eth) => eth.transmit(timestamp).map(IfaceTxToken::Eth),
            Self::Veth(veth) => veth.transmit(timestamp).map(IfaceTxToken::Veth),
        }
    }
}
```

### SmoltcpDeviceAdapter

`SmoltcpDeviceAdapter` 是内核 `NetDevice` trait 到 smoltcp `phy::Device` trait 的适配器。它包装 `Arc<dyn NetDevice>` 并通过 `&self` + 内部缓冲区实现 smoltcp 要求的 token 式收发接口。

```rust
pub struct SmoltcpDeviceAdapter {
    pub inner: Arc<dyn NetDevice>,
}
```

`receive` 从 `NetDevice::receive` 收取原始字节，封装为 `NetRxToken`。`transmit` 返回 `NetTxToken`，在其 `consume` 方法中调用 `NetDevice::transmit` 发送。

```rust
impl Device for SmoltcpDeviceAdapter {
    type RxToken<'a> = NetRxToken;
    type TxToken<'a> = NetTxToken;

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1500;
        caps.medium = Medium::Ethernet;
        caps
    }

    fn receive(&mut self, timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let mut buf = [0u8; 2048];
        if let Some(len) = self.inner.receive(&mut buf) {
            let packet = buf[..len].to_vec();
            Some((NetRxToken { buf: packet }, NetTxToken { inner: self.inner.clone() }))
        } else {
            None
        }
    }

    fn transmit(&mut self, timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(NetTxToken { inner: self.inner.clone() })
    }
}
```

**NetRxToken：** `consume` 方法中先调用 `neighbour::try_capture_arp_reply` 尝试从接收帧中提取 ARP 回复并更新邻居表，再交付给上层协议栈。

**NetTxToken：** `consume` 方法中先防空包检查（`len == 0`），再分配缓冲区交由用户填充，最后调用 `self.inner.transmit(&buf)`。

### NullNetDevice

当系统启动时未检测到物理 NIC（`NET_DEVICE.lock().is_none()`），`NetInterfaceInner::new()` 会创建一个 `NullNetDevice` 实例作为 eth0 的设备后端。它允许 smoltcp 协议栈正常创建 eth0 的 `Interface`，但所有数据包收发均为空操作。

```rust
pub struct NullNetDevice;

impl NetDevice for NullNetDevice {
    fn receive(&self, _buf: &mut [u8]) -> Option<usize> { None }
    fn transmit(&self, _buf: &[u8]) { /* silently drop */ }
    fn mac_address(&self) -> [u8; 6] {
        // Locally administered unicast MAC (not all-zero, required by smoltcp DHCP)
        [0x02, 0x00, 0x00, 0x00, 0x00, 0x01]
    }
}
```

MAC 地址取 `02:00:00:00:00:01` 而非全零，因为 smoltcp DHCP 客户端要求 MAC 地址为有效的单播地址，全零地址会导致 DHCP 请求被拒绝。

### Iface Trait

`Iface` trait 是所有网络接口的统一抽象。定义在 `os/src/net/iface.rs`，由 `net_core.rs` 重新导出。每种接口类型（loopback、eth0、veth）均实现此 trait。

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

`DeviceKind` 是三层变体枚举：

```rust
pub enum DeviceKind {
    Loopback,
    Ethernet,
    Veth,
}
```

`SmoltcpDeviceAccess` 是 `&self` 版本的设备抽象（smoltcp 原生的 `Device` trait 要求 `&mut self`），用于轮询循环中通过 `Arc` 共享的方式驱动设备：

```rust
pub trait SmoltcpDeviceAccess: Send + Sync {
    fn poll(&self, timestamp: Instant) -> core::result::Result<(), ()>;
    fn capabilities(&self) -> DeviceCapabilities;
}
```

### IfaceCommon

`IfaceCommon` 是 `Iface` 的共享状态承载结构体，存放元数据（名称、标志、IP 地址、MAC）和 smoltcp 协议引擎（`Interface` + `SocketSet`）。核心设计：smoltcp 的 `Interface` 不持有设备类型参数，设备在每次 `Interface::poll()` 调用时传入，因此 `IfaceCommon` 可以无泛型参数地存储 `Interface`。

```rust
pub struct IfaceCommon {
    pub nic_id: AtomicUsize,
    pub name: RwLock<String>,
    pub flags: AtomicU32,
    pub mtu: AtomicUsize,
    pub ip_addrs: Mutex<Vec<IpCidr>>,
    pub hwaddr: [u8; 6],
    pub kind: DeviceKind,
    pub peer_ifindex: Option<usize>,
    pub smoltcp_iface: Mutex<Interface>,
    pub sockets: Mutex<SocketSet<'static>>,
    pub net_namespace: RwLock<Option<Weak<NetNamespace>>>,
}
```

### NetDevice Trait

底层驱动抽象，位于 `os/src/drivers/net/mod.rs`。定义了物理网卡驱动必须实现的三元组：

```rust
pub trait NetDevice: Send + Sync {
    fn receive(&self, buf: &mut [u8]) -> Option<usize>;
    fn transmit(&self, buf: &[u8]);
    fn mac_address(&self) -> [u8; 6];
}
```

全局 `NET_DEVICE` 存储 `Option<Arc<dyn NetDevice>>`，在 `drivers::init_net_device()` 中初始化：

```rust
lazy_static! {
    pub static ref NET_DEVICE: Mutex<Option<Arc<dyn NetDevice>>> = Mutex::new(None);
}

pub fn init_net_device() {
    #[cfg(any(feature = "block_virt", feature = "block_virt_pci"))]
    {
        if let Some(net_dev) = virtio_net::VirtIONetWrapper::new() {
            *NET_DEVICE.lock() = Some(Arc::new(net_dev));
        }
    }
}
```

### VirtIONetWrapper

VirtIO 网卡驱动封装，支持两种传输层变体：

```rust
#[cfg(feature = "block_virt")]
pub struct VirtIONetWrapper(Mutex<VirtIONet<VirtioHal, MmioTransport<'static>, QUEUE_SIZE>>);

#[cfg(feature = "block_virt_pci")]
pub struct VirtIONetWrapper(Mutex<VirtIONet<VirtioHal, PciTransport, QUEUE_SIZE>>);
```

| 特性 | 传输层 | 基地址 / 发现方式 |
|------|--------|-------------------|
| `block_virt` | MMIO | `0x10008000`，固定 MMIO 基址 |
| `block_virt_pci` | PCI | `enumerate_virtio_pci(DeviceType::Network)` |

`QUEUE_SIZE = 16`，`NET_BUF_SIZE = 2048`。

MMIO 构造：

```rust
#[cfg(feature = "block_virt")]
impl VirtIONetWrapper {
    pub fn new() -> Option<Self> {
        unsafe {
            let transport = MmioTransport::new(
                NonNull::new_unchecked(VIRTIO_NET_BASE as *mut VirtIOHeader),
                0x1000,
            ).ok()?;
            let net = VirtIONet::<VirtioHal, MmioTransport<'static>, QUEUE_SIZE>::new(
                transport, NET_BUF_SIZE,
            ).ok()?;
            Some(Self(Mutex::new(net)))
        }
    }
}
```

PCI 构造通过 `enumerate_virtio_pci` 发现第一个 Network 类型的 PCI 设备。

`NetDevice` 实现中，`receive` 使用 `VirtIONet::receive()` 获取 `RxBuffer`，拷贝到调用方缓冲区后调用 `recycle_rx_buffer` 归还；`transmit` 构造 `TxBuffer` 调用 `send`。

### Veth 驱动

Veth（虚拟以太网）驱动用于容器网络命名空间互联。每个 Veth 端是一个 `VethInterface`，实现 `Iface` trait，由 `VethDriver` 实现 `Device` trait。

核心数据流：

```
VethInterface A (tx) -> Arc<Veth>.peer -> VethInterface B (rx_queue)
VethInterface B (tx) -> Arc<Veth>.peer -> VethInterface A (rx_queue)
```

**Veth** 是内部状态结构，持有 `rx_queue`（`VecDeque<Vec<u8>>`）和 `peer`（指向对端的 `Weak<VethInterface>`）。

**VethDriver** 是 `Device` trait 和 `SmoltcpDeviceAccess` trait 的实现。`receive` 从自身 `rx_queue` pop 数据，`transmit` 向对端 `rx_queue` push 数据。队列长度上限 `MAX_VETH_QUEUE_LEN = 4096`，超出时静默丢弃以防止 OOM。

**VethInterface** 包装 `IfaceCommon` + `VethDriver` + `Arc<Veth>`，是完整的 `Iface` 实现。

创建 veth 对端使用 `veth_pair_new(name1, name2)`：

```rust
pub fn veth_pair_new(name1: &str, name2: &str) -> (u32, u32) {
    let ifindex1 = net_core::next_ifindex();
    let ifindex2 = net_core::next_ifindex();
    let iface1 = VethInterface::new(name1, ifindex1);
    let iface2 = VethInterface::new(name2, ifindex2);
    *iface1.data.peer.lock() = Arc::downgrade(&iface2);
    *iface2.data.peer.lock() = Arc::downgrade(&iface1);
    // ... flags, registration
    net_core::add_device(iface1.clone());
    net_core::add_device(iface2.clone());
    crate::net::config::NET_INTERFACE.add_veth_stack(iface1.clone(), driver1);
    crate::net::config::NET_INTERFACE.add_veth_stack(iface2.clone(), driver2);
    (ifindex1, ifindex2)
}
```

---

## 路由层

### RouteSocketHandle

不透明句柄，用于在用户 socket 代码中引用已注册的路由式 socket。使用 `usize` 包装，实现了 `Ord` 以便用于 `BTreeMap` 键：

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RouteSocketHandle(pub(crate) usize);

impl fmt::Display for RouteSocketHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RH({})", self.0)
    }
}
```

### SocketBinding

将 `RouteSocketHandle` 映射到具体的 smoltcp `SocketHandle` 及其所属的设备栈：

```rust
#[derive(Clone, Copy, Debug)]
pub(crate) struct SocketBinding {
    pub ifindex: u32,
    pub handle: SocketHandle,
    pub proto: InetProtocol,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InetProtocol {
    Tcp,
    Udp,
    Raw,
}
```

### 绑定表

绑定表是 `NetInterfaceInner` 的核心数据结构，使用 `BTreeMap<RouteSocketHandle, SocketBinding>` 维护。哈希映射确保 socket 句柄到设备栈的一致路由：

```rust
pub struct NetInterfaceInner<'a> {
    pub stacks: Vec<DeviceStack<'a>>,
    pub bindings: BTreeMap<RouteSocketHandle, SocketBinding>,
    pub next_socket_id: usize,
}
```

### route_output() 函数

全局路由查询入口，位于 `routing.rs`。输入目标 IP 地址，返回 `RouteDecision` 或 `ENETUNREACH` 错误。

```rust
pub fn route_output(dest: IpAddress) -> Result<RouteDecision, SyscallErr>
```

**RouteDecision：**

```rust
#[derive(Clone, Debug)]
pub struct RouteDecision {
    pub ifindex: u32,
    pub source: IpAddress,
    pub next_hop: Option<IpAddress>,
    pub is_local: bool,
}
```

**路由查询流程（IPv4 为例）：**

1. 检查路由表是否为空，若空则延迟填充默认路由（`router.fill_default()`）。
2. 遍历 `netns.device_list`，检查目标地址是否属于本机任一接口的 IP。若匹配则返回 `is_local = true`，`ifindex` 指向该接口。
3. 检查是否为 127.x.x.x 段，若是则走 loopback（ifindex=1）。
4. 查询 `Router::lookup_route` 进行最长前缀匹配。
5. 若无匹配返回 `SyscallErr::ENETUNREACH`。

**IPv6 路径：** 先检查本地地址匹配，再查 `::1` loopback，最后查 v6 路由表。

### Router 与 RouteTable

**RouteTable** 是 `Vec<RouteEntry>` 的包装，提供 `add`、`remove`、`remove_connected` 等管理方法。

**Router** 包装 `RouteTable`，提供 `add_route`、`remove_route`、`lookup_route`（最长前缀匹配）以及 `fill_default`（填充默认路由）。

```rust
pub struct Router {
    pub(crate) table: RouteTable,
}
```

**RouteEntry** 结构：

```rust
pub struct RouteEntry {
    pub destination: IpCidr,
    pub next_hop: Option<IpAddress>,
    pub ifindex: u32,
    pub metric: u32,
    pub route_type: RouteType,
}
```

**RouteType** 枚举：

```rust
pub enum RouteType {
    Connected,
    Static,
    Default,
}
```

**最长前缀匹配实现：**

```rust
pub fn lookup_route(&self, dest_ip: Ipv4Address) -> Option<&RouteEntry> {
    let ip = IpAddress::Ipv4(dest_ip);
    let mut best_entry: Option<&RouteEntry> = None;
    let mut best_prefix_len: Option<u8> = None;

    for entry in &self.table.entries {
        if entry.destination.contains_addr(&ip) {
            let prefix_len = entry.destination.prefix_len();
            if best_prefix_len.map_or(true, |best| prefix_len > best) {
                best_prefix_len = Some(prefix_len);
                best_entry = Some(entry);
            }
        }
    }
    best_entry
}
```

**fill_default** 方法在 `route_output` 首次调用时惰性填充：

```rust
pub fn fill_default(&mut self) {
    // 127.0.0.0/8 -> lo (ifindex=1)
    self.add_route(IpCidr::new(IpAddress::Ipv4(Ipv4Address::new(127, 0, 0, 0)), 8),
                   None, 1, 0, RouteType::Connected);
    // DHCP 分配的 CIDR -> eth0
    if let Some(cidr) = net_core::eth0_ipv4_cidr() {
        self.add_route(IpCidr::new(cidr.address(), cidr.prefix_len()),
                       None, eth0_ifindex, 0, RouteType::Connected);
        // 0.0.0.0/0 -> gateway
        if let Some(gw) = net_core::default_gateway() {
            self.add_route(IpCidr::new(IpAddress::Ipv4(Ipv4Address::new(0, 0, 0, 0)), 0),
                           Some(IpAddress::Ipv4(gw)), eth0_ifindex, 100, RouteType::Default);
        }
    }
}
```

### RouteKind 枚举

用于套接字选项（如 `SO_BINDTODEVICE`、IP 选项处理）中的路由分类：

```rust
#[derive(Clone, Debug, PartialEq)]
pub enum RouteKind {
    Local { dst_ifindex: u32 },
    Connected { oif: u32 },
    Gateway { oif: u32, gw: Ipv4Address },
    Unreachable,
}
```

### lookup_source_ip() 和 route_check()

这两个函数是配置文件 `config.rs` 中的便利包装，供 syscall 层使用：

```rust
pub fn lookup_source_ip(dest_ip: IpAddress) -> IpAddress {
    crate::net::routing::route_output(dest_ip)
        .map(|r| r.source)
        .unwrap_or(match dest_ip {
            IpAddress::Ipv4(_) => IpAddress::v4(0, 0, 0, 0),
            IpAddress::Ipv6(_) => IpAddress::v6(0, 0, 0, 0, 0, 0, 0, 0),
        })
}

pub fn route_check(dest: IpAddress) -> Result<(), SyscallErr> {
    crate::net::routing::route_output(dest).map(|_| ())
}
```

---

## DHCP 初始化

DHCP 初始化的全部逻辑位于 `NetInterfaceInner::new()` 的 eth0 栈创建路径中。在具有真实 NIC（`has_real_nic = true`）时触发 DHCP 探针。

### 创建 DHCP Socket

```rust
let mut dhcp_socket = dhcpv4::Socket::new();
dhcp_socket.set_retry_config(dhcpv4::RetryConfig {
    discover_timeout: Duration::from_secs(2),
    initial_request_timeout: Duration::from_secs(1),
    request_retries: 3,
    min_renew_timeout: Duration::from_secs(60),
    ..dhcpv4::RetryConfig::default()
});
let dhcp_handle = eth_sockets.add(dhcp_socket);
```

### 同步探针（5 秒超时）

```rust
let deadline = Instant::from_millis(
    current_time_duration().as_millis() as i64 + 5000,
);

loop {
    let timestamp = Instant::from_millis(current_time_duration().as_millis() as i64);
    *CURRENT_POLL_IFINDEX.lock() = 2;
    eth_iface.poll(timestamp, &mut eth_device, &mut eth_sockets);

    let event = eth_sockets.get_mut::<dhcpv4::Socket>(dhcp_handle).poll();
    match event {
        Some(dhcpv4::Event::Configured(cfg)) => {
            net_core::set_eth0_ipv4(IpCidr::Ipv4(cfg.address));
            net_core::set_default_gateway(cfg.router);
            break;
        }
        Some(dhcpv4::Event::Deconfigured) => {}
        None => {}
    }

    if timestamp >= deadline {
        log::info!("[net::config] DHCP timeout, continuing without IP");
        break;
    }
}

eth_sockets.remove(dhcp_handle);
```

### 成功后的处理

`set_eth0_ipv4` 更新 `ETH0_CIDR` 全局变量和当前 netns device list 中 eth0 的 IP 地址：

```rust
pub fn set_eth0_ipv4(cidr: IpCidr) {
    *ETH0_CIDR.lock() = Some(cidr);
    let list = ns.device_list.lock();
    if let Some(eth0) = list.values().find(|iface| iface.iface_name() == "eth0") {
        for old in eth0.ip_addrs() { eth0.del_ip_addr(old); }
        eth0.add_ip_addr(cidr);
    }
}
```

`set_default_gateway` 更新 `DEFAULT_GW` 全局变量。

DHCP 完成后，将获取的 IP 地址同步到 smoltcp `Interface` 的地址表，并将默认网关写入 smoltcp 路由表：

```rust
if !addrs_src.is_empty() {
    eth_iface.update_ip_addrs(|addrs| {
        for cidr in &addrs_src { addrs.push(*cidr).unwrap(); }
    });
}

if let Some(gw) = net_core::default_gateway() {
    eth_iface.routes_mut().add_default_ipv4_route(gw).unwrap();
}
```

### 超时处理

若 5 秒内未收到 DHCP Offer，打印日志后继续执行，eth0 无 IP 地址。系统仍可通过 loopback（127.0.0.1）进行本地通信。

---

## 轮询编排

### DeviceStack

`DeviceStack` 是每个网络接口栈的封装结构，关联 Iface 元数据、IfaceDevice、smoltcp `Interface` 和 `SocketSet`：

```rust
pub struct DeviceStack<'a> {
    pub nic: Arc<dyn Iface>,
    pub device: IfaceDevice,
    pub iface: Interface,
    pub sockets: SocketSet<'a>,
}
```

`NetInterfaceInner` 维护 `stacks: Vec<DeviceStack>`。当前系统注册了两个固定设备栈：

| 索引 | 名称 | ifindex | 设备类型 | 用途 |
|------|------|---------|---------|------|
| 0 | lo | 1 | `Loopback` | 本地回环通信 |
| 1 | eth0 | 2 | `SmoltcpDeviceAdapter` | 物理网络通信 |

veth 设备通过 `add_veth_stack` 动态注册到 `stacks` 中，通过 `remove_veth_stack` 移除。

### poll() 与 try_poll()

`poll()` 是定时器中断和 syscall 路径调用的标准入口。内部加锁后调用 `poll_once()`。

```rust
pub fn poll(&self) {
    if self.inner.lock().is_none() { return; }
    self.poll_once();
}
```

`try_poll()` 是中断安全变体——使用 `try_lock()` 而非 `lock()`，若锁已被持有则立即返回 `false`，不会自旋：

```rust
pub fn try_poll(&self) -> bool {
    let guard = self.inner.try_lock();
    match guard {
        Some(inner) if inner.is_some() => {
            drop(inner);
            self.poll_once();
            true
        }
        _ => false,
    }
}
```

这在中断上下文（如定时器中断中调用 `try_poll`）中防止了与 syscall handler 的锁竞争。

### poll_once() 详细 5 步流程

`poll_once()` 是核心轮询逻辑，按以下顺序执行：

**步骤 1：预收集待移除的 socket。**

从全局延迟删除列表 `UDP_SOCKETS_TO_REMOVE` 和 `TCP_SOCKETS_TO_REMOVE` 中 drain 所有待移除的 `RouteSocketHandle`。为每个句柄解析其 `SocketHandle` 和所属 `ifindex`。

```rust
let udp_removes: Vec<(Option<SocketHandle>, u32, RouteSocketHandle)> = {
    let mut to_remove = UDP_SOCKETS_TO_REMOVE.lock();
    to_remove.drain(..).map(|rh| {
        let ifindex = inner.bindings.get(&rh).map(|b| b.ifindex)
            .or_else(|| net_core::find_by_name("eth0").map(|d| d.ifindex))
            .unwrap_or(1);
        (inner.resolve(rh), ifindex, rh)
    }).collect()
};
```

**步骤 2：遍历每个 DeviceStack 执行设备级处理。**

对每个栈依次执行以下子步骤：

2a. 设置 `CURRENT_POLL_IFINDEX` 为该栈的 nic_id，供 ARP 拦截器标记邻居条目。
2b. 移除属于该栈的 UDP socket（直接从 `SocketSet` 移除并清理绑定表）。
2c. 如果是 veth 设备，在 smoltcp 消费之前将 veth rx_queue 中的帧交付给 packet socket（`deliver_frames_from_veth_queue`）。
2d. 调用 `stack.iface.poll(timestamp, &mut stack.device, &mut stack.sockets)` 驱动 smoltcp 协议栈。
2e. 移除属于该栈的 TCP socket。TCP socket 必须在 `Closed` 状态时才可移除；若仍在连接中（如 `TIME_WAIT`），将其放回 `TCP_SOCKETS_TO_REMOVE` 等待下一轮。
2f. 调用 `dispatch_udp_packets` 分发收到的 UDP 数据报。

**步骤 3：唤醒等待者。**

若 `poll_once` 推进了协议栈（`progressed == true`），则调用 `wake_tcp_waiters()` 和 `wake_raw_waiters()` 通知所有在 TCP/RAW socket 上等待的线程：

```rust
if progressed {
    crate::net::wake_tcp_waiters();
    crate::net::wake_raw_waiters();
}
```

**步骤 4（备用 `_poll` 路径）：** 另一种轮询实现 `_poll()` 额外执行 `update_io_events()` 同步 IO 事件到 pollee，并增加 `TimeWait` 状态的 TCP 移除条件。

### poll_until_quiescent()

持续轮询直到没有更多数据可处理。每次迭代调用 `try_poll()` 避免死锁，且插入 `try_yield()` 防止独占 CPU：

```rust
pub fn poll_until_quiescent(&self) {
    while self.try_poll() {
        crate::task::try_yield();
    }
}
```

使用场景包括设备初始化后的快速 flush 和批量数据接收。

---

## Socket 管理 API

以下是 `NetInterface` 提供的 socket 管理方法汇总：

| 方法 | 签名 | 用途 |
|------|------|------|
| `add_socket` | `(ifindex: u32, socket: T) -> Option<SocketHandle>` | 向指定设备栈添加 socket，返回 smoltcp `SocketHandle` |
| `add_routed_socket` | `(proto: InetProtocol, socket: T) -> Option<RouteSocketHandle>` | 向默认接口（eth0/lo）添加 socket，返回 `RouteSocketHandle` |
| `add_routed_socket_on` | `(proto, socket, ifindex) -> Option<RouteSocketHandle>` | 向指定 ifindex 添加 socket |
| `tcp_socket` | `(handle: SocketHandle, ifindex, f) -> Option<T>` | 通过 `SocketHandle` 访问 TCP socket |
| `udp_socket` | `(handle: SocketHandle, ifindex, f) -> Option<T>` | 通过 `SocketHandle` 访问 UDP socket |
| `raw_socket` | `(handle: SocketHandle, ifindex, f) -> Option<T>` | 通过 `SocketHandle` 访问 RAW socket |
| `tcp_routed_socket` | `(rh: RouteSocketHandle, f) -> Option<T>` | 通过 `RouteSocketHandle` 访问 TCP socket |
| `udp_routed_socket` | `(rh: RouteSocketHandle, f) -> Option<T>` | 通过 `RouteSocketHandle` 访问 UDP socket |
| `raw_routed_socket` | `(rh: RouteSocketHandle, f) -> Option<T>` | 通过 `RouteSocketHandle` 访问 RAW socket |
| `tcp_connect` | `(rh, remote, local) -> Option<Result<(), ConnectError>>` | 发起 TCP 连接 |
| `remove_routed` | `(rh: RouteSocketHandle)` | 移除路由式 socket |
| `rebind_routed_udp` | `(rh, new_ifindex) -> Option<RouteSocketHandle>` | 将 UDP socket 迁移到另一设备栈 |
| `rebind_routed_raw` | `(rh, new_ifindex, ip_version, ip_protocol) -> Option<RouteSocketHandle>` | 将 RAW socket 迁移到另一设备栈 |
| `add_veth_stack` | `(nic, device: VethDriver)` | 注册 veth 设备栈 |
| `remove_veth_stack` | `(nic_id: u32)` | 移除 veth 设备栈 |
| `add_ip_to_stack` | `(ifindex, cidr: IpCidr)` | 向设备栈添加 IP 地址 |
| `remove_ip_from_stack` | `(ifindex, cidr: IpCidr)` | 从设备栈移除 IP 地址 |
| `stack_ifindexes` | `() -> Vec<u32>` | 返回所有已注册设备栈的 ifindex |
| `socket_stats` | `() -> (usize, usize, usize, usize)` | 返回 (tcp_count, udp_count, raw_count, pending_remove) |

---

## 邻居表

全局邻居表 `NEIGHBOUR_TABLE` 是一个 `BTreeMap<(u32, IpAddress), NeighbourEntry>`，按 `(ifindex, IP)` 键值存储。

```rust
pub static NEIGHBOUR_TABLE: Mutex<BTreeMap<(u32, IpAddress), NeighbourEntry>> =
    Mutex::new(BTreeMap::new());

pub struct NeighbourEntry {
    pub mac: EthernetAddress,
    pub state: u16,
}
```

**状态枚举：**

| 常量 | 值 | 含义 |
|------|-----|------|
| `NUD_REACHABLE` | `0x02` | 邻居可达（ARP 确认） |
| `NUD_STALE` | `0x04` | 邻居条目陈旧 |
| `NUD_PERMANENT` | `0x80` | 静态条目 |

**填充方式：** `CURRENT_POLL_IFINDEX` 在每次 `poll_once` 前设置，`NetRxToken::consume` 和 `VethRxToken::consume` 中调用 `try_capture_arp_reply` 从接收到的以太网帧中提取 ARP 回复。支持的查询入口包括 netlink `RTM_GETNEIGH` 和 `/proc/net/arp`。

```rust
pub fn try_capture_arp_reply(frame_buf: &[u8], ifindex: u32) {
    // 解析 EthernetFrame -> ArpPacket
    // 仅捕获 operation == ArpOperation::Reply
    // 提取 source_hardware_addr 和 source_protocol_addr 调用 neighbour_record
}
```

---

## 已废弃的组件

`adapter.rs` 中保留的 `RoutingDevice` 是多设备软件交换机的旧设计。它在 `transmit` 路径中对每个数据包做基于目标 MAC/IP 的端口路由决策。该设计已被 `IfaceDevice` 枚举 + 每个 `DeviceStack` 独立 `Interface` 的方案取代，保留仅用于兼容性参考。

```rust
pub struct RoutingDevice {
    pub eth: SmoltcpDeviceAdapter,
    pub lo: Loopback,
    pub hw_addr: EthernetAddress,
}
```

`RoutingTxToken::Mixed` 变体会检查目标 MAC 和 IP，决定将数据包发送到 loopback、ethernet 或两者。新设计中每个设备栈的 `IfaceDevice` 独立运行，无需此路由逻辑。

---

## 初始化流程

```
drivers::init_net_device()
  └─ VirtIONetWrapper::new() (MMIO 或 PCI)
  └─ NET_DEVICE = Some(Arc<VirtIONetWrapper>)

config::init()
  ├─ net_core::init()  // 注册 lo (ifindex=1) 和 eth0 (ifindex=2) 到 netns
  │   ├─ add_device(lo)
  │   └─ add_device(eth0)
  └─ NET_INTERFACE.init()
      └─ NetInterfaceInner::new()
          ├─ DeviceStack 0: lo (Loopback, 127.0.0.1/8, ::1/128)
          ├─ DeviceStack 1: eth0 (SmoltcpDeviceAdapter)
          │   ├─ DHCP 探针 (5s 超时)
          │   ├─ net_core::set_eth0_ipv4()
          │   ├─ net_core::set_default_gateway()
          │   └─ 同步 IP 到 smoltcp Interface
          └─ bindings: empty, next_socket_id: 1
```

---

## 文件地图

```
os/src/
├── drivers/
│   ├── net/
│   │   ├── mod.rs              NetDevice trait, NET_DEVICE 全局, init_net_device()
│   │   ├── virtio_net.rs       VirtIONetWrapper (MMIO + PCI 传输层)
│   │   └── veth.rs             VethDriver, VethInterface, veth_pair_new/delete
│   └── block/                  提供 VirtioHal 供 virtio_net 使用
├── net/
│   ├── config.rs               NET_INTERFACE, NetInterfaceInner, DeviceStack, DHCP, poll
│   ├── adapter.rs              IfaceDevice, SmoltcpDeviceAdapter, NullNetDevice, RoutingDevice
│   ├── routing.rs              RouteSocketHandle, SocketBinding, Router, RouteTable, route_output
│   ├── iface.rs                Iface trait, IfaceCommon, SmoltcpDeviceAccess, DeviceKind
│   ├── net_core.rs             NetDeviceEntry, 设备注册, ETH0_CIDR, DEFAULT_GW, current_netns
│   ├── neighbour.rs            NEIGHBOUR_TABLE, CURRENT_POLL_IFINDEX, try_capture_arp_reply
│   └── socket/
│       ├── packet/              packet socket 帧交付 (deliver_frames_from_veth_queue)
│       └── inet/
│           ├── datagram/udp/   udp dispatch (dispatch_udp_packets)
│           └── stream/inner/   tcp_state_code
```
