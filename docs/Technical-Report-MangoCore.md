# MangoCore: Implementation and Evolution of an Engineering\-Oriented Rust Operating System Kernel for Linux Compatibility

# MangoCore：面向 Linux 兼容性的工程化 Rust 操作系统内核实现技术报告

# 第1章 项目概述

## 1\.1 项目背景

现代操作系统内核需要同时处理进程、内存、文件、网络和设备抽象，并在自动化测试和复杂用户态负载下保持 Linux 语义兼容。Rust 的所有权模型、类型系统和 `no_std` 生态为内核开发提供了内存安全与低开销抽象能力。

全国大学生操作系统比赛以 Linux 兼容性与操作系统核心机制为主要约束，要求参赛系统支持标准系统调用接口、文件系统、网络协议栈、多任务调度以及复杂用户态程序运行，并通过自动化测试与性能测试验证功能正确性。

MangoCore 是面向该比赛场景开发的 Rust 操作系统内核。项目以 Linux 兼容性为目标，围绕进程管理、虚拟文件系统、网络协议栈、内存管理和调试分析能力组织内核结构，并在测试反馈中调整模块边界和内部抽象。

项目除基础功能实现外，还提供自动化测试脚本、procfs/sysfs 诊断接口、日志输出以及若干 feature 控制的调试设施，用于性能分析和问题定位。

---

## 1\.2 系统设计目标

MangoCore 的设计目标是实现具备 Linux 兼容能力、模块化结构和工程可维护性的 Rust 操作系统内核。

围绕这一目标，项目制定了以下四项核心设计原则。

第一，保持 Linux 兼容能力。系统遵循 Linux 系统调用语义，支持标准 ELF 程序加载、BusyBox 运行、多进程管理、文件系统访问以及 TCP/UDP 网络通信。

第二，采用模块化架构设计。项目将内存管理、文件系统、网络协议栈、任务管理以及驱动层进行解耦，通过明确的模块边界和局部抽象降低模块之间的耦合程度。

第三，建立工程调试与可观测能力。针对大型操作系统开发过程中难以定位的问题，项目提供procfs、sysfs、日志、heap\_trace以及部分feature-gated诊断输出，用于暴露任务、内存、缓存和调度相关状态。

第四，围绕性能路径进行迭代。项目针对缓存管理、等待队列、网络路径、页缓存管理等关键路径进行调优，并使用测试结果与日志分析验证修改效果。

上述目标共同约束了 MangoCore 的模块划分、测试流程和调试设施设计。

---

## 1\.3 MangoCore总体演进路线

MangoCore 的发展经历了多个实现阶段。

项目初期版本首先实现基本进程管理、系统调用以及内存管理框架，支持简单用户程序运行；后续阶段在此基础上扩展文件系统、网络、内存管理和调试能力。

文件系统阶段引入 ext4、FAT32、tmpfs、procfs 等实现，并通过虚拟文件系统（VFS）抽象向上提供统一访问接口。PageCache 负责文件数据缓存，并服务于 mmap、共享文件映射和缓存优化相关路径。

网络子系统最初采用较直接的 Socket 管理方式；多设备和路由需求增加后，项目重新组织 Socket、设备和路由之间的关系，引入 DeviceStack、RouteSocketHandle、SocketBinding 以及 PortManager 等组件，并在 TcpSocket 内部维护 `fast_route_id`、`fast_ifindex`、`fast_state` 等路由缓存提示字段，用于减少部分已连接 TCP 路径上的重复路由查询。

调试与诊断方面，项目增加 heap\_trace、procfs 状态导出、sysfs 调试接口以及部分诊断打印；其中 `zombie_owner` 属于 `heap_trace` feature 下的诊断输出项。

整个项目的发展过程可以概括为：

```Plain Text
早期版本

↓

基础任务管理

↓

Linux系统调用扩展

↓

BusyBox运行

↓

VFS架构引入

↓

多文件系统支持

↓

PageCache机制引入

↓

网络子系统重构

↓

调试与诊断能力增强

↓

兼容性与性能测试

↓

当前版本 MangoCore
```

该路线反映了项目从基础功能实现到模块重构、测试归档和运行时观测的演进顺序。

---

## 1\.4 关键设计

MangoCore 的关键设计集中在 VFS、网络管理和测试反馈三个方面。

首先，项目采用虚拟文件系统抽象，将 IndexNode、File、FileSystem 以及 MountFS 进行分层设计，并结合 PageCache 与文件系统各自的元数据缓存机制区分文件数据缓存和元数据管理职责。

其次，在网络子系统中设计了基于RouteSocketHandle和SocketBinding的管理机制，将Socket与具体设备、smoltcp SocketHandle之间的关系集中记录，降低协议处理与设备选择之间的耦合，并通过DeviceStack组织设备、Interface和SocketSet。

最后，项目在重要架构调整后使用功能测试、性能测试或归档日志验证修改效果，降低系统演进过程中的功能退化风险。

这些机制构成后续章节讨论文件系统、网络、内存和调试体系的基础。

---

## 1\.5 整体技术路线

MangoCore采用自底向上的分层设计思想，将整个操作系统划分为驱动层、核心管理层、资源抽象层以及用户接口层四个层次。

底层驱动负责Block Device、VirtIO设备以及网络设备管理；核心管理层实现任务管理、内存管理、文件系统以及网络协议栈；资源抽象层通过VFS、Socket trait等接口屏蔽具体实现差异；最上层通过系统调用接口向用户态程序提供服务。

该分层结构将硬件访问、内核资源管理和用户接口分离，降低了跨层修改对其他子系统的影响。

---

## 1\.6 项目贡献统计

截至本文版本，MangoCore 已覆盖多个操作系统核心组件。

系统支持多进程管理、信号机制、ELF程序加载、ext4/FAT32/tmpfs/procfs/sysfs等文件系统、TCP/UDP网络协议栈以及Linux兼容系统调用接口，并实现PageCache、元数据缓存、PortManager以及TCP fast route提示字段等多个组件。

项目同时提供自动化测试脚本、测试结果归档、procfs/sysfs 诊断接口和日志机制，通过功能测试、性能测试和部分压力测试记录系统行为。

在工程实践方面，项目完成了多轮 VFS 重构、网络架构重构以及缓存管理优化，并处理了 Socket 生命周期管理、Zombie PCB 泄漏、缓存一致性以及资源回收等问题。

---

## 1\.7 本章小结

本章介绍 MangoCore 的项目背景、设计目标、演进路线、关键设计以及技术路线。MangoCore 以 Linux 兼容性为基础，通过模块化架构、局部抽象和工程实践组织内核实现，覆盖进程管理、内存管理、文件系统、网络协议栈以及调试分析能力。

后续章节分别讨论系统总体架构、核心子系统实现、性能优化策略以及工程调试案例。

---

# 第2章 系统总体架构

本章介绍 MangoCore 的模块划分、架构组织和演进历程，并说明后续各子系统在内核整体结构中的位置。

---

## 2\.1 系统设计理念

MangoCore 在扩展进程管理、内存管理、虚拟文件系统、网络协议栈以及多种文件系统实现后，需要处理模块耦合和资源所有权问题。文件系统与具体实现之间存在依赖关系，Socket 生命周期管理与协议处理流程交织，缓存管理职责边界需要进一步明确。

针对上述问题，MangoCore 重新规划内核内部结构，以模块解耦、资源所有权管理和局部抽象为主要方向进行重构。

系统通过 VFS、Socket trait、AddressSpace/VmaSet、TaskManager 等模块边界隔离具体实现，减少跨模块状态共享。

与早期实现相比，后续版本使用兼容性测试、回归测试和归档日志验证功能修改。

---

## 2\.2 MangoCore发展历程

MangoCore 的发展按照问题驱动方式推进。项目初期首先完成内核启动、任务调度、Trap 处理以及基础内存管理，支持简单用户程序加载和运行。

BusyBox、libc\-test 等测试程序接入后，系统开始支持更复杂的 Linux 用户程序。文件系统部分建立虚拟文件系统（VFS）抽象，并引入 ext4、FAT32、tmpfs、procfs 等文件系统支持。

文件访问规模扩大后，项目引入 PageCache 等缓存机制，以减少重复磁盘访问。网络子系统则重新组织 Socket、路由以及设备之间的关系，引入 RouteSocketHandle、SocketBinding 等结构。

此外，项目增加 procfs、sysfs 以及 trace 等调试设施，并补充自动化测试和兼容性验证流程。



---

## 2\.3 系统总体架构

```Plain Text
User Space

 BusyBox   Shell   Lua   User Program

                     │

              System Call Layer

                     │

==================================================

                 Kernel Core

 Process     Memory      FileSystem      Network

 Task        AddressSpace    VFS          Socket

 Signal      Vma            ext4         TCP/UDP

 TaskManager PageCache      FAT32        Unix sockets

Trap        OomAwareAllocator procfs    smoltcp

==================================================

              Device Driver Layer

 VirtIO Block

 VirtIO Net

 UART

 PCI

==================================================

 Hardware
```

MangoCore采用模块化架构组织整个内核。用户程序通过系统调用进入内核，由Trap完成上下文切换和系统调用分发。内核内部按照任务管理、内存管理、文件系统和网络子系统进行划分，各模块通过统一接口协同工作。其中，内存管理负责地址空间、页管理和内核堆分配；文件系统通过VFS统一组织不同文件系统实现；网络模块基于Socket抽象完成用户接口，并结合smoltcp协议栈完成网络通信。

---

## 2\.4 模块解耦设计思想

MangoCore 的工程演进重点在于降低模块之间的耦合关系，并在此基础上扩展系统功能。

在文件系统中，VFS 负责统一抽象文件对象，不同文件系统通过统一接口完成资源访问；PageCache 负责文件数据缓存，而不同文件系统分别维护各自的元数据管理逻辑，降低了不同模块之间的耦合程度。

---

## 2\.5 Boot启动流程

系统启动遵循标准Rust内核初始化流程。

```Plain Text
OpenSBI

↓

_start

↓

rust_main()

↓

bootstrap_init()

↓

mem_clear()

↓

console::log_init()

↓

trace::init()

↓

mm::init()

↓

machine_init()

↓

task::timer_subsystem_init()

↓

fs/net初始化

↓

创建Init任务

↓

进入调度循环 run_tasks()
```

启动过程按照资源依赖关系组织，每个阶段负责自身资源构建，减少初始化顺序中的隐式依赖。

---

## 2\.6 文件系统总体架构

文件系统采用统一虚拟文件系统抽象。

```Plain Text
User Program
                      │
                System Call
                      │
             File / Directory API
                      │
                VFS Virtual Layer
                      │
      ┌───────────────┼────────────────┐
      │               │                │
    ext4           FAT32           procfs/tmpfs/sysfs
      │               │                │
      ├───────────────┘                │
      │                                │
   PageCache（文件数据缓存，可供支持缓存的文件系统使用）
      │
  Block Device Interface
      │
   VirtIO Block Driver
      │
     Disk Device
```

VFS 架构减少具体文件系统之间的直接依赖，不同文件系统通过统一接口完成资源访问。

PageCache负责缓存文件数据页。

文件系统各自维护元数据缓存和回写逻辑；元数据管理细节随具体文件系统而变化。

---

## 2\.7 网络总体架构

网络子系统采用模块化设计。

```Plain Text
User Socket Syscall
        │
   Socket trait
        │
 ┌──────┼──────────────┬──────────────┐
 │      │              │              │
TcpSocket UdpSocket RawSocket   Unix sockets
 │      │              │
 └──────┴──────┬───────┘
               │
        SocketBinding
               │
        DeviceStack / Interface
               │
       smoltcp SocketSet
               │
        VirtIO Net Driver
               │
             QEMU
```

MangoCore 网络子系统以 Socket trait 作为文件描述符层与具体协议实现之间的边界。TCP、UDP、RAW 等网络协议 socket 的 IPv4/IPv6 处理依赖 smoltcp 的 Interface 与 SocketSet；设备侧通过 DeviceStack 组织网卡设备、接口和 Socket 集合。

对于已建立的TCP连接，TcpSocket内部维护 `fast_route_id`、`fast_ifindex`、`fast_state` 等原子字段，用作具体 socket 实现内部的路由缓存提示。

Unix Domain Socket 通过 UnixStreamSocket、UnixDatagramSocket 等内核内部类型实现本机进程间通信，路径独立于网络协议 socket。



---

## 2\.8 内存管理总体架构

内存管理采用多层结构。

```Plain Text
Process
   │
AddressSpace
   │
VmaSet
   │
Vma
   │
Page Table
   │
StackFrameAllocator
   │
Physical Frames

Kernel Heap
   │
OomAwareAllocator
   │
buddy_system_allocator::Heap<32>
```

每个进程维护独立的 AddressSpace，AddressSpace 通过 VmaSet 管理用户地址空间中的多个 Vma 区域，用于描述代码段、数据段、堆、栈以及 mmap 映射区域。发生缺页异常时，内核根据 Vma 信息、页表状态和访问类型决定 lazy allocation、文件映射、共享写、CoW、swap/zram恢复等处理路径。

物理页由 `StackFrameAllocator` 管理。它优先复用 `recycled` 中的回收页；若无可复用页，则按照 `current` 游标递增分配新页。内核堆的小对象分配由 `buddy_system_allocator::Heap<32>` 承担。

内核堆由 `OomAwareAllocator` 包装 `buddy_system_allocator::Heap<32>` 实现，用于管理Rust内核对象的动态申请。PageCache负责文件数据页缓存；mmap文件映射缺页通过文件映射与PageCache相关路径建立映射。MAP_PRIVATE写缺页触发CoW复制，MAP_SHARED写缺页走共享写路径并维护脏页状态。

---

## 2\.9 Benchmark开发体系

为验证 MangoCore 各模块功能的正确性和 Linux 兼容性，项目建立兼容性测试与自动化回归测试流程。系统结合 BusyBox、libc\-test、iozone、iperf 等测试程序集，对系统调用、文件系统、网络通信以及基础运行环境进行验证，并保存测试日志和运行结果。

项目提供自动化测试脚本，用于在重要功能修改后重新执行兼容性测试，比较不同版本之间的功能变化。性能相关模块结合具体测试结果分析系统瓶颈，并针对缓存管理、网络处理及文件访问等路径进行调优，避免将单一 Benchmark 指标作为系统设计的唯一依据。

回归测试用于发现模块修改带来的兼容性退化，并为功能扩展提供可复查的测试记录。

---

## 2\.10 本章小结

本章介绍 MangoCore 的总体架构和子系统组织关系。任务管理、内存管理、文件系统和网络子系统按职责分层；文件系统以 VFS 为统一抽象，网络子系统采用 Socket 抽象并结合 smoltcp 协议栈，内存管理围绕 AddressSpace、Vma、页表和物理页分配建立虚拟内存管理体系。兼容性测试和自动化回归测试用于验证这些模块的修改效果。

# 第3章 开发环境与工程平台设计

---

## 3\.1 设计背景

MangoCore 采用 Rust 语言进行开发，为保证不同开发成员之间环境的一致性，项目提供 Docker 构建环境，同时支持在 Linux 或 Windows WSL 环境下完成开发与调试。整个开发过程主要依赖 Rust nightly 工具链、Cargo 构建系统、LLVM 工具链以及 QEMU 模拟器，实现双架构内核的编译、运行与调试。

项目分别针对 RISC\-V64 与 LoongArch64 维护独立配置，并通过构建脚本完成编译、镜像生成和启动流程。

---

## 3\.2 工程开发总体架构

MangoCore 采用模块化工程组织方式。内核主体位于 os 目录，下设任务管理、内存管理、文件系统、网络子系统、设备驱动以及系统调用等多个模块；用户程序位于 user 目录；scripts 提供自动化构建和测试脚本；docs 保存开发文档；testresult 用于保存测试结果和归档日志。

```Plain Text
os/src
 ├── hal
 ├── task
 ├── mm
 ├── fs
 ├── net
 ├── drivers
 ├── syscall
 ├── timer.rs
 ├── trace.rs
 └── main.rs

user

tools

scripts

docs

testresult
```

---

## 3\.3 Docker统一开发环境

Docker 环境用于固定编译环境和依赖版本。开发人员在容器中完成内核编译、QEMU 启动以及自动化测试，测试结果保存至 testresult 目录。

```Plain Text
Source

↓

Docker

↓

Compile

↓

Run QEMU

↓

Test

↓

Archive Result
```

---

## 3\.4 多平台交叉编译体系

MangoCore 的编译流程首先由 Cargo 完成整个内核源码编译，生成内核 ELF 文件；随后结合启动程序和文件系统镜像构建可启动映像，最后通过 QEMU 加载运行，并进入 BusyBox、libc\-test 等用户程序完成兼容性验证。

整个流程如下。

```Plain Text
Rust Source

↓

Cargo Build

↓

Kernel ELF

↓

Boot Image

↓

QEMU

↓

BusyBox

↓

Compatibility Test
```

---

## 3\.5 Git协同开发体系

MangoCore 采用 Git 进行版本管理。团队成员从 develop 分支同步代码，在个人开发分支完成模块修改，经测试验证后合并至 develop，最终同步至主分支。

整个协同流程如下。

```Plain Text
main

develop

个人开发分支

↓

Pull Request

↓

Code Review

↓

Merge develop

↓

同步main
```

---

## 3\.6 调试平台设计

MangoCore 在开发过程中结合 QEMU 调试、Rust 日志系统以及 procfs、sysfs 等运行时信息接口，对任务状态、内存管理、网络连接和文件系统运行情况进行分析。部分调试信息通过 trace 模块进行记录，为定位系统问题提供支持。

整个调试框架如下。

```Plain Text
MangoCore

                       │

        ┌──────────────┼──────────────┐

        │                             │

     procfs                       sysfs

        │                             │

        ├──────────────┐

        │              │

   heap_trace    perf_diag

        │              │

    Serial Log     Runtime Status

        │              │

        └──────Analysis───────┘
```

---

## 3\.7 本章小结

本章介绍 MangoCore 的开发环境、工程组织方式以及自动化测试流程。Rust/Cargo、Docker、QEMU 和 Git 共同构成编译、运行、测试和归档链路。

# 第4章 内核核心机制设计

## 4\.1 内核设计背景

为支持多进程、多线程、信号、系统调用以及复杂用户程序运行，MangoCore 将进程管理、线程调度、Trap 处理以及信号机制分别组织为独立模块，并通过各自接口协同完成任务执行、阻塞唤醒、信号投递和资源回收。

---

## 4\.2 Task设计

MangoCore 将进程与线程分别抽象为 ProcessControlBlock（PCB）和 TaskControlBlock（TCB）两个层次。PCB 负责维护整个进程共享资源，包括地址空间、文件描述符表、信号处理状态、线程集合以及子进程关系；TCB 作为调度实体，保存线程上下文、内核栈、Trap 上下文以及当前运行状态。上下文切换只保存和恢复 TaskControlBlock 中的寄存器状态，进程级共享资源继续由所属 ProcessControlBlock 管理。

---

## 4\.3 后台维护逻辑

后台维护逻辑由具体模块或调度路径按需触发。例如，文件系统缓存回收会在调度循环中按条件调用；其他维护任务也分别由所属子系统组织。

---

## 4\.4 Task生命周期

MangoCore 的线程状态围绕 TaskManager、阻塞原语和任务自身执行路径共同变化。线程创建后进入 Ready 队列，等待调度循环选择运行；获得处理器后进入 Running 状态；当线程等待资源或睡眠时进入 Interruptible 等阻塞状态，待事件满足后重新进入 Ready 队列；线程执行结束后进入 Zombie 状态。Task 层的 zombie 队列用于调度器回收退出任务，进程层仍保留 wait/wait4 可见的 zombie 子进程语义。页面、Socket、File 等资源分别由对应子系统通过引用计数、所有权转移、缓存回收和关闭路径管理。

---

## 4\.5 调度路径设计

调度相关逻辑由 TaskManager、Processor、`run_tasks()` 调度循环、`schedule()` 以及底层 `__switch` 共同完成。所有处于 Ready 状态的线程维护在运行队列中，调度循环选择下一运行线程；当线程主动调用 yield、等待资源或时间片耗尽时，内核保存当前线程上下文，并恢复下一线程的执行现场。

MangoCore 当前采用基于时钟节拍（Tick）的抢占式调度机制。系统通过定时器中断周期性进入调度相关路径，当检测到当前线程需要重新调度时触发任务切换；除此之外，线程也可以主动让出 CPU，因此系统同时支持抢占调度与主动调度两种方式。整个调度过程围绕 TaskControlBlock 进行管理，而线程共享资源仍由 ProcessControlBlock 统一维护。

---

## 4\.6 Trap与系统调用机制

Trap 机制承担用户态与内核态切换职责，是用户态进入内核的入口。

用户程序执行ECALL指令后进入Trap处理流程，Kernel首先保存用户上下文信息，随后进入统一Trap处理入口，根据Trap类型判断是否属于系统调用、异常或中断。

对于系统调用，Trap模块根据系统调用号进入统一Dispatcher，由Dispatcher完成参数解析并调用对应Kernel服务。所有系统调用均采用统一入口处理，不同模块无需单独维护Trap逻辑。

Trap 模块同时负责异常恢复和错误返回，使系统调用、异常和中断从同一入口进入后再按类型分发。

---

## 4\.7 Signal机制设计

Signal 用于完成异步事件通知，是 Linux 兼容语义的一部分。

Signal 采用 Pending Signal 机制。

当Signal产生时，内核将对应Signal加入 task 级 pending 队列或 process 级 shared pending 队列，不立即在任意内核上下文中执行用户处理函数。阻塞路径会主动调用 `has_actionable_signal()` 判断是否应被信号打断；从内核返回用户态前，`trap_return()` 路径会进入 `do_signal()`，完成信号帧设置和投递。

该机制使信号投递与阻塞唤醒、返回用户态路径协同工作，并避免在任意内核临界路径中直接执行用户态信号处理逻辑。

---

## 4\.8 本章小结

本章介绍 MangoCore 的任务管理机制以及用户态与内核态之间的执行流程。ProcessControlBlock 与 TaskControlBlock 分别承担进程资源管理和线程调度实体职责；TaskManager、Processor、`run_tasks()`、`schedule()` 和底层上下文切换代码共同完成线程调度；Trap、系统调用和信号机制构成用户程序访问内核资源的入口路径。

# 第5章 内存管理子系统设计

## 5\.1 设计背景

内存管理负责物理帧、内核堆、用户虚拟地址空间和文件映射等资源。MangoCore 支持多进程运行、虚拟文件系统、网络协议栈以及复杂用户程序加载后，内核需要同时维护 Task、File、Socket、Page、Inode 和缓存数据等动态对象；简单页分配方式难以覆盖这些对象的分配、回收和缺页处理需求。

MangoCore 当前的内存管理实现采用分层设计：物理页管理由 StackFrameAllocator 负责，内核堆由 buddy\_system\_allocator crate 提供的 Heap\<32\> 管理，虚拟内存通过 AddressSpace 配合 VmaSet/Vma 进行管理，并支持 lazy allocation、file\-backed mmap、Copy\-on\-Write（CoW）、以及 feature\-gated 的 OOM/zram/swap 机制。

内存管理系统按资源类型分层：物理帧、内核堆、虚拟地址空间、文件映射和缓存分别由对应模块维护。

---

## 5\.2 MangoCore内存管理演进过程

MangoCore 的内存管理在多轮功能扩展和测试反馈中形成。

项目初期采用简单页分配器管理物理内存，主要满足 Task 创建和内核对象申请需求。BusyBox 以及标准测试程序接入后，文件访问频率上升，缓存对象数量增加，原有分配方式需要调整。

第一次重构引入 StackFrameAllocator，采用栈式分配策略管理物理页，实现快速分配与释放，同时支持 OOM handler 回调机制。

随后引入 buddy\_system\_allocator crate 构建内核堆 Heap\<32\>，用于管理内核动态对象，替代小对象直接申请整页的方式。

VFS 扩展后，文件访问成为系统热点。项目建立 PageCache，将文件数据缓存与物理页管理分离，减少重复 Block Device 访问。

后续版本引入 AddressSpace/Vma 虚拟内存管理、lazy fault 按需分配、file\-backed mmap、Copy\-on\-Write 等机制，并增加 feature\-gated 的 OOM handler、zram 压缩内存和 swap 交换支持。

内存管理体系由简单物理页管理扩展为覆盖物理帧分配、内核堆、虚拟内存、PageCache、mmap、CoW 以及交换机制的分层架构。

---

## 5\.3 内存管理总体架构

MangoCore 将内存系统划分为物理内存管理层、内核堆管理层、虚拟内存管理层以及缓存管理层，各层通过明确接口协作。

整体架构如下：

```Plain Text
User Process
       │
       ▼
  Virtual Address
       │
       ▼
  AddressSpace + VmaSet/Vma
       │
       ├──────────────────────────────────┐
       │                                  │
       ▼                                  ▼
  Page Fault Handler              File-backed mmap
       │                                  │
       ▼                                  ▼
  Frame Allocation               PageCache (文件数据缓存)
       │                                  │
       └──────────────────────────────────┘
                      │
                      ▼
           StackFrameAllocator
                      │
                      ▼
             Physical Memory
```

```Plain Text
Kernel Object (Task/File/Socket/...)
       │
       ▼
  Heap<32> (buddy_system_allocator)
       │
       ▼
  StackFrameAllocator
       │
       ▼
  Physical Memory
```

该架构中的主要职责如下：

- 用户程序始终访问虚拟地址空间，由 AddressSpace 负责地址映射；

- AddressSpace 通过 VmaSet 管理多个 Vma 区域，每个 Vma 描述一段连续虚拟地址空间；

- 缺页异常由 Page Fault Handler 统一处理，根据 Vma 类型执行对应 FaultAction（懒分配、文件映射读/写、CoW 等）；

- 物理帧通过 StackFrameAllocator 管理；

- 内核对象通过 Heap\<32\> 分配，底层同样依赖 StackFrameAllocator；

- PageCache 负责文件数据缓存，并与 file-backed mmap 缺页路径发生交互。

与直接管理物理页的早期方式相比，分层结构将用户地址空间、内核对象分配和文件数据缓存拆分到不同模块。

---

## 5\.4 StackFrameAllocator 物理页管理

StackFrameAllocator 负责 Kernel 物理页管理，是用户页、页表页、缓存页以及部分内核结构最终申请物理帧的基础。

源码位置：`os/src/mm/frame_allocator.rs`

### 5\.4\.1 设计原理

MangoCore 采用栈式回收列表管理已释放物理页。分配时优先从 `recycled` 栈顶取回收页；若没有回收页，则通过 `current` 游标递增分配新的物理页。该路径负责整页粒度的物理帧分配，内核堆的小对象分配则由 `buddy_system_allocator::Heap<32>` 管理。

```rust
pub struct StackFrameAllocator {
    start: usize,
    current: usize,
    end: usize,
    recycled: Vec<usize>,
    recycled_flags: Vec<bool>,
}
```

### 5\.4\.2 分配流程

```rust
fn alloc(&mut self) -> Option<FrameTracker> {
    let result = if let Some(ppn) = self.recycled.pop() {
        self.mark_recycled(ppn, false);
        Some(FrameTracker::new(ppn.into())) // usize -> PhysPageNum
    } else if self.current == self.end {
        None
    } else {
        self.current += 1;
        Some(FrameTracker::new((self.current - 1).into())) // usize -> PhysPageNum
    };
    result
}
```

该摘录保留实际分配分支结构；性能计数和 `zero_init` 条件编译分支属于辅助逻辑。

### 5\.4\.3 释放流程

释放路径会将物理页号压入 `recycled`，并通过 `recycled_flags` 标记 membership，避免重复释放时线性扫描。`dealloc` 还包含重复释放检测与诊断逻辑。

### 5\.4\.4 OOM Handler（feature\-gated）

```rust
#[cfg(feature = "oom_handler")]
pub fn oom_handler(req: usize) -> Result<(), ()> {
    // try reclaim/swap/zram according to current build features
}
```

当物理内存耗尽时，如果启用了 `oom_handler` feature，系统会调用 `oom_handler(req)`，尝试回收、压缩或换出页面，然后重试分配。

---

## 5\.5 Kernel Heap设计

StackFrameAllocator 适合管理物理页（通常 4KB），而 Kernel 内部大量对象仅需要几十字节至几百字节内存，例如 Task、File、Socket、WaitQueue 等。如果全部直接申请物理页，将造成严重内存浪费。

### 5\.5\.1 设计原理

MangoCore 采用 `OomAwareAllocator` 包装 `buddy_system_allocator::Heap<32>` 构建内核堆。

源码位置：`os/src/mm/heap_allocator.rs`

```rust
pub struct OomAwareAllocator {
    inner: Mutex<Heap<32>>,
}

#[global_allocator]
static HEAP_ALLOCATOR: OomAwareAllocator = OomAwareAllocator::empty();
```

### 5\.5\.2 堆空间初始化

```rust
static mut HEAP_SPACE: [u8; KERNEL_HEAP_SIZE] = [0; KERNEL_HEAP_SIZE];

pub fn init_heap() {
    unsafe {
        HEAP_ALLOCATOR.init(HEAP_SPACE.as_ptr() as usize, KERNEL_HEAP_SIZE);
    }
}
```

### 5\.5\.3 分配流程

Kernel 对象通过标准 Rust 分配接口（`Box`、`Vec`、`Arc` 等）申请内存：

```Plain Text
Kernel Object Request
       │
       ▼
 #[global_allocator] HEAP_ALLOCATOR
       │
       ▼
  buddy_system_allocator::Heap<32>
       │
       ▼
  从 HEAP_SPACE 中分配
       │
       ▼
  返回对象指针
```

当前堆空间来自静态 `HEAP_SPACE: [u8; KERNEL_HEAP_SIZE]`。当分配失败时，`OomAwareAllocator` 会在 `oom_handler` feature 启用时尝试走 OOM recovery；堆扩展策略由该静态堆空间和 OOM recovery 路径共同决定。

## 5\.6 虚拟内存管理机制设计

用户程序访问虚拟地址空间，内核通过页表维护虚拟地址到物理帧的映射。虚拟内存机制同时承担地址隔离、文件映射、按需分配和共享内存支持。

### 5\.6\.1 AddressSpace 设计

源码位置：`os/src/mm/address_space.rs`

```rust
pub struct AddressSpace<T: PageTable> {
    pub(super) page_table: T,
    pub(super) vmas: VmaSet,
    pub(super) heap_bottom: usize,
    pub(super) heap_pt: usize,
    locked_pages: BTreeSet<VirtPageNum>,
}
```

AddressSpace 维护整个进程的虚拟地址空间，包括：

- `page_table`：页表（RISCV64 使用 Sv39，LoongArch64 使用对应硬件页表）；

- `vmas`：VmaSet，管理所有虚拟内存区域；

- `heap_bottom` / `heap_pt`：进程堆起点和当前 program break；

- `locked_pages`：用于 mlock/VmLck 统计的锁定页集合。

### 5\.6\.2 VmaSet 与 Vma 设计

源码位置：

- `os/src/mm/vma_set.rs:15` → `VmaSet`

- `os/src/mm/vma.rs:34` → `Vma`

```rust
pub struct Vma {
    pub inner: VmPageStore,
    pub map_perm: MapPermission,
    pub map_file: Option<Arc<dyn IndexNode>>,
    pub map_file_offset: usize,
    pub may_write: bool,
    pub write_sealed: bool,
    pub flags: MapFlags,
    pub wipe_on_fork: bool,
    pub dont_fork: bool,
    pub fork_inherited: bool,
}
```

每个 Vma 描述一段连续的用户虚拟内存区域：

- `inner`：记录页范围及每页对应的物理页存储信息；

- `map_perm`：访问权限（读/写/执行/用户态等）；

- `map_file` / `map_file_offset`：文件映射后端与文件偏移；

- `may_write` / `write_sealed`：写权限相关约束；

- `flags`、`wipe_on_fork`、`dont_fork`、`fork_inherited`：mmap/fork 相关标记。

### 5\.6\.3 VmaSet 管理

```rust
pub struct VmaSet {
    vmas: BTreeMap<VirtPageNum, Vma>,
    mmap_holes: BTreeMap<VirtPageNum, VirtPageNum>,
    user_area_count: usize,
    user_page_count: usize,
}
```

VmaSet 使用 `BTreeMap<VirtPageNum, Vma>` 按虚拟页号有序管理所有 Vma，并维护 mmap 空洞和用户区间统计，支持：

- 插入 Vma；

- 按地址查找 Vma；

- 区间重叠检查；

- 区间合并与分裂。

---

## 5\.7 Page Fault 与文件映射缺页处理

当用户访问某一虚拟地址但页表中不存在对应映射时，触发缺页异常（Page Fault），由 Kernel 统一处理。

源码位置：`os/src/mm/page_fault.rs`

### 5\.7\.1 FaultAction 枚举

```rust
pub enum FaultAction {
    LazyAlloc,
    FileBackedRead,
    FileBackedWrite,
    FileBackedSharedWrite,
    #[cfg(feature = "oom_handler")]
    Decompress,
    #[cfg(feature = "oom_handler")]
    SwapIn,
    SharedWrite,
    StaleLazyPte,
    Cow,
    MappedRead,
    ResidentWithoutPte,
}
```

其中 `Decompress` 和 `SwapIn` 受 `oom_handler` feature 影响；不同构建配置下实际可用路径以源码条件编译为准。

### 5\.7\.2 Page Fault 处理流程

```Plain Text
Access Address
       │
       ▼
  CPU 触发 Page Fault
       │
       ▼
  Trap Handler
       │
       ▼
  Page Fault Handler (page_fault_handler)
       │
       ▼
  AddressSpace 查找 Vma
       │
       ▼
  找到对应 Vma ──No──> SIGSEGV
       │
      Yes
       │
       ▼
  根据 Vma、PTE 和访问类型执行 FaultAction
       │
      ├── LazyAlloc ─────────────> 分配匿名页
      ├── FileBackedRead/Write ──> 处理文件映射缺页
      ├── FileBackedSharedWrite ─> 处理文件共享映射写
      ├── SharedWrite ───────────> 恢复共享页写权限
      ├── Cow ───────────────────> copy_private_page() 复制私有页
      ├── SwapIn/Decompress ─────> 恢复换出或压缩页
      └── MappedRead 等 ─────────> 处理已驻留页面的PTE恢复
       │
       ▼
  继续执行用户程序
```

### 5\.7\.3 CoW 实现

```rust
fn copy_private_page<T: PageTable>(
    area: &mut Vma,
    page_table: &mut T,
    ctx: FaultContext,
) -> Result<PhysAddr, MemoryError> {
    let allocated_ppn = area.copy_on_write(page_table, ctx.vpn)?;
    Ok(ctx.offset_phys(allocated_ppn))
}
```

当发生 CoW 缺页异常时：

1. fault 分类逻辑判定该访问应走 `FaultAction::Cow`；

2. 调用 `copy_private_page` 分配新物理帧；

3. 复制原物理帧内容到新帧；

4. 更新页表映射到新帧，标记为可写；

5. 原页生命周期由 `VmPageStore`、FrameTracker 和页表映射关系共同维护。

---

## 5\.8 mmap统一文件映射机制

mmap 允许用户地址空间直接映射文件页，用于实现文件映射、共享映射和减少数据复制。

### 5\.8\.1 mmap 流程

```Plain Text
// 用户调用 mmap
mmap(addr, length, prot, flags, fd, offset)
       │
       ▼
  syscall 处理
       │
       ▼
  创建 Vma
       │
      ├── 匿名映射 (MAP_ANONYMOUS) ──> Vma.map_file = None
      └── 文件映射 ──> Vma.map_file = Some(...)
       │
       ▼
  插入 VmaSet
       │
       ▼
  返回用户虚拟地址
```

### 5\.8\.2 Lazy Mapping 策略

mmap 执行时不立即分配物理帧，而是：

1. 在 VmaSet 中记录映射信息；

2. 在用户访问时触发 Page Fault；

3. 由 Page Fault Handler 按需分配/加载数据。

### 5\.8\.3 MAP\_PRIVATE 与 CoW

当使用 `MAP_PRIVATE` 映射文件时：

```Plain Text
mmap(fd, MAP_PRIVATE)
       │
       ▼
  建立 MAP_PRIVATE 文件映射 VMA
       │
       ▼
  首次读取 ──> 通过 PageCache 读取文件数据
       │
       ▼
  首次写入 ──> 触发 CoW，复制页面到新帧
```

文件映射缺页通过 filemap/PageCache 相关路径建立映射。文件访问与内存映射之间的一致性由 MAP_SHARED/MAP_PRIVATE 语义、CoW、dirty 标记以及 writeback 路径共同维护。

---

## 5\.9 交换与压缩机制（feature\-gated）

MangoCore 支持 feature\-gated 的 OOM handler、zram 压缩内存和 swap 交换机制。

源码位置：

- `os/src/mm/frame_store.rs:41` → swap out/in 支持

- `os/src/mm/zram.rs:29` → `Zram`

- `os/src/mm/zram.rs:141` → `ZRAM_DEVICE`

### 5\.9\.1 Zram 设计

```rust
pub struct Zram {
    compressed: Vec<Option<Vec<u8>>>,
    recycled: Vec<u16>,
    tail: u16,
}

lazy_static! {
    pub static ref ZRAM_DEVICE: Arc<Mutex<Zram>> =
        Arc::new(Mutex::new(Zram::new(2048)));
}
```

Zram 在内存中维护一个压缩块设备，通过 LZ4 等算法压缩数据，实现内存的压缩存储。

### 5\.9\.2 Swap 设计

```Plain Text
// os/src/mm/frame_store.rs:41
// swap out/in 支持
```

当物理内存不足时，系统可以将不活跃页面交换到交换分区（或 zram 设备），释放物理内存供其他进程使用。

OOM/zram/swap 均由 feature 控制。`Cargo.toml` 的 Cargo default features 不包含 `oom_handler`；项目常用的 board features 和 Makefile 构建路径会显式启用 `oom_handler`，并由该 feature 间接启用 `swap` 与 `zram`。

---

## 5\.10 TLB 管理

虚拟地址到物理地址的映射通过页表实现，页表修改后需要刷新 TLB（Translation Lookaside Buffer）以保证一致性。

### 5\.10\.1 TLB 刷新机制

MangoCore 在不同架构下实现对应的 TLB 刷新操作：

- RISCV64：使用 `sfence.vma` 指令刷新 TLB；

- LoongArch64：使用对应硬件指令刷新 TLB。

### 5\.10\.2 刷新时机

TLB 刷新发生在以下场景：

1. 页表映射修改（mprotect、munmap 等）；

2. 进程切换（AddressSpace 切换）；

3. CoW 完成后更新页表；

4. mmap 映射建立/撤销。

## 5\.11 本章小结

本章介绍 MangoCore 内存管理子系统。物理页由 StackFrameAllocator 管理，内核堆由 OomAwareAllocator/Heap\<32\> 管理，用户虚拟地址空间由 AddressSpace/VmaSet/Vma 管理，缺页异常处理覆盖 lazy allocation、文件映射、CoW 以及 feature\-gated 的 OOM/zram/swap 路径。

与早期版本相比，当前内存管理已经从简单页分配扩展到分层抽象、按需分配、共享与 CoW、交换与压缩等机制。

---

# 第6章 文件系统子系统设计

## 6\.1 设计背景

文件系统连接应用程序与存储设备，承担路径解析、文件对象管理、缓存和设备访问等职责。MangoCore 支持 BusyBox、Shell、Lua 解释器以及标准 Linux 应用程序后，文件访问成为系统调用路径中的高频操作。

MangoCore 的文件系统架构参考了 DragonOS 的 VFS/MountFS 设计模式，在此基础上进行适配和扩展。当前文件系统实现包括：

- VFS 层：File、IndexNode、FileSystem、MountFS 统一抽象；

- 具体文件系统：ext4、FAT32、tmpfs、ramfs、procfs、sysfs、devfs；

- 缓存层：PageCache（文件数据缓存）和 ext4 metadata cache（元数据缓存）。

文件系统架构以统一抽象和职责分离为设计目标：VFS 提供公共访问接口，具体文件系统保留自身数据结构和元数据管理逻辑。

---

## 6\.2 文件系统总体架构

MangoCore 采用统一分层设计，将文件访问划分为应用层、VFS 层、缓存层以及设备层。

整体架构如下：

```Plain Text
User Program
       │
       ▼
    libc
       │
       ▼
  read/write
       │
       ▼
  Virtual File System (VFS)
       │
       ▼
  ┌───────────────────────────────────────────────┐
  │                    File                        │
  │         (打开文件状态：偏移量/权限/引用)        │
  └───────────────────────────────────────────────┘
       │
       ▼
  ┌───────────────────────────────────────────────┐
  │                 IndexNode                      │
  │         (文件元数据：inode/操作接口)           │
  └───────────────────────────────────────────────┘
       │
       ▼
  ┌───────────────────────────────────────────────┐
  │              MountFS / FileSystem              │
  │         (具体文件系统实例/挂载管理)            │
  └───────────────────────────────────────────────┘
       │
       ├──────────┬──────────┬──────────┬─────────┐
       ▼          ▼          ▼          ▼         ▼
     ext4      FAT32     tmpfs    procfs    sysfs  (具体 FS)
       │          │
       ▼          ▼
  PageCache  ext4 metadata cache
       │
       ▼
  VirtIO Block Driver
```

通常情况下，用户程序通过 VFS 接口访问文件，具体文件系统实现由 VFS 分发到对应 IndexNode/FileSystem 实现。公共路径处理打开文件状态、偏移量、权限和挂载关系，具体读写逻辑由各文件系统承担。

---

## 6\.3 Virtual File System 设计

VFS 采用 File、IndexNode、FileSystem 和 MountFS 四层抽象。

### 6\.3\.1 File 层

File 负责维护打开文件状态：

以下摘录保留关键字段；部分 impl、trait bound 和默认方法未列出。

```rust
pub struct File {
    pub inode: Arc<dyn IndexNode>,
    offset: AtomicUsize,
    flags: AtomicU32,
    mode: FileMode,
    file_type: FileType,
    private_data: Mutex<FilePrivateData>,
    open_file_id: usize,
    posix_lock_key: (usize, usize),
    created_by_open: bool,
    owner: Mutex<FileOwner>,
    pub file_rw_hint: Mutex<u64>,
    pub lease: Mutex<Option<i16>>,
}
```

### 6\.3\.2 IndexNode 层

IndexNode 负责表示文件对象本身，维护 inode 信息和统一操作接口：

以下摘录保留关键接口；部分默认方法和参数命名细节未列出。

```rust
pub trait IndexNode: Any + Send + Sync + Debug {
    fn read_at(
        &self,
        offset: usize,
        len: usize,
        buf: &mut [u8],
        data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr>;

    fn write_at(
        &self,
        offset: usize,
        len: usize,
        buf: &[u8],
        data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr>;

    // trait 还包含目录、元数据、page_cache、ioctl、poll 等接口。
}
```

### 6\.3\.3 FileSystem 层

FileSystem 负责维护具体文件系统实例，实现实际读写逻辑：

以下摘录保留关键接口；部分默认方法未列出。

```rust
pub trait FileSystem: Any + Send + Sync + Debug {
    fn root_inode(&self) -> Arc<dyn IndexNode>;
    fn info(&self) -> FsInfo;
    fn name(&self) -> &str;
    fn super_block(&self) -> SuperBlock;
    fn statfs(&self, inode: &Arc<dyn IndexNode>) -> Result<SuperBlock, SyscallErr>;
    // trait 还包含 readahead、permission policy、on_umount 等接口。
}
```

### 6\.3\.4 整体关系

```Plain Text
Application
       │
       ▼
    File (打开文件状态)
       │
       ▼
    IndexNode (文件元数据 + 操作接口)
       │
       ▼
    FileSystem (具体文件系统实例)
       │
       ├───────┬───────┬───────┬─────────┐
       ▼       ▼       ▼       ▼         ▼
    ext4   FAT32  tmpfs  procfs  sysfs  (具体 FS)
```

应用程序通过统一 File 接口访问常规文件操作，底层文件系统类型由 VFS 分发路径处理。

---

## 6\.4 Mount 管理机制

Mount 管理框架用于支持多文件系统并存。

源码位置：`os/src/fs/vfs/mount.rs`

### 6\.4\.1 MountFS 与 MountList 结构

MountFS 包装具体文件系统并记录挂载相关状态；全局 MountList 维护挂载路径到 MountFS 的映射：

```Plain Text
MountFS
├── inner_filesystem: Arc<dyn FileSystem>
├── root_inner_inode: Option<Arc<dyn IndexNode>>
├── mountpoints: Mutex<BTreeMap<InodeId, Arc<MountFS>>>
├── mount_flags: Mutex<MountFlags>
├── mount_source: Mutex<Option<String>>
├── mount_path: Mutex<Option<String>>
└── dentry_cache: Mutex<DentryCache>

MountList
└── mounts: Mutex<BTreeMap<Arc<MountPath>, Vec<MountRecord>>>
```

### 6\.4\.2 文件查找流程

```Plain Text
Path Lookup (路径查找)
       │
       ▼
  从当前 MountFS 开始
       │
       ▼
  逐级查找 Dentry
       │
       ▼
  需要跨越挂载点？──No──> 继续查找
       │
      Yes
       │
       ▼
  切换到目标 MountFS
       │
       ▼
  继续查找
```

`MountFS` 和全局 `MountList` 负责管理挂载关系，使 ext4、tmpfs、procfs 以及 sysfs 能够同时存在于统一目录树中。

---

## 6\.5 PageCache 缓存设计

PageCache 是文件数据缓存组件。支持 PageCache 的文件数据访问路径会通过 PageCache 查询或填充缓存页；其他路径可直接由对应文件系统实现处理。

源码位置：`os/src/fs/page_cache.rs`

### 6\.5\.1 核心结构

```rust
pub struct PageCache {
    inner: Mutex<InnerPageCache>,
    backend: Mutex<Option<Arc<dyn PageCacheBackend>>>,
    inode: Mutex<Option<Weak<dyn IndexNode>>>,
    entries: Mutex<Vec<Option<Arc<PageEntry>>>>,
    unevictable: AtomicBool,
}
```

### 6\.5\.2 访问流程

```Plain Text
支持 PageCache 的文件读路径
       │
       ▼
  VFS / IndexNode
       │
       ▼
  PageCache.lookup(offset)
       │
       ▼
  Cache Hit? ──Yes──> 返回数据
       │
      No
       │
       ▼
  PageCacheBackend.read(offset)
       │
       ▼
  Block Device 读取
       │
       ▼
  创建 Page，插入 Cache
       │
       ▼
  返回数据
```

### 6\.5\.3 Dirty 跟踪与 WriteBack

PageCache 的 `inner: Mutex<InnerPageCache>` 内部维护 `dirty_pages: BTreeSet<usize>`，用于记录脏页索引。

```Plain Text
// os/src/fs/page_cache.rs:789
pub fn writeback_all(&self) -> Result<(), SyscallErr> {
    // 从 InnerPageCache.dirty_pages 取出脏页索引
    // 逐页调用 writeback_page 写回后端存储
}
```

Dirty 跟踪属于 PageCache 内部机制，由 `InnerPageCache.dirty_pages` 与 writeback 路径共同维护。

---

## 6\.6 ext4 元数据缓存

MangoCore 为 ext4 文件系统实现了专门的 metadata cache。

### 6\.6\.1 设计目的

ext4 文件系统的元数据访问包括 inode、bitmap、directory、journal 等内容，metadata cache 用于减少这些路径上的重复块设备访问。

### 6\.6\.2 缓存范围

ext4 metadata cache 仅缓存 ext4 文件系统的元数据：

```Plain Text
ext4 文件系统
       │
       ▼
  ext4 metadata cache
       │
       ├── SuperBlock
       ├── Inode Table
       ├── Block Bitmap
       ├── Inode Bitmap
       ├── Directory Entries
       └── Journal Metadata
```

### 6\.6\.3 与 PageCache 的关系

ext4 metadata cache 和 PageCache 是并列关系，而非上下层关系：

- PageCache：缓存文件数据（普通文件内容）；

- ext4 metadata cache：缓存 ext4 文件系统的元数据（inode、bitmap 等）。

两者各自独立，分别服务于不同的数据访问需求。

MetaBlockCache 仅在 ext4 上下文中存在，不适用于 tmpfs、procfs、sysfs 等文件系统。

---

## 6\.7 文件访问流程分析

一次 read 系统调用的典型流程如下：

```Plain Text
Application
       │
       ▼
  read()
       │
       ▼
  Syscall Handler
       │
       ▼
  VFS File.read()
       │
       ▼
  IndexNode.read()
       │
       ▼
  PageCache 查找
       │
       ▼
  Cache Hit? ──Yes──> 复制数据到用户空间 ──> 返回
       │
      No
       │
       ▼
  PageCacheBackend.read()
       │
       ▼
  具体文件系统实现 (ext4/FAT32/...)
       │
       ▼
  Block Driver
       │
       ▼
  插入 PageCache
       │
       ▼
  复制数据到用户空间
       │
       ▼
  返回
```

该流程展示了 File、IndexNode、PageCache、具体文件系统和块设备之间的调用关系。

---

## 6\.8 本章小结

本章介绍 MangoCore 文件系统子系统。VFS 四层抽象（File、IndexNode、FileSystem、MountFS）负责公共访问路径，PageCache 负责文件数据缓存，ext4 metadata cache 负责 ext4 元数据缓存，ext4、FAT32、tmpfs、procfs、sysfs 等文件系统提供具体实现。

该文件系统架构参考 DragonOS 的 VFS/MountFS 设计模式，并结合 MangoCore 的 PageCache、挂载管理和具体文件系统实现进行适配。

---

# 第7章 网络子系统设计

## 7\.1 设计背景

网络子系统负责用户程序之间的数据通信，并为远程访问、文件传输和网络测试程序提供系统调用接口。

MangoCore 的网络子系统基于 smoltcp 协议栈实现 TCP/UDP/RAW 等网络协议 socket。

Unix Domain Socket 在内核内部单独实现。网络子系统在此基础上构建与 Linux 兼容的 Socket 接口、设备管理、路由管理和端口绑定机制。

smoltcp 是一个用 Rust 编写的嵌入式网络协议栈，提供了 TCP、UDP、RAW、ICMP 等协议实现。MangoCore 将其集成到内核中，通过 `smoltcp::iface::Interface` 和 `smoltcp::socket::SocketSet` 管理每个网络设备上的 socket 集合。

---

## 7\.2  网络总体架构

MangoCore 将网络系统划分为用户接口层、协议层、路由层、设备管理层以及驱动层。

整体结构如下：

```Plain Text
User Program
       │
       ▼
  Socket API (syscall)
       │
       ▼
  SocketFile / Socket trait
       │
       ▼
  ┌──────────────────────────────┬──────────────────────────┐
  │ Inet sockets                 │ Unix stream/dgram        │
  │ TCP/UDP/RAW/ICMP via smoltcp │ 内核本地通信，不经网卡    │
  └──────────────────────────────┴──────────────────────────┘
       │
       ▼
  ┌───────────────────────────────────────────────┐
  │     Inet 网络管理层（仅网络协议 socket）       │
  │  ┌─────────────────────────────────────────┐  │
  │  │ RouteSocketHandle (usize id)           │  │
  │  ├─────────────────────────────────────────┤  │
  │  │ SocketBinding (ifindex/handle/proto)   │  │
  │  ├─────────────────────────────────────────┤  │
  │  │ DeviceStack (多设备管理)                │  │
  │  ├─────────────────────────────────────────┤  │
  │  │ PortManager (端口分配管理)              │  │
  │  └─────────────────────────────────────────┘  │
       │
       ▼
  smoltcp Interface + SocketSet
       │
       ▼
  VirtIO Net Driver
       │
       ▼
  Hardware
```

该结构将用户态 Socket 接口、协议实现、设备选择和 VirtIO 网卡驱动分离。

---

## 7\.3 smoltcp 协议栈集成

### 7\.3\.1 依赖声明

```toml
[patch.crates-io]
smoltcp = { path = "../dependency/smoltcp" }

[dependencies]
smoltcp = { version = "0.10.0", default-features = false, features = [...] }
```

源码位置：`os/src/net/iface.rs`、`os/src/net/socket/mod.rs`

### 7\.3\.2 Interface 与 SocketSet

每个网络设备（VirtIONet）对应一个 `smoltcp::iface::Interface` 和 `smoltcp::socket::SocketSet`：

```rust
pub struct DeviceStack<'a> {
    pub nic: Arc<dyn Iface>,
    pub device: IfaceDevice,
    pub iface: Interface,
    pub sockets: SocketSet<'a>,
}
```

### 7\.3\.3 协议支持

通过 smoltcp，MangoCore 支持网络协议 socket：

- TCP：面向连接的可靠传输

- UDP：无连接的数据报传输

- RAW：原始 IP 数据包

- ICMP：网络控制消息协议（ping 等）

Unix Domain Socket 由 MangoCore 在内核内部单独实现，用于本机进程间通信。

---

## 7\.4 Socket统一抽象

Socket 是用户程序访问网络资源的接口层抽象，具体协议语义由 TCP、UDP、RAW 和 Unix Domain Socket 等实现承载。

源码位置：`os/src/net/socket/mod.rs`

### 7\.4\.1 Socket 类型

```rust
pub trait Socket: Send + Sync {
    fn bind(&self, endpoint: &Endpoint) -> SyscallRet;
    fn listen(&self) -> SyscallRet;
    fn connect(&self, endpoint: &Endpoint) -> SyscallRet;
    fn accept(&self, sockfd: u32, addr: usize, addrlen: usize) -> SyscallRet;
    fn socket_type(&self) -> PSOCK;
    // trait 还包含 send/recv、poll、shutdown、setsockopt 等接口。
}
```

### 7\.4\.2 SocketFile

Socket 通过文件描述符访问：

```rust
// os/src/net/socket/mod.rs
pub struct SocketFile {
    pub inner: Arc<dyn Socket>,
}
```

### 7\.4\.3 Unix Domain Socket

Unix Domain Socket 由不同具体类型实现，包括 `UnixStreamSocket` 和 `UnixDatagramSocket`；stream/datagram 的状态、缓冲区和监听队列由各自子模块维护。本文在语义层面统称 Unix Domain Socket。

### 7\.4\.4 socketpair

以下摘录保留函数签名和语义摘要。

```rust
// os/src/net/syscall/socketpair.rs
pub fn sys_socketpair(domain: u32, socket_type: u32, protocol: u32, sv: usize) -> isize {
    // 实现负责校验 domain/type/protocol，创建两个 SocketFile，
    // 并将 fd 数组写回用户地址 sv。
}
```

socketpair 系统调用通过 `sys_socketpair` 创建一对已连接的 Unix Domain Socket，并将两个文件描述符写回用户提供的 `sv` 数组。

---

## 7\.5 RouteSocketHandle设计

RouteSocketHandle 是 MangoCore 网络架构中的关键抽象，用于建立 Socket 与网络路由之间的间接关联。

源码位置：`os/src/net/routing.rs`

### 7\.5\.1 核心定义

```Plain Text
// os/src/net/routing.rs:11
pub struct RouteSocketHandle(pub(crate) usize);
```

RouteSocketHandle 是一个间接 ID（usize），由 RoutingManager 分配和管理。

```rust
pub struct SocketBinding {
    pub ifindex: u32,
    pub handle: SocketHandle,
    pub proto: InetProtocol,
}
```

### 7\.5\.2 设计思想

RouteSocketHandle 作为中间层，将 Socket 与具体网络设备解耦：

```Plain Text
Socket
   │
   ▼
RouteSocketHandle (usize id)  ←── 逻辑连接标识
   │
   ▼
SocketBinding
   │
   ├── ifindex (设备索引)
   ├── handle (smoltcp SocketHandle)
   └── proto (协议)
   │
   ▼
DeviceStack 根据 ifindex 查找设备
   │
   ▼
具体网络设备
```

Socket 与具体 smoltcp SocketHandle、设备 ifindex 之间的关系集中记录在 routing/binding 表中；socket 生命周期仍受设备状态、binding 表和协议状态共同约束。

---

## 7\.6 SocketBinding机制

端口绑定是网络系统最容易出现冲突的位置。MangoCore 使用 PortManager 和全局端口表管理 TCP/UDP 端口分配；SocketBinding 主要记录某个网络 socket 在指定设备上的 smoltcp handle 和协议类型。

### 7\.6\.1 设计目的

- 保证端口唯一性；

- 统一管理端口生命周期；

- 支持快速端口查询；

- 避免 fork/dup 后的端口状态不一致。

### 7\.6\.2 绑定流程

```Plain Text
bind()
       │
       ▼
  PortManager / TCP_PORTS / UDP_PORTS
       │
       ▼
  Port Lookup (端口查找)
       │
       ▼
  Available? ──No──> 返回 EADDRINUSE
       │
      Yes
       │
       ▼
  创建或更新端口绑定记录
       │
   ├── 协议
   ├── 本地地址/端口
   └── reuse 相关状态
       │
       ▼
  插入 Binding Table
       │
       ▼
  Socket 保存 Binding 引用
       │
       ▼
  返回成功
```



---

## 7\.7 DeviceStack设计

多 VirtIONet 设备场景下，网络子系统需要管理每个设备对应的接口与 socket 集合。

源码位置：`os/src/net/config.rs`

### 7\.7\.1 核心结构

```rust
pub struct DeviceStack<'a> {
    pub nic: Arc<dyn Iface>,
    pub device: IfaceDevice,
    pub iface: Interface,
    pub sockets: SocketSet<'a>,
}

pub struct NetInterfaceInner<'a> {
    pub stacks: Vec<DeviceStack<'a>>,
    pub bindings: BTreeMap<RouteSocketHandle, SocketBinding>,
}
```

### 7\.7\.2 职责划分

- 每个 DeviceStack 维护对应的 smoltcp Interface 和 SocketSet；

- 路由负责设备选择；

- Socket 通过 binding 信息关联具体设备；

- 多设备场景由 DeviceStack 和 binding 表共同维护。

---

## 7\.8  TCP Fast Path 优化

源码位置：`os/src/net/socket/inet/stream/mod.rs`

### 7\.8\.1 实际实现

```rust
pub struct TcpSocket {
    // ... 其他字段
    fast_route_id: AtomicUsize,
    fast_ifindex: AtomicU32,
    fast_state: AtomicU8,
}
```

---

## 7\.9 PortManager 端口管理

源码位置：`os/src/net/socket/inet/common/port.rs`

PortManager 负责管理 TCP/UDP 端口分配。`PortManager` 是静态方法集合，端口占用状态由全局端口表维护：

```rust
pub struct PortManager;
```

PortManager 确保：

- 同一端口不会被重复绑定；

- 端口释放后可以被重新使用；

- 支持端口复用（SO\_REUSEADDR）。

---

## 7\.10 网络数据流流程

### 7\.10\.1 发送流程

```Plain Text
用户程序 send()
       │
       ▼
  syscall 处理
       │
       ▼
  Socket 层
       │
       ▼
  smoltcp 协议栈
       │
       ▼
  Interface 处理
       │
       ▼
  DeviceStack 查找设备
       │
       ▼
  VirtIO Net Driver 发送
       │
       ▼
  硬件
```

### 7\.10\.2 接收流程

```Plain Text
硬件接收数据包
       │
       ▼
  VirtIO Net Driver
       │
       ▼
  DeviceStack 分发
       │
       ▼
  Interface 处理
       │
       ▼
  smoltcp 协议栈
       │
       ▼
  Socket 层
       │
       ▼
  用户程序 recv()
```

## 7\.11 本章小结

本章介绍 MangoCore 网络子系统。MangoCore 基于 smoltcp 协议栈实现 TCP/UDP/RAW 等网络协议 socket。

Unix Domain Socket 由内核内部实现。

RouteSocketHandle（usize id）和 SocketBinding 记录网络 socket 与设备/handle 的间接关联，DeviceStack 管理设备、Interface 和 SocketSet，PortManager 管理端口分配。

该结构将协议栈集成、设备管理、端口分配和用户接口适配拆分到不同模块。

---

# 第8章 工程优化与调试体系设计

## 8\.1 工程优化设计背景

MangoCore 支持多进程调度、虚拟文件系统、网络协议栈以及 Linux 兼容接口后，Kernel 内部同时维护 Task、Page、File、Socket、Route、Inode 等动态对象。不同模块之间通过引用关系共享资源，调试手段需要覆盖更多运行时状态。

项目开发初期主要依赖串口日志观察系统状态；后续内存、文件系统、网络和任务调度机制增加后，单一日志难以覆盖资源泄漏、阻塞状态和缓存状态等问题。

MangoCore 的调试与观测体系包括：

1. trace ring：通用环形缓冲区跟踪机制；

2. heap\_trace：内核堆分配统计（仅记录 alloc/free 和 PC 地址）；

3. perf/task statistics：全局性能统计与 task/process 资源信息；

4. procfs：Linux 兼容的 `/proc` 文件系统；

5. sysfs：Linux 兼容的 `/sys` 文件系统（部分 feature\-gated）；

6. panic diagnostics 与 OOM handler：panic 诊断输出和 feature-gated OOM 处理路径。

---

## 8\.2 调试总体架构

MangoCore 按资源类别组织调试信息，主要通过 procfs、sysfs、trace ring、heap\_trace、panic diagnostics 和 OOM 相关路径导出运行状态。

整体架构如下：

```Plain Text
MangoCore Kernel
       │
       ├───────────────────────────────────────────────────────┐
       │                                                       │
       ▼                                                       ▼
  procfs (运行时状态)                                    sysfs (系统配置)
       │                                                       │
       ├───────────────────────────────────────────────────────┤
       │                                                       │
       ▼                                                       ▼
  /proc/<pid>/... (进程信息)                           /sys/class/net (网络设备)
  /proc/net/tcp (TCP socket)                          /sys/block (块设备)
  /proc/net/udp (UDP socket)                          /sys/kernel/stats (perf_diag)
  /proc/net/route (路由表)                            /sys/kernel/tracing (perf_diag)
       │                                                       │
       └───────────────────────────────────────────────────────┘
                              │
                              ▼
                   ┌─────────────────────┐
                   │    trace ring       │
                   │  (环形缓冲区跟踪)     │
                   └─────────────────────┘
                              │
                              ▼
                   ┌─────────────────────┐
                   │   heap_trace        │
                   │  (堆分配统计)        │
                   └─────────────────────┘
                              │
                              ▼
                   ┌─────────────────────┐
                   │ panic diagnostics   │
                   │  (panic 状态输出)    │
                   └─────────────────────┘
```

---

## 8\.3 trace ring 跟踪机制

### 8\.3\.1 设计目的

trace ring 是 MangoCore 的通用跟踪机制，用于记录内核运行过程中的关键事件。

### 8\.3\.2 核心特性

- 环形缓冲区设计，避免无限增长；

- 支持事件类型分类；

- 支持按需开启/关闭（feature\-gated）；

- 可通过 sysfs 导出跟踪数据。

### 8\.3\.3 实现范围

- trace ring 框架已实现；

- 具体跟踪点分布在关键路径（syscall、调度、文件操作等）；

- trace ring 的覆盖范围集中在已接入的关键路径，不承担 Task/Page/Socket/File/Route 的通用生命周期埋点。

---

## 8\.4 heap\_trace 内存分析工具

heap\_trace 是 MangoCore 用于跟踪内核堆分配的工具。

源码位置：`os/src/mm/heap_trace.rs`

### 8\.4\.1 核心功能

```Plain Text
// os/src/mm/heap_trace.rs:3
// heap allocator trace

// 输出 active/live/top_live 统计
// 输出 alloc/free 调用 PC 地址
```

### 8\.4\.2 实际输出内容

heap\_trace 输出的信息包括：

- `active`：当前活跃分配数量；

- `live`：当前存活分配数量；

- `top_live`：历史最大活跃分配数量；

- `alloc`：分配操作计数；

- `free`：释放操作计数；

- `PC 地址`：调用者的程序计数器地址。

### 8\.4\.3 heap\_trace 的限制

heap\_trace 不记录：

- 对象类型（无法区分 Task 还是 Page）；

- Owner（无法识别资源所有者）；

- 引用计数（无法追踪引用关系）；

- 生命周期状态（无法追踪状态转换）。

### 8\.4\.4 适用范围

heap\_trace 适合：

- 检测堆内存泄漏（active 持续增长）；

- 分析分配热点（高频 alloc 位置）；

- 辅助定位内存问题（配合日志和 procfs）。

heap\_trace 输出不包含对象类型、Owner、refcount 或生命周期状态字段。

---

## 8\.5 procfs 调试框架

procfs 负责导出 Kernel 动态运行状态，采用 Linux 兼容的路径格式。

源码位置：`os/src/fs/procfs/`

```Plain Text
// os/src/fs/procfs/files/mod.rs:33
// 注册 /proc/* 根节点

// os/src/fs/procfs/pid/*  →  /proc/<pid>/... (进程信息)
// os/src/fs/procfs/files/net_tcp.rs  →  /proc/net/tcp (TCP socket 信息)
// os/src/fs/procfs/files/net_udp.rs  →  /proc/net/udp (UDP socket 信息)
// os/src/fs/procfs/files/net_route.rs →  /proc/net/route (路由表)
```

---

## 8\.6 sysfs 运行状态管理

sysfs 负责导出系统配置和状态信息。

源码位置：`os/src/fs/sysfs/files/mod.rs`

### 8\.6\.1 注册路径

```Plain Text
// os/src/fs/sysfs/files/mod.rs:21,23
// 常规注册：
//   /sys/class/net  (网络设备)
//   /sys/block      (块设备)

// os/src/fs/sysfs/files/mod.rs:25,29
// perf_diag feature 下注册：
//   /sys/kernel/stats     (内核统计)
//   /sys/kernel/tracing   (跟踪数据)
```

### 8\.6\.2 Feature\-gated 路径

### 8\.6\.3 路径可用性

`/sys/kernel/stats` 和 `/sys/kernel/tracing` 在启用 `perf_diag` feature 时可用，默认未开启。

---

## 8\.7 性能统计与任务资源信息

源码位置：`os/src/task/perf.rs`、`os/src/fs/procfs/pid/stat.rs`

### 8\.7\.1 可统计的指标

`task/perf.rs` 主要维护全局性能统计；Task/Process 层另有运行时间、资源使用量和调度相关状态，可通过 procfs 等路径导出。

- 全局调度循环、上下文切换、定时器中断等计数；

- heap/frame/cache/reclaim 等全局统计；

- task/process 自身的运行时间与资源统计；

- `/proc/<pid>/stat` 等 procfs 兼容信息。

### 8\.7\.2 导出方式

- 通过 `/proc/<pid>/stat` 等 procfs 文件导出；

- 通过 sysfs（`perf_diag` feature）导出。

## 8\.8 Zombie 任务检测

MangoCore 支持 task/process zombie 状态管理和检测。

源码位置：`os/src/task/manager.rs:90`、`os/src/task/process.rs`

### 8\.8\.1 Zombie 队列

```Plain Text
// os/src/task/manager.rs:90
// 任务级 zombie_queue
```

### 8\.8\.2 Zombie 处理

- Task 层：退出后的任务进入 task zombie 队列，供调度管理路径清理任务级资源；

- Process 层：父进程通过 wait/wait4 回收可等待的 zombie 子进程状态；

- 这两个层次相关但不等价：task zombie queue 属于调度管理路径，wait/wait4 zombie 子进程状态属于进程语义；

- 相关统计可通过诊断路径或 feature-gated 输出辅助观察。

### 8\.8\.3 覆盖范围

Zombie 状态管理限定在 Task/Process 层；Page、Socket、File、Route 等对象由各自子系统的生命周期和回收路径管理。

---

## 8\.9 Panic Diagnostics 与 OOM 处理

### 8\.9\.1 Panic 处理

当 Kernel 发生 panic 时，panic handler 会调用 `panic_diag.rs` 中的诊断逻辑。诊断输出范围由 panic 处理代码和运行日志决定。

1. 打印 panic 信息和调用栈；

2. 导出当前 CPU 状态；

3. 输出部分诊断信息；

4. 停止系统执行（或重启）。

### 8\.9\.2 OOM 处理

当物理内存耗尽且 `oom_handler` feature 启用时，相关路径由 `oom_handler`、heap allocation recovery、pending OOM kill 等机制共同处理：

1. 调用 `oom_handler(req)` 尝试释放或换出页面；

2. 尝试回收内存；

3. 如果回收失败，分配路径返回失败或触发后续 OOM 处理；

4. 在安全点对 pending OOM kill 等状态进行处理。

## 8\.10 本章小结

本章介绍 MangoCore 的调试与观测体系，包括：

- trace ring：环形缓冲区跟踪机制；

- heap\_trace：内核堆分配统计（alloc/free/PC 地址）；

- perf/task statistics：全局性能统计以及 task/process 资源信息；

- procfs：Linux 兼容的 `/proc` 文件系统；

- sysfs：Linux 兼容的 `/sys` 文件系统（部分 feature\-gated）；

- panic diagnostics 与 OOM handler：panic 时输出诊断信息，OOM 路径按 feature 和分配失败路径处理。

上述组件分别对应源码中的具体实现路径，覆盖堆分配统计、运行状态导出、跟踪缓冲区、panic 诊断和 OOM 相关处理。

---

# 第9章 Benchmark驱动优化与测试验证

## 9\.1 Benchmark设计理念

Benchmark 和兼容性测试用于验证 Kernel 功能行为和部分性能表现。MangoCore 对架构调整、模块重构以及性能优化保留测试日志和归档数据，并通过回归测试检查功能退化。

测试体系覆盖功能正确性和部分性能表现，并通过归档日志记录资源、网络、文件系统等测试结果；长期稳定性和完整回归门禁以具体测试 artifact 为依据。

当前测试框架已建立，但部分性能数据尚未收集或未归档。本章仅报告有归档数据支撑的测试结果。

---

## 9\.2 实验平台与测试环境

### 9\.2\.1 测试环境

### 9\.2\.2 测试配置

测试配置位于 `scripts/run_full_test.py`：

```python
# scripts/run_full_test.py:33
QEMU_TIMEOUT = int(os.environ.get("QEMU_TIMEOUT", "7200"))
```

因此默认超时为 7200 秒，但可通过环境变量覆盖。

### 9\.2\.3 测试归档

测试结果归档于 `testresult/archive_YYYYMMDD_HHMMSS/`，包括：

- `summary.txt`：测试摘要（通过率）

- `output-rv64.txt`：RISCV64 测试日志

- `output-la64.txt`：LoongArch64 测试日志

---

## 9\.3 Benchmark总体框架

整个Benchmark平台主要划分为功能测试、性能测试、QEMU运行与回归验证几个部分。

```Plain Text
Benchmark
       │
       ├─────────────────────────────────────────────────┐
       │                                                 │
       ▼                                                 ▼
  功能测试 (Function Test)                      性能测试 (Performance Test)
       │                                                 │
  BusyBox                                          IOzone
  libc-test                                        iperf
  Lua (基础功能)                                    (部分未通过)
       │                                                 │
       └─────────────────────────────────────────────────┘
                              │
                              ▼
                     QEMU 运行记录
                              │
                    默认 timeout 7200s，可覆盖
                              │
                              ▼
                     回归测试 (Regression Test)
```

---

## 9\.4 Linux兼容性验证

Linux 兼容能力通过 BusyBox、libc\-test 以及 Lua 等用户程序进行验证。

### 9\.4\.1 BusyBox 验证

BusyBox 覆盖 Shell、文件管理、进程管理等大量系统调用。

测试内容包括但不限于：

```Plain Text
ls, cp, mv, cat, echo, mkdir, find, grep, ps, sh
```

测试结果（`testresult/archive_20260616_033630/summary.txt`）：

归档数据显示 BusyBox 大部分基础命令通过，覆盖了常用文件、Shell 和进程管理相关系统调用路径。

### 9\.4\.2 libc\-test 验证

libc\-test 主要验证 POSIX 兼容性，覆盖 fork、exec、wait、pipe、dup、mmap、fcntl、signal 等。

测试结果：

归档 summary 中直接可见的 RV64 数据显示，glibc libctest 通过数低于 musl libctest。LA64 精确 pass/all 或百分比需要对应解析 artifact 支撑。

### 9\.4\.3 Lua 运行验证

Lua 解释器属于典型 CPU 与 Memory 混合负载。

Lua 能够运行简单脚本；归档中未记录运行次数和长期稳定性数据。

## 9\.5 文件系统 Benchmark 分析

文件系统采用 IOzone 进行验证。

### 9\.5\.1 测试内容

IOzone 测试包括：

- Write

- Rewrite

- Read

- Reread

- Random Read

- Random Write

### 9\.5\.2 测试结果

IOzone 通过率较低，表明文件系统相关路径仍有未通过测试项。

### 9\.5\.3 PageCache 说明

- PageCache 框架已实现；

- 当前未实现 PageCache 命中率统计机制；

- 归档中未记录 PageCache 命中率数据，因而不报告命中率变化。

---

## 9\.6 网络 Benchmark 分析

网络测试采用 iperf 进行吞吐测试。

---

## 9\.7  回归验证体系

MangoCore 提供自动化测试脚本和 GitHub Actions 工作流；不同入口的覆盖范围不同。

### 9\.7\.1 验证流程

项目中可验证的回归入口包括：

```Plain Text
develop push / workflow_dispatch
       │
       └── .github/workflows/ci-develop.yml
           └── 编译 + basic/busybox (mask=0x003)

main push / workflow_dispatch
       │
       └── .github/workflows/ci-main.yml
           └── scripts/run_full_test.py 完整测试入口

本地完整测试
       │
       └── python3 scripts/run_full_test.py
```

### 9\.7\.2 说明

- 回归测试脚本存在（`scripts/run_full_test.py`）；

- develop CI 的范围是编译与 basic/busybox，不覆盖完整测试矩阵；

- main CI 提供完整测试入口，触发范围由分支保护和工作流规则决定；

- 具体测试通过阈值和性能回归检测机制当前未在文档中详细定义。

## 9\.8 本章小结

本章报告 MangoCore 的 Benchmark 与兼容性测试数据。测试框架位于 `scripts/run_full_test.py`，本章引用的测试结果归档于 `testresult/archive_20260616_033630/`。

关键测试结果：

- RV64 BusyBox：glibc 53/55，musl 53/55；

- RV64 libctest：glibc 177/220，musl 213/220；

- RV64 IOzone：glibc 5/20，musl 7/20；

- RV64 iperf：glibc 0/6，musl 0/6，日志中出现 Connection refused；

- `summary.txt` 中存在 `lmbench-glibc 37/36` 和 TOTAL 浮点异常值，因此本文不引用总通过率，仅引用逐组 pass/all。

本文只引用归档中直接可见的逐组 pass/all 数据；LA64 精确百分比需要补充可复现的解析 artifact。

# 第10章 MangoCore内核演进总结与工程化展望

---

## 10\.1 项目整体回顾

MangoCore 项目始于操作系统基础框架搭建，目标是构建具备 Linux 兼容能力、模块化设计、可在 RISCV64 和 LoongArch64 双架构运行的 Rust 操作系统内核。

项目开发过程中完成了内存管理、任务调度、文件系统、网络协议栈、设备驱动、调试观测等模块实现，并建立以下工程流程：

- 双架构统一编译与测试；

- Docker 统一开发环境；

- 自动化测试框架；

- 模块化架构设计；

- Linux 兼容系统调用接口；

- 内核状态观测与调试工具。

- 覆盖内存管理、文件系统、网络通信、进程管理、调试体系以及测试验证的操作系统框架。

---

## 10\.2 当前内核架构总览

MangoCore 当前架构如下：

```Plain Text
User Space
       │
       ▼
  POSIX Interface (Syscall)
       │
       ├─────────────────┼─────────────────┐
       │                 │                 │
       ▼                 ▼                 ▼
  Process (PCB)     File System        Network
       │                 │                 │
  TaskManager        VFS (VFS)        Socket Layer
  WaitQueue          │                 │
  Signal             │                 │
       │         ├────┼────┤      RouteSocketHandle
       │         │    │    │      SocketBinding
       ▼         ▼    ▼    ▼      DeviceStack
  Memory       ext4 FAT32 tmpfs   PortManager
       │      procfs sysfs        │
  AddressSpace    │               ▼
  VmaSet/Vma      ▼           smoltcp (协议栈)
  PageTable   PageCache         │
  StackFrame   │                ▼
  Allocator    ▼           VirtIO Net Driver
  Heap<32>   Block Driver
       │         │
       └─────────┼─────────────┘
                 │
                 ▼
           VirtIO Block / Net
                 │
                 ▼
             Hardware
```

该架构按用户接口、进程/内存/文件系统/网络、驱动和硬件层次组织。

---

## 10\.3 主要实现内容

基于源码实现，MangoCore 包括以下内容：

### 10\.3\.1 双架构 Rust no\_std 内核

- 支持 RISCV64 和 LoongArch64 两种架构；

- 统一的交叉编译和测试流程；

- Docker 统一开发环境（`docker-compose.yml`，使用预构建镜像 `zhouzhouyi/os-contest:20260104`）；

- Makefile 驱动的构建和测试（`Makefile`）；

### 10\.3\.2 Linux 系统调用兼容面扩展

- 支持 Linux 兼容系统调用接口；

- POSIX 兼容性验证（BusyBox、libc\-test）；

- 支持标准 ELF 程序加载与执行；

### 10\.3\.3 文件系统

- 参考 DragonOS 的 VFS/MountFS 设计模式；

- 统一 VFS 抽象：File、IndexNode、FileSystem、MountFS；

- 具体文件系统支持：ext4、FAT32、tmpfs、ramfs、procfs、sysfs、devfs；

- PageCache 文件数据缓存；

- ext4 元数据缓存；

### 10\.3\.4 网络系统

- 基于 smoltcp 协议栈的 TCP/UDP/RAW 支持；

- 内核内部 Unix Domain Socket 支持；

- SocketFile 文件接口适配；

- RouteSocketHandle（usize id）与 SocketBinding；

- DeviceStack 多设备管理；

- PortManager 端口管理；

- Unix Domain Socket 实现（含 socketpair）；

### 10\.3\.5 内存管理

- StackFrameAllocator 物理页管理；

- OomAwareAllocator / Heap\<32\> 内核堆（buddy\_system\_allocator）；

- AddressSpace/VmaSet/Vma 虚拟内存管理；

- Lazy allocation 按需分配；

- File\-backed mmap；

- Copy\-on\-Write（CoW）；

- Feature\-gated OOM handler；

- Feature\-gated zram 压缩内存；

- Feature\-gated swap 交换支持；

### 10\.3\.6 进程管理

- TCB（TaskControlBlock）线程级调度实体；

- PCB（ProcessControlBlock）进程级对象；

- 100 Hz tick\-based 抢占式调度；

- WaitQueue 统一等待机制；

- Signal 异步事件通知；

### 10\.3\.7 事件通知机制

- epoll（`os/src/fs/eventpoll.rs`）；

- eventfd（`os/src/fs/eventfd.rs`）；

- timerfd（`os/src/fs/timerfd.rs`）；

- signalfd（`os/src/syscall/process/signal.rs`）；

- pidfd（`os/src/fs/pidfd.rs`）；

### 10\.3\.8 调试与观测体系

- trace ring 环形缓冲区跟踪；

- heap\_trace 堆分配统计；

- 全局 perf 统计与 task/process 资源信息；

- procfs Linux 兼容 `/proc`；

- sysfs Linux 兼容 `/sys`（部分 feature\-gated）；

- panic diagnostics 与 OOM handler 相关诊断；

### 10\.3\.9 测试基础设施

- `scripts/run_full_test.py` 自动化测试；

- 测试分组管理（busybox、libctest、iozone、iperf 等）；

- 双架构 QEMU 测试；

- 测试结果归档与摘要；

---

## 10\.4 与早期版本的主要差异

本节所称早期版本指项目初期实现阶段，用于和当前实现进行阶段性对比。

---

## 10\.5 第三方依赖与参考

MangoCore 在设计和实现中参考/使用了以下开源项目：

- DragonOS：VFS/MountFS 等设计参考；

- smoltcp：TCP/UDP/RAW 等网络协议栈基础；

- buddy_system_allocator：内核堆底层分配器；

- virtio-drivers：VirtIO 块设备和网卡驱动基础；

- RustSBI/OpenSBI：启动和机器态固件环境。

---

## 10\.6 当前测试结果

基于归档数据（`testresult/archive_20260616_033630/`）：

- RV64 BusyBox：glibc 53/55，musl 53/55；

- RV64 libctest：glibc 177/220，musl 213/220；

- RV64 IOzone：glibc 5/20，musl 7/20；

- RV64 iperf：glibc 0/6，musl 0/6；

- 该归档的 `summary.txt` 存在 lmbench 与 TOTAL 汇总异常，因此不引用总通过率。

---

## 10\.7 后续发展规划

### 10\.7\.1 已实现

- TCB/PCB 分层任务模型；

- AddressSpace/VmaSet/Vma 虚拟内存管理；

- PageCache、CoW、file-backed mmap；

- epoll、eventfd、timerfd、signalfd、pidfd 等事件类接口；

- smoltcp TCP/UDP/RAW；

- 独立的 Unix Domain Socket。

### 10\.7\.2 部分实现

- Linux syscall 兼容面仍存在语义边界差异；

- glibc、IOzone、iperf 等测试仍有未通过项；

- 调试设施覆盖部分关键状态，但生命周期追踪范围未扩展到所有对象类型。

### 10\.7\.3 未实现

- 未实现项以 issue、测试失败记录或明确源码缺口为依据。

### 10\.7\.4 设想

- 后续工作可围绕测试失败项、性能热点和缺失 syscall 语义逐项推进；规划性内容需标注为未来工作。

---

## 10\.8 核心设计原则总结

项目实践中形成以下设计原则：

### 统一抽象

系统在局部子系统内使用明确抽象边界，例如 VFS 的 File/IndexNode/FileSystem、网络的 Socket trait、内存的 AddressSpace/VmaSet、任务管理的 TCB/PCB。不同资源类型由各自子系统管理，不共享单一对象接口。

### 模块解耦

Memory、VFS、Network、Driver、Debug 等目录按职责拆分，模块间通过 Arc、trait object、全局管理器和 feature-gated 路径协同工作。

### 测试驱动验证

重要架构调整会结合功能测试、性能测试或日志归档验证效果；完整 benchmark artifact 仅在对应测试入口运行后产生。

### 工程化开发

项目提供 Docker compose 开发环境、自动化测试脚本、CI 工作流、调试观测接口和测试归档；质量分析以已有测试结果和诊断指标为依据。

---

## 10\.9 本章小结

MangoCore 项目的主要工程成果包括：

1. 大量 Linux 兼容语义在裸机 Rust 内核中的集成与适配；

2. 双架构（RISCV64/LoongArch64）的统一工程化实践；

3. 基于 smoltcp 和 DragonOS 参考设计的模块化网络与文件系统；

4. 内核观测与调试工具；

5. 自动化测试与回归验证体系。

项目围绕模块解耦、统一抽象和测试驱动验证组织实现，形成了可通过源码和测试归档复查的内核架构。

源码实现、测试归档以及“测试—发现问题—修复—回归”的迭代过程共同记录了项目的系统集成路径。
