---
title: "2K1000LA 单 SSD 完整测试镜像"
module: "fs/board-image"
category: fs
status: draft
owner: MangoCore Team
last_updated: 2026-07-14
code_paths:
  - "scripts/make_2k1000_full_test_disk.py"
  - "scripts/make_2k1000_tools_partition.py"
  - "scripts/make_2k1000_p4_ext4.py"
  - "scripts/make_2k1000_p4_qemu_disk.py"
  - "scripts/restore_2k1000_p2.py"
  - "scripts/write_2k1000_p3.py"
  - "scripts/write_2k1000_p4.py"
  - "os/src/fs/filesystem.rs"
  - "os/src/fs/mod.rs"
  - "os/src/main.rs"
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
  - "mount_boot_block_devices_with_writable_persist"
  - "2k1000-cpython-p3-write"
  - "2k1000-p4-write"
arch:
  rv64: supported
  la64: supported
related_docs:
  - "docs/03_fs/init-and-rootfs.md"
  - "docs/03_fs/devfs.md"
  - "docs/01_architecture/boot-and-trap.md"
  - "docs/08_testing/cpython-isolated.md"
  - "docs/08_testing/apk-isolated.md"
---

# 2K1000LA 单 SSD 完整测试镜像

## 1. 产物

本机产物位于 `/private/tftpboot/`：

| 文件 | 大小 | SHA-256 |
|------|------|---------|
| `mango-2k1000la-full-test-mbr.img` | 6,443,499,520 B（6145MiB） | `416f84060bca79ab06ef5596d8cfd1801b8ae3e56ae3d2e65e99a66b612ef19f` |
| `mango-2k1000la-full-test-mbr.img.xz` | 400MiB | `80e1e2addac136da2b9ccffbcad349d915b3b4fec20ef25e11a86193162bc584` |
| `mango-2k1000la-full-test-mbr.img.layout.json` | 702 B | 分区起点、长度和 payload 哈希 |
| `kernel-2k1000-run.ui` | 12,319,472 B | `9fcb0df721f115af8b3d42358cf9560344d3fe1adabb5acc731ef5bf44c0f3f1` |

## 2. MBR 布局

磁盘使用传统 MBR（DOS disklabel），disk id 为 `0x4d414e47`，不包含 GPT 或扩展分区。

| 分区 | LBA 范围 | 大小 | 类型 | 用途 |
|------|----------|------|------|------|
| P1 | `2048..8390655` | 4GiB | `0x83` ext4 | 官方 LA64 完整测试集，自动挂载 `/sdcard` |
| P2 | `8390656..11012095` | 1280MiB | `0x0c` FAT32 | 保持 `/dev/vda2` 兼容；staged 镜像额外挂载为 `/scratch` |
| P3 | `11012096..12584959` | 768MiB | `0x83` ext4 | MangoCore 工具，单盘时自动挂载 `/tools` |
| P4 | `12584960..20973567` | 4GiB | `0x83` ext4 | staged 持久状态，身份校验后读写挂载 `/persist` |

P1 来自干净的 `fs-img-dir/sdcard-la.img.xz`，注入当前 `os_test.conf`（`mode=run`、`mask=0xFFF`）。不要使用被 QEMU 写过的仓库根目录 `sdcard-la.img` 作为母盘，除非它重新通过只读 `e2fsck -f -n`。原始 6GiB 完整镜像的 MBR 仍只有 P1-P3；P4 由独立 staged 工具在验证真实 SSD 后创建，不要求重写已有三个分区。4GiB P4 结束后到 32GB SSD 末端继续保留未分配空间。

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

`kernel-2k1000-run.ui` 是默认关闭上板诊断、`LOG=off` 且嵌入 `mode=run`/`mask=0xFFF` fallback 配置的正式镜像。早期只读挂载及写入探针镜像已完成验收，不再作为公共 Make 目标保留。

预期日志应包含 `/dev/sda1` Ext4、`/dev/sda2` Fat32、`/dev/sda3` Ext4，随后 P1 以 `RDONLY` 挂到 `/sdcard`、P3 以 `RDONLY` 挂到 `/tools`。

2026-07-11 已在 `TS32GMTS400` 实体 SSD 上完成 25 块网络写入，全部 `12584960` 个 sector 均通过逐块读回 CRC；U-Boot 可读取三个分区，MangoCore 实板启动后成功完成上述三分区识别、只读挂载及 `/tools`、`/musl`、`/glibc` bind。原始 AHCI 写入/flush、内核 P2 FAT32 文件探针和 staged 用户态 `/scratch` 冒烟测试均已通过；正式 `kernel-2k1000-run.ui` 仍保持全盘只读。

## 6. 内核 AHCI 写入探针

正式解除只读前曾使用独立 feature 构建自恢复探针。该一次性 Make 目标已在
原始写入、flush、恢复和 FAT32 持久化验收全部完成后移除，以下内容保留为安全
设计与历史验收记录，不是日常构建入口。

探针硬匹配 SSD 型号 `TS32GMTS400`、MBR 签名和 disk id `0x4d414e47`，解析四个主分区后，在最后一个分区末端之外保留 2048 个 sector，再测试连续 8 个 sector。当前镜像对应测试范围为 `12587008..12587015`，不属于 P1/P2/P3。

测试顺序固定为：备份原 4KiB → 写入确定性模式 → `FLUSH CACHE EXT` → 读回逐扇区比较 → 写回备份 → 再次 flush → 读回确认恢复。第一次写命令发出后，无论中间步骤成功或失败都必须执行恢复；恢复不能完成或不能验证时内核立即 panic，不再继续文件系统路径。该 feature 保持 ramfs-only，不注册可写设备节点，也不改变正式 run 镜像的三层只读保护。

第二阶段探针仍按正式路径只读挂载 P1/P3，并保持 `/dev/vda2` 只读；内核仅为探针构造一个不暴露给用户态的 P2 可写视图，创建 `MANGO_RW_PROBE/PAYLOAD.BIN`，写入 6KiB 后强制 page cache 写回，重新打开 FAT32 验证目录、文件和内容，再删除并第三次打开确认清理持久化。只有该阶段通过后，才允许把 P2 作为用户态 scratch 分区开放。

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

需要交互检查三个 SSD 分区、DHCP 和外网时，统一使用 HTTPS curl Shell：

```bash
make -C os la64-2k1000-curl-shell
make 2k1000-boot IMAGE=kernel-2k1000-curl-shell.ui
```

该镜像将 P1 只读挂载到 `/sdcard`、P2 FAT32 可写挂载到 `/scratch`、P3 只读
挂载到 `/tools`，启用 GMAC0/DHCP，随后通过易失 `/board_shell` 标记进入 Bash；
磁盘上的 `/sdcard/os_test.conf` 不会被改写。该综合目标取代早期 SATA-only、
静态网络联合和 scratch 中间镜像。

所有保留的 `sata_scratch_rw` 诊断/回归目标都要求 P2 同时满足 partno 2、MBR
type `0x0c` 和 FAT32 三个条件才挂载到 `/scratch`。P1 `/sdcard`、P3 `/tools`
以及 `/dev/sda*`、`/dev/vda*` 块设备节点仍为只读，避免用户态绕过挂载策略直接
覆盖磁盘。

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

首轮 glibc iozone 会立即在动态加载器 `_dl_runtime_resolve_lasx` 的 `xvst` 指令触发 `InstructionNonDefined`。根因不是文件系统，而是内核对两种架构统一写死 `AT_HWCAP=0x112d`：该值是 RISC-V IMAFDC 字母位图，在 LoongArch ABI 中却包含 LASX 和 LBT_MIPS。修复后 RISC-V 保留原值，LoongArch 根据 CPUCFG1/2 与内核上下文保存能力生成 HWCAP。当前 trap 与 signal context 已保存 LSX，因此可向硬件支持的用户态发布 `HWCAP_LSX`；LASX/LBT 仍未保存，对应 EUEN/HWCAP 保持关闭。

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

板载 GMAC0/PHY、DHCP、默认路由、DNS、HTTP 和带证书校验的 HTTPS 已在实板
通过。后续网络测试阶段使用保留的 `la64-2k1000-net-tests` 目标推进
netperf/iperf 与网络 LTP，不再重建早期单子系统探针镜像。

### 7.3 CPython 隔离运行时与 P3 替换镜像

CPython 使用 Alpine 目标架构预编译运行时，不依赖根文件系统中的 Python。运行时与 L3-L9 脚本被打包到只读 P3 的 `/tools/tests/cpython`，测试产生的临时文件统一写入 P2 `/scratch/cpython`。P3 同时预置 `/tools/usr/bin/python3` 和 `python` 启动链接；staged 启动还会在可写 `/usr/bin` 安装兜底链接，因此 Shell 可直接使用全局命令，私有 musl loader、动态库、`PYTHONHOME`、CA 和 scratch 路径由包装器设置。构建命令为：

```bash
make -C os la64-2k1000-cpython-tests
make -C os la64-2k1000-cpython-tools
```

产物分别为 `kernel-2k1000-cpython-tests.ui` 和 `mango-2k1000la-cpython-tools-p3.img`。后者是严格固定为 768 MiB 的裸 ext4 分区 payload，只能写入已验证 MBR 布局的 P3 `0xA80800..0xC00800`，不包含 MBR/P1/P2。生成器会输出 `.img.json`，其 `image_bytes` 必须为 `805306368`、`target_sectors` 必须为 `1572864`；任一数值不符都不得写盘。

网线替换 P3 使用受限目标，显式确认固定起点：

```bash
make 2k1000-cpython-p3-write CONFIRM_P3_START=0xA80800
```

`scripts/write_2k1000_p3.py` 在写盘前硬校验 payload JSON 清单及 SHA-256、`TS32GMTS400` 型号、完整 MBR CRC、disk id 和三个分区的起点/长度。它把镜像切成三个 256 MiB 块，每块 `0x80000` 个 sector，起始 LBA 固定为 `0xA80800`、`0xB00800`、`0xB80800`；每块依次验证 TFTP 字节数/CRC、SCSI 写入 sector 数和 SSD 读回 CRC。完成后重置 SCSI，并从 P3 `ext4load` 最新 `L7_filesystem.py` 与宿主文件做长度/CRC 比对。不得从 LBA0 写入该文件，也不得修改工具中的固定边界来复用为通用写盘器。

替换后先在 U-Boot 执行 `ext4ls scsi 0:3 /tests/cpython`，再启动专用内核：

```bash
make 2k1000-boot IMAGE=kernel-2k1000-cpython-tests.ui
```

2026-07-14 含全局启动器的新 payload SHA-256 为 `4a0f8a1bf6fad6ed89a9d0479438df8843f2d95d1482ddcdecc57276d364972c`。镜像内 `/usr/bin/python3` 已确认是指向 `/tools/tests/cpython/python3-wrapper.sh` 的符号链接，包装器模式为 `0755`；三个 256 MiB 块的宿主期望 CRC32 依次为 `e2118d3d`、`2d7315b8`、`638ff43b`，写入器的板端读回应逐项相同。专用 uImage 在实板完成 CPython L3-L9，judge 为 `72/72`、组退出码 0；全局启动器当前已完成 rv64/la64 QEMU 验收，等待新 P3 写入后的实板 Shell 复核。

详细测试层级、QEMU 门禁和已知边界见 `docs/08_testing/cpython-isolated.md`。

### 7.4 APK 易失安装根与 P2 缓存

当前 APK 阶段不放宽 P1/P3 的只读策略。`apk.static`、仓库列表和签名公钥嵌入
initramfs；软件包数据库、安装树和私有 musl 运行时写入 RAMFS
`/run/apk-root`，只有可删除的 `.apk` 下载缓存写入 P2
`/scratch/apk-cache`。FAT32 不能完整表达 APK 安装所需的 Unix mode 和符号链接，
因此不得把 `/scratch` 直接用作 `--root`。

构建与启动入口为：

```bash
make -C os la64-2k1000-apk-tests MODE=release
make 2k1000-boot IMAGE=kernel-2k1000-apk-tests.ui
```

该镜像依次验证 HTTPS 索引、签名、依赖下载、`busybox + zlib` 安装、维护脚本和
trigger，再通过安装根自己的 musl loader 执行 BusyBox。成功标志为
`[apk-test] RESULT=PASS`。该安装树随复位消失；长期持久安装必须新增独立可写 ext4
分区或 overlay，不得直接改写竞赛测试集 P1 或工具 P3。详细流程见
`docs/08_testing/apk-isolated.md`。

### 7.5 P4 ext4 持久状态

P4 是默认关闭的 staged 能力，不改变正式 run 镜像。固定范围为
`0xC00800..0x1400800`（末端不含），共 `0x800000` 个 512B sector、4GiB；卷标为
`MANGO_STATE`，UUID 为 `4d414e47-5354-4154-4500-000000000004`。当前内核没有 ext4
journal replay，因此 P4 必须以 `^has_journal` 创建，挂载前同时拒绝
`HAS_JOURNAL` 与 `RECOVER` 位。只凭“第四分区是 ext4”不能获得写权限。

生成 P4 payload 和同布局 QEMU 稀疏盘：

```bash
make 2k1000-p4-image
make 2k1000-p4-qemu-disk
make -C os la64-qemu-apk-persist-tests MODE=release
```

`mango-2k1000la-state-p4.img` 是裸分区 payload，不含 MBR。QEMU fixture 保持
P1/P3 只读、P2 为缓存、P4 为安装根；同一 fixture 连续启动两次时，第一次必须输出
`PASS mode=install`，第二次必须输出 `PASS mode=reuse`。提交标记只在 APK 数据库、
安装树和临时标记均完成 `sync` 后原子改名；无标记表示上次安装未完成，下一次启动
必须删除残留并重建，不能把半成品当作可复用状态。

真实 SSD 先执行只读预检，再显式确认三个危险边界写入：

```bash
make 2k1000-p4-preflight
make 2k1000-p4-write \
  CONFIRM_P4_START=0xC00800 \
  CONFIRM_P4_END=0x1400800 \
  CONFIRM_DISK_SECTORS=62533296
```

写入器硬匹配 `TS32GMTS400`、`62533296` sectors、disk id `0x4d414e47`、现有
P1-P3 的精确边界和旧 MBR CRC32 `f469e65a`。4GiB payload 按 16 个 256MiB 块
逐块完成 TFTP CRC、SSD 写入和读回 CRC，全部成功后才把 P4 分区项发布到 MBR；
MBR 提交或后续验证失败时尝试恢复旧 MBR。最后必须由 U-Boot 从 `scsi 0:4` 读取
`MANGO_STATE.txt` 并比对长度/CRC，随后才允许启动：

```bash
make -C os la64-2k1000-apk-persist-tests MODE=release
make 2k1000-boot IMAGE=kernel-2k1000-apk-persist-tests.ui
```

实板也要完整启动两次并分别得到 `mode=install`、`mode=reuse`。2026-07-14 已完成：
16 个 payload 分块覆盖 `0xC00800..0x1400800` 并全部读回一致，新 MBR CRC32 为
`6538e5cb`，P4 哨兵为 97 字节、CRC32 `c8f1b4ff`。同一专用 uImage 首次启动输出
`PASS mode=install`，再次复位后输出 `PASS mode=reuse`，两轮均为 `RESULT=PASS`；
P1-P3 边界及只读策略保持不变。
