---
title: "2K1000LA glibc 域名解析：IP_RECVERR 与 sendmmsg ABI 断链复盘"
category: debug
status: resolved-with-known-limits
author: MangoCore Team
last_update: 2026-07-15
tags: [postmortem, la64, 2k1000la, glibc, dns, resolver, ip-recverr, sendmmsg]
code_paths:
  - "os/src/net/posix.rs"
  - "os/src/net/socket/inet/datagram/udp.rs"
  - "os/src/net/syscall/sendmmsg.rs"
  - "os/src/net/syscall/setsockopt.rs"
  - "os/src/net/syscall/getsockopt.rs"
  - "os/src/syscall/syscall_id.rs"
  - "user/src/bin/inet_test.rs"
related_docs:
  - "docs/09_debug/la64_on_board/260710/11-dhcp-irq-lock-order.md"
  - "docs/09_debug/la64_on_board/260710/12a-https-build-epoch-and-ca-validation.md"
  - "docs/06_net/dhcp.md"
evidence:
  - "commit b96ab997"
  - "docs/Work_Log.md, 2026-07-13 glibc DNS/curl entry"
---

# 2K1000LA glibc 域名解析：IP_RECVERR 与 sendmmsg ABI 断链复盘

## 0. 一句话结论

板卡网络、DHCP 和 DNS 服务器都已经可用；glibc <code>curl</code> 仍无法按域名访问，并不是“DNS 包收不到”，而是解析器在发包前依赖的 Linux socket ABI 不完整：

1. glibc 先对 UDP socket 设置 <code>SOL_IP/IP_RECVERR</code>；
2. 随后用系统调用 269 <code>sendmmsg</code> 批量发送 A/AAAA 查询；
3. 当选项返回 <code>ENOPROTOOPT</code>、系统调用返回 <code>ENOSYS</code> 时，解析流程在 DNS 线包出现前终止。

修复的核心不是给 <code>curl</code> 加特判，而是补齐解析器真实经过的 ABI：保存并回读 <code>IP_RECVERR</code> 状态、空错误队列返回 <code>EAGAIN</code>，以及按 Linux 64 位布局实现 <code>mmsghdr</code> 和 <code>sendmmsg</code> 的部分成功语义。提交 <code>b96ab997</code> 后，板上 glibc <code>curl</code> 能通过 DHCP 下发的 DNS 解析域名并完成 HTTP 访问。

本文只闭环“域名解析 ABI”。HTTPS 的构建时间功能性回退、CA 与主机名正负门禁是
下一条独立证据链，见 <code>12a-https-build-epoch-and-ca-validation.md</code>。

---

## 1. 症状为什么容易误导

当时已经同时观察到：

- GMAC 链路已建立；
- DHCP 获得 <code>192.168.1.3/24</code>；
- 默认网关与 DNS 均为 <code>192.168.1.1</code>；
- 网关 ping 为 <code>2/2</code>；
- BusyBox <code>nslookup</code> 能解析 A 与 AAAA；
- glibc <code>curl http://域名</code> 却失败。

这组现象排除了“网线未通”“没有地址”“DNS 服务器不可达”等粗粒度假设，却不能推出“任何 libc 的 resolver 都能工作”。

关键区别是：

~~~text
BusyBox nslookup
  └─ 使用它自己的解析与发包路径

glibc curl
  └─ curl → getaddrinfo → glibc resolver
       ├─ setsockopt(IP_RECVERR)
       └─ sendmmsg(A 查询, AAAA 查询)
~~~

两者访问的是同一个 DNS 服务，却不共享同一套用户态 ABI 调用序列。因此“BusyBox 可解析”只证明 DNS 网络路径通，不证明 glibc 所需接口完整。

---

## 2. 分层定位：先确定断点在线包之前还是之后

### 2.1 四层排查模型

| 层次 | 要回答的问题 | 当时证据 |
|---|---|---|
| L1/L2 | PHY、MAC、DMA 是否收发 | 链路建立，已有 DHCP |
| L3 | 地址、路由、网关是否正确 | DHCP 地址、网关 ping 成功 |
| DNS 线协议 | 查询是否可达并能收到应答 | BusyBox A/AAAA 成功 |
| libc ABI | glibc 是否能走到真正的 DNS send | <code>IP_RECVERR</code>/<code>sendmmsg</code> 缺失 |

只有最后一层与症状吻合。

### 2.2 最关键的时序证据

glibc 路径的调用顺序是：

~~~text
socket(AF_INET, SOCK_DGRAM, ...)
  ↓
setsockopt(fd, SOL_IP, IP_RECVERR=1)
  ↓
sendmmsg(fd, [A query, AAAA query], ...)
  ↓
recv... / poll...
~~~

旧内核在第二步或第三步返回错误，故解析器停止。这里有一个非常重要的判断：

> 若断点发生在 <code>sendmmsg</code> 之前，就不应把“抓不到 DNS 请求”解释成驱动丢包；用户态根本没有把查询交给 UDP 发送路径。

---

## 3. 调试追溯

### 3.1 第一阶段：用 BusyBox 证明网络和 DNS 服务端

板上获得 DHCP 配置后，先验证地址、网关 ping 和 BusyBox A/AAAA 解析。这一步把故障域从 GMAC、ARP、路由、DNS 服务端缩到 glibc 专属路径，而不是宣告“DNS 功能全部完成”。

### 3.2 第二阶段：把 curl 失败拆到 resolver 系统调用

跟踪 glibc 的解析调用，识别到两个先决条件：

- <code>setsockopt(SOL_IP, IP_RECVERR=11)</code>；
- LoongArch64 系统调用号 269 的 <code>sendmmsg</code>。

旧实现的行为分别是：

| 调用 | 旧行为 | 对 glibc 的影响 |
|---|---|---|
| <code>IP_RECVERR</code> | 未支持，返回 <code>ENOPROTOOPT</code> | resolver 初始化/发送准备失败 |
| <code>sendmmsg(269)</code> | 未注册，返回 <code>ENOSYS</code> | A/AAAA 查询未按预期提交 |

这两个错误比“could not resolve host”更接近根因，因为它们明确指出用户态与内核 ABI 的断面。

### 3.3 第三阶段：先写最小 ABI 自测，再回到真实 glibc

增加两个独立的 <code>inet_test</code>：

- <code>net_core_ip_recverr</code>：
  - UDP socket 默认值为 0；
  - enable 后 <code>getsockopt</code> 读回 1；
  - 空 <code>MSG_ERRQUEUE</code> 返回 <code>EAGAIN(11)</code>；
  - disable 成功。
- <code>net_core_sendmmsg</code>：
  - <code>vlen=0</code> 返回 0；
  - 一次提交两条 loopback UDP datagram；
  - 返回值为 2；
  - 两个 <code>msg_len</code> 写回真实长度；
  - 接收端按内容收到两条 datagram。

这一步分别验证“选项状态机”和“批量消息 ABI”，而不是只看最终应用是否偶然绕过。

---

## 4. 底层原理一：IP_RECVERR 不是普通 no-op

### 4.1 Linux 语义中的两部分

<code>IP_RECVERR</code> 表示 UDP socket 是否接收异步网络错误。完整 Linux 实现通常包括：

1. socket 上的 enable/disable 状态；
2. ICMP 等错误转换成 <code>sock_extended_err</code>；
3. 用户通过 <code>recvmsg(MSG_ERRQUEUE)</code> 读取错误队列。

本次为 glibc resolver 补齐的是最小兼容闭环：

- UDP socket 存储布尔状态；
- <code>setsockopt</code> 设置状态；
- <code>getsockopt</code> 返回状态；
- 当前没有异步错误时，<code>MSG_ERRQUEUE</code> 返回 <code>EAGAIN</code>。

### 4.2 为什么“直接成功但不保存”不够

仅让 <code>setsockopt</code> 返回 0，会出现状态不自洽：

~~~text
setsockopt(IP_RECVERR=1) → 0
getsockopt(IP_RECVERR)   → 仍为 0 或不支持
~~~

用户态若回读状态或进入错误队列分支，就会看到互相矛盾的行为。因此采用“有状态最小实现”，而不是无条件吞掉选项。

### 4.3 明确的能力边界

本修复没有声称已经实现完整 Linux UDP extended error queue：

- 未把 ICMP unreachable 等事件排队为 <code>sock_extended_err</code>；
- 未证明控制消息布局、origin/type/code/info/data 全部兼容；
- 只证明 glibc 当前解析路径需要的状态与空队列语义成立。

---

## 5. 底层原理二：mmsghdr 的 64 位 ABI 布局必须逐字节一致

### 5.1 结构关系

Linux 的 <code>mmsghdr</code> 包含 <code>msghdr msg_hdr</code> 和 <code>unsigned int msg_len</code>。在本项目 64 位 ABI 中：

| 字段 | 偏移 | 大小 |
|---|---:|---:|
| <code>msg_hdr</code> | 0 | 56 |
| <code>msg_len</code> | 56 | 4 |
| 尾部 padding | 60 | 4 |
| 单项总大小 | 0..63 | 64 |

内核中的 <code>MMsgHdr</code> 使用 <code>#[repr(C)]</code>，并在 64 位目标显式保留 4 字节 padding。内核以 <code>index × sizeof(MMsgHdr)</code> 定位下一项，步长错误会从第二个消息开始把用户指针、iov 长度等字段全部错读。

### 5.2 两类典型布局错误

若错误地把步长当成 60：

~~~text
entry[1] 实际地址 = base + 64
内核错误地址      = base + 60
~~~

第二项会从前一项 padding 处开始解释。

若错误地写回 <code>msg_len</code> 偏移，datagram 可能已经发出，但长度覆盖错误字段，glibc 会误判哪些查询已提交，甚至污染下一项。因此实现使用 <code>size_of</code> 和 <code>offset_of</code>，不手写偏移魔数。

---

## 6. 底层原理三：sendmmsg 不是简单的 for 循环

### 6.1 本次实现的必要语义

<code>sys_sendmmsg</code> 对每一项复用已有 <code>sys_sendmsg</code> 校验与阻塞语义，并保证：

1. <code>vlen == 0</code> 返回 0；
2. MangoCore 为控制内核循环开销，将 <code>vlen</code> 上限定为 1024，超过即返回
   <code>EINVAL</code>；
3. <code>index × entry_size</code> 与基地址相加使用 checked arithmetic；
4. 每项成功后写回该项 <code>msg_len</code>；
5. 第一项失败返回原错误；
6. 已成功若干项后再失败，返回成功项数，即 short batch。

### 6.2 为什么 partial success 必须返回计数

假设批量为三项，前两项已进入网络栈，第三项失败：

~~~text
[msg0: sent] [msg1: sent] [msg2: error]
~~~

若返回第三项负错误，调用者可能重发整个批次，造成前两项重复。返回 2 后，调用者从第三项继续，才与实际副作用一致。

### 6.3 地址溢出为什么也要纳入证明

用户可控的 <code>msgvec</code> 和 <code>vlen</code> 参与地址计算。普通 wrapping arithmetic 可能把高地址绕回合法低地址。实现对乘法与加法逐步检查；首项前失败返回 <code>EFAULT</code>，已有成功项时按 short batch 返回计数。

### 6.4 与 Linux 上限行为的已知偏差

<code>1024</code> 是 MangoCore 当前的有界实现选择，用于避免一次 syscall 在内核中
遍历不受控的大批量；它不能写成“完全 Linux ABI 对齐”。Linux 对批量大小的限制与
错误行为并不等同于这里的固定 <code>vlen &gt; 1024 → EINVAL</code>。本轮 glibc
resolver 的 A/AAAA 两项批量远低于该上限，所以功能闭环成立；若应用依赖更大批量，
仍需单独按 Linux 行为校准并补边界测试。

---

## 7. 根因证明

### 7.1 事实、推导与结论

| 类型 | 内容 |
|---|---|
| 实测事实 | DHCP、网关、BusyBox A/AAAA 解析均成功 |
| 源码事实 | glibc 路径调用 <code>IP_RECVERR</code> 与 <code>sendmmsg(269)</code> |
| 旧内核事实 | 前者未支持、后者未注册 |
| 修复后自测 | 选项状态/空错误队列与两条批量 UDP 均通过 |
| 修复后板测 | glibc curl 按域名访问两个 HTTP 站点成功 |
| 结论 | 断点是 resolver ABI，不是 DNS 线协议或 GMAC 丢包 |

### 7.2 最小充分反证

若根因是 DNS 服务或外网不可达，则仅补本地 <code>IP_RECVERR/sendmmsg</code> 不应使同一块板、同一网络、同一 DNS 配置下的 glibc 域名访问转为成功。

实际只补 ABI 后，板上出现：

- <code>curl http://www.baidu.com</code>：HTTP 200，2381 B；
- <code>curl http://example.com</code>：HTTP 200，559 B。

这形成“旧错误点 → ABI 修复 → 自测通过 → 真实应用通过”的闭环。

---

## 8. 曾考虑但不足以闭环的做法

### 8.1 “BusyBox nslookup 通过，所以 DNS 已完成”

只能证明 DNS 服务与另一条客户端实现可用，无法覆盖 glibc resolver ABI。

### 8.2 “IP_RECVERR 直接 no-op 成功”

enable 后无法一致回读，也没有空错误队列语义；应用一旦观察状态就会暴露矛盾。

### 8.3 “把 sendmmsg 全部拆成 sendto”

会丢失 <code>msghdr</code> 的 iovec、name、flags 语义，无法正确写回每项 <code>msg_len</code> 和表达部分成功，还可能绕过 <code>sendmsg</code> 已有校验。

### 8.4 “curl 改用 IP 地址”

这是绕过 resolver，不是修复 resolver；也不能服务其他依赖 <code>getaddrinfo</code> 的 glibc 程序。

---

## 9. 验证矩阵

| 环境 | 用例 | 结果 | 证明范围 |
|---|---|---|---|
| 内核自测 | <code>IP_RECVERR</code> 默认/启用/回读/禁用 | 通过 | socket option 状态 |
| 内核自测 | 空 <code>MSG_ERRQUEUE</code> | <code>EAGAIN</code> | 当前无异步错误 |
| loopback | <code>sendmmsg(vlen=0)</code> | 0 | 边界语义 |
| loopback | 一批两条 UDP | 返回 2，长度与内容一致 | 布局、发送、写回 |
| 2K1000LA | DHCP 与网关 | <code>192.168.1.3/24</code>，ping 2/2 | 网络前置条件 |
| 2K1000LA | BusyBox nslookup | A/AAAA 成功 | DNS 线协议 |
| 2K1000LA | glibc curl + 域名 | 两站 HTTP 200 | resolver 真实链路 |

当时 QEMU 全链验证受缺少 <code>disk-la.img</code> 阻塞；不能把板测成功改写成“双环境全部覆盖”。本问题的板上闭环成立，但该历史检查点的 QEMU 镜像缺口应保留。

---

## 10. 修复边界与后续风险

本次已经证明：

- glibc 当前 resolver 可设置/读取 <code>IP_RECVERR</code>；
- 没有排队错误时得到 <code>EAGAIN</code>；
- 64 位 <code>mmsghdr</code> 布局和两项批量发送工作；
- 板上 glibc 域名 HTTP 访问成功。

本次没有证明：

- 完整 ICMP extended error queue；
- <code>recvmmsg</code>；
- 大批量、多 iovec、并发关闭等全部压力边界；
- <code>vlen &gt; 1024</code> 的 Linux 兼容行为；当前 <code>EINVAL</code> 是
  MangoCore 的有界实现偏差；
- HTTPS 证书可信性；
- 所有 glibc/NSS 配置组合。

---

## 11. 可复用调试方法

遇到“工具 A 能解析、工具 B 不能解析”时：

1. 用 IP 直连确认 L3/L4；
2. 用简单 DNS 客户端确认线协议；
3. 记录失败程序在第一条 DNS 包之前的系统调用；
4. 对每个缺失 ABI 写最小自测；
5. 布局结构检查大小、偏移、对齐和数组步长；
6. 批处理调用验证部分成功与结果写回；
7. 最后回到原应用，不以替代客户端通过代替验收。

~~~text
线上没有包，不等于驱动丢了包；
先确认用户态是否真的跨过了 ABI 边界。
~~~

---

## 12. 最终证据链

~~~text
DHCP / 网关 / BusyBox DNS 成功
  ↓ 排除基础网络与 DNS 服务端
glibc 在 DNS send 前调用 IP_RECVERR + sendmmsg
  ↓
旧内核分别返回 ENOPROTOOPT / ENOSYS
  ↓
补有状态 IP_RECVERR + 空 ERRQUEUE=EAGAIN
补 64-bit mmsghdr + sendmmsg partial-success 语义
  ↓
独立 loopback ABI 自测通过
  ↓
同一板卡 glibc curl 域名 HTTP 200
  ↓
根因闭环：glibc resolver ABI 缺口
~~~

对应修复提交：<code>b96ab997 feat(board): add curl and glibc DNS support</code>。
