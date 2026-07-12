---
title: "HAL 与平台后端 (HAL and Platform Backends)"
category: architecture
status: stable
author: MangoCore Team
last_update: 2026-07-12
tags: [architecture, hal, riscv64, loongarch64]
---

# HAL 与平台后端

## 1. 概述

`os/src/hal/` 是 MangoCore 的硬件抽象层。架构无关代码不直接操作 `sstatus/scause/stvec` 或 LoongArch CSR，而是通过 HAL 获得以下能力：

| 能力 | 上层使用者 |
|------|------------|
| 页表实现类型 | `mm::AddressSpace<PageTableImpl>`、`KernelSpace<PageTableImpl>` |
| trap context 类型 | task 创建、exec、signal frame、trap return |
| 上下文切换 | `task::processor` 调度循环 |
| TLB 刷新 | 页表 unmap、权限修改、CoW、缺页修复 |
| timer | task timer、nanosleep、futex timeout、调度 tick |
| console/shutdown | 日志、panic、系统关机 syscall |

HAL 的核心文件是 `hal/mod.rs` 和 `hal/arch/mod.rs`。前者向内核其他模块统一导出接口，后者按编译 feature 选择 `riscv` 或 `loongarch64` 后端。

## 2. 目录结构

```
os/src/hal/
├── mod.rs
├── arch/
│   ├── mod.rs
│   ├── riscv/
│   │   ├── mod.rs
│   │   ├── config.rs
│   │   ├── kern_stack.rs
│   │   ├── sbi.rs
│   │   ├── sv39.rs
│   │   ├── switch.{rs,S}
│   │   ├── time.rs
│   │   └── trap/
│   └── loongarch64/
│       ├── mod.rs
│       ├── config.rs
│       ├── kern_stack.rs
│       ├── laflex.rs
│       ├── register/
│       ├── switch.{rs,S}
│       ├── time.rs
│       ├── tlb.rs
│       └── trap/
├── configs/
└── platform/
```

`configs/` 存放平台配置 toml，`platform/` 存放板级常量；实际内核代码通过架构后端 re-export 使用这些常量。

## 3. 公共导出

`hal/mod.rs` 当前导出项如下：

| 类别 | 导出项 | 说明 |
|------|--------|------|
| 上下文切换 | `__switch` | 任务切换汇编入口 |
| 配置 | `config` | 架构配置模块 |
| 栈 | `kstack_alloc`, `KernelStack`, `trap_cx_bottom_from_tid`, `ustack_bottom_from_tid` | 内核栈和用户栈/trap context 地址计算 |
| 启动 | `bootstrap_init`, `machine_init` | 早期机器初始化和运行期机器初始化 |
| 用户 ABI | `user_hwcap` | 生成当前架构可安全暴露给 ELF `AT_HWCAP` 的能力位 |
| console | `console_flush`, `console_getchar`, `console_putchar` | 字符输出输入 |
| 中断 | `local_irq_save`, `local_irq_restore` | 保存/恢复本地中断状态 |
| trap 查询 | `get_bad_addr`, `get_bad_instruction`, `get_exception_cause` | fault address、fault instruction、异常原因 |
| 时间 | `get_clock_freq`, `get_time`, `program_timer_delta` | 读取时钟、获取时间、设置 timer delta |
| trap 出入口 | `trap_handler`, `trap_return` | 架构 trap 入口和返回用户态入口 |
| 页表类型 | `PageTableImpl`, `KernelPageTableImpl` | rv64 映射到 `Sv39PageTable`，la64 映射到 `LAFlexPageTable` |
| trap 类型 | `TrapContext`, `MachineContext`, `UserContext`, `UserSignalMask`, `TrapImpl` | task/signal/syscall 共享的上下文类型 |
| TLB | `tlb_invalidate` | 架构后端提供的 TLB 刷新入口；la64 另在后端内部导出 global/page 级辅助 |
| 平台常量 | `BLOCK_SZ`, `BUFFER_CACHE_NUM`, `KERNEL_HEAP_SIZE`, `MEMORY_END`, `MMIO`, `TICKS_PER_SEC` | 块大小、缓存数、堆大小、物理内存末尾、MMIO 表和 tick 频率 |
| 关机 | `shutdown` | 平台退出/关机 |

`hal/mod.rs` 还定义两个与 I/O 路径直接相关的常量：

```rust
pub mod arch;
pub use arch::__switch;
pub use arch::config;
pub use arch::kstack_alloc;
pub use arch::shutdown;
pub use arch::tlb_invalidate;
pub use arch::{bootstrap_init, machine_init, user_hwcap};
pub use arch::{console_flush, console_getchar, console_putchar};
pub use arch::{local_irq_restore, local_irq_save};
pub use arch::{get_bad_addr, get_bad_instruction, get_exception_cause};
pub use arch::{get_clock_freq, get_time};
pub use arch::program_timer_delta;
pub use arch::{trap_cx_bottom_from_tid, ustack_bottom_from_tid};
pub use arch::{trap_handler, trap_return};
pub use arch::{
    KernelPageTableImpl, KernelStack, MachineContext, PageTableImpl, TrapContext, TrapImpl,
    UserContext, UserSignalMask,
};
pub use arch::{BLOCK_SZ, BUFFER_CACHE_NUM, KERNEL_HEAP_SIZE, MEMORY_END};
pub use arch::{MMIO, TICKS_PER_SEC};

pub const IO_CHUNK_SIZE: usize = {
    let heap = KERNEL_HEAP_SIZE;
    let raw = heap / 128;
    if raw < 64 * 1024 {
        64 * 1024
    } else if raw > 256 * 1024 {
        256 * 1024
    } else {
        raw
    }
};

pub const MAX_RW_COUNT: usize =
    (i32::MAX as usize) & !(crate::config::PAGE_SIZE as usize - 1);
```

这组导出构成 HAL 对上层的稳定命名面。MM 层通过 `PageTableImpl` 和 `tlb_invalidate` 操作页表；task 层通过 `KernelStack`、`TrapContext`、`__switch`、`trap_return` 完成调度与返回用户态；syscall/trap 层通过 `get_bad_addr()`、`get_exception_cause()`、`program_timer_delta()` 接入异常和时钟。`IO_CHUNK_SIZE` 用于限制 I/O bounce buffer 的单块大小；`MAX_RW_COUNT` 对齐 Linux 可见的单次读写上限。

## 4. 架构选择

`hal/arch/mod.rs` 使用 feature 选择后端：

| feature | 后端模块 | 页表类型 | trap 类型 |
|---------|----------|----------|-----------|
| `riscv` | `hal::arch::riscv` | `Sv39PageTable` | `riscv::register::scause::Trap` |
| `loongarch64` | `hal::arch::loongarch64` | `LAFlexPageTable` | `loongarch64::register::Trap` |

两套后端都导出同名的 `bootstrap_init()`、`machine_init()`、`trap_handler()`、`trap_return()`、`PageTableImpl`、`TrapContext`、`KernelStack` 等接口。架构无关层只依赖这些统一名字。

## 5. RISC-V 后端

### 5.1 模块地图

| 模块 | 作用 |
|------|------|
| `config.rs` | 地址布局、页大小、内核堆、内核栈、平台常量 |
| `kern_stack.rs` | 内核栈分配和 trap context 地址计算 |
| `sbi.rs` | OpenSBI 调用、console、timer、shutdown、本地中断保存恢复 |
| `sv39.rs` | SV39 页表实现和 `sfence.vma` TLB 刷新 |
| `switch.rs`/`switch.S` | 任务上下文切换 |
| `time.rs` | `get_time()`、`get_clock_freq()`、`program_timer_delta()` |
| `trap/` | trap context、汇编入口、syscall/缺页/timer 分发 |

### 5.2 初始化

`hal/arch/riscv/mod.rs` 中：

```rust
pub fn machine_init() {
    trap::init();
    trap::enable_timer_interrupt();
}

pub fn bootstrap_init() {}
```

rv64 的 `machine_init()` 只安装 trap 并打开 supervisor timer interrupt。第一次 timer deadline 由 `task::timer_subsystem_init()` 之后的 timer 编程路径设置。

### 5.3 Trap 路径

RISC-V trap 后端负责：

| trap | 处理 |
|------|------|
| `UserEnvCall` | 保存 `origin_a0`，读取 `a7` 和 `a0..a5`，PC 加 4，调用 `syscall::syscall()` |
| instruction/load/store fault 或 page fault | 调用当前进程 `AddressSpace::do_page_fault(addr, access)` |
| `IllegalInstruction` / `InstructionMisaligned` | 向当前任务注入 `SIGILL` |
| `SupervisorTimer` | 记录调度统计并调用 `task::timer_interrupt_handler()` |
| 其他 trap | panic，输出 scause/stval 信息 |

`trap_return()` 调用 `do_signal()` 后设置用户 trap entry 为 trampoline，跳转到 `__restore`，传入 trap context 虚拟地址和用户页表 token，并执行 `fence.i`。

## 6. LoongArch64 后端

### 6.1 模块地图

| 模块 | 作用 |
|------|------|
| `config.rs` | 地址布局、页表宽度、DMW 常量、平台常量 |
| `kern_stack.rs` | 内核栈和 trap context 地址 |
| `laflex.rs` | LoongArch64 页表实现 |
| `register/` | CSR/架构寄存器封装 |
| `tlb.rs` | ASID、TLB invalidate、TLB read/search |
| `time.rs` | timer frequency、时间读取、timer delta |
| `trap/` | trap context、异常分发、非对齐访存模拟、返回用户态 |
| `acpi.rs`, `boot.rs`, `sbi.rs` | la64 平台相关辅助 |

### 6.2 `bootstrap_init()`

la64 的早期初始化较重：

| 配置项 | 代码行为 |
|--------|----------|
| CPU 核 | 非 0 号核进入死循环 |
| interrupt vector | `ECfg` 设置 timer line-based interrupt |
| FPU/SIMD | 按 CPUCFG2 打开 scalar FPU；LSX/LASX 在扩展上下文保存完成前保持关闭 |
| timer | `TIClr` 清 timer，`TCfg` 关闭早期 timer |
| paging | `CrMd` 打开 paging，关闭中断 |
| trap entry | `set_kernel_trap_entry()`、`set_machine_err_trap_ent()` |
| TLB refill | `TLBREntry` 指向 `srfill` |
| DMW | `DMW2` 设置为 PLV0 可用、SUC uncached |
| page walk | 配置 `STLBPS`、`TLBREHi`、`PWCL`、`PWCH` |

这些配置发生在 `main.rs::mem_clear()` 之前，因此文档把它归入“架构早期初始化”。

#### 6.2.1 ELF `AT_HWCAP`

`AddressSpace` 构造用户栈时通过 HAL 的 `user_hwcap()` 填写 `AT_HWCAP`，不能在架构无关代码中写死同一个数字。RISC-V 返回 Linux ISA 字母位图 `0x112d`（IMAFDC）；LoongArch 按 CPUCFG1/2 映射 CPUCFG、LAM、UAL、FPU、CRC32、COMPLEX、CRYPTO、LVZ、PTW 和 LSPW。

HWCAP 表示“用户态可安全使用”的能力，不只是裸硬件能力。当前 LoongArch trap context 只保存标量 FPU 状态，因此即使 CPUCFG 报告 LSX/LASX/LBT，内核也不会在 HWCAP 中发布这些位，并保持 EUEN 的对应单元关闭。否则 glibc 动态链接器可能选择向量化 resolver，而任务切换又不能保存完整扩展状态。

### 6.3 `machine_init()`

la64 `machine_init()` 做运行期配置：

```
trap::init()
get_timer_freq_first_time()
打印 CPUCFG 0..6 与 0x10..0x14
打印 Misc/RVACfg/MMAP_BASE
trap::enable_timer_interrupt()
```

`trap::enable_timer_interrupt()` 设置 timer 中断向量。实际 timer deadline 仍由 timer 子系统编程。

### 6.4 Trap 路径

la64 trap 后端覆盖：

| 场景 | 处理 |
|------|------|
| `Exception::Syscall` | `ERA::next_ins()` 和 trap context PC 加 4，读取 `a7/a0..a5`，调用 syscall |
| page invalid / page privilege / page modify | 转成 `FaultAccess`，调用 `do_page_fault()` |
| store/page modify 成功 | 通过 `LAFlexPageTable::set_dirty_bit()` 补 dirty bit |
| illegal/FPU/privilege 类异常 | 注入 `SIGILL` |
| address error | 注入 `SIGSEGV` |
| timer interrupt | 清 timer，记录调度统计，调用 `task::timer_interrupt_handler()` |
| address not aligned | 解码指令，通过 `copy_from_user`/`copy_to_user` 模拟用户态 load/store |

`trap_return()` 调用 `do_signal()`，设置 exception entry 为 `strampoline`，配置 `pplv=3` 与 `pie=true`，把 trap context、用户页表 token 和 ASID 交给恢复汇编。

## 7. HAL 与上层的接口契约

| 契约 | 依据 | 影响 |
|------|------|------|
| syscall ABI 固定为 `a7` + `a0..a5` | 两套 trap 后端均读取这组寄存器 | `syscall::syscall(id, args)` 得到统一 `[usize; 6]` |
| `rt_sigreturn` 不覆盖 `a0` | 两套 trap 后端均检查 syscall id 139 | 信号返回恢复完整用户上下文 |
| PTE 修改后刷新 TLB | 页表实现调用架构 invalidate | CoW、mprotect、munmap、缺页修复不能留下陈旧 TLB |
| 返回用户态前处理信号 | 两套 `trap_return()` 均调用 `do_signal()` | signal frame 构造发生在恢复用户上下文之前 |
| trap 后端只负责入口分派 | syscall/MM/task 语义在领域模块实现 | 架构差异集中在 trap context、寄存器、TLB、timer |

HAL 的核心价值是把“同一件内核语义”压缩成一组稳定契约。上层 task 代码只关心 `TrapContext` 能保存和恢复用户寄存器，不关心 rv64 的 `sstatus/sepc` 或 la64 的 `PrMd/ERA`；MM 只关心 `PageTable` trait 能 map/unmap/set flags/flush，不关心 `sfence.vma` 或 `invtlb` 的具体编码；syscall 分发只接收 `[usize; 6]`，不关心架构汇编如何保存寄存器。

读 HAL 相关 bug 时，应先确认失败发生在契约哪一侧：如果 `sys_read` 参数已经错，问题在 trap ABI；如果参数正确但文件语义错，问题在 syscall/fs；如果 `mprotect` 后仍可写，问题可能在页表/TLB；如果 signal handler 没进，先看 `trap_return()` 是否执行 `do_signal()`，再看 signal 模块是否有 pending。

## 8. 调试入口

| 问题 | 首选文件 | 断点/检查点 |
|------|----------|-------------|
| syscall 参数错误 | `hal/arch/*/trap/mod.rs` | `trap_handler()` 中读取 `a7/a0..a5` 的块 |
| 缺页信号不对 | `hal/arch/*/trap/mod.rs` | `MemoryError` 到信号的映射分支 |
| timer 不进调度 | `hal/arch/*/time.rs`, `trap/mod.rs` | `program_timer_delta()`、timer interrupt 分支 |
| la64 dirty bit 问题 | `hal/arch/loongarch64/trap/mod.rs`, `laflex.rs` | page modify 成功后的 `set_dirty_bit()` |
| TLB 陈旧 | `hal/arch/riscv/sv39.rs`, `hal/arch/loongarch64/tlb.rs` | invalidate 调用点 |

## 9. 测试映射

| 测试目标 | 覆盖接口 | 推荐验证 |
|----------|----------|----------|
| rv64 HAL 可用 | entry、trap、timer、SV39 | `cd os && make rv64-kernel-build-only`；`cd os && make rv64-run` |
| la64 HAL 可用 | bootstrap、LAFlex、TLB、timer | `cd os && make la64-kernel-build-only`；`cd os && make la64-run` |
| syscall ABI | trap + syscall 分发 | basic、busybox、LTP syscall 用例 |
| 缺页处理 | trap + MM | mmap、fork、exec、page fault 用例 |
| timer interrupt | HAL time + task timer | nanosleep、futex timeout、timer 系统调用 |

## 10. 源文件索引

| 路径 | 内容 |
|------|------|
| `os/src/hal/mod.rs` | 公共导出、`IO_CHUNK_SIZE`、`MAX_RW_COUNT` |
| `os/src/hal/arch/mod.rs` | feature 选择和后端 re-export |
| `os/src/hal/arch/riscv/mod.rs` | rv64 后端模块、类型别名、初始化 |
| `os/src/hal/arch/riscv/sv39.rs` | SV39 页表和 `sfence.vma` |
| `os/src/hal/arch/riscv/trap/mod.rs` | rv64 trap/syscall/page fault/timer |
| `os/src/hal/arch/loongarch64/mod.rs` | la64 bootstrap、machine init、类型别名 |
| `os/src/hal/arch/loongarch64/laflex.rs` | la64 页表实现 |
| `os/src/hal/arch/loongarch64/tlb.rs` | la64 ASID/TLB |
| `os/src/hal/arch/loongarch64/trap/mod.rs` | la64 trap/syscall/page fault/timer/unaligned access |
| `os/src/hal/platform/` | 板级常量 |
