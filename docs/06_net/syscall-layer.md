---
title: "网络系统调用层"
category: net
status: draft
owner: MangoCore Team
last_updated: 2026-06-29
tags: [net, syscall, socket, posix]
---

# 网络系统调用层

## 概述

网络系统调用层位于 `os/src/net/syscall/` 目录，负责处理所有 POSIX 网络相关的系统调用。该层是用户态与内核网络栈之间的桥梁，处理参数校验、地址解析、用户态缓冲区访问、阻塞与非阻塞 I/O 控制，并将操作委托给底层的 Socket trait 实现。

整个网络 syscall 层遵循以下设计原则：

- **扁平分发**：每个 syscall 有独立函数，通过 `syscall/mod.rs` 中的 `match` 分支注册。
- **返回值约定**：成功返回 `isize >= 0`，失败返回负 errno（如 `-11` = EAGAIN）。
- **非阻塞优先**：数据收发路径优先调用 `try_xxx` 方法，仅在阻塞模式下等待。
- **内核中转**：用户态缓冲区通过 `copy_from_user_array` 拷贝到内核中转，避免跨页问题。

---

## 调度表

| Syscall ID | 函数 | 源文件 | 描述 |
|-----------|------|--------|------|
| 198 | `sys_socket` | `socket.rs` | 创建 socket 文件描述符 |
| 199 | `sys_socketpair` | `socketpair.rs` | 创建一对已连接的 UNIX socket |
| 200 | `sys_bind` | `bind.rs` | 绑定地址到 socket |
| 201 | `sys_listen` | `listen.rs` | 将 socket 设为监听状态 |
| 202 | `sys_accept` | `accept.rs` | 接受连接（无额外标志） |
| 242 | `sys_accept4` | `accept.rs` | 接受连接（支持 SOCK_CLOEXEC 和 SOCK_NONBLOCK） |
| 203 | `sys_connect` | `connect.rs` | 发起连接（支持阻塞和非阻塞模式） |
| 204 | `sys_getsockname` | `getsockname.rs` | 获取本地绑定的地址 |
| 205 | `sys_getpeername` | `getpeername.rs` | 获取对端地址 |
| 206 | `sys_sendto` | `sendto.rs` | 发送数据（可指定目标地址） |
| 207 | `sys_recvfrom` | `recvfrom.rs` | 接收数据（可获取源地址） |
| 211 | `sys_sendmsg` | `sendmsg.rs` | 通过 scatter/gather I/O 发送数据 |
| 212 | `sys_recvmsg` | `recvmsg.rs` | 通过 scatter/gather I/O 接收数据并获取辅助信息 |
| 208 | `sys_setsockopt` | `setsockopt.rs` | 设置 socket 选项 |
| 209 | `sys_getsockopt` | `getsockopt.rs` | 获取 socket 选项 |
| 501 | `sys_sock_shutdown` | `shutdown.rs` | 关闭 socket 的收发通道 |

---

## Socket 生命周期

### socket 调用（syscall 198）

`sys_socket(domain, socket_type, protocol)` 分配一个新的 socket 并返回文件描述符。

```rust
// os/src/net/syscall/socket.rs
pub fn sys_socket(domain: u32, socket_type: u32, protocol: u32) -> isize
```

处理流程：

1. 将 `socket_type` 通过 `PosixArgsSocketType::from_bits_truncate` 解析为 `PSOCK` 枚举和标志位（`SOCK_NONBLOCK`、`SOCK_CLOEXEC`）。
2. 调用 `Socket::alloc(domain, psock, protocol, is_nonblock, is_cloexec)`，该函数根据 `domain` 和 `psock` 选择具体 Socket 实现（TCP、UDP、RAW、UNIX、Netlink、Packet）。
3. 成功返回 sockfd（非负值），失败返回负 errno。
4. 错误码包括：`EAFNOSUPPORT`（未知 domain）、`EINVAL`（未知 type）、`ENOBUFS`（资源不足）。

### bind 调用（syscall 200）

`sys_bind(sockfd, addr, addrlen)` 将地址绑定到 socket。

```rust
// os/src/net/syscall/bind.rs
pub fn sys_bind(sockfd: u32, addr: usize, addrlen: u32) -> isize
```

处理流程：

1. 通过 `check_addrlen(addrlen)` 校验地址长度（不超过 `MAX_ADDR_LEN=512`）。
2. 使用 `trans_ref!(addr, addrlen)` 读取用户空间地址数据。
3. 调用 `Endpoint::from_sockaddr(addr_buf)` 解析地址类型：
   - **IP 地址**：检查 `is_local_bind_addr`（回环或本机地址）；特权端口（小于 1024）检查 `CAP_NET_BIND_SERVICE`。
   - **Unix 地址**（Path/Abstract/Unnamed）：处理路径解析（相对路径通过 CWD 转绝对路径）、抽象命名空间注册（`ABSTRACT_TABLE`）、文件系统 socket 文件创建（`PATH_TABLE`）。
   - **Netlink/Packet/Unspecified**：直接委托给 `socket.bind(&endpoint)`。
4. 关键错误码包括：`EADDRNOTAVAIL`（非本机地址）、`EADDRINUSE`（地址已使用）、`EACCES`（特权端口无权限）、`EAFNOSUPPORT`（domain 不兼容）、`ENOTDIR`（Unix 路径父级不是目录）。

### listen 调用（syscall 201）

`sys_listen(sockfd, backlog)` 将 socket 标记为被动监听状态。

```rust
// os/src/net/syscall/listen.rs
pub fn sys_listen(sockfd: u32, _backlog: u32) -> isize
```

直接委托给 `socket.listen()`。`backlog` 参数当前被忽略（smoltcp 内部有独立队列限制）。

### accept 与 accept4 调用（syscall 202、242）

```rust
// os/src/net/syscall/accept.rs
pub fn sys_accept(sockfd: u32, addr: usize, addrlen: usize) -> isize
pub fn sys_accept4(sockfd: u32, addr: usize, addrlen: usize, flags: u32) -> isize
```

处理流程：

1. 通过 `get_socket!(sockfd)` 解析文件描述符，获取 `Arc<dyn Socket>` 引用。
2. 从文件描述符表中查询 `is_nonblock` 标志。
3. **阻塞模型**（首选 `WaitQueue`）：
   - 如果 socket 实现了 `accept_wait_queue()`，使用 `WaitQueue::wait_until_interruptible` 等待连接到来。
   - 否则回退到 `wait_io()`（带 `NET_INTERFACE.poll()` 的自旋循环）。
4. **非阻塞模型**：直接调用 `socket.accept()`，无可用连接时返回 `-EAGAIN`。
5. `sys_accept4` 的 `flags` 支持 `SOCK_CLOEXEC`（bit 19）和 `SOCK_NONBLOCK`（0x800），在接受后将对应标志设置到新文件描述符上。

### connect 调用（syscall 203）

`sys_connect(sockfd, addr, addrlen)` 发起连接。

```rust
// os/src/net/syscall/connect.rs
pub fn sys_connect(sockfd: u32, addr: usize, addrlen: u32) -> isize
```

处理流程：

1. 校验 `addrlen`（不超过 `MAX_ADDR_LEN`），通过 `trans_ref!` 读取用户地址。
2. 使用 `Endpoint::from_sockaddr` 解析地址。
3. **Unix 路径处理**：相对路径通过 CWD inode 转为绝对路径。
4. **首次连接尝试**：调用 `socket.connect(&endpoint)`。如果返回 `EAGAIN`（TCP 握手未完成），进入等待。
5. **阻塞模式**：通过 `WaitQueue::wait_until_interruptible` 等待连接完成，每次唤醒调用 `socket.try_connect()`。
6. **非阻塞模式**：返回 `-EINPROGRESS`，应用通过 `poll(EPOLLOUT)` 等待连接完成（遵循 Linux 语义）。
7. 错误码包括：`ECONNREFUSED`、`ETIMEDOUT`、`ENETUNREACH`、`EINPROGRESS`（非阻塞 connect）等。

### shutdown 调用（syscall 501）

`sys_sock_shutdown(sockfd, how)` 关闭 socket 的读写通道。

```rust
// os/src/net/syscall/shutdown.rs
pub fn sys_sock_shutdown(sockfd: u32, how: u32) -> isize
```

`how` 取值：`0` = SHUT_RD，`1` = SHUT_WR，`2` = SHUT_RDWR。委托给 `socket.shutdown(how)`。

### socketpair 调用（syscall 199）

`sys_socketpair(domain, type, protocol, sv)` 创建一对已连接的 UNIX socket。

```rust
// os/src/net/syscall/socketpair.rs
pub fn sys_socketpair(domain: u32, socket_type: u32, protocol: u32, sv: usize) -> isize
```

处理流程：

1. 校验 `domain` 必须为 `AF_UNIX` 或 `AF_UNSPEC`（否则返回 `EPROTONOSUPPORT`）。
2. 仅支持 `SOCK_STREAM` 和 `SOCK_DGRAM`（否则返回 `ESOCKTNOSUPPORT`）。
3. 调用 `make_unix_socket_pair()` 生成一对已连接的 Socket 对象。
4. 创建两个 `SocketFile` 和 `vfs::File` 包装，分配两个文件描述符。
5. 将两个文件描述符写入用户空间的 `sv[0]` 和 `sv[1]`（通过 `UserSlice::write_array_from`）。
6. 错误码：`EFAULT`（sv 地址无效）。

---

## 数据传输

### sendto 调用（syscall 206）

`sys_sendto(sockfd, buf, len, flags, dest_addr, addrlen)` 发送数据到指定地址。

```rust
// os/src/net/syscall/sendto.rs
pub fn sys_sendto(
    sockfd: u32,
    buf: usize,
    len: usize,
    flags: u32,
    dest_addr: usize,
    addrlen: u32,
) -> isize
```

完整的执行流程图：

```
sys_sendto 入口
  ├─ 截断 len 到 64MB 上限
  ├─ MsgFlags::validate_for_send() 校验 flags
  ├─ copy_from_user_array() → kernel_buf (内核中转)
  ├─ get_socket!(sockfd) 解析 fd
  ├─ 确定 is_nonblock (fd 标志 || MSG_DONTWAIT)
  ├─ 校验 dest_addr/addrlen（按 socket 类型）
  ├─ PSOCK::Datagram → 自动绑定 + 解析 dest_endpoint + try_sendmsg
  ├─ PSOCK::Stream → try_send (无需目标地址)
  ├─ PSOCK::Raw → try_sendmsg (同 Datagram)
  └─ 阻塞路径: WaitQueue::wait_until_interruptible
     非阻塞路径: NET_INTERFACE.try_poll() → try_send/try_sendmsg
```

关键细节：

- **内核中转 buffer**：使用 `copy_from_user_array` 将用户数据拷贝到内核分配的 `Vec<u8>`，避免 `trans_ref!` 在跨页边界返回不连续内存的 bug。
- **自动绑定**：对于未绑定的 DGRAM socket，在发送前自动绑定 `0.0.0.0:0`。
- **长度上限**：单次 sendto 最大 64MB，防止整数溢出和内核内存耗尽。

### recvfrom 调用（syscall 207）

`sys_recvfrom(sockfd, buf, len, flags, src_addr, addrlen)` 接收数据并获取源地址。

```rust
// os/src/net/syscall/recvfrom.rs
pub fn sys_recvfrom(
    sockfd: u32,
    buf: usize,
    len: u32,
    flags: u32,
    src_addr: usize,
    addrlen: usize,
) -> isize
```

完整的执行流程图：

```
sys_recvfrom 入口
  ├─ len 截断到 64MB
  ├─ MsgFlags::validate_for_recv() 校验 flags (MSG_OOB→EINVAL, MSG_ERRQUEUE→EAGAIN)
  ├─ 校验 src_addr 的 addrlen 值 (范围 12~512，否则 EINVAL)
  ├─ get_socket!(sockfd)
  ├─ 分配 kernel_buf (Vec<u8>)
  ├─ PSOCK::Stream → socket.try_recv(&mut kernel_buf)
  ├─ PSOCK::Datagram | Raw → socket.try_recvmsg() 或 try_peek_recvmsg()
  │   └─ 有源地址 → fill_sockaddr(src_addr, addrlen)
  ├─ 阻塞路径: WaitQueue::wait_until_interruptible(recv_wait_queue, ...)
  ├─ 非阻塞路径: NET_INTERFACE.try_poll() → recv()
  └─ copy_to_user_array(token, kernel_buf, buf, result)
```

关键细节：

- **MSG_PEEK**：仅 Datagram 和 Raw socket 支持，通过 `try_peek_recvmsg` 读取数据但不移除。
- **addrlen 验证**：在 syscall 入口通过 `UserPtr::<u32>::from_addr(addrlen).read(token)` 读取 `*addrlen` 的原始值，检查是否在 `[12, 512]` 范围内。
- **Stream 特殊处理**：TCP socket 的 recvfrom 忽略 `src_addr` 和 `addrlen`，行为等同于 recv。

### sendmsg 调用（syscall 211）

`sys_sendmsg(sockfd, msg_ptr, flags)` 通过 scatter/gather I/O 发送数据。

```rust
// os/src/net/syscall/sendmsg.rs
pub fn sys_sendmsg(sockfd: u32, msg_ptr: usize, flags: u32) -> isize
```

处理流程：

1. 从用户空间读取 `MsgHdr` 结构（包含 `msg_name`、`msg_namelen`、`msg_iov`、`msg_iovlen`）。
2. 通过 `UserIoVec::read_user_iovecs` 解析 iovec 数组。
3. 如果 `total_len == 0`，直接调用 `try_sendmsg(&[])`。
4. 根据 socket 类型选择发送策略：
   - **Stream**：`send_stream_chunked` — 分块发送，中间检查信号 `has_actionable_signal`。
   - **Datagram/Raw**：`send_single_shot` — 单次发送，长度超过 `IO_CHUNK_SIZE` 返回 `EMSGSIZE`。
5. 错误码包括：`EMSGSIZE`（DGRAM 或 Raw 超长）、`ENOBUFS`（内核 buffer 分配失败）、`EDESTADDRREQ`（无目标地址且未 connect）。

### recvmsg 调用（syscall 212）

`sys_recvmsg(sockfd, msg_ptr, flags)` 通过 scatter/gather I/O 接收数据并填充辅助信息。

```rust
// os/src/net/syscall/recvmsg.rs
pub fn sys_recvmsg(sockfd: u32, msg_ptr: usize, flags: u32) -> isize
```

处理流程：

1. 读取 `MsgHdr` 结构，解析 iovec。
2. 分配内核 buffer（上限 `IO_CHUNK_SIZE`），调用 `try_recvmsg` 或 `try_peek_recvmsg`。
3. 将数据写回用户 iovec 缓冲区。
4. 填充 `msg_name`（源地址）、`msg_namelen`、`msg_controllen`、`msg_flags`。
5. `msg_flags` 更新：如果数据被截断（nbytes 大于 buf.len()），设置 `MSG_TRUNC` 标志。

---

## Socket 选项

### getsockopt 调用（syscall 209）

`sys_getsockopt(sockfd, level, optname, optval, optlen)` 获取 socket 选项值。

```rust
// os/src/net/syscall/getsockopt.rs
pub fn sys_getsockopt(
    sockfd: u32,
    level: u32,
    optname: u32,
    optval_ptr: usize,
    optlen: usize,
) -> isize
```

支持的选项：

| Level | Optname | 行为 |
|-------|---------|------|
| SOL_SOCKET | SO_ERROR | 读取并清除 socket 待处理错误 |
| SOL_TCP | TCP_MAXSEG | 返回 MSS 值 |
| SOL_TCP | TCP_INFO | 返回 `TcpInfo` 结构（含 TCP 状态） |
| SOL_TCP | TCP_CONGESTION | 返回拥塞算法名称（"reno"） |
| SOL_SOCKET | SO_SNDBUF / SO_RCVBUF | 返回发送或接收缓冲区大小 |
| SOL_SOCKET | SO_REUSEADDR | 返回地址复用标志 |
| SOL_SOCKET | SO_PEERCRED | 返回对端进程凭证（pid、uid、gid） |
| SOL_SOCKET | SO_RCVTIMEO / SO_SNDTIMEO | 返回 `TimeVal`（当前始终返回 0） |
| SOL_IPV6 | IPV6_RECVPKTINFO | 返回 0 |

**错误码优先级**：先校验 `optlen >= 4`（否则返回 `EINVAL`），再匹配 level 和 optname。已知 level 但未知 optname 返回 `ENOPROTOOPT`，未知 level 返回 `EOPNOTSUPP`。

### setsockopt 调用（syscall 208）

`sys_setsockopt(sockfd, level, optname, optval, optlen)` 设置 socket 选项值。

```rust
// os/src/net/syscall/setsockopt.rs
pub fn sys_setsockopt(
    sockfd: u32,
    level: u32,
    optname: u32,
    optval_ptr: usize,
    optlen: u32,
) -> isize
```

支持的选项：

| Level | Optname | 行为 |
|-------|---------|------|
| SOL_SOCKET | SO_SNDBUF / SO_RCVBUF | 设置缓冲区大小（范围限制：4KB 到 256KB） |
| SOL_TCP | TCP_NODELAY | 关闭 Nagle 算法 |
| SOL_SOCKET | SO_KEEPALIVE | 开启或关闭 keepalive |
| SOL_SOCKET | SO_REUSEADDR | 设置地址复用 |
| SOL_SOCKET | SO_DONTROUTE | 空操作（仅接受，无实际效果） |
| SOL_SOCKET | SO_BINDTODEVICE | 绑定到指定网络设备 |
| SOL_SOCKET | SO_RCVTIMEO / SO_SNDTIMEO | 空操作（接受选项，当前未接入超时逻辑） |
| SOL_IP | IP_HDRINCL | 空操作（ping 程序会设置此选项） |
| SOL_IPV6 | IPV6_RECVPKTINFO / IPV6_RECVHOPLIMIT | 空操作 |
| SOL_IPV6 | IPV6_CHECKSUM | 空操作（兼容性接受） |
| SOL_IP | MCAST_JOIN_GROUP / MCAST_LEAVE_GROUP | 加入或离开多播组 |
| SOL_ICMPV6 | ICMP6_FILTER | 设置 ICMPv6 过滤器（32 字节） |
| SOL_RAW | IPV6_CHECKSUM | 设置 IPv6 校验和偏移量（必须为偶数） |

所有未知的 level 与 optname 组合返回 `ENOPROTOOPT`。

---

## 地址查询

### getsockname 调用（syscall 204）

```rust
// os/src/net/syscall/getsockname.rs
pub fn sys_getsockname(sockfd: u32, addr: usize, addrlen: usize) -> isize
```

委托给 `socket.addr(addr, addrlen)`，将本地绑定的地址通过 `fill_sockaddr` 写入用户缓冲区。

### getpeername 调用（syscall 205）

```rust
// os/src/net/syscall/getpeername.rs
pub fn sys_getpeername(sockfd: u32, addr: usize, addrlen: usize) -> isize
```

委托给 `socket.peer_addr(addr, addrlen)`，将对端地址写入用户缓冲区。如果 socket 未连接，返回 `-ENOTCONN`。行为遵循 Linux 语义：先检查 sockfd 有效性（返回 `EBADF`），再验证 addr 可访问性（返回 `EFAULT`），最后确认连接状态（返回 `ENOTCONN`）。

---

## I/O 阻塞抽象

网络 syscall 层使用多层 I/O 阻塞抽象，按优先级从高到低排列：

### WaitQueue::wait_until_interruptible（首选方案）

```rust
WaitQueue::wait_until_interruptible(wait_queue, || {
    match socket.try_recvmsg(&mut buf) {
        Ok(n) => Some(n as isize),
        Err(SyscallErr::EAGAIN) => None,  // 继续等待
        Err(e) => Some(-(e as isize)),    // 返回错误
    }
})
```

这是当前推荐的做法。条件闭包返回 `Some(result)` 时立即唤醒，返回 `None` 时将线程挂起等待通知。中断信号可使线程提前唤醒（返回 `WaitResult::Interrupted`）。

### wait_io（已废弃）

```rust
// os/src/syscall/utils.rs
pub fn wait_io<T: Into<isize>>(
    f: impl FnMut() -> Result<T, SyscallErr>,
    nonblock: bool,
) -> isize
```

自旋循环模式：
- 每次迭代调用 `NET_INTERFACE.poll()` 推进网络栈状态。
- 非阻塞模式遇到 `EAGAIN` 立即返回。
- 阻塞模式调用 `suspend_current_and_run_next()` 主动让出 CPU，下次调度时再次尝试。
- 唤醒后检查信号并处理超时。

**注意**：该函数已标记为废弃，新代码应优先使用 `WaitQueue::wait_until_interruptible`。

### wait_io_core（已废弃）

```rust
// os/src/syscall/utils.rs
pub fn wait_io_core(f: impl FnMut() -> isize, nonblock: bool) -> isize
```

与 `wait_io` 的区别在于：**不调用** `NET_INTERFACE.poll()`。适用于不需要网络轮询的文件 I/O 场景（管道、tty 等文件描述符）。

---

## 非阻塞路径规则

非阻塞路径的核心规则：

1. **MSG_DONTWAIT 覆盖 fd 标志**：`is_nonblock = fd_table.is_nonblock() || msg_dontwait`。
2. **try_poll 防止 livelock**：非阻塞路径在调用 `try_xxx` 前必须调用 `NET_INTERFACE.try_poll()`。这防止了无数据时反复 syscall 空转，确保 smoltcp 能在数据到达后及时进入 TCP 状态机。
3. **`try_xxx` 是纯尝试操作**：`try_recv`、`try_send`、`try_sendmsg`、`try_recvmsg` 都是单次非阻塞操作，内部从不执行 poll、sleep 或 yield。成功返回 `Ok(isize)`，失败返回 `Err(SyscallErr)`。
4. **EAGAIN 处理**：阻塞路径通过 `WaitQueue` 或 `wait_io` 在 EAGAIN 时等待，非阻塞路径直接返回 `-EAGAIN`。

---

## MsgFlags

定义在 `os/src/net/syscall/common.rs` 中。

| 标志 | 值 | 描述 |
|------|-----|------|
| `MSG_OOB` | 0x0001 | 带外数据（当前返回 EINVAL 或 EOPNOTSUPP） |
| `MSG_PEEK` | 0x0002 | 窥探数据，不移除 |
| `MSG_DONTROUTE` | 0x0004 | 绕过路由表 |
| `MSG_CTRUNC` | 0x0008 | 辅助数据被截断 |
| `MSG_PROXY` | 0x0010 | 代理操作（未使用） |
| `MSG_TRUNC` | 0x0020 | 数据报截断标志（recvmsg 回填） |
| `MSG_DONTWAIT` | 0x0040 | 非阻塞操作 |
| `MSG_EOR` | 0x0080 | 记录结束标志 |
| `MSG_WAITALL` | 0x0100 | 等待完整长度 |
| `MSG_FIN` | 0x0200 | TCP FIN（内部使用） |
| `MSG_SYN` | 0x0400 | TCP SYN（内部使用） |
| `MSG_CONFIRM` | 0x0800 | 路径确认 |
| `MSG_RST` | 0x1000 | TCP RST（内部使用） |
| `MSG_ERRQUEUE` | 0x2000 | 从错误队列接收 |
| `MSG_NOSIGNAL` | 0x4000 | 不发送 SIGPIPE |
| `MSG_MORE` | 0x8000 | 还有更多数据待发送 |

### 校验方法

- `validate_for_recv()`：`MSG_OOB` 返回 `EINVAL`，`MSG_ERRQUEUE` 返回 `EAGAIN`，其余返回 `is_nonblock`。
- `validate_for_send()`：`MSG_OOB` 返回 `EOPNOTSUPP`，`MSG_ERRQUEUE` 返回 `EOPNOTSUPP`，其余返回 `is_nonblock`。

---

## 返回值和错误码约定

| 层级 | 成功 | 错误 |
|----|------|------|
| Socket trait 方法（`try_recv`、`try_send` 等） | `Ok(isize)` | `Err(SyscallErr::XXX)` |
| syscall 处理函数 | `isize >= 0` | `-(errno as isize)`（负 errno） |
| `wait_io` 或 `WaitQueue` 包装 | `isize >= 0` | `isize < 0`（已编码为负 errno） |

常用 errno：`EAGAIN(11)`、`EINVAL(22)`、`EFAULT(14)`、`EINPROGRESS(115)`、`ENOTCONN(107)`、`EDESTADDRREQ(89)`、`EMSGSIZE(90)`、`ENOBUFS(105)`、`EOPNOTSUPP(95)`、`ENOPROTOOPT(92)`、`EAFNOSUPPORT(97)`、`ECONNREFUSED(111)`、`ETIMEDOUT(110)`。

---

## 公共模式

### get_socket! 宏

```rust
// os/src/syscall/syscall_macro.rs
macro_rules! get_socket {
    ($sockfd:expr) => {{
        let task = crate::task::current_task().unwrap();
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        let file = match fd_table.get_file($sockfd as usize) {
            Err(e) => return -(e as isize),
            Ok(f) => {
                if f.flags().contains(crate::fs::vfs::FileFlags::O_PATH) {
                    return -(SyscallErr::EBADF as isize);
                }
                f
            }
        };
        let any_ref = file.inode.as_any_ref();
        match any_ref.downcast_ref::<crate::net::SocketFile>() {
            Some(socket_file) => socket_file.inner.clone(),
            None => return crate::syscall::errno::ENOTSOCK,
        }
    }};
}
```

该宏的功能：

1. 获取当前任务的文件描述符表。
2. 检查 `O_PATH` 标志（通过 PATH 方式打开的文件描述符返回 `EBADF`）。
3. 通过 `downcast_ref` 将 Inode 转为 `SocketFile`。
4. 返回内部的 `Arc<dyn Socket>`。
5. 非 socket 文件描述符返回 `ENOTSOCK`。

### Endpoint::from_sockaddr

地址解析的统一入口，将用户空间的 `sockaddr` 二进制数据解析为内部 `Endpoint` 枚举：

```rust
pub enum Endpoint {
    Ip(IpEndpoint),        // AF_INET / AF_INET6
    Unix(UnixEndpoint),    // AF_UNIX (Path/Abstract/Unnamed)
    Netlink(u32),          // AF_NETLINK
    Packet(PacketEndpoint),// AF_PACKET
    Unspecified,           // AF_UNSPEC
}
```

使用模式：每个 socket syscall 在入口处先读取用户地址，调用 `Endpoint::from_sockaddr` 解析。`bind`、`connect`、`sendto`、`sendmsg` 均使用此模式。

### copy_from_user_array 与 copy_to_user_array

数据收发 syscall 使用内核中转 buffer 模式：

```rust
let token = task.get_user_token();
let mut kernel_buf = alloc::vec![0u8; len];
copy_from_user_array(token, buf as *const u8, kernel_buf.as_mut_ptr(), len)?;
// ... 在 kernel_buf 上操作 ...
copy_to_user_array(token, kernel_buf.as_ptr(), buf as *mut u8, result)?;
```

这种模式避免了两类问题：
- `trans_ref!` 返回单页切片导致跨页数据读取错误。
- `trans_refmut!` 跨页时写入非连续物理内存。

---

## 示例代码流程

### sys_sendto 完整流程

```
1. len = min(original_len, 64MB)           // 截断到上限
2. msg_flags = MsgFlags::from_bits_truncate(flags)
3. validate_for_send() → msg_dontwait       // 校验 flags
4. kernel_buf = alloc::vec![0u8; len]       // 分配内核中转 buffer
5. copy_from_user_array(token, user_buf, kernel_buf, len)
6. socket = get_socket!(sockfd)             // 解析 fd → Arc<dyn Socket>
7. is_nonblock = fd_nonblock || msg_dontwait
8. if socket_type == Datagram:
      if port == 0: auto_bind(0.0.0.0:0)     // 自动绑定
      dest = parse_dest_addr(dest_addr)      // 解析目标地址
      if dest is None && remote_endpoint is None:
        return -EDESTADDRREQ
9. if is_nonblock:
      NET_INTERFACE.try_poll()               // 防 livelock
      ret = socket.try_sendmsg(&kernel_buf, dest, flags)
      NET_INTERFACE.try_poll()
      return ret
    else:
      ret = WaitQueue::wait_until_interruptible(send_wait_queue, || {
        socket.try_sendmsg(&kernel_buf, dest, flags)
          .map(|n| n as isize)
          .or_else(|e| if e == EAGAIN { None } else { Some(-e as isize) })
      })
      NET_INTERFACE.try_poll()
      return ret
```

### sys_recvfrom 完整流程

```
1. len = min(original_len, 64MB)
2. msg_flags = MsgFlags::from_bits_truncate(flags)
3. validate_for_recv() → msg_dontwait       // 校验 flags
4. if src_addr != 0:                        // 校验 addrlen
      read *addrlen from user space
      if *addrlen < 12 || *addrlen > 512: return -EINVAL
5. socket = get_socket!(sockfd)
6. kernel_buf = alloc::vec![0u8; len]
7. define recv_fn:
      match socket_type:
        Stream  → socket.try_recv(&mut kernel_buf)
        Datagram|Raw:
          if MSG_PEEK: try_peek_recvmsg(&mut kernel_buf)
          else: try_recvmsg(&mut kernel_buf)
          if has src_addr: fill_sockaddr(src_addr, addrlen)
8. if is_nonblock:
      NET_INTERFACE.try_poll()
      result = recv_fn()
    else:
      result = WaitQueue::wait_until_interruptible(recv_wait_queue, || {
        recv_fn() → Ok → Some(n), EAGAIN → None, Err → Some(-e)
      })
9. if result > 0:
      copy_to_user_array(token, kernel_buf, user_buf, result)
10. return result
```

---

## 测试边界

覆盖以下场景：

- `MSG_OOB` 和 `MSG_ERRQUEUE` 在 send 和 recv 路径上返回预期错误。
- 非阻塞路径 `try_poll` 的前后调用顺序（防止 livelock）。
- 阻塞路径的信号中断处理（`WaitResult::Interrupted` 返回 `ERESTART`）。
- `sendto` 对未 connect 的 DGRAM socket 不指定目标地址（返回 `EDESTADDRREQ`）。
- `recvfrom` 的 `addrlen` 值过小（返回 `EINVAL`）或地址不可读（返回 `EFAULT`）。
- DGRAM 消息超过 `IO_CHUNK_SIZE`（返回 `EMSGSIZE`）。
- socketpair 的 `sv` 地址写入失败（返回 `EFAULT`）。
- `getsockopt` 的 `optlen` 校验优先级（`EINVAL` 优先于 `ENOPROTOOPT`）。

---

## 源文件索引

| 文件 | 导出函数 |
|------|----------|
| `os/src/net/syscall/socket.rs` | `sys_socket` |
| `os/src/net/syscall/bind.rs` | `sys_bind` |
| `os/src/net/syscall/connect.rs` | `sys_connect` |
| `os/src/net/syscall/listen.rs` | `sys_listen` |
| `os/src/net/syscall/accept.rs` | `sys_accept`、`sys_accept4` |
| `os/src/net/syscall/sendto.rs` | `sys_sendto` |
| `os/src/net/syscall/recvfrom.rs` | `sys_recvfrom` |
| `os/src/net/syscall/sendmsg.rs` | `sys_sendmsg` |
| `os/src/net/syscall/recvmsg.rs` | `sys_recvmsg` |
| `os/src/net/syscall/getsockopt.rs` | `sys_getsockopt` |
| `os/src/net/syscall/setsockopt.rs` | `sys_setsockopt` |
| `os/src/net/syscall/getsockname.rs` | `sys_getsockname` |
| `os/src/net/syscall/getpeername.rs` | `sys_getpeername` |
| `os/src/net/syscall/shutdown.rs` | `sys_sock_shutdown` |
| `os/src/net/syscall/socketpair.rs` | `sys_socketpair` |
| `os/src/net/syscall/common.rs` | `MsgFlags`、`check_addrlen`、`is_known_sockopt_level` |
| `os/src/syscall/utils.rs` | `wait_io`、`wait_io_core`、`wait_io_with_queue` |
| `os/src/syscall/syscall_macro.rs` | `get_socket!`、`trans_ref!`、`trans_refmut!` |
