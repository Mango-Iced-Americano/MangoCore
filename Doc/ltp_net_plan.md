# MangoCore NET-LTP 分诊与推进计划

> 最后更新: 2026-05-22
> 状态: Phase 0 — 体系建设中
> 参考: 沿袭 FS-LTP 计划 (Doc/ltp_fs_plan.md) 的框架设计

## 0. 核心原则

1. 先分类，再决定是否修复。只有 `FIXABLE_NOW` 才允许进入修复流程。
2. 不允许为单个 testcase 写硬编码 hack。不允许绕过 smoltcp/net-syscall 正常路径。
3. 不允许看到失败就直接改内核。每次修复前必须回答 4 个问题（见 2.1 节）。
4. 不允许大规模重构，除非当前问题确实无法局部修复。
5. 不允许修一个 testcase 导致已有 PASS testcase 回退。
6. 优先复用 `os_test.conf` 和 `scripts/` 下已有机制。
7. **MangoCore 网络栈**: smoltcp TCP/UDP/RAW + virtio-net。不支持 SCTP/IPsec/VLAN/netfilter/namespace。
8. 每次只推一个 family，不跨 family 并行修。
9. **所有用户可触达路径不得 panic** — 未实现功能返回 errno (EOPNOTSUPP/ENOPROTOOPT)。

---

## 1. 运行结果与行动分类（分离）

### 1.1 运行结果（客观）
| 结果 | 含义 |
|------|------|
| `TPASS` | 测试通过 |
| `TFAIL` | 测试失败（语义不符合预期） |
| `TBROK` | 测试框架/环境损坏，无法继续 |
| `TCONF` | 测试不适用（缺少内核配置/特性） |
| `PANIC` | 触发 kernel panic |
| `TIMEOUT` | 超时 |

### 1.2 行动分类（人工决策）
| 分类 | 含义 | 动作 |
|------|------|------|
| **PASS** | 已通过，进入回归集 | 加入回归集，每次修复后回归 |
| **FIXABLE_NOW** | 当前 round 应支持，是 MangoCore 语义 bug | 允许修复，修复前必须回答 4 个问题 |
| **FIXABLE_LATER** | 未来应支持，依赖前置能力 | 暂不修，写清依赖 |
| **UNSUPPORTED** | 特性过重/性价比低 | 加入 exclude，写清原因 |
| **ENV_FAIL** | LTP 环境/网络配置问题 | 优先修环境，不误判为内核 bug |
| **DANGEROUS_STRESS** | 压力/破坏性/长时间测试 | 基础阶段禁止运行 |

---

## 2. 失败诊断流程

### 2.1 修复前必须回答的 4 个问题
1. 这个 testcase 在验证什么 Linux 语义？
2. 这个语义对 MangoCore 当前比赛目标是否必要？
3. 当前失败属于以下哪一层：
   - **A: syscall 参数/errno 语义** — 入口参数校验、errno 返回
   - **B: fd table / socket 生命周期** — fd 分配/释放/socket 创建
   - **C: 协议栈状态机** — TCP 握手/挥手、UDP 状态
   - **D: 阻塞/非阻塞语义** — O_NONBLOCK、EAGAIN、EINPROGRESS
   - **E: smoltcp 适配层** — adapter.rs 行为差异
   - **F: virtio-net 驱动** — 数据收发、中断
   - **G: 地址/端口管理** — bind 冲突、SO_REUSEADDR
   - **H: socket option** — setsockopt/getsockopt 语义
   - **I: poll/select/epoll** — 网络 fd 就绪检测
   - **J: LTP 环境问题** — 网络配置、接口、路由
   - **K: MangoCore 暂不支持的协议/特性** — SCTP/IPsec/VLAN/RAW socket
   - **L: 压力测试导致 timeout** — 资源耗尽或死锁
4. 只有分类为 `FIXABLE_NOW` 的 testcase 才允许修。

### 2.2 常见误判提醒
- 很多早期 NET fail 不是 smoltcp bug，而是 **网络配置、接口初始化、路由表** 问题
- `TFAIL` 不等于"内核 bug" — 可能是 ENV_FAIL 或 UNSUPPORTED
- `TBROK` 往往是环境问题 — 优先排查网络接口/loopback
- `TCONF` 是预期内的"不适用" — 不需要修

---

## 3. NET-Round 设计

### 3.0 NET-Preflight: No-Panic 契约 + Runner 验证

> **目标**: 先保证"case 可以失败但不能打崩内核"，不修任何功能。

**验证项**:
1. LTP inline runner 可以连续跑网络白名单，单 case timeout 后能继续
2. 网络接口初始化正常（virtio-net + loopback）
3. PANIC/TIMEOUT 检测和隔离正确工作
4. 用户可触达网络路径 `panic!()/todo!()/unwrap()` 基本清零
5. 未实现协议返回明确 errno

### 3.1 NET-Round-0: Socket 基础生命周期 + 简单收发

> **目标**: 基本的 socket 创建、绑定、连接、收发稳定。

**核心 family（必须全部稳定才能晋级）**:
| Family | 测例数(约) | 说明 |
|--------|-----------|------|
| socket/socketpair | ~5 | socket 创建、domain/type 错误处理 |
| bind | ~6 | 地址绑定、冲突检测 |
| listen | ~1 | 监听队列 |
| accept/accept4 | ~4 | 接受连接 |
| connect | ~2 | TCP 连接建立 |
| sendto/recvfrom | ~6 | UDP 基础收发 |
| shutdown | ~1 | 半关闭 |
| getsockname/getpeername | ~2 | 地址查询 |
| getsockopt/setsockopt | ~11 | socket 选项基础 |

**进入本轮条件**: NET-Preflight 通过，网络接口可用。

**本轮排除**:
- SCTP（不支持）
- RAW socket（不支持或极其有限）
- IPv6 复杂功能
- TCP 拥塞控制高级特性
- sendmsg/recvmsg 复杂 scatter/gather

### 3.2 NET-Round-1: TCP 状态机 + 高级选项

> **目标**: TCP 完整握手/挥手、非阻塞语义、scatter-gather I/O。

**核心 family**:
| Family | 测例数(约) | 说明 |
|--------|-----------|------|
| sendmsg/recvmsg | ~6 | 向量 I/O + 辅助数据 |
| poll/select/epoll | ~20 | socket 就绪检测 |
| pselect/ppoll | ~6 | 信号安全轮询 |
| sendfile | ~9 | 零拷贝发送 |
| TCP 状态测试 | ~28 | TCP 多连接/断连 |

**进入本轮条件**: Round-0 全部稳定。

### 3.3 NET-Round-2: 压力 + 性能

> **目标**: 多连接稳定性、吞吐量、并发。

**候选**: netstress, tcp_multi*, udp_multi*, iperf, netperf 相关

### 3.4 长期排除

| 类别 | 原因 |
|------|------|
| SCTP | 协议未实现 |
| IPsec/VLAN/VXLAN | 网络子系统不支持 |
| CAN bus | 非 TCP/IP |
| Netfilter/nftables | 防火墙不支持 |
| Network namespace | Linux 命名空间 |
| Multicast/IGMP | 组播不支持 |
| Wireless | 无 WiFi 驱动 |
| TIPC/DCCP | 特殊协议 |
| RAW socket 高级特性 | 仅基础 raw socket |

---

## 4. 晋级规则（同 FS-LTP §5）

## 5. 每轮工作流程（同 FS-LTP §6）

## 6. 现有网络模块快速索引

| 层 | 关键文件 | 说明 |
|----|----------|------|
| syscall 分发 | `os/src/syscall/mod.rs` | NET syscall dispatch |
| net syscall | `os/src/net/syscall/*.rs` | socket/bind/connect/sendto/recvfrom 等 |
| Socket trait | `os/src/net/socket/mod.rs` | try_recv/try_send/poll/socket_type |
| TCP | `os/src/net/socket/inet/stream/mod.rs` | TcpSocket |
| UDP | `os/src/net/socket/inet/datagram/udp.rs` | UdpSocket |
| RAW | `os/src/net/socket/inet/datagram/raw.rs` | RawSocket |
| Unix | `os/src/net/socket/unix/` | UnixSocket |
| smoltcp 适配 | `os/src/drivers/net/adapter.rs` | smoltcp Interface 封装 |
| virtio-net | `os/src/drivers/net/virtio_net.rs` | 网络设备驱动 |
| poll | `os/src/fs/poll.rs` | poll/select/epoll（共用） |
