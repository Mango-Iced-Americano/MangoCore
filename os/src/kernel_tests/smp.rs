//! SMP 启动阶段的 focused ktest。

use alloc::{
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::sync::atomic::{fence, AtomicUsize, Ordering};
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

// 该用户程序没有 syscall、yield 或数据 load/store；进入后只能由硬件中断返回内核。
// 测试把同 CPU helper 排在它之后，因此 helper 能运行就直接证明了 timer 抢占。
#[cfg(target_arch = "riscv64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.smp_timer_probe, "a"
    .balign 4
    .global __smp_timer_probe_start
    .global __smp_timer_probe_end
__smp_timer_probe_start:
1:
    j 1b
__smp_timer_probe_end:
    .popsection
"#
);

#[cfg(target_arch = "loongarch64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.smp_timer_probe, "a"
    .balign 4
    .global __smp_timer_probe_start
    .global __smp_timer_probe_end
__smp_timer_probe_start:
1:
    b 1b
__smp_timer_probe_end:
    .popsection
"#
);

// 用户探针只做普通访存，不通过 syscall/yield 重新进入内核。a0/a1 分别是
// 待换页与进度页，a2/a3/a4 是原页、CoW 页和重映射页的 canary。前三个标记
// 证明 CPU1 的 load 越过 CoW 与 munmap/remap；标记 5 由 CPU0 在 mprotect 返回后
// 发布，此后的 store 必须触发 SIGSEGV，不得执行到失败标记 4。
#[cfg(target_arch = "riscv64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.smp_stale_tlb_probe, "a"
    .balign 4
    .global __smp_stale_tlb_probe_start
    .global __smp_stale_tlb_probe_end
__smp_stale_tlb_probe_start:
    ld t0, 0(a0)
    bne t0, a2, .Lstale_tlb_fail
    fence rw, rw
    addi t1, zero, 1
    sd t1, 0(a1)
.Lstale_tlb_wait:
    ld t0, 0(a0)
    beq t0, a2, .Lstale_tlb_wait
    bne t0, a3, .Lstale_tlb_fail
    fence rw, rw
    addi t1, zero, 2
    sd t1, 0(a1)
.Lstale_tlb_remap_wait:
    ld t0, 0(a0)
    beq t0, a3, .Lstale_tlb_remap_wait
    bne t0, a4, .Lstale_tlb_fail
    # 先在旧 RW 权限下执行一次 store，排除只验证过可读 TLB 的假阳性。
    sd a4, 0(a0)
    fence rw, rw
    addi t1, zero, 3
    sd t1, 0(a1)
.Lstale_tlb_protect_wait:
    ld t1, 0(a1)
    addi t2, zero, 5
    bne t1, t2, .Lstale_tlb_protect_wait
    # 正确的远程降权应在这条 store 触发 SIGSEGV，后续代码不应执行。
    sd a2, 0(a0)
    fence rw, rw
    addi t1, zero, 4
    sd t1, 0(a1)
    addi a0, zero, 1
    j .Lstale_tlb_exit
.Lstale_tlb_fail:
    fence rw, rw
    addi t1, zero, 4
    sd t1, 0(a1)
    addi a0, zero, 1
.Lstale_tlb_exit:
    addi a7, zero, {exit_syscall}
    ecall
.Lstale_tlb_hang:
    j .Lstale_tlb_hang
__smp_stale_tlb_probe_end:
    .popsection
"#,
    exit_syscall = const USER_PROBE_EXIT,
);

#[cfg(target_arch = "loongarch64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.smp_stale_tlb_probe, "a"
    .balign 4
    .global __smp_stale_tlb_probe_start
    .global __smp_stale_tlb_probe_end
__smp_stale_tlb_probe_start:
    ld.d $t0, $a0, 0
    bne $t0, $a2, .Lstale_tlb_fail
    dbar 0
    addi.d $t1, $zero, 1
    st.d $t1, $a1, 0
.Lstale_tlb_wait:
    ld.d $t0, $a0, 0
    beq $t0, $a2, .Lstale_tlb_wait
    bne $t0, $a3, .Lstale_tlb_fail
    dbar 0
    addi.d $t1, $zero, 2
    st.d $t1, $a1, 0
.Lstale_tlb_remap_wait:
    ld.d $t0, $a0, 0
    beq $t0, $a3, .Lstale_tlb_remap_wait
    bne $t0, $a4, .Lstale_tlb_fail
    # 先在旧 RW 权限下执行一次 store，排除只验证过可读 TLB 的假阳性。
    st.d $a4, $a0, 0
    dbar 0
    addi.d $t1, $zero, 3
    st.d $t1, $a1, 0
.Lstale_tlb_protect_wait:
    ld.d $t1, $a1, 0
    addi.d $t2, $zero, 5
    bne $t1, $t2, .Lstale_tlb_protect_wait
    # 正确的远程降权应在这条 store 触发 SIGSEGV，后续代码不应执行。
    st.d $a2, $a0, 0
    dbar 0
    addi.d $t1, $zero, 4
    st.d $t1, $a1, 0
    addi.d $a0, $zero, 1
    b .Lstale_tlb_exit
.Lstale_tlb_fail:
    dbar 0
    addi.d $t1, $zero, 4
    st.d $t1, $a1, 0
    addi.d $a0, $zero, 1
.Lstale_tlb_exit:
    addi.d $a7, $zero, {exit_syscall}
    syscall 0
.Lstale_tlb_hang:
    b .Lstale_tlb_hang
__smp_stale_tlb_probe_end:
    .popsection
"#,
    exit_syscall = const USER_PROBE_EXIT,
);

extern "C" {
    static __smp_user_probe_start: u8;
    static __smp_user_probe_end: u8;
    static __smp_user_probe_resched_ready: u8;
    static __smp_timer_probe_start: u8;
    static __smp_timer_probe_end: u8;
    static __smp_stale_tlb_probe_start: u8;
    static __smp_stale_tlb_probe_end: u8;
}

const IRQ_PROBE_NOT_RUN: usize = 0;
const IRQ_PROBE_DISABLED: usize = 1;
const IRQ_PROBE_ENABLED: usize = 2;
static IDLE_TO_TASK_IRQ_PROBE: AtomicUsize = AtomicUsize::new(IRQ_PROBE_NOT_RUN);
static SCHED_STATE_HELPER_RUNS: AtomicUsize = AtomicUsize::new(0);
static CPU0_IDLE_WAKE_ERRORS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf_stats")]
static CPU0_IDLE_WAKE_WAIT_BASELINE: AtomicUsize = AtomicUsize::new(0);
static AP_TASK_ERRORS: AtomicUsize = AtomicUsize::new(0);
static AP_TASK_RUNS: [AtomicUsize; crate::smp::MAX_CPUS] =
    [const { AtomicUsize::new(0) }; crate::smp::MAX_CPUS];
static AP_BLOCKED_WAKE_ERRORS: AtomicUsize = AtomicUsize::new(0);
static AP_BLOCKED_WAKE_PHASE: [AtomicUsize; crate::smp::MAX_CPUS] =
    [const { AtomicUsize::new(0) }; crate::smp::MAX_CPUS];
static AP_BLOCKED_WAKE_EXPECTED: [AtomicUsize; crate::smp::MAX_CPUS] =
    [const { AtomicUsize::new(usize::MAX) }; crate::smp::MAX_CPUS];
static QUEUED_AFFINITY_HOLDER_READY: AtomicUsize = AtomicUsize::new(0);
static QUEUED_AFFINITY_HOLDER_RELEASE: AtomicUsize = AtomicUsize::new(0);
static QUEUED_AFFINITY_RUNS: AtomicUsize = AtomicUsize::new(0);
static QUEUED_AFFINITY_RUN_CPU: AtomicUsize = AtomicUsize::new(usize::MAX);
static QUEUED_AFFINITY_ERRORS: AtomicUsize = AtomicUsize::new(0);
static RUNNING_AFFINITY_READY: AtomicUsize = AtomicUsize::new(0);
static RUNNING_AFFINITY_STOP: AtomicUsize = AtomicUsize::new(0);
static RUNNING_AFFINITY_RUNS: AtomicUsize = AtomicUsize::new(0);
static RUNNING_AFFINITY_RUN_CPU: AtomicUsize = AtomicUsize::new(usize::MAX);
static RUNNING_AFFINITY_ERRORS: AtomicUsize = AtomicUsize::new(0);
static STEAL_RUNS: AtomicUsize = AtomicUsize::new(0);
static STEAL_CPU: AtomicUsize = AtomicUsize::new(usize::MAX);
static STEAL_ERRORS: AtomicUsize = AtomicUsize::new(0);
static STEAL_PINNED_RUNS: AtomicUsize = AtomicUsize::new(0);
static STEAL_PINNED_CPU: AtomicUsize = AtomicUsize::new(usize::MAX);
static STEAL_CONTENTION_RELEASE: AtomicUsize = AtomicUsize::new(0);
static STEAL_CONTENTION_ERRORS: AtomicUsize = AtomicUsize::new(0);
static STEAL_CONTENTION_TIDS: [AtomicUsize; crate::smp::MAX_CPUS] =
    [const { AtomicUsize::new(usize::MAX) }; crate::smp::MAX_CPUS];
static STEAL_CONTENTION_RUNS: [AtomicUsize; crate::smp::MAX_CPUS] =
    [const { AtomicUsize::new(0) }; crate::smp::MAX_CPUS];
static STEAL_AFFINITY_RACE_START: AtomicUsize = AtomicUsize::new(0);
static STEAL_AFFINITY_RACE_READY: AtomicUsize = AtomicUsize::new(0);
static STEAL_AFFINITY_RACE_HELPER_DONE: AtomicUsize = AtomicUsize::new(0);
static STEAL_AFFINITY_RACE_HELPER_OK: AtomicUsize = AtomicUsize::new(0);
static STEAL_AFFINITY_RACE_RELEASE: AtomicUsize = AtomicUsize::new(0);
static STEAL_AFFINITY_RACE_RUNS: AtomicUsize = AtomicUsize::new(0);
static STEAL_AFFINITY_RACE_ERRORS: AtomicUsize = AtomicUsize::new(0);
static LOCAL_ZOMBIE_RUNS: AtomicUsize = AtomicUsize::new(0);
static LOCAL_ZOMBIE_CPU: AtomicUsize = AtomicUsize::new(usize::MAX);
static LOCAL_ZOMBIE_ERRORS: AtomicUsize = AtomicUsize::new(0);
static AP_KSTACK_RECLAIM_RUNS: AtomicUsize = AtomicUsize::new(0);
static AP_KSTACK_RECLAIM_ERRORS: AtomicUsize = AtomicUsize::new(0);
static AP_USER_TLB_RETIRE_PHASE: AtomicUsize = AtomicUsize::new(0);
static AP_USER_TLB_FREE_DURING_WAIT: AtomicUsize = AtomicUsize::new(usize::MAX);
static AP_USER_TLB_REQUEST_BEFORE: AtomicUsize = AtomicUsize::new(0);
static AP_SHARED_MM_ASID: AtomicUsize = AtomicUsize::new(0);
static AP_SHARED_MM_ASID_READY: AtomicUsize = AtomicUsize::new(0);
static PTE_UPDATE_START: AtomicUsize = AtomicUsize::new(0);
static PTE_UPDATE_READY: AtomicUsize = AtomicUsize::new(0);
static PTE_UPDATE_DONE: AtomicUsize = AtomicUsize::new(0);
static PTE_UPDATE_ERRORS: AtomicUsize = AtomicUsize::new(0);
static ACTIVE_MM_START: AtomicUsize = AtomicUsize::new(0);
static ACTIVE_MM_PHASE: AtomicUsize = AtomicUsize::new(0);
static ACTIVE_MM_ERRORS: AtomicUsize = AtomicUsize::new(0);
const USER_RESCHED_WAITING: usize = 0;
const USER_RESCHED_SENT: usize = 1;
const USER_RESCHED_TARGET_LOST: usize = 2;
const USER_RESCHED_TIMEOUT: usize = 3;
const USER_RESCHED_SEND_FAILED: usize = 4;
static USER_RESCHED_RESULT: AtomicUsize = AtomicUsize::new(USER_RESCHED_WAITING);
static TIMER_HOLDER_READY: AtomicUsize = AtomicUsize::new(0);
static TIMER_HOLDER_RELEASE: AtomicUsize = AtomicUsize::new(0);
static TIMER_HELPER_RUNS: AtomicUsize = AtomicUsize::new(0);
static TIMER_HELPER_CPU: AtomicUsize = AtomicUsize::new(usize::MAX);
static TIMER_PROBE_ERRORS: AtomicUsize = AtomicUsize::new(0);
static STALE_TLB_HOLDER_READY: AtomicUsize = AtomicUsize::new(0);
static STALE_TLB_HOLDER_RELEASE: AtomicUsize = AtomicUsize::new(0);
static STALE_TLB_TIMER_RESTORED: AtomicUsize = AtomicUsize::new(0);
static STALE_TLB_ERRORS: AtomicUsize = AtomicUsize::new(0);
static STALE_TLB_PROGRESS_PTR: AtomicUsize = AtomicUsize::new(0);
static GROUP_EXIT_START: AtomicUsize = AtomicUsize::new(0);
static GROUP_EXIT_LEADER_READY: AtomicUsize = AtomicUsize::new(0);
static GROUP_EXIT_BLOCKED_READY: AtomicUsize = AtomicUsize::new(0);
static GROUP_EXIT_REMOTE_READY: AtomicUsize = AtomicUsize::new(0);
static GROUP_EXIT_REMOTE_CPU: AtomicUsize = AtomicUsize::new(usize::MAX);
static GROUP_EXIT_LATE_RUNS: AtomicUsize = AtomicUsize::new(0);
static EXEC_START: AtomicUsize = AtomicUsize::new(0);
static EXEC_GATE_CHECKED: AtomicUsize = AtomicUsize::new(0);
static EXEC_OWNER_PHASE: AtomicUsize = AtomicUsize::new(0);
static EXEC_BLOCKED_READY: AtomicUsize = AtomicUsize::new(0);
static EXEC_REMOTE_READY: AtomicUsize = AtomicUsize::new(0);
static EXEC_REMOTE_CPU: AtomicUsize = AtomicUsize::new(usize::MAX);
static EXEC_LATE_RUNS: AtomicUsize = AtomicUsize::new(0);
static EXEC_ERRORS: AtomicUsize = AtomicUsize::new(0);
static EXEC_IDENTITY_LEADER_READY: AtomicUsize = AtomicUsize::new(0);
static EXEC_IDENTITY_PHASE: AtomicUsize = AtomicUsize::new(0);
static EXEC_IDENTITY_CHECKED: AtomicUsize = AtomicUsize::new(0);
static EXEC_IDENTITY_ERRORS: AtomicUsize = AtomicUsize::new(0);
static MEMBARRIER_REMOTE_READY: AtomicUsize = AtomicUsize::new(0);
static MEMBARRIER_REMOTE_RELEASE: AtomicUsize = AtomicUsize::new(0);
const AP_BARRIER_WAITING: usize = 0;
const AP_BARRIER_PASSED: usize = 1;
const AP_BARRIER_FAILED: usize = 2;
static AP_BARRIER_ROUNDS: AtomicUsize = AtomicUsize::new(0);
static AP_BARRIER_RESULT: [AtomicUsize; crate::smp::MAX_CPUS] =
    [const { AtomicUsize::new(AP_BARRIER_WAITING) }; crate::smp::MAX_CPUS];

lazy_static! {
    static ref SCHED_STATE_COMPLETION: Mutex<Option<Arc<crate::task::Completion>>> =
        Mutex::new(None);
    static ref CPU0_IDLE_WAKE_COMPLETION: Mutex<Option<Arc<crate::task::Completion>>> =
        Mutex::new(None);
    static ref CPU0_IDLE_WAKE_TARGET: Mutex<Option<Weak<crate::task::TaskControlBlock>>> =
        Mutex::new(None);
    static ref STEAL_AFFINITY_RACE_TARGET: Mutex<Option<Weak<crate::task::TaskControlBlock>>> =
        Mutex::new(None);
    static ref AP_BLOCKED_WAKE_COMPLETION: Mutex<Option<Arc<crate::task::Completion>>> =
        Mutex::new(None);
    /// B41 用未完成事件把 sibling 固定在真实 killable wait 路径。
    static ref EXEC_BLOCKED_COMPLETION: Mutex<Option<Arc<crate::task::Completion>>> =
        Mutex::new(None);
    static ref USER_TLB_RETIRE_VM: Mutex<Option<Arc<crate::mm::AddressSpace<crate::hal::PageTableImpl>>>> =
        Mutex::new(None);
    static ref SHARED_TLB_VM: Mutex<Option<Arc<crate::mm::AddressSpace<crate::hal::PageTableImpl>>>> =
        Mutex::new(None);
    static ref ACTIVE_MM_COMPLETION: Mutex<Option<Arc<crate::task::Completion>>> =
        Mutex::new(None);
    /// CPU1 helper 只在测试期间持有 Weak，不延长用户 TCB 生命周期。
    static ref USER_RESCHED_TARGET: Mutex<Option<(Weak<crate::task::TaskControlBlock>, usize)>> =
        Mutex::new(None);
    /// helper 只持有 Weak；用户进程的可回收性仍由 wait/reap 验证。
    static ref TIMER_PROBE_TARGET: Mutex<Option<Weak<crate::task::TaskControlBlock>>> =
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
        KernelTest::new(
            "smp::bsp_to_ap_memory_barrier",
            bsp_to_ap_memory_barrier,
        ),
        KernelTest::new(
            "smp::bsp_broadcasts_memory_barrier_to_all_aps",
            bsp_broadcasts_memory_barrier_to_all_aps,
        ),
        KernelTest::new(
            "smp::kernel_timer_irq_is_deferred",
            kernel_timer_irq_is_deferred,
        ),
        KernelTest::new(
            "smp::cpu0_idle_wakes_on_local_timer",
            cpu0_idle_wakes_on_local_timer,
        ),
        KernelTest::new(
            "smp::cpu0_idle_wakes_on_remote_reschedule",
            cpu0_idle_wakes_on_remote_reschedule,
        ),
        KernelTest::new(
            "smp::user_timer_preempts_on_secondary_cpu",
            user_timer_preempts_on_secondary_cpu,
        ),
        KernelTest::new(
            "smp::ap_to_bsp_memory_barrier",
            ap_to_bsp_memory_barrier,
        ),
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
            "smp::blocked_affinity_redirects_wake",
            blocked_affinity_redirects_wake,
        ),
        KernelTest::new(
            "smp::queued_affinity_moves_between_runqueues",
            queued_affinity_moves_between_runqueues,
        ),
        KernelTest::new(
            "smp::running_affinity_waits_for_owner_handoff",
            running_affinity_waits_for_owner_handoff,
        ),
        KernelTest::new("smp::idle_cpu_steals_one_task", idle_cpu_steals_one_task),
        KernelTest::new(
            "smp::pinned_victim_skips_ktlb_sync",
            pinned_victim_skips_ktlb_sync,
        ),
        KernelTest::new(
            "smp::multiple_idle_cpus_compete_for_victim",
            multiple_idle_cpus_compete_for_victim,
        ),
        KernelTest::new(
            "smp::affinity_update_races_with_steal",
            affinity_update_races_with_steal,
        ),
        KernelTest::new(
            "smp::zombie_reclaims_on_owner_idle",
            zombie_reclaims_on_owner_idle,
        ),
        KernelTest::new(
            "smp::inactive_mm_catches_up_on_wake",
            inactive_mm_catches_up_on_wake,
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
            "smp::user_tlb_range_sync_uses_arch_backend",
            user_tlb_range_sync_uses_arch_backend,
        ),
        KernelTest::new(
            "smp::remote_user_pte_updates_take_effect",
            remote_user_pte_updates_take_effect,
        ),
        KernelTest::new(
            "smp::concurrent_pte_updates_keep_shootdowns_separate",
            concurrent_pte_updates_keep_shootdowns_separate,
        ),
        KernelTest::new(
            "smp::user_tlb_retirement_waits_for_ack",
            user_tlb_retirement_waits_for_ack,
        ),
        KernelTest::new(
            "smp::membarrier_reaches_mm_cpus",
            membarrier_reaches_mm_cpus,
        ),
        KernelTest::new(
            "smp::kernel_stack_reclaim_waits_for_shootdown",
            kernel_stack_reclaim_waits_for_shootdown,
        ),
        KernelTest::new(
            "smp::user_task_reschedules_and_sets_affinity",
            user_task_reschedules_and_sets_affinity,
        ),
        KernelTest::new(
            "smp::group_exit_stops_remote_sibling",
            group_exit_stops_remote_sibling,
        ),
        KernelTest::new("smp::exec_stops_remote_sibling", exec_stops_remote_sibling),
        KernelTest::new(
            "smp::exec_does_not_mutate_shared_resources",
            exec_does_not_mutate_shared_resources,
        ),
        KernelTest::new(
            "smp::exec_owner_becomes_group_leader",
            exec_owner_becomes_group_leader,
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

/// 取得完全不进入内核的用户态忙循环。
fn timer_probe_program() -> &'static [u8] {
    // Safety: start/end 由同一个只读汇编 section 定义，且 end 紧跟程序末尾。
    unsafe {
        let start = core::ptr::addr_of!(__smp_timer_probe_start) as usize;
        let end = core::ptr::addr_of!(__smp_timer_probe_end) as usize;
        core::slice::from_raw_parts(start as *const u8, end - start)
    }
}

/// 取得只以普通 load 验证远端旧翻译是否失效的用户程序。
fn stale_tlb_probe_program() -> &'static [u8] {
    // Safety: start/end 由同一个只读汇编 section 定义，end 紧跟程序末尾。
    unsafe {
        let start = core::ptr::addr_of!(__smp_stale_tlb_probe_start) as usize;
        let end = core::ptr::addr_of!(__smp_stale_tlb_probe_end) as usize;
        core::slice::from_raw_parts(start as *const u8, end - start)
    }
}

/// 构造一个已登记 TID、但尚未加入线程组和 runqueue 的 kernel-only sibling。
///
/// 测试故意把构造和发布分开，才能验证 group-exit/exec 门禁对 late clone 的拒绝。
fn build_ktest_sibling(
    process: Arc<crate::task::ProcessControlBlock>,
    cpu: usize,
    entry: fn(),
) -> Arc<crate::task::TaskControlBlock> {
    let tid = crate::task::tid_alloc();
    let kstack = crate::hal::kstack_alloc();
    let task_cx =
        crate::task::TaskContext::goto_address(entry as usize, kstack.get_top());
    let task = crate::task::TaskControlBlock::new_kernel_only(
        tid, process, kstack, task_cx, entry,
    );
    task.set_initial_cpus_allowed(1usize << cpu);
    task
}

fn group_exit_leader() {
    crate::hal::local_irq_restore(true);
    GROUP_EXIT_LEADER_READY.store(1, Ordering::Release);
    while GROUP_EXIT_START.load(Ordering::Acquire) == 0 {
        crate::task::run_task_safe_point();
        core::hint::spin_loop();
    }
    crate::task::exit_group_and_run_next(42 << 8);
}

fn group_exit_remote() {
    crate::hal::local_irq_restore(true);
    GROUP_EXIT_REMOTE_CPU.store(crate::smp::cpu_id(), Ordering::Release);
    GROUP_EXIT_REMOTE_READY.store(1, Ordering::Release);
    loop {
        // 远端只在正式任务安全点观察 group-exit，不调用任何测试专用清理入口。
        crate::task::run_task_safe_point();
        core::hint::spin_loop();
    }
}

fn group_exit_blocked() {
    crate::hal::local_irq_restore(true);
    GROUP_EXIT_BLOCKED_READY.store(1, Ordering::Release);
    crate::task::block_current_and_run_next();
    loop {
        // group-exit 与 Running -> Blocking 交界由生产 sleep 复查负责；
        // 被唤醒后仍只经统一任务安全点完成本线程清理。
        crate::task::run_task_safe_point();
        core::hint::spin_loop();
    }
}

fn group_exit_late_thread() {
    GROUP_EXIT_LATE_RUNS.fetch_add(1, Ordering::Release);
}

/// 验证跨 CPU group-exit 与 clone 最终发布门禁。
fn group_exit_stops_remote_sibling() -> Result<(), &'static str> {
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }
    GROUP_EXIT_START.store(0, Ordering::Relaxed);
    GROUP_EXIT_LEADER_READY.store(0, Ordering::Relaxed);
    GROUP_EXIT_BLOCKED_READY.store(0, Ordering::Relaxed);
    GROUP_EXIT_REMOTE_READY.store(0, Ordering::Relaxed);
    GROUP_EXIT_REMOTE_CPU.store(usize::MAX, Ordering::Relaxed);
    GROUP_EXIT_LATE_RUNS.store(0, Ordering::Relaxed);

    let leader = crate::task::spawn_ktest_task_on(
        crate::smp::BOOT_CPU_ID,
        group_exit_leader,
    );
    let process = leader.process.clone();
    let blocked = build_ktest_sibling(process.clone(), 1, group_exit_blocked);
    crate::task::try_publish_task_on(blocked.clone(), 1)
        .map_err(|_| "failed to publish blocked group-exit sibling")?;
    let remote = build_ktest_sibling(process.clone(), 1, group_exit_remote);
    crate::task::try_publish_task_on(remote.clone(), 1)
        .map_err(|_| "failed to publish remote group-exit sibling")?;
    let late = build_ktest_sibling(process.clone(), 1, group_exit_late_thread);

    let mut ready_timed_out = false;
    crate::hal::with_local_interrupts_enabled(|| {
        let deadline =
            crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(3));
        while GROUP_EXIT_LEADER_READY.load(Ordering::Acquire) == 0
            || GROUP_EXIT_BLOCKED_READY.load(Ordering::Acquire) == 0
            || GROUP_EXIT_REMOTE_READY.load(Ordering::Acquire) == 0
        {
            if crate::hal::get_time() >= deadline {
                ready_timed_out = true;
                break;
            }
            crate::task::run_task_safe_point();
            core::hint::spin_loop();
        }
    });
    if ready_timed_out {
        return Err("group-exit siblings did not start on both CPUs");
    }

    GROUP_EXIT_START.store(1, Ordering::Release);
    let mut exit_timed_out = false;
    crate::hal::with_local_interrupts_enabled(|| {
        let deadline =
            crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(3));
        while !process.is_group_exiting() {
            if crate::hal::get_time() >= deadline {
                exit_timed_out = true;
                return;
            }
            crate::task::run_task_safe_point();
        }

        match crate::task::try_publish_task_on(late.clone(), 1) {
            Err(errno) if errno == crate::syscall::errno::EAGAIN => {}
            _ => exit_timed_out = true,
        }
        // 最后线程先发布 live token=0 和 TCB Zombie，随后才执行进程级
        // finish_exit()。三者必须一起成为完成条件，不能把中间窗口误报成失败。
        while process.live_thread_count() != 0
            || !process.is_zombie()
            || leader.task_status() != crate::task::TaskStatus::Zombie
            || blocked.task_status() != crate::task::TaskStatus::Zombie
            || remote.task_status() != crate::task::TaskStatus::Zombie
        {
            if crate::hal::get_time() >= deadline {
                exit_timed_out = true;
                break;
            }
            crate::task::run_task_safe_point();
            core::hint::spin_loop();
        }
    });

    if exit_timed_out {
        return Err("cross-CPU group exit or late-clone gate timed out");
    }
    if GROUP_EXIT_REMOTE_CPU.load(Ordering::Acquire) != 1
        || remote.last_cpu() != 1
        || blocked.last_cpu() != 1
    {
        return Err("group-exit siblings did not acknowledge on CPU1");
    }
    if !process.is_zombie() {
        return Err("last group-exit ack did not finish process cleanup");
    }
    if process.exit_code() != 42 << 8 {
        return Err("last group-exit ack replaced the shared exit code");
    }
    if late.task_status() != crate::task::TaskStatus::New
        || GROUP_EXIT_LATE_RUNS.load(Ordering::Acquire) != 0
    {
        return Err("late clone crossed the group-exit publication gate");
    }
    Ok(())
}

fn exec_owner() {
    crate::hal::local_irq_restore(true);
    EXEC_OWNER_PHASE.store(1, Ordering::Release);
    while EXEC_START.load(Ordering::Acquire) == 0 {
        crate::task::run_task_safe_point();
    }

    let task = crate::task::current_task().expect("exec ktest owner missing");
    let (exec, siblings) = match task.process.begin_exec(task.gettid()) {
        Ok(session) => session,
        Err(_) => {
            EXEC_ERRORS.fetch_add(1, Ordering::Release);
            EXEC_OWNER_PHASE.store(3, Ordering::Release);
            return;
        }
    };
    crate::task::request_sibling_exit(&siblings, task.gettid());
    drop(siblings);
    EXEC_OWNER_PHASE.store(2, Ordering::Release);

    // 保持 exec gate 打开一个确定窗口，让 CPU0 runner 验证 late clone 必须失败。
    while EXEC_GATE_CHECKED.load(Ordering::Acquire) == 0 {
        crate::task::run_task_safe_point();
    }
    exec.wait();
    if task.process.live_thread_count() != 1 {
        EXEC_ERRORS.fetch_add(1, Ordering::Release);
    }
    exec.finish();
    if task.process.thread_publish_blocked() {
        EXEC_ERRORS.fetch_add(1, Ordering::Release);
    }
    EXEC_OWNER_PHASE.store(3, Ordering::Release);
}

fn exec_remote_sibling() {
    crate::hal::local_irq_restore(true);
    EXEC_REMOTE_CPU.store(crate::smp::cpu_id(), Ordering::Release);
    EXEC_REMOTE_READY.store(1, Ordering::Release);
    loop {
        // 只经过生产安全点观察 exec owner，不调用测试专用退出入口。
        crate::task::run_task_safe_point();
        core::hint::spin_loop();
    }
}

fn exec_blocked_sibling() {
    crate::hal::local_irq_restore(true);
    let completion = EXEC_BLOCKED_COMPLETION
        .lock()
        .as_ref()
        .expect("exec blocked completion missing")
        .clone();
    EXEC_BLOCKED_READY.store(1, Ordering::Release);
    let result = completion.wait_killable();
    drop(completion);
    if !matches!(result, crate::task::WaitResult::Interrupted) {
        EXEC_ERRORS.fetch_add(1, Ordering::Release);
    }
    // killable wait 只负责正常展开等待栈；真正的 Zombie/TLB 清理由生产安全点完成。
    crate::task::run_task_safe_point();
    EXEC_ERRORS.fetch_add(1, Ordering::Release);
    crate::task::exit_current_and_run_next(1);
}

fn exec_late_thread() {
    EXEC_LATE_RUNS.fetch_add(1, Ordering::Release);
}

/// 验证多线程 exec 临时门禁、远端 owner 自清理和最后 sibling completion。
fn exec_stops_remote_sibling() -> Result<(), &'static str> {
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }
    EXEC_START.store(0, Ordering::Relaxed);
    EXEC_GATE_CHECKED.store(0, Ordering::Relaxed);
    EXEC_OWNER_PHASE.store(0, Ordering::Relaxed);
    EXEC_BLOCKED_READY.store(0, Ordering::Relaxed);
    EXEC_REMOTE_READY.store(0, Ordering::Relaxed);
    EXEC_REMOTE_CPU.store(usize::MAX, Ordering::Relaxed);
    EXEC_LATE_RUNS.store(0, Ordering::Relaxed);
    EXEC_ERRORS.store(0, Ordering::Relaxed);
    let previous = EXEC_BLOCKED_COMPLETION
        .lock()
        .replace(Arc::new(crate::task::Completion::new()));
    assert!(previous.is_none(), "stale exec blocked completion");

    let owner = crate::task::spawn_ktest_task_on(crate::smp::BOOT_CPU_ID, exec_owner);
    let process = owner.process.clone();
    let blocked = build_ktest_sibling(process.clone(), 1, exec_blocked_sibling);
    crate::task::try_publish_task_on(blocked.clone(), 1)
        .map_err(|_| "failed to publish blocked exec sibling")?;
    let remote = build_ktest_sibling(process.clone(), 1, exec_remote_sibling);
    crate::task::try_publish_task_on(remote.clone(), 1)
        .map_err(|_| "failed to publish remote exec sibling")?;
    let late = build_ktest_sibling(process.clone(), 1, exec_late_thread);

    let mut timed_out = false;
    crate::hal::with_local_interrupts_enabled(|| {
        let deadline =
            crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(5));
        while EXEC_OWNER_PHASE.load(Ordering::Acquire) != 1
            || EXEC_BLOCKED_READY.load(Ordering::Acquire) == 0
            || EXEC_REMOTE_READY.load(Ordering::Acquire) == 0
        {
            if crate::hal::get_time() >= deadline {
                timed_out = true;
                return;
            }
            crate::task::run_task_safe_point();
        }

        EXEC_START.store(1, Ordering::Release);
        while EXEC_OWNER_PHASE.load(Ordering::Acquire) != 2 {
            if EXEC_OWNER_PHASE.load(Ordering::Acquire) == 3 || crate::hal::get_time() >= deadline {
                timed_out = true;
                break;
            }
            crate::task::run_task_safe_point();
        }

        if !timed_out {
            match crate::task::try_publish_task_on(late.clone(), 1) {
                Err(errno) if errno == crate::syscall::errno::EAGAIN => {}
                _ => {
                    EXEC_ERRORS.fetch_add(1, Ordering::Release);
                }
            }
        }
        EXEC_GATE_CHECKED.store(1, Ordering::Release);

        while !timed_out
            && (EXEC_OWNER_PHASE.load(Ordering::Acquire) != 3
                || owner.task_status() != crate::task::TaskStatus::Zombie
                || blocked.task_status() != crate::task::TaskStatus::Zombie
                || remote.task_status() != crate::task::TaskStatus::Zombie)
        {
            if crate::hal::get_time() >= deadline {
                timed_out = true;
                break;
            }
            crate::task::run_task_safe_point();
        }
    });

    if timed_out {
        if let Some(completion) = EXEC_BLOCKED_COMPLETION.lock().take() {
            completion.complete();
        }
        return Err("cross-CPU exec stop/completion timed out");
    }
    assert!(
        EXEC_BLOCKED_COMPLETION.lock().take().is_some(),
        "exec blocked completion disappeared"
    );
    if EXEC_ERRORS.load(Ordering::Acquire) != 0
        || process.thread_publish_blocked()
        || process.live_thread_count() != 1
    {
        return Err("exec session did not close after sibling acknowledgements");
    }
    if EXEC_REMOTE_CPU.load(Ordering::Acquire) != 1
        || remote.last_cpu() != 1
        || blocked.last_cpu() != 1
    {
        return Err("exec siblings did not clean themselves on CPU1");
    }
    if late.task_status() != crate::task::TaskStatus::New
        || EXEC_LATE_RUNS.load(Ordering::Acquire) != 0
    {
        return Err("late clone crossed the exec publication gate");
    }
    Ok(())
}

/// 验证 exec 只重置当前 PCB 的资源，不修改由其它 PCB 持有的旧共享对象。
fn exec_does_not_mutate_shared_resources() -> Result<(), &'static str> {
    let task = crate::task::current_task().ok_or("exec resource test has no current task")?;
    let process = &task.process;
    let old_files = process.files();
    let old_sighand = process.sighand();
    let old_futex = process.futex();

    let executable = process.exe().lock().clone();
    let cloexec_fd = old_files
        .lock()
        .alloc_fd(executable, true)
        .map_err(|_| "failed to allocate CLOEXEC probe fd")?;
    let mut action = crate::task::SigAction::new();
    action.flags = crate::task::SigActionFlags::SA_RESTART;
    old_sighand.lock().set(10, Some(action));

    process
        .reset_exec_resources()
        .map_err(|_| "failed to reset exec resources")?;

    let new_files = process.files();
    let new_sighand = process.sighand();
    let new_futex = process.futex();
    if Arc::ptr_eq(&old_files, &new_files)
        || Arc::ptr_eq(&old_sighand, &new_sighand)
        || Arc::ptr_eq(&old_futex, &new_futex)
    {
        return Err("exec kept a resource shared with another PCB");
    }
    if old_files.lock().get_file(cloexec_fd).is_err()
        || new_files.lock().get_file(cloexec_fd).is_ok()
    {
        return Err("CLOEXEC close leaked into the old shared fd table");
    }
    if old_sighand.lock().get(10).is_none() || new_sighand.lock().get(10).is_some() {
        return Err("exec signal reset leaked into the old shared sighand");
    }
    Ok(())
}

fn exec_identity_leader() {
    crate::hal::local_irq_restore(true);
    EXEC_IDENTITY_LEADER_READY.store(1, Ordering::Release);
    loop {
        // 非 leader owner 会通过正式 exec 安全点请求本线程退出。
        crate::task::run_task_safe_point();
        core::hint::spin_loop();
    }
}

fn exec_identity_owner() {
    crate::hal::local_irq_restore(true);
    while EXEC_IDENTITY_LEADER_READY.load(Ordering::Acquire) == 0 {
        crate::task::run_task_safe_point();
    }

    let task = crate::task::current_task().expect("exec identity owner missing");
    let old_tid = task.gettid();
    let (exec, siblings) = match task.process.begin_exec(old_tid) {
        Ok(session) => session,
        Err(_) => {
            EXEC_IDENTITY_ERRORS.fetch_add(1, Ordering::Release);
            EXEC_IDENTITY_PHASE.store(2, Ordering::Release);
            // noreturn 调度不会展开当前 Rust 栈，必须先释放本地 current clone。
            drop(task);
            crate::task::zombify_current_and_run_next();
        }
    };
    crate::task::request_sibling_exit(&siblings, old_tid);
    drop(siblings);
    exec.wait();
    if task.process.live_thread_count() != 1 {
        EXEC_IDENTITY_ERRORS.fetch_add(1, Ordering::Release);
    }
    exec.finish();
    task.become_group_leader();

    if task.gettid() != task.pid()
        || crate::task::current_tid() != task.pid()
        || task.exit_signal() != crate::task::Signals::SIGCHLD
    {
        EXEC_IDENTITY_ERRORS.fetch_add(1, Ordering::Release);
    }
    let registry_matches = crate::task::find_task_by_tid(task.pid())
        .map(|registered| Arc::ptr_eq(&registered, &task))
        .unwrap_or(false);
    if !registry_matches || crate::task::find_task_by_tid(old_tid).is_some() {
        EXEC_IDENTITY_ERRORS.fetch_add(1, Ordering::Release);
    }

    // 保持 owner 为 Running，让 CPU0 runner 能验证旧 leader 延迟析构不会删新 PID 键。
    EXEC_IDENTITY_PHASE.store(1, Ordering::Release);
    while EXEC_IDENTITY_CHECKED.load(Ordering::Acquire) == 0 {
        crate::task::run_task_safe_point();
    }
    EXEC_IDENTITY_PHASE.store(2, Ordering::Release);
    // build_ktest_sibling() 直接把本函数作为裸 context 入口，没有外层
    // ktest_trampoline；验证完成后必须显式切走，不能从无返回地址的入口返回。
    // noreturn 切换也不会析构本栈上的 task Arc，因此必须先显式释放。
    drop(task);
    crate::task::zombify_current_and_run_next();
}

/// 验证非 leader exec 接管 PID、registry 与 Per-CPU current TID。
fn exec_owner_becomes_group_leader() -> Result<(), &'static str> {
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }
    EXEC_IDENTITY_LEADER_READY.store(0, Ordering::Relaxed);
    EXEC_IDENTITY_PHASE.store(0, Ordering::Relaxed);
    EXEC_IDENTITY_CHECKED.store(0, Ordering::Relaxed);
    EXEC_IDENTITY_ERRORS.store(0, Ordering::Relaxed);

    let leader = crate::task::spawn_ktest_task_on(1, exec_identity_leader);
    let leader_weak = Arc::downgrade(&leader);
    let process = leader.process.clone();
    let owner = build_ktest_sibling(process.clone(), 0, exec_identity_owner);
    let owner_weak = Arc::downgrade(&owner);
    let old_owner_tid = owner.gettid();
    crate::task::try_publish_task_on(owner.clone(), 0)
        .map_err(|_| "failed to publish non-leader exec owner")?;

    let mut failure = None;
    crate::hal::with_local_interrupts_enabled(|| {
        let deadline =
            crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(5));
        while EXEC_IDENTITY_PHASE.load(Ordering::Acquire) != 1 {
            if EXEC_IDENTITY_PHASE.load(Ordering::Acquire) == 2 {
                failure = Some("non-leader exec owner failed before identity exchange");
                return;
            }
            if crate::hal::get_time() >= deadline {
                failure = Some("non-leader exec owner did not publish exchanged identity");
                return;
            }
            crate::task::run_task_safe_point();
        }

        if owner.gettid() != process.pid
            || leader.gettid() != old_owner_tid
            || !leader.exit_inactive.load(Ordering::Acquire)
        {
            EXEC_IDENTITY_ERRORS.fetch_add(1, Ordering::Release);
        }
        drop(leader);
        // 等到旧 leader 的最后一个强引用真正消失，证明后续 registry 检查
        // 覆盖的是迟到 Drop，而不只是“可能已经回收”的时序猜测。
        while leader_weak.strong_count() != 0 {
            drop(crate::task::take_zombie_tasks(64));
            // 回收动作本身可能刚好释放最后一个调度 Arc；先确认结果再判断
            // deadline，且不要用 Weak::upgrade() 给被观察对象续一轮强引用。
            if leader_weak.strong_count() == 0 {
                break;
            }
            if crate::hal::get_time() >= deadline {
                failure = Some("former exec leader TCB was not reclaimed");
                break;
            }
            crate::task::run_task_safe_point();
        }
        if failure.is_none() {
            let pid_still_points_to_owner = crate::task::find_task_by_tid(process.pid)
                .map(|registered| Arc::ptr_eq(&registered, &owner))
                .unwrap_or(false);
            if !pid_still_points_to_owner
                || crate::task::find_task_by_tid(old_owner_tid).is_some()
            {
                EXEC_IDENTITY_ERRORS.fetch_add(1, Ordering::Release);
            }
        }

        EXEC_IDENTITY_CHECKED.store(1, Ordering::Release);
        while failure.is_none()
            && (EXEC_IDENTITY_PHASE.load(Ordering::Acquire) != 2
                || owner.task_status() != crate::task::TaskStatus::Zombie)
        {
            if crate::hal::get_time() >= deadline {
                failure = Some("new exec leader did not reach zombie state");
                break;
            }
            crate::task::run_task_safe_point();
        }
    });

    if let Some(failure) = failure {
        EXEC_IDENTITY_CHECKED.store(1, Ordering::Release);
        return Err(failure);
    }
    if EXEC_IDENTITY_ERRORS.load(Ordering::Acquire) != 0 {
        return Err("non-leader exec identity exchange broke a TID invariant");
    }
    drop(owner);
    drop(crate::task::take_zombie_tasks(64));
    if owner_weak.upgrade().is_some() {
        return Err("exec owner retained a strong TCB reference after test");
    }
    Ok(())
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

/// 构造完整用户 TCB，再把指定的极小程序放入新的匿名映射。
///
/// `/init` 只在 CPU0 上作为现有用户 ABI/stack/trap-context 脚手架被解析；
/// 它的入口不会执行，fd 也会在任务对 AP 可见前关闭。
fn build_user_task(
    program: &'static [u8],
) -> Result<(Arc<crate::task::TaskControlBlock>, usize), &'static str> {
    let inode = crate::fs::vfs_lookup_absolute("/init")
        .or_else(|_| crate::fs::vfs_lookup_absolute("/initproc"))
        .map_err(|_| "ktest initramfs has no user ELF scaffold")?;
    let elf = crate::fs::vfs::File::new(inode, crate::fs::vfs::FileFlags::O_RDONLY)
        .map_err(|_| "failed to open user ELF scaffold")?;
    let task = crate::task::TaskControlBlock::new(elf);
    task.process.close_files_on_exit();

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
        // Safety: 当前闭包独占测试地址空间，用户探针尚未发布，且范围检查
        // 已证明程序完全位于这个已 fault-in 的物理页内。
        unsafe {
            pa.floor().with_bytes_mut(|page| {
                page[offset..offset + program.len()].copy_from_slice(program)
            });
        }
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
    task.acquire_inner_lock().trap_context_mut().gp.pc = entry;
    Ok((task, entry))
}

fn build_user_probe_task() -> Result<(Arc<crate::task::TaskControlBlock>, usize), &'static str> {
    let (task, entry) = build_user_task(user_probe_program())?;
    Ok((task, entry + user_probe_resched_offset()))
}

/// 占住 CPU1，保证用户忙循环和 helper 能按确定的 FIFO 顺序预先排队。
fn hold_timer_probe_cpu() {
    let initially_enabled = crate::hal::local_irq_save();
    if initially_enabled {
        TIMER_PROBE_ERRORS.fetch_or(1, Ordering::Release);
    }
    crate::hal::local_irq_restore(true);
    TIMER_HOLDER_READY.store(1, Ordering::Release);
    while TIMER_HOLDER_RELEASE.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }
    if !crate::hal::local_irq_save() {
        TIMER_PROBE_ERRORS.fetch_or(2, Ordering::Release);
    }
}

/// 该 helper 只有在用户忙循环被本地 timer 抢占后才可能取得 CPU1。
fn stop_timer_probe() {
    let cpu = crate::smp::cpu_id();
    TIMER_HELPER_CPU.store(cpu, Ordering::Release);
    let Some(task) = TIMER_PROBE_TARGET.lock().take().and_then(|task| task.upgrade()) else {
        TIMER_PROBE_ERRORS.fetch_or(4, Ordering::Release);
        return;
    };
    if cpu != 1 || task.task_status() != crate::task::TaskStatus::Queued(1) {
        TIMER_PROBE_ERRORS.fetch_or(8, Ordering::Release);
    }
    task.acquire_inner_lock()
        .add_signal(crate::task::Signals::SIGKILL);
    TIMER_HELPER_RUNS.fetch_add(1, Ordering::Release);
}

/// CPU1 的用户态忙循环不执行 syscall/yield；只有本地调度 tick 能让 helper 运行。
fn user_timer_preempts_on_secondary_cpu() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("user timer preemption test did not run on CPU0");
    }
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }

    TIMER_HOLDER_READY.store(0, Ordering::Release);
    TIMER_HOLDER_RELEASE.store(0, Ordering::Release);
    TIMER_HELPER_RUNS.store(0, Ordering::Release);
    TIMER_HELPER_CPU.store(usize::MAX, Ordering::Release);
    TIMER_PROBE_ERRORS.store(0, Ordering::Release);
    if TIMER_PROBE_TARGET.lock().is_some() {
        return Err("stale timer probe target remained before test");
    }

    let (task, _) = build_user_task(timer_probe_program())?;
    task.set_initial_cpus_allowed(1usize << 1);

    let timer_irq_before = crate::smp::timer_irq_count(1);
    let timer_deferred_before = crate::smp::timer_deferred_count(1);
    let holder = crate::task::spawn_ktest_task_on(1, hold_timer_probe_cpu);
    let ready_deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
    while TIMER_HOLDER_READY.load(Ordering::Acquire) == 0
        || holder.task_status() != crate::task::TaskStatus::Running(1)
    {
        if crate::hal::get_time() >= ready_deadline {
            TIMER_HOLDER_RELEASE.store(1, Ordering::Release);
            return Err("CPU1 timer holder did not become current");
        }
        core::hint::spin_loop();
    }

    let parent_task = crate::task::current_task().ok_or("ktest runner has no current task")?;
    let parent = parent_task.process.clone();
    drop(parent_task);
    let process = task.process.clone();
    let pid = task.pid();
    if parent.add_child(process.clone()).is_err() {
        TIMER_HOLDER_RELEASE.store(1, Ordering::Release);
        return Err("failed to attach timer probe to ktest runner");
    }
    process.set_parent(Some(Arc::downgrade(&parent)));

    let previous_target = TIMER_PROBE_TARGET.lock().replace(Arc::downgrade(&task));
    assert!(
        previous_target.is_none(),
        "timer probe target changed during setup"
    );
    // holder 仍占有 current，因此两个远程入队 IPI 会先被 idle 安全点消费；
    // FIFO 保证用户任务先运行，helper 不会借入队 IPI 制造抢占假阳性。
    crate::task::publish_task_on(task.clone(), 1);
    let helper = crate::task::spawn_ktest_task_on(1, stop_timer_probe);
    let weak_task = Arc::downgrade(&task);
    let weak_helper = Arc::downgrade(&helper);
    let weak_holder = Arc::downgrade(&holder);
    TIMER_HOLDER_RELEASE.store(1, Ordering::Release);

    let mut timed_out = false;
    crate::hal::with_local_interrupts_enabled(|| {
        let deadline =
            crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(3));
        while !process.is_zombie()
            || task.task_status() != crate::task::TaskStatus::Zombie
            || helper.task_status() != crate::task::TaskStatus::Zombie
            || holder.task_status() != crate::task::TaskStatus::Zombie
            || crate::task::processor::cpu_has_current(1)
            || crate::task::run_queue_count(1) != 0
        {
            if crate::hal::get_time() >= deadline {
                timed_out = true;
                break;
            }
            // CPU0 只处理自身 timer 与来自 CPU1 的 TLB ack；不向 CPU1 发送
            // 调度 IPI，因此成功路径的唯一抢占来源仍是 CPU1 本地 timer。
            crate::task::run_task_safe_point();
            core::hint::spin_loop();
        }
    });

    if timed_out {
        // 失败路径才允许 IPI 介入，以便结束无限用户循环，不污染后续用例。
        task.acquire_inner_lock()
            .add_signal(crate::task::Signals::SIGKILL);
        let _ = crate::smp::request_reschedule(1);
        let cleanup_deadline =
            crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
        while (!process.is_zombie()
            || task.task_status() != crate::task::TaskStatus::Zombie
            || helper.task_status() != crate::task::TaskStatus::Zombie
            || holder.task_status() != crate::task::TaskStatus::Zombie
            || crate::task::processor::cpu_has_current(1)
            || crate::task::run_queue_count(1) != 0)
            && crate::hal::get_time() < cleanup_deadline
        {
            crate::hal::with_local_interrupts_enabled(core::hint::spin_loop);
        }
        if !process.is_zombie() || crate::task::processor::cpu_has_current(1) {
            return Err("timer probe cleanup did not quiesce CPU1");
        }
    }

    let evidence_ok = TIMER_HELPER_RUNS.load(Ordering::Acquire) == 1
        && TIMER_HELPER_CPU.load(Ordering::Acquire) == 1
        && TIMER_PROBE_ERRORS.load(Ordering::Acquire) == 0
        && crate::smp::timer_irq_count(1) > timer_irq_before
        && crate::smp::timer_deferred_count(1) > timer_deferred_before;
    if !timed_out
        && (!evidence_ok
            || task.last_cpu() != 1
            || crate::task::run_queue_count(1) != 0)
    {
        // 先完成统一 reap/drop，再向 runner 报告证据缺失，避免失败污染后续用例。
        TIMER_PROBE_ERRORS.fetch_or(16, Ordering::Release);
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
    .map_err(|_| "ktest parent could not reap timer probe")?
    .ok_or("timer probe was not waitable")?;
    if reaped.pid != pid {
        return Err("timer probe reaped the wrong child");
    }
    drop(crate::task::take_zombie_tasks(usize::MAX));
    drop(process);
    drop(task);
    drop(helper);
    drop(holder);
    let _ = TIMER_PROBE_TARGET.lock().take();
    if weak_task.upgrade().is_some()
        || weak_helper.upgrade().is_some()
        || weak_holder.upgrade().is_some()
    {
        return Err("timer preemption probe retained a strong TCB owner");
    }
    if timed_out {
        return Err("CPU1 user busy loop was not preempted by its local timer");
    }
    if TIMER_PROBE_ERRORS.load(Ordering::Acquire) != 0 {
        return Err("CPU1 timer preemption evidence was incomplete");
    }
    Ok(())
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
            let pc = task.acquire_inner_lock().trap_context_mut().gp.pc;
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
    let vm = crate::mm::AddressSpace::new(
        crate::mm::AddressSpaceInner::<crate::hal::PageTableImpl>::new_bare(),
    );
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
    let first_vm = crate::mm::AddressSpace::new(
        crate::mm::AddressSpaceInner::<crate::hal::PageTableImpl>::new_bare(),
    );
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

/// CPU0 逐个要求 AP 执行正式的 membarrier fence，并核对 request/ack。
fn bsp_to_ap_memory_barrier() -> Result<(), &'static str> {
    for cpu_id in 1..crate::smp::configured_cpu_count() {
        let before = crate::smp::memory_barrier_request(cpu_id);
        crate::smp::synchronize_memory(1usize << cpu_id)
            .map_err(|_| "failed to synchronize BSP-to-AP membarrier")?;
        let request = crate::smp::memory_barrier_request(cpu_id);
        if request != before.wrapping_add(1)
            || crate::smp::memory_barrier_ack(cpu_id) < request
        {
            return Err("AP did not acknowledge the production membarrier request");
        }
    }
    // 同步等待窗口可能接收本地 timer IRQ；在退出用例前消费正式 deferred work。
    crate::task::run_task_safe_point();
    Ok(())
}

/// CPU0 用一次正式 membarrier 广播覆盖全部在线 AP。
fn bsp_broadcasts_memory_barrier_to_all_aps() -> Result<(), &'static str> {
    let targets = crate::smp::online_cpu_mask();
    let mut before = [0usize; crate::smp::MAX_CPUS];
    for cpu_id in 1..crate::smp::configured_cpu_count() {
        before[cpu_id] = crate::smp::memory_barrier_request(cpu_id);
    }

    crate::smp::synchronize_memory(targets)
        .map_err(|_| "failed to broadcast the production membarrier")?;
    for cpu_id in 1..crate::smp::configured_cpu_count() {
        let request = crate::smp::memory_barrier_request(cpu_id);
        if request != before[cpu_id].wrapping_add(1)
            || crate::smp::memory_barrier_ack(cpu_id) < request
        {
            return Err("AP did not acknowledge the broadcast membarrier request");
        }
    }
    crate::task::run_task_safe_point();
    Ok(())
}

fn membarrier_remote_mm_helper() {
    crate::hal::local_irq_restore(true);
    let task = crate::task::current_task().expect("membarrier helper has no current task");
    let _context = task.process.activate_user_vm();
    MEMBARRIER_REMOTE_READY.store(1, Ordering::Release);
    // 本用例专门验证“目标仍在当前 MM 中运行”时的远端 IPI/ack，不能在观察
    // 窗口主动进入调度安全点。timer/IPI 仍可打断本循环，membarrier handler
    // 因而可以正常执行完整屏障；调度请求留到 helper 退出时统一消费。
    while MEMBARRIER_REMOTE_RELEASE.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }
    // 裸入口和 noreturn 调度都不会展开 Rust 栈，必须主动释放 current clone。
    drop(task);
    crate::task::zombify_current_and_run_next();
}

/// 验证 MM-owned 注册状态以及 PRIVATE/GLOBAL 的真实 IPI/ack。
fn membarrier_reaches_mm_cpus() -> Result<(), &'static str> {
    const SYSCALL_MEMBARRIER: usize = 283;
    const CMD_QUERY: usize = 0;
    const CMD_GLOBAL: usize = 1 << 0;
    const CMD_PRIVATE_EXPEDITED: usize = 1 << 3;
    const CMD_REGISTER_PRIVATE_EXPEDITED: usize = 1 << 4;
    const SUPPORTED: isize =
        (CMD_GLOBAL | CMD_PRIVATE_EXPEDITED | CMD_REGISTER_PRIVATE_EXPEDITED) as isize;
    let call = |cmd| crate::syscall::syscall(SYSCALL_MEMBARRIER, [cmd, 0, 0, 0, 0, 0]);

    if call(CMD_QUERY) != SUPPORTED {
        return Err("membarrier query did not report the implemented commands");
    }
    let fresh = crate::mm::AddressSpace::new(crate::mm::AddressSpaceInner::<
        crate::hal::PageTableImpl,
    >::new_bare());
    if fresh.private_expedited_targets().is_some() {
        return Err("new address space inherited membarrier registration");
    }
    let current = crate::task::current_task().unwrap();
    let process = current.process.clone();
    let was_unregistered = process.vm().private_expedited_targets().is_none();
    drop(current);
    // KREPEAT 会复用 ktest runner 的 MM；只有首轮尚未注册时检查 EPERM。
    if was_unregistered && call(CMD_PRIVATE_EXPEDITED) != crate::syscall::errno::EPERM {
        return Err("private expedited membarrier succeeded before registration");
    }
    if call(CMD_REGISTER_PRIVATE_EXPEDITED) != 0 {
        return Err("private expedited membarrier registration failed");
    }

    MEMBARRIER_REMOTE_READY.store(0, Ordering::Relaxed);
    MEMBARRIER_REMOTE_RELEASE.store(0, Ordering::Relaxed);
    let remote = if crate::smp::configured_cpu_count() > 1 {
        let task = build_ktest_sibling(process, 1, membarrier_remote_mm_helper);
        crate::task::try_publish_task_on(task.clone(), 1)
            .map_err(|_| "failed to publish membarrier MM helper")?;
        Some(task)
    } else {
        None
    };

    let mut timed_out = false;
    let mut private_failed = false;
    let mut private_request_before = 0;
    let mut global_request_before = [0usize; crate::smp::MAX_CPUS];
    crate::hal::with_local_interrupts_enabled(|| {
        let deadline =
            crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(3));
        if remote.is_some() {
            while MEMBARRIER_REMOTE_READY.load(Ordering::Acquire) == 0 {
                if crate::hal::get_time() >= deadline {
                    timed_out = true;
                    break;
                }
                crate::task::run_task_safe_point();
            }
            private_request_before = crate::smp::memory_barrier_request(1);
        }

        if !timed_out && call(CMD_PRIVATE_EXPEDITED) != 0 {
            private_failed = true;
        }
        if remote.is_some() && !timed_out {
            let request = crate::smp::memory_barrier_request(1);
            if request != private_request_before + 1
                || crate::smp::memory_barrier_ack(1) < request
            {
                private_failed = true;
            }
        }

        for cpu_id in 1..crate::smp::configured_cpu_count() {
            global_request_before[cpu_id] = crate::smp::memory_barrier_request(cpu_id);
        }
        if !timed_out && call(CMD_GLOBAL) != 0 {
            private_failed = true;
        }
        for cpu_id in 1..crate::smp::configured_cpu_count() {
            if !timed_out {
                let request = crate::smp::memory_barrier_request(cpu_id);
                if request != global_request_before[cpu_id] + 1
                    || crate::smp::memory_barrier_ack(cpu_id) < request
                {
                    private_failed = true;
                }
            }
        }

        MEMBARRIER_REMOTE_RELEASE.store(1, Ordering::Release);
        if let Some(task) = remote.as_ref() {
            let cleanup_deadline =
                crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
            while task.task_status() != crate::task::TaskStatus::Zombie {
                if crate::hal::get_time() >= cleanup_deadline {
                    timed_out = true;
                    break;
                }
                crate::task::run_task_safe_point();
            }
        }
    });
    drop(remote);
    drop(crate::task::take_zombie_tasks(64));

    if timed_out {
        return Err("membarrier MM helper timed out");
    }
    if private_failed {
        return Err("membarrier did not complete the expected CPU acknowledgements");
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

/// CPU0 没有其它本地 runnable 时，绝对超时只能依靠本地 one-shot timer
/// 使架构 wait 返回，再由 idle 栈消费 deferred timer 并唤醒 runner。
fn cpu0_idle_wakes_on_local_timer() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("CPU0 idle timer test ran on an AP");
    }

    let stats_was_on = crate::task::perf::STATS_ON.swap(true, Ordering::AcqRel);
    let profile_before = crate::task::perf::STATS_PROFILE.swap(
        crate::task::perf::STATS_PROFILE_CORE,
        Ordering::AcqRel,
    );
    #[cfg(feature = "perf_stats")]
    let waits_before = crate::task::perf::SCHED_IDLE_WAIT_LOOPS_BY_CPU
        [crate::smp::BOOT_CPU_ID]
        .load(Ordering::Relaxed);
    let timer_before = crate::smp::timer_irq_count(crate::smp::BOOT_CPU_ID);
    let wait_queue = Mutex::new(crate::task::WaitQueue::new());
    let deadline = crate::timer::TimeSpec::now() + crate::timer::TimeSpec::from_ms(30);
    let wait_result = crate::task::WaitQueue::wait_event_timeout(&wait_queue, || None, deadline);
    #[cfg(feature = "perf_stats")]
    let waits_after = crate::task::perf::SCHED_IDLE_WAIT_LOOPS_BY_CPU
        [crate::smp::BOOT_CPU_ID]
        .load(Ordering::Relaxed);
    crate::task::perf::STATS_PROFILE.store(profile_before, Ordering::Release);
    crate::task::perf::STATS_ON.store(stats_was_on, Ordering::Release);

    if !matches!(wait_result, crate::task::WaitResult::TimedOut) {
        return Err("CPU0 local timer wait did not time out");
    }
    if crate::smp::timer_irq_count(crate::smp::BOOT_CPU_ID) <= timer_before {
        return Err("CPU0 local timer did not interrupt idle wait");
    }
    #[cfg(feature = "perf_stats")]
    if waits_after <= waits_before {
        return Err("CPU0 local timer wait did not enter architecture idle");
    }
    Ok(())
}

/// CPU1 在确认 runner 已完全离开 CPU0 current 后完成事件。完成路径通过
/// `Blocked -> Queued(CPU0)` 发布任务，再发送生产 RESCHEDULE doorbell。
fn wake_cpu0_idle_from_ap() {
    let target = CPU0_IDLE_WAKE_TARGET.lock().as_ref().cloned();
    let completion = CPU0_IDLE_WAKE_COMPLETION.lock().as_ref().cloned();
    let (Some(target), Some(completion)) = (target, completion) else {
        CPU0_IDLE_WAKE_ERRORS.fetch_or(1, Ordering::Release);
        return;
    };

    let deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
    loop {
        let Some(task) = target.upgrade() else {
            CPU0_IDLE_WAKE_ERRORS.fetch_or(2, Ordering::Release);
            break;
        };
        #[cfg(feature = "perf_stats")]
        let idle_wait_entered = crate::task::perf::SCHED_IDLE_WAIT_LOOPS_BY_CPU
            [crate::smp::BOOT_CPU_ID]
            .load(Ordering::Acquire)
            > CPU0_IDLE_WAKE_WAIT_BASELINE.load(Ordering::Acquire);
        #[cfg(not(feature = "perf_stats"))]
        let idle_wait_entered = true;
        if task.task_status() == crate::task::TaskStatus::Blocked
            && !crate::task::processor::cpu_has_current(crate::smp::BOOT_CPU_ID)
            && idle_wait_entered
        {
            break;
        }
        if crate::hal::get_time() >= deadline {
            CPU0_IDLE_WAKE_ERRORS.fetch_or(4, Ordering::Release);
            break;
        }
        core::hint::spin_loop();
    }
    if !completion.complete() {
        CPU0_IDLE_WAKE_ERRORS.fetch_or(8, Ordering::Release);
    }
}

/// 覆盖 CPU0 的完整 check -> WFI -> IPI -> fetch 链路，并确认 profile 计数
/// 观察到真实 wait，而不是在关中断 idle 栈上继续 busy loop。
fn cpu0_idle_wakes_on_remote_reschedule() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("CPU0 remote idle wake test ran on an AP");
    }
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }
    if CPU0_IDLE_WAKE_COMPLETION.lock().is_some()
        || CPU0_IDLE_WAKE_TARGET.lock().is_some()
    {
        return Err("stale CPU0 idle wake state remained before test");
    }

    CPU0_IDLE_WAKE_ERRORS.store(0, Ordering::Release);
    let runner = crate::task::current_task().ok_or("CPU0 idle wake runner is missing")?;
    let completion = Arc::new(crate::task::Completion::new());
    *CPU0_IDLE_WAKE_TARGET.lock() = Some(Arc::downgrade(&runner));
    *CPU0_IDLE_WAKE_COMPLETION.lock() = Some(completion.clone());

    let stats_was_on = crate::task::perf::STATS_ON.swap(true, Ordering::AcqRel);
    let profile_before = crate::task::perf::STATS_PROFILE.swap(
        crate::task::perf::STATS_PROFILE_CORE,
        Ordering::AcqRel,
    );
    #[cfg(feature = "perf_stats")]
    let waits_before = crate::task::perf::SCHED_IDLE_WAIT_LOOPS_BY_CPU
        [crate::smp::BOOT_CPU_ID]
        .load(Ordering::Relaxed);
    #[cfg(feature = "perf_stats")]
    CPU0_IDLE_WAKE_WAIT_BASELINE.store(waits_before, Ordering::Release);
    let reschedules_before = crate::smp::reschedule_count(crate::smp::BOOT_CPU_ID);
    let helper = crate::task::spawn_ktest_task_on(1, wake_cpu0_idle_from_ap);
    let wait_result = completion.wait_killable();
    #[cfg(feature = "perf_stats")]
    let waits_after = crate::task::perf::SCHED_IDLE_WAIT_LOOPS_BY_CPU
        [crate::smp::BOOT_CPU_ID]
        .load(Ordering::Relaxed);
    crate::task::perf::STATS_PROFILE.store(profile_before, Ordering::Release);
    crate::task::perf::STATS_ON.store(stats_was_on, Ordering::Release);
    *CPU0_IDLE_WAKE_TARGET.lock() = None;
    *CPU0_IDLE_WAKE_COMPLETION.lock() = None;

    if !matches!(wait_result, crate::task::WaitResult::Ready(_)) {
        return Err("CPU0 remote completion wait was interrupted");
    }
    if CPU0_IDLE_WAKE_ERRORS.load(Ordering::Acquire) != 0 {
        return Err("CPU1 did not observe and wake a stable CPU0 idle runner");
    }
    if crate::smp::reschedule_count(crate::smp::BOOT_CPU_ID) <= reschedules_before
        && !crate::smp::reschedule_pending(crate::smp::BOOT_CPU_ID)
    {
        return Err("CPU0 remote idle RESCHEDULE was neither consumed nor pending");
    }
    #[cfg(feature = "perf_stats")]
    if waits_after <= waits_before {
        return Err("CPU0 remote wake did not pass through architecture idle");
    }

    let helper_deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
    while helper.task_status() != crate::task::TaskStatus::Zombie {
        if crate::hal::get_time() >= helper_deadline {
            return Err("CPU0 idle wake helper did not exit");
        }
        core::hint::spin_loop();
    }
    Ok(())
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

/// AP kernel task 使用正式 membarrier 协议向 CPU0 发起同步。
fn ap_to_bsp_memory_barrier_helper() {
    let cpu_id = crate::smp::cpu_id();
    let rounds = AP_BARRIER_ROUNDS.load(Ordering::Acquire);
    let mut result = AP_BARRIER_PASSED;
    if cpu_id == crate::smp::BOOT_CPU_ID || rounds == 0 {
        result = AP_BARRIER_FAILED;
    } else {
        for _ in 0..rounds {
            if crate::smp::synchronize_memory(1usize << crate::smp::BOOT_CPU_ID).is_err() {
                result = AP_BARRIER_FAILED;
                break;
            }
        }
    }
    AP_BARRIER_RESULT[cpu_id].store(result, Ordering::Release);
}

/// 在 CPU0 开中断窗口内运行一个 AP→BSP 正式同步批次，并收回 helper。
fn run_ap_to_bsp_memory_barriers(cpu_id: usize, rounds: usize) -> Result<(), &'static str> {
    AP_BARRIER_ROUNDS.store(rounds, Ordering::Release);
    AP_BARRIER_RESULT[cpu_id].store(AP_BARRIER_WAITING, Ordering::Release);
    let task = crate::task::spawn_ktest_task_on(cpu_id, ap_to_bsp_memory_barrier_helper);
    let deadline = crate::hal::get_time()
        .saturating_add(crate::hal::get_clock_freq().saturating_mul(3));

    while AP_BARRIER_RESULT[cpu_id].load(Ordering::Acquire) == AP_BARRIER_WAITING {
        if crate::hal::get_time() >= deadline {
            return Err("AP-to-BSP production membarrier timed out");
        }
        core::hint::spin_loop();
    }
    if AP_BARRIER_RESULT[cpu_id].load(Ordering::Acquire) != AP_BARRIER_PASSED {
        return Err("AP-to-BSP production membarrier failed");
    }

    // helper 先发布结果再从 trampoline 进入退出路径。Zombie 状态早于实际
    // context switch，必须继续等 AP current 槽清空，才能释放本地 Arc。
    while task.task_status() != crate::task::TaskStatus::Zombie
        || crate::task::processor::cpu_has_current(cpu_id)
    {
        if crate::hal::get_time() >= deadline {
            return Err("AP membarrier helper did not leave its current slot");
        }
        core::hint::spin_loop();
    }
    drop(task);
    drop(crate::task::take_zombie_tasks(64));
    Ok(())
}

/// 反复验证 AP→BSP 的生产 mailbox、doorbell、kernel trap 与 ack 闭环。
fn ap_to_bsp_memory_barrier() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("AP-to-BSP membarrier test ran on an AP");
    }
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }

    const ROUNDS_PER_AP: usize = 64;
    // AP 在正式同步入口等待 CPU0 ack，因此 CPU0 必须在整个批次保持 IRQ-on。
    let original_irq_state = crate::hal::local_irq_save();
    crate::hal::local_irq_restore(true);
    let mut result = Ok(());
    for cpu_id in 1..crate::smp::configured_cpu_count() {
        if let Err(error) = run_ap_to_bsp_memory_barriers(cpu_id, ROUNDS_PER_AP) {
            result = Err(error);
            break;
        }
    }
    let _ = crate::hal::local_irq_save();
    // 受控窗口内可能同时收到 timer hard IRQ；用 B11 的生产安全点收尾，
    // 避免把 quiesced one-shot 留给后续测试或 shutdown。
    crate::task::run_task_safe_point();
    crate::hal::local_irq_restore(original_irq_state);
    result
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

        // 窗口恢复后再接收一次 AP 发起的生产 membarrier，证明不只是 CSR
        // 位看起来开启，而是 kernel IPI trap 确实能在该任务上下文中往返。
        receive_ap_memory_barrier_while_irqs_enabled()
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

fn receive_ap_memory_barrier_while_irqs_enabled() -> Result<(), &'static str> {
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }
    run_ap_to_bsp_memory_barriers(1, 1)
        .map_err(|_| "AP membarrier did not interrupt the syscall IRQ window")?;
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

    if !matches!(
        completion.wait_killable(),
        crate::task::WaitResult::Ready(_)
    ) {
        return Err("scheduler-state completion was interrupted");
    }
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
    let expected = AP_BLOCKED_WAKE_EXPECTED[origin].load(Ordering::Acquire);
    let completion = AP_BLOCKED_WAKE_COMPLETION
        .lock()
        .as_ref()
        .expect("AP blocked-wake completion missing")
        .clone();
    AP_BLOCKED_WAKE_PHASE[origin].store(1, Ordering::Release);
    if !matches!(
        completion.wait_killable(),
        crate::task::WaitResult::Ready(_)
    ) {
        AP_BLOCKED_WAKE_ERRORS.fetch_or(1usize << origin, Ordering::Release);
        return;
    }

    let resumed = crate::smp::cpu_id();
    let owner_is_expected = crate::task::current_task()
        .map(|task| task.task_status() == crate::task::TaskStatus::Running(expected))
        .unwrap_or(false);
    if resumed != expected || !owner_is_expected {
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
    for cpu in 1..crate::smp::configured_cpu_count() {
        AP_BLOCKED_WAKE_EXPECTED[cpu].store(cpu, Ordering::Release);
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

/// CPU1 任务完全阻塞后由 CPU0 修改 affinity；生产 wake 必须忽略旧 last_cpu，
/// 把唯一 owner 交给新 mask 中的 CPU0。
fn blocked_affinity_redirects_wake() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("blocked-affinity test did not run on CPU0");
    }
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }

    AP_BLOCKED_WAKE_ERRORS.store(0, Ordering::Release);
    AP_BLOCKED_WAKE_PHASE[1].store(0, Ordering::Release);
    AP_BLOCKED_WAKE_EXPECTED[1].store(crate::smp::BOOT_CPU_ID, Ordering::Release);
    let completion = Arc::new(crate::task::Completion::new());
    *AP_BLOCKED_WAKE_COMPLETION.lock() = Some(completion.clone());
    let task = crate::task::spawn_ktest_task_on(1, wait_for_remote_completion);

    let deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
    while AP_BLOCKED_WAKE_PHASE[1].load(Ordering::Acquire) != 1
        || task.task_status() != crate::task::TaskStatus::Blocked
        || crate::task::processor::cpu_has_current(1)
        || crate::task::run_queue_count(1) != 0
    {
        if crate::hal::get_time() >= deadline {
            return Err("CPU1 task did not become stably Blocked");
        }
        core::hint::spin_loop();
    }

    let cpu0_mask = 1usize << crate::smp::BOOT_CPU_ID;
    if !crate::task::set_remote_affinity(&task, cpu0_mask) {
        return Err("stable Blocked task rejected affinity update");
    }
    if task.cpus_allowed() != cpu0_mask {
        return Err("Blocked task did not publish the new affinity mask");
    }
    if !completion.complete() {
        return Err("Blocked task completion did not publish wakeup");
    }

    // 本次目标就是当前 CPU，wake 不需要 IPI；runner 主动让出后，队首任务
    // 必须在 CPU0 恢复、退出，再把执行权还给 runner。
    crate::task::suspend_current_and_run_next();
    *AP_BLOCKED_WAKE_COMPLETION.lock() = None;
    if AP_BLOCKED_WAKE_PHASE[1].load(Ordering::Acquire) != 2
        || task.task_status() != crate::task::TaskStatus::Zombie
        || AP_BLOCKED_WAKE_ERRORS.load(Ordering::Acquire) != 0
    {
        return Err("Blocked task did not resume exactly on its new allowed CPU");
    }
    if crate::task::run_queue_count(1) != 0 || crate::task::processor::cpu_has_current(1) {
        return Err("old CPU retained the affinity-redirected task");
    }
    Ok(())
}

/// 占据 CPU1 current，同时开放硬中断以响应后续 kernel-stack TLB 同步。
/// RESCHEDULE IPI 只置位，不会从这个任意内核位置直接切换任务。
fn hold_affinity_source_cpu() {
    let initially_enabled = crate::hal::local_irq_save();
    if initially_enabled {
        QUEUED_AFFINITY_ERRORS.fetch_or(1, Ordering::Release);
    }
    crate::hal::local_irq_restore(true);
    QUEUED_AFFINITY_HOLDER_READY.store(1, Ordering::Release);
    while QUEUED_AFFINITY_HOLDER_RELEASE.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }
    if !crate::hal::local_irq_save() {
        QUEUED_AFFINITY_ERRORS.fetch_or(2, Ordering::Release);
    }
}

fn record_queued_affinity_cpu() {
    let cpu = crate::smp::cpu_id();
    let owns_current = crate::task::current_task()
        .map(|task| task.task_status() == crate::task::TaskStatus::Running(cpu))
        .unwrap_or(false);
    if !owns_current {
        QUEUED_AFFINITY_ERRORS.fetch_or(4, Ordering::Release);
    }
    QUEUED_AFFINITY_RUN_CPU.store(cpu, Ordering::Release);
    QUEUED_AFFINITY_RUNS.fetch_add(1, Ordering::Release);
}

/// CPU1 被 holder 占据时，第二个任务会稳定留在 Queued(1)。先扩展 mask 证明
/// owner 合法时不搬队，再收紧为 bit0，验证生产路径完成唯一的跨 rq 所有权交接。
fn queued_affinity_moves_between_runqueues() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("queued-affinity test did not run on CPU0");
    }
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }

    const SOURCE_CPU: usize = 1;
    QUEUED_AFFINITY_HOLDER_READY.store(0, Ordering::Release);
    QUEUED_AFFINITY_HOLDER_RELEASE.store(0, Ordering::Release);
    QUEUED_AFFINITY_RUNS.store(0, Ordering::Release);
    QUEUED_AFFINITY_RUN_CPU.store(usize::MAX, Ordering::Release);
    QUEUED_AFFINITY_ERRORS.store(0, Ordering::Release);

    let source_before = crate::task::run_queue_count(SOURCE_CPU);
    let target_before = crate::task::run_queue_count(crate::smp::BOOT_CPU_ID);
    let holder = crate::task::spawn_ktest_task_on(SOURCE_CPU, hold_affinity_source_cpu);
    let ready_deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
    while QUEUED_AFFINITY_HOLDER_READY.load(Ordering::Acquire) == 0
        || holder.task_status() != crate::task::TaskStatus::Running(SOURCE_CPU)
    {
        if crate::hal::get_time() >= ready_deadline {
            QUEUED_AFFINITY_HOLDER_RELEASE.store(1, Ordering::Release);
            let cleanup_deadline = crate::hal::get_time()
                .saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
            while holder.task_status() != crate::task::TaskStatus::Zombie
                && crate::hal::get_time() < cleanup_deadline
            {
                core::hint::spin_loop();
            }
            return Err("CPU1 holder did not become current");
        }
        core::hint::spin_loop();
    }

    let task = crate::task::spawn_ktest_task_on(SOURCE_CPU, record_queued_affinity_cpu);
    let result = (|| {
        if task.task_status() != crate::task::TaskStatus::Queued(SOURCE_CPU)
            || crate::task::run_queue_count(SOURCE_CPU) != source_before + 1
        {
            return Err("target task did not remain Queued on CPU1");
        }

        let wide_mask = (1usize << SOURCE_CPU) | (1usize << crate::smp::BOOT_CPU_ID);
        if !crate::task::set_remote_affinity(&task, wide_mask)
            || task.task_status() != crate::task::TaskStatus::Queued(SOURCE_CPU)
            || task.cpus_allowed() != wide_mask
            || crate::task::run_queue_count(SOURCE_CPU) != source_before + 1
        {
            return Err("queued affinity moved despite retaining its owner CPU");
        }

        let cpu0_mask = 1usize << crate::smp::BOOT_CPU_ID;
        if !crate::task::set_remote_affinity(&task, cpu0_mask)
            || task.task_status()
                != crate::task::TaskStatus::Queued(crate::smp::BOOT_CPU_ID)
            || task.cpus_allowed() != cpu0_mask
            || crate::task::run_queue_count(SOURCE_CPU) != source_before
            || crate::task::run_queue_count(crate::smp::BOOT_CPU_ID) != target_before + 1
        {
            return Err("queued affinity did not transfer the unique runqueue owner");
        }

        // 目标就是当前 CPU，任务会稳定留在队首；runner 让出后它必须恰好
        // 执行一次并在 CPU0 退出，再把执行权还给 runner。
        crate::task::suspend_current_and_run_next();
        if task.task_status() != crate::task::TaskStatus::Zombie
            || task.last_cpu() != crate::smp::BOOT_CPU_ID
            || QUEUED_AFFINITY_RUNS.load(Ordering::Acquire) != 1
            || QUEUED_AFFINITY_RUN_CPU.load(Ordering::Acquire) != crate::smp::BOOT_CPU_ID
            || crate::task::run_queue_count(crate::smp::BOOT_CPU_ID) != target_before
        {
            return Err("migrated queued task did not run exactly once on CPU0");
        }
        Ok(())
    })();

    // 无论主断言是否成功，都释放 holder，避免一个失败用例污染后续 AP 测试。
    QUEUED_AFFINITY_HOLDER_RELEASE.store(1, Ordering::Release);
    let cleanup_deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
    while holder.task_status() != crate::task::TaskStatus::Zombie
        || crate::task::processor::cpu_has_current(SOURCE_CPU)
    {
        if crate::hal::get_time() >= cleanup_deadline {
            return Err("CPU1 holder did not exit during queued-affinity cleanup");
        }
        core::hint::spin_loop();
    }
    // 失败可能发生在搬队之前或刚刚搬到 CPU0 之后；两种情况下都把 subject
    // 排空，确保返回错误时不会给后续用例遗留 runnable TCB。
    if task.task_status() == crate::task::TaskStatus::Queued(crate::smp::BOOT_CPU_ID) {
        crate::task::suspend_current_and_run_next();
    }
    let task_cleanup_deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
    while task.task_status() != crate::task::TaskStatus::Zombie {
        if crate::hal::get_time() >= task_cleanup_deadline {
            return Err("queued-affinity subject did not exit during cleanup");
        }
        core::hint::spin_loop();
    }
    if QUEUED_AFFINITY_ERRORS.load(Ordering::Acquire) != 0
        || crate::task::run_queue_count(SOURCE_CPU) != source_before
    {
        return Err("queued-affinity task observed an invalid CPU/IRQ owner");
    }
    result
}

/// 在 CPU1 上持续运行，仅在生产安全点消费 RESCHEDULE。
fn wait_for_running_affinity_handoff() {
    let initially_enabled = crate::hal::local_irq_save();
    if initially_enabled {
        RUNNING_AFFINITY_ERRORS.fetch_or(1, Ordering::Release);
    }
    crate::hal::local_irq_restore(true);
    RUNNING_AFFINITY_READY.store(1, Ordering::Release);

    while crate::smp::cpu_id() == 1 && RUNNING_AFFINITY_STOP.load(Ordering::Acquire) == 0 {
        // IPI handler 只发布 need-resched；任务必须在现场完整、
        // 不持业务锁的这个安全点主动切回 idle。
        crate::task::run_task_safe_point();
        core::hint::spin_loop();
    }

    let cpu = crate::smp::cpu_id();
    let owns_current = crate::task::current_task()
        .map(|task| {
            task.task_status() == crate::task::TaskStatus::Running(cpu)
                && task.cpus_allowed() == 1usize << crate::smp::BOOT_CPU_ID
        })
        .unwrap_or(false);
    if RUNNING_AFFINITY_STOP.load(Ordering::Acquire) == 0
        && (cpu != crate::smp::BOOT_CPU_ID || !owns_current)
    {
        RUNNING_AFFINITY_ERRORS.fetch_or(2, Ordering::Release);
    }
    RUNNING_AFFINITY_RUN_CPU.store(cpu, Ordering::Release);
    RUNNING_AFFINITY_RUNS.fetch_add(1, Ordering::Release);
    if !crate::hal::local_irq_save() {
        RUNNING_AFFINITY_ERRORS.fetch_or(4, Ordering::Release);
    }
}

/// 远程 Running 任务的 mask 仍包含 owner 时只更新约束；排除 owner
/// 时，调用方必须等到源 idle 完成唯一 owner 交接才能返回。
fn running_affinity_waits_for_owner_handoff() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("running-affinity test did not run on CPU0");
    }
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }

    RUNNING_AFFINITY_READY.store(0, Ordering::Release);
    RUNNING_AFFINITY_STOP.store(0, Ordering::Release);
    RUNNING_AFFINITY_RUNS.store(0, Ordering::Release);
    RUNNING_AFFINITY_RUN_CPU.store(usize::MAX, Ordering::Release);
    RUNNING_AFFINITY_ERRORS.store(0, Ordering::Release);
    let task = crate::task::spawn_ktest_task_on(1, wait_for_running_affinity_handoff);
    let result = (|| {
        let ready_deadline =
            crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
        while RUNNING_AFFINITY_READY.load(Ordering::Acquire) == 0
            || task.task_status() != crate::task::TaskStatus::Running(1)
        {
            if crate::hal::get_time() >= ready_deadline {
                return Err("CPU1 affinity subject did not become current");
            }
            core::hint::spin_loop();
        }

        let wide_mask = (1usize << crate::smp::BOOT_CPU_ID) | (1usize << 1);
        if !crate::task::set_remote_affinity(&task, wide_mask)
            || task.task_status() != crate::task::TaskStatus::Running(1)
            || task.cpus_allowed() != wide_mask
        {
            return Err("allowed Running affinity update changed the owner");
        }

        let cpu0_mask = 1usize << crate::smp::BOOT_CPU_ID;
        if !crate::task::set_remote_affinity(&task, cpu0_mask) {
            return Err("remote Running affinity request was rejected");
        }
        if task.task_status() == crate::task::TaskStatus::Running(1)
            || task.cpus_allowed() != cpu0_mask
            || crate::task::processor::cpu_has_current(1)
            || crate::task::run_queue_count(1) != 0
        {
            return Err("remote affinity returned before the old owner handed off");
        }

        let exit_deadline =
            crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
        while task.task_status() != crate::task::TaskStatus::Zombie {
            if crate::hal::get_time() >= exit_deadline {
                return Err("migrated Running task did not exit on CPU0");
            }
            crate::task::suspend_current_and_run_next();
        }
        if RUNNING_AFFINITY_RUNS.load(Ordering::Acquire) != 1
            || RUNNING_AFFINITY_RUN_CPU.load(Ordering::Acquire) != crate::smp::BOOT_CPU_ID
            || RUNNING_AFFINITY_ERRORS.load(Ordering::Acquire) != 0
        {
            return Err("remote Running task did not resume exactly once on CPU0");
        }
        Ok(())
    })();

    if result.is_err() {
        // 失败也要收回 subject，否则一个仍在 CPU1 自旋或留在
        // CPU0 runqueue 的 TCB 会污染后续 TLB/STOP 用例。
        RUNNING_AFFINITY_STOP.store(1, Ordering::Release);
        let _ = crate::smp::request_reschedule(1);
        let cleanup_deadline =
            crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
        while task.task_status() != crate::task::TaskStatus::Zombie {
            if crate::hal::get_time() >= cleanup_deadline {
                return Err("running-affinity subject cleanup timed out");
            }
            if matches!(
                task.task_status(),
                crate::task::TaskStatus::Queued(cpu) if cpu == crate::smp::BOOT_CPU_ID
            ) {
                crate::task::suspend_current_and_run_next();
            } else {
                core::hint::spin_loop();
            }
        }
    }
    result
}

fn record_stolen_task() {
    let cpu = crate::smp::cpu_id();
    let owner_ok = crate::task::current_task()
        .map(|task| task.task_status() == crate::task::TaskStatus::Running(cpu))
        .unwrap_or(false);
    if cpu != 1 || !owner_ok {
        STEAL_ERRORS.fetch_add(1, Ordering::Release);
    }
    STEAL_CPU.store(cpu, Ordering::Release);
    STEAL_RUNS.fetch_add(1, Ordering::Release);
}

fn begin_core_stats() -> (bool, usize) {
    let stats_was_on = crate::task::perf::STATS_ON.swap(true, Ordering::AcqRel);
    let profile_before = crate::task::perf::STATS_PROFILE
        .swap(crate::task::perf::STATS_PROFILE_CORE, Ordering::AcqRel);
    (stats_was_on, profile_before)
}

fn restore_core_stats(stats_was_on: bool, profile_before: usize) {
    crate::task::perf::STATS_PROFILE.store(profile_before, Ordering::Release);
    crate::task::perf::STATS_ON.store(stats_was_on, Ordering::Release);
}

/// CPU0 runner 占据本地 current，并把 subject 明确留在 CPU0 队列；mask 只允许
/// CPU0/CPU1，因此唯一空闲且合法的 CPU1 必须通过生产 steal 路径取得它。
fn idle_cpu_steals_one_task() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("work-stealing test did not run on CPU0");
    }
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }

    STEAL_RUNS.store(0, Ordering::Release);
    STEAL_CPU.store(usize::MAX, Ordering::Release);
    STEAL_ERRORS.store(0, Ordering::Release);
    let (stats_was_on, profile_before) = begin_core_stats();
    #[cfg(feature = "perf_stats")]
    let candidate_before = crate::task::perf::STEAL_CANDIDATE_FOUND.load(Ordering::Acquire);
    #[cfg(feature = "perf_stats")]
    let ktlb_before = crate::task::perf::STEAL_KTLB_SYNC_CALLS.load(Ordering::Acquire);
    #[cfg(feature = "perf_stats")]
    let success_before = crate::task::perf::STEAL_SUCCESS.load(Ordering::Acquire);
    #[cfg(feature = "perf_stats")]
    let recheck_before = crate::task::perf::STEAL_RECHECK_FAILED.load(Ordering::Acquire);
    let queue_before = crate::task::run_queue_count(crate::smp::BOOT_CPU_ID);
    let result = (|| {
        // 先用 bit0 把 subject 稳定留在 CPU0 队列，避免“发布后检查”与 AP
        // timer 唤醒竞争；确认 victim 后再通过生产 affinity 写侧允许 CPU1。
        let task = crate::task::spawn_ktest_task_on(crate::smp::BOOT_CPU_ID, record_stolen_task);
        if task.task_status() != crate::task::TaskStatus::Queued(crate::smp::BOOT_CPU_ID)
            || crate::task::run_queue_count(crate::smp::BOOT_CPU_ID) != queue_before + 1
        {
            return Err("work-stealing subject was not queued on the victim CPU");
        }
        let wide_mask = (1usize << crate::smp::BOOT_CPU_ID) | (1usize << 1);
        if !crate::task::set_remote_affinity(&task, wide_mask) {
            return Err("work-stealing subject affinity could not be widened");
        }

        let deadline =
            crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
        while task.task_status() != crate::task::TaskStatus::Zombie {
            if crate::hal::get_time() >= deadline {
                return Err("idle CPU did not steal the queued task");
            }
            core::hint::spin_loop();
        }
        if STEAL_RUNS.load(Ordering::Acquire) != 1
            || STEAL_CPU.load(Ordering::Acquire) != 1
            || STEAL_ERRORS.load(Ordering::Acquire) != 0
            || crate::task::run_queue_count(crate::smp::BOOT_CPU_ID) != queue_before
        {
            return Err("stolen task did not keep a unique CPU/runqueue owner");
        }
        #[cfg(feature = "perf_stats")]
        {
            let candidate = crate::task::perf::STEAL_CANDIDATE_FOUND
                .load(Ordering::Acquire)
                .wrapping_sub(candidate_before);
            let ktlb = crate::task::perf::STEAL_KTLB_SYNC_CALLS
                .load(Ordering::Acquire)
                .wrapping_sub(ktlb_before);
            let success = crate::task::perf::STEAL_SUCCESS
                .load(Ordering::Acquire)
                .wrapping_sub(success_before);
            let recheck = crate::task::perf::STEAL_RECHECK_FAILED
                .load(Ordering::Acquire)
                .wrapping_sub(recheck_before);
            if candidate != 1 || candidate != ktlb || ktlb != success || recheck != 0 {
                return Err("single steal counter/state transition mismatch");
            }
        }
        Ok(())
    })();
    restore_core_stats(stats_was_on, profile_before);
    result
}

fn record_pinned_steal_subject() {
    STEAL_PINNED_CPU.store(crate::smp::cpu_id(), Ordering::Release);
    STEAL_PINNED_RUNS.fetch_add(1, Ordering::Release);
}

/// pinned-only victim 只能计入 no-eligible，不能触发 kernel-TLB 同步。
fn pinned_victim_skips_ktlb_sync() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("pinned steal test did not run on CPU0");
    }
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }

    STEAL_PINNED_RUNS.store(0, Ordering::Release);
    STEAL_PINNED_CPU.store(usize::MAX, Ordering::Release);
    let queue_before = crate::task::run_queue_count(crate::smp::BOOT_CPU_ID);
    let task =
        crate::task::spawn_ktest_task_on(crate::smp::BOOT_CPU_ID, record_pinned_steal_subject);
    let (stats_was_on, profile_before) = begin_core_stats();
    #[cfg(feature = "perf_stats")]
    let no_eligible_before = crate::task::perf::STEAL_NO_ELIGIBLE_CANDIDATE.load(Ordering::Acquire);
    #[cfg(feature = "perf_stats")]
    let ktlb_before = crate::task::perf::STEAL_KTLB_SYNC_CALLS.load(Ordering::Acquire);
    #[cfg(not(feature = "perf_stats"))]
    let timer_before = crate::smp::timer_irq_count(1);

    let result = (|| {
        crate::smp::request_reschedule(1)
            .map_err(|_| "failed to kick CPU1 for pinned steal test")?;
        let deadline =
            crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
        loop {
            #[cfg(feature = "perf_stats")]
            let attempted = crate::task::perf::STEAL_NO_ELIGIBLE_CANDIDATE.load(Ordering::Acquire)
                > no_eligible_before;
            #[cfg(not(feature = "perf_stats"))]
            let attempted = crate::smp::timer_irq_count(1) > timer_before;
            if attempted {
                break;
            }
            if crate::hal::get_time() >= deadline {
                return Err("CPU1 did not inspect the pinned-only victim");
            }
            core::hint::spin_loop();
        }
        if task.task_status() != crate::task::TaskStatus::Queued(crate::smp::BOOT_CPU_ID)
            || crate::task::run_queue_count(crate::smp::BOOT_CPU_ID) != queue_before + 1
        {
            return Err("pinned subject lost its CPU0 runqueue owner");
        }
        #[cfg(feature = "perf_stats")]
        if crate::task::perf::STEAL_KTLB_SYNC_CALLS.load(Ordering::Acquire) != ktlb_before {
            return Err("pinned-only victim triggered a kernel-TLB sync");
        }
        Ok(())
    })();
    restore_core_stats(stats_was_on, profile_before);

    // 无论计数断言是否成功，都让 pinned subject 在 CPU0 正常执行并回收，避免
    // 污染后续并发用例。
    if task.task_status() == crate::task::TaskStatus::Queued(crate::smp::BOOT_CPU_ID) {
        crate::task::suspend_current_and_run_next();
    }
    if task.task_status() != crate::task::TaskStatus::Zombie
        || STEAL_PINNED_RUNS.load(Ordering::Acquire) != 1
        || STEAL_PINNED_CPU.load(Ordering::Acquire) != crate::smp::BOOT_CPU_ID
        || crate::task::run_queue_count(crate::smp::BOOT_CPU_ID) != queue_before
    {
        return Err("pinned subject cleanup did not restore CPU0 baseline");
    }
    result
}

fn run_contended_stolen_subject() {
    let cpu = crate::smp::cpu_id();
    let Some(task) = crate::task::current_task() else {
        STEAL_CONTENTION_ERRORS.fetch_or(1, Ordering::Release);
        return;
    };
    let tid = task.gettid();
    let owner_ok = task.task_status() == crate::task::TaskStatus::Running(cpu);
    let index = STEAL_CONTENTION_TIDS
        .iter()
        .position(|expected| expected.load(Ordering::Acquire) == tid);
    let Some(index) = index else {
        STEAL_CONTENTION_ERRORS.fetch_or(2, Ordering::Release);
        return;
    };
    if cpu == crate::smp::BOOT_CPU_ID || !owner_ok {
        STEAL_CONTENTION_ERRORS.fetch_or(4, Ordering::Release);
    }
    STEAL_CONTENTION_RUNS[index].fetch_add(1, Ordering::AcqRel);
    while STEAL_CONTENTION_RELEASE.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }
}

/// 所有 AP 同时从 CPU0 victim 竞争任务；每个 claim 必须恰好对应一次本地
/// kernel-TLB 同步和一次成功 dispatch。
fn multiple_idle_cpus_compete_for_victim() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("steal contention test did not run on CPU0");
    }
    let task_count = crate::smp::configured_cpu_count().saturating_sub(1);
    if task_count == 0 {
        return Ok(());
    }

    STEAL_CONTENTION_RELEASE.store(0, Ordering::Release);
    STEAL_CONTENTION_ERRORS.store(0, Ordering::Release);
    for index in 0..crate::smp::MAX_CPUS {
        STEAL_CONTENTION_TIDS[index].store(usize::MAX, Ordering::Release);
        STEAL_CONTENTION_RUNS[index].store(0, Ordering::Release);
    }
    let mut queue_before = [0usize; crate::smp::MAX_CPUS];
    for cpu in 0..crate::smp::configured_cpu_count() {
        queue_before[cpu] = crate::task::run_queue_count(cpu);
    }
    let mut tasks = Vec::new();
    for index in 0..task_count {
        let task =
            crate::task::spawn_ktest_task_on(crate::smp::BOOT_CPU_ID, run_contended_stolen_subject);
        STEAL_CONTENTION_TIDS[index].store(task.gettid(), Ordering::Release);
        tasks.push(task);
    }

    let (stats_was_on, profile_before) = begin_core_stats();
    #[cfg(feature = "perf_stats")]
    let candidate_before = crate::task::perf::STEAL_CANDIDATE_FOUND.load(Ordering::Acquire);
    #[cfg(feature = "perf_stats")]
    let ktlb_before = crate::task::perf::STEAL_KTLB_SYNC_CALLS.load(Ordering::Acquire);
    #[cfg(feature = "perf_stats")]
    let success_before = crate::task::perf::STEAL_SUCCESS.load(Ordering::Acquire);
    #[cfg(feature = "perf_stats")]
    let recheck_before = crate::task::perf::STEAL_RECHECK_FAILED.load(Ordering::Acquire);

    let result = (|| {
        let all_cpus = (1usize << crate::smp::configured_cpu_count()) - 1;
        for task in &tasks {
            if !crate::task::set_remote_affinity(task, all_cpus) {
                return Err("failed to widen contended steal subject affinity");
            }
        }
        for cpu in 1..crate::smp::configured_cpu_count() {
            crate::smp::request_reschedule(cpu)
                .map_err(|_| "failed to kick AP for steal contention")?;
        }

        let deadline =
            crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(3));
        while !(0..task_count)
            .all(|index| STEAL_CONTENTION_RUNS[index].load(Ordering::Acquire) == 1)
        {
            if crate::hal::get_time() >= deadline {
                return Err("not all APs claimed a contended victim task");
            }
            core::hint::spin_loop();
        }
        if STEAL_CONTENTION_ERRORS.load(Ordering::Acquire) != 0 {
            return Err("contended steal observed an invalid current/CPU owner");
        }
        #[cfg(feature = "perf_stats")]
        {
            let candidate = crate::task::perf::STEAL_CANDIDATE_FOUND
                .load(Ordering::Acquire)
                .wrapping_sub(candidate_before);
            let ktlb = crate::task::perf::STEAL_KTLB_SYNC_CALLS
                .load(Ordering::Acquire)
                .wrapping_sub(ktlb_before);
            let success = crate::task::perf::STEAL_SUCCESS
                .load(Ordering::Acquire)
                .wrapping_sub(success_before);
            let recheck = crate::task::perf::STEAL_RECHECK_FAILED
                .load(Ordering::Acquire)
                .wrapping_sub(recheck_before);
            if candidate != task_count || candidate != ktlb || ktlb != success || recheck != 0 {
                return Err("contended candidate/KTLB/success counters diverged");
            }
        }
        Ok(())
    })();

    STEAL_CONTENTION_RELEASE.store(1, Ordering::Release);
    let cleanup_deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(3));
    while !tasks
        .iter()
        .all(|task| task.task_status() == crate::task::TaskStatus::Zombie)
    {
        if crate::hal::get_time() >= cleanup_deadline {
            restore_core_stats(stats_was_on, profile_before);
            return Err("contended steal subjects did not exit");
        }
        core::hint::spin_loop();
    }
    restore_core_stats(stats_was_on, profile_before);
    result?;
    if (0..task_count).any(|index| STEAL_CONTENTION_RUNS[index].load(Ordering::Acquire) != 1) {
        return Err("a contended steal subject executed more than once");
    }
    for cpu in 0..crate::smp::configured_cpu_count() {
        if crate::task::run_queue_count(cpu) != queue_before[cpu] {
            return Err("steal contention did not restore runqueue baseline");
        }
    }
    Ok(())
}

fn affinity_race_gate() {
    STEAL_AFFINITY_RACE_READY.fetch_or(1, Ordering::AcqRel);
    while STEAL_AFFINITY_RACE_START.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }
}

fn affinity_race_helper() {
    STEAL_AFFINITY_RACE_READY.fetch_or(2, Ordering::AcqRel);
    while STEAL_AFFINITY_RACE_START.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }
    let target = STEAL_AFFINITY_RACE_TARGET.lock().as_ref().cloned();
    let Some(target) = target.and_then(|target| target.upgrade()) else {
        STEAL_AFFINITY_RACE_ERRORS.fetch_or(1, Ordering::Release);
        STEAL_AFFINITY_RACE_HELPER_DONE.store(1, Ordering::Release);
        return;
    };
    let mask = (1usize << 1) | (1usize << 2);
    if crate::task::set_remote_affinity(&target, mask) {
        STEAL_AFFINITY_RACE_HELPER_OK.store(1, Ordering::Release);
    } else {
        STEAL_AFFINITY_RACE_ERRORS.fetch_or(2, Ordering::Release);
    }
    STEAL_AFFINITY_RACE_HELPER_DONE.store(1, Ordering::Release);
}

fn affinity_race_subject() {
    let cpu = crate::smp::cpu_id();
    let owner_ok = crate::task::current_task()
        .map(|task| task.task_status() == crate::task::TaskStatus::Running(cpu))
        .unwrap_or(false);
    if cpu == crate::smp::BOOT_CPU_ID || !owner_ok {
        STEAL_AFFINITY_RACE_ERRORS.fetch_or(4, Ordering::Release);
    }
    STEAL_AFFINITY_RACE_RUNS.fetch_add(1, Ordering::AcqRel);
    while STEAL_AFFINITY_RACE_RELEASE.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }
}

/// CPU1 从 victim claim 的同时，CPU2 通过生产 affinity 写侧排除 CPU0。
/// 无论 runqueue 写侧还是 stealer 先线性化，最终都必须得到一个稳定 owner，
/// 不能留下永久 Migrating、重复执行或计数下溢。
fn affinity_update_races_with_steal() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("steal/affinity race test did not run on CPU0");
    }
    if crate::smp::configured_cpu_count() < 3 {
        return Ok(());
    }

    STEAL_AFFINITY_RACE_START.store(0, Ordering::Release);
    STEAL_AFFINITY_RACE_READY.store(0, Ordering::Release);
    STEAL_AFFINITY_RACE_HELPER_DONE.store(0, Ordering::Release);
    STEAL_AFFINITY_RACE_HELPER_OK.store(0, Ordering::Release);
    STEAL_AFFINITY_RACE_RELEASE.store(0, Ordering::Release);
    STEAL_AFFINITY_RACE_RUNS.store(0, Ordering::Release);
    STEAL_AFFINITY_RACE_ERRORS.store(0, Ordering::Release);
    *STEAL_AFFINITY_RACE_TARGET.lock() = None;

    // gate 占住 CPU1，确保 subject 在 start 发布前不会被 timer 提前 steal；
    // helper 占住 CPU2，并与 gate 退出后的 CPU1 steal 同时修改 affinity。
    let gate = crate::task::spawn_ktest_task_on(1, affinity_race_gate);
    let helper = crate::task::spawn_ktest_task_on(2, affinity_race_helper);
    let ready_deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
    while STEAL_AFFINITY_RACE_READY.load(Ordering::Acquire) != 3 {
        if crate::hal::get_time() >= ready_deadline {
            STEAL_AFFINITY_RACE_START.store(1, Ordering::Release);
            return Err("steal/affinity race helpers did not start");
        }
        core::hint::spin_loop();
    }

    let subject = crate::task::spawn_ktest_task_on(crate::smp::BOOT_CPU_ID, affinity_race_subject);
    *STEAL_AFFINITY_RACE_TARGET.lock() = Some(Arc::downgrade(&subject));
    let initial_mask = (1usize << crate::smp::BOOT_CPU_ID) | (1usize << 1);
    if !crate::task::set_remote_affinity(&subject, initial_mask) {
        STEAL_AFFINITY_RACE_START.store(1, Ordering::Release);
        STEAL_AFFINITY_RACE_RELEASE.store(1, Ordering::Release);
        return Err("failed to prepare steal/affinity race subject");
    }

    let (stats_was_on, profile_before) = begin_core_stats();
    #[cfg(feature = "perf_stats")]
    let candidate_before = crate::task::perf::STEAL_CANDIDATE_FOUND.load(Ordering::Acquire);
    #[cfg(feature = "perf_stats")]
    let ktlb_before = crate::task::perf::STEAL_KTLB_SYNC_CALLS.load(Ordering::Acquire);
    #[cfg(feature = "perf_stats")]
    let success_before = crate::task::perf::STEAL_SUCCESS.load(Ordering::Acquire);
    #[cfg(feature = "perf_stats")]
    let recheck_before = crate::task::perf::STEAL_RECHECK_FAILED.load(Ordering::Acquire);

    STEAL_AFFINITY_RACE_START.store(1, Ordering::Release);
    let result = (|| {
        let deadline =
            crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(3));
        while STEAL_AFFINITY_RACE_HELPER_DONE.load(Ordering::Acquire) == 0
            || STEAL_AFFINITY_RACE_RUNS.load(Ordering::Acquire) == 0
        {
            if crate::hal::get_time() >= deadline {
                return Err("steal/affinity race did not reach stable execution");
            }
            core::hint::spin_loop();
        }
        let status = subject.task_status();
        if !matches!(status, crate::task::TaskStatus::Running(1 | 2))
            || subject.cpus_allowed() != ((1usize << 1) | (1usize << 2))
            || STEAL_AFFINITY_RACE_HELPER_OK.load(Ordering::Acquire) != 1
            || STEAL_AFFINITY_RACE_ERRORS.load(Ordering::Acquire) != 0
        {
            return Err("affinity update did not publish one stable running owner");
        }
        #[cfg(feature = "perf_stats")]
        {
            let candidate = crate::task::perf::STEAL_CANDIDATE_FOUND
                .load(Ordering::Acquire)
                .wrapping_sub(candidate_before);
            let ktlb = crate::task::perf::STEAL_KTLB_SYNC_CALLS
                .load(Ordering::Acquire)
                .wrapping_sub(ktlb_before);
            let success = crate::task::perf::STEAL_SUCCESS
                .load(Ordering::Acquire)
                .wrapping_sub(success_before);
            let recheck = crate::task::perf::STEAL_RECHECK_FAILED
                .load(Ordering::Acquire)
                .wrapping_sub(recheck_before);
            if candidate != ktlb || ktlb != success || recheck != 0 {
                return Err("steal/affinity race counter schema diverged");
            }
        }
        Ok(())
    })();

    STEAL_AFFINITY_RACE_RELEASE.store(1, Ordering::Release);
    let cleanup_deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(3));
    while subject.task_status() != crate::task::TaskStatus::Zombie
        || helper.task_status() != crate::task::TaskStatus::Zombie
        || gate.task_status() != crate::task::TaskStatus::Zombie
    {
        if crate::hal::get_time() >= cleanup_deadline {
            restore_core_stats(stats_was_on, profile_before);
            return Err("steal/affinity race cleanup timed out");
        }
        core::hint::spin_loop();
    }
    *STEAL_AFFINITY_RACE_TARGET.lock() = None;
    restore_core_stats(stats_was_on, profile_before);
    result?;
    if STEAL_AFFINITY_RACE_RUNS.load(Ordering::Acquire) != 1
        || subject.task_status() == crate::task::TaskStatus::Migrating
    {
        return Err("steal/affinity race left duplicate or migrating ownership");
    }
    Ok(())
}

fn record_local_zombie_task() {
    let cpu = crate::smp::cpu_id();
    let owner_ok = crate::task::current_task()
        .map(|task| task.task_status() == crate::task::TaskStatus::Running(cpu))
        .unwrap_or(false);
    if cpu != 1 || !owner_ok {
        LOCAL_ZOMBIE_ERRORS.fetch_add(1, Ordering::Release);
    }
    LOCAL_ZOMBIE_CPU.store(cpu, Ordering::Release);
    LOCAL_ZOMBIE_RUNS.fetch_add(1, Ordering::Release);
}

/// CPU0 runner 始终保持 current，不主动进入 idle 或调用跨 CPU take 接口；因此
/// CPU1 任务的最后一个调度 Arc 只有在 CPU1 自己的 idle 循环回收后才会消失。
fn zombie_reclaims_on_owner_idle() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("local-zombie test did not run on CPU0");
    }
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }

    drop(crate::task::take_zombie_tasks(usize::MAX));
    if crate::task::zombie_queue_count_fast() != 0 {
        return Err("stale zombie remained before local reclaim test");
    }
    LOCAL_ZOMBIE_RUNS.store(0, Ordering::Release);
    LOCAL_ZOMBIE_CPU.store(usize::MAX, Ordering::Release);
    LOCAL_ZOMBIE_ERRORS.store(0, Ordering::Release);

    let task = crate::task::spawn_ktest_task_on(1, record_local_zombie_task);
    let weak = Arc::downgrade(&task);
    let deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
    while task.task_status() != crate::task::TaskStatus::Zombie
        || crate::task::processor::cpu_has_current(1)
    {
        if crate::hal::get_time() >= deadline {
            return Err("CPU1 zombie did not leave its current slot");
        }
        core::hint::spin_loop();
    }
    if LOCAL_ZOMBIE_RUNS.load(Ordering::Acquire) != 1
        || LOCAL_ZOMBIE_CPU.load(Ordering::Acquire) != 1
        || LOCAL_ZOMBIE_ERRORS.load(Ordering::Acquire) != 0
    {
        return Err("local-zombie subject observed an invalid owner");
    }

    drop(task);
    let reclaim_deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
    while weak.upgrade().is_some() {
        if crate::hal::get_time() >= reclaim_deadline {
            return Err("CPU1 did not reclaim its own zombie Arc");
        }
        core::hint::spin_loop();
    }
    if crate::task::zombie_queue_count_fast() != 0 {
        return Err("local zombie queue retained the reclaimed task");
    }
    Ok(())
}

fn block_with_active_mm() {
    while ACTIVE_MM_START.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }

    let cpu = crate::smp::cpu_id();
    let task = crate::task::current_task().expect("active-MM helper has no current task");
    let vm = task.process.vm();
    let _context = task.process.activate_user_vm();
    if cpu != 1 || vm.active_cpu_mask() != 1usize << cpu {
        ACTIVE_MM_ERRORS.fetch_add(1, Ordering::Release);
    }
    ACTIVE_MM_PHASE.store(1, Ordering::Release);

    let completion = ACTIVE_MM_COMPLETION
        .lock()
        .as_ref()
        .expect("active-MM completion missing")
        .clone();
    if !matches!(
        completion.wait_killable(),
        crate::task::WaitResult::Ready(_)
    ) {
        ACTIVE_MM_ERRORS.fetch_add(1, Ordering::Release);
    }

    // 阻塞切栈已经从 active mask 摘除 CPU1；再次进入必须观察 CPU0
    // 在空 mask 窗口推进的 generation，并在返回前完成本地失效。
    let _context = task.process.activate_user_vm();
    if vm.active_cpu_mask() != 1usize << cpu || !vm.cpu_tlb_is_current(cpu) {
        ACTIVE_MM_ERRORS.fetch_add(1, Ordering::Release);
    }
    ACTIVE_MM_PHASE.store(2, Ordering::Release);
    drop(vm);
    drop(task);
    crate::task::zombify_current_and_run_next();
}

/// CPU1 阻塞后必须从 MM active mask 中消失。CPU0 在该窗口撤映射时不向
/// CPU1 发 shootdown，但仍推进 generation；CPU1 被唤醒后通过正式激活路径补刷。
fn inactive_mm_catches_up_on_wake() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("active-MM test did not run on CPU0");
    }
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }
    const TEST_BASE: usize = 0x55_0000;

    ACTIVE_MM_START.store(0, Ordering::Release);
    ACTIVE_MM_PHASE.store(0, Ordering::Release);
    ACTIVE_MM_ERRORS.store(0, Ordering::Release);
    let completion = Arc::new(crate::task::Completion::new());
    *ACTIVE_MM_COMPLETION.lock() = Some(completion.clone());
    let task = crate::task::spawn_ktest_task_on(1, block_with_active_mm);
    let vm = task.process.vm();

    // 先在无人活跃时建立映射。第一次 activate 同样必须追上这一代修改。
    vm.write(|space| {
        space.insert_framed_area(
            crate::mm::VirtAddr::from(TEST_BASE),
            crate::mm::VirtAddr::from(TEST_BASE + crate::config::PAGE_SIZE),
            crate::mm::MapPermission::R | crate::mm::MapPermission::W | crate::mm::MapPermission::U,
        );
    });
    ACTIVE_MM_START.store(1, Ordering::Release);

    let deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
    while ACTIVE_MM_PHASE.load(Ordering::Acquire) != 1
        || task.task_status() != crate::task::TaskStatus::Blocked
    {
        if crate::hal::get_time() >= deadline {
            completion.complete();
            return Err("CPU1 did not block after activating its MM");
        }
        core::hint::spin_loop();
    }

    let request_before = crate::smp::user_tlb_request(1);
    let mut validation_error = None;
    if vm.active_cpu_mask() != 0 {
        validation_error = Some("blocked CPU remained in the MM active mask");
    } else if !vm.cpu_tlb_is_current(1) {
        validation_error = Some("CPU1 was stale before the inactive MM update");
    } else {
        vm.write(|space| {
            space
                .remove_area_with_start_vpn(crate::mm::VirtAddr::from(TEST_BASE).floor())
                .expect("inactive-MM test unmap failed");
        });
        if crate::smp::user_tlb_request(1) != request_before {
            validation_error = Some("inactive CPU received an unnecessary TLB shootdown");
        } else if vm.cpu_tlb_is_current(1) {
            validation_error = Some("inactive CPU incorrectly observed the new TLB generation");
        }
    }

    completion.complete();
    let finish_deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
    while ACTIVE_MM_PHASE.load(Ordering::Acquire) != 2
        || task.task_status() != crate::task::TaskStatus::Zombie
        || crate::task::processor::cpu_has_current(1)
    {
        if crate::hal::get_time() >= finish_deadline {
            validation_error.get_or_insert("CPU1 did not reactivate and exit after wake");
            break;
        }
        core::hint::spin_loop();
    }
    if ACTIVE_MM_ERRORS.load(Ordering::Acquire) != 0 {
        validation_error.get_or_insert("CPU1 observed an invalid active-MM transition");
    }
    if vm.active_cpu_mask() != 0 {
        validation_error.get_or_insert("exited CPU remained in the MM active mask");
    }

    *ACTIVE_MM_COMPLETION.lock() = None;
    validation_error.map_or(Ok(()), Err)
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

    crate::smp::synchronize_user_tlb(targets, 0, None, None).map_err(|error| {
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

const STALE_TLB_OLD_VALUE: u64 = 0x1357_2468_89ab_cdef;
const STALE_TLB_NEW_VALUE: u64 = 0xfedc_ba98_7654_3210;
const STALE_TLB_REMAP_VALUE: u64 = 0xa55a_33cc_f00d_9696;

/// 返回物理页首字的内核直映地址，不构造会和用户访存重叠的 Rust 引用。
fn stale_tlb_word_ptr(ppn: crate::mm::PhysPageNum) -> *mut u64 {
    (ppn.start_addr().0 | crate::config::MEMORY_HIGH_BASE) as *mut u64
}

fn read_stale_tlb_word(address: usize) -> u64 {
    assert_ne!(address, 0, "stale-TLB progress pointer is missing");
    assert_eq!(address & (core::mem::align_of::<u64>() - 1), 0);
    // Safety: 测试在整个访问期间持有对应 FrameTracker；用户侧是另一特权级
    // 的硬件访存，不受 Rust 引用模型管理，因此这里只保留瞬时 volatile 访问。
    unsafe { core::ptr::read_volatile(address as *const u64) }
}

fn write_stale_tlb_word(address: usize, value: u64) {
    assert_ne!(address, 0, "stale-TLB data pointer is missing");
    assert_eq!(address & (core::mem::align_of::<u64>() - 1), 0);
    // Safety: 同 read_stale_tlb_word；调用方持有 frame，且只写页内首个 u64。
    unsafe { core::ptr::write_volatile(address as *mut u64, value) };
    fence(Ordering::SeqCst);
}

/// CPU1 在用户探针前关闭本地 timer，但保留 IPI 响应能力。
fn hold_stale_tlb_probe_cpu() {
    let initially_enabled = crate::hal::local_irq_save();
    if initially_enabled || crate::smp::cpu_id() != 1 {
        STALE_TLB_ERRORS.fetch_or(1, Ordering::Release);
    }

    crate::hal::quiesce_local_timer_interrupt();
    if crate::smp::local_timer_pending() {
        // 旧 tick 必须在证明窗口前完成软件记账；处理函数短暂重编程后再次
        // quiesce，确保用户 probe 不会借 generation catch-up 全量刷 TLB。
        let _ = crate::task::run_deferred_timer_work();
        crate::hal::quiesce_local_timer_interrupt();
    }
    crate::hal::local_irq_restore(true);
    STALE_TLB_HOLDER_READY.store(1, Ordering::Release);
    while STALE_TLB_HOLDER_RELEASE.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }
    if !crate::hal::local_irq_save() {
        STALE_TLB_ERRORS.fetch_or(2, Ordering::Release);
    }
}

/// FIFO 中排在用户探针之后；只有探针已给出结果，才允许恢复 CPU1 timer。
fn restore_stale_tlb_probe_timer() {
    if crate::smp::cpu_id() != 1 {
        STALE_TLB_ERRORS.fetch_or(4, Ordering::Release);
    }
    let progress = STALE_TLB_PROGRESS_PTR.load(Ordering::Acquire);
    if progress == 0 || !matches!(read_stale_tlb_word(progress), 3 | 4 | 5) {
        // 这是防御性检查：正常 FIFO 顺序下 restore 只能在探针退出后运行。
        // 若结果尚未发布，说明发生了意外调度，全刷可能污染 stale-TLB 证据。
        STALE_TLB_ERRORS.fetch_or(8, Ordering::Release);
    }

    let irq_flags = crate::hal::local_irq_save();
    let delta = crate::timer::ns_to_ticks_ceil(10_000_000).max(1);
    crate::hal::program_timer_delta(delta);
    crate::hal::enable_local_timer_interrupt();
    STALE_TLB_TIMER_RESTORED.store(1, Ordering::Release);
    crate::hal::local_irq_restore(irq_flags);
}

/// CPU1 先以用户访存填充旧翻译；CPU0 依次执行真实 CoW、
/// munmap/remap 和 RW->R mprotect。前两次必须读到新物理页，最后一次
/// 必须让远端 store 产生 SIGSEGV，从硬件行为证明三类 PTE 更新都已生效。
fn remote_user_pte_updates_take_effect() -> Result<(), &'static str> {
    if crate::smp::cpu_id() != crate::smp::BOOT_CPU_ID {
        return Err("stale-TLB user probe did not run on CPU0");
    }
    if crate::smp::configured_cpu_count() == 1 {
        return Ok(());
    }

    STALE_TLB_HOLDER_READY.store(0, Ordering::Release);
    STALE_TLB_HOLDER_RELEASE.store(0, Ordering::Release);
    STALE_TLB_TIMER_RESTORED.store(0, Ordering::Release);
    STALE_TLB_ERRORS.store(0, Ordering::Release);
    STALE_TLB_PROGRESS_PTR.store(0, Ordering::Release);

    let old_frame = crate::mm::frame_alloc().ok_or("failed to allocate stale-TLB old frame")?;
    let remap_frame =
        crate::mm::frame_alloc().ok_or("failed to allocate stale-TLB remap frame")?;
    let progress_frame =
        crate::mm::frame_alloc().ok_or("failed to allocate stale-TLB progress frame")?;
    let old_word = stale_tlb_word_ptr(old_frame.ppn) as usize;
    let progress_word = stale_tlb_word_ptr(progress_frame.ppn) as usize;
    write_stale_tlb_word(old_word, STALE_TLB_OLD_VALUE);
    write_stale_tlb_word(
        stale_tlb_word_ptr(remap_frame.ppn) as usize,
        STALE_TLB_REMAP_VALUE,
    );
    write_stale_tlb_word(progress_word, 0);

    let (task, _) = build_user_task(stale_tlb_probe_program())?;
    let vm = task.process.vm();
    let (target_addr, progress_addr) = vm.write(|space| {
        let target = space.shm_mmap(
            0,
            crate::config::PAGE_SIZE,
            crate::mm::MapPermission::R
                | crate::mm::MapPermission::W
                | crate::mm::MapPermission::U,
            crate::mm::MapFlags::MAP_PRIVATE | crate::mm::MapFlags::MAP_ANONYMOUS,
            &[old_frame.clone()],
            true,
        );
        if target < 0 {
            return Err("failed to map stale-TLB target frame");
        }
        let progress = space.shm_mmap(
            0,
            crate::config::PAGE_SIZE,
            crate::mm::MapPermission::R
                | crate::mm::MapPermission::W
                | crate::mm::MapPermission::U,
            crate::mm::MapFlags::MAP_SHARED | crate::mm::MapFlags::MAP_ANONYMOUS,
            &[progress_frame.clone()],
            true,
        );
        if progress < 0 {
            return Err("failed to map stale-TLB progress frame");
        }
        // 私有可写 VMA 的实际 PTE 去掉 W；后续 Store fault 才会进入正式 CoW。
        space
            .mprotect(
                target as usize,
                crate::config::PAGE_SIZE,
                crate::mm::MapPermission::R
                    | crate::mm::MapPermission::W
                    | crate::mm::MapPermission::U,
            )
            .map_err(|_| "failed to arm stale-TLB COW mapping")?;
        Ok((target as usize, progress as usize))
    })?;
    if progress_addr == target_addr || progress_addr == 0 {
        return Err("stale-TLB mappings are not distinct user pages");
    }
    {
        let mut inner = task.acquire_inner_lock();
        let cx = inner.trap_context_mut();
        cx.gp.a0 = target_addr;
        cx.gp.a1 = progress_addr;
        cx.gp.a2 = STALE_TLB_OLD_VALUE as usize;
        cx.gp.a3 = STALE_TLB_NEW_VALUE as usize;
        cx.gp.a4 = STALE_TLB_REMAP_VALUE as usize;
    }
    task.set_initial_cpus_allowed(1usize << 1);
    STALE_TLB_PROGRESS_PTR.store(progress_word, Ordering::Release);
    let parent_task = crate::task::current_task().ok_or("ktest runner has no current task")?;
    let parent = parent_task.process.clone();
    drop(parent_task);
    let process = task.process.clone();
    let pid = task.pid();

    let holder = crate::task::spawn_ktest_task_on(1, hold_stale_tlb_probe_cpu);
    let holder_deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
    while STALE_TLB_HOLDER_READY.load(Ordering::Acquire) == 0
        || holder.task_status() != crate::task::TaskStatus::Running(1)
    {
        if crate::hal::get_time() >= holder_deadline {
            write_stale_tlb_word(progress_word, 4);
            let restore =
                crate::task::spawn_ktest_task_on(1, restore_stale_tlb_probe_timer);
            STALE_TLB_HOLDER_RELEASE.store(1, Ordering::Release);
            let cleanup_deadline = crate::hal::get_time()
                .saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
            while (holder.task_status() != crate::task::TaskStatus::Zombie
                || restore.task_status() != crate::task::TaskStatus::Zombie)
                && crate::hal::get_time() < cleanup_deadline
            {
                core::hint::spin_loop();
            }
            if holder.task_status() == crate::task::TaskStatus::Zombie
                && restore.task_status() == crate::task::TaskStatus::Zombie
            {
                STALE_TLB_PROGRESS_PTR.store(0, Ordering::Release);
            } else {
                // 活 helper 仍可能读取该直映地址；失败路径宁可泄漏一页，也不能
                // 返回后把物理页交给分配器造成 UAF。
                core::mem::forget(progress_frame);
            }
            return Err("CPU1 did not quiesce its timer for stale-TLB probe");
        }
        core::hint::spin_loop();
    }

    if parent.add_child(process.clone()).is_err() {
        write_stale_tlb_word(progress_word, 4);
        let restore = crate::task::spawn_ktest_task_on(1, restore_stale_tlb_probe_timer);
        STALE_TLB_HOLDER_RELEASE.store(1, Ordering::Release);
        let cleanup_deadline = crate::hal::get_time()
            .saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
        while (holder.task_status() != crate::task::TaskStatus::Zombie
            || restore.task_status() != crate::task::TaskStatus::Zombie)
            && crate::hal::get_time() < cleanup_deadline
        {
            core::hint::spin_loop();
        }
        if holder.task_status() == crate::task::TaskStatus::Zombie
            && restore.task_status() == crate::task::TaskStatus::Zombie
        {
            STALE_TLB_PROGRESS_PTR.store(0, Ordering::Release);
        } else {
            core::mem::forget(progress_frame);
        }
        return Err("failed to attach stale-TLB probe to ktest runner");
    }
    process.set_parent(Some(Arc::downgrade(&parent)));

    crate::task::publish_task_on(task.clone(), 1);
    let restore = crate::task::spawn_ktest_task_on(1, restore_stale_tlb_probe_timer);
    let weak_task = Arc::downgrade(&task);
    let weak_holder = Arc::downgrade(&holder);
    let weak_restore = Arc::downgrade(&restore);
    STALE_TLB_HOLDER_RELEASE.store(1, Ordering::Release);

    let mut validation_error = None;
    let ready_deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
    loop {
        let progress = read_stale_tlb_word(progress_word);
        if progress == 1 {
            // 看到 1 后再执行全屏障，保证用户 probe 在发布 ready 前完成的
            // 旧页 load 已经发生；此后 CPU0 才允许修改 PTE 并发起 shootdown。
            fence(Ordering::SeqCst);
            break;
        }
        if progress == 4 || process.is_zombie() {
            validation_error = Some("user probe rejected the initial stale-TLB canary");
            break;
        }
        if crate::hal::get_time() >= ready_deadline {
            validation_error = Some("user probe did not warm the old TLB entry");
            break;
        }
        core::hint::spin_loop();
    }

    let full_request_before = crate::smp::user_tlb_request(1);
    let mut new_ppn = None;
    if validation_error.is_none() {
        if task.task_status() != crate::task::TaskStatus::Running(1)
            || vm.active_cpu_mask() != 1usize << 1
            || STALE_TLB_TIMER_RESTORED.load(Ordering::Acquire) != 0
        {
            validation_error = Some("stale-TLB probe lost exclusive CPU1/MM residency");
        } else {
            let target = crate::mm::VirtAddr::from(target_addr);
            let before = vm.read(|space| space.translate(target.floor()));
            let cow = vm
                .fault_in_user_va_retry(target, crate::mm::FaultAccess::Store)
                .map(|_| vm.read(|space| space.translate(target.floor())))
                .and_then(|ppn| ppn.ok_or(crate::syscall::errno::EFAULT));
            match (before, cow) {
                (Some(before), Ok(after)) if before == old_frame.ppn && after != before => {
                    new_ppn = Some(after);
                    // PTE 与远端失效已经在 AddressSpace::write() 返回前完成；此时
                    // 才改新页 canary，旧翻译若未失效就会永久停留在 OLD_VALUE。
                    write_stale_tlb_word(
                        stale_tlb_word_ptr(after) as usize,
                        STALE_TLB_NEW_VALUE,
                    );
                }
                _ => validation_error = Some("production COW did not replace the target PPN"),
            }
        }
    }

    if validation_error.is_none() {
        let result_deadline =
            crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
        loop {
            match read_stale_tlb_word(progress_word) {
                2 => break,
                4 => {
                    validation_error = Some("user probe read an unexpected post-COW canary");
                    break;
                }
                _ if crate::hal::get_time() >= result_deadline => {
                    validation_error = Some("CPU1 user load remained on the stale physical page");
                    break;
                }
                _ => core::hint::spin_loop(),
            }
        }
    }
    if new_ppn.is_some() && !vm.cpu_tlb_is_current(1) {
        validation_error.get_or_insert("range handler did not publish the observed MM generation");
    }

    if validation_error.is_none() {
        let remapped = vm.write(|space| {
            space
                .munmap(target_addr, crate::config::PAGE_SIZE)
                .map_err(|_| "production munmap rejected the target page")?;
            let mapped = space.shm_mmap(
                target_addr,
                crate::config::PAGE_SIZE,
                crate::mm::MapPermission::R
                    | crate::mm::MapPermission::W
                    | crate::mm::MapPermission::U,
                crate::mm::MapFlags::MAP_PRIVATE
                    | crate::mm::MapFlags::MAP_ANONYMOUS
                    | crate::mm::MapFlags::MAP_FIXED_NOREPLACE,
                &[remap_frame.clone()],
                true,
            );
            (mapped == target_addr as isize)
                .then_some(())
                .ok_or("fixed remap did not reuse the target VPN")
        });
        if let Err(error) = remapped {
            validation_error = Some(error);
        }
    }

    if validation_error.is_none() {
        let remap_deadline =
            crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
        loop {
            match read_stale_tlb_word(progress_word) {
                3 => break,
                4 => {
                    validation_error = Some("user probe read a stale PPN after munmap/remap");
                    break;
                }
                _ if crate::hal::get_time() >= remap_deadline => {
                    validation_error = Some("CPU1 user load did not observe the remapped page");
                    break;
                }
                _ => core::hint::spin_loop(),
            }
        }
    }

    if validation_error.is_none() {
        // marker 3 在用户态的旧 RW 映射上完成一次 store 后才发布。
        // 只有这个前置条件成立，后续写保护异常才能排除“PTE 原本就不可写”。
        let protected = vm.write(|space| {
            space.mprotect(
                target_addr,
                crate::config::PAGE_SIZE,
                crate::mm::MapPermission::R | crate::mm::MapPermission::U,
            )
        });
        if protected.is_err() {
            validation_error = Some("production mprotect rejected the target page");
        }
    }

    if validation_error.is_none() {
        // AddressSpace::write() 已在返回前收齐远程 shootdown ack；此后
        // 才放行 CPU1 的 store，因而成功写入只能说明它仍使用旧 W 权限。
        write_stale_tlb_word(progress_word, 5);
        let protect_deadline =
            crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
        while !process.is_zombie() {
            if read_stale_tlb_word(progress_word) == 4 {
                validation_error = Some("CPU1 store bypassed the mprotect downgrade");
                break;
            }
            if crate::hal::get_time() >= protect_deadline {
                validation_error = Some("CPU1 store did not fault after mprotect");
                break;
            }
            core::hint::spin_loop();
        }
    }
    if crate::smp::user_tlb_request(1) != full_request_before {
        validation_error.get_or_insert("single-page updates degraded to a full user-TLB flush");
    }

    if validation_error.is_none() && !process.is_zombie() {
        let exit_deadline =
            crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
        while !process.is_zombie() && crate::hal::get_time() < exit_deadline {
            core::hint::spin_loop();
        }
    }
    if !process.is_zombie() {
        task.acquire_inner_lock()
            .add_signal(crate::task::Signals::SIGKILL);
        let _ = crate::smp::request_reschedule(1);
    }
    let cleanup_deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(3));
    while !process.is_zombie()
        || task.task_status() != crate::task::TaskStatus::Zombie
        || holder.task_status() != crate::task::TaskStatus::Zombie
        || restore.task_status() != crate::task::TaskStatus::Zombie
        || crate::task::processor::cpu_has_current(1)
        || crate::task::run_queue_count(1) != 0
    {
        if crate::hal::get_time() >= cleanup_deadline {
            // 用户任务若仍在执行，可能继续通过旧 TLB 访问 source、remap 或
            // progress。测试已经失败，只能永久保留外部 owner，禁止分配器
            // 在后续用例中复用这三页造成 UAF。
            core::mem::forget(old_frame);
            core::mem::forget(remap_frame);
            core::mem::forget(progress_frame);
            return Err("stale-TLB probe cleanup did not quiesce CPU1");
        }
        crate::hal::with_local_interrupts_enabled(core::hint::spin_loop);
    }
    STALE_TLB_PROGRESS_PTR.store(0, Ordering::Release);

    let reaped = crate::task::ProcessManager::wait_child(
        &parent,
        pid as isize,
        true,
        true,
        false,
        false,
        false,
    )
    .map_err(|_| "ktest parent could not reap stale-TLB probe")?
    .ok_or("stale-TLB probe was not waitable")?;
    if reaped.pid != pid {
        validation_error.get_or_insert("stale-TLB probe reaped the wrong child");
    }
    if validation_error.is_none() {
        let sigsegv = crate::task::Signals::SIGSEGV.to_signum().unwrap() as u32;
        if reaped.status & 0x7f != sigsegv {
            validation_error = Some("mprotect violation did not terminate the probe with SIGSEGV");
        }
    }
    if STALE_TLB_ERRORS.load(Ordering::Acquire) != 0
        || STALE_TLB_TIMER_RESTORED.load(Ordering::Acquire) != 1
    {
        validation_error.get_or_insert("stale-TLB timer isolation evidence was incomplete");
    }
    if read_stale_tlb_word(old_word) != STALE_TLB_OLD_VALUE {
        validation_error.get_or_insert("COW modified the retained source frame");
    }
    if read_stale_tlb_word(stale_tlb_word_ptr(remap_frame.ppn) as usize)
        != STALE_TLB_REMAP_VALUE
    {
        validation_error.get_or_insert("mprotect violation modified the read-only frame");
    }

    drop(crate::task::take_zombie_tasks(usize::MAX));
    drop(process);
    drop(task);
    drop(holder);
    drop(restore);
    if weak_task.upgrade().is_some()
        || weak_holder.upgrade().is_some()
        || weak_restore.upgrade().is_some()
    {
        validation_error.get_or_insert("stale-TLB probe retained a strong TCB owner");
    }
    validation_error.map_or(Ok(()), Err)
}

/// 连续区间在 RV64 由 RFENCE 完成，在 LA64 由固定槽传递 ASID/区间。
fn user_tlb_range_sync_uses_arch_backend() -> Result<(), &'static str> {
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

    let start = crate::mm::VirtAddr::from(0x51_0000).floor();
    let range = crate::mm::VPNRange::new(start, crate::mm::VirtPageNum(start.0 + 3));
    crate::smp::synchronize_user_tlb(targets, asid, Some(range), None)
    .map_err(|error| {
        crate::println!("# user TLB range sync failed: {:?}", error);
        "user TLB range sync failed"
    })?;

    if crate::smp::configured_cpu_count() > 1 && crate::smp::user_tlb_request(1) != request_before {
        return Err("range sync unexpectedly degraded to a full user-TLB flush");
    }
    crate::task::run_task_safe_point();
    Ok(())
}

const CONCURRENT_PTE_BASE: usize = crate::config::ELF_PIE_BASE + 0x40_0000;
const CONCURRENT_PTE_STRIDE: usize = 4 * crate::config::PAGE_SIZE;
const CONCURRENT_PTE_ROUNDS: usize = 8;

fn run_concurrent_pte_updates() {
    let cpu_id = crate::smp::cpu_id();
    let vm = SHARED_TLB_VM
        .lock()
        .as_ref()
        .expect("concurrent PTE-update VM missing")
        .clone();
    vm.activate_on(cpu_id);
    PTE_UPDATE_READY.fetch_add(1, Ordering::Release);
    while PTE_UPDATE_START.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }

    let address = CONCURRENT_PTE_BASE + cpu_id * CONCURRENT_PTE_STRIDE;
    for round in 0..CONCURRENT_PTE_ROUNDS {
        let mut permission = crate::mm::MapPermission::R | crate::mm::MapPermission::U;
        if round & 1 != 0 {
            permission |= crate::mm::MapPermission::W;
        }
        if vm
            .write(|space| space.mprotect(address, crate::config::PAGE_SIZE, permission))
            .is_err()
        {
            PTE_UPDATE_ERRORS.fetch_add(1, Ordering::Release);
            break;
        }
    }

    // 所有 writer 完成前保持 MM active，保证每轮 PTE 修改都必须向全部 CPU
    // 发送真实 shootdown。等待时开放本地中断，避免形成互等 ack 的环。
    PTE_UPDATE_DONE.fetch_add(1, Ordering::Release);
    crate::hal::with_local_interrupts_enabled(|| {
        while PTE_UPDATE_DONE.load(Ordering::Acquire) != crate::smp::configured_cpu_count() {
            core::hint::spin_loop();
        }
    });
    vm.deactivate_on(cpu_id);
}

/// 所有 CPU 经真实 `AddressSpace::write()` 修改不同 PTE。VM 锁内写入虽串行，
/// 解锁后的 `TlbFlush` 可以重叠，从而验证多代 generation 与固定槽 payload 不会串线。
fn concurrent_pte_updates_keep_shootdowns_separate() -> Result<(), &'static str> {
    PTE_UPDATE_START.store(0, Ordering::Release);
    PTE_UPDATE_READY.store(0, Ordering::Release);
    PTE_UPDATE_DONE.store(0, Ordering::Release);
    PTE_UPDATE_ERRORS.store(0, Ordering::Release);

    let vm = crate::mm::AddressSpace::new(
        crate::mm::AddressSpaceInner::<crate::hal::PageTableImpl>::new_bare(),
    );
    let mut frames = Vec::new();
    for cpu_id in 0..crate::smp::configured_cpu_count() {
        let frame = crate::mm::frame_alloc().ok_or("concurrent PTE frame allocation failed")?;
        let address = CONCURRENT_PTE_BASE + cpu_id * CONCURRENT_PTE_STRIDE;
        let mapped = vm.write(|space| {
            space.shm_mmap(
                address,
                crate::config::PAGE_SIZE,
                crate::mm::MapPermission::R
                    | crate::mm::MapPermission::W
                    | crate::mm::MapPermission::U,
                crate::mm::MapFlags::MAP_SHARED
                    | crate::mm::MapFlags::MAP_ANONYMOUS
                    | crate::mm::MapFlags::MAP_FIXED_NOREPLACE,
                core::slice::from_ref(&frame),
                true,
            )
        });
        if mapped != address as isize {
            return Err("concurrent PTE test could not map its fixed page");
        }
        frames.push(frame);
    }
    vm.activate_on(crate::smp::BOOT_CPU_ID);
    *SHARED_TLB_VM.lock() = Some(vm.clone());

    let mut full_requests = [0usize; crate::smp::MAX_CPUS];
    for cpu_id in 0..crate::smp::configured_cpu_count() {
        full_requests[cpu_id] = crate::smp::user_tlb_request(cpu_id);
    }
    let mut tasks = Vec::new();
    for cpu_id in 1..crate::smp::configured_cpu_count() {
        tasks.push(crate::task::spawn_ktest_task_on(
            cpu_id,
            run_concurrent_pte_updates,
        ));
    }

    let deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
    while PTE_UPDATE_READY.load(Ordering::Acquire) != tasks.len() {
        if crate::hal::get_time() >= deadline {
            return Err("APs did not enter the concurrent PTE-update barrier");
        }
        core::hint::spin_loop();
    }

    PTE_UPDATE_START.store(1, Ordering::Release);
    run_concurrent_pte_updates();

    let completion_deadline = crate::hal::get_time().saturating_add(crate::hal::get_clock_freq());
    while tasks
        .iter()
        .any(|task| task.task_status() != crate::task::TaskStatus::Zombie)
    {
        if crate::hal::get_time() >= completion_deadline {
            return Err("concurrent PTE updates did not finish before timeout");
        }
        core::hint::spin_loop();
    }
    *SHARED_TLB_VM.lock() = None;

    if PTE_UPDATE_ERRORS.load(Ordering::Acquire) != 0 {
        return Err("a concurrent production mprotect failed");
    }
    if vm.active_cpu_mask() != 0 {
        return Err("a concurrent PTE writer retained an active-MM bit");
    }
    for cpu_id in 0..crate::smp::configured_cpu_count() {
        if !vm.cpu_tlb_is_current(cpu_id) {
            return Err("a concurrent PTE writer missed the final MM generation");
        }
    }
    for cpu_id in 0..crate::smp::configured_cpu_count() {
        if crate::smp::user_tlb_request(cpu_id) != full_requests[cpu_id] {
            return Err("concurrent PTE updates degraded to full user-TLB flush");
        }
    }
    drop(frames);
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
    const TEST_PAGES: usize = crate::smp::MAX_USER_TLB_RANGE_PAGES + 1;

    AP_USER_TLB_RETIRE_PHASE.store(0, Ordering::Release);
    AP_USER_TLB_FREE_DURING_WAIT.store(usize::MAX, Ordering::Release);
    let mut space = crate::mm::AddressSpaceInner::<crate::hal::PageTableImpl>::new_bare();
    // 比精确区间上限多一页，主动进入全刷 sequence/IPI 路径；
    // 上一用例已独立验证有界区间不会退化为全刷。
    space.insert_framed_area(
        crate::mm::VirtAddr::from(TEST_BASE),
        crate::mm::VirtAddr::from(TEST_BASE + TEST_PAGES * crate::config::PAGE_SIZE),
        crate::mm::MapPermission::R | crate::mm::MapPermission::W | crate::mm::MapPermission::U,
    );
    let vm = crate::mm::AddressSpace::new(space);
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
    } else if free_after != free_before.saturating_add(TEST_PAGES) {
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
    // CPU1 已在自己的 idle 栈释放调度 Arc；等待最后一个测试 Arc 消失后，
    // KernelStack::drop 才会把缓存溢出的映射登记到退休队列。
    let reclaim_deadline =
        crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(2));
    while weak_tasks.iter().any(|task| task.upgrade().is_some()) {
        if crate::hal::get_time() >= reclaim_deadline {
            return Err("owner CPU did not drain the kernel-stack zombie wave");
        }
        core::hint::spin_loop();
    }
    if crate::hal::reclaim_retired_kernel_stacks(usize::MAX) == 0 {
        return Err("kernel-stack cache overflow did not queue a retirement");
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
