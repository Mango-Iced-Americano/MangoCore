---
title: "网络子系统 (Network Subsystem)"
module: "net"
category: net
status: draft
owner: MangoCore Team
last_updated: 2026-06-29
code_paths:
  - "os/src/net/"
entry_points:
  - "Socket::alloc()"
  - "NET_INTERFACE"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "socket01"
    - "socketpair01"
    - "bind01"
    - "listen01"
  oscomp:
    - "basic"
    - "busybox"
    - "libctest"
related_docs:
  - "docs/06_net/architecture.md"
  - "docs/06_net/socket-trait-and-fd.md"
  - "docs/06_net/syscall-layer.md"
---

# 网络子系统

## 概述

MangoCore 网络子系统基于 smoltcp 实现了兼容 POSIX 的网络协议栈。它通过统一的 Socket trait，支持 TCP、UDP、RAW、Unix、Netlink 和 Packet（AF_PACKET）等多种套接字类型。协议栈运行在 virtio-net 设备之上，借助 MangoCore 的等待队列基础设施提供阻塞 I/O 语义。

该子系统通过标准系统调用接口（socket、bind、connect、sendto、recvfrom 等）为用户进程提供服务，并与 epoll、signalfd 以及 /proc/net 集成，支持事件驱动 I/O 和网络状态监控。

## 架构

网络协议栈从硬件到用户空间分为六个层次：

```
+-------------------------------------------------------------------+
|                     Userspace (POSIX socket API)                   |
+-------------------------------------------------------------------+
|                syscall layer (17 files, 15 syscalls)               |
|  socket bind connect listen accept sendto recvfrom recvmsg sendmsg |
|  getsockopt setsockopt getsockname getpeername shutdown           |
|  socketpair                                                       |
+-------------------------------------------------------------------+
|                    Socket trait + SocketFile                       |
|  (os/src/net/socket/mod.rs: Socket trait, Endpoint, dispatch)     |
+-------------------------------------------------------------------+
|  TcpSocket | UdpSocket | RawSocket | UnixStream | UnixDatagram   |
|  PacketSocket | NetlinkSocket                                     |
+-------------------------------------------------------------------+
|              Routing + config + neighbour                          |
|  (os/src/net/config.rs, routing.rs, neighbour.rs)                 |
+-------------------------------------------------------------------+
|              Device adapter (IfaceDevice + polling)                |
|  (os/src/net/adapter.rs, router_device.rs, iface.rs)             |
+-------------------------------------------------------------------+
|  virtio_net driver  |  veth driver  | smoltcp (external crate)   |
+-------------------------------------------------------------------+
|                     QEMU / Hardware NIC                            |
+-------------------------------------------------------------------+
```

## 文件地图

### Socket trait 与分发

| 文件 | 说明 |
|------|------|
| `os/src/net/socket/mod.rs` | Socket trait、Endpoint、SocketFile、分发到各实现 |
| `os/src/net/socket/common/mod.rs` | 套接字共享工具和类型 |

### TCP

| 文件 | 说明 |
|------|------|
| `os/src/net/socket/inet/mod.rs` | INET 套接字模块根目录，工厂分发 |
| `os/src/net/socket/inet/stream/mod.rs` | TcpSocket 结构体，公开 API |
| `os/src/net/socket/inet/stream/inner.rs` | 内部状态机，smoltcp TcpSocket 封装 |
| `os/src/net/socket/inet/stream/io.rs` | 读写实现 |
| `os/src/net/socket/inet/stream/lifecycle.rs` | connect、listen、accept 生命周期 |
| `os/src/net/socket/inet/stream/events.rs` | epoll 事件集成 |
| `os/src/net/socket/inet/stream/tcp_info.rs` | getsockopt TCP_INFO 支持 |

### UDP

| 文件 | 说明 |
|------|------|
| `os/src/net/socket/inet/datagram/mod.rs` | UdpSocket 结构体与实现 |
| `os/src/net/socket/inet/datagram/udp.rs` | 本地交付优化，回环快速路径 |

### INET 公共

| 文件 | 说明 |
|------|------|
| `os/src/net/socket/inet/common/address.rs` | 地址解析与转换 |
| `os/src/net/socket/inet/common/bound.rs` | 已绑定端点追踪 |
| `os/src/net/socket/inet/common/port.rs` | 端口分配与管理（临时端口 + 知名端口） |
| `os/src/net/socket/inet/common/mod.rs` | 公共 INET 类型 |

### RAW 套接字

| 文件 | 说明 |
|------|------|
| `os/src/net/socket/inet/raw/mod.rs` | RawSocket 模块根目录 |
| `os/src/net/socket/inet/raw/raw.rs` | RawSocket 实现，IPPROTO_RAW 支持 |

### Unix 套接字

| 文件 | 说明 |
|------|------|
| `os/src/net/socket/unix/mod.rs` | Unix 套接字模块根目录 |
| `os/src/net/socket/unix/stream/mod.rs` | UnixStreamSocket |
| `os/src/net/socket/unix/stream/inner.rs` | Unix 流套接字内部实现 |
| `os/src/net/socket/unix/datagram/mod.rs` | UnixDatagramSocket |
| `os/src/net/socket/unix/ns/mod.rs` | 抽象命名空间和文件系统命名空间 |
| `os/src/net/socket/unix/ring_buffer.rs` | Unix 套接字数据环形缓冲区 |

### Packet 套接字

| 文件 | 说明 |
|------|------|
| `os/src/net/socket/packet.rs` | PacketSocket（AF_PACKET，SOCK_RAW / SOCK_DGRAM） |

### Netlink 套接字

| 文件 | 说明 |
|------|------|
| `os/src/net/socket/netlink/mod.rs` | NetlinkSocket 模块根目录 |
| `os/src/net/socket/netlink/netlink.rs` | NetlinkSocket 核心实现 |
| `os/src/net/socket/netlink/segment.rs` | Netlink 消息分段 |
| `os/src/net/socket/netlink/route/mod.rs` | NETLINK_ROUTE 分发 |
| `os/src/net/socket/netlink/route/link.rs` | RTM_GETLINK / RTM_SETLINK |
| `os/src/net/socket/netlink/route/addr.rs` | RTM_GETADDR / RTM_NEWADDR |
| `os/src/net/socket/netlink/route/route.rs` | RTM_GETROUTE / RTM_NEWROUTE |

### 核心基础设施

| 文件 | 说明 | 相关文档 |
|------|------|---------|
| `os/src/net/mod.rs` | 模块根目录，导出，Socket::alloc() | — |
| `os/src/net/config.rs` | NET_INTERFACE 静态变量，smoltcp Iface + SocketSet 管理 | [device-stack-and-poll.md](device-stack-and-poll.md) |
| `os/src/net/routing.rs` | FIB（转发信息库），路由查找 | [routing.md](routing.md) |
| `os/src/net/neighbour.rs` | 邻居缓存（ARP / NDP） | [neighbour.md](neighbour.md) |
| `os/src/net/adapter.rs` | SmoltcpDeviceAdapter trait 实现，轮询循环 | [device-adapter.md](device-adapter.md) |
| `os/src/net/router_device.rs` | 多接口路由的路由器设备 | [device-stack-and-poll.md](device-stack-and-poll.md) |
| `os/src/net/iface.rs` | 接口管理，地址配置 | [net-core-iface.md](net-core-iface.md) |
| `os/src/net/ioctl.rs` | SIOCGIF* ioctl 处理函数 | [net-core-iface.md](net-core-iface.md) |
| `os/src/net/macros.rs` | impl_file_for_socket! 及其他辅助宏 | — |
| `os/src/net/net_core.rs` | 网络核心初始化，DHCP 探测 | [net-core-iface.md](net-core-iface.md), [dhcp.md](dhcp.md) |
| `os/src/net/posix.rs` | POSIX 类型转换与常量定义 | — |

### 系统调用层

| 文件 | 说明 |
|------|------|
| `os/src/net/syscall/mod.rs` | 系统调用分发根目录 |
| `os/src/net/syscall/socket.rs` | sys_socket |
| `os/src/net/syscall/bind.rs` | sys_bind |
| `os/src/net/syscall/connect.rs` | sys_connect |
| `os/src/net/syscall/listen.rs` | sys_listen |
| `os/src/net/syscall/accept.rs` | sys_accept4 |
| `os/src/net/syscall/sendto.rs` | sys_sendto |
| `os/src/net/syscall/recvfrom.rs` | sys_recvfrom |
| `os/src/net/syscall/sendmsg.rs` | sys_sendmsg |
| `os/src/net/syscall/recvmsg.rs` | sys_recvmsg |
| `os/src/net/syscall/getsockopt.rs` | sys_getsockopt |
| `os/src/net/syscall/setsockopt.rs` | sys_setsockopt |
| `os/src/net/syscall/getsockname.rs` | sys_getsockname |
| `os/src/net/syscall/getpeername.rs` | sys_getpeername |
| `os/src/net/syscall/shutdown.rs` | sys_shutdown |
| `os/src/net/syscall/socketpair.rs` | sys_socketpair |
| `os/src/net/syscall/common.rs` | 共享系统调用辅助函数，sockaddr 解析 |

### 驱动

| 文件 | 说明 |
|------|------|
| `os/src/drivers/net/mod.rs` | 网络设备 trait，驱动注册 |
| `os/src/drivers/net/virtio_net.rs` | VirtIO-net 驱动（MMIO + PCI） |
| `os/src/drivers/net/veth.rs` | 虚拟以太网对设备 |

## 功能矩阵

| 功能 | 状态 | 备注 |
|------|------|------|
| TCP / IPv4 | 已完成 | 基于 smoltcp，SOCK_STREAM |
| UDP / IPv4 | 已完成 | 本地交付优化（回环路径） |
| RAW 套接字 | 已完成 | IPPROTO_RAW，SOCK_RAW |
| Unix 流套接字 | 已完成 | SOCK_STREAM，抽象 + 文件系统命名空间 |
| Unix 数据报套接字 | 已完成 | SOCK_DGRAM |
| Netlink 套接字 | 部分完成 | NETLINK_ROUTE：GETLINK、GETADDR、GETROUTE |
| Packet 套接字（AF_PACKET） | 已完成 | SOCK_RAW + SOCK_DGRAM 模式 |
| DHCP | 已完成 | 启动时同步探测，基于 smoltcp |
| 多接口 | 已完成 | 每设备独立 smoltcp 协议栈，路由器设备 |
| /proc/net | 已完成 | /proc/net/tcp、/proc/net/udp、/proc/net/unix 等 |
| 套接字 epoll 支持 | 已完成 | 通过 Socket::epoll_type() 集成事件 |
| SIOCGIF* ioctl | 已完成 | SIOCGIFHWADDR、SIOCGIFADDR、SIOCGIFNETMASK 等 |
| getsockopt TCP_INFO | 已完成 | 通过 tcp_info.rs 实现 |
| IPv6 | 不支持 | 暂无计划 |

## 文档索引

| 文档 | 说明 |
|------|------|
| [architecture.md](architecture.md) | 系统架构，设计决策，层边界定义 |
| [socket-trait-and-fd.md](socket-trait-and-fd.md) | Socket trait、SocketFile、FD 集成 |
| [syscall-layer.md](syscall-layer.md) | 系统调用分发，sockaddr 解析，错误处理 |
| [smoltcp-device-routing.md](smoltcp-device-routing.md) | Deprecated — 内容已拆分为 6 篇专题文档 |
| [device-stack-and-poll.md](device-stack-and-poll.md) | NetInterface、DeviceStack、polling、socket handle 管理 |
| [device-adapter.md](device-adapter.md) | IfaceDevice、SmoltcpDeviceAdapter、NullNetDevice |
| [net-core-iface.md](net-core-iface.md) | Iface trait、IfaceCommon、设备注册中心、ioctl |
| [routing.md](routing.md) | RouteSocketHandle、SocketBinding、FIB、route_output |
| [neighbour.md](neighbour.md) | NEIGHBOUR_TABLE、ARP 捕获 |
| [dhcp.md](dhcp.md) | DHCP 初始化流程 |
| [tcp.md](tcp.md) | TCP 状态机，connect/accept/数据流，epoll 事件 |
| [udp.md](udp.md) | UDP socket 实现，本地交付优化 |
| [raw.md](raw.md) | RAW socket 实现，IP_HDRINCL 语义 |
| [unix.md](unix.md) | Unix domain socket 实现（流/数据报/socketpair） |
| [netlink.md](netlink.md) | Netlink socket 实现，NETLINK_ROUTE 分发 |
| [packet.md](packet.md) | AF_PACKET socket 实现，帧分发 |
| [inet-common.md](inet-common.md) | INET 公共基础设施（端口管理、地址转换） |
| [udp-raw-unix-netlink-packet.md](udp-raw-unix-netlink-packet.md) | Deprecated — 内容已拆分为 6 篇专题文档 |
| [test-map.md](test-map.md) | 测试覆盖，已知缺口，LTP 测试映射 |
| [debugging.md](debugging.md) | 调试技巧，QEMU 网络，数据包捕获 |
