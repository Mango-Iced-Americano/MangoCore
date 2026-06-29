---
title: "Packet Socket (AF_PACKET) 实现"
module: "net/socket/packet"
category: net
status: draft
owner: MangoCore Team
last_updated: 2026-06-29
code_paths:
  - "os/src/net/socket/packet.rs"
entry_points:
  - PacketSocket
  - deliver_frame_to_packet_sockets
  - PACKET_SOCKETS
  - ETH_P_ALL
arch:
  rv64: supported
  la64: supported
tests:
  ltp: "N/A: no dedicated LTP test"
  oscomp: "N/A: indirectly covered by basic networking"
related_docs:
  - docs/06_net/architecture.md
  - docs/06_net/syscall-layer.md
  - docs/06_net/device-adapter.md
---

# Packet Socket (AF_PACKET) 实现

## 概述

Packet socket (AF_PACKET) 允许用户态程序直接收发原始链路层帧，绕过 smoltcp 协议栈的传输层处理。支持 SOCK_RAW 和 SOCK_DGRAM 两种创建类型，当前均使用相同的实现路径（完整帧收发），未区分链路层头部的剥离与否。

## 数据结构

### PacketSocket

```rust
pub struct PacketSocket {
    pub inner: Mutex<PacketSocketInner>,
    recv_waiters: EventWaitQueue,
    send_waiters: EventWaitQueue,
}
```

通过标准 Socket trait 接口暴露给用户态。`recv_waiters` 和 `send_waiters` 用于 epoll 事件集成。

### PacketSocketInner

```rust
pub struct PacketSocketInner {
    pub bound_ifindex: u32,
    pub bound_protocol: u16,
    pub rx_queue: VecDeque<Vec<u8>>,
    pub recvbuf_size: usize,
    pub sendbuf_size: usize,
}
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `bound_ifindex` | u32 | 绑定接口索引，0 匹配所有接口 |
| `bound_protocol` | u16 | 协议过滤，`ETH_P_ALL` 匹配所有 |
| `rx_queue` | VecDeque<Vec\<u8\>> | 接收帧队列 |
| `recvbuf_size` | usize | 接收缓冲区上限 (65536) |
| `sendbuf_size` | usize | 发送缓冲区上限 (65536) |

## 协议常量

```rust
pub const ETH_P_ALL: u16 = 0x0003;
pub const ETH_P_ARP: u16 = 0x0806;
pub const ETH_P_IP: u16  = 0x0800;
```

`ETH_P_ALL` 表示捕获所有以太网协议类型，不按 ethertype 过滤。

## 创建与注册

`Socket::alloc()` 中 `AF_PACKET` 接受 `PSOCK::Raw` 和 `PSOCK::Datagram`，均创建 `PacketSocket` 实例并调用 `register_packet_socket()` 将弱引用推入全局 `PACKET_SOCKETS`。

## 接口操作

### bind

```rust
fn bind(&self, endpoint: &Endpoint) -> SyscallRet
```

接收 `Endpoint::Packet(ep)`，设置 `bound_ifindex` 和 `bound_protocol`。`Endpoint::Unspecified` 可将 socket 绑定到任意接口。

### 不支持的操作

| 操作 | 返回值 |
|------|--------|
| listen | `EOPNOTSUPP` |
| connect | `EOPNOTSUPP` |
| accept | `EOPNOTSUPP` |

### try_send

通过设备层的 `transmit()` 直接发送原始帧。根据 `bound_ifindex` 查找目标接口，获取 `tx_token` 后调用 `consume()` 写入设备缓冲区。

`try_sendmsg` 支持通过 `Endpoint::Packet` 的 ifindex 临时覆盖发送接口，适配 `sendto` 指定出接口的场景。

### try_recv

```rust
fn try_recv(&self, buf: &mut [u8]) -> Result<isize, SyscallErr>
```

从 `inner.rx_queue` 弹出首个帧，拷贝到用户缓冲区。队列为空时返回 `EAGAIN`。

### SO_BINDTODEVICE

`set_bind_to_device(ifname)` 通过接口名动态绑定或解绑 socket。空字符串解绑（`bound_ifindex = 0`），有效名称通过 `current_netns().device_list` 查找对应 `nic_id`。接口不存在返回 `ENODEV`。

## 帧分发

### deliver_frame_to_packet_sockets

```rust
pub fn deliver_frame_to_packet_sockets(frame: &[u8], ifindex: u32)
```

核心分发函数，从原始帧中提取 ethertype 并遍历 `PACKET_SOCKETS`：

1. 从 `frame[12..14]` 提取 2 字节 ethertype（网络字节序）
2. 遍历 `PACKET_SOCKETS`，对所有存活 socket 的弱引用执行 `upgrade()`
3. 匹配条件：
   - **接口匹配**: `bound_ifindex == 0`（任意接口）或 `== ifindex`
   - **协议匹配**: `bound_protocol == ETH_P_ALL`（全部）或 `== ethertype`
4. 匹配的 socket 将完整帧推入 `rx_queue`
5. 通知 `recv_waiters` 触发 `EPOLLIN | EPOLLRDNORM` 事件
6. 清理已失效的弱引用（socket 已 drop）

### deliver_frames_from_veth_queue

```rust
pub fn deliver_frames_from_veth_queue(ifindex: u32, rx_queue: &VecDeque<Vec<u8>>)
```

在 `NET_INTERFACE` 的 poll 循环中调用，早于 smoltcp 协议栈对帧的消费。仅对 `IfaceDevice::Veth` 类型的设备生效，将 veth 驱动接收队列中的帧逐个分发。

### 分发时机

在 `try_poll()` 和 `_poll()` 中，帧分发在 smoltcp 协议栈 `iface.poll()` 之前执行，确保 packet socket 优先捕获原始帧。

## 全局跟踪

`PACKET_SOCKETS: Mutex<Vec<Weak<PacketSocket>>>` 定义在 `os/src/net/socket/mod.rs`。`Drop` 实现自动清理已失效的弱引用：

```rust
impl Drop for PacketSocket {
    fn drop(&mut self) {
        PACKET_SOCKETS.lock().retain(|w| w.upgrade().is_some());
    }
}
```

## Epoll 集成

| 方法 | 逻辑 |
|------|------|
| `socket_r_ready()` | `!inner.lock().rx_queue.is_empty()` |
| `socket_w_ready()` | `inner.lock().bound_ifindex != 0` |
| `recv_ready()` | 复用 `socket_r_ready()` |
| `send_ready()` | 复用 `socket_w_ready()` |

## Test Mapping

| 功能 | 覆盖方式 | 状态 |
|------|----------|------|
| 创建 (SOCK_RAW / DGRAM) | OSComp basic / 手动 | pass |
| bind(ETH_P_ALL / IP) | 手动验证 | pass |
| 帧收发 | 手动验证 | pass |
| SO_BINDTODEVICE | 手动验证 | pass |
| LTP 覆盖 | 无直接用例 | N/A |

Packet socket 没有 LTP 测试套件的直接覆盖。功能验证通过 OSComp basic 组的间接链路和手动 QEMU 抓包完成。

## Known Issues

1. **SOCK_RAW 与 SOCK_DGRAM 无区分**
   - `socket_type()` 始终返回 `PSOCK::Raw`，两种创建模式使用相同实现。
   - Linux 行为：SOCK_DGRAM 应剥离链路层头部，只传 L3 载荷。
   - 影响：依赖 SOCK_DGRAM 语义的应用可能行为异常。
   - 修复方向：在 `try_recv` 路径中根据创建时记录的 mode 标志决定是否剥离以太网头。

2. **Veth 专用分发**
   - `deliver_frames_from_veth_queue` 仅对 Veth 设备在 poll 中调用。virtio-net 设备的数据帧不经过此路径，需要额外的分发接入点。
   - 影响：物理网卡 (virtio-net) 的原始帧不会被 packet socket 捕获。
   - 修复方向：在 adapter 的 poll 中为所有设备添加帧分发钩子。

3. **PSOCK::Packet 未实现**
   - 枚举值 `PSOCK::Packet = 10` 对应 Linux 的 `SOCK_PACKET`（已废弃），当前分发中未处理，返回 `EINVAL`。
