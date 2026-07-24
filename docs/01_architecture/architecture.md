---
title: "系统架构详解 (System Architecture)"
category: architecture
status: draft
last_update: 2026-07-23
tags: [architecture, boot, hal, trap, runtime]
---

# 系统架构详解

## 分层

```text
QEMU / firmware
  → HAL (entry, trap, timer, TLB, page table, context switch)
  → drivers (virtio block/net, serial)
  → kernel services (MM/VMA, VFS/MountFS/PageCache, smoltcp)
  → task/process (TCB, PCB, signals, futex, wait queues)
  → syscall flat dispatch → user ELF/libc
```

两架构共享内核主体。HAL 向上提供页表、trap context、timer 与 TLB invalidate；修改 PTE 必须走该路径完成架构刷新。

## 当前启动合同

`rust_main()` 依次执行早期 HAL、BSS 清零、日志/trace、MM、machine/timer、随机数与 bootargs。随后：

```text
initramfs CPIO → VFS_ROOT → devfs + /dev/tty
normal only: network init → register /dev/vd* block devices
normal: /init → /sbin/init (PID1) → test-runner → run_tasks
ktest: kernel test task → run_tasks
```

CPIO initramfs 是生产根。内核只准备 VFS、devfs 和块设备节点；不选择或挂载外部 x0/x1。PID1 挂载 procfs、sysfs、`/run`、`/dev/shm`、normal x0 `/sdcard` 与 x1 P1 `/tools`；`/tmp` 首选 bind `/sdcard/tmp`，否则 tmpfs。

镜像 ABI 固定为 x0=rootfs/sdcard，x1=tools ext4 P1 + FAT32 scratch P2。regression 与 ktest 是零盘 profile。

## 执行路径

```text
user a7/a0..a5 → arch trap → syscall::syscall → domain sys_xxx → trap_return
fault → AddressSpace::do_page_fault → VMA/filemap/CoW → trap_return
timer interrupt → task::timer_interrupt_handler → scheduler maintenance
```

`run_tasks()` 除调度外还推进到期 timer、网络 poll、PageCache reclaim 与 zombie 清理。

## 构建门禁

```bash
make kernel ARCH=rv64 PROFILE=normal
make kernel ARCH=la64 PROFILE=normal
make check ARCH=rv64 PROFILE=normal
make -C os ktest-run ARCH=rv64 PROFILE=normal
make lint
```

双架构必须串行。`make lint` 覆盖双架构 debug/release 的首方 warning 基线。
