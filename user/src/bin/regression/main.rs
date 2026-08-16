//! MangoCore L4 user-mode regression test suite.
//!
//! Each historical bug gets a permanent regression test here.
//! Run via: `regression` binary in MangoCore user space.

#![no_std]
#![no_main]

extern crate alloc;

mod regression_usercopy_pipe;
mod regression_mmap_edge_cases;
mod regression_timer_realtime_jump;
mod regression_rename_long_name;
mod regression_lwext4_truncate_hole;
mod regression_proc_cpu;
mod regression_signalfd;
mod regression_pipe_wakeup;
mod regression_pipe_resize;
mod regression_pidfd;
mod regression_net_unix_pair;
mod regression_net_tcp_accept;
mod regression_net_tcp_connect;
mod regression_net_udp;
mod regression_eventfd;
mod regression_epoll;
mod regression_futex;
mod regression_timerfd;
mod regression_ipc_sem;
mod regression_nanosleep;
mod regression_futex_requeue;
mod regression_ipc_msg;
mod regression_child_wait;
mod regression_clone_vm_second_slot;
mod regression_wait_restart;

use user_lib::{
    exec, exit, fork, get_time, kill, println, waitpid, waitpid_wnohang, SIGKILL,
};
use user_lib::syscall::{sys_nanosleep, TimeSpec};

/// fork+exec 一个独立测试 binary 并等待其退出；返回退出码是否为 0。
///
/// fs/inet/unix 是大型独立测试程序，保持进程隔离：单个测试崩溃或越界
/// 不会破坏整个回归套件，也不需要把它们模块化进本 binary。
/// fork+exec 一个独立测试 binary，等待其退出，最多等待 `timeout_ms` 毫秒。
///
/// 返回 (是否通过, 是否超时)。超时视为该组失败：SIGKILL 子进程并回收，
/// 然后继续下一组——单个套件卡死不得阻塞整个 regression。
/// fs/inet/unix 是大型独立测试程序，保持进程隔离：单个测试崩溃或越界
/// 不会破坏整个回归套件，也不需要把它们模块化进本 binary。
fn run_child_timeout(prog: &str, extra_args: &[&str], timeout_ms: u64) -> (bool, bool) {
    let pid = fork();
    if pid == 0 {
        let mut args: [*const u8; 4] = [core::ptr::null(); 4];
        args[0] = prog.as_ptr();
        for (index, arg) in extra_args.iter().enumerate() {
            args[index + 1] = arg.as_ptr();
        }
        let envp: [*const u8; 1] = [core::ptr::null()];
        exec(prog, &args, &envp);
        exit(127);
    }
    if pid <= 0 {
        return (false, false);
    }
    let deadline = get_time() + timeout_ms as isize;
    let mut status = 0i32;
    loop {
        // WNOHANG：子进程已退出则立即回收，否则检查 deadline。
        if waitpid_wnohang(pid, &mut status) == pid {
            return (status & 0x7F == 0 && (status >> 8) & 0xFF == 0, false);
        }
        if get_time() >= deadline {
            // watchdog：超时杀掉卡死的子套件并阻塞回收（SIGKILL 立即生效）。
            kill(pid as usize, SIGKILL);
            let mut st = 0i32;
            waitpid(pid as usize, &mut st);
            return (false, true);
        }
        let req = TimeSpec { tv_sec: 0, tv_nsec: 20_000_000 };
        let mut rem = TimeSpec { tv_sec: 0, tv_nsec: 0 };
        let _ = sys_nanosleep(&req, &mut rem);
    }
}

/// 打印单个 TAP 测试行；超时组单独标记。
#[allow(clippy::too_many_arguments)]
fn report(
    index: u32,
    name: &str,
    passed: &mut u32,
    failed: &mut u32,
    skipped: &mut u32,
    pass: bool,
    timed_out: bool,
) {
    if timed_out {
        *failed += 1;
        println!("[31mnot ok[0m {} {} # TIMEOUT (killed by regression watchdog)", index, name);
    } else if pass {
        *passed += 1;
        println!("[32mok[0m {} {}", index, name);
    } else {
        *failed += 1;
        println!("[31mnot ok[0m {} {}", index, name);
    }
}

#[no_mangle]
fn main(_argc: usize, _argv: &[&str]) -> i32 {
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;
    let total = 28u32;

    println!("TAP version 13");
    #[cfg(target_arch = "riscv64")]
    println!("# arch: riscv64");
    #[cfg(target_arch = "loongarch64")]
    println!("# arch: loongarch64");
    println!("# mode: regression");
    println!("1..{}", total);

    // Test 1: usercopy pipe
    let r = regression_usercopy_pipe::run();
    if r == 0 { passed += 1; println!("[32mok[0m 1 usercopy_pipe"); }
    else { failed += 1; println!("[31mnot ok[0m 1 usercopy_pipe"); }

    // Test 2: mmap edge cases
    let r = regression_mmap_edge_cases::run();
    if r == 0 { passed += 1; println!("[32mok[0m 2 mmap_edge_cases"); }
    else { failed += 1; println!("[31mnot ok[0m 2 mmap_edge_cases"); }

    // Test 3: timer realtime jump
    let r = regression_timer_realtime_jump::run();
    if r == 0 { passed += 1; println!("[32mok[0m 3 timer_realtime_jump"); }
    else { failed += 1; println!("[31mnot ok[0m 3 timer_realtime_jump"); }

    // Test 4: rename long name
    let r = regression_rename_long_name::run();
    if r == 0 { passed += 1; println!("[32mok[0m 4 rename_long_name"); }
    else { failed += 1; println!("[31mnot ok[0m 4 rename_long_name"); }

    // Test 5: lwext4 truncate hole cold reopen
    let r = regression_lwext4_truncate_hole::run();
    if r == 0 { passed += 1; println!("[32mok[0m 5 lwext4_truncate_hole"); }
    else { failed += 1; println!("[31mnot ok[0m 5 lwext4_truncate_hole"); }

    // Test 6: signalfd blocking read and fork-inherited descriptor
    let r = regression_signalfd::run();
    if r == 0 { passed += 1; println!("[32mok[0m 6 signalfd"); }
    else { failed += 1; println!("[31mnot ok[0m 6 signalfd"); }

    // Test 7: configured SMP topology is visible through procfs.
    let r = regression_proc_cpu::run();
    if r == 0 { passed += 1; println!("[32mok[0m 7 proc_cpu"); }
    else { failed += 1; println!("[31mnot ok[0m 7 proc_cpu"); }

    // Test 8: wake-one pipe progress chain
    let r = regression_pipe_wakeup::run();
    if r == 0 { passed += 1; println!("[32mok[0m 8 pipe_wakeup"); }
    else { failed += 1; println!("[31mnot ok[0m 8 pipe_wakeup"); }

    // Test 9: pidfd poll exit notification
    let r = regression_pidfd::run();
    if r == 0 { passed += 1; println!("[32mok[0m 9 pidfd"); }
    else { failed += 1; println!("[31mnot ok[0m 9 pidfd"); }

    // Test 10: Unix stream and datagram blocking socket wakeups
    let r = regression_net_unix_pair::run();
    if r == 0 { passed += 1; println!("[32mok[0m 10 net_unix_pair"); }
    else { failed += 1; println!("[31mnot ok[0m 10 net_unix_pair"); }

    // Test 11: TCP accept wakes after the child connects
    let r = regression_net_tcp_accept::run();
    if r == 0 { passed += 1; println!("[32mok[0m 11 net_tcp_accept"); }
    else { failed += 1; println!("[31mnot ok[0m 11 net_tcp_accept"); }

    // Test 12: Unix stream connect reaches a delayed acceptor
    let r = regression_net_tcp_connect::run();
    if r == 0 { passed += 1; println!("[32mok[0m 12 net_unix_connect"); }
    else { failed += 1; println!("[31mnot ok[0m 12 net_unix_connect"); }

    // Test 13: UDP recvfrom wakes after the child sends a datagram
    let r = regression_net_udp::run();
    if r == 0 { passed += 1; println!("[32mok[0m 13 net_udp"); }
    else { failed += 1; println!("[31mnot ok[0m 13 net_udp"); }

    // Test 14: blocking eventfd read and counter semantics
    let r = regression_eventfd::run();
    if r == 0 { passed += 1; println!("[32mok[0m 14 eventfd"); }
    else { failed += 1; println!("[31mnot ok[0m 14 eventfd"); }

    // Test 15: epoll wait wakeup and bounded empty timeout
    let r = regression_epoll::run();
    if r == 0 { passed += 1; println!("[32mok[0m 15 epoll"); }
    else { failed += 1; println!("[31mnot ok[0m 15 epoll"); }

    // Test 16: shared futex wait/wake and EAGAIN fast path
    let r = regression_futex::run();
    if r == 0 { passed += 1; println!("[32mok[0m 16 futex"); }
    else { failed += 1; println!("[31mnot ok[0m 16 futex"); }

    // Test 17: blocking timerfd read
    let r = regression_timerfd::run();
    if r == 0 { passed += 1; println!("[32mok[0m 17 timerfd"); }
    else { failed += 1; println!("[31mnot ok[0m 17 timerfd"); }

    // Test 18: SysV semaphore release wakes a blocking decrement
    let r = regression_ipc_sem::run();
    if r == 0 { passed += 1; println!("[32mok[0m 18 ipc_sem"); }
    else { failed += 1; println!("[31mnot ok[0m 18 ipc_sem"); }

    // Test 19: monotonic nanosleep blocks and interrupted nanosleep reports remaining time
    let r = regression_nanosleep::run();
    if r == 0 { passed += 1; println!("[32mok[0m 19 nanosleep"); }
    else { failed += 1; println!("[31mnot ok[0m 19 nanosleep"); }

    // Test 20: FUTEX_CMP_REQUEUE moves a waiting child before wakeup
    let r = regression_futex_requeue::run();
    if r == 0 { passed += 1; println!("[32mok[0m 20 futex_requeue"); }
    else { failed += 1; println!("[31mnot ok[0m 20 futex_requeue"); }

    // Test 21: SysV message queues wake blocked receivers and full-queue senders
    let r = regression_ipc_msg::run();
    if r == 0 { passed += 1; println!("[32mok[0m 21 ipc_msg"); }
    else { failed += 1; println!("[31mnot ok[0m 21 ipc_msg"); }

    // Test 22: waitpid blocks until a delayed child exit notification
    let r = regression_child_wait::run();
    if r == 0 { passed += 1; println!("[32mok[0m 22 child_wait"); }
    else { failed += 1; println!("[31mnot ok[0m 22 child_wait"); }

    // Test 23: pipe capacity resize wakes a blocked writer
    let r = regression_pipe_resize::run();
    if r == 0 { passed += 1; println!("[32mok[0m 23 pipe_resize"); }
    else { failed += 1; println!("[31mnot ok[0m 23 pipe_resize"); }

    // Test 24: SIGUSR1 with and without SA_RESTART controls waitpid interruption.
    let r = regression_wait_restart::run();
    if r == 0 { passed += 1; println!("[32mok[0m 24 wait_restart"); }
    else { failed += 1; println!("[31mnot ok[0m 24 wait_restart"); }

    // Test 25: vfork may share the caller's VM; keep this destructive probe last.
    let r = regression_clone_vm_second_slot::run();
    if r == 0 { passed += 1; println!("[32mok[0m 25 clone_vm_second_slot"); }
    else { failed += 1; println!("[31mnot ok[0m 25 clone_vm_second_slot"); }

    // Test 26: INET connectivity suite, core profile (local, no external net).
    // 网络栈核心稳定性：interface/loopback/route/port/procfs/ioctl/rtnetlink/UDP loopback。
    let (pass, timed_out) = run_child_timeout("/tests/inet_test\0", &["core\0"], 120_000);
    report(26, "inet_test", &mut passed, &mut failed, &mut skipped, pass, timed_out);

    // Test 27: Unix socket suite（快，先于慢的 FS 套件完成）。
    let (pass, timed_out) = run_child_timeout("/tests/unix_test\0", &[], 90_000);
    report(27, "unix_test", &mut passed, &mut failed, &mut skipped, pass, timed_out);

    // Test 28: FS suite（/tmp tmpfs 下运行；依赖 /bin 的 perf_fork_exec 系列与
    // busybox 的 perf_read_bb 在 regression 精简 initramfs 中失败，属环境限制
    // 而非内核回归；放最后是因为 MTTCG 下 76 个用例较慢）。
    let (pass, timed_out) = run_child_timeout("/tests/fs_test\0", &[], 480_000);
    report(28, "fs_test", &mut passed, &mut failed, &mut skipped, pass, timed_out);

    println!(
        "# results: \x1b[32m{} passed\x1b[0m, \x1b[31m{} failed\x1b[0m, \x1b[33m{} skipped\x1b[0m, {} total",
        passed, failed, skipped, total
    );

    if failed > 0 { 1 } else { 0 }
}
