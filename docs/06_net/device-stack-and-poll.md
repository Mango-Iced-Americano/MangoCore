---
title: "DeviceStack 与 Polling 基础设施"
module: "config.rs"
category: net
status: current
owner: "MangoCore Team"
last_updated: "2026-08-06"
code_paths:
  - "os/src/net/config.rs"
  - "os/src/net/socket/inet/stream/mod.rs"
  - "os/src/net/socket/inet/stream/inner.rs"
entry_points:
  - "NET_INTERFACE"
  - "NetInterface::init"
  - "NetInterface::request_poll"
  - "NetInterface::poll_now"
  - "NetInterface::net_poll_worker"
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
- **CPU0 worker 轮询**：IRQ 只发布原子请求，CPU0 任务上下文按设备执行有界扫描。
- **双层 socket 句柄**：`RouteSocketHandle` 将用户态 socket 与底层 smoltcp `SocketHandle` 解耦，支持跨设备栈迁移。

---

## 核心数据结构

### `NetInterface`

```rust
pub static NET_INTERFACE: NetInterface = NetInterface::new();

pub struct NetInterface<'a> {
    directory: Mutex<Option<NetDirectory<'a>>>,
    next_route_id: AtomicUsize,
    poll: NetPollControl,
}
```

全局静态单例只集中管理目录和 worker 请求，不再用一把锁包住全部 smoltcp 栈。
`directory` 在 `init()` 前为 `None`；目录锁只查询或发布 route/stack 身份。

### `NetDirectory`

```rust
struct NetDirectory<'a> {
    stacks: BTreeMap<u32, Arc<DeviceStackCell<'a>>>,
    routes: BTreeMap<RouteSocketHandle, RouteDirectoryEntry<'a>>,
}
```

- `stacks`：ifindex 到独立 `DeviceStackCell` 的强引用。
- `routes`：route ID 到 `Weak<DeviceStackCell> + protocol + state` 的短目录项。
- `next_route_id`：位于 `NetInterface` 的单调计数器，旧 route ID 永不复用。

### `DeviceStackCell`

```rust
struct DeviceStackCell<'a> {
    ifindex: u32,
    state: AtomicU8,
    inner: Mutex<DeviceStackInner<'a>>,
}
```

每个设备一个 smoltcp 串行域，`inner` 同时保护 Interface、Device、SocketSet 与
本栈 `LocalSocketBinding`。访问者在目录解锁后取得该锁，并以 route ID 和 protocol
重验 binding，防止旧 route 命中已复用的 smoltcp slot。一个执行流同时最多持有
一个 DeviceStack 锁。

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
3. `NET_INTERFACE.init()` 调用 `NetDirectory::new()`，构造两个固定 DeviceStack：
   - lo 栈：`IfaceDevice::Lo(Loopback)`，IP 地址 127.0.0.1/8 + ::1/128。
   - eth0 栈：`IfaceDevice::Eth(SmoltcpDeviceAdapter)`；若存在物理 NIC 则注册常驻
     DHCP socket，否则使用 `NullNetDevice`。

`NetInterface::init()` 在目录发布后立即 `request_poll()`。此时 worker 尚未创建也没关系：
`pending=true` 会被 worker 首次 `wait_event` 的条件复查消费，boot 路径不直接 poll 设备。

veth 设备栈通过 `add_veth_stack()` 动态注册，在 `NetInterface::init()` 完成后由 veth 对创建路径调用。

---

## 轮询编排

轮询负责驱动 socket 收发、TCP 状态机、ARP 和 UDP 分发。SMP 下只有 CPU0 的
`net_poll_worker` 执行全栈扫描；hard IRQ 和普通生产者都只发布请求。

### `request_poll()` — 任务上下文异步入口

`request_poll()` 只以 AcqRel 把 `pending` 置为 true。只有 false→true 的生产者才
唤醒 worker；多个并发请求会合并为一轮扫描。调用方不得持有 DeviceStack、socket
或 `task.inner` 锁。

### `try_poll_irq()` — hard IRQ 发布入口

IRQ 不进入 smoltcp、不取得 WaitQueue、不分配，也不输出；它只设置 `pending` 和
`deferred_wake`。trap/idle 的安全点随后调用 `run_deferred_net_wake()`，把原子标志
转换成普通 WaitQueue 唤醒。这样长 syscall 仍能接收 IRQ，但不会在中断上下文进入
网络业务锁。

### `net_poll_worker()` — CPU0 消费者

worker 固定在 CPU0。每次醒来最多消费两轮 pending：先 AcqRel 清门，再快照
DeviceStack Arc，逐栈只 `try_lock()` 一次。第一轮扫描期间的新请求可以触发第二轮；
第二轮以后仍到达的请求保留 pending，由下一次调度重新进入等待协议，避免内核栈空转。

某个栈繁忙时只设置 `retry_armed`，CPU0 下一 scheduler tick 才再次请求。worker
释放 DeviceStack 后才提交 DHCP 事件并通知 TCP/UDP/RAW/accept/epoll 等等待者。
由于 kernel worker 从 IRQ-off 的调度边界进入，每轮真实扫描使用
`with_local_interrupts_enabled()` 临时开中断，避免同步 VirtIO TX 轮询 used ring
期间长时间屏蔽 timer/IPI；窗口关闭且网络锁释放后才调用任务安全点处理调度请求。

### `poll_now()` 与 `try_poll_stack()`

`poll_now()` 供 `O_NONBLOCK` 和零超时查询做一次有界扫描；它不会等待 worker，
也不会阻塞获取 DeviceStack。`try_poll_stack(ifindex)` 是同一机制的单栈入口。
两者拿不到栈锁时仅安排 retry，因此非阻塞 syscall 的执行时间不会退化成同步等待。

完整一轮的生产顺序为：

```text
drain 延迟删除 route
  -> 快照全部 DeviceStack Arc，释放 NetDirectory
  -> 打开本 CPU 受控 IRQ 窗口
  -> 每栈 try_lock：veth packet tap -> smoltcp poll -> 提取内核所有的 packet/DHCP 事件
  -> 释放 DeviceStack
  -> 提交 DHCP/route/device 状态并唤醒 socket、EventPoll 与 WaitQueue
  -> 关闭 IRQ 窗口 -> 任务安全点
```

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

当前 worker 的 `progressed` 只服务诊断；调度和重试依据 pending 与下一 tick 的
retry 位，不能把 smoltcp 单次返回值直接当作 liveness 判定。

---

## Socket 管理 API

### 添加 socket

| 方法 | 说明 |
|------|------|
| `add_socket(ifindex, socket)` | 向指定 DeviceStack 添加 smoltcp socket，返回 `SocketHandle` |
| `add_routed_socket(proto, socket)` | 向默认接口（由 `net_core::default_iface()` 决定）添加 socket，返回 `RouteSocketHandle` |
| `add_routed_socket_on(proto, socket, ifindex)` | 向指定 ifindex 的设备添加路由式 socket |

`add_routed_socket` 先在单个 DeviceStack 内插入 smoltcp socket 与本地 binding，
释放栈锁后才在 `NetDirectory` 发布 Active route。若目录发布失败，代码重新进入
原栈回滚本地对象；任何读者都不会看到“目录已发布但栈内尚不存在”的半状态。

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

`socket_stats()` 先快照 stack Arc，再逐栈分别加锁统计，不会同时持有目录锁和
DeviceStack 锁。

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
| IRQ publish-only | `net_smp::irq_poll_is_publish_only` | 双架构 8 核 pass |
| CPU0 worker 无丢失唤醒 | IRQ 请求 + 真实 veth→AF_PACKET 投递 | 双架构 8 核 pass |
| 单栈 poll | veth→AF_PACKET 直接单栈推进 | 双架构 8 核 pass |
| route ID 不复用 | remove 后创建新 routed socket | 双架构 8 核 pass |
| `rebind_routed_udp` | UDP 跨接口迁移 | not_run |
| RAW 按 ifindex 选择 handler | 2K1000LA loopback/网关/公网/domain ping | pass，无 DUP |
| `add_ip_to_stack` / `remove_ip_from_stack` | ifconfig 类操作 | not_run |
| `stack_ifindexes` | veth 注册与清理 | 双架构 8 核 pass |
| `socket_stats` | 统计信息准确性 | not_run |
| veth `add_veth_stack` / `remove_veth_stack` | focused ktest 重复清理 | 双架构 8 核 pass |

大多数统计和管理 API 尚未有针对性测试用例，由 QEMU 集成测试隐式覆盖。

---

## Known Issues

1. **同一设备栈仍是串行数据面**
   - `DeviceStackCell::inner` 保护整个 smoltcp Interface 与 SocketSet；不同设备可并行，
     同一设备上的 socket 不能并行进入 smoltcp。

2. **TCP 清理延迟**
   - 现象：`TCP_SOCKETS_TO_REMOVE` 的重试机制可能导致 socket 销毁延迟 1 到 2 个 poll 周期。
   - 根因：TCP socket 必须等待 smoltcp 状态机进入 `Closed` 才能移除，未关闭时放回重试。
   - 影响：正常场景无感。大量短连接（>1000 连接/秒）可能积压。
   - 修复方向：增加每轮最大重试次数上限，超时后强制关闭。

3. **CPU0 worker 可能成为吞吐瓶颈**
   - v1 选择单 poll owner 以收敛锁序；高 PPS 或多设备同时繁忙时，后续需依据
     poll/lock-busy 计数评估 NAPI 风格预算或多队列，而不是直接让多个 CPU 进入同一栈。

---

## References

- [architecture.md](architecture.md) — 网络子系统整体架构
- [device-adapter.md](device-adapter.md) — `IfaceDevice` 枚举与 `SmoltcpDeviceAdapter`
- [routing.md](routing.md) — `RouteSocketHandle` / route 目录与本地 binding / 路由查找
- [dhcp.md](dhcp.md) — 常驻 DHCP 状态机与租约提交流程
- [neighbour.md](neighbour.md) — ARP/NDP 邻居表与 `CURRENT_POLL_IFINDEX`
- [net-core-iface.md](net-core-iface.md) — `Iface` trait 与 `NetDeviceEntry`
- `os/src/net/config.rs` — 代码源文件
- [smoltcp 文档](https://docs.rs/smoltcp/) — smoltcp `Interface::poll()` API
