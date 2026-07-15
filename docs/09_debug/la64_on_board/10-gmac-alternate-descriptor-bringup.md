---
title: "2K1000LA GMAC：alternate descriptor 首包停转复盘"
category: debug
status: resolved
author: MangoCore Team
last_update: 2026-07-15
tags: [postmortem, la64, 2k1000la, gmac, dwmac, dma, descriptor, phy]
code_paths:
  - "os/src/drivers/net/gmac_2k1000.rs"
  - "os/src/drivers/net/mod.rs"
  - "os/src/net/config.rs"
  - "os/src/net/adapter.rs"
related_docs:
  - "docs/09_debug/la64_on_board/development-log.md"
  - "docs/07_driver/2k1000-gmac.md"
  - "docs/06_net/device-stack-and-poll.md"
entry_points:
  - "Gmac2k1000::new"
  - "GmacInner::receive"
  - "GmacInner::transmit"
evidence_commits:
  - "1ace76e5"
evidence_records:
  - "docs/Work_Log.md, 2026-07-12 GMAC bring-up entry"
---

# 2K1000LA GMAC：alternate descriptor 首包停转复盘

## 1. 一句话结论

GMAC 首包后停转不是 PHY、RGMII 或 smoltcp 故障，而是软件按错误的 DWMAC
descriptor 位布局设置 chain/first/last；硬件没有识别链式描述符，于是从 ring
基址线性走到 <code>base + 0x10</code>，而软件槽距是 64 字节。切换到厂商
U-Boot 明确启用的 alternate descriptor 位定义后，DMA 才按 next 指针跨槽推进并
回绕。

| 问题卡 | 结论 |
|---|---|
| 故障阶段 | 2K1000LA GMAC0 首次接入 |
| 外部现象 | 能见到首个 RX/TX，随后 OWN 不再按软件 ring 推进 |
| 直接根因 | normal 与 alternate descriptor 位布局混用 |
| 决定性证据 | DMA current RX descriptor 从基址前进到 <code>+0x10</code> |
| 硬件真值 | 星云板 U-Boot 定义 <code>CONFIG_DW_ALTDESCRIPTOR</code> |
| 修复提交 | <code>1ace76e5</code> |
| 首轮闭环 | alternate ring 回绕、FCS 长度修正、ARP/ICMP 实板通过 |
| 非本问题 | 后续 8 项 RX ring 吞吐饥饿，见专题 13 |

## 2. 为什么“链路是 up”仍然不能证明驱动正确

GMAC bring-up 至少有五层，每一层的成功只证明本层：

1. PHY/MDIO：能读出 PHY ID、协商速率和双工；
2. MAC：能读 DWMAC version、MAC 地址，配置收发使能；
3. DMA 可达性：描述符和 buffer 的物理地址在控制器地址能力内；
4. descriptor 协议：OWN、chain、buffer、FS/LS 和 next 的格式必须完全一致；
5. 网络协议：以太网帧长度正确，smoltcp 才能完成 ARP、ICMP、TCP。

本故障发生在第 4 层。PHY 已经是 1000M/full，只能证明模拟前端和 RGMII 链路
建立，不能证明 DMA 会按软件理解的 ring 走。

## 3. descriptor 的底层机制

### 3.1 软件对象大小不等于硬件步长

驱动中的描述符对象按 64 字节对齐：

~~~text
offset +0x00  status
offset +0x04  control
offset +0x08  buffer
offset +0x0c  next
offset +0x10  padding ...
下一软件槽位：base + 0x40
~~~

硬件只认识前四个 32-bit word。它如何找到下一个描述符取决于 chain 位：

- chain 被识别：读取 <code>next</code>，跳到软件写入的下一个 64 字节槽；
- chain 未被识别：按硬件基本描述符长度 16 字节线性递增，落入软件 padding。

所以 <code>base + 0x10</code> 不是随机地址，而是“硬件把当前对象当作非链式
16 字节 descriptor”的直接指纹。

### 3.2 normal 与 alternate 的关键位不兼容

本板厂商 U-Boot 启用 alternate descriptor。最终源码中的硬件位为：

| 语义 | alternate 位置 |
|---|---:|
| RX chain | control bit 14 |
| TX chain | status bit 20 |
| TX first segment | status bit 28 |
| TX last segment | status bit 29 |
| RX first segment | status bit 9 |
| RX last segment | status bit 8 |
| OWN | status bit 31 |

如果把 normal 格式的 chain/FS/LS 位写进 alternate 控制器，字段值在内存中看起来
“非零”，但硬件不会把它解释成相同语义。descriptor 是硬件 ABI，不能靠 Rust 类型
安全弥补位号错误。

### 3.3 OWN 是 CPU/DMA 所有权协议

RX 路径：

~~~text
CPU 填 buffer/next/control
  → 最后写 OWN=1
  → DMA 收包并清 OWN
  → CPU 读状态和数据
  → CPU 最后重新写 OWN=1
~~~

TX 路径：

~~~text
CPU 写 payload
  → 写长度、FS、LS、chain
  → 最后写 OWN=1
  → DMA 发包并清 OWN
~~~

驱动在 OWN 交接前后执行 <code>dbar 0</code>。SoC DTS 虽声明 DMA coherent，
coherent 只解决缓存一致性，不自动保证“描述符字段先于 OWN 对设备可见”的顺序。

## 4. 调试追溯

### 4.1 第一阶段：先确定板级硬件真值

没有直接套用通用 DWMAC 地址，而是交叉检查厂商 DTS/U-Boot，再做只读探测。
确认：

- GMAC0 MMIO 为 <code>0x4004_0000</code>；
- GMAC1 MMIO 为 <code>0x4005_0000</code>；
- 两路 DWMAC version 均为 <code>0x0000d137</code>；
- 两路 YT8511H PHY ID 均为 <code>0x0000010a</code>；
- GMAC0 为 1000 Mbps/full，GMAC1 未接线；
- 厂商配置存在 <code>CONFIG_DW_ALTDESCRIPTOR</code>。

这一阶段排除了“MMIO 基址错”“PHY 地址错”“根本没有链路”三类上游错误。

### 4.2 第二阶段：首包出现后，问题从 PHY 下移到 DMA ring

调试镜像能够看到首个 RX 描述符被硬件填写，也能提交 TX；随后 ring 不再按软件
索引变化。若继续只看 link、ARP 超时或 OWN，会得到多个等价假设：

- PHY 丢链；
- DMA buffer 不可达；
- cache/barrier 错；
- smoltcp 没有继续 poll；
- descriptor next 没被采用。

为把这些假设拆开，诊断同时打印：

- 软件当前 index；
- 描述符基址和每槽 status；
- <code>DMA_CURRENT_RX_DESC</code>；
- <code>DMA_CURRENT_TX_DESC</code>；
- DMA status。

### 4.3 关键转折：current descriptor 到了 <code>base + 0x10</code>

提交 <code>1ace76e5</code> 同步沉淀到调试模式库中的现场关系是：

~~~text
ring base               = B
DMA_CURRENT_RX_DESC     = B + 0x10
software descriptor slot = 0x40
~~~

仓库保留的是这个相对地址关系，没有保留该轮绝对 ring 基址、status 原值和完整
串口逐行输出。本文因此只使用可追溯的 <code>B + 0x10</code>，不把未归档的绝对
地址重构成“原始日志”。

因此可直接排除：

- PHY 停止：PHY 不决定 DMA current descriptor 地址；
- smoltcp 未 poll：协议栈不会把硬件 current pointer 改成 16 字节步长；
- 单纯 cache 不一致：即使 next 值暂时不可见，也必须解释稳定的基本步长；
- buffer 地址错误：buffer 错会破坏 payload，不会把 descriptor 指针固定成
  <code>+0x10</code>。

最小解释只剩一个：硬件没有识别 chain 位，按非链式 descriptor 步长走进 padding。

### 4.4 对照实验：alternate 位布局后按软件 ring 推进

修正位号后，Work Log 记录 RX/TX alternate ring 能跨槽推进并回绕。current
pointer 可能已被 DMA 预取到后续槽，不能机械要求它永远等于软件
<code>next_index</code>；真正可追溯的验收是：

- 地址落在 64 字节槽集合，而不是 <code>+0x10</code> padding；
- RX index 0..7 都实际被消费；
- OWN 能交还并再次使用；
- TX 0..3 能轮转；
- ring 可回绕，不再首包后停住。

### 4.5 再下移一层：RX 长度包含 4 字节 FCS

ring 推进后，又确认 DWMAC 报告的 RX frame length 包含 4 字节 FCS：

~~~text
hardware frame length = Ethernet payload/frame + 4-byte FCS
smoltcp input length  = hardware length - 4
~~~

驱动因此只有在 RX 无 error、FS/LS 同时有效且长度至少覆盖以太网头和 FCS 时，
才复制数据，并在交给 smoltcp 前减 4。若不减，descriptor 已经正确，协议层仍可能
因帧尾多出 CRC 而丢包。

### 4.6 最后补齐 DMA 地址与顺序边界

DWMAC descriptor 地址寄存器为 32 bit。驱动对描述符页、RX buffer 页和 TX buffer
页逐一检查小于 <code>0x1_0000_0000</code>，并使用原始物理地址给 DMA，而 CPU
通过平台映射访问。

这一步不是本次 <code>+0x10</code> 的根因，但它封住了第二 DRAM bank启用后最容易
混入的独立故障：把超过 4 GiB 或 DMW 虚拟别名写给 DMA。

## 5. 证据矩阵

| 证据 | 观察 | 能证明什么 | 不能单独证明什么 |
|---|---|---|---|
| 厂商 U-Boot 配置 | <code>CONFIG_DW_ALTDESCRIPTOR</code> | 本板 descriptor 模式真值 | MangoCore 位号已经正确 |
| 只读探测 | DWMAC <code>0xd137</code>、PHY <code>0x10a</code> | MMIO/MDIO/PHY 基本可达 | DMA ring 正确 |
| 错误镜像 current pointer | <code>base + 0x10</code> | chain 未生效，硬件线性走基本 descriptor | 是 RX 还是 TX 所有位都正确 |
| 源码 <code>1ace76e5</code> | RX chain bit14；TX chain bit20；FS/LS 28/29 | 软件已按 alternate 格式构造 | 实板一定回绕 |
| 修复后 ring 日志 | 0..7 RX、0..3 TX 推进并复用 | OWN/next/ring 形成闭环 | 高吞吐下 ring 容量足够 |
| 长度语义 | Work Log 与驱动均记录 RX 长度含 4 字节 FCS | 交给 smoltcp 前必须减 4 | 所有错误帧处理都完整 |
| ARP/ICMP | 首轮 9/10、后续 19/20，MAC 可学习 | 从驱动到协议层端到端可用 | TCP 吞吐无瓶颈 |

主要事实源：

- 提交 <code>1ace76e5 feat(board): advance 2K1000 full-system bring-up</code>；
- <code>os/src/drivers/net/gmac_2k1000.rs</code>；
- <code>docs/Work_Log.md</code> 的 2026-07-12 GMAC 条目。

仓库当前没有保存包含该轮 current descriptor、完整 ring 推进和 ARP/ICMP 轮次的
原始串口日志。因此相对观察 <code>base + 0x10</code> 追溯到随提交保存的
<code>debugging-patterns.md</code>，ring/FCS/ARP/ICMP 结果追溯到 Work Log；本文
不把无关日志列为原始证据。

## 6. 根因证明

设 ring 基址为 B，软件槽距为 S：

~~~text
B = ring base
S = 0x40
软件期望槽地址集合 = { B + n × 0x40 }
~~~

错误现场：

~~~text
DMA_CURRENT_RX_DESC = B + 0x10
~~~

因为 <code>0x10 mod 0x40 != 0</code>，该地址不属于任何软件描述符槽；它正好等于
四个 32-bit word 的硬件基本 descriptor 长度。结合厂商 alternate 配置，得到唯一
与全部证据一致的链：

~~~text
使用了错误模式的 chain 位
  → 硬件认为 chain=0
  → 不读取 word3 的 next
  → current pointer 线性增加 0x10
  → 进入软件 padding
  → 后续 OWN/next 与软件视图失配
  → 首包后 ring 停转
~~~

修正 alternate 位后，地址重新落入 64 字节槽集合并完成回绕；这是对根因的反向
干预验证，不只是相关性。

## 7. 修复设计

提交 <code>1ace76e5</code> 的核心修复包括：

1. RX chain 固定为 control bit14；
2. TX chain 固定为 status bit20；
3. TX FS/LS 固定为 status bit28/29；
4. RX 只接受无 error 且 FS/LS 完整的单 descriptor 帧；
5. RX 长度扣除 4 字节 FCS；
6. 每个 descriptor 显式写 buffer 与 next；
7. OWN 作为最后一次 CPU 写，并在交接前后执行 barrier；
8. 所有 DMA 帧验证 32-bit 地址上限；
9. probe、逐包诊断和生产 feature 分离，避免诊断输出污染正式路径。

## 8. 明确拒绝的 workaround

| 做法 | 为什么拒绝 |
|---|---|
| 把 Rust 描述符槽距改成 16 字节 | 掩盖 chain 位错误，失去 64 字节布局和后续扩展空间；TX/RX 语义仍可能错 |
| 首包后反复重启 DMA | 只能重复消费第一个错误槽，不能建立 ring |
| 看到 link up 就改 smoltcp poll | current descriptor 的 <code>+0x10</code> 已证明故障在协议栈以下 |
| 忽略 FS/LS，任何 OWN 清零都上送 | 会把分段或错误帧当完整包，制造更隐蔽的数据破坏 |
| 把含 FCS 的硬件长度原样交给 smoltcp | 把硬件 FCS 语义泄漏到协议层 |
| 复制一份通用 DWMAC 常量表 | 同一 IP 在 normal/enhanced/alternate 模式下字段并不通用 |

## 9. 验证矩阵

| 验证层 | 结果 |
|---|---|
| 静态硬件真值 | DTS/U-Boot 与 MMIO、PHY、alternate 配置一致 |
| 描述符地址 | 修复后 current pointer 不再落入 <code>base+0x10</code> padding |
| RX ring | 8 个槽推进、OWN 交还、回绕 |
| TX ring | 4 个槽推进、完成后复用 |
| 帧边界 | 硬件 RX 长度扣除 4 字节 FCS |
| ARP | Mac 学到 <code>8e:f8:cc:05:ed:1b</code> |
| ICMP | 直连 ping 持续收发，无 TX OWN 卡死 |
| 构建 | rv64、la64 内核顺序编译通过；2K1000 诊断/生产目标通过 |

## 10. 边界与剩余风险

- 本轮完成的是 GMAC0 轮询驱动；GMAC1 未接线，也未进入验收。
- IRQ/NAPI 风格预算轮询尚未接入。
- 初始 8 RX/4 TX 只证明功能闭环，不证明能承受 1 Gbit/s 突发；后续确实触发
  RX ring starvation，见
  <code>13-gmac-rx-ring-starvation.md</code>。
- 驱动当前只接受单 descriptor RX 帧；巨帧或跨描述符聚合不是本轮范围。
- 诊断日志是多轮串口追加文件。引用时必须按启动 banner/ring 基址切分会话，不能把
  不同镜像的行拼成同一次运行。

## 11. 可复用排障流程

遇到“DWMAC 首包后停转”，按下面顺序处理：

1. 先读厂商 U-Boot/Linux 的实际编译配置，确认 descriptor 模式；
2. 只读探测 version、PHY ID、link，不急着改协议栈；
3. 同时打印软件 next 与 DMA current descriptor；
4. 检查 current 地址是否属于软件槽集合；
5. 若出现基本 descriptor 步长，优先核对 chain/TER/RER/FS/LS 位；
6. 修复后必须验证跨槽和回绕，而不只是“又收到一个包”；
7. 再检查 FCS、错误位、DMA mask、barrier；
8. 最后用 ARP/ICMP 做端到端验收；
9. 功能通过后另开吞吐实验，不把容量问题和格式问题混在一起。

## 12. 闭合证据链

~~~text
PHY 1000M/full + DWMAC/PHY ID 正确
  → 排除链路与基址
首包可达但 current = base + 0x10
  → DMA 活着，但没有采用软件 next
厂商 U-Boot 启用 alternate descriptor
  → normal 位布局不适用
改用 RX chain bit14、TX chain bit20、TX FS/LS bit28/29
  → current 回到 64 字节槽集合，RX/TX 跨槽并回绕
扣除 4 字节 FCS
  → ARP/ICMP 从帧边界到协议栈闭环
~~~

因此本问题可以结案为：**descriptor 硬件 ABI 模式不一致，而非 PHY 或网络协议
故障。**
