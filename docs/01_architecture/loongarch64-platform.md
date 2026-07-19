---
title: "LoongArch64 平台后端"
category: architecture
status: stable
author: MangoCore Team
last_update: 2026-07-10
tags: [architecture, loongarch64, hal]
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
| 缩减虚地址 | `RVACFG.RBits=0`，关闭固件可能遗留的 reduced-VA 模式 |
| DMW2 | PLV0 可用，SUC VSEG，StronglyOrderedUnCached |
| DMW3 | 清空 |
| 页大小 | `STLBPS.PS=12`、`TLBREHi.PS=12`，对应 4KiB 页 |
| page walk | `PWCL`、`PWCH` 设置页目录层级和 PTE 宽度 |

最后读取 `CPUCFG1`，在 `mm::init()` 之前断言硬件 `VALEN/PALEN` 与构建常量一致，再输出 UART 地址和 `PRCfg1`。这使错误的平台 feature 或位宽常量在建立页表前直接失败，而不是到首次高地址访问时才表现为 `AddressError`。

## 4. `machine_init()`

la64 运行期机器初始化：

```rust
pub fn machine_init() {
    // remap_test not supported for lack of DMW read only privilege support
    trap::init();
    get_timer_freq_first_time();
    let cfg1 = CPUCfg1::read();
    println!(
        "[machine_init] address bits: hardware VALEN={} PALEN={}, build VALEN={} PALEN={}",
        cfg1.get_valen(),
        cfg1.get_palen(),
        VALEN,
        PALEN
    );
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

地址位宽来自 `CPUCFG1`，其中 `PABITS` 位于 `[11:4]`、`VABITS` 位于 `[19:12]`，字段值均需加一。QEMU 报告 `PALEN=VALEN=48`；当前 2K1000LA 实板的 `CPUCFG1=0x03e2727e`，解码为 `PALEN=VALEN=40`。平台构建常量必须与硬件一致，否则 CPU 可能在页表查询前直接产生 `AddressError`。

40 位虚拟地址的高半规范区从 `0xffffff8000000000` 开始。2K1000 的 guarded kernel stack 因此放在 `MMAP_END` 附近并向下分配；不能沿用 QEMU 的 `MMAP_BASE - PAGE_SIZE` 栈顶，因为该地址正好落在 40 位虚拟地址的非规范区。QEMU 仍保留原来的 48 位栈窗口。

2K1000 当前地址不变量如下：

| 项目 | 值 / 规则 |
|------|-----------|
| `VALEN/PALEN` | `40/40` |
| 合法低半区 | `0x0000000000000000..=0x0000007fffffffff` |
| 合法高半区 | `0xffffff8000000000..=0xffffffffffffffff` |
| `VA_MASK` / `SEG_MASK` | `0x000000ffffffffff` / `0xffffff0000000000` |
| 首栈顶 | `0xfffffffffffef000` |
| 1024-slot 窗口下界 | `0xfffffffff7bef000` |
| 首栈探针 | `0xfffffffffffeeff8` |

`VirtPageNum` 保存的是 canonical VA 逻辑右移 12 位后的 52 位页号表示。`VPN_MASK` 只保留 `VA[VALEN-1:12]`，`VPN_SEG_MASK` 恢复右移后仍应保留的高位符号扩展；TLB 的 paired-page `VPPN` 则只保存 `VA[VALEN-1:13]`。这些掩码均由 `VALEN` 推导，并有编译期断言覆盖高栈地址的 VA/VPN 往返。

软件页表只有 3 个 9-bit 索引，实际索引 `VA[38:12]`。因此 QEMU 的 48 位地址空间中，低 39 位相同的高地址会落到同一软件页表路径。`KERNEL_PROGRAM_END` 同时避开 2K1000 的真实栈窗口和 QEMU 中与栈窗口低 39 位相同的别名，临时内核 ELF 映射超出该上界会返回 `BadAddress`。

2K1000 PCI ECAM 物理地址 `0xfe00000000` 的 bit 39 为 1，若直接当普通 40 位页模式 VA 使用则不是 canonical 地址。CPU 对 ECAM/AHCI BAR 的访问使用 DMW2 的 VSEG=8、SUC 别名；DMA 描述符仍保存原始物理地址，不能把 DMW 虚拟别名交给设备。

## 5. TLB refill 与 page walk

la64 后端包含 `__rfill()` 裸函数，放置在 `.text.__rfill` 段，用于 TLB refill。该汇编读取 PGD、按目录层级查找 PTE，并执行 `tlbfill`。找不到页表项时设置 refill 相关 CSR，仍通过 `tlbfill` 建立异常项。

page walk 寄存器配置来自 `bootstrap_init()`：

| 寄存器 | 配置 |
|--------|------|
| `PWCL` | `ptbase=PAGE_SIZE_BITS`、`ptwidth=DIR_WIDTH`、`dir1_base=PAGE_SIZE_BITS+DIR_WIDTH`、`pte_width=PTE_WIDTH` |
| `PWCH` | `dir3_base=PAGE_SIZE_BITS + DIR_WIDTH * 2`、`dir3_width=DIR_WIDTH` |
| `TLBREHi` | `PS=PAGE_SIZE.trailing_zeros()=12` |
| `STLBPS` | `PS=PAGE_SIZE_BITS=12` |

`PTE_WIDTH_BITS=3` 只表示 8-byte PTE 的 `log2`，不能写入 TLB 页大小字段。旧代码把 3 写入 `STLBPS/TLBREHI.PS`，等价于 8-byte 页，是与 4KiB 软件页表不一致的严重配置错误。refill 失败分支也会先清空 `TLBREHI.PS[5:0]`，再写入 12，避免继承固件或前一异常中的旧值。

LAFlex PTE 和 `TLBELO/TLBRELO` 的物理页字段对应 `PA[PALEN-1:12]`。PTE 的 `PPN_MASK` 因此必须是 PALEN 位物理地址掩码再清除低 12 位，不能先左移整个 PALEN 掩码。TLBEHI 写入 VPPN 前必须裁剪到 `VALEN-13` 位；读回时左移一位恢复 paired-page VPN，并按 `VALEN` 对高地址 VPN 做符号扩展。

## 6. ASID 与 TLB

`mod.rs` 从 `tlb.rs` 导出：

```rust
pub use tlb::{asid_alloc, asid_free, set_asid, tlb_global_invalidate, tlb_invalidate};
```

`tlb.rs` 还提供 page 级辅助：

| 函数 | 作用 |
|------|------|
| `asid_alloc()` / `asid_free()` | 分配和释放 ASID |
| `set_asid()` | 设置当前地址空间 ASID |
| `tlb_invalidate()` | 刷新当前 TLB |
| `tlb_invalidate_page(vpn)` | 刷新指定虚拟页 |
| `tlb_invalidate_global_page(vpn)` | 刷新 global page |
| `tlb_global_invalidate()` | 全局刷新 |

`TaskControlBlock` 在 la64 架构下持有 ASID 字段。返回用户态时，`trap_return()` 把 ASID 传给恢复汇编。

`__restore` 分别比较当前 PGDL 和 ASID；任一变化时连续写入新 PGDL、ASID，再清除非 global TLB 项。`CSR.ASID.ASIDBITS[23:16]` 是只读能力字段，不能提取到低位后与新 ASID 做 OR。旧实现会把 ASIDBITS 污染到 ASID 值中，并在 PGDL 未变化时漏掉 ASID 更新。

ASID 分配器只使用 1..255。耗尽时的内部哨兵 `u16::MAX` 不能直接写入 10-bit CSR 字段；激活和返回路径会把它转换为 ASID 0，并在地址空间激活时先清除全部非 global TLB 项，作为正确性优先的退化路径。

动态内核 PGDH 映射当前不是可靠的 global paired-page TLB 项。栈或临时程序 PTE 修改后，页级 `invtlb` 无法覆盖所有 ASID 中可能存在的同 VA 非 global 项，因此当前采用全 TLB invalidate 保证回收后不会命中旧映射。后续若把成对内核 PTE 的 `G` 语义完整实现，可再收窄为按 VA 的 global invalidate。

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
current_trap_cx()
current_user_token()
copy_from_user(token, pc, &mut instruction)
Instruction::from(instruction).get_op_code()
addr = BadV::read().get_vaddr()
根据 load/store 和宽度逐字节 copy_from_user/copy_to_user
符号扩展 load 结果
写回通用寄存器或浮点寄存器
cx.gp.pc += 4
```

模拟路径只接受大小为 2、4、8 的访问。若无法解码操作码或 PC 没有推进，会 panic 输出诊断。

## 9. 返回用户态

la64 `trap_return()` 的关键语义：

| 步骤 | 行为 |
|------|------|
| 信号 | `do_signal()` |
| user entry | `set_user_trap_entry()` 设置 exception entry 为 `strampoline` |
| privilege | `PrMd` 设置 `pplv=3`、`pie=true` |
| 参数 | trap context、用户 token、ASID 传给 `__restore` |

该路径保证信号交付发生在恢复用户态前。

LoongArch64 后端比 rv64 多两个需要重点理解的机制：ASID 和硬件 dirty/page-modify 语义。任务创建或 clone 后会分配/继承 ASID，返回用户态时 token 和 ASID 一起传给恢复汇编；页被写入时可能先触发 page modify，trap 后端通过 `LAFlexPageTable::set_dirty_bit()` 补 dirty bit，再让用户指令重试。这些机制使 la64 的“同一个虚拟地址”是否命中旧 TLB，不仅取决于页表内容，也取决于 ASID 和 invalidate 是否正确。

非对齐访存模拟是 la64 的另一个架构特有路径。`AddressNotAligned` 分支读取用户 PC 处指令，解析访问宽度和方向，通过 uaccess 读写目标地址，然后手动推进 PC。调试该路径时必须同时确认三点：指令解码成功、用户内存访问返回正确 errno、模拟成功后 PC 推进 4 字节。

## 10. 调试入口

| 症状 | 文件 | 检查点 |
|------|------|--------|
| la64 启动早期卡住 | `mod.rs::bootstrap_init()` | core id、DMW/page walk、TLB refill entry |
| 缺页后仍反复写 fault | `trap/mod.rs`, `laflex.rs` | `set_dirty_bit()` 和 TLB 刷新 |
| ASID 异常 | `tlb.rs`, task 创建路径 | `asid_alloc()`、返回用户态传参 |
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
