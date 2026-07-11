---
title: "2K1000LA 单 SSD 完整测试镜像"
module: "fs/board-image"
category: fs
status: draft
owner: MangoCore Team
last_updated: 2026-07-11
code_paths:
  - "scripts/make_2k1000_full_test_disk.py"
  - "os/src/fs/mod.rs"
  - "os/src/syscall/fs.rs"
  - "os/src/drivers/block/partition.rs"
entry_points:
  - "discover_mount_devices"
  - "mount_boot_block_devices_read_only"
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

## 2. MBR 布局

磁盘使用传统 MBR（DOS disklabel），disk id 为 `0x4d414e47`，不包含 GPT 或扩展分区。

| 分区 | LBA 范围 | 大小 | 类型 | 用途 |
|------|----------|------|------|------|
| P1 | `2048..8390655` | 4GiB | `0x83` ext4 | 官方 LA64 完整测试集，自动挂载 `/sdcard` |
| P2 | `8390656..11012095` | 1280MiB | `0x0c` FAT32 | 保持 `/dev/vda2` 兼容，供 basic/mount 测试临时挂载 |
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

完整磁盘镜像不能通过 U-Boot `tftpboot` 加载：镜像约 6GiB，而开发板内存只有 2GiB。TFTP 仅用于约 12MiB 的内核 uImage。

在 Windows 上应先解压 `.img.xz`，再使用支持 raw/disk image 的写盘工具把整个 `.img` 从 LBA0 写到 32GB SSD。必须核对目标磁盘型号和容量；该操作会覆盖目标盘现有分区表及数据。写完后安全弹出 SSD并装回开发板。

不要把 `.img` 文件复制进 SSD 的某个现有分区，也不要只写 P1；内核和官方测试依赖 MBR、P2 `/dev/vda2` 与 P3 `/tools` 的固定布局。

## 5. 上板验收

U-Boot 先做只读检查：

```text
scsi reset
ext4ls scsi 0:1 /
fatls scsi 0:2 /
ext4ls scsi 0:3 /
```

内核仍使用固定 TFTP 网段启动：

```text
setenv ipaddr 192.168.9.20
setenv serverip 192.168.9.10
setenv netmask 255.255.255.0
tftpboot 0x9000000098000000 kernel-2k1000-sata-mount-ro.ui
bootm 0x9000000098000000
```

预期日志应包含 `/dev/sda1` Ext4、`/dev/sda2` Fat32、`/dev/sda3` Ext4，随后 P1 以 `RDONLY` 挂到 `/sdcard`、P3 以 `RDONLY` 挂到 `/tools`。当前阶段只验收完整文件树和读取路径；物理 SSD 尚未写入该镜像，AHCI 持久写入与 flush 未验收前不能解除只读保护。
