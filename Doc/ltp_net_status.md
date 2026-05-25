# NET-LTP Testcase 状态表

> 最后更新: 2026-05-22
> 当前阶段: NET-Preflight — 体系建设中
> 分支: net

## 字段说明

| 字段 | 说明 |
|------|------|
| **Testcase** | LTP 测例名 |
| **Round** | 所属 NET Round (0/1/2) |
| **Family** | 所属 syscall family |
| **运行结果** | TPASS / TFAIL / TBROK / TCONF / PANIC / TIMEOUT / NOT_RUN / NO_BIN |
| **行动分类** | PASS / FIXABLE_NOW / FIXABLE_LATER / UNSUPPORTED / ENV_FAIL |
| **回归集** | YES / NO |

---

## 三列表概览

| 列表 | 当前数量 | 说明 |
|------|----------|------|
| **回归集** | 0 | 待从 Preflight 开始累积 |
| **可用二进制** | ~106 | 镜像中存在的 NET 相关 LTP 二进制 |
| **强制排除集** | ~50+ | UNSUPPORTED (SCTP/IPsec/VLAN/netfilter 等) |

---

## NET-Round-0 核心 Family

### socket / socketpair (4 测例, Priority: 10)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| socket01 | NOT_RUN | — | NO | |
| socket02 | NOT_RUN | — | NO | |
| socketpair01 | NOT_RUN | — | NO | |
| socketpair02 | NOT_RUN | — | NO | |
| socketcall01-03 | — | UNSUPPORTED | NO | socketcall 是 x86 legacy |

### bind (6 测例, Priority: 10)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| bind01 | NOT_RUN | — | NO | |
| bind02 | NOT_RUN | — | NO | |
| bind03 | NOT_RUN | — | NO | |
| bind04 | NOT_RUN | — | NO | |
| bind05 | NOT_RUN | — | NO | |
| bind06 | NOT_RUN | — | NO | |
| bind_noport01.sh | — | ENV_FAIL | NO | network namespace |

### listen / accept / accept4 (4 测例, Priority: 10)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| listen01 | NOT_RUN | — | NO | |
| accept01 | NOT_RUN | — | NO | |
| accept02 | NOT_RUN | — | NO | |
| accept03 | NOT_RUN | — | NO | |
| accept4_01 | NOT_RUN | — | NO | |

### connect (2 测例, Priority: 10)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| connect01 | NOT_RUN | — | NO | |
| connect02 | NOT_RUN | — | NO | |

### sendto / recvfrom (6 测例, Priority: 10)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| sendto01 | NOT_RUN | — | NO | |
| sendto02 | NOT_RUN | — | NO | |
| sendto03 | NOT_RUN | — | NO | |
| recvfrom01 | NOT_RUN | — | NO | |
| send01 | NOT_RUN | — | NO | |
| send02 | NOT_RUN | — | NO | |
| recv01 | NOT_RUN | — | NO | |

### sendmsg / recvmsg (7 测例, Priority: 8)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| sendmsg01 | NOT_RUN | — | NO | |
| sendmsg02 | NOT_RUN | — | NO | |
| sendmsg03 | NOT_RUN | — | NO | |
| recvmsg01 | NOT_RUN | — | NO | |
| recvmsg02 | NOT_RUN | — | NO | |
| recvmsg03 | NOT_RUN | — | NO | |
| sendmmsg01 | NOT_RUN | — | NO | |
| sendmmsg02 | NOT_RUN | — | NO | |
| recvmmsg01 | NOT_RUN | — | NO | |

### getsockopt / setsockopt (11 测例, Priority: 9)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| getsockopt01 | NOT_RUN | — | NO | |
| getsockopt02 | NOT_RUN | — | NO | |
| setsockopt01-10 | NOT_RUN | — | NO | 镜像包含全部 10 个 |

### getsockname / getpeername (2 测例, Priority: 8)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| getsockname01 | NOT_RUN | — | NO | |
| getpeername01 | NOT_RUN | — | NO | |

### shutdown (1 测例, Priority: 7)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| (shutdown) | NOT_RUN | — | NO | 镜像中可能存在 |

### sockioctl (1 测例, Priority: 6)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| sockioctl01 | NOT_RUN | — | NO | |

---

## NET-Round-1 Family

### poll / select / pselect / ppoll (14+ 测例, Priority: 9)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| poll01 | NOT_RUN | — | NO | |
| poll02 | NOT_RUN | — | NO | |
| select01-04 | NOT_RUN | — | NO | |
| pselect01-03 | NOT_RUN | — | NO | |
| ppoll01 | NOT_RUN | — | NO | |

### epoll (20+ 测例, Priority: 8)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| epoll_create01-02 | NOT_RUN | — | NO | |
| epoll_create1_01-02 | NOT_RUN | — | NO | |
| epoll_ctl01-05 | NOT_RUN | — | NO | |
| epoll_wait01-07 | NOT_RUN | — | NO | |
| epoll_pwait01-05 | NOT_RUN | — | NO | |

### sendfile (9 测例, Priority: 7)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| sendfile02-09 | NOT_RUN | — | NO | 文件→socket 零拷贝 |

---

## 强制排除清单

### UNSUPPORTED

| 测例 | 原因 |
|------|------|
| sctp* | SCTP 协议未实现 |
| tcp_ipsec*, udp_ipsec*, sctp_ipsec* | IPsec 不支持 |
| vlan*, vxlan* | VLAN/VXLAN 不支持 |
| wireguard* | VPN 不支持 |
| netns*, netns_* | Network namespace 不支持 |
| nft*, nf_* | Netfilter/iptables 不支持 |
| mpls* | MPLS 不支持 |
| can_* | CAN bus 不支持 |
| vsock* | VM socket 不支持 |
| tcpdump01.sh | 依赖 tcpdump 工具 |
| bind_noport01.sh | Network namespace |

### DANGEROUS_STRESS

| 测例 | 原因 |
|------|------|
| netstress, tcp_cc*, tcp_fastopen* | 网络压力/高级 TCP |

### 非 NET 范围

| 测例 | 原因 |
|------|------|
| fork*/clone*/exec*/wait*/signal* | 进程管理 |
| futex* | 同步原语 |
| timer*/clock* | 时间 |
| mmap*/mprotect*/madvise* | 内存管理 |

---

## 变更记录

| 日期 | 变更内容 |
|------|----------|
| 2026-05-22 | 创建文档, 本地摸底 106 个 NET 二进制, 定义 3 个 Round |
