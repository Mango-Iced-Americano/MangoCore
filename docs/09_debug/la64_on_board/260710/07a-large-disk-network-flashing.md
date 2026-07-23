---
title: "完整磁盘镜像大于 DRAM 时的分块网络刷盘协议"
category: debug
status: completed-manual-procedure
author: MangoCore Team
last_update: 2026-07-15
tags: [postmortem, flashing, tftp, u-boot, scsi, crc32, disk-image, 2k1000la]
code_paths:
  - "scripts/make_2k1000_full_test_disk.py"
related_docs:
  - "docs/09_debug/la64_on_board/260710/development-log.md"
  - "docs/09_debug/la64_on_board/260710/07-safe-p4-persistence-protocol.md"
  - "docs/03_fs/2k1000-full-test-disk.md"
entry_points:
  - "U-Boot tftpboot"
  - "U-Boot crc32"
  - "U-Boot scsi write"
  - "U-Boot scsi read"
---

# 完整磁盘镜像大于 DRAM 时的分块网络刷盘协议

## 1. 摘要

2K1000LA 完整测试盘 raw image 为 6,443,499,520 字节（6145 MiB），开发板 DRAM
只有 2 GiB，无法一次 `tftpboot` 到内存再写 SSD。最终采用 256 MiB 固定块协议：
24 个完整块加最后 1 MiB 尾块，每块依次完成“传输长度 -> 内存 CRC -> SCSI 写入
计数 -> SSD 读回 -> 读回 CRC”，确认后才推进下一个固定 LBA。

协议的核心不是 `split`，而是把每个不可信边界都设为独立门禁：

```text
host chunk
  -> network transfer
  -> board DRAM
  -> U-Boot SCSI command
  -> SSD media
  -> board DRAM readback
```

每跨一层都比较长度或 CRC。写入前核对 SSD 型号、容量与镜像 manifest；目标 LBA
按块序号公式人工计算、逐条核对。仓库当时没有完整盘 writer 自动强制这一映射，
所以“块名与 LBA 同步推进”是操作纪律，不是代码保证。任何短传、短写或 CRC 不一致
都应立即停止，不能继续写后续块掩盖第一个失败。

2026-07-11，实体 `TS32GMTS400` 完成全部 25 块、12,584,960 个 sector 的读回
验证，总耗时 25.2 分钟；复位后 U-Boot 可读 P1/P2/P3，MangoCore 也识别并只读
挂载三个分区。

| 属性 | 结论 |
|------|------|
| 风险级别 | Critical / P0，完整刷盘从 LBA0 开始，会覆盖分区表与全部现有数据 |
| 镜像生成/QEMU | `2effeaaf`，2026-07-11 |
| 25 块实板刷写归档 | `85dd29af`，2026-07-11 |
| 镜像大小 | 6,443,499,520 B = 6145 MiB |
| 目标盘 | `TS32GMTS400`，62,533,296 个 512B sector |
| 块大小 | 256 MiB = `0x80000` sectors |
| 块数 | 24 个 256 MiB + 1 个 1 MiB |
| 最后范围 | `0xC00000..0xC00800` |
| 完整镜像 sector | `0xC00800` = 12,584,960 |
| 实板结果 | 25/25 块传输、写入、读回 CRC 全通过 |

## 2. 为什么不能“一次 TFTP”

完整 raw image 同时包含：

- LBA0 的 MBR 与 disk id；
- P1 4 GiB ext4；
- P2 1280 MiB FAT32；
- P3 768 MiB ext4；
- 最终对齐尾部。

镜像 6145 MiB，显著大于 2 GiB DRAM。即使 U-Boot 接受一个看似足够高的 load
address，也不能让物理内存容纳 6 GiB 连续 payload；溢出会覆盖 U-Boot 自身、其他
固件数据或不可用地址。

压缩包约 400 MiB 也不能直接写盘，因为 SSD 需要解压后的 raw bytes；U-Boot 路径
没有被验证为边解压边写。可靠方案必须在宿主完成解压和切块。

## 3. artifact 与目标身份

### 3.1 已验证 artifact

```text
file       mango-2k1000la-full-test-mbr.img
bytes      6443499520
MiB        6145
sectors    12584960
SHA-256    416f84060bca79ab06ef5596d8cfd1801b8ae3e56ae3d2e65e99a66b612ef19f
disk id    0x4d414e47
```

生成器构建 P3、检查 ext4、格式化 P2、嵌入 P1/P3 后逐字节复核，并输出 layout JSON。
刷盘前使用 raw 文件而不是 `.xz` 压缩包，也不能使用已经被 QEMU 写过且未通过
`e2fsck -f -n` 的工作副本。

### 3.2 目标设备门禁

写第一块之前必须用 `scsi reset/info` 核对：

- 型号精确为 `TS32GMTS400`；
- 容量大于等于 12,584,960 sectors；已归档实盘容量为 62,533,296 sectors；
- `scsi write` 命令在该 U-Boot/控制器上可用；
- DRAM load range 已在当前板型上验证。

仅核对“容量约 32GB”不够。两个同容量磁盘可能一个是宿主系统盘、一个是目标盘；
型号和容量应联合检查。完整刷盘没有 P4 协议那样的旧 MBR 回滚安全网。

## 4. 块划分与 LBA 推导

宿主切块：

```bash
split -d -b 256m -a 2 \
  mango-2k1000la-full-test-mbr.img \
  mango-full.part-
```

对 `i=0..23`：

```text
chunk bytes    = 0x10000000 = 256 MiB
chunk sectors  = 0x80000
start_lba(i)   = i * 0x80000
end_lba(i)     = (i + 1) * 0x80000
```

尾块：

```text
index          = 24
start_lba      = 0xC00000
bytes          = 0x100000 = 1 MiB
sectors        = 0x800
end_lba        = 0xC00800
```

算术闭环：

```text
24 * 256 MiB + 1 MiB = 6145 MiB
24 * 0x80000 + 0x800  = 0xC00800 sectors
0xC00800 * 512        = 6443499520 bytes
```

这三种表达必须一致。若文件实际尾块大小、sector 数或最终 LBA 任一不符，停止而不是
四舍五入。

## 5. 单块协议

固定加载地址：

```text
0x9000000098000000
```

以第 0 块为例：

```text
tftpboot 0x9000000098000000 mango-full.part-00
crc32    0x9000000098000000 0x10000000
scsi write 0x9000000098000000 0x0 0x80000
scsi read  0x9000000098000000 0x0 0x80000
crc32    0x9000000098000000 0x10000000
```

### 5.1 五个必须通过的检查

| 步骤 | 检查 | 排除的错误 |
|------|------|------------|
| TFTP | `Bytes transferred` 等于本块文件大小 | 超时、短传、错误文件 |
| 内存 CRC | U-Boot CRC 等于宿主预计算 CRC | 网络损坏、TFTP 根文件错误 |
| SCSI write | 报告完整 sector 数 written OK | 短写、命令失败 |
| SCSI read | 报告完整 sector 数 read OK | 读回命令失败 |
| 读回 CRC | SSD 读回 CRC 等于宿主 CRC | 写偏、介质/控制器数据错误 |

“SCSI write: OK”只证明命令层报告成功，不证明目标内容。读回必须先覆盖同一 DRAM
buffer，再对读回数据计算 CRC；不能误对仍在内存中的原 TFTP 数据重复算 CRC。

### 5.2 人工推进纪律

归档的 25 块刷写是人工串口流程。操作者只有在五项全通过后，才按公式把 LBA 增加
当前块 sectors；块名和 LBA 应由同一序号推导：

```text
part-07 <-> start 7 * 0x80000
```

但仓库没有脚本读取 ledger 并自动生成下一条命令，实际仍依赖人工复制、修改和复核。
最危险的错误是文件块推进了而 LBA 没推进，导致覆盖前一块，且当前块自身读回 CRC
仍会“通过”。公式只能指导核对，不能把人工流程描述成“不接受错误 LBA”的程序门禁。

## 6. 中断与失败策略

### 6.1 fail-stop，而不是“尽量写完”

遇到以下任一条件立即停止：

- TFTP bytes 不足；
- 内存 CRC 不匹配；
- `scsi write/read` sector 计数不匹配；
- 读回 CRC 不匹配；
- 串口失去 prompt；
- SSD 型号或容量变化；
- 人工不确定当前块序号/LBA。

继续写后续块不会修复前一块，只会让磁盘形成更难判断的混合状态。

### 6.2 完整盘刷写没有“旧 MBR 可见性保护”

第 0 块包含 LBA0，所以一旦第 0 块写入，旧磁盘布局已经被覆盖。如果第 10 块失败：

```text
MBR + early P1 data = new image
later P1/P2/P3      = old or未写状态
```

此时 U-Boot 可能仍能列出新分区表，甚至读取前部目录，但不能把它视为可启动完整盘。
安全恢复是从已验证 artifact 重新完成全盘协议；若选择断点续写，也必须有持久化的每块
host CRC/LBA ledger，并在最后重新读回**所有**块。当前仓库归档的是人工验证流程，
没有一个可证明断点 ledger 的完整盘自动写入脚本，因此默认策略是整盘重刷。

这与 P4 的 payload-first/MBR-last 不同：P4 在 payload 完整前不发布分区项，详见
`07-safe-p4-persistence-protocol.md`。

### 6.3 不用固定 sleep 驱动串口

TFTP、CRC 和 SCSI 命令耗时随网络和设备状态变化。自动化或人工流程都应等待 U-Boot
完整 prompt，再发送下一条命令；发送成功不代表上一条执行完成。任何 prompt 丢失都
应停止并重新确认当前块状态。

## 7. 全盘完成后的独立验收

全部 25 块完成后先 `scsi reset`，再从设备端检查：

```text
ext4ls scsi 0:1 /
fatls  scsi 0:2 /
ext4ls scsi 0:3 /
```

随后启动 MangoCore，确认：

- `/dev/sda1` 为 Ext4；
- `/dev/sda2` 为 Fat32；
- `/dev/sda3` 为 Ext4；
- P1 `/sdcard` 与 P3 `/tools` 为 RDONLY；
- `/tools`、`/musl`、`/glibc` bind 成功；
- 从 P1 读取到预期 `os_test.conf`。

这组验证分别覆盖分区表、三种 FS 读取和内核挂载路径。只看最后一个块 CRC，无法
证明 MBR、P1 头部或中间任一块正确。

## 8. 实板证据

`85dd29af` 对 2026-07-11 实板流程归档：

```text
target model       TS32GMTS400
chunks             25
full chunks        24 * 256 MiB
tail               1 MiB
verified sectors   12584960
elapsed            25.2 min
post-write U-Boot  P1/P2/P3 readable
post-write kernel  three partitions detected and mounted as designed
```

每块均完成 TFTP 内存 CRC、`scsi write`、`scsi read` 与读回 CRC，不是只在最后做
总体验证。

## 9. 证据边界

1. `2effeaaf` 归档镜像生成和 QEMU；`85dd29af` 才归档 25 块实板刷写，二者不能合并
   成一个提交证据。
2. 仓库保留了生成器、命令协议和 Work Log 结果，但当前没有专用的完整盘自动 writer；
   不应把手工流程描述成自动强制 LBA、rollback 或断点 ledger 的脚本。
3. CRC32 用于发现传输/写入差异，不提供密码学 artifact 身份；完整 raw artifact 另用
   SHA-256 固定。
4. 读回立即一致不能证明断电后介质一定保持；本轮通过复位后文件系统读取补充验证。
5. 从 LBA0 全盘刷写不可回滚，必须在操作前备份有价值数据并确认目标盘。
6. 256 MiB 是已验证的 DRAM/TFTP 折中，不是协议允许的任意值；改块大小需重新计算
   sectors、load range、timeout 和尾块。

## 10. 可复用检查表

- [ ] raw artifact 大小、SHA-256 和 layout manifest 已核对；
- [ ] 目标型号和 sector 容量已核对；
- [ ] chunk bytes 是 512 的整数倍；
- [ ] `start_lba = cumulative_bytes / 512`，不用手工猜；
- [ ] TFTP length 和内存 CRC 通过；
- [ ] write/read sector count 通过；
- [ ] 读回 CRC 与宿主 CRC 一致；
- [ ] 失败立即停止，不推进块号；
- [ ] 完成后重新扫描并从每个文件系统读内容；
- [ ] 启动内核验证真实挂载，而不只看 U-Boot 分区表。

## 11. 最终因果链

```text
raw image 6145MiB > board DRAM 2048MiB
  -> 无法单次 TFTP
  -> 24*256MiB + 1MiB 固定切块

网络传输、DRAM、SCSI 命令和 SSD 都可能独立失败
  -> 每块执行 length + memory CRC + write count + read count + readback CRC

完整盘从 LBA0 开始、无可见性事务
  -> 任一失败后磁盘都视为不完整
  -> fail-stop，默认从可信 raw image 重刷

25 块全部读回 + 重扫三个 FS + 内核实际挂载
  -> 证明 6GiB 级镜像在 2GiB 板上完成端到端落盘
```
