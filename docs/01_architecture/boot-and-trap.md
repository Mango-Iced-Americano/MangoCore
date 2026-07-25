---
title: "启动与陷阱路径 (Boot and Trap Flow)"
category: architecture
status: draft
owner: MangoCore Team
last_updated: 2026-07-25
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
打通 BSP→AP 的 IPI mailbox/ack 单播，并扩展为一次向全部 online AP 广播。
AP 尚未进入调度器，也不会访问文件系统、网络和旧的单核运行队列；这些共享
路径仍由 CPU0 独占。

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
`sstatus` 和 `sepc`；Rust handler 只接受 Supervisor Software Interrupt，
先清 SSIP，再消费 per-CPU mailbox。其他内核异常仍然 panic。

LoongArch 复用现有内核 trap frame，但把 IPI fast path 放在 BADV 和 console
诊断之前。handler 先向 IOCSR `CORE_CLEAR` 写 1 清除 level-triggered
vector 1，再消费 mailbox，避免陈旧 BADV 产生误诊，也避免在尚未多核安全的
console 路径中打印。

两个架构的 IPI handler 都只执行原子操作：不分配内存、不获取普通锁、不
打印，也不直接切换任务。AP 仅打开 IPI 线路和全局中断，timer/external
interrupt 继续关闭；IPI 返回后重新进入 `wfi`/`idle 0`。CPU0 的 timer
interrupt 仍进入旧的 `task::timer_interrupt_handler()`，每 CPU timer、
STOP、RESCHEDULE 和 AP 调度循环属于后续 Phase 2/3 范围。

当前只允许 CPU0 向 AP 发起 PING。AP→CPU0 或交叉发送必须等 CPU0 的内核
timer interrupt 也改为“只记账、在安全点延迟处理”后再开放，否则为了接收
IPI 打开 CPU0 的内核中断，会让旧 timer handler 在任意内核位置直接调度。

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
内向全部 online AP 广播并逐项观察对应 ack。四核测试应至少覆盖三个 AP；
RISC-V 还应保留一次物理启动 hart 不等于 0 的映射证据。
