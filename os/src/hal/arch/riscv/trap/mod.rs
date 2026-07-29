//! RISC-V trap 分发和返回路径。
//!
//! 处理 syscall、缺页、设备中断、timer interrupt，并在返回用户态前完成信号
//! 和调度相关收尾。

pub mod context;
use core::arch::{asm, global_asm};

use super::TrapImpl;
use crate::config::TRAMPOLINE;
use crate::mm::{frame_reserve, FaultAccess, MemoryError, VirtAddr};
use crate::syscall::syscall;
use crate::task::{current_task, do_signal, signal::SigInfo, Signals};
use crate::timer::{ITimerVal, TimeVal};
use alloc::format;
pub use context::{UserContext, UserSignalMask};
use riscv::register::{
    mtvec::TrapMode,
    scause::{self, Exception, Interrupt, Trap},
    sepc, sie, sstatus, stval, stvec,
};

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
    fn __kern_trap();
}

pub fn init() {
    set_kernel_trap_entry();
}

fn set_kernel_trap_entry() {
    unsafe {
        stvec::write(__kern_trap as usize, TrapMode::Direct);
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

/// CPU0 接收 AP→BSP IPI 所需的本地 SSIE；全局 SIE 仍由执行上下文控制。
pub fn enable_ipi_interrupt() {
    unsafe {
        sie::set_ssoft();
    }
}

/// 用户态和内核态共用的 IPI hard-IRQ fast path。
fn handle_ipi_interrupt() {
    // OpenSBI 把 IPI 表现为 SSIP。先清电平源，再消费 Release 发布的
    // mailbox；并发的新 doorbell 即使与 swap 交错，也只会产生空中断。
    unsafe { asm!("csrci sip, 2") };
    crate::smp::handle_ipi();
}

/// 两种 trap 来源共享的 timer hard-IRQ fast path。
///
/// 性能计数和硬件静默之外只发布 per-CPU pending；队列锁、callback 和调度
/// 统一延迟到 trap_return 或 scheduler idle 安全点。
fn handle_timer_interrupt() {
    let trap_profile_start = crate::task::processor::sched_profile_cycle_start();
    crate::task::perf::record_timer_interrupt();
    crate::task::processor::record_sched_timer_interrupt();
    crate::task::timer_interrupt_handler();
    crate::task::processor::record_sched_timer_trap_cycles(trap_profile_start);
}

/// 为 AP 建立只接收 IPI 的内核中断窗口。
pub fn init_ipi_only() {
    set_kernel_trap_entry();
    // Safety: AP 尚未 online，无发送者；先清全部局部 enable 和旧 SSIP，
    // 再单独打开 SSIE，避免 timer/external IRQ 混入本工作包。
    unsafe {
        asm!("csrw sie, zero", "csrci sip, 2");
        sie::set_ssoft();
        // 全局 SIE 最后打开，保证 trap vector 和局部 mask 已经生效。
        sstatus::set_sie();
    }
}

#[no_mangle]
pub fn trap_handler() -> ! {
    // Any diagnostic failure below must use the kernel trap vector rather than
    // re-entering the user trampoline with a kernel stack in `sp`.
    set_kernel_trap_entry();
    // User x4/tp has already been saved.  This validation proves the assembly
    // reinstalled a configured PerCpu pointer before Rust consumes CPU state.
    let cpu_id = crate::smp::cpu_id();
    assert_eq!(
        cpu_id,
        crate::smp::BOOT_CPU_ID,
        "Phase 1 user task trapped on non-boot CPU {}",
        cpu_id
    );
    let scause = scause::read();
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
        // trap frame、kernel stvec 和 CPU-local tp 都已完整建立，且
        // task.inner 已释放。只在真正执行 syscall 时开放 timer/IPI，
        // 写回 trap context 前 helper 会恢复为关中断。
        let result = crate::hal::with_local_interrupts_enabled(|| syscall(syscall_id, args));
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
        // trap_return() 最终以 noreturn 汇编离开，Rust 不会展开当前栈帧；
        // 必须在此释放 syscall 分支持有的 Arc，避免每次系统调用泄漏一次引用。
        drop(task);
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
            crate::task::perf::record_page_fault();
            // VM update 会在解锁后等待远端 TLB ack；task.inner 只能在结果
            // 返回后获取，否则会把普通锁带过 shootdown 等待点。
            let pf_result = task.process.vm().write(|vm| vm.do_page_fault(addr, access));
            crate::task::perf::record_pagefault_time_us(
                crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO)
                    .saturating_sub(_pf_start),
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
            handle_timer_interrupt();
        }
        Trap::Interrupt(Interrupt::SupervisorSoft) => {
            handle_ipi_interrupt();
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

#[no_mangle]
pub fn trap_return() -> ! {
    // trap frame 已完整、当前任务锁均已释放；这是 timer callback 和安全抢占
    // 可以运行的第一个统一边界。新产生的信号随后由 do_signal() 同轮处理。
    crate::task::run_deferred_timer_at_task_safe_point();
    let task = do_signal();
    set_user_trap_entry();
    // Refresh after signal/exec context changes and on every future migration:
    // the CPU performing this return owns the pointer installed on next trap.
    {
        let inner = task.acquire_inner_lock();
        let trap_cx = inner.get_trap_cx();
        trap_cx.kernel_cpu_local = crate::hal::cpu_local_ptr();
        // 返回汇编在写 sstatus 后仍需恢复寄存器。这里强制 SIE=0、SPIE=1，
        // 保证 timer/IPI 只能在最终 SRET 完成现场切换后重新响应。
        trap_cx.prepare_return();
    }
    let trap_cx_ptr = task.trap_cx_user_va();
    // 先登记本 CPU 可能缓存当前 MM，再取得权威 token。后续页表修改方将以
    // 该驻留集合为 shootdown 目标，不能继续只读无锁 token hint。
    let user_vm = task.process.activate_user_vm();
    let restore_va = __restore as usize - __alltraps as usize + TRAMPOLINE;
    // `asm!(noreturn)` 不会展开 Rust 栈帧。current 槽仍持有 owner，
    // 这个仅供恢复路径读取状态的本地 Arc 必须在跳转前释放。
    drop(task);
    unsafe {
        asm!(
            "fence.i",
            "jr {restore_va}",
            restore_va = in(reg) restore_va,
            in("a0") trap_cx_ptr,
            in("a1") user_vm.token,
            options(noreturn)
        );
    }
}

#[no_mangle]
pub extern "C" fn trap_from_kernel() {
    let cause = riscv::register::scause::read().cause();
    match cause {
        Trap::Interrupt(Interrupt::SupervisorSoft) => {
            handle_ipi_interrupt();
            return;
        }
        Trap::Interrupt(Interrupt::SupervisorTimer) => {
            // 内核 timer 与用户 timer 使用相同无锁 fast path；这里绝不从
            // 被中断的任意内核位置切换任务。
            handle_timer_interrupt();
            return;
        }
        _ => {}
    }
    panic!(
        "a trap {:?} from kernel! bad addr = {:#x}, bad instruction = {:#x}",
        cause,
        riscv::register::stval::read(),
        riscv::register::sepc::read(),
    );
}
