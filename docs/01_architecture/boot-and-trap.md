---
title: "启动与陷阱路径 (Boot and Trap Flow)"
category: architecture
status: draft
last_update: 2026-07-23
tags: [architecture, boot, trap, syscall]
---

# 启动与陷阱路径

## 启动

`main.rs` 显式引入 rv64 entry 与由 build script 为各架构生成的 initramfs 汇编段。启动只走 CPIO initramfs 根：

```text
firmware → arch entry → rust_main → VFS_ROOT/initramfs
→ /init → /sbin/init (PID1) → test-runner → scheduler
```

PID1 而非内核负责 normal 模式的 x0/x1 挂载和 `/tmp` 策略。x0 是根文件系统/sdcard；x1 P1 是 ext4 tools，P2 是 FAT32 scratch。regression/ktest 没有外部盘。

## syscall 与异常

```text
user a7/a0..a5 → arch trap handler → syscall::syscall(id, args)
→ sys_xxx → trap_return → user
```

两架构使用 `a7` 传 syscall id、`a0..a5` 传参数、`a0` 返回结果。缺页经 `AddressSpace::do_page_fault()` 分流到 VMA、filemap、shared-write 或 CoW；所有 PTE 改动必须经 HAL 刷新 TLB。

timer interrupt 进入 `task::timer_interrupt_handler()`；调度器同时推进 timeout、网络 poll、FS reclaim 和 zombie 回收。
