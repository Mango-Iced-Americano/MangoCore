---
title: "系统架构 (System Architecture)"
category: architecture
status: draft
last_update: 2026-07-23
tags: [architecture, boot, hal, kernel]
---

# 系统架构

MangoCore 是支持 RISC-V 64 与 LoongArch64 的 `#![no_std]` 裸机内核。HAL 隔离 entry、trap、页表、TLB、timer 与上下文切换；MM、task、VFS、network 和 syscall 保持架构无关。

## 启动主线

```text
QEMU → OpenSBI/firmware → arch entry → rust_main()
  → bootstrap_init → mem_clear → console/trace → mm → machine/timer
  → random::init → bootargs::load
  → initramfs CPIO → VFS_ROOT → devfs + /dev/tty bootstrap
  → normal: net init + register_boot_block_devices
  → add_initproc → /init → /sbin/init (PID1) → test-runner → run_tasks
```

`initramfs` 是唯一生产启动根。内核解包 CPIO、建立 `VFS_ROOT` 并只准备 devfs/tty 与挂载点；它不会选择或挂载外部磁盘。normal 模式随后仅发现并注册块设备，PID1 决定挂载策略。regression 与 ktest 不附加外部磁盘；ktest 直接建立内核测试任务而不加载用户态 PID1。

`/init` 是 shim，exec 到 `/sbin/init`。PID1 挂载 `/proc`、`/sys`、`/run`、`/dev/shm`，在 normal 模式将 x0 挂到 `/sdcard`、x1 P1 挂到 `/tools`；`/tmp` 优先 bind `/sdcard/tmp`，失败后使用 tmpfs。PID1 监督 `/usr/libexec/mangocore/test-runner`。

## 关键源码

| 主题 | 源码 |
|---|---|
| Rust 启动入口 | `os/src/main.rs` |
| initramfs/VFS bootstrap | `os/src/fs/{mod.rs,initramfs.rs,boot_block.rs}` |
| PID1 与 runner | `user/src/bin/{init.rs,initd.rs,test_runner.rs}` |
| 调度入口 | `os/src/task/{mod.rs,processor.rs}` |
| 角色清单 | `os/make/image-roles.mk` |

## 构建与验证

根目录 Make facade 要求显式 `ARCH` 与 `PROFILE`：

```bash
make kernel ARCH=rv64 PROFILE=normal
make kernel ARCH=la64 PROFILE=normal
make check ARCH=rv64 PROFILE=normal
make -C os ktest-run ARCH=rv64 PROFILE=normal
make test ARCH=rv64 PROFILE=regression
make lint
```

RV64 与 LA64 必须串行。内核 `lang_items.rs` 使用单一文件中的 `#[cfg(target_arch = ...)]` 分支。
