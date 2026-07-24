---
title: "MBR 512B LBA、平台块与文件系统原生块的三层适配"
category: debug
status: resolved-with-known-limits
author: MangoCore Team
last_update: 2026-07-15
tags: [postmortem, block-device, mbr, ext4, fat32, block-size, adapter, alignment]
code_paths:
  - "os/src/drivers/block/partition.rs"
  - "os/src/fs/filesystem.rs"
  - "os/src/fs/mod.rs"
  - "os/src/syscall/fs.rs"
related_docs:
  - "docs/09_debug/la64_on_board/260710/development-log.md"
  - "docs/09_debug/la64_on_board/260710/05a-readonly-mount-propagation.md"
  - "docs/03_fs/init-and-rootfs.md"
  - "docs/03_fs/2k1000-full-test-disk.md"
  - ".agents/skills/mango-workflow/references/debugging-patterns.md"
entry_points:
  - "probe_mbr"
  - "PartitionBlockDevice::new"
  - "BlockSizeAdapter::new"
  - "adapt_filesystem_device"
---

# MBR 512B LBA、平台块与文件系统原生块的三层适配

## 1. 摘要

2K1000LA SATA 文件系统首次挂载时，旧分区设备把 MBR 的 512 字节 LBA 先除以平台
`BLOCK_SZ/512`，只接受平台块对齐的分区。这会错误拒绝 LBA63 等合法布局；更根本
的是，它把 MBR sector、HAL 平台 I/O 块和 ext4/FAT 原生块混成了同一个单位。

修复后的层间不变量是**精确字节偏移**：MBR LBA 固定乘 512 得到 partition byte
offset；文件系统探测返回 native block size；`BlockSizeAdapter` 再把 FS block id
转换为相对字节位置。只有到父设备 I/O 的最后一步，才按平台 `BLOCK_SZ` 选择整块
直通或 bounce-buffer read-modify-write。

同一根因还曾因打开入口分叉而复发：启动自动挂载保存完整 `DetectedFs` 并使用
adapter，普通 `mount(2)` 却只保留 FS 类型，把原始分区设备直接交给 FAT/ext4。
最终所有块文件系统打开入口统一调用 `adapt_filesystem_device()`。

| 属性 | 结论 |
|------|------|
| 严重性 | High / P1；可能拒绝合法分区或把 block id 映射到错误字节 |
| 涉及提交 | `4705b28d`（字节视图/FS 原生块适配）、`2effeaaf`（普通 `mount(2)` 复用适配） |
| 块大小根因 | MBR LBA、平台块、文件系统块使用了同一换算常量 |
| 复发根因 | 自动挂载与 `mount(2)` 采用不同的 device-adapter 路径 |
| 正确不变量 | `byte_offset = on_disk_unit * unit_bytes` |
| 关键回归 | LBA63 FAT32 512B、LBA63 ext4 1KiB、2K 平台上的 ext4 4KiB |

## 2. 证据范围与术语

本文块大小问题的取证/功能基线为 `2031fd5909355994f768f845b2935e4509290a07`；之后
当前 HEAD 的前进未改变这里分析的分区/adapter 链。

- **平台块**：`BlockDevice::read_block/write_block` 在当前 HAL 中使用的 `BLOCK_SZ`；
  2K1000LA 为 2 KiB，QEMU 路径常见为 4 KiB；
- **MBR sector/LBA**：始终为 512 字节，本项目常量为 `LOGICAL_SECTOR_SIZE=512`；
- **文件系统原生块**：ext4 superblock 或 FAT BPB 声明的单位，可为 512 B、1 KiB、
  2 KiB、4 KiB；

若不先分清前三个概念，日志中的“block 1”没有可比较意义。

## 3. 第一条故障链：512B LBA 被错误换算为平台块

### 3.1 旧实现

`4705b28d` 之前的 `PartitionBlockDevice` 定义了：

```rust
const SECTOR_SIZE: u64 = 512;
const LBA_PER_BLOCK: u64 = BLOCK_SZ as u64 / SECTOR_SIZE;

start_block = start_lba / LBA_PER_BLOCK;
block_count = sectors / LBA_PER_BLOCK;
```

并只接受：

```text
start_lba % LBA_PER_BLOCK == 0
sectors   % LBA_PER_BLOCK == 0
```

这段代码在 `BLOCK_SZ=4096`、分区恰好 4 KiB 对齐时看起来正确，但它实际上做了
一次有损整数除法。

以合法的 `start_lba=63` 为例：

```text
真实字节偏移                   = 63 * 512 = 32256
旧 4KiB 平台 start_block       = 63 / 8 = 7
旧实现映射出的父设备字节偏移   = 7 * 4096 = 28672
丢失的偏移                     = 3584 bytes
```

旧代码通过“拒绝未对齐分区”避免了实际读错，但代价是把合法磁盘布局误判为不支持。
如果后续删除对齐检查而保留整数除法，就会直接把文件系统头读到错误位置。

### 3.2 三种单位不能相互推导

正确的数据链是：

```text
MBR start_lba
  -- 固定乘 512 --> partition start_byte
  -- 加文件系统相对字节偏移 --> parent absolute byte offset
  -- 最后才按父设备 BLOCK_SZ 拆整块/非整块 I/O
```

而不是：

```text
MBR LBA -- 除以平台比例 --> 平台 block id -- 假定 FS 也使用此单位
```

文件系统块也必须独立处理。例如同一个 2 KiB 平台设备上：

| 磁盘结构 | 原生单位 | 文件系统 block 1 的相对字节偏移 |
|----------|----------|----------------------------------|
| FAT32 512B BPB sector | 512 B | 512 |
| ext4 1KiB block | 1024 B | 1024 |
| ext4 4KiB block | 4096 B | 4096 |

“平台为 2 KiB”不能令 FAT sector 1 变成父设备 block 1，也不能令 ext4 4 KiB block
1 变成 2 KiB。

### 3.3 修复：以字节偏移作为层间不变量

新的 `PartitionBlockDevice` 保存：

```text
start_lba
sectors
start_byte = checked(start_lba * 512)
size_bytes = checked(sectors * 512)
```

每次 I/O 先计算：

```text
relative = block_id * 当前逻辑块大小
absolute = start_byte + relative
end      = absolute + len
```

并对乘法、加法和分区边界执行 checked 校验。自然对齐且长度为平台块整数倍时直接
I/O；否则使用一个平台块大小的 bounce buffer：

1. 读取覆盖目标字节范围的父设备整块；
2. 对读请求复制子区间；
3. 对写请求执行 read-modify-write，仅修改目标字节；
4. 跨父块时逐块推进。

这使 LBA63 这类分区无需特殊处理。它可能比自然对齐路径多一次读，但语义正确，
而且不会覆盖相邻分区字节。

### 3.4 文件系统原生块：`DetectedFs` 必须携带大小

只返回 `FS_Type::Ext4/Fat32` 不够。探测器需要同时返回：

```text
DetectedFs {
    fs_type,
    block_size,
    ...
}
```

打开文件系统前统一调用：

```text
PartitionBlockDevice
  -> BlockSizeAdapter(native_block_size)
  -> optional ReadOnlyBlockDevice
  -> Ext4FileSystem / EasyFileSystem
```

`BlockSizeAdapter` 把文件系统 block id 乘以其原生大小，再复用同一字节 I/O 辅助
函数映射到父平台块。这样 ext4/FAT 内部不需要知道 2K1000 的 `BLOCK_SZ`。

### 3.5 为什么普通 mount 入口也必须复用同一路径

启动自动挂载能成功，不代表用户态 `mount /dev/sdaX` 正确。曾存在两条打开路径：

```text
启动挂载: detect_fs_layout -> BlockSizeAdapter -> FS
mount(2):  detect_fs(type only) -> raw PartitionBlockDevice -> FS
```

第二条路径丢掉了探测出的 native block size。`2effeaaf` 让普通 `mount(2)` 也保存
完整 `DetectedFs`，并调用与启动挂载相同的 `adapt_filesystem_device()`。验证也因此
必须覆盖设备节点的 mount/I/O/umount，不能只看启动日志。

### 3.6 MBR 探测的其他安全边界

修复同时明确：

- boot flag 只接受 `0` 或 `0x80`；
- `start_lba + sectors` 使用 checked arithmetic，并校验不超设备；
- 扩展分区仍明确为 unsupported；
- protective GPT `0xee` 不能与其他条目混合后“部分按 MBR 挂载”；
- `0x55aa` 只证明 sector 末尾有签名，不证明它一定是可用 MBR，更不证明某分区是
  FAT。文件系统类型必须继续读取 superblock/BPB 验证。

## 4. 根因证明与排除

| 假设 | 观测 | 结论 |
|------|------|------|
| 分区表损坏 | 同一镜像可由外部工具/U-Boot 正确列出 | 排除 |
| LBA63 本身非法 | MBR 明确定义 512B LBA，不要求 4KiB 对齐 | 排除 |
| 只需改 `BLOCK_SZ` | 2K、4K 平台和 512B/1K/4K FS 组合同时存在 | 排除 |
| 整数除法可映射未对齐起点 | `63/8*4096 != 63*512` | 数学反证 |
| 字节视图 + bounce 正确 | LBA63 FAT32/ext4 回归可读写 | 根因闭环 |
| 只修启动挂载就足够 | 用户态 mount 曾绕过 adapter | 排除 |
| 所有入口统一适配 | 设备节点 mount/I/O/umount 通过 | 入口闭环 |

## 5. 验证证据

`4705b28d` 的字节视图/自动挂载验证覆盖了以下具有辨别力的布局：

| 平台/布局 | 起点 | 文件系统原生块 | 目的 |
|-----------|------|----------------|------|
| raw ext4 | 无 MBR | ext4 自报 | 保持裸文件系统兼容 |
| MBR ext4 | LBA2048 | 4KiB | 自然对齐快路径 |
| MBR ext4 | LBA63 | 1KiB | 未对齐 partition + 小 FS block |
| MBR FAT32 | LBA63 | 512B | 未对齐 + BPB sector 最小单位 |
| 2K1000 实盘 | 固定 P1/P2/P3 | ext4 4KiB / FAT 512B | 2KiB 平台适配 |

protective/hybrid MBR 保持明确 unsupported，避免“能挂一部分”被误当成功。

随后 `2effeaaf` 使用单 SSD 三分区镜像走普通 `mount(2)`，对 `/dev/vda2` 的 512B
FAT32 完成 mount/I/O/umount；该用例在修复前会让 512B FAT 请求直接到达 4KiB 平台
块设备并触发 I/O 粒度断言，因此它专门关闭了“自动挂载通过、syscall 入口仍错”的
证据缺口。

## 6. 容易再次引入的错误

### 6.1 在某个入口绕过 `DetectedFs`

新增 loop device、设备节点 mount 或自动恢复路径时，如果只调用 `detect_fs()` 得到
类型，而没保留 `block_size`，同一文件系统会因入口不同获得不同块语义。所有入口
必须收敛到 `adapt_filesystem_device()`。

### 6.2 用 `BLOCK_SZ/512` 修补 FAT 或 MBR

只要代码在解析 on-disk 字段后立即除以全局 `BLOCK_SZ/512`，就应警觉。正确做法是
把磁盘单位转换为字节，或交给声明了该单位的 adapter；不能在 FAT 内再做第二次换算。

### 6.3 bounce 写路径覆盖相邻字节

未对齐写必须 read-modify-write 父平台块。若直接用零 buffer 覆盖目标子区间以外的
字节，会破坏分区头前后的相邻内容。回归应在目标区间两侧放哨兵并验证不变。

### 6.4 把 `0x55aa` 当作 FAT 证据

MBR 和 FAT boot sector 都可能以 `0x55aa` 结尾。探测顺序必须继续解析结构字段，不能
“看到签名就按 FAT 打开”。

## 7. 可复用审计清单

- [ ] 每个 on-disk 数字都标明单位：sector、block、byte；
- [ ] 乘加使用 checked arithmetic；
- [ ] 分区边界按精确 bytes 校验；
- [ ] 未对齐 read/write 使用不会污染相邻字节的 bounce；
- [ ] FS 探测结果包含 native block size；
- [ ] 自动挂载与 `mount(2)` 走同一个 adapter；
- [ ] 覆盖 512B FAT、1KiB ext4、4KiB ext4 及未对齐起点；
- [ ] 启动自动挂载和用户态设备节点 mount 使用相同 adapter；
- [ ] bounce 写前后相邻字节保持不变。

## 8. 证据边界

1. 当前实现支持传统 MBR 四个主分区，不支持 GPT/扩展分区；这是明确限制，不是探测
   失败后静默回退。
2. bounce buffer 保证语义，不承诺未对齐布局与自然对齐同等性能。
3. `BlockSizeAdapter` 只解决单位映射；FAT/ext4 自身的 sector/metadata bug 仍需各自
   验证。
4. 只读 bind/传播是独立 VFS 根因，见 `05a-readonly-mount-propagation.md`。

## 9. 最终因果链

```text
MBR 的 512B LBA
  被提前除以平台 BLOCK_SZ/512
  -> 合法未对齐分区被拒绝，或潜在偏移截断

文件系统 native block size
  未由探测结果传递到所有打开入口
  -> 同一设备因挂载入口不同而解释不同

精确 start_byte/size_bytes
  + DetectedFs(native block size)
  + 统一 BlockSizeAdapter
  + 未对齐 bounce I/O
  -> MBR、平台块与 FS 块三层单位重新解耦
```

修复后，层间只传递无歧义的“精确字节偏移 + 原生块大小”，消除了整数截断和入口
分叉，使同一套代码能覆盖 2K1000 的 2KiB 平台块、QEMU 4KiB 平台块，以及 FAT
512B/ext4 1KiB/4KiB 的实际磁盘布局。
