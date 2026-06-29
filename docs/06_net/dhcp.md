---
title: "DHCP 初始化流程 (DHCP Probe Flow)"
module: "os/src/net/config.rs (+ net_core.rs: ETH0_CIDR, DEFAULT_GW)"
category: net
status: draft
owner: MangoCore Team
last_updated: "2026-06-29"
code_paths:
  - "os/src/net/config.rs"
  - "os/src/net/net_core.rs"
entry_points:
  - "NetInterfaceInner::new() — DHCP 探测的启动入口"
  - "net_core::set_eth0_ipv4() — DHCP 完成后写入网段"
  - "net_core::set_default_gateway() — DHCP 完成后写入网关"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "sendto01, connect01 (依赖 DHCP 获取 IP)"
  oscomp:
    - "basic (ping), busybox (ifconfig/route), iperf"
related_docs:
  - "docs/06_net/architecture.md"
  - "docs/06_net/device-stack-and-poll.md"
  - "docs/06_net/device-adapter.md"
  - "docs/06_net/routing.md"
---

# DHCP 初始化流程 (DHCP Probe Flow)

## 1. 概述

DHCP 探测是 MangoCore 网络栈初始化期间的关键步骤。它发生在 `NetInterfaceInner::new()` 中，位于 eth0 设备栈的构建阶段。当物理 NIC（virtio-net）存在时，内核会创建一个 smoltcp `dhcpv4::Socket`，通过阻塞式轮询循环向网络中的 DHCP 服务器请求 IPv4 地址和默认网关。如果没有检测到 NIC，则使用 `NullNetDevice` 回退，整个 DHCP 流程被跳过。

本流程完全在启动初始化路径上同步执行，不依赖后续的运行时 poll 循环。DHCP 完成后 socket 会被移除，不再参与运行时轮询。

## 2. DHCP 探测详细流程

### 2.1 触发条件

在 `config.rs::init()` 中，`NET_DEVICE.lock()` 检查是否存在已注册的物理网卡:

```rust
let has_nic = NET_DEVICE.lock().is_some();
net_core::init();
NET_INTERFACE.init();
```

- `has_nic == true`: 存在 virtio-net 设备，后续 `NetInterfaceInner::new()` 执行 DHCP
- `has_nic == false`: 无网卡，eth0 回退到 `NullNetDevice`，跳过 DHCP

### 2.2 DHCP Socket 创建

在 eth0（ifindex=2）的设备栈构造过程中:

```rust
if has_real_nic {
    let mut dhcp_socket = dhcpv4::Socket::new();
    dhcp_socket.set_retry_config(dhcpv4::RetryConfig {
        discover_timeout: Duration::from_secs(2),
        initial_request_timeout: Duration::from_secs(1),
        request_retries: 3,
        min_renew_timeout: Duration::from_secs(60),
        ..dhcpv4::RetryConfig::default()
    });
    let dhcp_handle = eth_sockets.add(dhcp_socket);
    // ...sync polling loop...
}
```

各参数的语义:

| 参数 | 值 | 说明 |
|------|-----|------|
| `discover_timeout` | 2s | DHCP DISCOVER 超时 |
| `initial_request_timeout` | 1s | DHCP REQUEST 超时 |
| `request_retries` | 3 | 最大 REQUEST 重试次数 |
| `min_renew_timeout` | 60s | 最小续租间隔（运行时用） |

### 2.3 同步轮询循环

DHCP socket 添加后，内核进入一个**阻塞式同步轮询循环**，最多等待 5 秒:

```rust
let deadline = Instant::from_millis(
    current_time_duration().as_millis() as i64 + 5000,
);

loop {
    let timestamp = Instant::from_millis(current_time_duration().as_millis() as i64);
    *CURRENT_POLL_IFINDEX.lock() = 2;
    eth_iface.poll(timestamp, &mut eth_device, &mut eth_sockets);

    let event = eth_sockets.get_mut::<dhcpv4::Socket>(dhcp_handle).poll();
    match event {
        Some(dhcpv4::Event::Configured(cfg)) => { /* 见 2.4 */ }
        Some(dhcpv4::Event::Deconfigured) => {}
        None => {}
    }

    if timestamp >= deadline {
        log::info!("[net::config] DHCP timeout, continuing without IP");
        break;
    }
}
```

关键点:

- 循环体内每次迭代先设置 `CURRENT_POLL_IFINDEX` 为 eth0（ifindex=2），确保 ARP 邻居表标记正确的接口
- 每轮调用 smoltcp `Interface::poll()` 驱动协议栈
- 调用 `dhcpv4::Socket::poll()` 获取 DHCP 事件
- `Deconfigured` 事件被静默忽略
- 超时后直接 break，不重试

### 2.4 `Event::Configured` 处理

收到 DHCP 服务器的配置后:

```rust
net_core::set_eth0_ipv4(IpCidr::Ipv4(cfg.address));
net_core::set_default_gateway(cfg.router);
```

`set_eth0_ipv4()` 负责:
1. 写入全局 `ETH0_CIDR`（`Mutex<Option<IpCidr>>`）
2. 将网段写入当前 netns 的 eth0 设备条目（清空旧地址后添加新地址）

`set_default_gateway()` 负责:
1. 写入全局 `DEFAULT_GW`（`Mutex<Option<Ipv4Address>>`）

### 2.5 路由注入

DHCP socket 在退出循环后立即被移除:

```rust
eth_sockets.remove(dhcp_handle);
```

随后从 `net_core` 读取 DHCP 写入的地址和网关:

```rust
// 从 netns 设备列表读取 eth0 的 IP 地址
let addrs_src: Vec<IpCidr> = { /* 过滤 ifindex==2 的地址 */ };
if !addrs_src.is_empty() {
    eth_iface.update_ip_addrs(|addrs| {
        for cidr in &addrs_src {
            addrs.push(*cidr).unwrap();
        }
    });
}

// 注入默认 IPv4 路由
if let Some(gw) = net_core::default_gateway() {
    eth_iface.routes_mut().add_default_ipv4_route(gw).unwrap();
}
```

这一步确保 smoltcp 的 `Interface` 内部路由表与 `net_core` 的全局状态一致。

### 2.6 超时回退

5 秒超时后的行为:

- DHCP socket 被移除（`eth_sockets.remove(dhcp_handle)`）
- `addrs_src` 为空，eth0 的 smoltcp Interface 不注入任何 IP
- `default_gateway()` 返回 `None`，不添加默认路由
- eth0 设备栈完成初始化，但**不具有 IP 连通性**
- 后续运行时轮询不会自动重试 DHCP

## 3. NullNetDevice 回退

当 `NET_DEVICE.lock().take()` 返回 `None` 时（无物理 NIC）:

```rust
let null_dev = Arc::new(NullNetDevice);
let null_mac = [0x02u8, 0, 0, 0, 0, 1];
(SmoltcpDeviceAdapter::new(null_dev), EthernetAddress(null_mac), false)
```

`NullNetDevice` 的行为:

- `receive()`: 始终返回 `None`（无数据可收）
- `transmit()`: 无操作，数据包静默丢弃
- `mac_address()`: 返回 `02:00:00:00:00:01`（本地管理单播 MAC）

此时 `has_real_nic == false`，整个 DHCP 流程被跳过。内核输出日志 `"[kernel] net interface initialized (loopback only, no NIC)"`。

## 4. 核心全局变量

定义在 `net_core.rs` 中，由 DHCP 流程写入:

```rust
lazy_static! {
    pub static ref ETH0_CIDR: Mutex<Option<IpCidr>> = Mutex::new(None);
    pub static ref DEFAULT_GW: Mutex<Option<Ipv4Address>> = Mutex::new(None);
}
```

- `ETH0_CIDR`: DHCP 分配的 IPv4 网段，供 `net_core::eth0_ipv4_cidr()` 读取
- `DEFAULT_GW`: 默认网关，供 `net_core::default_gateway()` 读取

## 5. 已知限制

1. **无 DHCP 重试**: 超时后不会在运行时重试 DHCP。仅在 `NetInterfaceInner::new()` 初始化时执行一次。如果启动时 DHCP 服务器不可用，eth0 将永久无 IP 地址。
2. **无 SLAAC**: 不支持 IPv6 无状态地址自动配置。仅支持 smoltcp 的 DHCPv4。
3. **单次探测**: DHCP socket 在完成或超时后立即从 SocketSet 中移除，后续运行时轮询不涉及 DHCP。
4. **无租约续期**: `min_renew_timeout` 配置存在但未被运行时使用，因为 DHCP socket 在初始化后已被移除。

## 6. 测试映射

| 测试 | 覆盖内容 | 验证方式 |
|------|---------|---------|
| ping（basic） | DHCP 获取 IP 后 ICMP 可达 | QEMU 启动后 ping 网关 |
| ifconfig（busybox） | `ETH0_CIDR` 正确写入设备列表 | `ifconfig eth0` 显示非零 IP |
| route（busybox） | `DEFAULT_GW` 写入路由表 | `route -n` 显示默认网关 |
| sendto01（LTP） | DHCP 后 UDP 收发 | LTP sendto 测试 |
| iperf / netperf | TCP 吞吐量，依赖 DHCP 分配 IP | iperf 客户端-服务器模式 |

## 7. 初始化顺序

DHCP 在整个网络栈初始化序列中的位置:

```
net::config::init()
  ├── NET_DEVICE.lock() 检查 NIC 是否存在
  ├── net_core::init()  注册 lo（ifindex=1）和 eth0（ifindex=2）
  └── NET_INTERFACE.init()
        └── NetInterfaceInner::new()
              ├── lo 设备栈（ifindex=1，127.0.0.1/8）
              └── eth0 设备栈（ifindex=2）
                    ├── DHCP 同步探测（仅当 has_real_nic == true）
                    │     ├── 创建 dhcpv4::Socket
                    │     ├── 同步轮询（5s 超时）
                    │     ├── Event::Configured → set_eth0_ipv4 + set_default_gateway
                    │     └── 移除 dhcpv4::Socket
                    ├── 注入地址和路由到 smoltcp Interface
                    └── 推入 stacks 列表
```
