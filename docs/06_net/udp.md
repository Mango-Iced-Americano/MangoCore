---
title: "UDP 协议实现"
module: net/socket/udp
category: net
status: current
owner: MangoCore Team
last_updated: 2026-08-04
code_paths:
  - "os/src/net/socket/inet/datagram/"
entry_points:
  - UdpSocket
  - dispatch_udp_packets
  - UDP_SOCKETS
arch:
  rv64: supported
  la64: supported
tests:
  ltp: "N/A (no direct LTP coverage for UDP)"
related_docs:
  - architecture.md
  - syscall-layer.md
  - socket-trait-and-fd.md
  - inet-common.md
---

# UDP 协议实现

## 概述

UDP 子系统基于 smoltcp 的 `udp::Socket` 实现，通过 `Socket` trait 对外暴露无连接数据报服务。设计以单次非阻塞尝试为基础，通过 `rx_queue` 解耦 smoltcp 接收与用户态消费，通过 `try_deliver_local` 实现本地回环快速路径。所有 I/O 操作遵循 `try_xxx` 约定，不做轮询或内部重试。

bind 先在 N1 快照 `BindIntent`，再在所属 `NetNamespace::ports` 完成 reserve→socket.bind→commit/abort；N0 不跨 socket/DeviceStack。N2 内 UDP drain 只形成内核所有数据，释放 N2 才写 OS queue、通知 waiter 或 copyout。readiness miss 只 kick generation worker 后进入纯条件等待，不在锁或条件闭包中 poll。

## UdpSocket 结构体

定义在 `datagram/udp.rs`。每个 `UdpSocket` 对应一个 smoltcp `RouteSocketHandle` 和一个独立接收队列：

```rust
pub struct UdpSocket {
    inner: Mutex<UdpSocketInner>,
    socket_handler: RouteSocketHandle,
    bound: Mutex<BoundInner>,
    bound_ifindex: Mutex<Option<u32>>,
    recv_waiters: EventWaitQueue,
    send_waiters: EventWaitQueue,
    pub ip_version: IpVersion,
    ipv6_v6only: AtomicBool,
}
```

- `socket_handler`: smoltcp 路由 socket 句柄，由 `NET_INTERFACE.add_routed_socket()` 分配，标识协议栈中的 UDP socket 实例。
- `bound`: 封装绑定的句柄、接口索引、地址和端口信息，提供 `bind()` / `unbind()` 操作。
- `bound_ifindex`: `SO_BINDTODEVICE` 设置的接口索引，覆盖路由选路结果。
- `ipv6_v6only`: 控制 IPv6 socket 是否仅接受 IPv6 流量。`false` 时允许 IPv4-mapped IPv6 地址。

### UdpSocketInner

核心内部状态，在 `Mutex` 保护下访问：

```rust
struct UdpSocketInner {
    remote_endpoint: Option<IpEndpoint>,
    local_endpoint: Option<IpListenEndpoint>,
    rx_queue: VecDeque<(Vec<u8>, IpEndpoint)>,
    last_recv_addr: Option<IpEndpoint>,
    msg_more_buf: Vec<u8>,
    recvbuf_size: usize,
    sendbuf_size: usize,
    reuse_addr: bool,
    multicast_group_joined: bool,
    ipv6_checksum_offset: Option<u32>,
}
```

- `rx_queue`: 接收队列，每个条目包含 payload 和源地址。数据通过 `dispatch_udp_packets`（来自 smoltcp）或 `try_deliver_local`（本地回环）放入。
- `msg_more_buf`: `MSG_MORE` 累积缓冲区，发送时与当前数据合并。
- `last_recv_addr`: 最近一次 `try_recvmsg` 的源地址，供 `recvfrom` 返回到用户空间。

## 实例化

`UdpSocket::new(ver)` 创建 smoltcp UDP socket，配置 1024 个 packet metadata 槽位和 `MAX_BUFFER_SIZE`（64KB）缓冲区，注册到 `NET_INTERFACE`，返回完工的 `UdpSocket` 实例。`register_udp_socket()` 将弱引用推入全局 `UDP_SOCKETS` 表。

## bind

`UdpSocket::bind()` 处理端口绑定：

1. 端口为 0 时调用 `PortManager::alloc_ephemeral_port()` 分配临时端口。该函数从 `local_port_range()` 获取范围（32768-60999，匹配 Linux 默认值），跳过 `TCP_PORTS` / `UDP_PORTS` 中已占用的端口。
2. `INADDR_ANY` 地址映射为 smoltcp 的 `addr: None` 语义。
3. 通过 `NET_INTERFACE.udp_routed_socket()` 调用底层 `socket.bind()`。
4. 调用 `bound.lock().bind()` 更新绑定记录，记录 ifindex。

`SO_REUSEADDR` 在 bind 时生效：双方都启用 `SO_REUSEADDR` 时跳过端口冲突检查。已 connect 到特定远端的 UDP socket 不阻止同端口其他 socket 的 bind。

## connect

`UdpSocket::connect()` 设置 `inner.remote_endpoint`。未 bind 时自动分配临时端口和源 IP（通过 `lookup_source_ip` 选择最匹配的路由地址）。`INADDR_ANY` 映射为本地回环地址（127.0.0.1 或 ::1）。connect 后的 socket 可通过 `try_send()` 直接发送，无需每次指定目标地址。

UDP 的 connect 是本地状态操作，不产生网络报文。

## 发送路径

### try_send / try_sendmsg

发送前先做 `EMSGSIZE` 检查（UDP 最大负载 = 65535 - 20 IP头 - 8 UDP头 = 65507 字节）：

```
buffer > 65507 --> EMSGSIZE
```

发送数据组装后按以下优先级处理：

1. **MSG_MORE 路径**: 若 flags 包含 `MSG_MORE`，数据追加到 `msg_more_buf`，立即返回。非 `MSG_MORE` 时若 `msg_more_buf` 非空则合并后清空。
2. **本地环路优化**: `try_deliver_local()` 检查目标地址是否为本地地址，是则走回环路径。
3. **smoltcp 发送**: 通过 `udp_routed_socket()` 调用 `socket.send_slice()`。`can_send()` 为 `false` 或缓冲区满时返回 `EAGAIN`。

`try_sendmsg` 在 smoltcp 发送前执行 `route_check()` 验证目标地址可达，并根据 `bound_ifindex` 或 `route_output()` 结果调用 `NET_INTERFACE.rebind_routed_udp()` 确保 socket 绑定到正确接口。

## 接收路径

### try_recv / try_recvmsg

从 `inner.rx_queue` 弹出首个数据包，拷贝到用户缓冲区，返回字节数和源地址。队列为空直接返回 `EAGAIN`，不做 poll。

### socket_r_ready

只异步请求 CPU0 poll worker，再检查已发布的 `rx_queue`；零超时和非阻塞 syscall 首试由调用层等待内部 ticket，不在 readiness scan 内同步 poll。

## 本地环路优化

`try_deliver_local()` 旁路 smoltcp 协议栈，在 OS 层直接将数据推入目标 socket 的 `rx_queue`：

```rust
fn try_deliver_local(&self, remote: IpEndpoint, data: &[u8])
    -> Result<Option<isize>, SyscallErr>
```

判定流程：

1. `is_local_udp_destination()`: 目标地址是回环、unspecified 或本地接口地址时触发。
2. `local_source_endpoint()`: 获取发送端本地地址端口。
3. `find_local_udp_recipient()`: 按 scoring 机制匹配最优接收 socket：
   - 地址精确匹配（addr == remote.addr）得 2 分，通配（unspecified）得 1 分
   - 对端精确匹配（已 connect 到 src）得 2 分，未 connect 得 1 分
   - 总分最高者胜出

匹配成功后直接将数据 push 到目标 socket 的 `rx_queue`，唤醒 `recv_waiters`。

## dispatch_udp_packets

`dispatch_udp_packets()` 在 smoltcp poll 后调用，从 `SocketSet` 的 UDP socket 中抽取数据包并分发到 OS 层：

```
for each smoltcp socket:
  if udp && can_recv():
    while can_recv():
      recv() -> (data, meta)
      find_best_match(&os_socks, local, remote) -> UdpSocket
      push to rx_queue
      notify recv_waiters
```

`find_best_match()` 的评分逻辑：

| 条件 | 得分 | 说明 |
|------|------|------|
| remote_endpoint == 收到的远端 | 2 | 精确匹配，专属 socket |
| remote_endpoint != 收到的远端 | 0 | connect 到其他地址，不匹配 |
| remote_endpoint == None | 1 | 未 connect，作为备胎接收 |

## MSG_MORE

`MSG_MORE` 标记指示内核暂不发送当前数据，等待更多数据一次发出。实现分两个路径：

- `try_send`（已 connect socket）：`MSG_MORE` 时数据追加到 `msg_more_buf`；非 `MSG_MORE` 时合并缓冲区后发送。
- `try_sendmsg`（通用 sendto/sendmsg）：逻辑相同，额外支持 sender 参数指定目标地址。

合并后的缓冲区通过 `send_slice` 一次发送给 smoltcp。

## 全局跟踪

```rust
pub static UDP_SOCKETS: Mutex<Vec<Weak<UdpSocket>>>;
pub static UDP_SOCKETS_TO_REMOVE: Mutex<Vec<RouteSocketHandle>>;
```

- `UDP_SOCKETS`: 所有存活 `UdpSocket` 的弱引用集合。`dispatch_udp_packets` 遍历此表分发数据包。每次 dispatch 时清理已失效的弱引用。
- `UDP_SOCKETS_TO_REMOVE`: `Drop` 时推入待移除的 smoltcp socket 句柄，由 `NET_INTERFACE` 的统一清理逻辑异步移除。

## Socket 选项

| 选项 | 方法 | 说明 |
|------|------|------|
| SO_REUSEADDR | `set_reuse_addr()` | 允许地址重用，双方均设置时才生效 |
| SO_BINDTODEVICE | `set_bind_to_device()` | 绑定 socket 到指定网络接口 |
| SO_RCVBUF | `set_recv_buf_size()` | 设置接收缓冲区上限（`rx_queue` 条目数限制） |
| SO_SNDBUF | `set_send_buf_size()` | 设置发送缓冲区大小 |
| IPV6_CHECKSUM | `set_ipv6_checksum()` | IPv6 伪头校验和偏移量 |
| IP_MULTICAST_JOIN | `join_multicast_group()` | 加入多播组 |
| IP_MULTICAST_LEAVE | `leave_multicast_group()` | 离开多播组 |

`SO_BROADCAST` 隐式支持，smoltcp 默认允许广播发送。UDP 不支持 TCP_NODELAY、TCP_KEEPALIVE 等 TCP 专用选项，调用返回 `EOPNOTSUPP`。

## 测试映射

UDP 没有独立的 LTP 测试套件。功能验证通过以下间接方式覆盖：

| 方式 | 内容 |
|------|------|
| LTP socket 基础 | `socket()`、`bind()`、`connect()` 的 AF_INET/SOCK_DGRAM 组合 |
| busybox 网络工具 | `udhcpc` DHCP 客户端（使用 UDP） |
| libctest | POSIX socket API 兼容性 |
| 手动 QEMU 测试 | 使用 `iperf -u`、`netperf -t UDP_STREAM` 验证吞吐和正确性 |

## 已知问题

1. **recvmsg 不返回辅助数据**：`IP_PKTINFO`、`IP_RECVORIGDSTADDR` 等辅助数据未实现。`try_recvmsg` 仅返回 payload 和源地址，不填充 `msg_control`。
2. **`send_to()` 未实现**：Socket trait 的 `send_to()` 方法声明了 `todo!()`，实际 sendto/sendmsg 路径走 `try_sendmsg`。
3. **IPv6 支持不完整**：`join_multicast_group` 和 `leave_multicast_group` 未区分 IPv4/IPv6 多播模型。
4. **smoltcp 缓冲区满丢失**：`dispatch_udp_packets` 抽干 smoltcp socket 时若目标 `UdpSocket` 的 `rx_queue` 队列满，数据丢弃（`push_back` 不做溢出检查）。
5. **Drop 路径异步**：`UdpSocket` 的 `Drop` 不直接关闭 smoltcp socket，委托给 `NET_INTERFACE` 的统一清理，存在窗口期。
