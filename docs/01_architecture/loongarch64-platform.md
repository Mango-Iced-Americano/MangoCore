---
title: "LoongArch64 平台后端"
category: architecture
status: stable
author: MangoCore Team
last_update: 2026-07-31
tags: [architecture, loongarch64, hal, smp, asid, tlb]
---

# LoongArch64 平台后端

## 1. 概述

LoongArch64 后端位于 `os/src/hal/arch/loongarch64/`。它向架构无关层提供 `LAFlexPageTable`、trap context、ASID/TLB、timer、上下文切换、console 和 shutdown。与 rv64 相比，la64 的 `bootstrap_init()` 承担更多早期机器配置，包括 exception entry、TLB refill entry、DMW、page walk 寄存器和 FPU/SIMD 状态。

关键类型别名：

```rust
pub type KernelPageTableImpl = laflex::LAFlexPageTable;
pub type PageTableImpl = laflex::LAFlexPageTable;
```

trap 类型由 `trap/mod.rs` 中的 `pub type TrapImpl = Trap` 提供，并通过 `hal/arch/mod.rs` re-export。

## 2. 模块地图

```
os/src/hal/arch/loongarch64/
├── mod.rs
├── acpi.rs
├── boot.rs
├── config.rs
├── entry.asm
├── kern_stack.rs
├── laflex.rs
├── mem_reg_macro.rs
├── register/
├── sbi.rs
├── switch.rs
├── switch.S
├── time.rs
├── tlb.rs
└── trap/
    ├── mod.rs
    ├── context.rs
    ├── mem_access.rs
    └── trap.S
```

| 文件 | 职责 |
|------|------|
| `mod.rs` | 后端聚合、类型别名、bootstrap/machine init |
| `config.rs` | 页表宽度、地址布局、DMW 常量、内存和平台常量 |
| `laflex.rs` | LoongArch64 页表实现 |
| `tlb.rs` | ASID 分配、TLB invalidate、TLB read/search |
| `register/` | CSR 和架构寄存器封装 |
| `time.rs` | timer frequency、当前时间、timer delta |
| `trap/mod.rs` | syscall、缺页、timer、非对齐访存和用户态返回 |
| `trap/mem_access.rs` | 非对齐访存模拟需要的指令解析 |
| `switch.S` | 任务上下文切换 |

## 3. `bootstrap_init()`

la64 的 `bootstrap_init()` 首先检查 CPU 核：

```rust
if CPUId::read().get_core_id() != 0 {
    loop {}
};
```

非 0 号核停在死循环。随后执行机器配置：

| 配置项 | 行为 |
|--------|------|
| 中断向量 | `ECfg` 设置 timer line-based interrupt vector |
| FPU/SIMD | `EUEn` 打开 floating point、SIMD、advanced SIMD |
| timer | `TIClr` 清 timer，`TCfg` 关闭早期 timer |
| CRMD | 关闭 watchpoint，打开 paging，关闭中断 |
| 普通异常入口 | `set_kernel_trap_entry()` |
| 机器错误入口 | `set_machine_err_trap_ent()` |
| TLB refill | `TLBREntry` 指向 `srfill` |
| DMW2 | PLV0 可用，SUC VSEG，StronglyOrderedUnCached |
| DMW3 | 清空 |
| 页大小 | `STLBPS`、`TLBREHi` 设置为 `PTE_WIDTH_BITS` |
| page walk | `PWCL`、`PWCH` 设置页目录层级和 PTE 宽度 |

最后输出 UART 地址和 `PRCfg1`。这些打印来自当前 `mod.rs`。

## 4. `machine_init()`

la64 运行期机器初始化：

```rust
pub fn machine_init() {
    // remap_test not supported for lack of DMW read only privilege support
    trap::init();
    get_timer_freq_first_time();
    /* println!(
     *     "[machine_init] VALEN: {}, PALEN: {}",
     *     cfg0.get_valen(),
     *     cfg0.get_palen()
     * ); */
    for i in 0..=6 {
        let j: usize;
        unsafe { core::arch::asm!("cpucfg {0},{1}",out(reg) j,in(reg) i) };
        println!("[CPUCFG {:#x}] {}", i, j);
    }
    for i in 0x10..=0x14 {
        let j: usize;
        unsafe { core::arch::asm!("cpucfg {0},{1}",out(reg) j,in(reg) i) };
        println!("[CPUCFG {:#x}] {}", i, j);
    }
    println!("{:?}", Misc::read());
    println!("{:?}", RVACfg::read());
    println!("[machine_init] MMAP_BASE: {:#x}", MMAP_BASE);
    trap::enable_timer_interrupt();
}
```

`trap::init()` 设置 kernel trap entry。`get_timer_freq_first_time()` 初始化 timer frequency。`trap::enable_timer_interrupt()` 设置 timer 中断向量；timer deadline 由后续 timer 子系统编程。

## 5. TLB refill 与 page walk

la64 后端包含 `__rfill()` 裸函数，放置在 `.text.__rfill` 段，用于 TLB refill。该汇编读取 PGD、按目录层级查找 PTE，并执行 `tlbfill`。找不到页表项时设置 refill 相关 CSR，仍通过 `tlbfill` 建立异常项。

page walk 寄存器配置来自 `bootstrap_init()`：

| 寄存器 | 配置 |
|--------|------|
| `PWCL` | `ptbase=PAGE_SIZE_BITS`、`ptwidth=DIR_WIDTH`、`dir1_base=PAGE_SIZE_BITS+DIR_WIDTH`、`pte_width=PTE_WIDTH` |
| `PWCH` | `dir3_base=PAGE_SIZE_BITS + DIR_WIDTH * 2`、`dir3_width=DIR_WIDTH` |
| `TLBREHi` | page size = `PTE_WIDTH_BITS` |
| `STLBPS` | page size = `PTE_WIDTH_BITS` |

这组配置说明 LAFlex 页表依赖硬件 page walk 参数，而不仅是软件页表遍历。

## 6. ASID 与 TLB

`mod.rs` 从 `tlb.rs` 导出低层 CSR/TLB 操作：

```rust
pub use tlb::{set_asid, tlb_global_invalidate, tlb_invalidate};
```

ASID 分配属于 MM 激活协议，不再作为 task 创建/析构时调用的公开接口。相关入口为：

| 函数 | 作用 |
|------|------|
| `init_asid_allocator()` | CPU0 从 `CSR.ASID.ASIDBITS` 读取硬件宽度并初始化编号范围 |
| `try_assign_asid(context)` | 在当前 epoch 内为 MM 取得或复用 ASID context；耗尽时返回 `None` |
| `rollover_asids()` | 全 CPU flush/ack 后推进 epoch，允许硬件编号重新分配 |
| `hardware_asid(context)` | 只取低 10 位硬件 ASID，绝不把软件 epoch 写入 CSR |
| `set_asid()` | 设置当前地址空间 ASID |
| `tlb_invalidate()` | 清除当前 CPU 的全部 non-global TLB 项 |
| `tlb_invalidate_user_page(asid, vpn)` | 按目标 MM 的 ASID 刷新指定虚拟页所在的硬件页对 |
| `tlb_invalidate_global_page(vpn)` | 刷新 global page |
| `tlb_global_invalidate()` | 全局刷新 |

`AddressSpace` 的 `TlbContext` 持有一个原子 `asid_context`：低 10 位是
`CSR.ASID[9:0]` 的硬件编号，高位是软件 epoch。同一地址空间的所有线程和 CPU 读取
同一个 context；TCB 不再持有或释放 ASID。一个 epoch 内只单调分配编号，MM 析构时
不立即回收。编号耗尽后，唯一 rollover leader 先通过 user-TLB IPI/ack 清空全部 online
CPU 的 non-global 项，收到全部确认后才发布新 epoch；这保证旧编号不会在 stale TLB
仍可命中时复用。

返回用户态时，`ProcessControlBlock::activate_user_vm()` 在同一个 `AddressSpace` 上取得
页表根和硬件 ASID 快照。`__restore` 比较并成对写入 `CSR.PGDL/CSR.ASID`，普通 context
switch 不再固定执行全量 `invtlb`。这与
[LoongArch 架构手册的 ASID/INVTLB 定义](https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html)
以及 [Linux LoongArch versioned ASID](https://codebrowser.dev/linux/linux/arch/loongarch/include/asm/mmu_context.h.html)
的原则一致：编号复用前统一换代失效，而不是每次切换地址空间都全刷。

Rust 到 `__restore` 的 ABI 桥接直接把 trap context、token、ASID 约束到
`$a0/$a1/$a2`，跳转地址使用独立寄存器。禁止用多个泛型 `in(reg)` 再顺序 `move` 到
参数寄存器：LLVM 可以让后续输入复用这些寄存器，模板内的前序写入会在消费前覆盖它。
从快照取得到 `ertn` 保持本地 IRQ 关闭，保证 rollover IPI 的 flush/ack 不会越过旧快照
的实际用户态恢复。

单页 shootdown 在持有 VM 锁时把目标 MM 的硬件 ASID 与 VPN 冻结进同一个 `TlbFlush`
快照；解锁后，每个发起 CPU 使用自己的固定原子 slot 发布这组 payload。IPI handler
扫描全部 slot，以 `invtlb 0x5` 完成 `G=0 + ASID + VA` 失效后才设置 slot 内 ack，期间
不分配内存，也不获取 MM 锁。多个 CPU 即使共用同一个 reason bit，其 payload 也不会
相互覆盖。LoongArch 普通 TLB entry 同时表示相邻偶/奇页，故 VA 必须对齐到
`2 * PAGE_SIZE`；这里的“页级精准”是指限定目标 MM/ASID 与目标硬件页对，并非只清除
一个 4 KiB 页。

## 7. Trap 分支

### 7.1 syscall

`Exception::Syscall` 分支：

| 步骤 | 行为 |
|------|------|
| 推进 ERA | `ERA::read().next_ins().write()` |
| 推进 trap context PC | `cx.gp.pc += 4` |
| 保存重启参数 | `cx.origin_a0 = cx.gp.a0` |
| 读取 ABI | `a7` 和 `a0..a5` |
| 分发 | `syscall(syscall_id, args)` |
| 写回返回值 | 非 id 139 时写回 `a0` |

### 7.2 缺页

匹配的异常包括：

```
PagePrivilegeIllegal
PageInvalidFetch
PageInvalidStore
PageInvalidLoad
PageModifyFault
PageNonReadableFault
PageNonExecutableFault
```

访问类型映射：

| 异常 | `FaultAccess` |
|------|---------------|
| `PageInvalidStore`, `PageModifyFault` | `Store` |
| `PageInvalidFetch`, `PageNonExecutableFault` | `Execute` |
| 其他 | `Load` |

缺页成功且原异常为 `PageModifyFault | PageInvalidStore` 时，后端调用 `LAFlexPageTable::set_dirty_bit(addr.floor())`。

### 7.3 信号异常

| 异常类别 | 信号 |
|----------|------|
| `InstructionNonDefined`、`Exception10`、`Exception11`、`Exception12`、`FloatingPointUnavailable`、`InstructionPrivilegeIllegal` | `SIGILL` + `ILL_ILLOPC` |
| `AddressError` | `SIGSEGV` + `SEGV_MAPERR` |

### 7.4 timer

`Interrupt::Timer` 分支清除 timer 中断状态：

```rust
TIClr::read().clear_timer().write();
crate::task::timer_interrupt_handler();
```

同时记录调度和 timer 统计。

## 8. 用户非对齐访存模拟

`Exception::AddressNotAligned` 分支执行以下流程：

```
current_task() 持有当前 TCB
task.inner：快照 pc 以及 store 源寄存器
释放 task.inner
copy_from_user(token, pc, &mut instruction)
Instruction::from(instruction).get_op_code()
addr = BadV::read().get_vaddr()
根据 load/store 和宽度逐字节 copy_from_user/copy_to_user
符号扩展 load 结果
task.inner：确认 pc 未变，写回 load 结果并推进 pc
```

模拟路径只接受大小为 2、4、8 的访问。用户访存可能触发缺页或 TLB shootdown，因此不得
跨这些操作持有 `task.inner`。重新加锁后会先确认 PC 仍等于入口快照；若不相等，说明有路径
越过 current-owner/inner 协议修改了同一个 trap frame，内核会立即报错而不是静默覆盖。
当前通用测试不保证触发该硬件异常，整数/浮点未对齐指令仍需要专门用户态用例补充覆盖。

## 9. 返回用户态

la64 `trap_return()` 的关键语义：

| 步骤 | 行为 |
|------|------|
| 信号 | `do_signal()` |
| user entry | `set_user_trap_entry()` 设置 exception entry 为 `strampoline` |
| privilege | `PrMd` 设置 `pplv=3`、`pie=true` |
| 参数 | trap context、用户 token、ASID 传给 `__restore` |

该路径保证信号交付发生在恢复用户态前。

### 9.1 用户态返回地址布局与 trap context 槽位

LA64 的用户态返回相关虚拟地址按从低到高排列为：

```
trap-context window → TRAMPOLINE → SIGNAL_TRAMPOLINE
[KERNEL_STACK_MAX_SLOTS pages]
```

用户 mmap arena 是半开区间 `[USR_MMAP_BASE, TRAP_CONTEXT_BASE)`。旧的 `USR_MMAP_END == TRAMPOLINE` 会使该 arena 错误地覆盖 `[TRAP_CONTEXT_BASE, TRAMPOLINE)`，因此不能再把 `USR_MMAP_END` 解释为 trampoline 地址。当前 exclusive end 为 `TRAP_CONTEXT_BASE`。

`TRAMPOLINE` 位于 trap-context window 之上，不属于该窗口。普通 mmap 和 SysV shm mmap 对 LA64 的 `MAP_FIXED`、`MAP_FIXED_NOREPLACE` 请求，必须在 unmap 前检查请求区间；只要与 `[TRAP_CONTEXT_BASE, TRAMPOLINE)` 相交就拒绝。`tid_alloc()` 使用从 1 开始的编号，合法槽位 `tid` 的底部地址为：

```text
trap_cx_bottom_from_tid(tid) = TRAP_CONTEXT_BASE + (tid - 1) * PAGE_SIZE
```

实现必须拒绝 `tid < 1` 或 `tid > KERNEL_STACK_MAX_SLOTS`，窗口范围是从 `TRAP_CONTEXT_BASE` 开始的连续 `KERNEL_STACK_MAX_SLOTS` 个 trap-context pages。新映射成功时直接返回新映射的 PPN，调用方不应从 trampoline 或其他相邻地址重新推导 PPN。这个布局和范围约束保护 `trap_return → __restore` 使用的 frame pointer，避免 trap context 映射覆盖 trampoline 物理页。

2026-07-21 的最终双架构 regression 已验证 mmap 边界和第二个槽位：RV64 和 LA64 均完成 TAP `1..6`，包含 `ok 2 mmap_edge_cases` 和 `ok 6 clone_vm_second_slot`，LA64 分类器为 `STATE=PASS STATUS=0`。最终证据目录为 `docs/Work_Log/evidence/2026-07-21/la64-mmap-boundary-final-20260721T060040+0800/`。该 focused regression 不代表 full LTP 或 basic 全量覆盖。

LoongArch64 后端比 rv64 多两个需要重点理解的机制：ASID 和硬件 dirty/page-modify
语义。ASID 在地址空间第一次激活时分配，同一 MM 的线程共享；返回用户态时页表根和
ASID 作为一个快照传给恢复汇编。页被写入时可能先触发 page modify，trap 后端通过
`LAFlexPageTable::set_dirty_bit()` 补 dirty bit，再让用户指令重试。这些机制使 la64 的
“同一个虚拟地址”是否命中旧 TLB，不仅取决于页表内容，也取决于 MM-owned ASID、epoch
和 invalidate 是否正确。

非对齐访存模拟是 la64 的另一个架构特有路径。`AddressNotAligned` 分支读取用户 PC 处指令，解析访问宽度和方向，通过 uaccess 读写目标地址，然后手动推进 PC。调试该路径时必须同时确认三点：指令解码成功、用户内存访问返回正确 errno、模拟成功后 PC 推进 4 字节。

## 10. 调试入口

| 症状 | 文件 | 检查点 |
|------|------|--------|
| la64 启动早期卡住 | `mod.rs::bootstrap_init()` | core id、DMW/page walk、TLB refill entry |
| 缺页后仍反复写 fault | `trap/mod.rs`, `laflex.rs` | `set_dirty_bit()` 和 TLB 刷新 |
| ASID 异常 | `tlb.rs`, `mm/tlb.rs`, `address_space.rs`, trap return | `asid_context` epoch、rollover flush/ack、页表根/ASID 快照 |
| 非对齐访存 panic | `trap/mem_access.rs` | 指令解析、访问宽度、PC 推进 |
| timer 不触发 | `time.rs`, `trap/mod.rs` | timer frequency、`TIClr`、interrupt vector |

## 11. 测试映射

| 测试目标 | 覆盖代码 | 命令/用例 |
|----------|----------|-----------|
| la64 编译 | 全后端 | `cd os && make la64-kernel-build-only` |
| la64 启动 | bootstrap、machine init、trap | `cd os && make la64-run` |
| syscall ABI | syscall trap 分支 | basic、busybox、LTP syscall |
| 页表/TLB | LAFlex、TLB、dirty bit | mmap、fork、exec、mprotect、munmap |
| 非对齐访存 | `AddressNotAligned` 分支 | 触发 la64 用户非对齐 load/store 的 libc/LTP 用例 |
| timer | time + trap + task | nanosleep、futex timeout、timer syscall |

## 12. 源文件索引

| 路径 | 内容 |
|------|------|
| `os/src/hal/arch/loongarch64/mod.rs` | 后端聚合、bootstrap、machine init |
| `os/src/hal/arch/loongarch64/config.rs` | 地址、页表和平台常量 |
| `os/src/hal/arch/loongarch64/kern_stack.rs` | 内核栈 |
| `os/src/hal/arch/loongarch64/laflex.rs` | 页表实现 |
| `os/src/hal/arch/loongarch64/tlb.rs` | ASID 和 TLB 操作 |
| `os/src/hal/arch/loongarch64/register/` | CSR 封装 |
| `os/src/hal/arch/loongarch64/time.rs` | timer 和时间 |
| `os/src/hal/arch/loongarch64/trap/mod.rs` | trap 分派和返回用户态 |
| `os/src/hal/arch/loongarch64/trap/mem_access.rs` | 非对齐访存指令解析 |
| `os/src/hal/platform/loongarch64/qemu.rs` | QEMU 平台常量 |
