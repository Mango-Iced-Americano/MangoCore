---
title: "RAW 套接字实现 (Raw Socket)"
module: "net/socket/raw"
category: net
status: draft
owner: MangoCore Team
last_updated: 2026-06-29
code_paths:
  - "os/src/net/socket/inet/raw/"
entry_points:
  - "RawSocket"
  - "RAW_SOCKETS"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "socket01"
    - "raw01"
    - "asapi_01"
  oscomp:
    - "basic"
related_docs:
  - "docs/06_net/architecture.md"
  - "docs/06_net/syscall-layer.md"
  - "docs/06_net/socket-trait-and-fd.md"
---

## 概述

RAW 套接字提供对 IP 层协议的原始访问。MangoCore 通过 `RawSocket` 实现 `SOCK_RAW` 语义，支持 IPv4 和 IPv6，覆盖 `IPPROTO_ICMP`、`IPPROTO_TCP`、`IPPROTO_UDP`、`IPPROTO_RAW` 等所有协议号。内核自动构建 IP 头，除非套接字处于未连接模式 （`IP_HDRINCL` 语义）。

实现文件位于 `os/src/net/socket/inet/raw/raw.rs`，通过 `Socket::alloc()` 工厂在 `AF_INET` / `AF_INET6` + `SOCK_RAW` 时创建。

## RawSocket 结构体

```rust
pub struct RawSocket {
    inner: Mutex<RawSocketInner>,
    socket_handlers: Vec<RouteSocketHandle>,
    recv_waiters: EventWaitQueue,
    send_waiters: EventWaitQueue,
}
```

- **inner**: 核心内部状态，通过 `Mutex` 保护
- **socket_handlers**: 路由套接字句柄列表。索引 0 为主句柄，后续元素覆盖其他协议栈 （lo, veth）。初始化时遍历 `NET_INTERFACE.stack_ifindexes()`，为每个接口创建独立的 smoltcp raw socket
- **recv_waiters / send_waiters**: 用于 epoll 集成和阻塞 I/O 的事件等待队列

### RawSocketInner

```rust
struct RawSocketInner {
    local_endpoint: Option<IpListenEndpoint>,
    remote_endpoint: Option<IpEndpoint>,
    ip_version: IpVersion,
    ip_protocol: IpProtocol,
    recvbuf_size: usize,
    sendbuf_size: usize,
    bound_ifindex: Option<u32>,
    ipv6_checksum_offset: Option<u32>,
    icmp6_filter: [u32; 8],
}
```

- **local_endpoint / remote_endpoint**: 绑定的本地地址和连接的远端地址。`bind()` 设置本地端点，`connect()` 设置远端端点
- **ip_version / ip_protocol**: 创建时确定的 IP 版本和协议号。协议号由 `socket(AF_INET, SOCK_RAW, IPPROTO_xxx)` 的第三个参数传入，通过 `smoltcp::wire::IpProtocol::from(protocol as u8)` 转换
- **recvbuf_size / sendbuf_size**: 接收/发送缓冲区大小，默认 `MAX_BUFFER_SIZE`，可通过 `SO_RCVBUF` / `SO_SNDBUF` 调整
- **bound_ifindex**: `SO_BINDTODEVICE` 绑定的接口索引
- **ipv6_checksum_offset**: `IPV6_CHECKSUM` 设置的校验和偏移。仅 IPv6 生效
- **icmp6_filter**: 256 位位图（8 x u32），位=1 表示**阻止**该 ICMPv6 类型 （与 Linux 语义一致）。仅 IPv6 + ICMPv6 协议时生效

## Connected vs Unconnected 模式

### Connected 模式

通过 `connect()` 设置远端端点后，`try_send()` 调用 `send_to()`：内核构造完整的 IP 头（IPv4 20 字节 / IPv6 40 字节），包含源地址（通过路由查找）、目的地址、协议号、TTL/Hop Limit，并计算校验和。

```rust
// IPv4 头部构造
ip_pkg.set_version(4);
ip_pkg.set_header_len(20);
ip_pkg.set_total_len((20 + user_buf.len()) as u16);
ip_pkg.set_next_header(protocol);
ip_pkg.set_hop_limit(64);
ip_pkg.set_dst_addr(target_ip);
// 源地址通过路由 / SO_BINDTODEVICE 确定
ip_pkg.set_src_addr(src_addr);
ip_pkg.payload_mut().copy_from_slice(user_buf);
ip_pkg.fill_checksum();
```

`send_to()` 在发送后执行双轮 `NET_INTERFACE.poll()`：第一轮将 TX 数据刷入对端 rx_queue，第二轮将对端的回复回收入本端 rx_queue。

接收时，`try_recv()` 返回完整 IP 数据包（含 IP 头）。IPv4 路径保留整个包（包括 IP 头），而 IPv6 路径剥离 40 字节的 IPv6 头，仅返回 payload。

### Unconnected mode （IP_HDRINCL）

当未调用 `connect()` 时，`try_send()` 将用户数据直接通过 smoltcp raw socket 发送，**不添加** IP 头。这与 Linux 的 `IP_HDRINCL` 语义一致：用户自行构建完整 IP 数据包。

`setsockopt(SOL_IP, IP_HDRINCL)` 被接受（返回 OK），但不改变行为 — 判断依据是 remote_endpoint 是否存在。

## 关键操作

### bind / listen / accept

- **bind**: 受支持。将 `RawSocketInner.local_endpoint` 设置为传入的 IP 地址，端口固定为 0
- **listen**: 返回 `EOPNOTSUPP`。RAW 套接字不支持监听
- **accept**: 返回 `EOPNOTSUPP`。RAW 套接字不支持接受连接

### ICMP6_FILTER

`setsockopt(SOL_ICMPV6, ICMP6_FILTER, &filter)` 通过 `set_icmp6_filter()` 写入 256 位过滤器。接收和就绪检查路径均会检查此过滤器：

- `try_recv()`：收到 IPv6 包后检查 `buf[40]`（ICMPv6 type），若对应位为 1 则跳过该包，继续尝试下一个
- `socket_r_ready()`：peek 数据检查 ICMPv6 type，若被过滤则 discard 并继续

### IPV6_CHECKSUM

`setsockopt(SOL_RAW, IPV6_CHECKSUM, &offset)` 或 `setsockopt(SOL_IPV6, IPV6_CHECKSUM, &offset)` 设置 IPv6 校验和偏移。offset 必须为偶数，否则返回 `EINVAL`。

IPv6 发送时调用 `ipv6_pseudo_header_checksum()` 计算 RFC 2460 第 8.1 节定义的伪头校验和，并写入 payload 的指定偏移位置。

```rust
let csum = ipv6_pseudo_header_checksum(
    &src_addr.0,
    &target_ip.0,
    user_buf.len() as u32,
    u8::from(protocol),
    payload,
);
```

`IPV6_CHECKSUM` 的值为 7 表示禁用内核校验和计算（与 Linux 约定一致）。

### SO_BINDTODEVICE

通过 `set_bind_to_device()` 实现。在 `send_to()` 中，优先使用 `bound_ifindex` 确定输出接口；若未绑定，则通过 `route_output()` 路由查找确定。

## 全局跟踪：RAW_SOCKETS

```rust
pub static RAW_SOCKETS: Mutex<Vec<(RouteSocketHandle, Weak<RawSocket>)>>;
pub static RAW_SOCKETS_TO_REMOVE: Mutex<Vec<RouteSocketHandle>>;
```

`RawSocket::register_raw_socket()` 在创建时将 `(handler, Weak<RawSocket>)` 注册到 `RAW_SOCKETS`。`wake_raw_waiters()` 在每次 poll 后遍历所有 RAW_SOCKETS，检查各套接字的 `can_recv()` 状态，若有数据则通过 `EventWaitQueue` 通知等待中的 epoll 或阻塞线程。

`Drop` 实现遍历 `socket_handlers`，从 `RAW_SOCKETS` 中移除所有句柄，并调用 `NET_INTERFACE.remove_routed()` 释放 smoltcp 资源。`RAW_SOCKETS_TO_REMOVE` 用于延迟清理。

## IPv6 Pseudo-Header Checksum

函数 `ipv6_pseudo_header_checksum()` 实现 RFC 2460 第 8.1 节算法：累加源地址、目的地址（均按 16 位大端字）、payload 长度、next_header 和 payload 数据，然后通过反码归约得到最终校验和。由 `IPV6_CHECKSUM` 触发调用。

## 发送流程

```
try_send() / send_to()
  ├─ connected: 构造 IP 头 → 路由确定源 IP / ifindex
  │   ├─ IPv4: smoltcp::wire::Ipv4Packet → fill_checksum
  │   └─ IPv6: smoltcp::wire::Ipv6Packet → 可选伪头校验和
  └─ unconnected: 直接 send_slice（用户包含完整 IP 头）
      └─ raw_routed_socket → NET_INTERFACE.poll() x2
```

## 接收流程

```
try_recv()
  ├─ 遍历 socket_handlers
  │   ├─ can_recv() 检查
  │   ├─ recv_slice() 读取
  │   ├─ ICMP6_FILTER 检查（仅 IPv6 + ICMPv6）
  │   ├─ IPv4: 保留完整包，更新 remote_endpoint
  │   └─ IPv6: 剥离 40 字节头，更新 remote_endpoint
  └─ 返回 EAGAIN 当所有 handler 均无数据
```

## 测试映射

| 测试 | 覆盖范围 | 状态 |
|------|---------|------|
| LTP socket01 | RAW 套接字创建、关闭 | 通过 |
| LTP raw01 | RAW 套接字 sendto/recvfrom | 通过 |
| LTP asapi_01 | IPV6_CHECKSUM / ICMP6_FILTER | 通过 |
| OSComp basic | 基础 RAW 通信 | 通过 |
| ping (busybox) | ICMP raw socket 使用 | 通过 |

## 已知问题

1. **shutdown 未实现**: `RawSocket::shutdown()` 当前为 `todo!()`。调用 `shutdown()` 会 panic。计划实现半关闭语义
2. **IPv6 头剥离不对称**: IPv6 接收时剥离 40 字节头而 IPv4 保留，与应用层期望可能不一致。当前行为与 Linux 的 `IP_HDRINCL=0` 模式对齐
3. **多 handler 遍历**: `try_recv()` 和 `socket_r_ready()` 遍历所有 handler 时按顺序返回第一个可用数据。多接口场景下可能存在饥饿
4. **发送后双轮 poll**: `send_to()` 的 `NET_INTERFACE.poll() x 2` 是经验值。在一些拓扑下可能不足或多余
5. **SO_BINDTODEVICE 无逆向检查**: 接收路径不检查设备绑定，可能收到来自非绑定接口的数据包
