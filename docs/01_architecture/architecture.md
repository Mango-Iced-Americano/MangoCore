---
title: "系统架构详解 (System Architecture)"
category: architecture
status: draft
author: MangoCore Team
last_update: 2026-06-29
tags: [architecture, boot, hal, trap, runtime]
---

# 系统架构详解

## 1. 概述

MangoCore 是 `#![no_std]` 裸机 Rust 内核。内核主入口位于 `os/src/main.rs::rust_main()`，硬件相关能力由 `os/src/hal/` 隔离，架构无关层由 MM、task/process、VFS、network、syscall、driver、timer、trace 等模块组成。

系统架构以 `main.rs` 的根模块声明和启动顺序为主线，向下展开到 HAL 后端、trap/syscall 入口、缺页路径、调度主循环和运行期维护任务。网络子系统在全局架构中的位置包括网卡初始化、`net::config::init()` 和调度循环中的后台 poll；协议栈和 socket 内部结构在网络目录的专题页展开。

## 2. 设计目标

| 目标 | 实现约束 |
|------|----------|
| 双架构支持 | rv64 与 la64 后端通过 HAL 统一导出 `PageTableImpl`、`TrapContext`、timer、TLB 和上下文切换接口 |
| 架构无关内核主体 | `mm`、`task`、`fs`、`net`、`syscall` 不直接依赖具体页表实现 |
| 单核抢占式调度 | timer interrupt 驱动任务切换；`TaskManager` 维护 ready/interruptible/zombie 队列 |
| Linux syscall 兼容 | trap 后端按 `a7/a0..a5` ABI 进入 `syscall::syscall()`，错误以负 errno 返回 |
| 可观测运行期 | syscall、调度、PageCache、trace 和 panic 诊断保留统计与日志入口 |
| 启动路径可切换 | `initramfs` 与 legacy root 路径由编译特性切换 |

## 3. 架构

### 3.1 总体层次

```
+-------------------------------------------------------------------+
|                           User ELF / libc                         |
+-------------------------------------------------------------------+
|                           syscall layer                           |
| os/src/syscall/mod.rs                                             |
| fs syscall | process syscall | mm syscall | net syscall | misc     |
+-------------------------------------------------------------------+
|                         process / task layer                      |
| TaskControlBlock | ProcessControlBlock | WaitQueue | signal       |
+-------------------------------------------------------------------+
|                     kernel service subsystems                     |
| MemorySet/VMA/PageTable | VFS/MountFS/PageCache | smoltcp net     |
+-------------------------------------------------------------------+
|                              drivers                              |
| block | net | serial | virtio frontends                           |
+-------------------------------------------------------------------+
|                                HAL                                |
| trap | timer | TLB | page table backend | switch | console        |
+-------------------------------------------------------------------+
|                      QEMU / OpenSBI / platform                    |
+-------------------------------------------------------------------+
```

### 3.2 根模块

`main.rs` 声明内核根模块：

| 模块 | 职责 |
|------|------|
| `console` | 日志输出、字符输入输出的内核侧入口 |
| `drivers` | 块设备、网卡、串口和设备初始化入口 |
| `fs` | root inode、VFS、MountFS、ext4/FAT/tmpfs/ramfs/procfs/devfs、PageCache |
| `hal` | 架构后端、trap、TLB、页表、timer、上下文切换和关机 |
| `lang_items` | 裸机 Rust 运行时所需 lang items |
| `math` | 数学辅助实现 |
| `mm` | 堆、物理页、内核地址空间、用户地址空间、VMA、mmap、缺页、uaccess |
| `net` | Socket trait、smoltcp、协议实现、网络 syscall |
| `panic_diag` | panic 诊断输出 |
| `syscall` | syscall 编号、名称映射、seccomp、分发、errno、公共辅助 |
| `task` | TCB/PCB、调度器、signal、futex、IPC、timer、进程生命周期 |
| `timer` | 时间结构、换算、TimeSpec |
| `trace` | trace 事件初始化和记录 |
| `utils` | 错误类型、路径、位图和通用工具 |

### 3.3 依赖方向

```
           +----------------+
           |      hal       |
           +----------------+
             ^     ^     ^
             |     |     |
       +-----+     |     +------+
       |           |            |
   +---+---+   +---+---+   +----+----+
   |  mm   |   | task  |   | drivers |
   +---+---+   +---+---+   +----+----+
       ^           ^            ^
       |           |            |
   +---+-----------+------------+---+
   |            syscall             |
   +---+-----------+------------+---+
       |           |            |
      fs          net         timer
```

关键边界：

| 依赖 | 说明 |
|------|------|
| `mm -> hal` | 使用 `PageTableImpl`、TLB invalidate、地址布局和 trap fault 信息 |
| `task -> hal` | 使用 `TrapContext`、`TaskContext`、`KernelStack`、timer interrupt 和 `__switch` |
| `syscall -> task/mm/fs/net` | 分发层不实现业务语义，只把 id 和参数送入领域模块 |
| `fs -> drivers` | rootfs、PageCache 后端和 devfs 依赖块设备/字符设备 |
| `net -> drivers` | 网络协议栈依赖网卡设备初始化和 poll |
| `task -> mm/fs` | 进程持有地址空间、fd table、cwd/root、procfs 可见状态 |

### 3.4 编译特性影响

| feature | 影响 |
|---------|------|
| `riscv` | 引入 `hal/arch/riscv/entry.asm`，选择 RISC-V HAL 后端 |
| `loongarch64` | 选择 LoongArch64 HAL 后端和 la64 trap/TLB 代码 |
| `initramfs` | 引入 initramfs 汇编段，启动时解包 CPIO 根文件系统 |
| `block_mem` | 引入 legacy rootfs 镜像段，`rust_main()` 调用 `move_to_high_address()` |
| `preload_payloads` | 引入预加载 payload，initramfs 路径调用 `fs::install_preload_payloads()` |
| `heap_trace` | `mm::init()` 中调用 `heap_trace::enable()` |
| `oom_handler` | 启用 frame allocator OOM 回收和调度 active tracker |

## 4. 关键数据结构和接口

### 4.1 HAL 公共导出

`hal/mod.rs` 向架构无关层导出：

| 类别 | 导出项 |
|------|--------|
| 上下文切换 | `__switch` |
| 启动 | `bootstrap_init`, `machine_init` |
| 页表 | `PageTableImpl`, `KernelPageTableImpl` |
| TLB | `tlb_invalidate` |
| trap context | `TrapContext`, `UserContext`, `MachineContext`, `TrapImpl` |
| trap 查询 | `get_bad_addr`, `get_bad_instruction`, `get_exception_cause` |
| timer | `get_clock_freq`, `get_time`, `program_timer_delta` |
| interrupt | `local_irq_save`, `local_irq_restore` |
| console | `console_putchar`, `console_getchar`, `console_flush` |
| shutdown | `shutdown` |

`IO_CHUNK_SIZE` 在 HAL 层定义，等于 `KERNEL_HEAP_SIZE / 128` 并限制在 64 KiB 到 256 KiB。`MAX_RW_COUNT` 是页对齐后的最大读写长度。

### 4.2 RISC-V 后端

| 项 | 实现 |
|----|------|
| 目录 | `os/src/hal/arch/riscv/` |
| 页表 | `Sv39PageTable` |
| trap 类型 | `scause::Trap` |
| 早期初始化 | `bootstrap_init()` 为空 |
| 运行期初始化 | `machine_init()` 调用 `trap::init()` 并启用 timer interrupt |
| 底层服务 | SBI console、timer、shutdown |

RISC-V trap 后端处理 `UserEnvCall`、instruction/load/store fault、timer interrupt 和异常信号注入。syscall 前后还更新进程时间统计并刷新 real timer。

### 4.3 LoongArch64 后端

| 项 | 实现 |
|----|------|
| 目录 | `os/src/hal/arch/loongarch64/` |
| 页表 | `LAFlexPageTable` |
| TLB | `tlb.rs` 管理 ASID 和 invalidate |
| 早期初始化 | 配置 exception entry、TLB refill、page walk、DMW、FPU/SIMD |
| 运行期初始化 | 安装 trap、初始化 timer frequency、启用 timer interrupt |
| 返回用户态 | `trap_return()` 传入 trap context、token 和 ASID |

la64 trap 后端还处理用户非对齐访存模拟、store/page modify 后补 dirty bit，以及 kernel trap 中的栈保护页诊断。

### 4.4 任务和进程

| 对象 | 粒度 | 说明 |
|------|------|------|
| `TaskControlBlock` | 线程 | 调度实体，持有 TID、内核栈、trap context、线程信号状态和调度字段 |
| `ProcessControlBlock` | 进程 | 持有 VM、fd table、fs 状态、namespace、sighand、futex、子进程关系 |
| `TaskManager` | 全局 | ready/interruptible/zombie 队列和 fetch/wake/remove 操作 |
| `Processor` | CPU | 当前任务、idle context、调度主循环 |

调度器是单核实现。默认 nice=0 时走 FIFO fast path；存在非零 nice 任务时扫描 ready queue，按 `(sched_vruntime, sched_nice, tid)` 选择。

### 4.5 内存对象

| 对象 | 说明 |
|------|------|
| `KernelSpace<PageTableImpl>` | 内核地址空间，映射内核段、物理内存和 MMIO |
| `AddressSpace<PageTableImpl>` | 用户进程地址空间 |
| `VmaSet` | 按 VPN 管理 VMA 和 mmap holes |
| `Vma` | 单个映射段，保存权限、文件后端、fork 标记和 page store |
| `FrameTracker` | 物理页 RAII 对象 |
| `PageTable` trait | 架构无关页表操作接口 |

## 5. 执行流程

### 5.1 `rust_main()` 主流程

```
bootstrap_init()
mem_clear()
move_to_high_address()              [block_mem]
console::log_init()
trace::init()
mm::init()
machine_init()
task::timer_subsystem_init()
initramfs branch or legacy branch
fs::vfs::posix_lock::init_posix_lock_manager()
task::add_initproc()
task::run_tasks()
```

`task::run_tasks()` 进入调度主循环后不返回。末尾 panic 用于不可达路径诊断。

### 5.2 MM 初始化

```
heap_allocator::init_heap()
heap_trace::enable()                   [heap_trace]
frame_allocator::init_frame_allocator()
KERNEL_SPACE.lock().activate()
```

`KERNEL_SPACE` 建立 trampoline、`.text`、`.rodata`、`.data`、`.bss`、`ekernel..MEMORY_END` 和 `MMIO` 表中区间的映射。内核空间激活后，后续驱动、FS 和 task 初始化运行在内核页表上。

### 5.3 initramfs 路径

```
fs::vfs::posix_lock::init_posix_lock_manager()
fs::initramfs_init()
drivers::init_net_device()
net::config::init()
fs::mount_boot_block_devices()
fs::install_preload_payloads()         [preload_payloads]
```

`fs::initramfs_init()` 创建 RamFS 根、解包 CPIO，并挂载 devfs、procfs、tmpfs 等公共文件系统。块设备挂载发生在 payload 安装之前。

### 5.4 legacy root 路径

```
drivers::init_net_device()
net::config::init()
fs::flush_preload()
fs::mount_tools_disk()
```

root inode 的具体 FS 由 `fs::ROOT_INODE` lazy 初始化决定。块设备可用时按 FAT32/ext4 探测，否则回退 ramfs。

### 5.5 syscall 路径

```
user a7/a0..a5
        |
        v
arch trap handler
        |
        v
syscall::syscall(id, args)
        |
        v
domain sys_xxx()
        |
        v
trap_return()
```

rv64 和 la64 都使用 `a7` 传 syscall id，`a0..a5` 传六个参数。trap 后端在进入分发前将用户 PC 前进 4 字节。`rt_sigreturn` 的 id 为 139，trap 后端不会用普通返回值覆盖 `a0`。

### 5.6 缺页路径

```
trap fault
        |
        v
AddressSpace::do_page_fault(addr, access, instruction)
        |
        +-- find covering VMA
        +-- expand MAP_GROWSDOWN if applicable
        |
        v
page_fault::handle_page_fault()
        |
        +-- lazy anonymous allocation
        +-- file-backed read/write
        +-- shared write
        +-- CoW
```

缺页失败后，trap 后端根据错误类型注入 `SIGSEGV`、`SIGBUS`、`SIGILL`，或设置 `pending_oom_kill`。

### 5.7 timer interrupt 和调度

```
timer interrupt
        |
        v
task::timer_interrupt_handler()
        |
        v
schedule / wake expired / task accounting
```

调度主循环同时执行：

| 维护项 | 行为 |
|--------|------|
| expired timer | 唤醒到期等待者 |
| network poll | 周期性调用 `NET_INTERFACE.try_poll()` |
| FS reclaim | 调用 `fs::reclaim::maybe_reclaim_fs_caches()` |
| zombie queue | 批量回收退出任务 |
| stale zombie cleanup | 周期性清理残留 zombie |
| shared futex compact | 清理共享 futex 表失效项 |

### 5.8 `rust_main()` 逐步解析

`rust_main()` 是理解全局架构的第一条源码路径。它不是普通应用里的 `main()`，而是 OpenSBI/平台入口切到 Rust 后的内核初始化编排器；它把硬件可用性、内存可用性、文件系统可用性和第一个用户进程的可运行性按顺序建立起来。

简化后的代码结构如下：

```rust
pub fn rust_main() -> ! {
    bootstrap_init();
    mem_clear();
    move_to_high_address();          // block_mem feature
    console::log_init();
    trace::init();
    mm::init();
    machine_init();
    task::timer_subsystem_init();

    // initramfs 或 legacy root 分支
    drivers::init_net_device();
    net::config::init();
    fs 初始化或挂载;

    fs::vfs::posix_lock::init_posix_lock_manager();
    task::add_initproc();
    task::run_tasks();
}
```

每一步的含义：

| 步骤 | 为什么在这里执行 |
|------|------------------|
| `bootstrap_init()` | 架构最早期初始化，la64 在这里准备 exception entry、TLB refill、直接映射窗口等低级状态；rv64 当前路径为空实现。 |
| `mem_clear()` | 清理 `.bss`，保证静态变量初值满足 Rust 全局对象假设。 |
| `move_to_high_address()` | `block_mem` feature 下把镜像迁移到高地址，避免 legacy 镜像布局和后续内存使用冲突。 |
| `console::log_init()` / `trace::init()` | 在大多数子系统初始化前建立日志和 trace，后续 panic、syscall、调度统计才能输出。 |
| `mm::init()` | 初始化内核堆、物理页分配器并激活内核页表；从这一步开始，内核可以安全分配 `Box/Arc/Vec` 和物理页。 |
| `machine_init()` | 架构运行期初始化，安装 trap、timer interrupt 和平台相关控制寄存器。 |
| `task::timer_subsystem_init()` | 初始化 kernel timer queue，为 sleep、timeout、POSIX timer、itimer 提供统一时间事件队列。 |
| rootfs 分支 | `initramfs` 分支构造 RamFS、解包 CPIO 并挂载 dev/proc/tmp；legacy 分支走工具盘/预加载路径。 |
| `task::add_initproc()` | 从根文件系统加载 `/init` 或 `/initproc`，构造第一个 `TaskControlBlock` 和 `ProcessControlBlock`。 |
| `task::run_tasks()` | 进入调度循环。进入后不会再回到 `rust_main()`。 |

这个顺序体现了内核依赖链：没有 MM 就不能可靠创建 VFS 对象；没有 rootfs 就不能加载 init ELF；没有 trap/timer 就无法安全返回用户态并执行抢占；没有 init 任务，调度器即使启动也没有用户任务可运行。

### 5.9 一次用户 syscall 的跨层路径

从用户态看，syscall 是 libc 包装的一次函数调用；从内核看，它跨过 HAL、syscall 分发、领域模块和具体资源对象：

```
user a7/a0..a5
  -> arch trap backend
  -> syscall::syscall(id, args)
  -> syscall/fs.rs 或 syscall/process/* 或 net/syscall/*
  -> task/mm/fs/net 对象
  -> 返回 isize，trap 后端写回 a0
```

以 `read(fd, buf, count)` 为例：

1. trap 后端从寄存器取出 syscall id 和 6 个参数。
2. `syscall::syscall()` 记录 perf/trace，检查 seccomp，再进入 `match syscall_id`。
3. `SYSCALL_READ` 分支调用 `sys_read(fd, buf, count)`。
4. `sys_read()` 从当前 task 的 PCB 中取得 fd table，查出 `Arc<dyn File>`；查表失败直接返回负 errno。
5. 函数检查文件是否可读，处理 `/dev/null`、`/dev/zero` 特例，并取得当前用户页表 token。
6. 非阻塞 fd 直接尝试读取；有读等待队列的 fd 使用 `WaitQueue::wait_until_interruptible()`；普通文件直接走 PageCache 读路径。
7. 读取过程中用户 buffer 通过 uaccess/fault-in 路径翻译，错误返回 `EFAULT`，正常返回字节数。

这条路径说明 MangoCore 的分层：syscall 分发层负责把编号送到函数，fd table 属于进程资源，具体 I/O 语义属于 `File` 对象或 socket 对象，用户指针安全属于 MM/uaccess。

### 5.10 一次缺页的跨层路径

缺页是 MM、HAL 和 task 交汇最密集的路径：

```
用户 load/store/execute
  -> arch trap backend 读取 fault addr 和 cause
  -> 当前进程 vm.lock().do_page_fault(addr, access)
  -> VmaSet 查找 VMA，必要时 growdown
  -> page_fault::handle_page_fault()
  -> Vma/FileMap/PageTable 安装或修复映射
  -> 返回用户态重试 fault 指令
```

关键点有三个：

| 关键点 | 说明 |
|--------|------|
| VMA 先于页表 | `AddressSpace::do_page_fault()` 先用 `VmaSet` 判断地址是否属于用户映射；页表中有没有 PTE 不是权限来源。 |
| fault action 分类 | `page_fault.rs` 根据 VMA 类型、访问类型、页表状态和 `VmPageStore` 状态分成 lazy alloc、file read、file shared write、CoW 等动作。 |
| TLB 由页表接口维护 | 映射、取消映射、修改 PTE 权限都经 `PageTable`/HAL 路径，避免架构无关层遗漏 TLB 刷新。 |

读缺页代码时应先读 `address_space.rs::do_page_fault()` 的外壳，再读 `page_fault.rs::classify()`，最后读具体动作落到 `vma.rs` 或 `filemap.rs` 的实现。

### 5.11 调度循环承担的后台职责

`task::run_tasks()` 不只是从 ready queue 取任务。它每轮调度还处理多个必须在内核上下文推进的后台职责：

| 职责 | 代码位置 | 原因 |
|------|----------|------|
| 控制台输入轮询 | `hal::console_getchar()` | 支持 Ctrl+C、trace magic key 和 TTY 读者唤醒。 |
| timeout sweep | `do_wake_expired()` | 唤醒 sleep、futex timeout、poll/select timeout 等等待者。 |
| 网络轮询 | `NET_INTERFACE.try_poll()` | smoltcp 需要周期性 poll 以推进收包、发包和 TCP 状态机。 |
| FS cache 回收 | `fs::reclaim::maybe_reclaim_fs_caches()` | PageCache 写回/回收不由独立内核线程驱动，调度循环提供合作式推进点。 |
| zombie drain | `take_zombie_tasks(64)` | 退出线程切回 idle 后才能安全 drop TCB 和内核栈。 |
| stale zombie cleanup | `cleanup_stale_zombies()` | 防止长期遗留的 zombie task 影响调度队列。 |
| shared futex compact | `compact_shared_futex()` | 控制 shared futex 表中失效 waiters 的积累。 |

这些工作解释了为什么系统空闲时仍然要进入调度循环，而不是停在某个简单的 wait-for-interrupt：文件系统、网络和定时器状态需要在可抢占点被合作式推进。

## 6. 接口与 API

### 6.1 HAL 初始化接口

| API | 调用点 | 说明 |
|-----|--------|------|
| `bootstrap_init()` | `rust_main()` 开始 | 架构早期配置 |
| `machine_init()` | `mm::init()` 后 | trap/timer 等运行期机器初始化 |

### 6.2 trap 接口

| API | 说明 |
|-----|------|
| `trap::init()` | 安装 trap entry |
| `trap_return()` | 信号交付后返回用户态 |
| `get_bad_addr()` | 读取 fault address |
| `get_bad_instruction()` | 读取 fault instruction |
| `get_exception_cause()` | 获取当前 trap 类型 |

### 6.3 调度接口

| API | 说明 |
|-----|------|
| `add_task()` | 加入 ready queue |
| `suspend_current_and_run_next()` | 当前任务让出并重新进入 ready |
| `block_current_and_run_next()` | 当前任务进入 interruptible sleep |
| `exit_current_and_run_next()` | 当前任务进入 zombie 并切换 |
| `run_tasks()` | 调度主循环 |

### 6.4 内存接口

| API | 说明 |
|-----|------|
| `mm::init()` | MM 初始化 |
| `kernel_token()` | 取得内核页表 token |
| `AddressSpace::from_elf()` | 构造用户 ELF 地址空间 |
| `AddressSpace::do_page_fault()` | 用户缺页处理入口 |
| `translated_ref/refmut/byte_buffer` | 用户内存访问入口 |

### 6.5 文件系统和网络入口

| API | 说明 |
|-----|------|
| `fs::initramfs_init()` | initramfs 根文件系统 |
| `fs::mount_boot_block_devices()` | 启动块设备挂载 |
| `fs::mount_tools_disk()` | legacy 工具盘挂载 |
| `drivers::init_net_device()` | 网卡初始化 |
| `net::config::init()` | 网络接口配置 |

## 7. 测试映射

| 测试目标 | 入口/配置 |
|----------|-----------|
| rv64 构建 | `cd os && make rv64-kernel-build-only` |
| la64 构建 | `cd os && make la64-kernel-build-only` |
| rv64 启动 | `cd os && make rv64-run` |
| la64 启动 | `cd os && make la64-run` |
| initramfs 路径 | 启用 `initramfs` feature 的镜像 |
| legacy root 路径 | 未启用 `initramfs` 的镜像 |
| syscall/trap | basic/busybox/LTP 中的 syscall 用例 |
| MM 缺页 | mmap、fork、exec、page fault 相关 LTP |
| 调度/timer | nanosleep、futex timeout、timerfd、cyclictest |

文档修改不改变内核行为，验证以 Markdown/frontmatter/链接扫描为主；功能变更仍需按项目规则执行双架构构建和 QEMU 测试。

## 8. 已知边界

| 边界 | 说明 |
|------|------|
| 单核调度 | `Processor` 和当前任务访问路径按单核模型设计 |
| namespace 对象 | net/mnt/ipc namespace 对象参与 clone/unshare/setns；隔离能力以对应 namespace 类型实现为准 |
| initramfs POSIX lock 初始化 | initramfs 分支内和分支后统一路径都会调用 POSIX lock manager 初始化入口 |
| `rt_sigreturn` | trap 后端特殊处理，不走普通 `a0` 覆盖 |
| TLB | PTE 修改后必须经页表/HAL 路径刷新 TLB |

## 9. 源文件索引

| 路径 | 内容 |
|------|------|
| `os/src/main.rs` | 根模块声明、编译特性、`rust_main()` |
| `os/src/hal/mod.rs` | HAL 公共导出 |
| `os/src/hal/arch/riscv/` | RISC-V 后端 |
| `os/src/hal/arch/loongarch64/` | LoongArch64 后端 |
| `os/src/mm/mod.rs` | MM 初始化和模块边界 |
| `os/src/mm/kernel_space.rs` | 内核地址空间 |
| `os/src/syscall/mod.rs` | syscall 名称、seccomp、分发和记录 |
| `os/src/task/mod.rs` | task 子系统导出和调度辅助 |
| `os/src/task/manager.rs` | TaskManager、WaitQueue、timer queue |
| `os/src/task/processor.rs` | `run_tasks()` 和当前任务 |
| `os/src/fs/mod.rs` | rootfs、initramfs、挂载、预加载 |
| `os/src/drivers/mod.rs` | driver 模块入口 |
| `os/src/net/mod.rs` | 网络模块入口 |
