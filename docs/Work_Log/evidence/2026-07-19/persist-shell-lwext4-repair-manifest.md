# lwext4 persist-shell 修复证据清单

## 当前结论（最终审查补记）

- 当前 `ext4_generic_open2()` 以每个 child inode 的真实 mode 校验路径类型；RV64/LA64
  `KTEST=ext4` 均 9/9，teardown 与离线 fsck clean，双架构 build-only 退出 0。
- untouched P4 来源与首次启动前临时完整磁盘的 P4 分段 SHA-256 均为
  `35ad1da5dbeb48ab514e920f0513f2e2cfdba90f2975b1b8309a3674e0117b18`。
  当前代码能清理旧 `sh` 并到达 `RESULT=PASS`，但随后一个 16 字节写读删探针把
  `P4_RW_OK` 覆盖进 DDGS `ddgs.py`，冷启动由整文件摘要门禁 fail closed。
- 离线定位证明这不是 DDGS 升级：旧 `ddgs.py` inode 60994 引用块
  `797722..797728`，但 untouched 卷块位图把 7 块全部标为空闲；原卷同时已有大量
  bitmap、extent checksum、重复目录项和 multiply-claimed block 错误。新 lwext4
  按磁盘位图分配新文件时因此复用了仍被旧文件引用的块。
- 仅在临时副本上多轮执行 `e2fsck -fy`，第 3～5 轮及最终 `e2fsck -fn` 均 clean；
  DDGS 保持已审计摘要
  `3c321b9445ec57db0bd1d06899c6a10eeeea2817fa7ecbc1b2e08f37878bed24`。
- 修复后的同一 P4 在新 lwext4 下完成首次启动、persist-shell Python、P4 写读删和
  冷启动复用；两轮均 `stage=reuse`、`stage=prepared`、`RESULT=PASS`、ready，最终
  离线 `e2fsck -fn` Pass 1-5 无修复项，DDGS 摘要不变。
- 结论只放行“新 lwext4 接管 offline-fsck-clean 卷”。完整备份和 QEMU 原始样本未被
  修改；但另一个终端随后启动了当前实板镜像，生产 P4 被 rw 挂载并出现第三个异常 DDGS
  摘要。生产 SSD 在自身离线 fsck/重建并复检 clean 前禁止再次由新 ext4 启动写入。

## 身份与安全边界

- 分支：`board-develop-combined`
- 测试前 HEAD：`5a8ad725be552b1f82e71932bdd732d149d29b89`，测试工作树为 `5a8ad725-dirty`
- Docker：`zhouzhouyi/os-contest:20260104`，`linux/amd64`，宿主 `/Users/luzimo/dev/MangoCore -> /app`
- `os_test.conf` SHA-256：`42ea3e4cebd1cbb5ab1c2ba61e69f2232da54910208703e3e68ced85f61e7a70`
- SSD 完整备份：`/Users/luzimo/dev/ssd-backups/2k1000la-ssd-20260718T125651Z/ssd-full-32017047552.raw.zst`；本轮写 P4 前已完成既有长度/hash/zstd/首 1 MiB 校验。
- 实板迁移只修改 `/persist/apk-root/bin/sh` 的历史重复 symlink，并运行既有幂等配置同步；未改分区表、未格式化、未执行 U-Boot `scsi write`。

## RED

命令：

```text
docker run --rm --platform linux/amd64 -v /Users/luzimo/dev/MangoCore:/app -w /app \
  zhouzhouyi/os-contest:20260104 \
  bash -o pipefail -lc 'make -C os rv64-ktest KTEST=ext4 MODE=release LOG=off ...'
```

- `persist-fix-red-eexist-rv64.log`：8 passed / 1 failed；失败为 `duplicate symlink creation unexpectedly succeeded`。
- 只读解压备份 P4 后用 `debugfs` 审计 `/apk-root/bin`：存在 inode 295、606、986 三个同名 `sh`，均为 symlink，target 均为 `/bin/busybox`。

## GREEN：QEMU 与编译

- `persist-fix-green-ext4-rv64.log`：RV64 ext4 9/9、`KTEST RESULT: PASS`、teardown PASS、离线 e2fsck Pass 1-5，exit 0。
- `persist-fix-green-ext4-la64.log`：LA64 ext4 9/9、`KTEST RESULT: PASS`、teardown PASS、离线 e2fsck Pass 1-5，exit 0。
- `persist-fix-final3-rv64-build.log`：`make -C os rv64-kernel-build-only`，exit 0。
- `persist-fix-final3-la64-build.log`：`make -C os la64-kernel-build-only`，exit 0。
- final3 是历史实板构建基线；当前 final12 构建日志晚于最后一次 `initproc` 源码修改，final9 ext4 QEMU 日志晚于 C/Rust ext4 与 ktest 修改。

当前最终 C/Rust ext4 证据：

- `persist-fix-final9-green-ext4-rv64.log`：9/9、teardown PASS、e2fsck Pass 1-5。
- `persist-fix-final9-green-ext4-la64.log`：9/9、teardown PASS、e2fsck Pass 1-5。
- `persist-fix-final12-build-rv64.log`、`persist-fix-final12-build-la64.log`：最后一次 `initproc` 修改后严格串行 build-only，exit 0。
- `persist-fix-final10-build-qemu-persist.log`：包含最终 shell 验证语法修复的 LA64 persist-shell 镜像构建成功。
- `persist-fix-final12-board-build.log`：当前 2K1000LA persist-shell uImage 构建成功；文件
  16,931,816 bytes，SHA-256
  `417b095b504b0fc9144f0ec71d76fc69a1debc1bc438af3ec25b9b0db7b51bae`。

## RED：当前实板生产 P4

- 另一个终端在构建后执行 `make 2k1000-boot IMAGE=kernel-2k1000-persist-shell.ui`；TFTP
  16,931,816 bytes、CRC32 `c3ad3664`，LoongArch uImage checksum 通过。
- 启动时生产 P4 的 DDGS 摘要为
  `fdc6789be67b00a8ad599ed55c21ddf1af206f18d0a7633a8a9fb60455c4d4bc`，既不等于已审计
  源码，也不等于 QEMU 串写样本；完整性门禁正确拒绝，不能加白名单。
- 启动仍到达 persist-shell `RESULT=PASS`，再次证明应用门禁不能替代离线 fsck；随后有
  Python REPL 交互，故生产 P4 确有文件系统写入。未执行 `scsi write`、分区表更新或格式化。
- 宿主 boot harness 已停止，串口释放后发送 Ctrl-C、退出 Python/应用 shell、
  `/bin/busybox sync`、`/bin/busybox poweroff -f`；串口程序已退出，未取得 poweroff 回显。
- 关键串口字段与安全响应归档于 `persist-fix-final12-board-red-summary.txt`；完整临时串口
  日志保留在宿主 `/private/tmp/mango-2k1000-boot.log`，未把含交互控制码的全量文件纳入提交。

## RED：旧 P4 位图不一致导致跨文件覆盖

- `persist-fix-final10-qemu-first.log`：untouched P4 完成旧 `sh` 修复并进入
  `RESULT=PASS`；交互写读删探针输出 `P4_RW_OK`。
- `persist-fix-final10-qemu-reuse.log`：同盘冷启动 DDGS 摘要变为
  `a0d47e45aa3f763f9b147a26bd383b689e570c284589f9c769c9b93431e7b816`，完整性门禁拒绝。
- `persist-fix-final10-offline-p4-before.log`：原始旧卷在任何新写入之前已存在严重 fsck
  错误；`ddgs.py` inode 60994 size 26641，extent 为 `797722..797728`。
- `debugfs testi <60994>` 显示 inode 在用，但 `testb` 对上述 7 块全部显示 `not in use`。
- `persist-fix-final10-offline-p4.log`：写入后 DDGS inode 仍指向相同 extent，但内容开头
  为 `P4_RW_OK` 后跟零填充，且 fsck 报 inode block count、bitmap 和目录损坏。
- 该摘要不是可审计的新 DDGS 版本，禁止加入白名单。

## GREEN：旧 P4 离线修复后接管

- `persist-fix-final10-offline-fsck-repair.log`、
  `persist-fix-final10-offline-fsck-converge.log`：只修改临时 P4 副本；多轮 `e2fsck -fy`
  收敛后，第 3～5 轮和最终只读复检均无修复项。
- 修复后 P4 SHA-256：
  `a1af08bb8a1e8bbe79526354e3dcf579a2fe2126afcad4753dd29e530e736681`。
- `persist-fix-final11-qemu-fscked-first.log`：首次接管 `RESULT=PASS`，进入 persist-shell，
  Python 输出 `PERSIST_FSCKED_FIRST_PYTHON_OK`，P4 写读删输出 `P4_FSCKED_RW_OK`。
- `persist-fix-final11-qemu-fscked-reuse.log`：同盘冷启动 `RESULT=PASS`，DDGS 在线 SHA-256
  仍为 `3c321b...ed24`。
- `persist-fix-final11-offline-p4.log`：关机后 `e2fsck -fn` Pass 1-5 无修复项，提取的
  DDGS SHA-256 仍为 `3c321b...ed24`。

## GREEN：2K1000LA 实板

- 构建命令：`make -C os la64-2k1000-apk-persist-shell MODE=release`，见 `persist-fix-final3-board-build.log`。
- 最终镜像：`kernel-2k1000-persist-shell.ui`，16,931,816 bytes，SHA-256 `691bbc6658aac197d20798b7ca17038208e95a87ca7f4aaca46d67d5afef8eda`。
- U-Boot：TFTP 16,931,816 bytes，CRC32 `30e57fa7`；`iminfo` 为 LoongArch 且 checksum OK。
- 首次修复历史卷时，三个旧 `sh` 条目被逐项移除；现场只读复核最终 inode 295、`sh -> busybox`、可执行。
- 中间两轮分别因 chroot 缺少独立 `cat`/`rm` applet 输出 `RESULT=FAIL`，证明修复后的 exact wait 正确传播失败状态。
- 最终冷启动复用同一 P4：`stage=reuse` → `stage=prepared` → `RESULT=PASS` → ready。
- 实际执行 `persist-shell` 进入 `MangoPersist:/#`：`apk info -e busybox` 成功；`readlink /bin/sh` 为 `busybox`；Python 输出 `PY_OK p4-strict-align-v1 /persist/python-runtime/releases/28f61fb764f3/usr`；显式 BusyBox 临时文件写读删输出 `PERSIST_INTERACTIVE_PASS`。
- 完整串口：`persist-fix-board-serial.log`；摘要：`persist-fix-board-head-tail.txt`。

备注：串口中第一次人工交互探针在 BusyBox 子 shell 内误用了未安装的裸 `cat`/`rm`，因此该人工命令失败；紧接着用显式 `/bin/busybox cat/rm` 重跑并得到 `PERSIST_INTERACTIVE_PASS`。这不属于启动门禁失败，启动门禁此前已独立输出 `RESULT=PASS`。
