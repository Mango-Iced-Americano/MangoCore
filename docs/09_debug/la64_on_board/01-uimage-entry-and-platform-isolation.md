---
title: "2K1000LA 首启：uImage 入口与 QEMU/实板构建隔离"
category: debug
status: resolved
author: MangoCore Team
date: 2026-07-15
last_update: 2026-07-15
tags: [loongarch64, 2k1000la, u-boot, uimage, linker, dmw, bringup]
code_paths:
  - "os/make/la64.mk"
  - "os/src/main.rs"
  - "os/src/hal/arch/loongarch64/mod.rs"
  - "os/src/hal/arch/loongarch64/entry.asm"
  - "os/src/hal/arch/loongarch64/linker-2k1000.ld"
  - "os/src/hal/arch/loongarch64/linker-laqemu.ld"
  - "os/src/fs/mod.rs"
related_docs:
  - "docs/09_debug/la64_on_board/development-log.md"
  - "docs/09_debug/la64_on_board/02-valen40-kernel-stack-and-tlb.md"
  - "docs/01_architecture/boot-and-trap.md"
  - "docs/01_architecture/initialization-flow.md"
entry_points:
  - "os/make/la64.mk::uimage"
  - "_start"
  - "rust_main"
  - "fs::force_ramfs"
---

# 2K1000LA 首启：uImage 入口与 QEMU/实板构建隔离

## 1. 一句话结论

首次上板不能直接复用 LA64 QEMU 产物：legacy uImage 头只能表达 32 位装载/入口
地址，2K1000LA 必须写低物理地址 `0x90000000`，再由 U-Boot 和 `entry.asm` 完成
DMW 别名到低地址链接视图的交接；同时必须把 QEMU/实板的入口、feature 和 linker
模板隔离，否则一次“编译成功”仍可能生成绝对地址属于另一平台的内核。

## 2. 问题卡

| 项目 | 结论 |
|------|------|
| 触发 | 将原 LA64 QEMU 构建链直接用于 2K1000LA legacy uImage |
| 表象 | 镜像可以生成，但 uImage 地址、ELF 绝对符号或 `_start` 入口不属于实板 |
| 真正故障层 | 构建产物契约与 bootloader 地址交接，不是 Rust 初始化逻辑 |
| 直接根因 | uImage 头误用 64 位 DMW 地址；QEMU/实板共享生成态 linker 和入口选择 |
| 根因修复 | 2K1000 头写 `0x90000000`；分离 linker/入口/feature；模板缺失即失败 |
| 隔离手段 | 首阶段强制 ramfs-only，暂不探测 SATA、GMAC 或 QEMU VirtIO 设备 |
| 首个适配提交 | `b5826a65`，2026-07-10 15:03；该提交本身尚未证明实板完整启动 |
| 首次完整实板证据 | `4705b28d` 所含 Work_Log：`bootm` 进入 initproc，无 panic |
| 当前边界 | 普通 `board_2k1000 + block_sata` 已进入 SATA 路径；ramfs-only 仍用于救援/探针镜像 |

## 3. 必要底层原理

### 3.1 uImage 头地址、ELF 链接地址和 CPU 当前 PC 是三件事

启动链中至少同时存在三种地址语义：

1. **legacy uImage 的 `ih_load`/`ih_ep`**：头字段为 32 位，告诉 U-Boot 把 payload
   放到哪里、从哪里开始执行；
2. **ELF/linker 绝对地址**：决定 `la.global`、静态对象和所有绝对重定位的数值；
3. **U-Boot 跳转时 CPU 看见的地址**：LoongArch U-Boot 可通过 DMW cached 别名访问
   同一物理内存，当前 PC 因而可能带高位段前缀。

三者不能靠“数值看起来都指向同一块内存”混为一谈。对本板的正确契约是：

```text
uImage ih_load / ih_ep = 0x90000000       # 32-bit low physical address
linker BASE_ADDRESS     = 0x90000000       # kernel absolute symbols
U-Boot bootm            = may enter via cached DMW alias
entry.asm               = preserve current segment temporarily
                        -> establish low direct window
                        -> clear current PC high 16 bits
                        -> continue at low linked address
                        -> install boot stack
                        -> rust_main
```

旧 Makefile 使用过：

```make
LA_LOAD_ADDR  := 0x9000000090000000
LA_ENTRY_POINT := 0x9000000090000000
```

该数值无法完整编码进 legacy uImage 的 32 位地址字段。把高 DMW 地址写进镜像头不是
“更精确”，而是破坏 bootloader 文件格式契约。

### 3.2 DMW 别名不改变 payload 的物理身份

2K1000LA 的 U-Boot 可能通过 cached DMW 地址执行 payload，但 `entry.asm` 的目标是把
执行环境收敛到内核链接时采用的低地址视图。关键操作不是复制内核，而是调整 DMW
并清除当前 PC 的高 16 位：

```asm
pcaddi  $t0, 0
slli.d  $t0, $t0, 0x10
srli.d  $t0, $t0, 0x10
jirl    $t0, $t0, 0x10
```

因此，uImage 头和 linker 都使用 `0x90000000`；高 DMW 地址只是 CPU 访问同一物理
内存的一种临时视图，不应扩散成 ELF 绝对符号。

### 3.3 linker 脚本是构建输入，不是可安全复用的缓存

仓库中的 `linker.ld` 是 Makefile 复制出的生成态文件。旧规则：

```make
cp linker-$(BOARD).ld linker.ld 2>/dev/null || true
```

如果模板缺失，命令仍返回成功，rustc 会继续读取上一次构建残留的 `linker.ld`。
于是“先构建 2K1000、再构建 QEMU”或反向顺序可能改变下一次产物，而源码和命令本身
不变。这是典型的隐藏构建状态污染。

根因修复必须满足：

```text
指定平台
-> 对应 linker 模板必须存在
-> 明确覆盖生成态 linker.ld
-> 再调用 rustc
```

模板不存在应立即失败，不能用旧文件兜底。

## 4. 调试追溯

### 4.1 起点：仓库“已有 2K1000 字样”不等于已经上板

更早的 `5111999d` 已有部分平台常量和 SATA 骨架，但仍混有 QEMU 内存布局、共享入口
及 64 位 uImage 地址。没有对应实板 `bootm → 用户态` 日志，因此不能把它算作首次
上板闭环。

### 4.2 第一轮审计先检查产物，不先改内核逻辑

2026-07-09 对构建链的静态检查得到三个直接冲突：

- 2K1000 uImage 头仍使用 `0x9000000090000000`；
- QEMU 和实板共用 `linker.ld` 生成态；
- QEMU `boot.rs` 和实板 `entry.asm` 的 `_start` 选择没有形成严格的平台契约。

这些问题发生在 `rust_main()` 之前。此时继续插桩 VFS、调度器或 SATA不能解释 U-Boot
是否跳到了正确代码。

### 4.3 修复产物选择，并把外设从首启变量中移除

提交 `b5826a65` 汇总了以下修改：

- `board_laqemu` / `board_2k1000` 选择不同平台模块；
- `main.rs` 对两个 feature 同时启用或 LoongArch 未选择 board 直接 `compile_error!`；
- QEMU 只编译 `boot.rs`，2K1000 只编译 `entry.asm`；
- `linker-laqemu.ld` 基址 `0x80000000`；
- `linker-2k1000.ld` 基址 `0x90000000`；
- 2K1000 uImage `Load/Entry = 0x90000000`；
- linker 模板缺失时在 rustc 前失败；
- 最小板级 initramfs 路径调用 `fs::force_ramfs()`，不让未验证外设污染首启边界。

首启的观测链因此被压缩为：

```text
TFTP -> uImage header/CRC -> entry.asm -> UART -> MM
-> initramfs -> TCB -> first __switch -> trap_return -> PLV3 initproc
```

如果仍卡住，最后一条串口探针能对应到地址/调度问题，而不是同时面对 AHCI、GMAC、
文件系统和 DMA 四类变量。

### 4.4 `b5826a65` 不是实板 PASS

该提交记录的有效证据是：双架构编译、LA64 QEMU 进入 init 用户态、2K1000 uImage
头部为 `0x90000000`。提交说明明确写了 SSD、GMAC 和实板关机仍待验证；Work_Log
当时也注明新镜像尚未在 2K1000 上复测。

因此，本报告不把 `b5826a65` 写成“首次启动成功”。首次明确的实板完整启动记录来自
后续 `4705b28d` 中的 Work_Log：TFTP 镜像经 `iminfo` 校验后，实板 `bootm` 进入
initproc，VALEN/PALEN、高栈探针、首次上下文切换和用户态入口全部通过。

## 5. 证据矩阵

| 证据 | 路径/符号 | 能证明什么 | 不能证明什么 |
|------|-----------|------------|--------------|
| `b5826a65^:os/make/la64.mk` | `LA_LOAD_ADDR=0x9000000090000000`、复制失败 `|| true` | 修复前存在格式/构建状态风险 | 没有保留逐字 U-Boot 失败日志 |
| `b5826a65` | 32 文件、1045 行新增；commit message | 平台入口、linker、地址和最小路径形成首次可审计提交 | 该提交时尚无完整实板复测 |
| `os/make/la64.mk` | `BOARD=2k1000` 分支、`LINKER_SCRIPT` 存在性检查 | 当前构建会写低 32 位地址并拒绝缺失模板 | 不替代对最终 `.ui` 的 `iminfo` 检查 |
| 两份 linker 模板 | `BASE_ADDRESS=0x80000000/0x90000000` | QEMU/实板绝对符号分离 | 不证明 CPU 实际进入该地址 |
| `entry.asm::_start` | DMW 设置、PC 高位清除、boot stack | 解释高 DMW 入口如何转入低链接视图 | 不证明后续页表/栈正确 |
| `4705b28d:docs/Work_Log.md` | 07-10 首条验证记录 | 首次明确实板 `bootm → initproc` PASS | 这是 Work_Log 记录，不冒充独立串口原文 |
| `4705b28d` 记录的 `iminfo` | payload `12339944`，Load/Entry `0x90000000`，CRC OK | 实际传输镜像头与校验和正确 | 哈希只对应当次产物 |

本文最终审计时，入口、Makefile、平台配置和随机/内存源码相对当时 HEAD 无工作区
修改；协作期间 HEAD 曾因同一 ext4 修复提交被重写，故不把易失的当前 HEAD 哈希当作
历史证据。本文的可复核历史锚点固定为 `b5826a65` 与 `4705b28d`；其他工作区修改不
用于本结论。

## 6. 根因证明

### 6.1 地址字段证明

legacy uImage 的装载地址和入口字段各为 32 位，所以可表达范围是：

```text
0 <= ih_load, ih_ep <= 0xffffffff
```

而：

```text
0x9000000090000000 > 0xffffffff
```

因此高 DMW 地址不可能作为该字段的完整值存在。板上物理装载点 `0x90000000` 在
范围内，且与 `linker-2k1000.ld` 的基址相同。`entry.asm` 再负责消除 U-Boot 当前
执行别名，两端契约闭合。

### 6.2 构建污染证明

旧规则在模板复制失败时继续执行。设生成态 `linker.ld` 当前来自平台 A：

```text
build(A) -> linker.ld=A
build(B), B模板缺失 -> copy failure ignored -> rustc still reads A
```

于是 `build(B)` 的命令成功不蕴含“产物按 B 链接”。修复后的存在性检查使第二步在
rustc 前退出，消除了对历史构建顺序的依赖。

## 7. 为什么这是根因修复

| 方案 | 是否采用 | 原因 |
|------|----------|------|
| 只在 U-Boot 命令行手工改跳转地址 | 否 | 镜像自身元数据仍错，无法复现和审计 |
| 在 uImage 头硬塞高 DMW 地址 | 否 | 32 位字段无法表达，概念上也混淆 VA/PA |
| 保留单一 linker，要求开发者手工复制 | 否 | 构建结果依赖历史状态，自动化无法保证 |
| 模板缺失时沿用旧 `linker.ld` | 否 | 产生“格式正确但平台错误”的危险产物 |
| 首启同时初始化 SATA/GMAC | 否 | 扩大故障面，最后日志无法界定入口问题 |
| 分平台入口/linker/feature + ramfs-only | 是 | 同时修复地址契约、构建确定性和故障隔离 |

## 8. 验证矩阵

| 层级 | 验证 | 结果 |
|------|------|------|
| Make 解析 | 2K1000 `LA_LOAD_ADDR/ENTRY=0x90000000` | PASS |
| Make 解析 | QEMU 与实板选择独立 linker 模板 | PASS |
| 构建防线 | 模板缺失在 rustc 前失败 | PASS（源码/规则审计） |
| 编译 | RV64、LA64 顺序构建 | PASS（`b5826a65` Work_Log） |
| 模拟器 | LA64 QEMU 进入 init 用户态 | PASS |
| 镜像 | 2K1000 `.ui` 的 Load/Entry 为 `0x90000000` | PASS |
| 传输 | TFTP 大小、`iminfo` CRC | PASS（`4705b28d` 记录） |
| 实板端到端 | `bootm → initproc`，首次切换和 PLV3 入口 | PASS（`4705b28d` 记录） |

本轮只补文档，没有重跑编译或实板测试；表中结果是对应历史提交和 Work_Log 的既有
验收，不能表述为 2026-07-15 新执行结果。

## 9. 剩余边界与风险

- 当前常规 2K1000 SATA 镜像不再永久 ramfs-only；只有救援、`sata_probe` 和
  `sata_write_probe` 等诊断路径继续隔离外盘。复盘不能把早期策略写成当前全局行为。
- 2K1000 真正 S5 关机仍是独立问题，入口修复不覆盖电源管理。
- 生成 `.ui` 后仍应对最终文件执行 `file/iminfo`、payload 大小和 CRC 检查；源码正确
  不能证明操作员传输的是同一产物。
- `linker.ld` 仍是生成态文件，禁止直接把它当作平台真值修改；应改模板。
- uImage 头正确只能证明 U-Boot 交接层，不能替代下一篇的 VALEN/TLB/栈验证。

## 10. 可复用排障流程

```text
U-Boot 无输出或首条 Rust 日志缺失
-> 对最终镜像跑 iminfo/file，核对 32-bit Load/Entry
-> 对 ELF 跑 readelf/objdump，核对 _start 与链接基址
-> 核对 board feature 只选一个入口
-> 删除“复制失败也继续”的构建兜底
-> 首启禁用所有非必要外设
-> 只在入口确认后进入地址位宽、页表和调度排查
```

## 11. 闭合证据链

```text
旧构建把64位DMW地址写入32位legacy uImage契约
+ QEMU/实板共享生成态linker与入口选择
-> 产物可生成但不保证U-Boot地址和ELF绝对符号一致
-> 按平台拆分feature、_start和linker模板
-> 2K1000头与ELF统一使用低PA 0x90000000
-> entry.asm把U-Boot的DMW执行视图切回低链接视图
-> ramfs-only排除SATA/GMAC等无关变量
-> b5826a65证明方案进入可审计提交
-> 4705b28d所含Work_Log确认iminfo CRC正确且bootm进入initproc
```

结论不是“换一个地址后碰巧启动”，而是镜像格式、链接地址、CPU 入口视图和平台构建
选择四层契约首次一致。
