---
title: "2K1000LA RAW socket：外网 ping 稳定 DUP 复盘"
category: debug
status: resolved
author: MangoCore Team
last_update: 2026-07-15
tags: [postmortem, la64, 2k1000la, raw-socket, icmp, ping, dup, multi-interface]
code_paths:
  - "os/src/net/socket/inet/raw/raw.rs"
  - "os/src/net/socket/mod.rs"
  - "os/src/net/config.rs"
  - "os/src/net/routing.rs"
related_docs:
  - "docs/09_debug/la64_on_board/260710/development-log.md"
  - "docs/09_debug/la64_on_board/260710/11-dhcp-irq-lock-order.md"
  - "docs/06_net/raw.md"
  - "docs/06_net/routing.md"
entry_points:
  - "RawSocket::new"
  - "RawSocket::handler_for_ifindex"
  - "RawSocket::try_send_to"
  - "RawSocket::try_recv_from"
  - "RawSocket::socket_r_ready"
  - "wake_raw_waiters"
evidence_commits:
  - "6ae5c274"
---

# 2K1000LA RAW socket：外网 ping 稳定 DUP 复盘

## 1. 一句话结论

ping 的 DUP 不是 GMAC 线上双发，而是同一逻辑 RAW socket 在 lo、eth0 各预创建
handler 后，又把主 lo handler rebind 到已经有 handler 的 eth0，导致一个 ICMP
reply 被同一 DeviceStack 中两个 handler 各入队一次。修复为“handler 与 ifindex
绑定保存，发送只选择已有 handler，永不迁移”。

| 问题卡 | 结论 |
|---|---|
| 现象 | lo 正常；网关和公网每个序号稳定多一个 DUP |
| 直接根因 | eth0 上出现两个属于同一逻辑 socket 的 RAW handler |
| 重复层级 | DMA 收包之后、用户态 read 之前的协议交付层 |
| 决定性对照 | <code>127.0.0.1</code> 无 DUP，真实接口稳定 DUP |
| 修复提交 | <code>6ae5c274</code> |
| 修复后 | 网关、公网、域名 4/4，0% 丢包，无 DUP |

## 2. 底层原理：一个逻辑 socket 可以有多个栈内 handler

MangoCore 有多个 DeviceStack：

~~~text
ifindex 1: lo
ifindex 2: eth0
未来还可能有 veth
~~~

用户看到一个 fd/RawSocket，但 smoltcp 的 raw socket 对象属于某个具体
SocketSet。为了让同一逻辑 socket 能从多个接口收包，内核为每个 stack 创建一个
handler：

~~~text
logical RawSocket
  ├─ (ifindex=1, lo handler)
  └─ (ifindex=2, eth0 handler)
~~~

这是一对多所有权关系。正确策略只能二选一：

1. 每接口预创建，发送时选择对应 handler；或
2. 只保留一个 handler，路由变化时迁移它。

旧实现同时用了两种策略，才制造重复对象。

## 3. 现场与分层排除

### 3.1 初始观察

DHCP 网络建立后：

- ping <code>127.0.0.1</code> 严格一发一收；
- ping 网关时每个序号稳定多一份 reply；
- ping 公网 IPv4 同样稳定 DUP；
- UDP/DNS 正常；
- GMAC TX/RX ring 没有同一帧重复提交证据。

### 3.2 为什么先做 lo/eth0 对照

| 假设 | lo 应有表现 | eth0 应有表现 | 与现场是否一致 |
|---|---|---|---|
| ping 用户程序重复发送 | 通常也重复 | 重复 | 否 |
| ICMP 通用解析重复 | lo 也重复 | 重复 | 否 |
| 交换机/对端偶发双回包 | lo 正常 | 不应每序号稳定恰好多一份 | 弱 |
| GMAC TX 双提交 | lo 正常 | ring/线上可见双发 | 无硬件证据 |
| eth0 内有两个 RAW handler | lo 正常 | 每 reply 交付两次 | 完全一致 |

稳定性和接口相关性把故障边界从“网络线路”缩到“多接口 RAW 对象管理”。

### 3.3 代码审计找到矛盾的两套策略

创建路径已经遍历全部 stack：

~~~text
RawSocket::new
  → add handler on lo
  → add handler on eth0
~~~

旧发送路径又根据 route 调用：

~~~text
rebind_routed_raw(primary_handler, target_ifindex)
~~~

primary 默认是 lo。发往网关时 target 是 eth0，于是：

~~~text
迁移前：
  lo:   primary
  eth0: existing

迁移后：
  lo:   none
  eth0: migrated primary + existing
~~~

一个 eth0 reply 同时匹配两份 handler。

## 4. 根因证明

设某逻辑 RAW socket 在出口 stack 中的匹配 handler 数为 N。创建后 eth0 已有：

~~~text
N = 1
~~~

旧 rebind 再迁入 lo primary：

~~~text
N = 2
~~~

smoltcp 对每个匹配 handler 各交付一次：

~~~text
用户态收到副本数 = N = 2
ping 标记的额外副本数 = 1
~~~

这精确解释“每个序号稳定一个 DUP”，而不是不定量的相关性。

修复后发送路径只做：

~~~text
handler_for_ifindex(eth0) → existing handler
N 始终为 1
~~~

网关和公网 DUP 同时消失，构成反向干预验证。

## 5. 修复设计

### 5.1 保存 handler identity

旧字段：

~~~text
Vec<RouteSocketHandle>
~~~

新字段：

~~~text
Vec<(u32 ifindex, RouteSocketHandle)>
~~~

handle 不再脱离所属 stack。路由选择得到 ifindex 后，
<code>handler_for_ifindex</code> 直接选择现有对象。

### 5.2 删除 RAW rebind

<code>6ae5c274</code> 删除
<code>NetInterface::rebind_routed_raw</code>。发送不再创建新 handler、删除旧
handler 或移动 primary。

### 5.3 接收、ready 和唤醒必须一起修

只修发送会留下新的漏唤醒问题，因为全局 RAW registry 为标识逻辑 socket，只保存
primary handle。最终同步修改：

- <code>try_recv_from</code> 扫描全部接口 handler；
- <code>socket_r_ready</code> 扫描全部 handler；
- <code>send_ready</code> 检查任一可发送 handler；
- <code>wake_raw_waiters</code> 回到逻辑 RawSocket 调
  <code>recv_ready</code>，而不是只查 registry 中的 lo primary；
- Drop 删除该逻辑 socket 的全部 handler。

这保证对象 identity、数据路径和等待路径使用同一模型。

## 6. 证据矩阵

| 证据 | 观察 | 证明 |
|---|---|---|
| loopback ping | 无 DUP | 通用 ping/ICMP 路径不是无条件重复 |
| 网关与公网 | 每序号稳定 DUP | 重复与 eth0 路由绑定 |
| UDP/DNS | 正常 | 不是所有 socket 都被双交付 |
| GMAC ring | 无重复提交证据 | 优先排除驱动双发/双收 |
| 旧 <code>RawSocket::new</code> | 每 stack 预创建 handler | eth0 已经有一份 |
| 旧 <code>rebind_routed_raw</code> | primary 可迁移到 eth0 | eth0 产生第二份 |
| 新 <code>Vec<(ifindex, handle)></code> | 所属 stack 显式化 | 发送可选择而无需迁移 |
| 删除 rebind | <code>6ae5c274</code> diff | 重复对象来源被移除 |
| 修复后实板 | 网关、公网、域名 4/4 无 DUP | 根因反向验证闭环 |

原始逐包抓包文件未作为单独日志保存；“修复前后 ping 结果、GMAC 无重复提交”
来自 <code>docs/Work_Log.md</code> 2026-07-13 条目。本文不伪造不存在的 tcpdump
逐帧文本。

## 7. 为什么不是线上双发

如果线上真的有两份请求或两份 reply，至少应满足一项：

- GMAC TX descriptor 提交两次；
- GMAC RX descriptor 收到两份；
- 宿主抓包看到两份；
- 与 lo/eth0 无关，或重复数量随网络抖动变化。

现有事实是：

- 重复只在真实接口；
- 数量稳定为额外一份；
- 代码恰好推导出 eth0 两个 handler；
- 删除重复 handler 模型后现象消失。

因此“协议栈多 handler 重复交付”是最小且完备的解释。由于没有保存宿主逐帧抓包，
更严谨的说法是“源码 identity + 接口对照 + 修复反证已经闭环；若未来复发，再补
线上抓包作为额外门禁”，而不是声称本轮已有未保存的抓包。

## 8. 拒绝的 workaround

| 方案 | 为什么拒绝 |
|---|---|
| 用户态按 ICMP sequence 去重 | 掩盖内核重复交付，其他 RAW 应用仍错 |
| 驱动看到相同 payload 就丢弃 | 驱动无权按协议 socket identity 去重 |
| 只保留 lo primary，发送时继续迁移 | 接收接口切换和 readiness 仍不稳定 |
| 只删除 eth0 预创建 handler | 放弃每接口稳定所有权，veth/多接口更难扩展 |
| registry 为每个 handler 注册一个逻辑 waiter | 可能把一次就绪重复通知为多个逻辑事件 |
| 收到第一份后清空其他 handler | 有竞态，且会误丢不同接口的合法包 |

## 9. 验证矩阵

| 项目 | 修复前 | 修复后 |
|---|---|---|
| <code>127.0.0.1</code> | 一发一收 | 一发一收 |
| LAN 网关 | 每序号 DUP | 4/4、0% 丢包、无 DUP |
| 公网 IPv4 | 每序号 DUP | 4/4、无 DUP |
| 域名目标 | 依赖 DNS 后同类 DUP | 4/4、无 DUP |
| nslookup | 正常 | A/AAAA 正常 |
| GMAC ring | 无双提交证据 | 仍无双提交 |
| rv64/la64 编译 | — | 顺序通过 |

## 10. 边界与剩余风险

- 本轮重点覆盖 lo 与 eth0。veth 热插拔后是否为已存在 RAW socket 动态补 handler，
  需要按当前生命周期单独验证。
- registry 仍用 primary handle 表示逻辑身份，这是有意压缩；所有 readiness 路径
  必须继续回到 RawSocket 扫描全量 handler。
- 真正的线上重复仍可能发生。若未来只有特定网络环境复发，应同时记录 TX/RX
  descriptor 序号和宿主抓包，不应机械沿用本次结论。
- DHCP IRQ 锁序是独立问题，见
  <code>11-dhcp-irq-lock-order.md</code>。

## 11. 可复用调试流程

1. 用 loopback 与真实出口做接口对照；
2. 同时数用户态副本、TX descriptor、RX descriptor；
3. 若 DMA 一份、用户态多份，转查协议对象数量；
4. 画出一个逻辑 fd 到每个 DeviceStack handler 的映射；
5. 检查是否同时使用预创建和迁移；
6. 修复对象 identity 后同步审计 send/recv/ready/wakeup/drop；
7. 用网关和公网两个真实接口目标复验；
8. 保留抓包作为复发时额外证据，不用用户态去重遮盖。

## 12. 闭合证据链

~~~text
lo 一发一收
  + 网关/公网稳定每序号一个 DUP
  → 重复与 eth0 handler 路径绑定
GMAC 无重复提交，UDP 正常
  → 不是驱动或通用 socket 双发
创建时 lo/eth0 各预创建一个 RAW handler
  + 发送时又把 lo primary rebind 到 eth0
  → eth0 恰有两个匹配 handler
保存 (ifindex, handle)，发送只选择已有对象，删除 rebind
  → eth0 handler 数恢复为 1
修复后网关/公网/域名均无 DUP
  → 根因闭环
~~~

最终结论：**重复发生在逻辑 socket 到栈内 handler 的身份管理层，不是线路层。**
