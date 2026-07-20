---
title: "内核模块地图 (Kernel Module Map)"
category: architecture
status: stable
author: MangoCore Team
last_update: 2026-07-20
tags: [architecture, modules, kernel]
---

# 内核模块地图

## 1. 概述

`os/src/main.rs` 是 MangoCore 的架构无关入口，也是根模块边界的来源。该文件声明 `console`、`drivers`、`fs`、`hal`、`mm`、`net`、`syscall`、`task`、`timer`、`trace`、`utils` 等模块，并在 `rust_main()` 中串联启动阶段。模块地图以这组根模块为准，不把目录名、历史文档或计划中的组件当作已经接入的运行路径。

核心依赖关系可以概括为：

```
main.rs::rust_main()
  ├── hal::{bootstrap_init, machine_init}
  ├── console::log_init()
  ├── trace::init()
  ├── mm::init()
  ├── drivers::init_net_device()
  ├── net::config::init()
  ├── fs::{initramfs_init, flush_preload, mount_*}
  └── task::{timer_subsystem_init, add_initproc, run_tasks}
```

MangoCore 的组织方式接近“硬件后端集中、内核服务分层、syscall 扁平分发”：HAL 隔离架构差异，MM/FS/net/task 持有内核状态，syscall 层只完成 ABI 参数到领域函数的转接。

## 2. 根模块总表

| 根模块 | 入口路径 | 运行期职责 |
|--------|----------|------------|
| `console` | `os/src/console.rs` | 日志初始化、`print!`/`println!` 输出、字符 I/O 的上层封装 |
| `drivers` | `os/src/drivers/mod.rs` | 设备驱动模块入口；当前启动主线显式调用 `drivers::init_net_device()` |
| `fs` | `os/src/fs/mod.rs` | VFS、MountFS、具体文件系统、dev/proc/tmp 挂载、预加载 payload、块设备挂载 |
| `hal` | `os/src/hal/mod.rs` | 架构后端导出；页表、trap、timer、TLB、上下文切换、console、shutdown |
| `lang_items` | `os/src/lang_items.rs.rv` / `os/src/lang_items.rs.la` | 裸机 Rust 所需 lang items；`main.rs` 以 `target_arch` 和 `#[path]` 在编译期选择变体 |
| `math` | `os/src/math/` | 内核数学辅助实现 |
| `mm` | `os/src/mm/mod.rs` | 堆、物理页、内核地址空间、用户地址空间、VMA、mmap、缺页、uaccess |
| `net` | `os/src/net/mod.rs` | Socket trait、TCP/UDP/RAW/Unix/Netlink/Packet、smoltcp 接入、网络 syscall |
| `panic_diag` | `os/src/panic_diag.rs` | panic 时的诊断输出 |
| `syscall` | `os/src/syscall/mod.rs` | syscall id/name、seccomp 检查、分发、errno、用户内存辅助 |
| `task` | `os/src/task/mod.rs` | TCB/PCB、调度器、信号、futex、IPC、timer、进程生命周期 |
| `timer` | `os/src/timer.rs` | `TimeSpec`/`TimeVal` 等时间结构和换算辅助 |
| `trace` | `os/src/trace.rs` | trace 事件初始化和记录 |
| `utils` | `os/src/utils/` | 错误类型、路径、位图、统计等通用工具 |

根模块声明的顺序本身不是依赖顺序；真正的运行顺序由 `rust_main()` 决定。比如 `net` 模块存在于根命名空间中，但网络设备初始化发生在 `mm::init()` 与 `machine_init()` 完成之后。

## 3. 启动主线中的模块位置

`rust_main()` 的主干按以下顺序调用模块入口：

| 阶段 | 调用 | 所属模块 | 作用 |
|------|------|----------|------|
| 架构早期 | `bootstrap_init()` | `hal` | rv64 为空实现；la64 配置异常入口、TLB refill、DMW、page walk 等机器状态 |
| 内存清理 | `mem_clear()` | `main.rs` | 清零 `.bss`；`zero_init` 下清到 `MEMORY_END` |
| 块内存镜像 | `move_to_high_address()` | `main.rs` | `block_mem` 下把内嵌镜像复制到 `DISK_IMAGE_BASE` |
| 日志 | `console::log_init()` | `console` | 初始化日志输出 |
| trace | `trace::init()` | `trace` | 初始化 trace 事件基础设施 |
| 内存 | `mm::init()` | `mm` | 初始化 heap、frame allocator，并激活内核地址空间 |
| 机器运行期 | `machine_init()` | `hal` | 安装 trap，启用 timer interrupt；la64 还初始化 timer frequency |
| timer | `task::timer_subsystem_init()` | `task` | 初始化任务层 timer/wakeup 结构 |
| 根文件系统 | `fs::initramfs_init()` 或 `fs::flush_preload()`/`fs::mount_tools_disk()` | `fs` | 根据 `initramfs` feature 选择启动路径 |
| 网络设备 | `drivers::init_net_device()` | `drivers` | 初始化网络设备 |
| 网络配置 | `net::config::init()` | `net` | 初始化网络接口和协议栈配置 |
| POSIX lock | `fs::vfs::posix_lock::init_posix_lock_manager()` | `fs` | 初始化 VFS POSIX 文件锁管理器 |
| init 任务 | `task::add_initproc()` | `task` | 将 `INITPROC` 加入 ready queue |
| 调度 | `task::run_tasks()` | `task` | 进入单核调度主循环 |

initramfs 分支中 POSIX lock manager 在 `fs::initramfs_init()` 前调用一次，分支外统一路径又调用一次。这一顺序直接来自 `main.rs::rust_main()`；是否幂等由对应实现保证。

## 4. 依赖方向

### 4.1 总体依赖图

```
                 +-------------------+
                 |       hal         |
                 | trap/timer/TLB/PT |
                 +---------^---------+
                           |
          +----------------+----------------+
          |                                 |
    +-----+-----+                     +-----+-----+
    |    mm     |                     |   task    |
    | VM/VMA/PT |                     | TCB/PCB   |
    +-----^-----+                     +-----^-----+
          |                                 |
          |                                 |
    +-----+-----+                     +-----+-----+
    |     fs    |<------------------->|  syscall  |
    | VFS/cache |                     | dispatch  |
    +-----^-----+                     +-----+-----+
          |                                 |
          v                                 v
    +-----------+                     +-----------+
    |  drivers  |                     |    net    |
    | block/net |                     | sockets   |
    +-----------+                     +-----------+
```

### 4.2 关键依赖解释

| 依赖 | 代码依据 | 说明 |
|------|----------|------|
| `mm -> hal` | `hal::PageTableImpl`, `hal::tlb_invalidate*` | 上层 MM 通过类型别名选择 rv64 `Sv39PageTable` 或 la64 `LAFlexPageTable` |
| `task -> hal` | `TrapContext`, `KernelStack`, `__switch` | 任务切换和返回用户态依赖架构上下文与内核栈 |
| `syscall -> task/mm/fs/net` | `syscall/mod.rs` 的 `match syscall_id` | 分发层按 id 调用文件、进程、内存、网络等领域函数 |
| `fs -> drivers` | rootfs 挂载、块设备探测 | ext4/FAT32 等实际介质来自块设备层；devfs 暴露设备文件 |
| `net -> drivers` | `drivers::init_net_device()` 后 `net::config::init()` | 网络协议栈建立在 virtio/veth 等网卡设备之上 |
| `task -> mm/fs` | `ProcessControlBlock` 的 `vm`、`files`、`fs` 字段 | 进程拥有地址空间、fd table、cwd/root 与 namespace 状态 |
| `trap -> syscall/mm/task` | 两套 `trap/mod.rs` | syscall 进入 `syscall::syscall()`；缺页进入 `AddressSpace::do_page_fault()`；timer 进入任务调度 |

依赖方向并非完全无环。例如 task 需要 mm 的地址空间，mm 缺页错误又通过 trap/task 注入信号。工程上通过短锁区间、`Arc` 克隆和领域入口函数控制互相调用的粒度。

## 5. 子系统内部地图

### 5.1 HAL

```
hal/
├── mod.rs                         # 公共导出和 IO_CHUNK_SIZE/MAX_RW_COUNT
├── arch/
│   ├── mod.rs                     # 按 feature 选择 riscv 或 loongarch64
│   ├── riscv/                     # Sv39、SBI、trap、switch、time
│   └── loongarch64/               # LAFlex、TLB/ASID、register、trap、time
└── platform/                      # 具体板级常量
```

`hal/mod.rs` 当前导出的公共接口包括：

| 类别 | 导出项 |
|------|--------|
| 上下文切换 | `__switch` |
| 配置/栈 | `config`, `kstack_alloc`, `trap_cx_bottom_from_tid`, `ustack_bottom_from_tid` |
| 启动 | `bootstrap_init`, `machine_init` |
| console | `console_flush`, `console_getchar`, `console_putchar` |
| 中断 | `local_irq_save`, `local_irq_restore` |
| trap 查询 | `get_bad_addr`, `get_bad_instruction`, `get_exception_cause` |
| 时间 | `get_clock_freq`, `get_time`, `program_timer_delta` |
| trap 出入口 | `trap_handler`, `trap_return` |
| 类型 | `KernelPageTableImpl`, `PageTableImpl`, `TrapContext`, `MachineContext`, `UserContext`, `UserSignalMask`, `TrapImpl` |
| 常量 | `BLOCK_SZ`, `BUFFER_CACHE_NUM`, `KERNEL_HEAP_SIZE`, `MEMORY_END`, `MMIO`, `TICKS_PER_SEC` |

`IO_CHUNK_SIZE` 和 `MAX_RW_COUNT` 也定义在 `hal/mod.rs`。`IO_CHUNK_SIZE` 取 `KERNEL_HEAP_SIZE / 128`，并限制在 64 KiB 到 256 KiB；`MAX_RW_COUNT` 是 `i32::MAX` 按页大小向下对齐。

### 5.2 MM

```
mm/
├── mod.rs                         # mm::init() 和公共导出
├── kernel_space.rs                # 内核页表映射
├── frame_allocator.rs             # 物理页分配
├── frame_store.rs                 # VMA 页状态
├── address_space.rs               # 用户地址空间
├── vma.rs / vma_set.rs            # VMA 与区间管理
├── mmap.rs                        # mmap/brk 实现
├── page_fault.rs                  # 缺页动作分类
├── filemap.rs                     # 文件映射 fault
└── uaccess.rs                     # 用户内存访问
```

MM 初始化只在 `rust_main()` 中显式调用一次。用户地址空间的创建和替换由 task/process 路径触发：init 进程、fork/clone、execve、mmap/brk 和缺页处理都会进入 MM 子模块。

### 5.3 Task/Process

```
task/
├── mod.rs                         # INITPROC、add_task、调度辅助导出
├── task.rs                        # TaskControlBlock
├── process.rs                     # ProcessControlBlock
├── processor.rs                   # run_tasks() 和当前任务
├── manager.rs                     # ready/interruptible/zombie 队列
├── signal/                        # 信号动作、投递、frame、pending、wait
├── threads.rs                     # 线程组、clear_child_tid、futex table
├── ipc_namespace.rs               # IPC namespace 状态
├── sleep.rs                       # sleep 阻塞辅助
└── completion.rs                  # Completion 一次性通知原语
```

调度主循环由 `task::run_tasks()` 启动。timer interrupt 到来时，trap 后端调用 `task::timer_interrupt_handler()`，其实现与 timeout 队列维护位于 `task/manager.rs`；futex syscall 位于 `syscall/process/futex.rs`，进程级 futex table 位于 `task/threads.rs`。

### 5.4 Syscall

```
syscall/
├── mod.rs                         # syscall_name + syscall() 扁平分发
├── syscall_id.rs                  # syscall 编号常量
├── errno.rs                       # 负 errno 常量
├── fs.rs                          # 文件 I/O syscall
├── uaccess.rs                     # syscall 层用户内存辅助
└── process/                       # 生命周期、mm、ids、signal、time、ipc、exec
```

网络 syscall 在 `net/syscall/` 下实现，但由 `syscall/mod.rs` 统一注册。trap 后端不直接调用各领域 syscall；它只调用 `syscall::syscall(id, args)`。

### 5.5 FS 与 Net

`fs` 的模块数量较多，启动主线关心的入口主要是：

| 入口 | 说明 |
|------|------|
| `fs::initramfs_init()` | 创建 initramfs 根、解包 CPIO 并挂载公共伪文件系统 |
| `fs::mount_boot_block_devices()` | initramfs 路径下探测并挂载启动块设备 |
| `fs::flush_preload()` | legacy 路径处理预加载内容 |
| `fs::mount_tools_disk()` | legacy 路径挂载工具盘 |

`net` 的启动主线分两步：driver 层先初始化网卡设备，随后 `net::config::init()` 建立网络接口与协议栈配置。socket 的详细结构由 `docs/06_net/` 对应专题维护。

## 6. 编译特性对模块图的影响

| feature | 影响 |
|---------|------|
| `riscv` | `main.rs` 引入 `hal/arch/riscv/entry.asm`，`hal/arch/mod.rs` 选择 RISC-V 后端 |
| `loongarch64` | `hal/arch/mod.rs` 选择 LoongArch64 后端；la64 entry 在当前 `main.rs` 中保留注释，不由该文件直接引入 |
| `initramfs` | 引入 `initramfs-rv.S` 或 `initramfs-la.S`，启动时走 `fs::initramfs_init()` 分支 |
| `block_mem` | 引入 `load_img-rv.S` 或 `load_img.S`，启动时调用 `move_to_high_address()` |
| `preload_payloads` | 引入 `preload_app-rv.S` 或 `preload_app.S`，initramfs 分支可调用 `fs::install_preload_payloads()` |
| `heap_trace` | `mm::init()` 中调用 `heap_trace::enable()` |
| `oom_handler` | frame allocator 的分配失败路径启用 OOM 回收和 active task 追踪 |

这些 feature 均在源码条件编译中出现。文档中不把未启用 feature 的分支描述为运行时动态切换能力。

## 7. 常见定位路径

| 问题 | 首个源码入口 | 后续定位 |
|------|--------------|----------|
| 启动卡在进入内核前 | `hal/arch/*/entry.asm` | linker script、OpenSBI/QEMU 启动参数 |
| `rust_main()` 打印后卡住 | `os/src/main.rs` | 对照初始化阶段最后一个输出点 |
| syscall 返回 `ENOSYS` | `os/src/syscall/mod.rs` | `syscall_id.rs` 与 match 分支是否一致 |
| 用户缺页后 SIGSEGV | `hal/arch/*/trap/mod.rs` | `mm/address_space.rs`、`mm/page_fault.rs` |
| timer 不触发调度 | `hal/arch/*/time.rs` | `task::timer_subsystem_init()`、`task::timer_interrupt_handler()` |
| init 进程未运行 | `task::add_initproc()` | `TaskControlBlock::new()`、ELF 路径 `/init` 与 `/initproc` |
| 网络 socket 无进展 | `net::config::init()` | `drivers::init_net_device()`、调度循环中的 `NET_INTERFACE.try_poll()` |

定位跨模块问题时可以先判断“事件从哪里进入内核”。syscall 类事件从 trap 后端进入 `syscall/mod.rs`；缺页类事件从 trap 后端进入 `mm/address_space.rs`；定时类事件从 HAL timer 进入 task timer；设备 I/O 事件通常从 driver 初始化和调度循环 poll 进入 VFS 或 net。确定入口后，再沿对象所有权查下去：当前线程是 TCB，进程资源是 PCB，虚拟内存是 `AddressSpace`，文件描述符最终落到 `Arc<dyn File>`，网络 fd 落到 socket 对象。

这张模块图的使用方式不是背目录，而是给源码跳转定方向。例如 `mmap` 失败时先看 `syscall/process/mm.rs` 的参数和 errno，再看 `mm/mmap.rs` 的 VMA 创建；`fork` 后写页异常时先看 `task/task.rs::sys_clone()` 是否调用 VM 复制，再看 `mm/vma.rs` 的 CoW；socket 读阻塞时先看 `net/syscall`，再看调度循环中的 `NET_INTERFACE.try_poll()` 是否推进协议栈。

## 8. 测试映射

| 验证目标 | 覆盖模块 | 推荐入口 |
|----------|----------|----------|
| 双架构能编译 | `hal`, `mm`, `task`, `syscall` | `cd os && make rv64-kernel-build-only`；`cd os && make la64-kernel-build-only` |
| 启动主线 | `main`, `hal`, `mm`, `fs`, `task` | `cd os && make rv64-run`；`cd os && make la64-run` |
| syscall 分发 | `trap`, `syscall` | basic、busybox、LTP syscall 用例 |
| 缺页路径 | `trap`, `mm` | mmap、fork、exec、page fault 相关 LTP |
| 调度路径 | `task`, `timer`, `hal` | nanosleep、futex timeout、wait、cyclictest |
| FS 初始化 | `fs`, `drivers` | basic/busybox 下的根目录、`/dev`、`/proc` 访问 |
| 网络初始化 | `drivers`, `net`, `task` | busybox 网络工具、iperf/netperf 或 `docs/06_net/test-map.md` |

## 9. 源文件索引

| 路径 | 内容 |
|------|------|
| `os/src/main.rs` | 根模块声明、feature 汇编段、`rust_main()` |
| `os/src/hal/mod.rs` | HAL 公共导出、I/O 分块常量 |
| `os/src/hal/arch/mod.rs` | 按架构 feature 选择后端 |
| `os/src/hal/arch/riscv/mod.rs` | RISC-V 后端模块和类型别名 |
| `os/src/hal/arch/loongarch64/mod.rs` | LoongArch64 后端模块和机器初始化 |
| `os/src/mm/mod.rs` | MM 初始化和公共导出 |
| `os/src/task/mod.rs` | task/process 子系统导出、`INITPROC`、`add_task` |
| `os/src/task/processor.rs` | 调度主循环 |
| `os/src/syscall/mod.rs` | syscall 名称、seccomp、分发、统计 |
| `os/src/fs/mod.rs` | 根文件系统、initramfs、挂载和预加载入口 |
| `os/src/net/mod.rs` | 网络模块根与 socket 分配入口 |
| `os/src/drivers/mod.rs` | driver 模块入口 |
