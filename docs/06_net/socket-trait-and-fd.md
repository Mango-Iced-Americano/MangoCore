---
title: "Socket Trait 与文件描述符层"
module: "net/socket"
category: net
status: draft
owner: MangoCore Team
last_updated: 2026-06-29
code_paths:
  - "os/src/net/socket/mod.rs"
entry_points:
  - "Socket::alloc()"
  - "SocketFile"
  - "Endpoint"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "socket01"
    - "socketpair01"
  oscomp:
    - "basic"
related_docs:
  - "docs/06_net/tcp.md"
  - "docs/06_net/udp.md"
  - "docs/06_net/raw.md"
  - "docs/06_net/unix.md"
  - "docs/06_net/netlink.md"
  - "docs/06_net/packet.md"
---

## 概述

Socket 子系统分为三层：

| 层 | 位置 | 职责 |
|----|------|------|
| `Socket` trait | `os/src/net/socket/mod.rs` | 所有 socket 类型的统一接口 |
| `SocketFile` | `os/src/net/socket/mod.rs` | 将 `Arc<dyn Socket>` 包装为 `IndexNode`，接入 VFS |
| `Socket::alloc()` | `os/src/net/socket/mod.rs` | 根据 domain + PSOCK 分派创建具体 socket |

## 地址族常量

定义在 `os/src/net/socket/mod.rs`：

```rust
pub const AF_UNSPEC: u16 = 0;
pub const AF_UNIX:   u16 = 1;
pub const AF_INET:   u16 = 2;
pub const AF_INET6:  u16 = 10;
pub const AF_NETLINK: u16 = 16;
pub const AF_PACKET: u16 = 17;
```

## 关闭模式常量

定义在 `os/src/net/socket/mod.rs`：

```rust
pub const SHUT_RD:   u32 = 0;
pub const SHUT_WR:   u32 = 1;
pub const SHUT_RDWR: u32 = 2;
```

## PSOCK 枚举

定义在 `os/src/net/socket/mod.rs`。POSIX socket 纯类型枚举，仅包含类型不包含控制标志：

| 变体 | 值 | 对应 POSIX 常量 | 用途 |
|------|----|-----------------|------|
| `Stream` | 1 | `SOCK_STREAM` | TCP、Unix 流式套接字 |
| `Datagram` | 2 | `SOCK_DGRAM` | UDP、Unix 数据报套接字 |
| `Raw` | 3 | `SOCK_RAW` | 原始 IP、Netlink、Packet 套接字 |
| `RDM` | 4 | `SOCK_RDM` | 保留 |
| `SeqPacket` | 5 | `SOCK_SEQPACKET` | 保留 |
| `DCCP` | 6 | `SOCK_DCCP` | 保留 |
| `Packet` | 10 | `SOCK_PACKET` | 保留 |

`TryFrom<PosixArgsSocketType>` 实现：从 syscall 参数 `types().bits()` 的低 4 位匹配映射，失败返回 `EINVAL`。

## Endpoint 枚举

定义在 `os/src/net/socket/mod.rs`。统一的 socket 端点抽象，覆盖所有地址族：

```rust
pub enum Endpoint {
    Ip(IpEndpoint),              // AF_INET / AF_INET6
    Unix(UnixEndpoint),          // AF_UNIX
    Netlink(u32),                // AF_NETLINK (nl_pid)
    Packet(PacketEndpoint),      // AF_PACKET (sockaddr_ll)
    Unspecified,                 // AF_UNSPEC
}
```

### PacketEndpoint 结构体

定义在 `os/src/net/socket/mod.rs`：

| 字段 | 类型 | 说明 |
|------|------|------|
| `ifindex` | `u32` | 网卡索引（0 = 任意） |
| `protocol` | `u16` | 以太网协议类型（网络字节序），如 ETH_P_ALL=0x0003 |
| `hatype` | `u16` | 硬件类型，ARPHRD_ETHER=1 |
| `pkttype` | `u8` | 包类型，PACKET_HOST/PACKET_BROADCAST 等 |
| `halen` | `u8` | 硬件地址长度（以太网为 6） |
| `addr` | `[u8; 8]` | 硬件地址（MAC） |

### Endpoint::from_sockaddr()

从原始 sockaddr 字节解析，根据 `sa_family` 自动分发：

| 地址族 | 最小长度 | 解析方式 |
|--------|----------|----------|
| `AF_INET` | 8 字节 | 端口（大端序） + 4 字节 IPv4 |
| `AF_INET6` | 24 字节 | 端口（大端序） + 16 字节 IPv6 |
| `AF_UNIX` | 2 字节 | 抽象路径（`\0` 前缀）、文件系统路径（`\0` 截断）、未命名 |
| `AF_UNSPEC` | 2 字节 | 直接返回 `Unspecified` |
| `AF_NETLINK` | 12 字节 | `sockaddr_nl`：family(2) + pad(2) + nl_pid(4) + nl_groups(4) |
| `AF_PACKET` | 20 字节 | `sockaddr_ll`：family(2) + protocol(2) + ifindex(4) + hatype(2) + pkttype(1) + halen(1) + addr(8) |
| 其他 | - | 返回 `EAFNOSUPPORT` |

### Endpoint::fill_sockaddr()

将 Endpoint 写入用户空间 sockaddr 缓冲区并更新 addrlen。

| 变体 | 写回长度 | 特殊处理 |
|------|----------|----------|
| `Ip` | 委托 `address::fill_with_endpoint` | - |
| `Unix` | 委托 `unix::fill_with_endpoint` | - |
| `Netlink` | 12 字节 | nl_groups 写 0 |
| `Packet` | 20 字节 | 完整 sockaddr_ll 布局 |
| `Unspecified` | 2 字节 | 仅写 AF_UNSPEC |

## Socket Trait

`pub trait Socket: Send + Sync`，所有 socket 类型的统一接口。

### 生命周期方法

| 方法 | 签名 | 说明 |
|------|------|------|
| `bind` | `fn bind(&self, endpoint: &Endpoint) -> SyscallRet` | 绑定到本地端点 |
| `listen` | `fn listen(&self) -> SyscallRet` | 开始监听 |
| `connect` | `fn connect(&self, endpoint: &Endpoint) -> SyscallRet` | 发起连接 |
| `try_connect` | `fn try_connect(&self) -> Result<isize, SyscallErr>` | 非阻塞尝试一次握手检查；TCP 实现可推进网络状态 |
| `try_connect_without_poll` | `fn try_connect_without_poll(&self) -> Result<isize, SyscallErr>` | 等待队列条件闭包专用：只检查状态，不推进网络状态 |
| `take_error` | `fn take_error(&self) -> Option<SyscallErr>` | 读取并清除 socket 待处理错误（用于 `getsockopt(SO_ERROR)`） |
| `accept` | `fn accept(&self, sockfd: u32, addr: usize, addrlen: usize) -> SyscallRet` | 接受新连接 |
| `shutdown` | `fn shutdown(&self, how: u32) -> GeneralRet<()>` | 关闭读/写/读写通道 |
| `push_pending_connected` | `fn push_pending_connected(&self, _conn: Connected) -> SyscallRet` | Unix 流式套接字专用：将已连接的 socket 推入监听器的传入队列 |

### 类型查询

| 方法 | 签名 | 说明 |
|------|------|------|
| `socket_type` | `fn socket_type(&self) -> PSOCK` | 返回 socket 的 PSOCK 类型 |
| `tcp_state` | `fn tcp_state(&self) -> Option<u8>` | 返回 TCP 状态（Linux `TCP_*` 枚举值），非 TCP 返回 None |
| `is_netlink_socket` | `fn is_netlink_socket(&self) -> bool` | 是否为 Netlink socket |

### 缓冲区大小

| 方法 | 签名 | 说明 |
|------|------|------|
| `recv_buf_size` | `fn recv_buf_size(&self) -> usize` | 获取接收缓冲区大小 |
| `send_buf_size` | `fn send_buf_size(&self) -> usize` | 获取发送缓冲区大小 |
| `set_recv_buf_size` | `fn set_recv_buf_size(&self, size: usize)` | 设置接收缓冲区大小 |
| `set_send_buf_size` | `fn set_send_buf_size(&self, size: usize)` | 设置发送缓冲区大小 |

### 端点查询

| 方法 | 签名 | 说明 |
|------|------|------|
| `local_endpoint` | `fn local_endpoint(&self) -> Option<Endpoint>` | 获取本地端点 |
| `remote_endpoint` | `fn remote_endpoint(&self) -> Option<Endpoint>` | 获取远端端点 |
| `last_recv_addr` | `fn last_recv_addr(&self) -> Option<Endpoint>` | 获取最近一次接收到的源地址（仅 UDP 有意义） |

### 非阻塞 I/O

普通 `try_*` 路径可为 TCP 推进网络状态；持有 `WaitQueue` 锁的条件闭包必须使用对应的 `_without_poll` 变体，poll 必须发生在进入等待前。

| 方法 | 签名 | 说明 |
|------|------|------|
| `try_recv` | `fn try_recv(&self, buf: &mut [u8]) -> Result<isize, SyscallErr>` | **必须实现**。尝试接收数据 |
| `try_send` | `fn try_send(&self, buf: &[u8], _flags: MsgFlags) -> Result<isize, SyscallErr>` | **必须实现**。尝试发送数据 |
| `try_recvmsg` | `fn try_recvmsg(&self, buf: &mut [u8]) -> Result<(isize, Option<Endpoint>), SyscallErr>` | 用于 recvmsg，返回（字节数，可选的源地址）。默认委托给 try_recv |
| `try_peek_recvmsg` | `fn try_peek_recvmsg(&self, buf: &mut [u8]) -> Result<(isize, Option<Endpoint>), SyscallErr>` | MSG_PEEK 查看但不消费。默认委托给 try_recvmsg（会消费），支持 peek 的 socket 应覆写 |
| `try_sendmsg` | `fn try_sendmsg(&self, buf: &[u8], dest: Option<Endpoint>, _flags: MsgFlags) -> Result<isize, SyscallErr>` | 用于 sendmsg。dest=None 时使用已连接远端。默认委托给 try_send |
| `try_recv_without_poll` / `try_send_without_poll` | 与 `try_recv` / `try_send` 相同 | 条件闭包安全的单次状态检查 |
| `try_recvmsg_without_poll` / `try_peek_recvmsg_without_poll` / `try_sendmsg_without_poll` | 与对应 message 方法相同 | 条件闭包安全的 message 操作 |
| `send_to` | `fn send_to(&self, _buf: &[u8], _dest: Endpoint) -> SyscallRet` | 发送到指定目标 |

### 就绪查询

| 方法 | 签名 | 说明 |
|------|------|------|
| `socket_r_ready` | `fn socket_r_ready(&self) -> bool` | poll/select 可读（不阻塞），默认返回 true |
| `socket_w_ready` | `fn socket_w_ready(&self) -> bool` | poll/select 可写（不阻塞），默认返回 true |
| `socket_hang_up` | `fn socket_hang_up(&self) -> bool` | poll/select 挂起，默认返回 false |
| `recv_ready` | `fn recv_ready(&self) -> bool` | 委托给 socket_r_ready |
| `send_ready` | `fn send_ready(&self) -> bool` | 委托给 socket_w_ready |
| `accept_ready` | `fn accept_ready(&self) -> bool` | 委托给 socket_r_ready |
| `connect_ready` | `fn connect_ready(&self) -> bool` | 委托给 socket_w_ready |

### 等待队列

| 方法 | 返回类型 | 说明 |
|------|----------|------|
| `recv_wait_queue` | `Option<&Mutex<WaitQueue>>` | 接收等待队列 |
| `recv_event_queue` | `Option<&EventWaitQueue>` | 接收事件队列（epoll） |
| `send_wait_queue` | `Option<&Mutex<WaitQueue>>` | 发送等待队列 |
| `send_event_queue` | `Option<&EventWaitQueue>` | 发送事件队列（epoll） |
| `connect_wait_queue` | `Option<&Mutex<WaitQueue>>` | 连接等待队列 |
| `connect_event_queue` | `Option<&EventWaitQueue>` | 连接事件队列（epoll） |
| `accept_wait_queue` | `Option<&Mutex<WaitQueue>>` | accept 等待队列 |
| `accept_event_queue` | `Option<&EventWaitQueue>` | accept 事件队列（epoll） |

`EventWaitQueue` 的通知必须使用 `notify_events_all` 或 `notify_events_at_most`。不得以 `try_lock()` 失败为由跳过任务唤醒，否则就绪边沿可能永久丢失。

### Socket 选项

| 方法 | 签名 | 默认行为 |
|------|------|----------|
| `set_nagle_enabled` | `fn set_nagle_enabled(&self, _enabled: bool) -> SyscallRet` | `EOPNOTSUPP` |
| `set_keep_alive` | `fn set_keep_alive(&self, _enabled: bool) -> SyscallRet` | `EOPNOTSUPP` |
| `reuse_addr` | `fn reuse_addr(&self) -> SyscallRet` | `EOPNOTSUPP` |
| `set_reuse_addr` | `fn set_reuse_addr(&self, _enabled: bool) -> SyscallRet` | `EOPNOTSUPP` |
| `set_bind_to_device` | `fn set_bind_to_device(&self, _ifname: &str) -> SyscallRet` | `EOPNOTSUPP` |
| `peer_creds` | `fn peer_creds(&self) -> Result<(u32, u32, u32), SyscallErr>` | `ENOPROTOOPT` |
| `set_ipv6_checksum` | `fn set_ipv6_checksum(&self, _offset: u32) -> SyscallRet` | `Ok(0)` |
| `set_icmp6_filter` | `fn set_icmp6_filter(&self, _filter: [u32; 8]) -> SyscallRet` | `ENOPROTOOPT` |
| `join_multicast_group` | `fn join_multicast_group(&self) -> SyscallRet` | `Ok(0)` |
| `leave_multicast_group` | `fn leave_multicast_group(&self) -> SyscallRet` | `EADDRNOTAVAIL` |

### 特殊方法

| 方法 | 签名 | 说明 |
|------|------|------|
| `push_netlink_message` | `fn push_netlink_message(&self, _data: Vec<u8>) -> Result<(), SyscallErr>` | Netlink 专用：推送消息。默认 `EOPNOTSUPP` |

## SocketFile 结构体

定义在 `os/src/net/socket/mod.rs`。统一的 Socket 文件包装类，所有 socket 类型通过此结构体对外体现为 `IndexNode`。

```rust
pub struct SocketFile {
    pub inner: Arc<dyn Socket>,
}
```

### IndexNode 实现

| 方法 | 实现 | 说明 |
|------|------|------|
| `read_at` | `self.inner.try_recv(buf).map(|n| n as usize)` | 委托给 Socket::try_recv |
| `write_at` | `self.inner.try_send(buf, MsgFlags::empty()).map(|n| n as usize)` | 委托给 Socket::try_send |
| `metadata` | 返回 S_IFSOCK + 0o777 权限模式 | dev_id/inode_id/size 均为 0 |
| `is_stream` | `true` | 所有 socket 视为流式 |
| `poll` | 组合 `socket_r_ready` + `socket_w_ready` + `socket_hang_up` 为 `EPOLLIN/OUT/HUP/ERR` | 返回 `usize` 位掩码 |
| `read_wait_queue` | 优先 `recv_wait_queue`，回退 `accept_wait_queue` | - |
| `read_event_queue` | 优先 `recv_event_queue`，回退 `accept_event_queue` | - |
| `write_wait_queue` | 优先 `send_wait_queue`，回退 `connect_wait_queue` | - |
| `write_event_queue` | 优先 `send_event_queue`，回退 `connect_event_queue` | - |
| `ioctl` | `0x8900..=0x89FF` 范围转发至 `siocgif_dispatch`，其余返回 `ENOTTY` | SIOCGIF* 网卡 ioctl |
| `fs` | 返回 `SOCKET_FS` 单例 | - |

### SocketFS 文件系统

极简 `FileSystem` 实现，仅用于为 socket inode 提供 `fs()` 返回值：

```rust
struct SocketFS;
impl FileSystem for SocketFS {
    fn root_inode(&self) -> Arc<dyn IndexNode> {
        panic!("SocketFS has no root inode")
    }
    fn info(&self) -> FsInfo {
        FsInfo { blk_dev_id: 0, max_name_len: 0, features: vec!["socketfs"] }
    }
    fn name(&self) -> &str { "socketfs" }
    fn super_block(&self) -> SuperBlock { SuperBlock::default() }
    fn as_any_ref(&self) -> &dyn Any { self }
}
```

全局单例：

```rust
lazy_static! {
    static ref SOCKET_FS: Arc<SocketFS> = Arc::new(SocketFS);
}
```

## Socket::alloc() 分发逻辑

根据 domain + PSOCK 分派创建具体 socket 并分配 fd：

```
match domain:
  AF_INET | AF_UNSPEC | AF_INET6:
    ver = Ipv6 if AF_INET6 else Ipv4
    match psock:
      Datagram -> UdpSocket::new(ver) -> register_udp_socket -> SocketFile -> alloc fd
      Stream   -> TcpSocket::new(ver) -> register_tcp_socket -> SocketFile -> alloc fd
      Raw      -> RawSocket::new(protocol, ver) -> register_raw_socket -> SocketFile -> alloc fd
      _        -> EINVAL

  AF_UNIX:
    match psock:
      Stream                    -> UnixStreamSocket::new(is_nonblock) -> SocketFile -> alloc fd
      Datagram | Raw            -> UnixDatagramSocket::new(is_nonblock) -> SocketFile -> alloc fd
      _                         -> EAFNOSUPPORT

  AF_NETLINK:
    match psock:
      Raw | Datagram            -> NetlinkSocket::new(protocol) -> SocketFile -> alloc fd
      _                         -> EINVAL

  AF_PACKET:
    match psock:
      Raw | Datagram            -> PacketSocket::new(protocol as u16) -> register_packet_socket -> SocketFile -> alloc fd
      _                         -> EINVAL

  _                             -> EAFNOSUPPORT
```

辅助函数 `alloc_socket_fd`：

```rust
let alloc_socket_fd = |socket_file: Arc<dyn IndexNode>| -> GeneralRet<usize> {
    let mut flags = FileFlags::O_RDWR;
    if is_nonblock { flags.insert(FileFlags::O_NONBLOCK); }
    let vf = File::new_without_open(socket_file, flags, FileType::Socket);
    let files_ref = current_task().unwrap().process.files();
    files_ref.lock().alloc_fd(vf, is_cloexec)
};
```

## Socket::addr() 与 Socket::peer_addr()

获取 sockname / peername 的辅助方法，严格遵循 Linux 的优先级检查顺序：

```
addr():
  1. prevalidate_sockaddr()   — NULL / 未对齐 → EFAULT
  2. prevalidate_socklen_value() — 负值 → EINVAL，< 2 → EINVAL
  3. local_endpoint()         — None → ENOTCONN
  4. fill_sockaddr()          — 写入用户空间

peer_addr():
  1. prevalidate_sockaddr()   — NULL / 未对齐 → EFAULT
  2. prevalidate_socklen_value() — 负值 → EINVAL，< 2 → EINVAL
  3. probe_user_write()       — 无效写指针 → EFAULT
  4. remote_endpoint()        — None → ENOTCONN
  5. fill_sockaddr()          — 写入用户空间
```

## 全局 Socket 列表

| 变量 | 类型 | 文件位置 | 说明 |
|------|------|----------|------|
| `UDP_SOCKETS` | `Mutex<Vec<Weak<UdpSocket>>>` | `os/src/net/socket/mod.rs` | 所有活跃 UDP socket |
| `UDP_SOCKETS_TO_REMOVE` | `Mutex<Vec<RouteSocketHandle>>` | `os/src/net/socket/mod.rs` | 待移除的 UDP route handle |
| `TCP_SOCKETS` | `Mutex<Vec<Weak<TcpSocket>>>` | `os/src/net/socket/mod.rs` | 所有活跃 TCP socket |
| `TCP_SOCKETS_TO_REMOVE` | `Mutex<Vec<RouteSocketHandle>>` | `os/src/net/socket/mod.rs` | 待移除的 TCP route handle |
| `RAW_SOCKETS` | `Mutex<Vec<(RouteSocketHandle, Weak<RawSocket>)>>` | `os/src/net/socket/mod.rs` | 所有活跃 Raw socket（带 route handle） |
| `RAW_SOCKETS_TO_REMOVE` | `Mutex<Vec<RouteSocketHandle>>` | `os/src/net/socket/mod.rs` | 待移除的 Raw route handle |
| `PACKET_SOCKETS` | `Mutex<Vec<Weak<PacketSocket>>>` | `os/src/net/socket/mod.rs` | 所有活跃 Packet socket |

全局遍历使用 `Weak` 引用，每次遍历时尝试 `upgrade()`，对已释放的条目记录索引延迟移除。

### wake_tcp_waiters()

定义在 `os/src/net/socket/mod.rs`。在每个网卡 poll 后调用，遍历 `TCP_SOCKETS` 唤醒等待队列。

```rust
pub fn wake_tcp_waiters() {
    // 1. 锁定全局列表，upgrade 所有弱引用
    // 2. 对每个活跃 socket 调用 socket.wake_if_ready()
    // 3. 标记失效条目索引
    // 4. 从尾部逆序移除失效条目
}
```

### wake_raw_waiters()

定义在 `os/src/net/socket/mod.rs`。在每个网卡 poll 后调用，遍历 `RAW_SOCKETS` 检查原始 socket 就绪状态。

```rust
pub fn wake_raw_waiters() {
    // 1. 锁定全局列表，upgrade 所有弱引用
    // 2. 对每个活跃 socket，查询 NET_INTERFACE.raw_routed_socket 的 can_recv()
    // 3. 可接收时通过 recv_event_queue 通知 epoll
    // 4. 标记失效条目索引
    // 5. 从尾部逆序移除失效条目
}
```

## MAX_BUFFER_SIZE

定义在 `os/src/net/socket/mod.rs`：

```rust
pub const MAX_BUFFER_SIZE: usize = 64 * 1024;
```

## 引用源文件

| 文件 | 内容 |
|------|------|
| `os/src/net/socket/mod.rs` | Socket trait、SocketFile、Endpoint、PSOCK、Socket::alloc()、全局列表 |
| `os/src/net/socket/inet/` | TcpSocket、UdpSocket、RawSocket（详见 [tcp.md](tcp.md)、[udp.md](udp.md)、[raw.md](raw.md)、[inet-common.md](inet-common.md)） |
| `os/src/net/socket/unix/` | UnixStreamSocket、UnixDatagramSocket、UnixEndpoint（详见 [unix.md](unix.md)） |
| `os/src/net/socket/netlink/` | NetlinkSocket（详见 [netlink.md](netlink.md)） |
| `os/src/net/socket/packet.rs` | PacketSocket（详见 [packet.md](packet.md)） |
| `os/src/net/macros.rs` | 废弃的 impl_file_for_socket! 宏说明 |
| `os/src/net/ioctl.rs` | SIOCGIF* ioctl 分发 |
| `os/src/net/routing/` | RouteSocketHandle |
