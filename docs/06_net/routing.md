---
title: "路由子系统 (Routing / FIB Layer)"
module: "os/src/net/routing.rs + config.rs (route_check, lookup_source_ip)"
category: net
status: current
owner: MangoCore Team
last_updated: "2026-08-05"
code_paths:
  - "os/src/net/routing.rs"
  - "os/src/net/config.rs"
entry_points:
  - "route_output() — 全局路由查询入口"
  - "Router::lookup_route() — 最长前缀匹配查找"
  - "Router::fill_default() — 默认路由惰性填充"
  - "lookup_source_ip() — 源地址选择"
  - "route_check() — 连通性检查"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "sendto01, connect01, bind01 (依赖路由可达)"
  oscomp:
    - "basic (ping), busybox (ifconfig/route)"
related_docs:
  - "docs/06_net/architecture.md"
  - "docs/06_net/device-stack-and-poll.md"
  - "docs/06_net/net-core-iface.md"
  - "docs/06_net/dhcp.md"
---

# 路由子系统 (Routing / FIB Layer)

> 2026-07-13：DHCP 运行时事件通过 Router::replace_dhcp_ipv4() 一次替换 eth0
> 的 connected/default 路由；租约失效时同时删除两者，避免保留过期网关。
> connected 路由在写表前按前缀归一化，例如租约 `192.168.1.3/24`
> 发布为 `192.168.1.0/24`，而接口地址仍保持主机地址。

## 概述

路由子系统实现 MangoCore 内核的转发信息库（FIB），负责将目标 IP 地址映射到出接口（ifindex）、下一跳网关和源 IP 地址。它是网络协议栈中 socket 层与设备层之间的桥梁，为 TCP/UDP/RAW 套接字的收发路径提供路由决策。

整个子系统分布在两个文件中：`routing.rs` 定义核心数据结构和主路由函数，`config.rs` 提供两个便利包装供 syscall 层使用。

## 核心数据结构

### RouteSocketHandle

不透明的 `usize` 包装体，用作套接字路由绑定的键。每个 routed socket 在
`NetDirectory.routes` 中拥有唯一且不复用的 `RouteSocketHandle`，格式化为
`RH(N)` 便于日志追踪。目录只快照 Active route、protocol 与目标栈 Weak，释放
目录锁后才进入 DeviceStack，并按 route ID/protocol/local binding 重验。

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RouteSocketHandle(pub(crate) usize);
```

`PartialOrd + Ord` 的实现使其可以作为 `BTreeMap` 的键使用。

### InetProtocol

三值枚举，标记已绑定 socket 的协议类型：

```rust
pub(crate) enum InetProtocol { Tcp, Udp, Raw }
```

不对外暴露，同时用于目录条目和设备栈内 binding 的一致性重验。

### RouteDirectoryEntry 与 LocalSocketBinding

route 映射拆成两个锁域。目录条目只定位设备栈；本地条目与 SocketSet 同锁域，最终确定 smoltcp handle：

```rust
struct RouteDirectoryEntry<'a> {
    stack: Weak<DeviceStackCell<'a>>,
    protocol: InetProtocol,
    state: RouteState,
}

struct LocalSocketBinding {
    handle: SocketHandle,
    protocol: InetProtocol,
}
```

访问者在目录锁内升级 `Weak` 并克隆稳定的栈 `Arc`，随后释放目录锁；取得目标设备栈锁后按 route ID 和 protocol 重验本地条目。该顺序禁止 N0/N2 嵌套，也防止已回收 SocketSet slot 被旧 route 误用。

### RouteDecision

`route_output()` 的返回值，描述完整的路由决策：

```rust
pub struct RouteDecision {
    pub ifindex: u32,              // 出接口索引
    pub source: IpAddress,         // 从该接口选出的源 IP
    pub next_hop: Option<IpAddress>, // 下一跳网关（Connected 路由为 None）
    pub is_local: bool,            // 目标是否为本机地址
}
```

`is_local = true` 时表示目标 IP 属于本机某个接口，数据包走本地交付路径（不经过 smoltcp 的 IP 层输出）。

### RouteKind

用于套接字选项（如 `SO_BINDTODEVICE`）中的路由分类枚举：

```rust
pub enum RouteKind {
    Local { dst_ifindex: u32 },
    Connected { oif: u32 },
    Gateway { oif: u32, gw: Ipv4Address },
    Unreachable,
}
```

### RouteEntry

路由表中的单条记录：

```rust
pub struct RouteEntry {
    pub destination: IpCidr,        // 目标 CIDR
    pub next_hop: Option<IpAddress>, // 下一跳（Connected 路由无）
    pub ifindex: u32,               // 出接口
    pub metric: u32,                // 路由度量（值小优先）
    pub route_type: RouteType,      // 路由类型
}
```

### RouteType

路由类型枚举：

```rust
pub enum RouteType {
    Connected,  // 直连网络
    Static,     // 静态配置路由
    Default,    // 默认路由 (0.0.0.0/0 或 ::/0)
}
```

### RouteTable

`Vec<RouteEntry>` 的简单包装，提供增删管理方法：

```rust
pub struct RouteTable {
    pub entries: Vec<RouteEntry>,
}
```

| 方法 | 签名 | 用途 |
|------|------|------|
| `add` | `(&mut self, entry: RouteEntry)` | 添加路由条目 |
| `remove` | `(&mut self, destination: &IpCidr)` | 按目标 CIDR 移除所有匹配条目 |
| `remove_connected` | `(&mut self, ifindex: u32, dest: &IpCidr)` | 仅移除指定接口的直连路由 |

### Router

路由表的高级操作接口，包装 `RouteTable`：

```rust
pub struct Router {
    pub(crate) table: RouteTable,
}
```

| 方法 | 用途 |
|------|------|
| `add_route(dest, next_hop, ifindex, metric, route_type)` | 向路由表添加条目 |
| `remove_route(dest: &IpCidr)` | 按目标 CIDR 移除路由 |
| `lookup_route(dest_ip: Ipv4Address)` | 最长前缀匹配查找 |
| `lookup_route_owned(dest_ip: Ipv4Address)` | 返回克隆的 `RouteEntry`，避免借用问题 |
| `fill_default()` | 惰性填充环路/直连/默认路由 |

## 最长前缀匹配

`lookup_route()` 遍历路由表，筛选出 CIDR 覆盖目标 IP 的条目，选择前缀长度最大的作为最优路由。当 `best_prefix_len` 为 `None`（首次匹配）时无条件选中，后续仅在 `prefix_len` 严格大于当前最优值时替换。

**注意**：该实现仅比较前缀长度，不比较 `metric`。如果多条不同 metric 的路由匹配同一目标，metric 较小的条目不保证被选中。当前路由表设计假设同一前缀不会出现多条不同 metric 的路由。

## 路由输出流程

`route_output()` 是全局路由查询的唯一入口。接收目标 IP 地址，返回 `RouteDecision` 或 `SyscallErr::ENETUNREACH`。

```
route_output(dest)
  │
  ├─ 1. 检查 Router.table.entries 是否为空
  │     空 → 调用 fill_default() 惰性填充
  │
  ├─ 2. [IPv4] 遍历 netns.device_list
  │     ├─ 目标为己方接口 IP → is_local = true
  │     ├─ 127.x.x.x → 强制走 loopback (ifindex=1)
  │     └─ 查 Router.lookup_route() → 返回 RouteEntry
  │
  ├─ 3. [IPv6] 遍历 netns.device_list
  │     ├─ 目标为己方接口 IP → is_local = true
  │     ├─ ::1 → 强制走 loopback (ifindex=1)
  │     └─ 线性扫描路由表 → 返回 RouteEntry
  │
  └─ 4. 均未命中 → ENETUNREACH
```

### IPv4 路径详解

1. **延迟填充**：首次调用时路由表为空，自动调用 `fill_default()` 生成三条路由。
2. **本机地址检查**：遍历 `netns.device_list`，检查目标 IP 是否属于任何接口。匹配时设置 `is_local = true`，源 IP 取该设备第一个地址。
3. **127.x.x.x 硬编码**：不依赖路由表，直接返回 loopback 接口。
4. **路由表查询**：调用 `lookup_route_owned()` 获取 `RouteEntry`，提取出接口和下一跳；源 IP 从出接口的第一个地址获取。
5. **无路由**：返回 `ENETUNREACH`。

### IPv6 路径

IPv6 路径先检查本机地址，再查 `::1` loopback，最后线性扫描路由表。**与 IPv4 不同**，IPv6 不做最长前缀比较——遍历时返回第一个匹配条目，而非前缀长度最长的条目。这是一个已知的简化实现。

### fill_default 惰性填充

`fill_default()` 生成三条基础路由：

| 目标 | 下一跳 | ifindex | metric | 类型 | 来源 |
|------|--------|---------|--------|------|------|
| `127.0.0.0/8` | None | 1 (lo) | 0 | Connected | 固定 |
| DHCP CIDR 的 network | None | 2 (eth0) | 0 | Connected | `eth0_ipv4_cidr()` |
| `0.0.0.0/0` | DHCP 网关 | 2 (eth0) | 100 | Default | `default_gateway()` |

默认路由的 metric 为 100（高于直连路由的 0），确保直连匹配优先。此方法可安全多次调用——调用前已确保路由表为空才触发填充。

## 便利包装函数

以下两个函数位于 `config.rs`，提供给 syscall 层直接使用：

### lookup_source_ip

根据目标 IP 选择源 IP 地址。在 socket 连接（connect）和发送（sendto）路径中调用，用于填充未绑定 socket 的源地址：

```rust
pub fn lookup_source_ip(dest_ip: IpAddress) -> IpAddress {
    route_output(dest_ip)
        .map(|r| r.source)
        .unwrap_or(match dest_ip {
            IpAddress::Ipv4(_) => IpAddress::v4(0, 0, 0, 0),
            IpAddress::Ipv6(_) => IpAddress::v6(0, 0, 0, 0, 0, 0, 0, 0),
        })
}
```

路由不可达时返回 `0.0.0.0` 或 `::`（全零地址），调用方需自行处理后续错误。

### route_check

连通性快速检查。在 `connect()` 路径中用于验证目标 IP 是否可达：

```rust
pub fn route_check(dest: IpAddress) -> Result<(), SyscallErr> {
    route_output(dest).map(|_| ())
}
```

成功返回 `Ok(())`，失败返回 `Err(ENETUNREACH)`。背后的 `RouteDecision` 被丢弃。

## RouteSocketHandle → DeviceStack 解析链

从用户 socket fd 到底层 smoltcp 协议栈的完整解析路径：

```
用户 socket fd
  → (通过 Socket trait →) SocketFile.read/write
  → TcpSocket.connect(remote)
  → route_check(remote) + lookup_source_ip(remote)
  → route_output(remote)
  └── RouteDecision { ifindex, source, next_hop, is_local }

  → NetInterface::tcp_connect(RouteSocketHandle, remote, local)
  → NetDirectory.routes[BTreeMap] → Weak<DeviceStackCell>
     └── 释放目录锁，取得目标 DeviceStack
     └── LocalSocketBinding 按 route ID/protocol 重验
     └── sockets.get_mut::<tcp::Socket>(handle)
```

1. 用户调用 `connect(fd, addr)` → syscall 分配 `RouteSocketHandle`。
2. syscall 先在目标栈建立本地 binding，再向 `NetDirectory` 发布 Active route。
3. 传输数据时通过 `tcp_routed_socket(rh, ...)` 等方法完成读写。
4. 销毁时调用 `remove_routed(rh)` 同时清理 `SocketSet` 和绑定表。

`RouteSocketHandle` 将用户态 fd 与底层 smoltcp 完全解耦——fd 只需持有此句柄，无需关心设备栈结构。

## 测试映射

| 场景 | 测试方式 | 覆盖范围 |
|------|----------|----------|
| 直连路由可达 | ping 127.0.0.1, ping DHCP 网段 | 基本路由查找 + is_local |
| 默认路由 | ping 8.8.8.8（需 DHCP 获取网关） | 默认路由命中 + next_hop |
| 路由不可达 | ping 10.255.255.1（无路由） | ENETUNREACH |
| 源 IP 选择 | connect() 未绑定 socket | lookup_source_ip |
| 多设备栈 | veth 对间通信（跨 netns） | route_output ifindex 正确性 |
| LTP | connect01, sendto01, bind01 | 路由可达性检查 |

## 已知问题

- **IPv6 路由查找不是 LPM**：IPv6 路径使用线性扫描返回第一个匹配条目，而非按前缀长度排序。对于存在多条匹配的路由（例如同时存在 `::/0` 默认路由和更具体的 `2001:db8::/32`），行为不可预测。
- **缺少 metric 比较**：`lookup_route()` 仅比较前缀长度，不比较 `route.metric`。如果两个条目有相同前缀长度但不同 metric，不保证低 metric 条目胜出。
- **无多路径支持**：`route_output()` 返回单条 `RouteDecision`，不支持 ECMP（等价多路径）或权重分发。
- **无动态路由协议**：路由表完全静态配置（`add_route` + `remove_route`），没有与 OSPF/BGP/RIP 等动态路由协议交互的接口。
- **无路由缓存**：每次 `route_output()` 均遍历全表。对于路由表条目较少的内核场景这不是问题，但大数据量下可能有性能影响。
