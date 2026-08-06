//! 供零盘 SMP ktest 复用的、固定 CPU 的最小用户 syscall 探针。

use alloc::sync::Arc;

use crate::{
    config::PAGE_SIZE,
    fs::vfs::{File, FileFlags},
    mm::{FaultAccess, MapFlags, MapPermission, VirtAddr},
    task::{ProcessManager, Signals, TaskControlBlock, TaskStatus},
};

const SYSCALL_EXIT: usize = 93;
const SYSCALL_SOCKET: usize = 198;
const SYSCALL_BIND: usize = 200;
const SYSCALL_CLOCK_GETTIME: usize = 113;
const SYSCALL_OPENAT: usize = 56;
const SYSCALL_CLOSE: usize = 57;
const SYSCALL_UNLINKAT: usize = 35;
const SYSCALL_READ: usize = 63;
const SYSCALL_WRITE: usize = 64;
/// 项目 syscall_id.rs 自定义编号：epoll 系列并非标准 Linux 的 291/292/293。
const SYSCALL_EPOLL_CREATE1: usize = 20;
const SYSCALL_EPOLL_CTL: usize = 21;
const SYSCALL_EPOLL_PWAIT: usize = 22;
const CLOCK_MONOTONIC: usize = 1;
const EADDRINUSE: isize = -98;
const UDP_BIND_TEST_PORT_LE: usize = 0xb1ee;
const UDP_BIND_HOLD_SECS: usize = 1;
const O_CREAT_EXCL_WRONLY: usize = 0o301;
const O_RDONLY: usize = 0;
const AT_FDCWD: isize = -100;
const EPOLL_CTL_ADD: usize = 1;
/// EPOLLIN | EPOLLET。探针只注册 edge 通知，第二次非阻塞 pwait 必须返回 0。
const EPOLLIN_ET: usize = 0x1 | 0x8000_0000;

#[cfg(target_arch = "riscv64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.ktest_write_probe, "a"
    .balign 4
    .global __ktest_write_probe_start
    .global __ktest_write_probe_end
__ktest_write_probe_start:
    ecall
    lui t0, 1
    bne a0, t0, 1f
    addi a0, zero, 0
    j 2f
1:
    addi a0, zero, 1
2:
    addi a7, zero, {exit_syscall}
    ecall
3:  j 3b
__ktest_write_probe_end:
    .popsection
"#,
    exit_syscall = const SYSCALL_EXIT,
);

#[cfg(target_arch = "loongarch64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.ktest_write_probe, "a"
    .balign 4
    .global __ktest_write_probe_start
    .global __ktest_write_probe_end
__ktest_write_probe_start:
    syscall 0
    lu12i.w $t0, 1
    bne $a0, $t0, 1f
    move $a0, $zero
    b 2f
1:
    addi.d $a0, $zero, 1
2:
    addi.d $a7, $zero, {exit_syscall}
    syscall 0
3:  b 3b
__ktest_write_probe_end:
    .popsection
"#,
    exit_syscall = const SYSCALL_EXIT,
);

#[cfg(target_arch = "riscv64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.ktest_tmpfs_rename_probe, "a"
    .balign 4
    .global __ktest_tmpfs_rename_probe_start
    .global __ktest_tmpfs_rename_probe_end
__ktest_tmpfs_rename_probe_start:
    addi t0, a0, 0
    li a0, -100
    addi a1, t0, 0
    li a2, -100
    addi a3, t0, 128
    li a4, 0
    li a7, {renameat2_syscall}
    ecall
    bnez a0, 1f
    li a0, 0
    j 2f
1:  li a0, 1
2:  li a7, {exit_syscall}
    ecall
3:  j 3b
__ktest_tmpfs_rename_probe_end:
    .popsection
"#,
    renameat2_syscall = const 276usize, exit_syscall = const SYSCALL_EXIT,
);

#[cfg(target_arch = "loongarch64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.ktest_tmpfs_rename_probe, "a"
    .balign 4
    .global __ktest_tmpfs_rename_probe_start
    .global __ktest_tmpfs_rename_probe_end
__ktest_tmpfs_rename_probe_start:
    move $t0, $a0
    addi.d $a0, $zero, -100
    move $a1, $t0
    addi.d $a2, $zero, -100
    addi.d $a3, $t0, 128
    move $a4, $zero
    addi.d $a7, $zero, {renameat2_syscall}
    syscall 0
    bnez $a0, 1f
    move $a0, $zero
    b 2f
1:  addi.d $a0, $zero, 1
2:  addi.d $a7, $zero, {exit_syscall}
    syscall 0
3:  b 3b
__ktest_tmpfs_rename_probe_end:
    .popsection
"#,
    renameat2_syscall = const 276usize, exit_syscall = const SYSCALL_EXIT,
);

// 路径操作的输入在 a0：第一个以 NUL 结尾的绝对路径位于 offset 0，第二个在
// offset PATH_SLOT。探针只在用户态触发 VFS，不以 runner 直接调用 inode 方法伪造竞争。
#[cfg(target_arch = "riscv64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.ktest_tmpfs_create_probe, "a"
    .balign 4
    .global __ktest_tmpfs_create_probe_start
    .global __ktest_tmpfs_create_probe_end
__ktest_tmpfs_create_probe_start:
    addi t0, a0, 0
    li a0, -100
    addi a1, t0, 0
    li a2, {create_flags}
    li a3, 384
    li a7, {openat_syscall}
    ecall
    blt a0, zero, 1f
    addi a1, a0, 0
    li a7, {close_syscall}
    ecall
    li a0, 0
    j 2f
1:  li t0, -17
    beq a0, t0, 3f
    li a0, 2
    j 2f
3:  li a0, 1
2:  li a7, {exit_syscall}
    ecall
4:  j 4b
__ktest_tmpfs_create_probe_end:
    .popsection
"#,
    create_flags = const O_CREAT_EXCL_WRONLY, openat_syscall = const SYSCALL_OPENAT,
    close_syscall = const SYSCALL_CLOSE, exit_syscall = const SYSCALL_EXIT,
);

#[cfg(target_arch = "loongarch64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.ktest_tmpfs_create_probe, "a"
    .balign 4
    .global __ktest_tmpfs_create_probe_start
    .global __ktest_tmpfs_create_probe_end
__ktest_tmpfs_create_probe_start:
    move $t0, $a0
    addi.d $a0, $zero, -100
    move $a1, $t0
    li.w $a2, {create_flags}
    li.w $a3, 384
    addi.d $a7, $zero, {openat_syscall}
    syscall 0
    blt $a0, $zero, 1f
    move $a1, $a0
    addi.d $a7, $zero, {close_syscall}
    syscall 0
    move $a0, $zero
    b 2f
1:  addi.d $t0, $zero, -17
    beq $a0, $t0, 3f
    addi.d $a0, $zero, 2
    b 2f
3:  addi.d $a0, $zero, 1
2:  addi.d $a7, $zero, {exit_syscall}
    syscall 0
4:  b 4b
__ktest_tmpfs_create_probe_end:
    .popsection
"#,
    create_flags = const O_CREAT_EXCL_WRONLY, openat_syscall = const SYSCALL_OPENAT,
    close_syscall = const SYSCALL_CLOSE, exit_syscall = const SYSCALL_EXIT,
);

// ── tmpfs lookup probe ─────────────────────────────────────────
// 循环 openat(AT_FDCWD=-100, path, O_RDONLY) 直到 ENOENT：
// 命中文件→close 后继续（并发下反复解析同一路径），ENOENT→exit(1)，
// 其他错误→exit(2)。用于验证 unlink 生效后 stale lookup 立即失败（目录 generation）。
// 该探针无循环上限；并发测试中 unlink 探针保证文件最终消失，runner 以有界
// deadline + SIGKILL 兜底，不会永久占用 CPU。
#[cfg(target_arch = "riscv64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.ktest_tmpfs_lookup_probe, "a"
    .balign 4
    .global __ktest_tmpfs_lookup_probe_start
    .global __ktest_tmpfs_lookup_probe_end
__ktest_tmpfs_lookup_probe_start:
    addi t0, a0, 0
loop:
    li a0, -100
    addi a1, t0, 0
    li a2, 0
    li a7, {openat_syscall}
    ecall
    blt a0, zero, 1f
    addi a1, a0, 0
    li a7, {close_syscall}
    ecall
    j loop
1:  li t1, -2
    beq a0, t1, 3f
    li a0, 2
    j 2f
3:  li a0, 1
2:  li a7, {exit_syscall}
    ecall
4:  j 4b
__ktest_tmpfs_lookup_probe_end:
    .popsection
"#,
    openat_syscall = const SYSCALL_OPENAT, close_syscall = const SYSCALL_CLOSE,
    exit_syscall = const SYSCALL_EXIT,
);

#[cfg(target_arch = "loongarch64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.ktest_tmpfs_lookup_probe, "a"
    .balign 4
    .global __ktest_tmpfs_lookup_probe_start
    .global __ktest_tmpfs_lookup_probe_end
__ktest_tmpfs_lookup_probe_start:
    move $t0, $a0
loop:
    addi.d $a0, $zero, -100
    move $a1, $t0
    move $a2, $zero
    addi.d $a7, $zero, {openat_syscall}
    syscall 0
    blt $a0, $zero, 1f
    move $a1, $a0
    addi.d $a7, $zero, {close_syscall}
    syscall 0
    b loop
1:  addi.d $t1, $zero, -2
    beq $a0, $t1, 3f
    addi.d $a0, $zero, 2
    b 2f
3:  addi.d $a0, $zero, 1
2:  addi.d $a7, $zero, {exit_syscall}
    syscall 0
4:  b 4b
__ktest_tmpfs_lookup_probe_end:
    .popsection
"#,
    openat_syscall = const SYSCALL_OPENAT, close_syscall = const SYSCALL_CLOSE,
    exit_syscall = const SYSCALL_EXIT,
);

// ── tmpfs unlink probe ─────────────────────────────────────────
// unlinkat(AT_FDCWD=-100, path, 0)：成功→exit(0)，ENOENT→exit(1)，其他→exit(2)。
#[cfg(target_arch = "riscv64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.ktest_tmpfs_unlink_probe, "a"
    .balign 4
    .global __ktest_tmpfs_unlink_probe_start
    .global __ktest_tmpfs_unlink_probe_end
__ktest_tmpfs_unlink_probe_start:
    addi t0, a0, 0
    li a0, -100
    addi a1, t0, 0
    li a2, 0
    li a7, {unlinkat_syscall}
    ecall
    bnez a0, 1f
    li a0, 0
    j 2f
1:  li t0, -2
    beq a0, t0, 3f
    li a0, 2
    j 2f
3:  li a0, 1
2:  li a7, {exit_syscall}
    ecall
4:  j 4b
__ktest_tmpfs_unlink_probe_end:
    .popsection
"#,
    unlinkat_syscall = const SYSCALL_UNLINKAT, exit_syscall = const SYSCALL_EXIT,
);

#[cfg(target_arch = "loongarch64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.ktest_tmpfs_unlink_probe, "a"
    .balign 4
    .global __ktest_tmpfs_unlink_probe_start
    .global __ktest_tmpfs_unlink_probe_end
__ktest_tmpfs_unlink_probe_start:
    move $t0, $a0
    addi.d $a0, $zero, -100
    move $a1, $t0
    move $a2, $zero
    addi.d $a7, $zero, {unlinkat_syscall}
    syscall 0
    bnez $a0, 1f
    move $a0, $zero
    b 2f
1:  addi.d $t0, $zero, -2
    beq $a0, $t0, 3f
    addi.d $a0, $zero, 2
    b 2f
3:  addi.d $a0, $zero, 1
2:  addi.d $a7, $zero, {exit_syscall}
    syscall 0
4:  b 4b
__ktest_tmpfs_unlink_probe_end:
    .popsection
"#,
    unlinkat_syscall = const SYSCALL_UNLINKAT, exit_syscall = const SYSCALL_EXIT,
);

// ── eventfd write probe ────────────────────────────────────────
// write(fd, &u64=1, 8)：eventfd 计数 +1；返回 8 字节→exit(0)，否则→exit(1)。
// 供 EPOLLET edge 测试的 writer 使用；fd 与 8 字节值由 build_user_probe 预装。
#[cfg(target_arch = "riscv64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.ktest_eventfd_write_probe, "a"
    .balign 4
    .global __ktest_eventfd_write_probe_start
    .global __ktest_eventfd_write_probe_end
__ktest_eventfd_write_probe_start:
    addi t1, a1, 0
    li a2, 8
    li a7, {write_syscall}
    ecall
    li t0, 8
    bne a0, t0, 1f
    li a0, 0
    j 2f
1:  li a0, 1
2:  li a7, {exit_syscall}
    ecall
3:  j 3b
__ktest_eventfd_write_probe_end:
    .popsection
"#,
    write_syscall = const SYSCALL_WRITE, exit_syscall = const SYSCALL_EXIT,
);

#[cfg(target_arch = "loongarch64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.ktest_eventfd_write_probe, "a"
    .balign 4
    .global __ktest_eventfd_write_probe_start
    .global __ktest_eventfd_write_probe_end
__ktest_eventfd_write_probe_start:
    move $t1, $a1
    addi.d $a2, $zero, 8
    addi.d $a7, $zero, {write_syscall}
    syscall 0
    addi.d $t0, $zero, 8
    bne $a0, $t0, 1f
    move $a0, $zero
    b 2f
1:  addi.d $a0, $zero, 1
2:  addi.d $a7, $zero, {exit_syscall}
    syscall 0
3:  b 3b
__ktest_eventfd_write_probe_end:
    .popsection
"#,
    write_syscall = const SYSCALL_WRITE, exit_syscall = const SYSCALL_EXIT,
);

// ── EPOLLET edge reader probe ──────────────────────────────────
// a0 = eventfd fd。流程：
//   epoll_create1(0) → epoll_ctl(ADD, fd, EPOLLIN|EPOLLET) →
//   epoll_pwait(epfd, &events, 1, -1, NULL) 断言 1 事件 →
//   再次 epoll_pwait(epfd, &events, 1, 0, NULL) 断言 0 事件（edge 不重复触发）→
//   read(fd, &u64, 8) drain → exit(0)。
// 失败退出码区分阶段（便于 ktest 定位）：2=epoll_create1、3=epoll_ctl、
// 4=首次 pwait 非 1、5=二次 pwait 非 0、6=read 非 8。
// 第二次 pwait 必须返回 0 是 EPOLLET 的核心断言：counter 尚未 drain 也不得重复交付。
#[cfg(target_arch = "riscv64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.ktest_epollet_probe, "a"
    .balign 4
    .global __ktest_epollet_probe_start
    .global __ktest_epollet_probe_end
__ktest_epollet_probe_start:
    addi sp, sp, -80
    sd a0, 0(sp)
    li a0, 0
    li a7, {epoll_create1_syscall}
    ecall
    blt a0, zero, 1f
    sd a0, 8(sp)
    li t0, {epollin_et}
    sw t0, 16(sp)
    sd zero, 24(sp)
    ld a0, 8(sp)
    li a1, 1
    ld a2, 0(sp)
    addi a3, sp, 16
    li a7, {epoll_ctl_syscall}
    ecall
    bnez a0, 2f
    ld a0, 8(sp)
    addi a1, sp, 32
    li a2, 1
    li a3, -1
    li a4, 0
    li a7, {epoll_pwait_syscall}
    ecall
    li t0, 1
    bne a0, t0, 3f
    ld a0, 8(sp)
    addi a1, sp, 32
    li a2, 1
    li a3, 0
    li a4, 0
    li a7, {epoll_pwait_syscall}
    ecall
    bnez a0, 4f
    ld a0, 0(sp)
    addi a1, sp, 40
    li a2, 8
    li a7, {read_syscall}
    ecall
    li t0, 8
    bne a0, t0, 5f
    li a0, 0
    j 6f
1:  li a0, 2
    j 6f
2:  li a0, 3
    j 6f
3:  li a0, 4
    j 6f
4:  li a0, 5
    j 6f
5:  li a0, 6
6:  addi sp, sp, 80
    li a7, {exit_syscall}
    ecall
7:  j 7b
__ktest_epollet_probe_end:
    .popsection
"#,
    epoll_create1_syscall = const SYSCALL_EPOLL_CREATE1,
    epoll_ctl_syscall = const SYSCALL_EPOLL_CTL,
    epoll_pwait_syscall = const SYSCALL_EPOLL_PWAIT,
    read_syscall = const SYSCALL_READ,
    exit_syscall = const SYSCALL_EXIT,
    epollin_et = const EPOLLIN_ET,
);

#[cfg(target_arch = "loongarch64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.ktest_epollet_probe, "a"
    .balign 4
    .global __ktest_epollet_probe_start
    .global __ktest_epollet_probe_end
__ktest_epollet_probe_start:
    addi.d $sp, $sp, -80
    st.d $a0, $sp, 0
    move $a0, $zero
    addi.d $a7, $zero, {epoll_create1_syscall}
    syscall 0
    blt $a0, $zero, 1f
    st.d $a0, $sp, 8
    li.w $t0, {epollin_et}
    st.w $t0, $sp, 16
    st.d $zero, $sp, 24
    ld.d $a0, $sp, 8
    addi.d $a1, $zero, 1
    ld.d $a2, $sp, 0
    addi.d $a3, $sp, 16
    addi.d $a7, $zero, {epoll_ctl_syscall}
    syscall 0
    bnez $a0, 2f
    ld.d $a0, $sp, 8
    addi.d $a1, $sp, 32
    addi.d $a2, $zero, 1
    addi.d $a3, $zero, -1
    move $a4, $zero
    addi.d $a7, $zero, {epoll_pwait_syscall}
    syscall 0
    addi.d $t0, $zero, 1
    bne $a0, $t0, 3f
    ld.d $a0, $sp, 8
    addi.d $a1, $sp, 32
    addi.d $a2, $zero, 1
    move $a3, $zero
    move $a4, $zero
    addi.d $a7, $zero, {epoll_pwait_syscall}
    syscall 0
    bnez $a0, 4f
    ld.d $a0, $sp, 0
    addi.d $a1, $sp, 40
    addi.d $a2, $zero, 8
    addi.d $a7, $zero, {read_syscall}
    syscall 0
    addi.d $t0, $zero, 8
    bne $a0, $t0, 5f
    move $a0, $zero
    b 6f
1:  addi.d $a0, $zero, 2
    b 6f
2:  addi.d $a0, $zero, 3
    b 6f
3:  addi.d $a0, $zero, 4
    b 6f
4:  addi.d $a0, $zero, 5
    b 6f
5:  addi.d $a0, $zero, 6
6:  addi.d $sp, $sp, 80
    addi.d $a7, $zero, {exit_syscall}
    syscall 0
7:  b 7b
__ktest_epollet_probe_end:
    .popsection
"#,
    epoll_create1_syscall = const SYSCALL_EPOLL_CREATE1,
    epoll_ctl_syscall = const SYSCALL_EPOLL_CTL,
    epoll_pwait_syscall = const SYSCALL_EPOLL_PWAIT,
    read_syscall = const SYSCALL_READ,
    exit_syscall = const SYSCALL_EXIT,
    epollin_et = const EPOLLIN_ET,
);

extern "C" {
    static __ktest_write_probe_start: u8;
    static __ktest_write_probe_end: u8;
    static __ktest_zero_probe_start: u8;
    static __ktest_zero_probe_end: u8;
    static __ktest_tmpfs_create_probe_start: u8;
    static __ktest_tmpfs_create_probe_end: u8;
    static __ktest_tmpfs_rename_probe_start: u8;
    static __ktest_tmpfs_rename_probe_end: u8;
    static __ktest_tmpfs_lookup_probe_start: u8;
    static __ktest_tmpfs_lookup_probe_end: u8;
    static __ktest_tmpfs_unlink_probe_start: u8;
    static __ktest_tmpfs_unlink_probe_end: u8;
    static __ktest_eventfd_write_probe_start: u8;
    static __ktest_eventfd_write_probe_end: u8;
    static __ktest_epollet_probe_start: u8;
    static __ktest_epollet_probe_end: u8;
}

#[cfg(target_arch = "riscv64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.ktest_zero_probe, "a"
    .balign 4
    .global __ktest_zero_probe_start
    .global __ktest_zero_probe_end
__ktest_zero_probe_start:
    ecall
    bnez a0, 1f
    j 2f
1:
    addi a0, zero, 1
2:
    addi a7, zero, {exit_syscall}
    ecall
3:  j 3b
__ktest_zero_probe_end:
    .popsection
"#,
    exit_syscall = const SYSCALL_EXIT,
);

#[cfg(target_arch = "loongarch64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.ktest_zero_probe, "a"
    .balign 4
    .global __ktest_zero_probe_start
    .global __ktest_zero_probe_end
__ktest_zero_probe_start:
    syscall 0
    beqz $a0, 1f
    addi.d $a0, $zero, 1
1:
    addi.d $a7, $zero, {exit_syscall}
    syscall 0
2:  b 2b
__ktest_zero_probe_end:
    .popsection
"#,
    exit_syscall = const SYSCALL_EXIT,
);

#[cfg(target_arch = "riscv64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.ktest_udp_bind_probe, "a"
    .balign 4
    .global __ktest_udp_bind_probe_start
    .global __ktest_udp_bind_probe_end
__ktest_udp_bind_probe_start:
    addi sp, sp, -32
    addi a0, zero, 2
    addi a1, zero, 2
    addi a2, zero, 0
    addi a7, zero, {socket_syscall}
    ecall
    blt a0, zero, 1f
    addi s0, a0, 0
    addi t0, zero, 2
    sh t0, 0(sp)
    li t0, {port_le}
    sh t0, 2(sp)
    li t0, 0x0100007f
    sw t0, 4(sp)
    sd zero, 8(sp)
    addi a0, s0, 0
    addi a1, sp, 0
    addi a2, zero, 16
    addi a7, zero, {bind_syscall}
    ecall
    beqz a0, 2f
    addi t0, zero, {eaddrinuse}
    beq a0, t0, 3f
    addi s0, zero, 3
    j 4f
2:
    addi s0, zero, 0
    # 成功者必须在单调 deadline 前保留 fd；另一 CPU 的 bind 才能观察到 reservation。
    addi a0, zero, {clock_monotonic}
    addi a1, sp, 16
    addi a7, zero, {clock_gettime_syscall}
    ecall
    ld s1, 16(sp)
    addi s1, s1, {hold_secs}
5:
    addi a0, zero, {clock_monotonic}
    addi a1, sp, 16
    addi a7, zero, {clock_gettime_syscall}
    ecall
    ld t0, 16(sp)
    blt t0, s1, 5b
    j 4f
1:
    addi s0, zero, 2
    j 4f
3:
    addi s0, zero, 1
    j 4f
4:
    addi a0, s0, 0
    addi sp, sp, 32
    addi a7, zero, {exit_syscall}
    ecall
4:  j 4b
__ktest_udp_bind_probe_end:
    .popsection
"#,
    socket_syscall = const SYSCALL_SOCKET,
    bind_syscall = const SYSCALL_BIND,
    exit_syscall = const SYSCALL_EXIT,
    clock_gettime_syscall = const SYSCALL_CLOCK_GETTIME,
    clock_monotonic = const CLOCK_MONOTONIC,
    eaddrinuse = const EADDRINUSE,
    hold_secs = const UDP_BIND_HOLD_SECS,
    port_le = const UDP_BIND_TEST_PORT_LE,
);

#[cfg(target_arch = "loongarch64")]
core::arch::global_asm!(
    r#"
    .pushsection .rodata.ktest_udp_bind_probe, "a"
    .balign 4
    .global __ktest_udp_bind_probe_start
    .global __ktest_udp_bind_probe_end
__ktest_udp_bind_probe_start:
    addi.d $sp, $sp, -32
    addi.d $a0, $zero, 2
    addi.d $a1, $zero, 2
    addi.d $a2, $zero, 0
    addi.d $a7, $zero, {socket_syscall}
    syscall 0
    blt $a0, $zero, 1f
    move $s0, $a0
    addi.d $t0, $zero, 2
    st.h $t0, $sp, 0
    li.w $t0, {port_le}
    st.h $t0, $sp, 2
    li.w $t0, 0x0100007f
    st.w $t0, $sp, 4
    st.d $zero, $sp, 8
    move $a0, $s0
    move $a1, $sp
    addi.d $a2, $zero, 16
    addi.d $a7, $zero, {bind_syscall}
    syscall 0
    beqz $a0, 2f
    addi.d $t0, $zero, {eaddrinuse}
    beq $a0, $t0, 3f
    addi.d $s0, $zero, 3
    b 4f
2:
    move $s0, $zero
    # 成功者必须在单调 deadline 前保留 fd；另一 CPU 的 bind 才能观察到 reservation。
    addi.d $a0, $zero, {clock_monotonic}
    addi.d $a1, $sp, 16
    addi.d $a7, $zero, {clock_gettime_syscall}
    syscall 0
    ld.d $s1, $sp, 16
    addi.d $s1, $s1, {hold_secs}
5:
    addi.d $a0, $zero, {clock_monotonic}
    addi.d $a1, $sp, 16
    addi.d $a7, $zero, {clock_gettime_syscall}
    syscall 0
    ld.d $t0, $sp, 16
    blt $t0, $s1, 5b
    b 4f
1:
    addi.d $s0, $zero, 2
    b 4f
3:
    addi.d $s0, $zero, 1
    b 4f
4:
    move $a0, $s0
    addi.d $sp, $sp, 32
    addi.d $a7, $zero, {exit_syscall}
    syscall 0
4:  b 4b
__ktest_udp_bind_probe_end:
    .popsection
"#,
    socket_syscall = const SYSCALL_SOCKET,
    bind_syscall = const SYSCALL_BIND,
    exit_syscall = const SYSCALL_EXIT,
    clock_gettime_syscall = const SYSCALL_CLOCK_GETTIME,
    clock_monotonic = const CLOCK_MONOTONIC,
    eaddrinuse = const EADDRINUSE,
    hold_secs = const UDP_BIND_HOLD_SECS,
    port_le = const UDP_BIND_TEST_PORT_LE,
);

extern "C" {
    static __ktest_udp_bind_probe_start: u8;
    static __ktest_udp_bind_probe_end: u8;
}

/// 用户 syscall 返回值的 ktest 成功条件。
pub(crate) enum ProbeResult {
    WritePage,
    Zero,
    UdpBind,
    TmpfsCreate,
    TmpfsRename,
    TmpfsLookup,
    TmpfsUnlink,
    EventFdWrite,
    EpollEdge,
}

fn user_probe_program(result: ProbeResult) -> &'static [u8] {
    let (start, end) = match result {
        ProbeResult::WritePage => (
            core::ptr::addr_of!(__ktest_write_probe_start),
            core::ptr::addr_of!(__ktest_write_probe_end),
        ),
        ProbeResult::Zero => (
            core::ptr::addr_of!(__ktest_zero_probe_start),
            core::ptr::addr_of!(__ktest_zero_probe_end),
        ),
        ProbeResult::UdpBind => (
            core::ptr::addr_of!(__ktest_udp_bind_probe_start),
            core::ptr::addr_of!(__ktest_udp_bind_probe_end),
        ),
        ProbeResult::TmpfsCreate => (
            core::ptr::addr_of!(__ktest_tmpfs_create_probe_start),
            core::ptr::addr_of!(__ktest_tmpfs_create_probe_end),
        ),
        ProbeResult::TmpfsRename => (
            core::ptr::addr_of!(__ktest_tmpfs_rename_probe_start),
            core::ptr::addr_of!(__ktest_tmpfs_rename_probe_end),
        ),
        ProbeResult::TmpfsLookup => (
            core::ptr::addr_of!(__ktest_tmpfs_lookup_probe_start),
            core::ptr::addr_of!(__ktest_tmpfs_lookup_probe_end),
        ),
        ProbeResult::TmpfsUnlink => (
            core::ptr::addr_of!(__ktest_tmpfs_unlink_probe_start),
            core::ptr::addr_of!(__ktest_tmpfs_unlink_probe_end),
        ),
        ProbeResult::EventFdWrite => (
            core::ptr::addr_of!(__ktest_eventfd_write_probe_start),
            core::ptr::addr_of!(__ktest_eventfd_write_probe_end),
        ),
        ProbeResult::EpollEdge => (
            core::ptr::addr_of!(__ktest_epollet_probe_start),
            core::ptr::addr_of!(__ktest_epollet_probe_end),
        ),
    };
    // SAFETY: [Category 10/11 — bounds/provenance] 同一 `global_asm!` section 定义的
    // start/end 符号包围连续只读指令流，链接器固定其相对顺序；没有把整数恢复为指针。
    unsafe { core::slice::from_raw_parts(start, end.offset_from(start) as usize) }
}

/// 创建以数据页路径为参数的用户探针。路径在用户页内，避免内核持锁进入 uaccess。
pub(crate) fn build_path_probe(
    result: ProbeResult,
    paths: &[u8],
) -> Result<Arc<TaskControlBlock>, &'static str> {
    let inode = crate::fs::vfs_lookup_absolute("/init")
        .or_else(|_| crate::fs::vfs_lookup_absolute("/initproc"))
        .map_err(|_| "ktest initramfs has no user ELF scaffold")?;
    let elf =
        File::new(inode, FileFlags::O_RDONLY).map_err(|_| "failed to open user ELF scaffold")?;
    let task = TaskControlBlock::new(elf);
    task.process.close_files_on_exit();
    let entry = map_user_page(&task, user_probe_program(result), true)?;
    let input = map_user_page(&task, paths, false)?;
    let mut inner = task.acquire_inner_lock();
    let gp = &mut inner.trap_context_mut().gp;
    gp.pc = entry;
    gp.a0 = input;
    drop(inner);
    Ok(task)
}

fn map_user_page(
    task: &Arc<TaskControlBlock>,
    data: &[u8],
    executable: bool,
) -> Result<usize, &'static str> {
    task.process.vm().write(|space| {
        let address = space.mmap(
            0,
            PAGE_SIZE,
            MapPermission::R | MapPermission::W | MapPermission::U,
            MapFlags::MAP_PRIVATE | MapFlags::MAP_ANONYMOUS,
            0,
            None,
            true,
            false,
        );
        if address < 0 || data.len() > PAGE_SIZE {
            return Err("failed to reserve user probe page");
        }
        let address = address as usize;
        let pa = space
            .fault_in_user_va(VirtAddr::from(address), FaultAccess::Store)
            .map_err(|_| "failed to populate user probe page")?;
        let offset = pa.page_offset();
        // Safety: `space.write()` 独占该测试地址空间，目标页已由上面的
        // store fault 建立，并且测试任务尚未发布到调度器。
        unsafe {
            pa.floor().with_bytes_mut(|page| {
                page[offset..offset + data.len()].copy_from_slice(data)
            });
        }
        if executable {
            space
                .mprotect(
                    address,
                    PAGE_SIZE,
                    MapPermission::R | MapPermission::X | MapPermission::U,
                )
                .map_err(|_| "failed to protect user probe code page")?;
        }
        Ok(address)
    })
}

pub(crate) fn build_user_probe(
    result: ProbeResult,
    file: Arc<File>,
    syscall: usize,
    data: Option<&[u8]>,
) -> Result<Arc<TaskControlBlock>, &'static str> {
    let inode = crate::fs::vfs_lookup_absolute("/init")
        .or_else(|_| crate::fs::vfs_lookup_absolute("/initproc"))
        .map_err(|_| "ktest initramfs has no user ELF scaffold")?;
    let elf =
        File::new(inode, FileFlags::O_RDONLY).map_err(|_| "failed to open user ELF scaffold")?;
    let task = TaskControlBlock::new(elf);
    task.process.close_files_on_exit();
    let fd = task
        .process
        .files()
        .lock()
        .alloc_fd(file, false)
        .map_err(|_| "failed to install user probe fd")?;
    let entry = map_user_page(&task, user_probe_program(result), true)?;
    let buffer = match data {
        Some(bytes) => map_user_page(&task, bytes, false)?,
        None => 0,
    };
    let mut inner = task.acquire_inner_lock();
    let gp = &mut inner.trap_context_mut().gp;
    gp.pc = entry;
    gp.a0 = fd;
    gp.a1 = buffer;
    gp.a2 = data.map_or(0, <[u8]>::len);
    gp.a7 = syscall;
    drop(inner);
    Ok(task)
}

/// 创建自行执行 socket()+bind() 的用户探针；网络操作只会在被发布后的普通用户 TCB 内发生。
pub(crate) fn build_udp_bind_probe() -> Result<Arc<TaskControlBlock>, &'static str> {
    let inode = crate::fs::vfs_lookup_absolute("/init")
        .or_else(|_| crate::fs::vfs_lookup_absolute("/initproc"))
        .map_err(|_| "ktest initramfs has no user ELF scaffold")?;
    let elf =
        File::new(inode, FileFlags::O_RDONLY).map_err(|_| "failed to open user ELF scaffold")?;
    let task = TaskControlBlock::new(elf);
    task.process.close_files_on_exit();
    let entry = map_user_page(&task, user_probe_program(ProbeResult::UdpBind), true)?;
    let mut inner = task.acquire_inner_lock();
    inner.trap_context_mut().gp.pc = entry;
    drop(inner);
    Ok(task)
}

/// 创建共享同一 eventfd 的 EPOLLET reader 探针；`a0` 为该进程内安装的 eventfd fd。
pub(crate) fn build_epoll_edge_reader(
    eventfd: Arc<File>,
) -> Result<Arc<TaskControlBlock>, &'static str> {
    build_probe_with_fd(ProbeResult::EpollEdge, eventfd, None)
}

/// 创建共享同一 eventfd 的 writer 探针；`a0` 为该进程内安装的 eventfd fd，
/// `a1` 指向 8 字节 `u64 = 1` 的用户页。
pub(crate) fn build_eventfd_writer(
    eventfd: Arc<File>,
) -> Result<Arc<TaskControlBlock>, &'static str> {
    build_probe_with_fd(
        ProbeResult::EventFdWrite,
        eventfd,
        Some(&1u64.to_le_bytes()),
    )
}

/// 以普通用户 TCB 构造探针，并把一个已存在的 `File` 安装到其 fd 表（`a0`）。
fn build_probe_with_fd(
    result: ProbeResult,
    file: Arc<File>,
    data: Option<&[u8]>,
) -> Result<Arc<TaskControlBlock>, &'static str> {
    let inode = crate::fs::vfs_lookup_absolute("/init")
        .or_else(|_| crate::fs::vfs_lookup_absolute("/initproc"))
        .map_err(|_| "ktest initramfs has no user ELF scaffold")?;
    let elf =
        File::new(inode, FileFlags::O_RDONLY).map_err(|_| "failed to open user ELF scaffold")?;
    let task = TaskControlBlock::new(elf);
    task.process.close_files_on_exit();
    let fd = task
        .process
        .files()
        .lock()
        .alloc_fd(file, false)
        .map_err(|_| "failed to install eventfd fd into probe")?;
    let entry = map_user_page(&task, user_probe_program(result), true)?;
    let buffer = match data {
        Some(bytes) => map_user_page(&task, bytes, false)?,
        None => 0,
    };
    let mut inner = task.acquire_inner_lock();
    let gp = &mut inner.trap_context_mut().gp;
    gp.pc = entry;
    gp.a0 = fd;
    gp.a1 = buffer;
    drop(inner);
    Ok(task)
}

pub(crate) fn attach_probe_to_runner(
    task: &Arc<TaskControlBlock>,
) -> Result<Arc<crate::task::ProcessControlBlock>, &'static str> {
    let runner = crate::task::current_task().ok_or("ktest runner has no current task")?;
    let parent = runner.process.clone();
    drop(runner);
    let process = task.process.clone();
    parent
        .add_child(process.clone())
        .map_err(|_| "failed to attach user probe to ktest runner")?;
    process.set_parent(Some(Arc::downgrade(&parent)));
    Ok(parent)
}

pub(crate) fn deadline_after(seconds: usize) -> usize {
    crate::hal::get_time().saturating_add(crate::hal::get_clock_freq().saturating_mul(seconds))
}

pub(crate) fn probe_quiesced(
    task: &Arc<TaskControlBlock>,
    process: &Arc<crate::task::ProcessControlBlock>,
    cpu: usize,
    deadline: usize,
) -> bool {
    crate::hal::with_local_interrupts_enabled(|| {
        while !process.is_zombie()
            || task.task_status() != TaskStatus::Zombie
            || crate::task::processor::cpu_has_current(cpu)
            || crate::task::run_queue_count(cpu) != 0
        {
            if crate::hal::get_time() >= deadline {
                return false;
            }
            crate::task::run_task_safe_point();
            core::hint::spin_loop();
        }
        true
    })
}

pub(crate) fn stop_probe(
    task: &Arc<TaskControlBlock>,
    process: &Arc<crate::task::ProcessControlBlock>,
    cpu: usize,
) -> bool {
    if !process.is_zombie() {
        task.acquire_inner_lock().add_signal(Signals::SIGKILL);
        let _ = crate::smp::request_reschedule(cpu);
    }
    probe_quiesced(task, process, cpu, deadline_after(2))
}

pub(crate) fn reap_probe(
    parent: &Arc<crate::task::ProcessControlBlock>,
    task: &Arc<TaskControlBlock>,
) -> bool {
    ProcessManager::wait_child(parent, task.pid() as isize, true, true, false, false, false)
        .ok()
        .flatten()
        .is_some()
}
