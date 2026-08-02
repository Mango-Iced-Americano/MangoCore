//! RISC-V trap 分发和返回路径。
//!
//! 处理 syscall、缺页、设备中断、timer interrupt，并在返回用户态前完成信号
//! 和调度相关收尾。

pub mod context;
use core::arch::{asm, global_asm};

use super::TrapImpl;
use crate::config::TRAMPOLINE;
use crate::hal::arch::riscv::time::set_next_trigger;
use crate::mm::{frame_reserve, FaultAccess, MemoryError, VirtAddr};
use crate::net::config::NET_INTERFACE;
use crate::syscall::syscall;
use crate::task::{
    current_task_ref, current_user_token, do_signal, do_wake_expired, signal::SigInfo,
    suspend_current_and_run_next, Signals,
};
use crate::timer::{ITimerVal, TimeVal};
use alloc::format;
pub use context::{UserContext, UserSignalMask};
use riscv::register::{
    mtvec::TrapMode,
    scause::{self, Exception, Interrupt, Trap},
    sepc, sie, stval, stvec,
};

pub static mut TIMER_INTERRUPT: usize = 0;

pub fn get_bad_addr() -> usize {
    stval::read()
}

pub fn get_bad_instruction() -> usize {
    stval::read()
}

pub fn get_exception_cause() -> TrapImpl {
    scause::read().cause()
}

global_asm!(include_str!("trap.S"));

extern "C" {
    pub fn __alltraps();
    pub fn __restore();
    pub fn __call_sigreturn();
}

pub fn init() {
    set_kernel_trap_entry();
}

fn set_kernel_trap_entry() {
    unsafe {
        stvec::write(trap_from_kernel as usize, TrapMode::Direct);
    }
}

fn set_user_trap_entry() {
    unsafe {
        stvec::write(TRAMPOLINE as usize, TrapMode::Direct);
    }
}

pub fn enable_timer_interrupt() {
    unsafe {
        sie::set_stimer();
    }
}

#[no_mangle]
pub fn trap_handler() -> ! {
    let scause = scause::read();
    set_kernel_trap_entry();
    let stval = stval::read();

    if let Trap::Exception(Exception::UserEnvCall) = scause.cause() {
        let _trap_start = crate::task::perf::perf_time_now();
        let task = current_task_ref().unwrap();
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
        if _trap_start != 0 {
            let _trap_ticks = crate::task::perf::perf_time_now().wrapping_sub(_trap_start);
            crate::task::perf::record_trap_cost_ticks(_trap_ticks);
        }
        trap_return();
    }

    {
        let task = current_task_ref().unwrap();
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
            let pagefault_entry_start = crate::task::perf::perf_memory_io_time_now();
            let task = current_task_ref().unwrap();
            let mut inner = task.acquire_inner_lock();
            let addr = VirtAddr::from(stval);
            // This is where we handle the page fault.
            frame_reserve(3);
            let access = match scause.cause() {
                Trap::Exception(Exception::StoreFault)
                | Trap::Exception(Exception::StorePageFault) => FaultAccess::Store,
                Trap::Exception(Exception::InstructionFault)
                | Trap::Exception(Exception::InstructionPageFault) => FaultAccess::Execute,
                _ => FaultAccess::Load,
            };
            let _pf_start =
                crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
            crate::task::perf::record_pagefault_stage(
                0,
                crate::task::perf::perf_memory_io_time_now().wrapping_sub(pagefault_entry_start),
            );
            crate::task::perf::record_page_fault();
            let pf_result = task.process.vm().lock().do_page_fault(addr, access);
            crate::task::perf::record_pagefault_time_us(
                crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO)
                    .saturating_sub(_pf_start),
            );
            if let Err(error) = pf_result {
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
            drop(inner);
            task.process.notify_signal_waiters();
            crate::task::perf::arm_pagefault_return();
        }
        Trap::Exception(Exception::IllegalInstruction)
        | Trap::Exception(Exception::InstructionMisaligned) => {
            let task = current_task_ref().unwrap();
            let mut inner = task.acquire_inner_lock();
            inner.sigmask.remove(Signals::SIGILL);
            inner.add_signal_with_code(Signals::SIGILL, SigInfo::ILL_ILLOPC);
            drop(inner);
            task.process.notify_signal_waiters();
        }
        Trap::Interrupt(Interrupt::SupervisorTimer) => {
            let trap_profile_start = crate::task::processor::sched_profile_cycle_start();
            crate::task::perf::record_timer_interrupt();
            crate::task::processor::record_sched_timer_interrupt();
            unsafe {
                TIMER_INTERRUPT += 1;
            }
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
        let task = current_task_ref().unwrap();
        let mut inner = task.acquire_inner_lock();
        inner.refresh_real_timer();
        inner.update_process_times_leave_trap(scause.cause());
    }
    trap_return();
}

#[no_mangle]
pub fn trap_return() -> ! {
    let task = do_signal();
    let pagefault_return_start = if crate::task::perf::take_pagefault_return_pending() {
        crate::task::perf::perf_memory_io_time_now()
    } else {
        0
    };
    set_user_trap_entry();
    let trap_cx_ptr = task.trap_cx_user_va();
    let user_satp = current_user_token();
    let restore_va = __restore as usize - __alltraps as usize + TRAMPOLINE;
    if pagefault_return_start != 0 {
        crate::task::perf::record_pagefault_stage(
            6,
            crate::task::perf::perf_memory_io_time_now().wrapping_sub(pagefault_return_start),
        );
    }
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

#[no_mangle]
pub fn trap_from_kernel() -> ! {
    panic!(
        "a trap {:?} from kernel! bad addr = {:#x}, bad instruction = {:#x}",
        riscv::register::scause::read().cause(),
        riscv::register::stval::read(),
        riscv::register::sepc::read(),
    );
}
