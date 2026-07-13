---
title: "2K1000LA 单 SSD 完整测试镜像"
module: "fs/board-image"
category: fs
status: draft
owner: MangoCore Team
last_updated: 2026-07-12
code_paths:
  - "scripts/make_2k1000_full_test_disk.py"
  - "scripts/restore_2k1000_p2.py"
  - "os/src/fs/mod.rs"
  - "os/src/fs/fat32/bitmap.rs"
  - "os/src/fs/fat32/efs.rs"
  - "os/src/fs/fat32/fat_inode.rs"
  - "os/src/fs/ramfs/mod.rs"
  - "os/src/fs/dev/pipe.rs"
  - "os/src/task/manager.rs"
  - "os/src/task/threads.rs"
  - "os/src/syscall/fs.rs"
  - "os/src/drivers/block/partition.rs"
  - "user/src/bin/init.rs"
  - "user/src/bin/initproc.rs"
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
| `kernel-2k1000-scratch-rw.ui` | 12,364,416 B | `e379aea367d27e51354cfd8cee620b76357f7278baa9e8e3b160240e189104aa` |

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
make 2k1000-boot-check IMAGE=kernel-2k1000-run.ui
make 2k1000-boot IMAGE=kernel-2k1000-run.ui
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
make 2k1000-boot IMAGE=kernel-2k1000-sata-write-probe.ui
```

探针硬匹配 SSD 型号 `TS32GMTS400`、MBR 签名和 disk id `0x4d414e47`，解析四个主分区后，在最后一个分区末端之外保留 2048 个 sector，再测试连续 8 个 sector。当前镜像对应测试范围为 `12587008..12587015`，不属于 P1/P2/P3。

测试顺序固定为：备份原 4KiB → 写入确定性模式 → `FLUSH CACHE EXT` → 读回逐扇区比较 → 写回备份 → 再次 flush → 读回确认恢复。第一次写命令发出后，无论中间步骤成功或失败都必须执行恢复；恢复不能完成或不能验证时内核立即 panic，不再继续文件系统路径。该 feature 保持 ramfs-only，不注册可写设备节点，也不改变正式 run 镜像的三层只读保护。

原始扇区探针通过后，使用第二个独立镜像验证 P2 FAT32：

```bash
make -C os la64-2k1000-sata-fs-write-probe
make 2k1000-boot IMAGE=kernel-2k1000-sata-fs-write-probe.ui
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

HBA reset 后还必须恢复平台可写的 CAP/PI。2K1000 按随板 U-Boot 保存 CAP bit28/bit17、强制 bit27 `SSS`，再写 `PI=0x0f`。只恢复 PI 时，暖复位实测 `PxCMD.SUD` 无法保持置位，最终报 `LinkTimeout { sata_status: 1, command: 4 }`；补齐 CAP 后同一复位路径可独立初始化 SSD。

## 7. Staged 用户态 `/scratch`

需要在不运行测例的情况下交互检查三个 SSD 分区时，使用独立 SATA Shell 镜像：

```bash
make -C os la64-2k1000-sata-shell
make 2k1000-boot IMAGE=kernel-2k1000-sata-shell.ui
```

该镜像将 P1 只读挂载到 `/sdcard`、P2 FAT32 可写挂载到 `/scratch`、P3 只读
挂载到 `/tools`，随后通过易失 `/board_shell` 标记进入 Bash。它不启用 GMAC，
用于把 AHCI/分区/文件系统问题与网卡 DMA 问题隔离开；磁盘上的
`/sdcard/os_test.conf` 不会被改写。

SATA Shell 验收后，可用完整 Shell 集成镜像同时启用纯净 GMAC0 驱动：

```bash
make -C os la64-2k1000-full-shell
make 2k1000-boot IMAGE=kernel-2k1000-full-shell.ui
```

该目标保持相同分区写保护，并增加 `gmac_2k1000`，用于在交互式 Shell 中同时
验证 `/sdcard`、`/scratch`、`/tools` 和 `192.168.9.20/24`。逐包
`gmac_diag` 仍保持关闭。

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

### 7.1 可写运行时与分组工作区

检测到可写 `/scratch` 后，stage-1 不再把只读 `/tools/bin`、`/tools/lib`、`/tools/usr` 覆盖到根目录对应路径，而是保留 initramfs 中的 `/bin`、`/sbin`、`/lib`、`/usr` 作为可写运行时。工具仍可通过扩展后的 `PATH` 和 `LD_LIBRARY_PATH` 从 `/tools` 读取；动态库链接和内嵌 `libgcc_s.so.1` 则写入 ramfs `/lib`。这部分运行时重启后丢失，不属于 SSD 持久数据。

P1 上的 `/musl`、`/glibc` 继续只读。执行需要当前目录写入的分组时，initproc 会按 libc 删除并重建独立工作区：

```text
/scratch/work/basic-musl
/scratch/work/basic-glibc
/scratch/work/busybox-musl
/scratch/work/busybox-glibc
/scratch/work/lua-musl
/scratch/work/lua-glibc
/scratch/work/lmbench-musl
/scratch/work/lmbench-glibc
/scratch/work/iozone-musl
/scratch/work/iozone-glibc
/scratch/work/libcbench-musl
/scratch/work/libcbench-glibc
/scratch/work/libctest-musl
/scratch/work/libctest-glibc
/scratch/work/cyclictest-musl
/scratch/work/cyclictest-glibc
```

各组只复制最小依赖：basic 包含 `basic/`、入口和 busybox；busybox 包含二进制、入口和命令清单；lua 包含 busybox、解释器、runner、入口及 9 个 Lua 脚本；lmbench 包含 busybox、入口、统一二进制、`hello` 和 `lat_sig`；iozone 包含 busybox、入口脚本和 iozone 二进制；libcbench 包含 busybox、入口脚本和静态 `libc-bench`；libctest 包含两个入口脚本、`runtest.exe`、静态/动态 entry、顶层 `dlopen_dso.so`/`tls_get_new-dtv_dso.so` 以及完整 DSO `lib/`；cyclictest 包含入口、cyclictest 和 hackbench。LoongArch musl cyclictest 会在 libc 的 scheduler stub 中提前失败，因此两套 wrapper 都使用已在 QEMU 验证的 glibc cyclictest，但 hackbench 仍保留当前 libc 版本。lmbench 的 `hello` wrapper 会通过绝对路径 `/code/lmbench_src/bin/build/lmbench_all` 回调，因此每次准备工作区后都要把该链接切到当前 libc 的 `lmbench_all`。递归复制只忽略 FAT32 不支持 chmod/权限元数据产生的诊断，但保留复制退出码；随后逐项确认关键文件存在。准备失败时明确拒绝回退到只读源，避免空脚本或缺文件仍以退出码 0 伪装成通过。

2026-07-12 实板复验中，启动脚本只执行 `ping`、`tftpboot`、`iminfo` 和 `bootm`，未执行 U-Boot `scsi reset/scan`；内核独立完成 AHCI 初始化并通过 `/scratch` 写入探针。musl/glibc 的 basic、busybox、lua 均从上述 SSD 路径运行：basic 全部子项到 END；busybox 的 touch/write/cp/mkdir/mv/rmdir/unlink 等命令全部 success；Lua 两套共 18 个子项全部 success。

busybox 首轮暴露 FAT 未实现原生 rename，默认 `link + unlink` 因 FAT 不支持硬链接而失败。当前实现对同一目录、目标不存在的 rename 创建保留原簇号/大小/属性/时间的新目录项，再删除旧项并同步，失败时回滚新项；跨目录和覆盖目标仍显式不支持。

lmbench 迁移前，`lat_select` 在只读源目录创建临时文件会报 `EROFS`。首版工作区只复制入口、统一二进制、busybox 和 `hello`，两套组虽然退出 0，`lat_sig -P 1 prot lat_sig` 仍因缺少作为映射对象的 `lat_sig` 文件打印 `mmap: Bad file descriptor`。补齐并校验该文件后，2026-07-12 最终实板复验中 musl/glibc 均输出 `Protection fault`，并完整运行 `lat_select`、fork/exec/shell、`/var/tmp/XXX` 写入、pagefault、mmap、`lat_fs`、文件/管道带宽和 context switch；两组分别用时 108s 和 216s，退出码均为 0。

iozone 原先直接在只读源目录创建 `iozone.tmp` 和 `iozone.DUMMY.*`，会稳定返回 `Read-only file system`。迁移后每套 libc 都在独立 scratch 工作区中执行自动模式，以及 4 进程顺序、随机、反向、跨步、stdio、pread/pwrite 测试。真实 2K1000 SATA/FAT32 上 1KiB record 工作负载明显慢于 QEMU，因此组超时从 480s 调整为 1800s；最终实板 musl 用时 1331s、glibc 用时 1229s，均到 GROUP END 且退出码为 0。两份 iozone 二进制都提示当前版本不提供 `pwritev/preadv` 选择项，随后按其既有行为正常结束，不属于内核失败。

首轮 glibc iozone 会立即在动态加载器 `_dl_runtime_resolve_lasx` 的 `xvst` 指令触发 `InstructionNonDefined`。根因不是文件系统，而是内核对两种架构统一写死 `AT_HWCAP=0x112d`：该值是 RISC-V IMAFDC 字母位图，在 LoongArch ABI 中却包含 LASX 和 LBT_MIPS。修复后 RISC-V 保留原值，LoongArch 根据 CPUCFG1/2 与内核上下文保存能力生成 HWCAP；当前内核未保存 LSX/LASX/LBT 扩展状态，因此不向用户态宣称或启用这些扩展。修复后的 glibc 完整 iozone 已通过。

libcbench 两套入口都只调用静态 `libc-bench`，二进制唯一外部路径是 `/proc/self/smaps`，没有隐藏的数据文件或当前目录写入依赖。迁移到独立工作区后，musl/glibc 均完整输出 27 个 malloc、string、pthread、UTF-8、stdio 和 regex benchmark；musl 用时 37s、glibc 用时 61s，均到 GROUP END 且退出码为 0。此前为 smaps 实现的 per-open 快照缓存也在实板上通过 pthread create 压力验证，没有复现 120s 超时。

### 7.2 核心测试聚焦镜像

`board_core_test` 是默认关闭的实板诊断 feature，只能与 `board_2k1000 + sata_scratch_rw` 组合。构建命令为：

```bash
make -C os la64-2k1000-core-tests
python3 scripts/boot_2k1000_tftp.py \
  --interface en8 \
  --image kernel-2k1000-core-tests.ui
```

该镜像在 ramfs 根目录创建 `/board_core_test` 标记。initproc 看到标记后，将运行计划覆盖为 `libctest -> cyclictest -> ltp`，并强制 LTP 使用 inline runner 和双 libc 的 274 个非网络白名单。白名单覆盖进程生命周期、虚拟内存、信号、时钟/定时器、调度、futex、pipe、epoll/eventfd 和本地 VFS，不包含 accept/bind/connect/listen/send/recv/socket 等网络用例。覆盖仅存在于本次启动的 ramfs，不写 P1 `/sdcard/os_test.conf`，也不改变正式 scratch 镜像的默认全量配置。

libctest 和 cyclictest 从上述独立 FAT32 工作区运行；LTP 二进制继续从只读 P1 执行，但显式设置 `TMPDIR=/tmp`、`TMPBASE=/tmp`、`HOME=/`，并清除继承自 QEMU 的 `LTP_DEV*=/dev/vdb2`，避免临时文件或设备测试误碰只读源/块节点。聚焦组超时分别为 900s、180s 和每 libc 3600s。

2026-07-12 最终实板镜像为 `12380736` 字节，SHA-256 `7dc2d763568026ff79edc36e91cbab16fcd623362f03c2c6e42f73ba2cd6807e`。musl libctest 静态/动态全通过；glibc 完整结束且修复目标 `statvfs`、`dlopen`、`tls_get_new_dtv` 通过，剩余为 libc 差异和待复核同步/stdio 项。cyclictest 两套均完成 400-task hackbench 压力。LTP 两套各执行 274 项并到达组尾；`futex_wait05`、`select01` 和两轮 1000-waiter `futex_cmp_requeue01` 均通过，未再出现 `kernel stack slot 1024` panic。

聚焦 LTP 仍暴露 symlink/execveat、getdents、`/proc/self/maps`、pipe 大写入、无测试块设备及 glibc 后半程 poll/select 时序长尾。外层 runner 返回 0 只代表调度完整结束，不能视为 548 次调用全部通过。

按正式顺序下一阶段是 netperf/iperf。当前 2K1000 路径仍跳过外部网卡探测并使用 loopback-only 网络栈，因此必须先完成板载 GMAC/PHY 驱动与 smoltcp 接入，再迁移网络组的运行目录；不能把 U-Boot TFTP 网卡可用误认为 MangoCore 已具备运行期网络设备。
