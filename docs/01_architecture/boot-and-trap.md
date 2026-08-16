---
title: "启动与陷阱路径 (Boot and Trap Flow)"
category: architecture
status: draft
owner: MangoCore Team
last_updated: 2026-08-14
tags: [architecture, boot, trap, syscall, smp]
entry_points:
  - "os/src/main.rs"
  - "os/src/smp.rs"
  - "os/src/hal/boot/mod.rs"
  - "os/src/hal/firmware/mod.rs"
  - "os/src/hal/arch/riscv/entry.asm"
  - "os/src/hal/arch/loongarch64/boot.rs"
code_paths:
  - "os/src/hal/platform/"
  - "os/src/hal/arch/riscv/time.rs"
  - "os/src/hal/arch/riscv/trap/"
  - "os/src/hal/arch/loongarch64/trap/"
---

# 启动与陷阱路径

## BSP/AP 启动总览

双架构把固件提供的 CPU/hart ID 统一传给 `rust_main(cpu_id, boot_arg)`。
`smp::register_cpu_entry()` 先建立硬件 ID 到逻辑 CPU ID 的映射，并安装
CPU-local 指针；逻辑 CPU0 进入 BSP 路径，其余 CPU 进入 AP 路径。

```text
firmware / QEMU
  → arch _start（按硬件 CPU ID 选择独立 boot stack）
  → rust_main(hardware_id, boot_arg)
  → register_cpu_entry() + install_cpu_local()
     ├─ logical CPU0 → bsp_main()
     │  → 冻结 BootInfo → CPU-local bootstrap
     │  → 冻结 FDT / 早期资源（.data.boot + .bss.boot）
     │  → BSS → MM → PlatformInfo → machine / random 全局初始化
     │  → bring_up_secondary_cpus()
     │  → initramfs → /init → /sbin/init (PID1) → test-runner
     │  → scheduler
     └─ logical CPU1..N-1 → secondary_main()
        → 等待 BSP Release
        → CPU-local bootstrap
        → 切换到 logical CPU 独占的 idle stack
        → 发布 idle，再发布 online
         → IPI-only idle loop 等待 scheduler-ready
         → 安装本 CPU 的内核页表根并刷新本地 TLB
         → RV64：初始化本 CPU PLIC context 后开放 SEIE
         → AP 本地调度循环
```

当前 Phase 1 已完成最小 AP 启动、独立 idle stack 和在线发布；Phase 2 已
打通 BSP→AP 的 IPI mailbox/ack 单播与广播、AP→BSP 请求/回复往返，以及
CPU0 发起的 AP STOP/ack 终态协议。CPU0 的用户 syscall 已在完整 trap
frame 内开放受控 timer/IPI 窗口。B19 又建立 scheduler-ready 屏障和 AP 本地
调度循环，B20 允许受控 kernel-only 任务阻塞后回到最近运行 AP。B28 再增加一个
严格受限的用户探针：CPU0 构造并发布，CPU1 进入真实用户态执行 getpid/yield/exit，
CPU0 负责观察与回收。它不访问文件系统、网络或设备；在该里程碑上普通新任务和用户任务
仍由 CPU0 独占。B29 将同一探针改为先在 CPU0 起跑，再在 syscall 内真实 yield 后从 CPU1
恢复；这动态覆盖了跨 CPU 恢复 `schedule()`、trap current owner 和 MM 激活。B30 在这条
既有闭环上让 getcpu 返回连续逻辑 CPU：探针在 yield 前后分别验证 0 和 1，当时尚未改变
普通任务的发布策略。
B31/B32 又为该受控迁移建立 per-thread `cpus_allowed` 并返回真实 affinity；B33 不再依赖
显式 yield，而是让 CPU1 的生产 `RESCHEDULE` IPI 在 CPU0 用户 trap-return 安全点触发
同一 owner 交接。B34 允许 current 线程在 syscall 安全点修改运行期 mask，并在排除 source
时自迁到合法 CPU；B35 允许远程稳定 Blocked 线程在 registry 锁内修改 mask，并让后续 wake
按新允许集重新选点；B36 允许稳定 Queued 线程经短暂 `Migrating` 搬到合法 runqueue。
B38 又通过单槽请求完成远程 Running/Blocking owner 交接。B39 为所有在线 CPU 开放
独立 100Hz 调度 tick；TCB 构造时 affinity 仍以 bit0 作为未发布安全默认值。正式 normal
启动会在 AP 全部 online 后把 PID1 mask 扩为全部在线 CPU，fork/exec 后代继承该 mask；
评测拓扑使用 RV64 8 核、LA64 12 核。
B40 让永久 group exit 复用同一个任务安全点：进程退出码用原子快照发布，
远端 sibling 只在所属 CPU 上清理并以 live token ack；clone 的成员登记与首次
runqueue 发布由同一线程组门禁线性化。
B41 又把多线程 exec 接到同一安全点：临时 exec owner 关闭 clone 门并等待，
sibling 在自己的 CPU 完成旧用户映射/TLB 清理后 ack；只有 live count 收缩为
owner 一人时才替换地址空间。

## 启动栈与 BSS 边界

BSP 在 `mem_clear()` 前完成两项不能延期的工作：`boot::init_bsp()` 用
`spin::Once<RawBootInfo>` 冻结固件寄存器，`firmware::discover_early_resources()`
把固定容量资源表写入 `.data.boot`，并把双架构 FDT 字节复制到 `sbss` 前的
`.bss.boot`。FDT 原地址可能被后续内存初始化覆盖或回收，因此只保存指针并在
`mm::init()` 后解析是不成立的。

两项操作之间必须先执行 `bootstrap_init()`：入口参数冻结不依赖架构状态，必须最早
完成；固件资源解析涉及更复杂的 Rust 内存访问，LA64 必须先建立 DMW、异常入口和页表
寄存器基线。该顺序同时保证 RV64 直接复制 `a1` 中的 FDT，也允许 LA64 从 `a2`
的 EFI system table 解析 FDT 后再复制。

LA64 QEMU 和 2K1000 U-Boot 的入口都按 ABI 从 `a2` 接收 EFI system table；入口先保存
该寄存器，2K1000 再在退出 U-Boot DMW 环境后把低 40 位物理地址交给 Rust。EFI 表内的
嵌套指针仍由 Rust 逐项去除 DMW 段号。某些 QEMU direct-ELF 运行未填充 `a2`，此时仅当
EFI 查找精确返回 `MissingSystemTable`，内核才从 QEMU virt 规定的物理 `0x0010_0000` 捕获并
完整校验 FDT；其他 EFI/FDT 错误仍 fail-closed。2K1000 可在合法 FDT 缺失时退回静态板级
内存描述。

堆就绪后，`platform::init_platform()` 才把早期快照转换为含 `String/Vec` 的 owned
`PlatformInfo`。该对象在启动 AP 前通过 `spin::Once` 完整发布；AP 不解析固件数据、
不写平台状态，只读取 BSP 已发布的快照。

共享 SMP 表与双架构入口都按 12 个硬件 CPU 的上限预留独立 boot stack；正式
BuildStorm 拓扑由架构 Make 合同进一步限制为 RV64 8 核、LA64 12 核。入口在
使用栈之前验证 CPU ID，并按 `base + (cpu_id + 1) * BOOT_STACK_SIZE`
计算向下增长栈的初始栈顶。整个 `.bss.stack` 位于普通 `sbss` 之前，因此
CPU0 的 `mem_clear()` 不会清除 AP 正在使用的启动栈。

RV64 的可重定位 Image 会在进入 Rust 前建立临时 Sv39 根页表，同时提供 DRAM
低地址恒等映射、链接高半区别名和 supervisor physmap 三组窗口。每组都从运行期
Image 所在的 1 GiB 对齐 DRAM base 动态覆盖到 64 GiB 物理上限，而不是固定映射
16 个叶子；因此高地址 FDT 和 final physmap 构造期的 frame 访问都在临时页表内。
最终内核页表仍由固件报告的真实 RAM/MMIO 区间构造，不继承这些宽松的早期 RWX 叶子。

两架构还按 configured CPU 数预留页对齐的 `.bss.idle_stack`。该 section
由现有 `.bss.*` 通配符放入 `sbss..ebss`，因此 CPU0 会在 Release AP 前清零
它，且它始终属于内核镜像、不会交给 frame allocator。AP 完成本地 bootstrap
后通过 naked trampoline 更新 `sp`，保留 `a0` 和 `tp/$r21`，再跳到新栈上的
Rust idle 入口；旧 boot stack 的 frame/return 链不会继续使用。

越界 CPU 没有安全栈，必须留在汇编 park 循环，不能进入 Rust 或日志路径。

## 逻辑 CPU 与硬件 ID

OpenSBI 在 MTTCG 下可以让任意已配置 hart 赢得 cold-boot lottery，操作系统
不能假定物理 hart0 必然是 BSP。MangoCore 把实际 cold-boot hart 映射为
逻辑 CPU0，并为其他 hart 建立连续、可逆映射；调用 SBI HSM 启动 AP 时再把
逻辑 ID 反向转换为真实 hart ID。

LoongArch QEMU direct-kernel boot 的其他 CPU 停在 slave boot ROM。CPU0
通过 mailbox 写入 `_start` 地址，执行 `dbar` 后发送 IPI vector 0，使 AP
重新进入统一入口。进入内核后，运行期 IPI 改用 vector 1，避免和只服务于
slave ROM 的启动门铃混淆。2K1000LA 当前仍保持默认单核配置。

## BSP/AP 内存序

启动握手状态和 `PerCpu` 锚点位于 `.data.boot`，不会被 BSS 清零。

1. CPU0 独占 BSS、内存管理和机器级全局初始化。
2. CPU0 用 Release 发布 `BOOT_PHASE=AP_RELEASED`。
3. AP 用 Acquire 观察启动阶段，成功前只能访问自己的 boot stack 和
   `.data.boot`。
4. AP 完成本地 bootstrap 并切换 idle stack，在新栈上先以 Release 发布
   `idle=true`，再用 Release 把自己的 `online` 从 `false` 改为 `true`。
5. CPU0 用 Acquire 扫描各 `PerCpu.online`，并在有界超时内等待目标 mask；
   观察到 online 同时证明该 AP 已不再使用 boot stack。

CPU0 与 AP 使用同一个 online 发布协议；重复发布会触发 CAS 不变量失败，
而不是被静默接受。

online 只证明 CPU-local 启动完成，不等于可以访问调度器。B19 使用第二个屏障：

1. CPU0 完成 VFS、任务 registry 等全局初始化后，以 Release 发布
   `SCHEDULER_RELEASED`；
2. AP 用 Acquire 观察该状态，在自己的恒等映射 idle stack 上安装 BSP 已构造的
   内核页表根；RV64 写本地 `satp`，LA64 写本地 `PGDH`，并完成本地 TLB 刷新；
   RV64 随后才可经 direct map 初始化其 FDT 发布的 PLIC local context，并仅在成功后
   开放 SEIE。AP 在此之前保留 bootstrap 阶段已开放的 SSIE；缺少 context 时维持 SEIE
   关闭。
3. AP 进入 `run_tasks()` 后以 Release 发布 `scheduler_entered`；
4. CPU0 以 Acquire 等待全部 configured CPU 的 bit，再允许创建远程测试任务。

页表根寄存器是 CPU-local 硬件状态。CPU0 激活内核页表不能替 AP 完成这一步；
早期 IPI 能工作只说明恒等映射的 text/data/idle stack 可访问，不能证明高虚拟地址
kernel stack 已可用。

运行期 IPI mailbox 使用另一组 Release/Acquire 关系：

1. 发送方用 Release 把 reason 合并进目标 `PerCpu.pending_ipi`；
2. 广播时先发布全部目标 mailbox，再开始逐个触发 SBI 或 IOCSR doorbell；
3. 接收方先清硬件电平源，再用 Acquire `swap(0)` 消费 mailbox；
4. 需要完成语义的生产协议通过独立 sequence/ack 或 fixed slot 确认，
   `RESCHEDULE` 等提示类 reason 只保留幂等状态。

mailbox 表示“待处理原因集合”，不是可累计的事件队列。B95 删除早期启动阶段
只供 ktest 使用的 PING/ROUND_TRIP reason 和 Per-CPU ack 字段；focused test
改为直接验证生产 `MEMORY_BARRIER` 的 request/ack。TLB shootdown、membarrier
和 STOP 各自使用独立 sequence、slot 或终态 ack，不能把事件次数塞进 reason bit。
发送某个 doorbell 失败时，发送方仍继续通知本轮其余目标，并保留失败目标
已经发布的 reason；原子 mailbox 不能安全“回滚”，后续中断仍可消费它。

B19 为远程任务的动态内核栈增加了一个受限的映射发布协议。当前 LA64 保留原始
eager 协议：

1. CPU0 在 `KERNEL_SPACE` 锁内建立 stack PTE，然后释放页表锁；
2. CPU0 递增目标 CPU 的 `kernel_tlb_request`，发送 `KERNEL_TLB_SYNC`；
3. 目标 hard-IRQ handler 执行本地全 TLB 失效，并以 Release 发布相同序号的 ack；
4. CPU0 在不持有页表/runqueue 锁时有界等待 ack；
5. 只有 ack 完成后才把任务加入目标 runqueue，释放队列锁后再发送 `RESCHEDULE`。

sequence 允许合并同一目标的并发发布请求；ack 覆盖该序号之前的 PTE 写入。

RV64 production 已把新增任务映射改为目标侧延迟确认：发布方在 runqueue 可见前以
AcqRel 递增目标 `kernel_tlb_request`，但不发送 `KERNEL_TLB_SYNC`、不等待 ack；目标 CPU
取得任务后仍运行在 idle 栈上，在 `__switch` 改写 SP 前 Acquire 快照 request、执行本地
全 TLB 失效并 Release ack。目标就是当前 CPU 时仍立即本地失效。该路径不依赖 Svvptc
跳过 fence，因为新内核栈的首次压栈不能安全承受 stale-invalid 引发的偶发页故障。

B21 把同一 mailbox/sequence 基础设施扩展为动态 kernel-global 撤映射协议；它不使用
上述任务发布快路：

1. 在 `KERNEL_SPACE` 锁内清除 PTE，并把含 `FrameTracker` 的映射对象移出集合；
2. 释放页表锁后，先为所有远端目标发布独立 request 序号，再做本地全量失效并广播 IPI；
3. handler 必须按“Acquire 快照 request → 全量失效 → Release ack”的顺序执行，禁止用旧
   flush 确认刚发布的新序号；
4. 发起者等待时临时开放本地中断，使两个并发 shootdown 发起者能互相处理 IPI；窗口内
   timer 仍只发布 deferred work，随后由 trap-return 或 scheduler 安全点消费；
5. 对撤映射而言，已经完成终态 STOP 的 CPU 不会再访问共享状态，其 stopped ack 可替代
   TLB ack；新增映射发布不能使用这一替代；
6. 全部目标完成后才析构映射对象、释放 frame，并在内核栈路径最后归还虚拟 slot。

RV64 使用无地址/ASID 参数的 `sfence.vma`，LA64 使用 `invtlb 0`，两者都覆盖当前 CPU
的全部 global 与 non-global 翻译。该协议只覆盖共享内核页表的动态映射；用户 MM 的
range shootdown 由下面的独立协议处理。

B22/B23 为用户 MM 增加了独立于 kernel-global 的激活、IPI 与修改侧提交协议，
B51 又把保守的历史集合收紧为精确驻留集合：

1. 用户 trap-return 在恢复用户现场前，取得进程 VM 锁并把当前 CPU 加入该 MM 的
   `active_cpus`；RV64 随后在同一个 IRQ-off transaction 内安装目标 SATP，登记必须
   先于 generation/ASID epoch 读取；
2. 若本 CPU 的 observed generation 落后，RV64 执行全量 `sfence.vma`，LA64 执行
   `invtlb 0x3` 清除全部 non-global 项，然后重查 generation；
3. `MmuGather` 在 VM 锁内合并失效范围和退休 frame，并以 active CPU mask
   直接决定无需失效、本地失效或远端 shootdown；
4. `USER_TLB_SYNC` 使用独立 request/ack。handler 按“Acquire request → 本地全用户
   失效 → Release ack”执行，不分配、不获取 VM/PTE 或普通锁；
5. 锁外等待原语会临时开放本地 IRQ，以便并发 shootdown 发起者互相处理 IPI；调用者
   必须先释放 VM/PTE/runqueue 锁，并保留失效 frame 直到全部 ack。

B51 让每个 `CpuTaskState` 用一个 `Arc<AddressSpace>` 记住本 CPU 当前仍可直接返回的
用户 MM。任务切回 idle 栈后、改变 current owner 前，RV64 `leave_user_vm()` 先在 IRQ-off
transaction 内安装共享内核根，再在同一 VM 锁序下清除 active bit；再次运行必须重新经过
generation/epoch 检查。PTE writer 若在
active mask 为空时修改映射，不发送无意义 IPI，但仍推进 generation，保证保留旧 ASID
翻译的 CPU 下次进入前先本地补刷。Per-CPU Arc 同时固定 exec 前的旧 MM，避免
`process.vm` 已替换后误清新地址空间的 bit。

B23 通过 `AddressSpace::write()` 把同步边界固化到公开接口：锁内通过
`UserMapper` 修改 PTE、由 `MmuGather` 合并失效范围和退休 frame，再推进
generation 并冻结目标；块作用域结束后先解锁，
再执行本地失效、IPI/ack 等待、observed 推进和 frame 释放。调用方不再能取得可变
`MutexGuard<AddressSpace>`，因而不能把 VM 锁意外带过 shootdown 等待点。该里程碑仍把
普通用户任务固定在 CPU0；当前 normal PID1 已在 AP online 后显式开放全 CPU affinity，
不再依赖单核发布掩盖用户 PTE 的远端一致性问题。

B24—B27 已补齐双架构页级后端与 ASID 生命周期：用户 trap-return 通过
`activate_user_vm()` 一次取得页表根与 MM-owned ASID，编号只在全 CPU flush/ack 后跨
epoch 复用。RV64 探测 `SATP.ASID`，本地执行 `sfence.vma va, asid`，远端优先使用
SBI RFENCE FID 2；有 ASID 时调度换根不再固定全刷，普通 trap 不换根，ASIDLEN=0 时
保留真实换根后的兼容全刷路径。
LA64 通过固定 per-CPU slot 传递 ASID/range 并执行 `invtlb 0x5`。B51 已完成安全
CPU detach；B52 已把双架构精准后端扩展到最多 64 页的连续区间，更大跨度仍全刷。
B53 规定软件 range handler 在硬件失效后、slot ack 前推进目标 CPU 的 observed
generation，避免本次 IPI 返回用户态时再由激活路径全刷。真实用户 CoW 探针在静默 timer
窗口内验证了这一顺序，而不只观察 request/ack 计数器。B82 在同一窗口继续执行正式
`munmap + MAP_FIXED_NOREPLACE`，证明远端用户 load 不会在同一 VPN 重映射后继续读取旧 PPN。

B28 首次让受控用户任务在 AP 走完整 trap 路径。远程发布必须先同步新内核栈映射，
再把任务放入 CPU1 runqueue，最后在队列锁释放后发送 `RESCHEDULE`。用户 trap 进入
Rust 后由 `current_trap_task()` 克隆本 CPU current，并在锁外校验状态必须为
`Running(cpu)`；它取代旧的 CPU0-only 断言，但在该里程碑上没有改变普通任务的目标选择。
每次返回用户态仍由执行返回的 CPU 重写 trap context 中的 `tp/$r21` 锚点并激活该
MM 的页表根与 ASID。

B45 将 Rust 可变访问统一收口到
`TaskControlBlockInner::trap_context_mut(&mut self)`；B87 进一步删除物理地址层能从安全
函数返回 `'static` 引用的五个通用 helper。现在只有 TCB owner 在持有 `task.inner` guard
时，才会从 trap frame 物理页的直映 raw pointer 建立 `&mut TrapContext`，其生命周期绑定
到 `&mut self`。trap return 释放 guard 后只把既有 frame 地址交给不返回的恢复汇编；
frame 地址、布局和双架构 ABI 均未改变。

AP→BSP 方向由绑定 AP 的 kernel-only helper 调用生产
`synchronize_memory(bit(CPU0))` 覆盖。AP 先发布 CPU0 的 request，再触发
doorbell，并在不持普通锁时等待 ack；CPU0 在受控 IRQ-on 窗口进入共用的
用户/内核 IPI fast path，执行完整屏障后 Release 发布 ack。该链路直接证明
正式 membarrier mailbox，而不再维护“请求 AP 回送测试 IPI”的第二套协议。

AP 的等待协议是“关闭全局中断—重查 deferred work—执行一次架构
`wfi`/`idle 0`—恢复原中断状态”。本地 IPI line 始终保持 enabled，因此
doorbell 在重查之后到达时会保持 pending 并唤醒 CPU；不会出现检查为空后
永久睡眠的 lost wakeup。doorbell 失败诊断只更新 per-CPU 原子
计数，不把日志、锁或分配带回 hard IRQ。

STOP 复用 reason mailbox 传递终态请求，但使用独立的 `stopped` ack 表达
完成状态：

1. CPU0 快照 online 且尚未 stopped 的 AP，以 Release 发布 STOP 后触发
   doorbell；
2. AP hard-IRQ handler 只以 Release 发布 `stop_requested`，不在 trap
   frame 上停止；
3. AP 回到独立 idle stack，以 Acquire 消费请求，先关闭全局中断和全部
   本地 IPI source，再以 Release 发布 `stopped=true`；
4. AP 随后只执行不可返回的 `wfi`/`idle 0`，不再恢复中断或访问共享状态；
5. CPU0 以 Acquire 等待全部目标 ack，等待有界；重复调用会排除已经 stopped
   的 AP，因此正常 shutdown 与测试共用同一幂等协议。

只有逻辑 CPU0 负责协调 STOP。极早期尚未发布 online 的 panic，以及 AP
上的致命异常，直接进入架构机器级关机兜底，避免在 CPU-local 尚不可用或
尚无 CPU0 安全点时伪造跨核协调。

## CPU-local 寄存器

内核运行时以架构寄存器保存 `PerCpu` 指针：

| 架构 | CPU-local 寄存器 | 用户 trap 保存槽 |
|---|---|---|
| RISC-V | `tp` | `TrapContext.kernel_cpu_local`，偏移 `70 * 8` |
| LoongArch64 | `$r21` | `TrapContext.kernel_cpu_local`，偏移 `70 * 8` |

用户态也可以使用这些通用寄存器，因此返回用户态前把当前内核 CPU-local
指针写入 trap context；下次 trap 入口在执行 Rust handler 前先恢复该指针。
`cpu_id()` 会验证指针属于静态 `PER_CPUS` 数组且按表项对齐，再进行索引，
避免损坏的 trap 偏移演变为任意指针解引用。

## syscall 与异常

```text
user a7/a0..a5 → arch trap entry → trap handler
→ syscall::syscall(id, args) → sys_xxx → trap_return → user
```

两架构使用 `a7` 传 syscall ID、`a0..a5` 传参数、`a0` 返回结果。缺页经
`AddressSpace::do_page_fault()` 分流到 VMA、filemap、shared-write 或
CoW；所有 PTE 改动必须经 HAL 刷新 TLB。

RISC-V 在 `trap_return()` 的 `switch_user_vm()` 中先安装目标 `satp`，trampoline 中的
`__restore` 只恢复现场；普通 trap 入口和返回都保持当前页表根。用户根共享
supervisor-only 内核分支，因此 Rust trap handler 可直接在用户根下执行。
LoongArch static link 直接使用已链接的 `__restore` 地址，避免对符号重复
重定位后误跳入 kernel trap stub。

## 内核 IPI trap

RISC-V 的 `stvec` 指向独立的 `__kern_trap`。入口在当前内核栈上建立
272 字节、16 字节对齐的 frame，保存 `x1`、`x3..x31`、原始 `sp`、
`sstatus` 和 `sepc`；Rust handler 接受 Supervisor Software Interrupt
和 Supervisor Timer Interrupt，其他内核异常仍然 panic。IPI 先清 SSIP，
再消费 per-CPU mailbox；timer 只把已选择 backend 的 one-shot compare 推到最大值
并发布 deferred 状态。RV64 backend 可能是 Sstc `stimecmp`，也可能是 SBI fallback。

LoongArch 复用现有内核 trap frame，但把 IPI fast path 放在 BADV 和 console
诊断之前。handler 先向 IOCSR `CORE_CLEAR` 写 1 清除 level-triggered
vector 1，再消费 mailbox，避免陈旧 BADV 产生误诊，也避免在尚未多核安全的
console 路径中打印。timer fast path 同样先于 BADV 诊断；它先清 `TCFG.En` 停止倒计时，
再写 TICLR 清除 level-triggered pending，最后只发布当前 CPU 的 deferred 状态。

两个架构的 IPI handler 只执行原子操作和有界的本地 TLB 失效：不分配内存、不获取
普通锁、不打印，也不直接切换任务。CPU0 已打开 RV64 SSIE 或 LA64 ECFG.IPI，
用户态和内核态 trap 共用同一 fast path；B39 后 AP 同时打开 IPI 与本地 timer，
external interrupt 继续关闭。AP 的 timer 只发布本地调度工作，不执行全局 callback。

B92 为这条生产路径增加了逐 CPU 诊断：发送端在 mailbox 发布后按目标 CPU 数累计各类
reason，接收端分别累计 hard handler 进入次数和实际从 mailbox 消费的 reason bit，硬件
doorbell 失败则统一在 `send_ipi_mask()` 记到发起 CPU。计数全部使用 `Relaxed`，不参与
mailbox/ack 的正确性同步。由于同一个 reason 在接收前可合并为一个 bit，`published` 大于
`consumed` 是允许的，不能把两者差值直接解释成“丢 IPI”；应结合 request/ack、失败计数和
目标 CPU 状态判断。

AP 的回复 doorbell 和不可返回 STOP 都在返回 idle
stack 后执行，而不在 handler 内递归触发跨核操作或遗弃 trap frame。
`RESCHEDULE` handler 只设置本地提示；真正 fetch/context switch 发生在 AP idle，或
运行中用户任务的 trap-return 安全点。AP 执行 B19 短 kernel-only 函数时仍保持全局 IRQ
关闭，因此 STOP/RESCHEDULE 最长
延迟到函数返回或主动 yield；通用内核线程必须等可抢占安全点完成后才能开放。

## Syscall 受控中断窗口

双架构用户 trap 入口都会先完整保存用户寄存器，安装 kernel
trap vector 和 CPU-local `tp/$r21`，再进入 Rust。syscall 分支先在
IRQ-off 状态下短暂持有 `task.inner`，只用于取参数和更新入口计时；
释放锁后才通过 `with_local_interrupts_enabled()` 执行真正的
`syscall()`。闭包返回后先关闭中断，才重新获取 trap context 写回
结果、处理信号并返回用户态。

这里不能让 trap handler 的临时 `Arc<TaskControlBlock>` 跨过 `syscall()`：
`exit/exit_group` 会切回 idle 且永不返回当前 Rust 栈帧，局部变量不会自动析构。
因此入口在调用 syscall 前显式 drop；只有返回型 syscall 才重新调用
`current_trap_task()`。后一次调用会重新读取 CPU ID，既避免引用泄漏，也为未来
“syscall 内 yield 后在另一 CPU 恢复”保留正确 owner 校验语义。B28 动态覆盖两次
返回用户态和一次非返回 exit trap，不把 exit 描述成一次完整往返。B29 已把此前的
“未来”变成受控现实：yield 前 trap 属于 CPU0，`schedule()` 在 CPU1 返回后，syscall
handler 重新取得 `Running(1)` current，随后由 CPU1 的 `trap_return()` 重写 CPU-local
指针并激活同一 MM。

`TaskContext` 与双架构 `__switch` 只保存 callee-saved GPR，不保存
`sstatus.SIE` 或 `CRMD.IE`。因此 `schedule()` 把中断状态作为任务的
动态切换快照：

1. 获取任何 scheduler 锁前先保存并关闭本地中断；
2. idle scheduler 始终接管 IRQ-off CPU，不把某个任务的窗口泄漏到
   console、net 或 FS housekeeping；
3. 原任务再次被切入、`__switch` 返回后，才恢复它切出前的状态；
4. `exit/exit_group` 等不返回路径不会析构窗口 guard，但
   `schedule()` 仍会在永久切离前把 CPU 关中断；
5. panic handler 在任何 console/锁诊断和 STOP 之前立即关闭本地中断。

B40 把所有线程退出收口到 `finish_current_exit()`。该入口不猜测调用者来自
syscall 的 IRQ-on 窗口还是 fatal signal 的 IRQ-off trap-return：先统一关中断，
再通过 `with_local_interrupts_enabled()` 执行 clear_child_tid、用户映射撤销和
TLB shootdown，最后由不返回的 `schedule()` 接管 IRQ-off idle。这样目标 CPU
等待远端 TLB ack 时仍能处理本地 IPI。

B41 的安全点先检查永久 group exit，再检查“当前线程是否是另一线程 exec 的
sibling”。前者决定统一进程退出码，必须覆盖后者的私有 SIGKILL。Completion 和
WaitQueue 只因这两类生命周期停止请求返回 `Interrupted`，普通信号仍按原语自身
规则处理；调用层先释放 syscall 栈上的 `Arc`，再回到同一个安全点完成 owner 自清理。

这是安全点抢占而不是任意内核指令抢占：timer hard IRQ 仍只发布
pending，IPI hard IRQ 仍只操作 per-CPU 原子状态，两者都不在被打断点
切换任务或获取普通锁。

## Timer hard/deferred 边界

每个在线 CPU 的用户/内核 timer trap 共用同一 hard-IRQ fast path：

1. RV64 通过启动时选定的 Sstc `stimecmp` 或 SBI fallback，把 timer compare 写成
   `usize::MAX`；LA64 先清 `TCFG.En` 停止计数，
   再写 TICLR 清除 level-triggered pending；
2. 当前 `PerCpu.timer_irq_count` 只做无锁诊断计数；
3. 以 Release 发布 `timer_pending=true` 后立即返回被中断现场。

hard IRQ 不读取 timer queue，不执行 callback、timeout wake、timerfd、网络
poll 或 schedule；性能统计也只做原子计数，原有周期性快照打印已经移到
deferred 阶段。多个 IRQ 可以合并成一个 pending bit，因为软件 timer 和
调度 tick 都使用绝对 deadline，而不是按中断次数推进。

`trap_return()` 在信号投递前进入统一的 `run_task_safe_point()`；各 CPU 的
`run_tasks()` 则在取得
Processor/runqueue 锁前消费 timer pending。统一安全点保持 IRQ-off，先以 Acquire 取走
group-exit 原子码；命中时当前 owner 直接进入本地退出清理。普通路径再取走 timer
pending 并完成本地 tick/one-shot 重编程，再取走本 CPU 的 `need_resched`，最后对
两类请求最多调度一次。B33 因而能在完整用户 trap frame 上响应远端 RESCHEDULE，同时
保持 hard IRQ 不直接切换。B39 让 CPU0 的硬件 deadline 取“本地 tick/最早全局 timer”
的较小值，AP 只按本地 tick 编程；timeout、timerfd、网络 poll 和 kernel timer callback
仍只能在 CPU0 的 deferred 分支执行。

AP 若插入新的最早全局 timer，会先在队列锁内发布 deadline，解锁后设置 CPU0 的
`timer_reprogram_requested`，再发送 `TIMER_REPROGRAM` IPI。CPU0 即使正运行 IRQ-off idle
循环也能直接观察该标志；IPI handler 不读取 timer queue。安全点抢占模型仍允许长 syscall
把 callback 推迟到返回边界，但请求不会丢失，CPU0 的周期 tick 也提供有界兜底重查。

CPU0 与 AP 共用 `cpu_wait_for_interrupt()`，在 IRQ-off 的队列重查之后进入架构等待。
RV64 直接执行 `wfi`；LoongArch `idle 0` 要求 `CRMD.IE=1`，HAL 因而使用一段对齐的
“开 IE→idle”汇编区域。kernel timer/IPI 若打断该区域，trap 分发在返回前把保存 PC
重定向到区域出口，避免处理完唯一 wake event 后再次执行 `idle 0`；HAL 返回调度器前
重新关闭 IE。远程唤醒必须先发布 runqueue，再发送 RESCHEDULE IPI，才能与这一
check→wait 协议共同保证无丢失唤醒。

该边界消除了 CPU0 接收内核 IPI 的 timer 前置风险。普通长 syscall
现在可在任务 yield/block 之前、之后响应 timer/IPI；B23—B27 的 TLB shootdown
reason/ack 已复用这个窗口，B33 又接入 RESCHEDULE，不需要为“长 syscall 能否被
打断”另建一套 trap 路径。

## 构建与验证

构建期 `CORE_NUM` 导出为 Cargo 环境变量 `MANGO_CORE_NUM`，仅作为 FDT `/cpus`
探测失败时的兜底；运行时的实际 CPU 数（`smp::runtime_cpu_count()`）在启动早期
从 FDT `/cpus` 的 `cpu@N` 子节点探测并截断到编译期上限 `MAX_CPUS`（Linux
`NR_CPUS`/`nr_cpu_ids` 双轨制）。因此同一镜像可运行在不同核数的 QEMU 上：

```text
-smp cpus=N,sockets=1,cores=N,threads=1
```

常用 Docker 内验证入口：

```bash
make kernel ARCH=rv64 PROFILE=normal CORE_NUM=2
make kernel ARCH=la64 PROFILE=normal CORE_NUM=2
make ktest ARCH=rv64 PROFILE=normal CORE_NUM=2 KTEST=smp
make ktest ARCH=la64 PROFILE=normal CORE_NUM=2 KTEST=smp
```

双架构构建必须串行。focused SMP 测试不仅检查 QEMU 退出码，还要检查
configured CPU 数、online/idle mask、独立 CPU-local 指针、测试 PASS 和无
panic。Phase 2 的 IPI 用例通过生产 `MEMORY_BARRIER` sequence/ack 验证
CPU0→单个 AP、CPU0→全部 AP 和 AP→CPU0 三个方向；多核 AP→BSP 用例让每个
AP 各完成 64 轮正式同步，并在每轮 ack 后才推进下一序号。RISC-V
应保留一次物理启动 hart 不等于 0 的映射证据。deferred timer 用例连续
执行两轮真实内核 timer IRQ，分别断言 hard IRQ 不推进 deferred 计数、
不切换当前任务，以及安全点恰好消费一批并成功重编程下一轮。STOP 属于
终态测试：普通用例按 `KREPEAT` 全部完成后只执行一次，并断言全部 AP ack
以及生产 shutdown 再次调用协议时走幂等快路径。
B14 还必须在窗口内真实 yield：新任务首次从 idle 切入时观测
IRQ-off，原任务恢复后观测 IRQ-on，并在恢复后完成一次真实
AP→BSP membarrier IPI/ack。
B29 要求双架构 8 核日志实际出现 `smp::user_task_migrates_on_yield`，并验证
CPU0 起跑、yield 后 CPU1 续跑、CPU0 wait/reap、退出后的 current/runqueue/zombie 与
TCB 强引用都已收口；
仅看到测试总数或 QEMU 退出 0 不足以判定通过。
B30 沿用同一测试名，但用户探针必须自行验证 getcpu 在迁移前返回逻辑 CPU0、迁移后返回
逻辑 CPU1，并把任一失败编码为非零 exit status。这里的逻辑编号来自 PerCpu/scheduler 映射，
不能用可能非零的 RISC-V 启动 hart ID 替代。
B33 将当前测试名更新为 `smp::user_task_reschedules_from_ipi`：CPU1 helper 必须在首次
CPU0 getcpu 后发送真实 RESCHEDULE，CPU0 的安全点消费计数必须增长，probe 不执行显式
yield 且最终仍在 CPU1 观察 getcpu=1、affinity=`0b11`、exit(0)。只看到迁移或计数之一
都不足以证明 IPI 返回安全点闭环。
