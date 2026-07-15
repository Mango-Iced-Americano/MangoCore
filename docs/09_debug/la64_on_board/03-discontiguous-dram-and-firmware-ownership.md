---
title: "2K1000LA 2 GiB 内存：非连续 DRAM 与固件所有权"
category: debug
status: resolved-with-reservations
author: MangoCore Team
date: 2026-07-15
last_update: 2026-07-15
tags: [loongarch64, 2k1000la, memory, frame-allocator, dma, firmware, carveout]
code_paths:
  - "os/src/hal/arch/loongarch64/config.rs"
  - "os/src/mm/frame_allocator.rs"
  - "os/src/mm/kernel_space.rs"
  - "os/src/mm/address_space.rs"
  - "os/src/main.rs"
  - "os/src/fs/initramfs.rs"
  - "os/src/drivers/block/virtio_blk.rs"
  - "os/src/drivers/block/virtio_blk_pci.rs"
  - "os/src/syscall/process/ipc.rs"
related_docs:
  - "docs/09_debug/la64_on_board/02-valen40-kernel-stack-and-tlb.md"
  - "docs/04_mm/frame-allocator.md"
  - "docs/04_mm/initialization-and-kernel-space.md"
entry_points:
  - "for_each_usable_frame_region"
  - "StackFrameAllocator::init"
  - "frames_alloc"
  - "frames_alloc_any"
  - "frame_reclaim_linker_range"
---

# 2K1000LA 2 GiB 内存：非连续 DRAM 与固件所有权

## 1. 一句话结论

`bdinfo` 显示 2 GiB 并不等于内核可以分配从 `0` 到 `2 GiB` 的连续物理地址：
2K1000LA 的两段 DRAM 之间夹着约 2 GiB 地址空洞，低 bank 顶部又仍由 U-Boot、
DVO framebuffer、CPU1 park loop 和启动数据占用。根因修复不是简单放大
`MEMORY_SIZE`，而是把**容量、物理地址上界、DRAM region、固件保留区和当前可用
容量**拆成五个独立事实，并让清零、直接映射、页帧分配和 DMA 全部遵守同一份
ownership map。

## 2. 问题卡

| 项目 | 结论 |
|------|------|
| 目标 | 从最小上板内存扩到板载 2 GiB，同时不破坏固件和活动设备状态 |
| 危险旧假设 | `MEMORY_START..MEMORY_END` 是一段连续、全部归内核所有的 RAM |
| 物理拓扑 | bank0 `[0, 0x10000000)`；bank1 `[0x90000000, 0x100000000)` |
| 地址空洞 | `[0x10000000, 0x90000000)`，不是可分配 DRAM |
| 静态 carveout | `[0x0cbf4000, 0x10000000)`，共 53,296 KiB |
| 额外排除 | 物理第 0 页；当前内核镜像；尚未完成最后读取的 linker payload |
| 直接后果 | 线性清零会访问空洞；线性 allocator 会把 MMIO 当页；DMA extent 可能跨 bank |
| 根因修复提交 | `29a8f40a3961b9b36b9f3c42bfdefdec9ebc4668` |
| 实板闭环 | 320 MiB RamFS 跨 bank 压力、低 bank AHCI DMA、内存统计均通过 |
| 当前保留 | 约 52.05 MiB 仍不归 MangoCore；没有完成动态 firmware handoff |

## 3. 先把五个容易混淆的量分开

### 3.1 安装容量不是地址区间

板载总容量为 2 GiB：

```text
bank0 capacity = 0x10000000                         = 256 MiB
bank1 capacity = 0x100000000 - 0x90000000           = 1792 MiB
installed      = 0x10000000 + 0x70000000            = 0x80000000 = 2 GiB
```

但最高 DRAM 地址到达 `0x100000000`，原因是 bank1 从 `0x90000000` 开始。于是：

```text
MEMORY_SIZE = 0x80000000       # 安装容量，不能用于 addr < start + size 判断
MEMORY_END  = 0x100000000      # 物理地址上界，不能表示中间全部是 RAM
```

本平台 `MEMORY_START=0x90000000` 仍表示内核装载 bank；若用
`MEMORY_START + MEMORY_SIZE` 构造单区间，会漏掉整个低 bank，并虚构
`[0x100000000, 0x110000000)` 的 256 MiB。若改从 0 开始用容量作终点，又会包含地址
空洞且漏掉真正的高 bank。若把 `0..MEMORY_END` 当连续 RAM，则会把
`[0x10000000, 0x90000000)` 全部放进 allocator。这些错误方向不同，却来自同一个
抽象混用。

### 3.2 DRAM 存在也不代表所有权已经交给内核

`bootm` 只完成控制流跳转，不自动撤销 U-Boot 和设备对内存的占用。上板审计记录
确认低 bank 顶部仍包含：

- U-Boot LMB、栈和堆；
- 活动 DVO framebuffer，设备可能继续 DMA；
- CPU1 仍执行 U-Boot park loop 的代码/数据；
- BPI、SMBIOS 等启动信息。

因此当前使用单个保守 carveout：

```text
reserved = [0x0cbf4000, 0x10000000)
size     = 0x0340c000 = 53,296 KiB = 52.046875 MiB
```

再排除物理第 0 页后，对用户 ABI 报告的安全容量为：

```text
USABLE_MEMORY_SIZE
  = 0x80000000 - 0x0340c000 - 0x1000
  = 0x7cbf3000
  = 2,043,852 KiB
```

这里的 `USABLE_MEMORY_SIZE` 是“内核当前拥有的 RAM 容量”，不是实时 `MemFree`；
内核镜像、页表和运行时分配会继续从 free 值中扣除。

### 3.3 一份 ownership map 必须约束所有内存消费者

只修 allocator 不够。启动清零、内核直接映射、页表映射、DMA、统计和回收路径
若各自猜测 RAM 边界，迟早会重新触碰空洞或保留区。最终以两张平台表为源：

```rust
MEMORY_REGIONS = &[
    (0x0000_0000, 0x1000_0000),
    (0x9000_0000, 0x1_0000_0000),
];

FIRMWARE_RESERVED_REGIONS = &[
    (0x0cbf_4000, 0x1000_0000),
];
```

对“可交给页帧分配器”的区间，还必须排除第 0 页和 `[skernel, ekernel)`。最终
fresh allocator 的实板探针所见是：

```text
low  usable: [0x00001000, 0x0cbf4000)
high usable: [ekernel,     0x100000000)
```

高 bank 的开头装着从 `0x90000000` 链接的内核，所以不能从 bank1 起点直接分配。

## 4. 故障模型：旧的线性实现为什么危险

### 4.1 启动清零会在 CPU 尚未稳定时访问非 RAM

单 bank 时代的清零逻辑可抽象为：

```text
for address in sbss .. MEMORY_END:
    *(address) = 0
```

由于本内核链接在高 bank，原 `sbss..MEMORY_END` 只覆盖高 bank：它不会清零新纳入
allocator 的低 bank，破坏 `zero_init` “fresh frame 已预清零”的不变量。若为覆盖低
bank 而把起点直接降到 `0x1000`，又会线性越过低 bank 顶部 firmware carveout 和
`[0x10000000, 0x90000000)` 地址空洞。两种写法分别是“漏清”和“越界清”，都不能
表达真实拓扑。

越界清零的破坏还可能延迟出现：清掉 CPU1 park code、framebuffer 或固件表后，故障
可到中断、显示 DMA 或次核活动时才表现为“随机上板死机”。修复后的 `zero_init` 按
`MEMORY_REGIONS` 逐段处理：低 bank 从物理第 1 页开始并跳过 firmware carveout；
包含内核的高 bank 从 `sbss` 开始，只清 BSS 和其后的 fresh 范围，从而保留此前的
text/rodata/data/链接 payload。它不再用一个容量或地址上界推导清零跨度。

### 4.2 单区间 frame allocator 会制造伪页

旧 allocator 只保存一个 `[start, end)` 游标。一旦 `end` 提到 4 GiB，它无法表达
中间的洞，最终会返回 `0x10000000..0x90000000` 中的 PPN。后续错误可能落在不同层：

- 清零该页时同步异常；
- 将 MMIO 地址作为普通缓存内存访问；
- 页表把设备寄存器映给用户；
- DMA 描述符把“连续长度”跨过空洞交给设备。

修复后的 `StackFrameAllocator` 为每个 region 保存独立 `current/end` 和回收位图。
单页 fresh 分配耗尽一个 region 后才前进到下一个 region，永不把洞注册为 frame。

### 4.3 “连续调用单页分配”不等于物理连续

多 region 后，这段伪代码不再成立：

```text
for i in 0..n:
    pages.push(frame_alloc())
# pages[i + 1].pa 不保证等于 pages[i].pa + 4096
```

在 region 边界、回收栈或 linker-reclaimed 页出现时，连续调用可得到任意离散 PPN。
因此最终明确拆成：

| API | 保证 | 使用者 |
|-----|------|--------|
| `frames_alloc(n)` | 在同一个 ownership region 内返回一段物理连续 extent | VirtIO 等 DMA |
| `frames_alloc_any(n)` | 只保证获得 n 个页，允许物理离散 | SysV SHM 等有页表承接的映射 |

`frames_alloc` 先寻找同区间 recycled extent，再找能够容纳完整长度的 fresh region；
region 尾部不够时会跳到下一 region，但绝不会拼接两边的尾/头。

## 5. 调试追溯过程

### 5.1 第一阶段：拒绝“2 GiB 就把常量改大”的假设

最初需要回答的不是“板子有多少内存”，而是以下四个独立问题：

1. 哪些物理地址真正译码到 DRAM？
2. 哪些 DRAM 在 `bootm` 后仍被固件或设备使用？
3. 内核镜像和嵌入 payload 占了哪些页？
4. 哪些消费者要求物理连续？

`bdinfo`/启动参数只能支持第一个问题的一部分，不能证明所有权。对照板级内存图、
U-Boot 运行状态和设备状态后，得到两个 bank 与低 bank 顶部 carveout。由于本仓库
没有保留这一阶段完整原始串口文本，固件 owner 列表属于 `docs/Work_Log.md` 和提交
`29a8f40a` 中的审计记录，不能伪装成可逐行复核的 raw log。

### 5.2 第二阶段：让代码显式暴露错误假设

全仓审计把 `MEMORY_SIZE`、`MEMORY_END` 和单区间 allocator 的消费者分成四类：

| 类别 | 旧风险 | 修复 |
|------|--------|------|
| 启动清零 | 从一个起点线性写到地址上界 | 遍历 DRAM region 并减去 exclusions |
| 内核映射 | 把整个地址跨度直接映射 | 逐 region 建立 direct mapping |
| 页帧分配 | 只有一个 fresh 游标 | region-local 游标和回收标记 |
| 容量 ABI | 把地址跨度或安装容量当可用量 | 使用 `USABLE_MEMORY_SIZE` |

同时，dirty-frame bitmap 必须覆盖**最高物理 PPN**而不是只覆盖 2 GiB 个连续字节。
原因是高 bank 地址达到 4 GiB 附近；bitmap 的索引域是 PA/PPN，不是容量排名。

### 5.3 第三阶段：处理 linker payload 的延迟所有权移交

initramfs/预装 payload 位于内核 ELF 映像内，allocator 初始化时必须视为内核所有，
否则在复制完成前就可能被再次分配。另一方面，永久保留会浪费数 MiB。

最终路径是：

```text
linker embeds payload
  -> allocator excludes [skernel, ekernel)
  -> fs copies payload to its final owner
  -> caller proves no future linker-symbol read
  -> frame_reclaim_linker_range(full_page_start, full_page_end)
  -> separately registered reclaimed region enters free list
```

只回收完整页；头尾与仍存活的内核对象共享的部分页不释放。这是显式 ownership
handoff，而不是因为“地址落在 DRAM”就调用普通 `frame_dealloc()`。

### 5.4 第四阶段：用跨 bank 压力而不是启动成功收口

仅启动到 shell 不能证明 allocator 会跨过 region0 尾部。实板专门向 RamFS 写入
320 MiB，使 fresh 分配必然从低 region 前进到高 region；随后删除文件，验证回收。
再用 AHCI 只读探针验证低 bank DMA buffer 和物理连续约束没有被多 region 改造破坏。

## 6. 证据链

### 6.1 源码/提交证据

| 证据 | 能证明什么 | 不能证明什么 |
|------|------------|--------------|
| `29a8f40a` 的 platform config | 两个 DRAM region、carveout 和容量公式进入代码 | carveout 内每个 owner 已被动态停用 |
| `for_each_usable_frame_region` | 页 0、固件区、内核镜像被统一排除 | 设备 DMA 在未来绝不越界 |
| `StackFrameAllocator` 多 region 状态 | allocator 不会 fresh 分配地址空洞 | 所有外部驱动都使用了正确 DMA API |
| `frames_alloc` / `frames_alloc_any` 拆分 | 连续 DMA 与离散普通页语义分离 | 任意第三方新调用者不会误选 API |
| linker reclaim 的独立登记 | payload 最后读取后才发生所有权移交 | 部分页可以安全回收 |

### 6.2 数值自洽证据

```text
MEMORY_REGIONS capacity
  = 0x10000000 + (0x100000000 - 0x90000000)
  = 0x80000000

reserved
  = 0x10000000 - 0x0cbf4000
  = 0x0340c000

reported
  = 0x80000000 - 0x0340c000 - 0x1000
  = 0x7cbf3000
  = 2043852 KiB
```

实板 `/proc/meminfo` 和 BusyBox `free` 都报告 `MemTotal=2043852 kB`，与源码公式
逐字节一致。这排除了“统计仍沿用旧常量”这一类假通过。

### 6.3 实板正向证据

以下结果来自提交 `29a8f40a` 对应的 `docs/Work_Log.md` 验收记录；仓库未保留该轮
完整独立串口文件，因此证据等级低于可直接逐行读取的 raw log：

```text
RAMFS length   = 335,544,320 bytes (320 MiB)
checksum       = 2,699,711,059
allocator      = fresh region0 -> region1
delete         = success
MemFree delta  = 4 KiB

AHCI model     = TS32GMTS400
operation      = read-only, LBA0 twice
result         = two reads identical, MBR signature 55aa
```

320 MiB 大于 low usable region，因而“切换到 region1”不是仅靠日志文字，而由压力规模
强制发生。删除后仅差一页同时验证大部分 frame 被归还，而不是悄悄泄漏在第二 bank。

AHCI 探针只读且重复读一致，证明低 bank 的连续 DMA buffer 可工作；它不证明任意长度
DMA 都可分配，也不授权写 SSD。

### 6.4 QEMU 和构建回归

同一 Work_Log 记录了：

- RV64 与 LA64 release kernel 串行编译通过；
- LA64 QEMU 4 GiB 测试盘启动，VirtIO block/entropy/net 正常，Ext4 挂载并运行 LTP；
- 2K1000LA clean uImage 生成，load/entry 均为 `0x90000000`；
- `git diff --check` 通过。

QEMU 回归主要证明抽象改造没有破坏原连续 RAM 平台；真正证明非连续拓扑的是实板
跨 bank 压力。

## 7. 排除过的错误方向

### 7.1 只增大 `MEMORY_SIZE`

这只改变容量数字，既不能表达 2 GiB 地址空洞，也不能表达固件 owner。结果可能是
`free` 看起来变大，同时 allocator 开始分配 MMIO，是最危险的“指标先绿”。

### 7.2 把 `MEMORY_END` 当作连续区间结尾

`0x100000000` 是高 bank 的上界，不是从零开始全部为 RAM 的证明。任何
`for addr in 0..MEMORY_END` 形式都必须重新审查。

### 7.3 先全部回收，再观察是否稳定

framebuffer/其他 CPU/DMA 的写入是异步的。启动一次不崩溃不能证明没有 double owner；
压力下的页表或文件数据会被设备晚到写破坏。没有 owner-by-owner handoff 前，保留区
必须保持不可分配。

### 7.4 用离散页满足 DMA 长度

设备拿到首地址和长度后会线性递增物理地址，不理解 Rust `Vec<FrameTracker>` 中的
页列表。只有 `frames_alloc` 的单 region 连续保证可用于这类 DMA。

## 8. 修复后的不变量

1. 物理地址只有在 `MEMORY_REGIONS` 中才可能是 RAM。
2. RAM 地址还必须避开 `FIRMWARE_RESERVED_REGIONS` 才可能分配。
3. 第 0 页、内核镜像和未 handoff 的 linker payload 永不进入 fresh allocator。
4. 启动清零与内核映射遍历 region，不遍历容量跨度。
5. DMA extent 完整落在同一 ownership region；普通页集合才允许离散。
6. 统计容量来自安全可用容量，不把 carveout 冒充给用户。
7. 回收链接器页必须是最后一次读取后的显式 unsafe ownership transfer。

## 9. 验证清单与判定边界

| 门禁 | 结果 | 说明 |
|------|------|------|
| 双架构串行 release build | PASS | Work_Log 记录 |
| LA64 QEMU block/net/entropy/Ext4 | PASS | 防止通用 allocator 回归 |
| 实板 region 边界 probe | PASS | 首尾可写并恢复，未触碰洞/保留区 |
| 320 MiB RamFS 跨 bank | PASS | 强制 low -> high |
| 删除后内存回收 | PASS | `MemFree` 差 4 KiB |
| AHCI 低 bank 连续 DMA | PASS | 只读 LBA0，一致且 `55aa` |
| 容量 ABI | PASS | `2043852 kB` 与公式一致 |
| carveout 动态回收 | NOT DONE | 当前仍为静态保留 |

## 10. 剩余风险与后续条件

当前“resolved”只表示安全启用约 1.95 GiB，不表示 2 GiB 每一页都已交给内核。
若要回收 `[0x0cbf4000, 0x10000000)`，必须逐 owner 完成：

1. 关闭 DVO，等待所有在途 DMA 完成并处理 cache 一致性；
2. 将 CPU1 重停放到 MangoCore 自有代码，确认不再取指/读写 U-Boot 区；
3. 复制仍需使用的 BPI/SMBIOS 数据，或明确丢弃；
4. 确认 U-Boot 初始化过的其他设备不再向旧 buffer DMA；
5. 将可释放范围按页对齐拆分，经过与 linker reclaim 等价的显式 handoff；
6. 重跑跨 bank、DMA、并发和长时间压力门禁。

在上述条件完成前，删除 carveout 常量不是优化，而是制造两个 owner 同时写同一物理页。

## 11. 证据索引

| 类型 | 位置 |
|------|------|
| 根因修复提交 | `29a8f40a3961b9b36b9f3c42bfdefdec9ebc4668` |
| 平台拓扑 | `os/src/hal/arch/loongarch64/config.rs` |
| ownership 切分 | `os/src/mm/frame_allocator.rs::for_each_usable_frame_region` |
| 连续/离散 API | `os/src/mm/frame_allocator.rs::{frames_alloc,frames_alloc_any}` |
| 启动清零 | `os/src/main.rs` 的 `zero_init` 分支 |
| payload handoff | `os/src/fs/initramfs.rs`、`frame_reclaim_linker_range` |
| 数值和实板验收记录 | `docs/Work_Log.md` 2026-07-13 `board/mm` 条目 |

## 12. 可复用结论

非连续内存适配的核心不是“把所有 DRAM 填进数组”，而是建立可审计的所有权模型：

```text
installed capacity
  != physical address ceiling
  != list of DRAM ranges
  != kernel-owned ranges
  != physically contiguous DMA extents
```

只要这五层被压成一个 `MEMORY_SIZE`，启动清零、allocator、DMA 和统计之间就一定会有
至少一个说谎。实板验收必须同时包含跨 region 压力、回收、DMA 和容量公式，而不能以
“能进 shell”收口。
