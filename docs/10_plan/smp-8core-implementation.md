---
title: "MangoCore 双架构 8 核 SMP 实施方案"
category: plan
status: proposed
owner: MangoCore Team
last_updated: 2026-07-21
tags: [smp, rv64, la64, scheduler, ipi, tlb, qemu]
entry_points:
  - "os/src/main.rs"
  - "os/src/task/processor.rs"
  - "os/src/mm/address_space.rs"
code_paths:
  - "os/src/hal/arch/riscv/"
  - "os/src/hal/arch/loongarch64/"
  - "os/src/task/"
  - "os/src/mm/"
related_docs:
  - "docs/10_plan/smp-agent-execution-spec.md"
  - "docs/01_architecture/boot-and-trap.md"
  - "docs/04_mm/page-table-and-tlb.md"
  - "docs/05_process/scheduler.md"
  - "docs/08_testing/README.md"
---

# MangoCore 双架构 8 核 SMP 实施方案

## 1. 目标、边界与完成定义

### 1.1 实施目标

本方案在同一套 SMP 代码路径下，使 MangoCore 在以下 QEMU 配置中稳定运行：

| 架构 | CPU 数量 |
|---|---|
| RISC-V QEMU virt | 1、2、4、8 |
| LoongArch QEMU virt | 1、2、4、8 |

最终实现：

- 8 个 CPU 的启动、在线管理和独立内核栈；
- 每 CPU 当前任务、idle 上下文、运行队列和调度计时；
- CPU 间 IPI、远程唤醒、停止和 TLB shootdown；
- 用户任务在不同 CPU 上并行执行和迁移；
- 正确的跨核 fork、CoW、mmap、munmap、mprotect、exec、信号和退出语义；
- SMP 安全的文件系统、网络、VirtIO、时间和诊断路径；
- getcpu、CPU affinity、membarrier 和 /proc CPU 信息返回真实数据。

### 1.2 不在本轮范围内

- 2K1000LA 实板多核启动；
- K210、FU740、VisionFive2 的多核验收；
- CPU 热插拔、NUMA、SMT 拓扑感知；
- 任意内核指令位置的完全抢占；
- CFS、实时调度类或复杂调度域；
- VirtIO 多队列、网络多队列和文件系统并行性能重构。

2K1000LA 等非 QEMU 平台继续使用
<code>configured_cpu_count() == 1</code>，不得因 SMP 改造破坏现有单核路径。

### 1.3 完成定义

只有同时满足以下条件，本文档状态才可改为 <code>implemented</code>：

- 双架构 1/2/4/8 核运行矩阵全部通过；
- 不再存在全局 PROCESSOR、全局 current-task 裸指针或全局 ready queue；
- 任一 TCB 不可能同时处于两个运行队列或两个 CPU 的 current 槽；
- 所有已发布页表的 PTE 修改均经过统一 TLB shootdown 协议；
- LoongArch ASID 不再由单个 TCB 持有或未经跨核失效直接复用；
- 所有以“当前为单核”为依据的 unsafe Send/Sync 和 static mut 均已消除或重新证明；
- 双架构 8 核连续压力测试无 panic、死锁、任务丢失、重复执行或 stale TLB；
- 单核测试结果不低于 SMP 改造前基线。

## 2. 当前状态与目标架构

### 2.1 当前主要缺口

| 子系统 | 当前状态 | SMP 风险 |
|---|---|---|
| 启动 | RISC-V 所有 hart 共用一个 boot stack；LA64 非零 CPU 永久自旋 | 栈覆盖、AP 无法上线 |
| 初始化 | rust_main() 无条件执行 BSS、MM、驱动、FS 初始化 | 多核重复初始化和全局状态破坏 |
| trap | RISC-V 内核态 trap 直接 panic；LA64 IPI 未实现 | 内核执行期间无法处理 IPI/shootdown |
| current task | 全局 PROCESSOR、裸指针和 relaxed atomic cache | 跨核读到其他 CPU 的任务或悬空引用 |
| 调度 | 全局 VecDeque ready queue | 全局锁争用、重复出队、无法表达 CPU 所有权 |
| timer | 全局 NEXT_SCHED_TICK_NS，中断中直接切换任务 | 多核重复推进时间或在危险位置调度 |
| MM/TLB | PTE 修改只刷新本地 TLB | 远端 CPU 继续使用旧权限、旧物理页 |
| LoongArch ASID | ASID 随 TCB 分配和释放 | 同一 MM 多线程不一致、跨核复用污染 |
| 网络/驱动 | ROUTING_BUF、DMA reservation 等全局状态 | 并发覆盖或错误匹配请求 |
| lwext4 | Send/Sync 依赖单核和 C 全局表 | 多核并发进入 C 状态导致数据竞争 |
| ABI | getcpu 固定返回 0、affinity 仅 bit0、membarrier 空操作 | 用户空间看不到真实 SMP 语义 |

### 2.2 总体结构

~~~mermaid
flowchart TD
    Q["QEMU -smp N"] --> E["架构启动入口"]
    E --> B["CPU0 BSP"]
    E --> A["CPU1..N-1 AP"]
    B --> G["一次性全局初始化"]
    A --> W["AP 启动栈上等待"]
    G --> R["发布 PerCpu、内核页表和 SCHED_READY"]
    R --> L["每 CPU 本地 trap/timer/IPI 初始化"]
    L --> S["每 CPU 调度循环"]
    S --> Q0["本地 RunQueue"]
    S --> I["IPI / 负载均衡"]
    S --> M["MM active mask / TLB generation"]
~~~

采用单一 SMP 内核实现。单核是
<code>online_mask == bit(0)</code> 的退化情况，不维护独立的 legacy 单核调度器。

### 2.3 核心接口

~~~rust
pub const MAX_CPUS: usize = 8;
pub type CpuId = usize;

pub struct CpuMask(u64);

pub fn cpu_id() -> CpuId;
pub fn configured_cpu_count() -> usize;
pub fn online_cpu_mask() -> CpuMask;
pub fn local_cpu_init();
pub fn start_secondary_cpus() -> Result<(), SmpError>;
pub fn send_ipi(targets: CpuMask, reason: IpiReason);
pub fn cpu_idle();
~~~

<code>configured_cpu_count()</code> 使用构建变量
<code>MANGO_CORE_NUM</code>，允许值固定为 1、2、4、8。Makefile 必须把同一个
<code>CORE_NUM</code> 同时传给 Cargo 和 QEMU，避免内核与虚拟机拓扑不一致。

~~~rust
bitflags! {
    pub struct IpiReason: u32 {
        const RESCHEDULE       = 1 << 0;
        const TLB_FLUSH        = 1 << 1;
        const MEMBARRIER       = 1 << 2;
        const TIMER_REPROGRAM  = 1 << 3;
        const STOP             = 1 << 4;
    }
}
~~~

<code>SmpError</code> 至少包含：

- <code>InvalidCpuCount</code>；
- <code>CpuIdOutOfRange</code>；
- <code>UnsupportedFirmware</code>；
- <code>StartFailed { cpu, error }</code>；
- <code>OnlineTimeout { missing }</code>。

### 2.4 Per-CPU 状态

每个 CPU 独占一个 cache-line 对齐的 <code>PerCpu</code>：

~~~rust
pub struct PerCpu {
    processor: IrqSpinLock<Processor>,
    run_queue: IrqSpinLock<RunQueue>,
    nr_running: AtomicUsize,

    online: AtomicBool,
    idle: AtomicBool,
    need_resched: AtomicBool,
    pending_ipi: AtomicU32,

    sched_tick_deadline: AtomicU64,
    active_mm_id: AtomicU64,
    observed_tlb_generation: AtomicU64,

    irq_depth: AtomicUsize,
    preempt_depth: AtomicUsize,

    local_zombies: IrqSpinLock<VecDeque<Arc<TaskControlBlock>>>,
    stats: PerCpuStats,
}
~~~

RISC-V 内核态以 <code>tp</code> 保存 PerCpu 指针，LoongArch64 使用
<code>$r21</code>。用户态对应寄存器必须在 trap 入口先保存、返回前恢复，内核上下文切换不得覆盖 CPU-local 指针。

### 2.5 调度状态与不变量

为 TCB 增加原子调度状态：

~~~text
New / Blocked
    -> Queued(cpu)
    -> Running(cpu)
    -> Queued(cpu) | Blocked | Zombie
~~~

同时保存：

- <code>owner_cpu</code>；
- <code>last_cpu</code>；
- <code>affinity</code>；
- <code>migration_pending</code>。

必须始终满足：

1. 一个任务最多属于一个 runqueue 或一个 CPU 的 current 槽；
2. 唤醒通过 CAS 完成 <code>Blocked -> Queued(cpu)</code>，重复唤醒不得重复入队；
3. 进入阻塞态必须在 WaitQueue 协议保护下完成，防止丢失 wakeup；
4. 不同时持有两个 runqueue 锁；
5. 不嵌套持有 task.inner 与 runqueue 锁；
6. 不跨 context switch、IPI ack 或其他等待点持锁；
7. current_task() 返回克隆的 Arc，删除 current-task 裸指针和伪造的 static 引用。

### 2.6 MM/TLB 协议

每个地址空间持有：

~~~rust
pub struct MmTlbState {
    mm_id: u64,
    active_cpus: AtomicU64,
    generation: AtomicU64,
    observed: [AtomicU64; MAX_CPUS],
    // LoongArch only
    asid: AtomicU16,
    asid_epoch: AtomicU64,
}
~~~

所有已发布页表的修改统一经过 <code>TlbBatch</code>：

~~~rust
pub enum TlbScope {
    User { mm_id: u64, start: usize, end: usize },
    KernelGlobal { start: usize, end: usize },
}

pub struct TlbBatch {
    scope: TlbScope,
    deferred_frames: Vec<FrameTracker>,
}
~~~

提交顺序固定为：

1. 持有 MM/PTE 锁修改页表并记录失效范围；
2. 增加 MM generation，快照 active CPU mask；
3. 释放 MM/PTE 锁；
4. 刷新本地 TLB；
5. 向远端 CPU 发出 shootdown；
6. 等待目标 CPU ack；等待循环自身必须能处理本地 IPI；
7. 收到全部 ack 后才释放被解除映射的物理页。

CPU 激活地址空间时必须先加入 <code>active_cpus</code>，再比较 generation；若 generation
已变化，先刷新本地 TLB，避免和并发 shootdown 错过。

RISC-V 优先使用 SBI RFENCE，探测不到时使用 IPI mailbox。LoongArch64 使用固定大小、无堆分配的
shootdown slot 池和 IPI ack，禁止在 IPI handler 中获取普通内核锁。

LoongArch ASID 改为 MM 所有。ASID 在一个 epoch 内不立即复用；耗尽后执行全 CPU TLB
flush、等待 ack、递增 epoch，再统一重新分配。

## 3. 分阶段实施

### Phase 0：环境、参数化与 RED 基线

#### 实施内容

- 将两架构 Makefile 的 <code>CORE_NUM := 1</code> 改为可覆盖的
  <code>CORE_NUM ?= 1</code>；
- 构建脚本将 <code>MANGO_CORE_NUM</code> 传给 Cargo，并在 build.rs 中声明
  <code>rerun-if-env-changed</code>；
- 所有 run、ktest、regression、诊断和全量测试脚本统一接收 CORE_NUM；
- QEMU 参数统一为：

~~~text
-smp cpus=N,sockets=1,cores=N,threads=1
~~~

- 非 1、2、4、8 的值在构建前直接报错；
- Docker 前置检查执行 pull 和 force-recreate，记录 image ID、repo digest、创建时间及两种 QEMU 版本；
- 不能仅凭容器内显示 9.2.1 判断镜像是否最新；当前
  <code>pull_policy: missing</code> 不会自动刷新已存在的同名 tag，必须以 digest
  和重建后的容器为准；
- 建立 <code>KTEST=smp</code> RED 用例：在线 CPU 数、独立栈、per-CPU 隔离、
  IPI ping-pong、任务唯一运行和 TLB 失效。

#### 退出条件

- 单核双架构基线日志归档；
- 所有测试入口均可显式传递 CORE_NUM；
- 多核 RED 测试稳定暴露当前缺口，而不是超时原因不明。

### Phase 1：BSP/AP 启动与 Per-CPU 基础

#### 实施内容

- rust_main 改为接收 cpu_id 和架构启动参数，并拆分为 BSP 与 AP 入口；
- .bss.stack 扩展为 <code>MAX_CPUS × BOOT_STACK_SIZE</code>，按 CPU ID 选择独立栈；
  该 section 保持位于 sbss 之前，禁止 BSP 清零 AP 正在使用的栈；
- 启动状态、release mask 和内核页表 token 放入不会被 mem_clear() 清除的
  .data.boot；
- CPU0 独占 BSS、堆、物理内存、驱动、文件系统和初始任务初始化；
- AP 只能在 Acquire 观察到 BSP Release 发布的阶段后访问堆、页表和全局对象；
- RISC-V 增加 SBI v0.2 返回结构、BASE extension probe 和 HSM hart_start；
  CORE_NUM 大于 1 且 HSM 不可用时明确失败；
- LoongArch QEMU 中非零 CPU 不再永久循环，改为在自己的启动栈上等待 release；
- 将现有 bootstrap_init()/machine_init() 拆为一次性 global init 和每 CPU local init；
- AP 完成本地 trap、timer、per-CPU 寄存器和 idle context 初始化后设置 online bit；
- BSP 使用有界超时等待目标 mask；超时时打印 missing mask 并停止启动；
- 所有 CPU 在 SCHED_READY 发布后进入各自调度循环。

#### 退出条件

- 1/2/4/8 核均能打印一次且仅一次的 CPU online 记录；
- 每个 CPU 的 boot stack、idle stack、cpu_id() 和 PerCpu 地址互不混淆；
- 全局初始化计数始终为 1；
- 本阶段用户任务仍固定在 CPU0。

### Phase 2：内核 trap、IPI 与 idle

#### 实施内容

- RISC-V 增加真正的内核 trap 汇编入口，完整保存易失寄存器并区分中断和内核异常；
- LoongArch64 扩展现有内核 trap，支持 line-based IPI；
- 内核异常仍 panic；内核 timer/IPI 中断只更新无锁 per-CPU 状态，不直接调度、不获取普通锁；
- 用户 trap 建立完整内核上下文后允许本地中断，使长 syscall 期间仍可响应 shootdown 和 STOP；
- IPI 发布顺序为：先 Release 写 mailbox/reason，再触发硬件 doorbell；接收端 Acquire 读取；
- RESCHEDULE 只设置 need_resched；
- TIMER_REPROGRAM 只根据原子 deadline 重编程本地 timer；
- STOP 关闭本地中断、设置停止 ack 并进入不可返回的 idle；
- idle 路径按“关中断—发布 idle—重查 runqueue/IPI—执行架构 idle—恢复”实现并测试丢失唤醒；
- panic 和 shutdown 向其他在线 CPU 广播 STOP；等待有界 ack 后由 CPU0 执行最终关机。

#### 退出条件

- 双架构 IPI 单播、广播、交叉发送和 10,000 次 ping-pong 无丢失；
- idle CPU 收到远程入队后必定恢复运行；
- 内核态收到 timer/IPI 不 panic，也不会从中断中直接 context switch。

### Phase 3：Per-CPU 调度器与时间系统

#### 实施内容

- 删除全局 PROCESSOR、全局 current-task cache 和全局 ready queue；
- 每 CPU 使用本地 Processor、RunQueue、idle context 和 zombie 回收队列；
- 本地选择继续保留 FIFO fast path 和现有 nice-aware 选择；
- 新任务或被唤醒任务的目标 CPU 选择规则固定为：
  - last_cpu 在线、在 affinity 内且负载不超过最小负载 +1 时优先复用；
  - 否则选择 affinity 内 nr_running 最小的 CPU；
- 远程入队后，如果目标 CPU idle 或任务优先级需要尽快运行，发送 RESCHEDULE IPI；
- idle CPU 只从一个选定 victim 偷取一个允许迁移的任务，整个过程不同时持有两个 runqueue 锁；
- affinity 变化后，正在运行的非法 CPU 设置 migration_pending 和 need_resched；
  已排队任务在出队时重新定向，避免跨队列双锁迁移；
- 安全抢占点仅包括：
  - 返回用户态之前；
  - 显式 yield；
  - block/exit；
  - idle 调度循环；
- timer interrupt 不直接切换任务，只推进本 CPU 100 Hz quantum 并设置 need_resched；
- CPU0 独占全局 timeout、timerfd、console input、网络后台 poll、文件系统 reclaim 和周期性 housekeeping；
- 非 CPU0 插入更早全局 timer 后，释放 timer queue 锁，再向 CPU0 发送 TIMER_REPROGRAM；
- zombie TCB 在退出 CPU 的 idle 栈上回收，禁止在仍使用自身内核栈时释放最后一个 Arc。

#### 退出条件

- CPU-bound 内核任务能分布到全部在线 CPU；
- 同一任务从不并发运行，重复 wake 不会重复入队；
- affinity、迁移、阻塞和远程唤醒压力测试通过；
- 当前阶段普通用户任务仍默认固定 CPU0，避免在 TLB shootdown 完成前跨核运行。

### Phase 4：TLB shootdown、ASID 与用户 MM

#### 实施内容

- 页表 trait 增加明确的 raw/no-flush 内部操作；对已发布页表的公开修改只能通过 TlbBatch；
- 覆盖 unmap、mprotect、CoW、MAP_SHARED 写缺页、匿名缺页、filemap、exec、
  内核栈映射和内核全局映射；
- RISC-V 实现 SBI RFENCE range/all flush，并提供 IPI fallback；
- LoongArch 实现 shootdown slot、generation、ack 和 ASID/epoch；
- shootdown slot 使用固定数组和原子状态，IPI handler 不分配内存、不获取 MM 锁；
- 被解除映射的 frame、页表页和内核栈必须延迟到全部目标 ack 后释放；
- membarrier：
  - GLOBAL 面向所有在线 CPU；
  - PRIVATE_EXPEDITED 面向当前进程 MM 的 active CPU；
  - 使用同一 IPI/ack 基础设施和完整内存屏障；
- 完成 MM 专项测试后，才允许受控用户测试任务跨 CPU 运行。

#### 退出条件

- 一核反复 unmap/protect/CoW，其他核并发访问时不出现旧权限或旧物理页；
- LoongArch 强制 ASID rollover 后无跨进程数据污染；
- shootdown 期间即使目标 CPU 正在执行长 syscall 也能及时 ack；
- frame 释放计数证明不存在 ack 前复用。

### Phase 5：共享子系统与进程语义审计

#### 实施内容

- console 使用全局 irq-safe 锁；panic 路径提供不等待锁的原始 UART fallback；
- TIME_SOURCE、CLOCK_FREQ、timer 计数、LoongArch DIRTY、UART 和诊断缓冲改为原子、
  受锁对象或 per-CPU 状态；
- VirtIO 队列在 v1 中继续单队列串行化；DMA reservation 改为 per-CPU，
  防止不同 CPU 的同步请求互相覆盖；
- smoltcp 保持单实例：
  - 所有 NET_INTERFACE 访问统一串行化；
  - 后台 poll 仅 CPU0 执行；
  - syscall 可在任意 CPU 短暂获取接口锁；
  - ROUTING_BUF 移入受保护对象，删除 static mut；
- lwext4 增加跨实例全局串行锁，因为 C 设备/挂载表为全局状态；
  锁保护区内只允许同步、不可调度的块 I/O，不得 yield 或等待任务事件；
- 审计 PageCache、VFS、FAT、frame allocator、heap/slab、futex、WaitQueue、
  signal、epoll/eventfd 和 pidfd；
- 删除所有仅以“当前单核”为安全依据的 unsafe Send/Sync；确需保留时必须写明真实共享所有权、锁和中断约束；
- exit_group、多线程 exec 和致命信号采用跨核停止协议：
  - queued sibling 原子标记退出并由出队路径丢弃；
  - running sibling 收到 reschedule IPI，在安全点退出并 ack；
  - 发起者等待全部 sibling 停止后才能替换 MM 或释放进程共享资源；
- 完成用户可见 CPU 语义：
  - getcpu() 返回当前 CPU；
  - sched_getaffinity() 返回真实 mask；
  - sched_setaffinity() 保存 mask 并触发必要迁移；
  - /proc/cpuinfo 为每个 configured CPU 输出处理器项；
  - /proc/stat 输出 cpu0..cpuN；
  - 默认任务 affinity 为 configured online mask。

#### 退出条件

- 文件系统、网络、futex、信号和退出压力测试可在 8 核用户任务下运行；
- 仓库中不再有未处理的单核安全注释；
- 普通用户任务默认解除 CPU0 固定并允许全核调度。

### Phase 6：稳定化、性能和文档同步

#### 实施内容

- 增加 per-CPU 诊断计数：
  - context switch、migration、steal；
  - runqueue 长度峰值；
  - 各类 IPI 收发与 ack；
  - TLB shootdown 数量、范围和等待时间；
  - timer interrupt、reschedule；
  - 无效任务状态转换和重复入队；
- panic 时输出所有 CPU 的 current TID、runqueue、IRQ/preempt depth、active MM 和 pending IPI；
- 同步更新启动/trap、调度器、页表/TLB、测试文档和根 AGENTS.md 中的“单核”描述；
- 每个阶段形成独立、可回退提交，不把启动、调度和 TLB 改动压成一次大提交。

## 4. 测试、证据与验收

### 4.1 每阶段强制门禁

严格串行执行：

~~~text
make rv64-kernel-build-only
make la64-kernel-build-only
~~~

随后执行该阶段 focused QEMU 测试，并至少复跑一次 <code>CORE_NUM=1</code>。
双架构编译不得并行，避免 nightly override 竞态。

### 4.2 SMP Ktest

新增 <code>KTEST=smp</code>，至少覆盖：

| 类别 | 场景 |
|---|---|
| 启动 | online mask、独立 boot/idle stack、per-CPU register |
| IPI | 单播、广播、ping-pong、并发 reason、STOP |
| 调度 | 唯一运行、重复 wake、远程 enqueue、steal、affinity、迁移 |
| 同步 | futex、WaitQueue、Completion、eventfd、signal |
| MM | unmap、mprotect、CoW、MAP_SHARED、exec、kernel mapping |
| TLB | generation race、ack 前不释放、目标正在 syscall |
| LA ASID | 多 MM、强制 rollover、epoch 后复用 |
| 进程 | exit_group、fatal signal、多线程 exec |
| FS | 多核 create/read/write/rename/unlink/fsync |
| 网络 | 多 socket 并发、单 poll owner、路由缓冲隔离 |
| 停机 | panic/normal shutdown 停止其他 CPU |

竞争敏感用例使用 <code>KREPEAT=100</code>；失败时打印 CPU、任务状态、队列归属和最后一次
IPI/TLB sequence。

### 4.3 最终矩阵

对两种架构分别执行：

- CORE_NUM=1/2/4/8 的 SMP focused ktest；
- CORE_NUM=1/2/4/8 的 basic + busybox（mask 0x003）；
- CORE_NUM=1 和 CORE_NUM=8 的竞赛 12 组全量（mask 0xFFF）；
- 每种架构、每种 CPU 数量连续启动 10 次；
- 每种架构 8 核混合压力运行至少 30 分钟：
  - CPU worker；
  - fork/exec/exit；
  - futex/pipe/eventfd；
  - mmap/CoW/unmap；
  - ext4 并发；
  - TCP/UDP 并发。

### 4.4 性能判定

不设置固定线性加速要求，因为 QEMU TCG 和宿主调度会显著影响结果，但必须满足：

- 结构上不存在全局 ready queue；
- 使用至少 5 次同宿主、同镜像、同 workload 样本报告 median 和 MAD；
- 8 核 CPU-bound 吞吐不得连续两组低于 4 核，除非差异位于
  <code>max(10%, 2×MAD)</code> 噪声带内；
- 若 8 核明显低于 4 核，必须用 runqueue、migration、IPI、shootdown 和锁等待计数解释后才能验收。

### 4.5 证据归档

每阶段证据统一保存到当天唯一的日期目录：

~~~text
docs/Work_Log/evidence/YYYY-MM-DD/
~~~

同一日期内使用 <code>smp-phase-&lt;N&gt;-</code> 文件名前缀区分阶段，不再创建主题子目录。
每阶段至少包含：

- git commit/worktree 状态；
- Docker image ID、digest 和 QEMU 版本；
- 完整命令与配置；
- 双架构构建日志和退出码；
- QEMU 完整输出和结果判定；
- CPU 数、online mask 和测试重复次数；
- 压力测试统计与性能原始样本。

阶段状态只能写为 <code>not-run</code>、<code>red</code>、<code>partial</code>
或 <code>pass</code>，不得用“预计通过”替代证据。

## 5. 固定假设与实施纪律

- MAX_CPUS=8，configured CPU ID 连续为 0..N-1，CPU0 固定为 BSP；
- v1 使用构建期 MANGO_CORE_NUM，不实现 DTB/ACPI 动态 CPU 枚举；
- QEMU CPU 拓扑固定为单 socket、N core、单 thread；
- 内核采用安全点抢占；中断可打断内核，但不得在任意内核中断点切换任务；
- 单核仍走 SMP 数据结构，不保留第二套调度器；
- 用户任务在 TLB 和共享子系统门禁通过前保持 CPU0 affinity；
- RISC-V HSM/RFENCE 缺失时明确报错或使用本文指定的 IPI fallback，不做静默降级；
- 实板 2K1000LA 始终配置为单核，本计划不宣称实板 SMP 支持；
- 实际进入代码实施后，各阶段必须重新加载 mango-workflow 对应调试参考，并更新 Work Log；
- 状态和测试结论必须引用可复核证据，不得将未执行项目标为通过。

## 6. 外部参考

- [RISC-V SBI Hart State Management Extension](https://github.com/riscv-non-isa/riscv-sbi-doc/blob/master/src/ext-hsm.adoc)
- [RISC-V SBI IPI Extension](https://github.com/riscv-non-isa/riscv-sbi-doc/blob/master/src/ext-ipi.adoc)
- [RISC-V SBI Remote Fence Extension](https://github.com/riscv-non-isa/riscv-sbi-doc/blob/master/src/ext-rfence.adoc)
- [LoongArch Reference Manual, Volume 1](https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html)
