---
title: "2K1000LA AHCI reset、链路恢复与 Bootloader 状态依赖"
category: debug
status: resolved-with-known-limits
author: MangoCore Team
last_update: 2026-07-15
tags: [postmortem, loongarch64, 2k1000la, ahci, sata, u-boot, reset, comreset, dma]
code_paths:
  - "dependency/dep_iso/src/provider.rs"
  - "dependency/dep_iso/src/block/ahci.rs"
  - "os/src/drivers/block/sata_blk.rs"
  - "os/src/main.rs"
related_docs:
  - "docs/09_debug/la64_on_board/development-log.md"
  - "docs/07_driver/2k1000-ahci.md"
  - "docs/03_fs/2k1000-full-test-disk.md"
  - ".agents/skills/mango-workflow/references/debugging-patterns.md"
entry_points:
  - "AHCI::new"
  - "AHCIPort::request_power_and_active_state"
  - "AHCIPort::hard_reset_link"
  - "AHCIPort::wait_link_active"
  - "SataBlock::new"
---

# 2K1000LA AHCI reset、链路恢复与 Bootloader 状态依赖

## 1. 摘要

2K1000LA 的 SATA 首次接入不是一个单点 bug，而是三个相互遮蔽的问题连续暴露：

1. 驱动最初照搬通用 AHCI 假设，仅凭 `PxSIG` 判断端口是否可用；HBA reset 后
   `PxSIG=0xffffffff`，导致一个实际可通过 `IDENTIFY DEVICE` 工作的 SSD 被提前拒绝；
2. HBA reset 会清掉该平台需要的软件可写 `PI`，直接 TFTP 启动时因此得到
   `NoUsablePort { implemented: 0, ... }`；
3. 只恢复 `PI` 仍不完整。reset 后 CAP.SSS 丢失，`PxCMD.SUD` 不能保持置位，暖复位
   链路停在 `PxSSTS.DET=1`。必须恢复平台声明的 CAP 位、请求上电/启动并执行带真实
   时间基准的 COMRESET，才能让内核不依赖 U-Boot `scsi scan` 独立初始化 SSD。

最终修复把“控制器身份”“端口实现位”“PHY 链路”“设备身份”“数据一致性”拆成
独立证据层。`PxSIG` 降级为分类提示，`IDENTIFY DEVICE` 成为设备是否可用的权威判据；
reset 前保存 2K1000 平台 CAP 子集，reset 后按 `CAP -> readback -> PI` 恢复；暖复位时
使用 stable counter 表达 200 ms 预等待、至少 1 ms COMRESET 和最长 10 s 链路等待。

| 属性 | 结论 |
|------|------|
| 严重性 | Critical / P0，上层文件系统完全不可达，且错误初始化会危及写盘 |
| 影响范围 | 2K1000LA 片上 AHCI；通用平台默认不写 CAP |
| 表面现象 | `PxSIG=ffffffff`、`implemented: 0`、`PxSSTS=1`、暖复位偶发失败 |
| 直接根因 | 驱动假定 HBA reset 后 CAP/PI 保持，且把协议时间写成无时间基准循环 |
| 遮蔽条件 | U-Boot 先执行 `scsi reset/scan` 会替内核补齐部分状态 |
| 最终判据 | 启动序列未显式执行 U-Boot SCSI 命令，内核仍可 IDENTIFY、读盘并完成写入探针 |

## 2. 证据口径

本文使用以下标记避免把推测写成事实：

- **[事实]**：能由提交、代码、串口输出或已经归档的 Work Log 直接复核；
- **[机制]**：由 AHCI 寄存器语义和同一组观测推出的解释；
- **[边界]**：当时未验证、后续才关闭，或当前设计仍不保证的事项。

本问题的取证/功能基线为 `2031fd5909355994f768f845b2935e4509290a07` 及下列历史
提交；之后当前 HEAD 的前进未改变这里涉及的 AHCI 初始化代码。未提交内容不作为
已落地证据。

| 提交 | 日期 | 关闭的故障面 |
|------|------|--------------|
| `49c1482d` | 2026-07-10 | 板级定址、`PxSIG` 误拒绝、IDENTIFY 与只读 LBA0 |
| `c4f0d2bc` | 2026-07-11 | HBA reset 后恢复 `PI=0x0f` |
| `5bb715c0` | 2026-07-11 | 真实时间延时、暖复位 COMRESET、原始写入/flush/恢复 |
| `8f7d8da6` | 2026-07-11 | SUD/POD/ICC active 请求与错误状态清理 |
| `3ce82f0a` | 2026-07-12 | 恢复平台 CAP 位，解除 U-Boot `scsi scan` 依赖 |

## 3. 硬件边界：为什么不能从“标准 PC AHCI”套结论

### 3.1 已确认的平台绑定

2K1000LA 的片上 SATA 控制器在项目中固定为：

```text
PCI BDF      00:08.0
vendor       0x0014
device       0x7a08
AHCI ABAR    BAR0, 0x400e0000
DMA mask     32 bit，所有命令结构和数据缓冲必须低于 4 GiB
```

**[事实]** `os/src/drivers/block/sata_blk.rs` 同时核对 vendor/device/class/prog-if，
并从板级固定 BAR0 建立 ABAR；DMA extent 的结束地址必须不超过
`0x1_0000_0000`。PCI Command 使用 16 位访问，避免把同一 32 位寄存器高半部的
W1C Status 位原样写回。

这解释了为什么“扫描到 PCI 设备”不能等价于“AHCI 已正确接入”：独立 PC 控制器
常见的 BAR5、64 位 DMA 和 capability-list 经验，在该 SoC 上都不是可靠前置条件。

### 3.2 控制器、端口、链路和设备是四个状态层

调试中必须分别回答：

| 层 | 关键证据 | 能证明什么 | 不能证明什么 |
|----|----------|------------|--------------|
| PCI/ABAR | ID、class、BAR、MMIO 可读 | 找到了目标控制器 | 端口和 PHY 已工作 |
| HBA | `GHC`、`CAP`、`PI` | 控制器全局状态、实现端口图 | 端口连接了 SATA 盘 |
| Port/PHY | `PxCMD`、`PxSSTS`、`PxSERR` | 电源、spin-up、DET/IPM | 设备能执行 ATA 命令 |
| ATA device | IDENTIFY model/serial/capacity | 设备真正响应命令 | 每个 LBA 数据都正确 |
| 数据路径 | 重复读、CRC、写回恢复 | DMA/FIS/命令路径一致 | 文件系统元数据一定正确 |

早期错误正是把 `PxSIG` 这一项横跨了后面三层：签名异常便提前退出，导致真正有
判别力的 IDENTIFY 根本没有机会执行。

## 4. 时间线与调试追溯

### 4.1 第一阶段：`PxSIG=0xffffffff`，但端口并非不可用

首次内核 AHCI 探测已经能够读取正确的 PCI ID 与 ABAR。端口链路处于可继续尝试的
状态，但旧驱动看到：

```text
PxSIG = 0xffffffff
```

便以“不是 SATA 设备”拒绝端口。

**[反证]** 去掉 `PxSIG` 硬前置条件后，同一端口成功完成只读 IDENTIFY：

```text
model       TS32GMTS400
serial      F697095467
firmware    S0322B
sectors     62533296
bytes       32017047552
```

随后两次读取 LBA0 内容一致。设备能够接收 ATA 命令、完成 DMA 并返回稳定数据，与
“端口上没有可用 SATA 设备”矛盾。因此：

> `PxSIG=0xffffffff` 在这个 reset 时刻只能作为分类提示，不能作为端口拒绝条件。

修复后的判定顺序是：先确认端口实现和链路，再发 IDENTIFY；IDENTIFY 成功且返回
合理容量，才认为设备可用。这样既没有无条件接受异常端口，也没有让瞬时签名覆盖
更权威的命令级证据。

### 4.2 第二阶段：直接启动失败，执行过 `scsi scan` 却成功

下一轮采用“一条命令 TFTP 后直接 `bootm`”的启动方式，内核报错：

```text
NoUsablePort { implemented: 0, port0_status: 1 }
```

其中 `implemented: 0` 对应 reset 后 `HOST_PORTS_IMPL/PI` 被清零；而相同内核若在
U-Boot 中先运行过 SCSI 扫描，之后却能识别 SSD。

这个对照非常关键：

```text
路径 A: tftpboot -> bootm                 -> PI=0，失败
路径 B: scsi reset/scan -> tftpboot -> bootm -> 可继续初始化
```

两条路径的内核镜像、SSD 和 PCI 定址相同，主要变量是 U-Boot 是否操作过 AHCI。
因此问题不应继续归咎于“SSD 偶发慢”或“镜像不稳定”，而应审计 bootloader 留给
内核的寄存器状态。

对照板级 U-Boot 驱动与 Linux 保存/恢复方式后确认，2K1000 HBA reset 后 `PI` 并不
满足通用驱动所假定的保持语义。`c4f0d2bc` 在 reset 后恢复平台声明的
`PI=0x0f`，关闭了 `implemented: 0` 这一直接故障。

### 4.3 第三阶段：冷启动能用，板载 RESET 后 `PxSSTS=1`

只恢复 PI 后，冷启动或前一轮探针可能成功；按板载 RESET 再启动时则出现：

```text
LinkTimeout { sata_status: 1 }
```

`PxSSTS.DET=1` 的含义是“检测到设备存在，但 PHY 通信尚未建立”，而不是“没有盘”。
旧实现使用固定次数空转作为等待上限，这个次数随 CPU 主频、编译优化和总线等待
变化，不对应协议中的毫秒或秒。

**[机制]** HBA reset 复位控制器逻辑，不等价于在 SATA PHY 上产生 COMRESET。
暖复位后设备和控制器可能处于不同步状态，需要：

1. 请求端口供电、spin-up，设置 `PxCMD.SUD/POD` 并将 ICC 请求为 active；
2. 清除遗留 `PxSERR`；
3. 先给正常协商一个真实 200 ms 窗口；
4. 若仍不是 `DET=3, IPM=1`，写 `PxSCTL.DET=1` 保持至少 1 ms；
5. 写回 `DET=0` 释放 COMRESET；
6. 以真实 10 s 为上限等待链路 active。

`Provider::delay_us()` 使用架构 stable counter 提供时间基准。`AHCIPort` 的等待不再
以“循环了多少次”表达协议 deadline。

### 4.4 第四阶段：PI 已恢复，SUD 仍被硬件清掉

真实时间 COMRESET 关闭了一部分暖复位失败，但独立启动仍会出现 `PxCMD.SUD` 无法
保持置位、链路停在 DET=1。继续比较 reset 前后寄存器发现，HBA reset 不只清了 PI，
还改变了平台可写 CAP 位。

2K1000 最终采用以下平台策略：

```text
reset 前保存: CAP bit 28, bit 17
reset 后强制: CAP bit 27 (SSS)
恢复顺序:     CAP -> MMIO readback -> PI=0x0f
```

CAP.SSS 表示控制器支持 staggered spin-up。该位丢失后，端口的 SUD 请求不具备预期
语义；因此“PI 中有端口”仍不足以令 PHY 上线。

这里没有把 2K1000 固定值写进通用 AHCI 核心。`Provider` 默认：

```text
AHCI_CAPABILITY_SAVE_MASK = 0
AHCI_CAPABILITY_FORCE_BITS = 0
AHCI_PORTS_IMPLEMENTED = None
```

只有 `SataBlock` 的 2K1000 Provider 覆盖为上述掩码和 `PI=0x0f`。这样避免在 CAP
真正只读、语义不同的控制器上盲写寄存器。

### 4.5 最终关闭 Bootloader 依赖

2026-07-12 的实板复验把 U-Boot 前置命令严格限制为：

```text
ping -> tftpboot -> iminfo -> bootm
```

该次串口启动序列未显式执行 `scsi reset`、`scsi scan`、`scsi read` 或文件系统命令。
内核随后完成：

- AHCI reset 与 CAP/PI 恢复；
- PHY 上电、COMRESET 和 IDENTIFY；
- P1/P2/P3 分区识别；
- `/scratch` 文件写入、同步、重开、读取、删除和目录删除探针；
- basic、busybox 和 Lua 从 SSD 工作区运行。

这是关闭“可能仍吃到了 U-Boot 残留状态”假设的决定性验证。仅仅在执行过
`scsi scan` 的交互环境中成功，不算内核驱动完成上板。

## 5. 根因证明

### 5.1 候选原因与排除过程

| 候选原因 | 证据 | 结论 |
|----------|------|------|
| PCI 定址错误 | vendor/device/class 与固定 BAR0 均匹配；MMIO 可读 | 排除 |
| SSD 不兼容或不存在 | IDENTIFY 返回稳定型号、序列号、容量 | 排除 |
| LBA/DMA 基本路径错误 | 两次 LBA0 完全一致 | 基本读路径排除 |
| `PxSIG` 证明无盘 | 忽略签名后 IDENTIFY 成功 | 反证，`PxSIG` 不是权威判据 |
| PI 天生为 0 | U-Boot 扫描后可用；reset 前后对照；恢复 PI 后故障推进 | 排除，PI 是 reset 丢失 |
| 只需恢复 PI | PI 恢复后 SUD 仍清零、DET=1 | 反证 |
| SSD 只需更久时间 | 固定循环不稳定；真实等待加 COMRESET改善但未完全关闭 | 仅为部分原因 |
| CAP 与 spin-up 语义缺失 | 恢复/强制 CAP 后 SUD 与独立启动闭环 | 根因成立 |
| U-Boot 是必要硬件初始化器 | 启动序列无显式 SCSI 前置命令时内核复验通过 | 排除 |

### 5.2 最小因果链

```text
HBA reset
  -> 2K1000 平台可写 CAP/PI 丢失
  -> PI=0 时驱动看不到实现端口
  -> 仅恢复 PI 时 CAP.SSS 仍缺失
  -> PxCMD.SUD 请求不能稳定生效
  -> PHY 暖复位停在 DET=1
  -> 固定次数轮询又把协议等待变成 CPU 速度相关行为
  -> 内核报告 NoUsablePort / LinkTimeout
```

U-Boot `scsi scan` 会在内核之前补写寄存器并训练链路，因此把上述缺失变成：

```text
内核错误初始化 + bootloader 副作用 = 表面成功
```

它是遮蔽条件，不是修复。

## 6. 修复设计

### 6.1 `Provider` 明确平台差异

通用核心只定义能力钩子，平台提供：

- 最大单命令 sector 数；
- 端口实现位覆盖；
- reset 前后 CAP 的保存掩码与强制位；
- 有真实时间语义的微秒延时；
- DMA 分配与物理地址检查。

这使板级特殊寄存器行为不污染其他 AHCI 平台。

### 6.2 `AHCI::new()` 的顺序约束

最终初始化顺序的关键部分是：

```text
read CAP before reset
enable AHCI
HBA reset with bounded wait
restore selected CAP bits
MMIO readback
restore PI when platform requires it
allocate low-4GiB RFIS / command list / command table / data slot
request port power + spin-up + ICC active
wait 200ms
if needed: timed COMRESET
wait up to 10s for DET=3/IPM=1
IDENTIFY DEVICE
```

每个等待点都有上限；错误携带 `TFD/PxIS/PxSERR/PxSSTS/PxCMD` 等上下文，避免
“驱动卡死”这一无信息结果。

### 6.3 设备判定顺序

`PxSIG` 可以区分 ATA/ATAPI/SEMB/port multiplier，但该平台 reset 窗口读值不稳定。
修复后：

- 链路未 active：不能发命令，按链路错误处理；
- 链路 active 且签名可识别：按分类继续；
- 链路 active 但签名异常：仍允许只读 IDENTIFY；
- IDENTIFY 超时、taskfile error 或容量异常：端口才判失败。

### 6.4 写路径开放前的自恢复探针

读通不代表写命令、cache flush 和掉电前持久性正确。独立 feature 曾在所有分区末端
之外的保护区测试连续 8 个 sector：

```text
读取并保存 4 KiB，CRC32 = c71c0011
写入确定性模式，CRC32 = 0b88cfd1
FLUSH CACHE EXT
读回逐 sector 比较
写回原 4 KiB
再次 FLUSH
读回并确认 CRC32 恢复为 c71c0011
```

第一次写命令发出后，无论中间成功或失败都进入恢复；恢复无法验证则 panic，不能
继续挂载文件系统。该探针保持 ramfs-only，也不向用户态暴露可写块设备节点。

## 7. 验证矩阵

| 验证项 | 条件 | 结果 | 能关闭的假设 |
|--------|------|------|----------------|
| IDENTIFY | 首次板级 AHCI | 型号/序列号/固件/容量正确 | 定址、基础命令路径 |
| LBA0 双读 | 同一次启动 | 两次内容一致 | 基础 DMA 读稳定性 |
| 直接 TFTP 启动 | 序列中未显式运行 U-Boot SCSI | CAP/PI 修复前失败 | 暴露 bootloader 状态依赖 |
| 暖复位 | 板载 RESET | 定时 COMRESET 后恢复 | 固定循环、PHY 不同步 |
| 原始写探针 | 分区外保护区 | 写、flush、读回、恢复均通过 | ATA 写与 cache flush |
| 独立启动复验 | `ping/tftp/iminfo/bootm` | 内核识盘并完成 scratch 探针 | U-Boot 显式 SCSI 命令依赖已关闭 |
| 上层工作负载 | basic/busybox/Lua | 两套 libc 路径完成 | 不只是一条探针命令可用 |

## 8. 尚存边界

1. CAP 恢复值是 2K1000 平台知识，不能推广为“所有 AHCI reset 后都写 CAP”。
2. 当前 SATA 控制器由互斥锁串行化；文档证明可靠初始化，不证明多 slot 并发正确。
3. 32 位 DMA 约束是硬边界。任何未来更换 allocator 或批量缓冲，都必须继续验证
   整个 extent 末端低于 4 GiB，而不只检查起始地址。
4. 正常启动成功不能替代暖复位、冷断电和不执行 bootloader 磁盘命令三种回归。
5. 原始写探针只能证明被测 LBA 和 FLUSH 命令链；文件系统一致性由后续 FAT/ext4
   持久化测试分别验证。

## 9. 可复用调试方法

### 9.1 对“偶尔可用”的设备先做前置状态差分

不要先增加重试。记录成功与失败启动前执行过的每一条 bootloader 命令，构造最小
A/B：

```text
A: 只加载并启动内核
B: 增加唯一一条设备初始化命令后启动
```

若只有 B 成功，优先比较 reset 后寄存器和链路状态，而不是把现象归因于时序抖动。

### 9.2 协议 deadline 必须有时间单位

`for _ in 0..N` 不是 1 ms，也不是 10 s。硬件等待必须由稳定计数器或已校准 timer
驱动，并在错误中报告最终寄存器快照。这样才能区分“尚未等够”“链路一直 DET=1”
和“命令引擎忙”。

### 9.3 上板按破坏性递增验收

```text
PCI 只读配置
  -> IDENTIFY
  -> LBA0 重复读
  -> 多 LBA 只读/CRC
  -> 分区解析
  -> 文件系统只读挂载
  -> 分区外自恢复原始写探针
  -> 隔离 scratch 文件系统写
  -> 才允许持久读写挂载
```

每一步只解锁下一层权限，失败时仍能从 ramfs 启动并输出证据。

## 10. 最终结论

这次问题的核心不是“AHCI 标准是否实现完整”，而是驱动错误假定了 bootloader
交接状态：

```text
通用 reset 假设
  + 2K1000 CAP/PI 实际会丢失
  + PxSIG 被误当成权威身份
  + 无时间基准的链路等待
  + U-Boot scsi scan 偶尔代为初始化
  = 同一镜像冷/暖启动与启动命令相关的非确定性识盘
```

修复后的证据链从控制器身份一直延伸到独立启动和持久写回，最终证明：内核能够在
在启动序列未显式执行 U-Boot SCSI 命令的条件下，内核自主恢复 2K1000LA AHCI
所需的 CAP/PI、训练 SATA PHY、IDENTIFY `TS32GMTS400`，并稳定进入文件系统读写
路径。该证据不声称 U-Boot 自身启动代码从未以任何隐式方式接触控制器状态。
