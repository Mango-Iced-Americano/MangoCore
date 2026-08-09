---
title: "Unix 域套接字 (Unix Domain Socket)"
module: net/socket/unix
category: net
status: draft
owner: MangoCore Team
last_updated: 2026-08-08
code_paths:
  - "os/src/net/socket/unix/"
entry_points:
  - "UnixStreamSocket"
  - "UnixDatagramSocket"
  - "UnixEndpoint"
  - "PATH_TABLE"
  - "ABSTRACT_TABLE"
  - "make_unix_socket_pair"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "socketpair01"
    - "socketpair02"
    - "unix_stream01"
    - "unix_dgram01"
  oscomp:
    - "basic"
    - "busybox"
related_docs:
  - "docs/06_net/architecture.md"
  - "docs/06_net/syscall-layer.md"
  - "docs/06_net/socket-trait-and-fd.md"
---

## 概述

Unix 域套接字模块实现 AF_UNIX 协议族，支持 SOCK_STREAM 和 SOCK_DGRAM 两种语义。相比 INET 套接字，Unix 域套接字不走网络协议栈，数据通过内核内存中的环形缓冲区直接传递。它不依赖网络接口，在本机进程间通信场景中比 TCP 回环快得多。

设计参考 DragonOS 的 unix socket 模块，但做了大幅简化。本模块依赖 `Socket` trait 和 `SocketFile` 层接入 VFS fd 系统。

## 地址模型

### UnixEndpoint

`UnixEndpoint` 是 Unix 域套接字的地址表示，定义在 `mod.rs`：

```rust
pub enum UnixEndpoint {
    Path(String),       // 文件系统路径，如 "/tmp/my.sock"
    Abstract(Vec<u8>),  // 抽象命名空间，名称不含前导 NUL
    Unnamed,            // 匿名地址
}
```

三种地址的编码方式遵循 Linux 惯例。`Path` 对应文件系统上的一个 socket 特殊文件。`Abstract` 对应 Linux 抽象命名空间，其 `sun_path[0]` 为 NUL 字节，后面跟自定义名称。`Unnamed` 用于 `socketpair` 创建的匿名对端，不关联任何名字。

### 地址绑定表

流式套接字使用两个全局表来绑定命名地址：

- **PATH_TABLE** (`BTreeMap<String, Weak<dyn Socket>>`): 路径名到 socket 弱引用的映射。socket 销毁时自动从表中移除。路径绑定监听 socket 用 listen 前必须先 bind 到路径。
- **ABSTRACT_TABLE** (`UnixAbstractTable`): 抽象命名空间表。内部也是 `BTreeMap<Arc<[u8]>, Weak<dyn Socket>>`。提供 `create_abstract_name_bytes`、`lookup_abstract_name_bytes`、`remove_abstract_name_bytes` 等操作。还支持 `alloc_ephemeral_abstract_name` 用于分配临时名字。

路径长度限制为 `UNIX_PATH_MAX = 108` 字节，与 Linux 保持一致。

数据报套接字使用独立的 **BIND_TABLE** (`BindTable`)，同时管理 path 和 abstract 两套注册。这是因为数据报的连接语义不同：两个数据报 socket 不需要 listen/accept，而是通过 BIND_TABLE 直接寻址发送。

### fill_with_endpoint

`fill_with_endpoint` 函数将 `UnixEndpoint` 序列化成用户空间的 `sockaddr_un` 结构。它处理三种地址变体的编码：Path 变体以自然字节写入 sun_path 并补 NUL，Abstract 变体在开头插入 NUL 字节，Unnamed 变体只写入 2 字节的 sa_family。

## UnixStreamSocket 状态机

流式套接字使用三态状态机管理生命周期，定义在 `stream/inner.rs`：

```
                ┌──────────┐
                │   Init   │
                ├──────────┤
                │ addr?    │
                └────┬─────┘
                     │
          ┌──────────┴──────────┐
          │ bind      │ listen  │
          ▼            ▼
    ┌──────────┐  ┌──────────┐
    │Connected │  │Listener  │
    ├──────────┤  ├──────────┤
    │ addr     │  │ local_   │
    │ peer_    │  │ addr     │
    │ addr     │  │ backlog  │
    │ peer_    │  │ incoming │
    │ creds    │  │ (vec)    │
    │ rx       │  └──────────┘
    │ peer_rx  │
    └──────────┘
```

### Init

初始状态。socket 刚被创建或已完成关闭。可选的 `addr` 字段存放 bind 设置的本地地址。Init 状态可以调用 bind 转换到带地址的 Init，或调用 listen 跳转到 Listener，或由对端发起 connect 变为 Connected。

### Connected

已连接状态。两端通过环形缓冲区交换数据。关键字段：

- `addr`: 本端本地地址
- `peer_addr`: 对端地址
- `peer_creds`: 对端进程凭证 (pid, uid, gid)
- `rx`: 本端接收缓冲区 (Arc)
- `peer_rx`: 对端的接收缓冲区（本端写入方向）

两个方向各有一个独立的 `RingBuffer<u8>`，通过 `Arc<Mutex<>>` 共享。`side_a.peer_rx == side_b.rx`，形成交叉引用。

### 对端进程退出与 Drop

进程关闭最后一个 Unix stream fd 时，`UnixStreamSocket::Drop` 必须把关闭状态传播到交叉的两个 RingBuffer：对端接收缓冲区标记发送端已关闭，本端接收缓冲区标记接收端已关闭。随后通知对端的读、写等待队列。这样，等待 `recvfrom()` 的任务会在已缓冲的数据耗尽后观察到 EOF；等待发送的任务也会重新检查接收端关闭状态，而不是永久睡眠。

该路径与显式 `shutdown()` 的方向性语义一致，但由最后一个 fd 的 RAII Drop 触发。Drop 只负责端点关闭和命名空间清理，不保留对端资源的强引用。

### Listener

监听状态。由 Init bind 后调用 listen 进入。持有 `local_addr`、`backlog` 和 `incoming` 连接队列。backlog 固定为 16。当对端发起 connect 时，`Connected` 对象被推入 incoming 队列，等待 accept 取出。

## socketpair

`socketpair()` 系统调用创建一对已连接的匿名 Unix 流套接字。实现由 `make_unix_socket_pair` 完成：

```rust
pub fn make_unix_socket_pair(
    is_nonblock: bool,
    socket_type: PSOCK,
) -> (Arc<dyn Socket>, Arc<dyn Socket>)
```

对 SOCK_STREAM，它调用 `Connected::new_pair(buf_size)` 创建交叉引用的两个 Connected 实例，然后分别包装成 `UnixStreamSocket`。两个 socket 的缓冲区大小默认 64KB。对 SOCK_DGRAM，它调用 `UnixDatagramSocket::new_pair`，分配两个抽象名并互相注册为对端。

## UnixDatagramSocket

数据报套接字使用消息队列代替流式缓冲区。每个消息保存为 `DatagramMessage { data, src_addr }`，接收队列容量默认 128 条消息。

关键流程：

- **发送**: `send_to_bound` 根据对端地址查 BIND_TABLE，找到目标 socket 后直接向它的 recv_queue 推送新消息，然后唤醒对端的等待队列。
- **接收**: `try_recv` 从 recv_queue 弹出最早的消息，返回数据字节数和发送端地址（通过 `try_recvmsg`）。
- **connect**: 设置 `peer_addr`，后续 `send` 直接使用该预设地址。不支持 listen/accept。
- **无连接发送**: 通过 `try_sendmsg` 在每次发送时指定目标地址。

## RingBuffer

`RingBuffer<T>` 是通用环形缓冲区，底层使用 `VecDeque<T>`。它不是传统意义上的固定容量循环数组，VecDeque 内部可自动扩容，但通过 `capacity` 字段限制消息总数。支持两个方向的 shutdown 标志位：`recv_shutdown`（对端关闭读取）和 `send_shutdown`（本端关闭写入）。

全局计数器 `RB_COUNT` 和 `RB_BYTES` 分别追踪活跃 RingBuffer 数量和分配的总容量，可通过 `rb_alive()` 和 `rb_bytes()` 查询。

## 对端凭证 (SO_PEERCRED)

`Socket` trait 的 `peer_creds()` 方法返回 `(pid, uid, gid)`。连接建立时，发起方的凭证通过 `current_task()` 获取并保存到双方的 `peer_creds` 字段。这符合 Linux 的 SO_PEERCRED 语义，让服务端可以验证客户端身份。

当前实现从任务的 `acquire_inner_lock` 中读取 uid 和 gid，直接使用内核态权限值。生产级实现可能需要更细粒度的权限检查。

## 文件组织

| 文件 | 职责 |
|------|------|
| `mod.rs` | UnixEndpoint、PATH_TABLE、工厂函数、fill_with_endpoint、alloc_socket_fd |
| `stream/mod.rs` | UnixStreamSocket 结构体及 Socket trait 实现、wait queue 管理、Drop 清理 |
| `stream/inner.rs` | 状态机：Init / Connected / Listener、Connected::new_pair、双向环形缓冲区引用 |
| `datagram/mod.rs` | UnixDatagramSocket、BIND_TABLE、DatagramMessage、消息队列收发 |
| `ns/mod.rs` | UnixAbstractTable、UNIX_PATH_MAX=108、抽象名字的创建/查询/移除 |
| `ring_buffer.rs` | RingBuffer<T> 通用环形缓冲区、shutdown 标志位、全局计数器 |

## Test Mapping

| 特性 | Syscall | 测试用例 | 状态 |
|------|---------|----------|------|
| socketpair SOCK_STREAM | `sys_socketpair` | `socketpair01` | pass |
| socketpair SOCK_DGRAM | `sys_socketpair` | `socketpair02` | pass |
| Unix stream connect (abstract) | `sys_connect` | `unix_stream01` | pass |
| Unix stream send/recv | `sys_sendto` / `sys_recvfrom` | `unix_stream01` | pass |
| Unix datagram send/recv | `sys_sendmsg` / `sys_recvmsg` | `unix_dgram01` | pass |
| Unix bind / listen / accept | `sys_bind` / `sys_listen` / `sys_accept4` | `socketpair01` | pass |
| SO_PEERCRED | `sys_getsockopt` | `unix_peercred01` | partial |
| Unix shutdown | `sys_shutdown` | `unix_shutdown01` | pass |
| 对端进程退出后的 EOF/唤醒 | fd Drop | buildstorm rustc QEMU 回归 | pass（minibuild cargo build） |
| O_NONBLOCK Unix I/O | `sys_read` / `sys_write` | `unix_nonblock01` | pass |
| Unix abstract namespace | `sys_bind` (abstract) | `unix_abstract01` | pass |

### LTP 跳过清单

| 用例 | 跳过原因 |
|------|----------|
| `unix_peercred01` (部分场景) | 凭证仅在内核态获取，未验证用户态 namespace 隔离 |
| `unix_pathname` 系列 | 文件系统路径绑定后文件残留问题待处理 |
| `unix_msgsnd` | 当前未实现 SCM_RIGHTS 控制消息传递 |

## Known Issues

1. **SCM_RIGHTS 控制消息未实现**
   当前不支持通过 Unix 套接字传递文件描述符。Linux 允许使用 `sendmsg` 的 `SCM_RIGHTS` 辅助数据跨进程传递 fd。未来可考虑在 Connected 或 DatagramMessage 中增加辅助数据缓冲区。

2. **PATH_TABLE 文件残留**
   路径绑定的 socket 创建 sock 文件后，如果进程崩溃退出 (Drop 未触发 unlink)，文件系统上会残留 socket 文件。Linux 的解决方式是在 bind 时创建文件，close 时删除。当前实现仅在 Drop 中从 PATH_TABLE 移除条目，但未删除实际文件。

3. **数据报 connect 只校验存在性**
   `UnixDatagramSocket::connect` 只检查对端是否已注册到 BIND_TABLE。它不会尝试建立真正的连接状态或验证对端是否存活。Linux 的 SOCK_DGRAM connect 行为也是如此，但 MangoCore 实现中 connect 后的 send 并没有重试机制。

4. **backlog 硬编码为 16**
   `listen()` 的 backlog 参数被忽略，始终使用固定值 16。如果 socketpair 测试或应用大量并发连接，可能在队列满时返回 EAGAIN。未来应改为从 listen 的参数获取 backlog 值。

5. **SeqPacket 映射为 Stream**
   当前 SOCK_SEQPACKET 被映射为 `UnixStreamSocket`，没有消息边界。这是简化做法。真正的 SOCK_SEQPACKET 需要保留消息边界语义，每个 sendmsg 对应一个 receive 单元。

## 设计决策

- **简化优先**: 相比 DragonOS 的原子 RingBuffer + RwSem，本模块使用 `Mutex<VecDeque>`，并发读写由队列锁串行化；该模型可跨 CPU 使用，但高竞争下的扩展性弱于无锁 RingBuffer。
- **弱引用注册表**: PATH_TABLE 和 ABSTRACT_TABLE 存储 Weak 引用而非 Arc，避免循环引用导致内存泄漏。socket 的 Drop 实现负责从表中清理自身。
- **不依赖 smoltcp**: Unix 套接字完全在内核中实现，不走 smoltcp 协议栈。polling 仅通过等待队列和 epoll 事件通知完成。
