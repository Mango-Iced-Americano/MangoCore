# MangoCore 网络栈系统性优化方案

> 创建: 2026-06-17 | 审查: Oracle (Phase 0/1/2/3/4/5 均需审查)

## 一句话目标

在不破坏 MangoCore "路由层和协议层解耦" 设计的前提下，学习 DragonOS 的 per-iface/per-stack 锁粒度，把 RouteSocketHandle 升级成真正的路由层 fd：不透明、可迁移、支持选路，解引用后数据面只锁相关 stack，不再全局串行化整个网络栈。

## 最终期望状态

```
协议层: TcpSocket / UdpSocket / RawSocket -> RouteSocketHandle
路由层: RouteSocketHandle -> Arc<BindingEntry> -> BindingTarget::Live { Arc<DeviceStack>, SocketHandle }
接口层: DeviceStack -> Mutex<DeviceStackState { Interface, SocketSet, Device, owners }>

数据路径:
  send/recv: RouteSocketHandle -> 短暂查 binding -> clone Arc<BindingEntry>
    -> lock entry.target -> extract (Arc<DeviceStack>, handle) -> lock stack.state
    -> validate owner -> 操作 smoltcp socket -> unlock

  poll: clone stacks -> 逐个 stack lock -> 收集事件 -> 释放 stack lock -> 投递/唤醒
```

## 设计约束

1. 不删除 RouteSocketHandle
2. 不让协议层直接依赖 Interface / DeviceStack
3. Interface 藏在路由层后面，不用全局大锁隐藏
4. RouteSocketHandle 解引用必须短查表
5. 数据面锁粒度降到 per-stack
6. 控制面可慢（add/del/bind/rebind），数据面不可慢
7. UDP rebind 能力保留
8. 多 stack 双锁必须固定顺序（ifindex 从小到大）
9. 每个阶段可编译，不一次重构全部
10. 每阶段结束写清改动清单

## Oracle 审查关键补充

### 生命周期正确性（Phase 1 核心）

`Arc<BindingEntry>` + `Mutex<BindingTarget>` + per-binding lifecycle guard:
- `BindingTarget::Live { stack, handle }` — 正常状态
- `BindingTarget::Closing` — 正在关闭中，阻止新 I/O
- `BindingTarget::Closed` — 已关闭，返回 EBADF

`DeviceStackState.owners: BTreeMap<SocketHandle, RouteSocketHandle>` — 防御性校验

### smoltcp SocketHandle 不可跨 SocketSet 移植

UDP rebind 必须 typed-remove 实际 smoltcp socket 对象再 add 到新 SocketSet。
如果 smoltcp API 不支持，则重建 + re-bind（代价：丢失旧 socket buffer）。

### 锁序规则（9 条）

1. 用户内存翻译/fault/大分配 → 任何 net lock 之前完成
2. `bindings/stacks` 表锁 → 只 clone `Arc`，禁止持有进入 `stack.state`
3. OS socket 锁 → 先 snapshot `RouteSocketHandle`，释放后再进 route
4. Per-binding: `entry.target` → `stack.state`
5. 双 stack: 永远按 `ifindex` 从小到大加锁
6. poll 持有 `stack.state` → 禁止锁 OS socket/wait queue
7. 睡眠前 → 不能持有任何 net lock
8. 本地 UDP 投递 → 不要持有自己 `UdpSocket.inner` 扫描 `UDP_SOCKETS`
9. remove/rebind/I/O → 全部经 `entry.target` 串行化

### poll 不能只靠 try_lock skip

必须配合非 best-effort 的 scheduled poll 路径保证 liveness。

---

## Phase 0: 代码阅读 + 设计确认

目标：完整阅读 net 核心代码，输出设计说明，Oracle 审查确认后进入 Phase 1。

阅读清单:
- `os/src/net/config.rs` — NetInterface, NetInterfaceInner, poll_once, tcp_routed_socket, udp_routed_socket
- `os/src/net/routing.rs` — RouteSocketHandle, SocketBinding, stack_mut
- `os/src/net/socket/mod.rs` — Socket trait
- `os/src/net/socket/inet/stream/*` — TcpSocket, inner, wait
- `os/src/net/socket/inet/datagram/*` — UdpSocket, udp dispatch, rx_queue
- `os/src/net/syscall/sendto.rs` — sys_sendto 路径
- `os/src/net/syscall/recvfrom.rs` — sys_recvfrom 路径
- `os/src/net/syscall/sendmsg.rs` — sys_sendmsg 路径
- `os/src/net/syscall/recvmsg.rs` — sys_recvmsg 路径
- `os/src/mm/uaccess.rs` — UserBuffer/UserBufferReader/UserBufferWriter
- `os/src/syscall/fs.rs` — fs read/write 中 UserBuffer 使用模式

## Phase 1: 拆锁 + 生命周期正确性

目标：
- 不改变对外 syscall 行为
- 不删除 RouteSocketHandle
- 不直接把 Interface 暴露给协议层
- 拆 NET_INTERFACE 全局 inner 大锁 + 引入 lifecycle guard

结构：
```rust
pub struct NetInterface {
    stacks: RwLock<Vec<Arc<DeviceStack>>>,
    bindings: RwLock<Vec<BindingSlot>>,
    next_socket_id: AtomicU64,
}

struct BindingSlot {
    generation: u32,
    entry: Option<Arc<BindingEntry>>,
}

struct BindingEntry {
    id: RouteSocketHandle,
    proto: InetProtocol,
    target: Mutex<BindingTarget>,
}

enum BindingTarget {
    Live { stack: Arc<DeviceStack>, ifindex: u32, handle: SocketHandle },
    Closing,
    Closed,
}

struct DeviceStack {
    ifindex: u32,
    state: Mutex<DeviceStackState>,
    need_poll: AtomicBool,
}

struct DeviceStackState {
    device: IfaceDevice,
    iface: Interface,
    sockets: SocketSet<'static>,
    owners: BTreeMap<SocketHandle, RouteSocketHandle>,
}
```

## Phase 2: UserBuffer 网络直通路径

- Socket trait 加 try_recv_user/try_send_user（默认 fallback）
- TCP 优先: send_slice/recv_slice 直通用户 buffer
- UDP recv: 直写 UserBufferWriter
- 约束: UserBuffer 翻译在进锁前完成，锁内只做小块 copy

## Phase 3: UDP 数据结构优化

- rx_queue 改可共享/引用计数 buffer
- UDP port index 替代全局扫描
- 本地投递消除 to_vec

## Phase 4: poll/NAPI 化

- per-stack bounded poll (budget)
- scheduled poll 路径保证 liveness
- 中断/事件只 schedule 对应 stack

## Phase 5: virtio/smoltcp adapter 拷贝优化

- 减少 RX/TX adapter 中重复复制
- 批量收包/发包
- 不破坏 smoltcp Device trait 安全边界
