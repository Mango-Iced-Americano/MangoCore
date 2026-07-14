---
title: "隔离 APK 可写运行时测试"
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
  - "os/src/fs/mod.rs"
  - "user/src/bin/initproc.rs"
entry_points:
  - "la64-qemu-apk-run"
  - "la64-2k1000-apk-tests"
arch:
  rv64: build-only
  la64: supported
related_docs:
  - "docs/03_fs/2k1000-full-test-disk.md"
  - "docs/03_fs/init-and-rootfs.md"
  - "docs/06_net/dhcp.md"
---

# 隔离 APK 可写运行时测试

## 1. 目标与边界

该门禁验证 MangoCore 能运行未经内核特制的 Alpine `apk-tools` 静态程序，并完成
DNS、HTTPS、签名索引、依赖解析、包下载、解包、维护脚本、trigger 和动态程序执行。
它不是把系统根目录直接改成可写，也不承诺重启后保留已安装软件包。

实板存储职责固定如下：

| 路径 | 后端 | 可写性 | 用途 |
|------|------|--------|------|
| `/sdcard` | SSD P1 ext4 | 只读 | 官方测试集 |
| `/scratch/apk-cache` | SSD P2 FAT32 | 可写 | 可删除、可重建的 APK 下载缓存 |
| `/tools` | SSD P3 ext4 | 只读 | 工具与测试资产 |
| `/run/apk-root` | RAMFS | 可写、易失 | APK 数据库、安装根和 musl 运行时 |

FAT32 只适合缓存原始 `.apk` 文件，不能作为安装根：APK 安装树依赖 Unix mode、
符号链接和其他 FAT32 无法可靠表达的元数据。P1/P3 及用户态块设备节点继续保持只读，
因此当前门禁不会改变已验证的竞赛测试盘内容。

## 2. 构建组成

`APK_RUNTIME=1` 使 initramfs 额外包含：

- 目标架构的 `/bin/apk.static`；
- `/etc/apk/repositories` 中的 Alpine edge main/community/testing；
- Alpine 仓库签名公钥；
- 由 `apk_test` feature 创建的易失 `/apk_test` 标记。

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

## 4. 当前结论与后续阶段

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

下一阶段若需要“重启后仍可使用 `apk add` 的系统”，应单独创建 P4 可写 ext4 数据
分区或 overlay 上层，并加入分区身份、容量、只读降级、`sync/fsync`、断电恢复和配额
门禁。不得直接把 P1/P3 改成可写来代替该设计。
