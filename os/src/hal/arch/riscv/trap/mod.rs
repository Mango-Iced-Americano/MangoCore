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
            if let Err(error) = task.process.vm().lock().do_page_fault(addr, access) {
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
            let task = current_task_ref().unwrap();
            let mut inner = task.acquire_inner_lock();
            inner.sigmask.remove(Signals::SIGILL);
            inner.add_signal_with_code(Signals::SIGILL, SigInfo::ILL_ILLOPC);
        }
        Trap::Interrupt(Interrupt::SupervisorTimer) => {
            crate::task::perf::record_timer_interrupt();
            do_wake_expired();
            NET_INTERFACE.try_poll();
            unsafe {
                TIMER_INTERRUPT += 1;
            }
            set_next_trigger();
            suspend_current_and_run_next();
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
    set_user_trap_entry();
    let trap_cx_ptr = task.trap_cx_user_va();
    let user_satp = current_user_token();
    let restore_va = __restore as usize - __alltraps as usize + TRAMPOLINE;
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
