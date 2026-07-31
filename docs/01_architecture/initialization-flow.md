---
title: "初始化流程 (Initialization Flow)"
category: architecture
status: draft
last_update: 2026-07-31
tags: [architecture, boot, init]
---

# 初始化流程

## 内核阶段

`rust_main()` 以固定次序建立平台、内存和启动文件系统：

```text
bootstrap_init → mem_clear → console::log_init → trace::init → mm::init
→ machine_init → timer_cpu_init(CPU0) → random::init → bring_up_secondary_cpus
→ bootargs::load
→ initramfs_init (CPIO → VFS_ROOT → devfs/tty)
→ normal only: init_net_device → net::config::init → register_boot_block_devices
```

AP 在 BSP 发布 scheduler-ready 后安装内核页表根，调用同一个
`timer_cpu_init()` 建立自己的绝对调度 tick，随后进入本地 `run_tasks()`。CPU0 的本地
timer 同时驱动全局 kernel timer；AP 的 timer 只负责本核调度抢占。

`initramfs_init()` 在 RamFS 中解包 newc CPIO，创建 PID1 所需目录并挂载最小 devfs。`register_boot_block_devices()` 只发现设备并注册 `/dev/vd*`；它不挂载 x0/x1。

随后 normal 启动执行 `task::add_initproc()` 和 `task::run_tasks()`。ktest 由 `spawn_ktest_task()` 直接进入调度器；regression 不初始化网络或块设备。

## 用户态阶段

内核加载 `/init`，该 shim exec `/sbin/init`。PID1：

1. 建立并挂载 `/proc`、`/sys`、`/run`、`/dev/shm`；
2. normal 模式挂载 x0 到 `/sdcard`、x1 的 ext4 P1 到 `/tools`；
3. 将 `/sdcard/tmp` bind 到 `/tmp`，失败或无 x0 时以 tmpfs 兜底；
4. fork/exec test runner，并负责 SIGCHLD 回收、失败关机和 rescue shell。

镜像角色固定为 x0=rootfs/sdcard、x1=tools（P1 ext4）+ scratch（P2 FAT32）。regression 与 ktest 为零盘 profile。

## 调试边界

| 症状 | 首先检查 |
|---|---|
| CPIO 前失败 | `mm::init()`、`fs::initramfs_init()` |
| 无 `/dev/tty` | `prepare_kernel_bootstrap_filesystem()`、`add_initproc()` |
| x0/x1 不可用 | `register_boot_block_devices()` 与 PID1 mount 日志 |
| runner 未启动 | `/sbin/init`、`/usr/libexec/mangocore/test-runner` |
| 内核测试失败 | ktest bootargs 与 `spawn_ktest_task()` |
