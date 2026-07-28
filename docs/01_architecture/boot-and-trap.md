---
title: "启动与陷阱路径 (Boot and Trap Flow)"
category: architecture
status: draft
owner: MangoCore Team
last_updated: 2026-07-28
tags: [architecture, boot, trap, syscall, smp]
entry_points:
  - "os/src/main.rs"
  - "os/src/smp.rs"
  - "os/src/hal/arch/riscv/entry.asm"
  - "os/src/hal/arch/loongarch64/boot.rs"
code_paths:
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
     │  → BSS / MM / machine / random 全局初始化
     │  → bring_up_secondary_cpus()
     │  → initramfs → /init → /sbin/init (PID1) → test-runner
     │  → scheduler
     └─ logical CPU1..N-1 → secondary_main()
        → 等待 BSP Release
        → CPU-local bootstrap
        → 切换到 logical CPU 独占的 idle stack
        → 发布 idle，再发布 online
        → IPI-only idle loop
```

当前 Phase 1 已完成最小 AP 启动、独立 idle stack 和在线发布；Phase 2 已
打通 BSP→AP 的 IPI mailbox/ack 单播与广播、AP→BSP 请求/回复往返，以及
CPU0 发起的 AP STOP/ack 终态协议。CPU0 的用户 syscall 已在完整 trap
frame 内开放受控 timer/IPI 窗口；AP 尚未进入调度器，也不会访问文件系统或网络。
生产任务执行仍由 CPU0 独占；Per-CPU RunQueue 虽已建立，AP 尚未进入调度循环。

## 启动栈与 BSS 边界

RISC-V 和 LoongArch 都为最多 8 个硬件 CPU 预留独立 boot stack。入口在
使用栈之前验证 CPU ID，并按 `base + (cpu_id + 1) * BOOT_STACK_SIZE`
计算向下增长栈的初始栈顶。整个 `.bss.stack` 位于普通 `sbss` 之前，因此
CPU0 的 `mem_clear()` 不会清除 AP 正在使用的启动栈。

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

运行期 PING IPI 使用另一组 Release/Acquire 关系：

1. 发送方用 Release 把 reason 合并进目标 `PerCpu.pending_ipi`；
2. 广播时先发布全部目标 mailbox，再开始逐个触发 SBI 或 IOCSR doorbell；
3. 接收方先清硬件电平源，再用 Acquire `swap(0)` 消费 mailbox；
4. handler 完成后以 Release 增加 ack，等待方用 Acquire 观察完成。

mailbox 表示“待处理原因集合”，不是可累计的事件队列。当前 PING 测试由
CPU0 串行发送，同一目标收到 ack 后才复用 PING bit；后续需要累计语义的
shootdown/STOP 会使用独立 sequence 或 slot，不能把事件次数塞进 reason bit。
发送某个 doorbell 失败时，发送方仍继续通知本轮其余目标，并保留失败目标
已经发布的 reason；原子 mailbox 不能安全“回滚”，后续中断仍可消费它。

AP→BSP 往返把“中断内确认”和“发送回复”分成两个阶段：

1. AP hard-IRQ handler 只以 Release 发布 `round_trip_reply_pending`；
2. AP 返回 idle stack，在全局中断关闭时以 Acquire 消费 deferred work；
3. idle 路径调用普通 `send_ipi()`，向 CPU0 发布回复并触发 doorbell；
4. CPU0 共用用户/内核 trap 的 IPI fast path，以 Release 增加 reply ack；
5. 发起方以 Acquire 观察 ack 后，才复用同一 reason bit 发起下一轮。

AP 的等待协议是“关闭全局中断—重查 deferred work—执行一次架构
`wfi`/`idle 0`—恢复原中断状态”。本地 IPI line 始终保持 enabled，因此
doorbell 在重查之后到达时会保持 pending 并唤醒 CPU；不会出现检查为空后
永久睡眠的 lost wakeup。发送回复可能失败的诊断也只更新 per-CPU 原子
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

RISC-V 返回用户态时通过 trampoline 中的 `__restore` 切换 `satp`；
LoongArch static link 直接使用已链接的 `__restore` 地址，避免对符号重复
重定位后误跳入 kernel trap stub。

## 内核 IPI trap

RISC-V 的 `stvec` 指向独立的 `__kern_trap`。入口在当前内核栈上建立
272 字节、16 字节对齐的 frame，保存 `x1`、`x3..x31`、原始 `sp`、
`sstatus` 和 `sepc`；Rust handler 接受 Supervisor Software Interrupt
和 Supervisor Timer Interrupt，其他内核异常仍然 panic。IPI 先清 SSIP，
再消费 per-CPU mailbox；timer 只静默 SBI one-shot 并发布 deferred 状态。

LoongArch 复用现有内核 trap frame，但把 IPI fast path 放在 BADV 和 console
诊断之前。handler 先向 IOCSR `CORE_CLEAR` 写 1 清除 level-triggered
vector 1，再消费 mailbox，避免陈旧 BADV 产生误诊，也避免在尚未多核安全的
console 路径中打印。timer fast path 同样先于 BADV 诊断，并只清 TICLR、
发布当前 CPU 的 deferred 状态。

两个架构的 IPI handler 都只执行原子操作：不分配内存、不获取普通锁、不
打印，也不直接切换任务。CPU0 已打开 RV64 SSIE 或 LA64 ECFG.IPI，
用户态和内核态 trap 共用同一 fast path；AP 仅打开 IPI 线路，timer/external
interrupt 继续关闭。AP 的回复 doorbell 和不可返回 STOP 都在返回 idle
stack 后执行，而不在 handler 内递归触发跨核操作或遗弃 trap frame。
RESCHEDULE、非 syscall 内核区间和 AP 调度循环属于后续 Phase 2/3 范围。

## Syscall 受控中断窗口

双架构用户 trap 入口都会先完整保存用户寄存器，安装 kernel
trap vector 和 CPU-local `tp/$r21`，再进入 Rust。syscall 分支先在
IRQ-off 状态下短暂持有 `task.inner`，只用于取参数和更新入口计时；
释放锁后才通过 `with_local_interrupts_enabled()` 执行真正的
`syscall()`。闭包返回后先关闭中断，才重新获取 trap context 写回
结果、处理信号并返回用户态。

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

这是安全点抢占而不是任意内核指令抢占：timer hard IRQ 仍只发布
pending，IPI hard IRQ 仍只操作 per-CPU 原子状态，两者都不在被打断点
切换任务或获取普通锁。

## Timer hard/deferred 边界

CPU0 的两种 timer trap 来源共用同一 hard-IRQ fast path：

1. RV64 把 SBI timer compare 写成 `usize::MAX`；LA64 清除 level-triggered
   TICLR，非周期 TCFG 保持停止；
2. 当前 `PerCpu.timer_irq_count` 只做无锁诊断计数；
3. 以 Release 发布 `timer_pending=true` 后立即返回被中断现场。

hard IRQ 不读取 timer queue，不执行 callback、timeout wake、timerfd、网络
poll 或 schedule；性能统计也只做原子计数，原有周期性快照打印已经移到
deferred 阶段。多个 IRQ 可以合并成一个 pending bit，因为软件 timer 和
调度 tick 都使用绝对 deadline，而不是按中断次数推进。

`trap_return()` 在信号投递前消费 pending，使 timer callback 新产生的信号
可以在同一轮返回中处理；`run_tasks()` 则在取得 Processor/ready queue 锁前
消费 pending。安全点以 Acquire 取走 pending，在关中断状态下完成旧 timer
工作并按完整队列重新编程 one-shot；只有全部工作结束后才决定是否在这个
显式边界让出 CPU。AP 仍为 IPI-only，不运行普通 timer callback。

该边界消除了 CPU0 接收内核 IPI 的 timer 前置风险。普通长 syscall
现在可在任务 yield/block 之前、之后响应 timer/IPI；后续 TLB shootdown
还需在这个窗口上增加具体 reason/ack 协议，不需再为“长 syscall 能否被
打断”另建一套 trap 路径。

## 构建与验证

构建期 `CORE_NUM` 同时导出为 Cargo 环境变量 `MANGO_CORE_NUM`，并生成 QEMU
拓扑：

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
panic。Phase 2 的 IPI 用例要求 CPU0 既能单播 PING，也能在统一的一秒期限
内向全部 online AP 广播并逐项观察对应 ack；四核 AP→BSP 用例还要让三个
AP 各完成 64 轮顺序请求/回复，并在每轮 ack 后才复用 reason。RISC-V
应保留一次物理启动 hart 不等于 0 的映射证据。deferred timer 用例连续
执行两轮真实内核 timer IRQ，分别断言 hard IRQ 不推进 deferred 计数、
不切换当前任务，以及安全点恰好消费一批并成功重编程下一轮。STOP 属于
终态测试：普通用例按 `KREPEAT` 全部完成后只执行一次，并断言全部 AP ack
以及生产 shutdown 再次调用协议时走幂等快路径。
B14 还必须在窗口内真实 yield：新任务首次从 idle 切入时观测
IRQ-off，原任务恢复后观测 IRQ-on，并在恢复后完成一次真实
AP→BSP IPI reply。
