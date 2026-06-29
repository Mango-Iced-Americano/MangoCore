---
title: Netlink Socket
module: net/socket/netlink
category: net
status: draft
owner: MangoCore Team
last_updated: 2026-06-29
code_paths:
  - os/src/net/socket/netlink/
entry_points:
  - NetlinkSocket
  - handle_netlink_msg
  - NETLINK_ROUTE
arch:
  - rv64
  - la64
tests: "N/A: No direct LTP coverage. Indirectly exercised by BusyBox iproute2 and LTP network tests."
related_docs:
  - architecture.md
  - syscall-layer.md
  - socket-trait-and-fd.md
---

# Netlink Socket

## 概述

Netlink 是 Linux 内核用于内核-用户空间通信的 socket 协议族 (`AF_NETLINK`, 16)。MangoCore 实现了 Netlink 的 **NETLINK_ROUTE** 协议子集，用于用户态工具（如 BusyBox iproute2）查询和修改网络设备的链路状态、IP 地址、路由表和邻居表。

当前实现只支持 `NETLINK_ROUTE` 协议（`protocol = 0`）。其他 Netlink 协议族如 `NETLINK_GENERIC`、`NETLINK_KOBJECT_UEVENT`、`NETLINK_NETFILTER` 均未实现。

Netlink socket 通过 `socket(AF_NETLINK, SOCK_RAW | SOCK_DGRAM, NETLINK_ROUTE)` 创建。两种 type 都路由到同一个 `NetlinkSocket` 实现。

---

## 数据结构

### NetlinkSocket

定义在 `os/src/net/socket/netlink/mod.rs`。

```rust
pub struct NetlinkSocket {
    pub protocol: u32,
    pub recv_queue: spin::Mutex<VecDeque<Vec<u8>>>,
    pub recv_wait: Mutex<WaitQueue>,
    local_portid: Mutex<u32>,
}
```

| 字段 | 类型 | 说明 |
|------|------|------|
| protocol | u32 | 创建时传入的协议号（只支持 0 = NETLINK_ROUTE） |
| recv_queue | `Mutex<VecDeque<Vec<u8>>>` | 接收消息队列，每个条目是一条完整的 nlmsghdr + payload |
| recv_wait | `Mutex<WaitQueue>` | 阻塞 recv 的等待队列，`push_recv` 成功时唤醒 |
| local_portid | `Mutex<u32>` | `bind()` 分配的本地端口 ID，用于回复消息的 `nlmsg_pid` 字段 |

### 队列限制

- **消息数量上限**: `MAX_NETLINK_QUEUE_LEN = 1024`
- **总字节上限**: `MAX_NETLINK_QUEUE_BYTES = 256 KB`

`push_recv()` 在任一上限达到时返回 `false`，调用方返回 `ENOBUFS`。

---

## Socket 操作

### bind

只处理 `Endpoint::Netlink(0)`（即 `sockaddr_nl.nl_pid = 0`）。从全局原子计数器 `NEXT_NETLINK_PORTID`（初始值 1）分配一个递增的 portid。非零 portid 的 bind 视为 no-op。

### 不支持的操作

| 操作 | 行为 |
|------|------|
| listen | 返回 `EOPNOTSUPP` |
| connect | 返回 `EOPNOTSUPP` |
| accept | 返回 `EOPNOTSUPP` |

Netlink 是无连接的。用户态直接通过 `sendmsg` / `recvmsg` 通信。

### try_sendmsg

`try_sendmsg` 从发送缓冲区中按 **NLMSG 流** 解析：

1. 从偏移量 0 开始读取 4 字节 `nlmsg_len`
2. 验证长度 >= 16（nlmsghdr 最小大小）且未超出缓冲区
3. 解析 `nlmsg_type` / `nlmsg_flags` / `nlmsg_seq` / `nlmsg_pid`
4. 调用 `route::handle_netlink_msg()` 处理消息
5. 按 `nlmsg_align(nlmsg_len)` 前移到下一条消息
6. 处理完所有完整消息后返回 `buf.len()`

如果 `consumed == 0`（没有任何有效消息），返回 `EINVAL`。

### try_recv

从 `recv_queue` 头部取出消息拷贝到用户缓冲区。返回原始消息的长度（非拷贝长度）。队列为空时返回 `EAGAIN`。

### try_peek_recvmsg

和 `try_recvmsg` 类似但不弹出队列。

---

## Route 消息分发

`handle_netlink_msg` 是整个 Netlink 路由子系统的入口，位于 `route/mod.rs`。

处理流程：

```
try_sendmsg
  └─ route::handle_netlink_msg(buf, sock)
       ├─ parse_nlmsg: 提取 type/flags/seq/pid
       ├─ 检查 NLM_F_REQUEST ─ 非 REQUEST 消息被忽略
       ├─ 判断是 dump 还是 single 操作
       │
       ├─ Dump 分支 (GET + NLM_F_DUMP/NLM_F_ROOT)
       │   ├─ RTM_GETLINK  → handle_getlink
       │   ├─ RTM_GETADDR  → handle_getaddr
       │   ├─ RTM_GETROUTE → handle_getroute
       │   └─ RTM_GETNEIGH → handle_getneigh
       │   └─ 结果以 NLM_F_MULTI 消息流返回，NLMSG_DONE 结尾
       │
       └─ Single 分支 (NEW/DEL/SET, 或 GET 不带 DUMP)
           ├─ link:   RTM_NEWLINK / RTM_DELLINK / RTM_SETLINK
           ├─ addr:   RTM_NEWADDR / RTM_DELADDR
           ├─ route:  RTM_NEWROUTE / RTM_DELROUTE
           ├─ neigh:  RTM_NEWNEIGH / RTM_DELNEIGH / RTM_GETNEIGH (single)
           ├─ link:   RTM_GETLINK (single, by ifindex/ifname)
           └─ other:  EOPNOTSUPP
```

发生错误时构造 `NLMSG_ERROR` 消息推入 `recv_queue`。

---

## Segment 类型

定义在 `segment.rs`。遵循 Linux uapi `<linux/rtnetlink.h>` 的 wire format。

### 消息头

```rust
pub struct CMsgSegHdr {
    len: u32,     // nlmsg_len (包含头 + 对齐)
    type_: u16,   // nlmsg_type (NLMSG_NOOP/ERROR/DONE + RTM_*)
    flags: u16,   // nlmsg_flags (NLM_F_REQUEST/DUMP/MULTI/ACK/...)
    seq: u32,     // nlmsg_seq
    pid: u32,     // nlmsg_pid (发送者 portid)
}
```

### 消息体类型

| 类型 | C 结构体等价 | 字节数 | 用途 |
|------|-------------|--------|------|
| CIfinfoMsg | `struct ifinfomsg` | 16 | 链路操作（family + pad + type + index + flags + change） |
| CIfaddrMsg | `struct ifaddrmsg` | 8 | 地址操作（family + prefixlen + flags + scope + index） |
| CRtMsg | `struct rtmsg` | 12 | 路由操作（family + dst_len + src_len + tos + table + protocol + scope + type + flags） |
| ErrorSegmentBody | `struct nlmsgerr` | 20 | NLMSG_ERROR（error_code + 原始请求头 16 字节） |
| DoneSegmentBody | NLMSG_DONE payload | 4 | NLMSG_DONE（error_code） |

### 属性类型

当前使用占位符枚举（`LinkAttr` / `AddrAttr` / `RouteAttr` / `NoAttr`），属性解析由各 handler 的 RTA walk 逻辑直接完成，不经过 segment 序列化层。

### 顶层消息枚举

```rust
pub enum RouteNlSegment {
    NewLink(LinkSegment), DelLink(LinkSegment), SetLink(LinkSegment), GetLink(LinkSegment),
    NewAddr(AddrSegment), DelAddr(AddrSegment), GetAddr(AddrSegment),
    NewRoute(RouteSegment), DelRoute(RouteSegment), GetRoute(RouteSegment),
    Error(ErrorSegment), Done(DoneSegment),
}
```

---

## 支持的 RTM 操作

| RTM 操作 | 常量值 | Dump | Single | 状态 |
|----------|--------|------|--------|------|
| RTM_GETLINK | 18 | pass | pass | pass: 遍历 namespace 下所有设备，返回 ifinfomsg + IFLA_IFNAME/MTU/ADDRESS。Single 支持按 ifindex 或 IFLA_IFNAME 查询 |
| RTM_NEWLINK | 16 | - | partial | 支持: veth 对创建、IFLA_NET_NS_PID 跨 netns 迁移。不支持: bridge/macvlan/tun 等其他 link kind |
| RTM_DELLINK | 17 | - | pass | 支持: veth 对删除。Loopback 和其他类型返回 EOPNOTSUPP |
| RTM_SETLINK | 19 | - | pass | 支持: IFF_UP/DOWN (flags/change mask)、iface 重命名、MTU 修改 |
| RTM_GETADDR | 22 | pass | N/A | pass: 遍历所有设备 IP 地址，支持 IPv4 (AF_INET) 和 IPv6 (AF_INET6)，返回 IFA_ADDRESS/LOCAL/LABEL |
| RTM_NEWADDR | 20 | - | pass | pass: 支持 IPv4/IPv6 地址添加、NLM_F_EXCL 冲突检测、NLM_F_REPLACE 替换。同时更新 smoltcp 协议栈和添加直连路由 |
| RTM_DELADDR | 21 | - | pass | pass: 支持 IPv4/IPv6 地址删除，清除 smoltcp 协议栈和直连路由 |
| RTM_GETROUTE | 26 | pass | N/A | pass: 遍历路由表，返回 rtmsg + RTA_DST/GATEWAY/OIF。支持默认路由和静态路由 |
| RTM_NEWROUTE | 24 | - | pass | pass: 支持 NLM_F_EXCL / NLM_F_REPLACE 语义。需要 RTA_OIF，可选 RTA_DST 和 RTA_GATEWAY |
| RTM_DELROUTE | 25 | - | pass | pass: 按目标 CIDR 删除路由 |
| RTM_GETNEIGH | 30 | pass | pass | pass: 遍历邻居表，返回 ndmsg + NDA_DST/LLADDR。Single 按 ifindex + IP 查询。只支持 IPv4 |
| RTM_NEWNEIGH | 28 | - | pass | pass: 记录邻居条目 |
| RTM_DELNEIGH | 29 | - | pass | pass: 按 ifindex + IP 删除邻居 |
| RTM_NEWRULE | 32 | - | - | N/A: 不支持 |
| RTM_DELRULE | 33 | - | - | N/A: 不支持 |
| RTM_GETRULE | 34 | - | - | N/A: 不支持 |

状态说明:
- **pass**: 功能正常，行为与 Linux 兼容
- **partial**: 功能部分实现，受限于模拟环境的虚拟设备模型
- **N/A**: 未实现，返回 EOPNOTSUPP

---

## 关键常量

定义在 `netlink.rs` 中：

- **NLMSG 类型**: NLMSG_NOOP(1), NLMSG_ERROR(2), NLMSG_DONE(3), NLMSG_OVERRUN(4), NLMSG_MIN_TYPE(0x10)
- **NLM_F 标志**: REQUEST(0x01), MULTI(0x02), ACK(0x04), ECHO(0x08), ROOT(0x100), MATCH(0x200), DUMP(0x300), REPLACE(0x100), EXCL(0x200), CREATE(0x400), APPEND(0x800)
- **IFLA 属性**: IFLA_UNSPEC(0) 到 IFLA_GSO_MAX_SIZE(40); 及 IFLA_INFO_KIND/DATA/XSTATS
- **IFA 属性**: IFA_UNSPEC(0) 到 IFA_FLAGS(8)
- **RTA 属性**: RTA_DST(1), RTA_OIF(4), RTA_GATEWAY(5)
- **NDA 属性**: NDA_UNSPEC(0), NDA_DST(1), NDA_LLADDR(2)
- **ARPHRD**: ARPHRD_ETHER(1), ARPHRD_LOOPBACK(772)
- **RTMGRP**: RTMGRP_LINK(1), RTMGRP_IPV4_IFADDR(0x10), RTMGRP_IPV4_ROUTE(0x40)

---

## 构建辅助函数

- `build_nlmsg(type, flags, seq, pid, payload)`: 构造完整 NLMSG（16 字节头 + 对齐后的 payload）
- `build_nlmsg_error(errno, seq, pid, orig)`: 构造 NLMSG_ERROR 消息（4 字节错误码 + 原始请求头 16 字节）
- `rta_data(type, payload)`: 构造 RTA 属性（4 字节 rta_len + rta_type + 对齐后的数据）
- `nlmsg_align(len)`: 向上对齐到 4 字节边界

---

## 测试映射

| 测试来源 | 覆盖内容 | 说明 |
|----------|---------|------|
| BusyBox iproute2 | RTM_GETLINK/GETADDR/GETROUTE dump | `ip link`, `ip addr`, `ip route` 命令 |
| BusyBox iproute2 | RTM_NEWLINK (veth) | `ip link add veth0 type veth peer name veth1` |
| BusyBox iproute2 | RTM_DELLINK | `ip link delete veth0` |
| BusyBox iproute2 | RTM_SETLINK | `ip link set eth0 up/down`, `ip link set eth0 name xxx` |
| BusyBox iproute2 | RTM_NEWADDR/DELADDR | `ip addr add/del 192.168.x.x/24 dev eth0` |
| BusyBox iproute2 | RTM_NEWROUTE/DELROUTE | `ip route add/del default via 192.168.x.1` |
| LTP network tests | Netlink 间接依赖 | 网络测试工具（ping, ifconfig, route）通过 netlink 查询状态 |
| OSComp basic test | 基础网络功能 | 通过 BusyBox iproute2 间接测试 |

没有独立的 LTP netlink 测试用例。所有覆盖来自用户态工具（BusyBox iproute2）的间接调用。

---

## 已知问题和限制

1. **协议限制**: 只支持 `NETLINK_ROUTE`。`NETLINK_KOBJECT_UEVENT`（udev）等未实现，不影响日常运行但 `udevadm` 不可用。

2. **单播组播**: Netlink 多播组（RTMGRP_*）已定义常量但没有实现组播推送。链路/地址/路由变化不会主动通知订阅者。

3. **属性解析**: `LinkAttr` / `AddrAttr` / `RouteAttr` 枚举目前是空的占位符。实际的 RTA 属性解析在各 handler 中直接通过字节偏移完成，未来可通过 segment.rs 的泛化统一。

4. **队列满丢弃**: `recv_queue` 满时消息被静默丢弃（`push_recv` 返回 `false`，调用方记录 `log::warn!`）。在高频率 netlink 请求下可能导致信息丢失。

5. **IPv6 邻居**: RTM_GETNEIGH dump 跳过 IPv6 条目（`continue`），只有 IPv4 邻居可查询。

6. **RTM_GETNEIGH 查找效率**: 单条查询通过 `NEIGHBOUR_TABLE` 的 HashMap 查找，但在 `handle_getneigh_single` 中只返回查询的确切条目，不返回错误的 `ENOENT`。

7. **NETLINK_NOENOBUFS**: socket option NETLINK_NOENOBUFS (`SOL_NETLINK`) 未实现，队列满时总是返回 ENOBUFS。

8. **消息对齐**: Linux 内核要求 nlmsghdr 对齐到 NLMSG_ALIGNTO（4 字节）。`nlmsg_align` 在流解析和消息构建中都正确使用了对齐，但 `try_sendmsg` 中不对齐检查会导致解析提前终止。

---

## 扩展指南

添加新的 RTM 操作：

1. 在 `netlink.rs` 中添加 RTM_XXX 常量
2. 在 `segment.rs` 中添加对应的 body 类型（如果需要）
3. 在 `route/mod.rs` 的 `handle_netlink_msg` 分发中添加匹配分支
4. 实现 handler 函数，通过 `sock.push_recv()` 返回消息
5. 错误时构造 `NLMSG_ERROR` 消息

添加新的 link kind（如 bridge）：

1. 在 `route/link.rs` 的 `handle_newlink` 中添加 `kind` 匹配分支
2. 调用对应的驱动层创建接口
3. 在 dump `handle_getlink` 中添加对应的属性输出
