---
title: "MangoCore 双架构 8 核 SMP 实施方案"
category: plan
status: proposed
owner: MangoCore Team
last_updated: 2026-07-26
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
- 所有以“当前为单核”为依据的 unsafe Send/Sync 和 static mut 均已消除或重新证明；
- 双架构 8 核连续压力测试无 panic、死锁、任务丢失、重复执行或 stale TLB；
- 单核测试结果不低于 SMP 改造前基线。

## 2. 当前状态与目标架构

### 2.1 当前主要缺口

| 子系统 | 当前状态 | SMP 风险 |
|---|---|---|
| 启动 | 双架构 8 槽 boot stack、BSP/AP 入口、RV SBI HSM、LA QEMU 启动 mailbox、独立 AP idle stack 和 1/2/4/8 核最小 online 闭环已完成 | AP 已进入可被 IPI 唤醒的 idle loop，但尚无 STOP、timer 和调度能力 |
| 初始化 | CPU0 独占 BSS/MM/驱动/FS；AP 安装 PerCpu 锚点，在 CPU-local bootstrap 和 idle stack 切换后发布 idle/online；运行期 `cpu_id()` 已可校验 | PerCpu 目前只增加最小 IPI reason/ack，完整 global/local init 仍未接入 |
| trap | 双架构用户 trap 已恢复内核 CPU-local 寄存器；用户/内核 trap 共用无锁 IPI fast path，CPU0 可在受控窗口接收 AP 回复；内核 timer 已 deferred | 普通长内核区间尚未常态开放中断窗口，STOP 和 shootdown 尚未接入 |
| current task | 全局 PROCESSOR、current 裸指针、12 个身份 hint 和 syscall 诊断缓存 | 跨核读到其他 CPU 的任务、悬空引用或可变 hint 失配 |
| 调度 | 全局 VecDeque ready queue | 全局锁争用、重复出队、无法表达 CPU 所有权 |
| 阻塞任务 | interruptible_queue 同时承担枚举、清理、统计和唤醒辅助 | 与 per-CPU runqueue 职责重叠，旧重复唤醒扫描依赖全局队列 |
| timer | CPU0 hard IRQ 只发布 per-CPU pending；旧 timer 工作已移至 trap-return/scheduler 安全点 | 调度 tick 和全局 timer owner 尚未 per-CPU/CPU0 化，AP timer 仍关闭 |
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
- CPU0 继续独占现有全局 PROCESSOR、ready queue 和 run_tasks()；AP 设置 online 后进入只检查
  release/mailbox 的 park loop，本阶段不得调用现有 run_tasks() 或运行普通任务。

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

#### 当前进度（SMP-P2-B09/B10/B11/B12）

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
- CPU0 目前只在 focused test 控制的窗口内开放中断。通用交叉发送、并发
  reason、10,000 次 ping-pong、STOP 和普通长 syscall 中断窗口仍未完成，
  Phase 2 状态保持 `partial`。

Phase 2 结束后设置一次人工 go/no-go 检查点：只有 trap 保存恢复、IPI 幂等、STOP 和 deferred
timer 均有双架构证据，才进入调度状态迁移；“能 ping-pong”不能替代内核中断安全证明。

### Phase 2.5：单核状态迁移 API 与本地 TLB batch

#### 实施内容

- 在仍只有 CPU0 调度任务时，将所有 task_status 写入集中到 transition API，并引入可编码
  `New/Blocked/Queued(cpu)/Running(cpu)/Zombie` 的原子调度状态；
- 在 smp_debug 下对非法转换、重复入队、队列 owner 不一致立即 panic；release 构建保留计数，
  使问题在 per-CPU runqueue 之前暴露；
- 逐一替换 wake、timeout、signal、block、yield、exit 的直接 task_status 写入；旧字段若暂时保留，
  只能是由 transition API 更新的兼容投影，不得继续作为并行真值来源；
- 盘点 interruptible_queue 的信号枚举、OOM、zombie 清理和统计调用方；先把 runnable 所有权与
  这些 registry 职责分离，CAS 成功后才允许入队，旧全局扫描不得再次把任务重复加入 ready queue；
- 在没有远程 CPU 使用用户 MM 时先引入本地 TlbBatch facade：复用现有 raw/no-flush 操作与
  本地 sfence.vma/invtlb，将所有已发布 PTE 修改机械收口到统一提交入口；
- 本阶段不实现 remote ack，但接口必须显式区分 unpublished/local-only/published，禁止把
  local-only 实现描述成 shootdown 完成。

#### 退出条件

- 单核 focused test 覆盖所有合法转换，非法转换和重复 wake 能稳定触发诊断；
- 仓库内不再有绕过 transition API 的 runnable 状态写入；
- interruptible_queue 不参与 runnable 唯一性判定，保留的 registry 职责有清晰 owner；
- 已发布 PTE 修改均通过 local TlbBatch，双架构单核 MM 回归不下降。

### Phase 3：Per-CPU 调度器与时间系统

#### 实施内容

- 删除全局 PROCESSOR、current-task 裸指针和全局 ready queue；把不可变 pid/tid hint 迁到
  per-CPU，可变身份 hint 按集中更新协议迁移或改读权威对象；
- 每 CPU 使用本地 Processor、RunQueue、idle context 和 zombie 回收队列；
- 本地选择继续保留 FIFO fast path 和现有 nice-aware 选择；
- 新任务或被唤醒任务的目标 CPU 选择规则固定为：
  - last_cpu 在线、在 affinity 内且负载不超过最小负载 +1 时优先复用；
  - 否则选择 affinity 内 nr_running 最小的 CPU；
- 远程入队后，如果目标 CPU idle 或任务优先级需要尽快运行，发送 RESCHEDULE IPI；
- Phase 3a 先只实现 per-CPU queue、目标选择和远程 enqueue；work stealing 默认关闭；
- Phase 3b 在 3a 唯一运行和远程唤醒门禁通过后再开启 steal：idle CPU 只从一个选定 victim
  取一个允许迁移的任务，整个过程不同时持有两个 runqueue 锁；
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

- Phase 3a 的 CPU-bound 内核任务能通过目标选择和远程 enqueue 分布到全部在线 CPU；
- 同一任务从不并发运行，重复 wake 不会重复入队；
- affinity、迁移、阻塞和远程唤醒压力测试通过；Phase 3b 另行验证 steal 并保持可关闭；
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
- RISC-V 现有 trap 入口/返回执行全量 sfence.vma，stale-TLB 用例必须建立“victim 无 trap
  观察窗口”或测试态暂停 victim timer，并记录窗口前后 trap count；普通用户循环不能单独证明
  远端 shootdown，因为周期 timer trap 可能偶然清掉旧项；
- TLB 用例同时校验 shootdown sequence/ack 和 ack 前 frame 不复用；LoongArch 作为不被
  trap 自动全刷掩盖的强暴露平台，必须单独保留证据；
- 完成 MM 专项测试后，才允许受控用户测试任务跨 CPU 运行。该测试必须是 hermetic 的
  CPU/MM-only workload，使用匿名或启动前预载内存；除串行化结果输出外，不进入尚未审计的
  文件系统、网络、VirtIO 或设备并发路径。

#### 退出条件

- 一核反复 unmap/protect/CoW，其他核并发访问时不出现旧权限或旧物理页；
- LoongArch 强制 ASID rollover 后无跨进程数据污染；
- shootdown 期间即使目标 CPU 正在执行长 syscall 也能及时 ack；
- frame 释放计数证明不存在 ack 前复用；
- RISC-V victim 观察窗口 trap count 不变，结果不能由 trap.S 的全量 sfence.vma 偶然制造。

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
- 用户任务在 TLB 和共享子系统门禁通过前保持 CPU0 affinity；
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
- [LoongArch Reference Manual, Volume 1](https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html)
- [QEMU 9.2 Invocation: TCG thread option](https://qemu.readthedocs.io/en/v9.2.0/system/invocation.html)
