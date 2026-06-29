---
title: "网络 syscall"
category: syscall
status: stable
author: MangoCore Team
last_update: 2026-06-29
tags: [syscall, net, socket]
---

# 网络 syscall

## 1. 概述

网络 syscall 在全局分发表中由 `os/src/syscall/mod.rs` 注册，实际实现位于 `os/src/net/syscall/`。分发层负责把 `usize` ABI 参数转成 fd、用户地址、flags 和长度；网络 syscall 文件负责 sockaddr 解析、fd 到 socket 的转换、阻塞/非阻塞策略；socket trait 和具体协议实现位于 `os/src/net/socket/`。

```
trap handler
  -> syscall::syscall(id, args)
    -> sys_socket/sys_bind/sys_sendto/...
      -> net/syscall/common.rs
        -> SocketFile / Socket trait
          -> TcpSocket / UdpSocket / RawSocket / UnixSocket / ...
```

## 2. 分发表

| 编号 | syscall | 分支 | 实现文件 |
|------|---------|------|----------|
| 198 | `socket` | `sys_socket(domain, type, protocol)` | `net/syscall/socket.rs` |
| 199 | `socketpair` | `sys_socketpair(domain, type, protocol, sv)` | `net/syscall/socketpair.rs` |
| 200 | `bind` | `sys_bind(sockfd, addr, addrlen)` | `net/syscall/bind.rs` |
| 201 | `listen` | `sys_listen(sockfd, backlog)` | `net/syscall/listen.rs` |
| 202 | `accept` | `sys_accept(sockfd, addr, addrlen)` | `net/syscall/accept.rs` |
| 203 | `connect` | `sys_connect(sockfd, addr, addrlen)` | `net/syscall/connect.rs` |
| 204 | `getsockname` | `sys_getsockname(sockfd, addr, addrlen)` | `net/syscall/getsockname.rs` |
| 205 | `getpeername` | `sys_getpeername(sockfd, addr, addrlen)` | `net/syscall/getpeername.rs` |
| 206 | `sendto` | `sys_sendto(sockfd, buf, len, flags, dest, addrlen)` | `net/syscall/sendto.rs` |
| 207 | `recvfrom` | `sys_recvfrom(sockfd, buf, len, flags, src, addrlen)` | `net/syscall/recvfrom.rs` |
| 208 | `setsockopt` | `sys_setsockopt(sockfd, level, optname, optval, optlen)` | `net/syscall/setsockopt.rs` |
| 209 | `getsockopt` | `sys_getsockopt(sockfd, level, optname, optval, optlen)` | `net/syscall/getsockopt.rs` |
| 210 | `shutdown` | `sys_sock_shutdown(sockfd, how)` | `net/syscall/shutdown.rs` |
| 211 | `sendmsg` | `sys_sendmsg(sockfd, msg, flags)` | `net/syscall/sendmsg.rs` |
| 212 | `recvmsg` | `sys_recvmsg(sockfd, msg, flags)` | `net/syscall/recvmsg.rs` |
| 242 | `accept4` | `sys_accept4(sockfd, addr, addrlen, flags)` | `net/syscall/accept.rs` |

`SYSCALL_SOCK_SHUTDOWN = 210` 是 socket 半关闭。`SYSCALL_SHUTDOWN = 501` 是系统关机 syscall，分发到 `sys_shutdown()`，不读取 socket fd。

## 3. 公共辅助层

`net/syscall/common.rs` 承担 syscall 参数和 socket 层之间的转换：

| 职责 | 说明 |
|------|------|
| fd 转 socket | 从当前进程 fd table 取得 `File`，再识别 socket file |
| sockaddr 解析 | 从用户空间读取 `sockaddr`，转换为内部 `Endpoint` |
| sockaddr 写回 | `getsockname`/`getpeername`/`accept`/`recvfrom` 写用户地址 |
| addrlen 检查 | 地址长度、NULL 指针和架构对齐检查 |
| flags 解析 | `MSG_DONTWAIT`、`MSG_PEEK`、`MSG_TRUNC` 等传入 socket 层 |

用户 buffer 仍通过 `mm/uaccess.rs` 翻译；网络层不直接解引用用户裸指针。

## 4. socket 创建

`sys_socket(domain, sock_type, protocol)`：

```
Socket::alloc(domain, sock_type, protocol)
  -> TcpSocket / UdpSocket / RawSocket / Unix / Netlink / Packet
impl_file_for_socket! 或 SocketFile
fd_table.alloc_fd(file, SOCK_CLOEXEC?)
```

错误边界由 `Socket::alloc()` 和协议族实现决定：

| 场景 | errno |
|------|-------|
| 未知 domain | `EAFNOSUPPORT` |
| socket type/protocol 不支持 | `EPROTONOSUPPORT` 或 `ESOCKTNOSUPPORT`，按具体实现 |
| fd 分配失败 | fd table 返回的 errno |

`socketpair` 只对 AF_UNIX 的 stream/datagram 路径有意义；非 AF_UNIX 返回 `EPROTONOSUPPORT`。

`sys_socket()` 的源码很短，分发层在这里完成 raw socket type flags 解析：

```rust
pub fn sys_socket(domain: u32, socket_type: u32, protocol: u32) -> isize {
    info!(
        "[sys_socket] domain: {}, type: {}, protocol: {}",
        domain, socket_type, protocol
    );
    // 在 syscall 入口处解析 raw u32 → PSOCK + bool flags
    let type_arg = PosixArgsSocketType::from_bits_truncate(socket_type);
    let psock = match PSOCK::try_from(type_arg) {
        Ok(s) => s,
        Err(e) => return -(e as isize),
    };
    let is_nonblock = type_arg.is_nonblock();
    let is_cloexec = type_arg.is_cloexec();
    let result = match crate::net::Socket::alloc(domain, psock, protocol, is_nonblock, is_cloexec) {
        Ok(sockfd) => {
            info!("[sys_socket] new sockfd: {}", sockfd);
            sockfd as isize
        }
        Err(e) => {
            info!("[sys_socket] new sockfd failed");
            -(e as isize)
        }
    };
    result
}
```

`PosixArgsSocketType::from_bits_truncate()` 保留 socket 类型和 `SOCK_NONBLOCK`、`SOCK_CLOEXEC` 等标志。`PSOCK::try_from()` 将 POSIX 参数转换成内部 socket 类型；真正的 domain/protocol 分派在 `Socket::alloc()` 中完成。

## 5. bind/listen/connect/accept

### 5.1 bind

`sys_bind()` 从用户 sockaddr 读出 `Endpoint` 后调用 `socket.bind(&endpoint)`。不同 socket 类型的 bind 语义不同：

| 类型 | bind 语义 |
|------|-----------|
| TCP/UDP | 地址和端口绑定，端口冲突由 port manager 检查 |
| Unix | 路径或 abstract namespace 绑定 |
| Netlink | 分配或记录 port id |
| Packet | 绑定 ifindex 和协议 |
| Raw | 记录本地 endpoint |

### 5.2 listen/accept

`listen` 对 TCP stream 和 Unix stream 有意义。UDP/RAW/Netlink/Packet 等不支持监听的 socket 返回 `EOPNOTSUPP`。

`accept` 和 `accept4` 共享 `accept.rs`。`accept4` 额外处理 flags，例如 nonblock/cloexec。成功后创建新的 socket fd，并按需写回 peer sockaddr。

`sys_accept()` 优先使用 socket 自带的 accept wait queue；没有 wait queue 的类型回退到通用 `wait_io()`：

```rust
pub fn sys_accept(sockfd: u32, addr: usize, addrlen: usize) -> isize {
    let socket = crate::get_socket!(sockfd);
    let task = current_task().unwrap();
    let is_nonblock = match task.process.files().lock().get_file(sockfd as usize) {
        Ok(f) => f.is_nonblock(),
        Err(e) => return -(e as isize),
    };

    if let Some(wait_queue) = socket.accept_wait_queue() {
        if is_nonblock {
            match socket.accept(sockfd, addr, addrlen) {
                Ok(n) => n as isize,
                Err(e) => -(e as isize),
            }
        } else {
            // Pre-poll OUTSIDE the WaitQueue closure — harness-patterns rule:
            // never put poll inside WaitQueue condition closure.
            NET_INTERFACE.try_poll();

            ACCEPT_WAITER_COUNT.fetch_add(1, Ordering::Relaxed);
            let result = loop {
                match WaitQueue::wait_until_interruptible(wait_queue, || {
                    // NO poll inside closure — only accept
                    match socket.accept(sockfd, addr, addrlen) {
                        Ok(n) => Some(n as isize),
                        Err(SyscallErr::EAGAIN) => None,
                        Err(e) => Some(-(e as isize)),
                    }
                }) {
                    WaitResult::Ready(val) => break val,
                    WaitResult::Interrupted => break -(SyscallErr::ERESTART as isize),
                    WaitResult::TimedOut => continue,
                }
            };
            ACCEPT_WAITER_COUNT.fetch_sub(1, Ordering::Relaxed);
            result
        }
    } else {
        wait_io(
            || socket.accept(sockfd, addr, addrlen).map(|s| s as isize),
            is_nonblock,
        )
    }
}
```

这个实现体现了网络 syscall 的等待边界：非阻塞 accept 只尝试一次；阻塞 accept 在进入 `WaitQueue` 前调用 `NET_INTERFACE.try_poll()`，等待闭包内部只执行 `socket.accept()`。

`accept4()` 复用 `sys_accept()`，成功后再修改新 fd 的 CLOEXEC 和 NONBLOCK 标志：

```rust
pub fn sys_accept4(sockfd: u32, addr: usize, addrlen: usize, flags: u32) -> isize {
    const SOCK_CLOEXEC: u32 = 1 << 19;
    const SOCK_NONBLOCK: u32 = 0x800;

    crate::trace_event!(0xB040, sockfd as u64, flags as u64, 0, 0, 0, 0);

    let ret = sys_accept(sockfd, addr, addrlen);

    crate::trace_event!(0xB041, ret as u64, 0, 0, 0, 0, 0);

    if ret < 0 {
        return ret;
    }
    let new_fd = ret as usize;

    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let mut fd_table = files_ref.lock();
    if flags & SOCK_CLOEXEC != 0 {
        let _ = fd_table.set_cloexec(new_fd, true);
    }
    if flags & SOCK_NONBLOCK != 0 {
        if let Ok(f) = fd_table.get_file(new_fd) {
            f.set_nonblock(true);
        }
    }

    ret
}
```

### 5.3 connect

`connect` 对 TCP 会进入连接状态机；非阻塞 socket 可返回 `EINPROGRESS`。UDP connect 只记录 remote endpoint，使后续 send 不必再传目标地址。Unix stream connect 建立本地 ring buffer 连接。

## 6. send/recv

### 6.1 sendto

`sys_sendto(sockfd, buf, len, flags, dest, addrlen)`：

```
fd -> socket
用户 buffer -> kernel/UserBuffer
dest sockaddr 可选解析
MSG_DONTWAIT / O_NONBLOCK 判断
socket.try_sendmsg()/try_send()
必要时等待 send_waiters 后重试
```

UDP 可使用 dest endpoint；已 connect socket 可不传 dest。TCP stream 发送忽略 datagram 目标地址。

### 6.2 recvfrom

`sys_recvfrom(sockfd, buf, len, flags, src, addrlen)`：

```
fd -> socket
用户输出 buffer 可写检查
socket.try_recvmsg()/try_recv()
可选写回 src sockaddr 和 addrlen
```

若 socket 暂无数据且为非阻塞路径，返回 `EAGAIN`；阻塞路径通过等待队列睡眠，信号打断后返回可重启/中断语义。

### 6.3 sendmsg/recvmsg

`sendmsg`/`recvmsg` 读取用户 `msghdr` 和 iovec，支持向量 I/O、可选 name 地址和控制信息字段。具体控制消息覆盖范围以 `net/syscall/sendmsg.rs`、`recvmsg.rs` 的分支为准。

## 7. setsockopt/getsockopt

socket 选项分层处理：

| 层 | 例子 |
|----|------|
| SOL_SOCKET | `SO_REUSEADDR`, `SO_RCVBUF`, `SO_SNDBUF`, `SO_ERROR`, `SO_TYPE`, `SO_BINDTODEVICE`, `SO_PEERCRED` |
| IP/IPV6 | multicast、checksum、raw 相关选项 |
| TCP | `TCP_NODELAY`, `TCP_INFO` 等 TCP 状态/行为 |
| packet/unix/netlink 特有 | 按 socket 类型实现 |

未知 level/optname 按 Linux 语义返回 `ENOPROTOOPT(92)`，而不是 `EOPNOTSUPP(95)`。这一点对 LTP socket option 用例很敏感。

## 8. getsockname/getpeername

两类 syscall 都要写回用户 sockaddr 和 addrlen：

| syscall | 数据来源 |
|---------|----------|
| `getsockname` | socket 本地 endpoint |
| `getpeername` | socket 对端 endpoint |

`getpeername` 的参数验证优先于连接状态检查：用户地址或 addrlen 指针错误应返回 `EFAULT`，不是先返回 `ENOTCONN`。RISC-V 未对齐地址不会由硬件自动报错，因此 addrlen 对齐需要显式检查。

## 9. shutdown

`sys_sock_shutdown(fd, how)` 调用 socket 的 shutdown/half-close 语义：

| `how` | 语义 |
|-------|------|
| `SHUT_RD` | 关闭读方向 |
| `SHUT_WR` | 关闭写方向 |
| `SHUT_RDWR` | 读写都关闭 |

不同 socket 类型的 shutdown 支持程度不同。TCP/Unix stream 有明确半关闭状态；不支持半关闭的类型返回具体 errno。

## 10. 阻塞与 poll 契约

网络 I/O 使用两类等待模式：

| 模式 | 行为 |
|------|------|
| 非阻塞路径 | 在 `try_xxx` 前执行 `NET_INTERFACE.try_poll()`，避免 smoltcp 数据未搬运导致 livelock |
| 阻塞路径 | 每次重试前 poll，失败为 `EAGAIN` 时挂到 socket wait queue 或让出 CPU |

socket readiness 由 `SocketFile::poll()` 和 socket 类型的 `socket_r_ready/socket_w_ready` 等方法提供，epoll/poll/select 都依赖这一路径。

## 11. errno 边界

| 场景 | errno |
|------|-------|
| fd 不是 socket | `ENOTSOCK` 或 fd 层错误 |
| domain 不支持 | `EAFNOSUPPORT` |
| socketpair 非 AF_UNIX | `EPROTONOSUPPORT` |
| 不支持 listen/accept 的 socket | `EOPNOTSUPP` |
| 非阻塞 connect 未完成 | `EINPROGRESS` 或 `EAGAIN`，按状态机 |
| 地址已占用 | `EADDRINUSE` |
| 目标不可达 | `ENETUNREACH`/`EHOSTUNREACH` |
| 未连接发送/查询 peer | `ENOTCONN` |
| 用户 sockaddr/buffer 错误 | `EFAULT` |
| 未知 socket option | `ENOPROTOOPT` |
| UDP payload 过大 | `EMSGSIZE` |

网络 syscall 在 02 目录中只作为分发表入口和 errno 边界索引；真实协议状态在 `docs/06_net` 展开。调试时仍要先经过通用 syscall 层：确认编号进入 `syscall/mod.rs` 的 socket 分支，确认 fd table 返回的是 socket File 对象，再进入 `net/syscall/*` 和具体 socket 类型。

和普通文件不同，socket 的可读写状态依赖协议栈 poll。非阻塞路径在 `try_xxx` 前需要推动网络接口，阻塞路径则通过 socket wait queue 反复尝试；如果 `send/recv/connect/accept` 看似没有进展，除了 syscall 参数，还要检查 `NET_INTERFACE.try_poll()` 和设备收发路径。

## 12. 测试映射

| 功能 | LTP/OSComp 入口 |
|------|-----------------|
| socket 创建 | `socket01`, `socket02` |
| socketpair | `socketpair01`, `socketpair02` |
| bind/listen/accept/connect | `bind*`, `listen01`, `accept*`, `connect*` |
| send/recv | `send*`, `recv*`, `sendto*`, `recvfrom*` |
| message I/O | `sendmsg*`, `recvmsg*`, `sendmmsg*`, `recvmmsg*` |
| options | `getsockopt*`, `setsockopt*` |
| readiness | `poll*`, `select*`, `epoll*` |
| 性能/网络组 | busybox 网络工具、iperf、netperf |

## 13. 源文件索引

| 路径 | 内容 |
|------|------|
| `os/src/syscall/mod.rs` | 网络 syscall 分发分支 |
| `os/src/net/syscall/common.rs` | sockaddr、fd/socket、公用参数处理 |
| `os/src/net/syscall/socket.rs` | socket 创建 |
| `os/src/net/syscall/socketpair.rs` | socketpair |
| `os/src/net/syscall/bind.rs` | bind |
| `os/src/net/syscall/listen.rs` | listen |
| `os/src/net/syscall/connect.rs` | connect |
| `os/src/net/syscall/accept.rs` | accept/accept4 |
| `os/src/net/syscall/sendto.rs` | sendto/send |
| `os/src/net/syscall/recvfrom.rs` | recvfrom/recv |
| `os/src/net/syscall/sendmsg.rs` | sendmsg |
| `os/src/net/syscall/recvmsg.rs` | recvmsg |
| `os/src/net/syscall/setsockopt.rs` | setsockopt |
| `os/src/net/syscall/getsockopt.rs` | getsockopt |
| `os/src/net/socket/mod.rs` | Socket trait 和 SocketFile |
| `docs/06_net/syscall-layer.md` | 网络 syscall 领域级详解 |
