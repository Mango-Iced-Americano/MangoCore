---
title: "INET 公共基础设施 (PortManager / BoundInner / Address)"
module: "net/socket/inet/common"
category: net
status: current
owner: MangoCore Team
last_updated: "2026-08-05"
code_paths:
  - "os/src/net/socket/inet/common/address.rs"
  - "os/src/net/socket/inet/common/port.rs"
  - "os/src/net/socket/inet/common/port/registry.rs"
  - "os/src/net/socket/inet/common/bound.rs"
entry_points:
  - "PortManager"
  - "BoundInner"
  - "address::fill_with_endpoint"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "bind01"
    - "bind02"
    - "bind03"
    - "bind04"
  oscomp:
    - "basic"
    - "busybox"
related_docs:
  - "docs/06_net/tcp.md"
  - "docs/06_net/udp.md"
  - "docs/06_net/raw.md"
  - "docs/06_net/socket-trait-and-fd.md"
---

## 概述

`inet/common` 模块提供 TCP、UDP、RAW 三种 INET socket 共享的公共类型：端口管理、绑定状态追踪、地址结构与转换。三个文件分别对应三个独立职责，无循环依赖。

## 源文件职责

| 文件 | 职责 |
|------|------|
| `port.rs` / `port/registry.rs` | `PortManager` 事务入口与每 netns 的端口预留表 |
| `bound.rs` | `BoundInner` 结构体：每个 socket 的绑定状态（handle、ifindex、addr、port） |
| `address.rs` | `SocketAddrv4`/`SocketAddrv6` 地址结构、`IpEndpoint`/`IpListenEndpoint` 转换、用户态地址读写 |

## PortManager

`PortManager` 保留系统调用侧的统一入口，权威状态位于每个
`NetNamespace::ports: Mutex<PortRegistry>`。registry 以
`(protocol, family, address, port, ifindex)` 建 bucket，每个 owner 由单调 token 与
`Weak<dyn Socket>` 共同标识；TCP/UDP 彼此独立，不同 netns 也不共享占用状态。

### 临时端口分配

显式 `bind(port=0)` 和 INET auto-bind 使用同一条事务路径。registry 锁内先清理
dead owner，再从 49152..65534 环形扫描可用端口，创建 `Reserved` owner；释放锁后
执行 socket 自身 bind，最后重新进入同一 netns 按 key、token 和 Weak identity
`commit`，失败则 `abort`。因此两个 CPU 不能同时把同一非复用 endpoint 绑定成功。

TCP connect/listen、UDP connect 与首个未连接 send 都通过
`ensure_auto_bound()` 复用该协议。候选源地址在 registry 锁外推导，registry 锁
绝不跨 socket lifecycle 或 DeviceStack 操作。

### 端口冲突检测

冲突检查同时覆盖 `Reserved` 与 `Bound` owner：同地址族 wildcard 与具体地址重叠；
未设置 `IPV6_V6ONLY` 的 IPv6 wildcard 会与 IPv4 端点重叠；ifindex 为 `None` 时
与任意接口重叠。`SO_REUSEADDR`/`SO_REUSEPORT` 只有双方快照均兼容时才放行。

### `bind_port(task, socket, endpoint)` 统一入口

`sys_bind` 应调用 `PortManager::bind_port()` 而不是手动 `check + bind`。该方法：

1. 非 TCP/UDP IP endpoint（如 Raw/Packet）直接调用 `socket.bind()`。
2. TCP/UDP 在不持 registry 锁时从 socket 快照不可变 `BindIntent`。
3. 执行 `reserve -> socket.bind -> commit/abort`；成功后把唯一的
   `PortReservation` 安装进 socket，socket Drop 时精确释放自己的 owner。

## BoundInner

每个 INET socket 持有 `Mutex<BoundInner>` 记录其绑定状态：

```rust
pub struct BoundInner {
    pub socket_handle: Option<RouteSocketHandle>,  // smoltcp socket handle
    pub ifindex: u32,                              // 绑定接口索引
    pub bound_addr: Option<IpAddress>,             // 绑定地址（None = 任意）
    pub bound_port: u16,                           // 绑定端口（0 = 未绑定）
}
```

`is_bound()` 检查 `socket_handle.is_some()`；`bound_iface()` 通过 `ifindex` 查询 `DeviceEntry`。`TcpSocket::bind()` 和 `UdpSocket::bind()` 内部调用 `bound.bind()` 更新这些字段。

## Address 类型与转换

### SocketAddrv4 / SocketAddrv6

`#[repr(C)]` 的内核态 sockaddr 表示，与用户态布局一致：

- **SocketAddrv4**: `sin_port`(2B) + `sin_addr`(4B) + `sin_zero`(8B)，网络字节序。
- **SocketAddrv6**: `sin6_port`(2B) + `sin6_flowinfo`(4B) + `sin6_addr`(16B)，网络字节序。

双向转换 `From<IpEndpoint>` / `From<SocketAddrvX>` 桥接 smoltcp 的 `IpEndpoint` 与内核地址结构。

### listen_endpoint / to_endpoint

`listen_endpoint(buf)` 从用户态 `sockaddr` 字节流解析出 `IpListenEndpoint`：

- 未指定地址（`0.0.0.0`）→ `addr: None`（表示监听所有接口）
- 端口为 0 → 保持未分配；只有拿到 socket `Arc` 的 bind/auto-bind 事务才能分配

`to_endpoint(listen_endpoint)` 将 `IpListenEndpoint` 转换为完整 `IpEndpoint`：将 `addr: None` 替换为实际接口 IP 或回环地址。

`fill_with_endpoint(endpoint, addr, addrlen)` 执行 `getsockname`/`getpeername` 的地址回写流程：

1. 校验 `addr` 和 `addrlen` 非空、4 字节对齐、长度充足。
2. 根据 `IpAddress::Ipv4/Ipv6` 选择填充 `SocketAddrv4` 或 `SocketAddrv6`。
3. 通过 `UserBufferWriter` 写入用户态缓冲区，更新 `*addrlen`。

### listen_to_ip_endpoint_preserve

与 `to_endpoint` 不同，此函数保留未指定地址（原样输出 `UNSPECIFIED`）。专门用于 `getsockname` 返回未绑定 socket 的场景，避免返回非预期的 IP 地址。

## SO_REUSEADDR 语义

`SO_REUSEADDR` 的 getter/setter 在 `Socket trait` 中定义默认返回 `EOPNOTSUPP`。TCP 和 UDP 各自覆盖实现：

| Socket 类型 | reuse_addr 存储 | 冲突影响 |
|------------|----------------|----------|
| TcpSocket | `AtomicBool` 字段 | 作为 `BindIntent` 快照的一部分参与双方兼容判断 |
| UdpSocket | `Inner.reuse_addr` 布尔字段 | 作为 `BindIntent` 快照的一部分参与双方兼容判断 |
| RawSocket / Unix / 其他 | 默认 `EOPNOTSUPP` | 不参与冲突检测 |

## 测试映射

| 特性 | 入口点 | LTP 用例 | 状态 |
|------|--------|----------|------|
| 并发端口预留 | `reserve` / `commit` | `KTEST=net_smp` | 双架构 8 核 pass |
| 精确 owner 释放 | `PortReservation::drop` | `KTEST=net_smp` | 双架构 8 核 pass |
| TCP/UDP 与 netns 隔离 | `PortRegistry` | `KTEST=net_smp` | 双架构 8 核 pass |
| 地址回写 | `fill_with_endpoint` | `getsockname01` | pass |
| 地址解析 | `listen_endpoint` | `socket01` | pass |
| BoundInner 状态追踪 | `BoundInner::bind` / `is_bound` | — | 间接验证 |

## Known Issues

1. **临时端口选择可预测**
   - 当前在每个 netns 内顺序扫描，没有 Linux 的 hash/randomized ephemeral 选择。
   - 正确性不受影响，但高并发短连接下可能形成相邻 bucket 热点。

2. **reuse 语义仍是简化模型**
   - 当前以双方 `reuse_addr` 或双方 `reuse_port` 作为兼容条件，尚未完整建模 Linux
     针对 TCP listen、TIME_WAIT、有效 UID 和 multicast 的细分规则。
