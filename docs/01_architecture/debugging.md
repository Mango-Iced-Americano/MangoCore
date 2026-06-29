---
title: "系统架构调试与测试映射"
category: architecture
status: stable
author: MangoCore Team
last_update: 2026-06-29
tags: [architecture, debug, boot, trap, test]
---

# 系统架构调试与测试映射

## 1. 调试入口地图

系统架构问题通常先表现为“启动不到用户态”“trap 后无法返回”“timer 不调度”或“后台服务不推进”。定位时先判断事件停在哪个全局阶段：

```
entry.asm
  -> rust_main()
  -> mm::init()
  -> machine_init()
  -> fs/drivers/net init
  -> task::add_initproc()
  -> task::run_tasks()
  -> trap_return()
  -> user mode
```

| 症状 | 首个源码入口 | 下一步 |
|------|--------------|--------|
| 无任何内核输出 | `hal/arch/*/entry.asm` | linker script、QEMU/OpenSBI 参数、`.bss` 清零 |
| 打印停在 `mm::init()` 前后 | `os/src/main.rs`, `os/src/mm/mod.rs` | heap、frame allocator、`KERNEL_SPACE` 映射 |
| fs/net 初始化后无 init | `task::add_initproc()` | `/init`、`/initproc`、`AddressSpace::from_elf()` |
| 用户态第一条 syscall 失败 | `hal/arch/*/trap/mod.rs` | `a7/a0..a5`、PC 前进、`syscall::syscall()` |
| 缺页后直接 SIGSEGV | `mm/address_space.rs::do_page_fault()` | VMA 查找、growdown、`page_fault.rs` 分类 |
| timer 不触发抢占 | `hal/arch/*/time.rs`, `task::timer_interrupt_handler()` | timer programming、interrupt enable、调度循环 |
| 网络/timeout/回收不推进 | `task/processor.rs::run_tasks()` | background net poll、`do_wake_expired()`、FS reclaim |

## 2. 启动阶段检查表

| 阶段 | 成功信号 | 失败时检查 |
|------|----------|------------|
| `bootstrap_init()` | 平台早期状态可继续执行 | la64 exception entry、TLB refill、DMW/page walk；rv64 通常为空路径 |
| `mem_clear()` | 全局静态变量初值正常 | `.bss` 边界和 linker script |
| `console::log_init()` | 能看到后续内核日志 | HAL console、SBI console、LOG 环境 |
| `mm::init()` | heap/frame/page table 可用 | `heap_trace::enable()`、`init_frame_allocator()`、内核段映射 |
| `machine_init()` | trap/timer 进入运行期 | trap entry、timer interrupt enable |
| rootfs 分支 | `/dev`、`/proc`、root inode 可用 | initramfs 解包、块设备挂载、工具盘 |
| `add_initproc()` | init TCB/PCB 入队 | ELF 文件、用户栈、trap context、fd 0/1/2 |
| `run_tasks()` | 调度循环开始取任务 | ready queue、current cache、`__switch` |

## 3. trap 调试路径

trap 问题要区分入口 ABI、领域处理和返回用户态三段：

| 段 | 文件 | 关键状态 |
|----|------|----------|
| 入口保存 | `hal/arch/*/trap/` | 用户寄存器、fault address、cause、PC |
| syscall 分发 | `syscall/mod.rs` | syscall id、args、seccomp、match 分支、返回值 |
| MM fault | `mm/address_space.rs`, `mm/page_fault.rs` | VMA、PTE、FaultAction、MemoryError |
| 信号交付 | `task/signal/` | pending、mask、signal frame、`rt_sigreturn` |
| 返回用户态 | `trap_return()` | trampoline、user token、ASID、signal delivery |

`rt_sigreturn` 是特殊 syscall：trap 后端不能用普通返回值覆盖 `a0`，否则用户上下文恢复会被破坏。

## 4. 运行期后台服务

`run_tasks()` 同时承担后台推进职责：

| 服务 | 入口 | 测试现象 |
|------|------|----------|
| console poll | `hal::console_getchar()` | Ctrl+C、Ctrl+T、TTY 输入 |
| timeout wake | `do_wake_expired()` | nanosleep、futex timeout、poll/select timeout |
| network poll | `NET_INTERFACE.try_poll()` | socket connect/accept/send/recv 进展 |
| FS reclaim | `fs::reclaim::maybe_reclaim_fs_caches()` | PageCache 压力、写回/回收 |
| zombie drain | `take_zombie_tasks(64)` | fork/exit 压力下内核栈释放 |
| shared futex compact | `compact_shared_futex()` | shared futex 表失效 waiter 清理 |

## 5. 测试映射

| 功能 | 推荐测试 |
|------|----------|
| 双架构构建 | `make rv64-kernel-build-only`, `make la64-kernel-build-only` |
| 启动到 shell/init | `make rv64-run`, `make la64-run` |
| syscall/trap | basic、busybox、LTP syscall 基础用例 |
| 缺页/信号 | mmap、mprotect、SIGSEGV、exec/fork 相关 LTP |
| timer/调度 | nanosleep、timerfd、futex timeout、cyclictest |
| 后台网络 | busybox 网络工具、iperf、netperf |
| PageCache/reclaim | iozone、文件压力、fork/exec 混合压力 |
