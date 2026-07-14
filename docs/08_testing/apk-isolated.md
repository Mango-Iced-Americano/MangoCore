---
title: "APK 隔离与 P4 持久化测试"
category: testing
status: draft
author: MangoCore Team
last_update: 2026-07-14
tags: [testing, apk, qemu, loongarch64, 2k1000, https, filesystem]
code_paths:
  - "os/Cargo.toml"
  - "os/Makefile"
  - "os/make/la64.mk"
  - "os/build_initramfs.sh"
  - "os/src/fs/filesystem.rs"
  - "os/src/fs/mod.rs"
  - "os/src/main.rs"
  - "user/src/bin/initproc.rs"
  - "scripts/make_2k1000_p4_ext4.py"
  - "scripts/make_2k1000_p4_qemu_disk.py"
  - "scripts/write_2k1000_p4.py"
entry_points:
  - "la64-qemu-apk-run"
  - "la64-2k1000-apk-tests"
  - "la64-qemu-apk-persist-tests"
  - "la64-2k1000-apk-persist-tests"
  - "2k1000-p4-write"
arch:
  rv64: build-only
  la64: supported
related_docs:
  - "docs/03_fs/2k1000-full-test-disk.md"
  - "docs/03_fs/init-and-rootfs.md"
  - "docs/06_net/dhcp.md"
---

# APK 隔离与 P4 持久化测试

## 1. 目标与边界

该门禁验证 MangoCore 能运行未经内核特制的 Alpine `apk-tools` 静态程序，并完成
DNS、HTTPS、签名索引、依赖解析、包下载、解包、维护脚本、trigger 和动态程序执行。
它不把系统根目录直接改成可写：第一阶段使用易失 RAMFS，第二阶段只在身份校验后的
P4 中验证跨重启保留，不代表正式系统已经切换为通用持久根。

实板存储职责固定如下：

| 路径 | 后端 | 可写性 | 用途 |
|------|------|--------|------|
| `/sdcard` | SSD P1 ext4 | 只读 | 官方测试集 |
| `/scratch/apk-cache` | SSD P2 FAT32 | 可写 | 可删除、可重建的 APK 下载缓存 |
| `/tools` | SSD P3 ext4 | 只读 | 工具与测试资产 |
| `/run/apk-root` | RAMFS | 可写、易失 | 第一阶段 APK 数据库、安装根和 musl 运行时 |
| `/persist/apk-root` | SSD P4 ext4 | staged 可写、持久 | 第二阶段 APK 数据库、安装根和 musl 运行时 |
| `/persist/apk-state` | SSD P4 ext4 | staged 可写、持久 | 提交标记和跨启动验收记录 |

FAT32 只适合缓存原始 `.apk` 文件，不能作为安装根：APK 安装树依赖 Unix mode、
符号链接和其他 FAT32 无法可靠表达的元数据。P1/P3 及用户态块设备节点继续保持只读，
因此当前门禁不会改变已验证的竞赛测试盘内容。

## 2. 构建组成

`APK_RUNTIME=1` 使 initramfs 额外包含：

- 目标架构的 `/bin/apk.static`；
- `/etc/apk/repositories` 中的 Alpine edge main/community/testing；
- Alpine 仓库签名公钥；
- 由 `apk_test` 或 `apk_persist_test` feature 创建的易失聚焦标记。

APK 3.x 不使用旧教程中的 `--initdb`。入口在运行前显式创建
`lib/apk/db`、`etc/apk`、`var/cache/apk` 和空的 `etc/apk/world`，再把仓库与公钥路径
指向 initramfs 中的只读配置。交互环境仍保留 main/community/testing 三仓库；自动
smoke 只在 RAMFS 生成 `edge/main` 列表，因为 `busybox`、`zlib` 及其依赖均来自
main，避免无关 testing 索引的 CDN 状态影响门禁。

## 3. 自动门禁

执行顺序固定为：

```text
version
  -> update edge/main (HTTPS + APKINDEX signature)
  -> fetch zlib (download cache write)
  -> add busybox zlib (dependency install + scripts/triggers)
  -> verify (APK database + installed executable)
  -> exec (private musl loader runs installed BusyBox)
```

成功日志必须同时包含：

```text
[apk-test] stage=verify
[apk-test] stage=exec
[apk-test] PASS root=/run/apk-root cache=...
[apk-test] RESULT=PASS
```

QEMU 使用官方 LA64 测试盘的 snapshot，且不挂 tools 磁盘：

```bash
make -C os la64-qemu-apk-run MODE=release
```

实板镜像和一键启动命令为：

```bash
make -C os la64-2k1000-apk-tests MODE=release
make 2k1000-boot IMAGE=kernel-2k1000-apk-tests.ui
```

实板通过 GMAC DHCP 获取网络，先执行既有 `/scratch` 持久化冒烟，再运行 APK 门禁。
共享网络上的三个 HTTPS 索引可能耗时数分钟，因此外层保留 900 秒有限超时；超时日志
同时打印原始 wait status 和按 shell 语义换算的退出码，避免把 `SIGKILL` 误判为 APK
自身返回值。

## 4. 易失阶段结论

2026-07-14 的 LA64 QEMU 已使用 `apk-tools 3.0.6-r0` 完成上述全部阶段，安装
`musl`、`busybox` 和 `zlib` 后，由 `/run/apk-root/lib/ld-musl-loongarch64.so.1`
成功执行新安装的 BusyBox。2K1000LA 首轮也完成在线索引、缓存写入和三包安装；原
300 秒保护在安装刚结束时触发，手工复核安装文件、P2 缓存和私有 loader 执行均成功，
据此将自动门禁时限改为 900 秒。后续复验还确认 `testing` 索引可独立长时间停滞，
因此最终 smoke 固定使用包含全部被测包的 `edge/main`，而不是把额外仓库可达性混入
包管理器功能判定。

最终 2K1000LA 镜像从 edge/main 识别 5920 个包，自动完成 zlib 下载、
musl/busybox/zlib 安装、post-install、trigger、数据库检查和私有 loader 执行，输出
`[apk-test] RESULT=PASS`。挂载表同时确认 P1 `/sdcard` 与 P3 `/tools` 保持只读，
P2 `/scratch` 为唯一持久可写分区。

P1/P3 和用户态块设备节点在该阶段始终只读。它验证了包管理器和网络链路，但安装树
随复位消失，不应把 P2 FAT32 缓存误当成持久软件根。

## 5. P4 持久化门禁

第二阶段新增固定 4GiB P4 ext4，挂载点为 `/persist`。内核只在以下条件全部满足时
开放写挂载：MBR 主分区号为 4、type 为 `0x83`、起点为 `0xC00800`、sector 数为
`0x800000`、文件系统为 ext4、UUID/卷标精确匹配，并且超级块既没有 journal，也不
要求 recovery。P1/P3 与块设备节点继续只读，P2 仍只承担可删除下载缓存。

持久门禁使用 `/persist/apk-state/committed-v1` 区分完整和中断安装：

1. 无提交标记时删除旧安装树，重新执行 HTTPS update、fetch 和 `apk add`。
2. 安装树与数据库先 `sync`，临时提交文件再 `sync`，原子改名后第三次 `sync`。
3. 有提交标记时禁止重装，直接验证数据库、私有 loader 和已安装 BusyBox。
4. 每次成功在 `boot-history` 追加 `install` 或 `reuse`，随后同步。

QEMU 验收必须复用同一块非 snapshot 稀疏磁盘：

```bash
make 2k1000-p4-image
make 2k1000-p4-qemu-disk
make -C os la64-qemu-apk-persist-tests MODE=release
```

第一次启动已验证输出 `PASS mode=install`，第二次启动输出 `PASS mode=reuse`，两次均
以 `[apk-persist] RESULT=PASS` 结束。实板构建入口为：

```bash
make -C os la64-2k1000-apk-persist-tests MODE=release
make 2k1000-boot IMAGE=kernel-2k1000-apk-persist-tests.ui
```

P4 payload、受限写盘命令和 MBR 发布顺序见
`docs/03_fs/2k1000-full-test-disk.md`。该功能仍是聚焦门禁，不代表正式 run 镜像已经
切换为通用可写根或实现了 overlay、配额和掉电一致性。真实 SSD 已完成 P4 写入；
首次实板启动完成 HTTPS update/fetch/add 并输出 `PASS mode=install`，第二次完整复位
后未重装，直接验证 P4 数据库和已安装 BusyBox，并输出 `PASS mode=reuse`。两轮均通过
GMAC DHCP、P2 scratch 冒烟和 `[apk-persist] RESULT=PASS`。
