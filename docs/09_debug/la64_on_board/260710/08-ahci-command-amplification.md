---
title: "AHCI 512B 命令放大与 64KiB 常驻 DMA 槽"
category: debug
status: resolved-with-known-limits
author: MangoCore Team
last_update: 2026-07-15
tags: [postmortem, performance, ahci, dma, sata, batching, 2k1000la]
code_paths:
  - "dependency/dep_iso/src/provider.rs"
  - "dependency/dep_iso/src/block/ahci.rs"
  - "os/src/drivers/block/sata_blk.rs"
related_docs:
  - "docs/09_debug/la64_on_board/260710/development-log.md"
  - "docs/09_debug/la64_on_board/260710/08a-python-bytecode-cache-bottleneck.md"
  - "docs/07_driver/2k1000-ahci.md"
entry_points:
  - "Provider::AHCI_MAX_TRANSFER_SECTORS"
  - "AHCI::read_blocks"
  - "AHCI::write_blocks"
  - "SataBlock::read_block"
  - "SataBlock::write_block"
---

# AHCI 512B 命令放大与 64KiB 常驻 DMA 槽

## 1. 摘要

AHCI 功能正确后，2K1000LA 冷读 `libpython` 5.48 MiB 只有 13.5 MB/s。根因不是
PageCache 请求过小，而是 SATA wrapper 把上层连续 buffer 再切成 512 字节：每个
sector 独立准备 FIS/PRDT、写 `PxCI` 并轮询完成。一个 256 KiB 上层请求因此放大为
512 条同步 ATA 命令。

修复在启动期分配一个 64 KiB、物理连续、低于 4 GiB 的常驻 DMA 槽；每条
`READ/WRITE DMA EXT` 最多传 128 个 sector。相同 256 KiB 请求从 512 条命令降为 4 条。
DMA 槽随 AHCI 实例复用，热路径不分配；一个 PRDT entry 按真实长度填写，ATA sector
count 与完整 LBA 范围均校验。

实板冷读从 13.5 MB/s 提升到 18.6 MB/s（+37.8%）；PageCache 热读约 108 MB/s
不变，符合“只优化打盘路径”的预期。将槽扩大到 256 KiB 后仍为 18.6 MB/s，Python
无缓存启动/导入也无可测收益，因此最终保留 64 KiB，避免永久额外占用 192 KiB
低端连续物理内存。

| 属性 | 结论 |
|------|------|
| 严重性 | Medium / P2；功能正确但大幅浪费控制器命令固定成本 |
| 修复提交 | `f133ba44`，2026-07-14 |
| 旧粒度 | 512 B / ATA command |
| 新粒度 | 最多 64 KiB = 128 sectors / ATA command |
| 256KiB 上层请求 | 512 commands -> 4 commands |
| 冷读结果 | 13.5 -> 18.6 MB/s |
| 256KiB 对照 | 18.6 MB/s，无进一步收益 |

Python 字节码缓存是另一个独立瓶颈，详见
`08a-python-bytecode-cache-bottleneck.md`。本文不把 pyc 收益归因于 AHCI。

## 2. 证据口径

本问题的取证/功能基线为 `2031fd5909355994f768f845b2935e4509290a07`；之后当前
HEAD 的前进未改变这里分析的 AHCI 批处理代码。性能
环境为 LA264 500 MHz、`TS32GMTS400` 32 GB SSD，同一 P3 CPython 文件集。

归档文档保存了 wall-time 中位数和吞吐量，但没有保留每轮 shell `time` 的完整
`real/user/sys` 原始三元组。因此本文：

- 只把 13.5/18.6 MB/s 与 1.925/1.714 s 等归档值写为实测；
- 不反推或伪造精确 user/sys；
- 用 PageCache、tmpfs 和 pyc A/B 的层级差分判断瓶颈归属。

## 3. 旧数据路径

### 3.1 上层已经提供连续请求

PageCache/ext4 的批量后端能够发出几十至 256 KiB 的连续 buffer：

```text
PageCache read-ahead / writeback
  -> Ext4PageCacheBackend contiguous run
  -> BlockDevice::read_block(block_id, buf[0..N])
```

这说明优化机会已经到达设备驱动入口。若设备层继续逐 sector 发命令，上层合并不会
转化成硬件请求合并。

### 3.2 wrapper 再拆成 512B

旧 `SataBlock`：

```rust
for sector in buf.chunks_mut(512) {
    controller.read_block(lba, sector)?;
    lba += 1;
}
```

每次 `read_block()` 都执行完整命令生命周期：

1. 清/填 command FIS；
2. 设置 command header 与 PRDT；
3. 清 `PxIS/PxSERR` 并 readback；
4. 等待 taskfile ready；
5. memory fence；
6. 写 `PxCI` slot 0；
7. 轮询 slot 完成并检查错误；
8. 从 512B DMA buffer 复制到调用者。

这部分固定成本与 payload 大小无关。对小 payload 重复 512 次，控制器/CPU 往返
远大于真正搬运数据的成本。

## 4. 命令放大量化

命令数公式：

```text
commands = ceil(request_bytes / command_payload_bytes)
```

| 上层连续请求 | 旧 512B 命令 | 64KiB 命令 | 缩减倍数 |
|--------------|--------------|-------------|----------|
| 4 KiB | 8 | 1 | 8x |
| 64 KiB | 128 | 1 | 128x |
| 128 KiB | 256 | 2 | 128x |
| 256 KiB | 512 | 4 | 128x |

即使 SSD 介质本身足够快，轮询模式仍让每条命令串行支付固定开销。该放大也解释了
为何 PageCache 热读很快：命中时根本不进入这 512 条硬件命令。

## 5. 修复设计

### 5.1 平台声明最大传输 sector 数

通用 `Provider` 新增：

```rust
const AHCI_MAX_TRANSFER_SECTORS: usize = 1;
```

默认值 1 保持其他平台旧行为。2K1000 Provider 覆盖为：

```text
SATA_DMA_BYTES   = 64 * 1024
BLOCK_SIZE       = 512
MAX_SECTORS      = 128
```

通用 AHCI 核心不假定任意平台都能提供大块连续 DMA。

### 5.2 一个常驻槽，而不是每请求分配

初始化时一次性分配：

- received FIS page；
- command list page；
- command table page；
- 64 KiB data extent。

data extent 使用 `frames_alloc(pages)`，保证物理连续。检查的是：

```text
base + pages * PAGE_SIZE <= 0x1_0000_0000
```

不能只检查 base；若 extent 跨过 4 GiB，末尾仍不可被 32 位 DMA 控制器访问。

AHCI 实例持有这段内存直到 drop。请求热路径只复制到/从常驻 slot，不触发 allocator，
也避免低端连续内存长期运行后碎片化导致偶发分配失败。

### 5.3 一个 PRDT entry 按真实长度传输

初始化验证：

```text
max sectors > 0
max sectors <= u16::MAX
max bytes <= 4 MiB          # 单 PRDT entry byte-count 上限
```

每次命令再验证：

```text
len > 0
len % 512 == 0
len <= DMA slot length
lba + len/512 <= disk sectors
```

然后填写：

```text
PRDT.DBC       = len - 1
FIS.sector_cnt = len / 512
FIS.LBA        = start LBA
```

`IDENTIFY DEVICE` 仍传 512B；`FLUSH CACHE EXT` 没有 data payload，因此 PRDT length
为 0。不能为了统一代码给 flush 虚构一段数据传输。

### 5.4 wrapper 按 64KiB 切上层请求

```rust
for chunk in buf.chunks_mut(SATA_DMA_BYTES) {
    controller.read_blocks(lba, chunk)?;
    lba += chunk.len() / 512;
}
```

write 同理；所有 chunk 完成后执行一次 `FLUSH CACHE EXT`。LBA 增量来自本次真实
chunk 长度，因此最后一个不足 64KiB、但仍 512B 对齐的 chunk 不会跳错位置。

## 6. 为什么只有一个槽

可参考 VirtIO 的“启动期预留 DMA slot”思想，但不能机械复制其多槽池：

```text
SataBlock(Mutex<AHCI<Provider>>)
```

mutex 覆盖整个 read/write，当前任何时刻只有一个在途 AHCI 请求。增加四个 data slot
不会增加并发，只会：

- 永久占用更多低 4 GiB 连续内存；
- 增加 slot 分配/回收状态；
- 扩大 DMA ownership 与错误恢复面。

当前收益来自“单命令多 sector”，不是来自多命令并行。若未来改为多 slot/中断驱动，
必须单独设计并发 ownership，不能借本次结果宣称已经安全。

## 7. 性能证据

### 7.1 控制变量

- 同一开发板、SSD、P3 CPython 文件；
- 只改变 AHCI data slot/command payload；
- PageCache cold 与 hot 分开；
- 64 KiB 后另做 256 KiB 对照；
- Python pyc 缓存单独关闭/另文报告。

### 7.2 结果

| 指标 | 512B/command | 64KiB slot | 变化 |
|------|-------------:|------------:|-----:|
| 5.48 MiB `libpython` 首次顺序读 | 13.5 MB/s | 18.6 MB/s | +37.8% |
| PageCache 命中顺序读 | ~108 MB/s | ~108 MB/s | 无变化 |
| `python3 -S -c pass` 热启动 | 1.925 s | 1.714 s | -11.0% |
| `python3 -c pass` 热启动 | 2.385 s | 2.175 s | -8.8% |
| `import json,ssl,hashlib,pathlib` | 18.322 s | 17.993 s | -1.8% |

PageCache hot 不变是正向证据：修改没有加速内存复制，它只减少真实设备命令。冷读
提升而热读不变，符合代码作用层。

Python 重导入只改善 1.8% 也不是驱动修复无效；该工作负载大部分 wall time 不在
SSD 读取，详见 08a。

### 7.3 256KiB 对照与停止条件

把常驻 slot 试验性扩大到 256 KiB 后：

- 5.48 MiB 冷读仍为 18.6 MB/s；
- Python 无 pyc 的启动/导入与 64 KiB 版无可测改善；
- 需永久多占 192 KiB 低端连续物理内存。

这说明 64 KiB 之后瓶颈已移到控制器/介质/CPU 复制或上层，继续扩大命令 payload
没有收益。最终回收到 64 KiB 是由 A/B 证据决定，不是凭经验选择。

## 8. 正确性验证

性能修改同时改变读写命令长度，必须证明不是“更快地写错”：

- RV64/LA64 串行编译通过；
- 两架构 QEMU 启动、挂载并运行多批 LTP，无 AHCI API 回归；
- 2K1000 P2 scratch 冒烟与 P4 reuse 通过；
- 实板向 P4 写 1 MiB 随机数据，调用 `sync`；
- 源/目标 SHA-256 一致，`cmp=0`；
- 删除并再次 `sync`；
- 64 KiB 与 256 KiB 两版 uImage 均通过 TFTP length/CRC 和 `iminfo`；
- 最终选择 64 KiB 版完成后续实板门禁。

## 9. 排除的假设

| 假设 | 证据 | 结论 |
|------|------|------|
| PageCache 总是只发 512B | 上层可提供 64/256KiB 连续 buffer | 排除 |
| SSD 介质只有 13.5 MB/s | 仅合并命令即到 18.6 MB/s | 排除 |
| 热读也被 AHCI 限制 | ~108 MB/s 前后不变 | 排除 |
| 多 DMA slot 可直接更快 | controller mutex 仅一个在途请求 | 当前架构下排除 |
| slot 越大越好 | 256KiB 与 64KiB 同为 18.6 MB/s | 排除 |
| Python 18s 主要是磁盘 | DMA 仅降低到 17.993s | 排除，另见 08a |

## 10. 已知边界

1. 单 PRDT entry 只覆盖物理连续 slot；当前实现通过 staging copy 实现，不是上层
   buffer 零拷贝。
2. controller mutex 保证 slot 独占，也限制了并发；本文不证明 NCQ/multi-slot。
3. 写路径在所有 chunk 后 flush 一次；未来异步 writeback 需重新审计 flush 时序。
4. 32 位 DMA 要求整段低于 4 GiB；frame allocator 改动后必须回归。
5. 18.6 MB/s 是这块板/SSD/轮询实现的观测值，不是通用 AHCI 性能上限。
6. 缺少原始 `real/user/sys` 全量日志，不能从 wall time 精确拆分系统态收益。

## 11. 可复用调试方法

### 11.1 在每层记录请求粒度

```text
syscall bytes
PageCache pages/run
filesystem physical run
BlockDevice buf.len
driver chunk.len
hardware command sector_count
```

只有最后一项与上层接近，批处理才真正到达硬件。上层日志显示 256 KiB，不代表设备
没有再拆成 512B。

### 11.2 增大 buffer 必须设停止对照

至少比较两档，并同时记录吞吐与常驻内存。若 64 -> 256 KiB 无收益，应回收内存，
转查下一层，而不是继续扩大到协议上限。

## 12. 最终因果链

```text
PageCache/ext4 已合并连续 I/O
  -> SataBlock 又按 512B 拆分
  -> 256KiB 请求变成 512 次 PxCI + polling
  -> 冷读只有 13.5 MB/s

64KiB 连续低端常驻 DMA slot
  + one PRDT / multi-sector ATA command
  -> 256KiB 请求只需 4 条命令
  -> 冷读 18.6 MB/s，热缓存不变

扩大到 256KiB 无进一步收益
  -> 命令粒度不再是主瓶颈
  -> 保留 64KiB，避免无收益的低端连续内存占用
```
