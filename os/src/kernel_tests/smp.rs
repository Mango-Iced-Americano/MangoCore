//! SMP 启动阶段的 focused ktest。

use alloc::{
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::sync::atomic::{AtomicUsize, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

use crate::kernel_tests::runner::KernelTest;

// 这些编号是 RV64/LA64 共用的 Linux generic syscall ABI。
// probe 只依赖 CPU/task 基础 syscall，不在 AP 上进入 FS、net 或设备路径。
const USER_PROBE_GETCPU: usize = 168;
const USER_PROBE_GETPID: usize = 172;
const USER_PROBE_SETAFFINITY: usize = 122;
const USER_PROBE_GETAFFINITY: usize = 123;
const USER_PROBE_EXIT: usize = 93;

#[cfg(target_arch = "riscv64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.smp_user_probe, "a"
    .balign 4
    .global __smp_user_probe_start
    .global __smp_user_probe_end
__smp_user_probe_start:
    addi a7, zero, {getpid}
    ecall
    addi s1, a0, 0
    sltiu s0, a0, 1

    # 低 8 字节保存 affinity word，高 8 字节供 getcpu 使用。
    addi sp, sp, -16
    sd zero, 0(sp)
    addi a0, s1, 0
    addi a1, zero, 8
    addi a2, sp, 0
    addi a7, zero, {getaffinity}
    ecall
    addi t1, zero, 8
    bne a0, t1, .Lsmp_probe_fail
    ld t0, 0(sp)
    addi t1, zero, 3
    bne t0, t1, .Lsmp_probe_fail

    addi a0, sp, 8
    addi a1, zero, 0
    addi a2, zero, 0
    addi a7, zero, {getcpu}
    ecall
    .global __smp_user_probe_resched_ready
__smp_user_probe_resched_ready:
    bnez a0, .Lsmp_probe_fail
    lw t0, 8(sp)
    bnez t0, .Lsmp_probe_fail

    # AP 只会在上面的 CPU0 getcpu 已完成后发送 RESCHEDULE。若返回安全点
    # 没有消费请求，本循环会一直停在 CPU0，最终由内核测试超时报错。
.Lsmp_probe_wait_cpu1:
    addi a0, sp, 8
    addi a1, zero, 0
    addi a2, zero, 0
    addi a7, zero, {getcpu}
    ecall
    bnez a0, .Lsmp_probe_fail
    lw t0, 8(sp)
    beqz t0, .Lsmp_probe_wait_cpu1
    addi t1, zero, 1
    bne t0, t1, .Lsmp_probe_fail

    # IPI 驱动迁移后，pid=0 仍须返回同一线程的 0b11。
    sd zero, 0(sp)
    addi a0, zero, 0
    addi a1, zero, 8
    addi a2, sp, 0
    addi a7, zero, {getaffinity}
    ecall
    addi t1, zero, 8
    bne a0, t1, .Lsmp_probe_fail
    ld t0, 0(sp)
    addi t1, zero, 3
    bne t0, t1, .Lsmp_probe_fail

    # 从 CPU1 把当前线程收紧到 CPU0。syscall 若只改 mask 而没有在安全点
    # 完成迁移，下面的 getcpu 仍会读到 1，从而拒绝假阳性。
    addi t0, zero, 1
    sd t0, 0(sp)
    addi a0, zero, 0
    addi a1, zero, 8
    addi a2, sp, 0
    addi a7, zero, {setaffinity}
    ecall
    bnez a0, .Lsmp_probe_fail

    addi a0, sp, 8
    addi a1, zero, 0
    addi a2, zero, 0
    addi a7, zero, {getcpu}
    ecall
    bnez a0, .Lsmp_probe_fail
    lw t0, 8(sp)
    bnez t0, .Lsmp_probe_fail

    # syscall 返回后，持久 affinity 也必须只剩 CPU0。
    sd zero, 0(sp)
    addi a0, zero, 0
    addi a1, zero, 8
    addi a2, sp, 0
    addi a7, zero, {getaffinity}
    ecall
    addi t1, zero, 8
    bne a0, t1, .Lsmp_probe_fail
    ld t0, 0(sp)
    addi t1, zero, 1
    bne t0, t1, .Lsmp_probe_fail

    addi a0, s0, 0
    j .Lsmp_probe_exit
.Lsmp_probe_fail:
    addi a0, zero, 1
.Lsmp_probe_exit:
    addi a7, zero, {exit_syscall}
    ecall
.Lsmp_probe_hang:
    j .Lsmp_probe_hang
__smp_user_probe_end:
    .popsection
"#,
    getcpu = const USER_PROBE_GETCPU,
    getpid = const USER_PROBE_GETPID,
    setaffinity = const USER_PROBE_SETAFFINITY,
    getaffinity = const USER_PROBE_GETAFFINITY,
    exit_syscall = const USER_PROBE_EXIT,
);

#[cfg(target_arch = "loongarch64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.smp_user_probe, "a"
    .balign 4
    .global __smp_user_probe_start
    .global __smp_user_probe_end
__smp_user_probe_start:
    addi.d $a7, $zero, {getpid}
    syscall 0
    move $s1, $a0
    sltui $s0, $a0, 1

    # 低 8 字节保存 affinity word，高 8 字节供 getcpu 使用。
    addi.d $sp, $sp, -16
    st.d $zero, $sp, 0
    move $a0, $s1
    addi.d $a1, $zero, 8
    move $a2, $sp
    addi.d $a7, $zero, {getaffinity}
    syscall 0
    addi.d $t1, $zero, 8
    beq $a0, $t1, 1f
    b .Lsmp_probe_fail
1:
    ld.d $t0, $sp, 0
    addi.d $t1, $zero, 3
    beq $t0, $t1, 2f
    b .Lsmp_probe_fail
2:
    addi.d $a0, $sp, 8
    move $a1, $zero
    move $a2, $zero
    addi.d $a7, $zero, {getcpu}
    syscall 0
    .global __smp_user_probe_resched_ready
__smp_user_probe_resched_ready:
    beqz $a0, 3f
    b .Lsmp_probe_fail
3:
    ld.w $t0, $sp, 8
    beqz $t0, 4f
    b .Lsmp_probe_fail
4:
    # AP 只会在上面的 CPU0 getcpu 已完成后发送 RESCHEDULE。若返回安全点
    # 没有消费请求，本循环会一直停在 CPU0，最终由内核测试超时报错。
.Lsmp_probe_wait_cpu1:
    addi.d $a0, $sp, 8
    move $a1, $zero
    move $a2, $zero
    addi.d $a7, $zero, {getcpu}
    syscall 0
    beqz $a0, 5f
    b .Lsmp_probe_fail
5:
    ld.w $t0, $sp, 8
    beqz $t0, .Lsmp_probe_wait_cpu1
    addi.d $t1, $zero, 1
    beq $t0, $t1, 6f
    b .Lsmp_probe_fail
6:
    # IPI 驱动迁移后，pid=0 仍须返回同一线程的 0b11。
    st.d $zero, $sp, 0
    move $a0, $zero
    addi.d $a1, $zero, 8
    move $a2, $sp
    addi.d $a7, $zero, {getaffinity}
    syscall 0
    addi.d $t1, $zero, 8
    beq $a0, $t1, 7f
    b .Lsmp_probe_fail
7:
    ld.d $t0, $sp, 0
    addi.d $t1, $zero, 3
    beq $t0, $t1, 8f
    b .Lsmp_probe_fail
8:
    # 从 CPU1 把当前线程收紧到 CPU0。返回位置与最终 mask 分别验证
    # “发生了迁移”和“affinity 已持久发布”。
    addi.d $t0, $zero, 1
    st.d $t0, $sp, 0
    move $a0, $zero
    addi.d $a1, $zero, 8
    move $a2, $sp
    addi.d $a7, $zero, {setaffinity}
    syscall 0
    beqz $a0, .Lsmp_probe_setaff_ok
    b .Lsmp_probe_fail
.Lsmp_probe_setaff_ok:
    addi.d $a0, $sp, 8
    move $a1, $zero
    move $a2, $zero
    addi.d $a7, $zero, {getcpu}
    syscall 0
    beqz $a0, .Lsmp_probe_getcpu0_ok
    b .Lsmp_probe_fail
.Lsmp_probe_getcpu0_ok:
    ld.w $t0, $sp, 8
    beqz $t0, .Lsmp_probe_cpu0_ok
    b .Lsmp_probe_fail
.Lsmp_probe_cpu0_ok:
    st.d $zero, $sp, 0
    move $a0, $zero
    addi.d $a1, $zero, 8
    move $a2, $sp
    addi.d $a7, $zero, {getaffinity}
    syscall 0
    addi.d $t1, $zero, 8
    beq $a0, $t1, .Lsmp_probe_getaffinity0_ok
    b .Lsmp_probe_fail
.Lsmp_probe_getaffinity0_ok:
    ld.d $t0, $sp, 0
    addi.d $t1, $zero, 1
    beq $t0, $t1, .Lsmp_probe_affinity0_ok
    b .Lsmp_probe_fail
.Lsmp_probe_affinity0_ok:
    move $a0, $s0
    b .Lsmp_probe_exit
.Lsmp_probe_fail:
    addi.d $a0, $zero, 1
.Lsmp_probe_exit:
    addi.d $a7, $zero, {exit_syscall}
    syscall 0
.Lsmp_probe_hang:
    b .Lsmp_probe_hang
__smp_user_probe_end:
    .popsection
"#,
    getcpu = const USER_PROBE_GETCPU,
    getpid = const USER_PROBE_GETPID,
    setaffinity = const USER_PROBE_SETAFFINITY,
    getaffinity = const USER_PROBE_GETAFFINITY,
    exit_syscall = const USER_PROBE_EXIT,
);

extern "C" {
    static __smp_user_probe_start: u8;
    static __smp_user_probe_end: u8;
    static __smp_user_probe_resched_ready: u8;
}

const IRQ_PROBE_NOT_RUN: usize = 0;
const IRQ_PROBE_DISABLED: usize = 1;
const IRQ_PROBE_ENABLED: usize = 2;
static IDLE_TO_TASK_IRQ_PROBE: AtomicUsize = AtomicUsize::new(IRQ_PROBE_NOT_RUN);
static SCHED_STATE_HELPER_RUNS: AtomicUsize = AtomicUsize::new(0);
static AP_TASK_ERRORS: AtomicUsize = AtomicUsize::new(0);
static AP_TASK_RUNS: [AtomicUsize; crate::smp::MAX_CPUS] =
    [const { AtomicUsize::new(0) }; crate::smp::MAX_CPUS];
static AP_BLOCKED_WAKE_ERRORS: AtomicUsize = AtomicUsize::new(0);
static AP_BLOCKED_WAKE_PHASE: [AtomicUsize; crate::smp::MAX_CPUS] =
    [const { AtomicUsize::new(0) }; crate::smp::MAX_CPUS];
static AP_KSTACK_RECLAIM_RUNS: AtomicUsize = AtomicUsize::new(0);
static AP_KSTACK_RECLAIM_ERRORS: AtomicUsize = AtomicUsize::new(0);
static AP_USER_TLB_RETIRE_PHASE: AtomicUsize = AtomicUsize::new(0);
static AP_USER_TLB_FREE_DURING_WAIT: AtomicUsize = AtomicUsize::new(usize::MAX);
static AP_USER_TLB_REQUEST_BEFORE: AtomicUsize = AtomicUsize::new(0);
static AP_SHARED_MM_ASID: AtomicUsize = AtomicUsize::new(0);
static AP_SHARED_MM_ASID_READY: AtomicUsize = AtomicUsize::new(0);
static PAGE_SYNC_START: AtomicUsize = AtomicUsize::new(0);
static PAGE_SYNC_READY: AtomicUsize = AtomicUsize::new(0);
static PAGE_SYNC_DONE: AtomicUsize = AtomicUsize::new(0);
static PAGE_SYNC_ERRORS: AtomicUsize = AtomicUsize::new(0);
const USER_RESCHED_WAITING: usize = 0;
const USER_RESCHED_SENT: usize = 1;
const USER_RESCHED_TARGET_LOST: usize = 2;
const USER_RESCHED_TIMEOUT: usize = 3;
const USER_RESCHED_SEND_FAILED: usize = 4;
static USER_RESCHED_RESULT: AtomicUsize = AtomicUsize::new(USER_RESCHED_WAITING);

lazy_static! {
    static ref SCHED_STATE_COMPLETION: Mutex<Option<Arc<crate::task::Completion>>> =
        Mutex::new(None);
    static ref AP_BLOCKED_WAKE_COMPLETION: Mutex<Option<Arc<crate::task::Completion>>> =
        Mutex::new(None);
    static ref USER_TLB_RETIRE_VM: Mutex<Option<Arc<crate::mm::AddressSpace<crate::hal::PageTableImpl>>>> =
        Mutex::new(None);
    static ref SHARED_TLB_VM: Mutex<Option<Arc<crate::mm::AddressSpace<crate::hal::PageTableImpl>>>> =
        Mutex::new(None);
    /// CPU1 helper 只在测试期间持有 Weak，不延长用户 TCB 生命周期。
    static ref USER_RESCHED_TARGET: Mutex<Option<(Weak<crate::task::TaskControlBlock>, usize)>> =
        Mutex::new(None);
}

/// 返回只依赖 Phase 1 启动不变量的测试集合。
pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new(
            "smp::configured_cpus_are_online",
            configured_cpus_are_online,
        ),
        KernelTest::new(
            "smp::ktest_runner_stays_on_boot_cpu",
            ktest_runner_stays_on_boot_cpu,
        ),
        KernelTest::new(
            "smp::configured_cpus_enter_scheduler",
            configured_cpus_enter_scheduler,
        ),
        KernelTest::new(
            "smp::secondary_cpus_enter_idle_context",
            secondary_cpus_enter_idle_context,
        ),
        KernelTest::new("smp::bsp_to_ap_ipi_ping", bsp_to_ap_ipi_ping),
        KernelTest::new(
            "smp::bsp_broadcasts_ipi_to_all_aps",
            bsp_broadcasts_ipi_to_all_aps,
        ),
        KernelTest::new(
            "smp::kernel_timer_irq_is_deferred",
            kernel_timer_irq_is_deferred,
        ),
        KernelTest::new("smp::ap_to_bsp_ipi_round_trip", ap_to_bsp_ipi_round_trip),
        KernelTest::new(
            "smp::syscall_irq_window_survives_schedule",
            syscall_irq_window_survives_schedule,
        ),
        KernelTest::new(
            "smp::scheduler_state_has_unique_owner",
            scheduler_state_has_unique_owner,
        ),
        KernelTest::new(
            "smp::remote_kernel_tasks_run_on_target_cpus",
            remote_kernel_tasks_run_on_target_cpus,
        ),
        KernelTest::new(
            "smp::blocked_kernel_tasks_wake_on_last_cpu",
            blocked_kernel_tasks_wake_on_last_cpu,
        ),
        KernelTest::new(
            "smp::user_tlb_full_flush_reaches_online_cpus",
            user_tlb_full_flush_reaches_online_cpus,
        ),
        KernelTest::new("smp::address_space_owns_asid", address_space_owns_asid),
        KernelTest::new(
            "smp::asid_rollover_flushes_before_reuse",
            asid_rollover_flushes_before_reuse,
        ),
        KernelTest::new(
            "smp::user_tlb_page_sync_uses_arch_backend",
            user_tlb_page_sync_uses_arch_backend,
        ),
        KernelTest::new(
            "smp::concurrent_page_shootdowns_keep_payloads_separate",
            concurrent_page_shootdowns_keep_payloads_separate,
        ),
        KernelTest::new(
            "smp::user_tlb_retirement_waits_for_ack",
            user_tlb_retirement_waits_for_ack,
        ),
        KernelTest::new(
            "smp::kernel_stack_reclaim_waits_for_shootdown",
            kernel_stack_reclaim_waits_for_shootdown,
        ),
        KernelTest::new(
            "smp::user_task_reschedules_and_sets_affinity",
            user_task_reschedules_and_sets_affinity,
        ),
        KernelTest::terminal(
            "smp::secondary_cpus_stop_and_ack",
            secondary_cpus_stop_and_ack,
        ),
    ]
}

/// 取得链接进内核的双架构用户 probe 指令流。
fn user_probe_program() -> &'static [u8] {
    // Safety: 两个符号由上方同一个汇编 section 定义，end 紧跟在
    // probe 末尾；链接后地址稳定，且返回切片只读。
    unsafe {
        let start = core::ptr::addr_of!(__smp_user_probe_start) as usize;
        let end = core::ptr::addr_of!(__smp_user_probe_end) as usize;
        core::slice::from_raw_parts(start as *const u8, end - start)
    }
}

/// 返回“CPU0 首次完成 getcpu”标签在 probe 内的偏移。
fn user_probe_resched_offset() -> usize {
    let start = core::ptr::addr_of!(__smp_user_probe_start) as usize;
    let end = core::ptr::addr_of!(__smp_user_probe_end) as usize;
    let ready = core::ptr::addr_of!(__smp_user_probe_resched_ready) as usize;
    assert!(
        (start..end).contains(&ready),
        "user reschedule label is outside the probe section"
    );
    ready - start
}

/// 构造完整用户 TCB，再把极小 probe 放入新的匿名映射。
///
/// `/init` 只在 CPU0 上作为现有用户 ABI/stack/trap-context 脚手架被解析；
/// 它的入口不会执行，fd 也会在任务对 AP 可见前关闭。
fn build_user_probe_task() -> Result<(Arc<crate::task::TaskControlBlock>, usize), &'static str> {
    let inode = crate::fs::vfs_lookup_absolute("/init")
        .or_else(|_| crate::fs::vfs_lookup_absolute("/initproc"))
        .map_err(|_| "ktest initramfs has no user ELF scaffold")?;
    let elf = crate::fs::vfs::File::new(inode, crate::fs::vfs::FileFlags::O_RDONLY)
        .map_err(|_| "failed to open user ELF scaffold")?;
    let task = crate::task::TaskControlBlock::new(elf);
    task.process.close_files_on_exit();

    let program = user_probe_program();
    if program.is_empty() || program.len() > crate::config::PAGE_SIZE {
        return Err("user probe does not fit in one page");
    }
    let entry = task.process.vm().write(|space| {
        let entry = space.mmap(
            0,
            crate::config::PAGE_SIZE,
            crate::mm::MapPermission::R | crate::mm::MapPermission::W | crate::mm::MapPermission::U,
            crate::mm::MapFlags::MAP_PRIVATE | crate::mm::MapFlags::MAP_ANONYMOUS,
            0,
            None,
            true,
            false,
        );
        if entry < 0 {
            return Err("failed to reserve anonymous user probe page");
        }
        let entry = entry as usize;
        let pa = space
            .fault_in_user_va(
                crate::mm::VirtAddr::from(entry),
                crate::mm::FaultAccess::Store,
            )
            .map_err(|_| "failed to populate anonymous user probe page")?;
        let offset = pa.page_offset();
        if offset + program.len() > crate::config::PAGE_SIZE {
            return Err("user probe crossed its mapped page");
        }
        pa.floor().get_bytes_array()[offset..offset + program.len()].copy_from_slice(program);
        // 只在装载指令时开放写权限；任务发布前收紧为 RX，避免测试把 W+X
        // 映射带到 AP，也顺带覆盖正式的 mprotect/PTE 提交流程。
        space
            .mprotect(
                entry,
                crate::config::PAGE_SIZE,
                crate::mm::MapPermission::R
                    | crate::mm::MapPermission::X
                    | crate::mm::MapPermission::U,
            )
            .map_err(|_| "failed to protect user probe code page")?;
        Ok(entry)
    })?;
    task.acquire_inner_lock().get_trap_cx().gp.pc = entry;
    Ok((task, entry + user_probe_resched_offset()))
}

/// CPU1 等到用户任务确实完成 CPU0 getcpu 后，才发送生产 RESCHEDULE IPI。
fn request_user_reschedule_from_ap() {
    let Some((weak_task, ready_pc)) = USER_RESCHED_TARGET.lock().take() else {
        USER_RESCHED_RESULT.store(USER_RESCHED_TARGET_LOST, Ordering::Release);
        return;
    };
    let deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(3));
    loop {
        let Some(task) = weak_task.upgrade() else {
            USER_RESCHED_RESULT.store(USER_RESCHED_TARGET_LOST, Ordering::Release);
            return;
        };
        if task.task_status() == crate::task::TaskStatus::Running(crate::smp::BOOT_CPU_ID) {
            // inner 锁只保护一次 PC 快照，不跨 IPI 发送或等待点。
            let pc = task.acquire_inner_lock().get_trap_cx().gp.pc;
            if pc >= ready_pc {
                drop(task);
                let result = if crate::smp::request_reschedule(crate::smp::BOOT_CPU_ID).is_ok() {
                    USER_RESCHED_SENT
                } else {
                    USER_RESCHED_SEND_FAILED
                };
                USER_RESCHED_RESULT.store(result, Ordering::Release);
                return;
            }
        }
        if crate::hal::get_time() >= deadline {
            USER_RESCHED_RESULT.store(USER_RESCHED_TIMEOUT, Ordering::Release);
            return;
        }
        core::hint::spin_loop();
    }
}

/// 用户任务先由远端 RESCHEDULE 从 CPU0 到 CPU1，再用 syscall 自迁回 CPU0。
fn user_task_reschedules_and_sets_affinity() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("AP user probe setup did not run on CPU0");
    }
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }

    // 前一项 ktest 已经结束；先清空它留下的 TCB，随后出现的 zombie 才能
    // 确定来自本 probe，而不是依赖整个测试套件的隐式执行顺序。
    drop(crate::task::take_zombie_tasks(usize::MAX));
    if crate::task::zombie_queue_count_fast() != 0 {
        return Err("stale zombie task remained before AP user probe");
    }

    let parent_task = crate::task::current_task().ok_or("ktest runner has no current task")?;
    let parent = parent_task.process.clone();
    drop(parent_task);
    let (task, ready_pc) = build_user_probe_task()?;
    let process = task.process.clone();
    let pid = task.pid();
    parent
        .add_child(process.clone())
        .map_err(|_| "failed to attach user probe to ktest runner")?;
    process.set_parent(Some(Arc::downgrade(&parent)));

    let weak_task = Arc::downgrade(&task);
    if task.cpus_allowed() != 1usize << crate::smp::BOOT_CPU_ID {
        return Err("ordinary user task did not start with CPU0-only affinity");
    }
    // New 状态由测试独占；显式放行 CPU0/CPU1 后才能登记一次性目标。
    // migration_target 本身不触发切换；只有 CPU0 返回安全点消费 AP 发来的
    // RESCHEDULE，任务切回 idle 后才会把唯一 owner 交给 CPU1。
    task.set_initial_cpus_allowed((1usize << crate::smp::BOOT_CPU_ID) | (1usize << 1));
    task.request_migration(1);
    // 先消费前序用例可能留下的合并提示，再采样本轮基线。此时 helper 尚未
    // 创建、用户任务也未发布，因此后续 CPU0 计数增量只能来自本轮远端 IPI。
    crate::task::run_task_safe_point();
    let reschedule_before = crate::smp::reschedule_count(crate::smp::BOOT_CPU_ID);
    USER_RESCHED_RESULT.store(USER_RESCHED_WAITING, Ordering::Relaxed);
    let previous = USER_RESCHED_TARGET
        .lock()
        .replace((Arc::downgrade(&task), ready_pc));
    if previous.is_some() {
        return Err("stale user reschedule target remained before probe");
    }
    let helper = crate::task::spawn_ktest_task_on(1, request_user_reschedule_from_ap);
    let weak_helper = Arc::downgrade(&helper);
    crate::task::publish_task_on(task.clone(), crate::smp::BOOT_CPU_ID);
    // runner 在 CPU0 让出一次；此后用户 probe 没有显式 yield，迁移只能由
    // helper 的远端 IPI 经 trap-return 安全点触发。
    crate::task::suspend_current_and_run_next();

    // CPU1 退出时会撤销已经在 CPU0/CPU1 激活过的 MM。runner 若在这里
    // 关中断自旋，CPU0 就无法确认来自 CPU1 的 user-TLB shootdown，双方
    // 会一直等到协议超时。受控窗口只让 CPU0 处理 timer/IPI；不在窗口内
    // 获取普通锁，也不改变生产调度状态。
    crate::hal::with_local_interrupts_enabled(|| {
        let deadline =
            crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(3));
        // Zombie 状态和 CPU1 的 current/runqueue 为空才是稳定的 owner 证据；
        // 全局 zombie 队列会被 CPU0 idle 及时回收，不能要求它保持非空。
        while !process.is_zombie()
            || task.task_status() != crate::task::TaskStatus::Zombie
            || helper.task_status() != crate::task::TaskStatus::Zombie
            || crate::task::processor::cpu_has_current(1)
            || crate::task::run_queue_count(1) != 0
        {
            if crate::hal::get_time() >= deadline {
                return Err("IPI-rescheduled user probe did not quiesce before timeout");
            }
            // B34 会把 probe 从 CPU1 重新排到当前 runner 所在的 CPU0。
            // 这里只开中断仍不足以让出 CPU：IPI handler 按安全点抢占约定
            // 只能发布 need_resched，必须由当前内核任务显式消费后再调度。
            crate::task::run_task_safe_point();
            core::hint::spin_loop();
        }
        Ok(())
    })?;

    match USER_RESCHED_RESULT.load(Ordering::Acquire) {
        USER_RESCHED_SENT => {}
        USER_RESCHED_TARGET_LOST => return Err("AP lost the user reschedule target"),
        USER_RESCHED_TIMEOUT => return Err("AP did not observe the CPU0 user trap"),
        USER_RESCHED_SEND_FAILED => return Err("AP failed to send RESCHEDULE to CPU0"),
        _ => return Err("AP did not finish the user reschedule request"),
    }
    if crate::smp::reschedule_count(crate::smp::BOOT_CPU_ID) <= reschedule_before {
        return Err("CPU0 did not consume the remote RESCHEDULE at a task safe point");
    }
    // probe 内部已验证自己确实先到过 CPU1；最终回到 CPU0 则只能来自
    // sched_setaffinity(0, bit0) 的自迁移安全点。
    if task.last_cpu() != crate::smp::BOOT_CPU_ID {
        return Err("user probe did not return to its new affinity CPU");
    }
    let reaped = crate::task::ProcessManager::wait_child(
        &parent,
        pid as isize,
        true,
        true,
        false,
        false,
        false,
    )
    .map_err(|_| "ktest parent could not reap AP user probe")?
    .ok_or("AP user probe was not waitable")?;
    if reaped.pid != pid || reaped.status != 0 || process.exit_code() != 0 {
        return Err("AP user probe syscall result or exit status was invalid");
    }

    // wait_child 只回收用户进程；helper 是独立 ktest TCB，显式清空 zombie
    // owner 后才能验证两个任务都没有隐藏的强引用。
    drop(crate::task::take_zombie_tasks(usize::MAX));
    drop(process);
    drop(task);
    drop(helper);
    if weak_task.upgrade().is_some() {
        return Err("reaped AP user probe retained a strong TCB owner");
    }
    if weak_helper.upgrade().is_some() {
        return Err("reschedule helper retained a strong TCB owner");
    }
    Ok(())
}

fn read_shared_mm_asid_on_ap() {
    let vm = SHARED_TLB_VM
        .lock()
        .as_ref()
        .expect("shared-ASID test VM missing")
        .clone();
    let context = vm.activate_on(crate::smp::cpu_id());
    AP_SHARED_MM_ASID.store(context.asid as usize, Ordering::Release);
    AP_SHARED_MM_ASID_READY.store(1, Ordering::Release);
}

/// 同一 AddressSpace 在不同 CPU 上必须取得同一个 ASID；ASID 不再属于 TCB。
fn address_space_owns_asid() -> Result<(), &'static str> {
    let vm = Arc::new(crate::mm::AddressSpace::new(
        crate::mm::AddressSpaceInner::<crate::hal::PageTableImpl>::new_bare(),
    ));
    let local = vm.activate_on(crate::smp::BOOT_CPU_ID);
    #[cfg(target_arch = "loongarch64")]
    if local.asid == 0 {
        return Err("LoongArch user MM received reserved ASID 0");
    }
    #[cfg(target_arch = "riscv64")]
    {
        let capacity = crate::hal::arch::riscv::sv39::asid_capacity();
        if capacity > 0 && local.asid == 0 {
            return Err("RISC-V user MM received reserved ASID 0");
        }
        if capacity == 0 && local.asid != 0 {
            return Err("ASIDLEN=0 RISC-V platform received a nonzero ASID");
        }
    }

    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }
    AP_SHARED_MM_ASID.store(0, Ordering::Release);
    AP_SHARED_MM_ASID_READY.store(0, Ordering::Release);
    *SHARED_TLB_VM.lock() = Some(vm);
    let task = crate::task::spawn_ktest_task_on(1, read_shared_mm_asid_on_ap);
    let deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
    while AP_SHARED_MM_ASID_READY.load(Ordering::Acquire) == 0
        || task.task_status() != crate::task::TaskStatus::Zombie
    {
        if crate::hal::get_time() >= deadline {
            return Err("AP did not activate the shared MM before timeout");
        }
        core::hint::spin_loop();
    }
    *SHARED_TLB_VM.lock() = None;
    if AP_SHARED_MM_ASID.load(Ordering::Acquire) != local.asid as usize {
        return Err("one AddressSpace received different ASIDs on two CPUs");
    }
    Ok(())
}

/// 耗尽架构 ASID，验证编号只能在全 CPU flush/ack 完成并换代后复用。
fn asid_rollover_flushes_before_reuse() -> Result<(), &'static str> {
    #[cfg(target_arch = "loongarch64")]
    let capacity = crate::hal::arch::loongarch64::tlb::asid_capacity();
    #[cfg(target_arch = "riscv64")]
    let capacity = crate::hal::arch::riscv::sv39::asid_capacity();
    if capacity == 0 {
        // RISC-V 规范允许 ASIDLEN=0；该平台由 switch-time 全刷保证隔离。
        return Ok(());
    }

    #[cfg(target_arch = "loongarch64")]
    let rollovers_before = crate::hal::arch::loongarch64::tlb::asid_rollover_count();
    #[cfg(target_arch = "riscv64")]
    let rollovers_before = crate::hal::arch::riscv::sv39::asid_rollover_count();
    let remote_request_before = if crate::smp::configured_cpu_count() > 1 {
        crate::smp::user_tlb_request(1)
    } else {
        0
    };
    let first_vm = Arc::new(crate::mm::AddressSpace::new(
        crate::mm::AddressSpaceInner::<crate::hal::PageTableImpl>::new_bare(),
    ));
    if first_vm.activate_on(crate::smp::BOOT_CPU_ID).asid == 0 {
        return Err("first rollover test MM received ASID 0");
    }

    // 直接驱动生产 allocator，避免 RV64 为 65535 个编号反复分配并清零页表根。
    // 首尾仍通过真实 AddressSpace 激活，覆盖 MM-owned context 的换代路径。
    for _ in 0..=capacity {
        #[cfg(target_arch = "loongarch64")]
        let assignment = crate::hal::arch::loongarch64::tlb::try_assign_asid(0);
        #[cfg(target_arch = "riscv64")]
        let assignment = crate::hal::arch::riscv::sv39::try_assign_asid(0);
        if assignment.is_none() {
            #[cfg(target_arch = "loongarch64")]
            crate::hal::arch::loongarch64::tlb::rollover_asids();
            #[cfg(target_arch = "riscv64")]
            crate::hal::arch::riscv::sv39::rollover_asids();
            break;
        }
    }

    #[cfg(target_arch = "loongarch64")]
    let rollovers_after = crate::hal::arch::loongarch64::tlb::asid_rollover_count();
    #[cfg(target_arch = "riscv64")]
    let rollovers_after = crate::hal::arch::riscv::sv39::asid_rollover_count();
    if rollovers_after != rollovers_before + 1 {
        return Err("ASID exhaustion did not complete exactly one epoch rollover");
    }
    if crate::smp::configured_cpu_count() > 1
        && crate::smp::user_tlb_request(1) <= remote_request_before
    {
        return Err("ASID rollover reused IDs without a remote TLB flush");
    }
    if first_vm.activate_on(crate::smp::BOOT_CPU_ID).asid == 0 {
        return Err("old MM did not receive a current-epoch ASID after rollover");
    }
    Ok(())
}

/// 启动函数返回后，配置拓扑中的每个 CPU 都必须已经发布 online。
fn configured_cpus_are_online() -> Result<(), &'static str> {
    let configured = crate::smp::configured_cpu_count();
    let expected = (1usize << configured) - 1;
    let online = crate::smp::online_cpu_mask();

    if online != expected {
        crate::println!(
            "# SMP topology mismatch: configured={} expected={:#x} online={:#x}",
            configured,
            expected,
            online
        );
        return Err("configured CPU set is not fully online");
    }
    Ok(())
}

/// CPU0 逐个唤醒 AP，并等待目标 CPU 在硬中断上下文发布 ack。
fn bsp_to_ap_ipi_ping() -> Result<(), &'static str> {
    let timeout_ticks = crate::hal::get_clock_freq();
    for cpu_id in 1..crate::smp::configured_cpu_count() {
        let expected = match crate::smp::send_ipi_ping(cpu_id) {
            Ok(expected) => expected,
            Err(error) => {
                crate::println!("# SMP IPI send failed: cpu={} error={}", cpu_id, error);
                return Err("failed to send BSP-to-AP IPI");
            }
        };
        let deadline = crate::hal::get_time().saturating_add(timeout_ticks);
        while crate::smp::ipi_ping_ack(cpu_id) != expected {
            if crate::hal::get_time() >= deadline {
                crate::println!(
                    "# SMP IPI ack timeout: cpu={} expected={} observed={}",
                    cpu_id,
                    expected,
                    crate::smp::ipi_ping_ack(cpu_id)
                );
                return Err("AP did not acknowledge IPI");
            }
            core::hint::spin_loop();
        }
    }
    Ok(())
}

/// CPU0 先发布全部 AP 的 mailbox，再连续敲响 doorbell，最后逐项等待 ack。
fn bsp_broadcasts_ipi_to_all_aps() -> Result<(), &'static str> {
    let targets = crate::smp::online_cpu_mask() & !(1usize << crate::smp::BOOT_CPU_ID);
    let mut expected = [0usize; crate::smp::MAX_CPUS];
    for cpu_id in 1..crate::smp::configured_cpu_count() {
        expected[cpu_id] = crate::smp::ipi_ping_ack(cpu_id).wrapping_add(1);
    }

    if let Err(error) = crate::smp::send_ipi_mask(targets, crate::smp::IpiReason::PING) {
        crate::println!(
            "# SMP IPI broadcast failed: targets={:#x} error={}",
            targets,
            error
        );
        return Err("failed to broadcast BSP-to-AP IPI");
    }

    let deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
    for cpu_id in 1..crate::smp::configured_cpu_count() {
        while crate::smp::ipi_ping_ack(cpu_id) != expected[cpu_id] {
            if crate::hal::get_time() >= deadline {
                crate::println!(
                    "# SMP IPI broadcast ack timeout: cpu={} expected={} observed={}",
                    cpu_id,
                    expected[cpu_id],
                    crate::smp::ipi_ping_ack(cpu_id)
                );
                return Err("AP did not acknowledge broadcast IPI");
            }
            core::hint::spin_loop();
        }
    }
    Ok(())
}

/// 验证 timer 硬中断只发布 pending，完整工作只能由显式安全点消费。
fn kernel_timer_irq_is_deferred() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("timer deferred test ran on an AP");
    }

    // ktest kernel task 默认关中断运行。保存原状态后连续做两轮，第二轮同时证明
    // 第一轮安全点已经把 one-shot timer 精确地重新编程。
    let original_irq_state = crate::hal::local_irq_save();
    let tid = crate::task::current_tid();
    let result = deferred_timer_round(tid).and_then(|_| deferred_timer_round(tid));
    crate::hal::local_irq_restore(original_irq_state);
    result
}

/// 在“入口关中断、出口仍关中断”的约束下完成一轮真实 timer IRQ 测试。
fn deferred_timer_round(expected_tid: usize) -> Result<(), &'static str> {
    let cpu_id = crate::smp::cpu_id();
    let irq_before = crate::smp::timer_irq_count(cpu_id);
    let deferred_before = crate::smp::timer_deferred_count(cpu_id);

    // 只在受控窗口打开全局中断；硬 IRQ 返回后仍停在本函数中，不会自动经过
    // trap_return，因此可以直接检查 deferred work 尚未执行。
    crate::hal::local_irq_restore(true);
    let deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
    while crate::smp::timer_irq_count(cpu_id) == irq_before {
        if crate::hal::get_time() >= deadline {
            let _ = crate::hal::local_irq_save();
            return Err("kernel timer interrupt did not arrive");
        }
        core::hint::spin_loop();
    }
    let was_enabled = crate::hal::local_irq_save();
    if !was_enabled {
        return Err("timer test lost its controlled interrupt window");
    }
    if crate::smp::timer_deferred_count(cpu_id) != deferred_before {
        return Err("timer hard IRQ executed deferred work");
    }
    if !crate::smp::local_timer_pending() {
        return Err("timer hard IRQ did not publish pending state");
    }
    if crate::task::current_tid() != expected_tid {
        return Err("timer hard IRQ switched the current task");
    }

    // 生产安全点可能因为 quantum 到期主动调度；恢复运行后必须仍是同一测试
    // 任务，且 pending 已被完整消费。
    crate::task::run_task_safe_point();
    if crate::smp::local_timer_pending() {
        return Err("timer safe point left pending work behind");
    }
    if crate::smp::timer_deferred_count(cpu_id) != deferred_before.wrapping_add(1) {
        return Err("timer safe point did not complete exactly one batch");
    }
    if crate::task::current_tid() != expected_tid {
        return Err("timer safe point resumed a different task");
    }
    Ok(())
}

/// 反复验证 AP hard IRQ → idle deferred reply → CPU0 kernel trap 的完整闭环。
fn ap_to_bsp_ipi_round_trip() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("round-trip test ran on an AP");
    }
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }

    // CPU0 的 kernel task 默认关中断。请求先送到 AP，随后只在受控窗口打开
    // 本地全局中断接收 reply；每轮结束仍保持关中断。
    let original_irq_state = crate::hal::local_irq_save();
    let result = round_trip_all_aps();
    // 受控窗口内可能同时收到 timer hard IRQ；用 B11 的生产安全点收尾，
    // 避免把 quiesced one-shot 留给后续测试或 shutdown。
    crate::task::run_task_safe_point();
    crate::hal::local_irq_restore(original_irq_state);
    result
}

fn round_trip_all_aps() -> Result<(), &'static str> {
    const ROUNDS_PER_AP: usize = 64;

    for cpu_id in 1..crate::smp::configured_cpu_count() {
        let failures_before = crate::smp::ipi_send_failures(cpu_id);
        for round in 0..ROUNDS_PER_AP {
            let expected = match crate::smp::send_ipi_round_trip(cpu_id) {
                Ok(expected) => expected,
                Err(error) => {
                    crate::println!(
                        "# SMP round-trip request failed: cpu={} round={} error={}",
                        cpu_id,
                        round,
                        error
                    );
                    return Err("failed to send round-trip request");
                }
            };

            crate::hal::local_irq_restore(true);
            let deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
            while crate::smp::round_trip_reply_ack() != expected {
                if crate::hal::get_time() >= deadline {
                    let _ = crate::hal::local_irq_save();
                    crate::println!(
                        "# SMP round-trip timeout: cpu={} round={} expected={} observed={} send_failures={}",
                        cpu_id,
                        round,
                        expected,
                        crate::smp::round_trip_reply_ack(),
                        crate::smp::ipi_send_failures(cpu_id)
                    );
                    return Err("AP-to-BSP IPI reply timed out");
                }
                core::hint::spin_loop();
            }
            if !crate::hal::local_irq_save() {
                return Err("round-trip test lost its controlled interrupt window");
            }
        }

        if crate::smp::ipi_send_failures(cpu_id) != failures_before {
            return Err("AP failed to send a deferred IPI reply");
        }
    }
    Ok(())
}

/// 读取并原样恢复本 CPU 的全局中断状态。
fn local_interrupts_enabled() -> bool {
    let enabled = crate::hal::local_irq_save();
    crate::hal::local_irq_restore(enabled);
    enabled
}

/// 这个新任务由 idle scheduler 首次切入，因此可以直接观测 idle
/// 传给任务的硬件中断状态。检查后保持关中断并走正常 ktest exit。
fn probe_idle_to_task_irq_state() {
    let enabled = crate::hal::local_irq_save();
    IDLE_TO_TASK_IRQ_PROBE.store(
        if enabled {
            IRQ_PROBE_ENABLED
        } else {
            IRQ_PROBE_DISABLED
        },
        Ordering::Release,
    );
}

/// 验证 syscall 窗口跨 yield 切换后恢复，idle 不继承开中断状态。
fn syscall_irq_window_survives_schedule() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("syscall IRQ-window test ran on an AP");
    }

    IDLE_TO_TASK_IRQ_PROBE.store(IRQ_PROBE_NOT_RUN, Ordering::Release);
    let original_irq_state = crate::hal::local_irq_save();
    let result = crate::hal::with_local_interrupts_enabled(|| {
        if !local_interrupts_enabled() {
            return Err("controlled syscall window did not enable interrupts");
        }

        // helper 先入队，当前 runner 后入队；FIFO fast path 会先切入
        // helper，使它观测到的正是 runner -> idle -> helper 的状态。
        crate::task::spawn_ktest_task(probe_idle_to_task_irq_state);
        crate::task::suspend_current_and_run_next();

        match IDLE_TO_TASK_IRQ_PROBE.load(Ordering::Acquire) {
            IRQ_PROBE_DISABLED => {}
            IRQ_PROBE_ENABLED => return Err("idle scheduler leaked enabled IRQs into a new task"),
            _ => return Err("idle IRQ-state probe task did not run"),
        }
        if !local_interrupts_enabled() {
            return Err("resumed task did not recover its IRQ window");
        }

        // 窗口恢复后再接收一次真实 AP reply，证明不只是 CSR 位看起来
        // 开启，而是 kernel trap 确实能在该任务上下文中往返。
        receive_one_ap_reply_while_irqs_enabled()
    });

    // helper 正常返回后必须恢复入口快照。先关中断再消费窗口内
    // 可能发布的 timer pending，避免把 one-shot 状态泄漏给下一用例。
    let restored_irq_state = crate::hal::local_irq_save();
    crate::task::run_task_safe_point();
    crate::hal::local_irq_restore(original_irq_state);

    result?;
    if restored_irq_state != original_irq_state {
        return Err("controlled syscall window did not restore entry IRQ state");
    }
    Ok(())
}

fn receive_one_ap_reply_while_irqs_enabled() -> Result<(), &'static str> {
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }
    let expected = crate::smp::send_ipi_round_trip(1)
        .map_err(|_| "failed to request AP reply inside syscall IRQ window")?;
    let deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
    while crate::smp::round_trip_reply_ack() != expected {
        if crate::hal::get_time() >= deadline {
            return Err("AP reply did not interrupt the syscall IRQ window");
        }
        core::hint::spin_loop();
    }
    if !local_interrupts_enabled() {
        return Err("kernel IPI trap returned with syscall IRQ window closed");
    }
    Ok(())
}

/// helper 被选中后确认 Queued -> Running，并通过 Completion 唤醒 blocked runner。
fn complete_scheduler_state_probe() {
    let task = crate::task::current_task().expect("scheduler-state helper has no current task");
    if task.task_status() == crate::task::TaskStatus::Running(crate::smp::BOOT_CPU_ID) {
        SCHED_STATE_HELPER_RUNS.fetch_add(1, Ordering::Release);
    }
    let completion = SCHED_STATE_COMPLETION
        .lock()
        .as_ref()
        .expect("scheduler-state completion missing")
        .clone();
    completion.complete();
}

/// 覆盖任务发布、提前取消阻塞、完整睡眠/唤醒和退出回收，并验证重复 wake
/// 不会改变队列 owner。测试只调用生产入口，不直接伪造原子状态。
fn scheduler_state_has_unique_owner() -> Result<(), &'static str> {
    let runner = crate::task::current_task().ok_or("scheduler-state runner is missing")?;
    if runner.task_status() != crate::task::TaskStatus::Running(crate::smp::BOOT_CPU_ID) {
        return Err("runner does not own CPU0 before scheduler-state test");
    }

    // checked block 会先登记 Blocking，再复查条件。这里故意返回 false，验证
    // 早到 wake 只撤销阻塞意图，任务必须等切回 idle 后才能重新进入 runqueue。
    let mut saw_blocking = false;
    crate::task::block_current_and_run_next_checked(|task| {
        saw_blocking =
            task.task_status() == crate::task::TaskStatus::Blocking(crate::smp::BOOT_CPU_ID);
        false
    });
    if !saw_blocking {
        return Err("checked block did not expose Blocking ownership window");
    }
    if runner.task_status() != crate::task::TaskStatus::Running(crate::smp::BOOT_CPU_ID) {
        return Err("cancelled block did not return runner to CPU0");
    }

    SCHED_STATE_HELPER_RUNS.store(0, Ordering::Release);
    let completion = Arc::new(crate::task::Completion::new());
    *SCHED_STATE_COMPLETION.lock() = Some(completion.clone());
    let cpu0_queued_before = crate::task::run_queue_count(crate::smp::BOOT_CPU_ID);
    let helper = crate::task::spawn_ktest_task(complete_scheduler_state_probe);
    if helper.task_status() != crate::task::TaskStatus::Queued(crate::smp::BOOT_CPU_ID) {
        return Err("new helper did not acquire the CPU0 ready queue");
    }
    if crate::task::run_queue_count(crate::smp::BOOT_CPU_ID) != cpu0_queued_before + 1 {
        return Err("CPU0 runqueue did not gain exactly one helper");
    }
    for cpu in 1..crate::smp::configured_cpu_count() {
        if crate::task::run_queue_count(cpu) != 0 {
            return Err("parked AP unexpectedly owns a runnable task");
        }
    }

    completion.wait_uninterruptible();
    *SCHED_STATE_COMPLETION.lock() = None;
    if SCHED_STATE_HELPER_RUNS.load(Ordering::Acquire) != 1 {
        return Err("scheduler-state helper did not run exactly once");
    }
    if helper.task_status() != crate::task::TaskStatus::Zombie {
        return Err("scheduler-state helper did not reach Zombie");
    }
    if runner.task_status() != crate::task::TaskStatus::Running(crate::smp::BOOT_CPU_ID) {
        return Err("woken runner did not reacquire CPU0 ownership");
    }
    if crate::task::run_queue_count(crate::smp::BOOT_CPU_ID) != cpu0_queued_before {
        return Err("helper lifecycle changed the baseline CPU0 runqueue length");
    }

    // 对已经 Running 的 runner 再发两次 wake；统一入口必须把它们识别为
    // 已唤醒，且不能改变 ready/interruptible 容器。
    let counts_before = crate::task::task_manager_counts();
    crate::task::wake_interruptible(runner.clone());
    crate::task::wake_interruptible(runner.clone());
    if crate::task::task_manager_counts() != counts_before {
        return Err("duplicate wake changed scheduler queue membership");
    }
    if runner.task_status() != crate::task::TaskStatus::Running(crate::smp::BOOT_CPU_ID) {
        return Err("duplicate wake changed the running owner");
    }
    Ok(())
}

/// 在所有可重复测试结束后永久停止 AP，并验证每个目标都发布了 ack。
fn secondary_cpus_stop_and_ack() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("STOP test ran on an AP");
    }

    let targets = crate::smp::online_cpu_mask() & !(1usize << crate::smp::BOOT_CPU_ID);
    if let Err(error) = crate::smp::stop_secondary_cpus() {
        crate::println!(
            "# SMP STOP failed: targets={:#x} error={:?}",
            targets,
            error
        );
        return Err("secondary CPUs did not stop");
    }

    let stopped = crate::smp::stopped_cpu_mask();
    if stopped & targets != targets {
        crate::println!(
            "# SMP STOP ack mismatch: targets={:#x} stopped={:#x}",
            targets,
            stopped
        );
        return Err("STOP returned before every AP acknowledged");
    }

    // 验证生产 shutdown 再次调用同一协议时走幂等快路径。
    crate::smp::stop_secondary_cpus().map_err(|_| "repeated STOP was not idempotent")
}

/// AP 只有在切换到独立 idle stack 后才允许发布 online。
fn secondary_cpus_enter_idle_context() -> Result<(), &'static str> {
    let configured = crate::smp::configured_cpu_count();
    let expected = ((1usize << configured) - 1) & !(1usize << crate::smp::BOOT_CPU_ID);
    let idle = crate::smp::idle_cpu_mask();

    if idle != expected {
        crate::println!(
            "# SMP idle mismatch: configured={} expected={:#x} idle={:#x}",
            configured,
            expected,
            idle
        );
        return Err("secondary CPU did not enter its idle context");
    }
    Ok(())
}

/// 测试 runner 本身仍固定 CPU0，避免 focused test 意外进入用户迁移路径。
fn ktest_runner_stays_on_boot_cpu() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("SMP ktest runner executed on an AP");
    }
    Ok(())
}

/// BSP 返回 scheduler-ready 发布函数前，所有配置 CPU 都必须进入调度循环。
fn configured_cpus_enter_scheduler() -> Result<(), &'static str> {
    let expected = (1usize << crate::smp::configured_cpu_count()) - 1;
    let entered = crate::smp::scheduler_cpu_mask();
    if entered != expected {
        crate::println!(
            "# SMP scheduler mask mismatch: expected={:#x} entered={:#x}",
            expected,
            entered
        );
        return Err("configured CPU set did not enter per-CPU schedulers");
    }
    Ok(())
}

fn record_remote_kernel_task_cpu() {
    let cpu = crate::smp::cpu_id();
    let status_ok = crate::task::current_task()
        .map(|task| task.task_status() == crate::task::TaskStatus::Running(cpu))
        .unwrap_or(false);
    if cpu == crate::smp::BOOT_CPU_ID || !status_ok {
        AP_TASK_ERRORS.fetch_or(1usize << cpu, Ordering::Release);
    }
    AP_TASK_RUNS[cpu].fetch_add(1, Ordering::Release);
}

/// 向每个 AP 的真实 runqueue 发布一个 kernel-only 任务，并验证 target/current 唯一归属。
fn remote_kernel_tasks_run_on_target_cpus() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("remote enqueue test did not run on CPU0");
    }
    AP_TASK_ERRORS.store(0, Ordering::Release);
    for runs in &AP_TASK_RUNS {
        runs.store(0, Ordering::Release);
    }

    let mut tasks = Vec::new();
    for cpu in 1..crate::smp::configured_cpu_count() {
        tasks.push((
            cpu,
            crate::task::spawn_ktest_task_on(cpu, record_remote_kernel_task_cpu),
        ));
    }

    let deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
    loop {
        let finished = tasks.iter().all(|(cpu, task)| {
            AP_TASK_RUNS[*cpu].load(Ordering::Acquire) == 1
                && task.task_status() == crate::task::TaskStatus::Zombie
                && !crate::task::processor::cpu_has_current(*cpu)
        });
        if finished {
            break;
        }
        if crate::hal::get_time() >= deadline {
            return Err("remote kernel task did not finish before timeout");
        }
        core::hint::spin_loop();
    }

    if AP_TASK_ERRORS.load(Ordering::Acquire) != 0 {
        return Err("remote kernel task observed wrong CPU/current owner");
    }
    for cpu in 1..crate::smp::configured_cpu_count() {
        if AP_TASK_RUNS[cpu].load(Ordering::Acquire) != 1 {
            return Err("remote kernel task ran more or less than once");
        }
        if crate::task::run_queue_count(cpu) != 0 {
            return Err("AP runqueue retained a completed kernel task");
        }
    }
    Ok(())
}

/// AP 任务走真实 Completion/WaitQueue 阻塞路径；恢复后必须仍由原 CPU 唯一拥有。
fn wait_for_remote_completion() {
    let origin = crate::smp::cpu_id();
    let completion = AP_BLOCKED_WAKE_COMPLETION
        .lock()
        .as_ref()
        .expect("AP blocked-wake completion missing")
        .clone();
    AP_BLOCKED_WAKE_PHASE[origin].store(1, Ordering::Release);
    completion.wait_uninterruptible();

    let resumed = crate::smp::cpu_id();
    let owner_is_origin = crate::task::current_task()
        .map(|task| task.task_status() == crate::task::TaskStatus::Running(origin))
        .unwrap_or(false);
    if resumed != origin || !owner_is_origin {
        AP_BLOCKED_WAKE_ERRORS.fetch_or(1usize << origin, Ordering::Release);
    }
    AP_BLOCKED_WAKE_PHASE[origin].store(2, Ordering::Release);
}

/// 一次 `Completion::complete()` 批量唤醒所有 AP，覆盖生产 batch wake、
/// `Blocked -> Queued(last_cpu)` 和释放调度锁后广播 RESCHEDULE 的完整链路。
fn blocked_kernel_tasks_wake_on_last_cpu() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("blocked-wake test did not run on CPU0");
    }
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }

    AP_BLOCKED_WAKE_ERRORS.store(0, Ordering::Release);
    for phase in &AP_BLOCKED_WAKE_PHASE {
        phase.store(0, Ordering::Release);
    }
    let completion = Arc::new(crate::task::Completion::new());
    *AP_BLOCKED_WAKE_COMPLETION.lock() = Some(completion.clone());

    let mut tasks = Vec::new();
    for cpu in 1..crate::smp::configured_cpu_count() {
        tasks.push((
            cpu,
            crate::task::spawn_ktest_task_on(cpu, wait_for_remote_completion),
        ));
    }

    let deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
    while !tasks.iter().all(|(cpu, task)| {
        AP_BLOCKED_WAKE_PHASE[*cpu].load(Ordering::Acquire) == 1
            && task.task_status() == crate::task::TaskStatus::Blocked
            && !crate::task::processor::cpu_has_current(*cpu)
            && crate::task::run_queue_count(*cpu) == 0
    }) {
        if crate::hal::get_time() >= deadline {
            return Err("AP tasks did not fully leave their CPUs before wake");
        }
        core::hint::spin_loop();
    }

    if !completion.complete() {
        return Err("first completion did not publish wakeup");
    }
    let wake_deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
    while !tasks.iter().all(|(cpu, task)| {
        AP_BLOCKED_WAKE_PHASE[*cpu].load(Ordering::Acquire) == 2
            && task.task_status() == crate::task::TaskStatus::Zombie
            && !crate::task::processor::cpu_has_current(*cpu)
    }) {
        if crate::hal::get_time() >= wake_deadline {
            return Err("remotely woken AP tasks did not finish before timeout");
        }
        core::hint::spin_loop();
    }

    if AP_BLOCKED_WAKE_ERRORS.load(Ordering::Acquire) != 0 {
        return Err("blocked AP task resumed on the wrong CPU or owner");
    }
    if completion.complete() {
        return Err("duplicate completion attempted a second wakeup");
    }
    *AP_BLOCKED_WAKE_COMPLETION.lock() = None;
    Ok(())
}

/// 直接调用生产 user-TLB 同步原语，验证独立 sequence、IPI handler 与 ack 闭环。
///
/// 本用例尚未让用户任务迁移，也不伪装 stale-PTE 证明；它只验收 B22 已完成的
/// 基础设施。真正的 generation race 与 ack 前 frame 生命周期留给锁外 batch 节点。
fn user_tlb_full_flush_reaches_online_cpus() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("user TLB sync test did not run on CPU0");
    }
    let targets = crate::smp::online_cpu_mask() & !crate::smp::stopped_cpu_mask();
    let mut ack_before = [0usize; crate::smp::MAX_CPUS];
    for cpu in 1..crate::smp::configured_cpu_count() {
        ack_before[cpu] = crate::smp::user_tlb_ack(cpu);
    }

    crate::smp::synchronize_user_tlb(targets, 0, None).map_err(|error| {
        crate::println!("# user TLB full-flush sync failed: {:?}", error);
        "user TLB full-flush sync failed"
    })?;
    for cpu in 1..crate::smp::configured_cpu_count() {
        if crate::smp::user_tlb_ack(cpu) <= ack_before[cpu] {
            return Err("an online AP did not acknowledge the user TLB flush");
        }
    }

    // 同步等待临时开放过本地 IRQ；ktest 不经过用户 trap-return，因此显式走
    // 已有任务安全点，避免把恰好到达的 one-shot timer pending 留给下一用例。
    crate::task::run_task_safe_point();
    Ok(())
}

/// 页级同步在 RV64 由 RFENCE 完成，在 LA64 由固定槽传递目标 ASID/VPN。
fn user_tlb_page_sync_uses_arch_backend() -> Result<(), &'static str> {
    let vm = crate::mm::AddressSpace::new(
        crate::mm::AddressSpaceInner::<crate::hal::PageTableImpl>::new_bare(),
    );
    let asid = vm.activate_on(crate::smp::BOOT_CPU_ID).asid;
    let mut targets = 1usize << crate::smp::BOOT_CPU_ID;
    if crate::smp::configured_cpu_count() > 1 {
        // 只选择逻辑 CPU0/1；当 cold-boot hart 非 0 时，物理 mask 不再碰巧等于
        // 逻辑 mask，从而让 focused 运行真正经过逆映射分支。
        targets |= 1usize << 1;
    }
    targets &= crate::smp::online_cpu_mask() & !crate::smp::stopped_cpu_mask();
    let request_before = if crate::smp::configured_cpu_count() > 1 {
        crate::smp::user_tlb_request(1)
    } else {
        0
    };

    crate::smp::synchronize_user_tlb(
        targets,
        asid,
        Some(crate::mm::VirtAddr::from(0x51_0000).floor()),
    )
    .map_err(|error| {
        crate::println!("# user TLB page sync failed: {:?}", error);
        "user TLB page sync failed"
    })?;

    if crate::smp::configured_cpu_count() > 1 && crate::smp::user_tlb_request(1) != request_before {
        return Err("page sync unexpectedly degraded to a full user-TLB flush");
    }
    crate::task::run_task_safe_point();
    Ok(())
}

fn run_concurrent_page_shootdown() {
    let cpu_id = crate::smp::cpu_id();
    let vm = SHARED_TLB_VM
        .lock()
        .as_ref()
        .expect("concurrent page-shootdown VM missing")
        .clone();
    let asid = vm.activate_on(cpu_id).asid;
    PAGE_SYNC_READY.fetch_add(1, Ordering::Release);
    while PAGE_SYNC_START.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }

    let targets = crate::smp::online_cpu_mask() & !crate::smp::stopped_cpu_mask();
    // 每个发起者选择不同的 LoongArch 双页 TLB entry，避免硬件对齐后碰巧合并。
    let vpn = crate::mm::VirtAddr::from(0x54_0000 + cpu_id * 2 * crate::config::PAGE_SIZE).floor();
    if crate::smp::synchronize_user_tlb(targets, asid, Some(vpn)).is_err() {
        PAGE_SYNC_ERRORS.fetch_add(1, Ordering::Release);
    }
    PAGE_SYNC_DONE.fetch_add(1, Ordering::Release);
}

/// 所有 CPU 同时发布不同 ASID/VPN payload，证明固定槽不会被 reason 合并覆盖。
fn concurrent_page_shootdowns_keep_payloads_separate() -> Result<(), &'static str> {
    PAGE_SYNC_START.store(0, Ordering::Release);
    PAGE_SYNC_READY.store(0, Ordering::Release);
    PAGE_SYNC_DONE.store(0, Ordering::Release);
    PAGE_SYNC_ERRORS.store(0, Ordering::Release);

    let vm = Arc::new(crate::mm::AddressSpace::new(
        crate::mm::AddressSpaceInner::<crate::hal::PageTableImpl>::new_bare(),
    ));
    let local_asid = vm.activate_on(crate::smp::BOOT_CPU_ID).asid;
    *SHARED_TLB_VM.lock() = Some(vm);

    let mut full_requests = [0usize; crate::smp::MAX_CPUS];
    for cpu_id in 0..crate::smp::configured_cpu_count() {
        full_requests[cpu_id] = crate::smp::user_tlb_request(cpu_id);
    }
    let mut tasks = Vec::new();
    for cpu_id in 1..crate::smp::configured_cpu_count() {
        tasks.push(crate::task::spawn_ktest_task_on(
            cpu_id,
            run_concurrent_page_shootdown,
        ));
    }

    let deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
    while PAGE_SYNC_READY.load(Ordering::Acquire) != tasks.len() {
        if crate::hal::get_time() >= deadline {
            return Err("APs did not enter the concurrent page-shootdown barrier");
        }
        core::hint::spin_loop();
    }

    PAGE_SYNC_START.store(1, Ordering::Release);
    let targets = crate::smp::online_cpu_mask() & !crate::smp::stopped_cpu_mask();
    let local_vpn = crate::mm::VirtAddr::from(0x53_0000).floor();
    crate::smp::synchronize_user_tlb(targets, local_asid, Some(local_vpn))
        .map_err(|_| "CPU0 concurrent page shootdown failed")?;

    let completion_deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
    while PAGE_SYNC_DONE.load(Ordering::Acquire) != tasks.len()
        || tasks
            .iter()
            .any(|task| task.task_status() != crate::task::TaskStatus::Zombie)
    {
        if crate::hal::get_time() >= completion_deadline {
            return Err("concurrent page shootdowns did not finish before timeout");
        }
        core::hint::spin_loop();
    }
    *SHARED_TLB_VM.lock() = None;

    if PAGE_SYNC_ERRORS.load(Ordering::Acquire) != 0 {
        return Err("an AP page shootdown returned an error");
    }
    for cpu_id in 0..crate::smp::configured_cpu_count() {
        if crate::smp::user_tlb_request(cpu_id) != full_requests[cpu_id] {
            return Err("concurrent page shootdown degraded to full user-TLB flush");
        }
    }
    crate::task::run_task_safe_point();
    Ok(())
}

fn observe_user_tlb_retirement_window() {
    let cpu = crate::smp::cpu_id();
    if cpu != 1 {
        AP_USER_TLB_RETIRE_PHASE.store(usize::MAX, Ordering::Release);
        return;
    }
    let vm = USER_TLB_RETIRE_VM
        .lock()
        .as_ref()
        .expect("user TLB retirement VM missing")
        .clone();
    vm.activate_on(cpu);
    let request_before = crate::smp::user_tlb_request(cpu);
    AP_USER_TLB_REQUEST_BEFORE.store(request_before, Ordering::Release);
    AP_USER_TLB_RETIRE_PHASE.store(1, Ordering::Release);

    // ktest kernel task 默认关中断运行：request 增加后 handler 尚不可能 ack，
    // 因而这里正好位于“PTE 已清除、远端 flush 未完成”的窗口。
    let deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
    while crate::smp::user_tlb_request(cpu) == request_before {
        if crate::hal::get_time() >= deadline {
            AP_USER_TLB_RETIRE_PHASE.store(usize::MAX, Ordering::Release);
            return;
        }
        core::hint::spin_loop();
    }
    AP_USER_TLB_FREE_DURING_WAIT.store(crate::mm::unallocated_frames(), Ordering::Release);
    AP_USER_TLB_RETIRE_PHASE.store(2, Ordering::Release);

    crate::hal::with_local_interrupts_enabled(|| {
        while AP_USER_TLB_RETIRE_PHASE.load(Ordering::Acquire) != 3 {
            core::hint::spin_loop();
        }
    });
    AP_USER_TLB_RETIRE_PHASE.store(4, Ordering::Release);
}

/// 用真实共享地址空间撤映射证明：request 已发布但 AP 尚未 ack 时，
/// 数据 frame 仍未回到分配器；`write()` 返回后它才完成退休。
fn user_tlb_retirement_waits_for_ack() -> Result<(), &'static str> {
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }
    const TEST_BASE: usize = 0x52_0000;

    AP_USER_TLB_RETIRE_PHASE.store(0, Ordering::Release);
    AP_USER_TLB_FREE_DURING_WAIT.store(usize::MAX, Ordering::Release);
    let mut space = crate::mm::AddressSpaceInner::<crate::hal::PageTableImpl>::new_bare();
    // 两个不同 VPN 会让 MmuGather 升级为 Full，确保本用例仍专门验证软件
    // IPI 的可观测 ack 窗口；单页 RFENCE 由前一用例独立覆盖。
    space.insert_framed_area(
        crate::mm::VirtAddr::from(TEST_BASE),
        crate::mm::VirtAddr::from(TEST_BASE + 2 * crate::config::PAGE_SIZE),
        crate::mm::MapPermission::R | crate::mm::MapPermission::W | crate::mm::MapPermission::U,
    );
    let vm = Arc::new(crate::mm::AddressSpace::new(space));
    vm.activate_on(crate::smp::BOOT_CPU_ID);
    *USER_TLB_RETIRE_VM.lock() = Some(vm.clone());
    let task = crate::task::spawn_ktest_task_on(1, observe_user_tlb_retirement_window);

    let ready_deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
    while AP_USER_TLB_RETIRE_PHASE.load(Ordering::Acquire) != 1 {
        if AP_USER_TLB_RETIRE_PHASE.load(Ordering::Acquire) == usize::MAX
            || crate::hal::get_time() >= ready_deadline
        {
            return Err("AP did not enter user TLB retirement window");
        }
        core::hint::spin_loop();
    }

    let free_before = crate::mm::unallocated_frames();
    let ack_before = crate::smp::user_tlb_ack(1);
    vm.write(|space| {
        space
            .remove_area_with_start_vpn(crate::mm::VirtAddr::from(TEST_BASE).floor())
            .expect("user TLB retirement test unmap failed");
    });

    let free_during = AP_USER_TLB_FREE_DURING_WAIT.load(Ordering::Acquire);
    let free_after = crate::mm::unallocated_frames();
    let validation_error = if free_during != free_before {
        Some("user frame was released before remote TLB ack")
    } else if free_after != free_before.saturating_add(2) {
        Some("user frames were not released after remote TLB ack")
    } else if crate::smp::user_tlb_ack(1) <= ack_before
        || crate::smp::user_tlb_request(1) <= AP_USER_TLB_REQUEST_BEFORE.load(Ordering::Acquire)
    {
        Some("user TLB retirement did not complete a new request/ack")
    } else {
        None
    };

    AP_USER_TLB_RETIRE_PHASE.store(3, Ordering::Release);
    let done_deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
    while task.task_status() != crate::task::TaskStatus::Zombie
        || AP_USER_TLB_RETIRE_PHASE.load(Ordering::Acquire) != 4
    {
        if crate::hal::get_time() >= done_deadline {
            return Err("AP retirement observer did not finish");
        }
        core::hint::spin_loop();
    }
    *USER_TLB_RETIRE_VM.lock() = None;
    crate::task::run_task_safe_point();
    validation_error.map_or(Ok(()), Err)
}

fn record_kstack_reclaim_task() {
    let cpu = crate::smp::cpu_id();
    let owner_ok = crate::task::current_task()
        .map(|task| task.task_status() == crate::task::TaskStatus::Running(cpu))
        .unwrap_or(false);
    if cpu == crate::smp::BOOT_CPU_ID || !owner_ok {
        AP_KSTACK_RECLAIM_ERRORS.fetch_add(1, Ordering::Release);
    }
    AP_KSTACK_RECLAIM_RUNS.fetch_add(1, Ordering::Release);
}

/// 在同一 AP 上执行一轮超过内核栈缓存容量的任务；等 AP current 已清空后，
/// 由仍在 CPU0 运行的测试任务显式析构这些“其它任务”的 zombie TCB。
fn run_kstack_reclaim_wave() -> Result<(), &'static str> {
    let task_count = crate::hal::KERNEL_STACK_CACHE_LIMIT + 1;
    AP_KSTACK_RECLAIM_RUNS.store(0, Ordering::Release);
    AP_KSTACK_RECLAIM_ERRORS.store(0, Ordering::Release);

    let mut tasks = Vec::with_capacity(task_count);
    for _ in 0..task_count {
        tasks.push(crate::task::spawn_ktest_task_on(
            1,
            record_kstack_reclaim_task,
        ));
    }

    let deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(5));
    while AP_KSTACK_RECLAIM_RUNS.load(Ordering::Acquire) != task_count
        || tasks
            .iter()
            .any(|task| task.task_status() != crate::task::TaskStatus::Zombie)
        || crate::task::processor::cpu_has_current(1)
        || crate::task::run_queue_count(1) != 0
        || crate::task::zombie_queue_count_fast() < task_count
    {
        if crate::hal::get_time() >= deadline {
            return Err("AP kernel-stack reclaim wave did not quiesce");
        }
        core::hint::spin_loop();
    }
    if AP_KSTACK_RECLAIM_ERRORS.load(Ordering::Acquire) != 0 {
        return Err("kernel-stack reclaim task observed wrong CPU owner");
    }

    let weak_tasks: Vec<_> = tasks.iter().map(Arc::downgrade).collect();
    drop(tasks);
    let zombies = crate::task::take_zombie_tasks(task_count);
    if zombies.len() != task_count {
        return Err("zombie queue did not transfer the complete reclaim wave");
    }
    drop(zombies);
    if crate::hal::reclaim_retired_kernel_stacks(usize::MAX) == 0 {
        return Err("kernel-stack cache overflow did not queue a retirement");
    }
    if weak_tasks.iter().any(|task| task.upgrade().is_some()) {
        return Err("reclaimed kernel task still has a strong TCB owner");
    }
    Ok(())
}

/// 第一轮强制让缓存溢出并撤销至少一个 AP 使用过的 stack mapping；第二轮
/// 随即耗尽缓存并重新映射回收 slot，验证 shootdown 后的真实复用闭环。
fn kernel_stack_reclaim_waits_for_shootdown() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("kernel-stack reclaim test did not run on CPU0");
    }
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }

    let stale = crate::task::take_zombie_tasks(usize::MAX);
    drop(stale);
    if crate::task::zombie_queue_count_fast() != 0 {
        return Err("zombie queue was not empty before kernel-stack reclaim test");
    }

    let mut ack_before = [0usize; crate::smp::MAX_CPUS];
    for cpu in 1..crate::smp::configured_cpu_count() {
        ack_before[cpu] = crate::smp::kernel_tlb_ack(cpu);
    }
    run_kstack_reclaim_wave()?;
    for cpu in 1..crate::smp::configured_cpu_count() {
        if crate::smp::kernel_tlb_ack(cpu) <= ack_before[cpu] {
            return Err("kernel-stack retirement missed an online AP shootdown");
        }
    }
    run_kstack_reclaim_wave()?;
    // ktest runner 不会像 syscall 一样返回 trap-return；上面的 shootdown 等待
    // 会临时开中断，若期间接住 one-shot timer，必须在离开用例前通过生产
    // 安全点消费 pending 并重新编程，否则下一轮 timer 用例会继承静默状态。
    crate::task::run_task_safe_point();
    Ok(())
}
