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
use crate::task::{current_task, current_trap_task, do_signal, signal::SigInfo, Signals};
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
    // 再单独打开 SSIE。timer 会在调度器发布首个 deadline 后另行开放，
    // external IRQ 在当前阶段继续保持关闭。
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
    // 用户 x4/tp 已保存，汇编已恢复本 CPU 的 PerCpu 指针；下方取得
    // current 时会同时校验 `Running(cpu)`，不再把用户 trap 限制在 CPU0。
    let scause = scause::read();
    let stval = stval::read();

    if let Trap::Exception(Exception::UserEnvCall) = scause.cause() {
        let _trap_start = crate::task::perf::perf_time_now();
        let task = current_trap_task();
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
        // exit(2) 不会返回到本 Rust 栈帧。在进入 syscall 前释放临时 Arc，
        // 避免退出任务永久留住 TCB 和它的内核栈。
        drop(task);
        // trap frame、kernel stvec 和 CPU-local tp 都已完整建立，且
        // task.inner 已释放。只在真正执行 syscall 时开放 timer/IPI，
        // 写回 trap context 前 helper 会恢复为关中断。
        let result = crate::hal::with_local_interrupts_enabled(|| syscall(syscall_id, args));
        // The trap context may be replaced by execve or restored by sigreturn,
        // so fetch it again after syscall returns.
        let task = current_trap_task();
        let (user_us, system_us) = {
            let mut inner = task.acquire_inner_lock();
            let cx = inner.trap_context_mut();
            // sigreturn(139) already restored the full trap context (including a0).
            if syscall_id != 139 {
                cx.gp.a0 = result as usize;
            }
            inner.update_process_times_before_safe_point()
        };
        task.process.account_cpu_time(user_us, system_us);
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
        let task = current_trap_task();
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
            crate::task::perf::record_pagefault_stage(
                0,
                crate::task::perf::perf_memory_io_time_now().wrapping_sub(pagefault_entry_start),
            );
            crate::task::perf::record_page_fault();
            // VM update 会在解锁后等待远端 TLB ack；task.inner 只能在结果
            // 返回后获取，否则会把普通锁带过 shootdown 等待点。
            let vm = task.process.vm();
            let pf_result = loop {
                let outcome = vm.write(|inner| inner.do_page_fault(addr, access));
                match outcome {
                    crate::mm::FaultOutcome::Completed(_) => break Ok(()),
                    crate::mm::FaultOutcome::Retry(wait) => {
                        // Retry token 离开 `AddressSpace::write` 后才等待；此时 VM
                        // 锁与本轮 TLB gather 都已经释放，writeback 可继续 mkclean。
                        wait.wait();
                    }
                    crate::mm::FaultOutcome::Error(error) => break Err(error),
                }
            };
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
            task.process.notify_signal_waiters();
            crate::task::perf::arm_pagefault_return();
        }
        Trap::Exception(Exception::IllegalInstruction)
        | Trap::Exception(Exception::InstructionMisaligned) => {
            let task = current_task().unwrap();
            let mut inner = task.acquire_inner_lock();
            inner.sigmask.remove(Signals::SIGILL);
            inner.add_signal_with_code(Signals::SIGILL, SigInfo::ILL_ILLOPC);
            drop(inner);
            task.process.notify_signal_waiters();
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
        let (user_us, system_us) = task
            .acquire_inner_lock()
            .update_process_times_before_safe_point();
        task.process.account_cpu_time(user_us, system_us);
    }
    trap_return();
}

#[no_mangle]
pub fn trap_return() -> ! {
    // trap frame 已完整、当前任务锁均已释放；timer callback 与 RESCHEDULE
    // 只能在这个统一边界让出 CPU，不能从 hard IRQ 直接切换任务。
    crate::task::run_task_safe_point();
    let task = do_signal();
    let pagefault_return_start = if crate::task::perf::take_pagefault_return_pending() {
        crate::task::perf::perf_memory_io_time_now()
    } else {
        0
    };
    set_user_trap_entry();
    // Refresh after signal/exec context changes and on every future migration:
    // the CPU performing this return owns the pointer installed on next trap.
    {
        let mut inner = task.acquire_inner_lock();
        let trap_cx = inner.trap_context_mut();
        trap_cx.kernel_cpu_local = crate::hal::cpu_local_ptr();
        // 返回汇编在写 sstatus 后仍需恢复寄存器。这里强制 SIE=0、SPIE=1，
        // 保证 timer/IPI 只能在最终 SRET 完成现场切换后重新响应。
        trap_cx.prepare_return();
    }
    let trap_cx_ptr = task.trap_cx_user_va();
    // rollover 的远端 ack 表示目标 CPU 已进入内核。这里到 SRET 必须持续关中断，
    // 防止 CPU 先 ack、再带着旧 epoch 的 SATP 返回用户态。
    assert!(
        !sstatus::read().sie(),
        "RISC-V trap_return requires local interrupts disabled"
    );
    // 先登记本 CPU 可能缓存当前 MM，再取得权威 token。后续页表修改方将以
    // 该驻留集合为 shootdown 目标，不能继续只读无锁 token hint。
    let user_vm = task.process.activate_user_vm();
    let user_satp = super::sv39::satp_with_asid(user_vm.token, user_vm.asid);
    let restore_va = __restore as usize - __alltraps as usize + TRAMPOLINE;
    if pagefault_return_start != 0 {
        crate::task::perf::record_pagefault_stage(
            6,
            crate::task::perf::perf_memory_io_time_now().wrapping_sub(pagefault_return_start),
        );
    }
    // 安全点、信号递送和 MM 激活都仍属于内核态。必须到真正执行 SRET 前
    // 才闭合 system 区间并开启 user 区间，否则安全点调度出去的时间会被
    // 错算为 user time，并在迁移后归到错误 CPU。
    let (user_us, system_us) = task
        .acquire_inner_lock()
        .update_process_times_enter_user();
    task.process.account_cpu_time(user_us, system_us);
    // `asm!(noreturn)` 不会展开 Rust 栈帧。current 槽仍持有 owner，
    // 这个仅供恢复路径读取状态的本地 Arc 必须在跳转前释放。
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
