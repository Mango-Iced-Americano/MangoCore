---
title: "2K1000LA 一键串口：输入未转发、突发丢字与控制键归属复盘"
category: debug
status: mixed
author: MangoCore Team
last_update: 2026-07-15
tags: [postmortem, la64, 2k1000la, serial, console, stdin, tty, ctrl-c, tooling]
code_paths:
  - "scripts/boot_2k1000_tftp.py"
related_docs:
  - "docs/01_architecture/boot-and-trap.md"
  - "docs/08_testing/mangocore-python-guide.md"
evidence_commits:
  - "f94c11d5"
  - "1ace76e5"
  - "56d8a224"
  - "6b08ed74"
  - "2031fd59"
evidence_records:
  - "docs/Work_Log.md, 2026-07-11 to 2026-07-14 board/tooling entries"
---

# 2K1000LA 一键串口：输入未转发、突发丢字与控制键归属复盘

## 0. 一句话结论

一键 TFTP 工具最初只实现了 <code>serial → stdout</code>，没有 <code>stdin → serial</code>。用户在本地终端看见自己输入的字符，是终端 canonical echo，不是开发板回显；因此“字看得见、Shell 不响应”的根因在主机转发方向，和板端 Bash、TTY、UART 驱动无关。

补成双向 <code>select</code> 后，长命令粘贴又暴露第二层：
<code>pyserial.write/flush</code> 只证明主机侧调用完成，不证明整条 USB-UART、硬件
FIFO、内核串口/TTY 与前台进程链已经消费。4 字节/1 ms 的突发在网络轮询期间仍会
丢字，改成 1 字节/4 ms 后长 marker 完整，证明故障对发送突发高度敏感。

当时最符合现象的工作假设是板端轮询/调度期间接收不及时，但没有 UART overrun、
FIFO 水位或逐层丢字计数，不能唯一归因板端 TTY；USB-UART、主机驱动队列、UART
FIFO 和板端串口驱动都仍在潜在故障边界内。

截至串口功能的已提交基线 <code>2031fd59</code>：

- 普通输入双向透传；
- 长输入按 1 字节/4 ms 节流；
- <code>Ctrl-C</code> 只关闭本地 monitor，不发给板端。

当前工作树另有未提交 WIP：

- <code>Ctrl-C</code> 改为发送板端 <code>0x03</code>；
- <code>Ctrl-] q</code> 关闭本地 monitor；
- parser 状态跨 <code>read()</code> 保留；
- 已做 Python/字节探针，尚不能计入当前已提交版本或已发布实板能力。

当前分支 HEAD 已前进到 <code>2031fd59</code> 之后的 ext4 提交，但未提交的串口
控制键 diff 仍不属于 HEAD。这条版本边界必须保留，不能把 WIP 控制键语义倒写成
<code>2031fd59</code> 或其后续已提交版本已有功能。

---

## 1. 输入链路的真实结构

串口交互至少经过：

~~~text
用户键盘
  ↓
macOS TTY line discipline
  ↓ stdin
boot_2k1000_tftp.py
  ↓ pyserial
USB-UART / CH340
  ↓
2K1000LA UART
  ↓
内核 TTY
  ↓
前台 Shell / 进程组
~~~

屏幕上出现字符还可能来自两处：

1. macOS 本地 TTY echo；
2. 板端 TTY 收到字节后回显，经 serial 返回。

二者视觉效果相似，但证明力完全不同。

---

## 2. 第一阶段：只有输出方向的 monitor

### 2.1 初始实现

提交 <code>f94c11d5</code> 首次实现一键 TFTP：

- 配置主机网口与 TFTP；
- 接管串口并截停 U-Boot；
- 校验传输与 uImage；
- 执行 <code>bootm</code>；
- 持续读取串口并写到 stdout。

启动后的核心循环只有：

~~~text
serial.read(...)
  → record log
  → stdout.write(...)
~~~

它从未读取 stdin。

### 2.2 症状为何伪装成板端故障

在 canonical 模式下，本地终端会回显键盘输入：

~~~text
keyboard → local TTY echo → screen
~~~

即使 Python 完全没有把字节写到串口，用户仍会看见命令。因此当 Enter 后没有任何板端响应，容易怀疑：

- Bash 卡住；
- 内核 TTY 不接收；
- UART RX 中断/轮询失败；
- Shell 镜像没有启动。

源码事实直接否定这些假设：转发程序根本不存在 stdin reader。

### 2.3 根因证明

| 证据 | 内容 |
|---|---|
| 旧源码 | <code>boot_and_stream()</code> 只读 serial |
| 视觉现象 | 本地输入可见但设备无响应 |
| 干预 | 增加 stdin→serial 后，同一内核可交互 |
| 结论 | 故障在主机单向 monitor，不在板端输入栈 |

修复只改 Python 工具，现有 uImage 无需重建即可交互，这也是重要反证。

---

## 3. 第二阶段：双向 select 与 raw TTY

提交 <code>1ace76e5</code> 将事件循环改为同时监听：

~~~text
select([serial_fd, stdin_fd])
  ├─ serial readable → log + stdout
  └─ stdin readable  → serial.write + flush
~~~

### 3.1 为什么不用两个阻塞线程

单个 <code>select</code> 循环保证：

- 串口只有一个 reader；
- stdin 只有一个 reader；
- 退出和终端恢复集中在一处；
- 不需要线程间争用串口或日志。

若两个路径同时读取 serial，任何一个都可能吞掉 prompt/输出，产生更难复现的字节丢失。

### 3.2 为什么要进入 raw 模式

canonical 模式会在主机侧：

- 缓冲到换行才交给程序；
- 本地 echo；
- 把控制字符解释为本地信号；
- 可能进行输入转换。

交互式串口 monitor 需要字节级控制，因此对 TTY stdin 调用 <code>tty.setraw()</code>。原终端属性在进入前保存，并在 <code>finally</code> 中用 <code>tcsetattr</code> 恢复。

<code>finally</code> 不是装饰：若异常、EOF 或 Ctrl-C 路径不恢复，用户退出后本地 shell 会无 echo、无 canonical editing，看起来像终端损坏。

### 3.3 当时 Ctrl-C 的设计

HEAD 路径把 <code>0x03</code> 解释为本地退出：

1. Ctrl-C 前的普通字节先发给板端；
2. <code>0x03</code> 本身不发；
3. monitor 返回；
4. 开发板继续运行。

pipe/PTY 探针验证普通输入只写入设备一次、CR 保留、Ctrl-C 未下发且 monitor 退出。

---

## 4. 第三阶段：双向不等于无丢字

### 4.1 新症状

人工短命令正常，但粘贴长命令或自动验收命令时：

- 字符缺失；
- 回显交错；
- 命令被截断；
- 网络轮询活跃时更易复现。

这说明方向已经正确，但链路中至少一个缓冲/消费阶段无法稳定承受该突发；仅凭终端
现象无法定位是哪一级。

### 4.2 三个不同的“完成”

~~~text
serial.write(data) 返回
  ≠ USB-UART 已在线上发完
  ≠ 板端 UART FIFO 已被读走
  ≠ 内核 TTY/前台 Shell 已消费
~~~

<code>flush</code> 的确切保障受 pyserial/主机驱动实现影响，但无论如何不能把
“主机 API 返回”当作“板端应用已处理”，也不能由此排除 USB-UART 或中间 FIFO。

### 4.3 节流迭代

| 提交 | 策略 | 原因/结果 |
|---|---|---|
| <code>1ace76e5</code> | 整块写入 | 交互打通，长粘贴易丢 |
| <code>56d8a224</code> | 4 B/chunk，间隔 1 ms | 缓解突发，实板短命令正常 |
| <code>6b08ed74</code> | 1 B/chunk，间隔 4 ms | 网络轮询时 4 B 突发仍丢，进一步收紧；未定位丢失层 |
| 功能基线 <code>2031fd59</code> | 保持 1 B/4 ms | 已提交行为 |

最终以超过 120 字符 marker 对比板端回显和结果，字节完全一致。

这是端到端保守节流，不是 UART 性能最优方案，也不是根因定位。更好的长期实现可能
使用硬件/软件流控、队列水位或协议级确认；在当前整条链路上，节流只证明能给验收
命令提供稳定性。

---

## 5. 第四阶段：Ctrl-C 到底属于谁

### 5.1 已提交行为的语义缺口

已提交脚本中 Ctrl-C 被 monitor 占有，用户无法向板端前台进程发送 VINTR：

~~~text
Ctrl-C
  → host monitor closes
  → byte 0x03 never reaches board
  → board process keeps running
~~~

这在早期可避免误中断板端，但进入常规 Shell 后妨碍停止 curl、测试或前台程序。

### 5.2 为什么不能简单“Ctrl-C 既发板又退出”

同一个字节不能可靠表达两个独立动作：

- 给板端发送 SIGINT；
- 关闭主机监视器但保持板端运行。

若两者绑定，用户无法只做其中一个。串口程序常用前缀 escape，把本地控制面从透明数据面分离。

### 5.3 当前工作树 WIP 方案

未提交修改保留 <code>Ctrl-]</code> 作为 monitor escape：

| 输入 | WIP 行为 |
|---|---|
| 普通字节、CR | 原样发板 |
| Ctrl-C | 原样发 <code>0x03</code>，由板端 TTY 转 SIGINT |
| Ctrl-] q | 本地关闭 monitor，不发送 q |
| Ctrl-] ? | 本地打印帮助 |
| Ctrl-] c | 向板端发送 Ctrl-C |
| Ctrl-] Ctrl-] | 发送字面 Ctrl-] |
| Ctrl-] + 未知字节 | 两字节都透传，避免静默丢数据 |

### 5.4 parser 状态必须跨 read 保留

stdin 的读取边界不等于按键/转义序列边界：

~~~text
read #1 → [Ctrl-]]
read #2 → [q]
~~~

若 parser 只看单次 buffer，第一个 read 结束时就丢失“等待命令字节”的状态，第二个 q 会被当普通输入发板。

WIP 用 <code>escape_pending</code> 在循环迭代间保存状态，并由 <code>_handle_console_input</code> 返回：

- <code>should_close</code>；
- 新的 <code>escape_pending</code>。

这是字节流协议的基本要求：消息语义不能依赖 <code>read()</code> 分包。

---

## 6. 版本边界：已提交与未提交不能混写

### 6.1 串口功能已提交基线 2031fd59

确定存在：

- 双向 <code>select</code>；
- TTY raw mode；
- <code>finally</code> 恢复；
- 1 字节/4 ms 输入节流；
- Ctrl-C 本地退出、不会发板；
- monitor 关闭后板继续运行。

### 6.2 当前工作树 WIP

当前 <code>scripts/boot_2k1000_tftp.py</code> 相对当前分支 HEAD 为 modified，新增：

- <code>MONITOR_ESCAPE=0x1d</code>；
- 跨 read 的 escape parser；
- Ctrl-C 发板；
- Ctrl-] q 本地退出等控制命令。

已有验证记录：

- <code>python3 -m py_compile</code>；
- 普通输入、CR、Ctrl-C 字节探针；
- Ctrl-] q 不进入 serial；
- 前缀跨两次 read；
- 字面量与未知转义不静默丢弃；
- 双架构 kernel build。

但这是工作树验证，不是 <code>2031fd59</code> 或当前后续 ext4 HEAD 的内容；现有
证据中也没有一次明确归档的 WIP 控制键实板交互日志。因此本文状态标为 mixed，
不能写“实板 Ctrl-C 已正式验收”。

---

## 7. 被排除的错误方向

### 7.1 修改板端 Bash

初始问题发生在 stdin 还没进入串口之前，修改 Bash 无法让主机 Python 开始读 stdin。

### 7.2 修改内核 UART RX

同一镜像在加入主机转发后即可交互，说明“完全无输入”的第一根因不在 UART RX。

### 7.3 看到本地 echo 就认为字节已发板

本地 echo 是主机 line discipline 的 UI 行为，不是链路确认。

### 7.4 serial.flush 后无条件认为命令完整

flush 不代表板端 TTY 已消费，更不代表 Shell 已解析；长粘贴实测反证了这一点。
但节流有效也不能反向证明丢字一定发生在板端 TTY；中间任一有限缓冲都可能对突发
敏感。

### 7.5 只在单次 buffer 中查 Ctrl-]

操作系统可以任意切分字节流。escape parser 若不跨 read 保存状态，会随机依赖读取边界。

---

## 8. 验证矩阵

| 能力 | f94c11d5 | 已提交功能基线 2031fd59 | 当前 WIP |
|---|---:|---:|---:|
| serial→stdout | 是 | 是 | 是 |
| stdin→serial | 否 | 是 | 是 |
| raw TTY + 恢复 | 否 | 是 | 是 |
| 长输入节流 | 否 | 1 B/4 ms | 1 B/4 ms |
| Ctrl-C 发板 | 否 | 否 | 字节探针通过 |
| 本地独立退出键 | Ctrl-C | Ctrl-C | Ctrl-] q |
| escape 跨 read | 不适用 | 不适用 | 字节探针通过 |
| WIP 控制键实板归档 | 不适用 | 不适用 | 尚无 |

历史双向与节流修改都只涉及主机工具，本质上无需重建 uImage。相关提交仍按项目流程完成了双架构 build，但该 build 不是“Python 转发正确”的直接证据；直接证据来自 pipe/PTY/marker 和实板短/长输入观察。

---

## 9. 修复边界与后续

已提交能力解决：

- 一键启动后 Shell 无输入；
- 普通输入/CR 透传；
- 长粘贴突发丢字；
- 异常退出后的本地终端恢复。

仍需：

- 将 WIP 控制键方案提交前做真实板端前台进程 SIGINT 验收；
- 验证 Ctrl-] q 关闭后板端持续运行且可重新 attach；
- 评估硬件流控或显式串口队列，替代固定 4 ms；
- 增加主机 write/USB-UART、UART overrun、FIFO 和板端驱动接收计数，定位真正丢失层；
- 测试非 TTY stdin、重定向和大批量自动输入；
- 对串口拔出、write 异常、partial write 做故障注入；
- 明确 UTF-8 多字节输入是否只做透明字节流。

---

## 10. 可复用调试方法

遇到“终端看得到输入，设备不响应”：

1. 关闭或识别本地 echo；
2. 检查程序是否真的读取 stdin；
3. 用 pipe/PTY 捕获写给 serial 的精确字节；
4. 只保留一个 serial reader；
5. raw 模式必须有 <code>finally</code> 恢复；
6. 粘贴与人工输入分别测试；
7. 不把 write/flush 当板端消费确认；
8. 本地控制键和远端控制字符分配不同编码；
9. escape parser 状态跨 read 保存；
10. 明确区分已提交、工作树 WIP 和实板已验收。

---

## 11. 最终证据链

~~~text
初始 boot_and_stream 只有 serial → stdout
  ↓
本地 TTY echo 造成“字已输入”错觉
  ↓
加入 stdin/serial 双向 select 后，同一 uImage 可交互
  ↓
长粘贴仍缺字：端到端链路对突发敏感
  ↓
4 B/1 ms 仍在网络轮询下丢字
  ↓
1 B/4 ms + >120 字符 marker 完整
  ↓
证明节流可稳定链路，但无逐层计数，不能唯一归因板端 TTY
  ↓
已提交脚本能可靠交互，但 Ctrl-C 被本地 monitor 占有
  ↓
WIP 用 Ctrl-] 前缀拆分远端 SIGINT 与本地退出
并保存跨-read parser 状态
~~~

已提交交互/节流链对应 <code>1ace76e5</code>、<code>56d8a224</code>、
<code>6b08ed74</code>，并由 <code>2031fd59</code> 这一网络功能基线继承；当前
分支虽已前进到后续 ext4 提交，Ctrl-] 控制面截至本文日期仍是未提交工作树修改。
