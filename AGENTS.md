# oskernel2026-mango (NPUCore Project Aspera) — AI 开发助手指令

## 项目简介

oskernel2026-mango 是一个**基于 Rust 的裸机 OS 内核**，支持 **riscv64** 和 **loongarch64** 双架构。它是 `#![no_std]` 的，通过 `opensbi` 引导直接在 QEMU 上运行，**不是一个用户态应用程序**。

| 属性   | 值                                                                                                              |
| ------ | --------------------------------------------------------------------------------------------------------------- |
| 语言   | Rust（nightly，双工具链）                                                                                       |
| 架构   | `riscv64gc-unknown-none-elf`、`loongarch64-unknown-linux-gnu`                                                   |
| 引导   | `opensbi`（RISC-V SBI）+ 自定义入口                                                                             |
| 测试   | 内置 initproc 读取 `os_test.conf`，运行分组测试（musl/glibc）                                                   |
| 功能   | ext4/fat32 文件系统、smoltcp TCP/UDP/RAW 网络、virtio 块/网卡、多任务、SV39 虚拟内存、zram 交换、POSIX 系统调用 |
| 代码量 | ~35,000+ 行（仅内核，不含 vendor/依赖）                                                                         |
## 设计思想

- Linux兼容性：系统调用接口/procfs/sysfs/devfs等的行为应当符合Linux语义。参考Linux 6.6的行为进行实现。
- 轻量：简化复杂的抽象设计，保留合理的、简洁、符合Rust开发最佳实践的的抽象，提升系统性能。
- 安全：注重内存安全、并发安全

---

## 目录

- [编译与验证](#编译与验证)
- [强制要求：QEMU 集成测试](#强制要求qemu-集成测试)
- [架构详解](#架构详解)
  - [双架构构建系统](#双架构构建系统)
  - [模块地图](#模块地图)
  - [启动流程](#启动流程)
  - [系统调用分发](#系统调用分发)
  - [网络栈架构](#网络栈架构)
  - [内存管理](#内存管理)
  - [任务/进程模型](#任务进程模型)
  - [I/O 阻塞抽象（wait_io / wait_io_core）](#io-阻塞抽象wait_io--wait_io_core)
- [编码规范](#编码规范)
- [常见踩坑](#常见踩坑)
- [新增功能](#新增功能)
- [调试与性能分析](#调试与性能分析)
- [更新本文档](#更新本文档)

---

## 编译与验证

> 详细运行步骤见 `how-to-run.md`。

### 1) 编译内核

```bash
# 完整编译（内核 + 文件系统镜像，双架构）
make env              # 设置 nightly Rust 工具链
make all              # 编译 rv64 + la64

# 仅编译内核（最快，不含镜像），推荐日常迭代开发
cd os && make rv64-kernel-build-only
cd os && make la64-kernel-build-only

# 单架构完整编译
cd os && make rv64-only
cd os && make la64-only
```

### 2) 在 Docker 中编译（推荐）

```bash
make docker           # 拉取并进入容器
make env
make all
```

### 3) 编译检查（无 `cargo test` 或 `clippy`）

这是一个裸机内核——**不支持** `cargo test` / `cargo clippy`。唯一可用的编译期验证是：

```bash
# 提交前务必验证两个架构
make rv64-kernel-build-only
make la64-kernel-build-only
```

### 4) 修改测试配置并重新注入

```bash
# 编辑 os_test.conf，然后注入到目标镜像
make -C os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt CONF_FILE=../os_test.conf
make -C os conf-inject CONF_ARCH=la64 CONF_BLK_MODE=mem  CONF_FILE=../os_test.conf
```

---

## 强制要求：QEMU 集成测试

**永远不要只依赖编译期验证。** 每次修改核心功能后，必须在 QEMU 中运行集成测试。

### 聚焦测试：按需运行特定组（推荐）

`os_test.conf` 的 `mask` 字段用 12-bit 控制哪些测试组运行。**不要一口气跑全部 12 组**——LTP、lmbench、unixbench 等组件未必全过，只需跑你改动的功能对应的组即可。

```bash
# os_test.conf 中修改 mask 值
# 12-bit 测试组掩码（bit0-bit11）：
# bit0=basic    bit1=busybox   bit2=lua       bit3=libctest
# bit4=iozone   bit5=unixbench bit6=iperf     bit7=libcbench
# bit8=lmbench  bit9=netperf   bit10=cyclictest bit11=ltp

# 示例：
mask=0x001    # 只跑 basic（基础功能测试）
mask=0x002    # 只跑 busybox
mask=0x200    # 只跑 netperf（网络性能测试）
mask=0x040    # 只跑 iperf
mask=0x103    # basic + busybox + lmbench
mask=0xFFF    # 跑全部 12 组（不推荐日常使用）
```

修改 `os_test.conf` 后需要重新注入镜像：
```bash
make -C os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt CONF_FILE=../os_test.conf
```

### 快速运行当前 mask

```bash
cd os && make rv64-run
cd os && make la64-run

# 分组批量运行（项目根目录，会按分组顺序依次跑）
TEST_ARCH=rv64 bash run_test.sh
```

### 详细日志

```bash
cd os && LOG=info make rv64-run
```

`LOG` 环境变量设置内核日志级别，可选值：`off`、`error`、`warn`、`info`、`debug`、`trace`。

### 测试架构

内核启动后运行 `initproc` ELF，其行为如下：
1. 读取 `/os_test.conf` 获取测试组定义和 mask
2. 根据 mask 过滤需要运行的组，跳过未启用的组
3. 对每个启用的测试组 fork 并 exec 测试二进制（bash 脚本 → 对应测试程序）
4. 根据 musl 和 glibc 两套测试套件的退出码，报告每组 pass/fail

### 问题排查速查表

| 现象                                 | 可能原因                                       | 排查方法                                         |
| ------------------------------------ | ---------------------------------------------- | ------------------------------------------------ |
| 启动卡死（"Hello, world!" 后无输出） | 链接脚本 / `.text` 段地址错误                  | 检查 `linker.ld` 的 `BASE_ADDRESS`               |
| SBI 初始化后卡死                     | 页表或早期陷阱设置问题                         | 检查 `boot.S` / `entry.asm` / `trap/mod.rs`      |
| 进程启动后在某个 syscall 上卡住      | socket 层缺少 `poll()`                         | 开启 `LOG=info`，搜索 "looping..."               |
| `Unexpected trap` 或页错误           | syscall 参数错误 / 内存翻译错误                | 检查 `translated_refmut` 使用                    |
| syscall 返回错误 errno               | 返回值类型不匹配（usize vs isize）             | `File::read` 返回 usize，注意符号处理            |
| `pselect` 永不返回                   | `socket_r_ready()` 缺少 `NET_INTERFACE.poll()` | 在检查就绪前加 `NET_INTERFACE.poll()`            |
| `connect` 永远不完成                 | TCP 握手重试耗尽                               | 检查 `TcpSocket::try_connect` 的 closed 状态重连 |
| QEMU 三重错误（无输出）              | 陷阱处理器未处理异常                           | 用 `-d int` 运行或检查 `stvec`                   |

---

## 架构详解

### 双架构构建系统

项目使用**两套独立的 nightly Rust 工具链**：

| 架构        | nightly 版本         | Rust 目标三元组                 | 内核二进制  |
| ----------- | -------------------- | ------------------------------- | ----------- |
| riscv64     | `nightly-2025-01-18` | `riscv64gc-unknown-none-elf`    | `kernel-rv` |
| loongarch64 | `nightly-2024-05-01` | `loongarch64-unknown-linux-gnu` | `kernel-la` |

**注意**：运行任何 make 目标时，都会**切换当前的 `rustup override`**。连续运行 `make rv64-kernel-build-only` 和 `make la64-kernel-build-only` 会失败，因为工具链会在中间被切换。请分开运行。

**语言项**（`lang_items.rs`、`alloc_error_handler`、`panic_handler`）也是架构相关的。Makefile 会自动复制 `.rv` / `.la` 变体：

```bash
cp ./src/lang_items.rs.rv  ./src/lang_items.rs   # rv64 用
cp ./src/lang_items.rs.la  ./src/lang_items.rs   # la64 用
```

永远不要直接修改 `lang_items.rs`——修改 `lang_items.rs.rv` 或 `lang_items.rs.la` 替代。

**Cargo features 编译变体：**

| Feature                               | 用途                             |
| ------------------------------------- | -------------------------------- |
| `board_rvqemu`                        | RISC-V QEMU virt 开发板          |
| `board_2k1000`                        | LoongArch 2k1000 开发板          |
| `block_virt`                          | virtio-blk                       |
| `block_mem`                           | 内存盘块设备（内嵌文件系统镜像） |
| `block_sata`                          | SATA/AHCI 块设备                 |
| `log_off/error/warn/info/debug/trace` | 日志级别                         |
| `oom_handler`                         | OOM 处理                         |
| `zero_init`                           | 启动时清零 BSS                   |
| `swap`                                | 交换/zram 支持                   |
| `riscv` / `loongarch64`               | 架构标志                         |
| `comp`                                | 启用编译器测试套件               |
| `zram`                                | 压缩内存块设备                   |

### 模块地图

```
os/src/
│
├── main.rs                    # 入口：#![no_std] #![no_main]，mem_clear、初始化链
│
├── syscall/                   # 系统调用分发与实现
│   ├── mod.rs                 #   syscall() 分发，~100+ 个 match 分支
│   ├── syscall_id.rs          #   系统调用号常量（Linux 兼容）
│   ├── syscall_macro.rs       #   trans_ref!、get_socket! 宏
│   ├── errno.rs               #   Linux errno 常量 + Errno 枚举
│   ├── fs.rs                  #   read/write/open/close/dup/ioctl/...（25+ 个）
│   ├── net.rs                 #   socket/bind/connect/sendto/recvfrom/...（15+ 个）
│   ├── process.rs             #   clone/execve/exit/wait4/signal/...（30+ 个）
│   └── utils.rs               #   wait_io / wait_io_core —— 阻塞 I/O 循环
│
├── fs/                        # 文件系统
│   ├── layout.rs              #   Stat、OpenFlags、SeekWhence、StatMode
│   ├── file_trait.rs          #   File trait（read/write/r_ready/lseek/...）
│   ├── file_descriptor.rs     #   FileDescriptor 包装（cloexec、nonblock）
│   ├── dirent.rs              #   目录项结构体
│   ├── directory_tree.rs      #   虚拟文件系统树（VFS 层）
│   ├── ext4/                  #   ext4 文件系统实现
│   ├── fat32/                 #   FAT32 文件系统 + DiskInode
│   ├── cache.rs               #   PageCache（块设备缓存层）
│   ├── inode.rs               #   Inode 抽象
│   ├── poll.rs                #   pselect/ppoll/select 实现
│   ├── vfs.rs                 #   Mount/umount/statfs
│   ├── filesystem.rs          #   文件系统 trait
│   ├── timestamp.rs           #   文件时间戳更新
│   └── dev/                   #   设备文件
│       ├── tty.rs             #     控制台/TTY（stdin/stdout/stderr）
│       ├── null.rs            #     /dev/null
│       ├── zero.rs            #     /dev/zero
│       ├── urandom.rs         #     /dev/urandom
│       ├── pipe.rs            #     管道（被 UnixSocket 使用）
│       ├── hwclock.rs         #     /dev/hwclock（用于 adjtimex）
│       └── socket.rs          #     后备 socket 文件（已被 net/ 替代）
│
├── net/                       # 网络栈（smoltcp 包装）
│   ├── mod.rs                 #   Socket trait、SocketTable、alloc()
│   ├── macros.rs              #   impl_file_for_socket! 宏
│   ├── tcp.rs                 #   TcpSocket（listen/connect/accept + try_recv/try_send）
│   ├── udp.rs                 #   UdpSocket（connect + rx_queue 分发）
│   ├── raw.rs                 #   RawSocket（IPv4 原始 socket）
│   ├── unix.rs                #   UnixSocket（基于管道的 unix 域 socket 对）
│   ├── address.rs             #   SocketAddrv4、endpoint 解析、IP 地址辅助
│   ├── config.rs              #   NET_INTERFACE 单例、smoltcp 初始化、poll 循环
│   └── adapter.rs             #   SmoltcpDeviceAdapter（NetworkDevice → smoltcp）
│
├── mm/                        # 内存管理
│   ├── mod.rs                 #   公开 re-export、init()
│   ├── address.rs             #   VirtAddr、PhysAddr、VirtPageNum、PhysPageNum
│   ├── page_table.rs          #   PageTable、UserBuffer、translated_* 函数
│   ├── memory_set.rs          #   MemorySet（进程内存布局）、KERNEL_SPACE
│   ├── map_area.rs            #   MapArea、MapPermission、Flags
│   ├── frame_allocator.rs     #   物理帧分配器（基于栈）
│   ├── heap_allocator.rs      #   内核堆分配器（buddy_system_allocator）
│   └── zram.rs                #   压缩内存块设备
│
├── task/                      # 进程/线程管理
│   ├── mod.rs                 #   公开 re-export、suspend_current_and_run_next()
│   ├── task.rs                #   TaskControlBlock（PCB，含文件/信号/调度信息）
│   ├── context.rs             #   TaskContext（上下文切换寄存器保存区）
│   ├── manager.rs             #   TaskManager（就绪队列 + 定时器队列）
│   ├── processor.rs           #   Processor（目前单核）、schedule()
│   ├── elf.rs                 #   ELF 加载器（用户态二进制）
│   ├── pid.rs                 #   PID 分配器、用户栈分配
│   ├── threads.rs             #   Clone 标志、线程创建
│   └── signal.rs              #   信号处理（Signals、sigaction、sigprocmask）
│
├── hal/                       # 硬件抽象层
│   ├── mod.rs                 #   Trait 定义（boot、mmio、timer、context switch）
│   └── arch/
│       ├── riscv/             #   RISC-V 64 实现
│       │   ├── mod.rs         #     HalImpl
│       │   ├── entry.asm      #     启动入口（global_asm!）
│       │   ├── linker.ld      #     链接脚本（BASE_ADDRESS = 0x80200000）
│       │   ├── sbi.rs         #     SBI 调用（ecall）
│       │   ├── sv39.rs        #     SV39 页表实现
│       │   ├── switch.S       #     __switch（上下文切换汇编）
│       │   ├── switch.rs      #     __switch 安全封装
│       │   ├── time.rs        #     通过 SBI 的定时器
│       │   ├── config.rs      #     内存映射、CLINT/PLIC 地址
│       │   ├── kern_stack.rs  #     每 CPU 内核栈分配
│       │   └── trap/          #     陷阱处理
│       │       ├── mod.rs     #       陷阱处理器（stvec）
│       │       ├── context.rs #       TrapContext 布局
│       │       └── trap.S     #       汇编陷阱向量
│       │
│       └── loongarch64/       # LoongArch 64 实现
│           ├── mod.rs         #     HalImpl
│           ├── entry.asm      #     启动入口
│           ├── linker.ld      #     链接脚本
│           ├── boot.rs        #     早期启动初始化
│           ├── tlb.rs         #     TLB 管理
│           ├── switch.S       #     上下文切换（汇编）
│           ├── switch.rs      #     安全封装
│           ├── trap/          #     陷阱处理
│           ├── sbi.rs         #     SBI 类固件调用
│           ├── time.rs        #     定时器
│           ├── config.rs      #     内存映射
│           ├── kern_stack.rs  #     栈分配
│           └── register/      #     CSR 寄存器定义
│
├── drivers/                   # 设备驱动
│   ├── mod.rs                 #   全局 NetDevice/BlockDevice 单例
│   ├── block/                 #   块设备驱动
│   │   ├── mod.rs             #     BlockDevice trait
│   │   ├── virtio_blk.rs      #     virtio-blk（MMIO）
│   │   └── virtio_blk_pci.rs  #     virtio-blk（PCI）[la64]
│   ├── net/                   #   网络设备驱动
│   │   ├── mod.rs             #     NetworkDevice trait
│   │   └── virtio_net.rs      #     virtio-net（MMIO）
│   └── serial/                #   UART/串口
│       ├── mod.rs             #     Serial trait
│       └── ns16550a.rs        #     NS16550A UART
│
├── console.rs                 # print!/println! 宏（通过串口输出）
├── timer.rs                   # TimeSpec（纳秒精度、CLOCK_REALTIME）
├── lang_items.rs              # panic_handler、oom_handler、eh_personality
│                              # （架构相关：.rv / .la 变体）
├── math/                      # 数学工具函数
└── utils/                     # 通用工具
    ├── error.rs               #   SyscallErr 枚举、SyscallRet、GeneralRet
    └── random.rs              #   RNG（弱伪随机数）
```

### 启动流程

```
QEMU → OpenSBI（M 模式）→ entry.asm（S 模式）
  |
  ├── 设置页表（RISC-V 的 SV39）
  ├── 清零 BSS
  ├── 设置栈指针
  ├── 跳转到 rust_main()
  │
  ├── rust_main():
  │   ├── console::init()         — UART/串口
  │   ├── timer::init()           — 定时器驱动
  │   ├── mm::init()              — 堆 + 物理帧分配器
  │   ├── drivers::init()         — virtio（块 + 网卡）
  │   ├── fs::init()              — 挂载根文件系统（ext4/fat32）
  │   ├── net::init()             — 初始化 smoltcp 接口
  │   ├── task::init()            — 创建 initproc 任务
  │   │   └── 从根文件系统加载 initproc ELF
  │   ├── task::run_tasks()       — 进入调度器（永不返回）
  │
  └── 调度器选中 initproc → 运行用户测试
```

### 系统调用分发

syscall 分发函数 `syscall(id, args)` 位于 `syscall/mod.rs`。它是一个扁平的 `match`，约有 100+ 个分支，每个分支将 `SYSCALL_XXX` 常量映射到对应的处理函数。

**主要 syscall 分组**（完整列表见 `syscall_id.rs`）：

| 分组     | Syscall ID                                                                                               | 模块                                 |
| -------- | -------------------------------------------------------------------------------------------------------- | ------------------------------------ |
| 文件 I/O | `read(63)`、`write(64)`、`openat(56)`、`close(57)`、`lseek(62)`                                          | `syscall/fs.rs`                      |
| 网络     | `socket(198)`、`bind(200)`、`connect(203)`、`sendto(206)`、`recvfrom(207)`、`accept(202)`、`listen(201)` | `syscall/net.rs`                     |
| 进程     | `clone(220)`、`execve(221)`、`exit(93)`、`wait4(260)`、`kill(129)`                                       | `syscall/process.rs`                 |
| 内存     | `mmap(222)`、`munmap(215)`、`brk(214)`、`mprotect(226)`                                                  | `syscall/process.rs`                 |
| 信号     | `sigaction(13)`、`sigprocmask(14)`、`sigtimedwait(137)`、`sigreturn(139)`                                | `syscall/process.rs`                 |
| 时间     | `clock_gettime(113)`、`nanosleep(101)`、`getitimer(102)`、`setitimer(103)`                               | `syscall/fs.rs`                      |
| 轮询     | `pselect6(72)`、`ppoll(73)`                                                                              | `syscall/fs.rs`（使用 `fs/poll.rs`） |

分发函数还有一个日志黑名单机制——高频 syscall（如 `yield`、`write`、`clock_gettime`）默认不会打印日志，除非设置 `LOG=trace`。

### 网络栈架构

```
用户应用程序（netperf / iperf）
        │  syscall（sendto / recvfrom / connect / accept / ...）
        ▼
┌─────────────────────────────────────────────────────────┐
│                  syscall/net.rs                          │
│  wait_io(|| socket.try_recv(buf), nonblock)              │
│  wait_io(|| socket.try_send(buf), nonblock)              │
│  wait_io(|| socket.try_connect(), nonblock)              │
│  wait_io(|| socket.accept(...), nonblock)                │
└──────────────────────────┬──────────────────────────────┘
                           │ Socket trait
                           ▼
┌─────────────────────────────────────────────────────────┐
│                  net/mod.rs（Socket trait）               │
│  + SocketTable（BTreeMap<Fd, Arc<dyn Socket>>）          │
│  + alloc() — 创建 Tcp/Udp/Raw socket，注册 FD           │
└──────┬──────────┬──────────┬────────────────────────────┘
       │          │          │
       ▼          ▼          ▼
   tcp.rs      udp.rs     raw.rs
   try_recv    try_recv   try_recv
   try_send    try_send   try_send
   try_connect            send_to（IP 头）
       │          │          │
       └──────────┴──────────┘
                   │ smoltcp socket API
                   ▼
┌─────────────────────────────────────────────────────────┐
│           net/config.rs（NET_INTERFACE）                  │
│  smoltcp::Interface + SocketSet                         │
│  poll() 函数推动网络栈状态前进                             │
│  dispatch_udp_packets() → 填充 UdpSocket.rx_queue        │
└──────────────────────┬──────────────────────────────────┘
                       │
                       ▼
┌─────────────────────────────────────────────────────────┐
│         net/adapter.rs → virtio_net → QEMU               │
└─────────────────────────────────────────────────────────┘
```

**关键设计规则：**
- `try_xxx` 方法从不调用 `poll()`，从不 `suspend_current_and_run_next()`，从不循环。
- `poll()` 由 `wait_io()`（在每次尝试前的循环中）和 `socket_r_ready()`/`socket_w_ready()` 调用。
- syscall 层始终将单次尝试用 `wait_io` 包装，实现阻塞语义。
- Socket 的 `File` 实现由 `impl_file_for_socket!` 宏生成。它将 `read` → `try_recv`、`write` → `try_send`、`r_ready` → `socket_r_ready` 映射。

### 内存管理

**物理内存**：基于栈的帧分配器（`frame_allocator.rs`），每帧 4KB。

**虚拟内存**：SV39（RISC-V）页表，使用 `PageTable` 结构体。每个进程拥有自己的 `MemorySet`：
- `.text`（R\|X\|U）、`.rodata`（R\|U）、`.data`（R\|W\|U）、`.bss`（R\|W\|U）
- 用户栈位于用户空间顶部，堆（通过 brk/mmap）

**内核映射**：`KERNEL_SPACE` 单例，恒等映射 + 高地址映射（0xFFFF...）。

**用户内存翻译**：`page_table.rs` 中的五个关键函数：
- `translated_ref(token, ptr)` → 翻译单个指针
- `translated_refmut(token, ptr)` → 翻译可变指针
- `translated_byte_buffer(token, ptr, len)` → 将缓冲区翻译成 `Vec<&mut [u8]>`（处理跨页）
- `copy_from_user` / `copy_to_user` — 批量拷贝，独占访问
- `translated_str(token, ptr)` → 翻译以 null 结尾的字符串

**注意**：`UserBuffer` 是 `Vec<&'static mut [u8]>`，包含总长度。**不是** `Clone` 的。`File` trait 中的 `read_user`/`write_user` 方法使用它在没有中间缓冲区的情况下直接读写用户空间。

### 任务/进程模型

单核、基于定时器中断的抢占式多任务。

**任务生命周期：**
```
就绪 ───► 运行 ───► 退出 ───► 僵尸
              │
              ▼
         可中断（阻塞 I/O、nanosleep、wait）
```

**关键结构体：**
- `TaskControlBlock`（PCB）：pid、内核栈、陷阱上下文、内存集、文件表、socket 表、信号状态、调度器信息、robust list、定时器、子进程列表、父进程引用
- `TaskStatus`：`Ready`、`Running`、`Interruptible`、`Zombie`
- 调度：通过 `TaskManager` 的 `VecDeque<Arc<TaskControlBlock>>` 实现轮转
- 上下文切换：`__switch(old_task_cx_ptr, new_task_cx_ptr)` 汇编实现（架构相关）

**Fork/clone：**
- `clone(220)`：创建新任务，共享/克隆内存和文件。退出信号传递给父进程。
- `execve(221)`：替换内存集，通过 `load_elf()` 加载新 ELF，重置信号处理器。
- `wait4(260)`：父进程等待子进程变为 Zombie，收集退出码。

**IPC/同步：**
- Futex（`futex`、`set_robust_list`、`get_robust_list`）用于用户空间同步
- 管道（`pipe2`）用于字节流 IPC
- Unix socket 对（`socketpair`）通过基于管道的 `UnixSocket` 实现
- 基于信号的进程间通知

### I/O 阻塞抽象（`wait_io` / `wait_io_core`）

```
wait_io_core(f, nonblock)
  │  每次迭代调用 f()
  │  遇到 EAGAIN：suspend_current_and_run_next → 检查信号 → 重试
  │  成功：返回字节数
  │  非阻塞：遇到 EAGAIN 立即返回
  │  （不调用 NET_INTERFACE.poll()）
  │
  └── 被 sys_read、sys_write 使用（适用于任何 fd 类型）

wait_io(f, nonblock)
  │  每次调用 f() 前先调用 NET_INTERFACE.poll()
  │  然后委托 wait_io_core 的逻辑
  │
  └── 被 sys_accept、sys_connect、sys_sendto、sys_recvfrom 使用
```

这种双层设计将 `poll()` 排除在通用文件 I/O 之外（管道/tty/文件/zero 都不需要网络轮询）。

---

## 编码规范

### 命名规则

| 模式         | 使用场景                              | 示例                                                 |
| ------------ | ------------------------------------- | ---------------------------------------------------- |
| `sys_xxx`    | syscall 处理函数                      | `sys_read`、`sys_sendto`                             |
| `_xxx`       | 内部辅助函数（单次执行，不循环）      | `_read`、`_connect`、`_accept`                       |
| `try_xxx`    | 一次非阻塞尝试，返回 `Result`         | `try_recv`、`try_send`、`try_connect`                |
| `socket_xxx` | socket 专用，避免与 `File` 方法名冲突 | `socket_r_ready`、`socket_w_ready`、`socket_hang_up` |

### 返回值编码

| 层                              | 成功              | 错误                                                       |
| ------------------------------- | ----------------- | ---------------------------------------------------------- |
| `File::read()/write()`          | `usize`（字节数） | `usize`（`as_errno_ret()` = `-(errno as isize) as usize`） |
| `Socket::try_recv()/try_send()` | `Ok(isize)`       | `Err(SyscallErr::XXX)`                                     |
| syscall 处理器                  | `isize`（>= 0）   | `isize`（负数：`-errno`，如 `-11` 表示 EAGAIN）            |

### 死锁预防

- 不要在跨越等待点的情况下同时持有 `task.files.lock()` 和 `task.socket_table.lock()`
- 推荐模式：获取锁 → clone Arc → 释放锁 → 执行操作
- `NET_INTERFACE.xxx_socket()` 使用内部的 `Mutex` — 保持闭包简短

---

## 常见踩坑

### 编译相关

| 问题                             | 解决方法                                                               |
| -------------------------------- | ---------------------------------------------------------------------- |
| `Vec` 重复定义                   | 不要同时使用 `use alloc::vec;` 和 `use alloc::vec::Vec;`——只用其中一个 |
| `lang_items` 不匹配              | 永远不要直接编辑 `lang_items.rs`；编辑 `.rv` / `.la` 变体              |
| 错误的 nightly 工具链            | `make rv64-kernel-build-only` 会自动切换；rv64 和 la64 目标分开运行    |
| `feature(asm_const)` 已稳定      | nightly feature 门控是为旧工具链准备的；新 Rust 可能需要不同的标志     |
| `cargo check` 在工作区根目录失败 | 始终在 `os/` 目录下用正确的 Makefile 目标运行                          |

### 网络栈

| 问题                                | 根因                                                       | 修复                                                      |
| ----------------------------------- | ---------------------------------------------------------- | --------------------------------------------------------- |
| `pselect` 永远挂起                  | `socket_r_ready()` 缺少 `NET_INTERFACE.poll()`             | 在检查 socket 状态前加 `NET_INTERFACE.poll()`             |
| `connect` 永不返回                  | TCP 握手失败，重试循环阻塞                                 | 使用 `try_connect` + `wait_io`；不要在 `connect()` 中循环 |
| `recvfrom` 永远返回 EAGAIN          | `try_recv` 未经 poll 直接调用                              | 让 `wait_io` 或 `socket_r_ready` 负责调用 `poll()`        |
| `sendto` 返回 0 字节                | `try_send` 返回 `Ok(nbytes)` 时 nbytes 是 `usize` 导致溢出 | 返回 `Ok(nbytes as isize)`                                |
| `accept` 返回 EAGAIN                | 监听池消耗完但不补充                                       | 检查 `_accept()` 是否补充了处理器队列                     |
| `UdpSocket` 收不到包                | `dispatch_udp_packets` 找不到匹配的 socket                 | 检查 `find_best_match()` 的 endpoint 比较逻辑             |
| TCP keepalive 定时器异常触发        | 无数据活动；向已关闭 socket 发送 ACK                       | 空闲 TCP 连接的预期行为                                   |
| `RawSocket::send_to` 构建 IP 头错误 | 手动 IPv4 头打包可能有误                                   | 验证校验和和头部长度                                      |
| 端口已占用                          | `SocketTable` 的 `can_bind` 检查                           | 如果指定端口失败，绑定随机端口                            |

### 内存管理

| 问题                          | 根因                               | 修复                                           |
| ----------------------------- | ---------------------------------- | ---------------------------------------------- |
| 内核入口处页错误              | 跳转到 `rust_main` 前未设置页表    | 检查 `entry.asm` 的页表初始化                  |
| `translated_refmut` 恐慌      | 用户地址未映射到当前页表           | 检查 VA 范围；先使用 `contains_valid_buffer()` |
| `UserBuffer::new` 失败        | `translated_byte_buffer` 返回错误  | 指针跨越了未映射的边界                         |
| 堆分配器恐慌                  | 内核堆耗尽                         | 检查 `heap_allocator.rs` 中的 `HEAP_SIZE`      |
| 帧分配器返回 None             | 物理内存耗尽                       | 启用 `oom_handler` 特性进行 OOM 恢复           |
| `munmap` 报告已预取消映射的页 | 双重释放或惰性分配的页尚未触发缺页 | 惰性分配的预期行为；警告是良性的               |
| `brk` 返回意外值              | 堆区域与 mmap 区域冲突             | 检查 `sys_brk` 中的 `program_break` 边界       |

### 文件系统

| 问题                           | 根因                           | 修复                                                  |
| ------------------------------ | ------------------------------ | ----------------------------------------------------- |
| `openat` 返回 ENOENT           | ext4/fat32 中找不到路径        | 检查根文件系统内容是否包含期望的路径                  |
| `read` 在 EOF 前返回 0         | 管道读端已关闭                 | 检查管道生命周期                                      |
| `write` 返回 EPIPE             | 读端关闭后写入                 | 检查 SIGPIPE 处理                                     |
| 对管道使用 `lseek` 返回 ESPIPE | 管道不支持寻址                 | 预期行为；不要对管道调用 lseek                        |
| `getdents64` 返回错误的条目    | ext4/fat32 目录迭代偏移错误    | 检查 `Dirent` 序列化                                  |
| 文件描述符泄漏                 | `dup` 未关闭旧的 FD            | 检查 `sys_dup2`/`dup3` 中的 `FileDescriptor` 生命周期 |
| `sendfile` 在 socket 上阻塞    | 阻塞 sendfile 在 EAGAIN 上循环 | 尚未重构以使用 `wait_io`                              |

### 任务/进程

| 问题                     | 根因                     | 修复                                  |
| ------------------------ | ------------------------ | ------------------------------------- |
| `clone` syscall 返回错误 | flags 包含不支持的位     | 检查 `CloneFlags` 解析                |
| `execve` 失败            | ELF 格式无效或缺少解释器 | 检查 `load_elf` 的段加载              |
| `wait4` 返回 ECHILD      | 没有子进程等待           | 检查进程树；WNOHANG 可能返回这个      |
| 信号未送达               | 信号被 sigprocmask 屏蔽  | 检查 `sigpending.difference(sigmask)` |
| 僵尸进程累积             | 父进程从未调用 `wait4`   | initproc 在其等待循环中回收孤儿进程   |
| `nanosleep` 提前返回     | 被信号打断               | 返回 `EINTR` 及剩余时间               |
| 定时器中断未触发         | `mtimecmp` 未正确设置    | 检查 `timer::init()` 和 SBI 超时调用  |

### QEMU / 测试

| 问题                         | 根因                                                   | 修复                                         |
| ---------------------------- | ------------------------------------------------------ | -------------------------------------------- |
| QEMU 启动无显示              | 首次打印前控制台输出未初始化                           | 检查 `console::init()` 是否第一个被调用      |
| 首次中断时双错误             | 未设置陷阱处理器（`stvec` = 0）                        | 检查 `rust_main()` 中的 `trap::init()`       |
| 测试组无输出挂起             | 测试二进制静默断言失败                                 | 单独运行该测试二进制                         |
| `os_test.conf` 修改不生效    | 配置在首次构建时已嵌入根文件系统                       | 使用 `conf-inject` 更新镜像                  |
| `make rv64-run` 重建所有内容 | 始终完整重建                                           | 用 `rv64-kernel-build-only` + 手动 QEMU 提速 |
| Ctrl-C 后 QEMU 进程残留      | `pkill qemu-system` 清理                               | 或使用 `-no-reboot`                          |
| LTP 测试失败                 | 内核可能缺少某些 syscall（如 `prlimit`、`membarrier`） | 添加返回 `ENOSYS` 的桩函数                   |

---

## 新增功能

### 1) 新增 Syscall

```rust
// 第 1 步：在 syscall/syscall_id.rs 中添加常量
pub const SYSCALL_MY_FEATURE: usize = 300;

// 第 2 步：在对应的 syscall/*.rs 中添加处理函数
pub fn sys_my_feature(arg1: usize, arg2: usize) -> isize {
    // 成功返回 >= 0，失败返回负 errno
    0
}

// 第 3 步：在 syscall/mod.rs 中注册分发
syscall_name 匹配：
    SYSCALL_MY_FEATURE => "my_feature",
dispatch 匹配：
    SYSCALL_MY_FEATURE => sys_my_feature(args[0], args[1]),
```

### 2) 新增 Socket 类型

```rust
// 第 1 步：实现 Socket trait
impl Socket for MySocket {
    fn try_recv(&self, buf: &mut [u8]) -> Result<isize, SyscallErr> { ... }
    fn try_send(&self, buf: &[u8]) -> Result<isize, SyscallErr> { ... }
    fn socket_type(&self) -> SocketType { SocketType::SOCK_MY }
    // ... 其他必需方法
    fn deep_clone_socket(&self) -> Arc<dyn File> { Arc::new(Self { ... }) }
}

// 第 2 步：一行实现 File trait
impl_file_for_socket!(MySocket);   // 搞定！

// 第 3 步：在 net/mod.rs 的 Socket::alloc() 中接入
```

### 3) 新增设备驱动

```rust
// 块设备：实现 drivers/block/mod.rs 中的 BlockDevice trait
impl BlockDevice for MyBlockDev { ... }

// 网络设备：实现 drivers/net/mod.rs 中的 NetworkDevice trait
impl NetworkDevice for MyNetDev { ... }
```

### 4) 新增文件系统

实现 `File` trait 的全部方法（见 `fs/file_trait.rs` —— 约 30 个方法），通过 `directory_tree.rs` 集成路径解析。

### 验证清单

- [ ] `make rv64-kernel-build-only` 通过
- [ ] `make la64-kernel-build-only` 通过
- [ ] QEMU 启动不 panic
- [ ] 新功能端到端工作（测试二进制输出）
- [ ] 已有测试全部通过（`make rv64-run`）
- [ ] 调试日志已删除或通过 `LOG=info` 控制

---

## 调试与性能分析

### ⚠️ 前置说明

**宿主机上工具链不全**（缺少 riscv64/larch64 gcc、QEMU、GDB 等），所有编译、运行、调试操作**必须在 Docker 容器内进行**。

```bash
# 在项目根目录进入 Docker 容器
make docker
```

进入容器后默认在 `/os` 工作目录，可直接执行以下所有命令。

### 日志级别

```bash
# 在 Docker 容器内用不同日志级别编译内核
cd os && LOG=info  make rv64-kernel-build-only
cd os && LOG=debug make rv64-kernel-build-only
cd os && LOG=trace make rv64-kernel-build-only
```

### 分析 Syscall 追踪

```bash
# 在 Docker 容器内以 LOG=info 运行，用 grep 过滤 syscall 模式
cd os && LOG=info make rv64-run 2>&1 | grep "\[syscall\]" | head -100

# 聚焦特定 PID
cd os && LOG=info make rv64-run 2>&1 | grep "pid 4:"
```

### QEMU 调试

> QEMU 和 GDB 只在 Docker 容器内可用，以下命令需在容器内执行。

```bash
# Debug 编译（容器内）
cd os && make rv64-debug

# 配合 gdb（容器内）
cd os && make rv64-gdb

# QEMU monitor — 手动启动调试会话
qemu-system-riscv64 -machine virt -kernel kernel-rv -S -s &
riscv64-unknown-elf-gdb kernel-rv -ex "target remote :1234"

# IRQ 追踪（容器内）
qemu-system-riscv64 -d int -no-reboot -serial stdio -kernel kernel-rv
```

---

## 更新本文档

本文档是 AI 助手在该项目上的单一事实来源。当你发现以下内容时：

- 新的 bug 模式 → 添加到**常见踩坑**
- 新的验证步骤 → 更新**编译与验证**
- 新模块或重构 → 更新**模块地图** / **架构详解**
- 编码规范 → 更新**编码规范**
- 代码修改 → 记录到 `WORK_LOG.md`
- 经验教训 → 记录到 `EXPERIENCE.md`

### 工作日志（`WORK_LOG.md`）

`WORK_LOG.md` 是**仅本地可见的 AI 工作日志**（已加入 `.gitignore`，不会被提交）。

**每次 AI 助手完成修改后，必须：**
1. 在 `WORK_LOG.md` 的日期分区下，记录本次修改的内容
2. 每个条目至少包含：
   - **日期**（按年月日分区）
   - **修改摘要**（1-2 句话说明做了什么）
   - **涉及文件**（文件路径列表）
   - **验证结果**（编译是否通过、QEMU 测试情况等）

**示例：**
```markdown
## 2026-04-26

### MSG flag 校验 & bitflags 重构

**涉及文件：** `os/src/syscall/net.rs`

- 将 MSG 标志改用 `bitflags!` 类型，添加 `validate_for_recv/send` 方法
- 修复 LTP recv01/recvfrom01 中 MSG_OOB/MSG_ERRQUEUE 返回 0 而非 -1 的问题

**验证：** `make rv64-kernel-build-only` ✅
```

保持条目简洁实用。使用代码库中的真实文件路径和函数名。

### 经验档案（`EXPERIENCE.md`）

`EXPERIENCE.md` 是**AI 助手的跨对话经验档案**（已加入 `.gitignore`，不会被提交），用于累积每次工作中发现的重要经验、教训和最佳实践。

**AI 助手每次工作完成后，必须检视本次工作是否有值得记录的通用经验：**
1. 有则追加到 `EXPERIENCE.md` 对应分类下
2. 分类包括：编译、网络栈、内存管理、文件系统、任务/进程、调试技巧、架构决策等
3. 条目格式：`[问题/现象] → [根因] → [解决方案/教训]`
4. 定期去重、合并同类条目

**示例：**
```markdown
## 网络栈

- 在 `wait_io` 外调用 `try_recv` 返回 EAGAIN → 因为缺少 `NET_INTERFACE.poll()` → 确保 `wait_io` 或 `socket_r_ready` 负责调用 `poll()`
```

与 `WORK_LOG.md` 的区别：
- `WORK_LOG.md` 按日期记录「本次改了啥」
- `EXPERIENCE.md` 按主题记录「学到了啥」，供将来复用
