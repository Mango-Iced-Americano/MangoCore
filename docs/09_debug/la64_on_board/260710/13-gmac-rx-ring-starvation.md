---
title: "2K1000LA GMAC：8 项 RX ring 饥饿导致百倍吞吐退化复盘"
category: debug
status: resolved
author: MangoCore Team
last_update: 2026-07-15
tags: [postmortem, la64, 2k1000la, gmac, rx-ring, dma, ru, tcp, performance]
code_paths:
  - "os/src/drivers/net/gmac_2k1000.rs"
  - "os/src/net/config.rs"
  - "os/src/net/socket/inet/stream/mod.rs"
  - "os/src/net/socket/inet/stream/inner.rs"
related_docs:
  - "docs/09_debug/la64_on_board/260710/10-gmac-alternate-descriptor-bringup.md"
  - "docs/06_net/debugging.md"
  - "docs/06_net/device-stack-and-poll.md"
  - "docs/07_driver/2k1000-gmac.md"
evidence_commits:
  - "2031fd59"
evidence_logs:
  - "logs/net-perf-board-baseline-run-20260715.log"
  - "logs/net-perf-board-ack-run-20260715.log"
  - "logs/net-perf-board-ring48-run-20260715.log"
  - "logs/net-perf-board-production-run-20260715.log"
---

# 2K1000LA GMAC：8 项 RX ring 饥饿导致百倍吞吐退化复盘

## 0. 一句话结论

2K1000LA 本地 8 MiB HTTP 下载长期只有约 <code>129649 B/s</code>，不是公网代理慢、curl 缓冲、TCP delayed ACK 或坏帧导致，而是 1 Gbit/s 接收突发在轮询驱动重新挂回描述符前耗尽了仅 8 项的 RX ring。DWMAC 每个活跃统计窗口都新触发 <code>RU=1</code>，TCP 因连续丢包与重传把有效吞吐压到百 KiB/s。

把生产 ring 从 RX/TX <code>8/4</code> 扩到单页可容纳的 <code>48/16</code> 后：

- 三轮实验平均从 <code>129649 B/s</code> 提升到 <code>12286495 B/s</code>，约 94.77 倍；
- 活跃窗口 <code>RU</code> 从持续 1 变为 0；
- <code>OVF=0</code>、<code>RPS=0</code>、<code>rx_bad=0</code> 保持为 0；
- 正式无诊断镜像复测平均 <code>12529330 B/s</code>，相对旧生产基线约 96.64 倍。

性能阶跃、RU 消失和正式镜像复现共同闭环了“RX 描述符供给不足”这一根因。

---

## 1. 症状与最初的混杂变量

最初看到的是“板上公网下载慢”。这个表述同时混入：

- Mac 宿主到公网的直连/代理质量；
- Clash 显式代理；
- 板卡 GMAC；
- smoltcp/TCP；
- curl 的 UserBuffer；
- 串口命令完整性。

只在公网对象上测，任何一层都能解释低速，无法归因。

只读排查得到：

| 路径 | 观测 |
|---|---:|
| Mac 本地代理入口 | 大于 500 MB/s |
| 所选公网代理链，同一 PyPI 文件 | 约 105–153 KiB/s |
| Mac 直连公网 | 约 828 KiB/s |
| 板端本地 HTTP | 约 136–205 KiB/s |

公网代理确实慢，但板端访问同一局域网宿主仍只有百 KiB/s，说明板端还有一个独立瓶颈。此后所有根因 A/B 都改用局域网 8 MiB 文件，不再让公网波动参与主实验。

---

## 2. 调试方法：把一条慢链拆成四个对照面

~~~text
宿主本地文件 / HTTP 服务
       │
       ├─ Mac 直连：验证服务端与磁盘
       ├─ QEMU LA64：验证通用 TCP/UserBuffer 路径
       └─ 2K1000LA：验证 GMAC + 板端轮询

公网代理/HTTPS 只做修复后的外部回归，不做根因基线
~~~

这个拆分产生两个关键反证：

1. QEMU 同容器宿主下载约 20 MB/s，说明通用 TCP/curl 路径不可能天然只有 0.13 MB/s；
2. 板端局域网仍慢，说明公网代理不是板端百倍差距的解释。

---

## 3. 调试时间线

### 3.1 建立严格三轮 baseline

旧生产配置：

~~~text
RX descriptors = 8
TX descriptors = 4
TCP ACK         = delayed
对象            = 局域网 8 MiB 文件
~~~

三轮结果：

| 轮次 | 总时间/s | 速度/B/s |
|---:|---:|---:|
| 1 | 65.399424 | 128267 |
| 2 | 64.399995 | 130258 |
| 3 | 64.319294 | 130422 |
| 平均 | — | 129649 |

速度很稳定，排除单次网络抖动。

### 3.2 先排查 TCP delayed ACK

QEMU 对 71.9 MiB 局域网文件：

| 策略 | 三轮 MB/s | 平均 |
|---|---|---:|
| delayed ACK | 19.72 / 20.08 / 20.00 | 19.93 MB/s |
| immediate ACK | 19.67 / 19.81 / 19.65 | 19.71 MB/s |

约 1.1% 差异在噪声范围。

板上保持 <code>8/4</code> ring，只切 immediate ACK：

| 轮次 | 总时间/s | 速度/B/s |
|---:|---:|---:|
| 1 | 64.338184 | 130383 |
| 2 | 64.798826 | 129456 |
| 3 | 65.510832 | 128049 |
| 平均 | — | 129296 |

相对 baseline 反而低约 0.27%，并且新鲜 <code>RU/TU</code> 仍出现。因此 delayed ACK 不是主因。

### 3.3 排查 curl UserBuffer 与通用 TCP 路径

QEMU UserBuffer 统计：

~~~text
calls=672
bytes=43480800
avg_req=102400
eagain=1
完整传输约 19.71 MB/s
~~~

100 KiB 临时缓冲仍有优化空间，但相同路径在 QEMU 达到约 20 MB/s，不能解释板上约 0.13 MB/s。

### 3.4 修正 DMA_STATUS 的观测语义

早期看到 <code>RU/TU</code>，但 DWMAC 的 DMA_STATUS 事件位是 W1C：写 1 清除。若只读不清，某次历史事件会长期黏住：

~~~text
窗口 1 发生 RU → 状态位变 1
窗口 2 未发生 RU → 若不清仍读到 1
窗口 3 未发生 RU → 仍读到 1
~~~

所以旧日志只能证明“曾发生过”，不能证明“每个窗口持续发生”。

诊断实现每两秒：

1. 读 <code>DMA_STATUS</code>；
2. 记录 RU/OVF/RPS/TU；
3. 只把 <code>status & 0x1ffff</code> 写回；
4. 清除低 17 位 latched event，保留 process-state 等非事件信息；
5. 下一窗口读到 1 才表示新事件。

事件位：

| 名称 | bit | 含义 |
|---|---:|---|
| TU | 2 | TX buffer unavailable |
| OVF | 4 | RX overflow |
| RU | 7 | RX buffer unavailable |
| RPS | 8 | RX process stopped |

修正观测后，旧 <code>8/4</code> baseline 的每个活跃窗口仍新出现 <code>RU=1</code>，这才是可用于归因的证据。

### 3.5 扩大 ring 做结构性 A/B

实验配置改为：

~~~text
RX descriptors = 48
TX descriptors = 16
TCP ACK         = delayed（恢复 baseline）
其他测试路径    = 不变
~~~

三轮结果：

| 轮次 | 总时间/s | 速度/B/s |
|---:|---:|---:|
| 1 | 0.701419 | 11965606 |
| 2 | 0.674911 | 12435784 |
| 3 | 0.673702 | 12458094 |
| 平均 | — | 12286495 |

与此同时，活跃窗口：

~~~text
RU=0
OVF=0
RPS=0
rx_bad=0
~~~

<code>TU</code> 仍可偶见，说明 TX underflow 是另一个问题；它没有阻止接收吞吐恢复。

### 3.6 固化生产配置并去掉诊断

正式镜像默认使用 <code>48/16</code>，且不包含 <code>[net-perf]</code>、ACK 实验或 ring feature 字符串。启动日志确认：

~~~text
[gmac] rings ... rx=48 tx=16 link=up 1000M full
[net] DHCP configured ... 192.168.2.2/24
~~~

三轮正式结果：

| 轮次 | 总时间/s | 速度/B/s |
|---:|---:|---:|
| 1 | 0.679372 | 12353974 |
| 2 | 0.662406 | 12670560 |
| 3 | 0.668058 | 12563457 |
| 平均 | — | 12529330 |

无诊断镜像仍保留约 96.64 倍收益，排除了“计数代码或实验 feature 偶然改变调度”的解释。

---

## 4. 底层原理：为什么 8 项 ring 在 1 Gbit/s 下如此脆弱

### 4.1 OWN 位代表缓冲所有权

RX 描述符生命周期：

~~~text
CPU 填好 buffer/next，置 OWN
  ↓
DMA 收帧，写 buffer，清 OWN
  ↓
CPU 轮询发现 OWN=0，交给 smoltcp
  ↓
CPU 处理后重新置 OWN
~~~

若 DMA 到达下一帧时所有描述符都处于 CPU 所有，硬件没有可写 buffer，就置 RU 并停止/等待恢复。

### 4.2 ring 容量对应的突发时间预算

用最大约 1538 字节（含 FCS）估算 1 Gbit/s 线速：

~~~text
8 项预算  = 8 × 1538 × 8 / 1e9  ≈ 98.4 μs
48 项预算 = 48 × 1538 × 8 / 1e9 ≈ 590.6 μs
~~~

这里只是数量级估算，未计前导码、IFG 等线开销；结论不依赖精确值：8 项只给轮询侧约百微秒级回收窗口。单核在内核、TCP、文件写入、用户态 curl 间切换时很容易错过。

### 4.3 为什么丢包会把 TCP 降到百 KiB/s

RX ring 耗尽不会表现为 <code>rx_bad</code>：

- 帧还没被 DMA 放入软件可见 buffer；
- 驱动没有机会把它计为 checksum/FCS 坏帧；
- TCP 只看到报文缺口；
- 触发重复 ACK、重传和拥塞窗口收缩；
- ring 暂时恢复后又遭下一轮突发，形成稳定低速。

所以 <code>rx_bad=0</code> 与吞吐极低并不矛盾，反而符合“描述符供给之前丢失”。

---

## 5. 单页 ring 几何与 release 安全条件

软件 descriptor 槽距为 64 字节：

~~~text
48 RX × 64 B = 3072 B
16 TX × 64 B = 1024 B
总计          = 4096 B
~~~

恰好占一个 4 KiB 页。布局约束若只用 <code>debug_assert!</code>，release 构建可能静默越界；因此改为 release 仍生效的 <code>assert!</code>：

~~~text
TX_DESC_OFFSET + TX_DESC_COUNT × DESC_ALIGN <= PAGE_SIZE
~~~

这使未来调整 ring 数量时在初始化阶段立即失败，而不是覆盖相邻内存后随机损坏。

---

## 6. 根因证明矩阵

| 假设 | 关键实验 | 结果 | 判定 |
|---|---|---|---|
| 公网代理是全部瓶颈 | 板端局域网 HTTP | 仍 136–205 KiB/s | 排除为板端主因 |
| 通用 TCP/curl 太慢 | LA64 QEMU 局域网 | 约 20 MB/s | 排除百倍差距 |
| delayed ACK 主导 | QEMU/板端 immediate ACK A/B | -1.1% / -0.27% | 排除 |
| 坏帧/FCS 错误 | active window <code>rx_bad</code> | 始终 0 | 不支持 |
| RX ring 耗尽 | W1C 后 8/4 每窗 <code>RU=1</code> | 稳定复现 | 支持 |
| 扩 ring 只是偶然 | 48/16 RU=0 且 94.77x | 指标与吞吐同步反转 | 强支持 |
| 诊断 feature 改变性能 | 无诊断正式镜像 | 96.64x 复现 | 排除 |

最有区分度的不是“换一个参数后更快”，而是：

~~~text
旧配置：RU 持续出现 + 0.13 MB/s
新配置：RU 消失     + 12.3 MB/s
正式版：无诊断仍约 12.5 MB/s
~~~

---

## 7. 证据日志定位与使用边界

| 日志 | 有效证据 |
|---|---|
| <code>net-perf-board-baseline-run-20260715.log</code> | 8/4 三轮、活跃窗口新鲜 RU |
| <code>net-perf-board-ack-run-20260715.log</code> | 8/4 immediate ACK 三轮、RU/TU |
| <code>net-perf-board-ring48-run-20260715.log</code> | 48/16 三轮、RU=0 |
| <code>net-perf-board-production-run-20260715.log</code> | 正式启动 48/16、DHCP、正式三轮及外网回归 |

串口日志会把同一监视器中的前后启动会话连续写入一个文件。生产日志中正式启动 banner 之前可能残留旧诊断会话文本；本文只把正式启动后的 <code>rx=48 tx=16</code>、DHCP 和测速结果作为 production 证据，不把前一会话的诊断行归到正式镜像。

---

## 8. 尚未完全隔离的变量

本轮实验同时把 RX 8→48、TX 4→16，没有做纯 <code>48/4</code> 的第四组。因此“参数变化”层面不能声称只改了一个常量。

RX 归因仍成立的原因是：

- 旧配置每个活跃窗口新鲜 <code>RU=1</code>；
- 新配置 <code>RU=0</code>；
- <code>rx_bad/OVF/RPS</code> 均未出现；
- 软件 TX <code>busy_drop/reject</code> 为 0；
- 新配置仍偶见 TU，但吞吐已经恢复。

这组状态变化把主要限制指向 RX 描述符耗尽。不过若要定量回答“TX 16 对最终 12 MB/s 贡献多少”，仍需独立 <code>48/4</code> 与 <code>48/16</code> A/B。

---

## 9. 修复边界

已完成：

- 生产默认 RX/TX ring 固化为 48/16；
- 单页几何使用 release assert；
- 串行双架构 kernel build 通过；
- LA64 QEMU 启动并运行到用户态/LTP，无 panic；
- 板上正式无诊断镜像复现性能收益；
- 代理 HTTPS 200，Cloudflare 对象板端 <code>722559 B/s</code>，与宿主 <code>863447 B/s</code> 同数量级。

未完成或独立问题：

- 48/16 不是 1 Gbit/s 线速证明；
- 偶发 TU 需要单独分析；
- 高频空 poll 在 QEMU 空闲时仍消耗约 24%–26% 模拟单核；
- IRQ/NAPI 风格调度尚未实现；
- 纯 RX-only ring A/B 未做；
- 长时间并发、多 TCP 流和双向压力未覆盖。

---

## 10. 可复用性能调试流程

1. 把公网、代理、宿主、本地网络、虚拟机、实板分层；
2. 主 A/B 使用局域网固定对象；
3. baseline 至少三轮；
4. 计数器先确认 clear/read 语义，避免 sticky bit；
5. 同时观察性能指标和机制指标；
6. 每次 A/B 记录镜像 SHA 与编译 feature；
7. 最后用无诊断正式镜像复现；
8. 对未单变量隔离之处明确降级结论强度。

---

## 11. 最终证据链

~~~text
公网代理慢，但板端局域网同样只有百 KiB/s
  ↓
QEMU 同一通用 TCP/UserBuffer 路径约 20 MB/s
  ↓
immediate ACK 在 QEMU 与板端均无改善
  ↓
按 W1C 清 DMA_STATUS 后，8/4 活跃窗口每次新 RU=1
  ↓
48/16：RU=0，三轮平均 12,286,495 B/s，提升 94.77x
  ↓
正式无诊断 48/16：平均 12,529,330 B/s，提升 96.64x
  ↓
根因闭环：8 项 RX ring 在轮询延迟下持续耗尽
~~~

对应修复提交：<code>2031fd59 fix(net): prevent 2K1000 GMAC RX ring starvation</code>。
