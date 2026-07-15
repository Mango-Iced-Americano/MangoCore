---
title: "P4 持久分区的 payload-first、MBR-last 发布协议"
category: debug
status: resolved-with-known-limits
author: MangoCore Team
last_update: 2026-07-15
tags: [postmortem, persistence, p4, mbr, ext4, rollback, commit-protocol, 2k1000la]
code_paths:
  - "scripts/make_2k1000_p4_ext4.py"
  - "scripts/make_2k1000_p4_qemu_disk.py"
  - "scripts/write_2k1000_p4.py"
  - "os/src/fs/filesystem.rs"
  - "os/src/fs/mod.rs"
  - "user/src/bin/initproc.rs"
related_docs:
  - "docs/09_debug/la64_on_board/development-log.md"
  - "docs/03_fs/2k1000-full-test-disk.md"
  - "docs/08_testing/apk-isolated.md"
entry_points:
  - "validate_manifest"
  - "prepare_mbrs"
  - "write_and_verify"
  - "validate_p4_persist"
---

# P4 持久分区的 payload-first、MBR-last 发布协议

## 1. 摘要

P4 的目标是在不改写现有 P1/P2/P3 的前提下，为 2K1000LA 增加一个 4 GiB 可写
ext4 持久区。真正的风险不在“如何把第四个 MBR 条目写进去”，而在中断时序：若先
发布分区表、再通过网络传输 4 GiB payload，任何断网、串口中断或复位都会留下一个
**对内核可见、内容却只写了一部分**的 ext4 分区。

最终协议把状态拆为两个独立维度：

```text
payload_complete: 4GiB 每个块均已写入并读回验证
partition_visible: MBR 第四项已经发布且重新扫描可见
```

固定顺序是：先写完整 payload，逐块读回 CRC；最后只提交 512 字节 LBA0。MBR
提交一旦尝试，外层 `BaseException` handler 会在后置验证异常时**尝试**恢复旧 MBR；
设计目标是恢复后重新扫描并证明 P4 不可见，但当前没有 fault injection 或真实回滚
成功记录，不能把“进入回滚分支”写成“所有中断均已完成回滚”。提交成功后还要由
U-Boot 从 P4 加载哨兵文件，证明分区表与目标 offset 上的 ext4 内容一致。

内核再执行第二重身份校验：P4 编号、类型、起止 LBA、ext4 UUID、卷标、journal 与
recovery 位必须全部匹配，才授予 `/persist` 读写权限。应用层另用 `committed-v1`
标记区分“文件系统可挂载”和“APK 安装事务完整”。

| 属性 | 结论 |
|------|------|
| 严重性 | Critical / P0，错误协议可让半成品分区被自动读写挂载 |
| 建立提交 | `32b93c89`，2026-07-14 |
| 实板闭环 | `08967aa1`，2026-07-14 |
| 固定范围 | `0xC00800..0x1400800`，末端不含 |
| 大小 | `0x800000` 个 512B sector，即 4 GiB |
| 发布点 | 仅 LBA0 的 512B MBR sector |
| 回滚设计 | 后发布异常时尝试恢复旧 MBR；当前无故障注入/真实成功样本 |

## 2. P4 身份与布局

### 2.1 四分区固定布局

| 分区 | start LBA | sectors | 大小 | 类型/用途 |
|------|-----------|---------|------|-----------|
| P1 | `0x800` | `0x800000` | 4 GiB | `0x83` ext4，只读系统/测试 |
| P2 | `0x800800` | `0x280000` | 1280 MiB | `0x0c` FAT32，scratch/cache |
| P3 | `0xA80800` | `0x180000` | 768 MiB | `0x83` ext4，只读 tools |
| P4 | `0xC00800` | `0x800000` | 4 GiB | `0x83` ext4，持久状态 |

P4 结束于 `0x1400800`；32 GB SSD 的后续空间继续保持未分配。脚本不允许操作者传入
任意 start LBA，而是要求显式重复确认这三个固定事实：

```text
CONFIRM_P4_START=0xC00800
CONFIRM_P4_END=0x1400800
CONFIRM_DISK_SECTORS=62533296
```

确认值不是配置项；它们必须与代码常量完全相等，否则在接触硬件前失败。

### 2.2 文件系统身份

P4 payload 固定为：

```text
filesystem       ext4
block size       4096
label            MANGO_STATE
UUID             4d414e47-5354-4154-4500-000000000004
has_journal      false
needs_recovery   false
marker           /MANGO_STATE.txt
marker bytes     97
marker CRC32     c8f1b4ff
```

生成器运行只读 `e2fsck -f -n`，随后直接解析 superblock magic、block size、feature、
UUID 和 label。manifest 记录 role、大小、SHA-256、目标 LBA、chunk size 和哨兵属性。

“第四分区是 ext4”不是身份，甚至“卷标相同”也不是充分条件。所有字段联合匹配，
才能把误盘、旧 payload、错误范围与 journal 不兼容区分开。

## 3. 风险模型

### 3.1 先写 MBR 的危险状态

错误顺序：

```text
publish P4 entry in MBR
  -> OS now sees /dev/sda4
  -> transfer/write chunk 0..15
  -> network interruption after chunk 7
  -> reboot
  -> mount probes a half-old, half-new ext4 image
```

即使 superblock 位于前面已写块，后半部 block group、inode table 或文件数据仍可能
不存在。仅靠 ext4 magic 会把半成品误认为合法。

### 3.2 payload-first 的安全状态

正确顺序在提交前允许磁盘尾部包含任意半成品，但旧 MBR 没有 P4 条目：

```text
P4 entry empty
  + partial payload in unallocated tail
  = normal boot path cannot discover or mount it
```

这不是说半成品数据已回滚，而是**可见性仍未发布**。下一次运行可以在相同固定范围
重新覆盖全部 16 块；P1-P3 与正常启动路径不受影响。

## 4. 状态机

```text
S0 OLD_VISIBLE
  old MBR，P1-P3 可见，P4 不可见
      |
      | write + readback each payload chunk
      v
S1 PAYLOAD_COMPLETE_NOT_VISIBLE
  4GiB payload 已验证，旧 MBR 仍在
      |
      | write/readback one 512B LBA0
      v
S2 MBR_ATTEMPTED
  必须验证重扫、分区表、ext4 marker
      |                         |
      | all checks pass         | outer BaseException
      v                         v
S3 P4_PUBLISHED              ROLLBACK_ATTEMPT
  P4 可见且 marker 可读         | success -> verify P4 absent
                               | failure/interruption -> UNKNOWN/CRITICAL
```

关键实现变量是 `mbr_attempted`。它只在新 MBR 即将写入前设为 true，所有后置验证
通过后才清零。

## 5. 写入前 fail-closed 门禁

### 5.1 宿主 artifact

脚本在连接串口之前验证：

- payload 文件存在且精确为 4,294,967,296 字节；
- JSON manifest schema/role/FS/label/UUID/边界/chunk/marker 字段逐项等值；
- payload SHA-256 与 manifest 一致；
- MBR source 为 512B 且有 `55 aa`；
- disk id 为 `0x4d414e47`；
- P1-P3 的 boot flag、type、start 和 sectors 完全匹配；
- P4 原条目 16 字节全部为零。

### 5.2 实际设备

连接 U-Boot 后验证：

- `scsi reset/info` 报告型号 `TS32GMTS400`；
- 容量精确为 `62,533,296 x 512`；
- P4 end 小于设备容量；
- `scsi part 0` 只有固定 P1-P3，P4 不存在；
- 请求读取实际 LBA0 后，DRAM 中 512B 的 CRC 与 old MBR source 相同。

脚本意图在宿主 artifact 身份和实际盘身份同时闭合后才进入 payload 写入，以避免把
“正确 payload”写到错误 SSD，或把“正确 SSD”配上错误 MBR 模板；下述 LBA0 read
状态缺口意味着这条门禁还不能称为形式上完全闭合。

这里有一个尚未关闭的 preflight 缺口：代码发送
`scsi read <loadaddr> 0x0 0x1` 后直接计算 DRAM CRC，没有像 `write_and_verify()` 那样
解析并要求 `1 blocks read: OK`。若该读命令失败而 load address 恰好残留相同旧 MBR
内容，CRC 理论上可以假通过。成功实板流程证明当次读取可用，但不能替代命令状态
解析；脚本应在计算 CRC 前显式校验单块 read 完成计数。

## 6. payload 写入与逐块证明

4 GiB payload 固定分成 16 个 256 MiB 块；每块是 `0x80000` sectors。对第 `i` 块：

```text
start_lba = 0xC00800 + i * 0x80000
```

每块执行：

1. 宿主从 payload 精确截取 256 MiB，同时计算 CRC32；
2. TFTP 到固定已验证 DRAM 地址；
3. 解析 `Bytes transferred`，必须等于 256 MiB；
4. U-Boot 对内存计算 CRC，必须等于宿主 CRC；
5. `scsi write` 固定 LBA/sector 数，解析完整写入计数；
6. 从同一 LBA `scsi read` 回 DRAM；
7. 再计算 CRC，必须等于宿主 CRC；
8. 删除临时 TFTP 文件，推进下一块。

任何 LBA 若落在 P4 范围之外，`write_and_verify()` 在发命令前拒绝。传输成功、写命令
返回 OK 和读回 CRC 是三个不同证据，缺一不可。

大镜像如何在 DRAM 小于镜像时进行通用分块刷盘，另见
`07a-large-disk-network-flashing.md`；本文只讨论 P4 的发布原子性。

## 7. MBR 最后提交与回滚

### 7.1 最小发布动作

16 块全部验证后，脚本才生成并传输新 MBR。新旧 MBR 的差异只应是第四个 16B
分区项；发布动作是：

```text
write LBA 0, sectors 1
read  LBA 0, sectors 1
verify new MBR CRC
```

实板新 MBR CRC32 为 `6538e5cb`，旧 MBR CRC32 为 `f469e65a`。

### 7.2 `BaseException` 扩大了回滚触发面，但不保证回滚完成

Python 的 `KeyboardInterrupt` 不属于普通 `Exception`。若操作者在 MBR 已写、marker
尚未验证时按 Ctrl-C，只捕获 `Exception` 会完全跳过恢复尝试。脚本在 MBR 提交段的
外层捕获 `BaseException`，因此以下情况会触发同一**回滚尝试**：

- TFTP/串口/解析异常；
- 新 MBR 写入或读回失败；
- `scsi reset` 后设备缺失；
- P4 分区表不匹配；
- ext4 listing/marker 失败；
- 用户 `KeyboardInterrupt`。

回滚流程：

```text
transfer old 512B MBR + CRC
write/readback LBA0 + old CRC
scsi reset
scsi part 0
assert P4 absent and P1-P3 exact
```

回滚内部却只捕获 `Exception`。这有两个后果：

1. 普通 `BootError`/I/O 异常会打印 CRITICAL；
2. 回滚过程中再次发生 `KeyboardInterrupt` 等 `BaseException` 时，会直接逃出内层，
   不一定打印 CRITICAL，也不会完成“P4 absent”复核。

所以准确表述是“外层覆盖 KeyboardInterrupt 并尝试回滚”，不是“任意中断都能回滚”。

### 7.3 回滚的能力边界

旧 MBR 回滚只恢复**分区可见性**，不会擦除已写入 SSD 尾部的 payload，也不是 ext4
事务回滚。这正是状态拆分的意义：即使尾部数据存在，只要 P4 未发布，正式内核不会
自动获得对它的读写入口。

此外，归档结果只有正常成功路径：16 块写入、MBR 发布、marker 与双启动均成功。
仓库没有在 MBR 发布后主动注入 TFTP 失败、串口断开或 Ctrl-C，也没有一份“旧 MBR
恢复并验证 P4 absent”的真实日志。因此回滚代码目前是经过静态审计的设计保护，尚未
形成故障注入闭环。

## 8. 发布后验证

新 MBR 读回正确仍不够。脚本继续：

1. `scsi reset` 强制重新枚举；
2. `scsi part 0` 验证 P1-P4 精确布局；
3. `ext4ls scsi 0:4 /` 验证 `MANGO_STATE.txt` 与预置目录；
4. `ext4load` 从 P4 读取 marker；
5. 比较 97 字节长度和 CRC32 `c8f1b4ff`。

这关闭了“MBR 指向正确范围，但 payload 实际写偏/仍是旧数据”的假设。

## 9. 内核身份门禁

`validate_p4_persist()` 在挂载前联合检查：

```text
partno           == 4
partition type   == 0x83
start_lba        == 0xC00800
sectors          == 0x800000
filesystem       == Ext4
UUID             == fixed P4 UUID
volume label     == MANGO_STATE
HAS_JOURNAL      == 0
RECOVER          == 0
```

任何一项失败即 panic/fail closed，不降级为“尝试读写未知第四分区”。只有 P4 包装为
读写 `/persist`；P1/P3 和用户态块设备节点保持只读，P2 仍只承担 scratch/cache。

### 9.1 为什么无 journal 且拒绝 RECOVER

当前 MangoCore ext4 不支持 journal replay。若允许有 journal 或 `needs_recovery` 的
文件系统读写挂载，内核可能在未重放事务的旧元数据上继续修改，扩大损坏。

因此 P4 构建时使用 `^has_journal`，内核又在运行时双检 feature 位。代价是没有
journal 提供的掉电恢复；这是已知能力边界，不应包装成“更安全”。

## 10. 应用层提交标记

文件系统 identity 只证明“这是预期的 P4”，不证明 APK 安装已完成。应用初始化采用
第二层提交协议：

```text
prepare apk database and install tree
sync payload/state
write temporary commit marker
sync marker
rename temporary marker -> committed-v1
sync parent/state
```

- 无 `committed-v1`：上次初始化可能中断，删除残留并重建；
- 有 `committed-v1`：验证关键文件后复用，不重复安装。

这个 marker 不能修复 ext4 断电一致性，但能防止“目录里已有一些文件”被误判为完整
业务状态。它与 MBR 发布是两个层级：

| 层 | commit 对象 | 失败后的安全状态 |
|----|-------------|------------------|
| 磁盘布局 | MBR P4 entry | P4 不可见 |
| 文件系统身份 | 固定 UUID/label/features | 拒绝读写挂载 |
| 应用初始化 | `committed-v1` | 清理并重建安装树 |

## 11. 验证结果

### 11.1 QEMU

- 4 GiB sparse payload 经 `e2fsck -f -n`；
- 四分区边界与实盘一致；
- `/sdcard ro`、`/scratch rw`、`/tools ro`、`/persist rw`；
- 同一个非 snapshot 磁盘连续启动：首轮 `PASS mode=install`，次轮
  `PASS mode=reuse`；
- 第二轮没有重新执行 update/fetch/add。

### 11.2 实板

- 16 个 `0x80000`-sector payload 块全部 TFTP、写入、读回 CRC 一致；
- 覆盖范围精确为 `0xC00800..0x1400800`；
- 只在最后写一次 LBA0；
- 新 MBR CRC32 `6538e5cb`；
- P1-P3 边界不变；
- P4 marker 97 字节、CRC32 `c8f1b4ff`；
- 同一专用 uImage 第一次 `PASS mode=install`，物理复位后第二次
  `PASS mode=reuse`；
- 两轮均 `RESULT=PASS`，第二轮直接使用 P4 安装树。

这组结果验证的是正常发布路径，没有执行 rollback 分支。

## 12. 尚存边界

1. P4 不是 overlay root；宿主 `/` 仍是 RAMFS，应用根在 `/persist/apk-root`。
2. 无 journal 意味着不能声称任意掉电点都可恢复。dirty/RECOVER 文件系统会被拒绝，
   需要离线检查或重建。
3. MBR 单 sector 发布在设备层仍不是断电原子性的形式化保证；协议提供读回与旧 MBR
   回滚，但突然断电可能令 LBA0 处于设备特定状态。
4. 应用 marker 是完整性门禁，不是通用事务日志；marker 发布后新增包的掉电语义仍需
   单独设计。
5. 回滚不擦除尾部 payload；安全性来自 P4 不可见和内核身份 fail-closed。
6. P4 固定针对该 SSD 布局，不能把脚本改成接受任意起止 LBA 的通用写盘器。
7. LBA0 preflight 尚未解析 `1 blocks read: OK`，存在 DRAM 残留导致 CRC 假通过的
   理论窗口。
8. rollback 未做 fault injection；内层仅捕获 `Exception`，二次 Ctrl-C 可中断恢复。

## 13. 可复用结论

任何“先传大 payload、再发布小索引”的操作都应复制这个模式：

```text
verify artifact identity
verify target identity and exact bounds
write payload to currently unreachable range
read back every payload chunk
publish smallest possible pointer/index last
read back pointer
reopen through the published namespace
verify a content sentinel
attempt pointer rollback on post-publish exceptions
verify rollback itself or report state as unknown
```

关键不是使用 MBR 还是 GPT，而是把“内容完整”和“名字/索引可见”拆成两个状态，并
保证可见性永远最后发布。

## 14. 最终因果链

```text
4GiB payload 大、网络链路可中断
  + MBR entry 一旦存在，内核即可发现 P4
  -> 先 MBR 后 payload 会暴露半成品文件系统

payload-first
  + 16 块逐块读回 CRC
  -> 内容完整但仍不可见

MBR-last
  + 外层 BaseException/KeyboardInterrupt 触发回滚尝试
  + 正常路径重扫分区与 marker 内容验证
  -> 正常发布路径已闭环
  -> 回滚路径仍需 read-status 修正与 fault injection

内核固定身份/feature 门禁
  + 应用 committed marker
  -> 物理分区、文件系统和业务状态分别 fail closed
```

P4 正常发布路径的可靠性不是来自某一次“写盘成功”，而是来自分层身份门禁和最后
发布；正常路径已由独立读取验证。异常回滚路径仍需补 read-status 校验与故障注入，
在此之前不能赋予同等级结论。
