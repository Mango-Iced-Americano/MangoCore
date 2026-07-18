# 网络子系统 Deferred LTP 失败清单

> 最后更新: 2026-07-17
> 分支: fs (filesystem)
> 状态: FIXABLE_LATER — 本分支网络代码 intentionally out of scope，仅记录不修改

---

## 目录

1. [范围声明](#1-范围声明)
2. [证据基线](#2-证据基线)
3. [分组根因总结](#3-分组根因总结)
4. [逐用例详情](#4-逐用例详情)
5. [架构差异清单](#5-架构差异清单)
6. [实现位置索引](#6-实现位置索引)
7. [Linux 6.6 参考语义](#7-linux-66-参考语义)

---

## 1. 范围声明

本文档记录 filesystem 分支（2026-07-16 基线）上所有**已知网络子系统 LTP 失败**。

**明确排除（本分支不处理）：**

- 网络源码修改（`os/src/net/`、`os/src/drivers/net/`、`os/src/fs/poll.rs`、`os/src/fs/eventpoll.rs`）
- 测试配置修改（`os_test.conf`、LTP 排除清单）
- Judge 脚本和编译配置
- QEMU/构建/Run 操作

**本文档目的：** 为后续 Net 专项修复提供完整上下文。每个失败用例记录了精确失败信息、双架构证据、实现位置和参考语义。

---

## 2. 证据基线

### 2.1 数据来源

| 来源 | 路径 | 说明 |
|------|------|------|
| rv64 输出 | `testresult/archive_20260716_170949/output-rv64.txt` | 2026-07-16 LTP 全量 syscalls suite，glibc |
| la64 输出 | `testresult/archive_20260716_170949/output-la64.txt` | 同上，loongarch64 |
| LTP 版本 | 20240524 | 内核测试框架 |
| QEMU | rv64: virtio-net, la64: virtio-net-pci | 用户态网络 |
| 注入配置 | mask=0x800 (ltp only), ltp_runner=suite | 全 LTP syscalls + fs suites |

### 2.2 LTP 运行概况

| 指标 | rv64 | la64 |
|------|------|------|
| 总执行 cases | 1367 | 1390 |
| 通过 (PASS) | 956 | 933 |
| 失败 (FAIL) | 130 | 118 |
| 跳过 (SKIP) | 408 | 444 |
| 超时/组截止 | 127 remaining | 105 remaining |
| 到期时间 | 2350s | 2350s |

### 2.3 排除与现有跳过

当前 `os_test.conf` 已排除的网络相关用例：`send02`（单独硬编码排除）。

架构特定跳过：
- rv64 musl: `epoll_create02`（epoll_create 标志位差异）
- la64 musl: 无特殊网络跳过

---

## 3. 分组根因总结

所有网络 deferred 失败按根因分为 **7 个分组**：

| 分组 | 根因类型 | 涉及用例 | 优先级 (估算) | 阻滞依赖 |
|------|---------|---------|-------------|---------|
| **G1** | AF_UNIX bind 地址处理错误 | bind03/04/05, getsockopt02, recvmsg01, sendmsg01 | P0 | 需修复 `sys_bind` 中 Unix 地址解析 |
| **G2** | sendmmsg/recvmmsg 未实现 | sendmmsg01/02, recvmmsg01 | P0 | 需实现 `sys_sendmmsg`/`sys_recvmmsg` |
| **G3** | socket/socketpair errno 语义不匹配 | socket01, socketpair01 | P1 | errno 优先级 + domain/type 校验 |
| **G4** | getsockopt 参数校验 | getsockopt01 | P1 | optlen >=0 检查缺失 |
| **G5** | epoll EPOLLRDHUP 事件未生成 | epoll_wait05 | P1 | TCP socket 关闭路径 + epoll 事件通知 |
| **G6** | connect02 setsockopt 兼容性 | connect02 | P2 | IPV6_ADDRFORM 选项不实现 |
| **G7** | select01 mkfifo 环境问题 + select02 差异 | select01, select02 | P2 | mkfifo 实现问题 (fs 相关) |

### 3.1 G1: AF_UNIX bind 地址处理错误（最严重）

这是**级联影响最广**的单一问题。`sys_bind` 在处理 `AF_UNIX` socket 地址 (`sockaddr_un`) 时对所有 path/abstract 地址返回 `EINVAL(22)`，导致：

- `bind03` — AF_UNIX stream 绑定 → `TBROK: bind(3, socket.1, 110) failed: EINVAL`
- `bind04` — AF_UNIX pathname stream → `TFAIL: bind() failed: EINVAL`
- `bind05` — AF_UNIX pathname datagram / abstract datagram → `TFAIL/TBROK: EINVAL`
- `getsockopt02` — 级联 TBROK，bind 失败无法测试 getsockopt
- `recvmsg01` — 级联 TBROK，UDS bind 失败
- `sendmsg01` — 级联 TBROK，UDS bind 失败

**执行证据（rv64，bind04）：**

```
bind04.c:117: TINFO: Testing AF_UNIX pathname stream
bind04.c:121: TFAIL: bind() failed: EINVAL (22)
```

**执行证据（la64，bind04）：** 完全一致。

**预期行为（Linux 6.6）：** `sockaddr_un` 的地址长度 `110` 字节是标准值（`sizeof(struct sockaddr_un)`），应为有效。`EINVAL` 返回表明 `check_addrlen()` 或 `Endpoint::from_sockaddr()` 拒绝了正确的地址结构。

### 3.2 G2: sendmmsg/recvmmsg 未实现

`sendmmsg` 和 `recvmmsg` 系统调用在 dispatch 表中不存在，所有调用返回 `ENOSYS(38)`。

**执行证据（rv64，recvmmsg01）：**

```
recvmmsg01.c:120: TFAIL: sendmmsg() failed: ENOSYS (38)
recvmmsg01.c:92:  TFAIL: recvmmsg() bad socket file descriptor expected EBADF: ENOSYS (38)
recvmmsg01.c:92:  TFAIL: recvmmsg() bad message vector address expected EFAULT: ENOSYS (38)
recvmmsg01.c:92:  TFAIL: recvmmsg() negative seconds in timeout expected EINVAL: ENOSYS (38)
recvmmsg01.c:92:  TFAIL: recvmmsg() overflow in nanoseconds in timeout expected EINVAL: ENOSYS (38)
recvmmsg01.c:92:  TFAIL: recvmmsg() bad timeout address expected EFAULT: ENOSYS (38)
```

**预期行为（Linux 6.6）：** `sendmmsg` 和 `recvmmsg` 是 `sendmsg`/`recvmsg` 的批量变体，接受 `struct mmsghdr` 数组。应在 `syscall/mod.rs` 注册 `SYSCALL_SENDMMSG`(269) 和 `SYSCALL_RECVMMSG`(299)。

### 3.3 G3: socket/socketpair errno 语义不匹配

`socket01` 和 `socketpair01` 的 errno 返回值与 Linux 6.6 不一致。

**socket01 具体失败点：**

| LTP 检查项 | 当前结果 | 期望 errno | 实际 errno |
|-----------|---------|-----------|-----------|
| invalid domain | 返回 -1 | EINVAL(22) | — 具体 errno 未显示 |
| raw open as non-root | 返回 -1 | EACCES(13) 或 EPERM(1) | — |
| UDP stream | 返回 -1 | ESOCKTNOSUPPORT(94) / EOPNOTSUPP(95) | — |
| TCP dgram | 返回 -1 | ESOCKTNOSUPPORT(94) / EOPNOTSUPP(95) | — |
| ICMP stream | 返回 -1 | EACCES(13) / EPROTONOSUPPORT(93) | — |

**socketpair01 具体失败点：**

| LTP 检查项 | 当前结果 | 期望 errno | 实际 errno |
|-----------|---------|-----------|-----------|
| invalid domain | 返回 0（成功） | EAFNOSUPPORT(97) | — |
| AF_UNIX + SOCK_STREAM + protocol=2 | 期望 EOPNOTSUPP(95) | EOPNOTSUPP(95) | EPROTONOSUPPORT(93) |
| AF_UNIX + SOCK_DGRAM + protocol=2 | 期望 EOPNOTSUPP(95) | EOPNOTSUPP(95) | EPROTONOSUPPORT(93) |

### 3.4 G4: getsockopt 参数校验缺失

`getsockopt01` 检测到无效的 `optlen` 参数（< 0 或超大值）未被拒绝：

```
getsockopt01.c:71: TFAIL: invalid optlen succeeded
```

**根因：** `sys_getsockopt` 在 `os/src/net/syscall/getsockopt.rs:58` 处检查 `optlen_val < 4` 返回 `EINVAL`，但 LTP 的 `invalid optlen` 测试传入了 `optlen_val=0` 或负值，函数在 NULL 指针检查时提前返回 `EFAULT` 或执行通过。**需要检查 optlen 有符号性：Linux 6.6 中 `optlen` 是 `socklen_t` 无符号类型，但 LTP 测试用 `(int)-1` 传参，期望 `EINVAL`。**

**双架构证据：** rv64 和 la64 完全一致。

### 3.5 G5: epoll EPOLLRDHUP 事件未生成

`epoll_wait05` 验证在 TCP 连接对端关闭后 epoll 是否能返回 `EPOLLRDHUP` 事件：

```
epoll_wait05.c:90: TFAIL: EPOLLRDHUP has not been received
```

**根因分析：** `EventPoll` 实现 (`os/src/fs/eventpoll.rs`) 依赖 socket 的 `poll()` 返回的 `EPollEvent` 来判断就绪事件。`EPOLLRDHUP` 标志位在 `EPollEvent` 中定义为 `0x2000`，但 TCP socket 的 `poll` 实现 (`impl_file_for_socket!` 宏 + socket trait 的 `poll()` 方法) 在连接关闭时可能只返回 `EPOLLIN|EPOLLHUP` 而缺少 `EPOLLRDHUP`。需要 TCP socket 层在收到对端 FIN 时标记 `EPOLLRDHUP`。

**双架构证据：** rv64 和 la64 完全一致。

### 3.6 G6: connect02 setsockopt 兼容性

`connect02` 尝试 `setsockopt(IPV6_ADDRFORM)`，该选项在 MangoCore 中未实现：

```
connect02.c:94: TFAIL: setsockopt(IPV6_ADDRFORM) failed: ENOPROTOOPT (92)
```

**预期行为（Linux 6.6）：** `IPV6_ADDRFORM` 允许 IPv6 socket 转换为 IPv4。MangoCore 可以用 `ENOPROTOOPT` 或 `EINVAL` 拒绝，具体取决于内核是否声明支持 IPV6 level 的 setsockopt。LTP `connect02` 期望此调用不导致 `TFAIL`。

**双架构证据：** rv64 和 la64 完全一致。

### 3.7 G7: select 相关

**select01:** 失败点不是 select 本身，而是前置条件 `mkfifo` 失败：

```
select01.c:103: TBROK: mkfifo(tmpfile2, 0666) failed: EINVAL (22)
```

这是 filesystem 的 mkfifo 实现问题（命名管道 mode 参数校验），与网络无关。修复后 select01 大概率自然 PASS。

**select02:** rv64 和 la64 行为不同：

| 架构 | select02 结果 |
|------|-------------|
| rv64 | PASS (0) |
| la64 | FAIL (1) |

select02 测试 `select()` 对异常 fdset 的处理。架构差异可能源于 loongarch64 上的 libc select() 封装差异或 syscall ABI 参数传递不同，需要 la64 专用调试。

---

## 4. 逐用例详情

### 4.1 总表（rv64 + la64）

| Testcase | rv64 | la64 | 分组 | 失败类型 | 速记根因 |
|----------|------|------|------|---------|---------|
| bind01 | PASS | PASS | — | — | — |
| bind02 | PASS | PASS | — | — | — |
| bind03 | FAIL(2) | FAIL(2) | G1 | TBROK | AF_UNIX sockaddr 被拒绝 |
| bind04 | FAIL(33) | FAIL(33) | G1 | TFAIL+TCONF | AF_UNIX path bind EINVAL |
| bind05 | FAIL(3) | FAIL(3) | G1 | TFAIL+TBROK | AF_UNIX dgram bind EINVAL |
| bind06 | SKIP(32) | SKIP(32) | G1 | SKIP | — |
| connect01 | PASS | PASS | — | — | — |
| connect02 | FAIL(1) | FAIL(1) | G6 | TFAIL | IPV6_ADDRFORM setsockopt |
| epoll_create01 | PASS | PASS | — | — | — |
| epoll_create02 | PASS | PASS | — | — | — |
| epoll_create1_01 | PASS | PASS | — | — | — |
| epoll_create1_02 | PASS | PASS | — | — | — |
| epoll01 | PASS | PASS | — | — | — |
| epoll_ctl01 | PASS | PASS | — | — | — |
| epoll_ctl02 | PASS | PASS | — | — | — |
| epoll_ctl03 | PASS | PASS | — | — | — |
| epoll_ctl04 | PASS | PASS | — | — | — |
| epoll_ctl05 | PASS | PASS | — | — | — |
| epoll_wait01 | PASS | PASS | — | — | — |
| epoll_wait02 | PASS | PASS | — | — | — |
| epoll_wait03 | PASS | PASS | — | — | — |
| epoll_wait04 | PASS | PASS | — | — | — |
| **epoll_wait05** | **FAIL(1)** | **FAIL(1)** | **G5** | **TFAIL** | **EPOLLRDHUP 未收到** |
| epoll_wait06 | PASS | PASS | — | — | — |
| epoll_wait07 | PASS | PASS | — | — | — |
| epoll_pwait01-05 | PASS | PASS | — | — | — |
| getsockopt01 | **FAIL(1)** | **FAIL(1)** | **G4** | **TFAIL** | **invalid optlen 未拒绝** |
| getsockopt02 | **FAIL(2)** | **FAIL(2)** | **G1** | **TBROK** | AF_UNIX bind 级联失败 |
| listen01 | PASS | PASS | — | — | — |
| poll01 | PASS | PASS | — | — | — |
| poll02 | PASS | PASS | — | — | — |
| ppoll01 | PASS | PASS | — | — | — |
| pselect01-03 | PASS | PASS | — | — | — |
| recvmsg01 | **FAIL(2)** | **FAIL(2)** | **G1** | **TBROK** | AF_UNIX bind 级联失败 |
| recvmsg02 | PASS | PASS | — | — | — |
| recvmsg03 | SKIP | SKIP | — | — | — |
| **recvmmsg01** | **FAIL(33)** | **FAIL(33)** | **G2** | **TFAIL** | **ENOSYS 未实现** |
| **select01** | **FAIL(6)** | **FAIL(6)** | **G7** | **TBROK** | **mkfifo 环境问题** |
| select02 | PASS | **FAIL(1)** | **G7** | TFAIL | la64 架构差异 |
| select03 | PASS | PASS | — | — | — |
| select04 | PASS | PASS | — | — | — |
| sendfile02-08 | PASS | PASS | — | — | — |
| sendfile09 | SKIP | SKIP | — | — | — |
| **sendmsg01** | **FAIL(2)** | **FAIL(2)** | **G1** | **TBROK** | **UDS bind 失败** |
| sendmsg02 | PASS | PASS | — | — | — |
| sendmsg03 | SKIP | SKIP | — | — | — |
| **sendmmsg01** | **FAIL(33)** | **FAIL(33)** | **G2** | **TFAIL** | **ENOSYS 未实现** |
| **sendmmsg02** | **FAIL(33)** | **FAIL(33)** | **G2** | **TFAIL** | **ENOSYS 未实现** |
| setsockopt01 | PASS | PASS | — | — | — |
| shutdown | — | — | — | — | 镜像中未包含 |
| **socket01** | **FAIL(1)** | **FAIL(1)** | **G3** | **TFAIL** | **errno 语义不匹配** |
| socket02 | PASS | PASS | — | — | — |
| socketcall01-03 | SKIP | SKIP | — | — | x86 legacy |
| **socketpair01** | **FAIL(1)** | **FAIL(1)** | **G3** | **TFAIL** | **errno 域/协议校验错** |
| socketpair02 | PASS | PASS | — | — | — |

### 4.2 强排除（非 deferred）

以下用例不在文档覆盖范围内，为永久不可用：

| 用例 | 原因 |
|------|------|
| sctp\* | SCTP 协议未实现 |
| tcp_ipsec\*, udp_ipsec\* | IPsec 不支持 |
| vlan\*, vxlan\* | VLAN/VXLAN 不支持 |
| wireguard\* | VPN 不支持 |
| netns\*, netns\_\* | Network namespace 不支持 |
| nft\*, nf\_\* | Netfilter/iptables 不支持 |
| can\_\* | CAN bus 不支持 |
| vsock\* | VM socket 不支持 |
| bind_noport01.sh | 依赖 network namespace |
| send02 | 已排除（硬编码） |

---

## 5. 架构差异清单

### 5.1 无差异的失败

以下用例在 rv64 和 la64 上行为**完全一致**：bind03, bind04, bind05, connect02, epoll_wait05, getsockopt01, getsockopt02, recvmsg01, recvmmsg01, select01, sendmsg01, sendmmsg01, sendmmsg02, socket01, socketpair01。

### 5.2 确认的架构差异

| 用例 | rv64 | la64 | 差异原因推测 |
|------|------|------|------------|
| select02 | PASS(0) | FAIL(1) | la64 libc select() 封装差异或 syscall ABI 参数对齐 |
| epoll_create02 (musl arch exclude) | rv64 musl 已排除 | la64 musl 不排除 | 架构相关 flags 检查 |

### 5.3 LA64 独有证据

la64 上有 118 个 FAIL 对比 rv64 的 130 个 FAIL，表明 la64 在 fs/mount 相关用例上通过率更高（但 select02 为 la64 独占失败）。la64 的 LTP 运行在 `qemu-system-loongarch64` + `virtio-net-pci` 网络设备上，与 rv64 的 `virtio-net-device` (MMIO) 不同，但网络行为一致。

---

## 6. 实现位置索引

### 6.1 G1: AF_UNIX bind

| 文件 | 关键代码 | 行号 |
|------|---------|------|
| `os/src/net/syscall/bind.rs` | `sys_bind()` → Unix endpoint 分发 | L69-L246 |
| `os/src/net/syscall/bind.rs` | `check_addrlen()` 调用 | L70-L73 |
| `os/src/net/syscall/bind.rs` | `Endpoint::from_sockaddr()` 解析 | L75-L81 |
| `os/src/net/syscall/bind.rs` | Unix path bind → `parent_node.create()` | L153-L220 |
| `os/src/net/socket/unix/ns/mod.rs` | `UnixStreamSocket` / `UnixDatagramSocket` | 全局 |
| `os/src/net/endpoint.rs` | `Endpoint::Unix` 变体定义 | — |
| `os/src/net/syscall/common.rs` | `check_addrlen()` 地址长度校验 | — |

### 6.2 G2: sendmmsg/recvmmsg

| 文件 | 关键代码 | 行号 |
|------|---------|------|
| `os/src/syscall/mod.rs` | dispatch match → 无 `SYSCALL_SENDMMSG`/`SYSCALL_RECVMMSG` | — |
| `os/src/net/syscall/sendmsg.rs` | `sys_sendmsg` 作为批量变体的基座 | — |
| `os/src/syscall/syscall_id.rs` | `SYSCALL_SENDMMSG`(269) / `SYSCALL_RECVMMSG`(299) 应在此定义 | — |

### 6.3 G3: socket/socketpair errno

| 文件 | 关键代码 | 行号 |
|------|---------|------|
| `os/src/net/syscall/socket.rs` | `sys_socket()` → domain/type/protocol 校验 | — |
| `os/src/net/syscall/socketpair.rs` | `sys_socketpair()` → domain 校验 + errno | — |
| `os/src/net/socket/mod.rs` | `Socket` trait 的 `alloc()` → domain 过滤 | — |

### 6.4 G4: getsockopt 参数校验

| 文件 | 关键代码 | 行号 |
|------|---------|------|
| `os/src/net/syscall/getsockopt.rs` | `optlen_val < 4` 检查 | L58-L59 |
| `os/src/net/syscall/getsockopt.rs` | NULL 指针提前 `EFAULT` 返回 | L45-L47 |
| `os/src/syscall/common.rs` | `socklen_t` 的有符号性处理 | — |

### 6.5 G5: epoll EPOLLRDHUP

| 文件 | 关键代码 | 行号 |
|------|---------|------|
| `os/src/fs/eventpoll.rs` | `EventPoll::record_observed_event()` | — |
| `os/src/fs/eventpoll.rs` | `EPollEvent::EPOLLRDHUP` 定义 | — |
| `os/src/net/socket/mod.rs` | `impl_file_for_socket!` 宏中的 `poll()` 实现 | — |
| `os/src/net/socket/inet/stream/inner.rs` | TCP socket `poll()` 方法 | — |

### 6.6 G6: connect02

| 文件 | 关键代码 | 行号 |
|------|---------|------|
| `os/src/net/syscall/connect.rs` | `sys_connect` 实现 | — |
| `os/src/net/syscall/common.rs` | SOL_IPV6 level 识别 | — |
| `os/src/net/syscall/setsockopt.rs` | `sys_setsockopt` 中的 level 分发 | — |

### 6.7 G7: select

| 文件 | 关键代码 | 行号 |
|------|---------|------|
| `os/src/fs/poll.rs` | `sys_select()`/`sys_pselect6()` | — |
| `os/src/syscall/mod.rs` | `SYSCALL_SELECT`/`SYSCALL_PSELECT6` dispatch | — |
| `os/src/fs/dev.rs` | `mkfifo` 实现（select01 断裂依赖） | — |

---

## 7. Linux 6.6 参考语义

### 7.1 AF_UNIX bind (G1)

**Linux 6.6 fs/af_unix.c:unix_bind_bsd()**:

- `sockaddr_un` 的 `sun_path` 以 null 结尾，最大长度 108 (`UNIX_PATH_MAX` — 108)。
- `bind()` 检查路径长度 `sizeof(sa_family_t) + strlen(path) + 1`，不应使用固定 110 作为总长拒绝。
- 绑定成功时在文件系统创建 socket 文件（`S_IFSOCK`），path 不存在则返回 `EADDRINUSE`。
- 抽象命名空间（`sun_path[0] == '\0'`）在 `unix_filesystem` 上创建伪目录项。

MangoCore 当前在 `check_addrlen(addrlen)` 或 `Endpoint::from_sockaddr()` 阶段就拒绝了合法的 110 字节地址，需要排查。

### 7.2 sendmmsg/recvmmsg (G2)

**Linux 6.6 net/socket.c:__sys_sendmmsg()**:
- `SYSCALL_DEFINE4(sendmmsg, ...)` — 在 loop 中多次调用 `___sys_sendmsg()`，超时或错误时提前返回已发送数。
- `SYSCALL_DEFINE4(recvmmsg, ...)` — 带 `timespec` 超时参数，同样多次调用 `___sys_recvmsg()`。
- 两者都返回**实际发送/接收的 message 数量**（可能为 0），而非字节数。
- 批量语义：一旦第一个 message 发送成功，后续错误只停止迭代，不返回错误码。

### 7.3 socket/socketpair errno (G3)

**Linux 6.6 net/socket.c:sock_create()**:
- `invalid domain` (`EAFNOSUPPORT`) — 内核在 `__sock_create` 中检查 `family <= NPROTO`，超出则返回 `EAFNOSUPPORT`。
- `socketpair` 对 `AF_UNIX` 以外的 domain 返回 `EOPNOTSUPP` （非 `EPROTONOSUPPORT`）。
- 对 `AF_UNIX` + 未知 type (`SOCK_RAW`) 返回 `EOPNOTSUPP`。
- `protocol` 非零时检查：`AF_UNIX` 不允许 `protocol != 0`（`EPROTONOSUPPORT`）。

### 7.4 getsockopt 参数 (G4)

**Linux 6.6 net/socket.c:sys_getsockopt()**:
- `optlen` 是 `socklen_t`（无符号 32 位）。LTP 测试通过传 `(int)-1` 被用户态转换成 `0xFFFFFFFF`，然后被内核的 `EFAULT` 或 `EINVAL` 拒绝。
- 关键差异：**如果 `*(int *)optlen < 0`，Linux 返回 `EINVAL`**。MangoCore 需要将 optlen 先读为有符号整型进行比较。

### 7.5 epoll EPOLLRDHUP (G5)

**Linux 6.6 include/uapi/linux/eventpoll.h**:
- `EPOLLRDHUP = 0x2000` — 仅用于 stream socket 类型（TCP, Unix stream）。
- 当对端关闭连接（发送 FIN）或执行 `shutdown(SHUT_WR)` 时触发。
- 在 `tcp_poll()` 中，当 `sk->sk_shutdown & RCV_SHUTDOWN` 时设置。
- 与 `EPOLLHUP` 不同：`EPOLLRDHUP` 指示**读关闭**（对端不再发送），而 `EPOLLHUP` 是**本端连接关闭**。

### 7.6 connect02 IPV6_ADDRFORM (G6)

**Linux 6.6 net/ipv6/ipv6_sockglue.c:do_ipv6_setsockopt()**:
- `IPV6_ADDRFORM` 允许 IPv6 socket 转换为 IPv4 或反之。选项值为 `AF_INET`(2) 或 `AF_INET6`(10)。
- 选项检查点：需 `capable(CAP_NET_ADMIN)`，socket 当前未连接，协议支持转换。
- 对于不支持转换的 socket（如 TCP），返回 `EINVAL`。
- LTP `connect02` 测试在非 IPv6 socket 上调用此选项，期望返回特定 errno。

### 7.7 select 语义 (G7)

**Linux 6.6 fs/select.c:do_select()**:
- `select()` 对 `nfds` 的安全检查：`nfds > RLIMIT_NOFILE` 或 `nfds > FD_SETSIZE` 返回 `EINVAL`。
- `select01` 的核心验证是 select 对常规文件、管道和 socket 的混合 fdset。mkfifo 是前置条件。
- `select02` 测试 `select()` 对空 fdset 和坏 fd 的响应。

---

## 8. 修复优先级建议

| 优先级 | 分组 | 工作量估计 | 建议策略 |
|--------|------|-----------|---------|
| **P0-立即** | G1 (AF_UNIX bind) | 中等 (2-4天) | 修复 `check_addrlen` 或 `from_sockaddr` 以接受 `sizeof(sockaddr_un)`；同步修复级联用例 |
| **P0-立即** | G2 (sendmmsg/recvmmsg) | 中等 (2-3天) | 基于 `sys_sendmsg`/`sys_recvmsg` 实现批量变体，处理超时和部分发送 |
| **P1-重要** | G3 (socket/pair errno) | 小-中 (1-2天) | 按 Linux 6.6 精确校验 domain/type/protocol 错误路径 |
| **P1-重要** | G4 (getsockopt optlen) | 小 (0.5天) | 修正 optlen 有符号比较 |
| **P1-重要** | G5 (epoll RDHUP) | 中-大 (3-5天) | TCP socket poll() 中加入 RCV_SHUTDOWN 检测；epoll 事件转发 |
| **P2-次要** | G6 (connect02) | 小 (0.5天) | setsockopt IPV6_ADDRFORM 返回 EINVAL |
| **P2-次要** | G7 (select) | 小 (select02 la64) | select02 la64 需单独调试 ABI 差异；select01 随 mkfifo 修复自动解决 |

---

## 9. 变更记录

| 日期 | 变更内容 |
|------|----------|
| 2026-07-17 | 创建文档，基于 2026-07-16 基线 LTP 输出，覆盖 rv64+la64 双架构 |
