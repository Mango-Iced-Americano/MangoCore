---
title: "内核模块地图 (Kernel Module Map)"
category: architecture
status: draft
last_update: 2026-07-23
tags: [architecture, modules, kernel]
---

# 内核模块地图

## 根模块

| 模块 | 职责 |
|---|---|
| `hal` | entry、trap、页表、TLB、timer、context switch |
| `mm` | heap、frame allocator、地址空间、VMA、fault、uaccess |
| `fs` | VFS/MountFS、initramfs、devfs、块设备注册、PageCache |
| `task` | TCB/PCB、调度、信号、futex、IPC、timer |
| `net` / `drivers` | 网卡驱动、smoltcp、socket 与网络 syscall |
| `syscall` | syscall id、负 errno、扁平分发 |

`os/src/lang_items.rs` 是单一 cfg-gated 文件；没有架构副本。

## 启动依赖

```text
rust_main
 ├─ hal::{bootstrap_init,machine_init}
 ├─ console/trace/mm/timer/random/bootargs
 ├─ fs::{initramfs_init,register_boot_block_devices}
 ├─ drivers::init_net_device → net::config::init
 └─ task::{add_initproc,run_tasks}
```

initramfs 构造 `VFS_ROOT` 并提供 devfs/tty bootstrap；块设备注册与 PID1 的挂载策略分离。`/init` 将控制权交给 `/sbin/init`，后者监督 test runner。ktest 使用独立内核任务。

## 编译和镜像合同

`initramfs` 是生产根；已不存在额外的内存块根、预装 payload 或 legacy root 分支。normal QEMU 固定使用 x0/rootfs 与 x1/tools+sandbox；regression 和 ktest 使用零盘 profile。
