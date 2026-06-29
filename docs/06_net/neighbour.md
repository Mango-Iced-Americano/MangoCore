---
title: "邻居表 (Neighbour Table / ARP Table)"
module: os/src/net/neighbour.rs
category: net
status: draft
owner: MangoCore Team
last_updated: "2026-06-29"
code_paths:
  - "os/src/net/neighbour.rs"
entry_points:
  - "NEIGHBOUR_TABLE"
  - "try_capture_arp_reply"
  - "neighbour_record"
  - "neighbour_dump"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "ipneigh01"
  oscomp:
    - "N/A: 间接由 ip neigh show 和 arp -an 覆盖"
related_docs:
  - "docs/06_net/architecture.md"
  - "docs/06_net/device-stack-and-poll.md"
  - "docs/06_net/device-adapter.md"
---

# 邻居表 (Neighbour Table)

## 概述

邻居表是内核维护的 IP 地址到 MAC 地址的映射表，对应 Linux 的 ARP 表（IPv4）和 NDP 表（IPv6）。它将网络层的 IP 地址解析为链路层的硬件地址，封装在 `os/src/net/neighbour.rs` 模块中。

表的内容在设备接收路径中被动填充：每次内核从网络接口收到数据包时，`try_capture_arp_reply()` 会尝试从原始以太网帧中提取 ARP Reply，并将发送方 IP 和 MAC 的对应关系记录到表中。用户态可以通过 netlink `RTM_GETNEIGH` 和 `/proc/net/arp` 查询表中的条目。

## 核心数据结构

### `NEIGHBOUR_TABLE`

全局邻居表，类型为 `Mutex<BTreeMap<(u32, IpAddress), NeighbourEntry>>`。键是 `(ifindex, IpAddress)` 二元组，其中 `ifindex` 是网络接口的索引，`IpAddress` 是目标的 IP 地址。值是对应的邻居条目。

```rust
pub static NEIGHBOUR_TABLE: Mutex<BTreeMap<(u32, IpAddress), NeighbourEntry>>;
```

### `NeighbourEntry`

单个邻居条目，包含解析出的 MAC 地址和 NUD 状态。

```rust
pub struct NeighbourEntry {
    pub mac: EthernetAddress,
    pub state: u16,
}
```

- `mac` — 目标的以太网硬件地址（6 字节）。
- `state` — NUD 状态码，取值为 Linux NUD 状态常量的子集。

### NUD 状态常量

模块定义了 Linux `linux/neighbour.h` 中 NUD 状态的子集：

| 常量 | 值 | 含义 |
|------|----|------|
| `NUD_REACHABLE` | `0x02` | 条目已验证可达，可直接使用 |
| `NUD_STALE`     | `0x04` | 条目标记为过期，下次使用前需确认 |
| `NUD_PERMANENT` | `0x80` | 静态条目，永不超时 |

新条目默认以 `NUD_REACHABLE` 状态插入（由 `neighbour_record()` 设置）。`NUD_PERMANENT` 可由管理操作（如 `ip neigh add`）设置。目前表中没有实现主动的 NUD 超时回收机制，`NUD_STALE` 作为模块常量定义供扩展使用。

### `CURRENT_POLL_IFINDEX`

跟踪当前正在被 smoltcp 轮询的网络接口索引。

```rust
pub static CURRENT_POLL_IFINDEX: Mutex<u32>;
```

每次 `poll_once()` 开始轮询一个设备栈之前，内核会将这个设备的 ifindex 写入 `CURRENT_POLL_IFINDEX`。接收路径上的 consume 方法从中读出 ifindex，作为调用 `try_capture_arp_reply()` 时的参数。

## 公开 API

### `try_capture_arp_reply(frame_buf: &[u8], ifindex: u32)`

尝试从原始以太网帧中捕获 ARP Reply 并记录到邻居表。

```
解析流程:
  EthernetFrame::new_checked(frame_buf)
    → 检查 ethertype == Arp
    → ArpPacket::new_checked(payload)
    → 检查 operation == ArpOperation::Reply
    → 提取 source_hardware_addr (MAC) + source_protocol_addr (IP)
    → 调用 neighbour_record(ifindex, ip, mac)
```

如果帧不是 ARP 包、不是 ARP Reply、或者地址长度不足，函数静默返回，不做任何处理。

### `neighbour_record(ifindex: u32, ip: IpAddress, mac: EthernetAddress)`

将 `(ifindex, ip) → mac` 的映射插入邻居表，状态设为 `NUD_REACHABLE`。只记录单播 IP 地址（非单播地址会被跳过）。

### `neighbour_delete(ifindex: u32, ip: IpAddress) -> bool`

从表中删除指定条目。返回 `true` 表明确实有条目被删除。

### `neighbour_dump() -> Vec<(u32, IpAddress, EthernetAddress, u16)>`

遍历整个邻居表，返回 `(ifindex, IpAddress, EthernetAddress, state)` 四元组向量。供 netlink `RTM_GETNEIGH` dump 和 `/proc/net/arp` 查询使用。

### NDA 属性常量

用于向 netlink 消息中填充邻居属性：

| 常量 | 值 | 含义 |
|------|----|------|
| `NDA_UNSPEC` | `0` | 未指定 |
| `NDA_DST`    | `1` | 目标 IP 地址 |
| `NDA_LLADDR` | `2` | 链路层地址（MAC） |

## 接收路径集成

邻居表的数据完全来自被动监听，内核不主动发送 ARP 请求。`try_capture_arp_reply()` 在两个 consume 路径中被调用。

### `NetRxToken::consume()` 路径

在 `os/src/net/adapter.rs`，物理网卡（virtio-net）的接收 token 在将数据帧交给 smoltcp 协议栈之前，先调用 `try_capture_arp_reply`：

```rust
// adapter.rs — NetRxToken::consume()
let ifindex = *crate::net::neighbour::CURRENT_POLL_IFINDEX.lock();
crate::net::neighbour::try_capture_arp_reply(&self.buf, ifindex);
f(&mut self.buf)
```

### `VethRxToken::consume()` 路径

在 `os/src/drivers/net/veth.rs`，veth 虚拟以太网设备的接收 token 采用相同的模式：

```rust
// veth.rs — VethRxToken::consume()
let ifindex = *crate::net::neighbour::CURRENT_POLL_IFINDEX.lock();
crate::net::neighbour::try_capture_arp_reply(&self.0, ifindex);
f(&mut self.0)
```

两个路径的实现完全对称：先锁定 `CURRENT_POLL_IFINDEX` 读出当前轮询的接口索引，再调用 `try_capture_arp_reply` 尝试捕获 ARP 应答。如果帧是有效的 ARP Reply，则对应的 IP-MAC 映射被插入 `NEIGHBOUR_TABLE`。

## 查询接口

### Netlink `RTM_GETNEIGH`

Netlink 路由协议处理 `RTM_GETNEIGH` 消息（消息类型 `30`）。处理流程在 `os/src/net/socket/netlink/route/mod.rs` 中：

- **Dump 模式**（NLM_F_DUMP）：调用 `handle_getneigh()`，内部调用 `neighbour_dump()` 遍历所有条目，每个条目封装为一个 `RTM_NEWNEIGH` 消息。
- **单条目查询**：调用 `handle_getneigh_single()`，根据请求中的 NDA_DST 属性查找对应条目。
- **删除操作**（`RTM_DELNEIGH`）：调用 `handle_delneigh()`，内部调用 `neighbour_delete()`。
- **添加操作**（`RTM_NEWNEIGH`）：调用 `handle_newneigh()`，调用 `neighbour_record()` 添加或更新条目。

### `/proc/net/arp`

由 `os/src/fs/procfs/files/net_arp.rs` 实现。读取 `NEIGHBOUR_TABLE` 的内容，输出标准格式的 `/proc/net/arp` 文件内容，包含 IP 地址、HW type、Flags、MAC 地址、Mask、Device 等列。

## 设计要点

- **被动填充**：内核不在驱动层主动发送 ARP 请求。ARP 请求由 smoltcp 协议栈在需要时自动发出，内核只负责监听回复并缓存结果。
- **全接口共享**：同一张 `BTreeMap` 存储所有网络接口的邻居信息，通过 `(ifindex, IpAddress)` 键区分。
- **IPv6 支持**：`IpAddress` 是 smoltcp 的枚举类型，同时支持 IPv4 和 IPv6。`try_capture_arp_reply` 目前只解析 IPv4 ARP 包，IPv6 NDP 的捕获待扩展。
- **双架构无关**：模块全是纯 Rust 代码 + smoltcp 类型，不涉及任何架构相关的汇编或 HAL 调用，rv64 和 la64 上行为完全一致。

## 测试映射

| 测试类型 | 用例 | 覆盖内容 | 状态 |
|----------|------|----------|------|
| LTP | `ipneigh01` | RTM_GETNEIGH dump、单条目查询 | 通过 |
| LTP | `ipneigh02` | RTM_DELNEIGH、条目增删 | 通过 |
| 手动 | `ip neigh show` | 通过 iproute2 验证邻居表内容 | 通过 |
| 手动 | `arp -an` | 通过 busybox arp 验证 | 通过 |
| 手动 | `cat /proc/net/arp` | 验证 procfs 输出格式 | 通过 |

## 已知问题

- **主动 ARP 请求不经过内核**：ARP 请求由 smoltcp 内部发出并处理响应，内核的 `try_capture_arp_reply` 只捕获 smoltcp 交付给上层之前的数据包。如果 smoltcp 在内部处理了 ARP 响应（而非重入到内核路径），该响应不会被捕获。当前行为已验证对常见场景（QEMU virtio-net、veth 对）工作正常。
- **IPv6 NDP 未捕获**：`try_capture_arp_reply` 只解析以太网帧中 `ethertype == Arp (0x0806)` 的包。IPv6 邻居发现使用 ICMPv6 而非 ARP，不在捕获范围内。
- **无主动超时回收**：表中条目不会因 NUD_STALE 状态而自动老化删除。`NUD_STALE` 常量已定义供将来实现使用，但目前依赖上层（netlink 接口的 NUD 管理）做状态迁移。
