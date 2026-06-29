---
title: "系统架构 (System Architecture)"
category: architecture
status: stable
author: MangoCore Team
last_update: 2026-06-29
tags: [architecture, boot, hal, kernel]
---

# 系统架构

## 概述

MangoCore 是一个 `#![no_std]` 裸机 Rust 内核，支持 RISC-V 64 和 LoongArch64 两套架构后端。内核入口、陷阱处理、页表实现和定时器操作由 `hal/` 隔离；文件系统、内存管理、进程调度、网络和系统调用位于架构无关层。

系统架构章节覆盖全局模块边界、启动顺序和 HAL 平台接口。网络协议栈的协议、套接字和设备适配细节由 `docs/06_net/` 维护。

## 依据范围

| 主题 | 主要源码 |
|------|----------|
| 模块根和启动顺序 | `os/src/main.rs` |
| HAL 抽象 | `os/src/hal/mod.rs` |
| RISC-V 平台后端 | `os/src/hal/arch/riscv/` |
| LoongArch64 平台后端 | `os/src/hal/arch/loongarch64/` |
| 文件系统初始化 | `os/src/fs/mod.rs` |
| 驱动初始化 | `os/src/drivers/mod.rs` |
| 初始进程和调度入口 | `os/src/task/mod.rs`, `os/src/task/processor.rs` |

## 总体分层

```
+------------------------------------------------------------------+
|                          userspace ELF                           |
+------------------------------------------------------------------+
| syscall/mod.rs flat dispatch + domain modules                    |
| fs/syscalls | syscall/process/* | net/syscall/* | misc helpers   |
+------------------------------------------------------------------+
| process/task layer                                               |
| TaskControlBlock | ProcessControlBlock | signal | futex | waitq  |
+------------------------------------------------------------------+
| kernel services                                                  |
| VFS/MountFS/PageCache | MemorySet/VMA/PageTable | smoltcp net    |
+------------------------------------------------------------------+
| drivers                                                          |
| virtio block | virtio net | serial | platform devices            |
+------------------------------------------------------------------+
| HAL                                                              |
| trap | timer | TLB | page table impl | context switch | console  |
+------------------------------------------------------------------+
| QEMU / OpenSBI / platform firmware                               |
+------------------------------------------------------------------+
```

## 顶层模块

`os/src/main.rs` 声明的内核模块是当前架构边界的第一层依据。

| 模块 | 职责 |
|------|------|
| `console` | 日志和字符输出初始化 |
| `drivers` | 块设备、网卡、串口等设备入口 |
| `fs` | VFS、具体文件系统、设备文件、PageCache 和挂载 |
| `hal` | 架构相关的启动、陷阱、页表、定时器、TLB 和上下文切换 |
| `mm` | 堆、物理页、页表、VMA、mmap、缺页处理和用户内存访问 |
| `net` | 网络协议栈、套接字和网络系统调用实现 |
| `syscall` | syscall 编号、分发、errno 和通用辅助函数 |
| `task` | 任务、进程、调度、信号、futex、IPC、timer 子系统 |
| `timer` | 时间结构、时钟换算和 timerfd/timeout 依赖的通用时间类型 |
| `trace`, `panic_diag`, `utils` | 诊断、追踪和工具代码 |

## 启动主线

`rust_main()` 是架构无关的 Rust 入口。该函数中的初始化顺序是：

```
bootstrap_init()
mem_clear()
console::log_init()
trace::init()
mm::init()
machine_init()
task::timer_subsystem_init()
drivers / fs / net init
fs::vfs::posix_lock::init_posix_lock_manager()
task::add_initproc()
task::run_tasks()
```

其中 `bootstrap_init()` 和 `machine_init()` 来自 HAL，`mm::init()` 完成内核堆和物理页分配器初始化并激活 `KERNEL_SPACE`。`task::run_tasks()` 进入调度循环后不再返回到普通初始化路径。

### `rust_main()` 主入口

`os/src/main.rs::rust_main()` 将上述顺序固定在同一个函数内。该函数是阅读全局初始化依赖的主入口：

```rust
#[no_mangle]
pub fn rust_main() -> ! {
    bootstrap_init();
    mem_clear();
    // 这一行可能有误，需要后续处理
    #[cfg(all(feature = "block_mem"))]
    move_to_high_address();
    console::log_init();
    trace::init();
    println!("[kernel] Console initialized.");
    mm::init();
    println!("[kernel] Hello, world!");
    // note that remap_test is currently NOT supported by LA64, for the whole kernel space is RW!
    // #[cfg(feature = "riscv")]
    // mm::remap_test();

    machine_init();
    crate::task::timer_subsystem_init();

    // ── Initramfs 启动路径 ──
    #[cfg(feature = "initramfs")]
    {
        // 在 mm::init() 之后创建 VFS_ROOT: 创建 RamFS + 解包 cpio + 挂载 devfs/proc/tmp
        crate::fs::vfs::posix_lock::init_posix_lock_manager();
        fs::initramfs_init();

        drivers::init_net_device();
        net::config::init();

        // 先探测块设备（需要连续物理页 DMA，必须在 preload 分配页之前做）
        fs::mount_boot_block_devices();

        // 安装预装载的测试 payload（迁移期保留，在块设备探测之后避免页碎片化）
        #[cfg(feature = "preload_payloads")]
        fs::install_preload_payloads();
    }

    // ── Legacy 启动路径（initramfs 特性未启用时）──
    #[cfg(not(feature = "initramfs"))]
    {
        drivers::init_net_device();
        net::config::init();
        #[cfg(feature = "block_virt")]
        println!("[kernel] block in virt mode!");
        #[cfg(feature = "oom_handler")]
        println!("[kernel] oom_handler is enabled!");
        #[cfg(feature = "heap_trace")]
        println!("[kernel] heap_trace is enabled!");
        fs::flush_preload();
        fs::mount_tools_disk();
    }

    crate::fs::vfs::posix_lock::init_posix_lock_manager();
    task::add_initproc();
    // note that in run_tasks(), there is yet *another* pre_start_init(),
    // which is used to turn on interrupts in some archs like LoongArch.
    task::run_tasks();
    panic!("Unreachable in rust_main!");
}
```

这段代码给出三个关键边界。`bootstrap_init()` 早于 `.bss` 清零，用于架构后端必须提前完成的机器状态；`mm::init()` 早于驱动、文件系统和任务创建，说明后续对象分配、页表构造和 VFS 初始化都依赖内核堆与物理页分配器；`task::run_tasks()` 是初始化阶段和调度运行期之间的分界点，进入后由 task 层循环推进 timer、网络轮询、文件系统回收和 ready queue。

## 初始化分支

文件系统初始化根据编译特性存在两条路径：

| 条件 | 初始化路径 |
|------|------------|
| `feature = "initramfs"` | 初始化 initramfs 根文件系统，随后初始化网卡和网络配置，再挂载启动块设备 |
| 非 `initramfs` | 初始化网卡和网络配置，打印特性信息，刷新预加载数据并挂载工具盘 |

分支结束后，`rust_main()` 统一调用 POSIX lock 管理器初始化、加入 init 进程并进入调度器。initramfs 分支在解包 cpio 前还会提前调用一次同一初始化入口。`INITPROC` 在 `task/mod.rs` 中以 lazy static 形式选择 `/init`，若不存在则回退到 `/initproc`。

## 文档索引

| 文档 | 内容 |
|------|------|
| `README.md` | 架构总览、模块边界、启动主线 |
| `architecture.md` | 系统架构详解，覆盖分层、数据结构、启动/异常/调度流程和测试映射 |
| `module-map.md` | 内核根模块、依赖方向、feature 影响 |
| `initialization-flow.md` | `rust_main()` 初始化阶段、initramfs/legacy 分支 |
| `boot-and-trap.md` | 启动细节、系统调用和异常/中断路径 |
| `trap-and-syscall-entry.md` | trap 分类、syscall ABI 接入、缺页和 timer 中断 |
| `hal-and-platform.md` | HAL 接口、rv64/la64 平台后端差异 |
| `riscv64-platform.md` | RISC-V 64 HAL 后端、SV39、SBI、trap |
| `loongarch64-platform.md` | LoongArch64 HAL 后端、TLB refill、ASID、trap |
| `runtime-services.md` | 日志、trace、timer、调度循环维护、关机诊断 |
| `debugging.md` | 架构级启动、trap、timer、后台服务调试路径和测试映射 |

## 与其他目录的关系

| 子系统 | 详细文档 |
|--------|----------|
| 系统调用 | `docs/02_syscall/` |
| 内存管理 | `docs/04_mm/` |
| 进程与任务 | `docs/05_process/` |
| 网络 | `docs/06_net/` |
