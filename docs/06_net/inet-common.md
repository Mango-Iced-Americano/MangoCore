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
| `port.rs` / `port/registry.rs` | `PortManager` 事务入口和每 netns 的 `PortRegistry`：预留、冲突检测、提交/回滚、精确释放 |
| `bound.rs` | `BoundInner` 结构体：每个 socket 的绑定状态（handle、ifindex、addr、port） |
| `address.rs` | `SocketAddrv4`/`SocketAddrv6` 地址结构、`IpEndpoint`/`IpListenEndpoint` 转换、用户态地址读写 |

## PortManager

`PortManager` 是系统调用兼容入口；权威状态位于每个 `NetNamespace::ports: Mutex<PortRegistry>`。registry 用 `(protocol, family, address, port, ifindex)` 建 bucket，owner 以 token 和 `Weak<dyn Socket>` 标识；TCP/UDP 彼此独立，netns 之间也不共享端口占用。

### 临时端口分配

显式 `bind(port=0)` 和所有 INET auto-bind 共享同一个 `NetNamespace.ports` 线性化点：具体 socket 先在未持 N0 时推导内核所有的候选端点，`PortManager::ensure_auto_bound()` 再复用 `bind_port()`。锁内 prune dead owner、选择 ephemeral、按完整冲突矩阵插入 `Reserved`；解锁后调用 socket bind；最后重锁将同一 key+token+Weak owner 改为 `Bound`，失败则删除 reservation。registry 锁绝不跨 socket/DeviceStack 操作。

`AutoBindPurpose::Connect`/`Send` 由 TCP/UDP 按 route 选择源 IP；`Listen` 使用对应地址族的未指定地址。TCP connect/listen、UDP connect 和首个未连接 `sendto`/`sendmsg` 都在 syscall 层持有 `Arc<dyn Socket>` 时调用该入口。并发 auto-bind 可能各自预留不同端口，但只有首个 `socket.bind()` 成功；另一个调用 abort 自己的 reservation，既不会覆盖已绑定 socket，也不会泄漏 owner。

### 端口冲突检测

冲突检查同时考虑 `Reserved` 与 `Bound`：同 family wildcard 与具体地址冲突；IPv6 wildcard 且未启用 `IPV6_V6ONLY` 与 IPv4 wildcard/具体地址冲突；`SO_REUSEADDR`/`SO_REUSEPORT` 必须双方快照兼容。UDP close 只按 key+token+Weak identity 删除自己的 owner，不能删除 reuse peer。

### `bind_port(task, socket, endpoint)` 统一入口

`sys_bind` 应调用 `PortManager::bind_port()` 而不是手动 `check + bind`。该方法：

1. 非 TCP/UDP IP endpoint（如 Raw/Packet）直接调用 `socket.bind()`。
2. TCP/UDP 先在 socket 生命周期锁内快照 `BindIntent`，随后释放 socket 锁。
3. 在调用 PCB 的 netns registry 中执行 `reserve → socket.bind → commit/abort`；commit 后才将 reservation 安装到 socket，Drop 精确释放。

`Socket::auto_bind_endpoint(peer, purpose)` 只在未持 registry 锁时返回可选端点；非 INET 或已绑定 socket 返回 `None`。它不执行端口分配或 smoltcp 操作。

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
- 端口为 0 → 保留为未绑定端点；只有持有 socket `Arc` 的 syscall auto-bind 入口可以实际分配端口

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

1. **临时端口耗尽不重试**
   - `alloc_ephemeral_port` 扫描一轮后仍无空闲端口则返回 0。
   - 影响: 防火墙或大量短连接场景可能意外端口分配失败。
   - 修复方向: 引入端口回收机制或参考 Linux 的 `inet_csk_get_port` 重试策略。

2. **fd_table 深路径未完成审计**
   - `check_bind_conflict` 的 fallback 路径持有 `files_ref.lock()`。
   - 影响: 高并发 bind 场景可能因锁竞争导致延迟。
   - 方向: 全局端口表应覆盖全场景，消除 fd_table fallback。
