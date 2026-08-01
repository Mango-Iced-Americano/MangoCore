---
title: "系统架构 (System Architecture)"
category: architecture
status: draft
last_update: 2026-07-30
tags: [architecture, boot, hal, kernel]
---

# 系统架构

MangoCore 是支持 RISC-V 64 与 LoongArch64 的 `#![no_std]` 裸机内核。HAL 隔离 entry、trap、页表、TLB、timer 与上下文切换；MM、task、VFS、network 和 syscall 保持架构无关。

## 启动主线

```text
QEMU → OpenSBI/firmware → arch entry → rust_main()
  → bootstrap_init → populate_memory_regions (FDT snapshot + DRAM regions)
  → mem_clear → console/trace → mm → PlatformInfo → machine/timer
  → random::init → bootargs::load + boot-profile defaults
  → initramfs CPIO → VFS_ROOT → devfs + /dev/tty bootstrap
  → normal: net init + register_boot_block_devices
  → add_initproc → /init (fallback: boot-profile init path) → /sbin/init (PID1) → test-runner → run_tasks
```

`populate_memory_regions()` 在 `bootstrap_init()` 之后、`mem_clear()` 之前执行。对 `BootProtocol::RiscvFdt`（RV64 QEMU ktest 已验证），它将验证过的 DTB 字节复制到 `#[link_section = ".data.boot"]` 标注的 2 MiB 静态缓冲区内，确保 BSS 清零不会擦除 DTB 数据。LA64（`LoongArchLegacy`）和 VF2（`UbootGo`）不走快照路径，使用编译期静态回退。post-heap `build_platform_info()` 始终从持久化快照解构设备信息，绝不回头读原始 firmware DTB 物理地址。

`initramfs` 是唯一生产启动根。内核解包 CPIO、建立 `VFS_ROOT` 并只准备 devfs/tty 与挂载点；它不会选择或挂载外部磁盘。normal 模式随后仅发现并注册块设备，PID1 决定挂载策略。regression 与 ktest 不附加外部磁盘；ktest 直接建立内核测试任务而不加载用户态 PID1。

`/init` 是 shim，exec 到 `/sbin/init`。PID1 挂载 `/proc`、`/sys`、`/run`、`/dev/shm`，在 normal 模式将 x0 挂到 `/sdcard`、x1 P1 挂到 `/tools`；`/tmp` 优先 bind `/sdcard/tmp`，失败后使用 tmpfs。PID1 监督 `/usr/libexec/mangocore/test-runner`。

## 关键源码

| 主题 | 源码 |
|---|---|---|
| Rust 启动入口 | `os/src/main.rs`（唯一的共享启动编排，读取 boot ABI profile） |
| 平台配置 | `os/src/hal/platform/{mod.rs,info.rs}`（共享 `PlatformInfo`、init 回退路径与默认 root 设备；静态 boot contract 不生成设备） |
| FDT 预堆快照 | `os/src/hal/firmware/{mod.rs,fdt.rs}`（`capture_fdt_snapshot()`、`.data.boot` 段快照缓冲区、`has_valid_dtb()` 协议门禁） |
| FDT 后堆枚举 | `os/src/hal/firmware/fdt.rs`（`build_platform_info()` 从 `.data.boot` 快照构建设备列表） |
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
