---
title: "la64_on_board：2K1000LA 从首次上板到完整系统的开发日志"
category: debug
status: current
author: MangoCore Team
last_update: 2026-07-15
tags: [loongarch64, 2k1000la, board, bringup, timeline, postmortem, storage, network, cpython]
code_paths:
  - "os/make/la64.mk"
  - "os/src/main.rs"
  - "os/src/hal/arch/loongarch64/config.rs"
  - "os/src/hal/arch/loongarch64/linker-2k1000.ld"
  - "os/src/hal/arch/loongarch64/laflex.rs"
  - "os/src/hal/arch/loongarch64/trap/trap.S"
  - "os/src/mm/frame_allocator.rs"
  - "os/src/drivers/block/sata_blk.rs"
  - "dependency/dep_iso/src/block/ahci.rs"
  - "os/src/drivers/net/gmac_2k1000.rs"
  - "os/src/net/config.rs"
  - "os/src/drivers/rng/mod.rs"
  - "os/src/fs/mod.rs"
  - "user/src/bin/initproc.rs"
  - "scripts/boot_2k1000_tftp.py"
entry_points:
  - "la64-2k1000-run-clean"
  - "la64-2k1000-shell"
  - "la64-2k1000-core-tests"
  - "la64-2k1000-net-tests"
  - "la64-2k1000-cpython-tests"
  - "la64-2k1000-apk-persist-shell"
  - "2k1000-boot"
related_docs:
  - "docs/09_debug/la64_on_board/260710/README.md"
  - "docs/09_debug/la64_on_board/260710/bug-hole-read-mismatch.md"
  - "docs/01_architecture/boot-and-trap.md"
  - "docs/01_architecture/loongarch64-platform.md"
  - "docs/03_fs/2k1000-full-test-disk.md"
  - "docs/07_driver/2k1000-ahci.md"
  - "docs/07_driver/2k1000-gmac.md"
  - "docs/08_testing/cpython-isolated.md"
  - "docs/08_testing/apk-isolated.md"
---

# la64_on_board：2K1000LA 从首次上板到完整系统的开发日志

## 1. 摘要

`la64-on-board` 不是一次“把 QEMU 镜像换个启动地址”的移植。2K1000LA 与 LA64
QEMU 在入口、物理内存、虚拟地址位宽、MMIO 访问、块设备、网卡、熵源和固件交接
上都不同。开发过程因此采用分层策略：先切断未验证外设，只证明
`U-Boot -> uImage -> UART -> initramfs -> initproc`；再按只读探测、自恢复写探针、
隔离分区和持久状态分区逐级开放硬件；最后以网络、CPython、APK 和性能测试扩大
覆盖面。

截至 2026-07-15 已提交基线 `b6c5c973`，主链完成：

- legacy uImage 从 U-Boot TFTP 启动，Load/Entry 均为 `0x90000000`；
- 40 位 VALEN/PALEN、PTE/VPPN/TLB/ASID、高地址 guarded kernel stack 闭环；
- 2 GiB 非连续 DRAM 安全纳管，保留固件、DVO、CPU1 等仍占用区域；
- 2K1000 片上 AHCI、真实 SSD、MBR、Ext4/FAT32、P1-P4 分层权限闭环；
- GMAC0、DHCP、默认路由、DNS、HTTP、CA 校验 HTTPS 闭环；
- 2K1000 APB RNG 播种 ChaCha20 CSPRNG，随机接口 fail closed；
- CPython 3.14.5 L3-L9 实板 `72/72`；
- 隔离 APK、P4 持久应用根、Python/pip 和跨复位缓存闭环；
- GMAC 慢路径经公网/本地/QEMU 分层、ACK A/B 和 8/4→48/16 ring 组合 A/B 定位；
  新鲜 RU 与吞吐同步反转把主要限制指向 RX 描述符饥饿，正式吞吐由约
  `129,649 B/s` 提升到 `12,529,330 B/s`，约 96.64 倍；TX=16 的定量贡献未隔离。
- ext4 lazy bitmap、目录 framing/checksum、元数据身份与计数链完成 clean fixture、
  双架构 `63/63`、离线 fsck、QEMU 和实板持久写回归；LA64 用户入口 16 字节 ABI
  同批提交闭环。

| 属性 | 结论 |
|------|------|
| 开发记录起点 | 2026-07-09 |
| 首次汇总提交 | `b5826a65ebca1e601ded5e8ad7832fc31ede693f`，2026-07-10 15:03 |
| 首次完整实板 bootm | 2026-07-10，进入 initproc，无 panic |
| 当前已提交基线 | `b6c5c973aec727539df32592841e5bb06aefa45d`，2026-07-15 17:43 最终 amend（author time 17:29） |
| 连续提交数 | 34 |
| 累计差异 | 326 files，`+30,302/-5,317` |
| 开发板 | 龙芯星云 2K1000LA，LA264 500 MHz |
| 内存 | 安装 2 GiB；内核报告 `2043852 KiB` 可用 |
| SSD | `TS32GMTS400`，`62533296` sectors，`32017047552` bytes |
| 网络 | GMAC0 + YT8511H，1000M/full，轮询驱动 |
| 当前生产 ring | 48 RX / 16 TX |

## 2. 证据范围与判定规则

### 2.1 证据优先级

本文按以下顺序采信证据：

1. Git commit hash、提交时间、提交差异；
2. `docs/Work_Log.md` 当日的构建、QEMU、实板和产物哈希记录；
3. 可保存的串口日志与性能日志；
4. 当前源码常量、feature 和 Make 目标；
5. 由上述事实推导的结论，且明确标记为推导。

不能用低一级证据覆盖高一级事实。例如源码中存在 GMAC 驱动，只能证明“已实现”；
只有串口中出现链路、DHCP、测试结果并完成行为验收，才能写“实板通过”。

### 2.2 三个容易混淆的时间点

| 时间点 | 实际含义 | 证据 |
|--------|----------|------|
| 2026-07-09 | 拆平台、内存起点、uImage 地址、ramfs-only 和启动探针的开发记录 | `docs/Work_Log.md` 2026-07-09 |
| 2026-07-10 15:03 | 上述工作与地址/TLB 修复汇总为首次最小适配提交 | `b5826a65` |
| 2026-07-10 稍后 | TFTP、uImage、内核、首次上下文切换和 initproc 在实板完整通过 | `4705b28d` 所含 Work_Log 验收记录 |

因此，“首次开发”“首次提交”“首次实板完整启动”不是同一个时间点。

### 2.3 已提交基线与工作树进行中内容

本文的“当前已交付”以 HEAD `b6c5c973` 为边界。2026-07-15 的 ext4 持久写修复、
用户入口 16 字节 ABI、两架构 `fs_test 63/63`、离线 fsck、QEMU 和实板回归已经进入
该提交，计为第 34 个提交。LA64 hole-read mismatch 的地址级证据和剩余专用 SP
遥测边界见 [bug-hole-read-mismatch.md](bug-hole-read-mismatch.md)。

取证时仍存在与该提交无关的工作树内容：串口 `Ctrl-C`/`Ctrl-]` 控制面、生成态
`lang_items.rs` 及本批文档。它们不写成 HEAD 已交付能力；尤其串口交互/节流的历史
提交可以引用，但控制键 WIP 必须单独标识。

### 2.4 本文是总账，不是问题原理的替代品

本文负责回答“什么时候进入哪个阶段、哪个提交改变了什么、阶段出口是什么”；每个
问题的“底层机制、调试岔路、排除过程、根因证明、修复边界”放在独立专题中。组会
先用本文建立主线，遇到追问再进入对应复盘，避免在一篇长时间线里把因果链压缩成
一句“修复了某问题”。

| 故障域 | 独立复盘 |
|--------|----------|
| 启动镜像与平台污染 | [01-uimage-entry-and-platform-isolation.md](01-uimage-entry-and-platform-isolation.md) |
| 40-bit VALEN、kernel stack、PTE/TLB/ASID | [02-valen40-kernel-stack-and-tlb.md](02-valen40-kernel-stack-and-tlb.md) |
| 非连续 DRAM、固件所有权与连续 DMA | [03-discontiguous-dram-and-firmware-ownership.md](03-discontiguous-dram-and-firmware-ownership.md) |
| zombie TCB 与 1024 kernel-stack slot | [19-zombie-kernel-stack-slot-reclamation.md](19-zombie-kernel-stack-slot-reclamation.md) |
| AHCI reset、bootloader 残留状态与暖复位 | [04-ahci-reset-and-bootloader-handoff.md](04-ahci-reset-and-bootloader-handoff.md) |
| MBR/平台/文件系统三种块大小 | [05-block-size-translation.md](05-block-size-translation.md) |
| bind/rbind/propagation 的只读属性丢失 | [05a-readonly-mount-propagation.md](05a-readonly-mount-propagation.md) |
| FAT32 显式持久化、inode/PageCache identity | [06-fat32-persistence-and-inode-identity.md](06-fat32-persistence-and-inode-identity.md) |
| P4 payload-first、MBR-last 安全发布 | [07-safe-p4-persistence-protocol.md](07-safe-p4-persistence-protocol.md) |
| 大于 DRAM 的整盘镜像分块网络写入 | [07a-large-disk-network-flashing.md](07a-large-disk-network-flashing.md) |
| AHCI 命令放大与批量 DMA | [08-ahci-command-amplification.md](08-ahci-command-amplification.md) |
| 只读 Python runtime 与持久 pyc | [08a-python-bytecode-cache-bottleneck.md](08a-python-bytecode-cache-bottleneck.md) |
| ext4 块首 dirent framing、checksum 与 rename 历史干预 | [09-ext4-variable-dirent-rename.md](09-ext4-variable-dirent-rename.md) |
| ext4 lazy-init、块组字段与累计计数 | [18-ext4-lazy-init-and-block-group-accounting.md](18-ext4-lazy-init-and-block-group-accounting.md) |
| ext4 metadata cache、inode 快照与延迟回收 | [18a-ext4-metadata-cache-and-inode-snapshot.md](18a-ext4-metadata-cache-and-inode-snapshot.md) |
| 跨文件系统 executable inode identity / ETXTBSY | [18b-cross-filesystem-executable-inode-identity.md](18b-cross-filesystem-executable-inode-identity.md) |
| GMAC alternate descriptor 首包后停转 | [10-gmac-alternate-descriptor-bringup.md](10-gmac-alternate-descriptor-bringup.md) |
| DHCP IRQ 锁序与两阶段状态提交 | [11-dhcp-irq-lock-order.md](11-dhcp-irq-lock-order.md) |
| 多接口 RAW handler 重复交付 | [11a-raw-socket-duplicate-delivery.md](11a-raw-socket-duplicate-delivery.md) |
| glibc resolver ABI | [12-glibc-resolver-abi.md](12-glibc-resolver-abi.md) |
| NTP、构建 epoch 功能退路与 HTTPS 正负门禁 | [12a-https-build-epoch-and-ca-validation.md](12a-https-build-epoch-and-ca-validation.md) |
| GMAC RX ring 饥饿的分层/组合 A/B | [13-gmac-rx-ring-starvation.md](13-gmac-rx-ring-starvation.md) |
| 跨架构 HWCAP 误发布与 loader resolver | [14-loongarch-hwcap-publication.md](14-loongarch-hwcap-publication.md) |
| LSX/FPR 低 lane 物理别名与上下文恢复 | [14a-loongarch-lsx-fpr-physical-alias.md](14a-loongarch-lsx-fpr-physical-alias.md) |
| 片上熵源、ChaCha20 与 fail-closed | [15-trusted-rng-and-fail-closed.md](15-trusted-rng-and-fail-closed.md) |
| 只读测试源、隐藏依赖与 exit 0 假通过 | [16-test-workspace-and-false-pass.md](16-test-workspace-and-false-pass.md) |
| APK raw wait status 9 与外层 timeout | [20-apk-wait-status-and-timeout-decoding.md](20-apk-wait-status-and-timeout-decoding.md) |
| P4 app-root、P3 runtime 与 Python 状态分层 | [21-persistent-app-root-and-private-loader.md](21-persistent-app-root-and-private-loader.md) |
| 串口单向监视、TTY raw 与控制键分权 | [17-serial-console-input-forwarding.md](17-serial-console-input-forwarding.md) |
| 已提交的用户栈 ABI 静默错读修复 | [bug-hole-read-mismatch.md](bug-hole-read-mismatch.md) |

## 3. 初始差异：为什么 QEMU 可用而实板不能直接启动

更早的仓库提交 `5111999d` 已包含部分 2K1000 平台常量和 SATA 骨架，因此本轮不是
“仓库第一次出现 2K1000 字样”。但当时仍混有 QEMU 内存布局、64 位 legacy uImage
地址等前提，现有证据不能证明已完成可靠实板启动。本日志把 `b5826a65` 定义为
“本轮首次可审计实板适配提交”，不把早期 skeleton 夸大为上板闭环。

| 维度 | LA64 QEMU | 2K1000LA | 若混用的后果 |
|------|-----------|----------|----------------|
| 内核基址 | `0x80000000` | `0x90000000` | ELF/uImage 入口错误 |
| uImage 头 | QEMU 使用高 DMW 地址 | 32 位字段写低 PA `0x90000000` | U-Boot 跳转错误 |
| VALEN/PALEN | 48/48 | 40/40 | 非规范 VA 直接 AddressError |
| DRAM | 单连续区 | `0..0x10000000` 与 `0x90000000..0x100000000` | 把 MMIO 空洞当 RAM |
| 固件占用 | QEMU 固件模型 | U-Boot/DVO/CPU1/BPI 仍占低 bank 顶部 | 清零或分配后破坏固件/设备 |
| 块设备 | VirtIO PCI | 片上 AHCI `00:08.0`，BAR0 | 找不到盘或访问错误 BAR |
| 网卡 | VirtIO net | DWMAC + YT8511H | 无网络或错误枚举 VirtIO |
| 熵源 | VirtIO RNG | APB Device 2 RNG | 不安全随机或启动失败 |
| 关机 | QEMU PM MMIO | 实板 S5 未验证 | 写入错误 MMIO |

首阶段因此只保留 UART、内存、initramfs、调度和用户态入口。SATA、GMAC 和外部
mount 均被显式隔离；这不是功能退化，而是为了让“最后一条串口输出”能准确界定
故障层。

## 4. 总时间线

| 阶段 | 日期 | 提交范围 | 阶段出口 |
|------|------|----------|----------|
| A. 板级最小启动 | 07-09 至 07-10 | `b5826a65` | bootm 进入 initproc，40 位地址/TLB 闭环 |
| B. SATA 只读 | 07-10 | `49c1482d..4705b28d` | IDENTIFY、重复 LBA0、MBR/Ext4/FAT32 只读挂载 |
| C. 安全开放写入 | 07-11 | `296a67a2..8f7d8da6` | 自恢复 raw 写、P2 FAT32 持久写、`/scratch` 隔离开放 |
| D. 实板测试与 GMAC | 07-12 | `0da6a13e..1ace76e5` | basic/bench/core 测试工作区，ARP/ICMP，SATA+GMAC 联合 |
| E. 网络/安全/内存 | 07-13 | `56d8a224..29a8f40a` | DHCP/DNS/HTTPS、CSPRNG、双 bank 2 GiB |
| F. CPython/APK/P4 | 07-14 | `6b628240..b62828cf` | CPython 72/72、P4 双启动、持久应用根、AHCI 性能 |
| G. 网络性能收敛 | 07-15 | `2031fd59` | RX ring 根因闭环，生产吞吐约 12.5 MB/s |
| H. ext4/ABI 收敛 | 07-15 | `b6c5c973` | ext4 持久写、离线 fsck、双架构 63/63、用户入口 16B ABI 与实板回归 |

## 5. 阶段 A：板级最小启动与 40 位地址闭环

### 5.1 先拆构建产物，阻断跨平台污染

> 原理与取证详见：[01-uimage-entry-and-platform-isolation.md](01-uimage-entry-and-platform-isolation.md)。

最初必须同时解决“编译选择了哪个平台”和“上一次构建留下了哪个 linker”两个问题：

- `board_laqemu` 与 `board_2k1000` feature 互斥；
- QEMU 使用 `linker-laqemu.ld`，2K1000 使用 `linker-2k1000.ld`；
- 每次构建前由 `os/make/la64.mk` 复制正确模板；模板不存在时直接失败；
- 2K1000 legacy uImage 的 `ih_load/ih_ep` 写 `0x90000000`；
- 实板使用自己的 `entry.asm`，不与 QEMU `boot.rs` 重复定义 `_start`；
- `lang_items.rs.la` 仍由 Makefile 复制，不能直接维护生成态文件。

U-Boot legacy uImage 地址字段只有 32 位。板端 `bootm` 将低物理地址经 DMW 映射为
cached 高地址后跳转，因此在镜像头硬塞 `0x9000000090000000` 不仅无益，而且字段
无法表达。最终构建事实为：

```text
Image Name: MangoCore
Architecture: LoongArch
Load Address: 0x90000000
Entry Point:  0x90000000
```

### 5.2 ramfs-only 最小路径

> 故障隔离与首次切换证据详见：[02-valen40-kernel-stack-and-tlb.md](02-valen40-kernel-stack-and-tlb.md)。

`board_2k1000 + initramfs` 首阶段调用 `fs::force_ramfs()`，并跳过：

- QEMU VirtIO 网卡枚举；
- SATA/AHCI 探测；
- 外部块设备启动挂载；
- 任何对 SSD 的写操作。

验收对象被缩小为：

```text
U-Boot TFTP
-> uImage header/CRC
-> entry.asm
-> UART
-> zero_init/mm::init
-> initramfs 解包
-> init 任务创建
-> 首次 __switch
-> trap_return/ertn
-> PLV3 initproc
```

### 5.3 分阶段探针把“卡住”切到具体指令区间

早期启动日志只能看到“进入调度前”，无法区分 payload、TCB、内核栈、`__switch`
还是 `trap_return`。因此增加 `preload:01..18`、`tcb:01..11`、`sched:01/02`、
`user:01..03` 等仅在板级诊断 feature 下存在的检查点。

第一次实板日志完成 TCB 创建并停在 `sched:01`，没有进入 `user:01`。进一步打印：

- 新内核栈 bottom/top、软件 PPN、PGDH；
- 栈顶 volatile 写入/读回；
- 首次 TaskContext 的 resume PC/SP；
- 预期 `trap_return` 地址。

边界因此收敛到“恢复高地址内核栈并执行 `trap_return` 第一条输出之前”，而不是继续
怀疑 initramfs、ELF 或任意调度逻辑。

### 5.4 首个决定性根因：40 位 VALEN 下的非规范栈地址

> 地址规范性、异常层级和地址级证明详见：[02-valen40-kernel-stack-and-tlb.md](02-valen40-kernel-stack-and-tlb.md)。

实板异常地址：

```text
bad addr = 0xffffff7fffffeff8
CPUCFG1 = 0x03e2727e
PABITS = VABITS = 40
```

40 位高半规范地址从 `0xffffff8000000000` 开始。旧实现把首个栈放在
`MMAP_BASE - PAGE_SIZE` 下方，恰好落入 `0xffffff7...` 非规范空洞。CPU 在页表
查询前就产生 AddressError，因此继续检查 PTE 内容不会解释该次异常。

修复没有退回 heap `Vec<u8>` 栈，而是保留 guard page，将实板栈窗口改到：

```text
KERNEL_STACK_BOTTOM = 0xfffffffff7bef000
KERNEL_STACK_TOP    = 0xfffffffffffef000
slot size           = 128 KiB + 4 KiB guard
slot count          = 1024
```

QEMU 继续保留 48 位窗口。构建期断言和启动期 CPUCFG 校验共同防止平台常量再次
静默失配。

### 5.5 地址审计不能只改栈常量

> PTE、VPPN、PS、ASID、PGDL 与 `invtlb` 的联审过程详见：[02-valen40-kernel-stack-and-tlb.md](02-valen40-kernel-stack-and-tlb.md)。

栈地址修正后，对完整地址/TLB 链进行复核，发现多个既有问题会让实板结果不稳定：

| 问题 | 错误 | 修复 |
|------|------|------|
| TLB 页大小 | `STLBPS/TLBREHI.PS=3` | 4 KiB 使用 PS=12 |
| PTE PPN | 掩码宽度过大 | 按 PALEN 与页偏移裁剪 |
| VPN/VPPN | 复用 VA 掩码 | 分离 `VPN_MASK`、`VPPN_MASK` |
| 高 VPN | refill 后未正确符号扩展 | paired-page VPN 恢复并 canonicalize |
| ASID | 混入 ASIDBITS 或哨兵写 CSR | 分离字段并修复耗尽处理 |
| PGDL | 与 ASID 切换不同步 | `__restore` 分别比较并连续写 CSR |
| 内核高地址 PTE | 修改后可能命中旧非 global 项 | 刷新所有 ASID 的相关 TLB |
| PCI ECAM | `0xfe00000000` 不是普通 40 位 canonical VA | CPU 访问走 DMW2 SUC 别名 |

目标文件反汇编确认 `__rfill` 清 PS 字段后写 12，`__restore` 分别处理 PGDL/ASID。
双架构编译通过，LA64 QEMU 进入 init 用户态，随后 2K1000 实板完成高栈探针、首次
上下文切换和 PLV3 入口。

### 5.6 首次完整实板启动的证据

固定直连网络：主机/TFTP `192.168.9.10`，开发板 `192.168.9.20/24`。实板记录：

- U-Boot ping 主机成功，链路 1000 Mbps/full；
- TFTP 下载 `kernel-2k1000-sata-mount-ro.ui` 共 `12340008` bytes；
- `iminfo` 确认 LoongArch、Load/Entry `0x90000000`、CRC OK；
- VALEN/PALEN、高栈探针、首次上下文切换、用户态入口全部通过；
- 进入 initproc，无 panic。

这才是“首次完整上板启动”的出口条件；仅生成合法 uImage 不计为实板启动成功。

## 6. 阶段 B：2K1000 AHCI 与只读文件系统

### 6.1 固定硬件真值

> PCI 集成方式、DMA 约束与分阶段只读验收详见：[04-ahci-reset-and-bootloader-handoff.md](04-ahci-reset-and-bootloader-handoff.md)。

2K1000LA SATA 是片上 AHCI，不是通用 PC 枚举假设：

| 项 | 实板真值 |
|----|----------|
| PCI BDF | `00:08.0` |
| vendor/device | `0014:7a08` |
| class | `01/06/01` |
| PCI config PA | `0xfe00004000` |
| ABAR | BAR0 `0x400e0000` |
| DMA mask | 32 bit，缓冲必须低于 4 GiB |

CPU 对 PCI ECAM/ABAR 使用 DMW2 SUC 别名；设备描述符中仍写原始物理地址，不能把
DMW 虚拟别名交给 DMA。

### 6.2 第一次探测失败不是“没有 SSD”

> `PxSIG=0xffffffff`、链路状态与 IDENTIFY 的证据优先级详见：[04-ahci-reset-and-bootloader-handoff.md](04-ahci-reset-and-bootloader-handoff.md)。

U-Boot 可识别 SSD，但旧内核在 `PxSIG=0xffffffff` 时提前拒绝端口。对照 U-Boot
和 Linux 后确认：链路 `PxSSTS.DET=3` 与只读 `IDENTIFY DEVICE` 才是设备可用证据，
`PxSIG` 主要用于分类，不能作为 reset 后的硬前置条件。

修复后实板获得：

```text
model    = TS32GMTS400
serial   = F697095467
firmware = S0322B
sectors  = 62533296
bytes    = 32017047552
LBA0 read #1 == LBA0 read #2
[sata-probe] PASS
```

此时仍保持 ramfs-only，不挂载、不写盘。LBA0 全零、签名 `0000` 与 U-Boot
`No partition table` 一致，是磁盘内容状态，不是 AHCI 读取失败。

### 6.3 从 raw device 到 MBR/多块大小文件系统

> 三套块大小的换算模型与失败用例详见：[05-block-size-translation.md](05-block-size-translation.md)。

文件系统接入顺序固定为：

```text
整盘识别 raw Ext4/FAT32
-> 失败后解析 MBR 四个主分区
-> 拒绝 GPT/越界分区
-> 按文件系统原生块大小包装 BlockSizeAdapter
-> 实际读取根目录
-> 才发布 mount
```

关键边界：

- MBR LBA 永远是 512 B；
- 2K1000 SATA wrapper 当时平台块为 2 KiB；
- Ext4 原生块可为 1/2/4 KiB；
- FAT BPB sector 可为 512/1/2/4 KiB；
- 未对齐分区访问使用 bounce buffer；
- protective/hybrid GPT 明确 unsupported，不猜测挂载。

QEMU 分别验证 raw ext4、LBA2048 MBR ext4、LBA63 + 1 KiB ext4、LBA63 +
512 B FAT32 和 protective MBR。实板随后识别 `/dev/sda1`，将 Ext4 只读挂载到
`/sdcard`，读取目录并 bind `/musl`、`/glibc`。

### 6.4 三层只读不是重复代码

> mount 属性、设备节点和块设备包装各自阻断哪一层写入，详见：[05a-readonly-mount-propagation.md](05a-readonly-mount-propagation.md)。

首次真实 SSD 验收同时保留三层防线：

1. `MountFlags::RDONLY` 在 VFS 修改入口拒绝操作；
2. `/dev/sda*`/兼容 `/dev/vda*` 节点为 `0440`，写请求返回 `EROFS`；
3. `ReadOnlyBlockDevice` 在底层屏蔽写入。

实板发现 bind mount 副本丢失 `RDONLY`，使 `mkdir /glibc/lib` 错误进入 ext4 分配器，
并出现误导性的无块/未实现错误。根因是 bind/propagation 没有继承源挂载持久 flags。
修复后 `RDONLY` 在普通 bind、recursive bind 和 shared/slave 副本中保留，操作在
VFS 边界直接返回 `Read-only file system`。

## 7. 阶段 C：以可恢复协议逐级开放 SSD 写入

### 7.1 先制作固定身份的单 SSD 镜像

> 镜像大于 DRAM 时的分块传输、CRC、设备身份与 LBA 门禁详见：[07a-large-disk-network-flashing.md](07a-large-disk-network-flashing.md)。

最初三分区 MBR：

| 分区 | LBA | 大小 | 文件系统 | 初始职责 |
|------|-----|------|----------|----------|
| P1 | `0x800..0x8007ff` | 4 GiB | Ext4 | 官方测试集，`/sdcard`，只读 |
| P2 | `0x800800..0xa807ff` | 1280 MiB | FAT32 | staged `/scratch` |
| P3 | `0xa80800..0xc007ff` | 768 MiB | Ext4 | `/tools`，只读 |

完整 raw 镜像为 `6,443,499,520` bytes，SHA-256
`416f84060bca79ab06ef5596d8cfd1801b8ae3e56ae3d2e65e99a66b612ef19f`。
因板上只有 2 GiB 内存，不能一次 TFTP 6 GiB；实板按 24 个 256 MiB 加最后 1 MiB
分块写入，全部 `12584960` sectors 均完成主机 CRC、`scsi write`、`scsi read` 和
读回 CRC，耗时约 25.2 分钟。

### 7.2 raw 写入必须先能恢复原数据

> AHCI 写路径的可恢复探针与状态机边界详见：[04-ahci-reset-and-bootloader-handoff.md](04-ahci-reset-and-bootloader-handoff.md)。

独立 `sata_write_probe` 不接触分区内数据。它在所有分区之后选 8 sectors：

```text
备份原4KiB
-> 写确定性模式
-> FLUSH CACHE EXT
-> 读回比较
-> 无条件写回备份
-> 再次flush
-> 再读回确认恢复
```

实板测试范围 `12587008..12587015`：备份 CRC `c71c0011`，模式 CRC
`0b88cfd1`，写入、flush、读回、恢复和恢复复核全部通过。任一中间步骤失败都必须
尝试恢复；恢复失败直接 panic，不继续文件系统测试。

### 7.3 AHCI reset 后寄存器会丢，不能依赖 U-Boot 残留状态

> CAP/PI/SSS/SUD 与定时 COMRESET 的完整追溯详见：[04-ahci-reset-and-bootloader-handoff.md](04-ahci-reset-and-bootloader-handoff.md)。

冷/暖复位路径暴露两个独立问题：

- HBA reset 后 `HOST_PORTS_IMPL` 变为 0，导致 `NoUsablePort`；
- 仅恢复 PI 仍不足，CAP 的 SSS/SMPS/SPM 等可写状态和 `PxCMD.SUD` 会影响链路。

修复依据随板 U-Boot/Linux：reset 前保存或按平台声明 CAP/PI，reset 后恢复并读回；
链路先等待，必要时按 AHCI 顺序执行 COMRESET、清 SERR、请求 POD/SUD/active。
最终启动脚本只做 TFTP/iminfo/bootm，不执行 U-Boot `scsi reset/scan`，内核仍独立
完成 AHCI 初始化与 scratch 冒烟，消除了隐藏前置条件。

### 7.4 P2 文件级持久化暴露 FAT32 两类根因

> 单位换算、析构回写和重复 inode/PageCache 的三层根因详见：[06-fat32-persistence-and-inode-identity.md](06-fat32-persistence-and-inode-identity.md)。

P2 首先只由内核私有视图访问，不暴露可写块节点。探针执行：

```text
create directory/file
-> write 6 KiB
-> page cache writeback + fsync
-> reopen/read/compare
-> unlink/rmdir
-> reopen filesystem
-> 确认内容和清理都持久
```

两类故障：

1. FAT 层在已通过 `BlockSizeAdapter` 的设备上再次按平台块换算，FAT 表定位错误；
2. 文件大小、首簇和目录项只依赖 inode `Drop`，独立 inode/PageCache 可能在落盘前
   被另一实例读取，甚至 stale Drop 覆盖新目录项。

修复后所有 FAT 位置使用 BPB sector 语义，支持双 FAT/ExtFlags；write/resize 显式
同步 size/first cluster；create/unlink/rmdir 返回前提交所属目录页。实板最终输出：

```text
[sata-fs-write-probe] PASS: create/write/flush/reopen/read/unlink/rmdir persisted
```

### 7.5 只开放 P2，而不是“把整盘改成可写”

`sata_scratch_rw` 严格匹配 P2、MBR type `0x0c` 和 FAT32，才把它挂为
`/scratch`。P1、P3 和用户态块节点继续只读。用户态 smoke 覆盖
write/fsync/truncate/reopen/read/unlink/rmdir，实板通过。

这一边界让后续测试可以拥有可写当前目录，同时不会修改官方测试集和工具运行时。
正式 `kernel-2k1000-run.ui` 在该阶段仍保持全盘只读；staged 能力只能通过显式
feature/目标启用。

## 8. 阶段 D：从“能写文件”到“能跑测试”，并接入 GMAC

### 8.1 测试源只读，当前目录必须可写

> 工作区分层、复制后校验与假通过识别详见：[16-test-workspace-and-false-pass.md](16-test-workspace-and-false-pass.md)。

官方 P1 `/sdcard` 和工具 P3 `/tools` 不应因为 benchmark 需要临时文件就整体改为
可写。initproc 因此为每个 libc/测试组建立独立 P2 工作区：

```text
/scratch/work/basic-{musl,glibc}
/scratch/work/busybox-{musl,glibc}
/scratch/work/lua-{musl,glibc}
/scratch/work/lmbench-{musl,glibc}
/scratch/work/iozone-{musl,glibc}
/scratch/work/libcbench-{musl,glibc}
/scratch/work/libctest-{musl,glibc}
/scratch/work/cyclictest-{musl,glibc}
```

每组按当前已知 workload 的人工审计结果复制必需 payload 显式清单，复制后逐项检查
关键文件；这不声称得到数学意义的完整或最小依赖闭包，workload 升级时必须重审。
准备失败时 fail closed，不允许退回只读源继续执行，否则一个空 wrapper 或缺文件的
脚本仍可能退出 0，制造假 PASS。

### 8.2 每个 benchmark 暴露的是不同问题

> `lat_sig` 隐藏 payload 和外层 exit 0 的追溯详见：[16-test-workspace-and-false-pass.md](16-test-workspace-and-false-pass.md)。

| 组 | 初始现象 | 根因/修改 | 实板结果 |
|----|----------|-----------|----------|
| basic | 只读源无法创建测试文件 | 复制到 P2，保持 runtime 在 ramfs | musl/glibc 均到 END，exit 0 |
| busybox | `mv`/连带 `rmdir` 失败 | FAT 同目录、目标不存在 rename | 双 libc 命令 success |
| lua | 需要当前目录 I/O | 最小脚本/解释器工作区 | 2×9=18 项 success |
| lmbench | `lat_select` EROFS；外层仍 exit 0 | 工作区；补齐隐藏 `lat_sig` 映射对象和绝对 wrapper 链接 | 108s/216s 到 GROUP END |
| iozone | tmp/DUMMY 文件 EROFS；glibc 非法 LASX 指令 | 工作区；HAL 按架构发布 HWCAP | 1331s/1229s，无 EROFS/非法指令 |
| libcbench | 需确认 `/proc/self/smaps` 与隐藏依赖 | 静态二进制最小工作区 | 双 libc 各 27 项，37s/61s |

iozone 的非法指令尤其说明测试名不能直接等同根因。旧代码把 RISC-V 的
`AT_HWCAP=0x112d` 复用于 LoongArch；该位图在 LA ABI 中误报 LASX/LBT，glibc
loader 因而合法选择内核尚未保存上下文的指令路径。修复后 HWCAP 由 HAL 按
CPUCFG 和内核保存能力生成；当时只发布真正可维护的扩展。

### 8.3 核心测试不是“全部通过”

> zombie TCB 如何继续持有 `KernelStack`、以及 1024 slot 的剩余硬上限，详见：[19-zombie-kernel-stack-slot-reclamation.md](19-zombie-kernel-stack-slot-reclamation.md)。

`board_core_test` 聚焦镜像运行 libctest、cyclictest 和双 libc 各 274 个非网络
LTP 用例：

- musl libctest 静态/动态套件完整结束；
- glibc libctest 完整结束，修复目标项通过，但仍有 libc 语义差异/待复核项；
- cyclictest 双 libc 完成 P1/P8 和 400-task hackbench 压力；
- LTP 两套各跑到组尾，`futex_wait05`、`select01`、两轮 1000-waiter
  `futex_cmp_requeue01` 通过；
- 汇总仍为 `passed=3569 failed=23 broken=18 skipped=94`。

因此可写“调度完整结束且修复目标通过”，不能写“LTP 全通过”。该轮还修复了
reap 后 TCB/内核栈槽未及时从专用 zombie queue 清除的问题，避免 1024 个 LA64
guarded stack slot 被历史僵尸实体耗尽。

### 8.4 GMAC0 从硬件真值开始，而不是套 VirtIO 接口

> alternate descriptor 位布局与 current descriptor 证据详见：[10-gmac-alternate-descriptor-bringup.md](10-gmac-alternate-descriptor-bringup.md)。

厂商 DTS/U-Boot 与实板共同确认：

- 两路 DWMAC version `0x0000d137`；
- 两路 YT8511H PHY ID `0x0000010a`；
- GMAC0 已接线，GMAC1 未接线；
- GMAC0 实测 1000M/full；
- descriptor 使用 alternate layout；
- RX 长度包含 4-byte FCS，交给 smoltcp 前必须减 4；
- descriptor 和数据 DMA 仍需低 4 GiB。

首版轮询驱动完成 PHY 初始化、RX/TX ring、OWN 交接、FCS 处理和链路状态轮询。
实板 alternate ring 可跨槽位推进并回绕；Mac 直连 ARP 成功，首轮 ping 9/10，
第二轮 19/20，没有 TX OWN 卡死。

### 8.5 SATA 与 GMAC 必须联合验收

单设备通过不代表两个 DMA engine 同时工作仍安全。联合 Shell 证明：

- P1 `/sdcard`、P3 `/tools` 及 bind 均为 `ro`；
- P2 `/scratch` 为 `rw`，smoke PASS；
- `touch /sdcard/MANGO_RO_GUARD` 返回 `EROFS`；
- GMAC0 为 1000M/full；
- 第二轮 ping 10/10、平均 RTT 0.409 ms；
- 串口命令可交互，不再只有输出没有 stdin。

串口工具旧实现只把 serial 转 stdout，从未把 stdin 转 serial；终端本地回显造成
“命令已经输入”的错觉。修复后使用双向 `select`，并逐步增加长输入节流和控制键
协议。

> 已提交能力与当前未提交控制键改造的边界详见：[17-serial-console-input-forwarding.md](17-serial-console-input-forwarding.md)。

## 9. 阶段 E：DHCP、DNS、HTTPS、可信随机与 2 GiB 内存

### 9.1 DHCP 事件不能在中断上下文跨锁提交

> IRQ/任务上下文两阶段提交详见：[11-dhcp-irq-lock-order.md](11-dhcp-irq-lock-order.md)；多接口 RAW 重复交付的独立证据链见 [11a-raw-socket-duplicate-delivery.md](11a-raw-socket-duplicate-delivery.md)。

实板网络从静态直连扩展到动态网络时，DHCP handle 常驻 DeviceStack。设计这条新路径
时即识别出结构性锁序风险：若 timer IRQ 在持有协议栈锁时直接更新地址、路由或
procfs，就可能反向获取 device list/router 等任务上下文锁。提交 `6ae5c274` 从引入
永久 DHCP 的第一版起就让 IRQ 只执行 `try_poll_irq()` 并暂存租约事件，再由任务上下文
完成跨子系统提交。现有证据证明的是“危险等待图被代码结构消除”，并没有旧实现的
死锁复现或提交前后 A/B，因此不能把它写成已经发生过的死锁事故。

租约提交同步：

```text
smoltcp interface address
-> device address
-> connected route（归一化网络号）
-> default route
-> /proc/net/resolv.conf
-> /etc/resolv.conf link
```

RAW socket 也改为按 route ifindex 选择预创建 handler，移除会在同一栈建立重复接收者
的 rebind，修复非 loopback ping 每个序号出现 DUP 的问题。

实板最终取得 `192.168.1.3/24` 或 macOS 共享环境的 `192.168.2.2/24`，网关/DNS
正确，网关、公网和域名 ping 4/4、无 DUP。

### 9.2 BusyBox DNS 通过，不代表 glibc resolver ABI 完整

> `IP_RECVERR`、`sendmmsg(269)` 和 resolver 调用链详见：[12-glibc-resolver-abi.md](12-glibc-resolver-abi.md)。

BusyBox `nslookup` 已通过，但 glibc curl 仍无法解析。strace/行为边界指向两个缺口：

- UDP `IP_RECVERR` 状态及空 `MSG_ERRQUEUE -> EAGAIN`；
- syscall 269 `sendmmsg`，glibc 用它批量发送 A/AAAA 查询。

补齐后实板 curl 对 Baidu 和 example.com 均解析域名、建立 IPv4 TCP、返回 HTTP 200。
这说明“DNS 服务可达”和“目标 libc 的 resolver ABI 可用”必须分别验证。

### 9.3 HTTPS 验收必须同时有时间来源边界、正向和反向

> NTP、构建 epoch 的安全边界、CA 正向与错误主机名负向门禁详见：[12a-https-build-epoch-and-ca-validation.md](12a-https-build-epoch-and-ca-validation.md)。

curl runtime 固定 curl 8.19.0、Mbed TLS 3.6.7、源码哈希和 CA bundle。NTP 成功时
取得网络当前时间；NTP 不可达时使用镜像 build epoch，只是避免零值/过旧硬编码时间
阻断功能。它没有真实性、抗回滚或“不得晚于当前时间”的保证，不能称为可信当前时间，
也不能单独支撑安全时间语义。QEMU 与实板均验证：

- 默认 CA 校验访问 `https://www.baidu.com/` 返回 HTTP 200；
- 访问错误主机名证书返回 curl 60；
- 不使用 `-k`；
- 实板 TFTP 长度、CRC、uImage checksum 同时通过。

`inet_test tls` 的 NoVerify 握手只能作诊断，不能代替 CA 与 hostname 验证。

### 9.4 从“能返回随机字节”升级为可信随机链

> 熵源、健康检查、CSPRNG 状态与 fail-closed ABI 详见：[15-trusted-rng-and-fail-closed.md](15-trusted-rng-and-fail-closed.md)。

HTTPS 功能首次打通时，旧 `/dev/urandom` 仍可能输出弱状态，不能承载真实密钥。
随后建立统一链路：

```text
VirtIO RNG / 2K1000 APB RNG
-> 64-byte 启动样本与重复/卡死健康检查
-> ChaCha20 CSPRNG
-> 每次输出后隐藏流重键
-> getrandom(2), /dev/random, /dev/urandom
```

2K1000 APB RNG 通过 DMW2 地址 `0x800000001fe2b000` 读取。安全熵源失败时普通请求
返回 `EAGAIN`，不回退到全零、时间戳或地址种子；设备写入只混入私有状态，不提高
ready。实板输出 `random: initialized from 2k1000-rng`，`rng_test` 连续 5 次通过，
SATA/GMAC/DHCP 同时正常。

### 9.5 2 GiB 不是一个连续 `[start, start + size)`

> region、hole、carveout、连续 DMA 与 320 MiB 跨 bank 证明详见：[03-discontiguous-dram-and-firmware-ownership.md](03-discontiguous-dram-and-firmware-ownership.md)。

2K1000LA 安装 DRAM 为：

```text
bank0: 0x00000000..0x10000000      256 MiB
hole : 0x10000000..0x90000000      MMIO/non-RAM
bank1: 0x90000000..0x100000000     1792 MiB
```

U-Boot 栈/堆、活动 DVO framebuffer、CPU1 park loop 和 BPI/SMBIOS 仍占
`0x0cbf4000..0x10000000`，另保留第 0 页。帧分配器因此改为多 region：

- 普通页集合可跨 region，使用 `frames_alloc_any()`；
- 连续 DMA extent 必须留在单 region，使用 `frames_alloc()`；
- 地址空洞和 carveout 永远不能加入 free list；
- `MEMORY_SIZE` 表容量，`MEMORY_END` 表物理上界，均不能替代 region 表；
- initramfs/preload payload 在最后一次复制后才移交并回收页所有权。

实板 320 MiB RamFS 压力从 bank0 切换到 bank1，校验和正确；删除后 `MemFree` 只差
4 KiB。`/proc/meminfo` 和 BusyBox `free` 均报告 `MemTotal=2043852 kB`；AHCI
只读探针继续通过。约 53 MiB 固件占用没有被冒进回收。

## 10. 阶段 F：CPython、APK、P4 持久环境与 AHCI 性能

### 10.1 CPython 是跨子系统验收，不只是“能打印版本”

> 动态 loader 的 HWCAP 选择与扩展状态门禁详见：[14-loongarch-hwcap-publication.md](14-loongarch-hwcap-publication.md)。

隔离运行时来自 Alpine 目标架构包，放在 P3 `/tools/tests/cpython`；临时文件与文件
系统用例写 P2 `/scratch/cpython`。L3-L9 覆盖：

| 层 | 覆盖 |
|----|------|
| L3 | 运行时文件、ELF、loader、动态库 |
| L4 | 启动、退出码、prefix、全局 wrapper |
| L5 | 语言核心 |
| L6 | stdlib、time、random、hash、sqlite、signal round-trip |
| L7 | FAT/TmpFS 文件系统、rename、symlink、fsync、open-unlink |
| L8 | thread/futex、subprocess/pipe/wait |
| L9 | DNS、TCP、HTTP、默认 CA HTTPS |

双架构 QEMU 最终均为 `72/72`。实板专用 P3 writer 只允许三个固定 256 MiB 块：
`0xA80800`、`0xB00800`、`0xB80800`，写前硬校验 manifest/SHA、SSD 型号、MBR
CRC 和分区边界，逐块 TFTP/写入/读回 CRC，最后从 P3 读回 L7 脚本比对。

2K1000LA 最终 L3-L9 也是 `72/72`、group exit 0、耗时 125 秒；原始证据保存在
`logs/cpython-la64-board.log`，其最后一轮从 L3 到 L9 均有 PASS，并以
`OS COMP TEST GROUP END cpython-isolated` 结束。

### 10.2 QEMU 不会可靠暴露 FPR/LSX 物理别名

> FPR/LSX 低 lane 别名、trap/signal 合并与实板证据详见：[14a-loongarch-lsx-fpr-physical-alias.md](14a-loongarch-lsx-fpr-physical-alias.md)。

实板 CPython 首次动态启动损坏，连续 trap/signal 后更明显。根因是 LA264 上标量
FPR 低 lane 与 LSX 128-bit vector 存在物理别名；旧 restore 路径先恢复 LSX，随后
标量 FPR restore 又覆盖低 lane 或破坏高 lane 关联状态。

修复原则：

- trap 返回按 `EUEN.SXE` 在完整 LSX 与标量 FPR 两条恢复路径中二选一；
- signal frame 保留 LSX；
- `sigreturn` 将标量 FPR 低 lane 合并到 LSX 快照后统一恢复；
- LASX/LBT 继续关闭，直到上下文完整保存。

实板最小命令连续 20 次稳定，signal、线程、子进程通过。该问题说明扩展寄存器最终
门禁必须包含真实硬件、高频时钟 trap 和 signal round-trip，不能只依赖 QEMU。

### 10.3 CPython 又暴露 FAT inode/PageCache 身份问题

> namespace 已切换但 payload 仍旧的对象身份证明详见：[06-fat32-persistence-and-inode-identity.md](06-fat32-persistence-and-inode-identity.md)。

无 `fsync` rename 压力出现：目录项和首簇已经切到源 inode，新路径却读到旧缓存。
诊断值：

```text
RENAME_FAIL 0 b'S00' b'D00' b'tar' 104464 104456 104464
```

根因是同一 FAT 对象产生重复 inode/PageCache；canonical inode 表只用 `Weak`，
owner Drop 中无法 upgrade，最后脏页写回可能丢失。修复包括：

- 以首簇或空文件父目录项建立 canonical inode identity；
- rename/unlink/首次分簇时重键；
- PageCache backend 直接共享 `FileContent`；
- 覆盖目标保留到旧 fd 最后关闭再释放簇；
- 支持普通文件 overwrite 与 `RENAME_NOREPLACE`。

实板真实 FAT32 最终通过 50 轮无 `fsync` rename、空文件覆盖、旧目标 open-fd，
随后完整 CPython L7 和全套 72/72 通过。

### 10.4 APK 先易失，验证完整包管理链再谈持久化

> raw wait status `9`、SIGKILL 位编码与 300→900 秒 timeout 追溯详见：[20-apk-wait-status-and-timeout-decoding.md](20-apk-wait-status-and-timeout-decoding.md)。

易失阶段把 `apk.static`、repositories、keys 放进 initramfs；安装根在 RAMFS
`/run/apk-root`，P2 只存可删除 `.apk` cache。FAT32 不能完整表达 Unix mode、
symlink 和 APK 原子安装语义，因此禁止直接把 P2 当 `apk --root`。

QEMU 与实板均完成 HTTPS 索引、签名、fetch、musl/busybox/zlib 安装、post-install、
trigger、数据库检查和私有 loader 执行，最终 `[apk-test] RESULT=PASS`。首轮原始
wait status 9 被证明是 300 秒外层保护发 SIGKILL，而不是 APK 返回 9；延长 timeout
并解码 wait status 后闭环。

### 10.5 P4 采用 payload-first、MBR-last

> 崩溃状态模型、固定身份、读回和回滚协议详见：[07-safe-p4-persistence-protocol.md](07-safe-p4-persistence-protocol.md)。

P4 固定为：

```text
LBA       0xC00800..0x1400800
size      4 GiB / 0x800000 sectors
fs        ext4, no journal
label     MANGO_STATE
uuid      4d414e47-5354-4154-4500-000000000004
mount     /persist (RW only after identity checks)
```

写入器硬匹配 `TS32GMTS400`、`62533296` sectors、disk id、P1-P3 精确边界、旧
MBR CRC 和 payload manifest。先把 16 个 256 MiB 块全部写入并读回；全部成功后
才发布 P4 MBR entry。发布后的异常会触发恢复旧 MBR 的尝试，但该分支尚无故障注入
或真实回滚成功日志；回滚过程中再次 `KeyboardInterrupt` 还可能逃出内层。因此已闭环
的是正常发布路径，不能把设计保护写成“任意失败都一定恢复”。当前 preflight 读取
LBA0 后也未强制解析 `1 blocks read: OK`，仍需修补陈旧 DRAM CRC 假通过的理论窗口。

QEMU 同一非 snapshot 盘首启 `PASS mode=install`，次启 `PASS mode=reuse`。实板
16 块全部读回一致，新 MBR CRC `6538e5cb`；首启/复位次启同样分别 install/reuse，
两轮均 `RESULT=PASS`。P1-P3 边界与只读策略保持不变。

### 10.6 持久应用根不是通用 overlay root

> chroot/bind/overlay、P3 runtime/P4 state、私有 loader、FAT `utime ENOSYS` 与 CA 路径的分层证据见：[21-persistent-app-root-and-private-loader.md](21-persistent-app-root-and-private-loader.md)。

`apk_persist_shell` 在 P4 `/persist/apk-root` 维护应用根，并 bind `/dev`、`/proc`、
`/tmp`、`/run`、`/scratch`。软件与 Python user site 保存在 P4，下载缓存仍在 P2，
临时状态留在 ramfs/tmpfs。宿主 `/` 仍是易失 RAMFS；P1/P3、用户块设备节点仍
只读。

该阶段补齐：

- `/etc/ssl/cert.pem -> certs/ca-certificates.crt`；
- P3 CPython runtime 与 P4 Python state 的 bind；
- 私有 loader 路径仅继承到 Python 进程树；
- `TMPDIR/PYTHONUSERBASE` 放到 Ext4，避开 FAT `utime -> ENOSYS`；
- QEMU 完整证据：bundled pip、`six`/`idna` 安装和跨重启持久化；实板已证明 P4
  `reuse` 与 HTTPS，但没有留下同口径的 pip/six/idna 安装及重启串口证据。

### 10.7 ext4 rename：先用记录算术推翻旧解释，再锁定块首 framing

> 历史改动、`rec_len` 算术反证、块首删除和 checksum 身份详见：[09-ext4-variable-dirent-rename.md](09-ext4-variable-dirent-rename.md)。
> 同一 `b6c5c973` 中其他独立问题分别见：[lazy-init/块组计数](18-ext4-lazy-init-and-block-group-accounting.md)、[metadata cache/inode 快照](18a-ext4-metadata-cache-and-inode-snapshot.md)、[跨 FS identity/ETXTBSY](18b-cross-filesystem-executable-inode-identity.md)。

APK 原子替换曾出现 `rename()` 返回 0 后源/目标同时消失。提交 `b62828cf` 将同目录
rename 调整为先移除源、再处理覆盖目标并发布新项，同时补齐回滚、link count 延后
和同 inode no-op；专项 QEMU 随之通过。但“改动有效”不等于“原解释唯一成立”。

2026-07-15 的源码级复核推翻了此前“删除旧项会跨过 slack 中的新项”这一说法。若
旧记录总长为 `R`、实际占用为 `S`，插入后布局为 `S + (R-S)`；随后删除这个非块首
旧记录，只会让它的前驱增加 `S`，边界恰好停在新记录起点，不会越过它。因此排序
调整可以作为历史干预和事务保守化证据，却不能单独证明上述空间吞并机制。

继续下钻后可直接由代码证明的 framing 缺陷是块内首记录删除：旧路径对
`offset=0` 同时留下 `prev_offset=0`，继而读取自身 `rec_len` 作为“前驱长度”并把它
加回自身，令跨度从 `R` 变成 `2R`。这会使后续目录扫描越过下一条记录。当前修复把
块内记录删除抽成 `remove_dir_entry_record()`：块首只清 inode/body 并原样保留
`rec_len`，非块首才合并到真实前驱；同时目录块 checksum 明确使用目录自身 inode
和 generation，而非从块首目录项猜父 inode。该修复已进入 `b6c5c973`。T6/T7 是
源码级回归用例；现有日志能证明双架构构建、两架构 `fs_test 63/63` 及板端 P4
用户级回归，但没有独立的 T6/T7 逐项运行输出，故不把“用例存在”写成“已单独执行”。

`b62828cf` 当时的事实边界仍是“双架构编译 + QEMU + 产物，尚未完成该轮 TFTP
实板复验”；块首 framing/checksum 是随后由 `b6c5c973` 提交、并完成更强整批回归的
独立低层修复，不能把后来的证据倒灌给早期提交。

上述三篇也不能被 rename 复盘替代：allocator 决定位图/group/superblock 三层计数，
cache/快照决定旧 owner 是否还能回写，跨 FS identity 则发生在 VFS executable busy
表。它们随同一整合提交通过系统回归，但历史 APK 单次损坏没有保存足够快照，不能
对各根因贡献做唯一分摊。

### 10.8 AHCI 性能：收益来自命令合并，不是盲目加槽

> 请求放大计算与 64/256 KiB A/B 详见：[08-ahci-command-amplification.md](08-ahci-command-amplification.md)；随后暴露的重复解析/编译瓶颈见 [08a-python-bytecode-cache-bottleneck.md](08a-python-bytecode-cache-bottleneck.md)。

旧路径每 512 B 发一条同步轮询 ATA 命令。2K1000 AHCI 由 Mutex 串行，没有 VirtIO
那样的多个在途请求；机械复制四槽 DMA pool 不会产生并发。

最终方案在启动期永久取得一个 64 KiB 连续低端 DMA 槽，一个 PRDT 按真实长度执行
多扇区 read/write。实板 A/B：

| 指标 | 旧 512 B | 64 KiB | 256 KiB 对照 |
|------|----------|--------|----------------|
| 首次顺序读 5.48 MiB | 13.5 MB/s | 18.6 MB/s | 18.6 MB/s |
| Python 热 `-S` | 约 1.925s | 约 1.714s | 无额外收益 |

256 KiB 没有收益，最终回收为 64 KiB，避免永久多占 192 KiB 连续低端内存。
Python 重 import 的主成本仍在用户态解析/编译，P4 pyc 将中位数从 18.322s 降到
4.495s，降低 75.5%；物理 RESET 后 33 个 pyc 仍在并命中。

## 11. 阶段 G：GMAC 慢下载的分层对照与 ring 组合 A/B

> 本章的完整实验设计、W1C 新鲜事件证明和原始日志索引见：[13-gmac-rx-ring-starvation.md](13-gmac-rx-ring-starvation.md)。

### 11.1 先分层，不能把公网慢直接归咎内核

排查分四层：宿主公网/代理、宿主本地 HTTP、LA64 QEMU、2K1000 实板。

- Mac 本地代理入口可超过 500 MB/s；
- 所选公网代理链仅约 105--153 KiB/s，宿主直连约 828 KiB/s；
- LA64 QEMU 下载同一宿主文件约 19.93 MB/s；
- 实板本地 HTTP 只有约 136--205 KiB/s。

因此存在两个独立问题：公网节点慢，以及板端 GMAC 路径更严重的本地瓶颈。不能
用其中一个解释另一个。

### 11.2 旧状态位“黏住”不能证明持续发生

DWMAC `RU/OVF/RPS/TU` 是 W1C 事件位。若不在窗口开始清除，看到 RU=1 只能说明
历史上发生过，不代表当前持续发生。诊断先按 W1C 清低 17 位，随后统计每个两秒
窗口的新事件、RX packet、bad descriptor、TX busy/reject 和 DMA status。

### 11.3 三组递进实验；ring 组仍有 TX 混杂变量

| 实验 | ring | ACK | 三轮 8 MiB 平均 | 新鲜事件 | 结论 |
|------|------|-----|-----------------|----------|------|
| baseline | 8 RX / 4 TX | delayed | `129649 B/s` | 每窗 `RU=1` | 可复现慢路径 |
| ACK A/B | 8/4 | immediate | `129296 B/s` | RU/TU 持续 | 排除 delayed ACK 主因 |
| ring 组合 A/B | 48/16 | delayed | `12286495 B/s` | `RU=0, OVF=0, RPS=0, rx_bad=0` | RX 描述符饥饿是主限制；TX=16 贡献未单独量化 |

ring48 相对 baseline 提升约 94.77 倍。1 Gbit/s 突发很快填满 8 项 RX ring，轮询
驱动来不及归还 descriptor，持续 RU/丢包压低 TCP congestion window。增大到单页
可容纳的 48/16 后 RU 消失，吞吐恢复；这种事件/性能同步反转强力指向 RX，但因为
RX 与 TX 数量同时变化、没有 `48/4` 对照，不能声称整个 ring 实验是严格单变量，
也不能定量拆出 TX=16 的贡献。

### 11.4 正式镜像复验

诊断 feature 全部默认关闭；生产默认直接固定 48 RX/16 TX，并用 release 生效的
断言保护单页 descriptor 布局。正式 `kernel-2k1000-persist-shell.ui`：

- 文件总长 `16,741,024` bytes；
- SHA-256 `4f5537736bf3ee2224d0eb341dce06a4501346451879d3315ff338ee6da02015`；
- 不含 `[net-perf]`/ACK/ring 诊断字符串；
- TFTP、CRC32 `35a6a1c4`、uImage checksum 通过；
- 启动报告 `rx=48 tx=16 link=up 1000M full`；
- DHCP 获得 `192.168.2.2/24`；
- 三轮 8 MiB 为 `12,353,974 / 12,670,560 / 12,563,457 B/s`，平均
  `12,529,330 B/s`，较旧生产约 96.64 倍；
- 经显式代理访问 PyPI HTTPS 200；同一 2 MiB 对象宿主/板端为
  `863447/722559 B/s`，进入同一数量级。

偶见 `TU=1`、高频空 poll 和 IRQ/NAPI 风格调度仍是独立后续项，但它们不推翻
RX ring 饥饿这一已闭环根因。

## 12. 34 个提交的可审计台账

以下范围为 `dfc2da05..b6c5c973`。表中“闭环”只写该提交当时已有的证据，不用后续
结果反向抬高早期提交状态。

| # | 日期 | commit | 主题 | 该提交的证据边界 |
|---|------|--------|------|------------------|
| 1 | 07-10 | `b5826a65` | 首次 2K1000LA 最小适配 | 双架构编译、LA QEMU、uImage；修复镜像当时尚未实板复测 |
| 2 | 07-10 | `49c1482d` | AHCI read path | 实板 IDENTIFY + 两次 LBA0 PASS，不挂载不写盘 |
| 3 | 07-10 | `4705b28d` | SATA/MBR/FS 只读挂载 | QEMU 多布局；首次明确实板 bootm→initproc PASS |
| 4 | 07-11 | `296a67a2` | bind mount 保留 RDONLY | QEMU/实板写请求在 VFS 返回 EROFS |
| 5 | 07-11 | `2effeaaf` | 单 SSD 三分区镜像 | 生成、e2fsck、QEMU 通过；本提交时尚未刷实体盘 |
| 6 | 07-11 | `85dd29af` | 补录实体 SSD 刷写 | docs-only；25 块全部写入/读回 CRC，三分区实板挂载 |
| 7 | 07-11 | `78547ef2` | clean mode-run 镜像 | 编译/uImage/strings 审计；该轮未启动板子 |
| 8 | 07-11 | `f94c11d5` | 一键 TFTP 工具 | Python/Make 与实机 check-only；未发送启动命令 |
| 9 | 07-11 | `c4f0d2bc` | reset 后恢复 AHCI PI | 实板 `implemented: 0` panic 消失，进入测例 |
| 10 | 07-11 | `5bb715c0` | raw/FAT 持久写探针 | raw 写/flush/恢复和 P2 文件全链 PASS |
| 11 | 07-11 | `8f7d8da6` | P2 `/scratch` | 用户态 scratch smoke PASS，P1/P3 保持只读 |
| 12 | 07-12 | `0da6a13e` | basic 工作区 | 无 U-Boot SCSI 前置，双 libc basic 到 END |
| 13 | 07-12 | `3ce82f0a` | busybox/lua + FAT rename | 暖复位稳定；busybox success，Lua 18 项 success |
| 14 | 07-12 | `bb5b9411` | lmbench 工作区 | 双 libc 到 GROUP END；补齐隐藏依赖后无 Bad FD |
| 15 | 07-12 | `e764958a` | iozone + LA HWCAP | 双 libc 完整结束，修复 glibc LASX 非法指令 |
| 16 | 07-12 | `6cf2f657` | libcbench 工作区 | 双 libc 各 27 项完成 |
| 17 | 07-12 | `1ace76e5` | full-system bring-up | GMAC ARP/ICMP；core tests 跑到尾但 LTP 仍有 fail/broken |
| 18 | 07-13 | `56d8a224` | SATA+GMAC 联合 | P1/P3 ro、P2 rw、ping 10/10 |
| 19 | 07-13 | `6ae5c274` | DHCP/route/DNS | 实板 DHCP、默认路由、DNS、ping 无 DUP；该轮 QEMU 缺盘 |
| 20 | 07-13 | `b96ab997` | glibc DNS/curl ABI | `IP_RECVERR`/`sendmmsg` 后实板 HTTP 200 |
| 21 | 07-13 | `3cd6955a` | 实板网络回归 | `core 29/29`、`external 9/9` |
| 22 | 07-13 | `6b08ed74` | CA 验证 HTTPS | QEMU/实板正向 200，错误主机名 curl 60 |
| 23 | 07-13 | `b68939a2` | 删除一次性 bring-up 目标 | 双架构编译、保留/删除目标 dry-run；无新增实板行为 |
| 24 | 07-13 | `5d2f16ef` | 可信熵 + ChaCha20 | QEMU 正反门禁；实板 RNG 5 次 PASS |
| 25 | 07-13 | `29a8f40a` | 2 GiB 多 region 内存 | 320 MiB 跨 bank、MemTotal、AHCI 只读实板 PASS |
| 26 | 07-14 | `6b628240` | CPython/LSX/FAT/TmpFS | 双 QEMU 72/72；P3 受限写；实板 72/72 |
| 27 | 07-14 | `0778a319` | 易失 APK gate | QEMU/实板 HTTPS 安装与私有 loader PASS |
| 28 | 07-14 | `32b93c89` | P4 设计与 QEMU 双启动 | QEMU install→reuse；该提交时 P4 尚未实板写入 |
| 29 | 07-14 | `08967aa1` | 补录 P4 实板验收 | docs-only；16 块读回，实板 install→reuse PASS |
| 30 | 07-14 | `85314659` | 持久 APK 应用根 | QEMU bootstrap/reuse；实板 reuse/HTTPS，最新 CA 版复位门禁当时未完成 |
| 31 | 07-14 | `f133ba44` | AHCI batch + pyc | 18.6 MB/s、pyc 75.5% 降时、物理复位持久命中 |
| 32 | 07-14 | `b62828cf` | persist Python + ext4 rename | 双编译/QEMU/pip/restart；提交时新镜像未实板复验 |
| 33 | 07-15 | `2031fd59` | GMAC RX ring starvation | 分层/ACK/8-4→48-16 组合 A/B + 正式镜像；TX 贡献未隔离 |
| 34 | 07-15 | `b6c5c973` | ext4 持久写与用户入口 ABI | 双架构 63/63、离线 fsck、QEMU 组回归、实板 P4/iozone；T6/T7 无独立逐项日志 |

提交计数复核：

```bash
git rev-list --count dfc2da05..b6c5c973
# 34

git diff --shortstat dfc2da05..b6c5c973
# 326 files changed, 30302 insertions(+), 5317 deletions(-)
```

## 13. 当前架构快照

### 13.1 启动与地址

```text
make target
-> BOARD=2k1000
-> linker-2k1000.ld @ 0x90000000
-> legacy uImage load/entry 0x90000000
-> U-Boot tftp to temporary DMW address
-> bootm maps PA through cached DMW
-> entry.asm / 256 KiB boot stack
-> CPUCFG VALEN/PALEN check
-> zero real DRAM regions except carveouts
-> mm/drivers/fs/net/task
-> initproc / shell / selected gate
```

TFTP 暂存地址和 uImage Load/Entry 不是同一概念：脚本通常把镜像下载到
`0x9000000098000000`，而镜像头仍必须是低 PA `0x90000000`。

### 13.2 内存所有权

| 类别 | 范围/规则 |
|------|-----------|
| 安装容量 | 2 GiB |
| DRAM regions | `[0,0x10000000)`、`[0x90000000,0x100000000)` |
| MMIO/non-RAM hole | `[0x10000000,0x90000000)` |
| 固件 carveout | `[0x0cbf4000,0x10000000)` + page 0 |
| Linux ABI 可用容量 | `0x7cbf3000` bytes，`2043852 KiB` |
| 连续 DMA | 不跨 region，低 4 GiB设备约束由调用方再收紧 |
| 内核栈 | 1024 个 128 KiB 栈，每个带 4 KiB guard |

### 13.3 SSD 与权限

| 分区 | 起点/长度（512 B sectors） | 文件系统 | 挂载 | 权限 |
|------|---------------------------|----------|------|------|
| P1 | `0x800 + 0x800000` | Ext4，4 GiB | `/sdcard` | 始终只读 |
| P2 | `0x800800 + 0x280000` | FAT32，1280 MiB | `/scratch` | 仅 staged scratch 可写 |
| P3 | `0xA80800 + 0x180000` | Ext4，768 MiB | `/tools` | 始终只读 |
| P4 | `0xC00800 + 0x800000` | Ext4，4 GiB | `/persist` | 身份校验 + staged feature 后可写 |

P4 必须匹配 label、UUID、feature bits；有 journal 或 `needs_recovery` 时拒绝挂载为
staged RW。根文件系统仍是 ramfs/initramfs，P4 是应用根/chroot，不是通用 overlay。

### 13.4 网络、随机与用户环境

| 子系统 | 当前状态 |
|--------|----------|
| GMAC | GMAC0 polling，YT8511H，48 RX/16 TX，1000M/full |
| 地址 | 静态直连或 `gmac_dhcp` 常驻客户端 |
| DNS | DHCP 租约 → procfs → `/etc/resolv.conf` |
| 网络 ABI | TCP/UDP/RAW、`IP_RECVERR` resolver 子集、`sendmmsg` |
| TLS | curl + CA/hostname 正反门禁 |
| 随机 | APB RNG → ChaCha20，secure-not-ready 返回 EAGAIN |
| Python | P3 read-only runtime，P2 temp，P4 user/pyc/pip state |
| APK | initramfs manager，P2 cache，P4 persistent app root |

## 14. 代表性根因闭环

| 表面现象 | 关键排除 | 根因 | 根因修复 |
|----------|----------|------|----------|
| 首次 `__switch` 后无输出 | TCB/ELF/preload 完成；resume PC/SP 正确 | 栈 VA 在 40 位非规范空洞 | 迁移高栈窗口并全链审计 TLB/PTE/ASID |
| U-Boot 有盘，内核无盘 | PCI/BAR 正确；link active | reset 后 `PxSIG=ffffffff` 被错误当不可用 | 用链路 + IDENTIFY 判定 |
| reset 后 `implemented=0` | SSD/分区无关 | HBA reset 清 PI | 按 Provider 恢复 `0x0f` |
| 暖复位 `DET=1` | 非文件系统问题 | CAP/PI/SUD/COMRESET 恢复不完整 | 对齐厂商 U-Boot/Linux 顺序 |
| bind ro 后 ext4 分配失败 | 底层只读 wrapper 正常 | bind/传播副本丢 RDONLY | 继承持久 mount flags |
| FAT 创建只在缓存可见 | 盘上根目录为空 | 重复 inode/PageCache + Drop 延迟写回 | canonical identity + 显式目录提交 |
| glibc iozone 非法 LASX | I/O 路径尚未执行 | RISC-V HWCAP 被当 LA 位图 | HAL 按架构和保存能力发布 |
| ping 每序号 DUP | loopback 正常 | RAW rebind 建立重复 handler | 按 route ifindex 选预创建 handler |
| BusyBox DNS 好、curl DNS 坏 | DNS server/UDP 基本可达 | glibc 依赖 `IP_RECVERR`/`sendmmsg` | 补齐 resolver ABI |
| CPython 实机 trap 后损坏 | QEMU 通过 | FPR/LSX 低 lane 物理别名 | 两条 restore 二选一并合并 signal 状态 |
| FAT rename 新路径读旧数据 | 目录项已更新 | 重复 inode/cache 与 owner Drop 写回丢失 | canonical inode/FileContent 共享 |
| APK rename 返回 0 但条目消失 | `rec_len` 算术否定“非块首 slack 被吞”；历史排序改动只证明干预有效 | 当前代码直接证明块首记录把自身当前驱、`rec_len` 自加倍，且 checksum 必须绑定目录 inode；历史单次 incident 因缺原始块 dump 保持 mixed | `b6c5c973` 保留块首 framing、非块首合并真实前驱、显式目录身份；排序/rollback 作为独立事务加固 |
| APK 小文件压力后 fsck 损坏 | 不是单一 mkdir/rename；历史无逐项 bitmap dump | lazy-init/字段宽度/累计计数与 cache/快照所有权存在多项可独立证明缺陷，历史贡献不可分摊 | `b6c5c973` 整批修复；双架构 63/63 + 关机 fsck + P4/实板回归 |
| `ftruncate` 报 `-9` | 先检查 reopen，实际先返回 `-26` | 不同 FS 共用 `dev_id=0` 且 ino 相同，global executable busy key 碰撞；旧测试再遮蔽为 EBADF | 引入 FS instance identity，MountFS 转发；修正测试逐阶段报错 |
| reap 后仍耗尽 1024 栈槽 | PID/quota 已回收但 slot 未回收 | `zombie_queue` 强 TCB Arc 继续持有 `KernelStack` | 清专用 zombie queue，并在锁外 drop TCB；硬上限仍保留 |
| APK raw wait status `9` | 真正 `exit(9)` 应为 `2304`；包与 loader 已存在 | 300 秒外层保护发送 SIGKILL | 延长预算、分阶段 verify/exec、同时打印 raw/decoded status |
| 实板本地下载约 130 KiB/s | QEMU 20 MB/s；ACK A/B 无差 | 新鲜 RU 与 8/4→48/16 性能同步反转，将主限制指向 8 项 RX ring；TX 定量贡献未隔离 | 48 RX/16 TX，正式约 12.5 MB/s |

## 15. 当前验证矩阵

| 层 | 验收 | 状态 | 直接证据 |
|----|------|------|----------|
| 构建 | rv64/LA64 顺序内核构建 | PASS | 各阶段 Work_Log；最新 `b6c5c973` |
| 启动 | TFTP、iminfo、bootm、initproc | PASS/实板 | 2026-07-10 首次完整启动记录 |
| 地址 | VALEN/PALEN、guarded stack、TLB/ASID | PASS/QEMU+实板 | CPUCFG、反汇编、启动探针 |
| 内存 | 320 MiB 跨 bank、回收、MemTotal | PASS/实板 | 2026-07-13 memory stress |
| SATA 读 | IDENTIFY、两次 LBA0 | PASS/实板 | `[sata-probe] PASS` |
| SATA 写 | raw write/flush/restore | PASS/实板 | 8-sector CRC 闭环 |
| FAT 写 | create→fsync→reopen→unlink/rmdir | PASS/实板 | scratch smoke |
| 分区 | P1/P2/P3/P4 与权限 | PASS/QEMU+实板 | mount 输出、EROFS guard、P4双启 |
| 基础/bench | basic、busybox、lua、lmbench、iozone、libcbench | PASS/实板目标范围 | 各组 END/exit/诊断审计 |
| libctest/LTP | 跑至组尾 | PARTIAL | LTP 仍 23 fail/18 broken，不写全通过 |
| 网络 | inet core/external | PASS/实板 | 29/29 + 9/9 |
| HTTPS | 正确证书 + 错误主机名拒绝 | PASS/QEMU+实板 | HTTP 200 / curl 60 |
| 随机 | RNG 正向和无设备负向 | PASS | 实板 5 次；QEMU EAGAIN 负向 |
| CPython | L3-L9 | PASS/QEMU+实板 | 三平台 72/72；板端 125s |
| APK | 易失安装、P4 install/reuse | PASS/QEMU+实板 | `RESULT=PASS`、双启动 |
| GMAC 性能 | 8/4、ACK、48/16、生产 | PASS/实板 A/B | 四份 20260715 日志 |
| ext4 持久写 | clean fixture、63/63、关机 fsck、P4/iozone | PASS/双架构 QEMU+实板 | `b6c5c973` 与 2026-07-15 Work_Log |
| 用户入口 ABI | 原 LA64 hole-read 用例 + 双架构集成回归 | PASS/已提交 | `b6c5c973`；专用 signal/SP 遥测仍为覆盖边界 |

## 16. 当前长期入口

### 16.1 构建与启动

```bash
# 生产型干净 run 镜像
make -C os la64-2k1000-run-clean MODE=release

# 基础救援/交互 shell（无 SSD 依赖）
make -C os la64-2k1000-shell MODE=release

# 上板前只读检查与正式启动
make 2k1000-boot-check IMAGE=kernel-2k1000-run.ui
make 2k1000-boot IMAGE=kernel-2k1000-run.ui
```

### 16.2 聚焦门禁

```bash
make -C os la64-2k1000-core-tests MODE=release
make -C os la64-2k1000-net-tests MODE=release
make -C os la64-2k1000-curl-shell MODE=release
make -C os la64-2k1000-cpython-tests MODE=release
make -C os la64-2k1000-apk-tests MODE=release
make -C os la64-2k1000-apk-persist-tests MODE=release
make -C os la64-2k1000-apk-persist-shell MODE=release
```

`la64-2k1000-net-perf-shell` 是 feature-gated 诊断入口，不应当作生产镜像。

### 16.3 受保护的 P3/P4 写入

```bash
make 2k1000-p3-backup \
  PERF_RUN_DIR=target/perf-runs/<run-id> \
  P3_BACKUP_ID=<UTC-id> \
  CONFIRM_P3_START=0xA80800

make 2k1000-cpython-p3-write \
  P3_BACKUP_ID=<UTC-id> \
  CONFIRM_P3_START=0xA80800

make 2k1000-p4-preflight
make 2k1000-p4-write \
  CONFIRM_P4_START=0xC00800 \
  CONFIRM_P4_END=0x1400800 \
  CONFIRM_DISK_SECTORS=62533296
```

确认值不是使用提示，而是防误写门禁。不得通过修改常量把脚本变成通用写盘器。

## 17. 仍未完成与不可夸大的边界

1. **单核**：`CORE_NUM=1`；CPU1 仍在 U-Boot park loop，尚未完成 SMP 接管。
2. **关机**：实板 `shutdown()` 仍自旋 halt；ACPI/PM S5 未验证。
3. **内存交接**：约 53 MiB carveout 仍由固件/DVO/CPU1 等持有，不能直接回收。
4. **分区**：只支持 MBR 四主分区；GPT/扩展分区不支持。
5. **P4 Ext4**：不支持 journal replay；journal/recovery 状态会被拒绝。
6. **AHCI**：轮询、Mutex 串行、单 DMA slot；无 IRQ、NCQ、multiqueue。
7. **GMAC**：轮询且只启用 GMAC0；48/16 修复饥饿，但未达到完整千兆线速；TU、
   空 poll 和 IRQ/NAPI 调度待独立优化。
8. **内核栈**：1024 slot 耗尽仍可能 panic，尚未改为 fallible 分配。
9. **测试**：core/LTP 已跑到尾但仍有明确 fail/broken，不能宣称全量 LTP 通过。
10. **持久环境**：P4 是受控应用根，不是通用可写 `/` 或 overlayfs。
11. **串口工具 WIP**：取证时 Ctrl-C 透传与 `Ctrl-] q` 本地退出仍在工作树，未进入
    `b6c5c973`，不能写成 HEAD 已交付行为。
12. **ABI 直接遥测**：16-byte 对齐修复已提交并完成集成回归，但尚无覆盖所有
    argc/envc 组合和 signal handler 入口的独立 `sp & 0xf` 归档日志。

## 18. 证据索引

### 18.1 主时间源

| 日期 | `docs/Work_Log.md` 主题 |
|------|------------------------|
| 2026-07-09 | 内存起点、linker/uImage、关机隔离、ramfs-only、探针 |
| 2026-07-10 | TFTP 首启、VALEN/TLB/高栈、AHCI 只读、MBR/FS |
| 2026-07-11 | 完整盘、RDONLY、AHCI reset、raw/P2 写、scratch |
| 2026-07-12 | 工作区、bench/core、GMAC、串口交互 |
| 2026-07-13 | SATA+GMAC、DHCP/DNS/HTTPS、RNG、2 GiB |
| 2026-07-14 | CPython/LSX/FAT/TmpFS、APK/P4、AHCI/pyc、Ext4 rename |
| 2026-07-15 | GMAC 分层与 ring 组合 A/B；ext4 持久写、离线 fsck、用户入口 ABI 与实板回归 |

### 18.2 原始日志

| 文件 | 可直接核验的事实 |
|------|------------------|
| `logs/cpython-la64-board.log` | L3-L9 每项 PASS、HTTP/HTTPS 200、GROUP END |
| `logs/net-perf-board-baseline-run-20260715.log` | 8/4 baseline 三轮约 64--65s |
| `logs/net-perf-board-ack-run-20260715.log` | immediate ACK 三轮无改善 |
| `logs/net-perf-board-ring48-run-20260715.log` | 48/16 三轮约 0.67--0.70s、RU 清零 |
| `logs/net-perf-board-production-run-20260715.log` | 无诊断正式镜像约 12.5 MB/s、代理 HTTPS 200 |

### 18.3 关键稳定文档

- `docs/01_architecture/boot-and-trap.md`：TFTP/uImage/启动控制流；
- `docs/01_architecture/loongarch64-platform.md`：40 位地址、TLB/PTE/ASID/DMW；
- `docs/09_debug/bug-la64-kernel-stack-overflow.md`：AddressError 与高栈前史；
- `docs/03_fs/2k1000-full-test-disk.md`：P1-P4、写盘协议、测试工作区；
- `docs/04_mm/frame-allocator.md`：多 region、carveout、连续 DMA；
- `docs/07_driver/2k1000-ahci.md`：AHCI 命令粒度与性能；
- `docs/07_driver/2k1000-gmac.md`：descriptor、网络功能和 ring A/B；
- `docs/07_driver/random.md`：可信熵与 CSPRNG；
- `docs/08_testing/cpython-isolated.md`、`apk-isolated.md`：运行时门禁。

### 18.4 代表性产物哈希

| 产物/阶段 | SHA-256 |
|-----------|---------|
| 6,145 MiB P1-P3 完整 raw 镜像 | `416f84060bca79ab06ef5596d8cfd1801b8ae3e56ae3d2e65e99a66b612ef19f` |
| P4 初始 payload | `ff8bceeb48efa3968bd8df2e30284a773f971872dc7d3116226e4361dc2c298e` |
| 实板 CPython gate uImage | `941d2986d3a4e4b750545efc23717ca864016437ec4260dd41ecdfbf0a356e7a` |
| 72/72 所用 P3 payload | `3ae7084a32a891bca5d4d5cde935c45401c65127a99530189e6bdc4af4960f26` |
| 含全局 Python wrapper 的后续 P3 | `4a0f8a1bf6fad6ed89a9d0479438df8843f2d95d1482ddcdecc57276d364972c` |
| ring 修复正式 persist-shell | `4f5537736bf3ee2224d0eb341dce06a4501346451879d3315ff338ee6da02015` |

哈希只对应当次具体产物；重新编译后允许变化。验收时必须同时核对提交、配置、文件
长度、CRC/uImage checksum 和行为日志，不能只比其中一个值。

## 19. 调试方法复盘

本轮真正有效的不是“多试几个补丁”，而是反复执行同一套证据纪律：

```text
先切小系统边界
-> 用最后一条可见输出界定故障层
-> 对照厂商 U-Boot/Linux/架构手册固定硬件真值
-> 构造正向与负向门禁
-> 危险写入先保证可恢复，再逐级开放
-> QEMU 与实板差异单独记录
-> 性能问题固定输入、受控 A/B，并显式记录未隔离变量
-> 无诊断生产镜像复验收益
-> 明确仍未完成的边界
```

最应复用的五条原则：

1. **地址异常先判 canonical，再查页表。** 非规范 VA 可能根本没进入 TLB walk。
2. **reset 后重新建立设备真值。** 不依赖 U-Boot 留下的 CAP/PI/PHY/queue 状态。
3. **写权限按对象逐级开放。** raw 可恢复写、文件探针、用户 scratch、持久分区不能跳级。
4. **测试名不等于根因。** iozone 可暴露 HWCAP，CPython 可暴露 LSX 和 inode identity。
5. **性能结论必须标明混杂变量。** ACK A/B、ring 组合 A/B 和正式镜像复验缺一不可；未做 `48/4` 就不能声称 TX 贡献已隔离。

## 20. 最终结论

2K1000LA 适配已经从“能生成 uImage”推进到可重复的完整运行环境。最关键的工程价值
不是某个驱动文件，而是建立了连续的安全迁移顺序：

```text
板级隔离
-> 首次用户态
-> 地址/TLB闭环
-> SATA只读
-> 可恢复写
-> 分区级权限
-> 测试工作区
-> 网络与可信随机
-> 多bank内存
-> CPython/APK真实负载
-> P4持久状态
-> 性能分层对照与组合 A/B 收敛
-> ext4 持久写与用户 ABI 最终回归
```

每一阶段都保留了失败证据、根因、修改点和对应门禁；仍未完成的 SMP、S5、GPT、
journal replay、IRQ/NCQ、GMAC 中断化和剩余 LTP 也被明确隔离。后续开发应继续以
`b6c5c973` 为当前已提交基线；串口控制面等工作树内容仍必须在独立门禁和提交后才能
追加到已交付时间线，避免“现在能运行”与“已经形成可复现交付”混为一谈。
