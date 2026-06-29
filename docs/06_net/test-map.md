---
title: "网络子系统测试映射"
category: testing
status: draft
owner: MangoCore Team
last_updated: 2026-06-29
tags: [net, testing, ltp, oscomp]
---

# 网络子系统测试映射

## 1. 文档目的

本文档建立网络子系统所有功能模块与 LTP 测试用例、OSComp 测试组之间的完整映射关系。用于：
- 追踪每个网络功能的测试覆盖状态
- 规划 LTP NET-Round 的推进优先级
- 识别暂不支持需排除的测试领域
- 为回归测试提供权威参考

## 2. 测试分类总表

### 2.1 分类体系

| 分类 | 标签 | 含义 | 处理方式 |
|------|------|------|----------|
| PASS | — | 测试通过或已确认语义等价 | 加入回归集 |
| BUG_KERNEL | 内核 Bug | 内核实现语义与 Linux 不一致 | 定位根因后修复 |
| BUG_TEST | 测试 Bug | LTP 测例本身问题或工具链差异 | 上报或本地适配 |
| FEATURE_MISSING | 特性缺失 | MangoCore 暂不支持的协议或功能 | 加入排除清单 |
| ENV_LIMIT | 环境限制 | 测试环境配置不足（接口、路由、命名空间） | 改善环境或排除 |
| TIMEOUT | 超时 | 测试因资源或性能超时 | 分析后决定优化或排除 |

### 2.2 运行结果（LTP 原生）

| 结果 | 含义 |
|------|------|
| TPASS | 测试通过 |
| TFAIL | 测试失败（语义不符合预期） |
| TBROK | 测试框架或环境损坏 |
| TCONF | 测试不适用（缺少内核配置） |
| PANIC | 触发内核 panic |
| TIMEOUT | 超时 |
| NOT_RUN | 尚未运行 |
| NO_BIN | 镜像中无对应二进制 |

## 3. LTP 测试用例映射

### 3.1 NET-Round-0: 套接字基础生命周期加简单收发

| 分类 | LTP 测例 | 系统调用 | 代码路径 | 状态 | 备注 |
|--------|--------------|----------|------------|--------|-------|
| 套接字创建 | socket01, socket02 | SYS_SOCKET | net/syscall/socket.rs, socket/mod.rs | NOT_RUN | 基础创建、domain 和 type 错误处理 |
| 套接字对 | socketpair01, socketpair02 | SYS_SOCKETPAIR | net/syscall/socketpair.rs | NOT_RUN | Unix 套接字对创建与关闭 |
| 地址绑定 | bind01-bind06 | SYS_BIND | net/syscall/bind.rs, inet/common/port.rs | NOT_RUN | 地址绑定、冲突检测、SO_REUSEADDR |
| 监听 | listen01 | SYS_LISTEN | net/syscall/listen.rs, stream/lifecycle.rs | NOT_RUN | 监听队列、backlog 参数 |
| 接受连接 | accept01-accept03, accept4_01 | SYS_ACCEPT, SYS_ACCEPT4 | net/syscall/accept.rs | NOT_RUN | 接受连接、非阻塞、flags |
| 连接建立 | connect01, connect02 | SYS_CONNECT | net/syscall/connect.rs | NOT_RUN | TCP 连接建立、EINPROGRESS |
| 基础收发 | send01, send02, recv01 | SYS_SENDTO, SYS_RECVFROM | net/syscall/sendto.rs, recvfrom.rs | NOT_RUN | 基础数据收发 |
| 地址指定收发 | sendto01-sendto03, recvfrom01 | SYS_SENDTO, SYS_RECVFROM | sendto.rs, recvfrom.rs, udp.rs | NOT_RUN | UDP 地址指定收发 |
| 向量收发 | sendmsg01-03, recvmsg01-03 | SYS_SENDMSG, SYS_RECVMSG | sendmsg.rs, recvmsg.rs | NOT_RUN | 向量 I/O 加辅助数据 |
| 套接字选项 | getsockopt01-02, setsockopt01-10 | SYS_GETSOCKOPT, SYS_SETSOCKOPT | getsockopt.rs, setsockopt.rs | NOT_RUN | 二十余种 socket 选项 |
| 地址查询 | getsockname01, getpeername01 | SYS_GETSOCKNAME, SYS_GETPEERNAME | getsockname.rs, getpeername.rs | NOT_RUN | 本地和对端地址查询 |
| 半关闭 | (shutdown test) | SYS_SHUTDOWN | net/syscall/shutdown.rs | NOT_RUN | 半关闭 SHUT_RD、SHUT_WR、SHUT_RDWR |
| 套接字 ioctl | sockioctl01 | SYS_IOCTL | net/ioctl.rs | NOT_RUN | 套接字 ioctl 操作 |

### 3.2 NET-Round-1: TCP 状态机加高级 I/O

| 分类 | LTP 测例 | 系统调用 | 代码路径 | 状态 | 备注 |
|--------|--------------|----------|------------|--------|-------|
| 轮询与选择 | poll01-02, select01-04, pselect01-03, ppoll01 | SYS_POLL, SYS_SELECT | fs/poll.rs, SocketFile::poll | NOT_RUN | 套接字就绪检测 |
| epoll 事件 | epoll_create01-02, epoll_create1_01-02, epoll_ctl01-05, epoll_wait01-07, epoll_pwait01-05 | SYS_EPOLL_CREATE, SYS_EPOLL_CTL, SYS_EPOLL_WAIT, SYS_EPOLL_PWAIT | fs/eventpoll.rs | NOT_RUN | 网络 fd 的 epoll 事件 |
| 零拷贝发送 | sendfile02-09 | SYS_SENDFILE | 文件到套接字路径 | NOT_RUN | 文件至套接字零拷贝发送 |
| 批量收发 | sendmmsg01-02, recvmmsg01-02 | SYS_SENDMMSG, SYS_RECVMMSG | sendmsg.rs, recvmsg.rs | NOT_RUN | 批量消息收发 |

### 3.3 NET-Round-2: 压力加性能测试（远期）

Round-2 测试包括多连接压力、TCP 拥塞控制、吞吐量基准。具体清单待 Round-0 和 Round-1 稳定后确定。候选类别包括：netstress, tcp_multi*, udp_multi*, tcp_cc*, tcp_fastopen*, iperf, netperf。

## 4. OSComp 测试组映射

| OSComp 测试组 | 配置掩码 | 网络关联性 | 关键系统调用 |
|-------------|-------------|-------------------|-------------|
| basic | 0x001 | 套接字创建、绑定、监听等基础操作 | socket, bind, listen, accept, connect |
| busybox | 0x002 | 网络工具（ping, wget, telnet） | 所有套接字系统调用 |
| lua | 0x004 | Lua 套接字库 | socket, connect, sendto, recvfrom |
| libctest | 0x008 | libc 网络函数 | 所有套接字系统调用 |
| iozone | 0x010 | 文件系统测试，不直接涉及网络 | 不适用 |
| unixbench | 0x020 | 系统综合性能，含部分网络 | socket, connect, send, recv |
| iperf | 0x040 | TCP 和 UDP 吞吐量测试 | sendto, recvfrom, connect, setsockopt |
| libcbench | 0x080 | libc 微基准，含网络函数 | 所有套接字系统调用 |
| lmbench | 0x100 | 延迟和带宽基准，含 TCP 延迟 | socket, connect, send, recv, select |
| netperf | 0x200 | 网络协议回归与性能 | 所有网络系统调用 |
| cyclictest | 0x400 | 实时性测试，不直接涉及网络 | 不适用 |
| ltp | 0x800 | 完整 LTP 网络测例集 | 所有网络系统调用 |

## 5. 当前状态总览

| 统计项 | 数量 | 说明 |
|--------|------|------|
| 镜像中 NET 相关 LTP 二进制 | 约 106 | 交叉编译后包含的网络 LTP 测试程序 |
| 强制排除集 | 约 50 以上 | FEATURE_MISSING 类测例，运行前已过滤 |
| 回归集 | 0 | 尚未完成任何轮次的验证 |
| NET-Round-0 核心测例 | 约 55 | 优先推进的套接字生命周期测例 |
| NET-Round-1 扩展测例 | 约 30 以上 | 依赖 Round-0 稳定 |
| NET-Round-2 压力测例 | 约 20 以上 | 远期规划 |

## 6. 不支持测试排除清单

### 6.1 协议或功能不支持（FEATURE_MISSING）

以下测试因 MangoCore 不实现对应协议或功能，永久排除：

| 类别 | 测例模式 | 原因 |
|------|----------|------|
| SCTP | sctp* | SCTP 协议未实现 |
| IPsec | tcp_ipsec*, udp_ipsec*, sctp_ipsec* | IPsec 不支持 |
| VLAN 和 VXLAN | vlan*, vxlan* | VLAN 和 VXLAN 虚拟网络不支持 |
| WireGuard | wireguard* | VPN 协议不支持 |
| 网络命名空间 | netns*, netns_*, bind_noport01.sh | 命名空间隔离不支持 |
| 网络过滤和 nftables | nft*, nf_* | 防火墙子系统不支持 |
| MPLS | mpls* | MPLS 协议不支持 |
| CAN 总线 | can_* | CAN 总线协议不支持 |
| VM 套接字 | vsock* | Virtio-vsock 不支持 |
| TIPC | tipc* | TIPC 集群协议不支持 |
| DCCP | dccp* | DCCP 协议不支持 |
| 组播和 IGMP | igmp*, mcast* | 组播协议不支持 |
| RAW 套接字高级功能 | raw*（非基础 raw） | 仅支持基础 raw 套接字收发 |
| 无线网络 | wireless* | 无 WiFi 驱动 |
| 工具依赖 | tcpdump01.sh | 依赖 tcpdump 外部工具 |

### 6.2 压力或破坏性测试排除（TIMEOUT 或 DANGEROUS_STRESS）

| 测例 | 原因 |
|------|------|
| netstress | 多连接压力测试，基础阶段禁止 |
| tcp_cc* | TCP 拥塞控制高级特性 |
| tcp_fastopen* | TCP Fast Open 未实现 |

### 6.3 非网络范围

| 测例 | 原因 |
|------|------|
| fork*/clone*/exec*/wait*/signal* | 进程管理 |
| futex* | 同步原语 |
| timer*/clock* | 时间子系统 |
| mmap*/mprotect*/madvise* | 内存管理 |
| socketcall01-03 | x86 平台传统系统调用 |

## 7. 测试运行配置

### 7.1 单分类白名单模式

```bash
# 示例：运行 Round-0 套接字基础测例
make -C os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt \
  CONF_FILE=../os_test.conf

# os_test.conf 关键配置
# ltp_runner=inline
# ltp_include=socket01,socket02,socketpair01,socketpair02, \
#            bind01,bind02,bind03,bind04,bind05,bind06, \
#            listen01,accept01,accept02,accept03,accept4_01, \
#            connect01,connect02
# mask=0x800
```

### 7.2 NET-Round-0 完整运行

```bash
# 在 os_test.conf 中设置
# ltp_runner=inline
# ltp_include=socket01,socket02,socketpair01,socketpair02, \
#             bind01,bind02,bind03,bind04,bind05,bind06, \
#             listen01,accept01,accept02,accept03,accept4_01, \
#             connect01,connect02, \
#             sendto01,sendto02,sendto03,recvfrom01, \
#             send01,send02,recv01, \
#             sendmsg01,sendmsg02,sendmsg03, \
#             recvmsg01,recvmsg02,recvmsg03, \
#             getsockopt01,getsockopt02, \
#             setsockopt01,setsockopt02,setsockopt03,setsockopt04, \
#             setsockopt05,setsockopt06,setsockopt07,setsockopt08, \
#             setsockopt09,setsockopt10, \
#             getsockname01,getpeername01, \
#             sockioctl01
# mask=0x800

# 注入并运行
make -C os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt CONF_FILE=../os_test.conf
cd os && make rv64-run LOG=info
```

### 7.3 回归集运行

待 NET-Round-0 通过后，PASS 测例加入回归集白名单。回归配置维护在 `os_test.conf` 的 `ltp_include` 字段中。

## 8. 网络模块代码路径索引

| 层 | 关键文件 | 说明 |
|----|----------|------|
| 系统调用分发 | syscall/mod.rs | 网络系统调用分发 |
| 套接字系统调用 | net/syscall/socket.rs | 套接字创建 |
| 绑定系统调用 | net/syscall/bind.rs | 地址绑定 |
| 监听系统调用 | net/syscall/listen.rs | 监听队列 |
| 接受系统调用 | net/syscall/accept.rs | 接受连接 |
| 连接系统调用 | net/syscall/connect.rs | TCP 连接 |
| 发送系统调用 | net/syscall/sendto.rs | 数据发送 |
| 接收系统调用 | net/syscall/recvfrom.rs | 数据接收 |
| 向量发送系统调用 | net/syscall/sendmsg.rs | 向量发送 |
| 向量接收系统调用 | net/syscall/recvmsg.rs | 向量接收 |
| 选项读取系统调用 | net/syscall/getsockopt.rs | 套接字选项读取 |
| 选项设置系统调用 | net/syscall/setsockopt.rs | 套接字选项设置 |
| 本地地址查询系统调用 | net/syscall/getsockname.rs | 本地地址查询 |
| 对端地址查询系统调用 | net/syscall/getpeername.rs | 对端地址查询 |
| 半关闭系统调用 | net/syscall/shutdown.rs | 半关闭 |
| Socket trait | net/socket/mod.rs | try_recv、try_send、poll |
| TCP 实现 | net/socket/inet/stream/mod.rs | TcpSocket 状态机 |
| UDP 实现 | net/socket/inet/datagram/udp.rs | UdpSocket 收发 |
| RAW 实现 | net/socket/inet/datagram/raw.rs | RawSocket |
| Unix 实现 | net/socket/unix/ | UnixSocket |
| smoltcp 适配 | drivers/net/adapter.rs | 网络接口封装 |
| virtio-net | drivers/net/virtio_net.rs | 网络设备驱动 |
| 轮询与选择 | fs/poll.rs | 套接字就绪检测 |
| epoll | fs/eventpoll.rs | 网络 fd 的 epoll 事件 |
| 端口管理 | net/inet/common/port.rs | 端口分配与冲突检测 |
| 网络 ioctl | net/ioctl.rs | SIOCGIF* 等 ioctl |

## 9. 晋级路线图

| 阶段 | 目标 | 前提条件 | 里程碑 |
|------|------|----------|--------|
| NET-Preflight | 无 Panic 契约加 Runner 验证 | 网络接口初始化正常 | 测例可失败但不打崩内核 |
| NET-Round-0 | 套接字基础生命周期加简单收发 | Preflight 通过 | Round-0 全部 TPASS |
| NET-Round-1 | TCP 状态机加高级 I/O | Round-0 全部稳定 | Round-1 全部 TPASS |
| NET-Round-2 | 压力加性能测试 | Round-1 稳定 | 吞吐量基准达标 |

## 10. 测试结果记录

每次运行按以下格式记录：

```text
# 日期: 2026-06-14
# Arch: rv64/la64
# Config: mask=0x800, ltp_runner=inline
# Round: NET-Round-0

## Total: XX  TPASS: XX  TFAIL: XX  TBROK: XX  TIMEOUT: XX

## 详细结果
- socket01: TPASS
- socket02: TFAIL (原因...)
- ...
```
