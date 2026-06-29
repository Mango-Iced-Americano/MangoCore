---
title: "INET 公共基础设施 (PortManager / BoundInner / Address)"
module: "net/socket/inet/common"
category: net
status: draft
owner: MangoCore Team
last_updated: "2026-06-29"
code_paths:
  - "os/src/net/socket/inet/common/address.rs"
  - "os/src/net/socket/inet/common/port.rs"
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
| `port.rs` | `PortManager` 全局端口管理器：临时端口分配、端口冲突检测、绑定表维护 |
| `bound.rs` | `BoundInner` 结构体：每个 socket 的绑定状态（handle、ifindex、addr、port） |
| `address.rs` | `SocketAddrv4`/`SocketAddrv6` 地址结构、`IpEndpoint`/`IpListenEndpoint` 转换、用户态地址读写 |

## PortManager

`PortManager` 是一个纯静态方法集合（无实例状态），对标 Linux 内核的临时端口管理。全局状态分为两部分：

- **NEXT\_EPHEMERAL\_PORT** (`AtomicU16`): 从 49152 开始递增的临时端口计数器。使用原子计数器而非 RNG，避免 `fork()` 后父子进程产生相同端口序列。
- **TCP\_PORTS** / **UDP\_PORTS** (`Mutex<BTreeMap>`): 全局端口绑定表，分别记录 TCP 和 UDP 已占用的端口。UDP 表支持每个端口多个绑定（`Vec<UdpPortBinding>`），以处理 `SO_REUSEADDR` 场景。

### 临时端口分配

```
PortManager::alloc_ephemeral_port()
  -> fetch_add NEXT_EPHEMERAL_PORT (起始 49152)
  -> clamp to local_port_range() (实际范围 32768..60999)
  -> loop: check TCP_PORTS + UDP_PORTS
  -> return first free port, or 0 on exhaustion
```

`SocketAddrv4`/`SocketAddrv6` 的 `From<IpListenEndpoint>` 实现中，当端口为 0 时会自动调用 `alloc_ephemeral_port()`。

### 端口冲突检测

`check_bind_conflict(task, endpoint, target_sock)` 分两轮检测：

1. **全局表扫描** (fast path): 查询 `TCP_PORTS` 或 `UDP_PORTS`，根据协议类型检查端口 + 地址是否匹配。
2. **fd\_table 扫描** (fallback): 遍历当前任务的 fd 表，对每个 `SocketFile` 检查 `local_endpoint()` 是否冲突。此路径处理尚未注册到全局表的 socket。

UDP 冲突跳过条件:
- 双方均启用 `SO_REUSEADDR`，跳过冲突。
- 已连接远程端的 UDP socket 不影响同端口新 bind。

### `bind_port(task, socket, endpoint)` 统一入口

`sys_bind` 应调用 `PortManager::bind_port()` 而不是手动 `check + bind`。该方法：

1. 非 IP endpoint（如 Unix）直接调用 `socket.bind()`。
2. 对 IP endpoint 先 `check_bind_conflict`，冲突返回 `EADDRINUSE`。
3. bind 成功后写入 `TCP_PORTS` 或 `UDP_PORTS` 表。

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
- 端口为 0 → 自动分配临时端口

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
| TcpSocket | `AtomicBool` 字段 | TCP 端口冲突检测不检查 `SO_REUSEADDR`（Linux TCP 语义：`TIME_WAIT` 时忽略，此处端口表已注销即不冲突） |
| UdpSocket | `Inner.reuse_addr` 布尔字段 | `check_bind_conflict` 中双方都启用时跳过冲突；`check_udp_conflict` 同样处理 |
| RawSocket / Unix / 其他 | 默认 `EOPNOTSUPP` | 不参与冲突检测 |

## 测试映射

| 特性 | 入口点 | LTP 用例 | 状态 |
|------|--------|----------|------|
| 临时端口分配 | `alloc_ephemeral_port` | — | 间接验证 |
| TCP 端口冲突 | `check_bind_conflict` / `bind_port` | `bind01`, `bind02` | pass |
| UDP 端口冲突 + REUSEADDR | `check_udp_conflict` | `bind03`, `bind04` | pass |
| 地址回写 | `fill_with_endpoint` | `getsockname01` | pass |
| 地址解析 | `listen_endpoint` | `socket01` | pass |
| BoundInner 状态追踪 | `BoundInner::bind` / `is_bound` | — | 间接验证 |

## Known Issues

1. **PortManager 仅支持 IPv4 端口表**
   - `addr_to_ipv4()` 将 `Option<IpAddress>` 截断为 `Option<Ipv4Address>`，IPv6 地址被忽略。
   - 影响: IPv6 socket 的 `check_bind_conflict` 退化为仅按端口匹配。
   - 修复方向: `TCP_PORTS`/`UDP_PORTS` 的 key 扩展为 `(port, addr_family)` 或使用完整 `IpAddress`。

2. **临时端口耗尽不重试**
   - `alloc_ephemeral_port` 扫描一轮后仍无空闲端口则返回 0。
   - 影响: 防火墙或大量短连接场景可能意外端口分配失败。
   - 修复方向: 引入端口回收机制或参考 Linux 的 `inet_csk_get_port` 重试策略。

3. **fd_table 扫描竞争**
   - `check_bind_conflict` 的 fallback 路径持有 `files_ref.lock()`。
   - 影响: 高并发 bind 场景可能因锁竞争导致延迟。
   - 方向: 全局端口表应覆盖全场景，消除 fd_table fallback。
