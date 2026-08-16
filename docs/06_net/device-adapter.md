---
title: "设备适配层 (Device Adapter)"
module: os/src/net/adapter.rs
category: net
status: draft
owner: MangoCore Team
last_updated: "2026-08-15"
code_paths:
  - "os/src/net/adapter.rs"
  - "os/src/drivers/net/mod.rs"
  - "os/src/drivers/net/virtio_net.rs"
  - "os/src/drivers/net/veth.rs"
entry_points:
  - "IfaceDevice"
  - "SmoltcpDeviceAdapter"
  - "NetDevice trait"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "socket01"
    - "sendto01"
    - "recvfrom01"
  oscomp:
    - "iperf"
    - "netperf"
related_docs:
  - "docs/06_net/architecture.md"
  - "docs/06_net/device-stack-and-poll.md"
  - "docs/06_net/net-core-iface.md"
---

## 概述

设备适配层是 smoltcp 协议栈与底层物理/虚拟网卡驱动之间的桥梁。它提供统一的抽象，将内核驱动的简单收发接口（`NetDevice` trait）映射为 smoltcp 要求的 token 模式（`Device` trait），并支持环回、virtio-net、veth 三种设备类型。

核心源码位于 `os/src/net/adapter.rs`，驱动的 trait 定义位于 `os/src/drivers/net/mod.rs`。

## 设计目标

- **解耦协议栈与硬件**：smoltcp 通过 `Device` trait 操作设备，内核驱动通过 `NetDevice` trait 暴露接口，适配层是两者之间的单向映射
- **多设备支持**：通过 `IfaceDevice` 枚举统一管理 lo、eth、veth，每个设备拥有独立的 smoltcp Interface
- **零开销抽象**：枚举分发消除 trait 对象的虚表开销，同时规避 smoltcp `Device` trait 的 GAT object-safety 限制
- **容错启动**：`NullNetDevice` 保证无物理网卡时协议栈仍可正常工作（仅限环回）

### VirtIO-MMIO FDT 探测约束

FDT 的 `virtio,mmio` 节点可同时包含块与网络设备。探测器必须在设备初始化前按
`DeviceType` 过滤；对未匹配设备创建的只读 transport 不能触发其析构重置，否则后续
跨类型枚举会破坏已初始化设备的 virtqueue。

## 架构

```
+------------------------------------------------------------------+
|                smoltcp Interface (per-device stack)              |
+------------------------------------------------------------------+
|       IfaceDevice (enum: Lo / Eth / Veth)                       |
|         implements smoltcp::phy::Device                          |
|         iface_device.receive() -> (IfaceRxToken, IfaceTxToken)  |
|         iface_device.transmit() -> IfaceTxToken                 |
+------------------------------------------------------------------+
|  SmoltcpDeviceAdapter   |  Loopback (smoltcp)  |  VethDriver    |
|  (wraps Arc<dyn NetDevice>)                    |  (wraps Veth)  |
+------------------------------------------------------------------+
|       NetDevice trait (receive / transmit / mac_address)        |
+------------------------------------------------------------------+
|  VirtIONetWrapper  |  NullNetDevice    |  (future drivers)       |
+------------------------------------------------------------------+
```

### 分层说明

| 层 | 文件 | 角色 |
|----|------|------|
| `IfaceDevice` | `os/src/net/adapter.rs` | 顶层枚举，向 smoltcp Interface 暴露统一的 `Device` impl |
| `SmoltcpDeviceAdapter` | `os/src/net/adapter.rs` | 将 `NetDevice` 包装为 smoltcp `Device`，实现 token 模式的转换 |
| `NetDevice` trait | `os/src/drivers/net/mod.rs` | 内核驱动的标准接口，简洁的 `receive`/`transmit`/`mac_address` |
| `VirtIONetWrapper` | `os/src/drivers/net/virtio_net.rs` | virtio-net 设备的具体驱动实现（MMIO 和 PCI 两种传输层） |
| `NullNetDevice` | `os/src/net/adapter.rs` | 无物理网卡时的空设备，`transmit` 静默丢包 |
| `VethDriver` | `os/src/drivers/net/veth.rs` | 虚拟以太网对的 smoltcp `Device` 实现 |

### VirtIO 设备发现

RV64 上的块设备和网络设备优先从 `platform_info().devices` 构造的
`DeviceManager` 查询 `virtio,mmio` transport 条目。FDT 条目按 MMIO 基址升序
排列，驱动逐项探测并由 VirtIO 设备头决定块设备或网络设备类型；该目录缺失或
无可响应设备时则继续使用原有的固定 MMIO 地址探测。LA64 的 virtio PCI 枚举和
2K1000 GMAC 初始化路径不变。

### RV64 virtio-net IRQ 链路

RV64 的 virtio-net 驱动将设备 callback 注册到 PLIC source。external IRQ handler
只完成 PLIC claim/dispatch/complete，并让驱动发布网络 deferred work；不会在 hard IRQ
中调用 smoltcp 或 scheduler。CPU0 随后的 task/idle 安全点运行
`run_deferred_external_work()`，再由既有 poll/wakeup 路径推进接收队列并唤醒阻塞
`recvfrom`。调度 tick 的网络 fallback 仅保留为测试可控的后备路径，不能替代真实
virtio-net IRQ。无 virtio-net 的默认 ktest 配置会跳过该 focused 场景。

## 核心数据结构

### `IfaceDevice` 枚举

```rust
pub enum IfaceDevice {
    Lo(Loopback),
    Eth(SmoltcpDeviceAdapter),
    Veth(VethDriver),
}
```

`IfaceDevice` 是设备适配层的入口，作为 smoltcp `Interface` 持有的设备对象。它使用枚举而非 trait 对象，原因在于 smoltcp 的 `Device` trait 包含通用关联类型（GAT）—— `type RxToken<'a>` 和 `type TxToken<'a>` —— 这些 GAT 导致 `Device` 不是 object-safe，无法使用 `Box<dyn Device>`。

每个变体对应一种设备类型：

- **`Lo`**：原生 smoltcp `Loopback`，无外部依赖，用于环回通信
- **`Eth`**：`SmoltcpDeviceAdapter` 包装的物理网卡（virtio-net 等）
- **`Veth`**：`VethDriver` 包装的虚拟以太网对，通过内存队列连接 peer

### `IfaceRxToken` / `IfaceTxToken`

```rust
pub enum IfaceRxToken<'a> {
    Lo(<Loopback as Device>::RxToken<'a>),
    Eth(<SmoltcpDeviceAdapter as Device>::RxToken<'a>),
    Veth(<VethDriver as Device>::RxToken<'a>),
}

pub enum IfaceTxToken<'a> {
    Lo(<Loopback as Device>::TxToken<'a>),
    Eth(<SmoltcpDeviceAdapter as Device>::TxToken<'a>),
    Veth(<VethDriver as Device>::TxToken<'a>),
}
```

这两个枚举是对应 `IfaceDevice` 的 token 类型。它们分别实现 smoltcp 的 `RxToken` 和 `TxToken` trait，在 `consume` 方法中通过 `match` 将调用委托给内部的具体 token。

### `SmoltcpDeviceAdapter`

```rust
pub struct SmoltcpDeviceAdapter {
    pub inner: Arc<dyn NetDevice>,
}
```

核心适配器结构。它持有 `Arc<dyn NetDevice>`（一个线程安全的驱动引用），实现 `Device` trait 完成类型转换：

- **`receive`**：分配 2048 字节栈上缓冲区，调用 `inner.receive(&mut buf)`，成功后将数据打包为 `NetRxToken`
- **`transmit`**：直接返回 `NetTxToken { inner: self.inner.clone() }`，保证每次发送都有可用的 token
- **`capabilities`**：固定返回 `max_transmission_unit = 1500`，`Medium::Ethernet`

### `NetRxToken` / `NetTxToken`

```rust
pub struct NetRxToken {
    buf: Vec<u8>,
}

pub struct NetTxToken {
    inner: Arc<dyn NetDevice>,
}
```

Token 是 smoltcp 的数据收发原语。smoltcp 在 `receive()` 返回 token 对后，会在适当时机调用 `RxToken::consume()` 和 `TxToken::consume()`：

- **`NetRxToken::consume`**：将内部 `Vec<u8>` 暴露给 smoltcp 处理，同时通过 `CURRENT_POLL_IFINDEX` 捕获 ARP 回复
- **`NetTxToken::consume`**：从 smoltcp 接收待发送数据，调用 `inner.transmit(&buf)` 将数据下推到驱动
- **空包保护**：`NetTxToken` 在 `len == 0` 时拦截并记录警告，防止驱动层处理零长度包

### `NetDevice` trait

```rust
pub trait NetDevice: Send + Sync {
    fn receive(&self, buf: &mut [u8]) -> Option<usize>;
    fn transmit(&self, buf: &[u8]);
    fn mac_address(&self) -> [u8; 6];
}
```

这是内核驱动层的最小接口，远比 smoltcp 的 `Device` trait 简单。三个方法分别对应收包、发包和 MAC 地址查询。

### `NullNetDevice`

```rust
pub struct NullNetDevice;
```

无物理网卡时的占位设备。`receive` 永远返回 `None`，`transmit` 静默丢弃。MAC 地址固定为 `02:00:00:00:00:01`（本地管理单播地址），这是 smoltcp DHCP 客户端的要求——全零 MAC 会导致 DHCP 拒绝。

### `RoutingDevice`（已弃用）

```rust
pub struct RoutingDevice {
    pub eth: SmoltcpDeviceAdapter,
    pub lo: Loopback,
    pub hw_addr: EthernetAddress,
}
```

多设备软件交换机，在 `IfaceDevice` 模型引入前负责在 eth 和 lo 之间路由数据包。它在 `transmit` 时解析以太网帧的目标 MAC 和 IP，决定走环回还是物理网卡。此结构已在新架构中由每个设备独立的 smoltcp `Interface` 替代，保留在代码中仅用于兼容过渡。

## 关键流程

### 数据接收

```
IfaceDevice::receive(timestamp)
  -> match device variant:
       Lo  -> Loopback::receive(timestamp) -> (LoRxToken, LoTxToken)
       Eth -> SmoltcpDeviceAdapter::receive(timestamp)
                -> NetDevice::receive(&mut buf) -> Option<len>
                -> NetRxToken { buf: packet }
                -> NetTxToken { inner: device.clone() }
       Veth -> VethDriver::receive(timestamp)
                -> Veth::rx_queue.pop_front() -> VethRxToken
```

### 数据发送

```
IfaceDevice::transmit(timestamp)
  -> match device variant:
       Lo  -> Loopback::transmit(timestamp) -> LoTxToken
       Eth -> SmoltcpDeviceAdapter::transmit(timestamp)
                -> NetTxToken { inner: self.inner.clone() }
                -> [later] NetTxToken::consume(len, f)
                   -> f(&mut buf)  // smoltcp fills buffer
                   -> NetDevice::transmit(&buf)
       Veth -> VethDriver::transmit(timestamp)
                -> VethTxToken { peer_veth }
                -> [later] VethTxToken::consume(len, f)
                   -> push_back to peer's rx_queue
```

## smoltcp Device trait 集成

smoltcp 的 `Device` trait 使用 token 模式设计：

```rust
trait Device {
    type RxToken<'a>: RxToken where Self: 'a;
    type TxToken<'a>: TxToken where Self: 'a;

    fn receive(&mut self, timestamp: Instant)
        -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)>;
    fn transmit(&mut self, timestamp: Instant)
        -> Option<Self::TxToken<'_>>;
    fn capabilities(&self) -> DeviceCapabilities;
}
```

适配层的核心工作是维护从 token 到实际硬件的映射。`SmoltcpDeviceAdapter` 在 `receive` 时调用 `NetDevice::receive` 获取原始数据，将其封装进 `NetRxToken`；在 `consume` 时，`NetTxToken` 反向调用 `NetDevice::transmit` 将数据发回驱动。`VethDriver` 的映射类似，但 tx 的终点是 peer 的内存队列而非硬件。

## 测试映射

| 特性 | 覆盖范围 | LTP / OSCOMP 用例 | 状态 |
|------|---------|-------------------|------|
| 环回设备 (Lo) | smoltcp Loopback 内部 | 隐式覆盖于全部 socket 测试 | stable |
| virtio-net 收发 (Eth) | VirtIONetWrapper 的 receive/transmit | iperf, netperf, socket01 | stable |
| veth 对收发 | VethDriver/Veth 的内存队列 | ip link add veth 系统测试 | stable |
| NullNetDevice 空设备 | 无网卡时的启动路径 | 手动验证 | stable |

## 已知问题

1. **`SmoltcpDeviceAdapter::receive` 栈上缓冲区大小固定**
   当前使用 2048 字节栈上缓冲区，超过该长度的巨型帧会被截断。实际环境中以太网 MTU 通常为 1500 字节，此限制在 QEMU 环境下不会触发，但若未来支持巨型帧（jumbo frame）需改用堆分配或可调缓冲区。

2. **`NetTxToken` 每次发送分配新 `Vec`**
   每次 `consume` 调用都会 `vec![0u8; len]` 分配堆内存，对于高吞吐场景可能引入分配器压力。无损路径的零拷贝优化（直接传递 smoltcp 的内部缓冲区引用）需要改造 `NetDevice` trait 签名。

3. **`NullNetDevice` 的 MAC 地址硬编码**
   `02:00:00:00:00:01` 是为满足 smoltcp DHCP 客户端而硬编码的。若未来同时存在多个 null 设备场景（不可能，因为 `NullNetDevice` 仅用做全局 fallback），则需要动态生成 MAC。

4. **GAT object-safety 限制**
   因 smoltcp `Device` trait 包含 GAT（`type RxToken<'a>`），无法使用 `Box<dyn Device>` 实现动态分发，必须使用枚举。这意味着新增设备类型必须修改 `IfaceDevice`、`IfaceRxToken`、`IfaceTxToken` 三处枚举定义及所有 `match` 分支。
