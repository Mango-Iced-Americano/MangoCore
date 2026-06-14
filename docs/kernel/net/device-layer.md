# 设备适配层

> 文件: `os/src/net/adapter.rs` (370+ lines)
> 相关: `os/src/drivers/net/mod.rs`, `os/src/drivers/net/virtio_net.rs`

## 组件

### IfaceDevice

```rust
pub enum IfaceDevice {
    Lo(Loopback),                    // smoltcp 内置回环设备
    Eth(SmoltcpDeviceAdapter),       // 物理 NIC 适配器
}
```

实现 smoltcp `Device` trait, 通过 delegating RxToken/TxToken 枚举分发到具体设备。

**为什么用 enum 而非 trait object**: smoltcp 的 `Device` trait 有泛型关联类型 (GAT):
```rust
pub trait Device {
    type RxToken<'a>: RxToken;
    type TxToken<'a>: TxToken;
    // ...
}
```
GAT 使 `Device` 不是对象安全的 (object-safe), 不能用 `Box<dyn Device>`. 因此用带 `Lo` 和 `Eth` 变体的 enum 包装两个具体类型。

### SmoltcpDeviceAdapter

```rust
pub struct SmoltcpDeviceAdapter {
    pub inner: Arc<dyn NetDevice>,  // 驱动层 NetDevice trait object
}

impl Device for SmoltcpDeviceAdapter { /* ... */ }
```

将内核的 `NetDevice` trait (驱动接口) 封装为 smoltcp 的 `Device` trait。
- `receive()`: 从 `NetDevice::receive(&mut buf)` 读包, 返回 `NetRxToken`
- `transmit()`: 返回 `NetTxToken`, consume 时调用 `NetDevice::transmit(&buf)`

### NullNetDevice

```rust
pub struct NullNetDevice;

impl NetDevice for NullNetDevice {
    fn receive(&self, _buf: &mut [u8]) -> Option<usize> { None }
    fn transmit(&self, _buf: &[u8]) { /* no-op */ }
    fn mac_address(&self) -> [u8; 6] { [0x02, 0, 0, 0, 0, 1] }
}
```

无物理 NIC 时的空设备。`transmit` 静默丢弃, `receive` 永远返回 `None`。MAC 地址为本地管理单播地址 (LAA), smoltcp DHCP 需要非零 MAC。

### RoutingDevice (已废弃)

原 `RoutingDevice` 将 lo 和 eth 合并为单个 smoltcp `Device`, 在 `TxToken::consume()` 中检查以太网帧手动路由:
```rust
// 已废弃的逻辑:
if dst_mac == hw_addr → send_to_lo
if broadcast → send_to_lo + send_to_eth
if IPv4 + is_local(dst_ip) → send_to_lo (override)
```

Phase 5 后被 `IfaceDevice` 取代, 每设备独立 `DeviceStack`。

## 驱动层

### NetDevice trait

```rust
// drivers/net/mod.rs
pub trait NetDevice: Send + Sync {
    fn receive(&self, buf: &mut [u8]) -> Option<usize>;
    fn transmit(&self, buf: &[u8]);
    fn mac_address(&self) -> [u8; 6];
}

pub static NET_DEVICE: Mutex<Option<Arc<dyn NetDevice>>>;
```

### VirtIONetWrapper

```rust
// drivers/net/virtio_net.rs
pub fn new(/* ... */) -> Option<Self>
```

两个变体 (MMIO + PCI):
- MMIO: `virtio-drivers` crate 的 `VirtIOHeader` + mmio 传输
- PCI: `virtio-drivers` crate 的 PCI transport

`new()` 返回 `Option<Self>` — 初始化失败时不 panic。

### 初始化流程

```
drivers::init()
  → init_net_device()
    → NET_DEVICE = if let Some(nic) = VirtIONetWrapper::new() { Some(Arc::new(nic)) }
    // 或者保持 None (无 NIC)

config.rs::NetInterfaceInner::new()
  → NET_DEVICE.lock().take()
    → Some(net) → SmoltcpDeviceAdapter::new(net) → IfaceDevice::Eth
    → None      → SmoltcpDeviceAdapter::new(Arc<NullNetDevice>) → IfaceDevice::Eth
  → Lo 栈: IfaceDevice::Lo(Loopback::new(Medium::Ip))
```

## 未来扩展

- **多 NIC 支持**: `NET_DEVICES: Vec<Arc<dyn NetDevice>>`, 每个 NIC 创建独立 `DeviceStack`
- **Bridge**: 虚拟桥接设备, 连接多个物理接口 (参考 DragonOS `bridge.rs`)
- **veth**: 虚拟以太网对, 用于容器网络 (参考 DragonOS `veth.rs`)
