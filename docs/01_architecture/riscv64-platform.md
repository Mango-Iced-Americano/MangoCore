---
title: "RISC-V 64 平台后端"
category: architecture
status: stable
author: MangoCore Team
last_update: 2026-07-31
tags: [architecture, riscv64, hal]
---

# RISC-V 64 平台后端

## 1. 概述

RISC-V 后端位于 `os/src/hal/arch/riscv/`。该后端面向 `riscv64gc-unknown-none-elf`，通过 OpenSBI 完成 console、timer、shutdown 等底层服务，并向架构无关层提供 `Sv39PageTable`、trap context、内核栈、上下文切换和时间接口。

统一类型别名位于 `hal/arch/riscv/mod.rs`：

```rust
pub type KernelPageTableImpl = sv39::Sv39PageTable;
pub type PageTableImpl = sv39::Sv39PageTable;
pub type TrapImpl = riscv::register::scause::Trap;
pub type InterruptImpl = riscv::register::scause::Interrupt;
pub type ExceptionImpl = riscv::register::scause::Exception;
```

上层 MM 通过 `PageTableImpl` 使用 SV39，不直接引用具体架构寄存器。

## 2. 模块地图

```
os/src/hal/arch/riscv/
├── mod.rs
├── config.rs
├── entry.asm
├── kern_stack.rs
├── linker.ld / linker-rvqemu.ld
├── sbi.rs
├── sv39.rs
├── switch.rs
├── switch.S
├── time.rs
└── trap/
    ├── mod.rs
    ├── context.rs
    └── trap.S
```

| 文件 | 职责 |
|------|------|
| `entry.asm` | 架构入口汇编，由 `main.rs` 在 `riscv` feature 下引入 |
| `config.rs` | 地址空间、页大小、内核堆、内核栈、物理内存和平台常量 |
| `kern_stack.rs` | 内核栈分配、trap context/user stack 地址计算 |
| `sbi.rs` | OpenSBI 调用、console、timer、shutdown、本地中断保存恢复 |
| `sv39.rs` | SV39 页表、PTE flag、TLB 刷新 |
| `switch.S` | 任务上下文切换汇编 |
| `time.rs` | 时间读取、时钟频率、timer delta |
| `trap/mod.rs` | trap handler、syscall、缺页、timer interrupt、返回用户态 |
| `trap/context.rs` | `TrapContext`、`UserContext`、用户信号 mask 上下文 |

## 3. 初始化路径

RISC-V 后端在 `mod.rs` 中定义：

```rust
pub fn machine_init() {
    trap::init();
    trap::enable_ipi_interrupt();
    // 多核时一次性探测 SBI RFENCE；缺失则保留软件 IPI fallback。
    // 每个 CPU 的首个 timer deadline 由 timer_cpu_init() 设置。
}

pub fn bootstrap_init(cpu_id: usize) { /* AP 只初始化 IPI */ }
pub fn enable_local_timer_interrupt() { /* deadline 写入后开放 STIE */ }
```

### 3.1 `bootstrap_init()`

rv64 的 `bootstrap_init()` 在 CPU0 上不重复做全局工作；AP 只安装本地 trap 并开放
supervisor software interrupt。页表根、完整 trap/timer 和 RFENCE 能力由 CPU0 的全局
初始化及后续 AP 启动阶段共同建立。

### 3.2 `machine_init()`

`machine_init()` 完成以下工作：

| 调用 | 文件 | 行为 |
|------|------|------|
| `trap::init()` | `trap/mod.rs` | 调用 `set_kernel_trap_entry()`，设置 `stvec` 为 kernel trap |
| `trap::enable_timer_interrupt()` | `trap/mod.rs` | `sie::set_stimer()` 打开 supervisor timer interrupt |
| `trap::enable_ipi_interrupt()` | `trap/mod.rs` | 为 CPU0 打开 supervisor software interrupt |
| `sbi::init_rfence()` | `sbi.rs` | 多核时通过 BASE extension 探测 RFENCE 并缓存结果；启动日志明确选择 RFENCE 或 IPI fallback |

第一次 timer deadline 没有在 `machine_init()` 中设置。每个 CPU 都先由
`timer_cpu_init()` 写入首个绝对 deadline，再开放本地 timer interrupt，避免在 deadline
尚未发布时收到无法归属的中断。

## 4. Trap 后端

### 4.1 trap entry

`trap/mod.rs` 引入 `trap.S`：

```rust
global_asm!(include_str!("trap.S"));

extern "C" {
    pub fn __alltraps();
    pub fn __restore();
    pub fn __call_sigreturn();
}
```

`__alltraps` 保存用户上下文并进入 Rust `trap_handler()`，`__restore` 从 trap context 恢复用户态。

### 4.2 trap entry 切换

| 函数 | 行为 |
|------|------|
| `set_kernel_trap_entry()` | `stvec::write(trap_from_kernel, Direct)` |
| `set_user_trap_entry()` | `stvec::write(TRAMPOLINE, Direct)` |

进入 trap handler 后先切回 kernel trap entry；返回用户态前再设置 user trap entry 为 trampoline。

## 5. syscall 分支

RISC-V syscall 分支匹配：

```rust
if let Trap::Exception(Exception::UserEnvCall) = scause.cause() {
    let _trap_start = crate::task::perf::perf_time_now();
    let task = current_task().unwrap();
    let (syscall_id, args) = {
        let mut inner = task.acquire_inner_lock();
        inner.update_process_times_enter_trap();
        let cx = inner.trap_context_mut();
        cx.gp.pc += 4;
        cx.origin_a0 = cx.gp.a0; // 保存重启参数
        let syscall_id = cx.gp.a7;
        (
            syscall_id,
            [cx.gp.a0, cx.gp.a1, cx.gp.a2, cx.gp.a3, cx.gp.a4, cx.gp.a5],
        )
    };
    let result = syscall(syscall_id, args);
    {
        let mut inner = task.acquire_inner_lock();
        let cx = inner.trap_context_mut();
        if syscall_id != 139 {
            cx.gp.a0 = result as usize;
        }
        inner.refresh_real_timer();
        inner.update_process_times_leave_trap(scause.cause());
    }
    let _trap_ticks = crate::task::perf::perf_time_now() - _trap_start;
    crate::task::perf::record_trap_cost_ticks(_trap_ticks);
    trap_return();
}
```

执行步骤：

| 步骤 | 代码行为 |
|------|----------|
| 获取当前任务 | `current_task().unwrap()` |
| 进入统计 | `inner.update_process_times_enter_trap()` |
| 推进 PC | `cx.gp.pc += 4` |
| 保存重启参数 | `cx.origin_a0 = cx.gp.a0` |
| 读取 syscall | `cx.gp.a7` |
| 收集参数 | `[a0, a1, a2, a3, a4, a5]` |
| 分发 | `syscall(syscall_id, args)` |
| 重新取 trap context | 防止 execve/sigreturn 替换上下文 |
| 写回返回值 | syscall id 非 139 时写回 `a0` |
| 退出统计 | `refresh_real_timer()`、`update_process_times_leave_trap()` |

这一分支直接结束于 `trap_return()`，不会继续进入普通异常 `match`。

## 6. 缺页与异常

### 6.1 缺页映射

| RISC-V 异常 | MM 访问类型 |
|-------------|-------------|
| `StoreFault`, `StorePageFault` | `FaultAccess::Store` |
| `InstructionFault`, `InstructionPageFault` | `FaultAccess::Execute` |
| `LoadFault`, `LoadPageFault` | `FaultAccess::Load` |

处理流程：

```
frame_reserve(3)
task.process.vm().write(|vm| vm.do_page_fault(addr, access))
```

缺页结果映射：

| `MemoryError` | 信号/状态 |
|---------------|-----------|
| `BeyondEOF`, `BackingStoreFailure` | `SIGBUS` |
| `NoPermission` | `SIGSEGV` + `SEGV_ACCERR` |
| `BadAddress`, `NotMapped` | `SIGSEGV` + `SEGV_MAPERR` |
| `OutOfMemory` | `pending_oom_kill = true` |
| 其他 | warn + `SIGSEGV` + `SEGV_MAPERR` |

### 6.2 非法指令

`IllegalInstruction` 和 `InstructionMisaligned` 注入 `SIGILL`，并使用 `SigInfo::ILL_ILLOPC`。其他未支持 trap 进入 panic，输出 cause 和 stval。

## 7. Timer interrupt

`Trap::Interrupt(Interrupt::SupervisorTimer)` 分支执行：

```
record_timer_interrupt()
record_sched_timer_interrupt()
TIMER_INTERRUPT += 1
record_sched_timer_trap_cycles()
task::timer_interrupt_handler()
```

该分支不直接调用 `set_next_trigger` 形式的接口；下一次 timer 触发由 task/timer 路径通过 HAL time 接口编程。

## 8. 返回用户态

RISC-V `trap_return()`：

```rust
let task = do_signal();
set_user_trap_entry();
let trap_cx_ptr = task.trap_cx_user_va();
let user_satp = current_user_token();
let restore_va = __restore as usize - __alltraps as usize + TRAMPOLINE;
asm!(
    "fence.i",
    "jr {restore_va}",
    restore_va = in(reg) restore_va,
    in("a0") trap_cx_ptr,
    in("a1") user_satp,
    options(noreturn)
);
```

关键点：

| 动作 | 含义 |
|------|------|
| `do_signal()` | 返回用户态前交付信号和构造 signal frame |
| `TRAMPOLINE` | 用户/内核页表都可见的恢复代码映射 |
| `current_user_token()` | 用户页表 token，即恢复用户态所需 satp |
| `fence.i` | 保证指令流一致性 |
| `options(noreturn)` | 恢复汇编不返回 Rust 调用点 |

## 9. SV39 与 TLB

`sv39.rs` 提供 SV39 页表实现。TLB 刷新入口包括：

| 函数/宏 | 作用 |
|---------|------|
| `tlb_invalidate()` | 执行全局 `sfence.vma` |
| `tlb_invalidate_addr(vaddr)` | 对单个虚拟地址执行 `sfence.vma vaddr` |
| `tlb_invalidate_addr_asid(vaddr, asid)` | 只失效指定 MM 的用户虚拟页 |
| `user_tlb_invalidate_range(asid, range)` | 对最多 64 页的连续区间逐页执行 ASID 定向失效 |
| `try_assign_asid()` / `rollover_asids()` | 分配 MM-owned ASID，并在全 CPU flush 后换代 |
| `sfence_vma!` | 宏形式刷新指定虚拟页 |

所有修改 PTE 的路径必须通过页表实现触发相应刷新。用户态 fork/CoW、mprotect、munmap、缺页修复都依赖这一契约。

## 10. OpenSBI 接口

`sbi.rs` 封装底层服务：

| 能力 | 作用 |
|------|------|
| console put/get | 支撑 `console_putchar`、`console_getchar` |
| timer | 设置 timer |
| shutdown | QEMU/OpenSBI 退出 |
| local irq save/restore | 本地中断状态保存恢复 |
| RFENCE FID 2 | 按 hart mask、字节 start/size 与 ASID 同步远端区间失效；跨度超过 64 页由上层改走全刷 |

上层代码通过 `hal/mod.rs` 的 re-export 使用这些能力，不直接引用 `sbi.rs`。

RISC-V 后端的阅读主线是“OpenSBI 提供底层服务，S-mode 内核建立 trap 和页表”。启动后 `entry.asm` 进入 Rust，`machine_init()` 安装 trap 并打开 timer；syscall 和 page fault 都从 `trap/mod.rs` 分派；页表修改落到 `sv39.rs`，TLB 刷新最终是 `sfence.vma`。因此 rv64 上遇到用户态异常时，先看 `scause/stval/sepc` 对应分支，再看架构无关层返回的 errno 或 signal。

RV64 现在会探测 `SATP.ASID` 的实际位数，并让一个 MM 在所有 hart 使用同一 versioned
ASID；ASIDLEN=0 的平台自动退化到 switch-time 全刷。有界区间由同一 `MmuGather` 主链
冻结，RFENCE 不可用时改走每发起 CPU 固定 slot，不建立第二套 MM 提交结构。它仍没有 LA64 的用户非对齐访存
模拟路径；未对齐兼容必须在 syscall/uaccess 或测试适配层显式处理，不能指望 trap 后端
解码并模拟 load/store。

## 11. 调试入口

| 症状 | 文件 | 检查点 |
|------|------|--------|
| 启动后无 trap | `trap::init()` | `stvec` 是否设置为 `trap_from_kernel` |
| syscall 编号错误 | `trap_handler()` syscall 分支 | `a7` 和 `a0..a5` 保存位置 |
| 用户缺页反复触发 | `sv39.rs`, `page_fault.rs` | PTE 权限和 `sfence.vma` |
| timer 不抢占 | `trap::enable_timer_interrupt()`、`time.rs` | `sie::set_stimer()` 和 timer delta |
| 返回用户态失败 | `trap_return()`、`trap.S` | trampoline、satp、trap context VA |

## 12. 测试映射

| 测试目标 | 覆盖代码 | 命令/用例 |
|----------|----------|-----------|
| rv64 编译 | 全后端 | `cd os && make rv64-kernel-build-only` |
| rv64 启动 | entry、trap、MM、task | `cd os && make rv64-run` |
| syscall ABI | trap + syscall | basic、busybox、LTP syscall |
| 页表/TLB | SV39 + MM | mmap、fork、exec、mprotect、munmap |
| timer | time + trap + task | nanosleep、futex timeout、timer syscall |

## 13. 源文件索引

| 路径 | 内容 |
|------|------|
| `os/src/hal/arch/riscv/mod.rs` | 模块声明、类型别名、初始化函数 |
| `os/src/hal/arch/riscv/entry.asm` | 架构入口 |
| `os/src/hal/arch/riscv/config.rs` | 地址和平台常量 |
| `os/src/hal/arch/riscv/kern_stack.rs` | 内核栈和上下文地址 |
| `os/src/hal/arch/riscv/sbi.rs` | OpenSBI 服务 |
| `os/src/hal/arch/riscv/sv39.rs` | SV39 页表和 TLB flush |
| `os/src/hal/arch/riscv/switch.S` | 上下文切换 |
| `os/src/hal/arch/riscv/time.rs` | 时间与 timer |
| `os/src/hal/arch/riscv/trap/mod.rs` | trap handler 和 trap return |
| `os/src/hal/arch/riscv/trap/context.rs` | trap context 类型 |
