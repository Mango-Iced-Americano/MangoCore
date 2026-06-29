---
title: "网络接口抽象与设备注册中心"
module: "net_core + iface"
category: net
status: draft
owner: MangoCore Team
last_updated: "2026-06-29"
code_paths:
  - "os/src/net/iface.rs"
  - "os/src/net/net_core.rs"
  - "os/src/net/ioctl.rs"
entry_points:
  - "net_core::init()"
  - "siocgif_dispatch()"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "sockioctl01"
  oscomp:
    - "basic"
    - "busybox"
related_docs:
  - "docs/06_net/architecture.md"
  - "docs/06_net/device-stack-and-poll.md"
  - "docs/06_net/device-adapter.md"
  - "docs/06_net/routing.md"
  - "docs/06_net/dhcp.md"
  - "docs/06_net/test-map.md"
---

# 网络接口抽象与设备注册中心

## 概述

网络接口抽象层和设备注册中心是 MangoCore 网络子系统的底层基础设施。`os/src/net/iface.rs` 定义了 `Iface` trait（所有网络设备的统一接口）、`IfaceCommon`（共享 per-interface 状态）和 `SmoltcpDeviceAccess`（&self 设备抽象）。`os/src/net/net_core.rs` 提供设备注册中心、全局 ifindex 计数器、DHCP 网关状态和 `NetDeviceEntry`（`Iface` 的基准实现）。`os/src/net/ioctl.rs` 实现 SIOCGIF\* 系列 ioctl，通过设备注册中心查询接口元数据。

本文将 iface trait 定义层和设备注册管理层合并描述，涵盖从接口元数据操作到设备生命周期管理的完整路径。轮询、路由和 DHCP 探针细节不在本文范围内，相关文档见 [device-stack-and-poll.md](device-stack-and-poll.md)、[routing.md](routing.md) 和 [dhcp.md](dhcp.md)。

## 设计目标

- **统一接口抽象**: 通过 `Iface` trait 为 loopback、以太网和 veth 设备提供一致的元数据操作接口（名称、MAC、IP、MTU、flags）。
- **无泛型 smoltcp 集成**: `IfaceCommon` 存储 `smoltcp::Interface` 时不携带设备类型泛型参数，通过运行时传入设备引用的方式绕过 Rust GAT 限制。
- **命名空间隔离**: 设备注册中心基于 `NetNamespace`，每个 netns 拥有独立的 `BTreeMap<usize, Arc<dyn Iface>>` 设备列表和路由表。
- **ioctl 兼容**: 支持 Linux SIOCGIF\* 系列 ioctl（`SIOCGIFINDEX`、`SIOCGIFFLAGS`、`SIOCGIFADDR`、`SIOCGIFHWADDR` 等），通过设备注册中心查询当前 netns 中的设备信息。

## 架构

```
+-----------------------------------------------------+
|                    用户的 ioctl / syscall             |
+-----------------------------------------------------+
|              net_core.rs (设备注册中心)                |
|  add_device() / remove_device() / find_by_name()    |
|  find_by_index() / current_netns() / init()         |
|  ETH0_CIDR / DEFAULT_GW                             |
+---------------------------+-------------------------+
                            |
               +-----------+-----------+
               |                       |
               v                       v
     iface.rs (trait 定义)    ioctl.rs (SIOCGIF*)
     Iface trait              siocgif_dispatch()
     IfaceCommon              每个 cmd→handler
     SmoltcpDeviceAccess      操作 DeviceEntry
     DeviceKind
               |
               v
     NetDeviceEntry (Iface impl)
     成员：nic_id, name, flags, mtu,
     ip_addrs, hwaddr, kind,
     smoltcp_iface, sockets
               |
               v
     NetNamespace (device_list)
     BTreeMap<usize, Arc<dyn Iface>>
```

## 关键数据结构

### DeviceKind

三层变体枚举，标识网络设备的链路层类型：

```rust
pub enum DeviceKind {
    Loopback,
    Ethernet,
    Veth,
}
```

`Loopback` 用于 lo 设备（127.0.0.1/8, ::1/128），`Ethernet` 用于物理 virtio-net 网卡 eth0，`Veth` 用于虚拟以太网对。

### Iface  trait

所有网络接口的统一抽象。每个实现必须提供以下方法：

| 方法 | 签名 | 说明 |
|------|--------|-------|
| `nic_id` | `fn nic_id(&self) -> usize` | 注册时分配的全局唯一 ID |
| `iface_name` | `fn iface_name(&self) -> String` | 接口名（"lo", "eth0", "veth0"） |
| `set_iface_name` | `fn set_iface_name(&self, name: &str)` | 重命名接口 |
| `flags` | `fn flags(&self) -> u32` | IFF_UP, IFF_RUNNING 等标志位 |
| `set_flags` | `fn set_flags(&self, flags: u32)` | 更新标志 |
| `mtu` | `fn mtu(&self) -> usize` | 最大传输单元 |
| `set_mtu` | `fn set_mtu(&self, mtu: usize)` | 更新 MTU |
| `ip_addrs` | `fn ip_addrs(&self) -> Vec<IpCidr>` | 所有 IP 地址（CIDR） |
| `add_ip_addr` | `fn add_ip_addr(&self, addr: IpCidr)` | 添加 IP 地址 |
| `del_ip_addr` | `fn del_ip_addr(&self, addr: IpCidr)` | 删除 IP 地址 |
| `mac` | `fn mac(&self) -> [u8; 6]` | MAC 地址 |
| `kind` | `fn kind(&self) -> DeviceKind` | 设备类型 |
| `peer_ifindex` | `fn peer_ifindex(&self) -> Option<usize>` | veth 对端 nic_id |
| `common` | `fn common(&self) -> &IfaceCommon` | 访问共享状态 |
| `as_smoltcp_device` | `fn as_smoltcp_device(&self) -> &dyn SmoltcpDeviceAccess` | 轮询用设备引用 |

`Iface` 需要 `Send + Sync + fmt::Debug` 以确保跨线程安全。

### IfaceCommon

共享 per-interface 状态结构体，存储元数据和 smoltcp 协议引擎。设计要点：smoltcp 的 `Interface` 在构造时不绑定设备类型，设备在每次 `poll()` 调用时作为参数传入。这使得 `Interface` 可以放在 `IfaceCommon` 中而无需泛型参数。

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

字段说明：

- `nic_id`: 从全局 `NEXT_IFINDEX` 分配的原子计数器，构造时初始化为 0，注册时赋值。
- `name`: 受 `RwLock` 保护的字符串读写。读多写少的场景（ioctl 查询）友好。
- `flags` / `mtu`: 原子类型，无需加锁。
- `ip_addrs`: 受 `Mutex` 保护的 IP 地址列表。
- `smoltcp_iface` / `sockets`: smoltcp 协议引擎的接口实例和 socket 集合。
- `net_namespace`: 使用 `Weak` 避免引用循环。namespace 持有 `Arc<dyn Iface>` 条目，接口通过弱引用指回所属 namespace。

`IfaceCommon::new()` 接收已初始化的 `Interface` 和 `SocketSet`，其他字段设置为默认值。

### SmoltcpDeviceAccess

`&self` 版本的设备抽象。smoltcp 原生的 `Device` trait 要求 `&mut self`，不适合 `Arc` 共享。`SmoltcpDeviceAccess` 暴露 `poll()` 和 `capabilities()`，轮询循环通过 `Iface::as_smoltcp_device()` 调用，具体设备内部使用 `Mutex` 管理可变状态。

### NetDeviceEntry

`NetDeviceEntry` 是 `Iface` 的基准实现，用于 lo 和 eth0 等元数据型设备。它保存完整的接口元数据（名称、flags、MTU、IP、MAC、设备类型等），并包含一个 dummy smoltcp `Interface` 和 `SocketSet` 以满足 trait 接口要求。

```rust
pub struct NetDeviceEntry {
    nic_id: AtomicUsize,
    name: Mutex<String>,
    flags: AtomicU32,
    mtu: AtomicUsize,
    ip_addrs: Mutex<Vec<IpCidr>>,
    hwaddr: [u8; 6],
    kind: DeviceKind,
    peer_ifindex: Option<usize>,
    operstate: AtomicU32,
    smoltcp_iface: Mutex<Interface>,
    sockets: Mutex<SocketSet<'static>>,
    net_namespace: RwLock<Option<Weak<NetNamespace>>>,
}
```

注意 `NetDeviceEntry` 的 `name` 使用 `Mutex<String>` 而非 `IfaceCommon` 的 `RwLock<String>`。这是历史遗留差异，未来迁移会消除。`set_nic_id()` 在构造后由注册中心调用，赋值真正的 ifindex。

`Iface` 实现中，`common()` 和 `as_smoltcp_device()` 当前 `panic!`（标记为 Wave 2 待实现）。因为 `NetDeviceEntry` 仅作为注册中心的元数据载体，实际协议处理由 `NetInterface` 的 `DeviceStack` 完成。当具体的 loopback 和 veth 类型被创建后，这两个方法将获得真实实现。

### 全局状态

```rust
static NEXT_IFINDEX: AtomicU32 = AtomicU32::new(3);
pub static ref ETH0_CIDR: Mutex<Option<IpCidr>> = Mutex::new(None);
pub static ref DEFAULT_GW: Mutex<Option<Ipv4Address>> = Mutex::new(None);
```

- `NEXT_IFINDEX`: 从 3 开始递增（ifindex 1 预留给 lo，2 预留给 eth0）。
- `ETH0_CIDR`: DHCP 探针完成后设置的 eth0 IPv4 CIDR。
- `DEFAULT_GW`: DHCP 获取的默认网关。

## 执行流程

### 设备注册流程

`net_core::init()` 在 `net::config::init()` 中调用，幂等地注册 lo 和 eth0：

1. 若当前 init netns 的设备列表非空，提前返回。
2. 创建 lo（`DeviceKind::Loopback`, ifindex=1, 127.0.0.1/8 + ::1/128, MTU 65536）。
3. 若 `NET_DEVICE` 存在（virtio-net 已初始化），创建 eth0（`DeviceKind::Ethernet`, ifindex=2, MAC 从硬件读取）。
4. 每个 `NetDeviceEntry` 构造后调用 `set_nic_id()` 分配 ifindex，然后 `ns.add_device(iface)` 插入 `BTreeMap`。

`add_device()` 还负责设置 iface 的 `net_namespace` 弱引用：

```rust
pub fn add_device(iface: Arc<dyn Iface>) {
    let ns = current_netns();
    *iface.common().net_namespace.write() = Some(Arc::downgrade(&ns));
    ns.add_device(iface);
}
```

### 设备查询流程

```
find_by_name("eth0")
  -> current_netns()
    -> 若当前任务有 netns，返回其 namespace
    -> 否则返回 INIT_NET_NAMESPACE
  -> ns.device_by_name("eth0")
    -> 遍历 BTreeMap values，匹配 iface_name
    -> 返回 Some(DeviceEntry { ifindex, iface })

find_by_index(2)
  -> current_netns()
  -> ns.device_by_index(2)
    -> BTreeMap.get(&2)
    -> 返回 Some(DeviceEntry { ifindex: 2, iface })
```

### SIOCGIF\* ioctl 流程

`ioctl.rs` 的 `siocgif_dispatch()` 是 SIOCGIF 系列 ioctl 的统一入口，匹配命令码后分发到对应 handle：

```
syscall (SYS_IOCTL, fd, SIOCGIFINDEX, &ifreq)
  -> siocgif_dispatch(SIOCGIFINDEX, arg)
    -> read_ifreq(arg): 从用户空间读取 ifreq
    -> siocgifindex(&mut ifr)
      -> find_dev(name_str)
        -> net_core::find_by_name(name) -> DeviceEntry
      -> ifr.set_ifr_ifindex(d.ifindex)
    -> write_ifreq(arg, &ifr): 写回用户空间
```

支持的 ioctl 命令：

| 命令 | 操作 |
|------|------|
| `SIOCGIFCONF` | 遍历 netns 所有设备，返回名称和 IPv4 地址 |
| `SIOCGIFINDEX` | 按名称查找，返回 ifindex |
| `SIOCGIFFLAGS` | 按名称查找，返回 flags |
| `SIOCGIFADDR` | 按名称查找，返回第一个 IPv4 地址 |
| `SIOCGIFNETMASK` | 从 prefix_len 计算子网掩码 |
| `SIOCGIFBRDADDR` | 计算广播地址 |
| `SIOCGIFMTU` / `SIOCGIFHWADDR` | 按名称查找，返回 MTU 或 MAC |
| `SIOCGIFNAME` | 按 ifindex 查找，返回名称 |
| `SIOCGIFTXQLEN` | 按名称查找，返回固定值 1000 |
| `SIOCSIFFLAGS` / `SIOCSIFADDR` / `SIOCSIFMTU` | 更新元数据并同步到 smoltcp DeviceStack |

设置类 ioctl（`SIOCSIF*`）在更新 netns 设备列表后，通过 `NET_INTERFACE.inner_handler()` 同步到对应的 smoltcp `DeviceStack`。

## 接口与 API

### 设备注册管理

| 函数 | 说明 |
|------|------|
| `init()` | 注册 lo (ifindex=1) 和 eth0 (ifindex=2) 到初始命名空间，幂等 |
| `add_device(iface)` | 将 iface 注册到当前 netns，自动设置 net_namespace 反向引用 |
| `remove_device(nic_id)` | 从当前 netns 移除设备 |
| `find_by_name(name)` | 按名称在当前 netns 中查找，返回 `DeviceEntry` |
| `find_by_index(idx)` | 按 ifindex 在当前 netns 中查找 |
| `next_ifindex()` | 返回并递增全局 ifindex 计数器 |
| `current_netns()` | 返回当前进程的 netns，无进程时返回 `INIT_NET_NAMESPACE` |
| `default_iface()` | 返回 eth0（优先）或 lo |
| `loopback_iface()` | 返回 loopback 接口 |

### DHCP 网关状态

| 函数 | 说明 |
|------|------|
| `eth0_ipv4_cidr()` | 读取 DHCP 分配的 eth0 CIDR |
| `set_eth0_ipv4(cidr)` | 设置 DHCP CIDR 并更新设备列表中的 eth0 IP |
| `default_gateway()` | 读取默认网关 |
| `set_default_gateway(gw)` | 设置默认网关 |
| `is_local_addr(addr)` | 检查地址是否属于本地任一接口 |
| `ifindex_for_local_addr(addr)` | 返回拥有该 IP 的设备 ifindex |

## 测试映射

| 特性 | LTP 用例 | OSComp 分组 | 状态 |
|------|----------|-------------|------|
| SIOCGIF\* ioctl | `sockioctl01` | basic / busybox | 待验证 |
| 设备注册 (`net_core::init`) | — | basic | 集成到启动流程 |
| `current_netns()` | — | — | 每系统调用间接覆盖 |
| `add_device` / `remove_device` | — | — | veth 创建销毁路径覆盖 |
| `is_local_addr` | — | busybox (ping localhost) | 通过 |
| loopback 设备 | — | basic (ping 127.0.0.1) | 通过 |
| SIOCSIFFLAGS / SIOCSIFADDR | `sockioctl01` | — | 待验证 |

### LTP 跳过清单

| 用例 | 跳过原因 |
|------|----------|
| 命名空间相关 (`netns\*`) | `NetNamespace` 支持但 ioctl 多空间枚举暂缺 |
| 高级 ioctl（`SIOCGIFBR`、`SIOCGIW\*`） | 桥接和无线功能不支持 |

## 已知问题

1. **NetDeviceEntry 的 common() 和 as_smoltcp_device() 未实现**
   - 现象: 调用 `NetDeviceEntry::common()` 或 `as_smoltcp_device()` 会 panic。
   - 根因: `NetDeviceEntry` 是纯元数据容器，dummy smoltcp `Interface` 不参与轮询。标记为 Wave 2 待实现，待具体的 loopback/veth 类型就绪后实现。
   - 影响: 当前注册路径不涉及这两个方法，功能正常。任何直接调用 `iface.common()` 的第三方代码会触发 panic。
   - 修复方向: 创建 `LoIface` 和 `VethIface` 具体类型，移出 `NetDeviceEntry` 的 trait 脏实现。

2. **NEXT_IFINDEX 跨命名空间共享**
   - 现象: 所有 netns 的 ifindex 来自同一个全局计数器，ifindex 跨 ns 唯一但不会复用。
   - 根因: `NEXT_IFINDEX` 是 `static AtomicU32`，初始值 3（1=lo, 2=eth0），单调递增永不回收。
   - 影响: 理论上 ifindex 可耗尽（但 2^32 个设备在实践中不会达到）。删除设备后 ifindex 不被复用。
   - 修复方向: 短期无影响，长期可考虑 per-ns 计数器或 ifindex 回收池。

3. **ioctl 设置类命令锁顺序依赖**
   - 现象: `SIOCSIFFLAGS`、`SIOCSIFADDR`、`SIOCSIFMTU` 先持 `device_list` 锁再取 `NET_INTERFACE.inner` 锁。
   - 风险: 如果轮询循环反向锁顺序（先 inner 再 device_list），会导致死锁。当前设计通过及时释放 `device_list` 锁来避免。
   - 影响: 当前单核环境无并发锁竞争风险。多核扩展时需统一锁顺序。

## 参考资料

- [网络子系统架构](architecture.md) — 三层架构总览和图解
- [device-stack-and-poll.md](device-stack-and-poll.md) — NetInterface、DeviceStack、poll 循环
- [device-adapter.md](device-adapter.md) — IfaceDevice、设备适配层
- [routing.md](routing.md) — 路由表、FIB、route_output
- [neighbour.md](neighbour.md) — ARP/NDP 邻居表
- [dhcp.md](dhcp.md) — DHCP 初始化流程
- [网络测试映射](test-map.md) — LTP 和 OSComp 测试覆盖详情
- `os/src/net/iface.rs` — Iface trait 和 IfaceCommon 定义
- `os/src/net/net_core.rs` — 设备注册中心具体实现
- `os/src/net/ioctl.rs` — SIOCGIF\* ioctl 分发
- `os/src/task/net_namespace.rs` — NetNamespace 和设备列表管理
- Linux `netdevice(7)` — SIOCGIF\* 标准语义
