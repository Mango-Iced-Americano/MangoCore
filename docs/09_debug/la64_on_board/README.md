---
title: "la64_on_board 实板适配专题"
category: debug
status: current
author: MangoCore Team
last_update: 2026-07-15
tags: [loongarch64, 2k1000la, board, bringup, debug, timeline, evidence]
related_docs:
  - "docs/09_debug/la64_on_board/development-log.md"
  - "docs/09_debug/la64_on_board/bug-hole-read-mismatch.md"
  - "docs/01_architecture/boot-and-trap.md"
  - "docs/03_fs/2k1000-full-test-disk.md"
  - "docs/07_driver/2k1000-ahci.md"
  - "docs/07_driver/2k1000-gmac.md"
---

# la64_on_board 实板适配专题

## 1. 用途

本目录集中保存 `la64-on-board` 分支从 2K1000LA 首次上板适配到当前状态的开发
时间线、修改原因、失败证据、根因闭环和验收结果。它面向组会汇报和后续接手者，
不以“改了哪些文件”为终点，而要求每个重要结论都能回到提交、源码、串口日志或
可重复命令。

## 2. 一眼看懂

| 项目 | 当前结论 |
|------|----------|
| 开发起点 | 2026-07-09：拆分 QEMU/2K1000 平台、建立 ramfs-only 最小路径 |
| 首次汇总提交 | `b5826a65`，2026-07-10 15:03，首次最小适配 |
| 首次完整实板启动 | 2026-07-10：TFTP/uImage → 内核 → initproc，无 panic |
| 当前已提交基线 | `b6c5c973`，2026-07-15 17:43 最终 amend（author time 17:29），`fix(fs): stabilize ext4 persistent writes` |
| 提交规模 | 34 个连续提交，326 个文件，`+30,302/-5,317` |
| 当前硬件 | 2K1000LA / LA264 500 MHz / 2 GiB DRAM / TS32GMTS400 32 GB SATA SSD |
| 已打通主链 | U-Boot、VALEN/TLB、双 bank 内存、AHCI、P1-P4、GMAC/DHCP/DNS/HTTPS、CSPRNG、CPython、APK、ext4 持久写与用户入口 ABI |
| 代表性验收 | CPython `72/72`；网络 `core 29/29 + external 9/9`；GMAC 本地 HTTP 约 12.5 MB/s |
| 安全边界 | P1/P3 与块设备节点只读；P2 仅 scratch/cache；P4 经身份校验后读写 |

最短因果链：

```text
QEMU 可运行
-> 板级入口/地址/设备全部不同
-> 先以 ramfs-only 隔离外设
-> 修通 40-bit VALEN、TLB/PTE/ASID 和高地址内核栈
-> 只读接入 SATA，再用自恢复探针逐级开放 P2/P4 写入
-> 接入 GMAC、DHCP、DNS、HTTPS 和可信随机
-> 用 CPython/APK 暴露扩展寄存器、文件系统和 ABI 深层问题
-> 用分层对照、ACK A/B 与 8/4→48/16 ring 组合 A/B 锁定 RX 描述符饥饿主限制
```

## 3. 阅读入口

| 文档 | 适合场景 | 内容 |
|------|----------|------|
| [development-log.md](development-log.md) | 组会主线、定位阶段、查提交 | 完整阶段时间线、34 个提交、当前架构和验证矩阵；它是总账，不替代单问题复盘 |
| [bug-hole-read-mismatch.md](bug-hole-read-mismatch.md) | 单点深挖示例 | “内核数据正确但用户比较失败”的 ABI/LLVM 反汇编证据链 |

专题目录现有 29 篇编号专题，加上迁入的 hole-read ABI 深挖，共 30 篇问题复盘；按
“一个可独立审计的问题链一篇”拆分。同一集成环境
若暴露多个小根因，则在同篇中用独立问题卡分栏，不把它们伪装成一个总根因。01-17
沿用首轮主线编号，18-21 是证据审计后补齐的独立专题；编号不表示严重性或严格时间
顺序，时间以 34 提交总账为准。小问题可以短，但每篇都必须回答六件事：**为什么会发生、如何把范围
缩小、排除了什么、哪条证据证明根因、修复为何有效、验证还没有覆盖什么**。

### 3.1 启动、地址与平台安全

| 问题 | 最底层问题 | 专题复盘 |
|------|------------|----------|
| uImage 与平台隔离 | 32-bit legacy uImage 字段、低 PA 与 DMW 高地址是什么关系；如何防止 QEMU/实板构建污染 | [01-uimage-entry-and-platform-isolation.md](01-uimage-entry-and-platform-isolation.md) |
| 40-bit 地址与首次切换 | AddressError 为何先于页表；VALEN、VPPN、PPN、ASID、PS 和栈窗口如何共同决定首次用户态 | [02-valen40-kernel-stack-and-tlb.md](02-valen40-kernel-stack-and-tlb.md) |
| 2 GiB 非连续内存 | DRAM 拓扑、地址上界、容量、固件所有权和 DMA 连续性为何不能混用 | [03-discontiguous-dram-and-firmware-ownership.md](03-discontiguous-dram-and-firmware-ownership.md) |
| zombie 栈槽滞留 | wait/reap 已归还 PID/quota 后，为何 `zombie_queue` 的强 TCB 仍持有 guarded stack，最终耗尽 1024 slot | [19-zombie-kernel-stack-slot-reclamation.md](19-zombie-kernel-stack-slot-reclamation.md) |
| 跨架构 HWCAP | RISC-V 位图如何在 LoongArch 被解释成 LASX/LBT，并把 loader 引到错误 resolver | [14-loongarch-hwcap-publication.md](14-loongarch-hwcap-publication.md) |
| LSX/FPR 物理别名 | QEMU 为何会假通过；同一物理寄存器的标量/向量视图为何必须二选一恢复 | [14a-loongarch-lsx-fpr-physical-alias.md](14a-loongarch-lsx-fpr-physical-alias.md) |
| 可信随机链 | “能返回变化字节”为何不等于安全随机；硬件熵、健康检查、CSPRNG 与 ABI ready 状态如何分层 | [15-trusted-rng-and-fail-closed.md](15-trusted-rng-and-fail-closed.md) |

### 3.2 SATA、分区与文件系统

| 问题 | 最底层问题 | 专题复盘 |
|------|------------|----------|
| AHCI 初始状态与暖复位 | 为什么 U-Boot `scsi scan` 会掩盖内核缺陷；PxSIG、CAP、PI、SUD、COMRESET 各证明什么 | [04-ahci-reset-and-bootloader-handoff.md](04-ahci-reset-and-bootloader-handoff.md) |
| 三种块大小 | MBR 512B LBA、平台块、文件系统原生块如何换算；启动 mount 与 `mount(2)` 为何必须走同一适配路径 | [05-block-size-translation.md](05-block-size-translation.md) |
| bind 只读传播 | 为什么底层只读仍挡不住 VFS 先进入分配器；持久 mount 属性如何跨 bind/recursive/propagation 复制 | [05a-readonly-mount-propagation.md](05a-readonly-mount-propagation.md) |
| FAT32 持久化与对象身份 | 为什么同一启动内可见仍可能未落盘；Drop、目录项、inode 与 PageCache identity 如何造成旧数据复现 | [06-fat32-persistence-and-inode-identity.md](06-fat32-persistence-and-inode-identity.md) |
| P4 安全发布协议 | 为什么必须 payload-first、MBR-last 才能阻断半成品可见；后发布异常为何只能说“尝试回滚”（尚无故障注入） | [07-safe-p4-persistence-protocol.md](07-safe-p4-persistence-protocol.md) |
| 超 DRAM 整盘写入 | 镜像大于开发板内存时，如何分块 TFTP、逐块 CRC/读回并防止写错设备或 LBA | [07a-large-disk-network-flashing.md](07a-large-disk-network-flashing.md) |
| AHCI 命令放大 | 512B 命令如何吞掉上层批量 I/O；为什么 64KiB 有效而继续扩大到 256KiB 无收益 | [08-ahci-command-amplification.md](08-ahci-command-amplification.md) |
| Python 字节码缓存 | 为什么只读 runtime 禁 pyc 会重复 parse/compile；为何 tmpfs 不能消除 user time 而 P4 pyc 可以 | [08a-python-bytecode-cache-bottleneck.md](08a-python-bytecode-cache-bottleneck.md) |
| ext4 可变长目录项 rename | 为什么旧有“slack 被吞”解释经 `rec_len` 算术复核并不成立；块首记录自合并、目录 checksum 身份与历史排序改动的证据边界 | [09-ext4-variable-dirent-rename.md](09-ext4-variable-dirent-rename.md) |
| ext4 lazy-init/计数 | `*_UNINIT` bitmap、描述符高位、跨组/重复释放和累计计数如何让 bitmap/group/superblock 三层真值分裂 | [18-ext4-lazy-init-and-block-group-accounting.md](18-ext4-lazy-init-and-block-group-accounting.md) |
| ext4 cache/快照所有权 | 旧 inode 快照和已释放块的 dirty metadata cache 为何能覆盖新所有者；延迟回收应在哪一层终结 | [18a-ext4-metadata-cache-and-inode-snapshot.md](18a-ext4-metadata-cache-and-inode-snapshot.md) |
| 跨 FS inode 身份 | `dev_id=0 + ino` 碰撞为何把 writable reopen 误判为 `ETXTBSY`，又如何被旧测试二次遮蔽成 `EBADF` | [18b-cross-filesystem-executable-inode-identity.md](18b-cross-filesystem-executable-inode-identity.md) |
| P4 持久应用根 | chroot、bind 与 overlay 的区别；P3 runtime/P4 state、私有 loader、FAT `utime ENOSYS` 与 CA 默认路径如何分层 | [21-persistent-app-root-and-private-loader.md](21-persistent-app-root-and-private-loader.md) |

### 3.3 网络、测试与主机工具

| 问题 | 最底层问题 | 专题复盘 |
|------|------------|----------|
| GMAC alternate descriptor | normal/alternate 位布局不一致时，为什么 DMA 会从 next 指针偏离到 `base+0x10` | [10-gmac-alternate-descriptor-bringup.md](10-gmac-alternate-descriptor-bringup.md) |
| DHCP IRQ 锁序 | 设计常驻 DHCP 时为何必须主动规避 IRQ 跨协议栈取阻塞锁；状态事件如何两阶段提交，以及哪些只属机制风险而非已复现死锁 | [11-dhcp-irq-lock-order.md](11-dhcp-irq-lock-order.md) |
| RAW ping DUP | 一个物理回包为何能被两个 handler 重复交付；如何排除 TX 双发、线路重包与 GMAC ring | [11a-raw-socket-duplicate-delivery.md](11a-raw-socket-duplicate-delivery.md) |
| glibc resolver ABI | BusyBox `nslookup` 为什么不能证明 glibc resolver；`IP_RECVERR` 与 `sendmmsg(269)` 如何阻断查询发出 | [12-glibc-resolver-abi.md](12-glibc-resolver-abi.md) |
| HTTPS 时间基线与 CA | 裸机无 RTC 时如何优先取得 NTP 当前时间；构建 epoch 退路为何只能保证功能、不能称为可信当前时间；CA/主机名如何正反验收 | [12a-https-build-epoch-and-ca-validation.md](12a-https-build-epoch-and-ca-validation.md) |
| GMAC RX ring 饥饿 | 如何用 W1C 新鲜 RU、ACK 对照和 ring 组合 A/B 排除公网/ACK；为何 RU 反转指向 RX，而 TX=16 的定量贡献仍未隔离 | [13-gmac-rx-ring-starvation.md](13-gmac-rx-ring-starvation.md) |
| 测试工作区与假通过 | 只读源、可写 CWD、隐藏 payload、wrapper 和外层 exit 0 如何共同制造误判 | [16-test-workspace-and-false-pass.md](16-test-workspace-and-false-pass.md) |
| APK wait status 9 | 原始 wait status `9` 为何表示 SIGKILL 而非 `exit(9)`；300 秒外层 timeout 如何被阶段/解码证据锁定 | [20-apk-wait-status-and-timeout-decoding.md](20-apk-wait-status-and-timeout-decoding.md) |
| 串口双向透传 | 本地 echo 为何会伪装成板端输入；TTY raw、控制字符和本地退出序列如何分权 | [17-serial-console-input-forwarding.md](17-serial-console-input-forwarding.md) |

### 3.4 独立 ABI 问题

| 问题 | 证据状态 | 专题复盘 |
|------|----------|----------|
| LA64 hole-read mismatch | 地址级根因闭环；16 字节用户入口栈与 signal frame 修复已进入 `b6c5c973`，仍保留专用 SP 遥测覆盖边界 | [bug-hole-read-mismatch.md](bug-hole-read-mismatch.md) |

稳定的子系统设计仍以架构文档为准：

- 启动与 TFTP：`docs/01_architecture/boot-and-trap.md`
- LoongArch 地址模型：`docs/01_architecture/loongarch64-platform.md`
- SSD/P1-P4：`docs/03_fs/2k1000-full-test-disk.md`
- AHCI：`docs/07_driver/2k1000-ahci.md`
- GMAC：`docs/07_driver/2k1000-gmac.md`
- CPython/APK：`docs/08_testing/cpython-isolated.md`、`docs/08_testing/apk-isolated.md`

## 4. 文档验收口径

每篇专题至少包含：问题卡、底层原理、按时间排序的调试过程、失败假设和排除证据、
根因证明、修复设计、验证矩阵、剩余边界和闭合证据链。以下内容不算完整复盘：

- 只有“现象 → 改动 → PASS”，没有说明中间如何排除替代解释；
- 只引用当前源码，没有提交、日志或对照实验来证明当时为何修改；
- 把 QEMU 通过写成实板通过，或把当前工作树写成已提交能力；
- 性能只给优化后数字，没有固定输入、基线、受控对照和未隔离变量说明；
- 危险写入只证明“写成功”，没有身份门禁、读回、失败恢复和权限边界。

## 5. 组会建议讲法

建议按六页展开，每页只回答一个问题：

1. **为什么不是重新编译就能上板**：入口、VALEN、DRAM、MMIO、SATA、网卡均不同。
2. **如何把首启故障切小**：ramfs-only + 分阶段探针，以最后一条串口输出界定边界。
3. **最关键的地址闭环**：40 位非规范栈地址 → AddressError → 栈窗口/TLB 全链修复。
4. **如何安全开放真实 SSD**：只读探测 → 自恢复 raw 写 → P2 文件写 → P4 payload-first/MBR-last。
5. **如何从“能启动”走到“能用”**：GMAC/DHCP/HTTPS、2 GiB、CSPRNG、CPython 72/72、APK 持久环境。
6. **如何证明性能主限制**：8/4 基线、immediate ACK 对照、48/16 ring 组合 A/B；吞吐提升约 95 倍且 RU 同步消失，但 TX=16 的独立贡献尚未测量。

## 6. 证据口径

本专题使用以下状态词，不能混用：

| 状态 | 含义 |
|------|------|
| 已实现 | 当前源码中存在对应逻辑 |
| 已编译 | 指定构建目标成功，不代表 QEMU 或实板运行 |
| QEMU 通过 | 有 QEMU 行为日志，不代表实机硬件语义相同 |
| 实板通过 | 有 2K1000LA 串口、CRC、测试结果或读回证据 |
| 当前已提交 | 位于 `b6c5c973` 或其祖先提交中 |
| 工作树进行中 | 尚未提交，不能作为已交付基线 |

`development-log.md` 中的日期优先表示实际开发或验收日期；提交日期和哈希另列。
同一天内“写代码、提交、实板复测”发生在不同时间时，分别记录，不用一个日期替代
完整证据链。
