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
const CLOCK_MONOTONIC: usize = 1;
const EADDRINUSE: isize = -98;
const UDP_BIND_TEST_PORT_LE: usize = 0xb1ee;
const UDP_BIND_HOLD_SECS: usize = 1;
const O_CREAT_EXCL_WRONLY: usize = 0o301;

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
    close_syscall = const 57usize, exit_syscall = const SYSCALL_EXIT,
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
    close_syscall = const 57usize, exit_syscall = const SYSCALL_EXIT,
);

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

extern "C" {
    static __ktest_write_probe_start: u8;
    static __ktest_write_probe_end: u8;
    static __ktest_zero_probe_start: u8;
    static __ktest_zero_probe_end: u8;
    static __ktest_tmpfs_create_probe_start: u8;
    static __ktest_tmpfs_create_probe_end: u8;
    static __ktest_tmpfs_rename_probe_start: u8;
    static __ktest_tmpfs_rename_probe_end: u8;
}

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
    };
    // SAFETY: [Category 10/11 — bounds/provenance] 同一 `global_asm!` section 定义的
    // start/end 符号包围连续只读指令流，链接器固定其相对顺序；没有把整数恢复为指针。
    unsafe { core::slice::from_raw_parts(start, end.offset_from(start) as usize) }
}

/// 创建以数据页路径为参数的用户探针。路径在用户页内，避免内核持锁进入 uaccess。
pub(crate) fn build_path_probe(result: ProbeResult, paths: &[u8]) -> Result<Arc<TaskControlBlock>, &'static str> {
    let inode = crate::fs::vfs_lookup_absolute("/init")
        .or_else(|_| crate::fs::vfs_lookup_absolute("/initproc"))
        .map_err(|_| "ktest initramfs has no user ELF scaffold")?;
    let elf = File::new(inode, FileFlags::O_RDONLY).map_err(|_| "failed to open user ELF scaffold")?;
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

fn map_user_page(task: &Arc<TaskControlBlock>, data: &[u8], executable: bool) -> Result<usize, &'static str> {
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
        pa.floor().get_bytes_array()[offset..offset + data.len()].copy_from_slice(data);
        if executable {
            space
                .mprotect(address, PAGE_SIZE, MapPermission::R | MapPermission::X | MapPermission::U)
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
    let elf = File::new(inode, FileFlags::O_RDONLY).map_err(|_| "failed to open user ELF scaffold")?;
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
    let elf = File::new(inode, FileFlags::O_RDONLY).map_err(|_| "failed to open user ELF scaffold")?;
    let task = TaskControlBlock::new(elf);
    task.process.close_files_on_exit();
    let entry = map_user_page(&task, user_probe_program(ProbeResult::UdpBind), true)?;
    let mut inner = task.acquire_inner_lock();
    inner.trap_context_mut().gp.pc = entry;
    drop(inner);
    Ok(task)
}

pub(crate) fn attach_probe_to_runner(task: &Arc<TaskControlBlock>) -> Result<Arc<crate::task::ProcessControlBlock>, &'static str> {
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

pub(crate) fn probe_quiesced(task: &Arc<TaskControlBlock>, process: &Arc<crate::task::ProcessControlBlock>, cpu: usize, deadline: usize) -> bool {
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

pub(crate) fn stop_probe(task: &Arc<TaskControlBlock>, process: &Arc<crate::task::ProcessControlBlock>, cpu: usize) -> bool {
    if !process.is_zombie() {
        task.acquire_inner_lock().add_signal(Signals::SIGKILL);
        let _ = crate::smp::request_reschedule(cpu);
    }
    probe_quiesced(task, process, cpu, deadline_after(2))
}

pub(crate) fn reap_probe(parent: &Arc<crate::task::ProcessControlBlock>, task: &Arc<TaskControlBlock>) -> bool {
    ProcessManager::wait_child(parent, task.pid() as isize, true, true, false, false, false)
        .ok()
        .flatten()
        .is_some()
}
