---
title: "MangoCore 双架构 8 核 SMP 实施方案"
category: plan
status: proposed
owner: MangoCore Team
last_updated: 2026-07-30
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
  - "docs/01_architecture/lock-order.md"
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
- Linux rseq ABI；syscall 293 本轮明确返回 ENOSYS，并关闭逐次 unknown-syscall 噪声日志。
  已观察到测试环境中的 glibc 可回退，但这不是对所有 libc/应用的兼容保证。

2K1000LA 等非 QEMU 平台继续使用
<code>configured_cpu_count() == 1</code>，不得因 SMP 改造破坏现有单核路径。

### 1.3 完成定义

只有同时满足以下条件，本文档状态才可改为 <code>implemented</code>：

- 双架构 1/2/4/8 核运行矩阵全部通过；
- 不再存在全局 PROCESSOR、全局 current-task 裸指针或全局 ready queue；
- 任一 TCB 不可能同时处于两个运行队列或两个 CPU 的 current 槽；
- 所有已发布页表的 PTE 修改均经过统一 TLB shootdown 协议；
- LoongArch ASID 不再由单个 TCB 持有或未经跨核失效直接复用；
- RISC-V ASID 由 MM 持有，且复用前已经过全 CPU 失效；
- 所有以“当前为单核”为依据的 unsafe Send/Sync 和 static mut 均已消除或重新证明；
- 双架构 8 核连续压力测试无 panic、死锁、任务丢失、重复执行或 stale TLB；
- 单核测试结果不低于 SMP 改造前基线。

## 2. 当前状态与目标架构

### 2.1 当前主要缺口

| 子系统 | 当前状态 | SMP 风险 |
|---|---|---|
| 启动 | 双架构 8 槽 boot/idle stack、BSP/AP 入口、online、scheduler-ready/entered 和 STOP/ack 已完成 | AP 仅运行受控任务；B29 的单个迁移探针不代表通用生产任务能力 |
| 初始化 | CPU0 独占 BSS/MM/驱动/FS；AP 安装 PerCpu、页表根和本地 trap/IPI 后进入调度循环 | 共享子系统的完整 global/local init 审计仍未完成 |
| trap | 双架构用户 trap 已恢复 CPU-local 寄存器；`current_trap_task()` 校验 `Running(cpu)`；syscall 受控窗口已在 CPU1 实际完成；B33 的 trap-return 安全点可消费远端 RESCHEDULE | 非 syscall 内核区间仍关中断，AP timer/外设 IRQ 仍关闭 |
| current task | current/idle 与不可变诊断快照已拆到 Per-CPU；B33 已验证同一 TCB 从 CPU0 current 经远端 IPI 安全点交给 CPU1、退出和 CPU0 回收 | 普通用户任务默认仍固定 CPU0，通用迁移与进程组停止语义待实现 |
| 调度 | Per-CPU current/idle/RunQueue、AP 精简循环、显式目标发布和受控迁移已完成；B31 用 per-thread `cpus_allowed` 约束三条 owner 交接，B33 让运行中用户任务在返回安全点消费 RESCHEDULE，B34 完成 current 线程运行期改 mask 与必要自迁移，B35 完成远程稳定 Blocked 线程改 mask 与 wake 重定向 | 远程 Running/Blocking/Queued affinity、通用新任务负载选择和 steal 尚未实现 |
| 阻塞任务 | interruptible_queue 同时承担枚举、清理、统计和唤醒辅助 | 与 per-CPU runqueue 职责重叠，旧重复唤醒扫描依赖全局队列 |
| timer | CPU0 hard IRQ 只发布 per-CPU pending；旧 timer 工作与 RESCHEDULE 已在统一任务安全点合并 | 调度 tick 和全局 timer owner 尚未 per-CPU/CPU0 化，AP timer 仍关闭 |
| MM/TLB | `AddressSpace` 统一 VM 锁与 `TlbContext`；`UserMapper/MmuGather` 锁内记录，`TlbFlush` 锁外完成 generation、失效同步和 frame 退休；双架构均使用 MM-owned versioned ASID；RV64 以 `sfence.vma va, asid`/SBI RFENCE FID 2 精确到单页，LA64 以固定 ASID/VPN slot 精确到硬件页对；B29 已让同一 MM 先后在 CPU0/CPU1 激活并在退出时完成双 CPU shootdown | 当前仍使用单调历史 CPU mask；连续 range、安全 detach 与通用用户迁移未完成 |
| 架构 ASID | `TlbContext` 原子保存软件 epoch/硬件 ASID；同一 MM 跨线程/CPU 共享，耗尽时全 CPU flush/ack 后换代；RV64 启动探测 ASIDLEN，LA64 读取 ASIDBITS | 连续 range 尚未实现；多 VPN 仍升级为全用户失效 |
| 网络/驱动 | ROUTING_BUF、DMA reservation 等全局状态 | 并发覆盖或错误匹配请求 |
| lwext4 | Send/Sync 依赖单核和 C 全局表 | 多核并发进入 C 状态导致数据竞争 |
| ABI | B30 已让 getcpu 返回当前连续逻辑 CPU；B31 内核 TCB 已持有真实 `cpus_allowed`；B32 raw `sched_getaffinity` 已按 TID 返回该 mask；B34 的 `sched_setaffinity` 已支持 current TID，B35 支持非 current 的稳定 Blocked TID | 远程 runnable affinity、membarrier 和默认全核 affinity 仍不完整；普通任务当前仍为 bit0 |

### 2.2 总体结构

~~~mermaid
flowchart TD
    Q["QEMU -smp N"] --> E["架构启动入口"]
    E --> B["CPU0 BSP"]
    E --> A["CPU1..N-1 AP"]
    B --> G["一次性全局初始化"]
    A --> W["AP 启动栈上等待"]
    G --> R["发布 PerCpu、内核页表和 SCHED_READY"]
    R --> L["每 CPU 本地 trap/IPI 初始化"]
    L --> P["Phase 2: AP IPI-only idle"]
    P --> S["Phase 3: 每 CPU 调度循环"]
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
7. current_task() 返回克隆的 Arc，删除 current-task 裸指针和伪造的 static 引用；
8. pid/tid 等不可变 per-CPU hint 可作为快路径；parent pid、pgid/sid、credentials、
   user token 等可变 hint 必须有集中更新/失效协议，否则读取权威对象；
9. interruptible_queue 不得成为第二套 runnable queue；其信号枚举、OOM、zombie 清理和
   统计职责迁入任务 registry、WaitQueue 或专用 registry 后，才能退役或降为非运行实体索引。

### 2.6 MM/TLB 协议

每个地址空间持有：

~~~rust
pub struct TlbContext {
    mm_id: u64,
    active_cpus: AtomicU64,
    generation: AtomicU64,
    observed: [AtomicU64; MAX_CPUS],
    // LoongArch only
    asid: AtomicU16,
    asid_epoch: AtomicU64,
}
~~~

所有已发布页表的修改统一经过 <code>MmuGather</code>：

~~~rust
pub enum TlbScope {
    User { mm_id: u64, start: usize, end: usize },
    KernelGlobal { start: usize, end: usize },
}

pub struct MmuGather {
    scope: TlbScope,
    retired_frames: Vec<FrameTracker>,
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
-accel tcg,thread=multi -smp cpus=N,sockets=1,cores=N,threads=1
~~~

- focused 竞态测试必须显式请求 MTTCG，并记录 QEMU 完整命令和宿主侧实际 vCPU 线程；
  若后端或功能组合不支持 MTTCG，证据标记覆盖限制，不得把单 TCG 线程时序当作并行证明；
- 非 1、2、4、8 的值在构建前直接报错；
- Docker 前置检查执行 pull 和 force-recreate，记录 image ID、repo digest、创建时间及两种 QEMU 版本；
- 不能仅凭容器内显示 9.2.1 判断镜像是否最新；当前
  <code>pull_policy: missing</code> 不会自动刷新已存在的同名 tag，必须以 digest
  和重建后的容器为准；
- 建立 <code>KTEST=smp</code> RED 用例：在线 CPU 数、独立栈、per-CPU 隔离、
  IPI ping-pong、任务唯一运行和 TLB 失效；
- 冻结实施基线：记录 commit、分支、dirty status、双架构 1 核日志和成绩；从该 commit
  创建专用 SMP branch/worktree，后续批次不得直接堆叠在持续变化的 develop 上；
- 默认开发核数为 CORE_NUM=2；是否补 CORE_NUM=1 由本工作包是否可能破坏单核退化路径决定。
  4/8 核主要用于并发覆盖或阶段门禁，不在不相关的局部修改后机械重复。

#### 退出条件

- 单核双架构基线日志归档；
- 基线 commit 和隔离 branch/worktree 可复核，当前参考基线成绩 RV64 199.1/200、
  LA64 197.1/200，不把成绩记录误写成 SMP 已验证；
- 所有测试入口均可显式传递 CORE_NUM；
- 多核 RED 测试稳定暴露当前缺口，而不是超时原因不明。

### Phase 0.5：IRQ-safe 原语、锁序与早期 console

#### 实施内容

- 实现并验证双架构 IrqSaveSpinLock/IrqSpinLock：guard 保存原中断状态、关闭本地中断并在
  Drop 时恢复原状态，嵌套严格 LIFO，不得无条件开中断；
- 明确 irq depth 与 preempt depth 的关系；正确性依赖“保存并恢复原状态”，不强制把某个
  depth 计数器当作唯一实现；
- 以 `docs/01_architecture/lock-order.md` 为中央锁契约，先固化 runqueue、task.inner、
  WaitQueue、MM/PTE、timer、console 和 lwext4 的部分序及禁止组合；
- console 在 AP 打印 online 之前改为全局 irq-safe 串行化；panic/STOP 路径提供不等待锁的
  raw UART/SBI fallback，避免最早的多核日志本身产生数据竞争或死锁；
- 增加 guard 嵌套恢复和持锁时 IRQ 重入的单核 focused ktest；准备双 CPU console 用例，
  实际并发证据在 Phase 1 AP 可启动后补齐。

#### 退出条件

- IrqSaveSpinLock 在 enabled→nested→restore 与 disabled→nested→restore 两种初始状态下通过；
- hard IRQ/IPI 路径不获取普通业务锁，锁序文档与代码中的新增关系一致；
- panic fallback 的单核持锁注入测试证明其不等待 console 锁。

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
- AP 完成 per-CPU 寄存器、最小 trap/IPI 向量和 idle context 初始化后设置 online bit；timer
  本阶段只写入配置，不使能本地 timer 中断；
- BSP 使用有界超时等待目标 mask；超时时打印 missing mask 并停止启动；
- CPU0 继续独占生产任务和 run_tasks()；AP 设置 online 后进入只检查
  release/mailbox 的 park loop，本阶段不得调用 run_tasks() 或运行普通任务。
  全局 PROCESSOR 已在后续 B17 拆为 Per-CPU current/idle 状态。

#### 退出条件

- 1/2/4/8 核均能打印一次且仅一次的 CPU online 记录；
- 每个 CPU 的 boot stack、idle stack、cpu_id() 和 PerCpu 地址互不混淆；
- 全局初始化计数始终为 1；
- 双 CPU 并发启动日志不会交叉破坏，panic fallback 不等待被其他 CPU 持有的 console 锁；
- AP 本地 timer/普通中断保持关闭，只能停驻或处理已证明安全的启动 mailbox；
- 本阶段所有内核和用户任务仍只在 CPU0 运行。

#### 当前进度（SMP-P1-B08）

- 已完成双架构 `rust_main(hardware_id, boot_arg)` BSP/AP 分流、
  `.data.boot` Release/Acquire 握手、5 秒有界 online mask 等待和 AP park；
- 已建立 8 项、每项独占 64 字节 cache line 的 `.data.boot` `PER_CPUS`
  表；`register_cpu_entry()` 在逻辑 ID 确定后将对应地址写入 RV64 `tp`
  或 LA64 `$r21`，并在同一 CPU 上立即回读断言；
- 每个 `PerCpu` 已拥有只由本 CPU 写入的 `online: AtomicBool`。CPU 通过
  Release CAS 唯一一次发布本地初始化完成，BSP 通过逐表项 Acquire 扫描
  汇总 online mask；旧全局 `ONLINE_MASK` 已删除，重复发布会明确 panic；
- 已公开 `configured_cpu_count()` 和 `online_cpu_mask()` 运行期查询，
  启动等待、日志和 focused ktest 读取同一权威状态；
- 双架构 `TrapContext` 已增加内核私有 `kernel_cpu_local`；`trap_return()`
  在每次用户返回前按当前 CPU 刷新它，用户 trap 汇编在保存用户 `tp/$r21`
  后、进入 Rust 前重装内核指针；
- 已公开带数组范围、对齐和 configured CPU 校验的运行期 `cpu_id()`。Phase 1
  用户 trap handler 同时断言任务仍只在逻辑 CPU0 运行；
- LA64 LSX 保存区因新增 8 字节字段和 16 字节对齐移至 `72 * 8`，Rust
  `offset_of!` 断言与汇编常量共同阻止布局静默漂移；
- RISC-V 已实现 SBI v0.2 BASE probe 与 HSM `hart_start`。OpenSBI cold-boot
  hart 映射为逻辑 CPU0，不能假设物理 hart 0 固定先启动；
- LoongArch QEMU 已按官方 slave boot ROM 协议实现 mailbox 写入口、`dbar`
  和 IPI vector 0 唤醒；2K1000LA 多核调用明确返回不支持；
- 双架构 `CORE_NUM=1/2/4/8` 均达到期望 online mask，现有 waitqueue ktest
  通过；比赛式省略 `-accel`、使用 `-smp 8` 的双架构命令也通过；
- 双架构 `CORE_NUM=2` 用户态 regression 均达到 `online_mask=0x3`，6/6
  用例通过；最终 ELF 反汇编确认用户寄存器保存、slot 70 CPU-local 加载和
  kernel stack 切换顺序正确；
- B06 新增 `KTEST=smp`，双架构 `CORE_NUM=2` 及 RV64 `CORE_NUM=1`
  均通过 online 拓扑与“旧调度器只运行于 CPU0”断言；ELF 符号确认
  `PER_CPUS` 大小为 `0x200` 且位于 `.data`，不存在 `ONLINE_MASK` 符号；
- 双架构已按 configured CPU 数在普通 BSS 中预留独立 idle stack。AP 的
  naked trampoline 保留 logical ID 和 CPU-local GPR，只切换 `sp` 并进入
  新栈上的 Rust idle 入口；该入口先发布 `idle`，再发布 `online`；
- B08 的双架构 `CORE_NUM=8 KTEST=smp` 均达到 `online_mask=0xff`，新增
  断言证明全部 7 个 AP 已进入 idle context。最终 ELF 中 RV64/LA64 idle
  区分别为 `8×64 KiB`/`8×128 KiB`，位于 `sbss..ebss` 且页对齐；
- 最小内核 trap/IPI 向量和可唤醒 idle loop 已在 B09 完成；其余 PerCpu
  字段、console 多核串行化和全局初始化计数仍未完成，因此整个 Phase 1
  状态仍为 `partial`。

### Phase 2：内核 trap、IPI 与 AP park/idle 唤醒

#### 实施内容

- 先完成 RISC-V 真正的内核 trap 汇编入口，完整保存易失寄存器并区分中断和内核异常；
  LoongArch64 扩展现有内核 trap，支持 line-based IPI；
- 第一子阶段仅开放 IPI 中断窗口：内核异常仍 panic，IPI handler 只更新无锁 per-CPU 状态，
  不调度、不分配、不获取普通锁；
- IPI 发布顺序为：先 Release 写 mailbox/reason，再触发硬件 doorbell；接收端 Acquire 读取；
- RESCHEDULE 只设置 need_resched；
- STOP 关闭本地中断、设置停止 ack 并进入不可返回的 idle；
- AP park/idle 路径按“关中断—发布 idle—重查 mailbox—执行架构 idle—恢复”实现；本阶段
  用 mailbox/IPI 唤醒测试 lost wakeup，不引用尚未存在的远程 runqueue；
- 第二子阶段才接入 timer IRQ：handler 只推进原子 deadline/need_resched 并把到期工作延后到
  安全点；旧 timer 回调、网络 poll、唤醒扫描和 schedule 不得在 IRQ 中直接执行；
- 只有 trap frame、锁序和 deferred timer 门禁完成后，才在受控的长 syscall 区间开放本地
  IPI/timer 中断，以保证后续 shootdown 和 STOP 能及时响应；
- TIMER_REPROGRAM 只根据原子 deadline 重编程本地 timer；
- panic 和 shutdown 向其他在线 CPU 广播 STOP；等待有界 ack 后由 CPU0 执行最终关机。

#### 退出条件

- 双架构 IPI 单播、广播、交叉发送和 10,000 次 ping-pong 无丢失；
- park CPU 收到 mailbox/IPI 后必定恢复检查，IPI-only 与 timer-enabled 两个子阶段证据分开；
- 内核态收到 timer/IPI 不 panic，也不会从中断中直接 context switch。

#### 当前进度（SMP-P2-B09/B10/B11/B12/B13/B14）

- `PerCpu` 已增加原子的 `pending_ipi` 和 PING ack。`IpiReason` 明确
  表示可合并的幂等 reason bit，而不是事件计数；发送方以 Release 发布，
  接收方以 Acquire `swap(0)` 消费；handler 不分配、不打印、不持普通锁，
  也不调度；
- 通用 `send_ipi_mask()` 已支持一次向多个 online AP 发布同一个 reason。
  广播严格分成“先发布全部 mailbox、再触发全部 doorbell”两轮；单个
  doorbell 失败不会阻止其余已发布目标被通知，并在整轮结束后返回首个错误。
  该顺序建立整轮 publication-before-delivery 边界，handler 仍只读本地
  mailbox；
- RV64 已接入 SBI v0.2 IPI extension。AP 的 `stvec` 指向独立内核 trap
  入口，汇编完整保存 GPR、原始 `sp`、`sstatus` 和 `sepc`，只开放 SSIP；
- LA64 QEMU 已把运行期 IPI 固定为 vector 1，与 slave boot ROM 使用的
  vector 0 分离；handler 先清 IOCSR level source，再进入 mailbox fast path；
- 双架构 AP 都从永久 park 改为 `wfi`/`idle 0` 的 IPI-only idle loop，
  仍不进入旧调度器；
- B09 的双架构 `CORE_NUM=2 KTEST=smp` 均通过 4/4，证明最小单播闭环；
  B10 的双架构 `CORE_NUM=4 KTEST=smp` 均通过 5/5，证明一次广播可唤醒
  三个 AP 并独立收到 ack。RV64 本次由硬件 hart 1 担任逻辑 CPU0，实际
  覆盖了逻辑 ID 到硬件 hart ID 的非恒等映射；
- B10 审计发现，CPU0 仍使用会在中断中直接调度的旧 timer handler；因此
  没有通过测试专用关 timer 绕过依赖，AP→CPU0 与交叉发送延后到 deferred
  timer 完成后；
- B11 已把双架构 timer hard IRQ 收敛为“静默 one-shot + Release 发布
  per-CPU pending”的无锁路径。timer queue、callback、timeout/timerfd、
  网络 poll、诊断打印和 schedule 全部移到 `trap_return()` 或 scheduler
  取锁前的安全点；安全点以 Acquire 消费 pending，在关中断状态下按完整
  队列重编程下一次硬件事件；
- B11 的双架构 `CORE_NUM=2 KTEST=smp` 均通过 6/6。新增用例连续执行两轮
  真实内核 timer IRQ，证明 hard IRQ 不执行 deferred 工作、不切换当前
  任务，安全点恰好消费一批且能重新触发下一轮 one-shot；
- B12 已在 CPU0 启用 RV64 SSIE，以及 LA64 QEMU 的 IOCSR/ECFG IPI line；
  用户态和内核态 trap 共用同一个无锁 fast path，AP→BSP 不再依赖只存在
  于 BSP→AP 方向的假设；
- AP 收到往返请求时，hard IRQ 只原子发布 reply pending；真正的回复
  doorbell 延后到 AP idle stack。idle loop 在全局中断关闭后先重查
  deferred work，再执行一次 `wfi`/`idle 0`，本地 IPI line 保持 enabled，
  从协议上消除 check→wait 窗口中的 lost wakeup；
- B12 的双架构 `CORE_NUM=4 KTEST=smp` 均通过 7/7。每个架构的三个 AP
  各完成 64 轮顺序请求/回复，共覆盖 192 次 AP→BSP doorbell；源码验证
  前后指纹一致；
- B13 已实现 CPU0 发起的终态 STOP/ack：hard IRQ 只发布 stop request，
  AP 返回独立 idle stack 后先关闭全局中断和本地 IPI source，再发布
  stopped ack 并永久执行 `wfi`/`idle 0`。CPU0 有界等待全部目标，重复调用
  排除已 stopped AP，协议保持幂等；
- `hal::shutdown()` 已统一在正常 CPU0 停机前 best-effort STOP 全部 online
  AP；极早期或 AP fatal path 直接进入架构机器级关机兜底。RV64 清空 `sie`，
  LA64 清空 `ECFG` 和 QEMU IOCSR `CORE_EN`，确保 ack 之后不再被 doorbell
  唤醒；
- ktest runner 新增 terminal test 语义：普通测试按 `KREPEAT` 重复，永久
  改变机器状态的 STOP 测试在整个计划末尾只运行一次。双架构四核证据中，
  RV64 与 LA64 均为 7 个普通用例重复两轮后 STOP 一次，共 15/15 PASS；
- B14 已在双架构 user-syscall 分支接入受控中断窗口。helper 只从
  IRQ-off 的完整 trap context 进入，且一定在重新获取 `task.inner`
  写回结果前关闭窗口；
- `schedule()` 已显式快照并关闭 `sstatus.SIE/CRMD.IE`，使 idle scheduler
  始终接管 IRQ-off CPU；原任务再次切入后才恢复自己的窗口。
  panic 入口也在任何诊断前立即关中断；
- B14 的双架构 `CORE_NUM=4 KTEST=smp KREPEAT=2` 均为 17/17 PASS。
  新测试在窗口内 yield，证明 idle→新任务为 IRQ-off、原任务恢复
  为 IRQ-on，然后完成真实 AP→BSP IPI reply；
- B33 已让运行中用户任务在 trap-return 安全点消费 RESCHEDULE；handler 仍只置位，
  与 timer 请求合并后最多调度一次。通用交叉发送、并发 reason 和 10,000 次 ping-pong
  仍未完成，Phase 2 状态保持 `partial`。

Phase 2 结束后设置一次人工 go/no-go 检查点：只有 trap 保存恢复、IPI 幂等、STOP 和 deferred
timer 均有双架构证据，才进入调度状态迁移；“能 ping-pong”不能替代内核中断安全证明。

### Phase 2.5：单核状态迁移 API 与本地 TLB batch

#### 实施内容

- 在仍只有 CPU0 调度任务时，将所有 task_status 写入集中到 transition API，并引入可编码
  `New/Queued(cpu)/Running(cpu)/Blocking(cpu)/Blocked/Zombie` 的原子调度状态；
- 对 publish、fetch、switch-out 等必成功所有权迁移在所有构建中 fail-stop；重复 wake
  使用允许失败的 CAS 返回 `AlreadyWaken`，不得把所有权损坏降级成计数后继续运行；
- 逐一替换 wake、timeout、signal、block、yield、exit 的直接 task_status 写入；旧字段若暂时保留，
  只能是由 transition API 更新的兼容投影，不得继续作为并行真值来源；
- 盘点 interruptible_queue 的信号枚举、OOM、zombie 清理和统计调用方；先把 runnable 所有权与
  这些 registry 职责分离，CAS 成功后才允许入队，旧全局扫描不得再次把任务重复加入 ready queue；
- 在没有远程 CPU 使用用户 MM 时先引入本地 MmuGather facade：复用现有 raw/no-flush 操作与
  本地 sfence.vma/invtlb，将所有已发布 PTE 修改机械收口到统一提交入口；
- 本阶段不实现 remote ack，但接口必须显式区分 unpublished/local-only/published，禁止把
  local-only 实现描述成 shootdown 完成。

#### 退出条件

- 单核 focused test 覆盖所有合法转换，重复 wake 不改变队列归属；非法必成功迁移
  由代码审查和 fail-stop 入口保证，测试不得为触发 panic 直接伪造生产状态；
- 仓库内不再有绕过 transition API 的 runnable 状态写入；
- interruptible_queue 不参与 runnable 唯一性判定，保留的 registry 职责有清晰 owner；
- 已发布 PTE 修改均通过 local MmuGather，双架构单核 MM 回归不下降。

#### 当前进度（SMP-P2.5-B15/B16/B17/B18/B19/B20/B21/B22/B23）

- B15 已删除 `TaskControlBlockInner.task_status`，用单个原子字编码
  `New/Queued(cpu)/Running(cpu)/Blocking(cpu)/Blocked/Zombie`，不再保留兼容投影
  或第二真值；
- publish、fetch、yield、block、wake、timeout、signal 和 exit 已统一经 CAS
  迁移。B18 后 runnable 成员关系由 owner CPU 的 RunQueue 持有，
  `TASK_MANAGER` 只保留 interruptible/zombie/timer registry；重复 wake 不会重复入队；
- `Processor.current` 保留到真实 context switch 返回 idle 后才清空；idle 的
  `finish_switch_out()` 统一提交 yield、阻塞和 zombie 回收。`Blocking(cpu)` 只表达
  “已经登记睡眠但尚未切离 CPU”的必要窗口，早到 wake 恢复 `Running(cpu)` 且不入队；
- `Queued` 任务退出前必须先从运行队列移除并转为 `Blocked`。可能造成任务丢失、
  双重 owner 或悬挂队列节点的错误在所有构建中 fail-stop；只有重复 wake 属于
  可恢复竞争；
- `scheduler_state_has_unique_owner` 只通过生产 API 覆盖提前取消阻塞、完整
  Completion 睡眠/唤醒、publish/fetch/yield/zombie 和重复 wake；冻结只读审查
  未发现并发缺陷；
- 双架构 `CORE_NUM=4 KTEST=smp KREPEAT=2` 均为 19/19 PASS，且
  `scheduler_state_has_unique_owner` 两轮通过、terminal STOP 仅最后执行；双架构
  normal build 退出 0，RV64 WaitQueue 为 4/4 PASS。详细命令、用时和证据边界见
  2026-07-27 Work Log 与 evidence；
- B18 已删除全局 ready queue，为每个 `CpuTaskState` 增加独立 RunQueue 和
  `nr_running` 排队数快照；本地 FIFO/nice-aware 选择保持原有语义，生产 target
  仍固定 CPU0，AP 队列保持为空；
- nice-aware 路径改读 TCB 的原子 nice/vruntime hint，消除了 runqueue 锁与
  `task.inner` 的嵌套；Blocked wake 和批量 remove 固定采用
  `TASK_MANAGER -> 单个 RunQueue`，其他调度路径只锁一个本地 RunQueue；
- OOM 候选按 CPU 和索引逐个克隆，避免低内存路径为 runqueue 快照再次分配；
- B16 将用户页表的 map/unmap、权限、PPN 和 dirty 修改拆为 raw/no-flush
  原语；B23 重构后由 `UserMapper` 直接借用页表和 `MmuGather`，每次 PTE 写入后
  立即 `record_change()`，`PageMapper` 只保留给内核页表路径；
- B16—B22 曾用 `Unpublished/LocalOnly/Published` 过渡状态阻止不完整远端语义。
  B23 已删除这套状态，直接按 cached CPU mask 的 0/仅本核/含远端三种情况执行；
- 单一 VPN 修改在 RV64 使用页级 `sfence.vma`，多 VPN 升级为本核全量刷新。
  LA64 的 ASID 已在 B25 下沉到外层 `TlbContext`；B26 在 VM 锁内与 VPN 一起冻结，
  本地及远端目标均按 ASID + 对齐硬件页对执行 `invtlb 0x5`；
- unmap、CoW/回滚、OOM/swap、exec 和 zombie 清理都先撤销 PTE，再通过
  `UserMapper::retire_frame()` 把旧 `FrameTracker` 交给本轮唯一 `MmuGather`；
  `TlbFlush::execute()` 完成 flush/ack 后才释放。存在远端观察者且退休队列 OOM 时
  故意泄漏并 fail-stop，不在 VM 锁内提前等待；
- 双架构 `CORE_NUM=1 KTEST=mm KREPEAT=2` 均为 8/8 PASS，新用例通过
  生产 API 覆盖 map、fault-in、mprotect 降权、munmap 和同 VPN 重新映射。
  该证据只验收本地提交，不外推远端 TLB 一致性；
- B17 已删除全局 `PROCESSOR`、`CURRENT_TASK_PTR` 和 `current_task_ref()`；每个
  `PerCpu` 内嵌 `CpuTaskState`，独占 current `Arc`、idle context、PID/TID 快照和
  诊断 syscall ID。`current_task()` 只在本 CPU processor 锁内克隆 `Arc`，panic
  路径通过地址校验与 `try_lock()` 安全降级；
- 可变的父 PID、身份、进程组和用户页表 token 改读 TCB/PCB 权威原子 hint，不再
  维护跨路径刷新缓存。dispatch 先读取 `task.inner` 再锁 processor，且 processor
  锁不跨 `__switch`；所有退出、信号和双架构 trap noreturn 路径在切换前显式
  释放本地 current `Arc`；
- B17 双架构 normal build 退出 0；`CORE_NUM=4 KTEST=smp KREPEAT=2` 均为
  19/19 PASS，online mask 均为 `0xf`。该证据不外推用户任务跨核、远程 enqueue
  或 MM shootdown；
- B18 在冻结源码指纹下完成双架构 `CORE_NUM=8` normal build 与
  `KTEST=smp KREPEAT=2`；RV64/LA64 均为 19/19 PASS，online mask 均为 `0xff`。
  该证据只验收容器拆分和 CPU0 owner 不变量，不外推 AP 调度或远程唤醒；
- B18 补跑双架构 `CORE_NUM=8 mask=0x003` 后，RV64 312/314 且失败集合与基线一致；
  LA64 raw 为 302/314，两个 libc 的 `test_pipe` 均因 cpid 物理行交错得到 1/4。
  最小判别确认官方测试二进制把 `printf("cpid: %d\n")` 拆成多个 write syscall，
  timer 只会在 syscall 返回后的 trap-return 安全点切换任务；两个原始块均包含恰好两个
  cpid 前缀、0/正 PID、write-success 和 END。按 §8.2 对 B16/B18 一致应用的语义
  归一化后，LA64 为 308/314，初赛非回归门禁 PASS；raw 302 仍原样保留，不宣称官方
  judge 满分。干净 B17 对照 raw 305/semantic 308，并再次在 glibc 组复现相同片段交错；
  官方镜像的两份 libc pipe 二进制哈希相同。该结论没有引入 TTY 跨 syscall 锁或行缓存
  workaround；
- B19 增加 scheduler-ready Release/Acquire 屏障。AP 越过屏障后先安装本 CPU 的
  kernel page-table root 并刷新本地 TLB，再进入共用 `run_tasks()`；BSP 等待全部
  `scheduler_entered` ack 后才创建远程测试任务；
- CPU0 保留完整 housekeeping；AP 只运行本地 RunQueue、IPI deferred work 和共用
  dispatch/switch-out。`RESCHEDULE` hard IRQ 只置 `need_resched`，真正 fetch/switch
  发生在 idle 安全点；
- ktest kernel entry 从有竞争的全局“下一入口”改为 TCB 不可变字段。focused test 可显式
  向每个 AP 发布一个短 kernel-only 任务，并验证 `Queued(cpu) -> Running(cpu) -> Zombie`、
  current 唯一归属和 exactly-once；普通新任务和用户任务仍固定 CPU0；
- 动态 kernel stack 在远程入队前执行受限的 kernel-mapping sync：释放 `KERNEL_SPACE`
  锁后发送带 sequence 的 IPI，目标本地全 TLB flush 并 ack，发送方确认后才发布 runqueue，
  再在释放队列锁后发 `RESCHEDULE`。该协议只覆盖新增 stack 的首次使用，不替代 Phase 4
  的 MM active mask/range shootdown/延迟释放；
- 首轮 RV64 8 核 focused 测试为 16/23，首个远程任务把全部 AP 卡住，后续 IPI/STOP
  级联失败。DeepSeek 只读审查与人工调用链复核定位为 AP 从未安装 CPU-local 页表根；
  修复后 RV64/LA64 `CORE_NUM=8 KTEST=smp KREPEAT=2` 均为 23/23 PASS，源码指纹
  前后一致；
- B20 不扩张六态状态机；TCB 只增加不参与 owner 判定的 `last_cpu` 提示。任务完成
  `Queued(cpu) -> Running(cpu)` 后发布该提示，真正 `Blocked` 时优先回到仍可调度且未 STOP
  的原 CPU。单个/批量 wake 都先在 `TASK_MANAGER -> 单个 RunQueue` 下完成唯一入队，
  再在锁外按聚合 mask 发送 `RESCHEDULE`；普通任务未离开 CPU0，因此既有用户行为不变；
- B20 focused 用例让每个 AP 的 kernel-only 任务同时进入真实 Completion/WaitQueue，CPU0
  确认全部 `Blocked` 后一次批量唤醒。RV64/LA64 `CORE_NUM=8 KTEST=smp KREPEAT=2`
  均为 25/25 PASS，双架构 normal build 退出 0，四项源码指纹前后一致；
- B21 将 kernel-global mapping sync 从“首次远程发布”扩展为安全撤映射。PTE 在
  `KERNEL_SPACE` 锁内以 no-flush 原语清除，mapping frame 跨锁保留；释放锁后对所有
  online CPU 做全量失效并等待 ack，最后才释放 frame。handler 固定按
  request-before-flush-before-ack 执行，撤映射可接受终态 stopped ack，发布路径不可接受；
- 内核栈析构不再跨进程锁等待 shootdown：缓存溢出的 slot 进入固定退休队列，由 CPU0
  idle 安全点按“摘映射 → 全核 ack → frame 释放 → slot dealloc”回收。focused test 以
  两轮 129 个 CPU1 任务强制 cache overflow、TCB 析构和 slot 重用；曾在 AP 使用的 TCB
  不再保留到关机；
- B21 最终冻结源码下，RV64/LA64 `CORE_NUM=8 KTEST=smp KREPEAT=2` 均为 27/27 PASS。
  双架构 `mask=0x003` 初赛门禁也通过：RV64 raw/semantic 312/314，LA64
  raw/semantic 308/314，失败身份均为既有允许集合；
- B22 为每个 `AddressSpace` 增加共享 `TlbContext`：MM ID、从 1 开始的 generation、
  per-CPU observed 和只增不减的 cached CPU mask。用户 trap-return 在恢复页表根前，
  先在 VM 锁内登记 CPU，再读取 generation；落后时执行本地全用户/non-global 失效并
  重查代际。B23 接通修改侧前，第二颗 CPU 登记会触发过渡 fail-stop；该临时状态现已删除；
- B22 另建独立的 `USER_TLB_SYNC` request/ack，不与 kernel-global sequence 复用。
  RV64 先采用全量 `sfence.vma`，LA64 采用 `invtlb 0x3`；handler 不分配、不取普通锁，
  发起者只可在释放 VM/PTE/runqueue 锁后等待。双架构 8 核 focused 均为 29/29 PASS；
  初赛 RV64 raw 309/semantic 312、LA64 raw/semantic 308，失败集合未扩大。该结果只验收
  激活与 IPI 基础设施及 CPU0 用户路径非回归，不证明 stale PTE、generation race 或
  frame 延迟释放；
- B23 将共享外层与锁内数据明确命名为 `AddressSpace`/`AddressSpaceInner`，不再向
  调用方暴露可变 guard。一个 `write()` 内只有一个 `MmuGather`；调用链固定为
  `record_change -> seal -> execute`，只推进一次 generation 并冻结 cached CPU
  目标；锁外完成本地失效、远端 IPI/ack 和 observed 单调推进，最后才析构撤映射
  数据页与页表页；
- 为封死“持 VM 锁等 ack”，`ProcessInner.vm` 改为 `Arc<AddressSpace<_>>`，
  mmap/munmap/mprotect、page fault、CoW/fork、exec、OOM、SysV SHM、uaccess 与
  zombie 清理等调用点全部迁移到 `read/write/try_write`。trap、clone、OOM 和 SHM
  回滚同时调整锁序，不把 `task.inner`/`TASK_MANAGER`/`SHM_REGISTRY` 带过 shootdown 等待点；
- trap-return 快路径对已登记 CPU 只读 cached mask，不再每次 `fetch_or`；ack 后
  立即更新本 MM 的 observed，避免下次返回再全刷。handler 还会在 `ack >= request`
  时忽略迟到的重复 reason。仍保留 VM 锁 + Acquire load 固定税、全用户失效和
  只增不减的历史 CPU 集合，不把当前正确性实现宣称为最终性能形态；
- 生产路径 focused 用例在 CPU1 关中断的 request/ack 窗口撤销真实用户 PTE，
  证明 frame 在 ack 前未返回分配器、`write()` 返回后才释放。最终冻结源码上
  RV64/LA64 `CORE_NUM=8 KTEST=smp KREPEAT=1` 均为 16/16 PASS；加上重复窗口的
  前一轮两次运行，两架构均为 31/31 PASS。双架构 `mask=0x003` 失败身份
  与 B22 一致：RV64 raw 309/semantic 312，LA64 raw/semantic 308；
- B24 沿用同一 `MmuGather -> TlbFlush` 主链，只把已有 `FlushRange::Page` 作为提示传给
  SMP 同步层。RV64 启动时通过 SBI BASE extension 一次性探测 RFENCE，单页远端失效把
  逻辑 CPU mask 转成物理 hart mask 后调用 `REMOTE_SFENCE_VMA`；固件不支持时明确打印并
  改走软件 slot。B24 当时 LA64 与 full flush 仍走全量 fallback，LA64 页级路径已由 B26 取代；
- RFENCE 路径没有 MangoCore 共享范围槽，因而不同 CPU 并发发起时不会互相覆盖 payload；
  OpenSBI 在调用返回前完成本地/远端 fence。focused 用例分别覆盖 RV64 页级 RFENCE、
  当时的 LA64 页级 fallback，以及双页 `Full` 的 ack 前 frame 不释放窗口；最终非平凡 CPU0/1
  mask 复测中 RV64 boot hart=5、LA64 boot hardware ID=0，两架构均为 17/17 PASS；
- B26 不增加第二条 MM 提交链：`MmuGather::seal()` 在原有 VM 锁内把 MM-owned ASID 与
  `FlushRange::Page` 冻结进 `TlbFlush`；锁外由每发起 CPU 固定原子 slot 发布 ASID/VPN，
  handler 扫描所有 slot、执行精准失效并 ack。slot 超时不复用，防止迟到 doorbell 与后续
  payload 发生 ABA 错配；8 个 CPU 并发发布不同 VPN 的用例证明 reason 合并不会覆盖 payload，
  双架构 8 核 focused 均为 20/20 PASS；
- Phase 2.5 的 task ownership 与本地 TLB batch 两项退场条件已完成；Phase 3 已完成
  Per-CPU current/idle/RunQueue、scheduler-ready、受控 AP kernel-only 执行与远程阻塞
  唤醒闭环。通用目标选择仍待后续批次；Phase 4 远端 MM shootdown 完成前不解除用户
  任务 CPU0 affinity。B21 完成共享内核页表撤映射，B23 完成用户 MM
  修改侧的锁外提交；两者仍不等于用户任务迁移与全进程语义已开放。

### Phase 3：Per-CPU 调度器与时间系统

#### 实施内容

- B17 已删除全局 PROCESSOR 和 current-task 裸指针，把 pid/tid/syscall 诊断 hint
  迁到 Per-CPU，并让可变身份字段改读权威对象；B18 已删除全局 ready queue；
  B19 已让 AP 在 scheduler-ready 后进入精简本地调度循环；B20 已让受控 AP 任务在
  WaitQueue 阻塞后通过锁外 `RESCHEDULE` 回到最近运行 CPU；B31 已为 TCB 增加
  `cpus_allowed`，三条 runnable owner 交接路径都拒绝越过 mask；B33 已让运行中用户任务
  在 trap-return 安全点消费远端 RESCHEDULE；B34 又允许 current 线程运行期修改 mask，
  新 mask 排除 owner 时复用同一安全点和单目标 runqueue 迁移，不从 hard IRQ 直接切换；
  B35 允许远程稳定 Blocked 线程在 `TASK_MANAGER` 锁内修改 mask，后续 wake 按新允许集选点；
- 每 CPU 使用本地 Processor、RunQueue 和 idle context；AP zombie 先交给受锁全局
  registry，由 CPU0 回收。B21 的固定内核栈退休队列只处理映射/slot 生命周期，不等同于
  完整的 Per-CPU zombie 回收队列；
- 本地选择继续保留 FIFO fast path 和现有 nice-aware 选择；
- 新任务或被唤醒任务的目标 CPU 选择规则固定为：
  - last_cpu 在线、在 affinity 内且负载不超过最小负载 +1 时优先复用；
  - 否则选择 affinity 内 nr_running 最小的 CPU；
- B31 已完成 affinity 内核 mask 与唤醒合法性筛选；B34 已为 current 线程实现按
  `nr_running + current` 最小值选择合法迁移目标；B35 已让远程稳定 Blocked 线程复用
  registry/wake 锁序发布新 mask。因普通任务仍 CPU0-only，last_cpu `+1` 通用放置、默认
  全核 mask 和远程 Running/Blocking/Queued 改 mask 仍是后续项；
- 远程入队后，如果目标 CPU idle 或任务优先级需要尽快运行，发送 RESCHEDULE IPI；
- Phase 3a 先只实现 per-CPU queue、目标选择和远程 enqueue；work stealing 默认关闭；
- Phase 3b 在 3a 唯一运行和远程唤醒门禁通过后再开启 steal：idle CPU 只从一个选定 victim
  取一个允许迁移的任务，整个过程不同时持有两个 runqueue 锁；
- affinity 变化后，正在运行的非法 CPU 设置 migration_pending 和 need_resched；
  已排队任务在出队时重新定向，避免跨队列双锁迁移；
- 安全抢占点仅包括：
  - 返回用户态之前（B33 已接入 timer/RESCHEDULE 合并入口）；
  - 显式 yield；
  - block/exit；
  - idle 调度循环；
- timer interrupt 不直接切换任务，只推进本 CPU 100 Hz quantum 并设置 need_resched；
- CPU0 独占全局 timeout、timerfd、console input、网络后台 poll、文件系统 reclaim 和周期性 housekeeping；
- 非 CPU0 插入更早全局 timer 后，释放 timer queue 锁，再向 CPU0 发送 TIMER_REPROGRAM；
- zombie TCB 在退出 CPU 的 idle 栈上回收，禁止在仍使用自身内核栈时释放最后一个 Arc。

#### 退出条件

- Phase 3a 的 CPU-bound 内核任务能通过目标选择和远程 enqueue 分布到全部在线 CPU；
- 同一任务从不并发运行，重复 wake 不会重复入队；
- affinity、迁移、阻塞和远程唤醒压力测试通过；Phase 3b 另行验证 steal 并保持可关闭；
- 当前阶段普通用户任务仍默认固定 CPU0，避免在 TLB shootdown 完成前跨核运行。

### Phase 4：TLB shootdown、ASID 与用户 MM

#### 实施内容

- B22 已在 B16 的 raw/no-flush 用户 PTE 边界上增加 MM ID、单调 cached CPU
  mask、generation/per-CPU observed、trap-return 激活登记，以及独立的全用户 IPI/ack
  原语；B23 已用 `AddressSpace` + `MmuGather/TlbFlush` 取消发布状态过渡门禁；
- B23 已在同一个 VM 锁临界区内完成 PTE 修改、失效范围合并、generation 推进和
  cached mask 快照，把唯一 `MmuGather` 移交给锁外 `TlbFlush`；释放 VM 锁后才执行本地失效、远端 IPI/ack 等待，
  全部目标完成后释放数据 frame 和页表 frame。调用方无法取得可变 VM guard，
  从类型形状上禁止在 VM 锁内等待 ack；
- B16 已覆盖用户 unmap、mprotect、CoW、MAP_SHARED 写缺页、匿名缺页、filemap 和
  exec；Phase 4 需将用户路径升级为远端语义。B21 已单独收口动态内核栈与临时 ELF
  kernel-global 映射的全 CPU 撤销和延迟释放；
- RISC-V 单页 SBI RFENCE 与 IPI fallback 已由 B24 接通；后续补连续 range/all 的策略与计数；
- B25 已删除 TCB-owned ASID，把 versioned ASID 下沉到 `TlbContext`；同一 epoch 内不立即
  复用编号，耗尽时先通过既有 user-TLB request/ack 清除全部 online CPU，再推进 epoch。
  `trap_return -> activate_user_vm -> AddressSpace::activate_on` 一次取得页表根/ASID 快照，
  LA64 普通 context switch 不再固定全刷 non-global TLB。首轮 LA64 初赛进一步暴露并修复
  trap-return 泛型 asm 输入覆盖：固定 ABI 参数现直接绑定 `$a0/$a1/$a2`，release ELF 已
  反汇编确认；双架构 8 核初赛分别为 RV64 312/314、LA64 308/314，失败集合未扩大；
- B26 已实现携带目标 ASID/VPN 的固定 shootdown slot：每个发起 CPU 独占一个槽，
  多个发起者共享 reason bit 时 handler 扫描全部槽；IPI handler 不分配内存、不获取 MM 锁；
- LoongArch 目标 CPU 使用 `invtlb 0x5` 限定 `G=0 + ASID + VA`；由于普通 TLB entry
  覆盖相邻偶/奇页，VA 按 `2 * PAGE_SIZE` 对齐，这是该架构可提供的最小粒度；
- B27 已实现 RISC-V `SATP.ASID` 容量探测、MM-owned versioned ASID、本地
  `sfence.vma va, asid` 和 SBI RFENCE FID 2；QEMU virt 实测提供 65535 个用户编号；
- RV64 trap 入口/返回从编码后的 SATP 提取 ASID，仅 ASIDLEN=0 兼容路径固定全刷；
  rollover ack 与 trap-return IRQ-off 边界共同保证旧 epoch SATP 不会在 ack 后重新安装；
- 被解除映射的 frame、页表页和内核栈必须延迟到全部目标 ack 后释放；
- membarrier：
  - GLOBAL 面向所有在线 CPU；
  - PRIVATE_EXPEDITED 面向当前进程 MM 的 active CPU；
  - 使用同一 IPI/ack 基础设施和完整内存屏障；
- RISC-V 非零 ASID 的 trap 入口/返回不再固定执行全量 `sfence.vma`；stale-TLB 用例仍需
  记录 victim trap 窗口，并同时核对目标 ASID/VPN、shootdown sequence 与 ack，避免把
  timer 或其它全刷误当成精准后端证据；
- TLB 用例同时校验 shootdown sequence/ack 和 ack 前 frame 不复用；LoongArch 作为不被
  trap 自动全刷掩盖的强暴露平台，必须单独保留证据；
- 完成 MM 专项测试后，才允许受控用户测试任务跨 CPU 运行。该测试必须是 hermetic 的
  CPU/MM-only workload，使用匿名或启动前预载内存；除串行化结果输出外，不进入尚未审计的
  文件系统、网络、VirtIO 或设备并发路径。
- B28 已打通受控 AP 用户 trap 闭环，而没有开放普通任务迁移。`publish_task_on()` 统一
  “远端内核栈映射同步 → runqueue 发布 → 锁外 doorbell”；双架构 trap handler 删除旧
  CPU0-only 断言，通过 `current_trap_task()` 一次取得 current 并校验 `Running(cpu)`。
  ktest 在 CPU0 构造只使用匿名代码页的用户任务，发布到 CPU1；探针依次执行 getpid、yield
  和非返回的 exit，CPU0 再观察 zombie、wait/reap 并确认 TCB 最后一个强引用已释放。
- 探针代码只在装载期映射 RW，发布前经正式 mprotect/PTE 提交流程收紧为 RX。syscall 前
  必须释放 trap handler 的临时 TCB `Arc`，因为 exit 不会回到该 Rust 栈帧；返回型 syscall
  则重新读取当前 CPU 和 current，避免未来迁移后沿用旧 owner。
- B28 动态证明的是 CPU1 用户 trap/return、MM/ASID 激活、协作式 yield、退出和 CPU0 回收。
  它没有制造并发 PTE 修改，因此不得表述为 generation race 或远端页级 shootdown 的新证据，
  也不代表同一用户任务已在 CPU0 与 CPU1 间迁移。AP timer、外部设备中断及 FS/net/driver
  仍关闭，普通用户任务继续首次发布到 CPU0。
- B29 已补上“同一 TCB 真迁移”的最小生产闭环。TCB 只增加一次性 `migration_target`，且
  只允许 New 任务独占持有者或本地 current 请求；目标 kernel stack 先同步，再发布请求。
  源 current 在 idle 栈清空后，`requeue_after_switch()` 只锁目标队列，完成
  `Running(source) -> Queued(target)`，锁外再发 `RESCHEDULE`。没有新增 `TaskStatus`，也不
  同时持有两个 runqueue。
- B29 ktest 把探针先发布到 CPU0，真实 getpid/yield 后在 CPU1 从原 syscall 栈恢复并退出。
  首轮双架构 RED 暴露 CPU0 runner 关中断自旋会阻塞 CPU1 的退出期 user-TLB shootdown；
  等待区改用既有受控中断窗口后，双架构 21/21 PASS。该证据覆盖 cached mask `0x3` 的退出
  shootdown，但仍不覆盖并发 PTE writer、普通任务 affinity、blocked/queued 迁移或共享 I/O。
- B30 把用户可见 CPU 查询接到既有逻辑 `cpu_id()`：一次 syscall 只采样一次 CPU，node 在
  当前无 NUMA 拓扑下返回 0，NULL 输出和 tcache 按 Linux ABI 处理。B29 探针现于 yield 前后
  分别断言 CPU0/CPU1，固定返回 0、未迁移或错误起跑都会 exit(1)。这只完善查询语义，不改变
  默认 CPU0 affinity，也不允许普通用户任务进入尚未审计的 AP 共享子系统路径。
- B31 把 affinity 从文档假设变为 TCB 内的真实位图。普通任务初始为 CPU0-only，
  clone 继承父 mask，exec 保留原 mask；定向 ktest 任务收紧为单 CPU，用户迁移探针
  显式允许 CPU0/CPU1。`publish`、yield requeue 和 blocked wake 在进入目标队列前
  都验证该 mask。当前只允许 New-only 初始写入，不宣称运行期 affinity 已完成。
- B32 把只读 ABI 接到这份权威位图：`pid=0` 读取调用线程，正数严格按 TID 查找，
  raw syscall 复制一个 `usize` 并返回 8。查询不持 registry/runqueue 锁进入 uaccess；
  双架构 probe 在 CPU0/CPU1 迁移前后均读到 `0b11`。它没有改变 mask 或 owner，
  也不替代后续 `sched_setaffinity` 的迁移串行化。
- B33 把 `RESCHEDULE` 从 AP idle 唤醒提示扩展为运行中用户任务的真实安全点请求。
  `run_task_safe_point()` 在 IRQ-off 窗口先完成 deferred timer，再 Acquire 消费本 CPU
  IPI 提示，并对两者最多调度一次；双架构 trap-return 对称调用。focused probe 删除
  显式 yield，由 CPU1 helper 向 CPU0 发送生产 IPI，并同时验证消费计数、getcpu 0→1、
  affinity `0b11`、exit/reap 和 Weak 释放。该闭环为后续排除 Running owner 提供切出机制，
  但没有解决 Queued/Blocked 的运行期 mask 串行化。
- B34 没有一次引入完整远程迁移状态机，而是先闭合 current 线程的真实写侧。raw
  `sched_setaffinity(0/current_tid)` 按 Linux 规则接收短 mask、忽略 configured 范围外高位，
  权限检查后确认目标就是本 CPU `Running` current；mask 仍含 source 时只发布新值，排除
  source 时先同步目标 kernel stack，再 Release 发布 mask 和一次性 target，并在 syscall
  安全点立即调度。源 idle 仍只锁目标 runqueue 完成 owner 交接。双架构 probe 依次验证
  CPU0→CPU1、setaffinity(bit0) 返回于 CPU0、getaffinity=bit0、exit/reap/Weak；8 核 focused
  均为 21/21，初赛仍为 RV64 312/314、LA64 308/314。远程 TID 当前返回 `EOPNOTSUPP`，
  Queued/Blocked 任务写侧仍待单独协议。
- B35 选择不拥有 current/runqueue 的稳定 Blocked 状态作为下一独立闭环。远程 syscall 在
  `TASK_MANAGER` 锁内同时确认精确状态和 registry 指针成员关系，再 Release 发布 mask；wake
  在同一锁域 Acquire 读取并按新允许集选点。这样既不新增状态/锁，也不会把退出路径短暂的
  Blocked 误认为可修改睡眠任务。focused 第 13 项让 CPU1 任务真实阻塞，CPU0 把 mask 改为
  bit0，再经 Completion 生产 wake 于 CPU0 恢复；双架构当前列表均为 22/22。初赛保持
  RV64 312/314、LA64 308/314，精确失败集合未扩大。远程 Running/Blocking/Queued 仍返回
  `EOPNOTSUPP`，B35 focused 尚未从用户态端到端覆盖远程 TID syscall。

#### 退出条件

- 一核反复 unmap/protect/CoW，其他核并发访问时不出现旧权限或旧物理页；
- LoongArch 强制 ASID rollover 后无跨进程数据污染；
- shootdown 期间即使目标 CPU 正在执行长 syscall 也能及时 ack；
- frame 释放计数证明不存在 ack 前复用；
- RISC-V victim 观察窗口、目标 ASID/VPN 与 sequence/ack 证据一致，结果不能由其它全刷偶然制造。

### Phase 5：共享子系统与进程语义审计

#### 实施内容

- 复核 Phase 0.5 已落地的 console irq-safe 锁和 panic raw fallback，不在本阶段首次补救；
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
  - 默认任务 affinity 为 configured online mask；
  - rseq(293) 明确返回 ENOSYS，非诊断构建不为每次调用输出 unknown-syscall 日志。

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
- Phase 2.5/3 已用于正确性门禁的无效状态转换和重复入队断言继续保留；Phase 6 只补充
  汇总、导出和低开销 release 计数，不能把首次发现竞态的能力拖到稳定化阶段；
- panic 时输出所有 CPU 的 current TID、runqueue、IRQ/preempt depth、active MM 和 pending IPI；
- 同步更新启动/trap、调度器、页表/TLB、测试文档和根 AGENTS.md 中的“单核”描述；
- 每个阶段形成独立、可回退提交，不把启动、调度和 TLB 改动压成一次大提交。

## 4. 测试、证据与验收

### 4.1 工作包与阶段的自适应门禁

日常工作包按 `smp-agent-execution-spec.md` 的 T0-T3 选择最小充分验证：

- T0 文档/注释只做静态 diff 检查；
- T1 局部或架构隔离改动先构建受影响架构，共享代码或准备提交时补齐双架构；
- T2 启动/per-CPU/共享原子改动按实际行为选择 CORE_NUM=1/2 focused QEMU；
- T3 trap、IPI、调度、TLB/ASID、锁序和 unsafe 生命周期执行双架构构建与对应并发测试；
- 只有 Phase 退出或合并门禁固定执行双架构 build、CORE_NUM=1 回归和该阶段
  CORE_NUM=1/2/4/8 focused 矩阵。

改变普通用户任务执行路径的 T3 节点、Phase 退出和合并候选还必须执行双架构
`CORE_NUM=8`、`mask=0x003` 初赛非回归门禁。它同时要求启动/组完整性硬条件和
judge 失败集合相对人工接受基线不扩大；精确基线、豁免和 ratchet 规则以
`smp-agent-execution-spec.md` §8.2 为准。该门禁是里程碑验收，不替代本批故障模型对应的
focused test，也不因纯文档收尾重复运行。

所有构建和 QEMU 均在 Docker 内执行。需要双架构时严格串行，避免 nightly override 竞态；
已经在同一源码状态通过的结果，不因随后只修改文档、注释或证据文件而重复运行。

### 4.2 SMP Ktest

新增 <code>KTEST=smp</code>，至少覆盖：

| 类别 | 场景 |
|---|---|
| 启动 | online mask、独立 boot/idle stack、per-CPU register |
| IPI | 单播、广播、ping-pong、并发 reason、STOP |
| 调度 | 唯一运行、重复 wake、远程 enqueue、steal、affinity、迁移 |
| 同步 | futex、WaitQueue、Completion、eventfd、signal |
| MM | unmap、mprotect、CoW、MAP_SHARED、exec、kernel mapping |
| TLB | generation race、sequence/ack、ack 前不释放、目标正在 syscall、victim 无 trap 窗口 |
| LA ASID | 多 MM、强制 rollover、epoch 后复用 |
| 进程 | exit_group、fatal signal、多线程 exec |
| FS | 多核 create/read/write/rename/unlink/fsync |
| 网络 | 多 socket 并发、单 poll owner、路由缓冲隔离 |
| 停机 | panic/normal shutdown 停止其他 CPU |

竞争敏感用例根据故障概率和单次耗时选择重复次数：日常 smoke 通常为 1～10 次，阶段门禁通常
为 20～100 次；不为达到固定数字重复已经稳定且与本批无关的测试。失败时打印 CPU、任务状态、
队列归属和最后一次 IPI/TLB sequence。调度/TLB 竞态测试显式使用 MTTCG；只有在证明真实并发
覆盖时才额外记录宿主 vCPU 线程和 victim 观察窗口 trap count。

### 4.3 最终矩阵

最终候选版本对两种架构分别执行以下基线矩阵；若某子系统本阶段未改变，可以引用同一候选
commit 上已有的新鲜结果，不重复制造等价运行：

- CORE_NUM=1/2/4/8 的 SMP focused ktest；
- CORE_NUM=1 和 CORE_NUM=8 的 basic + busybox（mask 0x003）；
- CORE_NUM=1 和 CORE_NUM=8 的竞赛 12 组全量（mask 0xFFF）；
- 每种架构的 1 核和 8 核至少连续启动 3 次；
- 每种架构 8 核混合压力先运行 10 分钟；出现不稳定、修改高风险并发路径或形成最终发布候选时
  扩展到 30 分钟：
  - CPU worker；
  - fork/exec/exit；
  - futex/pipe/eventfd；
  - mmap/CoW/unmap；
  - ext4 并发；
  - TCP/UDP 并发。

其中双架构 CORE_NUM=8、mask 0x003 结果还必须满足执行规范 §8.2 的递增非回归基线；
“四组完整执行”或 recipe 退出 0 本身不能代替 judge 失败集合判定。

### 4.4 性能判定

性能门禁只在修改调度、IPI、shootdown、锁竞争或明确以性能为目标时执行。QEMU TCG 和宿主
调度会显著影响结果，不设置固定线性加速要求，但相关工作包必须满足：

- 结构上不存在全局 ready queue；
- 日常比较至少使用 3 次同宿主、同镜像、同 workload 样本；噪声较大或结论接近阈值时再扩展
  到 5 次并报告 median 和 MAD；
- 8 核 CPU-bound 吞吐不得连续两组低于 4 核，除非差异位于
  <code>max(10%, 2×MAD)</code> 噪声带内；
- 若 8 核明显低于 4 核，必须用 runqueue、migration、IPI、shootdown 和锁等待计数解释后才能验收。

### 4.5 证据归档

T3、阶段门禁、性能对比和难复现失败的证据统一保存到当天唯一的日期目录：

~~~text
docs/Work_Log/evidence/YYYY-MM-DD/
~~~

同一日期内使用 <code>smp-phase-&lt;N&gt;-</code> 或批次前缀区分，不再创建主题子目录。
阶段门禁至少包含：

- git commit/worktree 状态；
- Docker image ID、digest 和 QEMU 版本；
- 完整命令与配置；
- 双架构构建日志和退出码；
- QEMU 完整输出和结果判定；
- CPU 数、online mask 和测试重复次数；
- 压力测试统计与性能原始样本。

T0/T1 只需在 Work Log 记录静态检查或构建结果；T2 保存命令、退出码和关键串口标记，只有不稳定
或需要交接时保存完整日志。同一连续任务的容器、镜像和 mount 元数据可记录一次后引用。

阶段状态只能写为 <code>not-run</code>、<code>red</code>、<code>partial</code>
或 <code>pass</code>，不得用“预计通过”替代证据。

## 5. 固定假设与实施纪律

- MAX_CPUS=8，configured CPU ID 连续为 0..N-1，CPU0 固定为 BSP；
- v1 使用构建期 MANGO_CORE_NUM，不实现 DTB/ACPI 动态 CPU 枚举；
- QEMU CPU 拓扑固定为单 socket、N core、单 thread；
- 内核采用安全点抢占；中断可打断内核，但不得在任意内核中断点切换任务；
- 单核仍走 SMP 数据结构，不保留第二套调度器；
- 普通用户任务在 TLB 和共享子系统门禁通过前默认保持 CPU0 affinity；受控 current 与稳定
  Blocked affinity 测试不等于默认全核调度已经开放；
- RISC-V HSM/RFENCE 缺失时明确报错或使用本文指定的 IPI fallback，不做静默降级；
- 实板 2K1000LA 始终配置为单核，本计划不宣称实板 SMP 支持；
- 实际进入代码实施后，新任务首次修改加载 mango-workflow；同一连续任务复用已加载状态，只有
  bug/性能场景才读取对应调试参考，并在工作包完成时更新 Work Log；
- 实施从冻结基线创建专用 SMP branch/worktree；每个完整工作包在人工审核后按用户授权独立提交，
  已批准的低/中风险紧耦合步骤可以合并为一个可审查工作包，不要求为每个机械子步骤制造提交；
- 状态和测试结论必须引用可复核证据，不得将未执行项目标为通过。

## 6. 外部参考

- [RISC-V SBI Hart State Management Extension](https://github.com/riscv-non-isa/riscv-sbi-doc/blob/master/src/ext-hsm.adoc)
- [RISC-V SBI IPI Extension](https://github.com/riscv-non-isa/riscv-sbi-doc/blob/master/src/ext-ipi.adoc)
- [RISC-V SBI Remote Fence Extension](https://github.com/riscv-non-isa/riscv-sbi-doc/blob/master/src/ext-rfence.adoc)
- [RISC-V Privileged Architecture: Supervisor interrupts](https://riscv.github.io/riscv-isa-manual/snapshot/privileged/)
- [LoongArch Reference Manual, Volume 1](https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html)
- [QEMU 9.2.1 Loongson IPI register definitions](https://gitlab.com/qemu-project/qemu/-/blob/v9.2.1/include/hw/intc/loongson_ipi_common.h)
- [QEMU 9.2.1 Loongson IPI register semantics](https://gitlab.com/qemu-project/qemu/-/blob/v9.2.1/hw/intc/loongson_ipi_common.c)
- [QEMU 9.2 Invocation: TCG thread option](https://qemu.readthedocs.io/en/v9.2.0/system/invocation.html)
