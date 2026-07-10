---
title: "初始化流程 (Initialization Flow)"
category: architecture
status: stable
author: MangoCore Team
last_update: 2026-07-09
tags: [architecture, boot, init]
---

# 初始化流程

## 1. 概述

初始化流程由 `os/src/main.rs::rust_main()` 串行执行。MangoCore 在进入调度器之前必须完成三类状态准备：

| 类别 | 代表入口 | 作用 |
|------|----------|------|
| 机器状态 | `bootstrap_init()`、`machine_init()` | 配置异常入口、timer interrupt、架构寄存器和页表后端 |
| 内核服务 | `console::log_init()`、`trace::init()`、`mm::init()` | 输出、trace、堆、物理页、内核地址空间 |
| 用户态运行环境 | `fs::*`、`drivers::init_net_device()`、`net::config::init()`、`task::add_initproc()` | 根文件系统、设备、网络、init 任务 |

初始化路径不创建独立内核线程。所有启动阶段工作都在 `rust_main()` 所在执行流中完成，直到 `task::run_tasks()` 进入单核调度循环。

## 2. 主流程

`rust_main()` 的可执行顺序如下：

```
bootstrap_init()
mem_clear()
move_to_high_address()                 [block_mem]
console::log_init()
trace::init()
mm::init()
machine_init()
task::timer_subsystem_init()

if initramfs:
  fs::vfs::posix_lock::init_posix_lock_manager()
  fs::force_ramfs()                    [board_2k1000]
  fs::initramfs_init()
  drivers::init_net_device()           [not board_2k1000]
  net::config::init()
  fs::mount_boot_block_devices()       [not board_2k1000]
  fs::install_preload_payloads()        [preload_payloads]
else:
  drivers::init_net_device()
  net::config::init()
  fs::flush_preload()
  fs::mount_tools_disk()

fs::vfs::posix_lock::init_posix_lock_manager()
task::add_initproc()
task::run_tasks()
```

`task::run_tasks()` 不返回。`rust_main()` 末尾的 `panic!("Unreachable in rust_main!")` 是不可达路径诊断。

### 2.1 主入口源码

启动主线在 `os/src/main.rs::rust_main()` 中以完整函数体现：

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
        #[cfg(feature = "board_2k1000")]
        {
            // 2K1000LA 实板首阶段只验证 U-Boot -> uImage -> UART -> initramfs。
            // 在 SATA/AHCI 和板载网卡路径逐项验证前，先禁止外部块设备 lazy probe。
            fs::force_ramfs();
        }
        fs::initramfs_init();

        #[cfg(not(feature = "board_2k1000"))]
        drivers::init_net_device();
        #[cfg(feature = "board_2k1000")]
        {
            // 实板网卡不是 QEMU virtio-net，最小上板阶段暂不枚举 virtio PCI 网卡。
        }
        net::config::init();

        // 先探测块设备（需要连续物理页 DMA，必须在 preload 分配页之前做）
        #[cfg(not(feature = "board_2k1000"))]
        fs::mount_boot_block_devices();
        #[cfg(feature = "board_2k1000")]
        {
            // 最小上板路径已 force_ramfs()，此处不触发 BLOCK_DEVICES。
        }

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

函数体中的 feature 分支决定启动阶段能看到哪些外部镜像和根文件系统。`block_mem` 路径会先把链接进内核的镜像移动到高地址；默认 `initramfs` 路径先建立 initramfs 根，再初始化网卡并挂载启动块设备；`board_2k1000` 实板最小上板路径在建立 initramfs 根前调用 `force_ramfs()`，并跳过外部 net/block probe。非 `initramfs` 路径则通过 `flush_preload()` 和 `mount_tools_disk()` 准备 legacy 测试环境。两条分支最后都把初始进程放入调度队列。

## 3. 早期阶段

### 3.1 `bootstrap_init()`

`bootstrap_init()` 由 HAL 后端提供：

| 架构 | 当前行为 |
|------|----------|
| rv64 | `hal/arch/riscv/mod.rs` 中为空实现 |
| la64 | `hal/arch/loongarch64/mod.rs` 中只允许 core 0 继续；关闭 `RVACFG` 缩减模式，配置 exception entry、TLB refill entry、4KiB TLB page size、timer vector、FPU/SIMD、paging、DMW2、page walk，并校验 `CPUCFG1` 的 VALEN/PALEN |

la64 的 `bootstrap_init()` 执行在 `mem_clear()` 前，因此该阶段只依赖架构寄存器、链接符号和已可用的基础输出路径。硬件/构建地址位宽不一致会在 `mm::init()` 前直接 panic。rv64 对应配置主要由 OpenSBI 和后续 `machine_init()` 承担。

### 3.2 `mem_clear()`

`mem_clear()` 使用链接脚本导出的 `sbss` 和 `ebss`：

```rust
extern "C" {
    fn sbss();
    fn ebss();
}
```

默认行为是清零 `sbss..ebss`。启用 `zero_init` feature 时，清零范围变为 `sbss..MEMORY_END`。该差异直接来自 `main.rs` 中的条件编译分支。

### 3.3 `move_to_high_address()`

`move_to_high_address()` 只在 `block_mem` feature 下编译和调用。它读取汇编段中的 `simg..eimg` 镜像，把目标内存 `DISK_IMAGE_BASE..DISK_IMAGE_BASE+0x1000_0000` 清零后拷贝镜像内容。该路径用于 legacy block memory rootfs 镜像。

## 4. 日志、trace 与 MM

### 4.1 日志和 trace

```
console::log_init()
trace::init()
println!("[kernel] Console initialized.")
```

`console::log_init()` 先于 `mm::init()`，用于启动阶段可见日志。`trace::init()` 在 MM 初始化前执行，后续 syscall、trap、调度和网络等模块可记录 trace/perf 事件。

### 4.2 MM 初始化

`mm::init()` 的顺序在 `os/src/mm/mod.rs` 中固定：

```
heap_allocator::init_heap()
heap_trace::enable()                    [heap_trace]
frame_allocator::init_frame_allocator()
KERNEL_SPACE.lock().activate()
```

顺序含义：

| 步骤 | 依赖 | 结果 |
|------|------|------|
| 初始化内核堆 | 链接脚本和静态 heap 区 | `alloc` 容器可用 |
| 初始化 heap trace | `heap_trace` feature | 记录堆分配追踪 |
| 初始化物理页分配器 | `ekernel..MEMORY_END` | `FrameTracker` 分配可用 |
| 激活内核地址空间 | `KERNEL_SPACE` lazy 构造 | 后续驱动、FS、task 在内核页表下运行 |

`KERNEL_SPACE` 映射 trampoline、内核代码/只读/数据/BSS 段、`ekernel..MEMORY_END` 物理内存和 `MMIO` 表中的设备区间。具体映射策略在 `docs/04_mm/initialization-and-kernel-space.md` 展开。

## 5. 平台运行期初始化

`machine_init()` 在 `mm::init()` 后执行，说明运行期机器初始化可以使用已激活的内核地址空间。

| 架构 | `machine_init()` 行为 |
|------|----------------------|
| rv64 | `trap::init()` 安装 kernel trap entry；`trap::enable_timer_interrupt()` 打开 supervisor timer interrupt |
| la64 | `trap::init()`；`get_timer_freq_first_time()`；打印 CPUCFG/Misc/RVACfg/MMAP_BASE；启用 timer interrupt |

随后调用 `task::timer_subsystem_init()`。项目注释明确说明第一次 timer deadline 由任务 timer 子系统在启动后设置，而不是 rv64 `machine_init()` 立即设置。

## 6. initramfs 启动路径

启用 `initramfs` feature 时，`rust_main()` 执行：

```
fs::vfs::posix_lock::init_posix_lock_manager()
fs::force_ramfs()                     [board_2k1000]
fs::initramfs_init()
drivers::init_net_device()            [not board_2k1000]
net::config::init()
fs::mount_boot_block_devices()        [not board_2k1000]
fs::install_preload_payloads()          [preload_payloads]
```

### 6.1 `fs::initramfs_init()`

`fs::initramfs_init()` 负责建立 initramfs 根。根据 `fs/mod.rs` 的职责划分，该路径创建 RamFS 根、解包 CPIO，并挂载 devfs/proc/tmpfs 等公共文件系统。

### 6.2 网卡与网络配置

默认 initramfs 路径先执行 `drivers::init_net_device()`，再执行 `net::config::init()`。这个顺序保证网络协议栈配置时可以看到已注册的网络设备。

`board_2k1000` 最小上板阶段跳过 `drivers::init_net_device()`，只保留 `net::config::init()` 的协议栈配置。原因是实板网卡不是 QEMU virtio-net，GMAC/PHY 驱动接入前不应枚举 virtio PCI 网卡。

### 6.3 块设备挂载和 payload

默认 initramfs 路径中，`fs::mount_boot_block_devices()` 在 `fs::install_preload_payloads()` 之前调用。`main.rs` 注释给出的理由是块设备探测需要连续物理页 DMA，先于 preload 分配页可以降低页碎片影响。

`board_2k1000` 最小上板阶段先调用 `fs::force_ramfs()`，再跳过 `fs::mount_boot_block_devices()`。这样可以先验证串口输出、cpio 解包和 `/init` 加载，避免 SATA/AHCI 或残留 QEMU virtio 块设备路径影响首启定位。

## 7. legacy 启动路径

未启用 `initramfs` feature 时执行：

```
drivers::init_net_device()
net::config::init()
println!("[kernel] block in virt mode!")        [block_virt]
println!("[kernel] oom_handler is enabled!")    [oom_handler]
println!("[kernel] heap_trace is enabled!")     [heap_trace]
fs::flush_preload()
fs::mount_tools_disk()
```

legacy 路径中没有 `fs::initramfs_init()`。根 inode 的实际文件系统由 `fs::ROOT_INODE` lazy 初始化路径决定；块设备可用时探测 FAT32/ext4，不可用时回退内存文件系统。

## 8. 进入调度器前的统一步骤

两条启动分支结束后都会执行：

```
fs::vfs::posix_lock::init_posix_lock_manager()
task::add_initproc()
task::run_tasks()
```

### 8.1 POSIX lock manager

统一路径调用 POSIX lock manager 初始化。initramfs 分支内也调用同一入口，文档保持这一路径描述与 `main.rs` 一致。

### 8.2 `task::add_initproc()`

`task::add_initproc()` 将 `INITPROC` 加入调度队列。`INITPROC` 在 `task/mod.rs` 中 lazy 构造，优先尝试加载 `/init`，失败后加载 `/initproc`。初始化过程会创建第一个 `TaskControlBlock`/`ProcessControlBlock`、用户地址空间、trap context、标准 fd 等进程资源。

### 8.3 `task::run_tasks()`

`run_tasks()` 是单核调度主循环。它不仅选择 ready task，还周期性执行：

| 维护动作 | 入口 |
|----------|------|
| 唤醒过期 timer | `do_wake_expired()` |
| 网络轮询 | `NET_INTERFACE.try_poll()` |
| FS 缓存回收 | `fs::reclaim::maybe_reclaim_fs_caches()` |
| zombie 回收 | zombie queue drain |
| stale zombie 清理 | 周期性 cleanup |
| shared futex 表压缩 | shared futex compact |

这些维护动作解释了为什么网络和文件系统在启动时初始化后，运行期仍依赖调度循环推进。

### 8.4 初始化依赖链

从源码阅读角度看，初始化流程可以分成三段，每段建立下一段需要的前置条件：

| 阶段 | 建立的能力 | 下一阶段依赖 |
|------|------------|--------------|
| 早期平台阶段 | 清 `.bss`、安装早期架构状态、建立日志输出 | 后续初始化可以使用全局静态变量并输出诊断信息。 |
| 内核基础设施阶段 | 初始化堆、frame allocator、内核页表、trap/timer | VFS、driver、task 可以分配内存并处理异常/定时器。 |
| 用户启动阶段 | rootfs、网卡配置、块设备挂载、initproc | 调度器有可运行用户任务，用户程序能通过 syscall 访问文件和网络。 |

`mm::init()` 是中间分界点。它之前的代码不能假设动态内存和完整页表能力已经可用；它之后的 `fs::initramfs_init()`、`drivers::init_net_device()`、`task::add_initproc()` 都会依赖堆对象、`Arc`、物理页或地址空间构造。

`machine_init()` 是另一条分界线。MM 激活内核页表后，平台后端安装 trap 和 timer；缺页、syscall、timer interrupt 都要等这一步完成后才能按运行期路径处理。`task::run_tasks()` 之前调用 `task::timer_subsystem_init()`，保证 sleep、futex timeout、POSIX timer 等等待对象可以注册到 kernel timer queue。

## 9. 初始化分支与 feature 对照

| feature | 对初始化流程的影响 |
|---------|--------------------|
| `riscv` | `main.rs` 引入 `hal/arch/riscv/entry.asm` |
| `loongarch64` | HAL 后端选择 la64；`main.rs` 中 la64 entry 引入语句处于注释状态 |
| `initramfs` | 选择 initramfs 分支，并引入架构对应 initramfs 汇编段 |
| `block_mem` | 调用 `move_to_high_address()`，并引入架构对应 rootfs 镜像段 |
| `preload_payloads` | 非 `block_mem` 下引入 preload 汇编段；initramfs 分支安装 payload |
| `zero_init` | `mem_clear()` 清零到 `MEMORY_END` |
| `heap_trace` | `mm::init()` 初始化 heap trace，并在 legacy 分支输出启用信息 |
| `oom_handler` | legacy 分支输出启用信息；MM 分配失败路径启用 OOM 处理 |

## 10. 失败定位路径

| 症状 | 首选断点/日志点 | 可能阶段 |
|------|-----------------|----------|
| 没有任何内核输出 | 架构 entry、`bootstrap_init()` | 固件到内核入口、console 不可用 |
| 只输出 console 初始化 | `mm::init()` | heap/frame allocator/kernel page table |
| `Hello, world!` 后卡住 | `machine_init()`、trap 初始化 | trap entry/timer interrupt 配置 |
| initramfs 解包前异常 | `fs::initramfs_init()` | rootfs 初始化、CPIO 数据段 |
| 网络初始化后卡住 | `drivers::init_net_device()`、`net::config::init()` | virtio/veth 设备或网络 poll |
| 无 init 进程 | `task::add_initproc()` | `/init`/`/initproc` ELF 装载、用户地址空间 |
| 进入调度后无输出 | `task::run_tasks()` | ready queue、timer interrupt、trap return |

## 11. 测试映射

| 测试目标 | 覆盖阶段 | 推荐命令 |
|----------|----------|----------|
| rv64 初始化可编译 | rv64 HAL、MM、task | `cd os && make rv64-kernel-build-only` |
| la64 初始化可编译 | la64 HAL、MM、task | `cd os && make la64-kernel-build-only` |
| rv64 启动路径 | entry 到 `run_tasks()` | `cd os && make rv64-run` |
| la64 启动路径 | entry 到 `run_tasks()` | `cd os && make la64-run` |
| init 进程装载 | FS、MM、task | basic/busybox 镜像启动 |
| syscall/trap | trap、syscall、task | basic/LTP syscall 用例 |

文档修改不改变内核行为；涉及初始化代码变更时需按项目规则执行双架构编译和对应 QEMU 启动验证。

## 12. 源文件索引

| 路径 | 内容 |
|------|------|
| `os/src/main.rs` | `rust_main()`、`mem_clear()`、feature 汇编段 |
| `os/src/mm/mod.rs` | `mm::init()` |
| `os/src/mm/kernel_space.rs` | 内核地址空间映射 |
| `os/src/hal/arch/riscv/mod.rs` | rv64 `bootstrap_init()` 和 `machine_init()` |
| `os/src/hal/arch/loongarch64/mod.rs` | la64 `bootstrap_init()` 和 `machine_init()` |
| `os/src/fs/mod.rs` | initramfs、legacy root、预加载和挂载入口 |
| `os/src/drivers/mod.rs` | driver 入口 |
| `os/src/net/config.rs` | 网络配置初始化 |
| `os/src/task/mod.rs` | `INITPROC`、`add_initproc()` |
| `os/src/task/processor.rs` | `run_tasks()` 调度主循环 |
