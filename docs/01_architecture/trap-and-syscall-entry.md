---
title: "陷阱与 syscall 入口 (Trap and Syscall Entry)"
category: architecture
status: stable
author: MangoCore Team
last_update: 2026-06-29
tags: [architecture, trap, syscall, interrupt]
---

# 陷阱与 syscall 入口

## 1. 概述

用户态进入内核的主要入口由架构 trap 后端提供：

| 架构 | 文件 | 入口 |
|------|------|------|
| rv64 | `os/src/hal/arch/riscv/trap/mod.rs` | `trap_handler()`、`trap_return()` |
| la64 | `os/src/hal/arch/loongarch64/trap/mod.rs` | `trap_handler()`、`trap_return()` |

两套后端都把 syscall ABI 规约收敛成：

```rust
let syscall_id = cx.gp.a7;
let args = [cx.gp.a0, cx.gp.a1, cx.gp.a2, cx.gp.a3, cx.gp.a4, cx.gp.a5];
let result = syscall(syscall_id, args);
```

因此，`syscall::syscall()` 以后看到的是统一的 `id + [usize; 6]`，不再关心底层异常编码。

## 2. syscall 入口总览

```
用户态
  a7      = syscall id
  a0..a5  = args
      |
      v
架构 trap handler
  切换 kernel trap entry
  更新任务时间统计
  PC/ERA 前进 4 字节
  保存 origin_a0
      |
      v
syscall::syscall(id, args)
      |
      v
写回 a0
  syscall id 139(rt_sigreturn) 除外
      |
      v
trap_return()
  do_signal()
  跳转恢复汇编
```

`rt_sigreturn` 的 syscall id 为 139。两套后端都在 syscall 返回后重新获取 trap context，并且在 id 为 139 时不把普通返回值写入 `a0`，因为该路径已经恢复完整用户上下文。

## 3. RISC-V syscall 路径

RISC-V syscall 由 `Trap::Exception(Exception::UserEnvCall)` 分支处理：

| 步骤 | 代码行为 |
|------|----------|
| 进入 trap | `set_kernel_trap_entry()` 把 `stvec` 切回 kernel trap |
| 读取 cause | `scause::read()` 和 `stval::read()` |
| 记录时间 | `inner.update_process_times_enter_trap()` |
| 推进 PC | `cx.gp.pc += 4` |
| 保存重启参数 | `cx.origin_a0 = cx.gp.a0` |
| 收集 ABI | `a7` 为 id，`a0..a5` 为参数 |
| 调用分发 | `syscall(syscall_id, args)` |
| 写回返回值 | 非 139 时 `cx.gp.a0 = result as usize` |
| 退出统计 | `refresh_real_timer()`、`update_process_times_leave_trap()`、`record_trap_cost_ticks()` |
| 返回用户态 | `trap_return()` |

RISC-V 在 syscall 路径和普通 trap 路径都维护任务时间统计。普通 trap 处理完成后也会刷新 real timer。

### 3.1 RISC-V `trap_handler()` 核心代码

`os/src/hal/arch/riscv/trap/mod.rs::trap_handler()` 的 syscall、缺页、timer 与返回路径集中在同一入口函数中：

```rust
pub fn trap_handler() -> ! {
    let scause = scause::read();
    set_kernel_trap_entry();
    let stval = stval::read();

    if let Trap::Exception(Exception::UserEnvCall) = scause.cause() {
        let _trap_start = crate::task::perf::perf_time_now();
        let task = current_task().unwrap();
        let (syscall_id, args) = {
            let mut inner = task.acquire_inner_lock();
            inner.update_process_times_enter_trap();
            let cx = inner.get_trap_cx();
            cx.gp.pc += 4;
            cx.origin_a0 = cx.gp.a0; // 保存重启参数
            let syscall_id = cx.gp.a7;
            (
                syscall_id,
                [cx.gp.a0, cx.gp.a1, cx.gp.a2, cx.gp.a3, cx.gp.a4, cx.gp.a5],
            )
        };
        let result = syscall(syscall_id, args);
        // The trap context may be replaced by execve or restored by sigreturn,
        // so fetch it again after syscall returns.
        {
            let mut inner = task.acquire_inner_lock();
            let cx = inner.get_trap_cx();
            // sigreturn(139) already restored the full trap context (including a0).
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

    {
        let task = current_task().unwrap();
        let mut inner = task.acquire_inner_lock();
        inner.update_process_times_enter_trap();
    }
    match scause.cause() {
        Trap::Exception(Exception::StoreFault)
        | Trap::Exception(Exception::StorePageFault)
        | Trap::Exception(Exception::InstructionFault)
        | Trap::Exception(Exception::InstructionPageFault)
        | Trap::Exception(Exception::LoadFault)
        | Trap::Exception(Exception::LoadPageFault) => {
            let task = current_task().unwrap();
            let addr = VirtAddr::from(stval);
            frame_reserve(3);
            let access = match scause.cause() {
                Trap::Exception(Exception::StoreFault)
                | Trap::Exception(Exception::StorePageFault) => FaultAccess::Store,
                Trap::Exception(Exception::InstructionFault)
                | Trap::Exception(Exception::InstructionPageFault) => FaultAccess::Execute,
                _ => FaultAccess::Load,
            };
            let _pf_start = crate::task::perf::perf_time_now();
            crate::task::perf::record_page_fault();
            let pf_result = task.process.vm().write(|vm| vm.do_page_fault(addr, access));
            crate::task::perf::record_pagefault_time_us(
                crate::task::perf::perf_time_now().saturating_sub(_pf_start),
            );
            if let Err(error) = pf_result {
                let mut inner = task.acquire_inner_lock();
                match error {
                    MemoryError::BeyondEOF | MemoryError::BackingStoreFailure => {
                        inner.add_signal(Signals::SIGBUS);
                    }
                    MemoryError::NoPermission => {
                        inner.sigmask.remove(Signals::SIGSEGV);
                        inner.add_signal_with_code(Signals::SIGSEGV, SigInfo::SEGV_ACCERR);
                    }
                    MemoryError::BadAddress | MemoryError::NotMapped => {
                        inner.sigmask.remove(Signals::SIGSEGV);
                        inner.add_signal_with_code(Signals::SIGSEGV, SigInfo::SEGV_MAPERR);
                    }
                    MemoryError::OutOfMemory => {
                        inner.pending_oom_kill = true;
                    }
                    other => {
                        log::warn!(
                            "[page_fault] unexpected memory error {:?}, send SIGSEGV",
                            other
                        );
                        inner.sigmask.remove(Signals::SIGSEGV);
                        inner.add_signal_with_code(Signals::SIGSEGV, SigInfo::SEGV_MAPERR);
                    }
                }
            };
        }
        Trap::Exception(Exception::IllegalInstruction)
        | Trap::Exception(Exception::InstructionMisaligned) => {
            let task = current_task().unwrap();
            let mut inner = task.acquire_inner_lock();
            inner.sigmask.remove(Signals::SIGILL);
            inner.add_signal_with_code(Signals::SIGILL, SigInfo::ILL_ILLOPC);
        }
        Trap::Interrupt(Interrupt::SupervisorTimer) => {
            let trap_profile_start = crate::task::processor::sched_profile_cycle_start();
            crate::task::perf::record_timer_interrupt();
            crate::task::processor::record_sched_timer_interrupt();
            unsafe { TIMER_INTERRUPT += 1; }
            crate::task::processor::record_sched_timer_trap_cycles(trap_profile_start);
            crate::task::timer_interrupt_handler();
        }
        _ => {
            panic!(
                "Unsupported trap {:?}, stval = {:#x}!",
                scause.cause(),
                stval
            );
        }
    }
    {
        let task = current_task().unwrap();
        let mut inner = task.acquire_inner_lock();
        inner.refresh_real_timer();
        inner.update_process_times_leave_trap(scause.cause());
    }
    trap_return();
}
```

这个入口先把 `stvec` 切回内核 trap，再区分 syscall 和普通 trap。syscall 分支在释放 `task.inner` 后调用分发函数，返回后重新取 trap context；这对应 `execve` 可能替换地址空间和 trap context、`rt_sigreturn` 可能恢复完整用户上下文的事实。缺页分支在进入 MM 前调用 `frame_reserve(3)`，并把硬件异常类型映射成 `FaultAccess`；timer 分支不直接调度，而是交给 task 层的 `timer_interrupt_handler()`。

## 4. LoongArch64 syscall 路径

LoongArch64 syscall 由 `Trap::Exception(Exception::Syscall)` 分支处理：

| 步骤 | 代码行为 |
|------|----------|
| 用户态检查 | `PrMd::read().get_pplv() == 0` 时 panic |
| 进入 trap | `set_kernel_trap_entry()` |
| 读取 cause | `get_exception_cause()`、`get_bad_addr()`、`get_bad_instruction()` |
| 记录时间 | `inner.update_process_times_enter_trap()` |
| 推进 PC | `ERA::read().next_ins().write()` 和 `cx.gp.pc += 4` |
| 保存重启参数 | `cx.origin_a0 = cx.gp.a0` |
| 收集 ABI | `a7` 为 id，`a0..a5` 为参数 |
| 调用分发 | `syscall(syscall_id, args)` |
| 写回返回值 | 非 139 时写回 `cx.gp.a0` |
| 退出统计 | `update_process_times_leave_trap()`、`record_trap_cost_ticks()` |
| 返回用户态 | `trap_return()` |

la64 同时更新 CSR 中的 ERA 和 trap context 中的 `pc`。这与该后端恢复用户态的汇编路径有关。

### 4.1 LoongArch64 `trap_handler()` 入口结构

la64 的 syscall 入口与 rv64 保持同一 ABI，但额外维护 `ERA` 和 PLV 检查：

```rust
pub fn trap_handler() -> ! {
    if PrMd::read().get_pplv() == 0 {
        panic!();
    }
    set_kernel_trap_entry();

    let cause = get_exception_cause();
    let stval = get_bad_addr();
    let badi = get_bad_instruction();

    if let Trap::Exception(Exception::Syscall) = cause {
        let _trap_start = crate::task::perf::perf_time_now();
        let task = current_task().unwrap();
        let (syscall_id, args) = {
            let mut inner = task.acquire_inner_lock();
            inner.update_process_times_enter_trap();
            let cx = inner.get_trap_cx();
            ERA::read().next_ins().write();
            cx.gp.pc += 4;
            cx.origin_a0 = cx.gp.a0; // 保存重启参数
            let syscall_id = cx.gp.a7;
            (
                syscall_id,
                [cx.gp.a0, cx.gp.a1, cx.gp.a2, cx.gp.a3, cx.gp.a4, cx.gp.a5],
            )
        };
        let result = syscall(syscall_id, args);
        // The trap context may be replaced by execve or restored by sigreturn,
        // so fetch it again after syscall returns.
        {
            let mut inner = task.acquire_inner_lock();
            let cx = inner.get_trap_cx();
            // sigreturn(139) already restored the full trap context (including a0).
            if syscall_id != 139 {
                cx.gp.a0 = result as usize;
            }
            inner.update_process_times_leave_trap(cause);
        }
        let _trap_ticks = crate::task::perf::perf_time_now() - _trap_start;
        crate::task::perf::record_trap_cost_ticks(_trap_ticks);
        trap_return();
    }

    {
        let task = current_task().unwrap();
        let mut inner = task.acquire_inner_lock();
        inner.update_process_times_enter_trap();
    }

    match cause {
        Trap::Exception(Exception::PagePrivilegeIllegal)
        | Trap::Exception(Exception::PageInvalidFetch)
        | Trap::Exception(Exception::PageInvalidStore)
        | Trap::Exception(Exception::PageInvalidLoad)
        | Trap::Exception(Exception::PageModifyFault)
        | Trap::Exception(Exception::PageNonReadableFault)
        | Trap::Exception(Exception::PageNonExecutableFault) => {
            let task = current_task().unwrap();
            let mut inner = task.acquire_inner_lock();
            let addr = VirtAddr::from(get_bad_addr());
            frame_reserve(3);
            let vm_ref = task.process.vm();
            let mut mset_lock = vm_ref.lock();
            let access = match cause {
                Trap::Exception(Exception::PageInvalidStore)
                | Trap::Exception(Exception::PageModifyFault) => FaultAccess::Store,
                Trap::Exception(Exception::PageInvalidFetch)
                | Trap::Exception(Exception::PageNonExecutableFault) => FaultAccess::Execute,
                _ => FaultAccess::Load,
            };
            crate::task::perf::record_page_fault();
            match mset_lock.do_page_fault(addr, access) {
                Err(error) => match error {
                    MemoryError::BeyondEOF | MemoryError::BackingStoreFailure => {
                        inner.add_signal(Signals::SIGBUS);
                    }
                    MemoryError::NoPermission => {
                        inner.sigmask.remove(Signals::SIGSEGV);
                        inner.add_signal_with_code(Signals::SIGSEGV, SigInfo::SEGV_ACCERR);
                    }
                    MemoryError::BadAddress | MemoryError::NotMapped => {
                        inner.sigmask.remove(Signals::SIGSEGV);
                        inner.add_signal_with_code(Signals::SIGSEGV, SigInfo::SEGV_MAPERR);
                    }
                    MemoryError::OutOfMemory => {
                        inner.pending_oom_kill = true;
                    }
                    other => {
                        log::warn!(
                            "[page_fault] unexpected memory error {:?}, send SIGSEGV",
                            other
                        );
                        inner.sigmask.remove(Signals::SIGSEGV);
                        inner.add_signal_with_code(Signals::SIGSEGV, SigInfo::SEGV_MAPERR);
                    }
                },
                Ok(_) => {
                    drop(mset_lock);
                    if let Trap::Exception(
                        Exception::PageModifyFault | Exception::PageInvalidStore,
                    ) = cause
                    {
                        LAFlexPageTable::from_token(task.get_user_token())
                            .set_dirty_bit(addr.floor())
                            .unwrap();
                    }
                }
            };
        }
        Trap::Interrupt(Interrupt::Timer) => {
            let trap_profile_start = crate::task::processor::sched_profile_cycle_start();
            crate::task::perf::record_timer_interrupt();
            crate::task::processor::record_sched_timer_interrupt();
            TIClr::read().clear_timer().write();
            crate::task::processor::record_sched_timer_trap_cycles(trap_profile_start);
            crate::task::timer_interrupt_handler();
        }
        Trap::Exception(Exception::Breakpoint) => {
            read_bp();
        }
        Trap::Exception(Exception::AddressNotAligned) => {
            let cx = current_trap_cx();
            let token = current_user_token();
            let pc = cx.gp.pc;
            let mut i = 0;
            copy_from_user(token, pc as *const u32, addr_of_mut!(i)).unwrap();
            let ins = Instruction::from(i);
            let op = ins.get_op_code();
            if op.is_err() {
                panic!("Unsupported OpCode! Instruction: {:?} ", ins);
            }
            let op = op.unwrap();
            let addr = BadV::read().get_vaddr();
            //debug!("{:#x}: {:?}, {:#x}", pc, op, addr);
            let sz = op.get_size();
            let is_aligned: bool = addr % sz == 0;
            if !is_aligned {
                assert!([2, 4, 8].contains(&sz));
                if op.is_store() {
                    let mut rd = if !op.is_float_op() {
                        cx.gp[ins.get_rd_num()]
                    } else {
                        cx.fp.f[ins.get_rd_num()]
                    };
                    for i in 0..sz {
                        let seg = rd as u8;
                        copy_to_user(token, addr_of!(seg), (addr + i) as *mut u8).unwrap();
                        rd >>= 8;
                    }
                } else {
                    let mut rd = 0;
                    for i in (0..sz).rev() {
                        rd <<= 8;
                        let mut read_byte: u8 = 0;
                        copy_from_user(token, (i + addr) as *const u8, addr_of_mut!((read_byte)))
                            .unwrap();
                        rd |= read_byte as usize;
                    }
                    if !op.is_unsigned_ld() {
                        match sz {
                            2 => rd = (rd as u16) as i16 as isize as usize,
                            4 => rd = (rd as u32) as i32 as isize as usize,
                            8 => rd = rd,
                            _ => unreachable!(),
                        }
                    }
                    if !op.is_float_op() {
                        cx.gp[ins.get_rd_num()] = rd;
                    } else {
                        cx.fp.f[ins.get_rd_num()] = rd;
                    }
                }
                cx.gp.pc += 4;
            }
            if cx.gp.pc == pc {
                panic!(
                    "Failed to execute the command. Bad Instruction: {}, PC:{}",
                    unsafe { *(cx.gp.pc as *const u32) },
                    pc
                );
            }
        }
        Trap::Interrupt(Interrupt::IPI)
        | Trap::MachineError(_)
        | Trap::Unknown
        | Trap::Exception(Exception::AddressError)
        | _ => {
            panic!(
                "Unsupported trap {:?}, stval = {:#x}, BadI = {:#x}!",
                cause, stval, badi
            );
        }
    }
    {
        let task = current_task().unwrap();
        let mut inner = task.acquire_inner_lock();
        inner.update_process_times_leave_trap(cause);
    }
    trap_return();
}
```

la64 的核心差异集中在三处：syscall 分支同时推进 `ERA` 和 `cx.gp.pc`；store/page modify 类缺页成功后补写 dirty bit；timer interrupt 分支通过 `TIClr` 清除硬件中断状态后再进入 task timer handler。完整函数还包含 `SIGILL`、`SIGSEGV`、breakpoint 和非对齐访存模拟分支。

## 5. 缺页入口

### 5.1 访问类型映射

| MM 访问类型 | rv64 trap | la64 trap |
|-------------|-----------|-----------|
| `FaultAccess::Execute` | `InstructionFault`, `InstructionPageFault` | `PageInvalidFetch`, `PageNonExecutableFault` |
| `FaultAccess::Load` | `LoadFault`, `LoadPageFault` | `PageInvalidLoad`, `PageNonReadableFault`, `PagePrivilegeIllegal` 的非 store/fetch 分支 |
| `FaultAccess::Store` | `StoreFault`, `StorePageFault` | `PageInvalidStore`, `PageModifyFault` |

两套后端都在进入 MM 前调用 `frame_reserve(3)`，为缺页处理保留少量物理页余量。

### 5.2 调用路径

rv64：

```rust
let addr = VirtAddr::from(stval);
let pf_result = task.process.vm().write(|vm| vm.do_page_fault(addr, access));
```

la64：

```rust
let addr = VirtAddr::from(get_bad_addr());
let vm_ref = task.process.vm();
let mut mset_lock = vm_ref.lock();
```

随后调用 `mset_lock.do_page_fault(addr, access)`，并在同一 match 分支中把 `MemoryError` 映射为 `SIGBUS`、`SIGSEGV` 或 `pending_oom_kill`。缺页处理由 `mm::address_space::AddressSpace::do_page_fault()` 继续完成。该函数会查找覆盖 VMA、处理 `MAP_GROWSDOWN`、再进入 `mm::page_fault` 的动作分类。

### 5.3 缺页错误到信号

| `MemoryError` | trap 后端行为 |
|---------------|---------------|
| `BeyondEOF`、`BackingStoreFailure` | 注入 `SIGBUS` |
| `NoPermission` | 移除当前 `SIGSEGV` mask，注入 `SIGSEGV` + `SEGV_ACCERR` |
| `BadAddress`、`NotMapped` | 移除当前 `SIGSEGV` mask，注入 `SIGSEGV` + `SEGV_MAPERR` |
| `OutOfMemory` | 设置 `inner.pending_oom_kill = true` |
| 其他错误 | 打印 warn，注入 `SIGSEGV` + `SEGV_MAPERR` |

该映射在 rv64 和 la64 trap 后端中保持一致。

### 5.4 la64 dirty bit 补写

la64 在 store/page modify 类异常缺页成功后执行：

```rust
LAFlexPageTable::from_token(task.get_user_token())
    .set_dirty_bit(addr.floor())
    .unwrap();
```

这条路径只在 `PageModifyFault | PageInvalidStore` 情况下触发，用于把硬件页表语义和 MM 的 fault 修复结果对齐。

## 6. 普通异常与信号

| 场景 | rv64 行为 | la64 行为 |
|------|-----------|-----------|
| 非法指令 | `IllegalInstruction`/`InstructionMisaligned` 注入 `SIGILL` | `InstructionNonDefined`、FPU unavailable、privilege illegal 等注入 `SIGILL` |
| 地址错误 | 未单独列出，未匹配 trap 会 panic | `AddressError` 注入 `SIGSEGV` |
| 用户非对齐访存 | 未接入模拟路径 | `AddressNotAligned` 解码指令并通过 uaccess 模拟 load/store |
| breakpoint | 未单独列出 | `Breakpoint` 调用 `read_bp()` |
| kernel trap | `trap_from_kernel()` panic | trap handler 入口检查与 kernel trap 路径诊断 |

la64 的非对齐访存模拟会读取用户 PC 处指令，解析 load/store 类型和宽度，再用 `copy_from_user` 或 `copy_to_user` 逐字节访问用户地址。模拟成功后手动推进 `cx.gp.pc += 4`。

## 7. Timer interrupt

### 7.1 RISC-V

`Trap::Interrupt(Interrupt::SupervisorTimer)` 分支：

```
record_sched_timer_interrupt()
TIMER_INTERRUPT += 1
record_sched_timer_trap_cycles()
task::timer_interrupt_handler()
```

该分支还记录 `task::perf::record_timer_interrupt()`。

### 7.2 LoongArch64

`Trap::Interrupt(Interrupt::Timer)` 分支：

```
record_sched_timer_interrupt()
TIClr::read().clear_timer().write()
record_sched_timer_trap_cycles()
task::timer_interrupt_handler()
```

la64 显式清除 timer 中断状态，随后交给 task 层 timer handler。

## 8. 返回用户态

### 8.1 RISC-V `trap_return()`

RISC-V 返回路径：

```
let task = do_signal();
set_user_trap_entry();
trap_cx.kernel_cpu_local = cpu_local_ptr();
trap_cx.prepare_return();
let trap_cx_ptr = task.trap_cx_user_va();
let user_satp = task.process.prepare_user_vm();
let restore_va = __restore - __alltraps + TRAMPOLINE;
drop(task);
fence.i;
jr restore_va(a0=trap_cx_ptr, a1=user_satp)
```

`set_user_trap_entry()` 把 `stvec` 设置为 `TRAMPOLINE`。恢复汇编由 trampoline 映射执行。
返回前必须把保存态规范为 `SPP=User、SIE=0、SPIE=1`：`__restore` 写入
`sstatus` 后还要恢复 `sepc` 和通用寄存器，期间仍处于 S-mode；只有最终 `sret`
才能从 `SPIE` 原子恢复 `SIE`。若保存态提前携带 `SIE=1`，timer/IPI 可在半恢复
现场上嵌套进入，进而破坏用户寄存器。该约束来自 RISC-V 特权架构的
[Supervisor Status 规范](https://docs.riscv.org/reference/isa/priv/supervisor.html)：显式写
`sstatus` 后必须立即重新评估中断条件，而 `sret` 才执行 `SIE <- SPIE`。

源码实现如下：

```rust
#[no_mangle]
pub fn trap_return() -> ! {
    let task = do_signal();
    set_user_trap_entry();
    {
        let inner = task.acquire_inner_lock();
        let trap_cx = inner.get_trap_cx();
        trap_cx.kernel_cpu_local = crate::hal::cpu_local_ptr();
        trap_cx.prepare_return();
    }
    let trap_cx_ptr = task.trap_cx_user_va();
    let user_satp = task.process.prepare_user_vm();
    let restore_va = __restore as usize - __alltraps as usize + TRAMPOLINE;
    drop(task);
    unsafe {
        asm!(
            "fence.i",
            "jr {restore_va}",
            restore_va = in(reg) restore_va,
            in("a0") trap_cx_ptr,
            in("a1") user_satp,
            options(noreturn)
        );
    }
}
```

syscall 分支自身通过 `current_task()` 持有一个 `Arc<TaskControlBlock>`，也必须在调用
`trap_return()` 前显式 `drop(task)`。原因是返回汇编标记为 `noreturn`，Rust 不会展开
调用者栈帧；若依赖作用域自动析构，每次 syscall 都会永久泄漏一个强引用。LA64 的
syscall 分支遵守同一条生命周期规则。

### 8.2 LoongArch64 `trap_return()`

la64 返回路径包含：

| 动作 | 说明 |
|------|------|
| `do_signal()` | 先执行信号交付 |
| `set_user_trap_entry()` | exception entry 指向 `strampoline` |
| `PrMd` 设置 | `pplv=3`、`pie=true`，准备回到用户态 |
| 传参 | trap context、用户 token、ASID 传给 `__restore` |

ASID 来自当前任务的 la64 字段；fork/clone 创建任务时由 la64 路径分配。

源码实现如下：

```rust
pub fn trap_return() -> ! {
    let task = do_signal();
    set_user_trap_entry();
    let trap_cx = task.acquire_inner_lock().get_trap_cx();
    let trap_cx_ptr = trap_cx as *const TrapContext as usize;
    trap_cx.sstatus.set_pplv(3).set_pie(true);
    let asid = task.asid.load(core::sync::atomic::Ordering::Relaxed);
    if asid != 0 {
        crate::task::perf::record_tlb_activate();
    }
    let user_satp = current_user_token();
    let restore_va = __restore as usize - __alltraps as usize + TRAMPOLINE;
    unsafe {
        asm!(
            "ibar 0",
            "move $ra, {0}",
            "move $a0, {1}",
            "move $a1, {2}",
            "move $a2, {3}",
            "jr $ra",
            in(reg) restore_va,
            in(reg) trap_cx_ptr,
            in(reg) user_satp,
            in(reg) asid as usize,
            options(noreturn)
        );
    }
}
```

LA64 将普通 `TRAMPOLINE`、`TRAP_CONTEXT_BASE` 与用户可执行的
`SIGNAL_TRAMPOLINE` 分为连续三页：普通恢复页仅映射 `R|X`，信号页映射
`R|X|U`。因此切换用户 PGDL 后，`__restore` 从普通别名执行而不会复用信号别名。

## 9. 与 syscall 分发层的边界

trap 后端只做 ABI 和异常入口工作。具体 syscall 语义位于：

| 领域 | 代码路径 |
|------|----------|
| 分发和统计 | `os/src/syscall/mod.rs` |
| syscall 编号 | `os/src/syscall/syscall_id.rs` |
| 文件 I/O | `os/src/syscall/fs.rs` |
| 进程、MM、信号、时间、IPC | `os/src/syscall/process/` |
| 网络 | `os/src/net/syscall/` |

`syscall::syscall()` 成功返回非负值，失败返回负 errno。trap 后端不重新解释 errno，只把 `isize` 结果放回 `a0`。

## 10. 调试入口

| 症状 | 检查点 |
|------|--------|
| syscall 参数错位 | trap handler 收集 `a0..a5` 的位置 |
| syscall 返回值被覆盖 | id 139 分支是否生效 |
| 用户缺页后信号不对 | `MemoryError` 到信号映射 |
| la64 写缺页反复触发 | `set_dirty_bit()` 是否成功 |
| 非阻塞 sleep/timeout 不醒 | timer interrupt 是否进入 `task::timer_interrupt_handler()` |
| 用户态无法返回 | `trap_return()` 的 trampoline 地址、token、ASID |

### 10.1 阅读 trap 代码的顺序

读 trap 后端时，建议按“入口保存现场 -> 分派原因 -> 修正上下文 -> 返回用户态”的顺序走：

| 阅读点 | 要确认的事实 |
|--------|--------------|
| trap entry 汇编 | 用户寄存器如何保存到 `TrapContext`，内核栈如何切换。 |
| syscall 分支 | 是否先推进用户 PC，再读取 `a7/a0..a5`，`rt_sigreturn` 是否走特殊返回。 |
| page fault 分支 | fault address、访问类型和 `MemoryError` 如何传给 MM 以及如何映射成信号。 |
| timer 分支 | 中断是否清除/重置，下游是否调用 task timer handler。 |
| 普通异常 | 非 syscall/fault 的异常如何注入 `SIGILL/SIGSEGV` 或进入诊断。 |
| `trap_return()` | 返回前是否执行 signal delivery，最终 token/ASID/trampoline 参数是否正确。 |

这条顺序能帮助区分“架构入口错误”和“领域子系统错误”。例如 `read()` 返回 `EFAULT` 通常要追 uaccess/MM；而 `read()` 参数整体错位则应先检查 trap 后端收集寄存器的位置。

## 11. 测试映射

| 测试类别 | 覆盖路径 |
|----------|----------|
| basic syscall | syscall ABI、分发、返回值 |
| mmap/fork/exec | 缺页、CoW、ELF 映射、trap signal |
| signal | `do_signal()`、`rt_sigreturn` 特殊返回 |
| nanosleep/futex timeout | timer interrupt 与 task timer |
| la64 非对齐访存用例 | `AddressNotAligned` 模拟路径 |
| QEMU 启动 | trap entry、timer interrupt、返回用户态 |

## 12. 源文件索引

| 路径 | 内容 |
|------|------|
| `os/src/hal/arch/riscv/trap/mod.rs` | rv64 trap 分派、syscall、缺页、timer、返回用户态 |
| `os/src/hal/arch/riscv/trap/trap.S` | rv64 trap 保存/恢复汇编 |
| `os/src/hal/arch/riscv/trap/context.rs` | rv64 trap context |
| `os/src/hal/arch/loongarch64/trap/mod.rs` | la64 trap 分派、syscall、缺页、timer、非对齐访存 |
| `os/src/hal/arch/loongarch64/trap/trap.S` | la64 trap 保存/恢复汇编 |
| `os/src/hal/arch/loongarch64/trap/context.rs` | la64 trap context |
| `os/src/syscall/mod.rs` | syscall 分发和统计 |
| `os/src/mm/address_space.rs` | `AddressSpace::do_page_fault()` |
| `os/src/mm/page_fault.rs` | fault 动作分类 |
| `os/src/task/signal/` | `do_signal()`、信号 frame 与 pending 队列 |
