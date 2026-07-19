---
title: "2K1000LA 常驻 DHCP：IRQ 锁序风险与两阶段 lease 提交设计"
category: debug
status: documented-design
author: MangoCore Team
last_update: 2026-07-15
tags: [postmortem, la64, 2k1000la, dhcp, irq, locking, routing, dns]
code_paths:
  - "os/src/net/config.rs"
  - "os/src/net/net_core.rs"
  - "os/src/net/routing.rs"
  - "os/src/task/manager.rs"
  - "os/src/fs/procfs/files/net_resolv.rs"
related_docs:
  - "docs/09_debug/la64_on_board/260710/development-log.md"
  - "docs/09_debug/la64_on_board/260710/11a-raw-socket-duplicate-delivery.md"
  - "docs/06_net/dhcp.md"
  - "docs/06_net/routing.md"
  - "docs/06_net/device-stack-and-poll.md"
entry_points:
  - "NetInterface::try_poll_irq"
  - "NetInterface::poll_once"
  - "take_dhcp_event"
  - "capture_dhcp_event"
  - "commit_dhcp_event"
  - "timer_interrupt_handler"
evidence_commits:
  - "6ae5c274"
---

# 2K1000LA 常驻 DHCP：IRQ 锁序风险与两阶段 lease 提交设计

## 1. 一句话结论

<code>NET_INTERFACE.try_lock()</code> 只能保证进入协议栈主锁时不在中断里等待，
不能让其后的 router/device/DNS 阻塞锁自动变成 IRQ-safe。引入常驻 DHCP 时，代码
没有先提交一个“IRQ 内跨子系统发布 lease”的危险版本再由现场死锁推动修复；永久
DHCP handle、<code>try_poll_irq</code>、pending event 与锁外 commit 都在
<code>6ae5c274</code> 同时落地。

两阶段设计是对可由调用图严格证明的锁序风险做主动规避：中断路径只推进协议状态机
并捕获最新 lease，任务上下文在释放协议栈锁后再发布 router/device/DNS 状态。本文
的等待图是“若直接复用普通提交路径会发生什么”的机制证明，不是已复现死锁现场。

| 问题卡 | 结论 |
|---|---|
| 场景 | 静态/初始化期 DHCP probe 升级为常驻 DHCP、续租和失租 |
| 设计风险 | 若 timer IRQ 消费 lease 后跨子系统取阻塞锁，会形成单核自等待 |
| 风险来源 | “主锁 try_lock”不能推出整个未来回调链 IRQ-safe |
| 设计 | <code>try_poll_irq</code> 只捕获；任务 poll 锁外 <code>commit_dhcp_event</code> |
| 一致性策略 | Configured/Deconfigured 使用 latest-wins |
| 设计提交 | <code>6ae5c274</code> |
| 实板闭环 | lease、connected/default route、动态 resolv.conf 均正确 |
| 证据边界 | 有调用图/锁序证明；没有危险旧提交、死锁复现、hang 日志或死锁 PC |

## 2. 背景：一次性地址探针为什么不够

GMAC 初次验收使用静态 <code>192.168.9.20/24</code>。静态配置只在初始化时写一次，
没有后续状态转换。

普通 LAN 需要常驻 DHCP socket，以处理：

- discover/request；
- lease renew/rebind；
- 网线拔出或 server 撤销后的 Deconfigured；
- 地址、网关和 DNS 的原子切换；
- 失租后重新 discover。

一次 lease 会同时影响五份状态：

~~~text
smoltcp interface IPv4 address
smoltcp default route
net_core 的 eth0 地址/默认网关/DNS
内核 net namespace router
/proc/net/resolv.conf → /etc/resolv.conf
~~~

这些对象不由同一把锁保护，不能在任意上下文里一次性更新。

## 3. 底层原理：中断上下文为何不能跨子系统提交

### 3.1 try_lock 只约束第一把锁

引入常驻 DHCP 时必须拒绝的错误推理：

~~~text
NET_INTERFACE.try_lock 不等待
  → try_poll 可在 IRQ 调用
  → try_poll 里的全部动作都中断安全
~~~

若把 lease 发布直接接到 IRQ poll，潜在调用链会是：

~~~text
try_lock(NET_INTERFACE) 成功
  → smoltcp poll
  → DHCP Configured/Deconfigured
  → 更新 net_core
  → lock(router)
  → 更新 DNS / 打印状态
~~~

第一把锁不等待，不代表后续锁不等待。

### 3.2 假设性不安全实现的单核等待图

以下是设计审计中的反例，不是仓库曾提交或日志曾捕获的现场。假设普通任务 T 已
持有 router 锁，timer IRQ 在此时打断 T，而开发者又让 IRQ 直接 commit lease：

~~~text
任务 T:
  lock(router)
  ... timer IRQ ...

IRQ:
  try_lock(NET_INTERFACE) 成功
  smoltcp poll 产生 Configured
  commit lease
  lock(router)  ← router 仍由 T 持有
~~~

若 router 锁为自旋/不可睡眠等待，IRQ 会等待 T；T 只有 IRQ 返回后才能继续并释放
router：

~~~text
IRQ 等 T 释放 router
T 等 IRQ 返回
=> 永久循环等待
~~~

单核上没有另一个 CPU 帮忙推进被打断任务，因此这种假设实现会产生“跨上下文锁序”
问题，而不是普通锁竞争。该图证明为什么设计必须分层，不证明历史上已经命中该环。

### 3.3 协议栈锁内也不应直接提交

即使在任务上下文，若持有 NET_INTERFACE 内锁再取 router 锁，其他路径若先取
router 再进入网络接口，就可能形成 ABBA。正确边界是：

~~~text
持 NET_INTERFACE 锁：
  只修改所属 DeviceStack 内状态
  把待提交事件移出

释放 NET_INTERFACE 锁：
  再更新 net_core/router/DNS
~~~

因此最终设计同时规避 IRQ 上下文约束和普通任务的嵌套锁序。

## 4. 演进与设计追溯

### 4.1 第一阶段：从启动探针改为常驻 DHCP handle

<code>6ae5c274</code> 的父提交只有初始化阶段的临时 DHCP probe：创建 DHCP
socket，得到地址后删除。它可以证明 server 可达，却无法维持续租/失租；由于
永久 handle 尚不存在，timer poll 也没有一条已提交的“消费长期 lease 后跨子系统
commit”路径。

提交 <code>6ae5c274</code> 在 <code>DeviceStack</code> 增加：

~~~text
dhcp_handle: Option<SocketHandle>
pending_dhcp_event: Option<DhcpLeaseEvent>
~~~

handle 表示状态机长期存在；pending slot 表示跨上下文提交边界。

### 4.2 第二阶段：把事件分成栈内应用与跨栈发布

<code>take_dhcp_event</code> 在仍持协议栈锁时只处理 DeviceStack 自己拥有的对象：

- 替换 smoltcp IPv4 地址；
- 添加或删除 smoltcp default route；
- 把事件内容复制成内核自有值。

它不在这一阶段获取 net namespace router 锁。

### 4.3 第三阶段：capture 只保存最新状态

<code>capture_dhcp_event</code> 把事件放入
<code>pending_dhcp_event</code>。为什么不是 Vec 队列？

DHCP lease 是当前状态。如果在任务提交前发生：

~~~text
Configured(A) → Deconfigured
~~~

依次提交会短暂发布已经失效的 A。latest-wins 直接覆盖：

~~~text
pending = Configured(A)
pending = Deconfigured
任务上下文只见 Deconfigured
~~~

它同时避免 IRQ 中分配无界队列。

### 4.4 第四阶段：在引入常驻状态机时同时拆出 IRQ poll

父提交的 timer handler 已调用普通网络 poll：

~~~text
NET_INTERFACE.try_poll()
~~~

但当时只有临时 probe，不能把它描述成“已提交的危险永久 DHCP”。同一提交在增加
常驻 DHCP handle 的同时把 timer 路径改为：

~~~text
NET_INTERFACE.try_poll_irq()
~~~

两条路径的语义是：

| 路径 | 进入方式 | 消费 DHCP | 保存 pending | 提交 router/DNS |
|---|---|---|---|---|
| <code>try_poll_irq</code> | try_lock | 是 | 是 | 否 |
| <code>try_poll</code>/<code>poll</code> | 任务上下文 | 是 | 是 | 释放主锁后是 |

这不是事后修复一份已复现死锁的旧实现，也不只是函数重命名；
<code>poll_once(false)</code> 从常驻 DHCP 首次落地起就明确禁止 IRQ 提交。

### 4.5 第五阶段：锁外统一发布 lease

普通任务 poll 在锁内收集：

~~~text
Vec<(ifindex, DhcpLeaseEvent)>
~~~

离开 <code>inner_handler</code> 后再逐项调用
<code>commit_dhcp_event</code>，更新：

- net_core 的 eth0 IPv4；
- default gateway；
- DNS server；
- net namespace router 的 connected/default route；
- 对外状态输出。

这条路径把“协议状态机推进”和“系统配置发布”变成两个明确事务边界。

## 5. 事实、机制推导与证据边界

为了不把主动风险规避写成伪造事故，证据分级如下。

### 5.1 直接事实

- <code>6ae5c274</code> 的父版本 timer IRQ 调普通
  <code>NET_INTERFACE.try_poll</code>，但 DHCP 只是初始化期临时 probe，获得地址后
  删除 handle；
- 父版本不存在“永久 DHCP event 在 IRQ 内跨子系统提交”的已提交路径；
- <code>6ae5c274</code> 同时新增永久 handle、<code>try_poll_irq</code>、
  pending event 与 <code>commit_dhcp_event</code>；
- timer IRQ 改调 <code>try_poll_irq</code>；
- task poll 释放接口锁后才调用 <code>commit_dhcp_event</code>。

### 5.2 可验证的设计风险

若在常驻 DHCP 设计中让 timer IRQ 直接进入跨子系统 commit，且任务持 router 锁时
被 timer 打断，等待图具备单核循环条件。这个结论来自锁所有权和调用图；它描述的
是被最终结构排除的候选实现，不是 <code>6ae5c274</code> 父提交的真实调用链。

### 5.3 不作出的声称

仓库没有常驻 DHCP 的危险旧提交，也没有“随机卡死”“hang”或 PC 停在
<code>router.lock</code> 的原始日志。因此本文不声称复现过死锁，不使用“事故直接
根因”措辞；依据是设计期调用图审计和最终实现的不变量。

## 6. 证据矩阵

| 证据 | 位置 | 结论 |
|---|---|---|
| 父提交临时 probe | <code>6ae5c274^: os/src/net/config.rs</code> | 当时没有永久 DHCP IRQ commit 路径 |
| timer 调用变更 | <code>6ae5c274: os/src/task/manager.rs</code> | 引入常驻 DHCP 时为 IRQ 建立非提交路径 |
| <code>pending_dhcp_event</code> | <code>os/src/net/config.rs</code> | lease 可跨上下文暂存 |
| <code>poll_once(commit_dhcp)</code> | 同上 | 是否提交由调用上下文显式决定 |
| 锁外 event Vec | 同上 | 提交发生在接口主锁释放后 |
| Configured→Deconfigured 注释 | 同上 | latest-wins 是刻意的一致性规则 |
| LAN lease | <code>docs/Work_Log.md</code> 2026-07-13 | 获得 <code>192.168.1.3/24</code> |
| route 输出 | 同上 | connected <code>192.168.1.0/24</code>，default via <code>192.168.1.1</code> |
| resolv.conf | 同上 | nameserver <code>192.168.1.1</code> |
| 无 server 路径 | 同上 | Deconfigured 后继续 discover，无 panic |

## 7. 结构性风险证明与设计充分性

一个不安全的“直接把永久 DHCP 接进旧 timer poll”模型会具备四个条件：

1. timer IRQ 调用普通网络 poll；
2. 普通 poll 可以消费 DHCP lease；
3. lease 提交获取 router/device/DNS 相关锁；
4. IRQ 可以打断已持有这些锁的任务。

四者同时成立时，会出现：

~~~text
task owns router
  → IRQ preempts task
  → IRQ waits router
  → task cannot resume
~~~

最终实现从首次引入常驻 DHCP 起就切断条件 3：

~~~text
IRQ 只写 DeviceStack.pending_dhcp_event
  → 不获取 router/device/DNS 锁
  → IRQ 返回
  → 持锁任务可继续并释放
  → 后续 task poll 锁外提交
~~~

因此这是结构性预防，而非延长 timeout 或降低轮询频率；功能验证证明两阶段设计
能够正确发布 lease，但由于没有先运行不安全版本，不能把“未死锁”当作事故复现后的
A/B 修复证据。

## 8. 设计时拒绝的替代方案

| 方案 | 拒绝原因 |
|---|---|
| IRQ 内 router.try_lock，失败就丢事件 | 会随机丢失 Configured 或 Deconfigured，系统状态分裂 |
| 关闭 timer 网络 poll | 破坏协议推进、续租和 socket timeout |
| 把 timer 频率调低 | 只降低命中概率，不消除锁环 |
| 对所有事件建无界队列 | 状态事件无需保留过时中间态，IRQ 分配和积压风险更大 |
| 持接口锁直接更新所有对象 | 保留 ABBA 风险，扩大临界区 |
| 收到 lease 后只改 smoltcp | /proc、路由查找和 resolver 会看到不同状态 |

## 9. 验证矩阵

| 项目 | 结果 |
|---|---|
| rv64 内核编译 | 通过 |
| la64 内核编译 | 通过 |
| 2K1000 DHCP uImage | 构建、TFTP 长度、CRC、iminfo 通过 |
| Mac 直连无 server | Deconfigured，继续 discover，不 panic |
| LAN discover | <code>192.168.1.3/24</code> |
| default gateway | <code>192.168.1.1</code> |
| connected route | <code>192.168.1.0/24</code> |
| DNS 发布 | <code>/etc/resolv.conf</code> 为 <code>192.168.1.1</code> |
| 网关/DNS 后续使用 | ping 与 nslookup 可继续进入下一阶段 |

QEMU 当时因缺少 <code>disk-la.img</code> 在固件启动前退出。文档保留这一环境缺口，
不把双架构编译写成 QEMU 行为通过。

## 10. 边界与剩余风险

- latest-wins 只适用于“当前状态”；数据包、审计事件、必须逐条确认的事务不能照搬。
- 当前网络驱动主要靠轮询。未来启用 GMAC IRQ 时，必须重新审计 IRQ 路径是否打印、
  分配或进入其他子系统。
- pending slot 若长期没有任务上下文 poll，事件会保留但不会发布；调度/轮询活性仍是
  系统前提。
- 多个 DHCP 接口同时存在时，提交必须继续携带真实 ifindex，不能硬编码 eth0。
- RAW ping DUP 是另一个独立根因，详见
  <code>11a-raw-socket-duplicate-delivery.md</code>。
- glibc resolver 还需要 <code>IP_RECVERR</code> 和
  <code>sendmmsg</code>，DHCP/DNS 地址可见不代表域名访问已经兼容。

## 11. 可复用锁序审计流程

1. 从 IRQ 入口画完整调用图，不只看第一把 try_lock；
2. 标出每个函数可能获取的锁、分配和输出；
3. 找出 IRQ 可能打断的持锁任务；
4. 对同一 CPU 建立等待图，检查“IRQ 等被打断任务”的环；
5. 把所属子系统内状态更新留在原锁内；
6. 把跨子系统提交移到释放主锁后的任务上下文；
7. 判断事件是状态还是事务，状态可 latest-wins，事务必须可靠排队；
8. 用 Configured→Deconfigured 连续转换验证不会发布 stale lease；
9. 验收必须同时看协议栈地址、系统路由和用户态 DNS 三个视图。

## 12. 闭合设计证据链

~~~text
初始化期临时 probe 只能证明一次配置
  → 常驻 DHCP 必须在运行期 poll
timer IRQ 也会 poll
  + lease 发布要获取 router/device/DNS 锁
  → 若直接接入，会产生可证明的锁序风险
  → 父提交尚无永久 DHCP，因此不是已发生事故
事件先写 pending，IRQ 不提交
  → IRQ 路径不再跨子系统取锁
任务 poll 释放接口锁后 commit
  → 假设等待环在最终结构中不可达
latest-wins
  → Configured 后紧随 Deconfigured 时不发布 stale lease
实板 lease + connected/default route + resolv.conf 一致
  → 两阶段提交功能闭环
~~~

最终结论：**常驻 DHCP 的关键不是修复一场已复现死锁，而是在首次引入运行期 lease
状态机时，就建立中断推进与系统状态发布之间可证明的上下文边界。**
