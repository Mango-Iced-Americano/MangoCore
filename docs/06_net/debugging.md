---
title: "网络子系统调试指南"
category: debugging
status: draft
owner: MangoCore Team
last_updated: 2026-07-15
tags: [net, debugging, qemu, gdb, troubleshooting]
code_paths:
  - "os/src/net/config.rs"
  - "os/src/net/socket/inet/stream/mod.rs"
  - "os/src/drivers/net/gmac_2k1000.rs"
---

# 网络子系统调试指南

## 1. 日志等级

网络子系统的日志输出通过内核统一的 `LOG` 环境变量控制。在 Makefile 构建时指定：

```bash
# 基本信息：syscall 分发、socket 创建/销毁
cd os && make rv64-run LOG=info

# 详细调试：协议栈内部操作、数据包跟踪
cd os && make rv64-run LOG=trace
```

### 日志等级说明

| 等级 | 用途 | 示例输出 |
|------|------|----------|
| `error` | 不可恢复的错误 | `[virtio_net] transmit failed` |
| `warn` | 可恢复的异常 | `[netlink] recv_queue full`、DHCP 超时 |
| `info` | 关键生命周期事件 | `[net::config] initialized 2 stacks`、`[net_core] registered eth0` |
| `debug` | 数据流跟踪 | socket 状态转换、缓冲操作 |
| `trace` | 逐函数级调试 | poll 循环细节、smoltcp 内部状态 |

### LOG=info 典型输出

```
[net_core] registered lo (ifindex=1)
[net_core] registered eth0 (ifindex=2, no static IP)
[net::config] eth0 addresses: [192.168.1.100/24]
[net::config] initialized 2 stacks
[Connecting::into_result] handle 42 -> Established
```

### LOG=trace 典型输出

```
[NetInterface::poll] poll...
[NetInterface::poll] poll done, progressed=true
[netlink] try_sendmsg: buf_len=128
[netlink] try_sendmsg done, consumed=128/128
```

---

## 2. 关键日志点

以下日志点是排查网络问题时的首选入口：

### Socket 分配 (Socket::alloc)

```rust
// os/src/net/mod.rs — Socket::alloc()
log::info!("[net] Socket::alloc domain={} type={}", domain, sock_type);
```

每次 `sys_socket()` 调用触发。检查 `domain` 和 `sock_type` 是否与预期一致：
- `domain=2` = AF_INET, `domain=1` = AF_UNIX
- `type=1` = SOCK_STREAM (TCP), `type=2` = SOCK_DGRAM (UDP)

### TCP 连接/绑定/监听

| 操作 | 日志格式 | 源文件 |
|------|----------|--------|
| bind | `[TcpSocket::bind] port=8080` | `lifecycle.rs` |
| connect | `[TcpSocket::connect] remote=10.0.2.2:80` | `lifecycle.rs` |
| listen | `[TcpSocket::listen] port=80 backlog=5` | `lifecycle.rs` |
| accept | `[TcpSocket::accept] new fd=7 peer=10.0.2.2:34567` | `lifecycle.rs` |

```rust
// lifecycle.rs:119
trace_event!(0xB031, handle.0 as u64, remote.port as u64, 0, 0, 0, 0);
log::info!("[TcpSocket::connect] handle={} remote={}", handle, remote);
```

### DHCP 探测进度

```rust
// config.rs — DHCP 探测
log::info!("[net::config] starting DHCP probe on eth0");
// ...
log::info!("[net::config] DHCP offer received: {}",
    dhcp_config.address);
// ...
log::info!("[net::config] DHCP timeout, continuing without IP");
```

DHCP 成功时输出分配的 IP 地址。超时后 eth0 无 IP 地址 — 这是已知限制，后续没有自动重试。

### Poll 统计

```rust
// config.rs try_poll_stack()
// 每个设备栈 poll 后检查 progressed 标志:
if progressed {
    log::trace!("[net::poll] poll progressed: socket events updated");
}
```

使用 `NET_INTERFACE.socket_stats()` 获取 poll 后 socket 分布：

```rust
// stats.rs:320
let (tn, un, rn, sp) = NET_INTERFACE.socket_stats();
log::info!("[net] TCP={} UDP={} RAW={} pending_remove={}", tn, un, rn, sp);
```

---

## 3. 跟踪事件 (Trace Events)

网络子系统使用 `trace_event!` 宏记录性能追踪事件。这些事件通过特定 ID 标识，可在 GDB 或日志输出中捕获。

### 注册事件表

| 事件 ID | 位置 | 触发时机 | 参数 |
|---------|------|----------|------|
| `0xB031` | `lifecycle.rs:119` | TCP connect 发起 | handle, remote_port |
| `0xB032` | `inner.rs:272` | Connecting 状态转出 (连接结果) | handle, result_code (1=ok, 2=refused) |
| `0xB033` | `config.rs:497` (注释中) | Poll 后 TCP socket 计数 | sockets.len() |
| `0xB034` | `inner.rs:423` | Listening 创建 | handle, index, port |
| `0xB036` | `config.rs:500` (注释中) | Poll 后唤醒阶段 | progressed flag |
| `0xB040` | `accept.rs:45` | sys_accept4 入口 | sockfd, flags |
| `0xB041` | `accept.rs:49` | sys_accept4 返回 | new_fd |

### 启用方法

trace_event 的历史事件点可在 `os/src/net/config.rs` 的单栈 poll 路径按需恢复；
不要依赖旧行号：

```rust
// 取消注释以启用 trace:
trace_event!(0xB033, sockets.len() as u64, 0, 0, 0, 0, 0);
trace_event!(0xB036, progressed as u64, 0, 0, 0, 0, 0);
```

### GDB 捕获 trace_event

```gdb
# 在 trace_event 宏展开处设断点
b trace_event

# 或按事件 ID 条件断点
break src/net/socket/inet/stream/inner.rs:272
```

### smoltcp TCP 状态码映射

用于 trace_event 和调试输出的 TCP 状态整数映射（`inner.rs:109`）：

| 值 | 状态 |
|----|------|
| 0 | Closed |
| 1 | Listen |
| 2 | SynSent |
| 3 | SynReceived |
| 4 | Established |
| 5 | FinWait1 |
| 6 | FinWait2 |
| 7 | CloseWait |
| 8 | Closing |
| 9 | LastAck |
| 10 | TimeWait |

---

## 4. QEMU 网络配置

### RV64 (MMIO)

```makefile
# os/make/rv64.mk
-device virtio-net-device,netdev=net,bus=virtio-mmio-bus.7 \
-netdev user,id=net
```

- 设备模型：`virtio-net-device` (MMIO)
- 挂载总线：`virtio-mmio-bus.7` (第 8 个 MMIO 槽位)
- MAC 地址：由 smoltcp 设备适配层自动生成，见 `adapter.rs:113` — 使用本地管理的单播 MAC（非全零，smoltcp DHCP 要求）
- QEMU 用户态网络栈提供 DHCP 服务，地址范围为 `10.0.2.0/24`
- DHCP 服务器地址：`10.0.2.2`
- DNS 转发：`10.0.2.3`
- 主机端口转发：未默认配置，可通过 `hostfwd` 参数添加

### LA64 (PCI)

```makefile
# os/make/la64.mk
-device virtio-net-pci,netdev=net0 \
-netdev user,id=net0
```

- 设备模型：`virtio-net-pci` (PCI)
- LA64 使用 PCI 接口而非 MMIO 访问 virtio 设备
- PCI 枚举通过 `virtio_blk_pci::enumerate_virtio_pci()` 实现

### QEMU 用户态网络栈特性

- 内置 DHCP 服务器（默认分配 `10.0.2.15`）
- 内置 DNS 代理（转发到宿主机 DNS）
- ICMP/ping 默认不支持（QEMU user mode 限制）
- 支持 TCP/UDP 连接到宿主机和外部网络
- 默认网关：`10.0.2.2`（指向宿主机）

### 添加端口转发

在 QEMU 命令行添加 `hostfwd` 参数以从宿主机访问内核网络服务：

```makefile
-netdev user,id=net,hostfwd=tcp::5555-:5555
```

### 慢下载必须拆分四条路径

“pip/curl 很慢”同时经过应用、内核 TCP、物理网卡和宿主代理，不能只测公网 URL。
应保持文件、服务端和下载落点一致，按以下顺序建立基线：

1. Mac 直接访问公网文件，再显式指定 Clash HTTP 代理访问同一文件，区分上游链路
   与代理节点/规则性能。
2. Mac 启动本地 HTTP 大文件服务，实板使用局域网地址下载到 `/dev/null`，同时把
   该网段放入 `NO_PROXY`；这一层不经过 DNS、TLS、CDN 或 Clash。
3. QEMU 使用同一 MangoCore 用户态和 TCP 路径访问宿主本地服务；若 QEMU 快而实板
   慢，优先转向网卡 DMA/ring、IRQ/poll 和链路协商，而不是继续修改 pip。
4. 本地链路恢复后再叠加 Clash、公网 DNS 和 HTTPS，每次只引入一个变量。

显式设置 `http_proxy=http://<mac-lan-ip>:7890` 的 HTTP CONNECT 路径不依赖 Clash
TUN/增强模式；TUN 主要负责透明接管未显式使用代理的流量，并可能引入 Fake-IP DNS
和 macOS 互联网共享的转发边界。因此“增强模式关闭”不能解释显式代理本身很慢，
而“增强模式开启”也不能修复板端本地 HTTP 吞吐低。

诊断时还应同步观察宿主 TCP send queue/重传、板端 poll progress、TCP 接收字节和
GMAC `RU/OVF/RPS/TU`。DWMAC 状态事件会黏住；每个统计窗口后必须按 W1C 语义清除
事件位，下一窗口才表示新发生的饥饿或 underflow。

2K1000LA 的已验证案例中，旧 8 项 RX ring 每个活跃窗口均出现新 `RU`，8 MiB 本地
HTTP 平均仅 `129649 B/s`；只把 ring 改为 48 RX/16 TX 后达到 `12286495 B/s`，
提升约 94.77 倍且 `RU` 消失。保持小 ring 只关闭 delayed ACK 没有收益。遇到相似
现象时，应优先用新鲜 DMA 事件和单变量 ring A/B 证明饥饿，不要先改应用缓冲区、
代理或 ACK 策略。

---

## 5. 数据包捕获

### 方法一：QEMU filter-dump（推荐）

QEMU 的 `filter-dump` 对象可以直接将网络流量写入 pcap 文件：

```makefile
# 已在 rv64.mk 中配置，默认输出到 packets.pcap
-object filter-dump,id=f1,netdev=net,file=packets.pcap
```

运行后生成 `packets.pcap`，可用 Wireshark、tcpdump 或 `tshark` 分析：

```bash
# 在 QEMU 工作目录查看
ls -la packets.pcap

# 用 tcpdump 读取
tcpdump -r packets.pcap -nn

# 用 Wireshark 过滤
wireshark packets.pcap
```

限制：
- 仅捕获通过 QEMU 用户态网络栈的流量
- 不捕获环回设备 (lo) 流量
- pcap 文件每轮 QEMU 运行覆盖写入

### 方法二：Guest 内 tcpdump

如果 busybox 镜像包含 tcpdump：

```bash
# 进入内核 shell
tcpdump -i eth0 -nn
tcpdump -i lo -nn
tcpdump -i any -nn port 80
```

如果 busybox 未包含 tcpdump，可考虑使用 `nc` (netcat) 手工测试：

```bash
# 监听端口
nc -l -p 8080

# 连接测试
echo "test" | nc 10.0.2.2 8080
```

### 方法三：smoltcp 内部跟踪

启用 `LOG=trace` 后，smoltcp 的 poll 过程会输出数据包处理信息。在 `adapter.rs` 的 `SmoltcpDeviceAdapter::transmit()` 和 `receive()` 方法中添加额外的 `log::trace!`：

```rust
// adapter.rs — 在 transmit 中添加
log::trace!("[net] transmit {} bytes on ifindex={}", len, CURRENT_POLL_IFINDEX);

// 在 receive 中添加
log::trace!("[net] received {} bytes", buf.len());
```

---

## 6. 常见 Panic 模式

### 6.1 SocketFS 无根 inode

**现象**：访问 `/proc/self/fd/3`（socket fd）时 panic。

**根因**：SocketFS 文件系统未注册 root inode，或 socket fd 的 `get_ino()` 返回无效值。

**复现条件**：任何 socket fd 被作为目录访问（如 `ls -l /proc/self/fd/`）。

**修复**：确保 `SocketFile` 实现了 `get_ino()` 方法，或在 `readlink`/`stat` 路径中处理 socket fd 的特殊情况。

**相关文件**：
- `os/src/fs/socketfs.rs`
- `os/src/net/socket/mod.rs` — SocketFile 实现

### 6.2 Null NET_INTERFACE

**现象**：网络请求或 socket 操作在初始化前无法找到目标栈——`NET_INTERFACE.directory` 为 `None`。

**根因**：`NetInterface::init()` 尚未被调用，但网络 syscall 已触发。

**复现条件**：用户程序在 `drivers::init()` 和 `net::config::init()` 之间发起 socket 操作。

**修复路径**：目录查询和 `request_poll()` 在未初始化时应安全短路；所有 socket
操作入口也应做保护。

```rust
// config.rs:391
let Some(stack) = self.stack_arc(ifindex) else { return; };
```

**相关文件**：`os/src/net/config.rs`

### 6.3 Double Lock (TicketMutex 重入)

**现象**：死锁 — 线程在 `spin::Mutex` 上自旋。

**根因**：每个 `DeviceStackCell::inner` 是不可重入锁。持有一个栈锁时反向进入
同栈 routed API，或在通知路径反向进入 DeviceStack，会导致死锁。

**典型场景**：
1. routed API 在 N2 取得目标 DeviceStack 锁；
2. 栈内只操作 smoltcp 和形成内核所有的结果；
3. 释放 N2 后才更新 OS socket、EventPoll 或 WaitQueue；
4. 若仍持 N2 时调用会阻塞取得同栈的 API，就会自锁。

**预防措施**：
- 非阻塞 syscall 只在所有 fd/socket/N2 锁外调用一次 `poll_now()`；
- 阻塞路径只异步 `request_poll()`，不得在 WaitQueue 条件闭包中 poll。
- 不要在 `NET_INTERFACE.tcp_routed_socket(rh, |sock| { ... })` 闭包内调用任何网络操作
- 不要在 NET_INTERFACE inner_handler 闭包内持有锁后又等待其他锁

**相关文件**：`os/src/net/config.rs`

### 6.4 用户缓冲区 Page Fault

**现象**：`kernel page fault at 0x7f....` — 访问用户缓冲区时触发。

**根因**：网络路径把用户裸指针当作内核切片，或在一次翻译后跨越等待点保存物理页视图。

**修复**：所有用户态缓冲区访问必须使用安全接口：

```rust
// 读取用户 payload：先分配内核 buffer，再在 VM 锁内复制。
let mut kernel_buf = alloc::vec![0u8; len];
crate::mm::copy_from_user_array(token, buf, kernel_buf.as_mut_ptr(), len)?;

// 解析 sockaddr：使用 common::read_sockaddr() 的内核所有快照。
let sockaddr = read_sockaddr(addr, addrlen)?;
let endpoint = Endpoint::from_sockaddr(&sockaddr)?;

// 错误：直接解引用裸指针
// let ptr = buf as *const u8; // 不安全！
```

`fault_in_user_range()` 只适合在生成随机数、分配 fd 等外部副作用前预检查；
它不会 pin 页，真正读写时仍必须调用 copy helper。

**相关文件**：
- `os/src/net/syscall/sendto.rs`
- `os/src/net/syscall/recvfrom.rs`
- `os/src/net/syscall/sendmsg.rs`
- `os/src/net/syscall/recvmsg.rs`

---

## 7. 诊断命令

### 7.1 socket_stats()

在内核代码中调用 `NET_INTERFACE.socket_stats()` 获取当前 socket 分布：

```rust
// os/src/net/config.rs:364
/// 返回 (tcp_count, udp_count, raw_count, pending_remove)
pub fn socket_stats(&self) -> (usize, usize, usize, usize)
```

已在 `stats.rs` 中集成到内核信息输出：

```
TCP=3 UDP=2 RAW=0 pending_remove=1
```

字段含义：

| 字段 | 来源 | 说明 |
|------|------|------|
| tcp_count | `TCP_SOCKETS` 全局列表长度 | 所有活跃 TCP socket 数量 |
| udp_count | smoltcp SocketSet 中非 TCP/RAW socket 数量 | UDP socket 数量（间接计算） |
| raw_count | `RAW_SOCKETS` 全局列表长度 | RAW 和 Packet socket 数量 |
| pending_remove | `TCP_SOCKETS_TO_REMOVE` + `UDP_SOCKETS_TO_REMOVE` | 等待清理的 socket 数量 |

### 7.2 /proc/net/tcp

```bash
cat /proc/net/tcp
```

示例输出格式：

```
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12345 1 00000000 0 0 0
```

字段说明：
- `local_address`: 16 进制小端 IP + 16 进制端口
- `rem_address`: 16 进制小端 IP + 16 进制端口
- `st`: TCP 状态码（Linux 编码：01=ESTABLISHED, 0A=LISTEN 等）

**注意**：当前实现可能不完全等同于 Linux 格式。如果 busybox `netstat` 或 `ss` 无法解析，检查 `os/src/fs/procfs/files/` 下的对应文件。

### 7.3 /proc/net/udp

```bash
cat /proc/net/udp
```

格式与 `/proc/net/tcp` 类似，列出所有 UDP socket。

### 7.4 /proc/net/dev

```bash
cat /proc/net/dev
```

显示各网络接口的收发统计：

```
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo:       0       0    0    0    0     0          0         0        0       0    0    0    0     0       0          0
  eth0:    1234      10    0    0    0     0          0         0      567       5    0    0    0     0       0          0
```

### 7.5 /proc/net/route

```bash
cat /proc/net/route
```

显示内核路由表（小端格式）。用于验证 DHCP 获取的默认网关是否已正确注入路由表。

### 7.6 /proc/net/arp

```bash
cat /proc/net/arp
```

显示 ARP/NDP 邻居缓存（需在 neighbour 表中查询）。

### 7.7 /proc/net/igmp 和 /proc/net/igmp6

```bash
cat /proc/net/igmp
cat /proc/net/igmp6
```

显示 IPv4/IPv6 多播组成员信息，用于 `netstat -gn`。

---

## 8. GDB 调试

### 8.1 启用调试会话

```bash
# RV64
cd os && make rv64-gdb        # 启动 QEMU + GDB

# LA64 (使用 loongarch64-linux-gnu-gdb)
cd os && make la64-gdb
```

### 8.2 关键断点

#### Syscall 层

| 函数 | 说明 | 源文件 |
|------|------|--------|
| `sys_socket` | 所有 socket 创建入口 | `net/syscall/socket.rs` |
| `sys_bind` | 地址绑定 | `net/syscall/bind.rs` |
| `sys_connect` | TCP 连接发起 | `net/syscall/connect.rs` |
| `sys_listen` | 监听 | `net/syscall/listen.rs` |
| `sys_accept4` | 接受连接 | `net/syscall/accept.rs` |
| `sys_sendto` | 数据发送 | `net/syscall/sendto.rs` |
| `sys_recvfrom` | 数据接收 | `net/syscall/recvfrom.rs` |
| `sys_getsockopt` | 获取 socket 选项 | `net/syscall/getsockopt.rs` |
| `sys_setsockopt` | 设置 socket 选项 | `net/syscall/setsockopt.rs` |
| `sys_getpeername` | 获取对端地址 | `net/syscall/getpeername.rs` |

#### Socket 实现层

| 函数 | 说明 | 源文件 |
|------|------|--------|
| `TcpSocket::try_send` | TCP 数据发送 | `socket/inet/stream/io.rs` |
| `TcpSocket::try_recv` | TCP 数据接收 | `socket/inet/stream/io.rs` |
| `UdpSocket::try_send` | UDP 数据发送 | `socket/inet/datagram/mod.rs` |
| `UdpSocket::try_recv` | UDP 数据接收 | `socket/inet/datagram/mod.rs` |
| `RawSocket::try_send` | RAW 数据发送 | `socket/inet/raw/raw.rs` |
| `RawSocket::try_recv` | RAW 数据接收 | `socket/inet/raw/raw.rs` |
| `Inner::connect` | TCP 连接状态机 | `socket/inet/stream/lifecycle.rs` |
| `Inner::into_result` | Connecting 状态转 Established | `socket/inet/stream/inner.rs` |

#### 网络核心

| 函数 | 说明 | 源文件 |
|------|------|--------|
| `try_poll_stack` | 单设备有界 poll | `config.rs` |
| `net_poll_worker` | CPU0 全栈 worker | `config.rs` |
| `wake_tcp_waiters` | TCP 唤醒分发 | `socket/mod.rs` |
| `dispatch_udp_packets` | UDP 数据分发 | `socket/inet/datagram/udp.rs` |
| `try_deliver_local` | UDP 本地交付 | `socket/inet/datagram/udp.rs` |

#### 驱动层

| 函数 | 说明 | 源文件 |
|------|------|--------|
| `VirtIONetWrapper::transmit` | virtio-net 发送 | `drivers/net/virtio_net.rs` |
| `VirtIONetWrapper::receive` | virtio-net 接收 | `drivers/net/virtio_net.rs` |
| `SmoltcpDeviceAdapter::transmit` | smoltcp 设备抽象发送 | `adapter.rs` |
| `SmoltcpDeviceAdapter::receive` | smoltcp 设备抽象接收 | `adapter.rs` |

### 8.3 GDB 调试技巧

```gdb
# 在 sys_sendto 设置断点
break sys_sendto

# 条件断点：仅在 fd=3 时触发
break sys_sendto if fd == 3

# 查看 TCP 连接状态
break TcpSocket::try_send
commands
  print *self
  continue
end

# 跟踪 poll 循环次数
break try_poll_stack
commands
  set $poll_count = $poll_count + 1
  printf "poll #%d\n", $poll_count
  continue
end

# 捕获 DHCP 事件
break dhcpv4::Socket::poll
commands
  print *this
  continue
end
```

### 8.4 打印关键数据结构

```gdb
# 查看 NET_INTERFACE 内部状态
print NET_INTERFACE
print NET_INTERFACE.directory.lock()

# 查看 TCP socket 列表
print TCP_SOCKETS.lock()
print TCP_SOCKETS.lock().len()

# 查看 smoltcp socket 状态
# (需要找到对应的 DeviceStack 和 SocketHandle)
```

---

## 9. Errno 模式

以下 errno 在网络操作中最常见，排查时优先检查：

| Errno | 值 | 含义 | 常见场景 |
|-------|-----|------|----------|
| `EAGAIN` | 11 | 操作会阻塞 | 非阻塞 socket 收发，缓冲区满/空 |
| `ENOTCONN` | 107 | socket 未连接 | 在 Listening/Init 状态调用 send/recv |
| `EADDRINUSE` | 98 | 地址已使用 | bind 到已占用端口，或 TIME_WAIT 残留 |
| `EAFNOSUPPORT` | 97 | 不支持的地址族 | domain != AF_INET/AF_UNIX/AF_NETLINK |
| `ENOPROTOOPT` | 92 | 不支持的协议选项 | setsockopt 未知 level/optname |
| `EPROTONOSUPPORT` | 93 | 不支持的协议类型 | socketpair 非 AF_UNIX |
| `ECONNREFUSED` | 111 | 连接被拒绝 | 目标端口未监听 |
| `ECONNRESET` | 104 | 连接被重置 | 对端异常关闭 |
| `EPIPE` | 32 | 写端关闭 | 已 shutdown(SHUT_WR) 或对端已关闭 |
| `EINVAL` | 22 | 无效参数 | 错误的 addrlen、flags 或 socket 状态 |
| `EFAULT` | 14 | 无效用户地址 | 用户缓冲区地址不可访问 |
| `ENETUNREACH` | 101 | 网络不可达 | 路由表找不到目标网络 |
| `EMSGSIZE` | 90 | 消息过大 | 数据超过 MTU 或缓冲区上限 |
| `EBADF` | 9 | 无效文件描述符 | fd 不是 socket 或已被关闭 |
| `ETIMEDOUT` | 110 | 连接超时 | TCP SYN 无响应 |

### 排查流程

1. **EAGAIN**：通常是正常的非阻塞行为。检查 socket 是否设置为 O_NONBLOCK。如果未设置，确认 `wait_io` 路径是否正确阻塞。

2. **ENOTCONN**：在 Established 状态检查 `try_send`/`try_recv`。确认 Connecting 状态已转换为 Established（检查 `into_result` 事件 0xB032）。

3. **EADDRINUSE**：确认端口是否被其他 socket 占用。检查 SO_REUSEADDR 是否正确设置。

4. **ENOPROTOOPT**：Linux 规范要求未知 optname 返回 `ENOPROTOOPT(92)`，不是 `EOPNOTSUPP(95)`。见 `setsockopt.rs` 实现。

---

## 10. 已知 Bug 模式

以下模式源于已修复的 bug，在新开发中需特别注意：

### 10.1 TLB 刷新遗漏

**现象**：VirtIO 设备 MMIO 区域写入后，driver 读到陈旧数据。

**根因**：`virtio_net.rs` 中 PMO 映射的 MMIO 页表项被修改后未刷新 TLB。RISC-V 上需 `sfence.vma`，LoongArch 上需 `invtlb`。

**修复**：所有 PTE 修改后必须立即执行架构相关 TLB 刷新：

```rust
// RISC-V
llvm_asm!("sfence.vma" :::: "volatile");

// LoongArch
llvm_asm!("invtlb 0x0, $zero, $zero" :::: "volatile");
```

**相关文件**：`os/src/drivers/net/virtio_net.rs`、`os/src/mm/page_table/`

### 10.2 锁顺序：NET_INTERFACE 与 task 锁

**现象**：死锁——`task.inner.lock()` 与 DeviceStack/socket 业务锁交叉持有。

**根因**：信号检查路径在持有 `task.inner` 时进入网络业务锁，而网络唤醒又反向
访问任务状态。

**规则**：持有 `task.inner` 时不得调用 `poll_now()` 或进入 routed socket；异步
`request_poll()` 也应在释放业务锁后调用，保持统一锁序。
- 锁 → clone Arc → 释放锁 → 执行操作
- 信号检查在释放 `task.inner` 后通过 `has_actionable_signal()` 完成

```rust
// 错误：持有 task 锁时进入同步网络扫描
let task = current_task();
let task_inner = task.inner.lock();
NET_INTERFACE.poll_now(); // 死锁风险

// 正确：先释放 task 锁
let task = current_task();
let sig = {
    let task_inner = task.inner.lock();
    // 快速读取信号信息
    task_inner.signal.clone()
};
drop(task_inner);
NET_INTERFACE.poll_now(); // 所有业务锁释放后才安全
```

### 10.3 非阻塞路径：锁外执行一次 poll_now

**现象**：非阻塞 socket 操作 (`MSG_DONTWAIT`) 返回 `EAGAIN`，即使数据已到达。

**根因**：非阻塞 syscall 在检查 socket 就绪前没有给已到达的数据一次有界搬运机会。

**规则**：非阻塞路径在取得 fd/socket 锁前调用一次 `poll_now()`：

```rust
// syscall/sendto.rs — 正确模式
fn sys_sendto(...) {
    // ...
    if flags & MSG_DONTWAIT == 0 {
        // 阻塞路径
        wait_io(..., || socket.try_send(buf, flags))
    } else {
        // 非阻塞路径：在所有业务锁外做一次有界扫描
        NET_INTERFACE.poll_now();
        socket.try_send(buf, flags)
    }
}
```

阻塞路径只异步 `request_poll()` 并等待事件；禁止把同步 poll 放进 WaitQueue 条件闭包。

### 10.4 getpeername: 地址验证优先

**现象**：`getpeername(NULL, &addrlen)` 返回 `ENOTCONN` 而非 `EFAULT`。

**根因**：实现中先检查连接状态，再验证用户地址。Linux 语义要求先验证参数。

**修复**：始终先验证用户地址参数，再检查 socket 状态：

```rust
// 正确顺序：
// 1. 验证 addr 和 addrlen 用户地址可读
// 2. 验证 addrlen >= sizeof(sockaddr)
// 3. 检查 socket 是否已连接
// 4. 写入结果

// 错误顺序：
// 1. 检查连接状态 → 如果未连接返回 ENOTCONN
// 2. 才验证用户地址 → 需要区分 EFAULT 和 ENOTCONN
```

**相关文件**：`os/src/net/syscall/getpeername.rs`

### 10.5 RISC-V 未对齐 addrlen

**现象**：`connect(fd, addr, addrlen)` 中 `addrlen` 不是 4 的倍数。

**根因**：RISC-V 硬件不报未对齐内存访问异常，因此必须显式检查 `addrlen % 4 != 0`。

**修复**：在 syscall 公共入口检查 addrlen 对齐：

```rust
// os/src/net/syscall/common.rs
if addrlen % 4 != 0 {
    return Err(SyscallErr::EINVAL);
}
```

**相关文件**：`os/src/net/syscall/common.rs`

### 10.6 UDP 本地交付 vs smoltcp 路径冲突

**现象**：向 127.0.0.1 发送 UDP 数据时，数据同时通过本地交付和 smoltcp 路径发送，导致接收端收到重复数据。

**根因**：`UdpSocket::try_send` 首先调用 `try_deliver_local()` 本地交付，然后继续走 smoltcp 发送。当目标为环回地址时，smoltcp 路径会将数据再次送到接收端。

**修复**：本地交付成功后直接返回，不走 smoltcp 路径。`try_deliver_local` 返回 `Result<Option<isize>>` — `Ok(Some(n))` 表示本地交付已完成，跳过 smoltcp。

```rust
// 正确模式：
if let Some(n) = try_deliver_local(remote, &data)? {
    return Ok(n); // 本地交付已处理
}
// 否则走 smoltcp 路径
```

**相关文件**：`os/src/net/socket/inet/datagram/mod.rs`

---

## 11. 文件参考

### 网络核心

| 文件 | 用途 |
|------|------|
| `os/src/net/config.rs` | NET_INTERFACE 全局单例，poll 循环，socket_stats |
| `os/src/net/adapter.rs` | SmoltcpDeviceAdapter，设备抽象层 |
| `os/src/net/mod.rs` | Socket::alloc，TCP_SOCKETS/RAW_SOCKETS 全局列表 |
| `os/src/net/routing.rs` | 路由表，FIB，RouteSocketHandle 管理 |
| `os/src/net/net_core.rs` | 设备注册，DHCP 探测，netns |

### Syscall 层

| 文件 | 用途 |
|------|------|
| `os/src/net/syscall/sendto.rs` | sys_sendto |
| `os/src/net/syscall/recvfrom.rs` | sys_recvfrom |
| `os/src/net/syscall/sendmsg.rs` | sys_sendmsg |
| `os/src/net/syscall/recvmsg.rs` | sys_recvmsg |
| `os/src/net/syscall/accept.rs` | sys_accept4 |
| `os/src/net/syscall/getpeername.rs` | sys_getpeername |
| `os/src/net/syscall/getsockname.rs` | sys_getsockname |
| `os/src/net/syscall/getsockopt.rs` | sys_getsockopt |
| `os/src/net/syscall/setsockopt.rs` | sys_setsockopt |
| `os/src/net/syscall/common.rs` | sockaddr 解析，公用帮助函数 |

### Socket 实现

| 文件 | 用途 |
|------|------|
| `os/src/net/socket/inet/stream/io.rs` | TCP try_send/try_recv |
| `os/src/net/socket/inet/stream/inner.rs` | TCP 状态机，with_tcp_mut，trace_event 映射 |
| `os/src/net/socket/inet/stream/lifecycle.rs` | TCP bind/connect/listen/accept |
| `os/src/net/socket/inet/stream/events.rs` | TCP 事件刷新 |
| `os/src/net/socket/inet/datagram/mod.rs` | UDP try_send/try_recv，本地交付 |
| `os/src/net/socket/inet/datagram/udp.rs` | UDP 数据分发，dispatch_udp_packets |
| `os/src/net/socket/inet/raw/raw.rs` | RAW socket 实现 |

### 驱动层

| 文件 | 用途 |
|------|------|
| `os/src/drivers/net/virtio_net.rs` | VirtIO-net 驱动（MMIO + PCI） |
| `os/src/drivers/net/mod.rs` | NetDevice trait 定义 |
| `os/src/drivers/net/veth.rs` | 虚拟以太网对驱动 |

### /proc/net

| 文件 | 用途 |
|------|------|
| `os/src/fs/procfs/files/net_dev.rs` | /proc/net/dev |
| `os/src/fs/procfs/files/net_route.rs` | /proc/net/route |
| `os/src/fs/procfs/files/net_igmp.rs` | /proc/net/igmp |
| `os/src/fs/procfs/files/net_igmp6.rs` | /proc/net/igmp6 |

### 调试脚本和工具

| 文件/脚本 | 用途 |
|-----------|------|
| `os/make/rv64.mk` | RV64 QEMU 启动参数，filter-dump 配置 |
| `os/make/la64.mk` | LA64 QEMU 启动参数 |
| `scripts/run_full_test.py` | 全自动化测试 |
| `os_test.conf` | 测试组 mask 配置 |
| `os/src/utils/stats.rs` | socket_stats 集成到内核信息输出 |
| `Doc/Work_Log.md` | 网络相关 bug 修复历史 |
| `AGENTS.md` | 网络栈架构概述，§网络栈 |

---

## 调试快速参考

### 一句话排查流程

```
问题: 网络不通

1. LOG=info 启 → 看 socket 创建和 DHCP 是否成功
2. 检查 /proc/net/tcp 和 /proc/net/dev 是否有数据
3. 看 packets.pcap 是否有双向流量
4. LOG=trace 启 → 看 poll 是否 progressed
5. socket_stats() 看 socket 分布
6. GDB 断 sys_sendto / sys_recvfrom 看 errno
```

### 常见症状 → 根因映射

| 症状 | 首要检查 | 可能根因 |
|------|----------|----------|
| socket() 返回 -ENOSYS | syscall 号是否正确 | dispatch match 缺失 |
| connect() 挂起 | TCP 状态机是否到 Connecting | poll 未执行 |
| send() 返回 0 但无响应 | packets.pcap 有出站帧吗？ | ARP 解析失败 |
| recv() 永远 EAGAIN | poll progressed? | 非阻塞路径缺 try_poll |
| bind 端口失败 | SO_REUSEADDR 设置了吗？ | TIME_WAIT 残留 |
| DHCP 超时 | QEMU 提供 virtio-net 吗？ | NET_DEVICE 为 None |
| 内核 Panic | 用户地址验证了吗？ | 缺 translated_byte_buffer |
| 死锁 | 锁顺序检查 | task 锁 → NET_INTERFACE 锁 |
