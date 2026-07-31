//! LoongArch64 trap 分发和返回路径。
//!
//! 处理 syscall、TLB refill/page fault、设备中断、timer interrupt、未对齐访存
//! 和返回用户态前的信号/调度收尾。

mod context;
mod mem_access;
use self::context::GeneralRegs;

use super::register::{self, Exception, Interrupt, Trap, ERA};
use super::MErrEntry;
use crate::hal::arch::get_clock_freq;
use crate::hal::arch::loongarch64::register::{CrMd, ECfg, LineBasedInterrupt, PrMd};
use crate::hal::arch::loongarch64::trap::mem_access::Instruction;
use crate::hal::arch::TICKS_PER_SEC;
use crate::mm::{copy_from_user, copy_to_user, frame_reserve, FaultAccess, MemoryError, VirtAddr};
use crate::net::config::NET_INTERFACE;
use crate::syscall::syscall;
use crate::task::{
    current_task, current_trap_cx, current_trap_task, current_user_token, do_signal,
    do_wake_expired, signal::SigInfo, suspend_current_and_run_next, Signals,
};
use core::arch::{asm, global_asm, naked_asm};
use core::ptr::{addr_of, addr_of_mut};

#[cfg(all(feature = "board_2k1000", feature = "board_bringup_trace"))]
static BOARD_FIRST_TRAP_RETURN: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub use context::{LsxRegs, MachineContext, TrapContext, UserContext, UserSignalMask};
use register::{
    BadV, EStat, TLBRBadV, TLBREHi, TLBRELo0, TLBRELo1, TLBRPrMd, PGD, PGDH, PGDL, PWCH, PWCL,
    TLBRERA,
};
pub type TrapImpl = Trap;
global_asm!(include_str!("trap.S"));

extern "C" {
    pub fn __alltraps();
    pub fn __restore();
    pub fn __call_sigreturn();
    pub fn strampoline();
    pub fn __kern_trap();
}

#[allow(unused)]
#[link_section = ".text.__rfill"]
#[unsafe(naked)]
#[no_mangle]
pub extern "C" fn __rfill() {
    //crmd = 0b0_01_01_10_0_00;
    //         w_dm_df_pd_i_lv;
    // let i = 0xA8;
    naked_asm!(
        // PGD: 0x1b CRMD:0x0 PWCL:0x1c TLBRBADV:0x89 TLBERA:0x8a TLBRSAVE:0x8b SAVE:0x30
        // TLBREHi: 0x8e STLBPS: 0x1e MERRsave:0x95
        "
    csrwr  $t0, 0x8b



    csrrd  $t0, 0x1b
    lddir  $t0, $t0, 3
    andi   $t0, $t0, 1
    beqz   $t0, 1f

    csrrd  $t0, 0x1b
    lddir  $t0, $t0, 3
    addi.d $t0, $t0, -1
    lddir  $t0, $t0, 1
    andi   $t0, $t0, 1
    beqz   $t0, 1f
    csrrd  $t0, 0x1b
    lddir  $t0, $t0, 3
    addi.d $t0, $t0, -1
    lddir  $t0, $t0, 1
    addi.d $t0, $t0, -1

    ldpte  $t0, 0
    ldpte  $t0, 1
    csrrd  $t0, 0x8c
    csrrd  $t0, 0x8d
    csrrd  $t0, 0x0
2:
    tlbfill
    csrrd  $t0, 0x89
    srli.d $t0, $t0, 13
    slli.d $t0, $t0, 13
    csrwr  $t0, 0x11
    tlbsrch
    tlbrd
    csrrd  $t0, 0x12
    csrrd  $t0, 0x13
    csrrd  $t0, 0x8b
    ertn
1:
    csrrd  $t0, 0x8e
    # TLBREHI.PS 可能保留固件或上一次 TLB 重填的状态。若不先清除第 5:0 位，
    # 直接与 0xC 按位或无法保证得到 4KiB 所需的 PS=12。
    bstrins.d $t0, $zero, 5, 0
    ori    $t0, $t0, 0xC
    csrwr  $t0, 0x8e

    rotri.d $t0, $t0, 61
    ori    $t0, $t0, 3
    rotri.d $t0, $t0, 3

    csrwr  $t0, 0x8c
    csrrd  $t0, 0x8c
    csrwr  $t0, 0x8d
    b      2b
"
    )
}

pub fn init() {
    set_kernel_trap_entry();
}
pub fn get_bad_ins_addr() -> usize {
    match get_exception_cause() {
        Trap::Interrupt(_) | Trap::Exception(_) => register::ERA::read().get_pc(),
        Trap::TLBReFill => register::TLBRERA::read().get_pc(),
        Trap::MachineError(_) => register::MErrEra::read().get_pc(),
        Trap::Unknown => 0,
    }
}
pub fn get_bad_addr() -> usize {
    match get_exception_cause() {
        Trap::Exception(_) => register::BadV::read().get_vaddr(),
        Trap::TLBReFill => register::TLBRBadV::read().get_vaddr(),
        _ => 0,
    }
}
pub fn get_bad_instruction() -> usize {
    register::BadI::read().get_inst()
}
pub fn get_exception_cause() -> TrapImpl {
    register::EStat::read().cause()
}
pub fn set_kernel_trap_entry() {
    register::EEntry::read()
        .set_exception_entry(__kern_trap as usize)
        .write()
}
pub fn set_machine_err_trap_ent() {
    MErrEntry::read().set_addr(trap_handler as usize).write();
}

fn set_user_trap_entry() {
    register::EEntry::read()
        .set_exception_entry(strampoline as usize)
        .write();
}

pub fn enable_timer_interrupt() {
    // 保留已经开放的 IPI 位；AP 必须同时接收调度 tick 与远程调度请求。
    ECfg::read()
        .set_line_based_interrupt_vector(LineBasedInterrupt::TIMER)
        .write();
}

/// 在不覆盖 timer mask 的前提下为 CPU0 开放本地 IPI line。
pub fn enable_ipi_interrupt() {
    ECfg::read()
        .set_line_based_interrupt_vector(LineBasedInterrupt::IPI)
        .write();
}

/// 用户态和内核态共用的 IPI hard-IRQ fast path。
fn handle_ipi_interrupt() {
    // IOCSR vector 是 level-triggered；先清硬件源，再 Acquire 消费 mailbox。
    super::clear_local_ipi();
    crate::smp::handle_ipi();
}

/// 用户态和内核态共用的 timer hard-IRQ fast path。
///
/// TICLR/one-shot 静默由 HAL 完成；这里不获取普通锁，也不执行 callback 或调度。
fn handle_timer_interrupt() {
    let trap_profile_start = crate::task::processor::sched_profile_cycle_start();
    crate::task::perf::record_timer_interrupt();
    crate::task::processor::record_sched_timer_interrupt();
    crate::task::timer_interrupt_handler();
    crate::task::processor::record_sched_timer_trap_cycles(trap_profile_start);
}

#[link_section = ".text.trap_handler"]
#[no_mangle]
pub fn trap_handler() -> ! {
    // Any diagnostic failure below must use the kernel trap vector rather than
    // re-entering the user trampoline with a kernel stack in `sp`.
    set_kernel_trap_entry();
    // 用户 r21 已保存，汇编已恢复本 CPU 的 PerCpu 指针；下方取得
    // current 时会同时校验 `Running(cpu)`，不再把用户 trap 限制在 CPU0。
    if PrMd::read().get_pplv() == 0 {
        panic!();
    }

    let cause = get_exception_cause();
    let stval = get_bad_addr();
    let badi = get_bad_instruction();

    if let Trap::Exception(Exception::Syscall) = cause {
        let _trap_start = crate::task::perf::perf_time_now();
        let task = current_trap_task();
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
        // exit(2) 不会返回到本 Rust 栈帧。在进入 syscall 前释放临时 Arc，
        // 避免退出任务永久留住 TCB 和它的内核栈。
        drop(task);
        // trap frame、kernel EENTRY 和 CPU-local r21 都已完整建立，且
        // task.inner 已释放。只在真正执行 syscall 时开放 timer/IPI，
        // 写回 trap context 前 helper 会恢复为关中断。
        let result = crate::hal::with_local_interrupts_enabled(|| syscall(syscall_id, args));
        // The trap context may be replaced by execve or restored by sigreturn,
        // so fetch it again after syscall returns.
        let task = current_trap_task();
        {
            let mut inner = task.acquire_inner_lock();
            let cx = inner.get_trap_cx();
            // sigreturn(139) already restored the full trap context (including a0).
            if syscall_id != 139 {
                cx.gp.a0 = result as usize;
            }
            inner.update_process_times_leave_trap(cause);
        }
        if _trap_start != 0 {
            let _trap_ticks = crate::task::perf::perf_time_now().wrapping_sub(_trap_start);
            crate::task::perf::record_trap_cost_ticks(_trap_ticks);
        }
        // trap_return() 通过不返回的恢复汇编离开，当前 Rust 栈帧不会析构；
        // 提前释放 syscall 分支的临时 Arc，避免每次系统调用累积一个强引用。
        drop(task);
        trap_return();
    }

    {
        let task = current_trap_task();
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
            let addr = VirtAddr::from(get_bad_addr());
            // This is where we handle the page fault.
            frame_reserve(3);
            let vm_ref = task.process.vm();
            let access = match cause {
                Trap::Exception(Exception::PageInvalidStore)
                | Trap::Exception(Exception::PageModifyFault) => FaultAccess::Store,
                Trap::Exception(Exception::PageInvalidFetch)
                | Trap::Exception(Exception::PageNonExecutableFault) => FaultAccess::Execute,
                _ => FaultAccess::Load,
            };
            let _pf_start =
                crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO);
            crate::task::perf::record_page_fault();
            // 缺页修复与 LA64 software-dirty PTE 更新合并在同一 VM 锁
            // 持有期；helper 先解锁再完成远端 shootdown。
            let pf_result = vm_ref.write(|vm| {
                let result = vm.do_page_fault(addr, access);
                if result.is_ok()
                    && matches!(
                        cause,
                        Trap::Exception(Exception::PageModifyFault | Exception::PageInvalidStore)
                    )
                {
                    vm.set_user_page_dirty(addr.floor()).unwrap();
                }
                result
            });
            crate::task::perf::record_pagefault_time_us(
                crate::task::perf::perf_time_now_for(crate::task::perf::STATS_PROFILE_MEMORY_IO)
                    .saturating_sub(_pf_start),
            );
            match pf_result {
                Err(error) => {
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
                }
                Ok(_) => {}
            };
        }
        Trap::Exception(Exception::InstructionNonDefined)
        | Trap::Exception(Exception::Exception10)
        | Trap::Exception(Exception::Exception11)
        | Trap::Exception(Exception::Exception12)
        | Trap::Exception(Exception::FloatingPointUnavailable)
        | Trap::Exception(Exception::InstructionPrivilegeIllegal) => {
            log::info!("[trap] trigger SIGILL/FPU from exception {:?}", cause);
            let task = current_task().unwrap();
            let mut inner = task.acquire_inner_lock();
            inner.sigmask.remove(Signals::SIGILL);
            inner.add_signal_with_code(Signals::SIGILL, SigInfo::ILL_ILLOPC);
        }
        Trap::Exception(Exception::AddressError) => {
            log::info!("[trap] trigger SIGSEGV from address error");
            let task = current_task().unwrap();
            let mut inner = task.acquire_inner_lock();
            inner.sigmask.remove(Signals::SIGSEGV);
            inner.add_signal_with_code(Signals::SIGSEGV, SigInfo::SEGV_MAPERR);
        }
        Trap::Interrupt(Interrupt::Timer) => {
            handle_timer_interrupt();
        }
        Trap::Interrupt(Interrupt::IPI) => {
            handle_ipi_interrupt();
        }
        Trap::Exception(Exception::Breakpoint) => {
            read_bp();
        }
        Trap::Exception(Exception::AddressNotAligned) => {
            let unaligned_start = crate::task::perf::perf_time_now();
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
            let is_store = op.is_store();
            let is_float = op.is_float_op();
            let is_aligned: bool = addr % sz == 0;
            if !is_aligned {
                assert!([2, 4, 8].contains(&sz));
                if is_store {
                    let mut rd = if !is_float {
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
                            8 => {}
                            _ => unreachable!(),
                        }
                    }
                    if !is_float {
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
            crate::task::perf::record_user_unaligned_trap(unaligned_start, is_store, sz, is_float);
        }
        Trap::MachineError(_) | Trap::Unknown | Trap::Exception(Exception::AddressError) | _ => {
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

fn read_bp() {
    println!(
        "[trap_handler] {:?}\n\
                 [trap_handler] {:?}\n\
                 [trap_handler] {:?}\n\
                 [trap_handler] {:?}\n\
                 [trap_handler] {:?}\n\
                 [trap_handler] {:?}\n\
                 [trap_handler] {:?}\n\
                 [trap_handler] {:?}\n\
                 [trap_handler] {:?}\n\
                 [trap_handler] {:?}",
        PrMd::read(),
        TLBRERA::read(),
        TLBRBadV::read(),
        TLBRPrMd::read(),
        TLBREHi::read(),
        TLBRELo0::read(),
        TLBRELo1::read(),
        PGD::read(),
        PWCL::read(),
        PWCH::read()
    );
    let cause = get_exception_cause();
    let stval = get_bad_addr();
    let badi = get_bad_instruction();
    panic!(
        "[trap_handler] {:?}, stval = {:#x}, BadI = {:#x}!",
        cause, stval, badi
    );
}
#[no_mangle]
pub fn trap_return() -> ! {
    #[cfg(all(feature = "board_2k1000", feature = "board_bringup_trace"))]
    let trace_first_return =
        !BOARD_FIRST_TRAP_RETURN.swap(true, core::sync::atomic::Ordering::Relaxed);
    #[cfg(all(feature = "board_2k1000", feature = "board_bringup_trace"))]
    if trace_first_return {
        println!("[bringup][user:01] first task reached trap_return");
    }
    // 当前任务的 trap frame 已完整且业务锁已经释放；timer callback 与
    // RESCHEDULE 只允许在这里让出 CPU，不能从任意 hard IRQ 位置切换。
    crate::task::run_task_safe_point();
    let task = do_signal();
    #[cfg(all(feature = "board_2k1000", feature = "board_bringup_trace"))]
    if trace_first_return {
        println!("[bringup][user:02] initial signal check complete");
    }
    set_user_trap_entry();
    let (trap_cx_ptr, _trap_pc, _trap_sp) = {
        let inner = task.acquire_inner_lock();
        let trap_cx = inner.get_trap_cx();
        // Refresh after signal/exec context changes and on every future migration:
        // the CPU performing this return owns the pointer installed on next trap.
        trap_cx.kernel_cpu_local = crate::hal::cpu_local_ptr();
        trap_cx.sstatus.set_pplv(3).set_pie(true);
        (
            trap_cx as *const TrapContext as usize,
            trap_cx.gp.pc,
            trap_cx.gp.sp,
        )
    };
    // 页表根、ASID 和 CPU 驻留登记必须来自同一个 AddressSpace 快照；不能再从
    // TCB 单独读取 ASID，否则 CLONE_VM 线程会破坏共享 MM 的标签一致性。
    let user_vm = task.process.activate_user_vm();
    if user_vm.asid != 0 {
        crate::task::perf::record_tlb_activate();
    }
    // On LA64, `strampoline` resolves to the kernel-trap stub under the
    // static link. `__restore` is already in the direct-map executable range.
    let restore_va = __restore as usize;
    // 下方恢复汇编不返回，Rust 不会为 trap 栈帧运行析构。
    // current 槽仍是任务 owner，因此在跳转前释放这个临时 Arc。
    drop(task);
    #[cfg(all(feature = "board_2k1000", feature = "board_bringup_trace"))]
    if trace_first_return {
        println!(
            "[bringup][user:03] entering PLV3: pc={:#x} sp={:#x} trap_cx={:#x} token={:#x} asid={} restore={:#x}",
            _trap_pc,
            _trap_sp,
            trap_cx_ptr,
            user_vm.token,
            user_vm.asid,
            restore_va
        );
    }
    unsafe {
        // trap context、页表根和 ASID 是 `__restore` 的固定 ABI 参数，必须直接绑定
        // 到 $a0/$a1/$a2。若先把多个 `in(reg)` 输入逐个 move 到参数寄存器，LLVM
        // 可以让后面的输入复用前面的目标寄存器；前一条 move 随后会覆盖尚未读取的
        // 输入，最终把错误的 ASID 交给汇编恢复入口。
        asm!(
            "ibar 0",
            "jr {restore}",
            restore = in(reg) restore_va,
            in("$a0") trap_cx_ptr,
            in("$a1") user_vm.token,
            in("$a2") user_vm.asid as usize,
            options(noreturn)
        );
    }
}

/// The KERNEL SPACE trap handler
/// 内核空间Trap处理程序
/// # ERA
/// The ERA kept "as-is" in the `__kern_trap` (See `trap.S`) after this function call.
/// If modification to `ERA` is needed, this should be taken into account.
#[no_mangle]
pub extern "C" fn trap_from_kernel(gr: &mut GeneralRegs) {
    // 获取Trap原因
    let cause = get_exception_cause();
    match cause {
        Trap::Interrupt(Interrupt::IPI) => {
            // IPI fast path 必须早于 BADV/console 诊断：中断不会更新 BADV，
            // 陈旧地址可能误触发栈溢出打印。
            handle_ipi_interrupt();
            return;
        }
        Trap::Interrupt(Interrupt::Timer) => {
            // timer fast path 同样必须早于 BADV 和普通异常诊断，并且只发布
            // deferred 状态；任意内核位置都不会在这里 context switch。
            handle_timer_interrupt();
            return;
        }
        _ => {}
    }
    // 读取异常子代码（二级编号）
    let sub_code = EStat::read().exception_sub_code();
    let bad_addr = get_bad_addr();
    if let Some(slot) = super::kernel_stack_guard_slot(bad_addr) {
        println!(
            "[kernel] kernel stack overflow: slot={}, bad addr={:#x}",
            slot, bad_addr
        );
    }
    // 模式匹配Trap原因并进行处理
    match cause {
        // TLB重填
        Trap::TLBReFill => {
            println!(
                "[trap_handler] {:?}\n\
                 [trap_handler] {:?}\n\
                 [trap_handler] {:?}\n\
                 [trap_handler] {:?}\n\
                 [trap_handler] {:?}\n\
                 [trap_handler] {:?}\n\
                 [trap_handler] {:?}",
                CrMd::read(),
                TLBRERA::read(),
                TLBRBadV::read(),
                TLBRPrMd::read(),
                PGD::read(),
                PWCL::read(),
                PWCH::read()
            );
        }
        // 地址未对齐
        Trap::Exception(Exception::AddressNotAligned) => {
            let pc = gr.pc;
            // 获取当前指令ins和操作码op
            let ins = Instruction::from(gr.pc as *const Instruction);
            let op = match ins.get_op_code() {
                Ok(op) => op,
                Err(_) => panic!(
                    "Failed to execute the command. Bad Instruction: {}, PC:{}",
                    unsafe { *(gr.pc as *const u32) },
                    pc
                ),
            };
            let addr = BadV::read().get_vaddr();
            //debug!("{:#x}: {:?}, {:#x}", pc, op, addr);
            let sz = op.get_size();
            let is_aligned: bool = addr % sz == 0;
            if is_aligned {
                panic!(
                    "Failed to execute the command. Bad Instruction: {}, PC:{}",
                    unsafe { *(gr.pc as *const u32) },
                    pc
                );
            }
            assert!([2, 4, 8].contains(&sz));
            if op.is_store() {
                let mut rd = gr[ins.get_rd_num()];
                for i in 0..sz {
                    unsafe { ((addr + i) as *mut u8).write_unaligned(rd as u8) };
                    rd >>= 8;
                }
            } else {
                let mut rd = 0;
                for i in (0..sz).rev() {
                    rd <<= 8;
                    let read_byte = (unsafe { ((addr + i) as *mut u8).read_unaligned() } as usize);
                    rd |= read_byte;
                    //debug!("{:#x}, {:#x}", rd, read_byte);
                }
                if !op.is_unsigned_ld() {
                    match sz {
                        2 => rd = (rd as u16) as i16 as isize as usize,
                        4 => rd = (rd as u32) as i32 as isize as usize,
                        8 => {}
                        _ => unreachable!(),
                    }
                }
                gr[ins.get_rd_num()] = rd;
            }
            gr.pc += 4;
            if gr.pc == pc {
                panic!(
                    "Failed to execute the command. Bad Instruction: {}, PC:{}",
                    unsafe { *(gr.pc as *const u32) },
                    pc
                );
            }
            //debug!("{:?}", gr);
            return;
        }
        // Xein Add This
        _ => {
            println!("Unhandled Trap Cause!!!");
        }
    }
    panic!(
        "a trap {:?} from kernel! bad addr = {:#x}, bad instruction = {:#x}, pc:{:#x}, (subcode:{}), PGDH: {:?}, PGDL: {:?}, {}",
        cause,
        bad_addr,
        get_bad_instruction(),
        get_bad_ins_addr(),
        sub_code,
        PGDH::read(),
        PGDL::read(),
        if let Trap::Exception(ty) = cause {
            match ty {
                Exception::AddressError => match sub_code {
                    0 => "Address error Exception for Fetching instructions",
                    1 => "Address error Exception for Memory access instructions",
                    _ => "Unknown",
                },
                _ => "",
            }
        } else {
            ""
        }
    );
}
