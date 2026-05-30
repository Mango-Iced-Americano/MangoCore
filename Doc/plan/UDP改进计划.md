# UDP 网络栈改进计划

> 基于 DragonOS 网络栈架构分析，对 Mango 内核 UDP 实现提出的改进方案。
> 分析日期：2026-05-07

---

## 当前 Mango UDP 架构概览

- **单文件** `os/src/net/socket/inet/datagram/udp.rs`：约 250 行
- **结构体** `UdpSocket`：用 `Mutex<UdpSocketInner>` 包裹所有状态
- **单全局 SocketSet**：`NET_INTERFACE` 只有一个 `SocketSet`，一个 `poll_once()`
- **分发逻辑**：`dispatch_udp_packets()` 遍历 smoltcp socket，用评分制 `find_best_match()`，把包推到 `rx_queue: VecDeque`
- **缺失功能**：SO_REUSEADDR/SO_REUSEPORT、shutdown 语义、MSG_PEEK、超时、组播/广播 loopback、connect/disconnect 状态机、per-interface 绑定、socket options 大全

## DragonOS UDP 架构概览

| 文件                                  | 大小   | 职责                                                                                   |
| ------------------------------------- | ------ | -------------------------------------------------------------------------------------- |
| `inet/datagram/mod.rs`                | ~900行 | `UdpSocket` 结构体 + `Socket` trait impl，含所有 state machine 逻辑                    |
| `inet/datagram/inner.rs`              | ~400行 | `UnboundUdp` / `BoundUdp` 状态机，recv/send 核心逻辑含 peek/filter                     |
| `inet/datagram/udp_bindings.rs`       | ~200行 | 全局 UDP 绑定注册表，loopback 投递（unicast/multicast/broadcast），REUSEPORT hash 分发 |
| `inet/datagram/option.rs`             | ~350行 | setsockopt/getsockopt 完整实现（SOL_SOCKET, SOL_IP, SOL_IPV6）                         |
| `inet/datagram/multicast_loopback.rs` | ~100行 | 组播成员注册表，IP_MULTICAST_LOOP 支持                                                 |
| `inet/common/port.rs`                 | ~200行 | 端口管理器，完整 REUSEADDR/REUSEPORT 语义                                              |
| `inet/common/mod.rs`                  | ~250行 | `BoundInner` 接口抽象，per-interface socket 绑定，源地址选择                           |

---

## 改进方案（按优先级分三阶段）

### 阶段一：高收益、低/中工作量 🔴

修复最常见的测试失败和基础语义缺失。

---

#### 1.1 添加 MSG_PEEK 支持

**问题**：当前 `try_recv()` 总是 `rx_queue.pop_front()` 破坏性取包，不支持 `MSG_PEEK`。

**DragonOS 做法**：
- `try_recv(buf, peek)` 使用 smoltcp 的 `socket.peek()` 先窥探，满足条件后再 `socket.recv()`
- `try_recv_with_metadata()` 同样支持 peek 参数，返回更多元信息

**修改文件**：`os/src/net/socket/inet/datagram/udp.rs`

**实现方案**：
1. 给 `try_recv()` 添加 `peek: bool` 参数
2. peek=true 时使用 `rx_queue.front()` 读取但不移除，peek=false 用 `pop_front()`
3. 同时修改 `try_recvmsg()` 的签名
4. 在外层 `Socket` trait 方法中传递 peek 标志（来自 `MsgFlags::MSG_PEEK`）

---

#### 1.2 实现 shutdown 语义

**问题**：当前 `shutdown()` 返回 `Ok(())`，完全没有实现。

**Linux 语义**：
- SHUT_RD：不再接收数据，缓冲数据读完后 `recv()` 返回 0（EOF）
- SHUT_WR：不再发送数据，`send()` 返回 `EPIPE`
- UDP 需要 socket 已 connect 才能 shutdown

**DragonOS 做法**：用 `AtomicU8` 存储两个 shutdown bit（bit0=SHUT_RD, bit1=SHUT_WR），在 `recv()`/`send()` 循环中检查状态

**修改文件**：`os/src/net/socket/inet/datagram/udp.rs`

**实现方案**：
1. `UdpSocket` 添加 `shutdown: AtomicU8` 字段
2. `shutdown()` 设置对应的 bit，并 wake 所有等待者
3. `try_recv()` 中：buffer 空且 SHUT_RD → 返回 0
4. `try_send()` 中：SHUT_WR → 返回 `EPIPE`
5. 不需要 connect 的不做 shutdown（或返回 `ENOTCONN`）

---

#### 1.3 SO_REUSEADDR / SO_REUSEPORT 端口管理

**问题**：当前端口管理全靠 smoltcp 自带的 simple bind，不支持多 socket 共享端口。

**DragonOS 做法**：
- `PortManager.udp_port_table: HashMap<u16, Vec<UdpPortBinding>>`，每个端口对应多个绑定
- `UdpPortBinding` 记录 `addr`, `reuseaddr`, `reuseport`, `bind_id`
- `bind_udp_port()` 检查冲突规则：
  - 两个绑定的地址冲突 + 都没设置 reuseaddr/reuseport → `EADDRINUSE`
  - 设置了 reuseport → 允许（用 hash 分发）
  - 设置了 reuseaddr → 允许（部分兼容）

**修改文件**：
- `os/src/net/socket/inet/common/port.rs`（新建或扩展现有）
- `os/src/net/socket/inet/datagram/udp.rs`

**实现方案**：
1. 扩展 `PortManager`：将 `port_table` 改为 `HashMap<u16, Vec<UdpPortBinding>>`
2. 添加 `bind_udp_port(port, addr, reuseaddr, reuseport, bind_id)` 方法
3. 添加 `unbind_udp_port(port, bind_id)` 方法
4. 在 `UdpSocket::bind()` 中使用新的端口管理 API
5. `dispatch_udp_packets()` 中的 `find_best_match()` 需要支持 REUSEPORT 场景（hash 选 socket）

---

#### 1.4 SO_BROADCAST 支持

**问题**：当前无广播权限检查，任何 socket 都可以发广播包。

**DragonOS 做法**：`so_broadcast: AtomicBool`，`try_send()` 中检查目标地址是否广播，若是则要求 `so_broadcast` 为 true

**修改文件**：`os/src/net/socket/inet/datagram/udp.rs`

**实现方案**：
1. `UdpSocketInner` 添加 `broadcast_enabled: bool`
2. `try_send()`/`try_sendmsg()` 中检查 `dest.is_broadcast()` → 若未启用返回 `EACCES`
3. `setsockopt(SO_BROADCAST)` 支持设置/读取

---

#### 1.5 收发超时 (SO_SNDTIMEO / SO_RCVTIMEO)

**问题**：当前阻塞 I/O 循环 `wait_io_core()` 无超时支持，永远阻塞直到成功或信号。

**DragonOS 做法**：
- `send_timeout_us: AtomicU64`, `recv_timeout_us: AtomicU64`（u64::MAX 表示无超时）
- 在 `send()`/`recv()` 循环中用 `wait_event_io_interruptible_timeout(|| can_send(), timeout)` 

**修改文件**：
- `os/src/syscall/utils.rs`（`wait_io_core`）
- `os/src/net/socket/inet/datagram/udp.rs`

**实现方案**：
1. `UdpSocketInner` 添加 `send_timeout: Option<Duration>`, `recv_timeout: Option<Duration>`
2. `wait_io_core()` 添加可选的超时参数
3. 阻塞循环中计算 `deadline = now + timeout`，每次重试前检查是否超时
4. 超时返回 `EAGAIN`（非 `ETIMEDOUT` 以兼容 Linux UDP 语义）
5. `setsockopt(SO_SNDTIMEO/SO_RCVTIMEO)` 支持

---

### 阶段二：中等收益、中等工作量 🟡

改善本地通信能力和连接生命周期管理。

---

#### 2.1 Loopback 投递（Unicast / Multicast / Broadcast）

**问题**：当前发往 127.0.0.1 或组播/广播地址的包可能无法被本地 socket 收到，smoltcp 的 loopback 支持有限。

**DragonOS 做法**：
- `udp_bindings.rs` 维护全局 `UDP_BINDINGS` 表（按 netns + port + addr 索引）
- `deliver_unicast_loopback()`：查绑定表找到匹配 socket，调用 `inject_loopback_packet()`
- `deliver_multicast_all()`：对所有有成员资格的 socket 投递
- `deliver_broadcast_all()`：对所有匹配的 socket 投递
- `match_udp_bindings()`：按 netns → port → addr 三层过滤
- `choose_reuseport_socket()`：用四元组 hash 选择 REUSEPORT socket
- `choose_recent_socket()`：选最近绑定的 socket

**修改文件**：
- `os/src/net/socket/inet/datagram/udp_bindings.rs`（新建）
- `os/src/net/socket/inet/datagram/udp.rs`

**实现方案**：
1. 新建 `udp_bindings.rs`，维护全局 `UDP_BINDINGS: RwSem<Vec<UdpBinding>>`
2. `UdpBinding` 包含 `socket: Weak<UdpSocket>`, `addr`, `port`, `reuseport`, `bound_seq`
3. 实现 `register_udp_binding()` / `unregister_udp_binding()` 生命周期管理
4. 实现 `deliver_unicast_loopback()` / `deliver_multicast_all()` / `deliver_broadcast_all()`
5. 在 `try_send()` 中检测 loopback 条件（目标 IP 是 127.0.0.1 或组播/广播），调用对应投递函数
6. `UdpSocket` 添加 `multicast_loopback_rx: Mutex<VecDeque<LoopbackPacket>>` 队列
7. 添加 `inject_loopback_packet()` 方法

---

#### 2.2 状态机：Unbound → Bound → Connected

**问题**：当前 `connect()` 只是简单设置 `remote_endpoint`，没有隐式绑定、disconnect、preconnect data 等语义。

**DragonOS 做法**：
- `UdpInner` 枚举：`Unbound(UnboundUdp)` / `Bound(BoundUdp)`
- `connect()` 对 unbound socket 调用 `bind_ephemeral()` 隐式绑定
- `connect(AF_UNSPEC)` 或 `connect(port=0)` 触发 disconnect
- `BoundUdp` 维护 `explicitly_bound` 标志：隐式绑定的 socket 在 disconnect 时解绑
- `has_preconnect_data`：connect 时 buffer 中已有数据 → 第一个 recv 不走 filter

**修改文件**：`os/src/net/socket/inet/datagram/udp.rs`

**实现方案**：
1. 引入 `UdpInner` 枚举（或重新组织 `UdpSocketInner`）
2. `connect()` 逻辑：
   - `remote.port == 0` → disconnect
   - socket 未绑定时先 `bind_ephemeral(remote.addr)`
   - connect 前检查 buffer 中有无数据 → 设置 `preconnect_data` 标志
3. `disconnect()` → 清除 `remote_endpoint`，如果 `!explicitly_bound` 则解绑
4. `try_recv()` 中根据是否 connected 做源地址过滤（参考 DragonOS `BoundUdp::try_recv`）

---

#### 2.3 Recv 源地址过滤（Connected Mode）

**问题**：当前 `try_recv()` 不区分 connected/unconnected 模式，connected socket 可能收到其他发送者的包。

**DragonOS 做法**：在 `BoundUdp::try_recv()` 中，如果 socket 已 connect：
- peek 每个包，检查 `metadata.endpoint == expected_remote`
- 不匹配的包 `socket.recv()` 然后丢弃，继续循环
- 匹配的才真正返回给用户

**修改文件**：`os/src/net/socket/inet/datagram/udp.rs`

**实现方案**：
1. 在 `try_recv()` 的 `rx_queue` 遍历逻辑中，connected 模式下跳过不匹配的包
2. 或改为在 `dispatch_udp_packets()` 投递时就过滤（当前 `find_best_match` 已经做了基本过滤，但 connected socket 可能收到 score=1 的匹配包）

---

#### 2.4 Per-Interface Socket 绑定

**问题**：当前所有 socket 都在一个全局 `SocketSet` 中，无法区分网卡，无法正确选择源地址。

**DragonOS 做法**：
- `BoundInner` 持有 `handle: SocketHandle` + `iface: Arc<dyn Iface>`
- `bind()` 时根据目标 IP 地址选择对应的 `Iface`
- `get_iface_to_bind()`：查 IP 属于哪个网卡
- `bind_ephemeral()`：根据 remote 地址查路由选 iface 和源地址
- 支持 `move_udp_to_iface()` 在发送多播时切换网卡

**修改文件**：
- `os/src/net/config.rs`（NET_INTERFACE 可能需要支持多 iface）
- `os/src/net/socket/inet/datagram/udp.rs`

**实现方案**：
1. 需要先有 iface 抽象层（当前只有一个全局 `NET_INTERFACE`）
2. 这是较大的架构改动，可以延后或在阶段三处理
3. 短期替代：在 connect/bind 时用 `lookup_source_ip()` 正确选源地址

---

### 阶段三：低收益、高工作量 🟢

高级功能和边缘场景。

---

#### 3.1 MSG_ERRQUEUE（错误队列）

**问题**：无 IP_RECVERR 支持，无法通过 `recvmsg(MSG_ERRQUEUE)` 获取 EMSGSIZE 等错误。

**DragonOS 做法**：
- `errqueue: Mutex<VecDeque<UdpErrQueueEntry>>`
- `SockExtendedErr` 结构体（ee_errno, ee_origin, ee_type, ee_code）
- `enqueue_errqueue()` / `pop_errqueue()` 方法
- `try_send()` 中 EMSGSIZE 时 enqueue 错误

**修改文件**：`os/src/net/socket/inet/datagram/udp.rs`

**实现方案**：
1. 添加 `SockExtendedErr` 结构体和 `UdpErrQueueEntry`
2. `try_send()` 中 EMSGSIZE 且启用 `IP_RECVERR` 时 enqueue
3. `recvmsg()` 中处理 `MSG_ERRQUEUE` 标志，pop 并格式化返回

---

#### 3.2 IP_PKTINFO / IP_ORIGDSTADDR 辅助数据

**问题**：`recvmsg()` 不支持返回控制消息（cmsg）。

**DragonOS 做法**：`build_udp_recv_cmsgs()` 构建 `IP_PKTINFO` 和 `IP_ORIGDSTADDR` 控制消息。

**修改文件**：`os/src/net/socket/inet/datagram/udp.rs`

---

#### 3.3 组播选项全集

- IP_MULTICAST_TTL
- IP_MULTICAST_LOOP
- IP_MULTICAST_IF
- IP_ADD_MEMBERSHIP / IP_DROP_MEMBERSHIP（含 `multicast_loopback.rs` 注册表）

**DragonOS 做法**：`multicast_loopback.rs` + `option.rs` 中的 `set_ip_option`

---

#### 3.4 UDP-Lite / UDP_CORK / UDP_SEGMENT

这些是 DragonOS 定义的 UDP 扩展选项，正常 UDP 不需要，仅列出。

---

## 推荐实施顺序

```
Phase 1（本周）：
  1.1 MSG_PEEK      → 修复 LTP recvfrom 测试
  1.2 shutdown       → 修复 LTP shutdown 测试
  1.3 SO_REUSEADDR   → 修复端口冲突问题
  1.4 SO_BROADCAST   → 修复广播发送
  1.5 超时           → 修复阻塞 I/O 测试

Phase 2（下周）：
  2.1 Loopback 投递  → 修复 127.0.0.1 和组播通信
  2.2 状态机         → 完善 connect/disconnect 语义
  2.3 Recv 过滤      → 完善 connected recv 语义
  2.4 Per-Interface  → 架构改进（可延后）

Phase 3（后续）：
  3.1 MSG_ERRQUEUE   → 高级错误处理
  3.2 IP_PKTINFO     → 控制消息
  3.3 组播选项        → 完整组播支持
```

---

## 验证清单

每次修改后必须：
- [ ] `make rv64-kernel-build-only` 编译通过
- [ ] `make la64-kernel-build-only` 编译通过
- [ ] QEMU 启动不 panic
- [ ] 相关 LTP/netperf 测试通过
- [ ] 代码内调试日志已删除或归入 `LOG=debug/trace` 控制

---

*本文档基于 2026-05-07 对 DragonOS `master/kernel/src/net/socket/inet/` 和 Mango `os/src/net/` 的对比分析。*
