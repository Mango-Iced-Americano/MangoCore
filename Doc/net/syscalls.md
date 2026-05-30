# 网络系统调用层

> 目录: `os/src/net/syscall/` (17 files)
> 分发: `os/src/syscall/mod.rs` 中的扁平 match 分支

## 系统调用列表

| # | syscall | 文件 | 说明 |
|---|---------|------|------|
| 198 | `sys_socket` | socket.rs | `Socket::alloc(domain, type, protocol)` → fd |
| 200 | `sys_bind` | bind.rs | 绑定地址 + `is_local_bind_addr` 校验 |
| 201 | `sys_listen` | listen.rs | TCP listen (backlog ≤ 8) |
| 202 | `sys_accept` | accept.rs | TCP accept, 返回 `(fd, sockaddr)` |
| 242 | `sys_accept4` | accept.rs | accept + 非阻塞/cloexec flag |
| 203 | `sys_connect` | connect.rs | TCP/UDP connect |
| 206 | `sys_sendto` | sendto.rs | UDP send (指定目标地址) |
| 207 | `sys_recvfrom` | recvfrom.rs | UDP recv (返回对端地址) |
| 211 | `sys_sendmsg` | sendmsg.rs | scatter-gather send (msghdr) |
| 212 | `sys_recvmsg` | recvmsg.rs | scatter-gather recv (msghdr) |
| 204 | `sys_getsockname` | getsockname.rs | 获取本地 socket 地址 |
| 205 | `sys_getpeername` | getpeername.rs | 获取对端 socket 地址 |
| 208 | `sys_getsockopt` | getsockopt.rs | SOL_SOCKET, IPPROTO_TCP 选项 |
| 209 | `sys_setsockopt` | setsockopt.rs | SOL_SOCKET, IPPROTO_TCP 选项 |
| 210 | `sys_shutdown` | shutdown.rs | SHUT_RD / SHUT_WR / SHUT_RDWR |
| 212 | `sys_socketpair` | socketpair.rs | AF_UNIX socketpair |

## 返回值约定

```rust
// 所有 syscall 处理函数:
//   成功 → 返回 >= 0 的 isize
//   失败 → 返回负 errno (-EAGAIN, -EINVAL, -ENOTCONN, ...)
```

## 通用流程

```
sys_xxx(fd, ...)
  → get_socket!(fd)                  // 从 fd_table 获取 Arc<dyn Socket>
    → socket.try_recv() / try_send() / try_sendmsg()
      → GeneralRet<usize> / GeneralRet<isize>
        → GeneralRet::Ok(n)    → 返回 n as isize
        → GeneralRet::Err(e)   → 返回 -(e as isize)
```

## MsgFlags

```rust
// syscall/common.rs
pub struct MsgFlags(pub i32);

// 支持的 flag:
MSG_DONTWAIT  // 非阻塞 I/O
MSG_PEEK      // 窥探数据 (不移除)
MSG_TRUNC     // 截断 (UDP)
MSG_NOSIGNAL  // 不发送 SIGPIPE (POSIX 兼容, 本内核无 SIGPIPE)
MSG_MORE      // 更多数据待发送 (UDP 缓冲区)
```

## 各调用详解

### bind.rs — 地址绑定

```rust
pub fn sys_bind(sockfd: usize, addr: *const u8, addrlen: usize) -> isize;
```

1. `get_socket!(sockfd)` → `Arc<dyn Socket>`
2. 从用户空间 copy sockaddr → 解析 `IpEndpoint`
3. `is_local_bind_addr(addr)` — 检查地址是否属于本机 (unspecified / 127.x / IFACES)
4. `PortManager::check_bind_conflict(port, addr)` — 端口冲突检测
5. `socket.bind(endpoint)` → TCP/UDP 各自的 bind 实现
6. 注册到 `BoundInner` (ifindex, addr, port)

### connect.rs — 建立连接

```rust
pub fn sys_connect(sockfd: usize, addr: *const u8, addrlen: usize) -> isize;
```

1. 解析目标 endpoint
2. `route_check(remote.addr)` — 路由可达性检查
3. TCP: `Inner::connect(remote)` → Lazy bind (route_output 选 ifindex) → 附着 socket → smoltcp connect
4. UDP: `UdpSocket::connect(remote)` → 设置 `remote_endpoint` 缓存

### sendto.rs — UDP 发送

```rust
pub fn sys_sendto(sockfd: usize, buf: *const u8, len: usize, flags: usize, dest_addr: *const u8, addrlen: usize) -> isize;
```

1. `NET_INTERFACE.try_poll()` — 非阻塞路径前必须 poll
2. `socket.try_sendmsg(&buf, remote, flags)` → UDP 本地投递优先 → smoltcp 发送

### recvfrom.rs — UDP 接收

```rust
pub fn sys_recvfrom(sockfd: usize, buf: *mut u8, len: usize, flags: usize, addr: *mut u8, addrlen: *mut u32) -> isize;
```

1. `NET_INTERFACE.try_poll()` — 确保数据已被 dispatch
2. `socket.try_recv(buf, flags)` → 从 `UdpSocketInner.rx_queue` 消费
3. 如果有 `addr` 参数 → 填充对端地址

### setsockopt.rs — 选项设置

```rust
pub fn sys_setsockopt(sockfd: usize, level: usize, optname: usize, optval: *const u8, optlen: usize) -> isize;
```

支持的 level/optname:

| level | optname | 说明 |
|-------|---------|------|
| SOL_SOCKET | SO_REUSEADDR | 地址重用 |
| SOL_SOCKET | SO_RCVBUF | 接收缓冲区大小 |
| SOL_SOCKET | SO_SNDBUF | 发送缓冲区大小 |
| SOL_SOCKET | SO_BROADCAST | 广播 (UDP) |
| SOL_SOCKET | SO_KEEPALIVE | TCP keepalive |
| SOL_SOCKET | SO_RCVTIMEO | 接收超时 |
| SOL_SOCKET | SO_SNDTIMEO | 发送超时 |
| SOL_SOCKET | SO_BINDTODEVICE | 绑定到特定设备 |
| IPPROTO_TCP | TCP_NODELAY | 禁用 Nagle 算法 |
| IPPROTO_TCP | TCP_KEEPIDLE | keepalive 空闲时间 |
| IPPROTO_TCP | TCP_KEEPINTVL | keepalive 间隔 |
| IPPROTO_TCP | TCP_KEEPCNT | keepalive 重试次数 |
| IPPROTO_TCP | TCP_INFO | (只读, getsockopt) |

未知 level/optname → `ENOPROTOOPT(92)` (Linux 语义, 不是 EOPNOTSUPP)。

## 错误码速查

| 错误码 | 值 | 触发场景 |
|--------|----|---------|
| EAGAIN | -11 | 非阻塞 socket 无可读/可写数据 |
| EINVAL | -22 | 无效参数 (backlog=0, addrlen 不匹配) |
| ENOTCONN | -107 | UDP 未 connect 时 recv (需要指定对端) |
| ECONNREFUSED | -111 | TCP connect 被拒绝 |
| EADDRINUSE | -98 | 端口已被占用 |
| EADDRNOTAVAIL | -99 | 绑定到非本机地址 |
| ENETUNREACH | -101 | 无路由到目标 (无 NIC 时连接外部地址) |
| EISCONN | -106 | 已连接的 socket 再次 connect |
| EPIPE | -32 | 对端已关闭 (发送到已关闭的 TCP 连接) |
| ENOPROTOOPT | -92 | 未知 setsockopt level/optname |
| EOPNOTSUPP | -95 | 不支持的 socket 操作 (RAW bind/listen) |
| EPROTONOSUPPORT | -93 | 不支持的协议族 (非 AF_UNIX socketpair) |
| EAFNOSUPPORT | -97 | 不支持的地址族 |
| EFAULT | -14 | 无效用户空间指针 |
