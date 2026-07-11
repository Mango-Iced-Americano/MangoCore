---
title: "2K1000LA 单 SSD 完整测试镜像"
module: "fs/board-image"
category: fs
status: draft
owner: MangoCore Team
last_updated: 2026-07-11
code_paths:
  - "scripts/make_2k1000_full_test_disk.py"
  - "scripts/restore_2k1000_p2.py"
  - "os/src/fs/mod.rs"
  - "os/src/fs/fat32/bitmap.rs"
  - "os/src/fs/fat32/efs.rs"
  - "os/src/fs/fat32/fat_inode.rs"
  - "os/src/syscall/fs.rs"
  - "os/src/drivers/block/partition.rs"
entry_points:
  - "discover_mount_devices"
  - "mount_boot_block_devices_read_only"
  - "mount_boot_block_devices_with_writable_scratch"
arch:
  rv64: supported
  la64: supported
related_docs:
  - "docs/03_fs/init-and-rootfs.md"
  - "docs/03_fs/devfs.md"
  - "docs/01_architecture/boot-and-trap.md"
---

# 2K1000LA 单 SSD 完整测试镜像

## 1. 产物

本机产物位于 `/private/tftpboot/`：

| 文件 | 大小 | SHA-256 |
|------|------|---------|
| `mango-2k1000la-full-test-mbr.img` | 6,443,499,520 B（6145MiB） | `416f84060bca79ab06ef5596d8cfd1801b8ae3e56ae3d2e65e99a66b612ef19f` |
| `mango-2k1000la-full-test-mbr.img.xz` | 400MiB | `80e1e2addac136da2b9ccffbcad349d915b3b4fec20ef25e11a86193162bc584` |
| `mango-2k1000la-full-test-mbr.img.layout.json` | 702 B | 分区起点、长度和 payload 哈希 |
| `kernel-2k1000-sata-mount-ro.ui` | 约 12MiB | `cd02b6dbb1d9c90945ebed2bfa9ac3c4848beed99e96ae5b670a2c2fec2f49d2` |
| `kernel-2k1000-run.ui` | 12,319,472 B | `9fcb0df721f115af8b3d42358cf9560344d3fe1adabb5acc731ef5bf44c0f3f1` |
| `kernel-2k1000-scratch-rw.ui` | 12,343,864 B | `8d152e9ba61f996c7c76778560a1f2d717509c075364835f2815bafc9f57ec98` |

## 2. MBR 布局

磁盘使用传统 MBR（DOS disklabel），disk id 为 `0x4d414e47`，不包含 GPT 或扩展分区。

| 分区 | LBA 范围 | 大小 | 类型 | 用途 |
|------|----------|------|------|------|
| P1 | `2048..8390655` | 4GiB | `0x83` ext4 | 官方 LA64 完整测试集，自动挂载 `/sdcard` |
| P2 | `8390656..11012095` | 1280MiB | `0x0c` FAT32 | 保持 `/dev/vda2` 兼容；staged 镜像额外挂载为 `/scratch` |
| P3 | `11012096..12584959` | 768MiB | `0x83` ext4 | MangoCore 工具，单盘时自动挂载 `/tools` |

P1 来自干净的 `fs-img-dir/sdcard-la.img.xz`，注入当前 `os_test.conf`（`mode=run`、`mask=0xFFF`）。不要使用被 QEMU 写过的仓库根目录 `sdcard-la.img` 作为母盘，除非它重新通过只读 `e2fsck -f -n`。

## 3. 生成

生成器必须在包含 `mke2fs`、`e2fsck` 和 `mkfs.vfat` 的竞赛 Docker 环境中运行：

```bash
python3 scripts/make_2k1000_full_test_disk.py \
  --official-img /hosttmp/mango-full-official-la.ext4 \
  --tools-root user/tools/loongarch64 \
  --user-bin-dir user/target/loongarch64-unknown-linux-gnu/release \
  --output /tftpboot/mango-2k1000la-full-test-mbr.img \
  --force
```

脚本会构建 P3 工具 ext4、执行只读 `e2fsck`、写 MBR、复制 P1/P3 payload、格式化 P2 FAT32，并逐字节比较嵌入前后的 P1/P3，最后输出 `.layout.json`。

## 4. 写入 SSD

完整磁盘镜像不能一次通过 U-Boot `tftpboot` 加载：镜像约 6GiB，而开发板内存只有 2GiB。可以选择主机直写，或在确认 U-Boot 提供 `scsi write` 后分块通过网线写入。

在 Windows 上应先解压 `.img.xz`，再使用支持 raw/disk image 的写盘工具把整个 `.img` 从 LBA0 写到 32GB SSD。必须核对目标磁盘型号和容量；该操作会覆盖目标盘现有分区表及数据。写完后安全弹出 SSD并装回开发板。

不要把 `.img` 文件复制进 SSD 的某个现有分区，也不要只写 P1；内核和官方测试依赖 MBR、P2 `/dev/vda2` 与 P3 `/tools` 的固定布局。

实板已验证的网线写入方式是将 raw image 切成 24 个 256MiB 块和最后一个 1MiB 块：

```bash
split -d -b 256m -a 2 mango-2k1000la-full-test-mbr.img mango-full.part-
```

每个 256MiB 块包含 `0x80000` 个 512B SCSI sector，起始 LBA 每次增加 `0x80000`。每块必须依次验证 TFTP 字节数、内存 CRC、写入块数和 SSD 读回 CRC：

```text
tftpboot 0x9000000098000000 mango-full.part-00
crc32 0x9000000098000000 0x10000000
scsi write 0x9000000098000000 0x0 0x80000
scsi read 0x9000000098000000 0x0 0x80000
crc32 0x9000000098000000 0x10000000
```

最后一块起始 LBA 为 `0xc00000`，大小 `0x100000` 字节、块数 `0x800`。写入前必须用 `scsi info` 核对 SSD 型号和容量；任何传输、短写或 CRC 错误都应立即停止。

## 5. 上板验收

U-Boot 先做只读检查：

```text
scsi reset
ext4ls scsi 0:1 /
fatls scsi 0:2 /
ext4ls scsi 0:3 /
```

内核仍使用固定 TFTP 网段启动：

macOS 上推荐直接使用一键入口：

```bash
make 2k1000-boot-check
make 2k1000-boot
```

手工等价命令为：

```text
setenv ipaddr 192.168.9.20
setenv serverip 192.168.9.10
setenv netmask 255.255.255.0
tftpboot 0x9000000098000000 kernel-2k1000-run.ui
bootm 0x9000000098000000
```

`kernel-2k1000-run.ui` 是默认关闭上板诊断、`LOG=off` 且嵌入 `mode=run`/`mask=0xFFF` fallback 配置的正式镜像。旧的 `kernel-2k1000-sata-mount-ro.ui` 仅保留为前期验收基线。

预期日志应包含 `/dev/sda1` Ext4、`/dev/sda2` Fat32、`/dev/sda3` Ext4，随后 P1 以 `RDONLY` 挂到 `/sdcard`、P3 以 `RDONLY` 挂到 `/tools`。

2026-07-11 已在 `TS32GMTS400` 实体 SSD 上完成 25 块网络写入，全部 `12584960` 个 sector 均通过逐块读回 CRC；U-Boot 可读取三个分区，MangoCore 实板启动后成功完成上述三分区识别、只读挂载及 `/tools`、`/musl`、`/glibc` bind。原始 AHCI 写入/flush、内核 P2 FAT32 文件探针和 staged 用户态 `/scratch` 冒烟测试均已通过；正式 `kernel-2k1000-run.ui` 仍保持全盘只读。

## 6. 内核 AHCI 写入探针

正式解除只读前，使用独立 feature 构建自恢复探针：

```bash
make -C os la64-2k1000-sata-write-probe
make 2k1000-boot BOARD_KERNEL=kernel-2k1000-sata-write-probe.ui
```

探针硬匹配 SSD 型号 `TS32GMTS400`、MBR 签名和 disk id `0x4d414e47`，解析四个主分区后，在最后一个分区末端之外保留 2048 个 sector，再测试连续 8 个 sector。当前镜像对应测试范围为 `12587008..12587015`，不属于 P1/P2/P3。

测试顺序固定为：备份原 4KiB → 写入确定性模式 → `FLUSH CACHE EXT` → 读回逐扇区比较 → 写回备份 → 再次 flush → 读回确认恢复。第一次写命令发出后，无论中间步骤成功或失败都必须执行恢复；恢复不能完成或不能验证时内核立即 panic，不再继续文件系统路径。该 feature 保持 ramfs-only，不注册可写设备节点，也不改变正式 run 镜像的三层只读保护。

原始扇区探针通过后，使用第二个独立镜像验证 P2 FAT32：

```bash
make -C os la64-2k1000-sata-fs-write-probe
make 2k1000-boot BOARD_KERNEL=kernel-2k1000-sata-fs-write-probe.ui
```

该镜像仍按正式路径只读挂载 P1/P3，并保持 `/dev/vda2` 只读；内核仅为探针构造一个不暴露给用户态的 P2 可写视图，创建 `MANGO_RW_PROBE/PAYLOAD.BIN`，写入 6KiB 后强制 page cache 写回，重新打开 FAT32 验证目录、文件和内容，再删除并第三次打开确认清理持久化。只有该阶段通过后，才允许把 P2 作为用户态 scratch 分区开放。

实板最终通过镜像 SHA-256 为 `8f3a6abef28b4a15fd6930da259ba0c9c1d112f393a26bd7c82c1ce4f4ee6fdb`，串口结果为：

```text
[sata-fs-write-probe] PASS: create/write/flush/reopen/read/unlink/rmdir persisted
```

调试中确认了两个独立 FAT32 持久化问题：FAT 层在已经经过 `BlockSizeAdapter` 的设备上再次按平台块重算扇区，导致 FAT 表定位错误；文件大小、首簇和删除目录项只依赖 inode `Drop` 写回，而 `find()` 创建的独立 inode/page cache 可能在新实例读取磁盘前尚未落盘。修复后 FAT 访问统一使用 BPB 扇区单位并正确处理双 FAT/ExtFlags，文件大小与首簇在 write/resize 时显式同步，unlink/rmdir 在返回成功前写回所属目录页，`sync()` 同步数据页和父目录项。

探针失败并留下 `MANGO_RW_PROBE` 时，不要手工猜测 FAT 元数据。可从母镜像提取的干净 P2 分块，通过受限脚本仅恢复 P2：

```bash
python3 scripts/restore_2k1000_p2.py \
  --interface en8 \
  --no-host-config \
  --confirm-p2-start 0x800800
```

脚本硬限制写入范围为 `0x800800..0xa80800`，并校验 `TS32GMTS400`、MBR CRC、每个 256MiB 分块的 TFTP/写入/读回 CRC，以及最终 FAT32 卷标和空根目录。不得把确认起点改成其他值来复用为通用写盘工具。

暖复位时若看到 `PxSSTS=1`，表示设备已检测但 PHY 通信尚未建立。驱动会先按稳定计数器等待 200ms；仍未上线时通过 `PxSCTL.DET` 发出至少 1ms 的 COMRESET，释放后按 AHCI 上限继续等待 10s。不得用固定次数空转替代这段真实时间，也不应要求操作者通过冷断电规避。

## 7. Staged 用户态 `/scratch`

内核文件探针通过后，可构建只开放 P2 的 staged 镜像：

```bash
make -C os la64-2k1000-scratch-rw
python3 scripts/boot_2k1000_tftp.py \
  --interface en8 \
  --image kernel-2k1000-scratch-rw.ui
```

该目标强制 `os_test.conf` 为 `mode=run`，并要求 P2 同时满足 partno 2、MBR type `0x0c` 和 FAT32 三个条件才挂载到 `/scratch`。P1 `/sdcard`、P3 `/tools` 以及 `/dev/sda*`、`/dev/vda*` 块设备节点仍为只读，避免用户态绕过挂载策略直接覆盖磁盘。

stage-1 会创建 `/scratch/MANGO_USR_PROBE/PAYLOAD.BIN`，写入 6144 字节确定性数据，执行 fsync、截断到 2048 字节、关闭重开、内容和 EOF 比对，最后 unlink/rmdir。任一步失败都会停止进入测例；成功标志为：

```text
[scratch-smoke] PASS: write/fsync/truncate/reopen/read/unlink/rmdir
```

首轮用户态测试曾出现 `unlink=0`、随后 `rmdir=-ENOENT`。U-Boot 复位后 `fatls scsi 0:2 /` 显示空根目录，证明创建目录项只存在于某个 FAT 根 PageCache。根因是 `EasyFileSystem::root_inode()` 产生独立 inode/PageCache，而 create 没有在返回前提交父目录页。修复后 create 显式写回父目录和新目录内容，stale inode `Drop` 不再隐式覆盖父目录项；实板已通过上述完整标志并继续进入测例。
