---
title: "DeviceStack 与 Polling 基础设施"
module: "config.rs"
category: net
status: draft
owner: "MangoCore Team"
last_updated: "2026-08-02"
code_paths:
  - "os/src/net/config.rs"
  - "os/src/task/processor.rs"
  - "os/src/drivers/net/mod.rs"
  - "os/src/net/socket/inet/stream/mod.rs"
  - "os/src/net/socket/inet/stream/inner.rs"
entry_points:
  - "NET_INTERFACE"
  - "NetInterface::init"
  - "NetInterface::poll"
  - "NetInterface::try_poll"
  - "NetInterface::poll_until_quiescent"
  - "NetInterface::add_routed_socket"
  - "NetInterface::remove_routed"
arch:
  rv64: supported
  la64: supported
tests:
  ltp:
    - "sendto01"
    - "recvfrom01"
    - "socketpair01"
  oscomp:
    - "basic"
    - "busybox"
    - "iperf"
related_docs:
  - "architecture.md"
  - "device-adapter.md"
  - "routing.md"
  - "dhcp.md"
  - "neighbour.md"
  - "net-core-iface.md"
---

## 概述

`config.rs` 是 MangoCore 网络子系统的核心编排模块。它定义了全局单例 `NET_INTERFACE`，将 smoltcp 的 `Interface` 和 `SocketSet` 按网络设备拆分为 `DeviceStack`，并提供统一的轮询（polling）编排和路由式 socket 管理 API。

整个模块围绕三个设计目标展开：

- **每设备独立协议栈**：每个网络设备（lo、eth0、veth）拥有独立的 smoltcp `Interface` 和 `SocketSet`，互不干扰。
- **单核友好轮询**：RV64 网卡 IRQ 只通知任务上下文轮询；定时器和系统调用路径保留轮询 fallback，使用 `try_lock` 防止重入死锁。
- **双层 socket 句柄**：`RouteSocketHandle` 将用户态 socket 与底层 smoltcp `SocketHandle` 解耦，支持跨设备栈迁移。

---

## 核心数据结构

### `NetInterface`

```rust
pub static NET_INTERFACE: NetInterface = NetInterface::new();

pub struct NetInterface<'a> {
    inner: Mutex<Option<NetInterfaceInner<'a>>>,
}
```

全局静态单例，整个网络子系统的入口。`inner` 在 `NetInterface::init()` 调用前为 `None`，确保所有网络操作在初始化完成前返回空结果。

### `NetInterfaceInner`

```rust
pub struct NetInterfaceInner<'a> {
    pub stacks: Vec<DeviceStack<'a>>,
    pub bindings: BTreeMap<RouteSocketHandle, SocketBinding>,
    pub next_socket_id: usize,
}
```

- `stacks`：所有已注册的 DeviceStack 列表。索引 0 为 lo（ifindex=1），索引 1 为 eth0（ifindex=2），随后是动态添加的 veth 栈。
- `bindings`：从 `RouteSocketHandle` 到 `SocketBinding` 的映射，使用 `BTreeMap` 确保有序性。
- `next_socket_id`：单调递增的 `RouteSocketHandle` 计数器。

### `DeviceStack`

```rust
pub struct DeviceStack<'a> {
    pub nic: Arc<dyn Iface>,
    pub device: IfaceDevice,
    pub iface: Interface,
    pub sockets: SocketSet<'a>,
    pub dhcp_handle: Option<SocketHandle>,
}
```

每个 `DeviceStack` 代表一个完整的网络设备协议栈。dhcp_handle 保存需要
续租的常驻 DHCP socket；静态地址、loopback 和 veth 栈均为 None。

| 字段 | 类型 | 职责 |
|------|------|------|
| `nic` | `Arc<dyn Iface>` | 设备元数据接口（名称、MAC、IP、flags）。通过 `Iface` trait 访问。 |
| `device` | `IfaceDevice` | smoltcp 硬件抽象层枚举。支持 `Lo`、`Eth`、`Veth` 三种变体。 |
| `iface` | `smoltcp::Interface` | smoltcp IP 层状态机，负责 IP 收发、ARP 解析、路由查找。 |
| `sockets` | `smoltcp::SocketSet` | 挂载在该设备上的所有 smoltcp socket 集合。 |

---

## 初始化流程

```rust
pub fn init() {
    let has_nic = NET_DEVICE.lock().is_some();
    net_core::init();
    NET_INTERFACE.init();
}
```

`init()` 在 `rust_main()` 启动序列中调用，执行顺序为：

1. `NET_DEVICE.lock()` 检查是否存在 virtio-net 硬件设备。
2. `net_core::init()` 注册 lo（ifindex=1）和 eth0（ifindex=2）到网络命名空间的设备列表。
3. `NET_INTERFACE.init()` 调用 `NetInterfaceInner::new()`，构造两个固定 DeviceStack：
   - lo 栈：`IfaceDevice::Lo(Loopback)`，IP 地址 127.0.0.1/8 + ::1/128。
   - eth0 栈：`IfaceDevice::Eth(SmoltcpDeviceAdapter)`，若存在物理 NIC 则执行 DHCP 探针（5 秒超时），否则使用 `NullNetDevice`。

veth 设备栈通过 `add_veth_stack()` 动态注册，在 `NetInterface::init()` 完成后由 veth 对创建路径调用。

---

## 轮询编排

轮询是单核环境下驱动整个网络协议栈的核心机制。所有 socket 的数据收发、TCP 状态机推进、ARP 解析、UDP 数据分发全部依赖轮询触发。

### `poll()` — 阻塞入口

```rust
pub fn poll(&self) {
    if self.inner.lock().is_none() {
        return;
    }
    self.poll_once(true);
}
```

标准轮询入口，由定时器中断和 syscall 路径调用。内部使用 `lock()` 阻塞等待 `Mutex`，拿到锁后委托 `poll_once()`。

### `try_poll()` — 非阻塞入口

```rust
pub fn try_poll(&self) -> bool {
    let guard = self.inner.try_lock();
    match guard {
        Some(inner) if inner.is_some() => {
            drop(inner);
            self.poll_once(true);
            true
        }
        _ => false,
    }
}
```

**`try_poll()` 使用 `try_lock()` 而非 `lock()`，这是防止死锁的关键设计。**

在单核环境下，场景如下：一个 syscall 处理函数（如 `sys_sendto`）持有 `NET_INTERFACE.inner` 锁并调用 `poll_once()`。如果在 `poll_once()` 执行期间触发中断，而中断处理函数调用 `poll()`，它会尝试获取同一把锁并导致死锁。

`try_poll()` 在锁已被持有时不等待不重试，直接返回 `false`，用于普通任务上下文。
中断上下文不再调用 smoltcp：RV64 PLIC 回调只设定接收 pending 位，调度器在任务
上下文取走该位后调用常规 `poll()`。这既避免重入 `NET_INTERFACE`，也避免中断路径
触及 DHCP、device_list 或 router 锁。

### RV64 接收中断唤醒

RV64 物理网卡的接收路径在“中断通知”和“协议栈推进”之间明确分层：

```
virtio-net / JH7110 GMAC DMA RX interrupt
  -> PLIC claim -> 驱动 NetDevice::interrupt()
  -> 硬件状态 acknowledge + notify_rx_interrupt()
  -> NET_RX_INTERRUPT_PENDING
  -> task::processor 取走 pending 位
  -> NET_INTERFACE.poll() -> smoltcp Interface::poll()
```

PLIC ISR 只完成 claim/complete、驱动级 acknowledge 和原子 pending 通知；它不获取
`NET_INTERFACE` 锁、不运行 smoltcp、也不唤醒或调度任务。`processor` 消费 pending 后
才在正常任务上下文执行 `NET_INTERFACE.poll()`。该标记是合并通知而非每包计数：多个
IRQ 可以合并为一次 poll，随后由 smoltcp/驱动 drain 已就绪 RX 队列。

周期性轮询仍保留，作为丢失通知、非 PLIC 平台和 LA64 路径的 fallback；因此这项改动
缩短 RX 唤醒延迟，但不把协议栈正确性依赖于某一次外部中断。

### `try_poll_stack()` — 单栈非阻塞轮询

```rust
pub fn try_poll_stack(&self, ifindex: u32) -> bool
```

选择性地仅轮询指定 ifindex 的 DeviceStack。适用于仅需推进特定设备（如 veth）的场景。跳过移除列表 drain 和 accept 扫描，只做 smoltcp 协议栈推进和 UDP 分发。这些轻量操作由周期性全量 `poll()` 在空闲循环中补充。

### `poll_once()` — 五阶段核心逻辑

`poll_once()` 是轮询的核心实现，按五个阶段执行：

```
poll_once()
  │
  ├── [阶段 1] 预收集待移除的 socket
  │     ├─ UDP_SOCKETS_TO_REMOVE.drain() → (SocketHandle, ifindex, RouteSocketHandle)
  │     └─ TCP_SOCKETS_TO_REMOVE.drain() → (SocketHandle, ifindex, RouteSocketHandle)
  │
  ├── [阶段 2] 逐 DeviceStack 处理
  │     for each stack in stacks:
  │       a) 设置 CURRENT_POLL_IFINDEX = stack.nic.nic_id()
  │       b) 清除本栈的 UDP socket（直接从 SocketSet 移除 + 清理 bindings）
  │       c) 若是 veth 设备，在 smoltcp 消费前将 rx_queue 帧交付给 packet socket
  │       d) stack.iface.poll(timestamp, device, sockets)
  │       e) 清除本栈的 TCP socket（仅当状态为 Closed 时移除，否则放回 TO_REMOVE）
  │       f) dispatch_udp_packets() 分发 UDP 数据报
  │
  ├── [阶段 3] 提取 DHCP 租约事件，释放 NET_INTERFACE 锁后提交地址/路由/DNS
  │
  ├── [阶段 4] 全局唤醒 TCP/RAW 等待者
  │     if progressed:
  │       ├─ wake_tcp_waiters()
  │       └─ wake_raw_waiters()
  │
  └── [阶段 5] 无条件唤醒 accept 等待者
        └─ wake_tcp_accept_waiters()
```

**阶段 1 — 预收集**：在遍历栈之前 drain 全局延迟删除列表 `UDP_SOCKETS_TO_REMOVE` 和 `TCP_SOCKETS_TO_REMOVE`，为每个 `RouteSocketHandle` 解析其 `SocketHandle` 和所属 `ifindex`。提前收集避免在遍历过程中修改全局列表。

**阶段 2a — CURRENT_POLL_IFINDEX**：设置当前轮询的接口索引，供 `NetRxToken::consume` 中的 ARP 捕获逻辑标记邻居条目所属的接口。

**阶段 2b — UDP 清理**：直接将 socket 从 `SocketSet` 移除并清理 `bindings` 表。UDP 是无状态协议，无需等待特定状态。

**阶段 2c — Veth 帧预分发**：在 smoltcp `poll()` 之前，从 veth 驱动的 `rx_queue` 中提取原始帧交付给 packet socket（AF_PACKET），确保嗅探类 socket 不错过任何帧。

**阶段 2d — smoltcp poll**：这是核心步骤，调用 `stack.iface.poll(timestamp, device, sockets)`。smoltcp 驱动协议栈：处理入站帧、执行 TCP 重传、推进连接状态、发送待发送数据。

**阶段 2e — TCP 清理**：TCP socket 只有在其 smoltcp 状态机进入 `Closed` 状态（四次挥手完成）后才可移除。未完成的 socket 重新放回 `TCP_SOCKETS_TO_REMOVE`，等待下一轮轮询再试。这确保了 TCP 连接的优雅关闭。

**阶段 2f — UDP 分发**：`dispatch_udp_packets()` 从 smoltcp udp::Socket 的接收缓冲区抽干数据，推送到 OS 层 `UdpSocket` 的 `rx_queue`，并唤醒接收等待队列。

**阶段 3 — 全局唤醒**：如果 `poll_once` 推进了协议栈（有数据收发），则调用 `wake_tcp_waiters()` 和 `wake_raw_waiters()`，遍历全局 `TCP_SOCKETS` 和 `RAW_SOCKETS` 列表，可靠唤醒有数据就绪的等待队列。

**WaitQueue 锁序**：网络 syscall 在调用 `wait_until_interruptible` 前先执行 `NET_INTERFACE.poll()`；条件闭包只调用 TCP 的 `_without_poll` 状态检查。poll 可能在阶段 3 重入同一 `EventWaitQueue` 的通知路径，因此不得在条件闭包或其调用链中执行 poll，也不得将通知降级为 best-effort `try_lock()`。

**阶段 4 — accept 唤醒**：无条件调用 `wake_tcp_accept_waiters()`，因为即使 smoltcp 未报告 poll 进展（`progressed == false`），也可能有新的连接请求到达。

### `_poll()` — 备用轮询实现

`_poll()` 是 `poll_once()` 的并行实现，额外执行：

- poll 结束后遍历 `TCP_SOCKETS`，调用每个 TCP socket 的 `update_io_events()` 同步 IO 事件到 pollee。
- TCP 清理条件增加 `TimeWait` 状态，比 `poll_once()` 更激进。
- 不返回 `progressed` 布尔值，始终在最后唤醒等待者。

`_poll()` 目前不参与主路径轮询，保留用于兼容和调试参考。

### `poll_until_quiescent()`

```rust
pub fn poll_until_quiescent(&self) {
    while self.try_poll() {
        crate::task::try_yield();
    }
}
```

反复调用 `try_poll()` 直到锁竞争停止。当前实现中 `try_poll()` 成功获取锁时始终返回 `true`（无论 `poll_once()` 是否推进了协议栈），因此循环条件实际是"锁可用且 `poll_once` 已执行"。每次迭代插入 `try_yield()` 防止独占 CPU。适用于设备初始化后的快速 flush 和批量数据接收场景。

> **注意**：当前 `try_poll()` 不返回 `poll_once()` 的 `progressed` 状态，因此循环不能判断"是否有数据可处理"。如需要精确的空闲检测，需先修改 `try_poll()` 的返回值语义。

### 默认关闭的性能诊断

通用 `perf_diag` 构建可在运行时选择 `network_runtime` profile，通过
`/sys/kernel/stats/net` 获取 poll/progress/lock-busy、RX/TX 字节与 drop，以及
Python 启动归因所需的 exec/openat/read/mmap 计数。该窗口使用前后快照，不输出
逐事件日志；下述 `net_perf_diag` 两秒滑动窗口只在异常稳定复现后短时启用。

构建时加入 `net_perf_diag` 会启用两秒滑动窗口，不改变正式镜像的轮询策略：

```text
[net-perf][poll] dt_ms=... full=<progress>/<count> stack=<progress>/<count> \
  lock_busy=... cpu_permille=... ticks_avg=... ticks_max=...
[net-perf][tcp-rx] dt_ms=... calls=... bytes=... kib_s=... \
  avg_req=... eagain=... zero=...
```

`full`/`stack` 的分子是 smoltcp 实际报告 progress 的次数，分母是调用次数；
`cpu_permille` 是窗口内所有网络 poll 消耗 tick 占墙钟 tick 的千分比。TCP 行同时
覆盖普通内核缓冲区和 curl 使用的 `UserBuffer` 接收路径，避免只插桩
`try_recv()` 后误以为应用没有读取。

LA64 QEMU 使用宿主本地 HTTP 服务传输 71.9 MB 时，通用路径稳定约 20 MB/s；一次
窗口记录 `calls=672`、`bytes=43480800`、`avg_req=102400`、`eagain=1`，说明 curl
每次请求 100 KiB，传输期间并未被持续 `EAGAIN` 限制。空闲期则每两秒出现约
7.3--8.1 万次 full poll，消耗模拟单核约 24%--26%，活跃窗口最高约 68%。

这两组结果的含义不同：高频空 poll 是明确的次级调度开销，但通用 TCP 路径仍能
达到约 20 MB/s。随后实板单变量 A/B 进一步确认，旧 8/4 GMAC ring 平均只有
`129649 B/s` 且每个活跃窗口都新触发 `RU`，生产 48/16 ring 达到
`12286495 B/s`、提升约 94.77 倍且 `RU` 消失。因此物理网卡退化由 RX ring 耗尽
造成。无性能诊断的正式 48/16 persist-shell 镜像三轮平均进一步达到
`12529330 B/s`，相对旧生产基线提升约 96.64 倍；空 poll 仍是独立的 CPU 效率问题。

在优化空闲轮询前必须先修正 `try_poll()` 的返回值语义：当前布尔值表示“拿到锁并
执行过 poll”，不是“协议栈有 progress”，直接拿它作退避信号会得到错误判断。

---

## Socket 管理 API

### 添加 socket

| 方法 | 说明 |
|------|------|
| `add_socket(ifindex, socket)` | 向指定 DeviceStack 添加 smoltcp socket，返回 `SocketHandle` |
| `add_routed_socket(proto, socket)` | 向默认接口（由 `net_core::default_iface()` 决定）添加 socket，返回 `RouteSocketHandle` |
| `add_routed_socket_on(proto, socket, ifindex)` | 向指定 ifindex 的设备添加路由式 socket |

`add_routed_socket` 分配新的 `RouteSocketHandle` 并写入 `bindings` 表：

```rust
inner_ref.bindings.insert(
    route_handle,
    SocketBinding {
        ifindex: target_ifindex,
        handle,
        proto,
    },
);
```

### 访问 socket

路由式访问方法通过 `RouteSocketHandle` 间接定位真实 socket：

| 方法 | 签名 | 功能 |
|------|------|------|
| `tcp_routed_socket` | `(rh, f: FnOnce(&mut tcp::Socket) -> T)` | 通过 RouteSocketHandle 访问 TCP socket |
| `udp_routed_socket` | `(rh, f: FnOnce(&mut udp::Socket) -> T)` | 通过 RouteSocketHandle 访问 UDP socket |
| `raw_routed_socket` | `(rh, f: FnOnce(&mut raw::Socket) -> T)` | 通过 RouteSocketHandle 访问 RAW socket |
| `tcp_connect` | `(rh, remote, local)` | 发起 TCP 连接 |

直接访问方法通过 `SocketHandle` + `ifindex` 直接定位：

| 方法 | 功能 |
|------|------|
| `tcp_socket(handler, ifindex, f)` | 直接通过 SocketHandle 访问 TCP socket |
| `udp_socket(handler, ifindex, f)` | 直接通过 SocketHandle 访问 UDP socket |
| `raw_socket(handler, ifindex, f)` | 直接通过 SocketHandle 访问 RAW socket |

### 移除与迁移

| 方法 | 功能 |
|------|------|
| `remove(handler, ifindex)` | 从指定 DeviceStack 直接移除 socket |
| `remove_routed(rh)` | 移除路由式 socket（从 sockets 和 bindings 同时移除） |
| `rebind_routed_udp(rh, new_ifindex)` | 将 UDP socket 迁移到另一设备栈。创建新 socket 并从旧栈移除。 |

`rebind_routed_udp` 在目标 ifindex 与当前相同时返回原句柄不变；不同时创建新 socket，销毁旧 socket并更新 bindings。RAW socket 创建时已为每个 DeviceStack 分配独立 handler，发送路径按 ifindex 选择既有 handler，不能把一个 handler 迁移到已经存在同协议 handler 的栈，否则接收包会被重复交付。

### 设备栈管理

| 方法 | 功能 |
|------|------|
| `add_veth_stack(nic, device)` | 注册 veth DeviceStack（必须在 `init()` 之后调用） |
| `remove_veth_stack(nic_id)` | 按 nic_id 移除 veth DeviceStack |
| `add_ip_to_stack(ifindex, cidr)` | 同步 IP 地址到 smoltcp Interface |
| `remove_ip_from_stack(ifindex, cidr)` | 从 smoltcp Interface 移除 IP 地址 |
| `stack_ifindexes()` | 返回所有已注册 DeviceStack 的 ifindex 列表 |

### 状态查询

| 方法 | 返回 | 功能 |
|------|------|------|
| `socket_stats()` | `(usize, usize, usize, usize)` | 返回全局 (tcp, udp, raw, pending_remove) 计数 |
| `inner_handler(f)` | `Option<T>` | 通用闭包访问 `NetInterfaceInner` |

`socket_stats()` 统计来自全局列表 `TCP_SOCKETS` 和 `RAW_SOCKETS` 的计数，通过遍历 inner 的 `stacks` 中 socket 类型推导 UDP 数量，并累计待移除列表。

### 便利函数

```rust
pub fn lookup_source_ip(dest_ip: IpAddress) -> IpAddress
pub fn route_check(dest: IpAddress) -> Result<(), SyscallErr>
```

`lookup_source_ip` 查询路由表返回目标地址对应的源 IP。`route_check` 检查目标是否可达，返回 `ENETUNREACH` 错误。

---

## Test Mapping

| 特性 | 测试覆盖 | 状态 |
|------|----------|------|
| `poll()` 定时器驱动 | 集成测试（QEMU 运行基础网络） | pass |
| RV64 virtio-net IRQ 通知 | PLIC ISR → pending 位 → 任务上下文 poll | RV64 编译 + regression 启动 | not_run（regression 无 NIC） |
| `try_poll()` 非阻塞路径 | 隐式覆盖（syscall 路径调用） | pass |
| `add_routed_socket` / `remove_routed` | TCP/UDP socket 生命周期测试 | pass |
| `rebind_routed_udp` | UDP 跨接口迁移 | not_run |
| RAW 按 ifindex 选择 handler | 2K1000LA loopback/网关/公网/domain ping | pass，无 DUP |
| `add_ip_to_stack` / `remove_ip_from_stack` | ifconfig 类操作 | not_run |
| `stack_ifindexes` | 多接口枚举 | not_run |
| `socket_stats` | 统计信息准确性 | not_run |
| `poll_until_quiescent` | 批量数据 flush | not_run |
| veth `add_veth_stack` / `remove_veth_stack` | 容器网络命名空间测试 | not_run |

大多数统计和管理 API 尚未有针对性测试用例，由 QEMU 集成测试隐式覆盖。

---

## Known Issues

1. **单核锁限制**
   - 现象：`NET_INTERFACE.inner` 使用 `spin::Mutex`，在多核环境下锁粒度粗，可能导致 CPU 空转。
   - 根因：当前系统为单核设计，`Mutex` 在单核上工作正常。迁移多核时需改为更细粒度的锁或无锁结构。
   - 影响：目前无实际影响。
   - 修复方向：多核支持时拆分为 per-stack 锁或读写锁。

2. **TCP 清理延迟**
   - 现象：`TCP_SOCKETS_TO_REMOVE` 的重试机制可能导致 socket 销毁延迟 1 到 2 个 poll 周期。
   - 根因：TCP socket 必须等待 smoltcp 状态机进入 `Closed` 才能移除，若 `poll_once()` 未推进到该状态则放回重试。
   - 影响：正常场景无感。大量短连接（>1000 连接/秒）可能积压。
   - 修复方向：增加每轮最大重试次数上限，超时后强制关闭。

3. **`_poll()` 与 `poll_once()` 代码重复**
   - 现象：两个函数实现高度相似但细节不同（TCP 清理条件、`update_io_events` 调用）。
   - 根因：`_poll()` 是较早的轮询实现，`poll_once()` 是优化重构版本。目前 `_poll()` 已不参与主路径。
   - 影响：维护负担。修改一处需同步另一处。
   - 修复方向：统一为一个实现，差异点通过参数控制。

4. **`try_poll_stack()` 与 `poll_once()` 职责边界模糊**
   - 现象：`try_poll_stack()` 内部直接调用 `stack.iface.poll()` 和 `dispatch_udp_packets()`，与 `poll_once()` 中的同类逻辑重复。
   - 根因：`try_poll_stack()` 是为 veth 特定场景优化的快路径，跳过了移除列表 drain 和 accept 扫描。
   - 影响：两个路径的行为可能随时间漂移。
   - 修复方向：提取公共的 per-stack 处理函数。

---

## References

- [architecture.md](architecture.md) — 网络子系统整体架构
- [device-adapter.md](device-adapter.md) — `IfaceDevice` 枚举与 `SmoltcpDeviceAdapter`
- [routing.md](routing.md) — `RouteSocketHandle` / `SocketBinding` / 路由查找
- [dhcp.md](dhcp.md) — 启动阶段 DHCP 探针流程
- [neighbour.md](neighbour.md) — ARP/NDP 邻居表与 `CURRENT_POLL_IFINDEX`
- [net-core-iface.md](net-core-iface.md) — `Iface` trait 与 `NetDeviceEntry`
- `os/src/net/config.rs` — 代码源文件
- [smoltcp 文档](https://docs.rs/smoltcp/) — smoltcp `Interface::poll()` API
